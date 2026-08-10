//! HTTP composition for the versioned FFDB data API.

mod auth;
mod billing;
mod commerce;
#[cfg(test)]
mod control_plane_migrations;
mod email;
mod host_updates;
mod idempotency;
mod instance;
mod management;
mod metering;
mod observability;
mod operations;
mod storage;
mod usage_reporting;
pub use auth::{OutboxAuthEmailDispatcher, ProjectAuthState};
pub use commerce::{CommerceConnectConfig, CommerceService, CommerceServiceConfig};
pub use host_updates::{CommandHostUpdater, HostUpdateError, HostUpdater};
pub use instance::{
    InstanceBillingProvider, InstanceService, InstanceServiceConfig, InstanceServiceError,
    InstanceStripeBillingConfig, InstanceStripeProviderCatalog, InstanceStripeUsageEventConfig,
};
pub use management::{ManagementState, ManagementStateConfig};
pub use metering::UsageMeteringService;
pub use observability::{ObservabilityService, ObservabilityWorkerHandle};
pub use storage::{StorageService, WorkerMetadataAuthorizer};
pub use usage_reporting::{
    ReportingCycleError, ReportingCycleSummary, UsageReportingConfig, UsageReportingService,
    UsageReportingWorkerHandle,
};

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::extract::{ConnectInfo, Path, Query, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use ffdb_audit::{ActorKind, AuditDraft, AuditOutcome, AuditSink};
use ffdb_database_router::{DatabaseExecutor, DatabaseRouter, ExecutionError, RoutingError};
use ffdb_email::PgEmailService;
use ffdb_observability::Metrics;
use ffdb_protocol::{
    AuthContext, BackupId, DatabaseId, DatabaseRoute, DeveloperPrincipal, DeveloperScope,
    ErrorEnvelope, ExecutionMode, MigrationSpec, NodeId, PROTOCOL_VERSION, PlatformError,
    ProjectId, QueryRequest, RequestId, ResourceLimits, SessionId, SnapshotRequest,
    SyncPullRequest, SyncPushRequest, TransactionRequest, WorkerOperation, WorkerRequest,
    WorkerResponse,
};
use ffdb_rate_limits::{
    PgTokenBucketLimiter, RateDimension, RateLimitDecision, RateLimitError, RateLimitKey,
};
use ipnet::IpNet;
use serde::Deserialize;
use serde_json::Value;
use sqlx::{PgPool, Row as _};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;
use zeroize::Zeroizing;

const AUTHORIZATION: axum::http::HeaderName = axum::http::header::AUTHORIZATION;

#[derive(Clone, Copy, Debug)]
struct TrustedClientIp(IpAddr);

tokio::task_local! {
    static REQUEST_SOURCE_IP: Option<IpAddr>;
}

fn trusted_source_ip() -> Option<IpAddr> {
    REQUEST_SOURCE_IP.try_with(|source| *source).ok().flatten()
}

#[derive(Clone)]
pub struct ApiState {
    pub router: Arc<dyn DatabaseRouter>,
    pub executor: Arc<dyn DatabaseExecutor>,
    pub credentials: Arc<dyn CredentialVerifier>,
    pub limits: ResourceLimits,
    pub metrics: Option<Arc<Metrics>>,
    pub observability: Option<Arc<ObservabilityService>>,
    pub management: Option<Arc<ManagementState>>,
    pub project_auth: Option<Arc<ProjectAuthState>>,
    pub storage: Option<Arc<StorageService>>,
    pub email: Option<Arc<PgEmailService>>,
    pub usage_metering: Option<Arc<UsageMeteringService>>,
    pub commerce: Option<Arc<CommerceService>>,
    pub instance: Option<Arc<InstanceService>>,
    pub host_updates: Option<Arc<dyn HostUpdater>>,
    pub cors_allowed_origins: Vec<String>,
    pub trusted_proxy_cidrs: Vec<IpNet>,
    pub rate_limiter: Option<Arc<dyn ApiRateLimiter>>,
    /// Required audit capability. Production and test compositions must choose
    /// an explicit sink; privileged paths never interpret an absent sink as a
    /// successful audit admission.
    pub audit: Arc<dyn AuditSink>,
    pub readiness_pool: Option<PgPool>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiState")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait ApiRateLimiter: Send + Sync {
    async fn check(
        &self,
        dimension: RateDimension,
        identifier: &[u8],
        cost: u32,
        now_ms: i64,
    ) -> Result<RateLimitDecision, RateLimitError>;

    async fn check_many(
        &self,
        checks: Vec<(RateDimension, Vec<u8>, u32)>,
        now_ms: i64,
    ) -> Result<Vec<RateLimitDecision>, RateLimitError> {
        let mut decisions = Vec::with_capacity(checks.len());
        for (dimension, identifier, cost) in checks {
            let decision = self.check(dimension, &identifier, cost, now_ms).await?;
            decisions.push(decision);
            if matches!(decision, RateLimitDecision::Denied { .. }) {
                break;
            }
        }
        Ok(decisions)
    }
}

/// Production rate-limiter adapter. Only HMAC-derived identifiers are sent to
/// PostgreSQL; the namespace secret is erased when the service is dropped.
#[derive(Clone, Debug)]
pub struct DurableRateLimiter {
    pre_auth_limiter: PgTokenBucketLimiter,
    execution_limiter: PgTokenBucketLimiter,
    namespace_secret: Arc<Zeroizing<Vec<u8>>>,
}

impl DurableRateLimiter {
    pub fn new(
        pre_auth_limiter: PgTokenBucketLimiter,
        execution_limiter: PgTokenBucketLimiter,
        namespace_secret: Vec<u8>,
    ) -> Result<Self, RateLimitError> {
        if namespace_secret.len() < 32 {
            return Err(RateLimitError::InvalidConfiguration);
        }
        Ok(Self {
            pre_auth_limiter,
            execution_limiter,
            namespace_secret: Arc::new(Zeroizing::new(namespace_secret)),
        })
    }

    fn limiter(&self, dimension: RateDimension) -> &PgTokenBucketLimiter {
        match dimension {
            // IP admission remains deliberately conservative. It protects
            // anonymous authentication and bootstrap traffic before a
            // project or actor identity is available.
            RateDimension::Ip
            | RateDimension::AuthProject
            | RateDimension::AuthUser
            | RateDimension::AuthApiKey => &self.pre_auth_limiter,
            // Authenticated execution is independently bounded, but must not
            // inherit the low anonymous refill rate. Otherwise every project,
            // user, and API key is hard-capped at two requests per second.
            RateDimension::Project | RateDimension::User | RateDimension::ApiKey => {
                &self.execution_limiter
            }
        }
    }
}

/// Run bounded, index-backed cleanup for durable request-admission state. The
/// handle is owned by the binary and aborted during graceful shutdown.
pub fn spawn_security_state_maintenance(
    pool: PgPool,
    rate_limiter: PgTokenBucketLimiter,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let now = now_ms();
            if rate_limiter.cleanup_expired(now, 10_000).await.is_err() {
                tracing::error!("rate-limit state cleanup failed");
            }
            if idempotency::purge_expired(&pool, 10_000).await.is_err() {
                tracing::error!("idempotency state cleanup failed");
            }
        }
    })
}

#[async_trait]
impl ApiRateLimiter for DurableRateLimiter {
    async fn check(
        &self,
        dimension: RateDimension,
        identifier: &[u8],
        cost: u32,
        now_ms: i64,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let key = RateLimitKey::derive(dimension, self.namespace_secret.as_slice(), identifier)?;
        self.limiter(dimension).check(key, cost, now_ms).await
    }

