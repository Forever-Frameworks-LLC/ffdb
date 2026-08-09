use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_audit::{ActorKind, AuditDraft, AuditOutcome};
use ffdb_auth::{
    AeadSigningKeyEnvelope, ApiKeyCodec, ApiKeyRepository, Argon2PasswordHasher, CredentialDigest,
    OpaqueTokenCodec, PasswordHasher, PgApiKeyRepository, PgSigningKeyManager, SecretString,
    SigningKeyManagementError,
};
use ffdb_billing::PlatformBillingProvider;
use ffdb_control_plane::{
    PgPlatformSessionStore, PgPlatformUserRepository, PgRegistry, PlatformAuthError,
    PlatformAuthService, PlatformSessionIdentity, PlatformSessionIssue, PlatformSessionRotation,
    Registry as _, RegistryError,
};
use ffdb_email::{
    OrganizationInvitationRequest as EmailOrganizationInvitationRequest, ScalarValue,
};
use ffdb_protocol::{
    AcceptOrganizationInvitationRequest, AddOrganizationMemberRequest, ApiKeyId, ApiKeySummary,
    CreateApiKeyRequest, CreateOrganizationInvitationRequest, CreateOrganizationRequest,
    CreateProjectRequest, CreatedApiKey, DeveloperBootstrapRequest, DeveloperPrincipal,
    DeveloperRefreshRequest, DeveloperScope, DeveloperSessionResponse, DeveloperSignInRequest,
    ExecutionMode, NodeId, OrganizationId, OrganizationMembershipSummary, OrganizationRole,
    PROTOCOL_VERSION, ProjectId, ProjectLifecycleState, ProjectSummary, RequestId, SensitiveString,
    UpdateOrganizationMemberRequest, UserId, WorkerOperation, WorkerRequest,
};
use ffdb_rate_limits::RateDimension;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use subtle::ConstantTimeEq as _;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{ApiError, ApiState, bearer, now_ms};

#[derive(Clone)]
pub struct ManagementState {
    pub(crate) platform_auth: Arc<PlatformAuthService>,
    registry: Arc<PgRegistry>,
    pub(crate) pool: PgPool,
    pub(crate) billing: super::billing::BillingService,
    api_keys: PgApiKeyRepository,
    api_key_codec: ApiKeyCodec,
    signing_keys: PgSigningKeyManager<AeadSigningKeyEnvelope>,
    invitation_codec: OpaqueTokenCodec,
    password_hasher: Arc<Argon2PasswordHasher>,
    public_base_url: Url,
    email_from_address: String,
    bootstrap_digest: [u8; 32],
    node_id: NodeId,
}

pub struct ManagementStateConfig {
    pub platform_session_pepper: Vec<u8>,
    pub api_key_pepper: Vec<u8>,
    pub invitation_pepper: Vec<u8>,
    pub signing_key_envelope: AeadSigningKeyEnvelope,
    pub bootstrap_token: String,
    pub node_id: NodeId,
    pub public_base_url: Url,
    pub email_from_address: String,
    pub billing_provider: Option<Arc<dyn PlatformBillingProvider>>,
    pub pro_billing_unit: ffdb_protocol::PlatformBillingUnit,
}

impl std::fmt::Debug for ManagementStateConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagementStateConfig")
            .field("platform_session_pepper", &"[REDACTED]")
            .field("api_key_pepper", &"[REDACTED]")
            .field("invitation_pepper", &"[REDACTED]")
            .field("signing_key_envelope", &"[REDACTED]")
            .field("bootstrap_token", &"[REDACTED]")
            .field("node_id", &self.node_id)
            .field("public_base_url", &self.public_base_url)
            .field("email_from_address", &self.email_from_address)
            .field(
                "billing_provider",
                &self.billing_provider.as_ref().map(|_| "configured"),
            )
            .field("pro_billing_unit", &self.pro_billing_unit)
            .finish()
    }
}

impl std::fmt::Debug for ManagementState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagementState")
            .field("platform_auth", &"PlatformAuthService")
            .field("registry", &self.registry)
            .field("bootstrap_digest", &"[REDACTED]")
            .field("node_id", &self.node_id)
            .finish_non_exhaustive()
    }
}

impl ManagementState {
    pub fn new(pool: PgPool, config: ManagementStateConfig) -> Result<Self, PlatformAuthError> {
        let users = Arc::new(PgPlatformUserRepository::new(pool.clone()));
        let sessions = Arc::new(
            PgPlatformSessionStore::new(pool.clone(), config.platform_session_pepper)
                .map_err(|_| PlatformAuthError::Unavailable)?,
        );
        let password_hasher = Arc::new(Argon2PasswordHasher::default());
        let platform_auth = Arc::new(PlatformAuthService::new(
            users,
            sessions,
            password_hasher.clone(),
        )?);
        let api_key_codec =
            ApiKeyCodec::new(config.api_key_pepper).map_err(|_| PlatformAuthError::Unavailable)?;
        Ok(Self {
            platform_auth,
            registry: Arc::new(PgRegistry::new(pool.clone())),
            pool: pool.clone(),
            billing: super::billing::BillingService::new(
                pool.clone(),
                config.billing_provider,
                config.pro_billing_unit,
            ),
            api_keys: PgApiKeyRepository::new(pool.clone()),
            api_key_codec,
            signing_keys: PgSigningKeyManager::new(pool, config.signing_key_envelope),
            invitation_codec: OpaqueTokenCodec::new("invitation", config.invitation_pepper)
                .map_err(|_| PlatformAuthError::Unavailable)?,
            password_hasher,
            public_base_url: config.public_base_url,
            email_from_address: config.email_from_address,
            bootstrap_digest: Sha256::digest(config.bootstrap_token.as_bytes()).into(),
            node_id: config.node_id,
        })
    }
}

pub(crate) async fn bootstrap(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<DeveloperBootstrapRequest>,
) -> Response {
    let management = match required(&state, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let provided = headers
        .get("x-ffdb-bootstrap-token")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 4_096);
    let Some(provided) = provided else {
        return platform_error(PlatformAuthError::InvalidCredentials, request_id).into_response();
    };
    let provided_digest: [u8; 32] = Sha256::digest(provided.as_bytes()).into();
    if !bool::from(provided_digest.ct_eq(&management.bootstrap_digest)) {
        return platform_error(PlatformAuthError::InvalidCredentials, request_id).into_response();
    }
    if let Err(response) = require_management_audit(
        &state,
        None,
        None,
        None,
        request_id,
        "developer.bootstrap",
        "platform_user",
        None,
    )
    .await
    {
        return response;
    }
    let password = Zeroizing::new(payload.password.expose().to_owned());
    let now = now_ms();
    if let Err(error) = management
        .platform_auth
        .bootstrap_first_user(
            &payload.email,
            SecretString::new(password.as_str().to_owned()),
            now,
        )
        .await
    {
        terminal_management_audit(
            &state,
            None,
            None,
            None,
            request_id,
            "developer.bootstrap",
            "platform_user",
            None,
            AuditOutcome::Failure,
        )
        .await;
        return platform_error(error, request_id).into_response();
    }
    match management
        .platform_auth
        .sign_in(
            &payload.email,
            SecretString::new(password.as_str().to_owned()),
            now,
        )
        .await
    {
        Ok(issue) => {
            terminal_management_audit(
                &state,
                None,
                None,
                Some(issue.identity.user_id),
                request_id,
                "developer.bootstrap",
                "platform_user",
                Some(issue.identity.user_id.0),
                AuditOutcome::Success,
            )
            .await;
            (StatusCode::CREATED, Json(session_response(issue))).into_response()
        }
        Err(error) => {
            terminal_management_audit(
                &state,
                None,
                None,
                None,
                request_id,
                "developer.bootstrap",
                "platform_user",
                None,
                AuditOutcome::Failure,
            )
            .await;
            platform_error(error, request_id).into_response()
        }
    }
}

