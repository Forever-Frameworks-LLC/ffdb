use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use ffdb_protocol::{ProjectId, UserId};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::{
    OneTimePurpose, OneTimeStoreError, OneTimeToken, OneTimeTokenStore, PasswordError,
    PasswordHash, PasswordHasher, SecretString, VerifyOutcome,
};

#[derive(Clone, Debug)]
pub struct AuthUserRecord {
    pub id: UserId,
    pub project_id: ProjectId,
    pub normalized_email: String,
    pub password_hash: PasswordHash,
    pub role: String,
    pub custom_claims: Map<String, Value>,
    pub email_verified_at_ms: Option<i64>,
    pub disabled_at_ms: Option<i64>,
    pub password_changed_at_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedUser {
    pub id: UserId,
    pub project_id: ProjectId,
    pub normalized_email: String,
    pub role: String,
    pub custom_claims: Map<String, Value>,
    pub email_verified: bool,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum AccountError {
    #[error("email address is invalid")]
    InvalidEmail,
    #[error("email address is already registered")]
    EmailInUse,
    #[error("credentials are invalid")]
    InvalidCredentials,
    #[error("custom claims are invalid")]
    InvalidClaims,
    #[error("email verification is required")]
    VerificationRequired,
    #[error("account is disabled")]
    Disabled,
    #[error("account was not found")]
    NotFound,
    #[error("account service is unavailable")]
    Unavailable,
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error(transparent)]
    OneTime(#[from] OneTimeStoreError),
}

#[async_trait]
pub trait AccountRepository: Send + Sync {
    async fn insert(&self, user: AuthUserRecord) -> Result<(), AccountError>;
    async fn find_by_email(
        &self,
        project_id: ProjectId,
        normalized_email: &str,
    ) -> Result<Option<AuthUserRecord>, AccountError>;
    async fn find_by_id(
        &self,
        project_id: ProjectId,
        user_id: UserId,
    ) -> Result<Option<AuthUserRecord>, AccountError>;
    async fn set_verified(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        at_ms: i64,
    ) -> Result<bool, AccountError>;
    async fn set_password(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        hash: PasswordHash,
        at_ms: i64,
    ) -> Result<bool, AccountError>;
    async fn disable(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        at_ms: i64,
    ) -> Result<bool, AccountError>;
}

#[derive(Debug, Default)]
struct AccountState {
    by_id: HashMap<(ProjectId, UserId), AuthUserRecord>,
    by_email: HashMap<(ProjectId, String), UserId>,
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryAccountRepository {
    state: Arc<Mutex<AccountState>>,
}

#[async_trait]
impl AccountRepository for InMemoryAccountRepository {
    async fn insert(&self, user: AuthUserRecord) -> Result<(), AccountError> {
        let mut state = self.state.lock().await;
        let email_key = (user.project_id, user.normalized_email.clone());
        if state.by_email.contains_key(&email_key) {
            return Err(AccountError::EmailInUse);
        }
        state.by_email.insert(email_key, user.id);
        state.by_id.insert((user.project_id, user.id), user);
        Ok(())
    }

    async fn find_by_email(
        &self,
        project_id: ProjectId,
        normalized_email: &str,
    ) -> Result<Option<AuthUserRecord>, AccountError> {
        let state = self.state.lock().await;
        let Some(user_id) = state
            .by_email
            .get(&(project_id, normalized_email.to_owned()))
            .copied()
        else {
            return Ok(None);
        };
        Ok(state.by_id.get(&(project_id, user_id)).cloned())
    }

    async fn find_by_id(
        &self,
        project_id: ProjectId,
        user_id: UserId,
    ) -> Result<Option<AuthUserRecord>, AccountError> {
        Ok(self
            .state
            .lock()
            .await
            .by_id
            .get(&(project_id, user_id))
            .cloned())
    }

    async fn set_verified(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        at_ms: i64,
    ) -> Result<bool, AccountError> {
        let mut state = self.state.lock().await;
        let Some(user) = state.by_id.get_mut(&(project_id, user_id)) else {
            return Ok(false);
        };
        user.email_verified_at_ms.get_or_insert(at_ms);
        Ok(true)
    }

    async fn set_password(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        hash: PasswordHash,
        at_ms: i64,
    ) -> Result<bool, AccountError> {
        let mut state = self.state.lock().await;
        let Some(user) = state.by_id.get_mut(&(project_id, user_id)) else {
            return Ok(false);
        };
        user.password_hash = hash;
        user.password_changed_at_ms = at_ms;
        Ok(true)
    }

    async fn disable(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        at_ms: i64,
    ) -> Result<bool, AccountError> {
        let mut state = self.state.lock().await;
        let Some(user) = state.by_id.get_mut(&(project_id, user_id)) else {
            return Ok(false);
        };
        user.disabled_at_ms.get_or_insert(at_ms);
        Ok(true)
    }
}

pub struct AccountService {
    project_id: ProjectId,
    repository: Arc<dyn AccountRepository>,
    password_hasher: Arc<dyn PasswordHasher>,
    one_time_tokens: Arc<dyn OneTimeTokenStore>,
    dummy_hash: PasswordHash,
}

impl fmt::Debug for AccountService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountService")
            .field("project_id", &self.project_id)
            .field("repository", &"dyn AccountRepository")
            .field("password_hasher", &"dyn PasswordHasher")
            .field("one_time_tokens", &"dyn OneTimeTokenStore")
            .field("dummy_hash", &"[REDACTED]")
            .finish()
    }
}

impl AccountService {
    pub fn new(
        project_id: ProjectId,
        repository: Arc<dyn AccountRepository>,
        password_hasher: Arc<dyn PasswordHasher>,
        one_time_tokens: Arc<dyn OneTimeTokenStore>,
    ) -> Result<Self, AccountError> {
        // Used for unknown-email verification so the password KDF still runs.
        let dummy_hash = password_hasher.hash(SecretString::new(
            "ffdb-dummy-password-verification-work-factor".into(),
        ))?;
        Ok(Self {
            project_id,
            repository,
            password_hasher,
            one_time_tokens,
            dummy_hash,
        })
    }

