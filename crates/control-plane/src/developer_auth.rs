//! Durable authentication for FFDB's control-plane developers.
//!
//! The HTTP bootstrap endpoint must authenticate a deployment bootstrap token
//! before calling `bootstrap_first_user`. This service separately guarantees
//! that only the first platform user can be created through that path.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use ffdb_auth::{
    CredentialDigest, OpaqueTokenCodec, PasswordHash, PasswordHasher, SecretString, SecretToken,
    TokenError, TokenParts, VerifyOutcome,
};
use ffdb_protocol::{TokenId, UserId};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

const PLATFORM_SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct PlatformUserRecord {
    pub id: UserId,
    pub normalized_email: String,
    pub password_hash: PasswordHash,
    pub email_verified_at_ms: Option<i64>,
    pub disabled_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct PlatformSessionRecord {
    pub id: TokenId,
    pub family_id: Uuid,
    pub user_id: UserId,
    pub prefix: String,
    pub digest: CredentialDigest,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub used_at_ms: Option<i64>,
    pub replaced_by: Option<TokenId>,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformSessionIdentity {
    pub user_id: UserId,
    pub normalized_email: String,
    pub family_id: Uuid,
    /// Original password sign-in time used to require recent authentication
    /// before host-level operations. It is server-derived, never a client
    /// claim.
    pub authenticated_at_ms: i64,
}

#[derive(Debug)]
pub struct PlatformSessionIssue {
    pub plaintext: SecretToken,
    pub session: PlatformSessionRecord,
    pub identity: PlatformSessionIdentity,
}

#[derive(Debug)]
pub enum PlatformSessionRotation {
    Rotated {
        plaintext: SecretToken,
        session: Box<PlatformSessionRecord>,
        identity: Box<PlatformSessionIdentity>,
    },
    ReuseDetected {
        family_id: Uuid,
        user_id: UserId,
    },
    Rejected,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum PlatformAuthError {
    #[error("platform authentication is already initialized")]
    AlreadyInitialized,
    #[error("platform email address is invalid")]
    InvalidEmail,
    #[error("platform password is invalid")]
    InvalidPassword,
    #[error("platform credentials are invalid")]
    InvalidCredentials,
    #[error("platform account is disabled")]
    Disabled,
    #[error("platform account verification is required")]
    VerificationRequired,
    #[error("platform session input is invalid")]
    InvalidSession,
    #[error("platform authentication datastore is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait PlatformUserRepository: Send + Sync {
    async fn insert_first(&self, user: PlatformUserRecord) -> Result<(), PlatformAuthError>;
    async fn find_by_email(
        &self,
        normalized_email: &str,
    ) -> Result<Option<PlatformUserRecord>, PlatformAuthError>;
}

#[async_trait]
pub trait PlatformSessionStore: Send + Sync {
    async fn issue(
        &self,
        user: &PlatformUserRecord,
        now_ms: i64,
    ) -> Result<PlatformSessionIssue, PlatformAuthError>;

    async fn rotate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<PlatformSessionRotation, PlatformAuthError>;

    async fn authenticate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<PlatformSessionIdentity, PlatformAuthError>;

    async fn revoke(&self, presented: &str, now_ms: i64) -> Result<bool, PlatformAuthError>;
}

pub struct PlatformAuthService {
    users: Arc<dyn PlatformUserRepository>,
    sessions: Arc<dyn PlatformSessionStore>,
    password_hasher: Arc<dyn PasswordHasher>,
    dummy_hash: PasswordHash,
}

impl fmt::Debug for PlatformAuthService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformAuthService")
            .field("users", &"dyn PlatformUserRepository")
            .field("sessions", &"dyn PlatformSessionStore")
            .field("password_hasher", &"dyn PasswordHasher")
            .field("dummy_hash", &"[REDACTED]")
            .finish()
    }
}

impl PlatformAuthService {
    pub fn new(
        users: Arc<dyn PlatformUserRepository>,
        sessions: Arc<dyn PlatformSessionStore>,
        password_hasher: Arc<dyn PasswordHasher>,
    ) -> Result<Self, PlatformAuthError> {
        let dummy_hash = password_hasher
            .hash(SecretString::new(
                "ffdb-platform-dummy-password-work-factor".into(),
            ))
            .map_err(|_| PlatformAuthError::InvalidPassword)?;
        Ok(Self {
            users,
            sessions,
            password_hasher,
            dummy_hash,
        })
    }

    /// Called only after the HTTP layer verifies the deployment bootstrap token.
    pub async fn bootstrap_first_user(
        &self,
        email: &str,
        password: SecretString,
        now_ms: i64,
    ) -> Result<PlatformUserRecord, PlatformAuthError> {
        let normalized_email = normalize_email(email)?;
        let password_hash = self
            .password_hasher
            .hash(password)
            .map_err(|_| PlatformAuthError::InvalidPassword)?;
        let user = PlatformUserRecord {
            id: UserId::new(),
            normalized_email,
            password_hash,
            email_verified_at_ms: Some(now_ms),
            disabled_at_ms: None,
            created_at_ms: now_ms,
        };
        self.users.insert_first(user.clone()).await?;
        Ok(user)
    }

    pub async fn sign_in(
        &self,
        email: &str,
        password: SecretString,
        now_ms: i64,
    ) -> Result<PlatformSessionIssue, PlatformAuthError> {
        let normalized_email =
            normalize_email(email).map_err(|_| PlatformAuthError::InvalidCredentials)?;
        let user = self.users.find_by_email(&normalized_email).await?;
        let Some(user) = user else {
            let _ = self
                .password_hasher
                .verify(password, &self.dummy_hash)
                .map_err(|_| PlatformAuthError::InvalidCredentials)?;
            return Err(PlatformAuthError::InvalidCredentials);
        };
        let verification = self
            .password_hasher
            .verify(password, &user.password_hash)
            .map_err(|_| PlatformAuthError::InvalidCredentials)?;
        if !matches!(
            verification,
            VerifyOutcome::Valid | VerifyOutcome::ValidNeedsRehash
        ) {
            return Err(PlatformAuthError::InvalidCredentials);
        }
        if user.disabled_at_ms.is_some() {
            return Err(PlatformAuthError::Disabled);
        }
        if user.email_verified_at_ms.is_none() {
            return Err(PlatformAuthError::VerificationRequired);
        }
        self.sessions.issue(&user, now_ms).await
    }

    pub async fn refresh(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<PlatformSessionRotation, PlatformAuthError> {
        self.sessions.rotate(presented, now_ms).await
    }

    pub async fn authenticate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<PlatformSessionIdentity, PlatformAuthError> {
        self.sessions.authenticate(presented, now_ms).await
    }

    pub async fn sign_out(&self, presented: &str, now_ms: i64) -> Result<bool, PlatformAuthError> {
        self.sessions.revoke(presented, now_ms).await
    }
}

#[derive(Clone, Debug)]
pub struct PgPlatformUserRepository {
    pool: PgPool,
}

impl PgPlatformUserRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PlatformUserRepository for PgPlatformUserRepository {
    async fn insert_first(&self, user: PlatformUserRecord) -> Result<(), PlatformAuthError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        sqlx::query("SELECT pg_advisory_xact_lock(632309907176934772::bigint)")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        let initialized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM platform_users)")
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        if initialized {
            return Err(PlatformAuthError::AlreadyInitialized);
        }
        sqlx::query(
            "INSERT INTO platform_users \
             (id,email,password_phc,email_verified_at,disabled_at,created_at,updated_at) \
             VALUES ($1,$2,$3,to_timestamp($4::double precision/1000), \
                     to_timestamp($5::double precision/1000),to_timestamp($6::double precision/1000), \
                     to_timestamp($6::double precision/1000))",
        )
        .bind(user.id.0)
        .bind(&user.normalized_email)
        .bind(user.password_hash.as_phc())
        .bind(user.email_verified_at_ms)
        .bind(user.disabled_at_ms)
        .bind(user.created_at_ms)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        sqlx::query("INSERT INTO instance_settings (singleton,owner_user_id) VALUES (true,$1)")
            .bind(user.id.0)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO instance_administrators (user_id,role,granted_by) \
             VALUES ($1,'owner',NULL)",
        )
        .bind(user.id.0)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    async fn find_by_email(
        &self,
        normalized_email: &str,
    ) -> Result<Option<PlatformUserRecord>, PlatformAuthError> {
        let row = sqlx::query(
            "SELECT id,email,password_phc, \
                    (extract(epoch FROM email_verified_at)*1000)::bigint email_verified_at_ms, \
                    (extract(epoch FROM disabled_at)*1000)::bigint disabled_at_ms, \
                    (extract(epoch FROM created_at)*1000)::bigint created_at_ms \
             FROM platform_users WHERE lower(email)=lower($1)",
        )
        .bind(normalized_email)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.map(platform_user_from_row).transpose()
    }
}

