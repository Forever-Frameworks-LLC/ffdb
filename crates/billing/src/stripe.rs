use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use ffdb_protocol::{BillingRedirect, OrganizationId, PlatformBillingTier, PlatformBillingUnit};
use hmac::{Hmac, Mac as _};
use reqwest::{Client, header, redirect::Policy};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use url::Url;
use uuid::Uuid;

use crate::{
    BillingError, PlatformBillingProvider, PlatformBillingUpdate, PlatformCheckoutInput,
    PlatformInvoiceUpdate, PlatformPortalInput, ProviderInvoiceStatus, ProviderSubscriptionStatus,
    STRIPE_API_VERSION, UsageMeterEvent, UsageMetric, UsageSummary, UsageSummaryInput,
    VerifiedBillingEvent,
};

type HmacSha256 = Hmac<Sha256>;
const SIGNATURE_TOLERANCE_SECONDS: u64 = 300;

pub struct StripeBillingConfig {
    pub secret_key: SecretString,
    pub webhook_secret: SecretString,
    /// Connected account used for direct platform-billing charges. `None`
    /// sends requests against the authenticated Stripe account itself.
    pub connected_account: Option<String>,
    pub pro_base_price_id: String,
    pub usage_meters: Vec<StripeUsageMeterConfig>,
    pub pro_billing_unit: PlatformBillingUnit,
    pub success_url: Url,
    pub cancel_url: Url,
    pub portal_return_url: Url,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripeUsageMeterConfig {
    pub metric: UsageMetric,
    pub event_name: String,
    pub meter_id: String,
    pub payg_price_id: String,
    pub pro_price_id: String,
}

impl std::fmt::Debug for StripeBillingConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StripeBillingConfig")
            .field("secret_key", &"[REDACTED]")
            .field("webhook_secret", &"[REDACTED]")
            .field("connected_account", &self.connected_account)
            .field("pro_base_price_id", &self.pro_base_price_id)
            .field("usage_meters", &self.usage_meters)
            .field("pro_billing_unit", &self.pro_billing_unit)
            .field("success_url", &self.success_url)
            .field("cancel_url", &self.cancel_url)
            .field("portal_return_url", &self.portal_return_url)
            .finish()
    }
}

pub struct StripeBillingProvider {
    client: Client,
    api_base: Url,
    config: StripeBillingConfig,
}

