//! Production API credential composition.
//!
//! This is the bridge from PostgreSQL-backed auth state to the HTTP API's
//! `CredentialVerifier` boundary. Every lookup is scoped by the project from the
//! request path; unverified token claims never choose a project or key set.

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ffdb_protocol::{AuthContext, DeveloperPrincipal, DeveloperScope, ExecutionMode, ProjectId};
use sqlx::PgPool;

use crate::{
    AccountRepository, ApiKeyCodec, ApiKeyRepository, ApiKeyVerification, JwtError, JwtIssuer,
    PgAccountRepository, PgApiKeyRepository, SigningKeyStore,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CredentialVerificationError {
    #[error("credential is invalid")]
    Invalid,
    #[error("credential has expired")]
    Expired,
    #[error("credential belongs to another project")]
    WrongProject,
    #[error("credential lacks the required scope")]
    InsufficientScope,
    #[error("account is disabled")]
    Disabled,
    #[error("credential verifier is unavailable")]
    Unavailable,
}

/// Storage-independent credential state machine, useful for alternate durable
/// stores and deterministic tests. Production callers should normally construct
/// [`PgCredentialVerifier`] so API keys and account status come from PostgreSQL.
#[derive(Clone)]
pub struct CredentialVerifierService {
    api_keys: Arc<dyn ApiKeyRepository>,
    accounts: Arc<dyn AccountRepository>,
    signing_keys: Arc<dyn SigningKeyStore>,
    api_key_codec: ApiKeyCodec,
    jwt: JwtIssuer,
}

impl fmt::Debug for CredentialVerifierService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialVerifierService")
            .field("api_keys", &"dyn ApiKeyRepository")
            .field("accounts", &"dyn AccountRepository")
            .field("signing_keys", &"dyn SigningKeyStore")
            .field("api_key_codec", &"[REDACTED]")
            .field("jwt", &self.jwt)
            .finish()
    }
}

impl CredentialVerifierService {
    pub fn new(
        api_keys: Arc<dyn ApiKeyRepository>,
        accounts: Arc<dyn AccountRepository>,
        signing_keys: Arc<dyn SigningKeyStore>,
        api_key_pepper: Vec<u8>,
        issuer: String,
        audience: String,
    ) -> Result<Self, CredentialVerificationError> {
        let api_key_codec =
            ApiKeyCodec::new(api_key_pepper).map_err(|_| CredentialVerificationError::Invalid)?;
        let jwt =
            JwtIssuer::new(issuer, audience).map_err(|_| CredentialVerificationError::Invalid)?;
        Ok(Self {
            api_keys,
            accounts,
            signing_keys,
            api_key_codec,
            jwt,
        })
    }

    pub async fn verify_query_at(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        now_ms: i64,
    ) -> Result<ExecutionMode, CredentialVerificationError> {
        if bearer_token.starts_with("ffdb_dev_") {
            self.verify_developer_at(
                project_id,
                bearer_token,
                DeveloperScope::DatabaseQuery,
                now_ms,
            )
            .await
            .map(ExecutionMode::Developer)
        } else {
            self.verify_end_user_at(project_id, bearer_token, now_ms / 1_000)
                .await
                .map(ExecutionMode::EndUser)
        }
    }

    pub async fn verify_developer_at(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        required_scope: DeveloperScope,
        now_ms: i64,
    ) -> Result<DeveloperPrincipal, CredentialVerificationError> {
        let candidate = self
            .api_key_codec
            .candidate(bearer_token)
            .map_err(|_| CredentialVerificationError::Invalid)?;
        let record = self
            .api_keys
            .find_by_prefix(&candidate.prefix)
            .await
            .map_err(|_| CredentialVerificationError::Unavailable)?
            .ok_or(CredentialVerificationError::Invalid)?;

        // Verify possession before disclosing project or scope information.
        let ApiKeyVerification::Verified(principal) =
            self.api_key_codec.verify(&candidate, &record, now_ms)
        else {
            return Err(CredentialVerificationError::Invalid);
        };
        match record.project_id {
            Some(bound_project) if bound_project != project_id => {
                return Err(CredentialVerificationError::WrongProject);
            }
            None => {
                let target_organization = self
                    .api_keys
                    .project_organization(project_id)
                    .await
                    .map_err(|_| CredentialVerificationError::Unavailable)?;
                if target_organization != Some(record.organization_id) {
                    return Err(CredentialVerificationError::WrongProject);
                }
            }
            Some(_) => {}
        }
        if !record.scopes.contains(&required_scope) {
            return Err(CredentialVerificationError::InsufficientScope);
        }
        Ok(principal)
    }

