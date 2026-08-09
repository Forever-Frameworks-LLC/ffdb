use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_audit::AuditOutcome;
use ffdb_billing::{
    BillingError, PlatformBillingProvider, PlatformBillingUpdate, PlatformCheckoutInput,
    PlatformInvoiceUpdate, PlatformPortalInput, ProviderInvoiceStatus, ProviderSubscriptionStatus,
    VerifiedBillingEvent,
};
use ffdb_protocol::{
    BillingRedirect, CreatePlatformCheckoutRequest, OrganizationId, PlatformBillingStatus,
    PlatformBillingSummary, PlatformBillingTier, PlatformBillingUnit, PlatformInvoiceStatus,
    PlatformInvoiceSummary, PlatformUsageAllowance, ProjectId, ProjectPaymentCapabilities,
    ProjectPaymentsStatus, ProjectPaymentsSummary, RequestId, UserId,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use uuid::Uuid;

use super::management::{
    authenticated, authorized_organization_admin, require_management_audit,
    terminal_management_audit,
};
use super::{ApiError, ApiState, now_ms};

#[derive(Clone)]
pub(crate) struct BillingService {
    pool: PgPool,
    provider: Option<Arc<dyn PlatformBillingProvider>>,
    pro_billing_unit: PlatformBillingUnit,
}

impl std::fmt::Debug for BillingService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BillingService")
            .field("provider", &self.provider.as_ref().map(|_| "configured"))
            .field("pro_billing_unit", &self.pro_billing_unit)
            .finish_non_exhaustive()
    }
}

impl BillingService {
    pub(crate) fn new(
        pool: PgPool,
        provider: Option<Arc<dyn PlatformBillingProvider>>,
        pro_billing_unit: PlatformBillingUnit,
    ) -> Self {
        Self {
            pool,
            provider,
            pro_billing_unit,
        }
    }

    fn provider(&self) -> Result<Arc<dyn PlatformBillingProvider>, ServiceError> {
        self.provider
            .clone()
            .ok_or(ServiceError::ProviderUnavailable)
    }