impl std::fmt::Debug for StripeBillingProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StripeBillingProvider")
            .field("api_base", &self.api_base)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl StripeBillingProvider {
    pub fn new(config: StripeBillingConfig) -> Result<Self, BillingError> {
        validate_config(&config)?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| BillingError::InvalidConfiguration)?;
        Ok(Self {
            client,
            api_base: Url::parse("https://api.stripe.com/")
                .map_err(|_| BillingError::InvalidConfiguration)?,
            config,
        })
    }

    fn meter(&self, metric: UsageMetric) -> Result<&StripeUsageMeterConfig, BillingError> {
        self.config
            .usage_meters
            .iter()
            .find(|meter| meter.metric == metric)
            .ok_or(BillingError::InvalidConfiguration)
    }

    async fn post_form(
        &self,
        path: &str,
        parameters: &[(String, String)],
        idempotency_key: &str,
        expected_host: &str,
    ) -> Result<BillingRedirect, BillingError> {
        validate_idempotency_key(idempotency_key)?;
        let endpoint = self
            .api_base
            .join(path)
            .map_err(|_| BillingError::InvalidConfiguration)?;
        let body = {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            serializer.extend_pairs(parameters.iter().map(|(key, value)| (key, value)));
            serializer.finish()
        };
        let mut request = self
            .client
            .post(endpoint)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.config.secret_key.expose_secret()),
            )
            .header("Stripe-Version", STRIPE_API_VERSION)
            .header("Idempotency-Key", idempotency_key)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body);
        if let Some(account_id) = self.config.connected_account.as_deref() {
            request = request.header("Stripe-Account", account_id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| BillingError::ProviderUnavailable)?;
        if !response.status().is_success() {
            return Err(if response.status().is_server_error() {
                BillingError::ProviderUnavailable
            } else {
                BillingError::ProviderRejected
            });
        }
        let payload: StripeRedirectResponse = response
            .json()
            .await
            .map_err(|_| BillingError::ProviderUnavailable)?;
        let url = Url::parse(&payload.url).map_err(|_| BillingError::ProviderUnavailable)?;
        if url.scheme() != "https" || url.host_str() != Some(expected_host) {
            return Err(BillingError::ProviderUnavailable);
        }
        Ok(BillingRedirect { url: url.into() })
    }

    fn parse_event(&self, payload: &[u8]) -> Result<VerifiedBillingEvent, BillingError> {
        let event: StripeEvent =
            serde_json::from_slice(payload).map_err(|_| BillingError::InvalidWebhookPayload)?;
        if !valid_provider_id(&event.id, "evt_") || event.event_type.len() > 128 {
            return Err(BillingError::InvalidWebhookPayload);
        }
        match self.config.connected_account.as_deref() {
            Some(expected) if event.account.as_deref() != Some(expected) => {
                return Err(BillingError::InvalidWebhookPayload);
            }
            None if event.account.is_some() => {
                return Err(BillingError::InvalidWebhookPayload);
            }
            Some(_) | None => {}
        }
        if event.livemode
            != self
                .config
                .secret_key
                .expose_secret()
                .starts_with("sk_live_")
        {
            return Err(BillingError::InvalidWebhookPayload);
        }
        let created_at_ms = event
            .created
            .checked_mul(1_000)
            .ok_or(BillingError::InvalidWebhookPayload)?;
        let platform_update = match event.event_type.as_str() {
            "checkout.session.completed" => self.checkout_update(&event.data.object)?,
            "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted"
            | "customer.subscription.paused"
            | "customer.subscription.resumed" => {
                self.subscription_update(&event.data.object, &event.event_type)?
            }
            _ => None,
        };
        let invoice_update = match event.event_type.as_str() {
            "invoice.created"
            | "invoice.finalized"
            | "invoice.paid"
            | "invoice.payment_failed"
            | "invoice.voided"
            | "invoice.marked_uncollectible" => {
                self.invoice_update(&event.data.object, &event.event_type)?
            }
            _ => None,
        };
        Ok(VerifiedBillingEvent {
            provider_event_id: event.id,
            event_type: event.event_type,
            livemode: event.livemode,
            created_at_ms,
            platform_update,
            invoice_update,
        })
    }

    fn checkout_update(
        &self,
        object: &Value,
    ) -> Result<Option<PlatformBillingUpdate>, BillingError> {
        let Some(metadata) = platform_metadata(object)? else {
            return Ok(None);
        };
        let organization_id = organization_from_metadata(metadata)?;
        let tier = tier_from_metadata(metadata)?;
        if tier == PlatformBillingTier::Free {
            return Err(BillingError::InvalidWebhookPayload);
        }
        let customer_id = provider_reference(object.get("customer"), "cus_")?;
        let subscription_id = provider_reference_optional(object.get("subscription"), "sub_")?;
        Ok(Some(PlatformBillingUpdate {
            organization_id,
            customer_id,
            subscription_id,
            tier,
            status: ProviderSubscriptionStatus::CheckoutPending,
            quantity: 1,
            current_period_start_ms: None,
            current_period_end_ms: None,
            cancel_at_period_end: false,
        }))
    }

    fn subscription_update(
        &self,
        object: &Value,
        event_type: &str,
    ) -> Result<Option<PlatformBillingUpdate>, BillingError> {
        let Some(metadata) = platform_metadata(object)? else {
            return Ok(None);
        };
        let organization_id = organization_from_metadata(metadata)?;
        let customer_id = provider_reference(object.get("customer"), "cus_")?;
        let subscription_id = provider_reference(object.get("id"), "sub_")?;
        let tier = tier_from_metadata(metadata)?;
        if tier == PlatformBillingTier::Free {
            return Err(BillingError::InvalidWebhookPayload);
        }
        let items = object
            .pointer("/items/data")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or(BillingError::InvalidWebhookPayload)?;
        let mut actual_prices = HashSet::with_capacity(items.len());
        let mut base_quantity = 1_u32;
        let mut period_start_seconds: Option<i64> = None;
        let mut period_end_seconds: Option<i64> = None;
        for item in items {
            let item = item
                .as_object()
                .ok_or(BillingError::InvalidWebhookPayload)?;
            let price_id = item
                .get("price")
                .and_then(|price| provider_reference(Some(price), "price_").ok())
                .ok_or(BillingError::InvalidWebhookPayload)?;
            if !actual_prices.insert(price_id.clone()) {
                return Err(BillingError::InvalidWebhookPayload);
            }
            if price_id == self.config.pro_base_price_id {
                base_quantity = item
                    .get("quantity")
                    .and_then(Value::as_u64)
                    .and_then(|quantity| u32::try_from(quantity).ok())
                    .filter(|quantity| (1..=100_000).contains(quantity))
                    .ok_or(BillingError::InvalidWebhookPayload)?;
            }
            if let Some(value) = item.get("current_period_start").and_then(Value::as_i64) {
                period_start_seconds =
                    Some(period_start_seconds.map_or(value, |current| current.min(value)));
            }
            if let Some(value) = item.get("current_period_end").and_then(Value::as_i64) {
                period_end_seconds =
                    Some(period_end_seconds.map_or(value, |current| current.max(value)));
            }
        }
        let expected_prices = expected_prices(&self.config, tier);
        if actual_prices != expected_prices {
            return Err(BillingError::InvalidWebhookPayload);
        }
        let status = if event_type == "customer.subscription.deleted" {
            ProviderSubscriptionStatus::Canceled
        } else {
            provider_status(
                object
                    .get("status")
                    .and_then(Value::as_str)
                    .ok_or(BillingError::InvalidWebhookPayload)?,
            )?
        };
        let quantity = if tier == PlatformBillingTier::Pro {
            base_quantity
        } else {
            1
        };
        let current_period_start_ms = checked_milliseconds(period_start_seconds)?;
        let current_period_end_ms = checked_milliseconds(period_end_seconds)?;
        let cancel_at_period_end = object
            .get("cancel_at_period_end")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Some(PlatformBillingUpdate {
            organization_id,
            customer_id,
            subscription_id: Some(subscription_id),
            tier,
            status,
            quantity,
            current_period_start_ms,
            current_period_end_ms,
            cancel_at_period_end,
        }))
    }

    fn invoice_update(
        &self,
        object: &Value,
        event_type: &str,
    ) -> Result<Option<PlatformInvoiceUpdate>, BillingError> {
        let metadata = object
            .pointer("/parent/subscription_details/metadata")
            .or_else(|| object.pointer("/subscription_details/metadata"))
            .or_else(|| object.get("metadata"))
            .and_then(Value::as_object);
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        if metadata.get("ffdb_scope").and_then(Value::as_str) != Some("platform") {
            return Ok(None);
        }
        let organization_id = organization_from_metadata(metadata)?;
        let invoice_id = provider_reference(object.get("id"), "in_")?;
        let customer_id = provider_reference(object.get("customer"), "cus_")?;
        let subscription_id = provider_reference_optional(
            object
                .pointer("/parent/subscription_details/subscription")
                .or_else(|| object.get("subscription")),
            "sub_",
        )?;
        let provider_status = object.get("status").and_then(Value::as_str);
        let status = match (event_type, provider_status) {
            ("invoice.payment_failed", _) => ProviderInvoiceStatus::PaymentFailed,
            (_, Some("draft")) => ProviderInvoiceStatus::Draft,
            (_, Some("open")) => ProviderInvoiceStatus::Open,
            (_, Some("paid")) => ProviderInvoiceStatus::Paid,
            (_, Some("uncollectible")) => ProviderInvoiceStatus::Uncollectible,
            (_, Some("void")) => ProviderInvoiceStatus::Void,
            _ => return Err(BillingError::InvalidWebhookPayload),
        };
        let currency = object
            .get("currency")
            .and_then(Value::as_str)
            .filter(|value| value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_lowercase()))
            .ok_or(BillingError::InvalidWebhookPayload)?
            .to_owned();
        let amount_due_minor = non_negative_amount(object, "amount_due")?;
        let amount_paid_minor = non_negative_amount(object, "amount_paid")?;
        let period_start_ms =
            checked_milliseconds(object.get("period_start").and_then(Value::as_i64))?;
        let period_end_ms = checked_milliseconds(object.get("period_end").and_then(Value::as_i64))?;
        let hosted_invoice_url = verified_stripe_url(object, "hosted_invoice_url")?;
        let invoice_pdf_url = verified_stripe_url(object, "invoice_pdf")?;
        Ok(Some(PlatformInvoiceUpdate {
            organization_id,
            invoice_id,
            customer_id,
            subscription_id,
            status,
            currency,
            amount_due_minor,
            amount_paid_minor,
            period_start_ms,
            period_end_ms,
            hosted_invoice_url,
            invoice_pdf_url,
        }))
    }
}

