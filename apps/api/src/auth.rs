use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_audit::{ActorKind, AuditDraft, AuditOutcome};
use ffdb_auth::{
    AccessTokenSessionPolicy, AccountError, AccountRepository as _, AccountService,
    Argon2PasswordHasher, AuthUserRecord, AuthenticatedUser, JwtIssuer, OneTimePurpose,
    OneTimeStoreError, OneTimeToken, PasswordHash, PasswordHasher as _, PgAccountRepository,
    PgOneTimeTokenStore, PgRefreshStore, RefreshRotation, RefreshStoreError, SecretString,
    SecretToken, SigningKeyStore,
};
use ffdb_email::{EmailEnqueueRequest, EmailError, PgEmailService, ScalarValue, TemplateKind};
use ffdb_protocol::{
    AuthContext, AuthSettings, AuthTokenPair, AuthUser, DeveloperScope, ExecutionMode,
    PROTOCOL_VERSION, PasswordChangeRequest, PasswordResetCompleteRequest,
    PasswordResetStartRequest, ProjectId, RefreshRequest, RegisterRequest, RegisterResponse,
    RequestId, SensitiveString, SessionId, SessionSummary, SetAuthUserDisabledRequest,
    SignInRequest, UserId, VerifyEmailRequest,
};
use ffdb_rate_limits::RateDimension;
use serde::Deserialize;
use sqlx::{PgPool, Row};
use tracing::warn;
use url::Url;
use uuid::Uuid;

use super::{ApiError, ApiState, bearer, now_ms};

#[async_trait]
pub trait AuthEmailDispatcher: Send + Sync {
    async fn enqueue_verification(
        &self,
        project_id: ProjectId,
        recipient: &str,
        token: &OneTimeToken,
        now_ms: i64,
    ) -> Result<(), EmailError>;

    async fn enqueue_password_reset(
        &self,
        project_id: ProjectId,
        recipient: &str,
        token: &OneTimeToken,
        now_ms: i64,
    ) -> Result<(), EmailError>;
}

/// Queues authentication mail in the encrypted PostgreSQL outbox. Request
/// handlers never wait on the external delivery provider.
#[derive(Clone)]
pub struct OutboxAuthEmailDispatcher {
    email: Arc<PgEmailService>,
    from_address: String,
    public_base_url: Url,
}

impl std::fmt::Debug for OutboxAuthEmailDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutboxAuthEmailDispatcher")
            .field("email", &self.email)
            .field("from_address", &self.from_address)
            .field("public_base_url", &self.public_base_url)
            .finish()
    }
}

impl OutboxAuthEmailDispatcher {
    #[must_use]
    pub fn new(email: Arc<PgEmailService>, from_address: String, public_base_url: Url) -> Self {
        Self {
            email,
            from_address,
            public_base_url,
        }
    }

    async fn enqueue(
        &self,
        project_id: ProjectId,
        recipient: &str,
        token: &OneTimeToken,
        now_ms: i64,
        kind: TemplateKind,
    ) -> Result<(), EmailError> {
        let action_url = action_url(
            &self.public_base_url,
            project_id,
            token.plaintext.expose(),
            kind,
        );
        let variables = BTreeMap::from([
            (
                "project_name".to_owned(),
                ScalarValue::String(project_id.to_string()),
            ),
            ("action_url".to_owned(), ScalarValue::String(action_url)),
            (
                "expires_in".to_owned(),
                ScalarValue::String("30 minutes".to_owned()),
            ),
        ]);
        self.email
            .enqueue(EmailEnqueueRequest {
                project_id: project_id.0,
                kind,
                recipient: recipient.to_owned(),
                from: self.from_address.clone(),
                reply_to: None,
                variables,
                idempotency_key: format!("auth-{}", token.record.id),
                now_ms,
            })
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl AuthEmailDispatcher for OutboxAuthEmailDispatcher {
    async fn enqueue_verification(
        &self,
        project_id: ProjectId,
        recipient: &str,
        token: &OneTimeToken,
        now_ms: i64,
    ) -> Result<(), EmailError> {
        self.enqueue(
            project_id,
            recipient,
            token,
            now_ms,
            TemplateKind::EmailVerification,
        )
        .await
    }

    async fn enqueue_password_reset(
        &self,
        project_id: ProjectId,
        recipient: &str,
        token: &OneTimeToken,
        now_ms: i64,
    ) -> Result<(), EmailError> {
        self.enqueue(
            project_id,
            recipient,
            token,
            now_ms,
            TemplateKind::PasswordReset,
        )
        .await
    }
}

#[derive(Clone)]
pub struct ProjectAuthState {
    pool: PgPool,
    accounts: Arc<PgAccountRepository>,
    one_time_tokens: Arc<PgOneTimeTokenStore>,
    refresh_tokens: Arc<PgRefreshStore>,
    password_hasher: Arc<Argon2PasswordHasher>,
    dummy_password_hash: PasswordHash,
    signing_keys: Arc<dyn SigningKeyStore>,
    jwt: JwtIssuer,
    email: Arc<dyn AuthEmailDispatcher>,
}

impl std::fmt::Debug for ProjectAuthState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectAuthState")
            .field("pool", &self.pool)
            .field("accounts", &self.accounts)
            .field("one_time_tokens", &self.one_time_tokens)
            .field("refresh_tokens", &self.refresh_tokens)
            .field("dummy_password_hash", &"[REDACTED]")
            .field("signing_keys", &"dyn SigningKeyStore")
            .field("jwt", &self.jwt)
            .field("email", &"dyn AuthEmailDispatcher")
            .finish_non_exhaustive()
    }
}

impl ProjectAuthState {
    pub fn new(
        pool: PgPool,
        one_time_pepper: Vec<u8>,
        refresh_pepper: Vec<u8>,
        signing_keys: Arc<dyn SigningKeyStore>,
        issuer: String,
        audience: String,
        email: Arc<dyn AuthEmailDispatcher>,
    ) -> Result<Self, ProjectAuthSetupError> {
        let password_hasher = Arc::new(Argon2PasswordHasher::default());
        let dummy_password_hash = password_hasher
            .hash(SecretString::new(
                "ffdb-dummy-password-verification-work-factor".into(),
            ))
            .map_err(|_| ProjectAuthSetupError)?;
        Ok(Self {
            pool: pool.clone(),
            accounts: Arc::new(PgAccountRepository::new(pool.clone())),
            one_time_tokens: Arc::new(
                PgOneTimeTokenStore::new(pool.clone(), one_time_pepper)
                    .map_err(|_| ProjectAuthSetupError)?,
            ),
            refresh_tokens: Arc::new(
                PgRefreshStore::new(pool, refresh_pepper).map_err(|_| ProjectAuthSetupError)?,
            ),
            password_hasher,
            dummy_password_hash,
            signing_keys,
            jwt: JwtIssuer::new(issuer, audience).map_err(|_| ProjectAuthSetupError)?,
            email,
        })
    }