    async fn summary(
        &self,
        organization_id: OrganizationId,
        actor: UserId,
    ) -> Result<PlatformBillingSummary, ServiceError> {
        ensure_membership(&self.pool, organization_id, actor).await?;
        let account = sqlx::query(
            "SELECT tier,status,billing_unit,seat_quantity, \
                    (extract(epoch FROM current_period_start)*1000)::bigint current_period_start_ms, \
                    (extract(epoch FROM current_period_end)*1000)::bigint current_period_end_ms, \
                    cancel_at_period_end FROM organization_billing_accounts \
             WHERE organization_id=$1",
        )
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
        let (
            tier,
            status,
            billing_unit,
            seat_quantity,
            current_period_start_ms,
            current_period_end_ms,
            cancel_at_period_end,
        ) = if let Some(row) = account {
            let tier = parse_tier(row.try_get("tier").map_err(|_| ServiceError::Unavailable)?)?;
            let status = parse_status(
                row.try_get("status")
                    .map_err(|_| ServiceError::Unavailable)?,
            )?;
            let billing_unit = parse_billing_unit(
                row.try_get("billing_unit")
                    .map_err(|_| ServiceError::Unavailable)?,
            )?;
            let quantity: i32 = row
                .try_get("seat_quantity")
                .map_err(|_| ServiceError::Unavailable)?;
            (
                tier,
                status,
                billing_unit,
                u32::try_from(quantity).map_err(|_| ServiceError::Unavailable)?,
                row.try_get("current_period_start_ms")
                    .map_err(|_| ServiceError::Unavailable)?,
                row.try_get("current_period_end_ms")
                    .map_err(|_| ServiceError::Unavailable)?,
                row.try_get("cancel_at_period_end")
                    .map_err(|_| ServiceError::Unavailable)?,
            )
        } else {
            (
                PlatformBillingTier::Free,
                PlatformBillingStatus::Free,
                PlatformBillingUnit::Organization,
                1,
                None,
                None,
                false,
            )
        };
        // Only active/trialing subscriptions confer paid entitlements. The
        // selected tier/status remain visible so the portal can guide recovery.
        let entitlement_tier = if matches!(
            status,
            PlatformBillingStatus::Active | PlatformBillingStatus::Trialing
        ) {
            tier
        } else {
            PlatformBillingTier::Free
        };
        let catalog_tier = tier_name(entitlement_tier);
        let catalog = sqlx::query(
            "SELECT project_limit,storage_bytes,monthly_reads,monthly_writes, \
                    monthly_active_users,overage_enabled FROM billing_price_catalog \
             WHERE tier=$1 AND active=true",
        )
        .bind(catalog_tier)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ServiceError::Unavailable)?
        .ok_or(ServiceError::Unavailable)?;
        let project_limit: Option<i32> = catalog
            .try_get("project_limit")
            .map_err(|_| ServiceError::Unavailable)?;
        let policy = sqlx::query(
            "SELECT COALESCE((SELECT billing_enforcement_enabled FROM instance_settings \
                    WHERE singleton=true),false) billing_enforcement_enabled, \
                    EXISTS(SELECT 1 FROM organization_billing_exemptions \
                           WHERE organization_id=$1) billing_exempt",
        )
        .bind(organization_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
        let billing_enforcement_enabled: bool = policy
            .try_get("billing_enforcement_enabled")
            .map_err(|_| ServiceError::Unavailable)?;
        let billing_exempt: bool = policy
            .try_get("billing_exempt")
            .map_err(|_| ServiceError::Unavailable)?;
        Ok(PlatformBillingSummary {
            organization_id,
            tier,
            status,
            billing_unit,
            seat_quantity,
            project_limit: if billing_enforcement_enabled && !billing_exempt {
                project_limit
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| ServiceError::Unavailable)?
            } else {
                None
            },
            usage_allowance: PlatformUsageAllowance {
                storage_bytes: positive_u64(&catalog, "storage_bytes")?,
                monthly_reads: positive_u64(&catalog, "monthly_reads")?,
                monthly_writes: positive_u64(&catalog, "monthly_writes")?,
                monthly_active_users: positive_u64(&catalog, "monthly_active_users")?,
                overage_enabled: catalog
                    .try_get("overage_enabled")
                    .map_err(|_| ServiceError::Unavailable)?,
            },
            current_period_start_ms,
            current_period_end_ms,
            cancel_at_period_end,
            provider_configured: self
                .provider
                .as_ref()
                .is_some_and(|provider| provider.is_configured()),
            billing_enforcement_enabled,
            billing_exempt,
        })
    }

    async fn checkout(
        &self,
        organization_id: OrganizationId,
        tier: PlatformBillingTier,
        email: &str,
        provider_idempotency_key: String,
    ) -> Result<BillingRedirect, ServiceError> {
        if tier == PlatformBillingTier::Free {
            return Err(ServiceError::InvalidRequest);
        }
        let provider = self.provider()?;
        let customer_id: Option<String> = sqlx::query_scalar(
            "SELECT provider_customer_id FROM organization_billing_accounts \
             WHERE organization_id=$1 AND provider='stripe'",
        )
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
        let members: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM organization_memberships WHERE organization_id=$1",
        )
        .bind(organization_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
        let quantity = if tier == PlatformBillingTier::Pro
            && self.pro_billing_unit == PlatformBillingUnit::Seat
        {
            u32::try_from(members.max(1)).map_err(|_| ServiceError::InvalidRequest)?
        } else {
            1
        };
        provider
            .create_checkout(&PlatformCheckoutInput {
                organization_id,
                tier,
                billing_email: email.to_owned(),
                existing_customer_id: customer_id,
                quantity,
                idempotency_key: provider_idempotency_key,
            })
            .await
            .map_err(ServiceError::Provider)
    }

    async fn portal(
        &self,
        organization_id: OrganizationId,
        provider_idempotency_key: String,
    ) -> Result<BillingRedirect, ServiceError> {
        let provider = self.provider()?;
        let customer_id: String = sqlx::query_scalar(
            "SELECT provider_customer_id FROM organization_billing_accounts \
             WHERE organization_id=$1 AND provider='stripe'",
        )
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ServiceError::Unavailable)?
        .ok_or(ServiceError::CustomerMissing)?;
        provider
            .create_portal(&PlatformPortalInput {
                organization_id,
                customer_id,
                idempotency_key: provider_idempotency_key,
            })
            .await
            .map_err(ServiceError::Provider)
    }

    async fn apply_webhook(
        &self,
        payload: &[u8],
        signature: &str,
        now_seconds: i64,
    ) -> Result<WebhookOutcome, ServiceError> {
        let provider = self.provider()?;
        let event = provider
            .verify_webhook(payload, signature, now_seconds)
            .map_err(ServiceError::Provider)?;
        persist_event(&self.pool, payload, &event, self.pro_billing_unit).await
    }
}