#[async_trait]
impl PlatformBillingProvider for StripeBillingProvider {
    async fn create_checkout(
        &self,
        input: &PlatformCheckoutInput,
    ) -> Result<BillingRedirect, BillingError> {
        if input.billing_email.len() > 320
            || !input.billing_email.contains('@')
            || !(1..=100_000).contains(&input.quantity)
        {
            return Err(BillingError::InvalidRequest);
        }
        if input.tier == PlatformBillingTier::Free {
            return Err(BillingError::InvalidRequest);
        }
        let organization_id = input.organization_id.to_string();
        let tier = tier_name(input.tier);
        let mut parameters = vec![
            ("mode".into(), "subscription".into()),
            ("ui_mode".into(), "hosted".into()),
            ("success_url".into(), self.config.success_url.to_string()),
            ("cancel_url".into(), self.config.cancel_url.to_string()),
            ("client_reference_id".into(), organization_id.clone()),
            ("metadata[ffdb_scope]".into(), "platform".into()),
            (
                "metadata[ffdb_organization_id]".into(),
                organization_id.clone(),
            ),
            ("metadata[ffdb_tier]".into(), tier.into()),
            (
                "subscription_data[metadata][ffdb_scope]".into(),
                "platform".into(),
            ),
            (
                "subscription_data[metadata][ffdb_organization_id]".into(),
                organization_id,
            ),
            ("subscription_data[metadata][ffdb_tier]".into(), tier.into()),
        ];
        let mut line_index = 0_usize;
        if input.tier == PlatformBillingTier::Pro {
            parameters.push((
                format!("line_items[{line_index}][price]"),
                self.config.pro_base_price_id.clone(),
            ));
            if self.config.pro_billing_unit == PlatformBillingUnit::Seat {
                parameters.push((
                    format!("line_items[{line_index}][quantity]"),
                    input.quantity.to_string(),
                ));
            }
            line_index += 1;
        }
        for meter in &self.config.usage_meters {
            let price_id = match input.tier {
                PlatformBillingTier::PayAsYouGo => &meter.payg_price_id,
                PlatformBillingTier::Pro => &meter.pro_price_id,
                PlatformBillingTier::Free => return Err(BillingError::InvalidRequest),
            };
            parameters.push((format!("line_items[{line_index}][price]"), price_id.clone()));
            line_index += 1;
        }
        if let Some(customer_id) = &input.existing_customer_id {
            if !valid_provider_id(customer_id, "cus_") {
                return Err(BillingError::InvalidRequest);
            }
            parameters.push(("customer".into(), customer_id.clone()));
        } else {
            parameters.push(("customer_email".into(), input.billing_email.clone()));
        }
        self.post_form(
            "v1/checkout/sessions",
            &parameters,
            &input.idempotency_key,
            "checkout.stripe.com",
        )
        .await
    }