    async fn settings(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectAuthSettings, ProjectAuthOperationError> {
        let row = sqlx::query(
            "SELECT registration_enabled,email_verification_required, \
                    access_token_ttl_seconds,refresh_token_ttl_seconds,password_min_length \
             FROM project_auth_settings WHERE project_id=$1",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ProjectAuthOperationError::Unavailable)?
        .ok_or(ProjectAuthOperationError::Unavailable)?;
        let access_ttl: i32 = row
            .try_get("access_token_ttl_seconds")
            .map_err(|_| ProjectAuthOperationError::Unavailable)?;
        let refresh_ttl: i32 = row
            .try_get("refresh_token_ttl_seconds")
            .map_err(|_| ProjectAuthOperationError::Unavailable)?;
        let password_min: i32 = row
            .try_get("password_min_length")
            .map_err(|_| ProjectAuthOperationError::Unavailable)?;
        let settings = ProjectAuthSettings {
            registration_enabled: row
                .try_get("registration_enabled")
                .map_err(|_| ProjectAuthOperationError::Unavailable)?,
            email_verification_required: row
                .try_get("email_verification_required")
                .map_err(|_| ProjectAuthOperationError::Unavailable)?,
            access_token_ttl_seconds: u32::try_from(access_ttl)
                .map_err(|_| ProjectAuthOperationError::Unavailable)?,
            refresh_token_ttl_seconds: u32::try_from(refresh_ttl)
                .map_err(|_| ProjectAuthOperationError::Unavailable)?,
            password_min_length: u16::try_from(password_min)
                .map_err(|_| ProjectAuthOperationError::Unavailable)?,
        };
        settings.validate()?;
        Ok(settings)
    }

    fn account_service(&self, project_id: ProjectId) -> Result<AccountService, AccountError> {
        Ok(AccountService::with_dummy_hash(
            project_id,
            self.accounts.clone(),
            self.password_hasher.clone(),
            self.one_time_tokens.clone(),
            self.dummy_password_hash.clone(),
        ))
    }

    async fn access_token(
        &self,
        user: &AuthenticatedUser,
        session_id: SessionId,
        now_ms: i64,
        ttl_seconds: u32,
    ) -> Result<SecretToken, ProjectAuthOperationError> {
        let signer = self
            .signing_keys
            .active_signer(user.project_id)
            .await
            .map_err(|_| ProjectAuthOperationError::Unavailable)?;
        let claims = self
            .jwt
            .claims_with_session_policy(
                user.project_id,
                user.id,
                user.role.clone(),
                user.custom_claims.clone(),
                AccessTokenSessionPolicy {
                    session_id: Some(session_id),
                    now_seconds: now_ms / 1_000,
                    ttl_seconds: i64::from(ttl_seconds),
                },
            )
            .map_err(|_| ProjectAuthOperationError::Unavailable)?;
        signer
            .sign(&claims)
            .map_err(|_| ProjectAuthOperationError::Unavailable)
    }

    async fn issue_session(
        &self,
        user: AuthenticatedUser,
        now_ms: i64,
        settings: ProjectAuthSettings,
    ) -> Result<AuthTokenPair, ProjectAuthOperationError> {
        let refresh = self
            .refresh_tokens
            .issue_session_with_ttl(
                user.project_id,
                user.id,
                now_ms,
                settings.refresh_token_ttl_seconds,
            )
            .await
            .map_err(ProjectAuthOperationError::Refresh)?;
        let access_token = match self
            .access_token(
                &user,
                refresh.session.id,
                now_ms,
                settings.access_token_ttl_seconds,
            )
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let _ignored = self
                    .refresh_tokens
                    .revoke_user_session(user.project_id, user.id, refresh.session.id, now_ms)
                    .await;
                return Err(error);
            }
        };
        Ok(token_pair(
            access_token,
            refresh.plaintext,
            refresh.session.id,
            user,
            settings.access_token_ttl_seconds,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
struct ProjectAuthSettings {
    registration_enabled: bool,
    email_verification_required: bool,
    access_token_ttl_seconds: u32,
    refresh_token_ttl_seconds: u32,
    password_min_length: u16,
}

impl ProjectAuthSettings {
    fn is_valid(self) -> bool {
        (60..=900).contains(&self.access_token_ttl_seconds)
            && (3_600..=7_776_000).contains(&self.refresh_token_ttl_seconds)
            && (8..=128).contains(&self.password_min_length)
    }

    fn validate(self) -> Result<(), ProjectAuthOperationError> {
        if !self.is_valid() {
            return Err(ProjectAuthOperationError::Unavailable);
        }
        Ok(())
    }

    fn applying(self, update: &UpdateAuthSettingsRequest) -> Self {
        Self {
            registration_enabled: update
                .registration_enabled
                .unwrap_or(self.registration_enabled),
            email_verification_required: update
                .email_verification_required
                .unwrap_or(self.email_verification_required),
            access_token_ttl_seconds: update
                .access_token_ttl_seconds
                .unwrap_or(self.access_token_ttl_seconds),
            refresh_token_ttl_seconds: update
                .refresh_token_ttl_seconds
                .unwrap_or(self.refresh_token_ttl_seconds),
            password_min_length: update
                .password_min_length
                .unwrap_or(self.password_min_length),
        }
    }
}

impl From<ProjectAuthSettings> for AuthSettings {
    fn from(value: ProjectAuthSettings) -> Self {
        Self {
            registration_enabled: value.registration_enabled,
            email_verification_required: value.email_verification_required,
            access_token_ttl_seconds: value.access_token_ttl_seconds,
            refresh_token_ttl_seconds: value.refresh_token_ttl_seconds,
            password_min_length: value.password_min_length,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateAuthSettingsRequest {
    registration_enabled: Option<bool>,
    email_verification_required: Option<bool>,
    access_token_ttl_seconds: Option<u32>,
    refresh_token_ttl_seconds: Option<u32>,
    password_min_length: Option<u16>,
}

#[derive(Clone, Copy, Debug)]
pub struct ProjectAuthSetupError;

impl std::fmt::Display for ProjectAuthSetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("project authentication configuration is invalid")
    }
}

impl std::error::Error for ProjectAuthSetupError {}

#[derive(Debug)]
enum ProjectAuthOperationError {
    Account(AccountError),
    Refresh(RefreshStoreError),
    Unavailable,
}

pub(crate) async fn register(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<RegisterRequest>,
) -> Response {
    let (auth, project_id, settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::Anonymous,
        "auth.register",
    )
    .await
    {
        return response;
    }
    if !settings.registration_enabled {
        terminal_auth_audit(
            &state,
            project_id,
            request_id,
            AuthAuditActor::Anonymous,
            "auth.register",
            None,
            AuditOutcome::Denied,
        )
        .await;
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "auth.registration_disabled",
            "registration is disabled for this project",
            request_id,
        )
        .into_response();
    }
    let now = now_ms();
    let registration_usage = match &state.usage_metering {
        Some(metering) => match metering
            .prepare_registration(
                project_id,
                request_id.0,
                &payload.email.to_ascii_lowercase(),
                now,
            )
            .await
        {
            Ok(value) => Some(value),
            Err(error) => return super::metering_error(error, request_id).into_response(),
        },
        None => None,
    };
    let service = match auth.account_service(project_id) {
        Ok(value) => value,
        Err(error) => {
            if let (Some(metering), Some(prepared)) = (&state.usage_metering, &registration_usage) {
                metering.release_registration(prepared, now);
            }
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.register",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    let user = match service
        .register_with_policy(
            &payload.email,
            SecretString::new(payload.password.into_inner()),
            payload.custom_claims,
            settings.email_verification_required,
            settings.password_min_length,
            now,
        )
        .await
    {
        Ok(user) => {
            if let (Some(metering), Some(prepared)) = (&state.usage_metering, &registration_usage)
                && let Err(error) = metering.settle_registration(prepared, now)
            {
                return super::metering_error(error, request_id).into_response();
            }
            user
        }
        Err(AccountError::EmailInUse) => {
            if let (Some(metering), Some(prepared)) = (&state.usage_metering, &registration_usage) {
                metering.release_registration(prepared, now);
            }
            if !settings.email_verification_required {
                terminal_auth_audit(
                    &state,
                    project_id,
                    request_id,
                    AuthAuditActor::Anonymous,
                    "auth.register",
                    None,
                    AuditOutcome::Denied,
                )
                .await;
                return accepted_registration(false);
            }
            // The response remains identical. Reissuing to the submitted
            // mailbox lets a transient provider failure be retried without
            // revealing whether the account already existed.
            match service
                .issue_verification_for_email(&payload.email, now)
                .await
            {
                Ok(Some(token)) => {
                    return deliver_verification(
                        &state,
                        &auth,
                        project_id,
                        &payload.email,
                        &token,
                        now,
                        request_id,
                    )
                    .await;
                }
                Ok(None) => {
                    terminal_auth_audit(
                        &state,
                        project_id,
                        request_id,
                        AuthAuditActor::Anonymous,
                        "auth.register",
                        None,
                        AuditOutcome::Denied,
                    )
                    .await;
                    return accepted_registration(true);
                }
                Err(error) => {
                    return audited_operation_error(
                        &state,
                        project_id,
                        request_id,
                        AuthAuditActor::Anonymous,
                        "auth.register",
                        ProjectAuthOperationError::Account(error),
                    )
                    .await;
                }
            }
        }
        Err(error) => {
            if let (Some(metering), Some(prepared)) = (&state.usage_metering, &registration_usage) {
                metering.release_registration(prepared, now);
            }
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.register",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    if !settings.email_verification_required {
        terminal_auth_audit(
            &state,
            project_id,
            request_id,
            AuthAuditActor::User(user.id),
            "auth.register",
            Some(user.id.0),
            AuditOutcome::Success,
        )
        .await;
        return accepted_registration(false);
    }
    let token = match service.issue_verification(user.id, now).await {
        Ok(value) => value,
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.register",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    deliver_verification(
        &state,
        &auth,
        project_id,
        &user.normalized_email,
        &token,
        now,
        request_id,
    )
    .await
}

async fn deliver_verification(
    state: &ApiState,
    auth: &ProjectAuthState,
    project_id: ProjectId,
    recipient: &str,
    token: &OneTimeToken,
    now_ms: i64,
    request_id: RequestId,
) -> Response {
    match auth
        .email
        .enqueue_verification(project_id, recipient, token, now_ms)
        .await
    {
        Ok(()) => {
            terminal_auth_audit(
                state,
                project_id,
                request_id,
                AuthAuditActor::User(token.record.user_id),
                "auth.register",
                Some(token.record.user_id.0),
                AuditOutcome::Success,
            )
            .await;
            accepted_registration(true)
        }
        Err(_) => {
            warn!(%project_id, %request_id, "verification email enqueue failed");
            terminal_auth_audit(
                state,
                project_id,
                request_id,
                AuthAuditActor::User(token.record.user_id),
                "auth.register",
                Some(token.record.user_id.0),
                AuditOutcome::Failure,
            )
            .await;
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth.email_unavailable",
                "verification enqueue is temporarily unavailable",
                request_id,
            )
            .into_response()
        }
    }
}

pub(crate) async fn verify_email(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<VerifyEmailRequest>,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let now = now_ms();
    let user_id = match auth
        .one_time_tokens
        .identify_for_project(
            project_id,
            payload.token.expose(),
            OneTimePurpose::EmailVerification,
            now,
        )
        .await
    {
        Ok(Some(user_id)) => user_id,
        Ok(None) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.email.verify",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return invalid_token(request_id);
        }
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.email.verify",
                ProjectAuthOperationError::Account(AccountError::OneTime(error)),
            )
            .await;
        }
    };
    if let Err(response) = enforce_user_rate(&state, user_id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(user_id),
        "auth.email.verify",
    )
    .await
    {
        return response;
    }
    match auth
        .one_time_tokens
        .verify_email_for_project(project_id, payload.token.expose(), now)
        .await
    {
        Ok(verified_user_id) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(verified_user_id),
                "auth.email.verify",
                Some(verified_user_id.0),
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            let outcome = if matches!(error, OneTimeStoreError::Unavailable) {
                AuditOutcome::Failure
            } else {
                AuditOutcome::Denied
            };
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.email.verify",
                None,
                outcome,
            )
            .await;
            one_time_error(error, request_id)
        }
    }
}