pub(crate) async fn sign_in(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<DeveloperSignInRequest>,
) -> Response {
    let management = match required(&state, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match management
        .platform_auth
        .sign_in(
            &payload.email,
            SecretString::new(payload.password.expose().to_owned()),
            now_ms(),
        )
        .await
    {
        Ok(issue) => (StatusCode::OK, Json(session_response(issue))).into_response(),
        Err(error) => platform_error(error, request_id).into_response(),
    }
}

pub(crate) async fn refresh(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<DeveloperRefreshRequest>,
) -> Response {
    let management = match required(&state, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match management
        .platform_auth
        .refresh(payload.session_token.expose(), now_ms())
        .await
    {
        Ok(PlatformSessionRotation::Rotated {
            plaintext,
            session,
            identity,
        }) => Json(DeveloperSessionResponse {
            session_token: SensitiveString::new(plaintext.expose()),
            user_id: identity.user_id,
            email: identity.normalized_email,
            expires_at_ms: session.expires_at_ms,
        })
        .into_response(),
        Ok(PlatformSessionRotation::ReuseDetected { .. })
        | Ok(PlatformSessionRotation::Rejected) => {
            platform_error(PlatformAuthError::InvalidCredentials, request_id).into_response()
        }
        Err(error) => platform_error(error, request_id).into_response(),
    }
}

pub(crate) async fn sign_out(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<DeveloperRefreshRequest>,
) -> Response {
    let management = match required(&state, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match management
        .platform_auth
        .sign_out(payload.session_token.expose(), now_ms())
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => platform_error(error, request_id).into_response(),
    }
}

pub(crate) async fn organizations(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match management
        .registry
        .list_organizations(identity.user_id)
        .await
    {
        Ok(values) => Json(values).into_response(),
        Err(error) => registry_error(error, request_id).into_response(),
    }
}

pub(crate) async fn create_organization(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateOrganizationRequest>,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(instance) = &state.instance else {
        return super::instance::service_error(
            super::instance::InstanceServiceError::Unavailable,
            request_id,
        );
    };
    if let Err(error) = instance
        .authorize_organization_creation(identity.user_id)
        .await
    {
        return super::instance::service_error(error, request_id);
    }
    if let Err(response) = require_management_audit(
        &state,
        None,
        None,
        Some(identity.user_id),
        request_id,
        "organization.create",
        "organization",
        None,
    )
    .await
    {
        return response;
    }
    match management
        .registry
        .create_organization(payload, identity.user_id, now_ms())
        .await
    {
        Ok(value) => {
            terminal_management_audit(
                &state,
                None,
                None,
                Some(identity.user_id),
                request_id,
                "organization.create",
                "organization",
                Some(value.id.0),
                AuditOutcome::Success,
            )
            .await;
            (StatusCode::CREATED, Json(value)).into_response()
        }
        Err(error) => {
            terminal_management_audit(
                &state,
                None,
                None,
                Some(identity.user_id),
                request_id,
                "organization.create",
                "organization",
                None,
                AuditOutcome::Failure,
            )
            .await;
            registry_error(error, request_id).into_response()
        }
    }
}

pub(crate) async fn projects(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match management
        .registry
        .list_projects(organization_id, identity.user_id)
        .await
    {
        Ok(values) => Json(values).into_response(),
        Err(error) => registry_error(error, request_id).into_response(),
    }
}

pub(crate) async fn members(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) =
        authorized_organization_admin(&management, identity.user_id, organization_id, request_id)
            .await
    {
        return error.into_response();
    }
    if let Err(response) = enforce_platform_user_rate(&state, identity.user_id, request_id).await {
        return response;
    }
    let rows = sqlx::query(
        "SELECT m.organization_id,m.user_id,u.email,m.role, \
                (extract(epoch FROM m.created_at)*1000)::bigint created_at_ms \
         FROM organization_memberships m JOIN platform_users u ON u.id=m.user_id \
         WHERE m.organization_id=$1 ORDER BY lower(u.email),m.user_id LIMIT 1000",
    )
    .bind(organization_id.0)
    .fetch_all(&management.pool)
    .await;
    match rows {
        Ok(rows) => {
            let values = rows
                .into_iter()
                .map(membership_from_row)
                .collect::<Result<Vec<_>, _>>();
            match values {
                Ok(values) => {
                    terminal_membership_audit(
                        &state,
                        organization_id,
                        identity.user_id,
                        request_id,
                        "organization.members.list",
                        AuditOutcome::Success,
                    )
                    .await;
                    Json(values).into_response()
                }
                Err(_) => membership_error(MembershipMutationError::Unavailable, request_id),
            }
        }
        Err(_) => {
            terminal_membership_audit(
                &state,
                organization_id,
                identity.user_id,
                request_id,
                "organization.members.list",
                AuditOutcome::Failure,
            )
            .await;
            membership_error(MembershipMutationError::Unavailable, request_id)
        }
    }
}

pub(crate) async fn add_member(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<AddOrganizationMemberRequest>,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) =
        authorized_organization_admin(&management, identity.user_id, organization_id, request_id)
            .await
    {
        return error.into_response();
    }
    if let Err(response) = enforce_platform_user_rate(&state, identity.user_id, request_id).await {
        return response;
    }
    if payload.email.trim() != payload.email
        || payload.email.len() > 320
        || !payload.email.contains('@')
    {
        return membership_error(MembershipMutationError::Invalid, request_id);
    }
    if require_membership_audit(
        &state,
        organization_id,
        identity.user_id,
        request_id,
        "organization.member.add",
    )
    .await
    .is_err()
    {
        return super::audit_unavailable(request_id).into_response();
    }
    match add_member_transaction(
        &management.pool,
        organization_id,
        identity.user_id,
        &payload.email,
        payload.role,
    )
    .await
    {
        Ok(value) => {
            terminal_membership_audit(
                &state,
                organization_id,
                identity.user_id,
                request_id,
                "organization.member.add",
                AuditOutcome::Success,
            )
            .await;
            (StatusCode::CREATED, Json(value)).into_response()
        }
        Err(error) => {
            terminal_membership_audit(
                &state,
                organization_id,
                identity.user_id,
                request_id,
                "organization.member.add",
                membership_audit_outcome(error),
            )
            .await;
            membership_error(error, request_id)
        }
    }
}