pub(crate) async fn status(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    match management
        .billing
        .summary(organization_id, identity.user_id)
        .await
    {
        Ok(summary) => Json(summary).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn checkout(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreatePlatformCheckoutRequest>,
) -> Response {
    redirect_operation(
        &state,
        &organization,
        request_id,
        &headers,
        RedirectOperation::Checkout(payload.tier),
    )
    .await
}

pub(crate) async fn portal(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    redirect_operation(
        &state,
        &organization,
        request_id,
        &headers,
        RedirectOperation::Portal,
    )
    .await
}

pub(crate) async fn invoices(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    if let Err(error) = ensure_membership(&management.pool, organization_id, identity.user_id).await
    {
        return service_error(error, request_id);
    }
    let rows = sqlx::query(
        "SELECT provider_invoice_id,status,currency,amount_due_cents,amount_paid_cents, \
                (extract(epoch FROM period_start)*1000)::bigint period_start_ms, \
                (extract(epoch FROM period_end)*1000)::bigint period_end_ms, \
                hosted_invoice_url,invoice_pdf_url, \
                (extract(epoch FROM provider_created_at)*1000)::bigint created_at_ms \
         FROM organization_billing_invoices WHERE organization_id=$1 \
         ORDER BY provider_created_at DESC,provider_invoice_id DESC LIMIT 100",
    )
    .bind(organization_id.0)
    .fetch_all(&management.pool)
    .await;
    let rows = match rows {
        Ok(value) => value,
        Err(_) => return service_error(ServiceError::Unavailable, request_id),
    };
    let result = rows
        .into_iter()
        .map(|row| {
            let status: String = row
                .try_get("status")
                .map_err(|_| ServiceError::Unavailable)?;
            Ok(PlatformInvoiceSummary {
                id: row
                    .try_get("provider_invoice_id")
                    .map_err(|_| ServiceError::Unavailable)?,
                organization_id,
                status: parse_invoice_status(&status)?,
                currency: row
                    .try_get("currency")
                    .map_err(|_| ServiceError::Unavailable)?,
                amount_due_minor: positive_or_zero_u64(&row, "amount_due_cents")?,
                amount_paid_minor: positive_or_zero_u64(&row, "amount_paid_cents")?,
                period_start_ms: row
                    .try_get("period_start_ms")
                    .map_err(|_| ServiceError::Unavailable)?,
                period_end_ms: row
                    .try_get("period_end_ms")
                    .map_err(|_| ServiceError::Unavailable)?,
                hosted_invoice_url: row
                    .try_get("hosted_invoice_url")
                    .map_err(|_| ServiceError::Unavailable)?,
                invoice_pdf_url: row
                    .try_get("invoice_pdf_url")
                    .map_err(|_| ServiceError::Unavailable)?,
                created_at_ms: row
                    .try_get("created_at_ms")
                    .map_err(|_| ServiceError::Unavailable)?,
            })
        })
        .collect::<Result<Vec<_>, ServiceError>>();
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn usage(
    State(state): State<ApiState>,
    Path(organization): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(&organization) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    if let Err(error) = ensure_membership(&management.pool, organization_id, identity.user_id).await
    {
        return service_error(error, request_id);
    }
    let Some(metering) = &state.usage_metering else {
        return super::metering_error(super::metering::MeteringError::Unavailable, request_id)
            .into_response();
    };
    match metering
        .organization_summary(organization_id, now_ms())
        .await
    {
        Ok(summary) => Json(summary).into_response(),
        Err(error) => super::metering_error(error, request_id).into_response(),
    }
}

pub(crate) async fn stripe_webhook(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(management) = state.management.as_ref() else {
        return service_error(ServiceError::Unavailable, request_id);
    };
    let Some(signature) = headers
        .get("stripe-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return service_error(ServiceError::InvalidSignature, request_id);
    };
    match management
        .billing
        .apply_webhook(&body, signature, now_ms() / 1_000)
        .await
    {
        Ok(WebhookOutcome::Processed) => Json(json!({"received": true})).into_response(),
        Ok(WebhookOutcome::Duplicate) => {
            Json(json!({"received": true, "duplicate": true})).into_response()
        }
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn project_payments(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let project_id = match Uuid::parse_str(&project) {
        Ok(value) => ProjectId(value),
        Err(_) => return service_error(ServiceError::InvalidRequest, request_id),
    };
    let row = sqlx::query(
        "SELECT p.organization_id,c.provider,c.status,c.capabilities \
         FROM projects p JOIN organization_memberships m \
           ON m.organization_id=p.organization_id AND m.user_id=$2 \
         LEFT JOIN project_commerce_accounts c ON c.project_id=p.id \
         WHERE p.id=$1 AND p.lifecycle_state <> 'deleted' \
         ORDER BY c.provider LIMIT 1",
    )
    .bind(project_id.0)
    .bind(identity.user_id.0)
    .fetch_optional(&management.pool)
    .await;
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => return service_error(ServiceError::Forbidden, request_id),
        Err(_) => return service_error(ServiceError::Unavailable, request_id),
    };
    let organization_id = match row.try_get::<Uuid, _>("organization_id") {
        Ok(value) => OrganizationId(value),
        Err(_) => return service_error(ServiceError::Unavailable, request_id),
    };
    let provider: Option<String> = match row.try_get("provider") {
        Ok(value) => value,
        Err(_) => return service_error(ServiceError::Unavailable, request_id),
    };
    let status = match row.try_get::<Option<String>, _>("status") {
        Ok(Some(value)) if value == "enabled" => ProjectPaymentsStatus::Enabled,
        Ok(Some(value))
            if matches!(
                value.as_str(),
                "configuring" | "onboarding" | "restricted" | "disconnected"
            ) =>
        {
            ProjectPaymentsStatus::Restricted
        }
        Ok(None) => ProjectPaymentsStatus::NotConfigured,
        _ => return service_error(ServiceError::Unavailable, request_id),
    };
    let capabilities: Vec<String> = match row.try_get::<Option<Vec<String>>, _>("capabilities") {
        Ok(Some(value)) => value,
        Ok(None) => Vec::new(),
        Err(_) => return service_error(ServiceError::Unavailable, request_id),
    };
    Json(ProjectPaymentsSummary {
        project_id,
        organization_id,
        status,
        provider,
        capabilities: ProjectPaymentCapabilities {
            checkout_sessions: capabilities
                .iter()
                .any(|value| matches!(value.as_str(), "one_time_payments" | "recurring_payments")),
            recurring_billing: capabilities
                .iter()
                .any(|value| value == "recurring_payments"),
            customer_portal: capabilities.iter().any(|value| value == "customer_portal"),
            webhooks: status != ProjectPaymentsStatus::NotConfigured,
        },
    })
    .into_response()
}

#[derive(Clone, Copy)]
enum RedirectOperation {
    Checkout(PlatformBillingTier),
    Portal,
}

impl RedirectOperation {
    const fn name(self) -> &'static str {
        match self {
            Self::Checkout(_) => "billing.checkout",
            Self::Portal => "billing.portal",
        }
    }
}

async fn redirect_operation(
    state: &ApiState,
    organization: &str,
    request_id: RequestId,
    headers: &HeaderMap,
    operation: RedirectOperation,
) -> Response {
    let (management, identity) = match authenticated(state, headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let organization_id = match parse_organization(organization) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    if let Err(error) =
        authorized_organization_admin(&management, identity.user_id, organization_id, request_id)
            .await
    {
        return error.into_response();
    }
    if management.billing.provider.is_none() {
        return service_error(ServiceError::ProviderUnavailable, request_id);
    }
    if let Err(response) = require_management_audit(
        state,
        Some(organization_id),
        None,
        Some(identity.user_id),
        request_id,
        operation.name(),
        "organization_billing",
        Some(organization_id.0),
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
        "organization_id": organization_id,
        "operation": operation.name(),
        "tier": match operation { RedirectOperation::Checkout(tier) => Some(tier), RedirectOperation::Portal => None },
    })) {
        Ok(value) => value,
        Err(error) => return super::idempotency_error(error, request_id),
    };
    let claim = match super::idempotency::admit(
        &management.pool,
        super::idempotency::Scope::Organization(organization_id),
        operation.name(),
        key,
        request_hash,
    )
    .await
    {
        Ok(super::idempotency::Admission::Owner(claim)) => claim,
        Ok(super::idempotency::Admission::Replay { status, body }) => {
            return (status, Json(body)).into_response();
        }
        Ok(super::idempotency::Admission::Conflict) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                "idempotency.request_conflict",
                "the idempotency key was already used for a different request",
                request_id,
            )
            .into_response();
        }
        Ok(super::idempotency::Admission::InProgress) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                "idempotency.in_progress",
                "an operation with this idempotency key is still in progress",
                request_id,
            )
            .into_response();
        }
        Err(error) => return super::idempotency_error(error, request_id),
    };
    let heartbeat =
        super::idempotency::LeaseHeartbeat::start(management.pool.clone(), claim.clone());
    let provider_key = provider_idempotency_key(operation.name(), organization_id, key);
    let result = match operation {
        RedirectOperation::Checkout(tier) => {
            management
                .billing
                .checkout(
                    organization_id,
                    tier,
                    &identity.normalized_email,
                    provider_key,
                )
                .await
        }
        RedirectOperation::Portal => {
            management
                .billing
                .portal(organization_id, provider_key)
                .await
        }
    };
    let redirect = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = super::idempotency::abandon(&management.pool, &claim).await;
            terminal_management_audit(
                state,
                Some(organization_id),
                None,
                Some(identity.user_id),
                request_id,
                operation.name(),
                "organization_billing",
                Some(organization_id.0),
                AuditOutcome::Failure,
            )
            .await;
            return service_error(error, request_id);
        }
    };
    if !super::idempotency::confirm_owner(&management.pool, &claim, &heartbeat).await {
        return service_error(ServiceError::Unavailable, request_id);
    }
    let body = serde_json::to_value(&redirect).unwrap_or(Value::Null);
    if super::idempotency::complete(&management.pool, &claim, StatusCode::CREATED, &body)
        .await
        .is_err()
    {
        return service_error(ServiceError::Unavailable, request_id);
    }
    terminal_management_audit(
        state,
        Some(organization_id),
        None,
        Some(identity.user_id),
        request_id,
        operation.name(),
        "organization_billing",
        Some(organization_id.0),
        AuditOutcome::Success,
    )
    .await;
    (StatusCode::CREATED, Json(redirect)).into_response()
}