    async fn check_many(
        &self,
        checks: Vec<(RateDimension, Vec<u8>, u32)>,
        now_ms: i64,
    ) -> Result<Vec<RateLimitDecision>, RateLimitError> {
        let derived = checks
            .into_iter()
            .map(|(dimension, identifier, cost)| {
                RateLimitKey::derive(dimension, self.namespace_secret.as_slice(), &identifier)
                    .map(|key| (key, cost))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if derived.is_empty() || derived.len() > 16 {
            return Err(RateLimitError::InvalidCost);
        }

        // Reject duplicate keys across the whole request, including a
        // pathological mixed-policy ordering. PgTokenBucketLimiter performs
        // the same check within one batch; doing it here preserves that
        // invariant when a mixed list is split into consecutive policy runs.
        let mut unique = std::collections::HashSet::with_capacity(derived.len());
        if derived.iter().any(|(key, _)| !unique.insert(*key)) {
            return Err(RateLimitError::InvalidCost);
        }

        let mut decisions = Vec::with_capacity(derived.len());
        let mut start = 0;
        while start < derived.len() {
            let uses_pre_auth = is_pre_auth_dimension(derived[start].0.dimension);
            let end = derived[start..]
                .iter()
                .position(|(key, _)| is_pre_auth_dimension(key.dimension) != uses_pre_auth)
                .map_or(derived.len(), |offset| start + offset);
            let limiter = if uses_pre_auth {
                &self.pre_auth_limiter
            } else {
                &self.execution_limiter
            };
            let batch = limiter.check_many(&derived[start..end], now_ms).await?;
            let denied = batch
                .last()
                .is_some_and(|decision| matches!(decision, RateLimitDecision::Denied { .. }));
            decisions.extend(batch);
            if denied {
                break;
            }
            start = end;
        }
        Ok(decisions)
    }
}

fn is_pre_auth_dimension(dimension: RateDimension) -> bool {
    matches!(
        dimension,
        RateDimension::Ip
            | RateDimension::AuthProject
            | RateDimension::AuthUser
            | RateDimension::AuthApiKey
    )
}

#[async_trait]
pub trait CredentialVerifier: Send + Sync {
    async fn verify_query_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<ExecutionMode, CredentialError>;

    async fn verify_developer_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        required_scope: DeveloperScope,
    ) -> Result<DeveloperPrincipal, CredentialError>;

    async fn verify_end_user_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<AuthContext, CredentialError>;

    async fn verify_end_user_session_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<(AuthContext, Option<SessionId>), CredentialError> {
        self.verify_end_user_credential(project_id, bearer_token)
            .await
            .map(|context| (context, None))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialError {
    Missing,
    Invalid,
    Expired,
    WrongProject,
    InsufficientScope,
    Disabled,
    Unavailable,
}

#[async_trait]
impl CredentialVerifier for ffdb_auth::PgCredentialVerifier {
    async fn verify_query_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<ExecutionMode, CredentialError> {
        ffdb_auth::PgCredentialVerifier::verify_query_credential(self, project_id, bearer_token)
            .await
            .map_err(map_auth_credential_error)
    }

    async fn verify_developer_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
        required_scope: DeveloperScope,
    ) -> Result<DeveloperPrincipal, CredentialError> {
        ffdb_auth::PgCredentialVerifier::verify_developer_credential(
            self,
            project_id,
            bearer_token,
            required_scope,
        )
        .await
        .map_err(map_auth_credential_error)
    }

    async fn verify_end_user_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<AuthContext, CredentialError> {
        ffdb_auth::PgCredentialVerifier::verify_end_user_credential(self, project_id, bearer_token)
            .await
            .map_err(map_auth_credential_error)
    }

    async fn verify_end_user_session_credential(
        &self,
        project_id: ProjectId,
        bearer_token: &str,
    ) -> Result<(AuthContext, Option<SessionId>), CredentialError> {
        ffdb_auth::PgCredentialVerifier::verify_end_user_session_credential(
            self,
            project_id,
            bearer_token,
        )
        .await
        .map_err(map_auth_credential_error)
    }
}

fn map_auth_credential_error(error: ffdb_auth::CredentialVerificationError) -> CredentialError {
    match error {
        ffdb_auth::CredentialVerificationError::Invalid => CredentialError::Invalid,
        ffdb_auth::CredentialVerificationError::Expired => CredentialError::Expired,
        ffdb_auth::CredentialVerificationError::WrongProject => CredentialError::WrongProject,
        ffdb_auth::CredentialVerificationError::InsufficientScope => {
            CredentialError::InsufficientScope
        }
        ffdb_auth::CredentialVerificationError::Disabled => CredentialError::Disabled,
        ffdb_auth::CredentialVerificationError::Unavailable => CredentialError::Unavailable,
    }
}

pub fn router(state: ApiState) -> Router {
    let instance_origins = Arc::new(state.cors_allowed_origins.to_vec());
    let project_auth = state.project_auth.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::async_predicate(
            move |origin: HeaderValue, parts| {
                let instance_origins = instance_origins.clone();
                let project_auth = project_auth.clone();
                let path = parts.uri.path().to_owned();
                async move {
                    let Ok(origin) = origin.to_str() else {
                        return false;
                    };
                    if instance_origins.iter().any(|value| value == origin) {
                        return true;
                    }
                    let Some(project_id) = project_id_from_request_path(&path) else {
                        return false;
                    };
                    let Some(project_auth) = project_auth else {
                        return false;
                    };
                    project_auth.allows_web_origin(project_id, origin).await
                }
            },
        ))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderName::from_static("idempotency-key"),
        ])
        .expose_headers([axum::http::HeaderName::from_static("x-request-id")]);
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .route("/openapi.json", get(openapi))
        .route("/v1/instance/setup/status", get(instance::public_status))
        .route(
            "/v1/instance/observability",
            get(observability::instance_summary),
        )
        .route("/v1/instance/updates", get(host_updates::status))
        .route("/v1/instance/updates/check", post(host_updates::check))
        .route("/v1/instance/updates/install", post(host_updates::install))
        .route(
            "/v1/instance/updates/rollback",
            post(host_updates::rollback),
        )
        .route("/v1/instance/updates/jobs/{job_id}", get(host_updates::job))
        .route(
            "/v1/instance/updates/settings",
            get(host_updates::settings).patch(host_updates::configure),
        )
        .route(
            "/v1/instance",
            get(instance::status).post(instance::complete_setup),
        )
        .route(
            "/v1/instance/organization-creation-policy",
            axum::routing::patch(instance::update_policy),
        )
        .route(
            "/v1/instance/billing/connect/onboarding",
            post(instance::connect_onboarding),
        )
        .route(
            "/v1/instance/billing/refresh",
            post(instance::refresh_billing_account),
        )
        .route(
            "/v1/instance/administrators",
            get(instance::administrators).post(instance::grant_administrator),
        )
        .route(
            "/v1/instance/administrators/{user_id}",
            axum::routing::delete(instance::revoke_administrator),
        )
        .route("/v1/instance/organizations", get(instance::organizations))
        .route(
            "/v1/instance/organizations/{organization_id}",
            axum::routing::patch(instance::set_organization_disabled),
        )
        .route("/v1/instance/users", get(instance::users))
        .route(
            "/v1/instance/users/{user_id}",
            axum::routing::patch(instance::set_user_disabled),
        )
        .route(
            "/v1/instance/billing-exemptions",
            get(instance::billing_exemptions),
        )
        .route(
            "/v1/instance/billing-exemptions/{organization_id}",
            axum::routing::put(instance::grant_billing_exemption)
                .delete(instance::revoke_billing_exemption),
        )
        .route("/v1/instance/plans", get(instance::plans))
        .route(
            "/v1/instance/plans/{tier}",
            axum::routing::put(instance::put_plan).delete(instance::retire_plan),
        )
        .route("/v1/developer/bootstrap", post(management::bootstrap))
        .route("/v1/developer/sign-in", post(management::sign_in))
        .route("/v1/developer/refresh", post(management::refresh))
        .route("/v1/developer/sign-out", post(management::sign_out))
        .route(
            "/v1/developer/invitations/accept",
            post(management::accept_invitation),
        )
        .route(
            "/v1/organizations",
            get(management::organizations).post(management::create_organization),
        )
        .route(
            "/v1/organizations/{organization_id}/projects",
            get(management::projects),
        )
        .route(
            "/v1/organizations/{organization_id}/members",
            get(management::members).post(management::add_member),
        )
        .route(
            "/v1/organizations/{organization_id}/members/{user_id}",
            axum::routing::patch(management::update_member).delete(management::remove_member),
        )
        .route(
            "/v1/organizations/{organization_id}/invitations",
            post(management::create_invitation),
        )
        .route(
            "/v1/organizations/{organization_id}/billing",
            get(billing::status),
        )
        .route(
            "/v1/organizations/{organization_id}/billing/checkout",
            post(billing::checkout),
        )
        .route(
            "/v1/organizations/{organization_id}/billing/portal",
            post(billing::portal),
        )
        .route(
            "/v1/organizations/{organization_id}/billing/invoices",
            get(billing::invoices),
        )
        .route(
            "/v1/organizations/{organization_id}/billing/usage",
            get(billing::usage),
        )
        .route("/v1/billing/webhooks/stripe", post(billing::stripe_webhook))
        .route("/v1/projects", post(management::create_project))
        .route(
            "/v1/projects/{project_id}/payments",
            get(billing::project_payments),
        )
        .route(
            "/v1/projects/{project_id}/commerce/account",
            get(commerce::account).delete(commerce::disconnect_account),
        )
        .route(
            "/v1/projects/{project_id}/commerce/account/byo",
            post(commerce::configure_byo),
        )
        .route(
            "/v1/projects/{project_id}/commerce/account/connect/onboarding",
            post(commerce::connect_onboarding),
        )
        .route(
            "/v1/projects/{project_id}/commerce/account/refresh",
            post(commerce::refresh_account),
        )
        .route(
            "/v1/projects/{project_id}/commerce/products",
            get(commerce::products).post(commerce::create_product),
        )
        .route(
            "/v1/projects/{project_id}/commerce/products/{product_id}",
            axum::routing::delete(commerce::archive_product),
        )
        .route(
            "/v1/projects/{project_id}/commerce/prices",
            get(commerce::prices).post(commerce::create_price),
        )
        .route(
            "/v1/projects/{project_id}/commerce/prices/{price_id}",
            axum::routing::delete(commerce::retire_price),
        )
        .route(
            "/v1/projects/{project_id}/commerce/checkouts/one-time",
            post(commerce::one_time_checkout),
        )
        .route(
            "/v1/projects/{project_id}/commerce/checkouts/recurring",
            post(commerce::recurring_checkout),
        )
        .route(
            "/v1/projects/{project_id}/commerce/customer-portal",
            post(commerce::customer_portal),
        )
        .route(
            "/v1/projects/{project_id}/commerce/orders",
            get(commerce::orders),
        )
        .route(
            "/v1/projects/{project_id}/commerce/orders/{order_id}",
            get(commerce::order),
        )
        .route(
            "/v1/projects/{project_id}/commerce/orders/{order_id}/fulfillment",
            axum::routing::patch(commerce::fulfillment),
        )
        .route(
            "/v1/projects/{project_id}/commerce/payments",
            get(commerce::payments),
        )
        .route(
            "/v1/projects/{project_id}/commerce/refunds",
            post(commerce::refunds),
        )
        .route(
            "/v1/projects/{project_id}/commerce/subscriptions",
            get(commerce::subscriptions),
        )
        .route(
            "/v1/projects/{project_id}/commerce/subscriptions/{subscription_id}/cancel",
            post(commerce::cancel_subscription),
        )
        .route(
            "/v1/projects/{project_id}/commerce/entitlements",
            get(commerce::entitlements),
        )
        .route(
            "/v1/projects/{project_id}/commerce/webhooks/stripe",
            post(commerce::stripe_webhook),
        )
        .route(
            "/v1/commerce/webhooks/stripe-connect",
            post(commerce::stripe_connect_webhook),
        )
        .route(
            "/v1/projects/{project_id}/auth/register",
            post(auth::register),
        )
        .route(
            "/v1/projects/{project_id}/auth/verify",
            post(auth::verify_email),
        )
        .route(
            "/v1/projects/{project_id}/auth/sign-in",
            post(auth::sign_in),
        )
        .route(
            "/v1/projects/{project_id}/auth/refresh",
            post(auth::refresh),
        )
        .route(
            "/v1/projects/{project_id}/auth/sign-out",
            post(auth::sign_out),
        )
        .route(
            "/v1/projects/{project_id}/auth/password/reset",
            post(auth::password_reset_start),
        )
        .route(
            "/v1/projects/{project_id}/auth/password/reset/complete",
            post(auth::password_reset_complete),
        )
        .route(
            "/v1/projects/{project_id}/auth/password/change",
            post(auth::change_password),
        )
        .route(
            "/v1/projects/{project_id}/auth/sessions",
            get(auth::sessions),
        )
        .route(
            "/v1/projects/{project_id}/auth/sessions/{session_id}",
            axum::routing::delete(auth::revoke_session),
        )
        .route(
            "/v1/projects/{project_id}/auth/settings",
            get(auth::get_settings).patch(auth::update_settings),
        )
        .route(
            "/v1/projects/{project_id}/auth/users",
            get(auth::admin_users),
        )
        .route(
            "/v1/projects/{project_id}/auth/users/{user_id}",
            axum::routing::patch(auth::set_user_disabled),
        )
        .route("/v1/projects/{project_id}/query", post(query))
        .route("/v1/projects/{project_id}/transaction", post(transaction))
        .route(
            "/v1/projects/{project_id}/migrations",
            get(operations::migration_history).post(apply_migration),
        )
        .route("/v1/projects/{project_id}/seed", post(operations::seed))
        .route(
            "/v1/projects/{project_id}/migrations/{migration_id}/rollback",
            post(rollback_migration),
        )
        .route("/v1/projects/{project_id}/schema", get(schema))
        .route("/v1/projects/{project_id}/policies", get(policies))
        .route("/v1/projects/{project_id}/sync", get(sync_pull))
        .route("/v1/projects/{project_id}/sync/push", post(sync_push))
        .route("/v1/projects/{project_id}/snapshot", get(snapshot))
        .route(
            "/v1/projects/{project_id}/backups",
            get(operations::backups).post(create_backup),
        )
        .route(
            "/v1/projects/{project_id}/backups/{backup_id}/restore",
            post(restore_backup),
        )
        .route("/v1/projects/{project_id}/logs", get(operations::logs))
        .route(
            "/v1/projects/{project_id}/observability",
            get(observability::project_summary),
        )
        .route(
            "/v1/projects/{project_id}/storage/buckets",
            get(storage::buckets).post(storage::create_bucket),
        )
        .route(
            "/v1/projects/{project_id}/storage/sign",
            post(storage::sign),
        )
        .route(
            "/v1/projects/{project_id}/storage/commit",
            post(storage::commit),
        )
        .route(
            "/v1/projects/{project_id}/storage/release",
            post(storage::release),
        )
        .route(
            "/v1/projects/{project_id}/storage/objects",
            get(storage::objects),
        )
        .route(
            "/v1/projects/{project_id}/storage/cleanup",
            post(storage::cleanup),
        )
        .route(
            "/v1/projects/{project_id}/storage/multipart/commit",
            post(storage::multipart_commit),
        )
        .route(
            "/v1/projects/{project_id}/storage/multipart/authorize",
            post(storage::multipart_authorize),
        )
        .route(
            "/v1/projects/{project_id}/storage/multipart/create",
            post(storage::multipart_create),
        )
        .route(
            "/v1/projects/{project_id}/email/templates",
            get(email::templates),
        )
        .route(
            "/v1/projects/{project_id}/email/templates/artifacts",
            post(email::import_artifact),
        )
        .route(
            "/v1/projects/{project_id}/email/templates/{kind}/{version}/publish",
            post(email::publish),
        )
        .route(
            "/v1/projects/{project_id}/email/templates/{kind}/{version}/preview",
            post(email::preview),
        )
        .route(
            "/v1/projects/{project_id}/api-keys",
            get(management::api_keys).post(management::create_api_key),
        )
        .route(
            "/v1/projects/{project_id}/api-keys/{api_key_id}/revoke",
            post(management::revoke_api_key),
        )
        .route(
            "/v1/projects/{project_id}/keys/rotate",
            post(management::rotate_signing_key),
        )
        .route(
            "/v1/projects/{project_id}/integrity-check",
            post(integrity_check),
        )
        .layer(Extension(state.instance.clone()))
        .with_state(state.clone())
        .layer(RequestBodyLimitLayer::new(512 * 1024))
        .layer(cors)
        .layer(SetSensitiveRequestHeadersLayer::new([
            AUTHORIZATION,
            axum::http::HeaderName::from_static("stripe-signature"),
        ]))
        .layer(CatchPanicLayer::new())
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                tracing::info_span!(
                    "http.request",
                    method = stable_method(request.method()),
                    route = stable_route(request.uri().path())
                )
            }),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            pre_auth_rate_limit,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_context,
        ))
        .layer(middleware::from_fn_with_state(state, observe_metrics))
}

