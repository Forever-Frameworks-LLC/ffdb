//! PostgreSQL persistence for security-sensitive auth state.
//!
//! Refresh rotation and one-time consumption use row locks. Password changes and
//! account disabling revoke every backing session in the same transaction.

use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;
use ffdb_protocol::{
    ApiKeyId, DeveloperScope, OrganizationId, ProjectId, SessionId, TokenId, UserId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    AccountError, AccountRepository, ApiKeyRecord, AuthUserRecord, CredentialDigest, JwtError,
    OneTimePurpose, OneTimeStoreError, OneTimeToken, OneTimeTokenRecord, OneTimeTokenStore,
    OpaqueTokenCodec, PasswordHash, ProjectSigner, RefreshFamily, RefreshIssue, RefreshRotation,
    RefreshStoreError, RefreshTokenRecord, RefreshTokenStore, SessionRecord, SigningKeyStore,
    TokenError, VerificationKey, VerificationKeyStatus, account::validate_custom_claims,
    refresh::REFRESH_TOKEN_TTL_MS,
};

#[async_trait]
pub trait ApiKeyRepository: Send + Sync {
    async fn insert(
        &self,
        record: &ApiKeyRecord,
        name: &str,
        created_by: UserId,
        created_at_ms: i64,
    ) -> Result<(), AccountError>;

    async fn find_by_prefix(&self, prefix: &str) -> Result<Option<ApiKeyRecord>, AccountError>;

    async fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<OrganizationId>, AccountError>;

    async fn revoke(&self, id: ApiKeyId, now_ms: i64) -> Result<bool, AccountError>;
}

#[derive(Clone, Debug)]
pub struct PgApiKeyRepository {
    pool: PgPool,
}

impl PgApiKeyRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ApiKeyRepository for PgApiKeyRepository {
    async fn insert(
        &self,
        record: &ApiKeyRecord,
        name: &str,
        created_by: UserId,
        created_at_ms: i64,
    ) -> Result<(), AccountError> {
        if name.is_empty() || name.len() > 128 || record.scopes.is_empty() {
            return Err(AccountError::Unavailable);
        }
        let scopes: Vec<&str> = record.scopes.iter().copied().map(scope_name).collect();
        sqlx::query(
            "INSERT INTO api_keys \
             (id,organization_id,project_id,name,lookup_prefix,keyed_hash,scopes,created_by,expires_at,revoked_at,created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,to_timestamp($9::double precision/1000), \
                     to_timestamp($10::double precision/1000),to_timestamp($11::double precision/1000))",
        )
        .bind(record.id.0)
        .bind(record.organization_id.0)
        .bind(record.project_id.map(|id| id.0))
        .bind(name)
        .bind(&record.prefix)
        .bind(record.digest.as_bytes().as_slice())
        .bind(scopes)
        .bind(created_by.0)
        .bind(record.expires_at_ms)
        .bind(record.revoked_at_ms)
        .bind(created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|_| AccountError::Unavailable)?;
        Ok(())
    }

    async fn find_by_prefix(&self, prefix: &str) -> Result<Option<ApiKeyRecord>, AccountError> {
        let row = sqlx::query(
            "SELECT id,organization_id,project_id,lookup_prefix,keyed_hash,scopes, \
                    (extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms, \
                    (extract(epoch FROM revoked_at)*1000)::bigint AS revoked_at_ms \
             FROM api_keys WHERE lookup_prefix=$1",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        row.map(api_key_from_row).transpose()
    }

    async fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<OrganizationId>, AccountError> {
        let organization_id: Option<Uuid> =
            sqlx::query_scalar("SELECT organization_id FROM projects WHERE id=$1")
                .bind(project_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_account_sqlx)?;
        Ok(organization_id.map(OrganizationId))
    }

    async fn revoke(&self, id: ApiKeyId, now_ms: i64) -> Result<bool, AccountError> {
        let result = sqlx::query(
            "UPDATE api_keys SET revoked_at=COALESCE(revoked_at,to_timestamp($2::double precision/1000)) \
             WHERE id=$1",
        )
        .bind(id.0)
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        Ok(result.rows_affected() == 1)
    }
}

#[derive(Clone, Debug)]
pub struct PgAccountRepository {
    pool: PgPool,
}

impl PgAccountRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Return project-scoped account records for trusted administration code.
    /// Password hashes remain internal to the auth crate and callers must map
    /// records to a safe response type before serialization.
    pub async fn list_project_users(
        &self,
        project_id: ProjectId,
        limit: i64,
    ) -> Result<Vec<AuthUserRecord>, AccountError> {
        if !(1..=1_000).contains(&limit) {
            return Err(AccountError::InvalidClaims);
        }
        let rows = sqlx::query(
            "SELECT id,project_id,email,password_phc,role,custom_claims, \
                    (extract(epoch FROM email_verified_at)*1000)::bigint AS email_verified_at_ms, \
                    (extract(epoch FROM disabled_at)*1000)::bigint AS disabled_at_ms, \
                    (extract(epoch FROM password_changed_at)*1000)::bigint AS password_changed_at_ms, \
                    (extract(epoch FROM created_at)*1000)::bigint AS created_at_ms \
             FROM auth_users WHERE project_id=$1 ORDER BY created_at,id LIMIT $2",
        )
        .bind(project_id.0)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        rows.into_iter().map(account_from_row).collect()
    }

    /// Enable or disable a project account. Disabling and session revocation
    /// are committed atomically so no refresh family survives the state change.
    pub async fn set_disabled_for_project(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        disabled: bool,
        now_ms: i64,
    ) -> Result<bool, AccountError> {
        let mut transaction = self.pool.begin().await.map_err(map_account_sqlx)?;
        let result = if disabled {
            sqlx::query(
                "UPDATE auth_users SET disabled_at=COALESCE(disabled_at, \
                 to_timestamp($3::double precision/1000)), \
                 password_changed_at=GREATEST(password_changed_at,to_timestamp($3::double precision/1000)), \
                 updated_at=now() \
                 WHERE project_id=$1 AND id=$2",
            )
            .bind(project_id.0)
            .bind(user_id.0)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await
            .map_err(map_account_sqlx)?
        } else {
            sqlx::query(
                "UPDATE auth_users SET disabled_at=NULL, \
                 password_changed_at=GREATEST(password_changed_at,to_timestamp($3::double precision/1000)), \
                 updated_at=now() \
                 WHERE project_id=$1 AND id=$2",
            )
            .bind(project_id.0)
            .bind(user_id.0)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await
            .map_err(map_account_sqlx)?
        };
        if disabled && result.rows_affected() == 1 {
            revoke_user_sessions(
                &mut transaction,
                project_id,
                user_id,
                now_ms,
                "account_disabled",
            )
            .await?;
        }
        transaction.commit().await.map_err(map_account_sqlx)?;
        Ok(result.rows_affected() == 1)
    }
}