async fn persist_event(
    pool: &PgPool,
    payload: &[u8],
    event: &VerifiedBillingEvent,
    pro_billing_unit: PlatformBillingUnit,
) -> Result<WebhookOutcome, ServiceError> {
    let payload_hash: [u8; 32] = Sha256::digest(payload).into();
    let mut transaction = pool.begin().await.map_err(|_| ServiceError::Unavailable)?;
    let inserted = sqlx::query(
        "INSERT INTO billing_webhook_events \
         (provider,provider_event_id,event_type,livemode,payload_sha256,organization_id,provider_created_at) \
         VALUES ('stripe',$1,$2,$3,$4,$5,to_timestamp($6::double precision/1000)) \
         ON CONFLICT (provider,provider_event_id) DO NOTHING",
    )
    .bind(&event.provider_event_id)
    .bind(&event.event_type)
    .bind(event.livemode)
    .bind(payload_hash.as_slice())
    .bind(
        event
            .platform_update
            .as_ref()
            .map(|update| update.organization_id.0)
            .or_else(|| {
                event
                    .invoice_update
                    .as_ref()
                    .map(|update| update.organization_id.0)
            }),
    )
    .bind(event.created_at_ms)
    .execute(&mut *transaction)
    .await
    .map_err(|_| ServiceError::Unavailable)?;
    if inserted.rows_affected() == 0 {
        let existing: Vec<u8> = sqlx::query_scalar(
            "SELECT payload_sha256 FROM billing_webhook_events \
             WHERE provider='stripe' AND provider_event_id=$1",
        )
        .bind(&event.provider_event_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
        transaction
            .rollback()
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        return if existing.as_slice() == payload_hash {
            Ok(WebhookOutcome::Duplicate)
        } else {
            Err(ServiceError::ReplayConflict)
        };
    }
    if let Some(update) = &event.platform_update {
        apply_update(&mut transaction, event, update, pro_billing_unit).await?;
    }
    if let Some(update) = &event.invoice_update {
        apply_invoice_update(&mut transaction, event, update).await?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    Ok(WebhookOutcome::Processed)
}

async fn apply_invoice_update(
    transaction: &mut Transaction<'_, Postgres>,
    event: &VerifiedBillingEvent,
    update: &PlatformInvoiceUpdate,
) -> Result<(), ServiceError> {
    let account = sqlx::query(
        "SELECT provider_customer_id,provider_subscription_id FROM organization_billing_accounts \
         WHERE organization_id=$1 AND provider='stripe' FOR UPDATE",
    )
    .bind(update.organization_id.0)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| ServiceError::Unavailable)?
    .ok_or(ServiceError::InvalidEvent)?;
    let customer_id: String = account
        .try_get("provider_customer_id")
        .map_err(|_| ServiceError::Unavailable)?;
    let subscription_id: Option<String> = account
        .try_get("provider_subscription_id")
        .map_err(|_| ServiceError::Unavailable)?;
    if customer_id != update.customer_id
        || subscription_id.as_ref().is_some_and(|expected| {
            update
                .subscription_id
                .as_ref()
                .is_none_or(|actual| actual != expected)
        })
    {
        return Err(ServiceError::InvalidEvent);
    }
    let applied = sqlx::query(
        "INSERT INTO organization_billing_invoices \
         (organization_id,provider,provider_invoice_id,provider_subscription_id,status,currency, \
          amount_due_cents,amount_paid_cents,period_start,period_end,hosted_invoice_url,invoice_pdf_url, \
          provider_created_at,last_provider_event_created_at,last_provider_event_id) \
         VALUES ($1,'stripe',$2,$3,$4,$5,$6,$7,to_timestamp($8::double precision/1000), \
                 to_timestamp($9::double precision/1000),$10,$11,to_timestamp($12::double precision/1000), \
                 to_timestamp($12::double precision/1000),$13) \
         ON CONFLICT (provider,provider_invoice_id) DO UPDATE SET \
           provider_subscription_id=COALESCE(EXCLUDED.provider_subscription_id,organization_billing_invoices.provider_subscription_id), \
           status=EXCLUDED.status,currency=EXCLUDED.currency,amount_due_cents=EXCLUDED.amount_due_cents, \
           amount_paid_cents=EXCLUDED.amount_paid_cents,period_start=EXCLUDED.period_start,period_end=EXCLUDED.period_end, \
           hosted_invoice_url=COALESCE(EXCLUDED.hosted_invoice_url,organization_billing_invoices.hosted_invoice_url), \
           invoice_pdf_url=COALESCE(EXCLUDED.invoice_pdf_url,organization_billing_invoices.invoice_pdf_url), \
           last_provider_event_created_at=EXCLUDED.last_provider_event_created_at, \
           last_provider_event_id=EXCLUDED.last_provider_event_id,updated_at=now() \
         WHERE organization_billing_invoices.organization_id=EXCLUDED.organization_id AND \
           (EXCLUDED.last_provider_event_created_at,EXCLUDED.last_provider_event_id) >= \
           (organization_billing_invoices.last_provider_event_created_at,organization_billing_invoices.last_provider_event_id)",
    )
    .bind(update.organization_id.0)
    .bind(&update.invoice_id)
    .bind(&update.subscription_id)
    .bind(invoice_status_name(update.status))
    .bind(&update.currency)
    .bind(i64::try_from(update.amount_due_minor).map_err(|_| ServiceError::InvalidEvent)?)
    .bind(i64::try_from(update.amount_paid_minor).map_err(|_| ServiceError::InvalidEvent)?)
    .bind(update.period_start_ms)
    .bind(update.period_end_ms)
    .bind(&update.hosted_invoice_url)
    .bind(&update.invoice_pdf_url)
    .bind(event.created_at_ms)
    .bind(&event.provider_event_id)
    .execute(&mut **transaction)
    .await
    .map_err(|_| ServiceError::Unavailable)?;
    if applied.rows_affected() == 0 {
        let existing_organization: Option<Uuid> = sqlx::query_scalar(
            "SELECT organization_id FROM organization_billing_invoices \
             WHERE provider='stripe' AND provider_invoice_id=$1",
        )
        .bind(&update.invoice_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
        if existing_organization != Some(update.organization_id.0) {
            return Err(ServiceError::InvalidEvent);
        }
    }
    Ok(())
}

async fn apply_update(
    transaction: &mut Transaction<'_, Postgres>,
    event: &VerifiedBillingEvent,
    update: &PlatformBillingUpdate,
    pro_billing_unit: PlatformBillingUnit,
) -> Result<(), ServiceError> {
    let existing = sqlx::query(
        "SELECT provider_customer_id,provider_subscription_id,status, \
                (extract(epoch FROM last_provider_event_created_at)*1000)::bigint last_event_ms, \
                last_provider_event_id FROM organization_billing_accounts \
         WHERE organization_id=$1 FOR UPDATE",
    )
    .bind(update.organization_id.0)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| ServiceError::Unavailable)?;
    if let Some(row) = existing {
        let customer_id: String = row
            .try_get("provider_customer_id")
            .map_err(|_| ServiceError::Unavailable)?;
        let subscription_id: Option<String> = row
            .try_get("provider_subscription_id")
            .map_err(|_| ServiceError::Unavailable)?;
        let existing_status: String = row
            .try_get("status")
            .map_err(|_| ServiceError::Unavailable)?;
        let last_event_ms: i64 = row
            .try_get("last_event_ms")
            .map_err(|_| ServiceError::Unavailable)?;
        let last_event_id: String = row
            .try_get("last_provider_event_id")
            .map_err(|_| ServiceError::Unavailable)?;
        if customer_id != update.customer_id
            || subscription_id.as_ref().is_some_and(|existing| {
                update
                    .subscription_id
                    .as_ref()
                    .is_some_and(|incoming| incoming != existing)
                    && existing_status != "canceled"
            })
        {
            // A provider customer/subscription is permanently bound to one
            // FFDB organization. Never let signed but mis-scoped metadata move
            // an entitlement across tenants or silently orphan a subscription.
            return Err(ServiceError::InvalidEvent);
        }
        if (event.created_at_ms, event.provider_event_id.as_str())
            < (last_event_ms, last_event_id.as_str())
        {
            // Stripe does not guarantee event delivery order. Record the event
            // for replay safety but do not regress newer entitlement state.
            return Ok(());
        }
    }
    let billing_unit = if update.tier == PlatformBillingTier::Pro {
        pro_billing_unit
    } else {
        PlatformBillingUnit::Organization
    };
    sqlx::query(
        "INSERT INTO organization_billing_accounts \
         (organization_id,provider,provider_customer_id,provider_subscription_id,tier,status, \
          billing_unit,seat_quantity,current_period_start,current_period_end,cancel_at_period_end, \
          last_provider_event_created_at,last_provider_event_id) \
         VALUES ($1,'stripe',$2,$3,$4,$5,$6,$7,to_timestamp($8::double precision/1000), \
                 to_timestamp($9::double precision/1000),$10,to_timestamp($11::double precision/1000),$12) \
         ON CONFLICT (organization_id) DO UPDATE SET \
           provider_customer_id=EXCLUDED.provider_customer_id, \
           provider_subscription_id=COALESCE(EXCLUDED.provider_subscription_id,organization_billing_accounts.provider_subscription_id), \
           tier=EXCLUDED.tier,status=EXCLUDED.status,billing_unit=EXCLUDED.billing_unit, \
           seat_quantity=EXCLUDED.seat_quantity,current_period_start=EXCLUDED.current_period_start, \
           current_period_end=EXCLUDED.current_period_end, \
           cancel_at_period_end=EXCLUDED.cancel_at_period_end, \
           last_provider_event_created_at=EXCLUDED.last_provider_event_created_at, \
           last_provider_event_id=EXCLUDED.last_provider_event_id,updated_at=now() \
         WHERE (EXCLUDED.last_provider_event_created_at,EXCLUDED.last_provider_event_id) >= \
               (organization_billing_accounts.last_provider_event_created_at,organization_billing_accounts.last_provider_event_id)",
    )
    .bind(update.organization_id.0)
    .bind(&update.customer_id)
    .bind(&update.subscription_id)
    .bind(tier_name(update.tier))
    .bind(status_name(update.status))
    .bind(billing_unit_name(billing_unit))
    .bind(i32::try_from(update.quantity).map_err(|_| ServiceError::InvalidEvent)?)
    .bind(update.current_period_start_ms)
    .bind(update.current_period_end_ms)
    .bind(update.cancel_at_period_end)
    .bind(event.created_at_ms)
    .bind(&event.provider_event_id)
    .execute(&mut **transaction)
    .await
    .map_err(|_| ServiceError::Unavailable)?;
    Ok(())
}

async fn ensure_membership(
    pool: &PgPool,
    organization_id: OrganizationId,
    actor: UserId,
) -> Result<(), ServiceError> {
    let permitted: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM organization_memberships m JOIN organizations o \
         ON o.id=m.organization_id WHERE m.organization_id=$1 AND m.user_id=$2 \
         AND o.disabled_at IS NULL)",
    )
    .bind(organization_id.0)
    .bind(actor.0)
    .fetch_one(pool)
    .await
    .map_err(|_| ServiceError::Unavailable)?;
    if permitted {
        Ok(())
    } else {
        Err(ServiceError::Forbidden)
    }
}

