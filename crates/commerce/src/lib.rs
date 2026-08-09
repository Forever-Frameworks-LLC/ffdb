//! Provider-neutral, project-scoped commerce domain model.
//!
//! This crate owns business invariants only. Provider adapters, persistence,
//! HTTP handlers, and secret decryption live outside it. A project can use
//! either credentials stored in the project's secret store or a connected
//! provider account configured for direct charges. Both modes use the same
//! products, checkout, order, payment, refund, and subscription API.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use ffdb_protocol::ProjectId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// Largest minor-unit amount that remains an exact integer in JSON clients.
pub const MAX_MINOR_AMOUNT: u64 = 9_007_199_254_740_991;
/// Defensive upper bound for one line item or subscription quantity.
pub const MAX_QUANTITY: u32 = 1_000_000;

macro_rules! commerce_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

commerce_id!(MerchantAccountId);
commerce_id!(ProductId);
commerce_id!(PriceId);
commerce_id!(OrderId);
commerce_id!(CheckoutIntentId);
commerce_id!(PaymentId);
commerce_id!(RefundId);
commerce_id!(IndividualId);
commerce_id!(TeamId);
commerce_id!(SubjectOrganizationId);
commerce_id!(SubscriptionId);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Currency(String);

impl Currency {
    /// Creates a strict ISO-style, three-letter uppercase currency code.
    pub fn new(value: impl Into<String>) -> Result<Self, CommerceError> {
        let value = value.into();
        if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(CommerceError::InvalidCurrency);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Money {
    currency: Currency,
    minor: u64,
}

impl Money {
    pub fn new(currency: Currency, minor: u64) -> Result<Self, CommerceError> {
        if minor > MAX_MINOR_AMOUNT {
            return Err(CommerceError::AmountOutOfRange);
        }
        Ok(Self { currency, minor })
    }

    pub fn positive(currency: Currency, minor: u64) -> Result<Self, CommerceError> {
        if minor == 0 {
            return Err(CommerceError::NonPositiveAmount);
        }
        Self::new(currency, minor)
    }

    #[must_use]
    pub fn currency(&self) -> &Currency {
        &self.currency
    }

    #[must_use]
    pub const fn minor(&self) -> u64 {
        self.minor
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, CommerceError> {
        self.ensure_same_currency(other)?;
        let minor = self
            .minor
            .checked_add(other.minor)
            .ok_or(CommerceError::ArithmeticOverflow)?;
        Self::new(self.currency.clone(), minor)
    }

    pub fn checked_sub(&self, other: &Self) -> Result<Self, CommerceError> {
        self.ensure_same_currency(other)?;
        let minor = self
            .minor
            .checked_sub(other.minor)
            .ok_or(CommerceError::AmountUnderflow)?;
        Self::new(self.currency.clone(), minor)
    }

    pub fn checked_mul(&self, quantity: u32) -> Result<Self, CommerceError> {
        validate_quantity(quantity)?;
        let minor = self
            .minor
            .checked_mul(u64::from(quantity))
            .ok_or(CommerceError::ArithmeticOverflow)?;
        Self::new(self.currency.clone(), minor)
    }

    fn ensure_same_currency(&self, other: &Self) -> Result<(), CommerceError> {
        if self.currency != other.currency {
            return Err(CommerceError::CurrencyMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ProviderReference(String);

impl ProviderReference {
    pub fn new(value: impl Into<String>) -> Result<Self, CommerceError> {
        let value = value.into();
        validate_nonempty_bounded("provider_reference", &value, 255)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reference into an encrypted project secret store. Plain provider keys are
/// deliberately not representable as merchant configuration.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SecretReference(String);

impl SecretReference {
    pub fn new(value: impl Into<String>) -> Result<Self, CommerceError> {
        let value = value.into();
        validate_nonempty_bounded("secret_reference", &value, 255)?;
        if !value.starts_with("secret://") || value.chars().any(char::is_whitespace) {
            return Err(CommerceError::InvalidSecretReference);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Mutually exclusive provider connection modes. Values are opaque references,
/// never decrypted credentials.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MerchantProviderMode {
    BringYourOwnCredentials {
        credential_reference: SecretReference,
    },
    ConnectedAccount {
        account_reference: ProviderReference,
        charge_model: ConnectedChargeModel,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectedChargeModel {
    /// The project-owned account creates the charge and is merchant of record.
    Direct,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MerchantOfRecord {
    ProjectOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MerchantAccountStatus {
    Pending,
    Restricted,
    Active,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MerchantCapability {
    OneTimePayments,
    RecurringPayments,
    Refunds,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MerchantAccount {
    id: MerchantAccountId,
    project_id: ProjectId,
    provider_mode: MerchantProviderMode,
    merchant_of_record: MerchantOfRecord,
    status: MerchantAccountStatus,
    capabilities: BTreeSet<MerchantCapability>,
}

impl MerchantAccount {
    #[must_use]
    pub fn new(
        id: MerchantAccountId,
        project_id: ProjectId,
        provider_mode: MerchantProviderMode,
    ) -> Self {
        Self {
            id,
            project_id,
            provider_mode,
            merchant_of_record: MerchantOfRecord::ProjectOwner,
            status: MerchantAccountStatus::Pending,
            capabilities: BTreeSet::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> MerchantAccountId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn status(&self) -> MerchantAccountStatus {
        self.status
    }

    #[must_use]
    pub const fn merchant_of_record(&self) -> MerchantOfRecord {
        self.merchant_of_record
    }

    #[must_use]
    pub const fn provider_mode(&self) -> &MerchantProviderMode {
        &self.provider_mode
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<MerchantCapability> {
        &self.capabilities
    }

    pub fn set_provider_state(
        &mut self,
        status: MerchantAccountStatus,
        capabilities: impl IntoIterator<Item = MerchantCapability>,
    ) {
        self.status = status;
        self.capabilities = capabilities.into_iter().collect();
    }

    pub fn require_capability(&self, capability: MerchantCapability) -> Result<(), CommerceError> {
        if self.status != MerchantAccountStatus::Active {
            return Err(CommerceError::MerchantUnavailable);
        }
        if !self.capabilities.contains(&capability) {
            return Err(CommerceError::MissingCapability(capability));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductStatus {
    Draft,
    Active,
    Archived,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Product {
    id: ProductId,
    project_id: ProjectId,
    name: String,
    description: Option<String>,
    status: ProductStatus,
}

impl Product {
    pub fn new(
        id: ProductId,
        project_id: ProjectId,
        name: impl Into<String>,
        description: Option<String>,
    ) -> Result<Self, CommerceError> {
        let name = name.into();
        validate_nonempty_bounded("product_name", &name, 200)?;
        if let Some(value) = description.as_ref() {
            validate_bounded("product_description", value, 10_000)?;
        }
        Ok(Self {
            id,
            project_id,
            name,
            description,
            status: ProductStatus::Draft,
        })
    }

    #[must_use]
    pub const fn id(&self) -> ProductId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub const fn status(&self) -> ProductStatus {
        self.status
    }

    pub fn activate(&mut self) -> Result<(), CommerceError> {
        match self.status {
            ProductStatus::Draft => {
                self.status = ProductStatus::Active;
                Ok(())
            }
            ProductStatus::Active | ProductStatus::Archived => {
                Err(CommerceError::InvalidStateTransition)
            }
        }
    }

    pub fn archive(&mut self) -> Result<(), CommerceError> {
        match self.status {
            ProductStatus::Draft | ProductStatus::Active => {
                self.status = ProductStatus::Archived;
                Ok(())
            }
            ProductStatus::Archived => Err(CommerceError::InvalidStateTransition),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingIntervalUnit {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BillingInterval {
    unit: BillingIntervalUnit,
    count: u16,
}

impl BillingInterval {
    pub fn new(unit: BillingIntervalUnit, count: u16) -> Result<Self, CommerceError> {
        if count == 0 || count > 365 {
            return Err(CommerceError::InvalidBillingInterval);
        }
        Ok(Self { unit, count })
    }

    #[must_use]
    pub const fn unit(self) -> BillingIntervalUnit {
        self.unit
    }

    #[must_use]
    pub const fn count(self) -> u16 {
        self.count
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EntitlementKey(String);

impl EntitlementKey {
    pub fn new(value: impl Into<String>) -> Result<Self, CommerceError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || (index > 0
                        && (byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.' | b':')))
            });
        if !valid {
            return Err(CommerceError::InvalidEntitlementKey);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum EntitlementValue {
    Enabled(bool),
    Quantity(u64),
    Text(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", content = "interval", rename_all = "snake_case")]
pub enum PriceKind {
    OneTime,
    Recurring(BillingInterval),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct PriceTerms {
    amount: Money,
    kind: PriceKind,
    entitlements: BTreeMap<EntitlementKey, EntitlementValue>,
}

impl PriceTerms {
    pub fn one_time(amount: Money) -> Result<Self, CommerceError> {
        require_positive_money(&amount)?;
        Ok(Self {
            amount,
            kind: PriceKind::OneTime,
            entitlements: BTreeMap::new(),
        })
    }

    pub fn recurring(
        amount: Money,
        interval: BillingInterval,
        entitlements: BTreeMap<EntitlementKey, EntitlementValue>,
    ) -> Result<Self, CommerceError> {
        require_positive_money(&amount)?;
        Ok(Self {
            amount,
            kind: PriceKind::Recurring(interval),
            entitlements,
        })
    }

    #[must_use]
    pub const fn amount(&self) -> &Money {
        &self.amount
    }

    #[must_use]
    pub const fn kind(&self) -> &PriceKind {
        &self.kind
    }

    #[must_use]
    pub const fn entitlements(&self) -> &BTreeMap<EntitlementKey, EntitlementValue> {
        &self.entitlements
    }
}

/// A price's terms are intentionally private and have no mutation methods.
/// Retiring a price only prevents new purchases; existing snapshots remain valid.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Price {
    id: PriceId,
    project_id: ProjectId,
    product_id: ProductId,
    terms: PriceTerms,
    active: bool,
}

impl Price {
    pub fn new(id: PriceId, product: &Product, terms: PriceTerms) -> Result<Self, CommerceError> {
        if product.status != ProductStatus::Active {
            return Err(CommerceError::ProductNotActive);
        }
        Ok(Self {
            id,
            project_id: product.project_id,
            product_id: product.id,
            terms,
            active: true,
        })
    }

    #[must_use]
    pub const fn id(&self) -> PriceId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn product_id(&self) -> ProductId {
        self.product_id
    }

    #[must_use]
    pub const fn terms(&self) -> &PriceTerms {
        &self.terms
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    pub fn retire(&mut self) -> Result<(), CommerceError> {
        if !self.active {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.active = false;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct OrderLineSnapshot {
    project_id: ProjectId,
    product_id: ProductId,
    price_id: PriceId,
    product_name: String,
    unit_amount: Money,
    quantity: u32,
    total: Money,
}

impl OrderLineSnapshot {
    pub fn from_price(
        product: &Product,
        price: &Price,
        quantity: u32,
    ) -> Result<Self, CommerceError> {
        validate_quantity(quantity)?;
        if !price.active {
            return Err(CommerceError::PriceInactive);
        }
        if product.project_id != price.project_id || product.id != price.product_id {
            return Err(CommerceError::ProductPriceMismatch);
        }
        if product.status != ProductStatus::Active {
            return Err(CommerceError::ProductNotActive);
        }
        if price.terms.kind != PriceKind::OneTime {
            return Err(CommerceError::PriceKindMismatch);
        }
        let total = price.terms.amount.checked_mul(quantity)?;
        Ok(Self {
            project_id: product.project_id,
            product_id: product.id,
            price_id: price.id,
            product_name: product.name.clone(),
            unit_amount: price.terms.amount.clone(),
            quantity,
            total,
        })
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn product_id(&self) -> ProductId {
        self.product_id
    }

    #[must_use]
    pub const fn price_id(&self) -> PriceId {
        self.price_id
    }

    #[must_use]
    pub fn product_name(&self) -> &str {
        &self.product_name
    }

    #[must_use]
    pub const fn unit_amount(&self) -> &Money {
        &self.unit_amount
    }

    #[must_use]
    pub const fn quantity(&self) -> u32 {
        self.quantity
    }

    #[must_use]
    pub const fn total(&self) -> &Money {
        &self.total
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    AwaitingPayment,
    Paid,
    FulfillmentInProgress,
    Fulfilled,
    Canceled,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Order {
    id: OrderId,
    project_id: ProjectId,
    merchant_account_id: MerchantAccountId,
    lines: Vec<OrderLineSnapshot>,
    total: Money,
    status: OrderStatus,
    created_at_ms: i64,
    fulfilled_at_ms: Option<i64>,
}

impl Order {
    pub fn new(
        id: OrderId,
        merchant: &MerchantAccount,
        lines: Vec<OrderLineSnapshot>,
        created_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        merchant.require_capability(MerchantCapability::OneTimePayments)?;
        let first = lines.first().ok_or(CommerceError::EmptyOrder)?;
        let mut total = Money::new(first.total.currency.clone(), 0)?;
        for line in &lines {
            if line.project_id != merchant.project_id {
                return Err(CommerceError::ProjectMismatch);
            }
            total = total.checked_add(&line.total)?;
        }
        require_positive_money(&total)?;
        Ok(Self {
            id,
            project_id: merchant.project_id,
            merchant_account_id: merchant.id,
            lines,
            total,
            status: OrderStatus::AwaitingPayment,
            created_at_ms,
            fulfilled_at_ms: None,
        })
    }

    #[must_use]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn merchant_account_id(&self) -> MerchantAccountId {
        self.merchant_account_id
    }

    #[must_use]
    pub fn lines(&self) -> &[OrderLineSnapshot] {
        &self.lines
    }

    #[must_use]
    pub const fn total(&self) -> &Money {
        &self.total
    }

    #[must_use]
    pub const fn status(&self) -> OrderStatus {
        self.status
    }

    #[must_use]
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    #[must_use]
    pub const fn fulfilled_at_ms(&self) -> Option<i64> {
        self.fulfilled_at_ms
    }

    pub fn verify_paid(&mut self, payments: &[&Payment]) -> Result<(), CommerceError> {
        if self.status != OrderStatus::AwaitingPayment {
            return Err(CommerceError::InvalidStateTransition);
        }
        ensure_order_paid(self, payments)?;
        self.status = OrderStatus::Paid;
        Ok(())
    }

    pub fn begin_fulfillment(&mut self, payments: &[&Payment]) -> Result<(), CommerceError> {
        if self.status != OrderStatus::Paid {
            return Err(CommerceError::OrderNotPaid);
        }
        // Re-evaluate at the fulfillment boundary so a pending or completed
        // refund cannot rely on a stale paid flag.
        ensure_order_paid(self, payments)?;
        self.status = OrderStatus::FulfillmentInProgress;
        Ok(())
    }

    pub fn mark_fulfilled(&mut self, fulfilled_at_ms: i64) -> Result<(), CommerceError> {
        if self.status != OrderStatus::FulfillmentInProgress {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = OrderStatus::Fulfilled;
        self.fulfilled_at_ms = Some(fulfilled_at_ms);
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), CommerceError> {
        if self.status != OrderStatus::AwaitingPayment {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = OrderStatus::Canceled;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Pending,
    Authorized,
    Captured,
    PartiallyRefunded,
    Refunded,
    Failed,
    Canceled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefundStatus {
    Requested,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Refund {
    id: RefundId,
    amount: Money,
    status: RefundStatus,
    requested_at_ms: i64,
    settled_at_ms: Option<i64>,
    provider_reference: Option<ProviderReference>,
}

impl Refund {
    #[must_use]
    pub const fn id(&self) -> RefundId {
        self.id
    }

    #[must_use]
    pub const fn amount(&self) -> &Money {
        &self.amount
    }

    #[must_use]
    pub const fn status(&self) -> RefundStatus {
        self.status
    }

    #[must_use]
    pub const fn requested_at_ms(&self) -> i64 {
        self.requested_at_ms
    }

    #[must_use]
    pub const fn settled_at_ms(&self) -> Option<i64> {
        self.settled_at_ms
    }

    #[must_use]
    pub const fn provider_reference(&self) -> Option<&ProviderReference> {
        self.provider_reference.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum PaymentTarget {
    Order(OrderId),
    Subscription(SubscriptionId),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Payment {
    id: PaymentId,
    project_id: ProjectId,
    merchant_account_id: MerchantAccountId,
    target: PaymentTarget,
    requested_amount: Money,
    captured_amount: Option<Money>,
    status: PaymentStatus,
    provider_reference: Option<ProviderReference>,
    refunds: Vec<Refund>,
    events: EventLedger,
    created_at_ms: i64,
    captured_at_ms: Option<i64>,
}

impl Payment {
    pub fn new(
        id: PaymentId,
        order: &Order,
        requested_amount: Money,
        created_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        require_positive_money(&requested_amount)?;
        requested_amount.ensure_same_currency(&order.total)?;
        if requested_amount.minor > order.total.minor {
            return Err(CommerceError::PaymentExceedsOrderTotal);
        }
        Ok(Self {
            id,
            project_id: order.project_id,
            merchant_account_id: order.merchant_account_id,
            target: PaymentTarget::Order(order.id),
            requested_amount,
            captured_amount: None,
            status: PaymentStatus::Pending,
            provider_reference: None,
            refunds: Vec::new(),
            events: EventLedger::default(),
            created_at_ms,
            captured_at_ms: None,
        })
    }

    /// Creates the payment record for a recurring subscription period from
    /// the subscription's immutable price and quantity snapshot.
    pub fn for_subscription(
        id: PaymentId,
        merchant: &MerchantAccount,
        subscription: &Subscription,
        created_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        merchant.require_capability(MerchantCapability::RecurringPayments)?;
        if merchant.project_id != subscription.project_id
            || merchant.id != subscription.merchant_account_id
        {
            return Err(CommerceError::ProjectMismatch);
        }
        if matches!(
            subscription.status,
            SubscriptionStatus::Canceled | SubscriptionStatus::Expired
        ) {
            return Err(CommerceError::InvalidStateTransition);
        }
        let requested_amount = subscription
            .terms
            .unit_amount
            .checked_mul(subscription.terms.quantity)?;
        require_positive_money(&requested_amount)?;
        Ok(Self {
            id,
            project_id: subscription.project_id,
            merchant_account_id: subscription.merchant_account_id,
            target: PaymentTarget::Subscription(subscription.id),
            requested_amount,
            captured_amount: None,
            status: PaymentStatus::Pending,
            provider_reference: None,
            refunds: Vec::new(),
            events: EventLedger::default(),
            created_at_ms,
            captured_at_ms: None,
        })
    }

    #[must_use]
    pub const fn id(&self) -> PaymentId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn target(&self) -> &PaymentTarget {
        &self.target
    }

    #[must_use]
    pub const fn order_id(&self) -> Option<OrderId> {
        match &self.target {
            PaymentTarget::Order(id) => Some(*id),
            PaymentTarget::Subscription(_) => None,
        }
    }

    #[must_use]
    pub const fn subscription_id(&self) -> Option<SubscriptionId> {
        match &self.target {
            PaymentTarget::Order(_) => None,
            PaymentTarget::Subscription(id) => Some(*id),
        }
    }

    #[must_use]
    pub const fn requested_amount(&self) -> &Money {
        &self.requested_amount
    }

    #[must_use]
    pub const fn captured_amount(&self) -> Option<&Money> {
        self.captured_amount.as_ref()
    }

    #[must_use]
    pub const fn status(&self) -> PaymentStatus {
        self.status
    }

    #[must_use]
    pub fn refunds(&self) -> &[Refund] {
        &self.refunds
    }

    #[must_use]
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    pub fn mark_authorized(&mut self) -> Result<(), CommerceError> {
        if self.status != PaymentStatus::Pending {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = PaymentStatus::Authorized;
        Ok(())
    }

    /// Records a capture only from a verified, ordered provider event.
    pub fn record_capture(
        &mut self,
        event: &EventEnvelope,
        captured_amount: Money,
        provider_reference: ProviderReference,
        captured_at_ms: i64,
    ) -> Result<EventApplication, CommerceError> {
        match self.events.classify(event)? {
            EventClassification::Duplicate => return Ok(EventApplication::Duplicate),
            EventClassification::New => {}
        }
        if !matches!(
            self.status,
            PaymentStatus::Pending | PaymentStatus::Authorized
        ) {
            return Err(CommerceError::InvalidStateTransition);
        }
        require_positive_money(&captured_amount)?;
        captured_amount.ensure_same_currency(&self.requested_amount)?;
        if captured_amount.minor > self.requested_amount.minor {
            return Err(CommerceError::CaptureExceedsAuthorized);
        }
        self.events.record(event)?;
        self.captured_amount = Some(captured_amount);
        self.provider_reference = Some(provider_reference);
        self.captured_at_ms = Some(captured_at_ms);
        self.status = PaymentStatus::Captured;
        Ok(EventApplication::Applied)
    }

    pub fn mark_failed(&mut self) -> Result<(), CommerceError> {
        if !matches!(
            self.status,
            PaymentStatus::Pending | PaymentStatus::Authorized
        ) {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = PaymentStatus::Failed;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), CommerceError> {
        if !matches!(
            self.status,
            PaymentStatus::Pending | PaymentStatus::Authorized
        ) {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = PaymentStatus::Canceled;
        Ok(())
    }

    /// Reserves refundable funds immediately so concurrent partial refunds
    /// cannot exceed captured funds when persistence uses aggregate locking.
    pub fn request_refund(
        &mut self,
        merchant: &MerchantAccount,
        id: RefundId,
        amount: Money,
        requested_at_ms: i64,
    ) -> Result<(), CommerceError> {
        merchant.require_capability(MerchantCapability::Refunds)?;
        if merchant.project_id != self.project_id || merchant.id != self.merchant_account_id {
            return Err(CommerceError::ProjectMismatch);
        }
        if self.refunds.iter().any(|refund| refund.id == id) {
            return Err(CommerceError::DuplicateIdentifier);
        }
        require_positive_money(&amount)?;
        let available = self.refundable_amount()?;
        amount.ensure_same_currency(&available)?;
        if amount.minor > available.minor {
            return Err(CommerceError::RefundExceedsAvailable);
        }
        self.refunds.push(Refund {
            id,
            amount,
            status: RefundStatus::Requested,
            requested_at_ms,
            settled_at_ms: None,
            provider_reference: None,
        });
        Ok(())
    }

    pub fn settle_refund(
        &mut self,
        id: RefundId,
        event: &EventEnvelope,
        outcome: RefundSettlement,
        settled_at_ms: i64,
    ) -> Result<EventApplication, CommerceError> {
        match self.events.classify(event)? {
            EventClassification::Duplicate => return Ok(EventApplication::Duplicate),
            EventClassification::New => {}
        }
        let index = self
            .refunds
            .iter()
            .position(|refund| refund.id == id)
            .ok_or(CommerceError::RefundNotFound)?;
        if self.refunds[index].status != RefundStatus::Requested {
            return Err(CommerceError::RefundAlreadySettled);
        }
        self.events.record(event)?;
        let refund = &mut self.refunds[index];
        match outcome {
            RefundSettlement::Succeeded { provider_reference } => {
                refund.status = RefundStatus::Succeeded;
                refund.provider_reference = Some(provider_reference);
            }
            RefundSettlement::Failed => refund.status = RefundStatus::Failed,
        }
        refund.settled_at_ms = Some(settled_at_ms);
        self.recompute_refund_status()?;
        Ok(EventApplication::Applied)
    }

    /// Amount not already refunded or reserved by an in-flight refund.
    pub fn refundable_amount(&self) -> Result<Money, CommerceError> {
        let captured = self
            .captured_amount
            .as_ref()
            .ok_or(CommerceError::PaymentNotCaptured)?;
        let reserved = self.refunds.iter().try_fold(
            Money::new(captured.currency.clone(), 0)?,
            |total, refund| {
                if refund.status == RefundStatus::Failed {
                    Ok(total)
                } else {
                    total.checked_add(&refund.amount)
                }
            },
        )?;
        captured.checked_sub(&reserved)
    }

    /// Verified captured funds less successful and pending refund reservations.
    fn fulfillable_amount(&self) -> Result<Money, CommerceError> {
        if !matches!(
            self.status,
            PaymentStatus::Captured | PaymentStatus::PartiallyRefunded | PaymentStatus::Refunded
        ) {
            return Err(CommerceError::PaymentNotVerified);
        }
        self.refundable_amount()
    }

    fn recompute_refund_status(&mut self) -> Result<(), CommerceError> {
        let captured = self
            .captured_amount
            .as_ref()
            .ok_or(CommerceError::PaymentNotCaptured)?;
        let succeeded = self.refunds.iter().try_fold(
            Money::new(captured.currency.clone(), 0)?,
            |total, refund| {
                if refund.status == RefundStatus::Succeeded {
                    total.checked_add(&refund.amount)
                } else {
                    Ok(total)
                }
            },
        )?;
        self.status = if succeeded.minor == 0 {
            PaymentStatus::Captured
        } else if succeeded.minor == captured.minor {
            PaymentStatus::Refunded
        } else {
            PaymentStatus::PartiallyRefunded
        };
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefundSettlement {
    Succeeded {
        provider_reference: ProviderReference,
    },
    Failed,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum MembershipSubject {
    Individual(IndividualId),
    Team(TeamId),
    Organization(SubjectOrganizationId),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SubscriptionTermsSnapshot {
    price_id: PriceId,
    product_id: ProductId,
    unit_amount: Money,
    quantity: u32,
    interval: BillingInterval,
    entitlements: BTreeMap<EntitlementKey, EntitlementValue>,
}

impl SubscriptionTermsSnapshot {
    #[must_use]
    pub const fn price_id(&self) -> PriceId {
        self.price_id
    }

    #[must_use]
    pub const fn product_id(&self) -> ProductId {
        self.product_id
    }

    #[must_use]
    pub const fn unit_amount(&self) -> &Money {
        &self.unit_amount
    }

    #[must_use]
    pub const fn quantity(&self) -> u32 {
        self.quantity
    }

    #[must_use]
    pub const fn interval(&self) -> BillingInterval {
        self.interval
    }

    #[must_use]
    pub const fn entitlements(&self) -> &BTreeMap<EntitlementKey, EntitlementValue> {
        &self.entitlements
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Incomplete,
    Trialing,
    Active,
    PastDue,
    Paused,
    Canceled,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BillingPeriod {
    start_ms: i64,
    end_ms: i64,
}

impl BillingPeriod {
    pub fn new(start_ms: i64, end_ms: i64) -> Result<Self, CommerceError> {
        if start_ms >= end_ms {
            return Err(CommerceError::InvalidBillingPeriod);
        }
        Ok(Self { start_ms, end_ms })
    }

    #[must_use]
    pub const fn start_ms(self) -> i64 {
        self.start_ms
    }

    #[must_use]
    pub const fn end_ms(self) -> i64 {
        self.end_ms
    }

    #[must_use]
    pub const fn contains(self, at_ms: i64) -> bool {
        at_ms >= self.start_ms && at_ms < self.end_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionTransition {
    StartTrial { period: BillingPeriod },
    Activate { period: BillingPeriod },
    Renew { period: BillingPeriod },
    MarkPastDue,
    Pause,
    Cancel,
    Expire,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Subscription {
    id: SubscriptionId,
    project_id: ProjectId,
    merchant_account_id: MerchantAccountId,
    subject: MembershipSubject,
    terms: SubscriptionTermsSnapshot,
    status: SubscriptionStatus,
    current_period: Option<BillingPeriod>,
    events: EventLedger,
    created_at_ms: i64,
    ended_at_ms: Option<i64>,
}

impl Subscription {
    pub fn new(
        id: SubscriptionId,
        merchant: &MerchantAccount,
        price: &Price,
        subject: MembershipSubject,
        quantity: u32,
        created_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        merchant.require_capability(MerchantCapability::RecurringPayments)?;
        if merchant.project_id != price.project_id {
            return Err(CommerceError::ProjectMismatch);
        }
        if !price.active {
            return Err(CommerceError::PriceInactive);
        }
        validate_quantity(quantity)?;
        let interval = match price.terms.kind {
            PriceKind::Recurring(interval) => interval,
            PriceKind::OneTime => return Err(CommerceError::PriceKindMismatch),
        };
        // Validate the aggregate amount now, while retaining immutable unit terms.
        price.terms.amount.checked_mul(quantity)?;
        Ok(Self {
            id,
            project_id: merchant.project_id,
            merchant_account_id: merchant.id,
            subject,
            terms: SubscriptionTermsSnapshot {
                price_id: price.id,
                product_id: price.product_id,
                unit_amount: price.terms.amount.clone(),
                quantity,
                interval,
                entitlements: price.terms.entitlements.clone(),
            },
            status: SubscriptionStatus::Incomplete,
            current_period: None,
            events: EventLedger::default(),
            created_at_ms,
            ended_at_ms: None,
        })
    }

    #[must_use]
    pub const fn id(&self) -> SubscriptionId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn merchant_account_id(&self) -> MerchantAccountId {
        self.merchant_account_id
    }

    #[must_use]
    pub const fn subject(&self) -> &MembershipSubject {
        &self.subject
    }

    #[must_use]
    pub const fn terms(&self) -> &SubscriptionTermsSnapshot {
        &self.terms
    }

    #[must_use]
    pub const fn status(&self) -> SubscriptionStatus {
        self.status
    }

    #[must_use]
    pub const fn current_period(&self) -> Option<BillingPeriod> {
        self.current_period
    }

    #[must_use]
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    #[must_use]
    pub const fn ended_at_ms(&self) -> Option<i64> {
        self.ended_at_ms
    }

    pub fn apply_event(
        &mut self,
        event: &EventEnvelope,
        transition: SubscriptionTransition,
        applied_at_ms: i64,
    ) -> Result<EventApplication, CommerceError> {
        match self.events.classify(event)? {
            EventClassification::Duplicate => return Ok(EventApplication::Duplicate),
            EventClassification::New => {}
        }
        let (next_status, next_period, ended_at_ms) =
            self.preview_transition(&transition, applied_at_ms)?;
        self.events.record(event)?;
        self.status = next_status;
        self.current_period = next_period;
        self.ended_at_ms = ended_at_ms;
        Ok(EventApplication::Applied)
    }

    #[must_use]
    pub fn is_entitled_at(&self, at_ms: i64) -> bool {
        matches!(
            self.status,
            SubscriptionStatus::Trialing | SubscriptionStatus::Active
        ) && self
            .current_period
            .is_some_and(|period| period.contains(at_ms))
    }

    #[must_use]
    pub fn entitlement(&self, key: &EntitlementKey, at_ms: i64) -> Option<&EntitlementValue> {
        self.is_entitled_at(at_ms)
            .then(|| self.terms.entitlements.get(key))
            .flatten()
    }

    fn preview_transition(
        &self,
        transition: &SubscriptionTransition,
        applied_at_ms: i64,
    ) -> Result<(SubscriptionStatus, Option<BillingPeriod>, Option<i64>), CommerceError> {
        use SubscriptionStatus as Status;
        use SubscriptionTransition as Transition;

        let result = match (self.status, transition) {
            (Status::Incomplete, Transition::StartTrial { period }) => {
                (Status::Trialing, Some(*period), None)
            }
            (
                Status::Incomplete | Status::Trialing | Status::PastDue | Status::Paused,
                Transition::Activate { period },
            ) => (Status::Active, Some(*period), None),
            (Status::Active | Status::Trialing, Transition::Renew { period }) => {
                if self
                    .current_period
                    .is_some_and(|current| period.start_ms < current.end_ms)
                {
                    return Err(CommerceError::OverlappingBillingPeriod);
                }
                (Status::Active, Some(*period), None)
            }
            (Status::Active | Status::Trialing, Transition::MarkPastDue) => {
                (Status::PastDue, self.current_period, None)
            }
            (Status::Active | Status::Trialing | Status::PastDue, Transition::Pause) => {
                (Status::Paused, self.current_period, None)
            }
            (
                Status::Incomplete
                | Status::Trialing
                | Status::Active
                | Status::PastDue
                | Status::Paused,
                Transition::Cancel,
            ) => (Status::Canceled, self.current_period, Some(applied_at_ms)),
            (
                Status::Incomplete
                | Status::Trialing
                | Status::Active
                | Status::PastDue
                | Status::Paused,
                Transition::Expire,
            ) => (Status::Expired, self.current_period, Some(applied_at_ms)),
            _ => return Err(CommerceError::InvalidStateTransition),
        };
        Ok(result)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CheckoutKind {
    OneTime {
        order_id: OrderId,
        amount: Money,
    },
    Recurring {
        subscription_id: SubscriptionId,
        terms: SubscriptionTermsSnapshot,
        subject: MembershipSubject,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckoutStatus {
    Created,
    Completed { outcome: CheckoutOutcome },
    Expired,
    Canceled,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CheckoutOutcome {
    Payment { payment_id: PaymentId },
    Subscription { subscription_id: SubscriptionId },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CheckoutIntent {
    id: CheckoutIntentId,
    project_id: ProjectId,
    merchant_account_id: MerchantAccountId,
    kind: CheckoutKind,
    status: CheckoutStatus,
    created_at_ms: i64,
    expires_at_ms: i64,
}

impl CheckoutIntent {
    pub fn one_time(
        id: CheckoutIntentId,
        merchant: &MerchantAccount,
        order: &Order,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        merchant.require_capability(MerchantCapability::OneTimePayments)?;
        validate_expiration(created_at_ms, expires_at_ms)?;
        if merchant.project_id != order.project_id || merchant.id != order.merchant_account_id {
            return Err(CommerceError::ProjectMismatch);
        }
        if order.status != OrderStatus::AwaitingPayment {
            return Err(CommerceError::InvalidStateTransition);
        }
        Ok(Self {
            id,
            project_id: merchant.project_id,
            merchant_account_id: merchant.id,
            kind: CheckoutKind::OneTime {
                order_id: order.id,
                amount: order.total.clone(),
            },
            status: CheckoutStatus::Created,
            created_at_ms,
            expires_at_ms,
        })
    }

    pub fn recurring(
        id: CheckoutIntentId,
        merchant: &MerchantAccount,
        subscription: &Subscription,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        merchant.require_capability(MerchantCapability::RecurringPayments)?;
        validate_expiration(created_at_ms, expires_at_ms)?;
        if merchant.project_id != subscription.project_id
            || merchant.id != subscription.merchant_account_id
        {
            return Err(CommerceError::ProjectMismatch);
        }
        if subscription.status != SubscriptionStatus::Incomplete {
            return Err(CommerceError::InvalidStateTransition);
        }
        Ok(Self {
            id,
            project_id: merchant.project_id,
            merchant_account_id: merchant.id,
            kind: CheckoutKind::Recurring {
                subscription_id: subscription.id,
                terms: subscription.terms.clone(),
                subject: subscription.subject.clone(),
            },
            status: CheckoutStatus::Created,
            created_at_ms,
            expires_at_ms,
        })
    }

    #[must_use]
    pub const fn id(&self) -> CheckoutIntentId {
        self.id
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn merchant_account_id(&self) -> MerchantAccountId {
        self.merchant_account_id
    }

    #[must_use]
    pub const fn kind(&self) -> &CheckoutKind {
        &self.kind
    }

    #[must_use]
    pub const fn status(&self) -> &CheckoutStatus {
        &self.status
    }

    #[must_use]
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    #[must_use]
    pub const fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    pub fn complete_one_time(
        &mut self,
        payment: &Payment,
        completed_at_ms: i64,
    ) -> Result<(), CommerceError> {
        self.ensure_completable(completed_at_ms)?;
        let (order_id, amount) = match &self.kind {
            CheckoutKind::OneTime { order_id, amount } => (*order_id, amount),
            CheckoutKind::Recurring { .. } => return Err(CommerceError::CheckoutKindMismatch),
        };
        if payment.project_id != self.project_id || payment.order_id() != Some(order_id) {
            return Err(CommerceError::ProjectMismatch);
        }
        let available = payment.fulfillable_amount()?;
        available.ensure_same_currency(amount)?;
        if available.minor < amount.minor {
            return Err(CommerceError::InsufficientVerifiedPayment);
        }
        self.status = CheckoutStatus::Completed {
            outcome: CheckoutOutcome::Payment {
                payment_id: payment.id,
            },
        };
        Ok(())
    }

    pub fn complete_recurring(
        &mut self,
        subscription: &Subscription,
        completed_at_ms: i64,
    ) -> Result<(), CommerceError> {
        self.ensure_completable(completed_at_ms)?;
        let subscription_id = match self.kind {
            CheckoutKind::Recurring {
                subscription_id, ..
            } => subscription_id,
            CheckoutKind::OneTime { .. } => return Err(CommerceError::CheckoutKindMismatch),
        };
        if subscription.project_id != self.project_id || subscription.id != subscription_id {
            return Err(CommerceError::ProjectMismatch);
        }
        if !matches!(
            subscription.status,
            SubscriptionStatus::Trialing | SubscriptionStatus::Active
        ) {
            return Err(CommerceError::SubscriptionNotEntitled);
        }
        self.status = CheckoutStatus::Completed {
            outcome: CheckoutOutcome::Subscription { subscription_id },
        };
        Ok(())
    }

    pub fn expire(&mut self, at_ms: i64) -> Result<(), CommerceError> {
        if self.status != CheckoutStatus::Created || at_ms < self.expires_at_ms {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = CheckoutStatus::Expired;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), CommerceError> {
        if self.status != CheckoutStatus::Created {
            return Err(CommerceError::InvalidStateTransition);
        }
        self.status = CheckoutStatus::Canceled;
        Ok(())
    }

    fn ensure_completable(&self, at_ms: i64) -> Result<(), CommerceError> {
        if self.status != CheckoutStatus::Created {
            return Err(CommerceError::InvalidStateTransition);
        }
        if at_ms >= self.expires_at_ms {
            return Err(CommerceError::CheckoutExpired);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub struct EventKey {
    source: String,
    idempotency_key: String,
}

impl EventKey {
    pub fn new(
        source: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Result<Self, CommerceError> {
        let source = source.into();
        let idempotency_key = idempotency_key.into();
        validate_nonempty_bounded("event_source", &source, 100)?;
        validate_nonempty_bounded("event_idempotency_key", &idempotency_key, 255)?;
        Ok(Self {
            source,
            idempotency_key,
        })
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct EventEnvelope {
    key: EventKey,
    stream: String,
    sequence: u64,
    payload_sha256: [u8; 32],
    occurred_at_ms: i64,
}

impl EventEnvelope {
    pub fn from_payload(
        key: EventKey,
        stream: impl Into<String>,
        sequence: u64,
        payload: &[u8],
        occurred_at_ms: i64,
    ) -> Result<Self, CommerceError> {
        let stream = stream.into();
        validate_nonempty_bounded("event_stream", &stream, 255)?;
        if sequence == 0 {
            return Err(CommerceError::InvalidEventSequence);
        }
        Ok(Self {
            key,
            stream,
            sequence,
            payload_sha256: Sha256::digest(payload).into(),
            occurred_at_ms,
        })
    }

    #[must_use]
    pub const fn key(&self) -> &EventKey {
        &self.key
    }

    #[must_use]
    pub fn stream(&self) -> &str {
        &self.stream
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn payload_sha256(&self) -> &[u8; 32] {
        &self.payload_sha256
    }

    #[must_use]
    pub const fn occurred_at_ms(&self) -> i64 {
        self.occurred_at_ms
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventClassification {
    New,
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventApplication {
    Applied,
    Duplicate,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct EventLedger {
    accepted: BTreeMap<EventKey, AcceptedEventRecord>,
    stream_sequences: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct AcceptedEventRecord {
    stream: String,
    sequence: u64,
    payload_sha256: [u8; 32],
}

impl EventLedger {
    pub fn classify(&self, event: &EventEnvelope) -> Result<EventClassification, CommerceError> {
        if let Some(existing) = self.accepted.get(&event.key) {
            if existing.payload_sha256 != event.payload_sha256 {
                return Err(CommerceError::EventHashConflict);
            }
            if existing.stream == event.stream && existing.sequence == event.sequence {
                return Ok(EventClassification::Duplicate);
            }
            return Err(CommerceError::EventMetadataConflict);
        }
        if self
            .stream_sequences
            .get(&event.stream)
            .is_some_and(|last| event.sequence <= *last)
        {
            return Err(CommerceError::EventOutOfOrder {
                last_sequence: self.stream_sequences[&event.stream],
                incoming_sequence: event.sequence,
            });
        }
        Ok(EventClassification::New)
    }

    pub fn accept(&mut self, event: &EventEnvelope) -> Result<EventApplication, CommerceError> {
        match self.classify(event)? {
            EventClassification::Duplicate => Ok(EventApplication::Duplicate),
            EventClassification::New => {
                self.record(event)?;
                Ok(EventApplication::Applied)
            }
        }
    }

    #[must_use]
    pub fn accepted_count(&self) -> usize {
        self.accepted.len()
    }

    #[must_use]
    pub fn last_sequence(&self, stream: &str) -> Option<u64> {
        self.stream_sequences.get(stream).copied()
    }

    fn record(&mut self, event: &EventEnvelope) -> Result<(), CommerceError> {
        if self.classify(event)? != EventClassification::New {
            return Err(CommerceError::DuplicateEvent);
        }
        self.accepted.insert(
            event.key.clone(),
            AcceptedEventRecord {
                stream: event.stream.clone(),
                sequence: event.sequence,
                payload_sha256: event.payload_sha256,
            },
        );
        self.stream_sequences
            .insert(event.stream.clone(), event.sequence);
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CommerceError {
    #[error("{field} cannot be empty and must be at most {max_len} bytes")]
    InvalidText { field: &'static str, max_len: usize },
    #[error("currency must contain exactly three uppercase ASCII letters")]
    InvalidCurrency,
    #[error("BYO credentials must use an encrypted secret-store reference")]
    InvalidSecretReference,
    #[error("amount exceeds the supported minor-unit range")]
    AmountOutOfRange,
    #[error("amount must be greater than zero")]
    NonPositiveAmount,
    #[error("money currencies do not match")]
    CurrencyMismatch,
    #[error("monetary arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("monetary subtraction underflowed")]
    AmountUnderflow,
    #[error("merchant account is not active")]
    MerchantUnavailable,
    #[error("merchant account lacks required capability: {0:?}")]
    MissingCapability(MerchantCapability),
    #[error("product is not active")]
    ProductNotActive,
    #[error("price is inactive")]
    PriceInactive,
    #[error("product and price do not match")]
    ProductPriceMismatch,
    #[error("price kind does not match the operation")]
    PriceKindMismatch,
    #[error("quantity must be between 1 and {MAX_QUANTITY}")]
    InvalidQuantity,
    #[error("billing interval is invalid")]
    InvalidBillingInterval,
    #[error("billing period is invalid")]
    InvalidBillingPeriod,
    #[error("billing period overlaps the current period")]
    OverlappingBillingPeriod,
    #[error("entitlement key is invalid")]
    InvalidEntitlementKey,
    #[error("order must contain at least one line")]
    EmptyOrder,
    #[error("resource belongs to a different project or aggregate")]
    ProjectMismatch,
    #[error("state transition is invalid")]
    InvalidStateTransition,
    #[error("payment exceeds the order total")]
    PaymentExceedsOrderTotal,
    #[error("capture exceeds the requested amount")]
    CaptureExceedsAuthorized,
    #[error("payment is not captured")]
    PaymentNotCaptured,
    #[error("payment has not been verified as captured")]
    PaymentNotVerified,
    #[error("verified payment is insufficient")]
    InsufficientVerifiedPayment,
    #[error("refund exceeds captured funds not already refunded or reserved")]
    RefundExceedsAvailable,
    #[error("refund does not exist")]
    RefundNotFound,
    #[error("refund is already settled")]
    RefundAlreadySettled,
    #[error("identifier already exists")]
    DuplicateIdentifier,
    #[error("order is not in a verified paid state")]
    OrderNotPaid,
    #[error("checkout kind does not match completion operation")]
    CheckoutKindMismatch,
    #[error("checkout is expired")]
    CheckoutExpired,
    #[error("checkout expiration must be after creation")]
    InvalidCheckoutExpiration,
    #[error("subscription is not entitled")]
    SubscriptionNotEntitled,
    #[error("event sequence must be greater than zero")]
    InvalidEventSequence,
    #[error("idempotency key was reused with a different payload hash")]
    EventHashConflict,
    #[error("idempotency key was reused with different stream ordering metadata")]
    EventMetadataConflict,
    #[error(
        "event is out of order: last sequence {last_sequence}, incoming sequence {incoming_sequence}"
    )]
    EventOutOfOrder {
        last_sequence: u64,
        incoming_sequence: u64,
    },
    #[error("event is already recorded")]
    DuplicateEvent,
}

fn ensure_order_paid(order: &Order, payments: &[&Payment]) -> Result<(), CommerceError> {
    let mut paid = Money::new(order.total.currency.clone(), 0)?;
    for payment in payments {
        if payment.project_id != order.project_id || payment.order_id() != Some(order.id) {
            return Err(CommerceError::ProjectMismatch);
        }
        paid = paid.checked_add(&payment.fulfillable_amount()?)?;
    }
    if paid.minor < order.total.minor {
        return Err(CommerceError::InsufficientVerifiedPayment);
    }
    Ok(())
}

fn validate_quantity(quantity: u32) -> Result<(), CommerceError> {
    if quantity == 0 || quantity > MAX_QUANTITY {
        return Err(CommerceError::InvalidQuantity);
    }
    Ok(())
}

fn require_positive_money(amount: &Money) -> Result<(), CommerceError> {
    if amount.minor == 0 {
        return Err(CommerceError::NonPositiveAmount);
    }
    Ok(())
}

fn validate_expiration(created_at_ms: i64, expires_at_ms: i64) -> Result<(), CommerceError> {
    if expires_at_ms <= created_at_ms {
        return Err(CommerceError::InvalidCheckoutExpiration);
    }
    Ok(())
}

fn validate_bounded(field: &'static str, value: &str, max_len: usize) -> Result<(), CommerceError> {
    if value.len() > max_len {
        return Err(CommerceError::InvalidText { field, max_len });
    }
    Ok(())
}

fn validate_nonempty_bounded(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), CommerceError> {
    if value.trim().is_empty() || value.len() > max_len {
        return Err(CommerceError::InvalidText { field, max_len });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(minor: u64) -> Result<Money, CommerceError> {
        Money::new(Currency::new("USD")?, minor)
    }

    fn active_merchant(project_id: ProjectId) -> Result<MerchantAccount, CommerceError> {
        let mode = MerchantProviderMode::BringYourOwnCredentials {
            credential_reference: SecretReference::new("secret://project/payments")?,
        };
        let mut merchant = MerchantAccount::new(MerchantAccountId::new(), project_id, mode);
        merchant.set_provider_state(
            MerchantAccountStatus::Active,
            [
                MerchantCapability::OneTimePayments,
                MerchantCapability::RecurringPayments,
                MerchantCapability::Refunds,
            ],
        );
        Ok(merchant)
    }

    fn active_product(project_id: ProjectId, name: &str) -> Result<Product, CommerceError> {
        let mut product = Product::new(ProductId::new(), project_id, name, None)?;
        product.activate()?;
        Ok(product)
    }

    fn one_time_price(product: &Product, minor: u64) -> Result<Price, CommerceError> {
        Price::new(PriceId::new(), product, PriceTerms::one_time(usd(minor)?)?)
    }

    fn recurring_price(product: &Product) -> Result<Price, CommerceError> {
        let mut entitlements = BTreeMap::new();
        entitlements.insert(
            EntitlementKey::new("projects.max")?,
            EntitlementValue::Quantity(10),
        );
        Price::new(
            PriceId::new(),
            product,
            PriceTerms::recurring(
                usd(700)?,
                BillingInterval::new(BillingIntervalUnit::Month, 1)?,
                entitlements,
            )?,
        )
    }

    fn order_fixture() -> Result<(MerchantAccount, Order), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Keyboard")?;
        let price = one_time_price(&product, 5_000)?;
        let line = OrderLineSnapshot::from_price(&product, &price, 2)?;
        let order = Order::new(OrderId::new(), &merchant, vec![line], 100)?;
        Ok((merchant, order))
    }

    fn event(
        id: &str,
        stream: &str,
        sequence: u64,
        payload: &[u8],
    ) -> Result<EventEnvelope, CommerceError> {
        EventEnvelope::from_payload(
            EventKey::new("test-provider", id)?,
            stream,
            sequence,
            payload,
            1_000,
        )
    }

    fn captured_payment(order: &Order, sequence: u64) -> Result<Payment, CommerceError> {
        let mut payment = Payment::new(PaymentId::new(), order, order.total.clone(), 110)?;
        let capture = event(
            &format!("capture-{sequence}"),
            &format!("payment-{}", payment.id()),
            sequence,
            b"captured",
        )?;
        payment.record_capture(
            &capture,
            order.total.clone(),
            ProviderReference::new(format!("pay-{sequence}"))?,
            120,
        )?;
        Ok(payment)
    }

    #[test]
    fn currency_and_amount_validation_is_strict() -> Result<(), CommerceError> {
        assert_eq!(Currency::new("usd"), Err(CommerceError::InvalidCurrency));
        assert_eq!(Currency::new("US"), Err(CommerceError::InvalidCurrency));
        assert_eq!(Currency::new("US1"), Err(CommerceError::InvalidCurrency));
        assert_eq!(
            Money::new(Currency::new("USD")?, MAX_MINOR_AMOUNT + 1),
            Err(CommerceError::AmountOutOfRange)
        );
        assert_eq!(
            Money::positive(Currency::new("USD")?, 0),
            Err(CommerceError::NonPositiveAmount)
        );
        assert_eq!(
            usd(10)?.checked_add(&Money::new(Currency::new("EUR")?, 1)?),
            Err(CommerceError::CurrencyMismatch)
        );
        assert_eq!(
            usd(MAX_MINOR_AMOUNT)?.checked_add(&usd(1)?),
            Err(CommerceError::AmountOutOfRange)
        );
        assert_eq!(
            usd(1)?.checked_sub(&usd(2)?),
            Err(CommerceError::AmountUnderflow)
        );
        assert_eq!(
            usd(MAX_MINOR_AMOUNT)?.checked_mul(2),
            Err(CommerceError::AmountOutOfRange)
        );
        Ok(())
    }

    #[test]
    fn interval_entitlement_and_quantity_values_are_validated() -> Result<(), CommerceError> {
        assert_eq!(
            BillingInterval::new(BillingIntervalUnit::Month, 0),
            Err(CommerceError::InvalidBillingInterval)
        );
        assert_eq!(
            BillingInterval::new(BillingIntervalUnit::Day, 366),
            Err(CommerceError::InvalidBillingInterval)
        );
        assert_eq!(
            EntitlementKey::new("Projects.Max"),
            Err(CommerceError::InvalidEntitlementKey)
        );
        assert_eq!(
            EntitlementKey::new("1project"),
            Err(CommerceError::InvalidEntitlementKey)
        );
        assert!(EntitlementKey::new("project.members:max").is_ok());
        assert_eq!(usd(1)?.checked_mul(0), Err(CommerceError::InvalidQuantity));
        assert_eq!(
            usd(1)?.checked_mul(MAX_QUANTITY + 1),
            Err(CommerceError::InvalidQuantity)
        );
        assert_eq!(
            BillingPeriod::new(10, 10),
            Err(CommerceError::InvalidBillingPeriod)
        );
        Ok(())
    }

    #[test]
    fn provider_modes_are_explicit_mutually_exclusive_references() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let byo = MerchantProviderMode::BringYourOwnCredentials {
            credential_reference: SecretReference::new("secret://project/key")?,
        };
        let connected = MerchantProviderMode::ConnectedAccount {
            account_reference: ProviderReference::new("account://owner/123")?,
            charge_model: ConnectedChargeModel::Direct,
        };
        let byo_merchant = MerchantAccount::new(MerchantAccountId::new(), project_id, byo);
        let connected_merchant =
            MerchantAccount::new(MerchantAccountId::new(), project_id, connected);
        assert!(matches!(
            byo_merchant.provider_mode(),
            MerchantProviderMode::BringYourOwnCredentials { .. }
        ));
        assert!(matches!(
            connected_merchant.provider_mode(),
            MerchantProviderMode::ConnectedAccount {
                charge_model: ConnectedChargeModel::Direct,
                ..
            }
        ));
        assert_eq!(
            connected_merchant.merchant_of_record(),
            MerchantOfRecord::ProjectOwner
        );
        assert!(ProviderReference::new("  ").is_err());
        assert_eq!(
            SecretReference::new("sk_live_plaintext"),
            Err(CommerceError::InvalidSecretReference)
        );
        Ok(())
    }

    #[test]
    fn merchant_status_and_capabilities_gate_operations() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let mode = MerchantProviderMode::ConnectedAccount {
            account_reference: ProviderReference::new("acct-reference")?,
            charge_model: ConnectedChargeModel::Direct,
        };
        let mut merchant = MerchantAccount::new(MerchantAccountId::new(), project_id, mode);
        assert_eq!(
            merchant.require_capability(MerchantCapability::OneTimePayments),
            Err(CommerceError::MerchantUnavailable)
        );
        merchant.set_provider_state(
            MerchantAccountStatus::Active,
            [MerchantCapability::RecurringPayments],
        );
        assert_eq!(
            merchant.require_capability(MerchantCapability::OneTimePayments),
            Err(CommerceError::MissingCapability(
                MerchantCapability::OneTimePayments
            ))
        );
        assert!(
            merchant
                .require_capability(MerchantCapability::RecurringPayments)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn product_lifecycle_and_price_creation_are_guarded() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let mut product = Product::new(ProductId::new(), project_id, "Plan", None)?;
        let terms = PriceTerms::one_time(usd(100)?)?;
        assert_eq!(
            Price::new(PriceId::new(), &product, terms.clone()),
            Err(CommerceError::ProductNotActive)
        );
        product.activate()?;
        let mut price = Price::new(PriceId::new(), &product, terms)?;
        assert!(price.is_active());
        price.retire()?;
        assert!(!price.is_active());
        assert_eq!(price.retire(), Err(CommerceError::InvalidStateTransition));
        product.archive()?;
        assert_eq!(
            product.activate(),
            Err(CommerceError::InvalidStateTransition)
        );
        Ok(())
    }

    #[test]
    fn order_lines_are_immutable_monetary_snapshots() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let mut product = active_product(project_id, "Original name")?;
        let mut price = one_time_price(&product, 250)?;
        let line = OrderLineSnapshot::from_price(&product, &price, 4)?;
        price.retire()?;
        product.archive()?;
        let order = Order::new(OrderId::new(), &merchant, vec![line], 1)?;
        assert_eq!(order.lines()[0].product_name(), "Original name");
        assert_eq!(order.lines()[0].unit_amount().minor(), 250);
        assert_eq!(order.lines()[0].total().minor(), 1_000);
        assert_eq!(order.total().minor(), 1_000);
        Ok(())
    }

    #[test]
    fn orders_reject_empty_mixed_currency_and_recurring_lines() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        assert_eq!(
            Order::new(OrderId::new(), &merchant, Vec::new(), 0),
            Err(CommerceError::EmptyOrder)
        );
        let product = active_product(project_id, "Subscription")?;
        let price = recurring_price(&product)?;
        assert_eq!(
            OrderLineSnapshot::from_price(&product, &price, 1),
            Err(CommerceError::PriceKindMismatch)
        );
        assert_eq!(
            OrderLineSnapshot::from_price(&product, &price, 0),
            Err(CommerceError::InvalidQuantity)
        );

        let usd_product = active_product(project_id, "USD item")?;
        let usd_price = one_time_price(&usd_product, 100)?;
        let usd_line = OrderLineSnapshot::from_price(&usd_product, &usd_price, 1)?;
        let eur_product = active_product(project_id, "EUR item")?;
        let eur_price = Price::new(
            PriceId::new(),
            &eur_product,
            PriceTerms::one_time(Money::positive(Currency::new("EUR")?, 100)?)?,
        )?;
        let eur_line = OrderLineSnapshot::from_price(&eur_product, &eur_price, 1)?;
        assert_eq!(
            Order::new(OrderId::new(), &merchant, vec![usd_line, eur_line], 0),
            Err(CommerceError::CurrencyMismatch)
        );

        let other_project = ProjectId::new();
        let other_product = active_product(other_project, "Foreign item")?;
        let other_price = one_time_price(&other_product, 100)?;
        let other_line = OrderLineSnapshot::from_price(&other_product, &other_price, 1)?;
        assert_eq!(
            Order::new(OrderId::new(), &merchant, vec![other_line], 0),
            Err(CommerceError::ProjectMismatch)
        );
        Ok(())
    }

    #[test]
    fn event_ledger_handles_duplicate_conflict_and_ordering() -> Result<(), CommerceError> {
        let first = event("evt-1", "subscription-1", 1, b"first")?;
        let duplicate = event("evt-1", "subscription-1", 1, b"first")?;
        let conflict = event("evt-1", "subscription-1", 1, b"changed")?;
        let metadata_conflict = event("evt-1", "subscription-1", 2, b"first")?;
        let stale = event("evt-2", "subscription-1", 1, b"stale")?;
        let next = event("evt-3", "subscription-1", 2, b"next")?;
        let other_stream = event("evt-4", "payment-1", 1, b"other")?;
        let mut ledger = EventLedger::default();
        assert_eq!(ledger.accept(&first)?, EventApplication::Applied);
        assert_eq!(ledger.accept(&duplicate)?, EventApplication::Duplicate);
        assert_eq!(
            ledger.accept(&conflict),
            Err(CommerceError::EventHashConflict)
        );
        assert_eq!(
            ledger.accept(&metadata_conflict),
            Err(CommerceError::EventMetadataConflict)
        );
        assert_eq!(
            ledger.accept(&stale),
            Err(CommerceError::EventOutOfOrder {
                last_sequence: 1,
                incoming_sequence: 1
            })
        );
        assert_eq!(ledger.accept(&next)?, EventApplication::Applied);
        assert_eq!(ledger.accept(&other_stream)?, EventApplication::Applied);
        assert_eq!(ledger.accepted_count(), 3);
        assert_eq!(ledger.last_sequence("subscription-1"), Some(2));
        assert_eq!(
            EventEnvelope::from_payload(EventKey::new("x", "y")?, "s", 0, b"", 0),
            Err(CommerceError::InvalidEventSequence)
        );
        Ok(())
    }

    #[test]
    fn payment_capture_requires_valid_amount_and_is_idempotent() -> Result<(), CommerceError> {
        let (_, order) = order_fixture()?;
        let mut payment = Payment::new(PaymentId::new(), &order, order.total.clone(), 1)?;
        payment.mark_authorized()?;
        let capture = event("capture", "payment", 1, b"10000")?;
        assert_eq!(
            payment.record_capture(
                &capture,
                usd(order.total.minor() + 1)?,
                ProviderReference::new("pay")?,
                2
            ),
            Err(CommerceError::CaptureExceedsAuthorized)
        );
        assert_eq!(
            payment.record_capture(
                &capture,
                order.total.clone(),
                ProviderReference::new("pay")?,
                2
            )?,
            EventApplication::Applied
        );
        assert_eq!(
            payment.record_capture(
                &capture,
                order.total.clone(),
                ProviderReference::new("pay")?,
                2
            )?,
            EventApplication::Duplicate
        );
        assert_eq!(payment.status(), PaymentStatus::Captured);
        Ok(())
    }

    #[test]
    fn partial_and_full_refunds_are_bounded_and_reserved() -> Result<(), CommerceError> {
        let (merchant, order) = order_fixture()?;
        let mut payment = captured_payment(&order, 1)?;
        let first_id = RefundId::new();
        payment.request_refund(&merchant, first_id, usd(3_000)?, 200)?;
        assert_eq!(payment.refundable_amount()?.minor(), 7_000);
        assert_eq!(
            payment.request_refund(&merchant, RefundId::new(), usd(7_001)?, 201),
            Err(CommerceError::RefundExceedsAvailable)
        );
        let failed = event("refund-failed", "payment-refunds", 1, b"failed")?;
        payment.settle_refund(first_id, &failed, RefundSettlement::Failed, 202)?;
        assert_eq!(payment.refundable_amount()?.minor(), 10_000);

        let partial_id = RefundId::new();
        payment.request_refund(&merchant, partial_id, usd(4_000)?, 203)?;
        let partial = event("refund-partial", "payment-refunds", 2, b"succeeded")?;
        payment.settle_refund(
            partial_id,
            &partial,
            RefundSettlement::Succeeded {
                provider_reference: ProviderReference::new("refund-1")?,
            },
            204,
        )?;
        assert_eq!(payment.status(), PaymentStatus::PartiallyRefunded);
        assert_eq!(payment.refundable_amount()?.minor(), 6_000);

        let rest_id = RefundId::new();
        payment.request_refund(&merchant, rest_id, usd(6_000)?, 205)?;
        let rest = event("refund-rest", "payment-refunds", 3, b"succeeded")?;
        payment.settle_refund(
            rest_id,
            &rest,
            RefundSettlement::Succeeded {
                provider_reference: ProviderReference::new("refund-2")?,
            },
            206,
        )?;
        assert_eq!(payment.status(), PaymentStatus::Refunded);
        assert_eq!(payment.refundable_amount()?.minor(), 0);
        assert_eq!(
            payment.request_refund(&merchant, RefundId::new(), usd(1)?, 207),
            Err(CommerceError::RefundExceedsAvailable)
        );
        Ok(())
    }

    #[test]
    fn refunds_require_the_same_capable_merchant_and_unique_ids() -> Result<(), CommerceError> {
        let (merchant, order) = order_fixture()?;
        let mut payment = captured_payment(&order, 1)?;
        let id = RefundId::new();
        payment.request_refund(&merchant, id, usd(1)?, 1)?;
        assert_eq!(
            payment.request_refund(&merchant, id, usd(1)?, 2),
            Err(CommerceError::DuplicateIdentifier)
        );
        let other = active_merchant(ProjectId::new())?;
        assert_eq!(
            payment.request_refund(&other, RefundId::new(), usd(1)?, 3),
            Err(CommerceError::ProjectMismatch)
        );
        Ok(())
    }

    #[test]
    fn fulfillment_requires_current_verified_net_payment() -> Result<(), CommerceError> {
        let (merchant, mut order) = order_fixture()?;
        let mut unverified = Payment::new(PaymentId::new(), &order, order.total.clone(), 1)?;
        assert_eq!(
            order.verify_paid(&[&unverified]),
            Err(CommerceError::PaymentNotVerified)
        );
        let capture = event("capture", "payment", 1, b"capture")?;
        unverified.record_capture(
            &capture,
            order.total.clone(),
            ProviderReference::new("payment")?,
            2,
        )?;
        order.verify_paid(&[&unverified])?;

        let refund_id = RefundId::new();
        unverified.request_refund(&merchant, refund_id, order.total.clone(), 3)?;
        assert_eq!(
            order.begin_fulfillment(&[&unverified]),
            Err(CommerceError::InsufficientVerifiedPayment)
        );
        let failed = event("refund-failed", "refund", 1, b"failed")?;
        unverified.settle_refund(refund_id, &failed, RefundSettlement::Failed, 4)?;
        order.begin_fulfillment(&[&unverified])?;
        order.mark_fulfilled(5)?;
        assert_eq!(order.status(), OrderStatus::Fulfilled);
        assert_eq!(order.fulfilled_at_ms(), Some(5));
        Ok(())
    }

    #[test]
    fn multiple_verified_partial_payments_can_pay_one_order() -> Result<(), CommerceError> {
        let (_, mut order) = order_fixture()?;
        let first_amount = usd(4_000)?;
        let second_amount = usd(6_000)?;
        let mut first = Payment::new(PaymentId::new(), &order, first_amount.clone(), 1)?;
        let mut second = Payment::new(PaymentId::new(), &order, second_amount.clone(), 1)?;
        first.record_capture(
            &event("capture-1", "payment-1", 1, b"4000")?,
            first_amount,
            ProviderReference::new("payment-1")?,
            2,
        )?;
        second.record_capture(
            &event("capture-2", "payment-2", 1, b"6000")?,
            second_amount,
            ProviderReference::new("payment-2")?,
            2,
        )?;
        assert_eq!(
            order.verify_paid(&[&first]),
            Err(CommerceError::InsufficientVerifiedPayment)
        );
        order.verify_paid(&[&first, &second])?;
        assert_eq!(order.status(), OrderStatus::Paid);
        Ok(())
    }

    #[test]
    fn recurring_prices_snapshot_all_membership_subject_types() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Membership")?;
        let price = recurring_price(&product)?;
        let subjects = [
            MembershipSubject::Individual(IndividualId::new()),
            MembershipSubject::Team(TeamId::new()),
            MembershipSubject::Organization(SubjectOrganizationId::new()),
        ];
        for subject in subjects {
            let subscription = Subscription::new(
                SubscriptionId::new(),
                &merchant,
                &price,
                subject.clone(),
                2,
                0,
            )?;
            assert_eq!(subscription.subject(), &subject);
            assert_eq!(subscription.terms().quantity(), 2);
            assert_eq!(subscription.terms().unit_amount().minor(), 700);
        }
        Ok(())
    }

    #[test]
    fn recurring_subscription_payments_use_the_terms_snapshot() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Membership")?;
        let price = recurring_price(&product)?;
        let subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Team(TeamId::new()),
            3,
            0,
        )?;
        let mut payment = Payment::for_subscription(PaymentId::new(), &merchant, &subscription, 1)?;
        assert_eq!(payment.subscription_id(), Some(subscription.id()));
        assert_eq!(payment.order_id(), None);
        assert_eq!(payment.requested_amount().minor(), 2_100);
        let capture = event("renewal-payment", "subscription-payment", 1, b"captured")?;
        let requested_amount = payment.requested_amount().clone();
        payment.record_capture(
            &capture,
            requested_amount,
            ProviderReference::new("recurring-payment")?,
            2,
        )?;
        let refund_id = RefundId::new();
        payment.request_refund(&merchant, refund_id, usd(100)?, 3)?;
        assert_eq!(payment.refundable_amount()?.minor(), 2_000);
        Ok(())
    }

    #[test]
    fn subscription_lifecycle_controls_entitlements() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Pro")?;
        let price = recurring_price(&product)?;
        let mut subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Team(TeamId::new()),
            1,
            0,
        )?;
        let key = EntitlementKey::new("projects.max")?;
        assert_eq!(subscription.entitlement(&key, 10), None);
        let trial = event("trial", "subscription", 1, b"trial")?;
        subscription.apply_event(
            &trial,
            SubscriptionTransition::StartTrial {
                period: BillingPeriod::new(0, 100)?,
            },
            0,
        )?;
        assert_eq!(
            subscription.entitlement(&key, 99),
            Some(&EntitlementValue::Quantity(10))
        );
        assert_eq!(subscription.entitlement(&key, 100), None);

        let active = event("active", "subscription", 2, b"active")?;
        subscription.apply_event(
            &active,
            SubscriptionTransition::Activate {
                period: BillingPeriod::new(100, 200)?,
            },
            100,
        )?;
        let past_due = event("past-due", "subscription", 3, b"past-due")?;
        subscription.apply_event(&past_due, SubscriptionTransition::MarkPastDue, 150)?;
        assert_eq!(subscription.entitlement(&key, 150), None);
        let recovered = event("recovered", "subscription", 4, b"recovered")?;
        subscription.apply_event(
            &recovered,
            SubscriptionTransition::Activate {
                period: BillingPeriod::new(100, 200)?,
            },
            151,
        )?;
        assert_eq!(
            subscription.entitlement(&key, 151),
            Some(&EntitlementValue::Quantity(10))
        );
        let canceled = event("canceled", "subscription", 5, b"canceled")?;
        subscription.apply_event(&canceled, SubscriptionTransition::Cancel, 160)?;
        assert_eq!(subscription.entitlement(&key, 161), None);
        assert_eq!(subscription.ended_at_ms(), Some(160));
        Ok(())
    }

    #[test]
    fn subscription_events_are_atomic_idempotent_and_ordered() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Pro")?;
        let price = recurring_price(&product)?;
        let mut subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Individual(IndividualId::new()),
            1,
            0,
        )?;
        let invalid = event("invalid", "sub", 1, b"renew")?;
        assert_eq!(
            subscription.apply_event(
                &invalid,
                SubscriptionTransition::Renew {
                    period: BillingPeriod::new(0, 10)?
                },
                0
            ),
            Err(CommerceError::InvalidStateTransition)
        );
        // Invalid mutation did not consume the event or sequence.
        assert_eq!(
            subscription.apply_event(
                &invalid,
                SubscriptionTransition::Activate {
                    period: BillingPeriod::new(0, 10)?
                },
                0
            )?,
            EventApplication::Applied
        );
        assert_eq!(
            subscription.apply_event(
                &invalid,
                SubscriptionTransition::Activate {
                    period: BillingPeriod::new(0, 10)?
                },
                0
            )?,
            EventApplication::Duplicate
        );
        let conflict = event("invalid", "sub", 1, b"different")?;
        assert_eq!(
            subscription.apply_event(&conflict, SubscriptionTransition::Cancel, 1),
            Err(CommerceError::EventHashConflict)
        );
        let stale = event("stale", "sub", 1, b"stale")?;
        assert_eq!(
            subscription.apply_event(&stale, SubscriptionTransition::Cancel, 1),
            Err(CommerceError::EventOutOfOrder {
                last_sequence: 1,
                incoming_sequence: 1
            })
        );
        Ok(())
    }

    #[test]
    fn subscription_pause_renewal_and_terminal_rules_are_enforced() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Pro")?;
        let price = recurring_price(&product)?;
        let mut subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Individual(IndividualId::new()),
            1,
            0,
        )?;
        subscription.apply_event(
            &event("active", "sub-lifecycle", 1, b"active")?,
            SubscriptionTransition::Activate {
                period: BillingPeriod::new(0, 100)?,
            },
            0,
        )?;
        assert_eq!(
            subscription.apply_event(
                &event("overlap", "sub-lifecycle", 2, b"overlap")?,
                SubscriptionTransition::Renew {
                    period: BillingPeriod::new(99, 200)?
                },
                99
            ),
            Err(CommerceError::OverlappingBillingPeriod)
        );
        // The rejected event did not consume sequence 2.
        subscription.apply_event(
            &event("pause", "sub-lifecycle", 2, b"pause")?,
            SubscriptionTransition::Pause,
            100,
        )?;
        assert_eq!(subscription.status(), SubscriptionStatus::Paused);
        subscription.apply_event(
            &event("resume", "sub-lifecycle", 3, b"resume")?,
            SubscriptionTransition::Activate {
                period: BillingPeriod::new(100, 200)?,
            },
            101,
        )?;
        subscription.apply_event(
            &event("renew", "sub-lifecycle", 4, b"renew")?,
            SubscriptionTransition::Renew {
                period: BillingPeriod::new(200, 300)?,
            },
            200,
        )?;
        subscription.apply_event(
            &event("expire", "sub-lifecycle", 5, b"expire")?,
            SubscriptionTransition::Expire,
            300,
        )?;
        assert_eq!(subscription.status(), SubscriptionStatus::Expired);
        assert_eq!(
            subscription.apply_event(
                &event("reactivate", "sub-lifecycle", 6, b"reactivate")?,
                SubscriptionTransition::Activate {
                    period: BillingPeriod::new(300, 400)?
                },
                301
            ),
            Err(CommerceError::InvalidStateTransition)
        );
        Ok(())
    }

    #[test]
    fn one_time_checkout_requires_verified_payment_and_not_expired() -> Result<(), CommerceError> {
        let (merchant, order) = order_fixture()?;
        let mut checkout =
            CheckoutIntent::one_time(CheckoutIntentId::new(), &merchant, &order, 0, 100)?;
        let pending = Payment::new(PaymentId::new(), &order, order.total.clone(), 1)?;
        assert_eq!(
            checkout.complete_one_time(&pending, 2),
            Err(CommerceError::PaymentNotVerified)
        );
        let payment = captured_payment(&order, 1)?;
        assert_eq!(
            checkout.complete_one_time(&payment, 100),
            Err(CommerceError::CheckoutExpired)
        );
        checkout.complete_one_time(&payment, 99)?;
        assert_eq!(
            checkout.status(),
            &CheckoutStatus::Completed {
                outcome: CheckoutOutcome::Payment {
                    payment_id: payment.id()
                }
            }
        );
        Ok(())
    }

    #[test]
    fn recurring_checkout_requires_entitled_matching_subscription() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let product = active_product(project_id, "Pro")?;
        let price = recurring_price(&product)?;
        let mut subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Organization(SubjectOrganizationId::new()),
            1,
            0,
        )?;
        let mut checkout =
            CheckoutIntent::recurring(CheckoutIntentId::new(), &merchant, &subscription, 0, 100)?;
        assert_eq!(
            checkout.complete_recurring(&subscription, 1),
            Err(CommerceError::SubscriptionNotEntitled)
        );
        let active = event("active", "subscription", 1, b"active")?;
        subscription.apply_event(
            &active,
            SubscriptionTransition::Activate {
                period: BillingPeriod::new(0, 1_000)?,
            },
            1,
        )?;
        checkout.complete_recurring(&subscription, 2)?;
        assert!(matches!(
            checkout.status(),
            CheckoutStatus::Completed {
                outcome: CheckoutOutcome::Subscription { .. }
            }
        ));
        Ok(())
    }

    #[test]
    fn checkout_kind_cancel_and_expiration_transitions_are_enforced() -> Result<(), CommerceError> {
        let (merchant, order) = order_fixture()?;
        assert_eq!(
            CheckoutIntent::one_time(CheckoutIntentId::new(), &merchant, &order, 10, 10),
            Err(CommerceError::InvalidCheckoutExpiration)
        );
        let mut checkout =
            CheckoutIntent::one_time(CheckoutIntentId::new(), &merchant, &order, 0, 10)?;
        checkout.cancel()?;
        assert_eq!(
            checkout.cancel(),
            Err(CommerceError::InvalidStateTransition)
        );

        let mut expiring =
            CheckoutIntent::one_time(CheckoutIntentId::new(), &merchant, &order, 0, 10)?;
        assert_eq!(
            expiring.expire(9),
            Err(CommerceError::InvalidStateTransition)
        );
        expiring.expire(10)?;
        assert_eq!(expiring.status(), &CheckoutStatus::Expired);

        let product = active_product(merchant.project_id(), "Recurring")?;
        let price = recurring_price(&product)?;
        let subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Individual(IndividualId::new()),
            1,
            0,
        )?;
        let mut recurring =
            CheckoutIntent::recurring(CheckoutIntentId::new(), &merchant, &subscription, 0, 10)?;
        let payment = captured_payment(&order, 1)?;
        assert_eq!(
            recurring.complete_one_time(&payment, 1),
            Err(CommerceError::CheckoutKindMismatch)
        );
        Ok(())
    }

    #[test]
    fn snapshots_and_provider_modes_serialize_without_secrets() -> Result<(), CommerceError> {
        let project_id = ProjectId::new();
        let merchant = active_merchant(project_id)?;
        let json =
            serde_json::to_string(&merchant).map_err(|_| CommerceError::InvalidStateTransition)?;
        assert!(json.contains("secret://project/payments"));
        assert!(!json.contains("sk_live"));

        let product = active_product(project_id, "Pro")?;
        let price = recurring_price(&product)?;
        let subscription = Subscription::new(
            SubscriptionId::new(),
            &merchant,
            &price,
            MembershipSubject::Individual(IndividualId::new()),
            1,
            0,
        )?;
        let encoded = serde_json::to_string(&subscription)
            .map_err(|_| CommerceError::InvalidStateTransition)?;
        assert!(encoded.contains("projects.max"));
        Ok(())
    }
}