fn project_id_from_request_path(path: &str) -> Option<ProjectId> {
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let ["v1", "projects", project_id, ..] = segments.as_slice() else {
        return None;
    };
    Uuid::parse_str(project_id).ok().map(ProjectId)
}

async fn pre_auth_rate_limit(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !requires_pre_auth_rate_limit(request.method(), path) {
        return next.run(request).await;
    }
    let Some(limiter) = &state.rate_limiter else {
        return next.run(request).await;
    };
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .copied()
        .unwrap_or_default();
    let source = request.extensions().get::<TrustedClientIp>().map_or_else(
        || b"transport-identity-unavailable".to_vec(),
        |client| client.0.to_string().into_bytes(),
    );
    match limiter.check(RateDimension::Ip, &source, 1, now_ms()).await {
        Ok(RateLimitDecision::Allowed { .. }) => next.run(request).await,
        Ok(RateLimitDecision::Denied { retry_after_ms }) => {
            rate_limited(request_id, retry_after_ms)
        }
        Err(_) => rate_limit_unavailable(request_id).into_response(),
    }
}

fn requires_pre_auth_rate_limit(method: &Method, path: &str) -> bool {
    if method != Method::POST {
        return false;
    }
    if matches!(
        path,
        "/v1/instance"
            | "/v1/developer/bootstrap"
            | "/v1/developer/sign-in"
            | "/v1/developer/refresh"
            | "/v1/developer/invitations/accept"
    ) {
        return true;
    }

    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    matches!(
        segments.as_slice(),
        ["v1", "projects", project_id, "auth", "register" | "verify" | "sign-in" | "refresh"]
            if !project_id.is_empty()
    ) || matches!(
        segments.as_slice(),
        ["v1", "projects", project_id, "auth", "password", "reset"]
            | ["v1", "projects", project_id, "auth", "password", "reset", "complete"]
            if !project_id.is_empty()
    )
}

async fn observe_metrics(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let method = stable_method(request.method());
    let route = stable_route(request.uri().path());
    let project_id = observability::project_from_path(request.uri().path());
    let started = Instant::now();
    if let Some(metrics) = &state.metrics {
        metrics.request_started();
    }
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    if let Some(metrics) = &state.metrics {
        metrics.request_finished();
        let _ignored = metrics.observe_request(
            method,
            route,
            response.status().as_u16(),
            elapsed.as_secs_f64(),
        );
    }
    if let Some(observability) = &state.observability {
        observability.record_http(
            project_id,
            method,
            route,
            response.status().as_u16(),
            elapsed,
            now_ms(),
        );
    }
    response
}

fn stable_method(method: &axum::http::Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        _ => "OTHER",
    }
}

fn stable_route(path: &str) -> &'static str {
    if matches!(path, "/healthz" | "/readyz" | "/metrics" | "/openapi.json") {
        return match path {
            "/healthz" => "/healthz",
            "/readyz" => "/readyz",
            "/openapi.json" => "/openapi.json",
            _ => "/metrics",
        };
    }
    let segments: Vec<_> = path.trim_matches('/').split('/').collect();
    match segments.as_slice() {
        ["v1", "instance"] => "/v1/instance",
        ["v1", "instance", "observability"] => "/v1/instance/observability",
        ["v1", "instance", "updates"] => "/v1/instance/updates",
        ["v1", "instance", "updates", "check"] => "/v1/instance/updates/check",
        ["v1", "instance", "updates", "install"] => "/v1/instance/updates/install",
        ["v1", "instance", "updates", "rollback"] => "/v1/instance/updates/rollback",
        ["v1", "instance", "updates", "settings"] => "/v1/instance/updates/settings",
        ["v1", "instance", "updates", "jobs", _] => "/v1/instance/updates/jobs/:job",
        ["v1", "instance", "setup", "status"] => "/v1/instance/setup/status",
        ["v1", "instance", "organizations"] => "/v1/instance/organizations",
        ["v1", "instance", "organizations", _] => "/v1/instance/organizations/:organization",
        ["v1", "instance", "users"] => "/v1/instance/users",
        ["v1", "instance", "users", _] => "/v1/instance/users/:user",
        ["v1", "instance", "plans"] => "/v1/instance/plans",
        ["v1", "instance", "plans", _] => "/v1/instance/plans/:tier",
        ["v1", "projects", _, "query"] => "/v1/projects/:id/query",
        ["v1", "projects", _, "transaction"] => "/v1/projects/:id/transaction",
        ["v1", "projects", _, "migrations"] => "/v1/projects/:id/migrations",
        ["v1", "projects", _, "seed"] => "/v1/projects/:id/seed",
        ["v1", "projects", _, "migrations", _, "rollback"] => {
            "/v1/projects/:id/migrations/:migration/rollback"
        }
        ["v1", "projects", _, "schema"] => "/v1/projects/:id/schema",
        ["v1", "projects", _, "policies"] => "/v1/projects/:id/policies",
        ["v1", "projects", _, "sync"] => "/v1/projects/:id/sync",
        ["v1", "projects", _, "sync", "push"] => "/v1/projects/:id/sync/push",
        ["v1", "projects", _, "snapshot"] => "/v1/projects/:id/snapshot",
        ["v1", "projects", _, "backups"] => "/v1/projects/:id/backups",
        ["v1", "projects", _, "backups", _, "restore"] => {
            "/v1/projects/:id/backups/:backup/restore"
        }
        ["v1", "projects", _, "logs"] => "/v1/projects/:id/logs",
        ["v1", "projects", _, "observability"] => "/v1/projects/:id/observability",
        ["v1", "projects", _, "storage", "buckets"] => "/v1/projects/:id/storage/buckets",
        ["v1", "projects", _, "storage", "sign"] => "/v1/projects/:id/storage/sign",
        ["v1", "projects", _, "storage", "commit"] => "/v1/projects/:id/storage/commit",
        ["v1", "projects", _, "storage", "release"] => "/v1/projects/:id/storage/release",
        ["v1", "projects", _, "storage", "objects"] => "/v1/projects/:id/storage/objects",
        ["v1", "projects", _, "storage", "cleanup"] => "/v1/projects/:id/storage/cleanup",
        ["v1", "projects", _, "storage", "multipart", "authorize"] => {
            "/v1/projects/:id/storage/multipart/authorize"
        }
        ["v1", "projects", _, "storage", "multipart", "create"] => {
            "/v1/projects/:id/storage/multipart/create"
        }
        ["v1", "projects", _, "storage", "multipart", "commit"] => {
            "/v1/projects/:id/storage/multipart/commit"
        }
        ["v1", "projects", _, "email", "templates"] => "/v1/projects/:id/email/templates",
        ["v1", "projects", _, "email", "templates", "artifacts"] => {
            "/v1/projects/:id/email/templates/artifacts"
        }
        ["v1", "projects", _, "email", "templates", _, _, "publish"] => {
            "/v1/projects/:id/email/templates/:kind/:version/publish"
        }
        ["v1", "projects", _, "email", "templates", _, _, "preview"] => {
            "/v1/projects/:id/email/templates/:kind/:version/preview"
        }
        ["v1", "projects", _, "integrity-check"] => "/v1/projects/:id/integrity-check",
        ["v1", "developer", "bootstrap"] => "/v1/developer/bootstrap",
        ["v1", "developer", "sign-in"] => "/v1/developer/sign-in",
        ["v1", "developer", "refresh"] => "/v1/developer/refresh",
        ["v1", "developer", "sign-out"] => "/v1/developer/sign-out",
        ["v1", "organizations"] => "/v1/organizations",
        ["v1", "organizations", _, "projects"] => "/v1/organizations/:id/projects",
        ["v1", "organizations", _, "members"] => "/v1/organizations/:id/members",
        ["v1", "organizations", _, "members", _] => "/v1/organizations/:id/members/:user",
        ["v1", "projects"] => "/v1/projects",
        ["v1", "projects", _, "auth", "register"] => "/v1/projects/:id/auth/register",
        ["v1", "projects", _, "auth", "verify"] => "/v1/projects/:id/auth/verify",
        ["v1", "projects", _, "auth", "sign-in"] => "/v1/projects/:id/auth/sign-in",
        ["v1", "projects", _, "auth", "refresh"] => "/v1/projects/:id/auth/refresh",
        ["v1", "projects", _, "auth", "sign-out"] => "/v1/projects/:id/auth/sign-out",
        ["v1", "projects", _, "auth", "sessions"] => "/v1/projects/:id/auth/sessions",
        ["v1", "projects", _, "auth", "sessions", _] => "/v1/projects/:id/auth/sessions/:session",
        ["v1", "projects", _, "auth", "settings"] => "/v1/projects/:id/auth/settings",
        ["v1", "projects", _, "auth", "users"] => "/v1/projects/:id/auth/users",
        ["v1", "projects", _, "auth", "users", _] => "/v1/projects/:id/auth/users/:user",
        ["v1", "developer", "invitations", "accept"] => "/v1/developer/invitations/accept",
        ["v1", "organizations", _, "invitations"] => "/v1/organizations/:id/invitations",
        ["v1", "organizations", _, "billing"] => "/v1/organizations/:id/billing",
        ["v1", "organizations", _, "billing", "checkout"] => {
            "/v1/organizations/:id/billing/checkout"
        }
        ["v1", "organizations", _, "billing", "portal"] => "/v1/organizations/:id/billing/portal",
        ["v1", "organizations", _, "billing", "invoices"] => {
            "/v1/organizations/:id/billing/invoices"
        }
        ["v1", "organizations", _, "billing", "usage"] => "/v1/organizations/:id/billing/usage",
        ["v1", "billing", "webhooks", "stripe"] => "/v1/billing/webhooks/stripe",
        ["v1", "projects", _, "payments"] => "/v1/projects/:id/payments",
        ["v1", "projects", _, "commerce", "account"] => "/v1/projects/:id/commerce/account",
        ["v1", "projects", _, "commerce", "account", "byo"] => {
            "/v1/projects/:id/commerce/account/byo"
        }
        [
            "v1",
            "projects",
            _,
            "commerce",
            "account",
            "connect",
            "onboarding",
        ] => "/v1/projects/:id/commerce/account/connect/onboarding",
        ["v1", "projects", _, "commerce", "account", "refresh"] => {
            "/v1/projects/:id/commerce/account/refresh"
        }
        ["v1", "projects", _, "commerce", "products"] => "/v1/projects/:id/commerce/products",
        ["v1", "projects", _, "commerce", "products", _] => {
            "/v1/projects/:id/commerce/products/:product"
        }
        ["v1", "projects", _, "commerce", "prices"] => "/v1/projects/:id/commerce/prices",
        ["v1", "projects", _, "commerce", "prices", _] => "/v1/projects/:id/commerce/prices/:price",
        ["v1", "projects", _, "commerce", "checkouts", "one-time"] => {
            "/v1/projects/:id/commerce/checkouts/one-time"
        }
        ["v1", "projects", _, "commerce", "checkouts", "recurring"] => {
            "/v1/projects/:id/commerce/checkouts/recurring"
        }
        ["v1", "projects", _, "commerce", "customer-portal"] => {
            "/v1/projects/:id/commerce/customer-portal"
        }
        ["v1", "projects", _, "commerce", "orders"] => "/v1/projects/:id/commerce/orders",
        ["v1", "projects", _, "commerce", "orders", _, "fulfillment"] => {
            "/v1/projects/:id/commerce/orders/:order/fulfillment"
        }
        ["v1", "projects", _, "commerce", "orders", _] => "/v1/projects/:id/commerce/orders/:order",
        ["v1", "projects", _, "commerce", "payments"] => "/v1/projects/:id/commerce/payments",
        ["v1", "projects", _, "commerce", "refunds"] => "/v1/projects/:id/commerce/refunds",
        ["v1", "projects", _, "commerce", "subscriptions"] => {
            "/v1/projects/:id/commerce/subscriptions"
        }
        [
            "v1",
            "projects",
            _,
            "commerce",
            "subscriptions",
            _,
            "cancel",
        ] => "/v1/projects/:id/commerce/subscriptions/:subscription/cancel",
        ["v1", "projects", _, "commerce", "entitlements"] => {
            "/v1/projects/:id/commerce/entitlements"
        }
        ["v1", "projects", _, "commerce", "webhooks", "stripe"] => {
            "/v1/projects/:id/commerce/webhooks/stripe"
        }
        ["v1", "commerce", "webhooks", "stripe-connect"] => "/v1/commerce/webhooks/stripe-connect",
        ["v1", "projects", _, "auth", "password", "reset"] => {
            "/v1/projects/:id/auth/password/reset"
        }
        ["v1", "projects", _, "auth", "password", "reset", "complete"] => {
            "/v1/projects/:id/auth/password/reset/complete"
        }
        ["v1", "projects", _, "auth", "password", "change"] => {
            "/v1/projects/:id/auth/password/change"
        }
        ["v1", "projects", _, "api-keys"] => "/v1/projects/:id/api-keys",
        ["v1", "projects", _, "api-keys", _, "revoke"] => "/v1/projects/:id/api-keys/:key/revoke",
        ["v1", "projects", _, "keys", "rotate"] => "/v1/projects/:id/keys/rotate",
        _ => "unmatched",
    }
}