pub(crate) async fn sign_in(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<SignInRequest>,
) -> Response {
    let (auth, project_id, settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let service = match auth.account_service(project_id) {
        Ok(value) => value,
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.sign_in",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    let user = match service
        .authenticate_with_verification_policy(
            &payload.email,
            SecretString::new(payload.password.into_inner()),
            settings.email_verification_required,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.sign_in",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    if let Err(response) = enforce_user_rate(&state, user.id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(user.id),
        "auth.sign_in",
    )
    .await
    {
        return response;
    }
    let user_id = user.id;
    match auth.issue_session(user, now_ms(), settings).await {
        Ok(pair) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.sign_in",
                Some(user_id.0),
                AuditOutcome::Success,
            )
            .await;
            Json(pair).into_response()
        }
        Err(error) => {
            audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.sign_in",
                error,
            )
            .await
        }
    }
}

pub(crate) async fn refresh(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<RefreshRequest>,
) -> Response {
    let (auth, project_id, settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let now = now_ms();
    let user_id = match auth
        .refresh_tokens
        .identify_for_project(project_id, payload.refresh_token.expose(), now)
        .await
    {
        Ok(Some((user_id, _))) => user_id,
        Ok(None) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.refresh",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return invalid_refresh(request_id);
        }
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.refresh",
                ProjectAuthOperationError::Refresh(error),
            )
            .await;
        }
    };
    if let Err(response) = enforce_user_rate(&state, user_id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(user_id),
        "auth.refresh",
    )
    .await
    {
        return response;
    }
    match auth
        .refresh_tokens
        .rotate_for_project(project_id, payload.refresh_token.expose(), now)
        .await
    {
        Ok(RefreshRotation::Rotated {
            plaintext, family, ..
        }) => {
            let record = match auth.accounts.find_by_id(project_id, family.user_id).await {
                Ok(Some(value))
                    if value.disabled_at_ms.is_none()
                        && (!settings.email_verification_required
                            || value.email_verified_at_ms.is_some()) =>
                {
                    value
                }
                Ok(_) => {
                    let _ignored = auth
                        .refresh_tokens
                        .revoke_user_session(project_id, family.user_id, family.session_id, now)
                        .await;
                    terminal_auth_audit(
                        &state,
                        project_id,
                        request_id,
                        AuthAuditActor::User(user_id),
                        "auth.refresh",
                        Some(family.session_id.0),
                        AuditOutcome::Denied,
                    )
                    .await;
                    return invalid_refresh(request_id);
                }
                Err(error) => {
                    return audited_operation_error(
                        &state,
                        project_id,
                        request_id,
                        AuthAuditActor::User(user_id),
                        "auth.refresh",
                        ProjectAuthOperationError::Account(error),
                    )
                    .await;
                }
            };
            let user = authenticated_user(record);
            let access = match auth
                .access_token(
                    &user,
                    family.session_id,
                    now,
                    settings.access_token_ttl_seconds,
                )
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    let _ignored = auth
                        .refresh_tokens
                        .revoke_user_session(project_id, family.user_id, family.session_id, now)
                        .await;
                    return audited_operation_error(
                        &state,
                        project_id,
                        request_id,
                        AuthAuditActor::User(user_id),
                        "auth.refresh",
                        error,
                    )
                    .await;
                }
            };
            let response = Json(token_pair(
                access,
                plaintext,
                family.session_id,
                user,
                settings.access_token_ttl_seconds,
            ))
            .into_response();
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.refresh",
                Some(family.session_id.0),
                AuditOutcome::Success,
            )
            .await;
            response
        }
        Ok(RefreshRotation::ReuseDetected { .. }) | Ok(RefreshRotation::Rejected) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.refresh",
                None,
                AuditOutcome::Denied,
            )
            .await;
            invalid_refresh(request_id)
        }
        Err(error) => {
            audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.refresh",
                ProjectAuthOperationError::Refresh(error),
            )
            .await
        }
    }
}