fn positive_u64(row: &sqlx::postgres::PgRow, name: &str) -> Result<u64, ServiceError> {
    let value: i64 = row.try_get(name).map_err(|_| ServiceError::Unavailable)?;
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ServiceError::Unavailable)
}

fn positive_or_zero_u64(row: &sqlx::postgres::PgRow, name: &str) -> Result<u64, ServiceError> {
    let value: i64 = row.try_get(name).map_err(|_| ServiceError::Unavailable)?;
    u64::try_from(value).map_err(|_| ServiceError::Unavailable)
}

fn parse_organization(value: &str) -> Result<OrganizationId, ServiceError> {
    Uuid::parse_str(value)
        .map(OrganizationId)
        .map_err(|_| ServiceError::InvalidRequest)
}

fn provider_idempotency_key(
    operation: &str,
    organization_id: OrganizationId,
    caller_key: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.stripe.idempotency.v1\0");
    digest.update(operation.as_bytes());
    digest.update(organization_id.0.as_bytes());
    digest.update(caller_key.as_bytes());
    format!("ffdb-{operation}-{:x}", digest.finalize())
}

const fn tier_name(value: PlatformBillingTier) -> &'static str {
    match value {
        PlatformBillingTier::Free => "free",
        PlatformBillingTier::PayAsYouGo => "pay_as_you_go",
        PlatformBillingTier::Pro => "pro",
    }
}