async fn request_context(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = RequestId::new();
    let source_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect| {
            resolve_client_ip(
                connect.0.ip(),
                request.headers(),
                &state.trusted_proxy_cidrs,
            )
        });
    request.extensions_mut().insert(request_id);
    if let Some(source_ip) = source_ip {
        request.extensions_mut().insert(TrustedClientIp(source_ip));
    }
    let mut response = REQUEST_SOURCE_IP.scope(source_ip, next.run(request)).await;
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Resolve a client address without allowing an untrusted transport peer to
/// choose its rate-limit or audit identity. Trusted proxy hops are stripped
/// from the right of the X-Forwarded-For chain; the first untrusted hop is the
/// client. Invalid or excessively long chains fail closed to the transport IP.
fn resolve_client_ip(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpNet]) -> IpAddr {
    if !trusted_proxies
        .iter()
        .any(|network| network.contains(&peer))
    {
        return peer;
    }

    let mut chain = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        let Ok(value) = value.to_str() else {
            return peer;
        };
        if value.len() > 1_024 {
            return peer;
        }
        for candidate in value.split(',') {
            if chain.len() >= 16 {
                return peer;
            }
            let Ok(address) = candidate.trim().parse::<IpAddr>() else {
                return peer;
            };
            chain.push(address);
        }
    }
    if chain.is_empty() {
        return peer;
    }
    chain.push(peer);
    chain
        .iter()
        .rev()
        .find(|address| {
            !trusted_proxies
                .iter()
                .any(|network| network.contains(*address))
        })
        .copied()
        .unwrap_or(chain[0])
}

async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({"status":"ok","version":PROTOCOL_VERSION})),
    )
}

async fn ready(State(state): State<ApiState>) -> impl IntoResponse {
    if state.limits.validate().is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status":"not_ready","dependency":"limits"})),
        )
            .into_response();
    }
    if let Some(pool) = &state.readiness_pool
        && sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(pool)
            .await
            .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status":"not_ready","dependency":"control_plane"})),
        )
            .into_response();
    }
    (StatusCode::OK, Json(serde_json::json!({"status":"ready"}))).into_response()
}

async fn metrics(State(state): State<ApiState>) -> Response {
    let Some(metrics) = &state.metrics else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match metrics.encode_prometheus() {
        Ok(body) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn openapi() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json; charset=utf-8")],
        include_str!("../../../docs/API/openapi.json"),
    )
}

async fn query(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<QueryRequest>,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = payload.validate(&state.limits) {
        return validation_error(error.to_string(), request_id).into_response();
    }
    let token = match bearer(&headers) {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    let mode = match state
        .credentials
        .verify_query_credential(project_id, token)
        .await
    {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    let mutating = ffdb_sql_parser::classify_statement(&payload.sql)
        .map(|class| !class.read_only)
        .unwrap_or(true);
    let idempotency = mutating
        .then(|| {
            idempotency_input(
                &headers,
                "database.query.write",
                serde_json::to_value(&payload).unwrap_or(Value::Null),
            )
        })
        .flatten();
    dispatch(
        &state,
        project_id,
        request_id,
        mode,
        WorkerOperation::Query(payload),
        idempotency,
    )
    .await
}

async fn transaction(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<TransactionRequest>,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = payload.validate(&state.limits) {
        return validation_error(error.to_string(), request_id).into_response();
    }
    let token = match bearer(&headers) {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    let mode = match state
        .credentials
        .verify_query_credential(project_id, token)
        .await
    {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    let mutating = payload.statements.iter().any(|statement| {
        ffdb_sql_parser::classify_statement(&statement.sql)
            .map(|class| !class.read_only)
            .unwrap_or(true)
    });
    let idempotency = mutating
        .then(|| {
            idempotency_input(
                &headers,
                "database.transaction.write",
                serde_json::to_value(&payload).unwrap_or(Value::Null),
            )
        })
        .flatten();
    dispatch(
        &state,
        project_id,
        request_id,
        mode,
        WorkerOperation::Transaction(payload),
        idempotency,
    )
    .await
}

async fn apply_migration(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<MigrationSpec>,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = payload.validate() {
        return validation_error(error.to_string(), request_id).into_response();
    }
    let principal = match developer(
        &state,
        project_id,
        &headers,
        DeveloperScope::DatabaseMigrate,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    let idempotency = idempotency_input(
        &headers,
        "migration.apply",
        serde_json::to_value(&payload).unwrap_or(Value::Null),
    );
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::Developer(principal),
        WorkerOperation::ApplyMigration(payload),
        idempotency,
    )
    .await
}

async fn rollback_migration(
    State(state): State<ApiState>,
    Path((project, migration_id)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if migration_id.is_empty() || migration_id.len() > 128 {
        return validation_error("invalid migration identifier", request_id).into_response();
    }
    let principal = match developer(
        &state,
        project_id,
        &headers,
        DeveloperScope::DatabaseMigrate,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::Developer(principal),
        WorkerOperation::RollbackMigration {
            migration_id: migration_id.clone(),
        },
        idempotency_input(
            &headers,
            "migration.rollback",
            serde_json::json!({"migration_id": migration_id}),
        ),
    )
    .await
}

async fn schema(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    developer_dispatch(
        &state,
        &project,
        request_id,
        &headers,
        DeveloperScope::DatabaseSchema,
        WorkerOperation::Schema,
    )
    .await
}

async fn policies(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    developer_dispatch(
        &state,
        &project,
        request_id,
        &headers,
        DeveloperScope::DatabaseSchema,
        WorkerOperation::Policies,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct SyncQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

async fn sync_pull(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(parameters): Query<SyncQuery>,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let auth = match end_user(&state, project_id, &headers).await {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    let limit = parameters.limit.unwrap_or(1_000);
    if limit == 0 || limit > 1_000 {
        return validation_error("sync limit must be between 1 and 1000", request_id)
            .into_response();
    }
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::EndUser(auth),
        WorkerOperation::SyncPull(SyncPullRequest {
            cursor: parameters.cursor,
            limit,
        }),
        None,
    )
    .await
}

async fn sync_push(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<SyncPushRequest>,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if payload.mutations.is_empty() || payload.mutations.len() > 100 {
        return validation_error("sync push must contain 1 to 100 mutations", request_id)
            .into_response();
    }
    let auth = match end_user(&state, project_id, &headers).await {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::EndUser(auth),
        WorkerOperation::SyncPush(payload),
        None,
    )
    .await
}

async fn snapshot(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let tables = match snapshot_tables(raw_query.as_deref()) {
        Ok(value) => value,
        Err(message) => return validation_error(message, request_id).into_response(),
    };
    let auth = match end_user(&state, project_id, &headers).await {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::EndUser(auth),
        WorkerOperation::Snapshot(SnapshotRequest { tables }),
        None,
    )
    .await
}

fn snapshot_tables(raw_query: Option<&str>) -> Result<Option<Vec<String>>, &'static str> {
    let Some(raw_query) = raw_query.filter(|query| !query.is_empty()) else {
        return Ok(None);
    };
    let mut tables = Vec::new();
    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if name != "table" || value.is_empty() || value.len() > 128 || tables.len() >= 256 {
            return Err("snapshot table filters are invalid");
        }
        tables.push(value.into_owned());
    }
    if tables.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tables))
    }
}

async fn create_backup(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let principal =
        match developer(&state, project_id, &headers, DeveloperScope::BackupsManage).await {
            Ok(value) => value,
            Err(error) => return credential_error(error, request_id).into_response(),
        };
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::Developer(principal),
        WorkerOperation::Backup {
            backup_id: BackupId::new(),
        },
        idempotency_input(&headers, "backup.create", serde_json::json!({})),
    )
    .await
}

async fn restore_backup(
    State(state): State<ApiState>,
    Path((project, backup)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let project_id = match parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let backup_id = match Uuid::parse_str(&backup) {
        Ok(value) => BackupId(value),
        Err(_) => {
            return ApiError::new(
                StatusCode::BAD_REQUEST,
                "backup.invalid_id",
                "invalid backup identifier",
                request_id,
            )
            .into_response();
        }
    };
    let principal =
        match developer(&state, project_id, &headers, DeveloperScope::BackupsManage).await {
            Ok(value) => value,
            Err(error) => return credential_error(error, request_id).into_response(),
        };
    let Some(pool) = &state.readiness_pool else {
        return control_plane_unavailable(request_id).into_response();
    };
    let available = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM backups WHERE id=$1 AND project_id=$2 \
         AND state IN ('ready','restoring'))",
    )
    .bind(backup_id.0)
    .bind(project_id.0)
    .fetch_one(pool)
    .await;
    match available {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::new(
                StatusCode::NOT_FOUND,
                "backup.not_found",
                "backup was not found",
                request_id,
            )
            .into_response();
        }
        Err(_) => return control_plane_unavailable(request_id).into_response(),
    }
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::Developer(principal),
        WorkerOperation::Restore { backup_id },
        idempotency_input(
            &headers,
            "backup.restore",
            serde_json::json!({"backup_id": backup_id}),
        ),
    )
    .await
}

async fn integrity_check(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    developer_dispatch(
        &state,
        &project,
        request_id,
        &headers,
        DeveloperScope::BackupsManage,
        WorkerOperation::IntegrityCheck,
    )
    .await
}