pub(crate) async fn sign_out(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<RefreshRequest>,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let now = now_ms();
    let (user_id, session_id) = match auth
        .refresh_tokens
        .identify_for_project(project_id, payload.refresh_token.expose(), now)
        .await
    {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.sign_out",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return invalid_refresh(request_id);
        }
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.sign_out",
                ProjectAuthOperationError::Refresh(error),
            )
            .await;
        }
    };
    if let Err(response) = enforce_user_rate(&state, user_id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(user_id),
        "auth.sign_out",
    )
    .await
    {
        return response;
    }
    match auth
        .refresh_tokens
        .revoke_presented_for_project(project_id, payload.refresh_token.expose(), now)
        .await
    {
        Ok(true) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.sign_out",
                Some(session_id.0),
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.sign_out",
                Some(session_id.0),
                AuditOutcome::Denied,
            )
            .await;
            invalid_refresh(request_id)
        }
        Err(error) => {
            audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.sign_out",
                ProjectAuthOperationError::Refresh(error),
            )
            .await
        }
    }
}

pub(crate) async fn password_reset_start(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<PasswordResetStartRequest>,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::Anonymous,
        "auth.password_reset.start",
    )
    .await
    {
        return response;
    }
    let service = match auth.account_service(project_id) {
        Ok(value) => value,
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.password_reset.start",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    let now = now_ms();
    match service.issue_password_reset(&payload.email, now).await {
        Ok(Some(token)) => {
            let outcome = if auth
                .email
                .enqueue_password_reset(project_id, &payload.email, &token, now)
                .await
                .is_err()
            {
                // The public response is deliberately identical for unknown
                // accounts and provider failures. Correlation uses only safe ids.
                warn!(%project_id, %request_id, "password reset email enqueue failed");
                AuditOutcome::Failure
            } else {
                AuditOutcome::Success
            };
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(token.record.user_id),
                "auth.password_reset.start",
                Some(token.record.user_id.0),
                outcome,
            )
            .await;
        }
        Ok(None) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.password_reset.start",
                None,
                AuditOutcome::Success,
            )
            .await;
        }
        Err(error) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.password_reset.start",
                None,
                if matches!(error, AccountError::Unavailable) {
                    AuditOutcome::Failure
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
        }
    }
    generic_reset_response()
}