    async fn create_portal(
        &self,
        input: &PlatformPortalInput,
    ) -> Result<BillingRedirect, BillingError> {
        if !valid_provider_id(&input.customer_id, "cus_") {
            return Err(BillingError::InvalidRequest);
        }
        self.post_form(
            "v1/billing_portal/sessions",
            &[
                ("customer".into(), input.customer_id.clone()),
                (
                    "return_url".into(),
                    self.config.portal_return_url.to_string(),
                ),
            ],
            &input.idempotency_key,
            "billing.stripe.com",
        )
        .await
    }

    fn verify_webhook(
        &self,
        payload: &[u8],
        signature: &str,
        now_seconds: i64,
    ) -> Result<VerifiedBillingEvent, BillingError> {
        if signature.len() > 4_096 || payload.len() > 512 * 1_024 {
            return Err(BillingError::InvalidWebhookSignature);
        }
        let mut timestamp = None;
        let mut signatures = Vec::new();
        for field in signature.split(',') {
            let Some((name, value)) = field.trim().split_once('=') else {
                continue;
            };
            match name {
                "t" if timestamp.is_none() => {
                    timestamp = value.parse::<i64>().ok();
                }
                "v1" => signatures.push(value),
                _ => {}
            }
        }
        let timestamp = timestamp.ok_or(BillingError::InvalidWebhookSignature)?;
        if now_seconds.abs_diff(timestamp) > SIGNATURE_TOLERANCE_SECONDS {
            return Err(BillingError::InvalidWebhookSignature);
        }
        let verified = signatures.into_iter().any(|signature| {
            let Some(signature) = decode_hex_32(signature) else {
                return false;
            };
            let Ok(mut mac) = <HmacSha256 as hmac::KeyInit>::new_from_slice(
                self.config.webhook_secret.expose_secret().as_bytes(),
            ) else {
                return false;
            };
            mac.update(timestamp.to_string().as_bytes());
            mac.update(b".");
            mac.update(payload);
            mac.verify_slice(&signature).is_ok()
        });
        if !verified {
            return Err(BillingError::InvalidWebhookSignature);
        }
        self.parse_event(payload)
    }