async fn developer_dispatch(
    state: &ApiState,
    project: &str,
    request_id: RequestId,
    headers: &HeaderMap,
    scope: DeveloperScope,
    operation: WorkerOperation,
) -> Response {
    let project_id = match parse_project(project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let principal = match developer(state, project_id, headers, scope).await {
        Ok(value) => value,
        Err(error) => return credential_error(error, request_id).into_response(),
    };
    dispatch(
        state,
        project_id,
        request_id,
        ExecutionMode::Developer(principal),
        operation,
        None,
    )
    .await
}

async fn developer(
    state: &ApiState,
    project_id: ProjectId,
    headers: &HeaderMap,
    scope: DeveloperScope,
) -> Result<DeveloperPrincipal, CredentialError> {
    state
        .credentials
        .verify_developer_credential(project_id, bearer(headers)?, scope)
        .await
}

async fn end_user(
    state: &ApiState,
    project_id: ProjectId,
    headers: &HeaderMap,
) -> Result<AuthContext, CredentialError> {
    state
        .credentials
        .verify_end_user_credential(project_id, bearer(headers)?)
        .await
}

async fn dispatch(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: ExecutionMode,
    mut operation: WorkerOperation,
    idempotency: Option<IdempotencyInput>,
) -> Response {
    if !execution_mode_matches_project(&mode, project_id) {
        return credential_error(CredentialError::WrongProject, request_id).into_response();
    }
    let (action, resource_kind, cost) = operation_descriptor(&operation);
    if let Err(response) =
        enforce_execution_rate_limit(state, project_id, request_id, &mode, cost).await
    {
        return response;
    }
    if append_audit(
        state,
        project_id,
        request_id,
        &mode,
        &format!("{action}.requested"),
        resource_kind,
        AuditOutcome::Success,
    )
    .await
    .is_err()
    {
        return audit_unavailable(request_id).into_response();
    }
    let claim = match idempotency {
        Some(input) => {
            let Some(pool) = &state.readiness_pool else {
                return control_plane_unavailable(request_id).into_response();
            };
            let actor_id = match &mode {
                ExecutionMode::Developer(principal) => principal.api_key_id.0,
                ExecutionMode::EndUser(context) => context.subject.0,
            };
            let request_hash = match idempotency::request_hash(&serde_json::json!({
                "actor_id": actor_id,
                "request": input.request,
            })) {
                Ok(value) => value,
                Err(error) => return idempotency_error(error, request_id),
            };
            match idempotency::admit(
                pool,
                idempotency::Scope::Project(project_id),
                input.operation,
                &input.key,
                request_hash,
            )
            .await
            {
                Ok(idempotency::Admission::Owner(claim)) => Some(claim),
                Ok(idempotency::Admission::Replay { status, body }) => {
                    append_audit_best_effort(
                        state,
                        project_id,
                        request_id,
                        &mode,
                        action,
                        resource_kind,
                        AuditOutcome::Success,
                    )
                    .await;
                    return (status, Json(body)).into_response();
                }
                Ok(idempotency::Admission::Conflict) => {
                    return ApiError::new(
                        StatusCode::CONFLICT,
                        "idempotency.request_conflict",
                        "the idempotency key was already used for a different request",
                        request_id,
                    )
                    .into_response();
                }
                Ok(idempotency::Admission::InProgress) => {
                    let mut response = ApiError::new(
                        StatusCode::CONFLICT,
                        "idempotency.in_progress",
                        "an operation with this idempotency key is still in progress",
                        request_id,
                    )
                    .into_response();
                    response.headers_mut().insert(
                        axum::http::header::RETRY_AFTER,
                        HeaderValue::from_static("1"),
                    );
                    return response;
                }
                Err(error) => return idempotency_error(error, request_id),
            }
        }
        None => None,
    };
    let lease_heartbeat = match (&state.readiness_pool, &claim) {
        (Some(pool), Some(claim)) => Some(idempotency::LeaseHeartbeat::start(
            pool.clone(),
            claim.clone(),
        )),
        _ => None,
    };
    if let (WorkerOperation::Backup { backup_id }, Some(claim)) = (&mut operation, &claim) {
        *backup_id = BackupId(idempotency::deterministic_uuid(claim));
    }
    let operation_receipt_id = claim.as_ref().map(idempotency::receipt_uuid);
    let route = match if matches!(operation, WorkerOperation::Restore { .. }) {
        resolve_restore_route(state, project_id).await
    } else {
        state.router.resolve(project_id).await
    } {
        Ok(value) => value,
        Err(error) => {
            append_audit_best_effort(
                state,
                project_id,
                request_id,
                &mode,
                action,
                resource_kind,
                AuditOutcome::Failure,
            )
            .await;
            return abandon_then(
                state,
                claim.as_ref(),
                routing_error(error, request_id).into_response(),
            )
            .await;
        }
    };
    if route.project_id != project_id {
        append_audit_best_effort(
            state,
            project_id,
            request_id,
            &mode,
            action,
            resource_kind,
            AuditOutcome::Failure,
        )
        .await;
        return abandon_then(
            state,
            claim.as_ref(),
            internal_error(request_id).into_response(),
        )
        .await;
    }
    let restore_id = match &operation {
        WorkerOperation::Restore { backup_id } => Some(*backup_id),
        _ => None,
    };
    let restore_receipt_id = match restore_id {
        Some(_) => match operation_receipt_id {
            Some(value) => Some(value),
            None => {
                return abandon_then(
                    state,
                    claim.as_ref(),
                    internal_error(request_id).into_response(),
                )
                .await;
            }
        },
        None => None,
    };
    if let (Some(backup_id), Some(receipt_id)) = (restore_id, restore_receipt_id)
        && begin_restore_lifecycle(state, project_id, backup_id, receipt_id)
            .await
            .is_err()
    {
        append_audit_best_effort(
            state,
            project_id,
            request_id,
            &mode,
            action,
            resource_kind,
            AuditOutcome::Failure,
        )
        .await;
        return abandon_then(
            state,
            claim.as_ref(),
            control_plane_unavailable(request_id).into_response(),
        )
        .await;
    }
    let usage_plan = match metering::UsagePlan::for_operation(&operation) {
        Ok(value) => value,
        Err(error) => {
            return abandon_then(
                state,
                claim.as_ref(),
                metering_error(error, request_id).into_response(),
            )
            .await;
        }
    };
    let prepared_usage = match (&state.usage_metering, usage_plan) {
        (Some(metering), Some(plan)) => match metering
            .prepare(
                project_id,
                operation_receipt_id.unwrap_or(request_id.0),
                &mode,
                plan,
                now_ms(),
                state.limits.max_database_bytes,
            )
            .await
        {
            Ok(value) => Some(value),
            Err(error) => {
                append_audit_best_effort(
                    state,
                    project_id,
                    request_id,
                    &mode,
                    action,
                    resource_kind,
                    AuditOutcome::Denied,
                )
                .await;
                return abandon_then(
                    state,
                    claim.as_ref(),
                    metering_error(error, request_id).into_response(),
                )
                .await;
            }
        },
        _ => None,
    };
    let mut worker_limits = state.limits.clone();
    if let Some(prepared) = &prepared_usage {
        worker_limits.max_database_bytes = prepared.max_database_bytes;
    }
    let query_profiles = observability::profiles_for_operation(&operation);
    let request = WorkerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        route: route.clone(),
        mode: mode.clone(),
        deadline_epoch_ms: now_ms().saturating_add(state.limits.transaction_timeout_ms as i64),
        limits: worker_limits,
        expected_schema_version: None,
        operation_receipt_id,
        operation,
    };
    let execution_started = Instant::now();
    match state.executor.execute(&route, request).await {
        Ok(execution) => {
            let execution_elapsed = execution_started.elapsed();
            if let Some(observability) = &state.observability {
                observability.record_execution(observability::ExecutionObservation {
                    project_id,
                    profiles: &query_profiles,
                    telemetry: &execution.statement_telemetry,
                    fallback_duration: execution_elapsed,
                    failed: false,
                    logical_database_bytes: Some(execution.usage.logical_database_bytes),
                    now_ms: now_ms(),
                });
            }
            if let (Some(metering), Some(prepared)) = (&state.usage_metering, &prepared_usage)
                && let Err(error) = metering.ingest(prepared, &execution.usage, now_ms())
            {
                append_audit_best_effort(
                    state,
                    project_id,
                    request_id,
                    &mode,
                    action,
                    resource_kind,
                    AuditOutcome::Failure,
                )
                .await;
                // The worker has already committed the data mutation and its
                // stable usage receipt. Keep the reservation so a retry can
                // safely replay and finish metering without double-counting.
                return abandon_then(
                    state,
                    claim.as_ref(),
                    metering_error(error, request_id).into_response(),
                )
                .await;
            }
            let response = execution.response;
            if let Some(backup_id) = restore_id
                && !restore_response_matches(backup_id, &response)
            {
                fail_restore_lifecycle(
                    state,
                    project_id,
                    backup_id,
                    RestoreFailureStage::WorkerResponseMismatch,
                )
                .await;
                append_audit_best_effort(
                    state,
                    project_id,
                    request_id,
                    &mode,
                    action,
                    resource_kind,
                    AuditOutcome::Failure,
                )
                .await;
                return abandon_then(
                    state,
                    claim.as_ref(),
                    internal_error(request_id).into_response(),
                )
                .await;
            }
            if persist_control_plane_result(
                state,
                project_id,
                &route,
                &mode,
                operation_receipt_id,
                &response,
            )
            .await
            .is_err()
            {
                append_audit_best_effort(
                    state,
                    project_id,
                    request_id,
                    &mode,
                    action,
                    resource_kind,
                    AuditOutcome::Failure,
                )
                .await;
                return abandon_then(
                    state,
                    claim.as_ref(),
                    control_plane_unavailable(request_id).into_response(),
                )
                .await;
            }
            if let Some(claim) = &claim {
                let Some(pool) = &state.readiness_pool else {
                    return control_plane_unavailable(request_id).into_response();
                };
                let body = match serde_json::to_value(&response) {
                    Ok(value) => value,
                    Err(_) => {
                        return abandon_then(
                            state,
                            Some(claim),
                            internal_error(request_id).into_response(),
                        )
                        .await;
                    }
                };
                if let Some(heartbeat) = &lease_heartbeat
                    && !idempotency::confirm_owner(pool, claim, heartbeat).await
                {
                    return control_plane_unavailable(request_id).into_response();
                }
                if idempotency::complete(pool, claim, StatusCode::OK, &body)
                    .await
                    .is_err()
                {
                    return control_plane_unavailable(request_id).into_response();
                }
            }
            append_audit_best_effort(
                state,
                project_id,
                request_id,
                &mode,
                action,
                resource_kind,
                AuditOutcome::Success,
            )
            .await;
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(error) => {
            if let Some(observability) = &state.observability {
                observability.record_execution(observability::ExecutionObservation {
                    project_id,
                    profiles: &query_profiles,
                    telemetry: &[],
                    fallback_duration: execution_started.elapsed(),
                    failed: true,
                    logical_database_bytes: None,
                    now_ms: now_ms(),
                });
            }
            if let (Some(metering), Some(prepared)) = (&state.usage_metering, &prepared_usage) {
                metering.release(prepared, now_ms());
            }
            let outcome = if matches!(error, ExecutionError::Rejected { .. }) {
                AuditOutcome::Denied
            } else {
                AuditOutcome::Failure
            };
            append_audit_best_effort(
                state,
                project_id,
                request_id,
                &mode,
                action,
                resource_kind,
                outcome,
            )
            .await;
            abandon_then(
                state,
                claim.as_ref(),
                execution_error(error, request_id).into_response(),
            )
            .await
        }
    }
}

#[derive(Debug)]
struct IdempotencyInput {
    operation: &'static str,
    key: String,
    request: Value,
}

fn idempotency_input(
    headers: &HeaderMap,
    operation: &'static str,
    request: Value,
) -> Option<IdempotencyInput> {
    Some(IdempotencyInput {
        operation,
        key: headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        request,
    })
}

async fn abandon_then(
    state: &ApiState,
    claim: Option<&idempotency::Claim>,
    response: Response,
) -> Response {
    if let (Some(pool), Some(claim)) = (&state.readiness_pool, claim)
        && idempotency::abandon(pool, claim).await.is_err()
    {
        tracing::error!("failed to abandon an incomplete idempotency claim");
    }
    response
}

fn idempotency_error(error: idempotency::Error, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        idempotency::Error::InvalidKey => (
            StatusCode::BAD_REQUEST,
            "idempotency.invalid_key",
            "the idempotency key is invalid",
        ),
        idempotency::Error::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "idempotency.invalid_request",
            "the idempotent request is invalid",
        ),
        idempotency::Error::InvalidStoredResponse
        | idempotency::Error::ResponseTooLarge
        | idempotency::Error::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "idempotency.unavailable",
            "idempotency service unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}

async fn persist_control_plane_result(
    state: &ApiState,
    project_id: ProjectId,
    route: &ffdb_protocol::DatabaseRoute,
    mode: &ExecutionMode,
    operation_receipt_id: Option<Uuid>,
    response: &WorkerResponse,
) -> Result<(), sqlx::Error> {
    let Some(pool) = &state.readiness_pool else {
        return Ok(());
    };
    match response {
        WorkerResponse::Migration(record) => {
            let checksum = decode_hex_checksum(&record.spec.checksum).ok_or(
                sqlx::Error::Decode("invalid worker migration checksum".into()),
            )?;
            let actor = match mode {
                ExecutionMode::Developer(principal) => principal.api_key_id.0,
                ExecutionMode::EndUser(_) => {
                    return Err(sqlx::Error::Protocol("invalid migration actor".into()));
                }
            };
            let mut transaction = pool.begin().await?;
            match record.status {
                ffdb_protocol::MigrationStatus::Applied => {
                    sqlx::query(
                        "INSERT INTO project_migrations \
                         (project_id,migration_id,name,checksum,up_sql,down_sql,schema_version,applied_by,applied_at,rolled_back_at) \
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,to_timestamp($9::double precision/1000),NULL) \
                         ON CONFLICT (project_id,migration_id) DO UPDATE SET name=EXCLUDED.name, \
                         checksum=EXCLUDED.checksum,up_sql=EXCLUDED.up_sql,down_sql=EXCLUDED.down_sql, \
                         schema_version=EXCLUDED.schema_version,applied_by=EXCLUDED.applied_by, \
                         applied_at=EXCLUDED.applied_at,rolled_back_at=NULL",
                    )
                    .bind(project_id.0)
                    .bind(&record.spec.id)
                    .bind(&record.spec.name)
                    .bind(checksum)
                    .bind(&record.spec.up_sql)
                    .bind(&record.spec.down_sql)
                    .bind(i64::try_from(record.schema_version_after).unwrap_or(i64::MAX))
                    .bind(actor)
                    .bind(record.applied_at_ms)
                    .execute(&mut *transaction)
                    .await?;
                }
                ffdb_protocol::MigrationStatus::RolledBack => {
                    sqlx::query(
                        "UPDATE project_migrations SET rolled_back_at=to_timestamp($3::double precision/1000), \
                         schema_version=$4,applied_by=$5 WHERE project_id=$1 AND migration_id=$2",
                    )
                    .bind(project_id.0)
                    .bind(&record.spec.id)
                    .bind(record.applied_at_ms)
                    .bind(i64::try_from(record.schema_version_after).unwrap_or(i64::MAX))
                    .bind(actor)
                    .execute(&mut *transaction)
                    .await?;
                }
                ffdb_protocol::MigrationStatus::Pending
                | ffdb_protocol::MigrationStatus::Applying
                | ffdb_protocol::MigrationStatus::RollingBack
                | ffdb_protocol::MigrationStatus::Failed => {
                    return Err(sqlx::Error::Protocol(
                        "worker returned a non-terminal migration status".into(),
                    ));
                }
            }
            sqlx::query(
                "UPDATE project_databases SET schema_version=$2,updated_at=now() WHERE id=$1",
            )
            .bind(route.database_id.0)
            .bind(i64::try_from(record.schema_version_after).unwrap_or(i64::MAX))
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
        }
        WorkerResponse::Backup(backup) => {
            let actor = match mode {
                ExecutionMode::Developer(principal) => principal.api_key_id.0,
                ExecutionMode::EndUser(_) => {
                    return Err(sqlx::Error::Protocol("invalid backup actor".into()));
                }
            };
            let sha256 = decode_hex_checksum(&backup.sha256)
                .ok_or(sqlx::Error::Decode("invalid worker backup checksum".into()))?;
            let provider_key = format!("local/{project_id}/{}.sqlite3", backup.backup_id);
            sqlx::query(
                "INSERT INTO backups \
                 (id,project_id,database_id,provider_key,sha256,size_bytes,encryption_key_version,state,created_by,verified_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,1,'ready',$7,now()) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(backup.backup_id.0)
            .bind(project_id.0)
            .bind(route.database_id.0)
            .bind(provider_key)
            .bind(sha256)
            .bind(i64::try_from(backup.size_bytes).unwrap_or(i64::MAX))
            .bind(actor)
            .execute(pool)
            .await?;
        }
        WorkerResponse::Restore(restore) => {
            if !restore.integrity_ok {
                return Err(sqlx::Error::Protocol(
                    "worker returned an unverified restore".into(),
                ));
            }
            let mut transaction = pool.begin().await?;
            let updated = sqlx::query(
                "UPDATE backups SET state='ready',verified_at=now(),restore_receipt_id=NULL \
                 WHERE id=$1 AND project_id=$2 AND state='restoring' AND restore_receipt_id=$3",
            )
            .bind(restore.backup_id.0)
            .bind(project_id.0)
            .bind(operation_receipt_id.ok_or_else(|| {
                sqlx::Error::Protocol("restore operation receipt is missing".into())
            })?)
            .execute(&mut *transaction)
            .await?;
            if updated.rows_affected() != 1 {
                return Err(sqlx::Error::Protocol(
                    "restored backup metadata missing".into(),
                ));
            }
            let database = sqlx::query(
                "UPDATE project_databases SET lifecycle_state='active',schema_version=$2,updated_at=now() \
                 WHERE id=$1 AND lifecycle_state='restoring'",
            )
            .bind(route.database_id.0)
            .bind(i64::try_from(restore.schema_version).unwrap_or(i64::MAX))
            .execute(&mut *transaction)
            .await?;
            if database.rows_affected() != 1 {
                return Err(sqlx::Error::Protocol(
                    "restored database lifecycle state changed".into(),
                ));
            }
            let project = sqlx::query(
                "UPDATE projects SET lifecycle_state='active',updated_at=now() \
                 WHERE id=$1 AND lifecycle_state='restoring'",
            )
            .bind(project_id.0)
            .execute(&mut *transaction)
            .await?;
            if project.rows_affected() != 1 {
                return Err(sqlx::Error::Protocol(
                    "restored project lifecycle state changed".into(),
                ));
            }
            transaction.commit().await?;
        }
        _ => {}
    }
    Ok(())
}

/// Restore retries must be able to reach the same fenced worker while ordinary
/// traffic remains blocked by the registry's active-only routing policy.
async fn resolve_restore_route(
    state: &ApiState,
    project_id: ProjectId,
) -> Result<DatabaseRoute, RoutingError> {
    let Some(pool) = &state.readiness_pool else {
        return state.router.resolve(project_id).await;
    };
    let row = sqlx::query(
        "SELECT p.database_id,r.node_id,r.generation,p.lifecycle_state, \
                d.lifecycle_state database_state,n.lifecycle_state node_state \
         FROM projects p JOIN project_databases d ON d.project_id=p.id AND d.id=p.database_id \
         JOIN database_routes r ON r.id=d.route_id AND r.database_id=d.id \
         JOIN nodes n ON n.id=r.node_id WHERE p.id=$1",
    )
    .bind(project_id.0)
    .fetch_optional(pool)
    .await
    .map_err(|_| RoutingError::Unavailable)?
    .ok_or(RoutingError::NotFound)?;
    let project_state: String = row
        .try_get("lifecycle_state")
        .map_err(|_| RoutingError::Inconsistent)?;
    let database_state: String = row
        .try_get("database_state")
        .map_err(|_| RoutingError::Inconsistent)?;
    let node_state: String = row
        .try_get("node_state")
        .map_err(|_| RoutingError::Inconsistent)?;
    if !matches!(project_state.as_str(), "active" | "restoring")
        || !matches!(database_state.as_str(), "active" | "restoring")
        || node_state != "active"
    {
        return Err(RoutingError::Unavailable);
    }
    let generation: i64 = row
        .try_get("generation")
        .map_err(|_| RoutingError::Inconsistent)?;
    Ok(DatabaseRoute {
        project_id,
        database_id: DatabaseId(
            row.try_get("database_id")
                .map_err(|_| RoutingError::Inconsistent)?,
        ),
        node_id: NodeId(
            row.try_get("node_id")
                .map_err(|_| RoutingError::Inconsistent)?,
        ),
        generation: u64::try_from(generation).map_err(|_| RoutingError::Inconsistent)?,
    })
}

async fn begin_restore_lifecycle(
    state: &ApiState,
    project_id: ProjectId,
    backup_id: BackupId,
    receipt_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(pool) = &state.readiness_pool else {
        return Ok(());
    };
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(project_id.0)
        .fetch_one(&mut *transaction)
        .await?;
    let backup = sqlx::query(
        "UPDATE backups SET state='restoring',restore_receipt_id=$3 \
         WHERE id=$1 AND project_id=$2 \
           AND (state='ready' OR (state='restoring' AND restore_receipt_id=$3))",
    )
    .bind(backup_id.0)
    .bind(project_id.0)
    .bind(receipt_id)
    .execute(&mut *transaction)
    .await?;
    if backup.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol("backup is not restorable".into()));
    }
    sqlx::query("UPDATE projects SET lifecycle_state='restoring',updated_at=now() WHERE id=$1")
        .bind(project_id.0)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "UPDATE project_databases SET lifecycle_state='restoring',updated_at=now() WHERE project_id=$1",
    )
    .bind(project_id.0)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestoreFailureStage {
    WorkerResponseMismatch,
}