    /// Construct with a process-cached dummy hash. This avoids running an
    /// additional Argon2 hash merely to create a request-scoped project service
    /// while preserving equal KDF work for unknown-email authentication.
    pub fn with_dummy_hash(
        project_id: ProjectId,
        repository: Arc<dyn AccountRepository>,
        password_hasher: Arc<dyn PasswordHasher>,
        one_time_tokens: Arc<dyn OneTimeTokenStore>,
        dummy_hash: PasswordHash,
    ) -> Self {
        Self {
            project_id,
            repository,
            password_hasher,
            one_time_tokens,
            dummy_hash,
        }
    }

    pub async fn register(
        &self,
        email: &str,
        password: SecretString,
        now_ms: i64,
    ) -> Result<AuthUserRecord, AccountError> {
        self.register_with_claims(email, password, Map::new(), now_ms)
            .await
    }

    pub async fn register_with_claims(
        &self,
        email: &str,
        password: SecretString,
        custom_claims: Map<String, Value>,
        now_ms: i64,
    ) -> Result<AuthUserRecord, AccountError> {
        self.register_with_policy(email, password, custom_claims, true, 8, now_ms)
            .await
    }

    pub async fn register_with_policy(
        &self,
        email: &str,
        password: SecretString,
        custom_claims: Map<String, Value>,
        email_verification_required: bool,
        password_min_length: u16,
        now_ms: i64,
    ) -> Result<AuthUserRecord, AccountError> {
        if !(8..=128).contains(&password_min_length)
            || password.expose().len() < usize::from(password_min_length)
        {
            return Err(AccountError::Password(PasswordError::Policy));
        }
        let normalized_email = normalize_email(email)?;
        validate_custom_claims(&custom_claims)?;
        let record = AuthUserRecord {
            id: UserId::new(),
            project_id: self.project_id,
            normalized_email,
            password_hash: self.password_hasher.hash(password)?,
            // Public registration can never choose a privileged role.
            role: "authenticated".into(),
            custom_claims,
            email_verified_at_ms: (!email_verification_required).then_some(now_ms),
            disabled_at_ms: None,
            password_changed_at_ms: now_ms,
            created_at_ms: now_ms,
        };
        self.repository.insert(record.clone()).await?;
        Ok(record)
    }

    pub async fn authenticate(
        &self,
        email: &str,
        password: SecretString,
    ) -> Result<AuthenticatedUser, AccountError> {
        self.authenticate_with_verification_policy(email, password, true)
            .await
    }