#[derive(Clone, Debug)]
pub struct PgPlatformSessionStore {
    pool: PgPool,
    codec: OpaqueTokenCodec,
}

impl PgPlatformSessionStore {
    pub fn new(pool: PgPool, pepper: Vec<u8>) -> Result<Self, TokenError> {
        Ok(Self {
            pool,
            codec: OpaqueTokenCodec::new("platform", pepper)?,
        })
    }
}

#[async_trait]
impl PlatformSessionStore for PgPlatformSessionStore {
    async fn issue(
        &self,
        user: &PlatformUserRecord,
        now_ms: i64,
    ) -> Result<PlatformSessionIssue, PlatformAuthError> {
        let expires_at_ms = now_ms
            .checked_add(PLATFORM_SESSION_TTL_MS)
            .ok_or(PlatformAuthError::InvalidSession)?;
        let (plaintext, parts) = self
            .codec
            .issue()
            .map_err(|_| PlatformAuthError::Unavailable)?;
        let record = PlatformSessionRecord {
            id: TokenId::new(),
            family_id: Uuid::now_v7(),
            user_id: user.id,
            prefix: parts.prefix,
            digest: parts.digest,
            issued_at_ms: now_ms,
            expires_at_ms,
            used_at_ms: None,
            replaced_by: None,
            revoked_at_ms: None,
        };
        insert_platform_session(&self.pool, &record).await?;
        Ok(PlatformSessionIssue {
            plaintext,
            session: record.clone(),
            identity: PlatformSessionIdentity {
                user_id: user.id,
                normalized_email: user.normalized_email.clone(),
                family_id: record.family_id,
                authenticated_at_ms: record.issued_at_ms,
            },
        })
    }