pub(crate) async fn password_reset_complete(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<PasswordResetCompleteRequest>,
) -> Response {
    let (auth, project_id, settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let now = now_ms();
    let user_id = match auth
        .one_time_tokens
        .identify_for_project(
            project_id,
            payload.token.expose(),
            OneTimePurpose::PasswordReset,
            now,
        )
        .await
    {
        Ok(Some(user_id)) => user_id,
        Ok(None) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.password_reset.complete",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return invalid_token(request_id);
        }
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::Anonymous,
                "auth.password_reset.complete",
                ProjectAuthOperationError::Account(AccountError::OneTime(error)),
            )
            .await;
        }
    };
    if let Err(response) = enforce_user_rate(&state, user_id, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(user_id),
        "auth.password_reset.complete",
    )
    .await
    {
        return response;
    }
    if payload.new_password.expose().len() < usize::from(settings.password_min_length) {
        return audited_operation_error(
            &state,
            project_id,
            request_id,
            AuthAuditActor::User(user_id),
            "auth.password_reset.complete",
            ProjectAuthOperationError::Account(AccountError::Password(
                ffdb_auth::PasswordError::Policy,
            )),
        )
        .await;
    }
    // Do KDF work before consuming the one-time credential so a transient KDF
    // failure never burns the reset token.
    let hash = match auth
        .password_hasher
        .hash(SecretString::new(payload.new_password.into_inner()))
    {
        Ok(value) => value,
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.password_reset.complete",
                ProjectAuthOperationError::Account(AccountError::Password(error)),
            )
            .await;
        }
    };
    match auth
        .one_time_tokens
        .reset_password_for_project(project_id, payload.token.expose(), hash, now)
        .await
    {
        Ok(reset_user_id) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(reset_user_id),
                "auth.password_reset.complete",
                Some(reset_user_id.0),
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(user_id),
                "auth.password_reset.complete",
                None,
                if matches!(error, OneTimeStoreError::Unavailable) {
                    AuditOutcome::Failure
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
            one_time_error(error, request_id)
        }
    }
}

pub(crate) async fn change_password(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<PasswordChangeRequest>,
) -> Response {
    let (auth, project_id, settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let context = match access_context(&state, &headers, project_id, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_user_rate(&state, context.subject, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(context.subject),
        "auth.password.change",
    )
    .await
    {
        return response;
    }
    if payload.new_password.expose().len() < usize::from(settings.password_min_length) {
        return audited_operation_error(
            &state,
            project_id,
            request_id,
            AuthAuditActor::User(context.subject),
            "auth.password.change",
            ProjectAuthOperationError::Account(AccountError::Password(
                ffdb_auth::PasswordError::Policy,
            )),
        )
        .await;
    }
    let service = match auth.account_service(project_id) {
        Ok(value) => value,
        Err(error) => {
            return audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.password.change",
                ProjectAuthOperationError::Account(error),
            )
            .await;
        }
    };
    // Password changes always revoke every session, including the current
    // session, in the same transaction as the new hash. The compatibility
    // flag cannot weaken that project-wide security invariant.
    let _compatibility_flag = payload.revoke_other_sessions;
    match service
        .change_password(
            context.subject,
            SecretString::new(payload.current_password.into_inner()),
            SecretString::new(payload.new_password.into_inner()),
            now_ms(),
        )
        .await
    {
        Ok(()) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.password.change",
                Some(context.subject.0),
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.password.change",
                ProjectAuthOperationError::Account(error),
            )
            .await
        }
    }
}

pub(crate) async fn sessions(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let (context, current_session_id) =
        match access_session_context(&state, &headers, project_id, request_id).await {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    if let Err(response) = enforce_user_rate(&state, context.subject, request_id).await {
        return response;
    }
    match auth
        .refresh_tokens
        .list_user_sessions(project_id, context.subject)
        .await
    {
        Ok(values) => {
            let response = Json(
                values
                    .into_iter()
                    .map(|value| SessionSummary {
                        id: value.id,
                        created_at_ms: value.created_at_ms,
                        last_seen_at_ms: value.last_seen_at_ms,
                        expires_at_ms: value.expires_at_ms,
                        user_agent: None,
                        ip_address: None,
                        current: current_session_id == Some(value.id),
                        revoked_at_ms: value.revoked_at_ms,
                    })
                    .collect::<Vec<_>>(),
            )
            .into_response();
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.sessions.list",
                None,
                AuditOutcome::Success,
            )
            .await;
            response
        }
        Err(error) => {
            audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.sessions.list",
                ProjectAuthOperationError::Refresh(error),
            )
            .await
        }
    }
}