fn parse_tier(value: &str) -> Result<PlatformBillingTier, ServiceError> {
    match value {
        "free" => Ok(PlatformBillingTier::Free),
        "pay_as_you_go" => Ok(PlatformBillingTier::PayAsYouGo),
        "pro" => Ok(PlatformBillingTier::Pro),
        _ => Err(ServiceError::Unavailable),
    }
}

const fn status_name(value: ProviderSubscriptionStatus) -> &'static str {
    match value {
        ProviderSubscriptionStatus::CheckoutPending => "checkout_pending",
        ProviderSubscriptionStatus::Trialing => "trialing",
        ProviderSubscriptionStatus::Active => "active",
        ProviderSubscriptionStatus::PastDue => "past_due",
        ProviderSubscriptionStatus::Unpaid => "unpaid",
        ProviderSubscriptionStatus::Canceled => "canceled",
        ProviderSubscriptionStatus::Paused => "paused",
        ProviderSubscriptionStatus::Incomplete => "incomplete",
    }
}

const fn invoice_status_name(value: ProviderInvoiceStatus) -> &'static str {
    match value {
        ProviderInvoiceStatus::Draft => "draft",
        ProviderInvoiceStatus::Open => "open",
        ProviderInvoiceStatus::Paid => "paid",
        ProviderInvoiceStatus::Uncollectible => "uncollectible",
        ProviderInvoiceStatus::Void => "void",
        ProviderInvoiceStatus::PaymentFailed => "payment_failed",
    }
}