    pub async fn authenticate_with_verification_policy(
        &self,
        email: &str,
        password: SecretString,
        email_verification_required: bool,
    ) -> Result<AuthenticatedUser, AccountError> {
        let normalized_email =
            normalize_email(email).map_err(|_| AccountError::InvalidCredentials)?;
        let record = self
            .repository
            .find_by_email(self.project_id, &normalized_email)
            .await?;
        let Some(record) = record else {
            let _ = self.password_hasher.verify(password, &self.dummy_hash)?;
            return Err(AccountError::InvalidCredentials);
        };
        if !matches!(
            self.password_hasher
                .verify(password, &record.password_hash)?,
            VerifyOutcome::Valid | VerifyOutcome::ValidNeedsRehash
        ) {
            return Err(AccountError::InvalidCredentials);
        }
        // Status is disclosed only after possession of the correct password.
        if record.disabled_at_ms.is_some() {
            return Err(AccountError::Disabled);
        }
        if email_verification_required && record.email_verified_at_ms.is_none() {
            return Err(AccountError::VerificationRequired);
        }
        Ok(AuthenticatedUser {
            id: record.id,
            project_id: record.project_id,
            normalized_email: record.normalized_email,
            role: record.role,
            custom_claims: record.custom_claims,
            email_verified: record.email_verified_at_ms.is_some(),
            created_at_ms: record.created_at_ms,
        })
    }

    pub async fn issue_verification(
        &self,
        user_id: UserId,
        now_ms: i64,
    ) -> Result<OneTimeToken, AccountError> {
        let user = self
            .repository
            .find_by_id(self.project_id, user_id)
            .await?
            .ok_or(AccountError::NotFound)?;
        if user.disabled_at_ms.is_some() {
            return Err(AccountError::Disabled);
        }
        Ok(self
            .one_time_tokens
            .issue(
                self.project_id,
                user_id,
                OneTimePurpose::EmailVerification,
                now_ms,
            )
            .await?)
    }

    /// Issue a verification credential without revealing whether the submitted
    /// address exists. The caller must keep the outward response identical for
    /// `Some` and `None`.
    pub async fn issue_verification_for_email(
        &self,
        normalized_or_raw_email: &str,
        now_ms: i64,
    ) -> Result<Option<OneTimeToken>, AccountError> {
        let normalized = normalize_email(normalized_or_raw_email)?;
        let Some(user) = self
            .repository
            .find_by_email(self.project_id, &normalized)
            .await?
        else {
            return Ok(None);
        };
        if user.disabled_at_ms.is_some() {
            return Ok(None);
        }
        self.issue_verification(user.id, now_ms).await.map(Some)
    }

    pub async fn verify_email(&self, token: &str, now_ms: i64) -> Result<UserId, AccountError> {
        let consumed = self
            .one_time_tokens
            .consume(token, OneTimePurpose::EmailVerification, now_ms)
            .await?;
        if consumed.project_id != self.project_id
            || !self
                .repository
                .set_verified(self.project_id, consumed.user_id, now_ms)
                .await?
        {
            return Err(AccountError::NotFound);
        }
        Ok(consumed.user_id)
    }

    pub async fn issue_password_reset(
        &self,
        normalized_or_raw_email: &str,
        now_ms: i64,
    ) -> Result<Option<OneTimeToken>, AccountError> {
        let normalized = normalize_email(normalized_or_raw_email)?;
        let user = self
            .repository
            .find_by_email(self.project_id, &normalized)
            .await?;
        let Some(user) = user else {
            // Callers must return the same HTTP response for Some and None.
            return Ok(None);
        };
        if user.disabled_at_ms.is_some() {
            return Ok(None);
        }
        Ok(Some(
            self.one_time_tokens
                .issue(
                    self.project_id,
                    user.id,
                    OneTimePurpose::PasswordReset,
                    now_ms,
                )
                .await?,
        ))
    }

    pub async fn reset_password(
        &self,
        token: &str,
        new_password: SecretString,
        now_ms: i64,
    ) -> Result<UserId, AccountError> {
        // Hash before consuming so a transient KDF failure does not burn the token.
        let hash = self.password_hasher.hash(new_password)?;
        let consumed = self
            .one_time_tokens
            .consume(token, OneTimePurpose::PasswordReset, now_ms)
            .await?;
        if consumed.project_id != self.project_id
            || !self
                .repository
                .set_password(self.project_id, consumed.user_id, hash, now_ms)
                .await?
        {
            return Err(AccountError::NotFound);
        }
        Ok(consumed.user_id)
    }