    async fn rotate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<PlatformSessionRotation, PlatformAuthError> {
        let candidate = match self.codec.parse_and_digest(presented) {
            Ok(candidate) => candidate,
            Err(_) => return Ok(PlatformSessionRotation::Rejected),
        };
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let row = sqlx::query(
            "SELECT s.id,s.family_id,s.user_id,s.lookup_prefix,s.keyed_hash, \
                    (extract(epoch FROM s.issued_at)*1000)::bigint issued_at_ms, \
                    (extract(epoch FROM s.expires_at)*1000)::bigint expires_at_ms, \
                    (extract(epoch FROM s.used_at)*1000)::bigint used_at_ms,s.replaced_by, \
                    (extract(epoch FROM s.revoked_at)*1000)::bigint revoked_at_ms, \
                    (SELECT (extract(epoch FROM min(f.issued_at))*1000)::bigint \
                     FROM platform_sessions f WHERE f.family_id=s.family_id) authenticated_at_ms, \
                    u.email,(extract(epoch FROM u.disabled_at)*1000)::bigint user_disabled_at_ms \
             FROM platform_sessions s JOIN platform_users u ON u.id=s.user_id \
             WHERE s.lookup_prefix=$1 FOR UPDATE OF s,u",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else {
            return Ok(PlatformSessionRotation::Rejected);
        };
        let current = platform_session_from_row(&row)?;
        if !self.codec.verify_digest(&candidate.digest, &current.digest) {
            return Ok(PlatformSessionRotation::Rejected);
        }
        let disabled_at_ms: Option<i64> = row
            .try_get("user_disabled_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        if current.revoked_at_ms.is_some()
            || disabled_at_ms.is_some()
            || current.expires_at_ms <= now_ms
        {
            return Ok(PlatformSessionRotation::Rejected);
        }
        if current.used_at_ms.is_some() {
            revoke_platform_family(&mut transaction, current.family_id, now_ms, "session_reuse")
                .await?;
            transaction.commit().await.map_err(map_sqlx)?;
            return Ok(PlatformSessionRotation::ReuseDetected {
                family_id: current.family_id,
                user_id: current.user_id,
            });
        }
        let (plaintext, parts) = self
            .codec
            .issue()
            .map_err(|_| PlatformAuthError::Unavailable)?;
        let next = PlatformSessionRecord {
            id: TokenId::new(),
            family_id: current.family_id,
            user_id: current.user_id,
            prefix: parts.prefix,
            digest: parts.digest,
            issued_at_ms: now_ms,
            expires_at_ms: current.expires_at_ms,
            used_at_ms: None,
            replaced_by: None,
            revoked_at_ms: None,
        };
        insert_platform_session_transaction(&mut transaction, &next).await?;
        let updated = sqlx::query(
            "UPDATE platform_sessions SET used_at=to_timestamp($2::double precision/1000), \
             replaced_by=$3 WHERE id=$1 AND used_at IS NULL",
        )
        .bind(current.id.0)
        .bind(now_ms)
        .bind(next.id.0)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(PlatformAuthError::Unavailable);
        }
        let email: String = row
            .try_get("email")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        let authenticated_at_ms: i64 = row
            .try_get("authenticated_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(PlatformSessionRotation::Rotated {
            plaintext,
            session: Box::new(next),
            identity: Box::new(PlatformSessionIdentity {
                user_id: current.user_id,
                normalized_email: email,
                family_id: current.family_id,
                authenticated_at_ms,
            }),
        })
    }