    async fn report_usage(&self, input: &UsageMeterEvent) -> Result<(), BillingError> {
        if !valid_provider_id(&input.customer_id, "cus_")
            || !(8..=100).contains(&input.identifier.len())
            || !input
                .identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || input.value == 0
            || input.timestamp_ms < 0
        {
            return Err(BillingError::InvalidRequest);
        }
        let timestamp = input.timestamp_ms / 1_000;
        let meter = self.meter(input.metric)?;
        let endpoint = self
            .api_base
            .join("v1/billing/meter_events")
            .map_err(|_| BillingError::InvalidConfiguration)?;
        let mut request = self
            .client
            .post(endpoint)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.config.secret_key.expose_secret()),
            )
            .header("Stripe-Version", STRIPE_API_VERSION)
            .header("Idempotency-Key", &input.identifier)
            .form(&[
                ("event_name", meter.event_name.as_str()),
                ("identifier", input.identifier.as_str()),
                ("payload[stripe_customer_id]", input.customer_id.as_str()),
                ("payload[value]", &input.value.to_string()),
                ("timestamp", &timestamp.to_string()),
            ]);
        if let Some(account_id) = self.config.connected_account.as_deref() {
            request = request.header("Stripe-Account", account_id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| BillingError::ProviderUnavailable)?;
        if response.status().is_success() {
            Ok(())
        } else if response.status().is_server_error()
            || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            Err(BillingError::ProviderUnavailable)
        } else {
            Err(BillingError::ProviderRejected)
        }
    }

    async fn usage_summary(&self, input: &UsageSummaryInput) -> Result<UsageSummary, BillingError> {
        if !valid_provider_id(&input.customer_id, "cus_")
            || input.start_ms < 0
            || input.end_ms <= input.start_ms
        {
            return Err(BillingError::InvalidRequest);
        }
        let meter = self.meter(input.metric)?;
        let endpoint = self
            .api_base
            .join(&format!(
                "v1/billing/meters/{}/event_summaries",
                meter.meter_id
            ))
            .map_err(|_| BillingError::InvalidConfiguration)?;
        let mut request = self
            .client
            .get(endpoint)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.config.secret_key.expose_secret()),
            )
            .header("Stripe-Version", STRIPE_API_VERSION)
            .query(&[
                ("customer", input.customer_id.as_str()),
                ("start_time", &(input.start_ms / 1_000).to_string()),
                ("end_time", &(input.end_ms / 1_000).to_string()),
                ("limit", "100"),
            ]);
        if let Some(account_id) = self.config.connected_account.as_deref() {
            request = request.header("Stripe-Account", account_id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| BillingError::ProviderUnavailable)?;
        if !response.status().is_success() {
            return Err(
                if response.status().is_server_error()
                    || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                {
                    BillingError::ProviderUnavailable
                } else {
                    BillingError::ProviderRejected
                },
            );
        }
        let payload: StripeUsageSummaryList = response
            .json()
            .await
            .map_err(|_| BillingError::ProviderUnavailable)?;
        if payload.has_more {
            return Err(BillingError::ProviderUnavailable);
        }
        let aggregated_value = payload.data.into_iter().try_fold(0_u64, |total, row| {
            if row.aggregated_value < 0.0 || row.aggregated_value.fract() != 0.0 {
                return Err(BillingError::ProviderUnavailable);
            }
            let value = row.aggregated_value as u64;
            total
                .checked_add(value)
                .ok_or(BillingError::ProviderUnavailable)
        })?;
        Ok(UsageSummary {
            metric: input.metric,
            aggregated_value,
            start_ms: input.start_ms,
            end_ms: input.end_ms,
        })
    }
}

#[derive(Deserialize)]
struct StripeRedirectResponse {
    url: String,
}

#[derive(Deserialize)]
struct StripeUsageSummaryList {
    data: Vec<StripeUsageSummaryRow>,
    has_more: bool,
}