    /// Change a password after proving possession of the current password.
    /// The PostgreSQL repository revokes all refresh families atomically with
    /// the password update, and the access-token password cutoff invalidates
    /// already-issued JWTs.
    pub async fn change_password(
        &self,
        user_id: UserId,
        current_password: SecretString,
        new_password: SecretString,
        now_ms: i64,
    ) -> Result<(), AccountError> {
        let record = self
            .repository
            .find_by_id(self.project_id, user_id)
            .await?
            .ok_or(AccountError::InvalidCredentials)?;
        if record.disabled_at_ms.is_some()
            || !matches!(
                self.password_hasher
                    .verify(current_password, &record.password_hash)?,
                VerifyOutcome::Valid | VerifyOutcome::ValidNeedsRehash
            )
        {
            return Err(AccountError::InvalidCredentials);
        }
        let hash = self.password_hasher.hash(new_password)?;
        if !self
            .repository
            .set_password(self.project_id, user_id, hash, now_ms)
            .await?
        {
            return Err(AccountError::InvalidCredentials);
        }
        Ok(())
    }

    pub async fn disable(&self, user_id: UserId, now_ms: i64) -> Result<(), AccountError> {
        if !self
            .repository
            .disable(self.project_id, user_id, now_ms)
            .await?
        {
            return Err(AccountError::NotFound);
        }
        Ok(())
    }
}

fn normalize_email(email: &str) -> Result<String, AccountError> {
    if email.len() > 254 || email.trim() != email || !email.is_ascii() {
        return Err(AccountError::InvalidEmail);
    }
    let (local, domain) = email.rsplit_once('@').ok_or(AccountError::InvalidEmail)?;
    if local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || !domain.contains('.')
        || email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(AccountError::InvalidEmail);
    }
    Ok(format!(
        "{}@{}",
        local.to_ascii_lowercase(),
        domain.to_ascii_lowercase()
    ))
}

pub(crate) fn validate_custom_claims(claims: &Map<String, Value>) -> Result<(), AccountError> {
    const RESERVED: &[&str] = &[
        "iss",
        "aud",
        "sub",
        "project_id",
        "jti",
        "iat",
        "nbf",
        "exp",
        "role",
    ];
    if claims.len() > 32
        || serde_json::to_vec(claims)
            .map_err(|_| AccountError::InvalidClaims)?
            .len()
            > 4_096
    {
        return Err(AccountError::InvalidClaims);
    }
    for (key, value) in claims {
        if key.is_empty()
            || key.len() > 64
            || RESERVED.contains(&key.as_str())
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !valid_claim_value(value, 0)
        {
            return Err(AccountError::InvalidClaims);
        }
    }
    Ok(())
}