pub(crate) async fn create_invitation(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateOrganizationInvitationRequest>,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let actor_role = match authorized_organization_admin(
        &management,
        identity.user_id,
        organization_id,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if actor_role != OrganizationRole::Owner && payload.role == OrganizationRole::Owner {
        return invitation_error(request_id);
    }
    if let Err(response) = enforce_platform_user_rate(&state, identity.user_id, request_id).await {
        return response;
    }
    let normalized_email = match normalize_platform_email(&payload.email) {
        Some(value) => value,
        None => return invitation_error(request_id),
    };
    if require_membership_audit(
        &state,
        organization_id,
        identity.user_id,
        request_id,
        "organization.invitation.create",
    )
    .await
    .is_err()
    {
        return super::audit_unavailable(request_id).into_response();
    }
    let existing_user: bool = match sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM platform_users WHERE lower(email)=$1)",
    )
    .bind(&normalized_email)
    .fetch_one(&management.pool)
    .await
    {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    // Existing platform users are deliberately indistinguishable at this endpoint.
    // Administrators can add those accounts through the membership endpoint.
    if existing_user {
        terminal_membership_audit(
            &state,
            organization_id,
            identity.user_id,
            request_id,
            "organization.invitation.create",
            AuditOutcome::Success,
        )
        .await;
        return StatusCode::ACCEPTED.into_response();
    }
    let Some(email) = &state.email else {
        return control_plane_unavailable(request_id).into_response();
    };
    let (plaintext, token_parts) = match management.invitation_codec.issue() {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let invitation_id = Uuid::now_v7();
    let now = now_ms();
    let expires_at_ms = now.saturating_add(24 * 60 * 60 * 1_000);
    let mut transaction = match management.pool.begin().await {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let locked_actor_role =
        match lock_organization_actor(&mut transaction, organization_id, identity.user_id).await {
            Ok(value) => value,
            Err(MembershipMutationError::Unavailable) => {
                return control_plane_unavailable(request_id).into_response();
            }
            Err(_) => return invitation_error(request_id),
        };
    if locked_actor_role != OrganizationRole::Owner && payload.role == OrganizationRole::Owner {
        return invitation_error(request_id);
    }
    if sqlx::query(
        "UPDATE organization_invitations SET revoked_at=now() WHERE organization_id=$1 \
         AND lower(normalized_email)=$2 AND accepted_at IS NULL AND revoked_at IS NULL",
    )
    .bind(organization_id.0)
    .bind(&normalized_email)
    .execute(&mut *transaction)
    .await
    .is_err()
    {
        return control_plane_unavailable(request_id).into_response();
    }
    if sqlx::query(
        "INSERT INTO organization_invitations \
         (id,organization_id,normalized_email,role,lookup_prefix,keyed_hash,invited_by,expires_at,created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,to_timestamp($8::double precision/1000),\
                 to_timestamp($9::double precision/1000))",
    )
    .bind(invitation_id)
    .bind(organization_id.0)
    .bind(&normalized_email)
    .bind(membership_role_name(payload.role))
    .bind(&token_parts.prefix)
    .bind(token_parts.digest.as_bytes().as_slice())
    .bind(identity.user_id.0)
    .bind(expires_at_ms)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .is_err()
        || transaction.commit().await.is_err()
    {
        return control_plane_unavailable(request_id).into_response();
    }
    let organization_name: String =
        match sqlx::query_scalar("SELECT display_name FROM organizations WHERE id=$1")
            .bind(organization_id.0)
            .fetch_one(&management.pool)
            .await
        {
            Ok(value) => value,
            Err(_) => return control_plane_unavailable(request_id).into_response(),
        };
    let action_url = match invitation_action_url(&management.public_base_url, plaintext.expose()) {
        Some(value) => value,
        None => return control_plane_unavailable(request_id).into_response(),
    };
    let variables = BTreeMap::from([
        (
            "project_name".to_owned(),
            ScalarValue::String(organization_name),
        ),
        (
            "action_url".to_owned(),
            ScalarValue::String(action_url.to_string()),
        ),
        (
            "expires_in".to_owned(),
            ScalarValue::String("24 hours".to_owned()),
        ),
    ]);
    if email
        .enqueue_organization_invitation(EmailOrganizationInvitationRequest {
            organization_id: organization_id.0,
            recipient: normalized_email,
            from: management.email_from_address.clone(),
            variables,
            idempotency_key: format!("organization-invitation:{invitation_id}"),
            now_ms: now,
        })
        .await
        .is_err()
    {
        let _ = sqlx::query(
            "UPDATE organization_invitations SET revoked_at=now() \
             WHERE id=$1 AND accepted_at IS NULL AND revoked_at IS NULL",
        )
        .bind(invitation_id)
        .execute(&management.pool)
        .await;
        terminal_membership_audit(
            &state,
            organization_id,
            identity.user_id,
            request_id,
            "organization.invitation.create",
            AuditOutcome::Failure,
        )
        .await;
        return control_plane_unavailable(request_id).into_response();
    }
    terminal_membership_audit(
        &state,
        organization_id,
        identity.user_id,
        request_id,
        "organization.invitation.create",
        AuditOutcome::Success,
    )
    .await;
    StatusCode::ACCEPTED.into_response()
}

pub(crate) async fn accept_invitation(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<AcceptOrganizationInvitationRequest>,
) -> Response {
    let management = match required(&state, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let token_parts = match management
        .invitation_codec
        .parse_and_digest(payload.invitation_token.expose())
    {
        Ok(value) => value,
        Err(_) => return invitation_error(request_id),
    };
    let password = Zeroizing::new(payload.password.expose().to_owned());
    let password_hash = match management
        .password_hasher
        .hash(SecretString::new(password.as_str().to_owned()))
    {
        Ok(value) => value,
        Err(_) => return invitation_error(request_id),
    };
    let mut transaction = match management.pool.begin().await {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let row = match sqlx::query(
        "SELECT id,organization_id,normalized_email,role,keyed_hash \
         FROM organization_invitations WHERE lookup_prefix=$1 AND accepted_at IS NULL \
         AND revoked_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(&token_parts.prefix)
    .fetch_optional(&mut *transaction)
    .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return invitation_error(request_id),
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let stored_digest: Vec<u8> = match row.try_get("keyed_hash") {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let stored_digest = match CredentialDigest::from_slice(&stored_digest) {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    if !management
        .invitation_codec
        .verify_digest(&token_parts.digest, &stored_digest)
    {
        return invitation_error(request_id);
    }
    let invitation_id: Uuid = match row.try_get("id") {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let organization_id = match row.try_get::<Uuid, _>("organization_id") {
        Ok(value) => OrganizationId(value),
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let normalized_email: String = match row.try_get("normalized_email") {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let role: String = match row.try_get("role") {
        Ok(value) => value,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    if let Err(response) = require_management_audit(
        &state,
        Some(organization_id),
        None,
        None,
        request_id,
        "organization.invitation.accept",
        "organization_invitation",
        Some(invitation_id),
    )
    .await
    {
        return response;
    }
    let user_id = UserId::new();
    let now = now_ms();
    let inserted = sqlx::query(
        "INSERT INTO platform_users \
         (id,email,password_phc,email_verified_at,created_at,updated_at) \
         VALUES ($1,$2,$3,to_timestamp($4::double precision/1000),\
                 to_timestamp($4::double precision/1000),to_timestamp($4::double precision/1000)) \
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id.0)
    .bind(&normalized_email)
    .bind(password_hash.as_phc())
    .bind(now)
    .execute(&mut *transaction)
    .await;
    if !matches!(inserted, Ok(result) if result.rows_affected() == 1) {
        terminal_management_audit(
            &state,
            Some(organization_id),
            None,
            None,
            request_id,
            "organization.invitation.accept",
            "organization_invitation",
            Some(invitation_id),
            AuditOutcome::Denied,
        )
        .await;
        return invitation_error(request_id);
    }
    if sqlx::query(
        "INSERT INTO organization_memberships (organization_id,user_id,role,created_at) \
         VALUES ($1,$2,$3,to_timestamp($4::double precision/1000))",
    )
    .bind(organization_id.0)
    .bind(user_id.0)
    .bind(&role)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .is_err()
    {
        terminal_management_audit(
            &state,
            Some(organization_id),
            None,
            None,
            request_id,
            "organization.invitation.accept",
            "organization_invitation",
            Some(invitation_id),
            AuditOutcome::Failure,
        )
        .await;
        return control_plane_unavailable(request_id).into_response();
    }
    let consumed = sqlx::query(
        "UPDATE organization_invitations SET accepted_at=to_timestamp($2::double precision/1000) \
         WHERE id=$1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(invitation_id)
    .bind(now)
    .execute(&mut *transaction)
    .await;
    if !matches!(consumed, Ok(result) if result.rows_affected() == 1)
        || transaction.commit().await.is_err()
    {
        terminal_management_audit(
            &state,
            Some(organization_id),
            None,
            None,
            request_id,
            "organization.invitation.accept",
            "organization_invitation",
            Some(invitation_id),
            AuditOutcome::Failure,
        )
        .await;
        return control_plane_unavailable(request_id).into_response();
    }
    let issue = match management
        .platform_auth
        .sign_in(
            &normalized_email,
            SecretString::new(password.as_str().to_owned()),
            now,
        )
        .await
    {
        Ok(value) => value,
        Err(_) => {
            terminal_management_audit(
                &state,
                Some(organization_id),
                None,
                Some(user_id),
                request_id,
                "organization.invitation.accept",
                "organization_invitation",
                Some(invitation_id),
                AuditOutcome::Failure,
            )
            .await;
            return control_plane_unavailable(request_id).into_response();
        }
    };
    terminal_management_audit(
        &state,
        Some(organization_id),
        None,
        Some(user_id),
        request_id,
        "organization.invitation.accept",
        "organization_invitation",
        Some(invitation_id),
        AuditOutcome::Success,
    )
    .await;
    (StatusCode::CREATED, Json(session_response(issue))).into_response()
}

fn invitation_error(request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invitation.invalid",
        "the invitation is invalid or expired",
        request_id,
    )
    .into_response()
}

fn invitation_action_url(base: &Url, token: &str) -> Option<Url> {
    let mut url = base.join("accept-invitation").ok()?;
    url.set_fragment(Some(&format!("token={token}")));
    Some(url)
}

fn normalize_platform_email(email: &str) -> Option<String> {
    if email.len() > 254 || email.trim() != email || !email.is_ascii() {
        return None;
    }
    let (local, domain) = email.rsplit_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || !domain.contains('.')
        || email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return None;
    }
    Some(format!(
        "{}@{}",
        local.to_ascii_lowercase(),
        domain.to_ascii_lowercase()
    ))
}

pub(crate) async fn update_member(
    State(state): State<ApiState>,
    Path((organization, user)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<UpdateOrganizationMemberRequest>,
) -> Response {
    membership_change(
        state,
        organization,
        user,
        request_id,
        headers,
        Some(payload.role),
    )
    .await
}

pub(crate) async fn remove_member(
    State(state): State<ApiState>,
    Path((organization, user)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    membership_change(state, organization, user, request_id, headers, None).await
}

async fn membership_change(
    state: ApiState,
    organization: String,
    user: String,
    request_id: RequestId,
    headers: HeaderMap,
    role: Option<OrganizationRole>,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let target_user_id = match Uuid::parse_str(&user) {
        Ok(value) if !value.is_nil() => UserId(value),
        _ => return membership_error(MembershipMutationError::Invalid, request_id),
    };
    if let Err(error) =
        authorized_organization_admin(&management, identity.user_id, organization_id, request_id)
            .await
    {
        return error.into_response();
    }
    if let Err(response) = enforce_platform_user_rate(&state, identity.user_id, request_id).await {
        return response;
    }
    let action = if role.is_some() {
        "organization.member.update"
    } else {
        "organization.member.remove"
    };
    if require_membership_audit(
        &state,
        organization_id,
        identity.user_id,
        request_id,
        action,
    )
    .await
    .is_err()
    {
        return super::audit_unavailable(request_id).into_response();
    }
    let result = if let Some(role) = role {
        update_member_transaction(
            &management.pool,
            organization_id,
            identity.user_id,
            target_user_id,
            role,
        )
        .await
        .map(Some)
    } else {
        remove_member_transaction(
            &management.pool,
            organization_id,
            identity.user_id,
            target_user_id,
        )
        .await
        .map(|()| None)
    };
    match result {
        Ok(Some(value)) => {
            terminal_membership_audit(
                &state,
                organization_id,
                identity.user_id,
                request_id,
                action,
                AuditOutcome::Success,
            )
            .await;
            Json(value).into_response()
        }
        Ok(None) => {
            terminal_membership_audit(
                &state,
                organization_id,
                identity.user_id,
                request_id,
                action,
                AuditOutcome::Success,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            terminal_membership_audit(
                &state,
                organization_id,
                identity.user_id,
                request_id,
                action,
                membership_audit_outcome(error),
            )
            .await;
            membership_error(error, request_id)
        }
    }
}

pub(crate) async fn create_project(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateProjectRequest>,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(instance) = &state.instance else {
        return super::instance::service_error(
            super::instance::InstanceServiceError::Unavailable,
            request_id,
        );
    };
    if let Err(error) = instance.require_setup_complete().await {
        return super::instance::service_error(error, request_id);
    }
    if let Err(error) = authorized_organization_admin(
        &management,
        identity.user_id,
        payload.organization_id,
        request_id,
    )
    .await
    {
        return error.into_response();
    }
    if let Err(response) = enforce_platform_user_rate(&state, identity.user_id, request_id).await {
        return response;
    }
    if let Err(response) = require_management_audit(
        &state,
        Some(payload.organization_id),
        None,
        Some(identity.user_id),
        request_id,
        "project.create",
        "project",
        None,
    )
    .await
    {
        return response;
    }
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let request_hash = match super::idempotency::request_hash(&json!({
        "actor": identity.user_id,
        "request": payload,
    })) {
        Ok(value) => value,
        Err(error) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Failure,
            )
            .await;
            return super::idempotency_error(error, request_id);
        }
    };
    let claim = match super::idempotency::admit(
        &management.pool,
        super::idempotency::Scope::Organization(payload.organization_id),
        "project.create",
        key,
        request_hash,
    )
    .await
    {
        Ok(super::idempotency::Admission::Owner(claim)) => claim,
        Ok(super::idempotency::Admission::Replay { status, body }) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                body.get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok()),
                AuditOutcome::Success,
            )
            .await;
            return (status, Json(body)).into_response();
        }
        Ok(super::idempotency::Admission::Conflict) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return ApiError::new(
                StatusCode::CONFLICT,
                "idempotency.request_conflict",
                "the idempotency key was already used for a different request",
                request_id,
            )
            .into_response();
        }
        Ok(super::idempotency::Admission::InProgress) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return ApiError::new(
                StatusCode::CONFLICT,
                "idempotency.in_progress",
                "an operation with this idempotency key is still in progress",
                request_id,
            )
            .into_response();
        }
        Err(error) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Failure,
            )
            .await;
            return super::idempotency_error(error, request_id);
        }
    };
    let lease_heartbeat =
        super::idempotency::LeaseHeartbeat::start(management.pool.clone(), claim.clone());
    let provisioned = match management
        .registry
        .create_project_authorized(
            payload.clone(),
            management.node_id,
            identity.user_id,
            now_ms(),
        )
        .await
    {
        Ok(value) => value,
        Err(RegistryError::Conflict) => {
            match recover_completed_project(&management, &payload).await {
                Ok(Some(project)) => {
                    let body = serde_json::to_value(&project).unwrap_or(Value::Null);
                    if !super::idempotency::confirm_owner(
                        &management.pool,
                        &claim,
                        &lease_heartbeat,
                    )
                    .await
                    {
                        terminal_management_audit(
                            &state,
                            Some(payload.organization_id),
                            Some(project.id),
                            Some(identity.user_id),
                            request_id,
                            "project.create",
                            "project",
                            Some(project.id.0),
                            AuditOutcome::Failure,
                        )
                        .await;
                        return control_plane_unavailable(request_id).into_response();
                    }
                    if super::idempotency::complete(
                        &management.pool,
                        &claim,
                        StatusCode::CREATED,
                        &body,
                    )
                    .await
                    .is_err()
                    {
                        terminal_management_audit(
                            &state,
                            Some(payload.organization_id),
                            Some(project.id),
                            Some(identity.user_id),
                            request_id,
                            "project.create",
                            "project",
                            Some(project.id.0),
                            AuditOutcome::Failure,
                        )
                        .await;
                        return control_plane_unavailable(request_id).into_response();
                    }
                    terminal_management_audit(
                        &state,
                        Some(payload.organization_id),
                        Some(project.id),
                        Some(identity.user_id),
                        request_id,
                        "project.create",
                        "project",
                        Some(project.id.0),
                        AuditOutcome::Success,
                    )
                    .await;
                    return (StatusCode::CREATED, Json(project)).into_response();
                }
                Ok(None) => {}
                Err(_) => {
                    terminal_management_audit(
                        &state,
                        Some(payload.organization_id),
                        None,
                        Some(identity.user_id),
                        request_id,
                        "project.create",
                        "project",
                        None,
                        AuditOutcome::Failure,
                    )
                    .await;
                    return super::abandon_then(
                        &state,
                        Some(&claim),
                        control_plane_unavailable(request_id).into_response(),
                    )
                    .await;
                }
            }
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Denied,
            )
            .await;
            return super::abandon_then(
                &state,
                Some(&claim),
                registry_error(RegistryError::Conflict, request_id).into_response(),
            )
            .await;
        }
        Err(error) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Failure,
            )
            .await;
            return super::abandon_then(
                &state,
                Some(&claim),
                registry_error(error, request_id).into_response(),
            )
            .await;
        }
    };
    match provision(
        &state,
        &management,
        provisioned.project,
        provisioned.route,
        request_id,
    )
    .await
    {
        Ok(project) => {
            let body = serde_json::to_value(&project).unwrap_or(Value::Null);
            if !super::idempotency::confirm_owner(&management.pool, &claim, &lease_heartbeat).await
            {
                terminal_management_audit(
                    &state,
                    Some(payload.organization_id),
                    Some(project.id),
                    Some(identity.user_id),
                    request_id,
                    "project.create",
                    "project",
                    Some(project.id.0),
                    AuditOutcome::Failure,
                )
                .await;
                return control_plane_unavailable(request_id).into_response();
            }
            if super::idempotency::complete(&management.pool, &claim, StatusCode::CREATED, &body)
                .await
                .is_err()
            {
                terminal_management_audit(
                    &state,
                    Some(payload.organization_id),
                    Some(project.id),
                    Some(identity.user_id),
                    request_id,
                    "project.create",
                    "project",
                    Some(project.id.0),
                    AuditOutcome::Failure,
                )
                .await;
                return control_plane_unavailable(request_id).into_response();
            }
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                Some(project.id),
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                Some(project.id.0),
                AuditOutcome::Success,
            )
            .await;
            (StatusCode::CREATED, Json(project)).into_response()
        }
        Err(error) => {
            terminal_management_audit(
                &state,
                Some(payload.organization_id),
                None,
                Some(identity.user_id),
                request_id,
                "project.create",
                "project",
                None,
                AuditOutcome::Failure,
            )
            .await;
            super::abandon_then(&state, Some(&claim), error.into_response()).await
        }
    }
}

async fn recover_completed_project(
    management: &ManagementState,
    request: &CreateProjectRequest,
) -> Result<Option<ProjectSummary>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT p.id,p.organization_id,p.display_name,p.slug,p.region,p.lifecycle_state,\
         d.schema_version,(extract(epoch FROM p.created_at)*1000)::bigint created_at_ms \
         FROM projects p JOIN project_databases d ON d.id=p.database_id \
         WHERE p.organization_id=$1 AND p.slug=$2",
    )
    .bind(request.organization_id.0)
    .bind(&request.slug)
    .fetch_optional(&management.pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let name: String = row.try_get("display_name")?;
    let region: String = row.try_get("region")?;
    let lifecycle: String = row.try_get("lifecycle_state")?;
    let requested_region = request.region.as_deref().unwrap_or("local");
    if name != request.name || region != requested_region || lifecycle != "active" {
        return Ok(None);
    }
    let schema_version: i64 = row.try_get("schema_version")?;
    Ok(Some(ProjectSummary {
        id: ProjectId(row.try_get("id")?),
        organization_id: OrganizationId(row.try_get("organization_id")?),
        name,
        slug: row.try_get("slug")?,
        region,
        state: ProjectLifecycleState::Active,
        schema_version: u64::try_from(schema_version).unwrap_or_default(),
        created_at_ms: row.try_get("created_at_ms")?,
    }))
}