#[derive(Deserialize)]
struct StripeUsageSummaryRow {
    aggregated_value: f64,
}

#[derive(Deserialize)]
struct StripeEvent {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    created: i64,
    livemode: bool,
    #[serde(default)]
    account: Option<String>,
    data: StripeEventData,
}

#[derive(Deserialize)]
struct StripeEventData {
    object: Value,
}

fn validate_config(config: &StripeBillingConfig) -> Result<(), BillingError> {
    let metric_set = config
        .usage_meters
        .iter()
        .map(|meter| meter.metric)
        .collect::<HashSet<_>>();
    let mut provider_ids = HashSet::new();
    let meters_valid = config.usage_meters.len() == UsageMetric::ALL.len()
        && metric_set.len() == UsageMetric::ALL.len()
        && UsageMetric::ALL
            .iter()
            .all(|metric| metric_set.contains(metric))
        && config.usage_meters.iter().all(|meter| {
            valid_event_name(&meter.event_name)
                && valid_provider_id(&meter.meter_id, "mtr_")
                && valid_provider_id(&meter.payg_price_id, "price_")
                && valid_provider_id(&meter.pro_price_id, "price_")
                && provider_ids.insert(meter.meter_id.as_str())
                && provider_ids.insert(meter.payg_price_id.as_str())
                && provider_ids.insert(meter.pro_price_id.as_str())
        });
    if !config.secret_key.expose_secret().starts_with("sk_")
        || config.secret_key.expose_secret().len() < 16
        || !config.webhook_secret.expose_secret().starts_with("whsec_")
        || config.webhook_secret.expose_secret().len() < 16
        || !valid_provider_id(&config.pro_base_price_id, "price_")
        || config
            .connected_account
            .as_deref()
            .is_some_and(|value| !valid_provider_id(value, "acct_"))
        || !provider_ids.insert(config.pro_base_price_id.as_str())
        || !meters_valid
        || !valid_return_url(&config.success_url)
        || !valid_return_url(&config.cancel_url)
        || !valid_return_url(&config.portal_return_url)
    {
        return Err(BillingError::InvalidConfiguration);
    }
    Ok(())
}

fn valid_event_name(value: &str) -> bool {
    (3..=100).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn expected_prices(config: &StripeBillingConfig, tier: PlatformBillingTier) -> HashSet<String> {
    let mut prices = config
        .usage_meters
        .iter()
        .map(|meter| match tier {
            PlatformBillingTier::PayAsYouGo => meter.payg_price_id.clone(),
            PlatformBillingTier::Pro => meter.pro_price_id.clone(),
            PlatformBillingTier::Free => String::new(),
        })
        .collect::<HashSet<_>>();
    if tier == PlatformBillingTier::Pro {
        prices.insert(config.pro_base_price_id.clone());
    }
    prices
}

fn checked_milliseconds(seconds: Option<i64>) -> Result<Option<i64>, BillingError> {
    seconds
        .map(|value| {
            value
                .checked_mul(1_000)
                .ok_or(BillingError::InvalidWebhookPayload)
        })
        .transpose()
}

fn non_negative_amount(object: &Value, key: &str) -> Result<u64, BillingError> {
    object
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(BillingError::InvalidWebhookPayload)
}

fn verified_stripe_url(object: &Value, key: &str) -> Result<Option<String>, BillingError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return if value.is_null() {
            Ok(None)
        } else {
            Err(BillingError::InvalidWebhookPayload)
        };
    };
    let url = Url::parse(raw).map_err(|_| BillingError::InvalidWebhookPayload)?;
    let host = url.host_str().ok_or(BillingError::InvalidWebhookPayload)?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || !(host == "stripe.com" || host.ends_with(".stripe.com"))
    {
        return Err(BillingError::InvalidWebhookPayload);
    }
    Ok(Some(url.into()))
}

fn valid_return_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn validate_idempotency_key(value: &str) -> Result<(), BillingError> {
    if !(8..=255).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(BillingError::InvalidRequest);
    }
    Ok(())
}

fn valid_provider_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && (prefix.len() + 4..=255).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn platform_metadata(
    object: &Value,
) -> Result<Option<&serde_json::Map<String, Value>>, BillingError> {
    let Some(metadata) = object.get("metadata").and_then(Value::as_object) else {
        return Ok(None);
    };
    match metadata.get("ffdb_scope").and_then(Value::as_str) {
        Some("platform") => Ok(Some(metadata)),
        Some(_) | None => Ok(None),
    }
}