impl RestoreFailureStage {
    fn name(self) -> &'static str {
        match self {
            Self::WorkerResponseMismatch => "worker_response_mismatch",
        }
    }
}

fn restore_response_matches(expected: BackupId, response: &WorkerResponse) -> bool {
    matches!(response, WorkerResponse::Restore(value) if value.backup_id == expected)
}

async fn fail_restore_lifecycle(
    state: &ApiState,
    project_id: ProjectId,
    backup_id: BackupId,
    stage: RestoreFailureStage,
) {
    let Some(pool) = &state.readiness_pool else {
        return;
    };
    tracing::error!(%project_id, %backup_id, stage = stage.name(), "restore lifecycle failed");
    if let Ok(mut transaction) = pool.begin().await {
        let backup = sqlx::query(
            "UPDATE backups SET state='ready',restore_receipt_id=NULL \
             WHERE id=$1 AND project_id=$2 AND state='restoring'",
        )
        .bind(backup_id.0)
        .bind(project_id.0)
        .execute(&mut *transaction)
        .await;
        let project = sqlx::query(
            "UPDATE projects SET lifecycle_state='failed',updated_at=now() WHERE id=$1",
        )
        .bind(project_id.0)
        .execute(&mut *transaction)
        .await;
        let database = sqlx::query(
            "UPDATE project_databases SET lifecycle_state='failed',updated_at=now() WHERE project_id=$1",
        )
        .bind(project_id.0)
        .execute(&mut *transaction)
        .await;
        if backup.is_ok() && project.is_ok() && database.is_ok() {
            let _ = transaction.commit().await;
        }
    }
}