    pub async fn verify_end_user_at(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        now_seconds: i64,
    ) -> Result<AuthContext, CredentialVerificationError> {
        self.verify_end_user_token_at(project_id, bearer_token, now_seconds)
            .await
            .map(|token| token.context().clone())
    }

    pub async fn verify_end_user_token_at(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        now_seconds: i64,
    ) -> Result<crate::VerifiedAccessToken, CredentialVerificationError> {
        self.verify_end_user_token_with_policy_at(project_id, bearer_token, now_seconds, true)
            .await
    }

    pub async fn verify_end_user_token_with_policy_at(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        now_seconds: i64,
        email_verification_required: bool,
    ) -> Result<crate::VerifiedAccessToken, CredentialVerificationError> {
        let keys = self
            .signing_keys
            .verification_keys(project_id)
            .await
            .map_err(map_jwt_error)?;
        let verified = self
            .jwt
            .verify(bearer_token, project_id, &keys, now_seconds)
            .map_err(map_jwt_error)?;
        let context = verified.context().clone();
        if context.project_id != project_id {
            return Err(CredentialVerificationError::WrongProject);
        }

        // A disabled account and credentials minted before a password change
        // are rejected immediately rather than waiting for the short JWT TTL.
        let user = self
            .accounts
            .find_by_id(project_id, context.subject)
            .await
            .map_err(|_| CredentialVerificationError::Unavailable)?
            .ok_or(CredentialVerificationError::Invalid)?;
        if user.disabled_at_ms.is_some() {
            return Err(CredentialVerificationError::Disabled);
        }
        if (email_verification_required && user.email_verified_at_ms.is_none())
            || verified.issued_at() < user.password_changed_at_ms.div_euclid(1_000)
        {
            return Err(CredentialVerificationError::Invalid);
        }
        Ok(verified)
    }
}

/// Concrete PostgreSQL-backed verifier for the public API. The signing-key
/// store is injected because encrypted key decryption belongs to the deployment
/// KMS adapter, not the database layer.
#[derive(Clone, Debug)]
pub struct PgCredentialVerifier {
    pool: PgPool,
    service: CredentialVerifierService,
}

impl PgCredentialVerifier {
    pub fn new(
        pool: PgPool,
        signing_keys: Arc<dyn SigningKeyStore>,
        api_key_pepper: Vec<u8>,
        issuer: String,
        audience: String,
    ) -> Result<Self, CredentialVerificationError> {
        let api_keys: Arc<dyn ApiKeyRepository> = Arc::new(PgApiKeyRepository::new(pool.clone()));
        let accounts: Arc<dyn AccountRepository> = Arc::new(PgAccountRepository::new(pool.clone()));
        Ok(Self {
            pool,
            service: CredentialVerifierService::new(
                api_keys,
                accounts,
                signing_keys,
                api_key_pepper,
                issuer,
                audience,
            )?,
        })
    }

    #[must_use]
    pub fn service(&self) -> &CredentialVerifierService {
        &self.service
    }

    pub async fn verify_query_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<ExecutionMode, CredentialVerificationError> {
        let now_ms = unix_time_ms()?;
        if bearer_token.starts_with("ffdb_dev_") {
            self.service
                .verify_developer_at(
                    project_id,
                    bearer_token,
                    DeveloperScope::DatabaseQuery,
                    now_ms,
                )
                .await
                .map(ExecutionMode::Developer)
        } else {
            self.verify_end_user_token_with_project_policy(project_id, bearer_token, now_ms / 1_000)
                .await
                .map(|token| ExecutionMode::EndUser(token.context().clone()))
        }
    }