pub(crate) async fn revoke_session(
    State(state): State<ApiState>,
    Path((project, session)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let context = match access_context(&state, &headers, project_id, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let session_id = match Uuid::parse_str(&session) {
        Ok(value) => SessionId(value),
        Err(_) => {
            return ApiError::new(
                StatusCode::BAD_REQUEST,
                "auth.invalid_session_id",
                "invalid session identifier",
                request_id,
            )
            .into_response();
        }
    };
    if let Err(response) = enforce_user_rate(&state, context.subject, request_id).await {
        return response;
    }
    if let Err(response) = require_auth_audit(
        &state,
        project_id,
        request_id,
        AuthAuditActor::User(context.subject),
        "auth.session.revoke",
    )
    .await
    {
        return response;
    }
    match auth
        .refresh_tokens
        .revoke_user_session(project_id, context.subject, session_id, now_ms())
        .await
    {
        Ok(true) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.session.revoke",
                Some(session_id.0),
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => {
            terminal_auth_audit(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.session.revoke",
                Some(session_id.0),
                AuditOutcome::Denied,
            )
            .await;
            ApiError::new(
                StatusCode::NOT_FOUND,
                "auth.session_not_found",
                "session was not found",
                request_id,
            )
            .into_response()
        }
        Err(error) => {
            audited_operation_error(
                &state,
                project_id,
                request_id,
                AuthAuditActor::User(context.subject),
                "auth.session.revoke",
                ProjectAuthOperationError::Refresh(error),
            )
            .await
        }
    }
}

pub(crate) async fn get_settings(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (_auth, project_id, settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let principal = match developer_context(
        &state,
        &headers,
        project_id,
        DeveloperScope::AuthManage,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_api_key_rate(&state, principal.api_key_id.0, request_id).await {
        return response;
    }
    let mode = ExecutionMode::Developer(principal);
    super::append_audit_best_effort(
        &state,
        project_id,
        request_id,
        &mode,
        "auth.settings.read",
        "auth_settings",
        AuditOutcome::Success,
    )
    .await;
    Json(AuthSettings::from(settings)).into_response()
}

pub(crate) async fn update_settings(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<UpdateAuthSettingsRequest>,
) -> Response {
    let (auth, project_id, current) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let principal = match developer_context(
        &state,
        &headers,
        project_id,
        DeveloperScope::AuthManage,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_api_key_rate(&state, principal.api_key_id.0, request_id).await {
        return response;
    }
    let proposed = current.applying(&payload);
    if !proposed.is_valid() {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "auth.invalid_settings",
            "authentication settings are outside the supported bounds",
            request_id,
        )
        .into_response();
    }
    let mode = ExecutionMode::Developer(principal);
    if super::append_audit(
        &state,
        project_id,
        request_id,
        &mode,
        "auth.settings.update.requested",
        "auth_settings",
        AuditOutcome::Success,
    )
    .await
    .is_err()
    {
        return super::audit_unavailable(request_id).into_response();
    }
    let update = sqlx::query(
        "UPDATE project_auth_settings SET registration_enabled=$2, \
                email_verification_required=$3,access_token_ttl_seconds=$4, \
                refresh_token_ttl_seconds=$5,password_min_length=$6, \
                updated_by=NULL,updated_at=now() WHERE project_id=$1",
    )
    .bind(project_id.0)
    .bind(proposed.registration_enabled)
    .bind(proposed.email_verification_required)
    .bind(i32::try_from(proposed.access_token_ttl_seconds).unwrap_or(i32::MAX))
    .bind(i32::try_from(proposed.refresh_token_ttl_seconds).unwrap_or(i32::MAX))
    .bind(i32::from(proposed.password_min_length))
    .execute(&auth.pool)
    .await;
    match update {
        Ok(result) if result.rows_affected() == 1 => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                "auth.settings.update",
                "auth_settings",
                AuditOutcome::Success,
            )
            .await;
            Json(AuthSettings::from(proposed)).into_response()
        }
        Ok(_) | Err(_) => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                "auth.settings.update",
                "auth_settings",
                AuditOutcome::Failure,
            )
            .await;
            operation_error(ProjectAuthOperationError::Unavailable, request_id)
        }
    }
}

pub(crate) async fn admin_users(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let principal = match developer_context(
        &state,
        &headers,
        project_id,
        DeveloperScope::AuthManage,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_api_key_rate(&state, principal.api_key_id.0, request_id).await {
        return response;
    }
    let mode = ExecutionMode::Developer(principal);
    match auth.accounts.list_project_users(project_id, 1_000).await {
        Ok(users) => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                "auth.users.list",
                "auth_user",
                AuditOutcome::Success,
            )
            .await;
            Json(users.into_iter().map(safe_auth_user).collect::<Vec<_>>()).into_response()
        }
        Err(_) => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                "auth.users.list",
                "auth_user",
                AuditOutcome::Failure,
            )
            .await;
            operation_error(ProjectAuthOperationError::Unavailable, request_id)
        }
    }
}