#[async_trait]
impl AccountRepository for PgAccountRepository {
    async fn insert(&self, user: AuthUserRecord) -> Result<(), AccountError> {
        sqlx::query(
            "INSERT INTO auth_users \
             (id,project_id,email,password_phc,role,custom_claims,email_verified_at,disabled_at,password_changed_at,created_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,to_timestamp($7::double precision/1000),to_timestamp($8::double precision/1000), \
                     to_timestamp($9::double precision/1000),to_timestamp($10::double precision/1000),to_timestamp($10::double precision/1000))",
        )
        .bind(user.id.0)
        .bind(user.project_id.0)
        .bind(user.normalized_email)
        .bind(user.password_hash.as_phc())
        .bind(&user.role)
        .bind(sqlx::types::Json(&user.custom_claims))
        .bind(user.email_verified_at_ms)
        .bind(user.disabled_at_ms)
        .bind(user.password_changed_at_ms)
        .bind(user.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        Ok(())
    }

    async fn find_by_email(
        &self,
        project_id: ProjectId,
        normalized_email: &str,
    ) -> Result<Option<AuthUserRecord>, AccountError> {
        let row = sqlx::query(
            "SELECT id,project_id,email,password_phc,role,custom_claims, \
                    (extract(epoch FROM email_verified_at)*1000)::bigint AS email_verified_at_ms, \
                    (extract(epoch FROM disabled_at)*1000)::bigint AS disabled_at_ms, \
                    (extract(epoch FROM password_changed_at)*1000)::bigint AS password_changed_at_ms, \
                    (extract(epoch FROM created_at)*1000)::bigint AS created_at_ms \
             FROM auth_users WHERE project_id=$1 AND lower(email)=lower($2)",
        )
        .bind(project_id.0)
        .bind(normalized_email)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        row.map(account_from_row).transpose()
    }

    async fn find_by_id(
        &self,
        project_id: ProjectId,
        user_id: UserId,
    ) -> Result<Option<AuthUserRecord>, AccountError> {
        let row = sqlx::query(
            "SELECT id,project_id,email,password_phc,role,custom_claims, \
                    (extract(epoch FROM email_verified_at)*1000)::bigint AS email_verified_at_ms, \
                    (extract(epoch FROM disabled_at)*1000)::bigint AS disabled_at_ms, \
                    (extract(epoch FROM password_changed_at)*1000)::bigint AS password_changed_at_ms, \
                    (extract(epoch FROM created_at)*1000)::bigint AS created_at_ms \
             FROM auth_users WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        row.map(account_from_row).transpose()
    }

    async fn set_verified(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        at_ms: i64,
    ) -> Result<bool, AccountError> {
        let result = sqlx::query(
            "UPDATE auth_users SET email_verified_at=COALESCE(email_verified_at,to_timestamp($3::double precision/1000)), \
                    updated_at=now() WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(at_ms)
        .execute(&self.pool)
        .await
        .map_err(map_account_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    async fn set_password(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        hash: PasswordHash,
        at_ms: i64,
    ) -> Result<bool, AccountError> {
        let mut transaction = self.pool.begin().await.map_err(map_account_sqlx)?;
        let result = sqlx::query(
            "UPDATE auth_users SET password_phc=$3,password_changed_at=to_timestamp($4::double precision/1000),updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND disabled_at IS NULL",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(hash.as_phc())
        .bind(at_ms)
        .execute(&mut *transaction)
        .await
        .map_err(map_account_sqlx)?;
        if result.rows_affected() == 1 {
            revoke_user_sessions(
                &mut transaction,
                project_id,
                user_id,
                at_ms,
                "password_changed",
            )
            .await?;
        }
        transaction.commit().await.map_err(map_account_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    async fn disable(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        at_ms: i64,
    ) -> Result<bool, AccountError> {
        let mut transaction = self.pool.begin().await.map_err(map_account_sqlx)?;
        let result = sqlx::query(
            "UPDATE auth_users SET disabled_at=COALESCE(disabled_at,to_timestamp($3::double precision/1000)),updated_at=now() \
             WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(at_ms)
        .execute(&mut *transaction)
        .await
        .map_err(map_account_sqlx)?;
        if result.rows_affected() == 1 {
            revoke_user_sessions(
                &mut transaction,
                project_id,
                user_id,
                at_ms,
                "account_disabled",
            )
            .await?;
        }
        transaction.commit().await.map_err(map_account_sqlx)?;
        Ok(result.rows_affected() == 1)
    }
}

#[derive(Clone, Debug)]
pub struct PgOneTimeTokenStore {
    pool: PgPool,
    codec: OpaqueTokenCodec,
}

impl PgOneTimeTokenStore {
    pub fn new(pool: PgPool, pepper: Vec<u8>) -> Result<Self, TokenError> {
        Ok(Self {
            pool,
            codec: OpaqueTokenCodec::new("action", pepper)?,
        })
    }

    /// Identify the trusted user bound to a still-active one-time credential
    /// without consuming it. Handlers use this only for admission control and
    /// mutation-intent audit immediately before the atomic consume operation.
    pub async fn identify_for_project(
        &self,
        project_id: ProjectId,
        plaintext: &str,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<Option<UserId>, OneTimeStoreError> {
        let candidate = match self.codec.parse_and_digest(plaintext) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(None),
        };
        let row = sqlx::query(
            "SELECT id,project_id,user_id,purpose,lookup_prefix,keyed_hash, \
                    (extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms, \
                    (extract(epoch FROM consumed_at)*1000)::bigint AS consumed_at_ms \
             FROM auth_one_time_tokens WHERE lookup_prefix=$1",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let record = one_time_from_row(row).map_err(|_| OneTimeStoreError::Unavailable)?;
        if record.project_id != project_id
            || record.purpose != purpose
            || record.consumed_at_ms.is_some()
            || record.expires_at_ms <= now_ms
            || !self.codec.verify_digest(&candidate.digest, &record.digest)
        {
            return Ok(None);
        }
        Ok(Some(record.user_id))
    }

    /// Consume only when the verified token belongs to the project selected by
    /// the trusted URL. A valid token presented on another project's endpoint
    /// is rejected without burning it.
    pub async fn consume_for_project(
        &self,
        project_id: ProjectId,
        plaintext: &str,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<OneTimeTokenRecord, OneTimeStoreError> {
        let candidate = self
            .codec
            .parse_and_digest(plaintext)
            .map_err(|_| OneTimeStoreError::Rejected)?;
        let row = sqlx::query(
            "SELECT project_id,keyed_hash FROM auth_one_time_tokens WHERE lookup_prefix=$1",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?
        .ok_or(OneTimeStoreError::Rejected)?;
        let stored_project = ProjectId(
            row.try_get("project_id")
                .map_err(|_| OneTimeStoreError::Unavailable)?,
        );
        let digest = CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| OneTimeStoreError::Unavailable)?,
        )
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        if stored_project != project_id || !self.codec.verify_digest(&candidate.digest, &digest) {
            return Err(OneTimeStoreError::Rejected);
        }
        self.consume(plaintext, purpose, now_ms).await
    }

    /// Consume an email-verification credential and update the account in one
    /// transaction. A datastore failure cannot burn the one-time token without
    /// applying the account mutation.
    pub async fn verify_email_for_project(
        &self,
        project_id: ProjectId,
        plaintext: &str,
        now_ms: i64,
    ) -> Result<UserId, OneTimeStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        let record = lock_one_time_token(
            &mut transaction,
            &self.codec,
            project_id,
            plaintext,
            OneTimePurpose::EmailVerification,
            now_ms,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE auth_users SET email_verified_at=COALESCE(email_verified_at, \
             to_timestamp($3::double precision/1000)),updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND disabled_at IS NULL",
        )
        .bind(project_id.0)
        .bind(record.user_id.0)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(OneTimeStoreError::Rejected);
        }
        consume_one_time_token(&mut transaction, record.id, now_ms).await?;
        transaction
            .commit()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        Ok(record.user_id)
    }

    /// Consume a reset credential, install the new hash, and revoke every user
    /// session atomically.
    pub async fn reset_password_for_project(
        &self,
        project_id: ProjectId,
        plaintext: &str,
        password_hash: PasswordHash,
        now_ms: i64,
    ) -> Result<UserId, OneTimeStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        let record = lock_one_time_token(
            &mut transaction,
            &self.codec,
            project_id,
            plaintext,
            OneTimePurpose::PasswordReset,
            now_ms,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE auth_users SET password_phc=$3, \
             password_changed_at=to_timestamp($4::double precision/1000),updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND disabled_at IS NULL",
        )
        .bind(project_id.0)
        .bind(record.user_id.0)
        .bind(password_hash.as_phc())
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(OneTimeStoreError::Rejected);
        }
        revoke_user_sessions(
            &mut transaction,
            project_id,
            record.user_id,
            now_ms,
            "password_reset",
        )
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        consume_one_time_token(&mut transaction, record.id, now_ms).await?;
        transaction
            .commit()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        Ok(record.user_id)
    }
}

async fn lock_one_time_token(
    transaction: &mut Transaction<'_, Postgres>,
    codec: &OpaqueTokenCodec,
    project_id: ProjectId,
    plaintext: &str,
    purpose: OneTimePurpose,
    now_ms: i64,
) -> Result<OneTimeTokenRecord, OneTimeStoreError> {
    let candidate = codec
        .parse_and_digest(plaintext)
        .map_err(|_| OneTimeStoreError::Rejected)?;
    let row = sqlx::query(
        "SELECT id,project_id,user_id,purpose,lookup_prefix,keyed_hash, \
                (extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms, \
                (extract(epoch FROM consumed_at)*1000)::bigint AS consumed_at_ms \
         FROM auth_one_time_tokens WHERE lookup_prefix=$1 FOR UPDATE",
    )
    .bind(&candidate.prefix)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| OneTimeStoreError::Unavailable)?
    .ok_or(OneTimeStoreError::Rejected)?;
    let record = one_time_from_row(row).map_err(|_| OneTimeStoreError::Unavailable)?;
    if record.project_id != project_id
        || record.purpose != purpose
        || record.consumed_at_ms.is_some()
        || record.expires_at_ms <= now_ms
        || !codec.verify_digest(&candidate.digest, &record.digest)
    {
        return Err(OneTimeStoreError::Rejected);
    }
    Ok(record)
}

async fn consume_one_time_token(
    transaction: &mut Transaction<'_, Postgres>,
    token_id: TokenId,
    now_ms: i64,
) -> Result<(), OneTimeStoreError> {
    let updated = sqlx::query(
        "UPDATE auth_one_time_tokens SET consumed_at=to_timestamp($2::double precision/1000) \
         WHERE id=$1 AND consumed_at IS NULL",
    )
    .bind(token_id.0)
    .bind(now_ms)
    .execute(&mut **transaction)
    .await
    .map_err(|_| OneTimeStoreError::Unavailable)?;
    if updated.rows_affected() != 1 {
        return Err(OneTimeStoreError::Rejected);
    }
    Ok(())
}

#[async_trait]
impl OneTimeTokenStore for PgOneTimeTokenStore {
    async fn issue(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        purpose: OneTimePurpose,
        now_ms: i64,
    ) -> Result<OneTimeToken, OneTimeStoreError> {
        let expiry = now_ms
            .checked_add(crate::one_time::ONE_TIME_TOKEN_TTL_MS)
            .ok_or(OneTimeStoreError::Unavailable)?;
        let (plaintext, parts) = self
            .codec
            .issue()
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        let record = OneTimeTokenRecord {
            id: TokenId::new(),
            project_id,
            user_id,
            purpose,
            prefix: parts.prefix,
            digest: parts.digest,
            expires_at_ms: expiry,
            consumed_at_ms: None,
        };
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        // Serialize issue/supersede for one user. The partial unique index is a
        // second line of defense, but this lock makes concurrent issuers
        // deterministically leave exactly one active token.
        let user_exists: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM auth_users WHERE project_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        if user_exists.is_none() {
            return Err(OneTimeStoreError::Rejected);
        }
        sqlx::query(
            "UPDATE auth_one_time_tokens SET consumed_at=to_timestamp($4::double precision/1000) \
             WHERE project_id=$1 AND user_id=$2 AND purpose=$3 AND consumed_at IS NULL",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(purpose_name(purpose))
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        sqlx::query(
            "INSERT INTO auth_one_time_tokens \
             (id,project_id,user_id,purpose,lookup_prefix,keyed_hash,expires_at,created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,to_timestamp($7::double precision/1000),to_timestamp($8::double precision/1000))",
        )
        .bind(record.id.0)
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(purpose_name(purpose))
        .bind(&record.prefix)
        .bind(record.digest.as_bytes().as_slice())
        .bind(expiry)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        let row = sqlx::query(
            "SELECT id,project_id,user_id,purpose,lookup_prefix,keyed_hash, \
                    (extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms, \
                    (extract(epoch FROM consumed_at)*1000)::bigint AS consumed_at_ms \
             FROM auth_one_time_tokens WHERE lookup_prefix=$1 FOR UPDATE",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?
        .ok_or(OneTimeStoreError::Rejected)?;
        let mut record = one_time_from_row(row).map_err(|_| OneTimeStoreError::Unavailable)?;
        if record.purpose != purpose
            || record.consumed_at_ms.is_some()
            || record.expires_at_ms <= now_ms
            || !self.codec.verify_digest(&candidate.digest, &record.digest)
        {
            return Err(OneTimeStoreError::Rejected);
        }
        sqlx::query(
            "UPDATE auth_one_time_tokens SET consumed_at=to_timestamp($2::double precision/1000) WHERE id=$1",
        )
        .bind(record.id.0)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| OneTimeStoreError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| OneTimeStoreError::Unavailable)?;
        record.consumed_at_ms = Some(now_ms);
        Ok(record)
    }
}

#[derive(Clone, Debug)]
pub struct PgRefreshStore {
    pool: PgPool,
    codec: OpaqueTokenCodec,
}

impl PgRefreshStore {
    pub fn new(pool: PgPool, pepper: Vec<u8>) -> Result<Self, TokenError> {
        Ok(Self {
            pool,
            codec: OpaqueTokenCodec::new("refresh", pepper)?,
        })
    }

    /// Return trusted ownership for a presented refresh credential without
    /// rotating it. This supports per-user admission control before a mutation.
    pub async fn identify_for_project(
        &self,
        project_id: ProjectId,
        presented: &str,
        now_ms: i64,
    ) -> Result<Option<(UserId, SessionId)>, RefreshStoreError> {
        let candidate = match self.codec.parse_and_digest(presented) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(None),
        };
        let row = sqlx::query(
            "SELECT t.keyed_hash,(extract(epoch FROM t.expires_at)*1000)::bigint token_expires_at_ms, \
                    f.project_id,f.user_id,f.session_id, \
                    (f.revoked_at IS NOT NULL) family_revoked, \
                    (s.revoked_at IS NOT NULL) session_revoked, \
                    (extract(epoch FROM s.expires_at)*1000)::bigint session_expires_at_ms \
             FROM refresh_tokens t JOIN refresh_token_families f ON f.id=t.family_id \
             JOIN auth_sessions s ON s.id=f.session_id WHERE t.lookup_prefix=$1",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let digest = CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        )
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let stored_project = ProjectId(
            row.try_get("project_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        );
        let token_expiry: i64 = row
            .try_get("token_expires_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let session_expiry: i64 = row
            .try_get("session_expires_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let family_revoked: bool = row
            .try_get("family_revoked")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let session_revoked: bool = row
            .try_get("session_revoked")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        if stored_project != project_id
            || token_expiry <= now_ms
            || session_expiry <= now_ms
            || family_revoked
            || session_revoked
            || !self.codec.verify_digest(&candidate.digest, &digest)
        {
            return Ok(None);
        }
        Ok(Some((
            UserId(
                row.try_get("user_id")
                    .map_err(|_| RefreshStoreError::Unavailable)?,
            ),
            SessionId(
                row.try_get("session_id")
                    .map_err(|_| RefreshStoreError::Unavailable)?,
            ),
        )))
    }

    /// Rotate only when the verified refresh credential belongs to the project
    /// selected by the trusted request path. A cross-project presentation is
    /// rejected without consuming or revoking the otherwise valid token.
    pub async fn rotate_for_project(
        &self,
        project_id: ProjectId,
        presented: &str,
        now_ms: i64,
    ) -> Result<RefreshRotation, RefreshStoreError> {
        let candidate = match self.codec.parse_and_digest(presented) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(RefreshRotation::Rejected),
        };
        let row = sqlx::query(
            "SELECT t.keyed_hash,f.project_id FROM refresh_tokens t \
             JOIN refresh_token_families f ON f.id=t.family_id WHERE t.lookup_prefix=$1",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(RefreshRotation::Rejected);
        };
        let digest = CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        )
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let stored_project = ProjectId(
            row.try_get("project_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        );
        if stored_project != project_id || !self.codec.verify_digest(&candidate.digest, &digest) {
            return Ok(RefreshRotation::Rejected);
        }
        self.rotate(presented, now_ms).await
    }

    /// Revoke the family selected by a refresh credential. Project scope is
    /// verified before mutation so a token cannot be used against another
    /// project's route.
    pub async fn revoke_presented_for_project(
        &self,
        project_id: ProjectId,
        presented: &str,
        now_ms: i64,
    ) -> Result<bool, RefreshStoreError> {
        let candidate = match self.codec.parse_and_digest(presented) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(false),
        };
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let row = sqlx::query(
            "SELECT t.keyed_hash,f.id family_id,f.project_id,f.session_id \
             FROM refresh_tokens t JOIN refresh_token_families f ON f.id=t.family_id \
             WHERE t.lookup_prefix=$1 FOR UPDATE OF t,f",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let digest = CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        )
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let stored_project = ProjectId(
            row.try_get("project_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        );
        if stored_project != project_id || !self.codec.verify_digest(&candidate.digest, &digest) {
            return Ok(false);
        }
        let family_id: Uuid = row
            .try_get("family_id")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let session_id = SessionId(
            row.try_get("session_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        );
        revoke_family(
            &mut transaction,
            family_id,
            session_id,
            now_ms,
            "signed_out",
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        Ok(true)
    }

    pub async fn list_user_sessions(
        &self,
        project_id: ProjectId,
        user_id: UserId,
    ) -> Result<Vec<AuthSessionSummary>, RefreshStoreError> {
        let rows = sqlx::query(
            "SELECT id,(extract(epoch FROM created_at)*1000)::bigint created_at_ms, \
                    (extract(epoch FROM last_seen_at)*1000)::bigint last_seen_at_ms, \
                    (extract(epoch FROM expires_at)*1000)::bigint expires_at_ms, \
                    (extract(epoch FROM revoked_at)*1000)::bigint revoked_at_ms \
             FROM auth_sessions WHERE project_id=$1 AND user_id=$2 \
             ORDER BY created_at DESC LIMIT 100",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        rows.into_iter()
            .map(|row| {
                Ok(AuthSessionSummary {
                    id: SessionId(
                        row.try_get("id")
                            .map_err(|_| RefreshStoreError::Unavailable)?,
                    ),
                    created_at_ms: row
                        .try_get("created_at_ms")
                        .map_err(|_| RefreshStoreError::Unavailable)?,
                    last_seen_at_ms: row
                        .try_get("last_seen_at_ms")
                        .map_err(|_| RefreshStoreError::Unavailable)?,
                    expires_at_ms: row
                        .try_get("expires_at_ms")
                        .map_err(|_| RefreshStoreError::Unavailable)?,
                    revoked_at_ms: row
                        .try_get("revoked_at_ms")
                        .map_err(|_| RefreshStoreError::Unavailable)?,
                })
            })
            .collect()
    }

    pub async fn revoke_user_session(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        session_id: SessionId,
        now_ms: i64,
    ) -> Result<bool, RefreshStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let family: Option<Uuid> = sqlx::query_scalar(
            "SELECT f.id FROM refresh_token_families f JOIN auth_sessions s ON s.id=f.session_id \
             WHERE f.project_id=$1 AND f.user_id=$2 AND f.session_id=$3 \
               AND s.project_id=$1 AND s.user_id=$2 FOR UPDATE OF f,s",
        )
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(session_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let Some(family_id) = family else {
            return Ok(false);
        };
        revoke_family(
            &mut transaction,
            family_id,
            session_id,
            now_ms,
            "user_revoked",
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        Ok(true)
    }

    pub async fn issue_session_with_ttl(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        now_ms: i64,
        ttl_seconds: u32,
    ) -> Result<RefreshIssue, RefreshStoreError> {
        if !(3_600..=7_776_000).contains(&ttl_seconds) {
            return Err(RefreshStoreError::Invalid);
        }
        let ttl_ms = i64::from(ttl_seconds)
            .checked_mul(1_000)
            .ok_or(RefreshStoreError::Invalid)?;
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or(RefreshStoreError::Invalid)?;
        let (plaintext, parts) = self
            .codec
            .issue()
            .map_err(|_| RefreshStoreError::Unavailable)?;
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        sqlx::query(
            "INSERT INTO auth_sessions (id,project_id,user_id,created_at,last_seen_at,expires_at) \
             VALUES ($1,$2,$3,to_timestamp($4::double precision/1000),to_timestamp($4::double precision/1000), \
                     to_timestamp($5::double precision/1000))",
        )
        .bind(session.id.0)
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(now_ms)
        .bind(expires_at_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        sqlx::query(
            "INSERT INTO refresh_token_families (id,project_id,user_id,session_id,created_at) \
             VALUES ($1,$2,$3,$4,to_timestamp($5::double precision/1000))",
        )
        .bind(family.id)
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(session.id.0)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        insert_refresh_token(&mut transaction, &token).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        Ok(RefreshIssue {
            plaintext,
            session,
            family,
            token,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthSessionSummary {
    pub id: SessionId,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

#[async_trait]
impl RefreshTokenStore for PgRefreshStore {
    async fn issue_session(
        &self,
        project_id: ProjectId,
        user_id: UserId,
        now_ms: i64,
    ) -> Result<RefreshIssue, RefreshStoreError> {
        self.issue_session_with_ttl(
            project_id,
            user_id,
            now_ms,
            u32::try_from(REFRESH_TOKEN_TTL_MS / 1_000).map_err(|_| RefreshStoreError::Invalid)?,
        )
        .await
    }

    async fn rotate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<RefreshRotation, RefreshStoreError> {
        let candidate = match self.codec.parse_and_digest(presented) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(RefreshRotation::Rejected),
        };
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let row = sqlx::query(
            "SELECT t.id,t.family_id,t.lookup_prefix,t.keyed_hash, \
                    (extract(epoch FROM t.issued_at)*1000)::bigint issued_at_ms, \
                    (extract(epoch FROM t.expires_at)*1000)::bigint expires_at_ms, \
                    (extract(epoch FROM t.used_at)*1000)::bigint used_at_ms,t.replaced_by, \
                    f.project_id,f.user_id,f.session_id, \
                    (extract(epoch FROM f.revoked_at)*1000)::bigint family_revoked_at_ms, \
                    f.revoke_reason family_reason, \
                    (extract(epoch FROM s.expires_at)*1000)::bigint session_expires_at_ms, \
                    (extract(epoch FROM s.revoked_at)*1000)::bigint session_revoked_at_ms \
             FROM refresh_tokens t JOIN refresh_token_families f ON f.id=t.family_id \
             JOIN auth_sessions s ON s.id=f.session_id WHERE t.lookup_prefix=$1 FOR UPDATE OF t,f,s",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(RefreshRotation::Rejected);
        };
        let current = refresh_token_from_row(&row)?;
        if !self.codec.verify_digest(&candidate.digest, &current.digest) {
            return Ok(RefreshRotation::Rejected);
        }
        let family = refresh_family_from_row(&row)?;
        let session_expiry: i64 = row
            .try_get("session_expires_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let session_revoked: Option<i64> = row
            .try_get("session_revoked_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?;
        if family.revoked_at_ms.is_some()
            || session_revoked.is_some()
            || session_expiry <= now_ms
            || current.expires_at_ms <= now_ms
        {
            return Ok(RefreshRotation::Rejected);
        }
        if current.used_at_ms.is_some() {
            revoke_family(
                &mut transaction,
                family.id,
                family.session_id,
                now_ms,
                "refresh_reuse",
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(|_| RefreshStoreError::Unavailable)?;
            return Ok(RefreshRotation::ReuseDetected {
                family_id: family.id,
                session_id: family.session_id,
            });
        }
        let (plaintext, parts) = self
            .codec
            .issue()
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let next = RefreshTokenRecord {
            id: TokenId::new(),
            family_id: family.id,
            prefix: parts.prefix,
            digest: parts.digest,
            issued_at_ms: now_ms,
            expires_at_ms: current.expires_at_ms,
            used_at_ms: None,
            replaced_by: None,
        };
        insert_refresh_token(&mut transaction, &next).await?;
        let updated = sqlx::query(
            "UPDATE refresh_tokens SET used_at=to_timestamp($2::double precision/1000),replaced_by=$3 \
             WHERE id=$1 AND used_at IS NULL",
        )
        .bind(current.id.0)
        .bind(now_ms)
        .bind(next.id.0)
        .execute(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(RefreshStoreError::Unavailable);
        }
        sqlx::query(
            "UPDATE auth_sessions SET last_seen_at=GREATEST(last_seen_at, \
             to_timestamp($2::double precision/1000)) WHERE id=$1",
        )
        .bind(family.session_id.0)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        Ok(RefreshRotation::Rotated {
            plaintext,
            token: Box::new(next),
            family: Box::new(family),
        })
    }

    async fn revoke_session(
        &self,
        session_id: SessionId,
        now_ms: i64,
        reason: &str,
    ) -> Result<bool, RefreshStoreError> {
        if reason.is_empty() || reason.len() > 64 {
            return Err(RefreshStoreError::Invalid);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        let family: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM refresh_token_families WHERE session_id=$1 FOR UPDATE",
        )
        .bind(session_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| RefreshStoreError::Unavailable)?;
        let Some(family_id) = family else {
            return Ok(false);
        };
        revoke_family(&mut transaction, family_id, session_id, now_ms, reason).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RefreshStoreError::Unavailable)?;
        Ok(true)
    }
}

/// Encrypted private-key envelope loaded from PostgreSQL. Decryption is delegated
/// to a production KMS/envelope adapter; plaintext is zeroized after signer setup.
#[derive(Clone, Debug)]
pub struct EncryptedSigningKey {
    pub project_id: ProjectId,
    pub kid: String,
    pub ciphertext: Vec<u8>,
    pub encryption_key_version: i32,
}

impl Drop for EncryptedSigningKey {
    fn drop(&mut self) {
        self.ciphertext.zeroize();
    }
}

#[async_trait]
pub trait SigningKeyDecryptor: Send + Sync {
    async fn decrypt(&self, key: &EncryptedSigningKey) -> Result<Zeroizing<[u8; 32]>, JwtError>;
}

#[derive(Clone, Debug)]
pub struct PgSigningKeyStore<D> {
    pool: PgPool,
    decryptor: D,
}

impl<D> PgSigningKeyStore<D> {
    #[must_use]
    pub fn new(pool: PgPool, decryptor: D) -> Self {
        Self { pool, decryptor }
    }
}

#[async_trait]
impl<D: SigningKeyDecryptor> SigningKeyStore for PgSigningKeyStore<D> {
    async fn active_signer(&self, project_id: ProjectId) -> Result<ProjectSigner, JwtError> {
        let row = sqlx::query(
            "SELECT kid,encrypted_private_key,encryption_key_version FROM jwt_signing_keys \
             WHERE project_id=$1 AND status='active' AND revoked_at IS NULL AND valid_from<=now() \
               AND (valid_until IS NULL OR valid_until>now())",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JwtError::KeyStoreUnavailable)?
        .ok_or(JwtError::UnknownKey)?;
        let key = EncryptedSigningKey {
            project_id,
            kid: row
                .try_get("kid")
                .map_err(|_| JwtError::KeyStoreUnavailable)?,
            ciphertext: row
                .try_get("encrypted_private_key")
                .map_err(|_| JwtError::KeyStoreUnavailable)?,
            encryption_key_version: row
                .try_get("encryption_key_version")
                .map_err(|_| JwtError::KeyStoreUnavailable)?,
        };
        let plaintext = self.decryptor.decrypt(&key).await?;
        ProjectSigner::from_bytes(project_id, key.kid.clone(), &plaintext)
    }

    async fn verification_keys(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<VerificationKey>, JwtError> {
        let rows = sqlx::query(
            "SELECT kid,public_key,status,(extract(epoch FROM valid_from))::bigint valid_from_seconds, \
                    (extract(epoch FROM valid_until))::bigint valid_until_seconds \
             FROM jwt_signing_keys WHERE project_id=$1 AND status IN ('active','grace') AND revoked_at IS NULL",
        )
        .bind(project_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| JwtError::KeyStoreUnavailable)?;
        rows.into_iter()
            .map(|row| verification_key_from_row(project_id, row))
            .collect()
    }
}

async fn insert_refresh_token(
    transaction: &mut Transaction<'_, Postgres>,
    token: &RefreshTokenRecord,
) -> Result<(), RefreshStoreError> {
    sqlx::query(
        "INSERT INTO refresh_tokens (id,family_id,lookup_prefix,keyed_hash,issued_at,expires_at) \
         VALUES ($1,$2,$3,$4,to_timestamp($5::double precision/1000),to_timestamp($6::double precision/1000))",
    )
    .bind(token.id.0)
    .bind(token.family_id)
    .bind(&token.prefix)
    .bind(token.digest.as_bytes().as_slice())
    .bind(token.issued_at_ms)
    .bind(token.expires_at_ms)
    .execute(&mut **transaction)
    .await
    .map_err(|_| RefreshStoreError::Unavailable)?;
    Ok(())
}

async fn revoke_family(
    transaction: &mut Transaction<'_, Postgres>,
    family_id: Uuid,
    session_id: SessionId,
    now_ms: i64,
    reason: &str,
) -> Result<(), RefreshStoreError> {
    sqlx::query(
        "UPDATE refresh_token_families \
         SET revoked_at=COALESCE(revoked_at,to_timestamp($2::double precision/1000)), \
             revoke_reason=COALESCE(revoke_reason,$3) WHERE id=$1",
    )
    .bind(family_id)
    .bind(now_ms)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| RefreshStoreError::Unavailable)?;
    sqlx::query(
        "UPDATE auth_sessions \
         SET revoked_at=COALESCE(revoked_at,to_timestamp($2::double precision/1000)), \
             revoke_reason=COALESCE(revoke_reason,$3) WHERE id=$1",
    )
    .bind(session_id.0)
    .bind(now_ms)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| RefreshStoreError::Unavailable)?;
    Ok(())
}

async fn revoke_user_sessions(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    user_id: UserId,
    now_ms: i64,
    reason: &str,
) -> Result<(), AccountError> {
    sqlx::query(
        "UPDATE auth_sessions \
         SET revoked_at=COALESCE(revoked_at,to_timestamp($3::double precision/1000)), \
             revoke_reason=COALESCE(revoke_reason,$4) WHERE project_id=$1 AND user_id=$2",
    )
    .bind(project_id.0)
    .bind(user_id.0)
    .bind(now_ms)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(map_account_sqlx)?;
    sqlx::query(
        "UPDATE refresh_token_families \
         SET revoked_at=COALESCE(revoked_at,to_timestamp($3::double precision/1000)), \
             revoke_reason=COALESCE(revoke_reason,$4) WHERE project_id=$1 AND user_id=$2",
    )
    .bind(project_id.0)
    .bind(user_id.0)
    .bind(now_ms)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(map_account_sqlx)?;
    Ok(())
}

fn api_key_from_row(row: sqlx::postgres::PgRow) -> Result<ApiKeyRecord, AccountError> {
    let scopes: Vec<String> = row
        .try_get("scopes")
        .map_err(|_| AccountError::Unavailable)?;
    let scopes = scopes
        .into_iter()
        .map(|scope| parse_scope(&scope))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ApiKeyRecord {
        id: ApiKeyId(row.try_get("id").map_err(|_| AccountError::Unavailable)?),
        organization_id: OrganizationId(
            row.try_get("organization_id")
                .map_err(|_| AccountError::Unavailable)?,
        ),
        project_id: row
            .try_get::<Option<Uuid>, _>("project_id")
            .map_err(|_| AccountError::Unavailable)?
            .map(ProjectId),
        prefix: row
            .try_get("lookup_prefix")
            .map_err(|_| AccountError::Unavailable)?,
        digest: CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| AccountError::Unavailable)?,
        )
        .map_err(|_| AccountError::Unavailable)?,
        scopes,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
        revoked_at_ms: row
            .try_get("revoked_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
    })
}

fn account_from_row(row: sqlx::postgres::PgRow) -> Result<AuthUserRecord, AccountError> {
    let claims: sqlx::types::Json<serde_json::Value> = row
        .try_get("custom_claims")
        .map_err(|_| AccountError::Unavailable)?;
    let custom_claims = claims
        .0
        .as_object()
        .cloned()
        .ok_or(AccountError::Unavailable)?;
    validate_custom_claims(&custom_claims).map_err(|_| AccountError::Unavailable)?;
    let role: String = row.try_get("role").map_err(|_| AccountError::Unavailable)?;
    if role.is_empty() || role.len() > 64 || role.chars().any(char::is_control) {
        return Err(AccountError::Unavailable);
    }
    Ok(AuthUserRecord {
        id: UserId(row.try_get("id").map_err(|_| AccountError::Unavailable)?),
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| AccountError::Unavailable)?,
        ),
        normalized_email: row
            .try_get("email")
            .map_err(|_| AccountError::Unavailable)?,
        password_hash: PasswordHash::parse(
            row.try_get("password_phc")
                .map_err(|_| AccountError::Unavailable)?,
        )?,
        role,
        custom_claims,
        email_verified_at_ms: row
            .try_get("email_verified_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
        disabled_at_ms: row
            .try_get("disabled_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
        password_changed_at_ms: row
            .try_get("password_changed_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
    })
}

fn one_time_from_row(row: sqlx::postgres::PgRow) -> Result<OneTimeTokenRecord, AccountError> {
    let purpose: String = row
        .try_get("purpose")
        .map_err(|_| AccountError::Unavailable)?;
    Ok(OneTimeTokenRecord {
        id: TokenId(row.try_get("id").map_err(|_| AccountError::Unavailable)?),
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| AccountError::Unavailable)?,
        ),
        user_id: UserId(
            row.try_get("user_id")
                .map_err(|_| AccountError::Unavailable)?,
        ),
        purpose: parse_purpose(&purpose)?,
        prefix: row
            .try_get("lookup_prefix")
            .map_err(|_| AccountError::Unavailable)?,
        digest: CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| AccountError::Unavailable)?,
        )
        .map_err(|_| AccountError::Unavailable)?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
        consumed_at_ms: row
            .try_get("consumed_at_ms")
            .map_err(|_| AccountError::Unavailable)?,
    })
}

fn refresh_token_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RefreshTokenRecord, RefreshStoreError> {
    Ok(RefreshTokenRecord {
        id: TokenId(
            row.try_get("id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        ),
        family_id: row
            .try_get("family_id")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        prefix: row
            .try_get("lookup_prefix")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        digest: CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        )
        .map_err(|_| RefreshStoreError::Unavailable)?,
        issued_at_ms: row
            .try_get("issued_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        used_at_ms: row
            .try_get("used_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        replaced_by: row
            .try_get::<Option<Uuid>, _>("replaced_by")
            .map_err(|_| RefreshStoreError::Unavailable)?
            .map(TokenId),
    })
}

fn refresh_family_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RefreshFamily, RefreshStoreError> {
    Ok(RefreshFamily {
        id: row
            .try_get("family_id")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        ),
        user_id: UserId(
            row.try_get("user_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        ),
        session_id: SessionId(
            row.try_get("session_id")
                .map_err(|_| RefreshStoreError::Unavailable)?,
        ),
        revoked_at_ms: row
            .try_get("family_revoked_at_ms")
            .map_err(|_| RefreshStoreError::Unavailable)?,
        revoke_reason: row
            .try_get("family_reason")
            .map_err(|_| RefreshStoreError::Unavailable)?,
    })
}

fn verification_key_from_row(
    project_id: ProjectId,
    row: sqlx::postgres::PgRow,
) -> Result<VerificationKey, JwtError> {
    let bytes: Vec<u8> = row
        .try_get("public_key")
        .map_err(|_| JwtError::KeyStoreUnavailable)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| JwtError::KeyStoreUnavailable)?;
    let status: String = row
        .try_get("status")
        .map_err(|_| JwtError::KeyStoreUnavailable)?;
    Ok(VerificationKey {
        project_id,
        kid: row
            .try_get("kid")
            .map_err(|_| JwtError::KeyStoreUnavailable)?,
        public_key: VerifyingKey::from_bytes(&bytes).map_err(|_| JwtError::KeyStoreUnavailable)?,
        status: match status.as_str() {
            "active" => VerificationKeyStatus::Active,
            "grace" => VerificationKeyStatus::Grace,
            _ => return Err(JwtError::KeyStoreUnavailable),
        },
        valid_from_seconds: row
            .try_get("valid_from_seconds")
            .map_err(|_| JwtError::KeyStoreUnavailable)?,
        valid_until_seconds: row
            .try_get("valid_until_seconds")
            .map_err(|_| JwtError::KeyStoreUnavailable)?,
    })
}

fn map_account_sqlx(error: sqlx::Error) -> AccountError {
    if let sqlx::Error::Database(database) = &error
        && database.code().as_deref() == Some("23505")
    {
        return AccountError::EmailInUse;
    }
    AccountError::Unavailable
}

fn purpose_name(purpose: OneTimePurpose) -> &'static str {
    match purpose {
        OneTimePurpose::EmailVerification => "email_verification",
        OneTimePurpose::PasswordReset => "password_reset",
    }
}

fn parse_purpose(value: &str) -> Result<OneTimePurpose, AccountError> {
    match value {
        "email_verification" => Ok(OneTimePurpose::EmailVerification),
        "password_reset" => Ok(OneTimePurpose::PasswordReset),
        _ => Err(AccountError::Unavailable),
    }
}

fn scope_name(scope: DeveloperScope) -> &'static str {
    match scope {
        DeveloperScope::ProjectsRead => "projects_read",
        DeveloperScope::ProjectsWrite => "projects_write",
        DeveloperScope::DatabaseQuery => "database_query",
        DeveloperScope::DatabaseMigrate => "database_migrate",
        DeveloperScope::DatabaseSchema => "database_schema",
        DeveloperScope::AuthManage => "auth_manage",
        DeveloperScope::StorageManage => "storage_manage",
        DeveloperScope::EmailManage => "email_manage",
        DeveloperScope::KeysRotate => "keys_rotate",
        DeveloperScope::BackupsManage => "backups_manage",
        DeveloperScope::LogsRead => "logs_read",
        DeveloperScope::CommerceManage => "commerce_manage",
    }
}

fn parse_scope(value: &str) -> Result<DeveloperScope, AccountError> {
    match value {
        "projects_read" => Ok(DeveloperScope::ProjectsRead),
        "projects_write" => Ok(DeveloperScope::ProjectsWrite),
        "database_query" => Ok(DeveloperScope::DatabaseQuery),
        "database_migrate" => Ok(DeveloperScope::DatabaseMigrate),
        "database_schema" => Ok(DeveloperScope::DatabaseSchema),
        "auth_manage" => Ok(DeveloperScope::AuthManage),
        "storage_manage" => Ok(DeveloperScope::StorageManage),
        "email_manage" => Ok(DeveloperScope::EmailManage),
        "keys_rotate" => Ok(DeveloperScope::KeysRotate),
        "backups_manage" => Ok(DeveloperScope::BackupsManage),
        "logs_read" => Ok(DeveloperScope::LogsRead),
        "commerce_manage" => Ok(DeveloperScope::CommerceManage),
        _ => Err(AccountError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_mapping_is_total_and_round_trips() -> Result<(), AccountError> {
        let scopes = [
            DeveloperScope::ProjectsRead,
            DeveloperScope::ProjectsWrite,
            DeveloperScope::DatabaseQuery,
            DeveloperScope::DatabaseMigrate,
            DeveloperScope::DatabaseSchema,
            DeveloperScope::AuthManage,
            DeveloperScope::StorageManage,
            DeveloperScope::EmailManage,
            DeveloperScope::KeysRotate,
            DeveloperScope::BackupsManage,
            DeveloperScope::LogsRead,
            DeveloperScope::CommerceManage,
        ];
        for scope in scopes {
            assert_eq!(parse_scope(scope_name(scope))?, scope);
        }
        Ok(())
    }

    #[test]
    fn migration_has_refresh_and_encrypted_key_state() {
        let sql = include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql");
        assert!(sql.contains("refresh_token_families"));
        assert!(sql.contains("encrypted_private_key bytea"));
        assert!(sql.contains("lookup_prefix text NOT NULL UNIQUE"));
    }
}