    pub async fn verify_developer_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        required_scope: DeveloperScope,
    ) -> Result<DeveloperPrincipal, CredentialVerificationError> {
        let now_ms = unix_time_ms()?;
        self.service
            .verify_developer_at(project_id, bearer_token, required_scope, now_ms)
            .await
    }

    pub async fn verify_end_user_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<AuthContext, CredentialVerificationError> {
        let now_ms = unix_time_ms()?;
        let token = self
            .verify_end_user_token_with_project_policy(project_id, bearer_token, now_ms / 1_000)
            .await?;
        Ok(token.context().clone())
    }

    pub async fn verify_end_user_session_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<(AuthContext, Option<ffdb_protocol::SessionId>), CredentialVerificationError> {
        let now_ms = unix_time_ms()?;
        let verified = self
            .verify_end_user_token_with_project_policy(project_id, bearer_token, now_ms / 1_000)
            .await?;
        Ok((verified.context().clone(), verified.session_id()))
    }

    async fn verify_end_user_token_with_project_policy(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        now_seconds: i64,
    ) -> Result<crate::VerifiedAccessToken, CredentialVerificationError> {
        let verification_required: Option<bool> = sqlx::query_scalar(
            "SELECT email_verification_required FROM project_auth_settings WHERE project_id=$1",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CredentialVerificationError::Unavailable)?;
        let verified = self
            .service
            .verify_end_user_token_with_policy_at(
                project_id,
                bearer_token,
                now_seconds,
                verification_required.ok_or(CredentialVerificationError::Unavailable)?,
            )
            .await?;
        let session_id = verified
            .session_id()
            .ok_or(CredentialVerificationError::Invalid)?;
        if !live_session_is_active(
            &self.pool,
            project_id,
            verified.context().subject,
            session_id,
            now_seconds,
        )
        .await?
        {
            return Err(CredentialVerificationError::Invalid);
        }
        Ok(verified)
    }
}

/// Bind a signed access-token session to the same project and user and require
/// both the session and its refresh family to remain live. This is deliberately
/// checked on every production access-token verification so revocation is not
/// delayed until the JWT's independent expiry.
async fn live_session_is_active(
    pool: &PgPool,
    project_id: ProjectId,
    user_id: ffdb_protocol::UserId,
    session_id: ffdb_protocol::SessionId,
    now_seconds: i64,
) -> Result<bool, CredentialVerificationError> {
    sqlx::query_scalar(
        "SELECT EXISTS(\
           SELECT 1 FROM auth_sessions s \
           JOIN refresh_token_families f ON f.session_id=s.id \
           WHERE s.id=$1 AND s.project_id=$2 AND s.user_id=$3 \
             AND f.project_id=$2 AND f.user_id=$3 \
             AND s.revoked_at IS NULL AND f.revoked_at IS NULL \
             AND s.expires_at>to_timestamp($4::double precision))",
    )
    .bind(session_id.0)
    .bind(project_id.0)
    .bind(user_id.0)
    .bind(now_seconds)
    .fetch_one(pool)
    .await
    .map_err(|_| CredentialVerificationError::Unavailable)
}

fn unix_time_ms() -> Result<i64, CredentialVerificationError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CredentialVerificationError::Unavailable)?
        .as_millis();
    i64::try_from(millis).map_err(|_| CredentialVerificationError::Unavailable)
}

