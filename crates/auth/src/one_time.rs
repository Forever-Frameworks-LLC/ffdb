use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ffdb_protocol::{ProjectId, TokenId, UserId};
use tokio::sync::Mutex;

use crate::{CredentialDigest, OpaqueTokenCodec, SecretToken, TokenError};

pub const ONE_TIME_TOKEN_TTL_MS: i64 = 30 * 60 * 1000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OneTimePurpose {
    EmailVerification,
    PasswordReset,
}

#[derive(Clone, Debug)]
pub struct OneTimeTokenRecord {
    pub id: TokenId,
    pub project_id: ProjectId,
    pub user_id: UserId,
    pub purpose: OneTimePurpose,
    pub prefix: String,
    pub digest: CredentialDigest,
    pub expires_at_ms: i64,
    pub consumed_at_ms: Option<i64>,
}

#[derive(Debug)]
pub struct OneTimeToken {
    pub plaintext: SecretToken,
    pub record: OneTimeTokenRecord,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum OneTimeStoreError {
    #[error("one-time token is invalid or expired")]
    Rejected,
    #[error("one-time token service is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait OneTimeTokenStore: Send + Sync {
    async fn issue(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<OneTimeToken, OneTimeStoreError>;

    async fn consume(
        &self,
        plaintext: &str,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<OneTimeTokenRecord, OneTimeStoreError>;
}

#[derive(Debug, Default)]
struct OneTimeState {
    tokens: HashMap<TokenId, OneTimeTokenRecord>,
    by_prefix: HashMap<String, TokenId>,
}

#[derive(Clone, Debug)]
pub struct InMemoryOneTimeTokenStore {
    codec: OpaqueTokenCodec,
    state: Arc<Mutex<OneTimeState>>,
}

impl InMemoryOneTimeTokenStore {
    pub fn new(pepper: Vec<u8>) -> Result<Self, TokenError> {
        Ok(Self {
            codec: OpaqueTokenCodec::new("action", pepper)?,
            state: Arc::new(Mutex::new(OneTimeState::default())),
        })
    }
}

#[async_trait]
impl OneTimeTokenStore for InMemoryOneTimeTokenStore {
    async fn issue(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<OneTimeToken, OneTimeStoreError> {
        let expires_at_ms = now_ms
            .checked_add(ONE_TIME_TOKEN_TTL_MS)
            .ok_or(OneTimeStoreError::Unavailable)?;
        let (plaintext, parts) = self
            .codec
            .issue()
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        let mut state = self.state.lock().await;
        // Issuance supersedes prior unconsumed tokens for the same action.
        for record in state.tokens.values_mut().filter(|record| {
            record.project_id == project_id
                && record.user_id == user_id
                && record.purpose == purpose
                && record.consumed_at_ms.is_none()
        }) {
            record.consumed_at_ms = Some(now_ms);
        }
        let record = OneTimeTokenRecord {
            id: TokenId::new(),
            project_id,
            user_id,
            purpose,
            prefix: parts.prefix,
            digest: parts.digest,
            expires_at_ms,
            consumed_at_ms: None,
        };
        state.by_prefix.insert(record.prefix.clone(), record.id);
        state.tokens.insert(record.id, record.clone());
        Ok(OneTimeToken { plaintext, record })
    }

    async fn consume(
        &self,
        plaintext: &str,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<OneTimeTokenRecord, OneTimeStoreError> {
        let candidate = self
            .codec
            .parse_and_digest(plaintext)
            .map_err(|_| OneTimeStoreError::Rejected)?;
        let mut state = self.state.lock().await;
        let token_id = state
            .by_prefix
            .get(&candidate.prefix)
            .copied()
            .ok_or(OneTimeStoreError::Rejected)?;
        let record = state
            .tokens
            .get_mut(&token_id)
            .ok_or(OneTimeStoreError::Unavailable)?;
        if record.purpose != purpose
            || record.consumed_at_ms.is_some()
            || record.expires_at_ms <= now_ms
            || !self.codec.verify_digest(&candidate.digest, &record.digest)
        {
            return Err(OneTimeStoreError::Rejected);
        }
        record.consumed_at_ms = Some(now_ms);
        Ok(record.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_is_purpose_bound_and_single_use() -> Result<(), OneTimeStoreError> {
        let store = InMemoryOneTimeTokenStore::new(vec![3; 32])
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        let issue = store
            .issue(
                ProjectId::new(),
                UserId::new(),
                OneTimePurpose::PasswordReset,
                1,
            )
            .await?;
        assert!(matches!(
            store
                .consume(
                    issue.plaintext.expose(),
                    OneTimePurpose::EmailVerification,
                    2
                )
                .await,
            Err(OneTimeStoreError::Rejected)
        ));
        assert!(
            store
                .consume(issue.plaintext.expose(), OneTimePurpose::PasswordReset, 2)
                .await
                .is_ok()
        );
        assert!(matches!(
            store
                .consume(issue.plaintext.expose(), OneTimePurpose::PasswordReset, 3)
                .await,
            Err(OneTimeStoreError::Rejected)
        ));
        Ok(())
    }
}