pub(crate) async fn create_api_key(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id =
        match authorized_project_admin(&management, identity.user_id, project_id, request_id).await
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let now = now_ms();
    if payload.name.trim() != payload.name
        || payload.name.is_empty()
        || payload.name.len() > 128
        || payload.scopes.is_empty()
        || payload
            .scopes
            .iter()
            .enumerate()
            .any(|(index, scope)| payload.scopes[index + 1..].contains(scope))
        || payload.expires_at_ms.is_some_and(|expiry| expiry <= now)
    {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "api_key.invalid",
            "invalid API key request",
            request_id,
        )
        .into_response();
    }
    let issue = match management.api_key_codec.issue(
        organization_id,
        Some(project_id),
        payload.scopes.clone(),
    ) {
        Ok(value) => value,
        Err(_) => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "api_key.unavailable",
                "API key service is unavailable",
                request_id,
            )
            .into_response();
        }
    };
    let mut record = issue.record;
    record.expires_at_ms = payload.expires_at_ms;
    if let Err(response) = require_management_audit(
        &state,
        Some(organization_id),
        Some(project_id),
        Some(identity.user_id),
        request_id,
        "api_key.create",
        "api_key",
        Some(record.id.0),
    )
    .await
    {
        return response;
    }
    if management
        .api_keys
        .insert(&record, &payload.name, identity.user_id, now)
        .await
        .is_err()
    {
        terminal_management_audit(
            &state,
            Some(organization_id),
            Some(project_id),
            Some(identity.user_id),
            request_id,
            "api_key.create",
            "api_key",
            Some(record.id.0),
            AuditOutcome::Failure,
        )
        .await;
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_key.unavailable",
            "API key service is unavailable",
            request_id,
        )
        .into_response();
    }
    terminal_management_audit(
        &state,
        Some(organization_id),
        Some(project_id),
        Some(identity.user_id),
        request_id,
        "api_key.create",
        "api_key",
        Some(record.id.0),
        AuditOutcome::Success,
    )
    .await;
    (
        StatusCode::CREATED,
        Json(CreatedApiKey {
            id: record.id,
            name: payload.name,
            prefix: record.prefix,
            secret: SensitiveString::new(issue.plaintext.expose()),
            scopes: record.scopes,
            expires_at_ms: record.expires_at_ms,
            created_at_ms: now,
        }),
    )
        .into_response()
}