    async fn authenticate(
        &self,
        presented: &str,
        now_ms: i64,
    ) -> Result<PlatformSessionIdentity, PlatformAuthError> {
        let candidate = self
            .codec
            .parse_and_digest(presented)
            .map_err(|_| PlatformAuthError::InvalidCredentials)?;
        let row = sqlx::query(
            "SELECT s.id,s.family_id,s.user_id,s.lookup_prefix,s.keyed_hash, \
                    (extract(epoch FROM s.issued_at)*1000)::bigint issued_at_ms, \
                    (extract(epoch FROM s.expires_at)*1000)::bigint expires_at_ms, \
                    (extract(epoch FROM s.used_at)*1000)::bigint used_at_ms,s.replaced_by, \
                    (extract(epoch FROM s.revoked_at)*1000)::bigint revoked_at_ms, \
                    (SELECT (extract(epoch FROM min(f.issued_at))*1000)::bigint \
                     FROM platform_sessions f WHERE f.family_id=s.family_id) authenticated_at_ms, \
                    u.email,(extract(epoch FROM u.disabled_at)*1000)::bigint user_disabled_at_ms \
             FROM platform_sessions s JOIN platform_users u ON u.id=s.user_id \
             WHERE s.lookup_prefix=$1",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or(PlatformAuthError::InvalidCredentials)?;
        let record = platform_session_from_row(&row)?;
        let email: String = row
            .try_get("email")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        let disabled_at_ms: Option<i64> = row
            .try_get("user_disabled_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        let authenticated_at_ms: i64 = row
            .try_get("authenticated_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        authenticate_record(
            &self.codec,
            &candidate,
            &record,
            email,
            disabled_at_ms,
            authenticated_at_ms,
            now_ms,
        )
    }

    async fn revoke(&self, presented: &str, now_ms: i64) -> Result<bool, PlatformAuthError> {
        let candidate = self
            .codec
            .parse_and_digest(presented)
            .map_err(|_| PlatformAuthError::InvalidSession)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        let row = sqlx::query(
            "SELECT family_id,keyed_hash FROM platform_sessions WHERE lookup_prefix=$1 FOR UPDATE",
        )
        .bind(&candidate.prefix)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let family_id: Uuid = row
            .try_get("family_id")
            .map_err(|_| PlatformAuthError::Unavailable)?;
        let digest = CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| PlatformAuthError::Unavailable)?,
        )
        .map_err(|_| PlatformAuthError::Unavailable)?;
        if !self.codec.verify_digest(&candidate.digest, &digest) {
            return Ok(false);
        }
        revoke_platform_family(&mut transaction, family_id, now_ms, "signed_out").await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(true)
    }
}