fn organization_from_metadata(
    metadata: &serde_json::Map<String, Value>,
) -> Result<OrganizationId, BillingError> {
    metadata
        .get("ffdb_organization_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .map(OrganizationId)
        .ok_or(BillingError::InvalidWebhookPayload)
}

fn tier_from_metadata(
    metadata: &serde_json::Map<String, Value>,
) -> Result<PlatformBillingTier, BillingError> {
    match metadata.get("ffdb_tier").and_then(Value::as_str) {
        Some("pay_as_you_go") => Ok(PlatformBillingTier::PayAsYouGo),
        Some("pro") => Ok(PlatformBillingTier::Pro),
        Some("free") => Ok(PlatformBillingTier::Free),
        _ => Err(BillingError::InvalidWebhookPayload),
    }
}

fn provider_reference(value: Option<&Value>, prefix: &str) -> Result<String, BillingError> {
    let value = value.ok_or(BillingError::InvalidWebhookPayload)?;
    let identifier = value
        .as_str()
        .or_else(|| value.get("id").and_then(Value::as_str))
        .ok_or(BillingError::InvalidWebhookPayload)?;
    if !valid_provider_id(identifier, prefix) {
        return Err(BillingError::InvalidWebhookPayload);
    }
    Ok(identifier.to_owned())
}

fn provider_reference_optional(
    value: Option<&Value>,
    prefix: &str,
) -> Result<Option<String>, BillingError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => provider_reference(Some(value), prefix).map(Some),
    }
}

fn provider_status(value: &str) -> Result<ProviderSubscriptionStatus, BillingError> {
    match value {
        "trialing" => Ok(ProviderSubscriptionStatus::Trialing),
        "active" => Ok(ProviderSubscriptionStatus::Active),
        "past_due" => Ok(ProviderSubscriptionStatus::PastDue),
        "unpaid" => Ok(ProviderSubscriptionStatus::Unpaid),
        "canceled" | "incomplete_expired" => Ok(ProviderSubscriptionStatus::Canceled),
        "paused" => Ok(ProviderSubscriptionStatus::Paused),
        "incomplete" => Ok(ProviderSubscriptionStatus::Incomplete),
        _ => Err(BillingError::InvalidWebhookPayload),
    }
}