pub(crate) async fn api_keys(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) =
        authorized_project_admin(&management, identity.user_id, project_id, request_id).await
    {
        return error.into_response();
    }
    let rows = match sqlx::query(
        "SELECT id,name,lookup_prefix,scopes, \
         (extract(epoch FROM expires_at)*1000)::bigint expires_at_ms, \
         (extract(epoch FROM created_at)*1000)::bigint created_at_ms, \
         (extract(epoch FROM revoked_at)*1000)::bigint revoked_at_ms \
         FROM api_keys WHERE project_id=$1 ORDER BY created_at DESC LIMIT 1000",
    )
    .bind(project_id.0)
    .fetch_all(&management.pool)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    };
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let scope_names = match row.try_get::<Vec<String>, _>("scopes") {
            Ok(value) => value,
            Err(_) => return control_plane_unavailable(request_id).into_response(),
        };
        let scopes = match scope_names
            .iter()
            .map(|scope| decode_scope(scope))
            .collect::<Option<Vec<_>>>()
        {
            Some(value) => value,
            None => return control_plane_unavailable(request_id).into_response(),
        };
        let summary = ApiKeySummary {
            id: ApiKeyId(row.get("id")),
            name: row.get("name"),
            prefix: row.get("lookup_prefix"),
            scopes,
            expires_at_ms: row.get("expires_at_ms"),
            created_at_ms: row.get("created_at_ms"),
            revoked_at_ms: row.get("revoked_at_ms"),
        };
        values.push(summary);
    }
    Json(values).into_response()
}

pub(crate) async fn revoke_api_key(
    State(state): State<ApiState>,
    Path((project, key)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let key_id = match Uuid::parse_str(&key) {
        Ok(value) => ApiKeyId(value),
        Err(_) => {
            return ApiError::new(
                StatusCode::BAD_REQUEST,
                "api_key.invalid_id",
                "invalid API key identifier",
                request_id,
            )
            .into_response();
        }
    };
    let organization_id =
        match authorized_project_admin(&management, identity.user_id, project_id, request_id).await
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    if let Err(response) = require_management_audit(
        &state,
        Some(organization_id),
        Some(project_id),
        Some(identity.user_id),
        request_id,
        "api_key.revoke",
        "api_key",
        Some(key_id.0),
    )
    .await
    {
        return response;
    }
    let result = match sqlx::query(
        "UPDATE api_keys SET revoked_at=COALESCE(revoked_at,now()) WHERE id=$1 AND project_id=$2",
    )
    .bind(key_id.0)
    .bind(project_id.0)
    .execute(&management.pool)
    .await
    {
        Ok(value) => value,
        Err(_) => {
            terminal_management_audit(
                &state,
                Some(organization_id),
                Some(project_id),
                Some(identity.user_id),
                request_id,
                "api_key.revoke",
                "api_key",
                Some(key_id.0),
                AuditOutcome::Failure,
            )
            .await;
            return control_plane_unavailable(request_id).into_response();
        }
    };
    if result.rows_affected() == 0 {
        terminal_management_audit(
            &state,
            Some(organization_id),
            Some(project_id),
            Some(identity.user_id),
            request_id,
            "api_key.revoke",
            "api_key",
            Some(key_id.0),
            AuditOutcome::Denied,
        )
        .await;
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "api_key.not_found",
            "API key was not found",
            request_id,
        )
        .into_response();
    }
    terminal_management_audit(
        &state,
        Some(organization_id),
        Some(project_id),
        Some(identity.user_id),
        request_id,
        "api_key.revoke",
        "api_key",
        Some(key_id.0),
        AuditOutcome::Success,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn rotate_signing_key(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id =
        match authorized_project_admin(&management, identity.user_id, project_id, request_id).await
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    if let Err(response) = require_management_audit(
        &state,
        Some(organization_id),
        Some(project_id),
        Some(identity.user_id),
        request_id,
        "signing_key.rotate",
        "signing_key",
        None,
    )
    .await
    {
        return response;
    }
    match management
        .signing_keys
        .rotate(project_id, now_ms() / 1_000, 15 * 60)
        .await
    {
        Ok(rotation) => {
            terminal_management_audit(
                &state,
                Some(organization_id),
                Some(project_id),
                Some(identity.user_id),
                request_id,
                "signing_key.rotate",
                "signing_key",
                None,
                AuditOutcome::Success,
            )
            .await;
            Json(json!({
                "active_kid": rotation.active.kid,
                "previous_kid": rotation.previous_kid,
                "previous_valid_until_seconds": rotation.previous_valid_until_seconds
            }))
            .into_response()
        }
        Err(error) => {
            terminal_management_audit(
                &state,
                Some(organization_id),
                Some(project_id),
                Some(identity.user_id),
                request_id,
                "signing_key.rotate",
                "signing_key",
                None,
                AuditOutcome::Failure,
            )
            .await;
            signing_key_error(error, request_id).into_response()
        }
    }
}

pub(crate) async fn authenticated(
    state: &ApiState,
    headers: &HeaderMap,
    request_id: RequestId,
) -> Result<(Arc<ManagementState>, PlatformSessionIdentity), ApiError> {
    let management = required(state, request_id)?;
    let token = bearer(headers)
        .map_err(|_| platform_error(PlatformAuthError::InvalidCredentials, request_id))?;
    let identity = management
        .platform_auth
        .authenticate(token, now_ms())
        .await
        .map_err(|error| platform_error(error, request_id))?;
    Ok((management, identity))
}

async fn authorized_project_admin(
    management: &ManagementState,
    user_id: ffdb_protocol::UserId,
    project_id: ProjectId,
    request_id: RequestId,
) -> Result<OrganizationId, ApiError> {
    let row = sqlx::query_scalar::<_, Uuid>(
        "SELECT p.organization_id FROM projects p JOIN organization_memberships m \
         ON m.organization_id=p.organization_id WHERE p.id=$1 AND m.user_id=$2 \
         AND m.role IN ('owner','admin')",
    )
    .bind(project_id.0)
    .bind(user_id.0)
    .fetch_optional(&management.pool)
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_plane.unavailable",
            "control plane is unavailable",
            request_id,
        )
    })?;
    row.map(OrganizationId).ok_or_else(|| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "control_plane.forbidden",
            "operation is not permitted",
            request_id,
        )
    })
}