fn parse_status(value: &str) -> Result<PlatformBillingStatus, ServiceError> {
    match value {
        "free" => Ok(PlatformBillingStatus::Free),
        "checkout_pending" => Ok(PlatformBillingStatus::CheckoutPending),
        "trialing" => Ok(PlatformBillingStatus::Trialing),
        "active" => Ok(PlatformBillingStatus::Active),
        "past_due" => Ok(PlatformBillingStatus::PastDue),
        "unpaid" => Ok(PlatformBillingStatus::Unpaid),
        "canceled" => Ok(PlatformBillingStatus::Canceled),
        "paused" => Ok(PlatformBillingStatus::Paused),
        "incomplete" => Ok(PlatformBillingStatus::Incomplete),
        _ => Err(ServiceError::Unavailable),
    }
}

fn parse_invoice_status(value: &str) -> Result<PlatformInvoiceStatus, ServiceError> {
    match value {
        "draft" => Ok(PlatformInvoiceStatus::Draft),
        "open" => Ok(PlatformInvoiceStatus::Open),
        "paid" => Ok(PlatformInvoiceStatus::Paid),
        "uncollectible" => Ok(PlatformInvoiceStatus::Uncollectible),
        "void" => Ok(PlatformInvoiceStatus::Void),
        "payment_failed" => Ok(PlatformInvoiceStatus::PaymentFailed),
        _ => Err(ServiceError::Unavailable),
    }
}