async fn insert_platform_session(
    pool: &PgPool,
    record: &PlatformSessionRecord,
) -> Result<(), PlatformAuthError> {
    sqlx::query(
        "INSERT INTO platform_sessions \
         (id,family_id,user_id,lookup_prefix,keyed_hash,issued_at,expires_at) \
         VALUES ($1,$2,$3,$4,$5,to_timestamp($6::double precision/1000), \
                 to_timestamp($7::double precision/1000))",
    )
    .bind(record.id.0)
    .bind(record.family_id)
    .bind(record.user_id.0)
    .bind(&record.prefix)
    .bind(record.digest.as_bytes().as_slice())
    .bind(record.issued_at_ms)
    .bind(record.expires_at_ms)
    .execute(pool)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

async fn insert_platform_session_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    record: &PlatformSessionRecord,
) -> Result<(), PlatformAuthError> {
    sqlx::query(
        "INSERT INTO platform_sessions \
         (id,family_id,user_id,lookup_prefix,keyed_hash,issued_at,expires_at) \
         VALUES ($1,$2,$3,$4,$5,to_timestamp($6::double precision/1000), \
                 to_timestamp($7::double precision/1000))",
    )
    .bind(record.id.0)
    .bind(record.family_id)
    .bind(record.user_id.0)
    .bind(&record.prefix)
    .bind(record.digest.as_bytes().as_slice())
    .bind(record.issued_at_ms)
    .bind(record.expires_at_ms)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

async fn revoke_platform_family(
    transaction: &mut Transaction<'_, Postgres>,
    family_id: Uuid,
    now_ms: i64,
    reason: &str,
) -> Result<(), PlatformAuthError> {
    sqlx::query(
        "UPDATE platform_sessions SET \
         revoked_at=COALESCE(revoked_at,to_timestamp($2::double precision/1000)), \
         revoke_reason=COALESCE(revoke_reason,$3) WHERE family_id=$1",
    )
    .bind(family_id)
    .bind(now_ms)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

fn platform_user_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<PlatformUserRecord, PlatformAuthError> {
    Ok(PlatformUserRecord {
        id: UserId(
            row.try_get("id")
                .map_err(|_| PlatformAuthError::Unavailable)?,
        ),
        normalized_email: row
            .try_get("email")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        password_hash: PasswordHash::parse(
            row.try_get("password_phc")
                .map_err(|_| PlatformAuthError::Unavailable)?,
        )
        .map_err(|_| PlatformAuthError::Unavailable)?,
        email_verified_at_ms: row
            .try_get("email_verified_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        disabled_at_ms: row
            .try_get("disabled_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
    })
}

fn platform_session_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PlatformSessionRecord, PlatformAuthError> {
    Ok(PlatformSessionRecord {
        id: TokenId(
            row.try_get("id")
                .map_err(|_| PlatformAuthError::Unavailable)?,
        ),
        family_id: row
            .try_get("family_id")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        user_id: UserId(
            row.try_get("user_id")
                .map_err(|_| PlatformAuthError::Unavailable)?,
        ),
        prefix: row
            .try_get("lookup_prefix")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        digest: CredentialDigest::from_slice(
            &row.try_get::<Vec<u8>, _>("keyed_hash")
                .map_err(|_| PlatformAuthError::Unavailable)?,
        )
        .map_err(|_| PlatformAuthError::Unavailable)?,
        issued_at_ms: row
            .try_get("issued_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        used_at_ms: row
            .try_get("used_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
        replaced_by: row
            .try_get::<Option<Uuid>, _>("replaced_by")
            .map_err(|_| PlatformAuthError::Unavailable)?
            .map(TokenId),
        revoked_at_ms: row
            .try_get("revoked_at_ms")
            .map_err(|_| PlatformAuthError::Unavailable)?,
    })
}

fn normalize_email(email: &str) -> Result<String, PlatformAuthError> {
    if email.len() > 254 || email.trim() != email || !email.is_ascii() {
        return Err(PlatformAuthError::InvalidEmail);
    }
    let (local, domain) = email
        .rsplit_once('@')
        .ok_or(PlatformAuthError::InvalidEmail)?;
    if local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || !domain.contains('.')
        || email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(PlatformAuthError::InvalidEmail);
    }
    Ok(format!(
        "{}@{}",
        local.to_ascii_lowercase(),
        domain.to_ascii_lowercase()
    ))
}

fn authenticate_record(
    codec: &OpaqueTokenCodec,
    candidate: &TokenParts,
    record: &PlatformSessionRecord,
    normalized_email: String,
    user_disabled_at_ms: Option<i64>,
    authenticated_at_ms: i64,
    now_ms: i64,
) -> Result<PlatformSessionIdentity, PlatformAuthError> {
    if !codec.verify_digest(&candidate.digest, &record.digest)
        || record.expires_at_ms <= now_ms
        || record.used_at_ms.is_some()
        || record.revoked_at_ms.is_some()
        || user_disabled_at_ms.is_some()
    {
        return Err(PlatformAuthError::InvalidCredentials);
    }
    Ok(PlatformSessionIdentity {
        user_id: record.user_id,
        normalized_email,
        family_id: record.family_id,
        authenticated_at_ms,
    })
}

fn map_sqlx(error: sqlx::Error) -> PlatformAuthError {
    if let sqlx::Error::Database(database) = &error
        && database.code().as_deref() == Some("23505")
    {
        return PlatformAuthError::AlreadyInitialized;
    }
    PlatformAuthError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_persists_hashed_rotating_platform_sessions() {
        let sql = include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql");
        assert!(sql.contains("CREATE TABLE platform_sessions"));
        assert!(sql.contains("keyed_hash bytea"));
        assert!(sql.contains("platform_sessions_replacement_fk"));
    }

    #[test]
    fn platform_email_normalization_is_strict() {
        assert_eq!(
            normalize_email("Admin@Example.test"),
            Ok("admin@example.test".into())
        );
        assert_eq!(
            normalize_email(" admin@example.test"),
            Err(PlatformAuthError::InvalidEmail)
        );
    }

    #[test]
    fn session_authentication_rejects_expiry_revocation_and_wrong_digest()
    -> Result<(), Box<dyn std::error::Error>> {
        let codec = OpaqueTokenCodec::new("platform", vec![8; 32])?;
        let (plaintext, parts) = codec.issue()?;
        let record = PlatformSessionRecord {
            id: TokenId::new(),
            family_id: Uuid::now_v7(),
            user_id: UserId::new(),
            prefix: parts.prefix,
            digest: parts.digest,
            issued_at_ms: 1,
            expires_at_ms: 100,
            used_at_ms: None,
            replaced_by: None,
            revoked_at_ms: None,
        };
        let candidate = codec.parse_and_digest(plaintext.expose())?;
        assert!(
            authenticate_record(&codec, &candidate, &record, "a@b.test".into(), None, 1, 50)
                .is_ok()
        );

        let mut expired = record.clone();
        expired.expires_at_ms = 50;
        assert_eq!(
            authenticate_record(&codec, &candidate, &expired, "a@b.test".into(), None, 1, 50),
            Err(PlatformAuthError::InvalidCredentials)
        );
        let mut revoked = record.clone();
        revoked.revoked_at_ms = Some(40);
        assert_eq!(
            authenticate_record(&codec, &candidate, &revoked, "a@b.test".into(), None, 1, 50),
            Err(PlatformAuthError::InvalidCredentials)
        );
        let wrong_codec = OpaqueTokenCodec::new("platform", vec![9; 32])?;
        let wrong_candidate = wrong_codec.parse_and_digest(plaintext.expose())?;
        assert_eq!(
            authenticate_record(
                &codec,
                &wrong_candidate,
                &record,
                "a@b.test".into(),
                None,
                1,
                50,
            ),
            Err(PlatformAuthError::InvalidCredentials)
        );
        Ok(())
    }

    #[test]
    fn rotated_identity_preserves_original_password_authentication_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let codec = OpaqueTokenCodec::new("platform", vec![8; 32])?;
        let (plaintext, parts) = codec.issue()?;
        let record = PlatformSessionRecord {
            id: TokenId::new(),
            family_id: Uuid::now_v7(),
            user_id: UserId::new(),
            prefix: parts.prefix,
            digest: parts.digest,
            issued_at_ms: 900_000,
            expires_at_ms: 2_000_000,
            used_at_ms: None,
            replaced_by: None,
            revoked_at_ms: None,
        };
        let identity = authenticate_record(
            &codec,
            &codec.parse_and_digest(plaintext.expose())?,
            &record,
            "a@b.test".into(),
            None,
            1,
            1_000_000,
        )?;
        assert_eq!(identity.authenticated_at_ms, 1);
        assert_ne!(identity.authenticated_at_ms, record.issued_at_ms);
        Ok(())
    }

    #[tokio::test]
    async fn database_rotation_preserves_session_family_password_authentication_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        let user_id = UserId::new();
        let email = format!("platform-rotation-{user_id}@example.test");
        let password = format!("Platform-rotation-test-{user_id}");
        let hasher = ffdb_auth::Argon2PasswordHasher::default();
        let password_hash = hasher.hash(SecretString::new(password.clone()))?;
        sqlx::query(
            "INSERT INTO platform_users \
             (id,email,password_phc,email_verified_at,created_at,updated_at) \
             VALUES ($1,$2,$3,now(),now(),now())",
        )
        .bind(user_id.0)
        .bind(&email)
        .bind(password_hash.as_phc())
        .execute(&pool)
        .await?;

        let service = PlatformAuthService::new(
            Arc::new(PgPlatformUserRepository::new(pool.clone())),
            Arc::new(PgPlatformSessionStore::new(pool.clone(), vec![17; 32])?),
            Arc::new(hasher),
        )?;
        let password_authenticated_at_ms = 1_000_000;
        let original = service
            .sign_in(
                &email,
                SecretString::new(password),
                password_authenticated_at_ms,
            )
            .await?;
        let rotated = service
            .refresh(original.plaintext.expose(), 1_900_000)
            .await?;
        let (rotated_token, rotation_identity) = match rotated {
            PlatformSessionRotation::Rotated {
                plaintext,
                identity,
                ..
            } => (plaintext, identity),
            other => return Err(format!("unexpected rotation result: {other:?}").into()),
        };
        assert_eq!(
            rotation_identity.authenticated_at_ms,
            password_authenticated_at_ms
        );
        let authenticated = service
            .authenticate(rotated_token.expose(), 2_000_000)
            .await?;
        assert_eq!(
            authenticated.authenticated_at_ms,
            password_authenticated_at_ms
        );
        assert_ne!(authenticated.authenticated_at_ms, 1_900_000);

        sqlx::query("DELETE FROM platform_sessions WHERE user_id=$1")
            .bind(user_id.0)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM platform_users WHERE id=$1")
            .bind(user_id.0)
            .execute(&pool)
            .await?;
        Ok(())
    }
}
