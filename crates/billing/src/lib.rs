//! Provider-neutral platform billing and project-commerce boundaries.
//!
//! Platform billing belongs to an FFDB organization. Project commerce belongs
//! to one customer project and never reuses the platform's customer or
//! subscription identifiers. Provider adapters must preserve that boundary.

use async_trait::async_trait;
use ffdb_protocol::{BillingRedirect, OrganizationId, PlatformBillingTier, ProjectId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod stripe;

pub use stripe::{StripeBillingConfig, StripeBillingProvider, StripeUsageMeterConfig};

pub const STRIPE_API_VERSION: &str = "2026-02-25.clover";
pub const FREE_PROJECT_LIMIT: u32 = 2;
/// Provider storage usage is reported in decimal kilobyte-hours. This keeps
/// Stripe's 12-decimal minor-unit prices precise enough for $0.20/GB-month.
pub const STORAGE_BILLING_UNIT_BYTES: u64 = 1_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageMetric {
    Reads,
    Writes,
    StorageByteHours,
    MonthlyActiveUsers,
}

impl UsageMetric {
    pub const ALL: [Self; 4] = [
        Self::Reads,
        Self::Writes,
        Self::StorageByteHours,
        Self::MonthlyActiveUsers,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::StorageByteHours => "storage_byte_hours",
            Self::MonthlyActiveUsers => "monthly_active_users",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageAllowance {
    pub storage_bytes: u64,
    pub monthly_reads: u64,
    pub monthly_writes: u64,
    pub monthly_active_users: u64,
    pub overage_enabled: bool,
}

#[must_use]
pub const fn default_allowance(tier: PlatformBillingTier) -> UsageAllowance {
    match tier {
        PlatformBillingTier::Free => UsageAllowance {
            storage_bytes: 1_000_000_000,
            monthly_reads: 1_000_000,
            monthly_writes: 50_000,
            monthly_active_users: 5_000,
            overage_enabled: false,
        },
        PlatformBillingTier::PayAsYouGo => UsageAllowance {
            storage_bytes: 1_000_000_000,
            monthly_reads: 1_000_000,
            monthly_writes: 50_000,
            monthly_active_users: 5_000,
            overage_enabled: true,
        },
        PlatformBillingTier::Pro => UsageAllowance {
            storage_bytes: 10_000_000_000,
            monthly_reads: 15_000_000,
            monthly_writes: 750_000,
            monthly_active_users: 50_000,
            overage_enabled: true,
        },
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformCheckoutInput {
    pub organization_id: OrganizationId,
    pub tier: PlatformBillingTier,
    pub billing_email: String,
    pub existing_customer_id: Option<String>,
    pub quantity: u32,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformPortalInput {
    pub organization_id: OrganizationId,
    pub customer_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformBillingUpdate {
    pub organization_id: OrganizationId,
    pub customer_id: String,
    pub subscription_id: Option<String>,
    pub tier: PlatformBillingTier,
    pub status: ProviderSubscriptionStatus,
    pub quantity: u32,
    pub current_period_start_ms: Option<i64>,
    pub current_period_end_ms: Option<i64>,
    pub cancel_at_period_end: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageMeterEvent {
    pub customer_id: String,
    pub metric: UsageMetric,
    /// Globally stable local outbox identifier. Stripe only guarantees rolling
    /// 24-hour identifier uniqueness, so FFDB remains the durable authority.
    pub identifier: String,
    pub value: u64,
    pub timestamp_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageSummaryInput {
    pub customer_id: String,
    pub metric: UsageMetric,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageSummary {
    pub metric: UsageMetric,
    pub aggregated_value: u64,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderSubscriptionStatus {
    CheckoutPending,
    Trialing,
    Active,
    PastDue,
    Unpaid,
    Canceled,
    Paused,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderInvoiceStatus {
    Draft,
    Open,
    Paid,
    Uncollectible,
    Void,
    PaymentFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformInvoiceUpdate {
    pub organization_id: OrganizationId,
    pub invoice_id: String,
    pub customer_id: String,
    pub subscription_id: Option<String>,
    pub status: ProviderInvoiceStatus,
    pub currency: String,
    pub amount_due_minor: u64,
    pub amount_paid_minor: u64,
    pub period_start_ms: Option<i64>,
    pub period_end_ms: Option<i64>,
    pub hosted_invoice_url: Option<String>,
    pub invoice_pdf_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBillingEvent {
    pub provider_event_id: String,
    pub event_type: String,
    pub livemode: bool,
    pub created_at_ms: i64,
    /// Events outside platform billing are acknowledged at this isolated
    /// endpoint but cannot mutate platform entitlements.
    pub platform_update: Option<PlatformBillingUpdate>,
    pub invoice_update: Option<PlatformInvoiceUpdate>,
}

#[async_trait]
pub trait PlatformBillingProvider: Send + Sync {
    /// Whether this handle currently has validated credentials and a usable
    /// provider account. Static providers are ready once constructed; dynamic
    /// instance providers override this while deployment modes are reconfigured.
    fn is_configured(&self) -> bool {
        true
    }

    async fn create_checkout(
        &self,
        input: &PlatformCheckoutInput,
    ) -> Result<BillingRedirect, BillingError>;

    async fn create_portal(
        &self,
        input: &PlatformPortalInput,
    ) -> Result<BillingRedirect, BillingError>;

    fn verify_webhook(
        &self,
        payload: &[u8],
        signature: &str,
        now_seconds: i64,
    ) -> Result<VerifiedBillingEvent, BillingError>;

    async fn report_usage(&self, input: &UsageMeterEvent) -> Result<(), BillingError>;

    async fn usage_summary(&self, input: &UsageSummaryInput) -> Result<UsageSummary, BillingError>;
}

/// Provider adapter seam for a project's own shop/payments tenant. It remains
/// separate from `PlatformBillingProvider`; the public project-commerce routes
/// compose the richer `ffdb-commerce` domain and per-project credential modes.
#[async_trait]
pub trait ProjectCommerceProvider: Send + Sync {
    async fn create_checkout(
        &self,
        account: &ProjectCommerceAccount,
        input: &ProjectCommerceCheckout,
    ) -> Result<BillingRedirect, BillingError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceChargeModel {
    Destination,
    Direct,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectCommerceAccount {
    pub project_id: ProjectId,
    pub provider: String,
    pub provider_account_id: String,
    pub charge_model: CommerceChargeModel,
    /// Explicit provider capabilities; never a legacy bundled account label.
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectCommerceCheckout {
    pub order_reference: String,
    pub currency: String,
    pub amount_minor: u64,
    pub customer_reference: Option<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BillingError {
    #[error("billing configuration is invalid")]
    InvalidConfiguration,
    #[error("billing request is invalid")]
    InvalidRequest,
    #[error("billing webhook signature is invalid")]
    InvalidWebhookSignature,
    #[error("billing webhook payload is invalid")]
    InvalidWebhookPayload,
    #[error("billing provider is unavailable")]
    ProviderUnavailable,
    #[error("billing provider rejected the request")]
    ProviderRejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_preserves_free_payg_and_pro_defaults() {
        let free = default_allowance(PlatformBillingTier::Free);
        assert_eq!(FREE_PROJECT_LIMIT, 2);
        assert_eq!(free.storage_bytes, 1_000_000_000);
        assert_eq!(free.monthly_reads, 1_000_000);
        assert_eq!(free.monthly_writes, 50_000);
        assert_eq!(free.monthly_active_users, 5_000);
        assert!(!free.overage_enabled);

        assert!(default_allowance(PlatformBillingTier::PayAsYouGo).overage_enabled);
        let pro = default_allowance(PlatformBillingTier::Pro);
        assert_eq!(pro.storage_bytes, 10_000_000_000);
        assert_eq!(pro.monthly_reads, 15_000_000);
        assert_eq!(pro.monthly_writes, 750_000);
        assert_eq!(pro.monthly_active_users, 50_000);
    }
}