const fn tier_name(tier: PlatformBillingTier) -> &'static str {
    match tier {
        PlatformBillingTier::Free => "free",
        PlatformBillingTier::PayAsYouGo => "pay_as_you_go",
        PlatformBillingTier::Pro => "pro",
    }
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = hex_nibble(pair[0])?.checked_mul(16)? | hex_nibble(pair[1])?;
    }
    Some(decoded)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> Result<StripeBillingProvider, BillingError> {
        provider_for_account(None)
    }

    fn provider_for_account(
        connected_account: Option<&str>,
    ) -> Result<StripeBillingProvider, BillingError> {
        StripeBillingProvider::new(StripeBillingConfig {
            secret_key: SecretString::from("sk_test_12345678901234567890"), // gitleaks:allow -- synthetic Stripe test fixture
            webhook_secret: SecretString::from("whsec_12345678901234567890"),
            connected_account: connected_account.map(str::to_owned),
            pro_base_price_id: "price_pro_base_1234".into(),
            usage_meters: vec![
                meter(UsageMetric::Reads, "reads"),
                meter(UsageMetric::Writes, "writes"),
                meter(UsageMetric::StorageByteHours, "storage"),
                meter(UsageMetric::MonthlyActiveUsers, "mau"),
            ],
            pro_billing_unit: PlatformBillingUnit::Organization,
            success_url: Url::parse("https://portal.example.test/billing/success")
                .map_err(|_| BillingError::InvalidConfiguration)?,
            cancel_url: Url::parse("https://portal.example.test/billing/cancel")
                .map_err(|_| BillingError::InvalidConfiguration)?,
            portal_return_url: Url::parse("https://portal.example.test/billing")
                .map_err(|_| BillingError::InvalidConfiguration)?,
        })
    }

    fn meter(metric: UsageMetric, suffix: &str) -> StripeUsageMeterConfig {
        StripeUsageMeterConfig {
            metric,
            event_name: format!("ffdb_{suffix}"),
            meter_id: format!("mtr_{suffix}_1234"),
            payg_price_id: format!("price_payg_{suffix}_1234"),
            pro_price_id: format!("price_pro_{suffix}_1234"),
        }
    }

    fn signature(payload: &[u8], timestamp: i64) -> Result<String, BillingError> {
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(b"whsec_12345678901234567890")
            .map_err(|_| BillingError::InvalidConfiguration)?;
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(payload);
        let encoded = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!("t={timestamp},v1={encoded}"))
    }

    #[test]
    fn verifies_raw_payload_and_maps_platform_subscription() -> Result<(), BillingError> {
        let organization = OrganizationId::new();
        let payload = format!(
            r#"{{"id":"evt_123456","type":"customer.subscription.updated","created":1000,"livemode":false,"data":{{"object":{{"id":"sub_123456","customer":"cus_123456","status":"active","cancel_at_period_end":false,"metadata":{{"ffdb_scope":"platform","ffdb_organization_id":"{organization}","ffdb_tier":"pro"}},"items":{{"data":[{{"quantity":1,"current_period_start":1000,"current_period_end":2000,"price":{{"id":"price_pro_base_1234"}}}},{{"quantity":null,"current_period_start":1000,"current_period_end":2000,"price":{{"id":"price_pro_reads_1234"}}}},{{"quantity":null,"current_period_start":1000,"current_period_end":2000,"price":{{"id":"price_pro_writes_1234"}}}},{{"quantity":null,"current_period_start":1000,"current_period_end":2000,"price":{{"id":"price_pro_storage_1234"}}}},{{"quantity":null,"current_period_start":1000,"current_period_end":2000,"price":{{"id":"price_pro_mau_1234"}}}}]}}}}}}}}"#
        );
        let event = provider()?.verify_webhook(
            payload.as_bytes(),
            &signature(payload.as_bytes(), 1_000)?,
            1_000,
        )?;
        let update = event
            .platform_update
            .ok_or(BillingError::InvalidWebhookPayload)?;
        assert_eq!(update.organization_id, organization);
        assert_eq!(update.tier, PlatformBillingTier::Pro);
        assert_eq!(update.status, ProviderSubscriptionStatus::Active);
        assert_eq!(update.current_period_start_ms, Some(1_000_000));
        assert_eq!(update.current_period_end_ms, Some(2_000_000));
        Ok(())
    }

    #[test]
    fn rejects_tampering_and_stale_signatures() -> Result<(), BillingError> {
        let payload = br#"{"id":"evt_123456","type":"ignored","created":1000,"livemode":false,"data":{"object":{}}}"#;
        let valid = signature(payload, 1_000)?;
        assert_eq!(
            provider()?.verify_webhook(b"{}", &valid, 1_000),
            Err(BillingError::InvalidWebhookSignature)
        );
        assert_eq!(
            provider()?.verify_webhook(payload, &valid, 1_301),
            Err(BillingError::InvalidWebhookSignature)
        );
        Ok(())
    }

    #[test]
    fn project_scoped_events_cannot_mutate_platform_billing() -> Result<(), BillingError> {
        let payload = br#"{"id":"evt_123456","type":"checkout.session.completed","created":1000,"livemode":false,"data":{"object":{"metadata":{"ffdb_scope":"project_commerce"}}}}"#;
        let event = provider()?.verify_webhook(payload, &signature(payload, 1_000)?, 1_000)?;
        assert!(event.platform_update.is_none());
        Ok(())
    }

    #[test]
    fn connected_webhooks_are_bound_to_the_configured_account() -> Result<(), BillingError> {
        let matching = br#"{"id":"evt_123456","type":"ignored","created":1000,"livemode":false,"account":"acct_expected_1234","data":{"object":{}}}"#;
        assert!(
            provider_for_account(Some("acct_expected_1234"))?
                .verify_webhook(matching, &signature(matching, 1_000)?, 1_000)
                .is_ok()
        );
        let cross_account = br#"{"id":"evt_123456","type":"ignored","created":1000,"livemode":false,"account":"acct_attacker_1234","data":{"object":{}}}"#;
        assert_eq!(
            provider_for_account(Some("acct_expected_1234"))?.verify_webhook(
                cross_account,
                &signature(cross_account, 1_000)?,
                1_000,
            ),
            Err(BillingError::InvalidWebhookPayload)
        );
        assert_eq!(
            provider()?.verify_webhook(matching, &signature(matching, 1_000)?, 1_000),
            Err(BillingError::InvalidWebhookPayload)
        );
        Ok(())
    }
}