pub(crate) async fn set_user_disabled(
    State(state): State<ApiState>,
    Path((project, user)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<SetAuthUserDisabledRequest>,
) -> Response {
    let (auth, project_id, _settings) = match required(&state, &project, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let user_id = match Uuid::parse_str(&user) {
        Ok(user_id) if !user_id.is_nil() => UserId(user_id),
        _ => {
            return ApiError::new(
                StatusCode::BAD_REQUEST,
                "auth.invalid_user_id",
                "invalid user identifier",
                request_id,
            )
            .into_response();
        }
    };
    if let Err(response) = enforce_project_rate(&state, project_id, request_id).await {
        return response;
    }
    let principal = match developer_context(
        &state,
        &headers,
        project_id,
        DeveloperScope::AuthManage,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(response) = enforce_api_key_rate(&state, principal.api_key_id.0, request_id).await {
        return response;
    }
    let mode = ExecutionMode::Developer(principal);
    let action = if payload.disabled {
        "auth.user.disable"
    } else {
        "auth.user.enable"
    };
    if super::append_audit(
        &state,
        project_id,
        request_id,
        &mode,
        &format!("{action}.requested"),
        "auth_user",
        AuditOutcome::Success,
    )
    .await
    .is_err()
    {
        return super::audit_unavailable(request_id).into_response();
    }
    match auth
        .accounts
        .set_disabled_for_project(project_id, user_id, payload.disabled, now_ms())
        .await
    {
        Ok(true) => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                action,
                "auth_user",
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                action,
                "auth_user",
                AuditOutcome::Denied,
            )
            .await;
            ApiError::new(
                StatusCode::NOT_FOUND,
                "auth.user_not_found",
                "authentication user was not found",
                request_id,
            )
            .into_response()
        }
        Err(_) => {
            super::append_audit_best_effort(
                &state,
                project_id,
                request_id,
                &mode,
                action,
                "auth_user",
                AuditOutcome::Failure,
            )
            .await;
            operation_error(ProjectAuthOperationError::Unavailable, request_id)
        }
    }
}

async fn developer_context(
    state: &ApiState,
    headers: &HeaderMap,
    project_id: ProjectId,
    scope: DeveloperScope,
    request_id: RequestId,
) -> Result<ffdb_protocol::DeveloperPrincipal, ApiError> {
    let credential = bearer(headers).map_err(|error| super::credential_error(error, request_id))?;
    state
        .credentials
        .verify_developer_credential(project_id, credential, scope)
        .await
        .map_err(|error| super::credential_error(error, request_id))
}

async fn access_context(
    state: &ApiState,
    headers: &HeaderMap,
    project_id: ProjectId,
    request_id: RequestId,
) -> Result<AuthContext, ApiError> {
    let credential = bearer(headers).map_err(|error| super::credential_error(error, request_id))?;
    state
        .credentials
        .verify_end_user_credential(project_id, credential)
        .await
        .map_err(|error| super::credential_error(error, request_id))
}

async fn access_session_context(
    state: &ApiState,
    headers: &HeaderMap,
    project_id: ProjectId,
    request_id: RequestId,
) -> Result<(AuthContext, Option<SessionId>), ApiError> {
    let credential = bearer(headers).map_err(|error| super::credential_error(error, request_id))?;
    state
        .credentials
        .verify_end_user_session_credential(project_id, credential)
        .await
        .map_err(|error| super::credential_error(error, request_id))
}

#[derive(Clone, Copy)]
enum AuthAuditActor {
    Anonymous,
    User(UserId),
}

async fn enforce_project_rate(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
) -> Result<(), Response> {
    let Some(limiter) = &state.rate_limiter else {
        return Ok(());
    };
    super::enforce_rate_dimension(
        limiter,
        RateDimension::AuthProject,
        project_id.to_string().as_bytes(),
        1,
        request_id,
    )
    .await
}

async fn enforce_user_rate(
    state: &ApiState,
    user_id: UserId,
    request_id: RequestId,
) -> Result<(), Response> {
    let Some(limiter) = &state.rate_limiter else {
        return Ok(());
    };
    super::enforce_rate_dimension(
        limiter,
        RateDimension::AuthUser,
        user_id.to_string().as_bytes(),
        1,
        request_id,
    )
    .await
}

async fn enforce_api_key_rate(
    state: &ApiState,
    api_key_id: Uuid,
    request_id: RequestId,
) -> Result<(), Response> {
    let Some(limiter) = &state.rate_limiter else {
        return Ok(());
    };
    super::enforce_rate_dimension(
        limiter,
        RateDimension::AuthApiKey,
        api_key_id.to_string().as_bytes(),
        1,
        request_id,
    )
    .await
}

async fn append_auth_audit(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    actor: AuthAuditActor,
    action: &str,
    resource_id: Option<Uuid>,
    outcome: AuditOutcome,
) -> Result<(), ()> {
    let sink = &state.audit;
    let (actor_kind, actor_id) = match actor {
        AuthAuditActor::Anonymous => (ActorKind::Anonymous, None),
        AuthAuditActor::User(user_id) => (ActorKind::User, Some(user_id.0)),
    };
    sink.append(AuditDraft {
        occurred_at_ms: now_ms(),
        organization_id: None,
        project_id: Some(project_id),
        request_id,
        actor_kind,
        actor_id,
        action: action.to_owned(),
        resource_kind: "auth".into(),
        resource_id,
        outcome,
        source_ip: super::trusted_source_ip(),
        metadata: serde_json::json!({"protocol_version": PROTOCOL_VERSION}),
    })
    .await
    .map(|_| ())
    .map_err(|_| ())
}

async fn require_auth_audit(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    actor: AuthAuditActor,
    action: &str,
) -> Result<(), Response> {
    append_auth_audit(
        state,
        project_id,
        request_id,
        actor,
        &format!("{action}.requested"),
        None,
        AuditOutcome::Success,
    )
    .await
    .map_err(|()| super::audit_unavailable(request_id).into_response())
}

async fn terminal_auth_audit(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    actor: AuthAuditActor,
    action: &str,
    resource_id: Option<Uuid>,
    outcome: AuditOutcome,
) {
    if append_auth_audit(
        state,
        project_id,
        request_id,
        actor,
        action,
        resource_id,
        outcome,
    )
    .await
    .is_err()
    {
        tracing::error!(%project_id, %request_id, action, "failed to append auth audit event");
    }
}

async fn audited_operation_error(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    actor: AuthAuditActor,
    action: &str,
    error: ProjectAuthOperationError,
) -> Response {
    let outcome = match &error {
        ProjectAuthOperationError::Account(AccountError::Unavailable)
        | ProjectAuthOperationError::Account(AccountError::OneTime(
            OneTimeStoreError::Unavailable,
        ))
        | ProjectAuthOperationError::Refresh(RefreshStoreError::Unavailable)
        | ProjectAuthOperationError::Unavailable => AuditOutcome::Failure,
        _ => AuditOutcome::Denied,
    };
    terminal_auth_audit(state, project_id, request_id, actor, action, None, outcome).await;
    operation_error(error, request_id)
}

async fn required(
    state: &ApiState,
    project: &str,
    request_id: RequestId,
) -> Result<(Arc<ProjectAuthState>, ProjectId, ProjectAuthSettings), ApiError> {
    let project_id = super::parse_project(project, request_id)?;
    state
        .router
        .resolve(project_id)
        .await
        .map_err(|error| super::routing_error(error, request_id))?;
    let auth = state.project_auth.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth.unavailable",
            "authentication service is unavailable",
            request_id,
        )
    })?;
    let settings = auth.settings(project_id).await.map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth.unavailable",
            "authentication service is unavailable",
            request_id,
        )
    })?;
    Ok((auth, project_id, settings))
}

fn token_pair(
    access_token: SecretToken,
    refresh_token: SecretToken,
    session_id: SessionId,
    user: AuthenticatedUser,
    access_token_ttl_seconds: u32,
) -> AuthTokenPair {
    AuthTokenPair {
        access_token: SensitiveString::new(access_token.expose()),
        refresh_token: SensitiveString::new(refresh_token.expose()),
        token_type: "Bearer".into(),
        expires_in_seconds: access_token_ttl_seconds,
        session_id,
        user: AuthUser {
            id: user.id,
            email: user.normalized_email,
            email_verified: user.email_verified,
            disabled: false,
            role: user.role,
            custom_claims: user.custom_claims,
            created_at_ms: user.created_at_ms,
        },
    }
}

fn authenticated_user(record: AuthUserRecord) -> AuthenticatedUser {
    AuthenticatedUser {
        id: record.id,
        project_id: record.project_id,
        normalized_email: record.normalized_email,
        role: record.role,
        custom_claims: record.custom_claims,
        email_verified: record.email_verified_at_ms.is_some(),
        created_at_ms: record.created_at_ms,
    }
}

fn safe_auth_user(record: AuthUserRecord) -> AuthUser {
    AuthUser {
        id: record.id,
        email: record.normalized_email,
        email_verified: record.email_verified_at_ms.is_some(),
        disabled: record.disabled_at_ms.is_some(),
        role: record.role,
        custom_claims: record.custom_claims,
        created_at_ms: record.created_at_ms,
    }
}

fn accepted_registration(verification_required: bool) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(RegisterResponse {
            // The public response intentionally does not disclose whether an
            // identity already existed. A nil identifier is not a resource id;
            // callers continue through email verification.
            user_id: UserId(Uuid::nil()),
            verification_required,
        }),
    )
        .into_response()
}