pub(crate) async fn authorized_project_member(
    management: &ManagementState,
    user_id: ffdb_protocol::UserId,
    project_id: ProjectId,
    request_id: RequestId,
) -> Result<OrganizationId, ApiError> {
    let row = sqlx::query_scalar::<_, Uuid>(
        "SELECT p.organization_id FROM projects p JOIN organization_memberships m \
         ON m.organization_id=p.organization_id WHERE p.id=$1 AND m.user_id=$2",
    )
    .bind(project_id.0)
    .bind(user_id.0)
    .fetch_optional(&management.pool)
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_plane.unavailable",
            "control plane is unavailable",
            request_id,
        )
    })?;
    row.map(OrganizationId).ok_or_else(|| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "control_plane.forbidden",
            "operation is not permitted",
            request_id,
        )
    })
}

pub(crate) async fn authorized_organization_admin(
    management: &ManagementState,
    user_id: UserId,
    organization_id: OrganizationId,
    request_id: RequestId,
) -> Result<OrganizationRole, ApiError> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT m.role FROM organization_memberships m JOIN organizations o \
         ON o.id=m.organization_id WHERE m.organization_id=$1 AND m.user_id=$2 \
         AND o.disabled_at IS NULL AND m.role IN ('owner','admin')",
    )
    .bind(organization_id.0)
    .bind(user_id.0)
    .fetch_optional(&management.pool)
    .await
    .map_err(|_| control_plane_unavailable(request_id))?;
    role.as_deref()
        .and_then(parse_membership_role)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "control_plane.forbidden",
                "operation is not permitted",
                request_id,
            )
        })
}

#[derive(Clone, Copy, Debug)]
enum MembershipMutationError {
    Invalid,
    Forbidden,
    LastOwner,
    NotFound,
    Conflict,
    Unavailable,
}

async fn lock_organization_actor<'a>(
    transaction: &mut Transaction<'a, Postgres>,
    organization_id: OrganizationId,
    actor: UserId,
) -> Result<OrganizationRole, MembershipMutationError> {
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM organizations WHERE id=$1 AND disabled_at IS NULL FOR UPDATE",
    )
    .bind(organization_id.0)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    if exists.is_none() {
        return Err(MembershipMutationError::NotFound);
    }
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM organization_memberships WHERE organization_id=$1 \
         AND user_id=$2 FOR UPDATE",
    )
    .bind(organization_id.0)
    .bind(actor.0)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    match role.as_deref().and_then(parse_membership_role) {
        Some(role @ (OrganizationRole::Owner | OrganizationRole::Admin)) => Ok(role),
        _ => Err(MembershipMutationError::Forbidden),
    }
}

async fn add_member_transaction(
    pool: &PgPool,
    organization_id: OrganizationId,
    actor: UserId,
    email: &str,
    role: OrganizationRole,
) -> Result<OrganizationMembershipSummary, MembershipMutationError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| MembershipMutationError::Unavailable)?;
    let actor_role = lock_organization_actor(&mut transaction, organization_id, actor).await?;
    if role == OrganizationRole::Owner && actor_role != OrganizationRole::Owner {
        return Err(MembershipMutationError::Forbidden);
    }
    let target: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM platform_users WHERE lower(email)=lower($1) \
         AND disabled_at IS NULL FOR SHARE",
    )
    .bind(email)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    let target = target.ok_or(MembershipMutationError::NotFound)?;
    let inserted = sqlx::query(
        "INSERT INTO organization_memberships (organization_id,user_id,role) \
         VALUES ($1,$2,$3) ON CONFLICT (organization_id,user_id) DO NOTHING",
    )
    .bind(organization_id.0)
    .bind(target)
    .bind(membership_role_name(role))
    .execute(&mut *transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    if inserted.rows_affected() != 1 {
        return Err(MembershipMutationError::Conflict);
    }
    let value = fetch_membership(&mut transaction, organization_id, UserId(target)).await?;
    transaction
        .commit()
        .await
        .map_err(|_| MembershipMutationError::Unavailable)?;
    Ok(value)
}

async fn update_member_transaction(
    pool: &PgPool,
    organization_id: OrganizationId,
    actor: UserId,
    target: UserId,
    role: OrganizationRole,
) -> Result<OrganizationMembershipSummary, MembershipMutationError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| MembershipMutationError::Unavailable)?;
    let actor_role = lock_organization_actor(&mut transaction, organization_id, actor).await?;
    let current: Option<String> = sqlx::query_scalar(
        "SELECT role FROM organization_memberships WHERE organization_id=$1 \
         AND user_id=$2 FOR UPDATE",
    )
    .bind(organization_id.0)
    .bind(target.0)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    let current = current
        .as_deref()
        .and_then(parse_membership_role)
        .ok_or(MembershipMutationError::NotFound)?;
    authorize_member_transition(
        &mut transaction,
        organization_id,
        actor_role,
        current,
        Some(role),
    )
    .await?;
    sqlx::query(
        "UPDATE organization_memberships SET role=$3 WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(organization_id.0)
    .bind(target.0)
    .bind(membership_role_name(role))
    .execute(&mut *transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    let value = fetch_membership(&mut transaction, organization_id, target).await?;
    transaction
        .commit()
        .await
        .map_err(|_| MembershipMutationError::Unavailable)?;
    Ok(value)
}

async fn remove_member_transaction(
    pool: &PgPool,
    organization_id: OrganizationId,
    actor: UserId,
    target: UserId,
) -> Result<(), MembershipMutationError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| MembershipMutationError::Unavailable)?;
    let actor_role = lock_organization_actor(&mut transaction, organization_id, actor).await?;
    let current: Option<String> = sqlx::query_scalar(
        "SELECT role FROM organization_memberships WHERE organization_id=$1 \
         AND user_id=$2 FOR UPDATE",
    )
    .bind(organization_id.0)
    .bind(target.0)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?;
    let current = current
        .as_deref()
        .and_then(parse_membership_role)
        .ok_or(MembershipMutationError::NotFound)?;
    authorize_member_transition(&mut transaction, organization_id, actor_role, current, None)
        .await?;
    let deleted =
        sqlx::query("DELETE FROM organization_memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(organization_id.0)
            .bind(target.0)
            .execute(&mut *transaction)
            .await
            .map_err(|_| MembershipMutationError::Unavailable)?;
    if deleted.rows_affected() != 1 {
        return Err(MembershipMutationError::NotFound);
    }
    transaction
        .commit()
        .await
        .map_err(|_| MembershipMutationError::Unavailable)
}