fn map_jwt_error(error: JwtError) -> CredentialVerificationError {
    match error {
        JwtError::TimeInvalid => CredentialVerificationError::Expired,
        JwtError::KeyStoreUnavailable | JwtError::KeyGeneration => {
            CredentialVerificationError::Unavailable
        }
        JwtError::Malformed
        | JwtError::UnsupportedAlgorithm
        | JwtError::UnknownKey
        | JwtError::InvalidSignature
        | JwtError::InvalidClaims
        | JwtError::Encoding => CredentialVerificationError::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ffdb_protocol::{ApiKeyId, OrganizationId, TokenId, UserId};
    use serde_json::Map;
    use uuid::Uuid;

    use crate::{
        AccessTokenClaims, ApiKeyRecord, CredentialDigest, InMemoryAccountRepository,
        ProjectSigner, SecretToken, VerificationKey, VerificationKeyStatus,
    };

    #[derive(Debug)]
    struct FixedApiKeys {
        record: Option<ApiKeyRecord>,
        project_organization: Option<(ProjectId, OrganizationId)>,
    }

    #[async_trait]
    impl ApiKeyRepository for FixedApiKeys {
        async fn insert(
            &self,
            _record: &ApiKeyRecord,
            _name: &str,
            _created_by: UserId,
            _created_at_ms: i64,
        ) -> Result<(), crate::AccountError> {
            Err(crate::AccountError::Unavailable)
        }

        async fn find_by_prefix(
            &self,
            prefix: &str,
        ) -> Result<Option<ApiKeyRecord>, crate::AccountError> {
            Ok(self
                .record
                .as_ref()
                .filter(|record| record.prefix == prefix)
                .cloned())
        }

        async fn project_organization(
            &self,
            project_id: ProjectId,
        ) -> Result<Option<OrganizationId>, crate::AccountError> {
            Ok(self
                .project_organization
                .filter(|(allowed_project, _)| allowed_project == &project_id)
                .map(|(_, organization_id)| organization_id))
        }

        async fn revoke(&self, _id: ApiKeyId, _now_ms: i64) -> Result<bool, crate::AccountError> {
            Err(crate::AccountError::Unavailable)
        }
    }

    #[derive(Debug)]
    struct FixedKeys(Vec<VerificationKey>);

    #[async_trait]
    impl SigningKeyStore for FixedKeys {
        async fn active_signer(&self, _project_id: ProjectId) -> Result<ProjectSigner, JwtError> {
            Err(JwtError::KeyStoreUnavailable)
        }

        async fn verification_keys(
            &self,
            project_id: ProjectId,
        ) -> Result<Vec<VerificationKey>, JwtError> {
            Ok(self
                .0
                .iter()
                .filter(|key| key.project_id == project_id)
                .cloned()
                .collect())
        }
    }

    #[tokio::test]
    async fn project_bound_api_key_cannot_cross_project() -> Result<(), Box<dyn std::error::Error>>
    {
        let project = ProjectId::new();
        let foreign = ProjectId::new();
        let codec = ApiKeyCodec::new(vec![3; 32])?;
        let issue = codec.issue(
            OrganizationId::new(),
            Some(project),
            vec![DeveloperScope::DatabaseQuery],
        )?;
        let service = CredentialVerifierService::new(
            Arc::new(FixedApiKeys {
                record: Some(issue.record),
                project_organization: None,
            }),
            Arc::new(InMemoryAccountRepository::default()),
            Arc::new(FixedKeys(Vec::new())),
            vec![3; 32],
            "https://auth.ffdb.test".into(),
            "ffdb".into(),
        )?;
        assert_eq!(
            service
                .verify_developer_at(
                    foreign,
                    issue.plaintext.expose(),
                    DeveloperScope::DatabaseQuery,
                    0,
                )
                .await,
            Err(CredentialVerificationError::WrongProject)
        );
        Ok(())
    }

    #[tokio::test]
    async fn organization_key_cannot_cross_tenant() -> Result<(), Box<dyn std::error::Error>> {
        let target_project = ProjectId::new();
        let key_organization = OrganizationId::new();
        let target_organization = OrganizationId::new();
        let codec = ApiKeyCodec::new(vec![5; 32])?;
        let issue = codec.issue(key_organization, None, vec![DeveloperScope::DatabaseQuery])?;
        let service = CredentialVerifierService::new(
            Arc::new(FixedApiKeys {
                record: Some(issue.record),
                project_organization: Some((target_project, target_organization)),
            }),
            Arc::new(InMemoryAccountRepository::default()),
            Arc::new(FixedKeys(Vec::new())),
            vec![5; 32],
            "https://auth.ffdb.test".into(),
            "ffdb".into(),
        )?;
        assert_eq!(
            service
                .verify_developer_at(
                    target_project,
                    issue.plaintext.expose(),
                    DeveloperScope::DatabaseQuery,
                    0,
                )
                .await,
            Err(CredentialVerificationError::WrongProject)
        );
        Ok(())
    }

    #[tokio::test]
    async fn password_change_uses_jwt_precision_and_invalidates_older_access_token()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = ProjectId::new();
        let user_id = UserId::new();
        let signer = ProjectSigner::generate(project, "current-key-01".into())?;
        let key = signer.verification_key(VerificationKeyStatus::Active, 0, None);
        let claims = AccessTokenClaims {
            iss: "https://auth.ffdb.test".into(),
            aud: "ffdb".into(),
            sub: user_id,
            project_id: project,
            jti: TokenId::new(),
            sid: None,
            iat: 10,
            nbf: 10,
            exp: 100,
            role: "authenticated".into(),
            claims: Map::new(),
        };
        let token: SecretToken = signer.sign(&claims)?;
        let accounts = InMemoryAccountRepository::default();
        accounts
            .insert(crate::AuthUserRecord {
                id: user_id,
                project_id: project,
                normalized_email: "person@example.test".into(),
                password_hash: crate::PasswordHash::parse(
                    "$argon2id$v=19$m=8192,t=1,p=1$c2FsdHNhbHQ$M9tsHU7YoSkWD/Raw21qifhOvtsA07BxNaD6hn3oUw8"
                        .into(),
                )?,
                role: "authenticated".into(),
                custom_claims: Map::new(),
                email_verified_at_ms: Some(1),
                disabled_at_ms: None,
                password_changed_at_ms: 10_999,
                created_at_ms: 1,
            })
            .await?;
        let service = CredentialVerifierService::new(
            Arc::new(FixedApiKeys {
                record: None,
                project_organization: None,
            }),
            Arc::new(accounts.clone()),
            Arc::new(FixedKeys(vec![key])),
            vec![4; 32],
            "https://auth.ffdb.test".into(),
            "ffdb".into(),
        )?;
        assert!(
            service
                .verify_end_user_at(project, token.expose(), 20)
                .await
                .is_ok(),
            "a token issued later in the same represented second must remain valid"
        );
        accounts
            .set_password(
                project,
                user_id,
                crate::PasswordHash::parse(
                    "$argon2id$v=19$m=8192,t=1,p=1$c2FsdHNhbHQ$M9tsHU7YoSkWD/Raw21qifhOvtsA07BxNaD6hn3oUw8"
                        .into(),
                )?,
                11_000,
            )
            .await?;
        assert_eq!(
            service
                .verify_end_user_at(project, token.expose(), 20)
                .await,
            Err(CredentialVerificationError::Invalid)
        );
        Ok(())
    }

    #[tokio::test]
    async fn project_policy_can_allow_an_unverified_access_token()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = ProjectId::new();
        let user_id = UserId::new();
        let signer = ProjectSigner::generate(project, "policy-key-01".into())?;
        let key = signer.verification_key(VerificationKeyStatus::Active, 0, None);
        let claims = AccessTokenClaims {
            iss: "https://auth.ffdb.test".into(),
            aud: "ffdb".into(),
            sub: user_id,
            project_id: project,
            jti: TokenId::new(),
            sid: None,
            iat: 10,
            nbf: 10,
            exp: 100,
            role: "authenticated".into(),
            claims: Map::new(),
        };
        let token = signer.sign(&claims)?;
        let accounts = InMemoryAccountRepository::default();
        accounts
            .insert(crate::AuthUserRecord {
                id: user_id,
                project_id: project,
                normalized_email: "unverified@example.test".into(),
                password_hash: crate::PasswordHash::parse(
                    "$argon2id$v=19$m=8192,t=1,p=1$c2FsdHNhbHQ$M9tsHU7YoSkWD/Raw21qifhOvtsA07BxNaD6hn3oUw8"
                        .into(),
                )?,
                role: "authenticated".into(),
                custom_claims: Map::new(),
                email_verified_at_ms: None,
                disabled_at_ms: None,
                password_changed_at_ms: 1,
                created_at_ms: 1,
            })
            .await?;
        let service = CredentialVerifierService::new(
            Arc::new(FixedApiKeys {
                record: None,
                project_organization: None,
            }),
            Arc::new(accounts),
            Arc::new(FixedKeys(vec![key])),
            vec![4; 32],
            "https://auth.ffdb.test".into(),
            "ffdb".into(),
        )?;
        assert!(
            service
                .verify_end_user_token_with_policy_at(project, token.expose(), 20, false)
                .await
                .is_ok()
        );
        assert!(matches!(
            service
                .verify_end_user_token_with_policy_at(project, token.expose(), 20, true)
                .await,
            Err(CredentialVerificationError::Invalid)
        ));
        Ok(())
    }

    #[test]
    fn credential_digest_is_not_constructed_from_plaintext() {
        assert!(CredentialDigest::from_slice(&[0; 31]).is_err());
    }

    #[tokio::test]
    async fn postgres_live_session_binding_rejects_revocation_reuse_and_cross_tenant_tamper()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        let organization_id = OrganizationId::new();
        let project_id = ProjectId::new();
        let database_id = ffdb_protocol::DatabaseId::new();
        let route_id = Uuid::now_v7();
        let node_id = ffdb_protocol::NodeId::new();
        let user_id = UserId::new();
        let session_id = ffdb_protocol::SessionId::new();
        let family_id = Uuid::now_v7();
        let mut transaction = pool.begin().await?;
        sqlx::query("INSERT INTO organizations (id,slug,display_name) VALUES ($1,$2,$3)")
            .bind(organization_id.0)
            .bind(format!(
                "auth-session-{}",
                &organization_id.to_string()[..12]
            ))
            .bind("Auth session test")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO nodes (id,name) VALUES ($1,$2)")
            .bind(node_id.0)
            .bind(format!("auth-session-{}", &node_id.to_string()[..12]))
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO projects (id,organization_id,database_id,slug,display_name,lifecycle_state) \
             VALUES ($1,$2,$3,$4,$5,'active')",
        )
        .bind(project_id.0)
        .bind(organization_id.0)
        .bind(database_id.0)
        .bind(format!("auth-session-{}", &project_id.to_string()[..12]))
        .bind("Auth session project")
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO project_databases (id,project_id,route_id,lifecycle_state) \
             VALUES ($1,$2,$3,'active')",
        )
        .bind(database_id.0)
        .bind(project_id.0)
        .bind(route_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO database_routes (id,project_id,database_id,node_id,generation) \
             VALUES ($1,$2,$3,$4,1)",
        )
        .bind(route_id)
        .bind(project_id.0)
        .bind(database_id.0)
        .bind(node_id.0)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO auth_users (id,project_id,email,password_phc,email_verified_at,password_changed_at) \
             VALUES ($1,$2,$3,$4,to_timestamp(1),to_timestamp(1))",
        )
        .bind(user_id.0)
        .bind(project_id.0)
        .bind(format!("{}@example.test", user_id))
        .bind("test-only-phc")
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO auth_sessions (id,project_id,user_id,expires_at) \
             VALUES ($1,$2,$3,to_timestamp(200))",
        )
        .bind(session_id.0)
        .bind(project_id.0)
        .bind(user_id.0)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO refresh_token_families (id,project_id,user_id,session_id) \
             VALUES ($1,$2,$3,$4)",
        )
        .bind(family_id)
        .bind(project_id.0)
        .bind(user_id.0)
        .bind(session_id.0)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        assert!(live_session_is_active(&pool, project_id, user_id, session_id, 100).await?);
        assert!(!live_session_is_active(&pool, ProjectId::new(), user_id, session_id, 100).await?);
        assert!(!live_session_is_active(&pool, project_id, UserId::new(), session_id, 100).await?);

        sqlx::query("UPDATE auth_sessions SET revoked_at=to_timestamp(101) WHERE id=$1")
            .bind(session_id.0)
            .execute(&pool)
            .await?;
        assert!(!live_session_is_active(&pool, project_id, user_id, session_id, 101).await?);
        sqlx::query("UPDATE auth_sessions SET revoked_at=NULL WHERE id=$1")
            .bind(session_id.0)
            .execute(&pool)
            .await?;
        sqlx::query(
            "UPDATE refresh_token_families SET revoked_at=to_timestamp(102),revoke_reason='refresh_reuse' \
             WHERE id=$1",
        )
        .bind(family_id)
        .execute(&pool)
        .await?;
        assert!(!live_session_is_active(&pool, project_id, user_id, session_id, 102).await?);
        sqlx::query(
            "UPDATE refresh_token_families SET revoked_at=NULL,revoke_reason=NULL WHERE id=$1",
        )
        .bind(family_id)
        .execute(&pool)
        .await?;
        assert!(!live_session_is_active(&pool, project_id, user_id, session_id, 200).await?);
        Ok(())
    }
}