fn decode_hex_checksum(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

fn operation_descriptor(operation: &WorkerOperation) -> (&'static str, &'static str, u32) {
    match operation {
        WorkerOperation::Query(_) => ("database.query", "query", 1),
        WorkerOperation::Transaction(payload) => (
            "database.transaction",
            "transaction",
            u32::try_from(payload.statements.len())
                .unwrap_or(10)
                .clamp(1, 10),
        ),
        WorkerOperation::ApplyMigration(_) => ("database.migration.apply", "migration", 10),
        WorkerOperation::RollbackMigration { .. } => {
            ("database.migration.rollback", "migration", 10)
        }
        WorkerOperation::MigrationHistory { .. } => ("database.migration.history", "migration", 1),
        WorkerOperation::Schema => ("database.schema.read", "schema", 1),
        WorkerOperation::Policies => ("database.policy.read", "policy", 1),
        WorkerOperation::Snapshot(_) => ("database.snapshot.read", "snapshot", 5),
        WorkerOperation::SyncPull(_) => ("database.sync.pull", "sync", 1),
        WorkerOperation::SyncPush(payload) => (
            "database.sync.push",
            "sync",
            u32::try_from(payload.mutations.len())
                .unwrap_or(10)
                .clamp(1, 10),
        ),
        WorkerOperation::Backup { .. } => ("database.backup.create", "backup", 10),
        WorkerOperation::Restore { .. } => ("database.backup.restore", "backup", 10),
        WorkerOperation::IntegrityCheck => ("database.integrity.check", "database", 5),
        WorkerOperation::StorageAuthorize(_) => ("storage.authorize", "object", 2),
        WorkerOperation::StorageReserve(_) => ("storage.reserve", "object", 2),
        WorkerOperation::StorageCommit(_) => ("storage.commit", "object", 2),
        WorkerOperation::StorageReceipt(_) => ("storage.receipt", "object", 1),
        WorkerOperation::StorageUsage => ("storage.usage", "object", 1),
        WorkerOperation::StorageRelease(_) => ("storage.release", "object", 1),
        WorkerOperation::StorageList(payload) => (
            "storage.list",
            "object",
            (payload.limit.saturating_add(99) / 100).clamp(1, 10),
        ),
        WorkerOperation::StorageCleanup { .. } => ("storage.cleanup", "object", 5),
        WorkerOperation::StorageCleanupClaim(_) => ("storage.cleanup.claim", "object", 5),
        WorkerOperation::StorageCleanupAck(_) => ("storage.cleanup.ack", "object", 2),
        WorkerOperation::StorageBuckets => ("storage.bucket.list", "bucket", 1),
        WorkerOperation::StorageCreateBucket(_) => ("storage.bucket.create", "bucket", 3),
    }
}

async fn enforce_execution_rate_limit(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    cost: u32,
) -> Result<(), Response> {
    let Some(limiter) = &state.rate_limiter else {
        return Ok(());
    };
    let (dimension, actor) = match mode {
        ExecutionMode::Developer(principal) => {
            (RateDimension::ApiKey, principal.api_key_id.to_string())
        }
        ExecutionMode::EndUser(context) => (RateDimension::User, context.subject.to_string()),
    };
    let decisions = limiter
        .check_many(
            vec![
                (
                    RateDimension::Project,
                    project_id.to_string().into_bytes(),
                    cost,
                ),
                (dimension, actor.into_bytes(), cost),
            ],
            now_ms(),
        )
        .await
        .map_err(|_| rate_limit_unavailable(request_id).into_response())?;
    if decisions.len() != 2
        && decisions
            .last()
            .is_none_or(|decision| !matches!(decision, RateLimitDecision::Denied { .. }))
    {
        return Err(rate_limit_unavailable(request_id).into_response());
    }
    for decision in decisions {
        if let RateLimitDecision::Denied { retry_after_ms } = decision {
            return Err(rate_limited(request_id, retry_after_ms));
        }
    }
    Ok(())
}

async fn enforce_rate_dimension(
    limiter: &Arc<dyn ApiRateLimiter>,
    dimension: RateDimension,
    identifier: &[u8],
    cost: u32,
    request_id: RequestId,
) -> Result<(), Response> {
    match limiter.check(dimension, identifier, cost, now_ms()).await {
        Ok(RateLimitDecision::Allowed { .. }) => Ok(()),
        Ok(RateLimitDecision::Denied { retry_after_ms }) => {
            Err(rate_limited(request_id, retry_after_ms))
        }
        Err(_) => Err(rate_limit_unavailable(request_id).into_response()),
    }
}

async fn append_audit(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    action: &str,
    resource_kind: &str,
    outcome: AuditOutcome,
) -> Result<(), ()> {
    let sink = &state.audit;
    let (organization_id, actor_kind, actor_id) = match mode {
        ExecutionMode::Developer(principal) => (
            Some(principal.organization_id),
            ActorKind::ApiKey,
            Some(principal.api_key_id.0),
        ),
        ExecutionMode::EndUser(context) => (None, ActorKind::User, Some(context.subject.0)),
    };
    sink.append(AuditDraft {
        occurred_at_ms: now_ms(),
        organization_id,
        project_id: Some(project_id),
        request_id,
        actor_kind,
        actor_id,
        action: action.to_owned(),
        resource_kind: resource_kind.to_owned(),
        resource_id: None,
        outcome,
        source_ip: trusted_source_ip(),
        metadata: serde_json::json!({"protocol_version": PROTOCOL_VERSION}),
    })
    .await
    .map(|_| ())
    .map_err(|_| ())
}

async fn append_audit_best_effort(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    action: &str,
    resource_kind: &str,
    outcome: AuditOutcome,
) {
    if append_audit(
        state,
        project_id,
        request_id,
        mode,
        action,
        resource_kind,
        outcome,
    )
    .await
    .is_err()
    {
        tracing::error!(%request_id, action, "failed to append terminal audit event");
    }
}

fn execution_mode_matches_project(mode: &ExecutionMode, project_id: ProjectId) -> bool {
    match mode {
        ExecutionMode::EndUser(context) => context.project_id == project_id,
        // The verifier must bind the stored developer key's project/organization
        // before constructing a principal. DeveloperPrincipal intentionally does
        // not carry caller-controlled routing data.
        ExecutionMode::Developer(_) => true,
    }
}

fn parse_project(value: &str, request_id: RequestId) -> Result<ProjectId, ApiError> {
    Uuid::parse_str(value).map(ProjectId).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "project.invalid_id",
            "invalid project identifier",
            request_id,
        )
    })
}

fn bearer(headers: &HeaderMap) -> Result<&str, CredentialError> {
    let header = headers.get(AUTHORIZATION).ok_or(CredentialError::Missing)?;
    let value = header.to_str().map_err(|_| CredentialError::Invalid)?;
    value
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty() && token.len() <= 8_192)
        .ok_or(CredentialError::Invalid)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    body: ErrorEnvelope,
}

impl ApiError {
    fn new(status: StatusCode, code: &str, message: &str, request_id: RequestId) -> Self {
        Self {
            status,
            body: ErrorEnvelope {
                error: PlatformError::safe(code, message, request_id),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

fn validation_error(message: impl Into<String>, request_id: RequestId) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "query.invalid_request",
        &message.into(),
        request_id,
    )
}

fn credential_error(error: CredentialError, request_id: RequestId) -> ApiError {
    let (status, code, message) = match error {
        CredentialError::Missing => (
            StatusCode::UNAUTHORIZED,
            "auth.missing_credential",
            "authorization is required",
        ),
        CredentialError::Invalid => (
            StatusCode::UNAUTHORIZED,
            "auth.invalid_credential",
            "credential is invalid",
        ),
        CredentialError::Expired => (
            StatusCode::UNAUTHORIZED,
            "auth.expired_credential",
            "credential has expired",
        ),
        CredentialError::WrongProject => (
            StatusCode::FORBIDDEN,
            "auth.wrong_project",
            "credential is not valid for this project",
        ),
        CredentialError::InsufficientScope => (
            StatusCode::FORBIDDEN,
            "auth.insufficient_scope",
            "credential lacks the required scope",
        ),
        CredentialError::Disabled => (
            StatusCode::FORBIDDEN,
            "auth.account_disabled",
            "account is disabled",
        ),
        CredentialError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "auth.unavailable",
            "authentication service is temporarily unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id)
}

fn rate_limited(request_id: RequestId, retry_after_ms: u64) -> Response {
    let retry_after_seconds = retry_after_ms.saturating_add(999) / 1_000;
    let mut response = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limit.exceeded",
        "request rate limit exceeded",
        request_id,
    )
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_seconds.max(1).to_string()) {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

fn rate_limit_unavailable(request_id: RequestId) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "rate_limit.unavailable",
        "rate limiting service is temporarily unavailable",
        request_id,
    )
}

fn audit_unavailable(request_id: RequestId) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "audit.unavailable",
        "audit service is temporarily unavailable",
        request_id,
    )
}

fn control_plane_unavailable(request_id: RequestId) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_plane.unavailable",
        "control plane is temporarily unavailable",
        request_id,
    )
}

fn routing_error(error: RoutingError, request_id: RequestId) -> ApiError {
    match error {
        RoutingError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "project.not_found",
            "project was not found",
            request_id,
        ),
        RoutingError::Unavailable | RoutingError::StaleGeneration => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "project.unavailable",
            "project is temporarily unavailable",
            request_id,
        ),
        RoutingError::Inconsistent => internal_error(request_id),
    }
}

fn execution_error(error: ExecutionError, request_id: RequestId) -> ApiError {
    match error {
        ExecutionError::QueueFull => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "quota.queue_full",
            "project queue is full",
            request_id,
        ),
        ExecutionError::DeadlineExceeded => ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "query.deadline_exceeded",
            "query deadline exceeded",
            request_id,
        ),
        ExecutionError::StaleGeneration | ExecutionError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "project.worker_unavailable",
            "database worker is temporarily unavailable",
            request_id,
        ),
        ExecutionError::Protocol => internal_error(request_id),
        ExecutionError::Rejected { code } => {
            let safe_code = if code.starts_with("query.")
                || code.starts_with("rls.")
                || code.starts_with("migration.")
                || code.starts_with("sync.")
            {
                code
            } else {
                "query.rejected".into()
            };
            ApiError::new(
                StatusCode::BAD_REQUEST,
                &safe_code,
                "database request was rejected",
                request_id,
            )
        }
    }
}

fn metering_error(error: metering::MeteringError, request_id: RequestId) -> ApiError {
    match error {
        metering::MeteringError::Store(ffdb_org_metrics::MetricsError::LimitExceeded(
            "write_units",
        )) => ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "billing.write_limit_reached",
            "the organization's included write allowance is exhausted",
            request_id,
        ),
        metering::MeteringError::Store(ffdb_org_metrics::MetricsError::LimitExceeded(
            "storage_bytes",
        )) => ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "billing.storage_limit_reached",
            "the organization's included storage allowance is exhausted",
            request_id,
        ),
        metering::MeteringError::Store(ffdb_org_metrics::MetricsError::LimitExceeded(
            "monthly_active_users",
        )) => ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "billing.active_user_limit_reached",
            "the organization's included monthly active user allowance is exhausted",
            request_id,
        ),
        metering::MeteringError::ReportingBlocked => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing.reporting_unhealthy",
            "billable writes are paused until usage reporting recovers",
            request_id,
        ),
        metering::MeteringError::InvalidOperation => ApiError::new(
            StatusCode::BAD_REQUEST,
            "query.invalid_statement",
            "database request could not be classified",
            request_id,
        ),
        metering::MeteringError::Store(ffdb_org_metrics::MetricsError::LimitExceeded(_))
        | metering::MeteringError::Store(_)
        | metering::MeteringError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing.metering_unavailable",
            "usage metering is temporarily unavailable",
            request_id,
        ),
    }
}