async fn authorize_member_transition(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: OrganizationId,
    actor_role: OrganizationRole,
    current: OrganizationRole,
    next: Option<OrganizationRole>,
) -> Result<(), MembershipMutationError> {
    let owner_count = if current == OrganizationRole::Owner && next != Some(OrganizationRole::Owner)
    {
        sqlx::query_scalar(
            "SELECT count(*) FROM organization_memberships \
             WHERE organization_id=$1 AND role='owner'",
        )
        .bind(organization_id.0)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| MembershipMutationError::Unavailable)?
    } else {
        2
    };
    validate_member_transition(actor_role, current, next, owner_count)
}

fn validate_member_transition(
    actor_role: OrganizationRole,
    current: OrganizationRole,
    next: Option<OrganizationRole>,
    owner_count: i64,
) -> Result<(), MembershipMutationError> {
    if actor_role != OrganizationRole::Owner
        && (current == OrganizationRole::Owner || next == Some(OrganizationRole::Owner))
    {
        return Err(MembershipMutationError::Forbidden);
    }
    if current == OrganizationRole::Owner
        && next != Some(OrganizationRole::Owner)
        && owner_count <= 1
    {
        return Err(MembershipMutationError::LastOwner);
    }
    Ok(())
}

async fn fetch_membership(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: OrganizationId,
    user_id: UserId,
) -> Result<OrganizationMembershipSummary, MembershipMutationError> {
    let row = sqlx::query(
        "SELECT m.organization_id,m.user_id,u.email,m.role, \
                (extract(epoch FROM m.created_at)*1000)::bigint created_at_ms \
         FROM organization_memberships m JOIN platform_users u ON u.id=m.user_id \
         WHERE m.organization_id=$1 AND m.user_id=$2",
    )
    .bind(organization_id.0)
    .bind(user_id.0)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| MembershipMutationError::Unavailable)?
    .ok_or(MembershipMutationError::NotFound)?;
    membership_from_row(row).map_err(|_| MembershipMutationError::Unavailable)
}

fn membership_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<OrganizationMembershipSummary, sqlx::Error> {
    let role: String = row.try_get("role")?;
    Ok(OrganizationMembershipSummary {
        organization_id: OrganizationId(row.try_get("organization_id")?),
        user_id: UserId(row.try_get("user_id")?),
        email: row.try_get("email")?,
        role: parse_membership_role(&role)
            .ok_or_else(|| sqlx::Error::Decode("invalid organization role".into()))?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

fn parse_membership_role(value: &str) -> Option<OrganizationRole> {
    match value {
        "owner" => Some(OrganizationRole::Owner),
        "admin" => Some(OrganizationRole::Admin),
        "developer" => Some(OrganizationRole::Developer),
        "viewer" => Some(OrganizationRole::Viewer),
        _ => None,
    }
}

fn membership_role_name(value: OrganizationRole) -> &'static str {
    match value {
        OrganizationRole::Owner => "owner",
        OrganizationRole::Admin => "admin",
        OrganizationRole::Developer => "developer",
        OrganizationRole::Viewer => "viewer",
    }
}

async fn enforce_platform_user_rate(
    state: &ApiState,
    user_id: UserId,
    request_id: RequestId,
) -> Result<(), Response> {
    let Some(limiter) = &state.rate_limiter else {
        return Ok(());
    };
    super::enforce_rate_dimension(
        limiter,
        RateDimension::User,
        user_id.to_string().as_bytes(),
        1,
        request_id,
    )
    .await
}

async fn append_membership_audit(
    state: &ApiState,
    organization_id: OrganizationId,
    actor: UserId,
    request_id: RequestId,
    action: &str,
    outcome: AuditOutcome,
) -> Result<(), ()> {
    append_management_audit(
        state,
        Some(organization_id),
        None,
        Some(actor),
        request_id,
        action,
        "organization_membership",
        None,
        outcome,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn append_management_audit(
    state: &ApiState,
    organization_id: Option<OrganizationId>,
    project_id: Option<ProjectId>,
    actor: Option<UserId>,
    request_id: RequestId,
    action: &str,
    resource_kind: &str,
    resource_id: Option<Uuid>,
    outcome: AuditOutcome,
) -> Result<(), ()> {
    state
        .audit
        .append(AuditDraft {
            occurred_at_ms: now_ms(),
            organization_id,
            project_id,
            request_id,
            actor_kind: if actor.is_some() {
                ActorKind::User
            } else {
                ActorKind::Anonymous
            },
            actor_id: actor.map(|value| value.0),
            action: action.to_owned(),
            resource_kind: resource_kind.to_owned(),
            resource_id,
            outcome,
            source_ip: super::trusted_source_ip(),
            metadata: json!({"protocol_version": PROTOCOL_VERSION}),
        })
        .await
        .map(|_| ())
        .map_err(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn require_management_audit(
    state: &ApiState,
    organization_id: Option<OrganizationId>,
    project_id: Option<ProjectId>,
    actor: Option<UserId>,
    request_id: RequestId,
    action: &str,
    resource_kind: &str,
    resource_id: Option<Uuid>,
) -> Result<(), Response> {
    append_management_audit(
        state,
        organization_id,
        project_id,
        actor,
        request_id,
        &format!("{action}.requested"),
        resource_kind,
        resource_id,
        AuditOutcome::Success,
    )
    .await
    .map_err(|()| super::audit_unavailable(request_id).into_response())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn terminal_management_audit(
    state: &ApiState,
    organization_id: Option<OrganizationId>,
    project_id: Option<ProjectId>,
    actor: Option<UserId>,
    request_id: RequestId,
    action: &str,
    resource_kind: &str,
    resource_id: Option<Uuid>,
    outcome: AuditOutcome,
) {
    if append_management_audit(
        state,
        organization_id,
        project_id,
        actor,
        request_id,
        action,
        resource_kind,
        resource_id,
        outcome,
    )
    .await
    .is_err()
    {
        tracing::error!(%request_id, action, "management audit failed");
    }
}

async fn require_membership_audit(
    state: &ApiState,
    organization_id: OrganizationId,
    actor: UserId,
    request_id: RequestId,
    action: &str,
) -> Result<(), ()> {
    append_membership_audit(
        state,
        organization_id,
        actor,
        request_id,
        &format!("{action}.requested"),
        AuditOutcome::Success,
    )
    .await
}

async fn terminal_membership_audit(
    state: &ApiState,
    organization_id: OrganizationId,
    actor: UserId,
    request_id: RequestId,
    action: &str,
    outcome: AuditOutcome,
) {
    if append_membership_audit(state, organization_id, actor, request_id, action, outcome)
        .await
        .is_err()
    {
        tracing::error!(%organization_id, %request_id, action, "membership audit failed");
    }
}

fn membership_audit_outcome(error: MembershipMutationError) -> AuditOutcome {
    if matches!(error, MembershipMutationError::Unavailable) {
        AuditOutcome::Failure
    } else {
        AuditOutcome::Denied
    }
}

fn membership_error(error: MembershipMutationError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        MembershipMutationError::Invalid => (
            StatusCode::BAD_REQUEST,
            "membership.invalid",
            "membership input is invalid",
        ),
        MembershipMutationError::Forbidden => (
            StatusCode::FORBIDDEN,
            "membership.forbidden",
            "membership operation is not permitted",
        ),
        MembershipMutationError::LastOwner => (
            StatusCode::CONFLICT,
            "membership.last_owner",
            "the final organization owner cannot be removed or demoted",
        ),
        MembershipMutationError::NotFound => (
            StatusCode::NOT_FOUND,
            "membership.not_found",
            "organization member was not found",
        ),
        MembershipMutationError::Conflict => (
            StatusCode::CONFLICT,
            "membership.exists",
            "organization membership already exists",
        ),
        MembershipMutationError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "membership.unavailable",
            "membership service is unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}

async fn provision(
    state: &ApiState,
    management: &ManagementState,
    mut project: ProjectSummary,
    route: ffdb_protocol::DatabaseRoute,
    request_id: RequestId,
) -> Result<ProjectSummary, ApiError> {
    if let Err(error) = management
        .signing_keys
        .bootstrap(project.id, now_ms() / 1_000)
        .await
    {
        let _ignored = management
            .registry
            .set_project_state(project.id, ProjectLifecycleState::Failed)
            .await;
        return Err(signing_key_error(error, request_id));
    }
    let request = WorkerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        route: route.clone(),
        mode: ExecutionMode::Developer(DeveloperPrincipal {
            organization_id: project.organization_id,
            api_key_id: ApiKeyId::new(),
            scopes: vec![DeveloperScope::BackupsManage],
            actor_label: "service:project-provisioner".into(),
        }),
        deadline_epoch_ms: now_ms().saturating_add(state.limits.transaction_timeout_ms as i64),
        limits: state.limits.clone(),
        expected_schema_version: Some(0),
        operation_receipt_id: None,
        operation: WorkerOperation::IntegrityCheck,
    };
    if state.executor.execute(&route, request).await.is_err()
        || management
            .registry
            .set_project_state(project.id, ProjectLifecycleState::Active)
            .await
            .is_err()
    {
        let _ignored = management
            .registry
            .set_project_state(project.id, ProjectLifecycleState::Failed)
            .await;
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "project.provisioning_failed",
            "project provisioning failed",
            request_id,
        ));
    }
    project.state = ProjectLifecycleState::Active;
    Ok(project)
}

fn session_response(issue: PlatformSessionIssue) -> DeveloperSessionResponse {
    DeveloperSessionResponse {
        session_token: SensitiveString::new(issue.plaintext.expose()),
        user_id: issue.identity.user_id,
        email: issue.identity.normalized_email,
        expires_at_ms: issue.session.expires_at_ms,
    }
}

fn required(state: &ApiState, request_id: RequestId) -> Result<Arc<ManagementState>, ApiError> {
    state.management.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_plane.unavailable",
            "control plane is unavailable",
            request_id,
        )
    })
}

fn parse_organization(value: &str, request_id: RequestId) -> Result<OrganizationId, ApiError> {
    Uuid::parse_str(value).map(OrganizationId).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "organization.invalid_id",
            "invalid organization identifier",
            request_id,
        )
    })
}