fn valid_claim_value(value: &Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(value) => value.len() <= 1_024,
        Value::Array(values) => {
            values.len() <= 32
                && values
                    .iter()
                    .all(|value| valid_claim_value(value, depth + 1))
        }
        Value::Object(values) => {
            values.len() <= 32
                && values.iter().all(|(key, value)| {
                    !key.is_empty() && key.len() <= 64 && valid_claim_value(value, depth + 1)
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Argon2PasswordHasher, InMemoryOneTimeTokenStore};

    fn service(project_id: ProjectId) -> Result<AccountService, AccountError> {
        let repository: Arc<dyn AccountRepository> = Arc::new(InMemoryAccountRepository::default());
        let tokens: Arc<dyn OneTimeTokenStore> = Arc::new(
            InMemoryOneTimeTokenStore::new(vec![6; 32]).map_err(|_| AccountError::Unavailable)?,
        );
        AccountService::new(
            project_id,
            repository,
            Arc::new(Argon2PasswordHasher::new(8 * 1024, 1, 1, 1024)?),
            tokens,
        )
    }

    #[tokio::test]
    async fn registration_requires_verification_and_disable_blocks_login()
    -> Result<(), AccountError> {
        let service = service(ProjectId::new())?;
        let user = service
            .register(
                "Person@Example.test",
                SecretString::new("long password".into()),
                1,
            )
            .await?;
        assert_eq!(
            service
                .authenticate(
                    "person@example.test",
                    SecretString::new("long password".into())
                )
                .await,
            Err(AccountError::VerificationRequired)
        );
        let token = service.issue_verification(user.id, 2).await?;
        service.verify_email(token.plaintext.expose(), 3).await?;
        assert!(
            service
                .authenticate(
                    "person@example.test",
                    SecretString::new("long password".into())
                )
                .await
                .is_ok()
        );
        service.disable(user.id, 4).await?;
        assert_eq!(
            service
                .authenticate(
                    "person@example.test",
                    SecretString::new("long password".into())
                )
                .await,
            Err(AccountError::Disabled)
        );
        Ok(())
    }

    #[tokio::test]
    async fn registration_policy_controls_minimum_and_verification() -> Result<(), AccountError> {
        let service = service(ProjectId::new())?;
        assert!(matches!(
            service
                .register_with_policy(
                    "short@example.test",
                    SecretString::new("too short".into()),
                    Map::new(),
                    false,
                    12,
                    1,
                )
                .await,
            Err(AccountError::Password(PasswordError::Policy))
        ));
        let user = service
            .register_with_policy(
                "direct@example.test",
                SecretString::new("longer password".into()),
                Map::new(),
                false,
                12,
                2,
            )
            .await?;
        assert!(user.email_verified_at_ms.is_some());
        assert!(
            service
                .authenticate_with_verification_policy(
                    "direct@example.test",
                    SecretString::new("longer password".into()),
                    false,
                )
                .await
                .is_ok()
        );
        Ok(())
    }

    #[tokio::test]
    async fn reset_is_single_use_and_changes_password() -> Result<(), AccountError> {
        let service = service(ProjectId::new())?;
        let user = service
            .register(
                "person@example.test",
                SecretString::new("old password".into()),
                1,
            )
            .await?;
        let verification = service.issue_verification(user.id, 2).await?;
        service
            .verify_email(verification.plaintext.expose(), 3)
            .await?;
        let Some(reset) = service
            .issue_password_reset("person@example.test", 4)
            .await?
        else {
            return Err(AccountError::Unavailable);
        };
        service
            .reset_password(
                reset.plaintext.expose(),
                SecretString::new("new password".into()),
                5,
            )
            .await?;
        assert_eq!(
            service
                .reset_password(
                    reset.plaintext.expose(),
                    SecretString::new("other password".into()),
                    6
                )
                .await,
            Err(AccountError::OneTime(OneTimeStoreError::Rejected))
        );
        assert!(
            service
                .authenticate(
                    "person@example.test",
                    SecretString::new("new password".into())
                )
                .await
                .is_ok()
        );
        Ok(())
    }

    #[tokio::test]
    async fn password_change_requires_current_password() -> Result<(), AccountError> {
        let service = service(ProjectId::new())?;
        let user = service
            .register(
                "change@example.test",
                SecretString::new("old password".into()),
                1,
            )
            .await?;
        let verification = service.issue_verification(user.id, 2).await?;
        service
            .verify_email(verification.plaintext.expose(), 3)
            .await?;
        assert_eq!(
            service
                .change_password(
                    user.id,
                    SecretString::new("wrong password".into()),
                    SecretString::new("new password".into()),
                    4,
                )
                .await,
            Err(AccountError::InvalidCredentials)
        );
        service
            .change_password(
                user.id,
                SecretString::new("old password".into()),
                SecretString::new("new password".into()),
                5,
            )
            .await?;
        assert_eq!(
            service
                .authenticate(
                    "change@example.test",
                    SecretString::new("old password".into())
                )
                .await,
            Err(AccountError::InvalidCredentials)
        );
        assert!(
            service
                .authenticate(
                    "change@example.test",
                    SecretString::new("new password".into())
                )
                .await
                .is_ok()
        );
        Ok(())
    }

    #[tokio::test]
    async fn registration_bounds_custom_claims_and_fixes_public_role() -> Result<(), AccountError> {
        let service = service(ProjectId::new())?;
        let claims = Map::from_iter([("plan".into(), Value::String("starter".into()))]);
        let user = service
            .register_with_claims(
                "claims@example.test",
                SecretString::new("long password".into()),
                claims.clone(),
                1,
            )
            .await?;
        assert_eq!(user.role, "authenticated");
        assert_eq!(user.custom_claims, claims);

        let reserved = Map::from_iter([("role".into(), Value::String("admin".into()))]);
        assert!(matches!(
            service
                .register_with_claims(
                    "reserved@example.test",
                    SecretString::new("long password".into()),
                    reserved,
                    2,
                )
                .await,
            Err(AccountError::InvalidClaims)
        ));
        Ok(())
    }
}