const fn billing_unit_name(value: PlatformBillingUnit) -> &'static str {
    match value {
        PlatformBillingUnit::Organization => "organization",
        PlatformBillingUnit::Seat => "seat",
    }
}

fn parse_billing_unit(value: &str) -> Result<PlatformBillingUnit, ServiceError> {
    match value {
        "organization" => Ok(PlatformBillingUnit::Organization),
        "seat" => Ok(PlatformBillingUnit::Seat),
        _ => Err(ServiceError::Unavailable),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WebhookOutcome {
    Processed,
    Duplicate,
}

#[derive(Debug)]
enum ServiceError {
    InvalidRequest,
    InvalidSignature,
    InvalidEvent,
    Forbidden,
    CustomerMissing,
    ReplayConflict,
    ProviderUnavailable,
    Unavailable,
    Provider(BillingError),
}

fn service_error(error: ServiceError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        ServiceError::InvalidRequest | ServiceError::InvalidEvent => (
            StatusCode::BAD_REQUEST,
            "billing.invalid_request",
            "billing request is invalid",
        ),
        ServiceError::InvalidSignature
        | ServiceError::Provider(BillingError::InvalidWebhookSignature) => (
            StatusCode::BAD_REQUEST,
            "billing.invalid_webhook_signature",
            "billing webhook signature is invalid",
        ),
        ServiceError::Provider(BillingError::InvalidWebhookPayload) => (
            StatusCode::BAD_REQUEST,
            "billing.invalid_webhook_payload",
            "billing webhook payload is invalid",
        ),
        ServiceError::Forbidden => (
            StatusCode::FORBIDDEN,
            "billing.forbidden",
            "billing operation is not permitted",
        ),
        ServiceError::CustomerMissing => (
            StatusCode::CONFLICT,
            "billing.customer_missing",
            "complete billing checkout before opening the customer portal",
        ),
        ServiceError::ReplayConflict => (
            StatusCode::CONFLICT,
            "billing.webhook_replay_conflict",
            "billing webhook identifier was reused with different content",
        ),
        ServiceError::ProviderUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "billing.provider_unavailable",
            "billing provider is not configured",
        ),
        ServiceError::Provider(BillingError::ProviderRejected) => (
            StatusCode::BAD_GATEWAY,
            "billing.provider_rejected",
            "billing provider rejected the request",
        ),
        ServiceError::Provider(_) | ServiceError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "billing.unavailable",
            "billing service is unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}