fn internal_error(request_id: RequestId) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: ErrorEnvelope {
            error: PlatformError {
                code: "internal.error".into(),
                message: "internal server error".into(),
                request_id,
                details: BTreeMap::<String, Value>::new(),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use ffdb_audit::InMemoryAuditSink;
    use ffdb_protocol::{
        DatabaseId, DatabaseRoute, NodeId, OrganizationId, RestoreResult, TokenId, UserId,
        WorkerResponse,
    };
    use http::Request as HttpRequest;
    use tower::ServiceExt as _;

    #[test]
    fn snapshot_filter_parser_accepts_repeated_tables_and_rejects_unknown_parameters() {
        assert_eq!(snapshot_tables(None), Ok(None));
        assert_eq!(
            snapshot_tables(Some("table=documents&table=comments")),
            Ok(Some(vec!["documents".to_owned(), "comments".to_owned()]))
        );
        assert!(snapshot_tables(Some("other=documents")).is_err());
        assert!(snapshot_tables(Some("table=")).is_err());
    }

    #[derive(Debug)]
    struct TestServices;
    #[derive(Debug)]
    struct DenyRateLimiter;

    #[async_trait]
    impl ApiRateLimiter for DenyRateLimiter {
        async fn check(
            &self,
            _dimension: RateDimension,
            _identifier: &[u8],
            _cost: u32,
            _now_ms: i64,
        ) -> Result<RateLimitDecision, RateLimitError> {
            Ok(RateLimitDecision::Denied {
                retry_after_ms: 1_500,
            })
        }
    }

    #[async_trait]
    impl DatabaseRouter for TestServices {
        async fn resolve(&self, project_id: ProjectId) -> Result<DatabaseRoute, RoutingError> {
            Ok(DatabaseRoute {
                project_id,
                database_id: DatabaseId::new(),
                node_id: NodeId::new(),
                generation: 1,
            })
        }
    }
    #[async_trait]
    impl DatabaseExecutor for TestServices {
        async fn execute(
            &self,
            _route: &DatabaseRoute,
            request: WorkerRequest,
        ) -> Result<ffdb_protocol::WorkerExecution, ExecutionError> {
            let receipt_id = request.operation_receipt_id.unwrap_or(request.request_id.0);
            let request_id = request.request_id;
            match request.operation {
                WorkerOperation::Policies => Ok(ffdb_protocol::WorkerExecution {
                    response: WorkerResponse::Policies(Vec::new()),
                    usage: ffdb_protocol::UsageReceipt {
                        receipt_id,
                        request_id,
                        reads: 0,
                        writes: 0,
                        logical_database_bytes: 0,
                        subject: None,
                        recorded_at_ms: 0,
                    },
                    statement_telemetry: Vec::new(),
                }),
                _ => Err(ExecutionError::Rejected {
                    code: "query.statement_not_allowed".into(),
                }),
            }
        }
    }
    #[async_trait]
    impl CredentialVerifier for TestServices {
        async fn verify_query_credential(
            &self,
            project_id: ProjectId,
            _bearer_token: &str,
        ) -> Result<ExecutionMode, CredentialError> {
            Ok(ExecutionMode::EndUser(AuthContext {
                project_id,
                subject: UserId::new(),
                role: "authenticated".into(),
                claims: Default::default(),
                token_id: TokenId::new(),
            }))
        }
        async fn verify_developer_credential(
            &self,
            _project_id: ProjectId,
            _bearer_token: &str,
            required_scope: DeveloperScope,
        ) -> Result<DeveloperPrincipal, CredentialError> {
            Ok(DeveloperPrincipal {
                organization_id: ffdb_protocol::OrganizationId::new(),
                api_key_id: ffdb_protocol::ApiKeyId::new(),
                scopes: vec![required_scope],
                actor_label: "test".into(),
            })
        }
        async fn verify_end_user_credential(
            &self,
            project_id: ProjectId,
            _bearer_token: &str,
        ) -> Result<AuthContext, CredentialError> {
            Ok(AuthContext {
                project_id,
                subject: UserId::new(),
                role: "authenticated".into(),
                claims: Default::default(),
                token_id: TokenId::new(),
            })
        }
    }

    fn app() -> Router {
        app_with_rate_limiter(None)
    }

    fn app_with_rate_limiter(rate_limiter: Option<Arc<dyn ApiRateLimiter>>) -> Router {
        let services = Arc::new(TestServices);
        router(ApiState {
            router: services.clone(),
            executor: services.clone(),
            credentials: services,
            limits: ResourceLimits::default(),
            metrics: None,
            observability: None,
            management: None,
            project_auth: None,
            storage: None,
            email: None,
            usage_metering: None,
            commerce: None,
            instance: None,
            host_updates: None,
            cors_allowed_origins: vec!["https://portal.example.test".to_owned()],
            trusted_proxy_cidrs: Vec::new(),
            rate_limiter,
            audit: Arc::new(InMemoryAuditSink::default()),
            readiness_pool: None,
        })
    }

    #[tokio::test]
    async fn health_has_request_id() {
        let response = app()
            .oneshot(
                HttpRequest::get("/healthz")
                    .body(Body::empty())
                    .unwrap_or_else(|_| HttpRequest::new(Body::empty())),
            )
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-request-id"));
    }

    #[tokio::test]
    async fn query_rejects_missing_authorization_without_dispatch() {
        let project = ProjectId::new();
        let response = app()
            .oneshot(
                HttpRequest::post(format!("/v1/projects/{project}/query"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"select 1"}"#))
                    .unwrap_or_else(|_| HttpRequest::new(Body::empty())),
            )
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap_or_default();
        assert!(String::from_utf8_lossy(&body).contains("auth.missing_credential"));
    }

    #[tokio::test]
    async fn pre_auth_rate_limit_is_fail_closed_and_sets_retry_after() {
        let project = ProjectId::new();
        let response = app_with_rate_limiter(Some(Arc::new(DenyRateLimiter)))
            .oneshot(
                HttpRequest::post(format!("/v1/projects/{project}/auth/sign-in"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"user@example.test","password":"test"}"#,
                    ))
                    .unwrap_or_else(|_| HttpRequest::new(Body::empty())),
            )
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("2")
        );
        assert!(response.headers().contains_key("x-request-id"));
    }

    #[tokio::test]
    async fn generic_credentialed_routes_do_not_consume_anonymous_ip_bucket() {
        let project = ProjectId::new();
        let response = app_with_rate_limiter(Some(Arc::new(DenyRateLimiter)))
            .oneshot(
                HttpRequest::post(format!("/v1/projects/{project}/query"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"select 1"}"#))
                    .unwrap_or_else(|_| HttpRequest::new(Body::empty())),
            )
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn anonymous_admission_policy_is_limited_to_sensitive_post_routes() {
        let project = ProjectId::new();
        for path in [
            "/v1/instance".to_owned(),
            "/v1/developer/bootstrap".to_owned(),
            "/v1/developer/sign-in".to_owned(),
            "/v1/developer/refresh".to_owned(),
            "/v1/developer/invitations/accept".to_owned(),
            format!("/v1/projects/{project}/auth/register"),
            format!("/v1/projects/{project}/auth/verify"),
            format!("/v1/projects/{project}/auth/sign-in"),
            format!("/v1/projects/{project}/auth/refresh"),
            format!("/v1/projects/{project}/auth/password/reset"),
            format!("/v1/projects/{project}/auth/password/reset/complete"),
        ] {
            assert!(requires_pre_auth_rate_limit(&Method::POST, &path), "{path}");
        }

        for (method, path) in [
            (Method::GET, "/v1/instance".to_owned()),
            (Method::POST, format!("/v1/projects/{project}/query")),
            (
                Method::POST,
                format!("/v1/projects/{project}/auth/sign-out"),
            ),
            (Method::POST, "/v1/billing/webhooks/stripe".to_owned()),
            (Method::GET, "/readyz".to_owned()),
        ] {
            assert!(!requires_pre_auth_rate_limit(&method, &path), "{path}");
        }
    }

    #[test]
    fn authentication_dimensions_keep_the_conservative_policy() {
        for dimension in [
            RateDimension::Ip,
            RateDimension::AuthProject,
            RateDimension::AuthUser,
            RateDimension::AuthApiKey,
        ] {
            assert!(is_pre_auth_dimension(dimension));
        }
        for dimension in [
            RateDimension::Project,
            RateDimension::User,
            RateDimension::ApiKey,
        ] {
            assert!(!is_pre_auth_dimension(dimension));
        }
    }

    #[test]
    fn project_cors_policy_resolves_the_project_from_the_request_path() {
        let project = ProjectId::new();
        assert_eq!(
            project_id_from_request_path(&format!("/v1/projects/{project}/query")),
            Some(project)
        );
        assert_eq!(project_id_from_request_path("/v1/organizations"), None);
        assert_eq!(
            project_id_from_request_path("/v1/projects/not-a-project/query"),
            None
        );
    }

    #[tokio::test]
    async fn audit_uses_transport_source_ip_not_forwarded_headers() {
        let services = Arc::new(TestServices);
        let audit = Arc::new(InMemoryAuditSink::default());
        let app = router(ApiState {
            router: services.clone(),
            executor: services.clone(),
            credentials: services,
            limits: ResourceLimits::default(),
            metrics: None,
            observability: None,
            management: None,
            project_auth: None,
            storage: None,
            email: None,
            usage_metering: None,
            commerce: None,
            instance: None,
            host_updates: None,
            cors_allowed_origins: Vec::new(),
            trusted_proxy_cidrs: Vec::new(),
            rate_limiter: None,
            audit: audit.clone(),
            readiness_pool: None,
        });
        let project = ProjectId::new();
        let mut request = HttpRequest::get(format!("/v1/projects/{project}/policies"))
            .header("authorization", "Bearer test")
            .header("x-forwarded-for", "203.0.113.99")
            .body(Body::empty())
            .unwrap_or_else(|_| HttpRequest::new(Body::empty()));
        let transport = SocketAddr::from(([192, 0, 2, 44], 443));
        request.extensions_mut().insert(ConnectInfo(transport));
        let response = app
            .oneshot(request)
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status(), StatusCode::OK);
        let events = audit.events().await;
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| event.source_ip == Some(transport.ip()))
        );
    }

    #[test]
    fn trusted_proxy_chain_uses_the_first_untrusted_hop_from_the_right()
    -> Result<(), Box<dyn std::error::Error>> {
        let trusted = [
            "127.0.0.1/32".parse::<IpNet>()?,
            "10.0.0.0/8".parse::<IpNet>()?,
        ];
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.99, 203.0.113.45, 10.1.2.3"),
        );
        let peer = IpAddr::from([127, 0, 0, 1]);
        assert_eq!(
            resolve_client_ip(peer, &headers, &trusted),
            IpAddr::from([203, 0, 113, 45])
        );
        Ok(())
    }

    #[test]
    fn malformed_forwarded_chain_fails_closed_to_the_transport_peer()
    -> Result<(), Box<dyn std::error::Error>> {
        let trusted = ["127.0.0.1/32".parse::<IpNet>()?];
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        let peer = IpAddr::from([127, 0, 0, 1]);
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), peer);
        Ok(())
    }

    #[test]
    fn forged_end_user_context_cannot_cross_project_boundary() {
        let routed = ProjectId::new();
        let foreign = ProjectId::new();
        let mode = ExecutionMode::EndUser(AuthContext {
            project_id: foreign,
            subject: UserId::new(),
            role: "authenticated".into(),
            claims: Default::default(),
            token_id: TokenId::new(),
        });
        assert!(!execution_mode_matches_project(&mode, routed));
    }

    #[test]
    fn restore_response_must_match_requested_backup_before_terminal_failure() {
        let expected = BackupId::new();
        let matching = WorkerResponse::Restore(RestoreResult {
            backup_id: expected,
            integrity_ok: true,
            schema_version: 7,
        });
        let mismatched = WorkerResponse::Restore(RestoreResult {
            backup_id: BackupId::new(),
            integrity_ok: true,
            schema_version: 7,
        });
        assert!(restore_response_matches(expected, &matching));
        assert!(!restore_response_matches(expected, &mismatched));
        assert_eq!(
            RestoreFailureStage::WorkerResponseMismatch.name(),
            "worker_response_mismatch"
        );
    }

    #[test]
    fn tracing_route_never_contains_signed_sync_cursor() {
        let cursor = "super-secret-signed-cursor";
        let uri = format!("/v1/projects/{}/sync?cursor={cursor}", ProjectId::new());
        let parsed = uri.parse::<axum::http::Uri>();
        assert!(parsed.is_ok());
        let route = parsed
            .ok()
            .map(|value| stable_route(value.path()))
            .unwrap_or("unmatched");
        assert_eq!(route, "/v1/projects/:id/sync");
        assert!(!route.contains(cursor));
    }

    #[test]
    fn instance_disable_routes_have_bounded_trace_names() {
        let organization_id = OrganizationId::new();
        let user_id = UserId::new();
        assert_eq!(
            stable_route(&format!("/v1/instance/organizations/{organization_id}")),
            "/v1/instance/organizations/:organization"
        );
        assert_eq!(
            stable_route(&format!("/v1/instance/users/{user_id}")),
            "/v1/instance/users/:user"
        );
    }

    #[test]
    fn host_update_job_route_never_exposes_job_ids_to_metrics() {
        let job_id = Uuid::now_v7();
        assert_eq!(
            stable_route(&format!("/v1/instance/updates/jobs/{job_id}")),
            "/v1/instance/updates/jobs/:job"
        );
    }
}