fn generic_reset_response() -> Response {
    StatusCode::ACCEPTED.into_response()
}

fn one_time_error(error: OneTimeStoreError, request_id: RequestId) -> Response {
    match error {
        OneTimeStoreError::Rejected => invalid_token(request_id),
        OneTimeStoreError::Unavailable => {
            operation_error(ProjectAuthOperationError::Unavailable, request_id)
        }
    }
}

fn invalid_token(request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "auth.invalid_token",
        "authentication token is invalid or expired",
        request_id,
    )
    .into_response()
}

fn invalid_refresh(request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "auth.invalid_refresh_token",
        "refresh token is invalid or expired",
        request_id,
    )
    .into_response()
}

fn operation_error(error: ProjectAuthOperationError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        ProjectAuthOperationError::Account(AccountError::InvalidEmail)
        | ProjectAuthOperationError::Account(AccountError::InvalidClaims)
        | ProjectAuthOperationError::Account(AccountError::Password(_)) => (
            StatusCode::BAD_REQUEST,
            "auth.invalid_input",
            "authentication input is invalid",
        ),
        ProjectAuthOperationError::Account(AccountError::InvalidCredentials) => (
            StatusCode::UNAUTHORIZED,
            "auth.invalid_credentials",
            "authentication credentials are invalid",
        ),
        ProjectAuthOperationError::Account(AccountError::VerificationRequired) => (
            StatusCode::FORBIDDEN,
            "auth.verification_required",
            "email verification is required",
        ),
        ProjectAuthOperationError::Account(AccountError::Disabled) => (
            StatusCode::FORBIDDEN,
            "auth.disabled",
            "account is disabled",
        ),
        ProjectAuthOperationError::Account(AccountError::EmailInUse) => {
            (StatusCode::ACCEPTED, "auth.accepted", "request accepted")
        }
        ProjectAuthOperationError::Account(AccountError::NotFound) => (
            StatusCode::NOT_FOUND,
            "auth.not_found",
            "authentication resource was not found",
        ),
        ProjectAuthOperationError::Account(AccountError::OneTime(OneTimeStoreError::Rejected)) => (
            StatusCode::BAD_REQUEST,
            "auth.invalid_token",
            "authentication token is invalid or expired",
        ),
        ProjectAuthOperationError::Refresh(RefreshStoreError::Invalid) => (
            StatusCode::BAD_REQUEST,
            "auth.invalid_input",
            "authentication input is invalid",
        ),
        ProjectAuthOperationError::Account(AccountError::OneTime(
            OneTimeStoreError::Unavailable,
        ))
        | ProjectAuthOperationError::Account(AccountError::Unavailable)
        | ProjectAuthOperationError::Refresh(RefreshStoreError::Unavailable)
        | ProjectAuthOperationError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "auth.unavailable",
            "authentication service is unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}

fn action_url(
    public_base_url: &Url,
    project_id: ProjectId,
    token: &str,
    kind: TemplateKind,
) -> String {
    let route = match kind {
        TemplateKind::EmailVerification => "verify",
        TemplateKind::PasswordReset => "password-reset",
        TemplateKind::EmailChange | TemplateKind::Invitation | TemplateKind::MagicLink => "auth",
    };
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("project_id", &project_id.to_string())
        .append_pair("token", token)
        .finish();
    let mut url = public_base_url.clone();
    // Fragments are consumed by the portal and are not sent to HTTP servers or
    // request logs, avoiding one-time credential leakage through access logs.
    url.set_fragment(Some(&format!("/auth/{route}?{query}")));
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_action_token_is_only_in_fragment() -> Result<(), url::ParseError> {
        let base = Url::parse("https://api.example.test/")?;
        let project = ProjectId::new();
        let rendered = action_url(
            &base,
            project,
            "ffdb_action_secret-value",
            TemplateKind::EmailVerification,
        );
        let parsed = Url::parse(&rendered)?;
        assert!(parsed.query().is_none());
        assert!(!parsed.path().contains("secret-value"));
        assert!(
            parsed
                .fragment()
                .is_some_and(|value| value.contains("secret-value"))
        );
        Ok(())
    }

    #[test]
    fn auth_state_debug_is_secret_free() {
        assert!(!format!("{:?}", ProjectAuthSetupError).contains("token"));
    }

    #[test]
    fn auth_settings_updates_are_typed_and_bounded() -> Result<(), serde_json::Error> {
        let current = ProjectAuthSettings {
            registration_enabled: true,
            email_verification_required: true,
            access_token_ttl_seconds: 900,
            refresh_token_ttl_seconds: 2_592_000,
            password_min_length: 8,
        };
        let update: UpdateAuthSettingsRequest = serde_json::from_value(serde_json::json!({
            "registration_enabled": false,
            "access_token_ttl_seconds": 120,
            "password_min_length": 16
        }))?;
        let proposed = current.applying(&update);
        assert!(proposed.is_valid());
        assert!(!proposed.registration_enabled);
        assert_eq!(proposed.access_token_ttl_seconds, 120);
        assert_eq!(proposed.password_min_length, 16);

        let invalid: UpdateAuthSettingsRequest = serde_json::from_value(serde_json::json!({
            "refresh_token_ttl_seconds": 3_599
        }))?;
        assert!(!current.applying(&invalid).is_valid());
        assert!(
            serde_json::from_value::<UpdateAuthSettingsRequest>(serde_json::json!({
                "password_min_length": 12,
                "unexpected": true
            }))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn admin_user_response_cannot_serialize_password_material() -> Result<(), AccountError> {
        let hash = Argon2PasswordHasher::new(8 * 1024, 1, 1, 128)?.hash(SecretString::new(
            "test password that never leaves this test".into(),
        ))?;
        let phc = hash.as_phc().to_owned();
        let safe = safe_auth_user(AuthUserRecord {
            id: UserId::new(),
            project_id: ProjectId::new(),
            normalized_email: "person@example.test".into(),
            password_hash: hash,
            role: "authenticated".into(),
            custom_claims: Default::default(),
            email_verified_at_ms: Some(1),
            disabled_at_ms: None,
            password_changed_at_ms: 1,
            created_at_ms: 1,
        });
        let encoded = serde_json::to_string(&safe).map_err(|_| AccountError::Unavailable)?;
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains(&phc));
        assert!(encoded.contains("person@example.test"));
        Ok(())
    }
}
