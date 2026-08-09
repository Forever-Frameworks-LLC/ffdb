use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ffdb_protocol::{ProjectId, SessionId, TokenId, UserId};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{CredentialDigest, OpaqueTokenCodec, SecretToken, TokenError};

pub const REFRESH_TOKEN_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub user_id: UserId,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
    pub revoke_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RefreshFamily {
    pub id: Uuid,
    pub project_id: ProjectId,
    pub user_id: UserId,
    pub session_id: SessionId,
    pub revoked_at_ms: Option<i64>,
    pub revoke_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RefreshTokenRecord {
    pub id: TokenId,
    pub family_id: Uuid,
    pub prefix: String,
    pub digest: CredentialDigest,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub used_at_ms: Option<i64>,
    pub replaced_by: Option<TokenId>,
}

#[derive(Debug)]
pub struct RefreshIssue {
    pub plaintext: SecretToken,
    pub session: SessionRecord,
    pub family: RefreshFamily,
    pub token: RefreshTokenRecord,
}

#[derive(Debug)]
pub enum RefreshRotation {
    Rotated {
        plaintext: SecretToken,
        token: Box<RefreshTokenRecord>,
        /// Trusted identity/session context loaded under the same rotation lock.
        family: Box<RefreshFamily>,
    },
    /// A previously consumed token was presented. The whole family and its
    /// backing session have already been revoked when this is returned.
    ReuseDetected {
        family_id: Uuid,
        session_id: SessionId,
    },
    Rejected,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum RefreshStoreError {
    #[error("refresh token service is unavailable")]
    Unavailable,
    #[error("refresh token input is invalid")]
    Invalid,
}

#[async_trait]
pub trait RefreshTokenStore: Send + Sync {
    async fn issue_session(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        now_ms: i64,
    ) -> Result<RefreshIssue, RefreshStoreError>;

    async fn rotate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<RefreshRotation, RefreshStoreError>;

    async fn revoke_session(
        &self,
        session_id: SessionId,
        now_ms: i64,
        reason: &str,
    ) -> Result<bool, RefreshStoreError>;
}

#[derive(Debug, Default)]
struct RefreshState {
    sessions: HashMap<SessionId, SessionRecord>,
    families: HashMap<Uuid, RefreshFamily>,
    tokens: HashMap<TokenId, RefreshTokenRecord>,
    token_by_prefix: HashMap<String, TokenId>,
}

/// Reference implementation of the required atomic refresh state machine.
/// Production PostgreSQL code must preserve the same row-locking semantics.
#[derive(Clone, Debug)]
pub struct InMemoryRefreshStore {
    codec: OpaqueTokenCodec,
    state: Arc<Mutex<RefreshState>>,
}

impl InMemoryRefreshStore {
    pub fn new(pepper: Vec<u8>) -> Result<Self, TokenError> {
        Ok(Self {
            codec: OpaqueTokenCodec::new("refresh", pepper)?,
            state: Arc::new(Mutex::new(RefreshState::default())),
        })
    }

    pub async fn issue_session(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        now_ms: i64,
    ) -> Result<RefreshIssue, RefreshStoreError> {
        let expires_at_ms = now_ms
            .checked_add(REFRESH_TOKEN_TTL_MS)
            .ok_or(RefreshStoreError::Invalid)?;
        let (plaintext, parts) = self.codec.issue().map_err(map_token_error)?;
        let session = SessionRecord {
            id: SessionId::new(),
            project_id,
            user_id,
            expires_at_ms,
            revoked_at_ms: None,
            revoke_reason: None,
        };
        let family = RefreshFamily {
            id: Uuid::now_v7(),
            project_id,
            user_id,
            session_id: session.id,
            revoked_at_ms: None,
            revoke_reason: None,
        };
        let token = RefreshTokenRecord {
            id: TokenId::new(),
            family_id: family.id,
            prefix: parts.prefix,
            digest: parts.digest,
            issued_at_ms: now_ms,
            expires_at_ms,
            used_at_ms: None,
            replaced_by: None,
        };
        let mut state = self.state.lock().await;
        state.sessions.insert(session.id, session.clone());
        state.families.insert(family.id, family.clone());
        state.token_by_prefix.insert(token.prefix.clone(), token.id);
        state.tokens.insert(token.id, token.clone());
        Ok(RefreshIssue {
            plaintext,
            session,
            family,
            token,
        })
    }

    pub async fn rotate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<RefreshRotation, RefreshStoreError> {
        let candidate = match self.codec.parse_and_digest(presented) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(RefreshRotation::Rejected),
        };
        let mut state = self.state.lock().await;
        let Some(token_id) = state.token_by_prefix.get(&candidate.prefix).copied() else {
            return Ok(RefreshRotation::Rejected);
        };
        let Some(current) = state.tokens.get(&token_id).cloned() else {
            return Err(RefreshStoreError::Unavailable);
        };
        if !self.codec.verify_digest(&candidate.digest, &current.digest) {
            return Ok(RefreshRotation::Rejected);
        }
        let Some(family) = state.families.get(&current.family_id).cloned() else {
            return Err(RefreshStoreError::Unavailable);
        };
        let Some(session) = state.sessions.get(&family.session_id).cloned() else {
            return Err(RefreshStoreError::Unavailable);
        };
        if family.revoked_at_ms.is_some()
            || session.revoked_at_ms.is_some()
            || session.expires_at_ms <= now_ms
            || current.expires_at_ms <= now_ms
        {
            return Ok(RefreshRotation::Rejected);
        }

        if current.used_at_ms.is_some() {
            revoke_family_locked(&mut state, family.id, now_ms, "refresh_reuse");
            return Ok(RefreshRotation::ReuseDetected {
                family_id: family.id,
                session_id: family.session_id,
            });
        }

        let (plaintext, next_parts) = self.codec.issue().map_err(map_token_error)?;
        let next = RefreshTokenRecord {
            id: TokenId::new(),
            family_id: family.id,
            prefix: next_parts.prefix,
            digest: next_parts.digest,
            issued_at_ms: now_ms,
            expires_at_ms: current.expires_at_ms,
            used_at_ms: None,
            replaced_by: None,
        };
        let Some(current_mut) = state.tokens.get_mut(&token_id) else {
            return Err(RefreshStoreError::Unavailable);
        };
        current_mut.used_at_ms = Some(now_ms);
        current_mut.replaced_by = Some(next.id);
        state.token_by_prefix.insert(next.prefix.clone(), next.id);
        state.tokens.insert(next.id, next.clone());
        Ok(RefreshRotation::Rotated {
            plaintext,
            token: Box::new(next),
            family: Box::new(family),
        })
    }

    pub async fn revoke_session(
        &self,
        session_id: SessionId,
        now_ms: i64,
        reason: &str,
    ) -> Result<bool, RefreshStoreError> {
        if reason.is_empty() || reason.len() > 64 {
            return Err(RefreshStoreError::Invalid);
        }
        let mut state = self.state.lock().await;
        let family_id = state
            .families
            .values()
            .find(|family| family.session_id == session_id)
            .map(|family| family.id);
        let Some(family_id) = family_id else {
            return Ok(false);
        };
        revoke_family_locked(&mut state, family_id, now_ms, reason);
        Ok(true)
    }

    pub async fn session(&self, session_id: SessionId) -> Option<SessionRecord> {
        self.state.lock().await.sessions.get(&session_id).cloned()
    }
}

#[async_trait]
impl RefreshTokenStore for InMemoryRefreshStore {
    async fn issue_session(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        now_ms: i64,
    ) -> Result<RefreshIssue, RefreshStoreError> {
        InMemoryRefreshStore::issue_session(self, project_id, user_id, now_ms).await
    }

    async fn rotate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<RefreshRotation, RefreshStoreError> {
        InMemoryRefreshStore::rotate(self, presented, now_ms).await
    }

    async fn revoke_session(
        &self,
        session_id: SessionId,
        now_ms: i64,
        reason: &str,
    ) -> Result<bool, RefreshStoreError> {
        InMemoryRefreshStore::revoke_session(self, session_id, now_ms, reason).await
    }
}

fn revoke_family_locked(state: &mut RefreshState, family_id: Uuid, now_ms: i64, reason: &str) {
    let session_id = if let Some(family) = state.families.get_mut(&family_id) {
        family.revoked_at_ms.get_or_insert(now_ms);
        family
            .revoke_reason
            .get_or_insert_with(|| reason.to_owned());
        Some(family.session_id)
    } else {
        None
    };
    if let Some(session_id) = session_id
        && let Some(session) = state.sessions.get_mut(&session_id)
    {
        session.revoked_at_ms.get_or_insert(now_ms);
        session
            .revoke_reason
            .get_or_insert_with(|| reason.to_owned());
    }
}

fn map_token_error(_: TokenError) -> RefreshStoreError {
    RefreshStoreError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refresh_is_single_use_and_replay_revokes_family() -> Result<(), RefreshStoreError> {
        let store = InMemoryRefreshStore::new(vec![5; 32]).map_err(map_token_error)?;
        let issue = store
            .issue_session(ProjectId::new(), UserId::new(), 1_000)
            .await?;
        let original = issue.plaintext.expose().to_owned();
        let first = store.rotate(&original, 2_000).await?;
        assert!(matches!(first, RefreshRotation::Rotated { .. }));
        let replay = store.rotate(&original, 2_001).await?;
        assert!(matches!(replay, RefreshRotation::ReuseDetected { .. }));
        let session = store.session(issue.session.id).await;
        assert!(session.is_some_and(|session| session.revoked_at_ms == Some(2_001)));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_rotation_has_one_winner_and_revokes_on_replay()
    -> Result<(), RefreshStoreError> {
        let store = InMemoryRefreshStore::new(vec![8; 32]).map_err(map_token_error)?;
        let issue = store
            .issue_session(ProjectId::new(), UserId::new(), 1_000)
            .await?;
        let token = Arc::new(issue.plaintext.expose().to_owned());
        let first_store = store.clone();
        let first_token = Arc::clone(&token);
        let first = tokio::spawn(async move { first_store.rotate(&first_token, 2_000).await });
        let second_store = store.clone();
        let second_token = Arc::clone(&token);
        let second = tokio::spawn(async move { second_store.rotate(&second_token, 2_000).await });
        let first = first.await.map_err(|_| RefreshStoreError::Unavailable)??;
        let second = second.await.map_err(|_| RefreshStoreError::Unavailable)??;
        let rotations = usize::from(matches!(first, RefreshRotation::Rotated { .. }))
            + usize::from(matches!(second, RefreshRotation::Rotated { .. }));
        let reuses = usize::from(matches!(first, RefreshRotation::ReuseDetected { .. }))
            + usize::from(matches!(second, RefreshRotation::ReuseDetected { .. }));
        assert_eq!(rotations, 1);
        assert_eq!(reuses, 1);
        Ok(())
    }
}