fn platform_error(error: PlatformAuthError, request_id: RequestId) -> ApiError {
    match error {
        PlatformAuthError::AlreadyInitialized => ApiError::new(
            StatusCode::CONFLICT,
            "developer.already_initialized",
            "developer authentication is already initialized",
            request_id,
        ),
        PlatformAuthError::InvalidEmail | PlatformAuthError::InvalidPassword => ApiError::new(
            StatusCode::BAD_REQUEST,
            "developer.invalid_input",
            "developer authentication input is invalid",
            request_id,
        ),
        PlatformAuthError::InvalidCredentials
        | PlatformAuthError::InvalidSession
        | PlatformAuthError::VerificationRequired => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "developer.invalid_credentials",
            "developer credentials are invalid",
            request_id,
        ),
        PlatformAuthError::Disabled => ApiError::new(
            StatusCode::FORBIDDEN,
            "developer.disabled",
            "developer account is disabled",
            request_id,
        ),
        PlatformAuthError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "developer.unavailable",
            "developer authentication is unavailable",
            request_id,
        ),
    }
}

fn registry_error(error: RegistryError, request_id: RequestId) -> ApiError {
    let (status, code, message) = match error {
        RegistryError::InvalidInput => (
            StatusCode::BAD_REQUEST,
            "control_plane.invalid_input",
            "control-plane input is invalid",
        ),
        RegistryError::NotFound => (
            StatusCode::NOT_FOUND,
            "control_plane.not_found",
            "resource was not found",
        ),
        RegistryError::Conflict => (
            StatusCode::CONFLICT,
            "control_plane.conflict",
            "resource already exists",
        ),
        RegistryError::Forbidden => (
            StatusCode::FORBIDDEN,
            "control_plane.forbidden",
            "operation is not permitted",
        ),
        RegistryError::BillingLimit => (
            StatusCode::PAYMENT_REQUIRED,
            "billing.project_limit_reached",
            "the organization project allowance is exhausted",
        ),
        RegistryError::Unavailable
        | RegistryError::DatastoreUnavailable
        | RegistryError::StaleGeneration
        | RegistryError::Inconsistent => (
            StatusCode::SERVICE_UNAVAILABLE,
            "control_plane.unavailable",
            "control plane is unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id)
}

fn signing_key_error(error: SigningKeyManagementError, request_id: RequestId) -> ApiError {
    match error {
        SigningKeyManagementError::InvalidConfiguration
        | SigningKeyManagementError::Encryption
        | SigningKeyManagementError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "signing_key.unavailable",
            "signing key service is unavailable",
            request_id,
        ),
        SigningKeyManagementError::ProjectNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "project.not_found",
            "project was not found",
            request_id,
        ),
        SigningKeyManagementError::ActiveKeyExists => ApiError::new(
            StatusCode::CONFLICT,
            "signing_key.active_exists",
            "an active signing key already exists",
            request_id,
        ),
        SigningKeyManagementError::NoActiveKey | SigningKeyManagementError::InvalidInput => {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "signing_key.invalid",
                "signing key request is invalid",
                request_id,
            )
        }
    }
}

fn decode_scope(value: &str) -> Option<DeveloperScope> {
    match value {
        "projects_read" => Some(DeveloperScope::ProjectsRead),
        "projects_write" => Some(DeveloperScope::ProjectsWrite),
        "database_query" => Some(DeveloperScope::DatabaseQuery),
        "database_migrate" => Some(DeveloperScope::DatabaseMigrate),
        "database_schema" => Some(DeveloperScope::DatabaseSchema),
        "auth_manage" => Some(DeveloperScope::AuthManage),
        "storage_manage" => Some(DeveloperScope::StorageManage),
        "email_manage" => Some(DeveloperScope::EmailManage),
        "keys_rotate" => Some(DeveloperScope::KeysRotate),
        "backups_manage" => Some(DeveloperScope::BackupsManage),
        "logs_read" => Some(DeveloperScope::LogsRead),
        "commerce_manage" => Some(DeveloperScope::CommerceManage),
        _ => None,
    }
}

fn control_plane_unavailable(request_id: RequestId) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_plane.unavailable",
        "control plane is unavailable",
        request_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_transitions_protect_owners() {
        assert!(matches!(
            validate_member_transition(
                OrganizationRole::Admin,
                OrganizationRole::Owner,
                Some(OrganizationRole::Admin),
                2,
            ),
            Err(MembershipMutationError::Forbidden)
        ));
        assert!(matches!(
            validate_member_transition(OrganizationRole::Owner, OrganizationRole::Owner, None, 1,),
            Err(MembershipMutationError::LastOwner)
        ));
        assert!(
            validate_member_transition(
                OrganizationRole::Owner,
                OrganizationRole::Owner,
                Some(OrganizationRole::Admin),
                2,
            )
            .is_ok()
        );
        assert!(
            validate_member_transition(
                OrganizationRole::Admin,
                OrganizationRole::Developer,
                Some(OrganizationRole::Viewer),
                1,
            )
            .is_ok()
        );
    }

    #[test]
    fn invitation_email_normalization_matches_platform_auth_policy() {
        assert_eq!(
            normalize_platform_email("Admin@Example.test").as_deref(),
            Some("admin@example.test")
        );
        assert!(normalize_platform_email(" admin@example.test").is_none());
        assert!(normalize_platform_email("admin@localhost").is_none());
        assert!(normalize_platform_email("admin@example.test\n").is_none());
    }

    #[test]
    fn invitation_token_is_kept_out_of_http_request_components() {
        let base = Url::parse("https://portal.example.test/").ok();
        let url =
            base.and_then(|base| invitation_action_url(&base, "ffdb_invitation_prefix.secret"));
        assert_eq!(url.as_ref().and_then(Url::query), None);
        assert_eq!(url.as_ref().map(Url::path), Some("/accept-invitation"));
        assert!(
            url.as_ref()
                .and_then(Url::fragment)
                .is_some_and(|fragment| fragment.starts_with("token=ffdb_invitation_"))
        );
    }
}
