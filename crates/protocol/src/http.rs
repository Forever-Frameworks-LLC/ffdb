//! Public HTTP payloads that are not direct worker operations.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zeroize::Zeroize;

use crate::{
    ApiKeyId, BackupId, CommerceCustomerId, CommerceOrderId, CommercePaymentId, CommercePriceId,
    CommerceProductId, CommerceRefundId, CommerceSubscriptionId, OrganizationId, ProjectId,
    SessionId, UserId,
};

/// A wire string whose debug output is always redacted.
#[derive(Clone, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SensitiveString(String);

impl SensitiveString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl Drop for SensitiveString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SensitiveString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveString([REDACTED])")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CreateOrganizationRequest {
    pub name: String,
    pub slug: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeveloperBootstrapRequest {
    pub email: String,
    pub password: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeveloperSignInRequest {
    pub email: String,
    pub password: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeveloperRefreshRequest {
    pub session_token: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeveloperSessionResponse {
    /// Opaque rotating bearer credential. Returned only at issue/refresh time.
    pub session_token: SensitiveString,
    pub user_id: UserId,
    pub email: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct OrganizationSummary {
    pub id: OrganizationId,
    pub name: String,
    pub slug: String,
    pub role: OrganizationRole,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationRole {
    Owner,
    Admin,
    Developer,
    Viewer,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct OrganizationMembershipSummary {
    pub organization_id: OrganizationId,
    pub user_id: UserId,
    pub email: String,
    pub role: OrganizationRole,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AddOrganizationMemberRequest {
    pub email: String,
    pub role: OrganizationRole,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateOrganizationMemberRequest {
    pub role: OrganizationRole,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateOrganizationInvitationRequest {
    pub email: String,
    pub role: OrganizationRole,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptOrganizationInvitationRequest {
    pub invitation_token: SensitiveString,
    pub password: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CreateProjectRequest {
    pub organization_id: OrganizationId,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLifecycleState {
    Provisioning,
    Active,
    Suspended,
    Restoring,
    Deleting,
    Deleted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub organization_id: OrganizationId,
    pub name: String,
    pub slug: String,
    pub region: String,
    pub state: ProjectLifecycleState,
    pub schema_version: u64,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformBillingTier {
    Free,
    PayAsYouGo,
    Pro,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformBillingStatus {
    Free,
    CheckoutPending,
    Trialing,
    Active,
    PastDue,
    Unpaid,
    Canceled,
    Paused,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformBillingUnit {
    Organization,
    Seat,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PlatformUsageAllowance {
    pub storage_bytes: u64,
    pub monthly_reads: u64,
    pub monthly_writes: u64,
    pub monthly_active_users: u64,
    pub overage_enabled: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageReportingStatus {
    Healthy,
    Degraded,
    Reconciling,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PlatformUsageSummary {
    pub organization_id: OrganizationId,
    pub period_start_ms: i64,
    pub period_end_ms: i64,
    pub reads: u64,
    pub writes: u64,
    pub storage_bytes: u64,
    pub storage_byte_hours: u64,
    pub monthly_active_users: u64,
    pub reporting_status: UsageReportingStatus,
    pub reporting_last_success_ms: Option<i64>,
    pub as_of_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PlatformBillingSummary {
    pub organization_id: OrganizationId,
    pub tier: PlatformBillingTier,
    pub status: PlatformBillingStatus,
    pub billing_unit: PlatformBillingUnit,
    pub seat_quantity: u32,
    pub project_limit: Option<u32>,
    pub usage_allowance: PlatformUsageAllowance,
    pub current_period_start_ms: Option<i64>,
    pub current_period_end_ms: Option<i64>,
    pub cancel_at_period_end: bool,
    pub provider_configured: bool,
    pub billing_enforcement_enabled: bool,
    pub billing_exempt: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePlatformCheckoutRequest {
    pub tier: PlatformBillingTier,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct BillingRedirect {
    pub url: String,
}

/// Deployment shape selected by the owner during first-run setup.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceDeploymentMode {
    Unconfigured,
    Private,
    Team,
    PlatformByo,
    PlatformConnect,
}

/// Controls who may create organizations on this installation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationCreationPolicy {
    OwnerOnly,
    Authenticated,
    InvitationOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceAdministratorRole {
    Owner,
    Admin,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceBillingMode {
    ByoKeys,
    StripeConnect,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceBillingAccountStatus {
    Pending,
    Onboarding,
    Enabled,
    Restricted,
    Disconnected,
}

/// Deliberately contains no user, provider, or credential details.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PublicInstanceSetupStatus {
    /// True only before the first platform user has been bootstrapped.
    pub bootstrap_available: bool,
    /// True after bootstrap and before the owner has selected a deployment mode.
    pub setup_required: bool,
    /// This runtime can provision the FFDB catalog into a supplied Stripe account.
    pub platform_byo_available: bool,
    /// This runtime can create and onboard an Accounts v2 Connect account using
    /// owner-supplied platform credentials.
    pub platform_connect_available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceBillingAccountSummary {
    pub mode: InstanceBillingMode,
    pub status: InstanceBillingAccountStatus,
    pub provider_account_id: Option<String>,
    pub charges_enabled: bool,
    pub payouts_enabled: bool,
    pub details_submitted: bool,
    pub capabilities: Vec<String>,
    pub credentials_configured: bool,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceStatus {
    pub owner_user_id: UserId,
    pub current_user_role: InstanceAdministratorRole,
    pub deployment_mode: InstanceDeploymentMode,
    pub organization_creation_policy: OrganizationCreationPolicy,
    pub billing_enforcement_enabled: bool,
    pub setup_completed_at_ms: Option<i64>,
    pub billing_account: Option<InstanceBillingAccountSummary>,
    pub administrator_count: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Complete first-run setup. The tagged shape prevents credentials intended
/// for one deployment mode from being silently accepted for another.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "deployment_mode",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CompleteInstanceSetupRequest {
    Private {
        organization_creation_policy: OrganizationCreationPolicy,
    },
    Team {
        organization_creation_policy: OrganizationCreationPolicy,
    },
    PlatformByo {
        organization_creation_policy: OrganizationCreationPolicy,
        secret_key: SensitiveString,
        webhook_secret: SensitiveString,
    },
    PlatformConnect {
        organization_creation_policy: OrganizationCreationPolicy,
        secret_key: SensitiveString,
        webhook_secret: SensitiveString,
        country: String,
        email: String,
        return_url: String,
        refresh_url: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceBillingOnboarding {
    pub url: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CompleteInstanceSetupResponse {
    pub instance: InstanceStatus,
    pub onboarding: Option<InstanceBillingOnboarding>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateOrganizationCreationPolicyRequest {
    pub organization_creation_policy: OrganizationCreationPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceAdministratorSummary {
    pub user_id: UserId,
    pub email: String,
    pub role: InstanceAdministratorRole,
    pub granted_by: Option<UserId>,
    pub created_at_ms: i64,
}

/// Release channel accepted by the host updater. Additional channels must be
/// introduced as protocol variants rather than forwarded as unchecked text to
/// the privileged updater.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostUpdateChannel {
    Stable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostUpdateOperation {
    Check,
    Install,
    Rollback,
    Configure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostUpdateJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostUpdateVersionRequest {
    /// An exact signed FFDB release version, without a leading `v`.
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostUpdateSettings {
    pub channel: HostUpdateChannel,
    pub automatic_checks: bool,
    pub check_interval_hours: u16,
    pub automatic_apply: bool,
    /// Recurring UTC start in zero-padded `HH:MM` form.
    pub maintenance_window_start: Option<String>,
    pub maintenance_window_duration_minutes: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HostUpdateRelease {
    pub version: String,
    pub active: bool,
    pub rollback_compatible: bool,
    #[serde(default)]
    pub state_schema: u32,
    #[serde(default)]
    pub minimum_rollback_version: Option<String>,
    #[serde(default)]
    pub signature_verified: bool,
    #[serde(default)]
    pub signature_identity: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HostUpdateCapabilities {
    pub check: bool,
    pub install: bool,
    pub rollback: bool,
    pub automatic_checks: bool,
    pub automatic_apply: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HostUpdateJob {
    pub job_id: String,
    pub operation: HostUpdateOperation,
    pub requested_version: Option<String>,
    pub state: HostUpdateJobState,
    pub phase: String,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub previous_version: Option<String>,
    pub backup_path: Option<String>,
    pub message: String,
    pub error_code: Option<String>,
    pub retryable: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HostUpdateStatus {
    pub supported: bool,
    pub unavailable_reason: Option<String>,
    pub capabilities: HostUpdateCapabilities,
    pub state_schema: u32,
    pub minimum_rollback_version: Option<String>,
    pub signature_identity: Option<String>,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub update_available: bool,
    pub last_check_at_ms: Option<i64>,
    pub active_job: Option<HostUpdateJob>,
    pub releases: Vec<HostUpdateRelease>,
    pub settings: HostUpdateSettings,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrantInstanceAdministratorRequest {
    pub user_id: UserId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceOrganizationSummary {
    pub id: OrganizationId,
    pub name: String,
    pub slug: String,
    pub disabled: bool,
    pub member_count: u64,
    pub project_count: u64,
    pub billing_exempt: bool,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceOrganizationPage {
    pub organizations: Vec<InstanceOrganizationSummary>,
    pub total: u64,
    pub limit: u32,
    pub offset: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceUserSummary {
    pub id: UserId,
    pub email: String,
    pub email_verified: bool,
    pub disabled: bool,
    pub instance_role: Option<InstanceAdministratorRole>,
    pub organization_count: u64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstanceUserPage {
    pub users: Vec<InstanceUserSummary>,
    pub total: u64,
    pub limit: u32,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateInstanceDisabledRequest {
    pub disabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct OrganizationBillingExemptionSummary {
    pub organization_id: OrganizationId,
    pub organization_name: String,
    pub reason: String,
    pub created_by: UserId,
    pub created_by_email: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrantOrganizationBillingExemptionRequest {
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceReadsAtLimit {
    Continue,
    Overage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceWritesAtLimit {
    Pause,
    Overage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceSignupsAtLimit {
    Pause,
    Overage,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InstancePlanCatalogEntry {
    pub tier: PlatformBillingTier,
    pub display_name: String,
    pub billing_unit: PlatformBillingUnit,
    pub base_price_cents: Option<u64>,
    pub currency: String,
    pub project_limit: Option<u32>,
    pub storage_bytes: u64,
    pub monthly_reads: u64,
    pub monthly_writes: u64,
    pub monthly_active_users: u64,
    pub overage_enabled: bool,
    pub reads_at_limit: InstanceReadsAtLimit,
    pub writes_at_limit: InstanceWritesAtLimit,
    pub signups_at_limit: InstanceSignupsAtLimit,
    pub requires_payment_method_for_overage: bool,
    pub active: bool,
    /// Provider-priced fields are immutable while this account-scoped catalog is active.
    pub provider_catalog_bound: bool,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PutInstancePlanCatalogEntryRequest {
    pub display_name: String,
    pub billing_unit: PlatformBillingUnit,
    pub base_price_cents: Option<u64>,
    pub currency: String,
    pub project_limit: Option<u32>,
    pub storage_bytes: u64,
    pub monthly_reads: u64,
    pub monthly_writes: u64,
    pub monthly_active_users: u64,
    pub overage_enabled: bool,
    pub reads_at_limit: InstanceReadsAtLimit,
    pub writes_at_limit: InstanceWritesAtLimit,
    pub signups_at_limit: InstanceSignupsAtLimit,
    pub requires_payment_method_for_overage: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInstanceConnectOnboardingRequest {
    pub return_url: String,
    pub refresh_url: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformInvoiceStatus {
    Draft,
    Open,
    Paid,
    Uncollectible,
    Void,
    PaymentFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PlatformInvoiceSummary {
    pub id: String,
    pub organization_id: OrganizationId,
    pub status: PlatformInvoiceStatus,
    pub currency: String,
    pub amount_due_minor: u64,
    pub amount_paid_minor: u64,
    pub period_start_ms: Option<i64>,
    pub period_end_ms: Option<i64>,
    pub hosted_invoice_url: Option<String>,
    pub invoice_pdf_url: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPaymentsStatus {
    NotConfigured,
    Enabled,
    Restricted,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProjectPaymentCapabilities {
    pub checkout_sessions: bool,
    pub recurring_billing: bool,
    pub customer_portal: bool,
    pub webhooks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProjectPaymentsSummary {
    pub project_id: ProjectId,
    pub organization_id: OrganizationId,
    pub status: ProjectPaymentsStatus,
    pub provider: Option<String>,
    pub capabilities: ProjectPaymentCapabilities,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceProviderMode {
    BringYourOwnKeys,
    StripeConnect,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceAccountStatus {
    Configuring,
    Onboarding,
    Enabled,
    Restricted,
    Disconnected,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceAccountCapabilities {
    pub one_time_payments: bool,
    pub recurring_payments: bool,
    pub refunds: bool,
    pub customer_portal: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceAccountSummary {
    pub project_id: ProjectId,
    pub mode: CommerceProviderMode,
    pub status: CommerceAccountStatus,
    pub livemode: bool,
    pub provider_account_id: Option<String>,
    pub capabilities: CommerceAccountCapabilities,
    pub requirements_due: Vec<String>,
    pub disabled_reason: Option<String>,
    pub webhook_url: String,
    pub secrets_configured: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureCommerceByoRequest {
    pub secret_key: SensitiveString,
    pub webhook_secret: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCommerceConnectOnboardingRequest {
    pub country: String,
    pub email: String,
    pub return_url: String,
    pub refresh_url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceOnboardingResponse {
    pub account: CommerceAccountSummary,
    pub onboarding_url: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceProductStatus {
    Draft,
    Active,
    Archived,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCommerceProductRequest {
    pub name: String,
    pub description: Option<String>,
    pub tax_code: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceProductSummary {
    pub id: CommerceProductId,
    pub project_id: ProjectId,
    pub name: String,
    pub description: Option<String>,
    pub tax_code: Option<String>,
    pub status: CommerceProductStatus,
    pub metadata: Map<String, Value>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceBillingIntervalUnit {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CommerceEntitlementValue {
    Enabled(bool),
    Quantity(u64),
    Text(String),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommercePriceBilling {
    OneTime,
    Recurring {
        interval: CommerceBillingIntervalUnit,
        interval_count: u16,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCommercePriceRequest {
    pub product_id: CommerceProductId,
    pub lookup_key: Option<String>,
    pub currency: String,
    pub unit_amount_minor: u64,
    pub billing: CommercePriceBilling,
    #[serde(default)]
    pub entitlements: std::collections::BTreeMap<String, CommerceEntitlementValue>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommercePriceSummary {
    pub id: CommercePriceId,
    pub project_id: ProjectId,
    pub product_id: CommerceProductId,
    pub lookup_key: Option<String>,
    pub currency: String,
    pub unit_amount_minor: u64,
    pub billing: CommercePriceBilling,
    pub entitlements: std::collections::BTreeMap<String, CommerceEntitlementValue>,
    pub active: bool,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceMembershipSubjectKind {
    Individual,
    Team,
    Organization,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceMembershipSubject {
    pub kind: CommerceMembershipSubjectKind,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceCheckoutLine {
    pub price_id: CommercePriceId,
    pub quantity: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateOneTimeCommerceCheckoutRequest {
    pub lines: Vec<CommerceCheckoutLine>,
    pub subject: Option<CommerceMembershipSubject>,
    pub customer_email: Option<String>,
    pub client_reference: Option<String>,
    pub success_url: String,
    pub cancel_url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRecurringCommerceCheckoutRequest {
    pub price_id: CommercePriceId,
    pub quantity: u32,
    pub subject: CommerceMembershipSubject,
    pub customer_email: Option<String>,
    pub success_url: String,
    pub cancel_url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCommerceCustomerPortalRequest {
    pub subject: CommerceMembershipSubject,
    pub return_url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceCheckoutResponse {
    pub url: String,
    pub expires_at_ms: i64,
    pub order_id: Option<CommerceOrderId>,
    pub subscription_id: Option<CommerceSubscriptionId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceOrderStatus {
    Pending,
    CheckoutCreated,
    Processing,
    Paid,
    PaymentFailed,
    Canceled,
    PartiallyRefunded,
    Refunded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceFulfillmentStatus {
    Unfulfilled,
    Processing,
    Fulfilled,
    Canceled,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceOrderLineSummary {
    pub product_id: CommerceProductId,
    pub price_id: CommercePriceId,
    pub product_name: String,
    pub currency: String,
    pub unit_amount_minor: u64,
    pub quantity: u32,
    pub line_total_minor: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceOrderSummary {
    pub id: CommerceOrderId,
    pub project_id: ProjectId,
    pub customer_id: Option<CommerceCustomerId>,
    pub client_reference: Option<String>,
    pub status: CommerceOrderStatus,
    pub fulfillment_status: CommerceFulfillmentStatus,
    pub currency: String,
    pub subtotal_minor: u64,
    pub discount_minor: u64,
    pub tax_minor: u64,
    pub shipping_minor: u64,
    pub total_minor: u64,
    pub refunded_minor: u64,
    pub lines: Vec<CommerceOrderLineSummary>,
    pub paid_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommercePaymentStatus {
    RequiresPaymentMethod,
    RequiresAction,
    Processing,
    Authorized,
    Captured,
    PartiallyRefunded,
    Refunded,
    Failed,
    Canceled,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommercePaymentSummary {
    pub id: CommercePaymentId,
    pub project_id: ProjectId,
    pub order_id: Option<CommerceOrderId>,
    pub subscription_id: Option<CommerceSubscriptionId>,
    pub status: CommercePaymentStatus,
    pub currency: String,
    pub authorized_minor: u64,
    pub captured_minor: u64,
    pub refunded_minor: u64,
    pub provider_created_at_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceRefundReason {
    Duplicate,
    Fraudulent,
    RequestedByCustomer,
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCommerceRefundRequest {
    pub payment_id: CommercePaymentId,
    pub amount_minor: Option<u64>,
    pub reason: Option<CommerceRefundReason>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceRefundStatus {
    Pending,
    Succeeded,
    Failed,
    Canceled,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceRefundSummary {
    pub id: CommerceRefundId,
    pub payment_id: CommercePaymentId,
    pub status: CommerceRefundStatus,
    pub amount_minor: u64,
    pub currency: String,
    pub reason: Option<CommerceRefundReason>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceSubscriptionStatus {
    CheckoutPending,
    Trialing,
    Active,
    PastDue,
    Unpaid,
    Paused,
    Canceled,
    Incomplete,
    Expired,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceSubscriptionSummary {
    pub id: CommerceSubscriptionId,
    pub project_id: ProjectId,
    pub customer_id: CommerceCustomerId,
    pub price_id: CommercePriceId,
    pub subject: CommerceMembershipSubject,
    pub quantity: u32,
    pub status: CommerceSubscriptionStatus,
    pub current_period_start_ms: Option<i64>,
    pub current_period_end_ms: Option<i64>,
    pub cancel_at_period_end: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CancelCommerceSubscriptionRequest {
    /// When true, service continues through the paid period. When false,
    /// cancellation takes effect immediately and entitlements are revoked by
    /// the resulting provider webhook.
    pub at_period_end: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommerceEntitlementSummary {
    pub subject: CommerceMembershipSubject,
    pub key: String,
    pub value: CommerceEntitlementValue,
    pub subscription_id: Option<CommerceSubscriptionId>,
    pub order_id: Option<CommerceOrderId>,
    pub valid_from_ms: i64,
    pub valid_until_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCommerceFulfillmentRequest {
    pub status: CommerceFulfillmentStatus,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub scopes: Vec<crate::DeveloperScope>,
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CreatedApiKey {
    pub id: ApiKeyId,
    pub name: String,
    pub prefix: String,
    /// Returned exactly once.
    pub secret: SensitiveString,
    pub scopes: Vec<crate::DeveloperScope>,
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ApiKeySummary {
    pub id: ApiKeyId,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<crate::DeveloperScope>,
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: SensitiveString,
    #[serde(default)]
    pub custom_claims: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RegisterResponse {
    pub user_id: UserId,
    pub verification_required: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct VerifyEmailRequest {
    pub token: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SignInRequest {
    pub email: String,
    pub password: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AuthTokenPair {
    pub access_token: SensitiveString,
    pub refresh_token: SensitiveString,
    pub token_type: String,
    pub expires_in_seconds: u32,
    pub session_id: SessionId,
    pub user: AuthUser,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AuthUser {
    pub id: UserId,
    pub email: String,
    pub email_verified: bool,
    pub disabled: bool,
    pub role: String,
    pub custom_claims: Map<String, Value>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetAuthUserDisabledRequest {
    pub disabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RefreshRequest {
    pub refresh_token: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PasswordResetStartRequest {
    pub email: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PasswordResetCompleteRequest {
    pub token: SensitiveString,
    pub new_password: SensitiveString,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PasswordChangeRequest {
    pub current_password: SensitiveString,
    pub new_password: SensitiveString,
    #[serde(default)]
    pub revoke_other_sessions: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub expires_at_ms: i64,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
    pub current: bool,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTemplateKind {
    Verification,
    PasswordReset,
    EmailChange,
    Invitation,
    MagicLink,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct EmailTemplateVersion {
    pub kind: EmailTemplateKind,
    pub version: u32,
    pub subject_template: String,
    pub source: String,
    pub compiled_html: Option<String>,
    pub plain_text: Option<String>,
    pub allowed_variables: Vec<String>,
    pub compilation_errors: Vec<String>,
    pub last_successful_compilation_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageBucketRequest {
    pub name: String,
    pub public: bool,
    pub max_object_bytes: Option<u64>,
    pub versioning: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageBucket {
    pub id: String,
    pub name: String,
    pub public: bool,
    pub max_object_bytes: u64,
    pub versioning: bool,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SignObjectRequest {
    pub bucket: String,
    pub key: String,
    pub operation: ObjectOperation,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub checksum_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectOperation {
    Upload,
    Download,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SignedObjectRequest {
    pub url: SensitiveString,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub expires_at_ms: i64,
    pub upload_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageObject {
    pub bucket: String,
    pub key: String,
    pub owner_id: Option<UserId>,
    pub size_bytes: u64,
    pub content_type: String,
    pub checksum_sha256: String,
    pub version_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct BackupSummary {
    pub id: BackupId,
    pub project_id: ProjectId,
    pub status: BackupStatus,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub last_restore_test_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MigrationSummary {
    pub id: String,
    pub name: String,
    pub checksum: String,
    pub status: String,
    pub schema_version_before: u64,
    pub schema_version_after: u64,
    pub applied_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AuthSettings {
    pub registration_enabled: bool,
    pub email_verification_required: bool,
    pub access_token_ttl_seconds: u32,
    pub refresh_token_ttl_seconds: u32,
    pub password_min_length: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AuditLogEntry {
    pub id: String,
    pub occurred_at_ms: i64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub outcome: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupStatus {
    Queued,
    Running,
    Complete,
    Failed,
    Restoring,
    RestoreVerified,
}
