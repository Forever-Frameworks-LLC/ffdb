//! Project commerce service and Stripe adapter.
//!
//! Platform billing credentials are never used here. Every request resolves a
//! project-owned BYO credential envelope or an explicitly connected account.
//! Connected-account requests always use direct charges through Stripe's
//! `Stripe-Account` header; destination charges and platform transfers are not
//! representable by this module.

use std::collections::BTreeMap;
use std::ops::DerefMut as _;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_audit::{ActorKind, AuditDraft, AuditOutcome};
use ffdb_commerce::{
    BillingInterval, BillingIntervalUnit, Currency, EntitlementKey, EntitlementValue,
    MerchantAccount, MerchantAccountId, MerchantAccountStatus, MerchantCapability,
    MerchantProviderMode, Money, Order, OrderId, OrderLineSnapshot, Price, PriceId, PriceTerms,
    Product, ProductId, ProviderReference, SecretReference, Subscription, SubscriptionId,
};
use ffdb_protocol::{
    BillingRedirect, CancelCommerceSubscriptionRequest, CommerceAccountCapabilities,
    CommerceAccountStatus, CommerceAccountSummary, CommerceBillingIntervalUnit,
    CommerceCheckoutResponse, CommerceCustomerId, CommerceEntitlementSummary,
    CommerceEntitlementValue, CommerceFulfillmentStatus, CommerceMembershipSubject,
    CommerceMembershipSubjectKind, CommerceOnboardingResponse, CommerceOrderId,
    CommerceOrderLineSummary, CommerceOrderStatus, CommerceOrderSummary, CommercePaymentId,
    CommercePaymentStatus, CommercePaymentSummary, CommercePriceBilling, CommercePriceId,
    CommercePriceSummary, CommerceProductId, CommerceProductStatus, CommerceProductSummary,
    CommerceProviderMode, CommerceRefundId, CommerceRefundReason, CommerceRefundStatus,
    CommerceRefundSummary, CommerceSubscriptionId, CommerceSubscriptionStatus,
    CommerceSubscriptionSummary, ConfigureCommerceByoRequest,
    CreateCommerceConnectOnboardingRequest, CreateCommerceCustomerPortalRequest,
    CreateCommercePriceRequest, CreateCommerceProductRequest, CreateCommerceRefundRequest,
    CreateOneTimeCommerceCheckoutRequest, CreateRecurringCommerceCheckoutRequest, DeveloperScope,
    OrganizationId, PROTOCOL_VERSION, ProjectId, RequestId,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};
use secrecy::{ExposeSecret as _, SecretString as ProtectedString};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::idempotency;
use super::{ApiState, CredentialError, credential_error, developer, end_user, now_ms};

const STRIPE_API_VERSION: &str = "2026-02-25.clover";
const WEBHOOK_TOLERANCE_SECONDS: i64 = 300;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_PROVIDER_BODY_BYTES: usize = 1_048_576;

#[derive(Clone)]
pub struct CommerceService {
    pool: PgPool,
    cipher: ProviderSecretEnvelope,
    stripe: StripeRequestClient,
    public_base_url: Url,
    connect: Option<Arc<ConnectCredentials>>,
}

pub struct CommerceServiceConfig {
    pub master_key: Vec<u8>,
    pub key_version: i32,
    pub public_base_url: Url,
    pub connect: Option<CommerceConnectConfig>,
}

pub struct CommerceConnectConfig {
    pub secret_key: ProtectedString,
    pub webhook_secret: ProtectedString,
}

#[derive(Clone)]
struct ConnectCredentials {
    secret_key: ProtectedString,
    webhook_secret: ProtectedString,
}

impl std::fmt::Debug for CommerceServiceConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommerceServiceConfig")
            .field("master_key", &"[REDACTED]")
            .field("key_version", &self.key_version)
            .field("public_base_url", &self.public_base_url)
            .field("connect", &self.connect.as_ref().map(|_| "configured"))
            .finish()
    }
}

impl std::fmt::Debug for CommerceConnectConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommerceConnectConfig")
            .field("secret_key", &"[REDACTED]")
            .field("webhook_secret", &"[REDACTED]")
            .finish()
    }
}

impl std::fmt::Debug for CommerceService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommerceService")
            .field("public_base_url", &self.public_base_url)
            .field("connect", &self.connect.as_ref().map(|_| "configured"))
            .finish_non_exhaustive()
    }
}

impl CommerceService {
    pub fn new(pool: PgPool, config: CommerceServiceConfig) -> Result<Self, CommerceServiceError> {
        if config.public_base_url.host_str().is_none()
            || !matches!(config.public_base_url.scheme(), "http" | "https")
        {
            return Err(CommerceServiceError::InvalidConfiguration);
        }
        Ok(Self {
            pool,
            cipher: ProviderSecretEnvelope::new(config.master_key, config.key_version)?,
            stripe: StripeRequestClient::production()?,
            public_base_url: config.public_base_url,
            connect: config.connect.map(|value| {
                Arc::new(ConnectCredentials {
                    secret_key: value.secret_key,
                    webhook_secret: value.webhook_secret,
                })
            }),
        })
    }

    fn webhook_url(
        &self,
        project_id: ProjectId,
        mode: CommerceProviderMode,
    ) -> Result<String, CommerceServiceError> {
        let path = match mode {
            CommerceProviderMode::BringYourOwnKeys => {
                format!("v1/projects/{project_id}/commerce/webhooks/stripe")
            }
            CommerceProviderMode::StripeConnect => "v1/commerce/webhooks/stripe-connect".to_owned(),
        };
        self.public_base_url
            .join(&path)
            .map(|value| value.to_string())
            .map_err(|_| CommerceServiceError::InvalidConfiguration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct MockStripeState {
        account_id: String,
        requests: Arc<std::sync::Mutex<Vec<(Method, String)>>>,
    }

    async fn mock_stripe(
        State(state): State<MockStripeState>,
        method: Method,
        uri: axum::http::Uri,
    ) -> Response {
        if let Ok(mut requests) = state.requests.lock() {
            requests.push((method.clone(), uri.path().to_owned()));
        }
        match (method, uri.path()) {
            (Method::GET, "/v1/account") => Json(json!({
                "id": state.account_id,
                "charges_enabled": true,
                "details_submitted": true,
                "payouts_enabled": false,
                "capabilities": {"card_payments": "active"},
                "requirements": {"currently_due": []}
            }))
            .into_response(),
            (Method::POST, "/v2/core/accounts") => Json(json!({
                "id": state.account_id,
                "configuration": {"merchant": {"capabilities": {
                    "card_payments": {"status": "active"}
                }}},
                "requirements": {"entries": []}
            }))
            .into_response(),
            (Method::GET, path) if path.starts_with("/v2/core/accounts/") => Json(json!({
                "id": state.account_id,
                "configuration": {"merchant": {"capabilities": {
                    "card_payments": {"status": "active"}
                }}},
                "requirements": {"entries": []}
            }))
            .into_response(),
            (Method::POST, "/v2/core/account_links") => Json(json!({
                "url": "https://connect.stripe.com/setup/test",
                "expires_at": "2030-01-01T00:00:00Z"
            }))
            .into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn spawn_mock_stripe(
        account_id: String,
    ) -> Result<
        (
            Url,
            Arc<std::sync::Mutex<Vec<(Method, String)>>>,
            tokio::task::JoinHandle<()>,
        ),
        Box<dyn std::error::Error>,
    > {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .fallback(mock_stripe)
            .with_state(MockStripeState {
                account_id,
                requests: requests.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok((Url::parse(&format!("http://{address}/"))?, requests, task))
    }

    async fn create_test_project(pool: &PgPool) -> Result<ProjectId, sqlx::Error> {
        let organization_id = Uuid::now_v7();
        let project_id = ProjectId::new();
        let database_id = Uuid::now_v7();
        let route_id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        let suffix = &project_id.to_string()[..12];
        let mut transaction = pool.begin().await?;
        sqlx::query("SET CONSTRAINTS ALL DEFERRED")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO organizations (id,slug,display_name) VALUES ($1,$2,$3)")
            .bind(organization_id)
            .bind(format!("commerce-{suffix}"))
            .bind("Commerce integration test")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO nodes (id,name,lifecycle_state) VALUES ($1,$2,'active')")
            .bind(node_id)
            .bind(format!("commerce-node-{suffix}"))
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO projects \
             (id,organization_id,database_id,slug,display_name,lifecycle_state) \
             VALUES ($1,$2,$3,$4,$5,'active')",
        )
        .bind(project_id.0)
        .bind(organization_id)
        .bind(database_id)
        .bind(format!("commerce-{suffix}"))
        .bind("Commerce integration test")
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO project_databases \
             (id,project_id,route_id,lifecycle_state) VALUES ($1,$2,$3,'active')",
        )
        .bind(database_id)
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
        .bind(database_id)
        .bind(node_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(project_id)
    }

    fn test_service(
        pool: PgPool,
        stripe_base: Url,
        connect: Option<CommerceConnectConfig>,
    ) -> Result<CommerceService, CommerceServiceError> {
        let mut service = CommerceService::new(
            pool,
            CommerceServiceConfig {
                master_key: vec![17; 32],
                key_version: 1,
                public_base_url: Url::parse("https://ffdb.example.test/")
                    .map_err(|_| CommerceServiceError::InvalidConfiguration)?,
                connect,
            },
        )?;
        service.stripe = StripeRequestClient::new(stripe_base)?;
        Ok(service)
    }

    fn stripe_signature(secret: &str, payload: &[u8], timestamp: i64) -> String {
        let mut signed = timestamp.to_string().into_bytes();
        signed.push(b'.');
        signed.extend_from_slice(payload);
        let signature = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()),
            &signed,
        );
        format!(
            "t={timestamp},v1={}",
            signature
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }

    async fn apply_byo_test_event(
        service: &CommerceService,
        project_id: ProjectId,
        secret: &str,
        event_id: &str,
        event_type: &str,
        created: i64,
        object: Value,
    ) -> Result<WebhookOutcome, Box<dyn std::error::Error>> {
        let payload = serde_json::to_vec(&json!({
            "id": event_id,
            "type": event_type,
            "livemode": false,
            "created": created,
            "data": {"object": object}
        }))?;
        let signature = stripe_signature(secret, &payload, created);
        Ok(service
            .apply_byo_webhook(project_id, &payload, &signature, created)
            .await?)
    }

    #[test]
    fn provider_secret_envelope_round_trips_and_is_scope_bound() -> Result<(), CommerceServiceError>
    {
        let envelope = ProviderSecretEnvelope::new(vec![7_u8; 32], 3)?;
        let project = ProjectId::new();
        let sealed = envelope.seal(
            ProviderSecretScope::ProjectCommerce(project),
            "secret_key",
            "sk_test_private",
        )?;
        let packed = sealed.to_packed();
        let unpacked = SealedProviderSecret::from_packed(3, &packed)?;
        assert_eq!(
            envelope
                .open(
                    ProviderSecretScope::ProjectCommerce(project),
                    "secret_key",
                    &unpacked,
                )?
                .expose_secret(),
            "sk_test_private"
        );
        assert!(matches!(
            envelope.open(
                ProviderSecretScope::PlatformInstanceBilling(project.0),
                "secret_key",
                &unpacked,
            ),
            Err(CommerceServiceError::Encryption)
        ));
        assert!(matches!(
            envelope.open(
                ProviderSecretScope::ProjectCommerce(project),
                "webhook_secret",
                &unpacked,
            ),
            Err(CommerceServiceError::Encryption)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn order_collection_loads_each_orders_lines() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        crate::control_plane_migrations::migrator()
            .run(&pool)
            .await?;
        let project_id = create_test_project(&pool).await?;
        let service = test_service(pool.clone(), Url::parse("http://127.0.0.1:9/")?, None)?;
        let product_id = Uuid::now_v7();
        let price_id = Uuid::now_v7();
        let first_order = Uuid::now_v7();
        let second_order = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO commerce_products (id,project_id,name) VALUES ($1,$2,'Batch product')",
        )
        .bind(product_id)
        .bind(project_id.0)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_prices \
             (id,project_id,product_id,currency,unit_amount_minor,billing_type,provider_price_id) \
             VALUES ($1,$2,$3,'usd',100,'one_time',$4)",
        )
        .bind(price_id)
        .bind(project_id.0)
        .bind(product_id)
        .bind(format!("price_{}", Uuid::now_v7().simple()))
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_orders \
             (id,project_id,status,currency,subtotal_minor,total_minor) \
             VALUES ($1,$3,'paid','usd',200,200),($2,$3,'paid','usd',100,100)",
        )
        .bind(first_order)
        .bind(second_order)
        .bind(project_id.0)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_order_lines \
             (id,project_id,order_id,product_id,price_id,product_name,currency,unit_amount_minor,quantity,line_total_minor) \
             VALUES ($1,$4,$5,$7,$8,'Batch product','usd',100,1,100), \
                    ($2,$4,$5,$7,$8,'Batch product','usd',100,1,100), \
                    ($3,$4,$6,$7,$8,'Batch product','usd',100,1,100)",
        )
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(project_id.0)
        .bind(first_order)
        .bind(second_order)
        .bind(product_id)
        .bind(price_id)
        .execute(&pool)
        .await?;

        let orders = service.orders(project_id).await?;
        assert_eq!(orders.len(), 2);
        assert_eq!(
            orders
                .iter()
                .find(|order| order.id.0 == first_order)
                .map(|order| order.lines.len()),
            Some(2)
        );
        assert_eq!(
            orders
                .iter()
                .find(|order| order.id.0 == second_order)
                .map(|order| order.lines.len()),
            Some(1)
        );
        Ok(())
    }

    #[test]
    fn stripe_signature_verification_accepts_rotation_and_rejects_tampering()
    -> Result<(), CommerceServiceError> {
        let secret = "whsec_test_secret";
        let payload = br#"{"id":"evt_test"}"#;
        let timestamp = 2_000_000_000_i64;
        let mut signed = timestamp.to_string().into_bytes();
        signed.push(b'.');
        signed.extend_from_slice(payload);
        let signature = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()),
            &signed,
        );
        let header = format!(
            "t={timestamp},v1={},v1={}",
            "00".repeat(32),
            signature
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        verify_stripe_signature(secret, payload, &header, timestamp)?;
        assert_eq!(
            verify_stripe_signature(secret, b"tampered", &header, timestamp),
            Err(CommerceServiceError::InvalidSignature)
        );
        assert_eq!(
            verify_stripe_signature(
                secret,
                payload,
                &header,
                timestamp + WEBHOOK_TOLERANCE_SECONDS + 1,
            ),
            Err(CommerceServiceError::InvalidSignature)
        );
        Ok(())
    }

    #[test]
    fn provider_account_refunds_do_not_require_payouts() -> Result<(), CommerceServiceError> {
        let account = parse_v1_account(
            &json!({
                "id": "acct_123456789",
                "charges_enabled": true,
                "details_submitted": true,
                "payouts_enabled": false,
                "capabilities": {"card_payments": "active"},
                "requirements": {"currently_due": []}
            }),
            false,
        )?;
        assert!(account.capabilities.one_time_payments);
        assert!(account.capabilities.recurring_payments);
        assert!(account.capabilities.refunds);
        Ok(())
    }

    #[test]
    fn product_metadata_cannot_override_ffdb_provider_bindings() {
        assert!(validate_product_metadata_key("display_group").is_ok());
        assert!(validate_product_metadata_key("ffdb_project_id").is_err());
        assert!(validate_product_metadata_key("nested[key]").is_err());
        assert!(validate_product_metadata_key("").is_err());
    }

    #[test]
    fn accounts_v2_capabilities_require_active_card_payments() -> Result<(), CommerceServiceError> {
        let active = parse_v2_account(
            &json!({
                "id": "acct_123456789",
                "configuration": {"merchant": {"capabilities": {
                    "card_payments": {"status": "active"}
                }}},
                "requirements": {"entries": []}
            }),
            false,
        )?;
        assert_eq!(active.status, CommerceAccountStatus::Enabled);
        assert!(active.capabilities.customer_portal);
        let onboarding = parse_v2_account(
            &json!({
                "id": "acct_987654321",
                "configuration": {"merchant": {"capabilities": {
                    "card_payments": {"status": "pending"}
                }}},
                "requirements": {"entries": [{
                    "id": "identity.verification_document",
                    "minimum_deadline": {"status": "currently_due"}
                }]}
            }),
            true,
        )?;
        assert_eq!(onboarding.status, CommerceAccountStatus::Onboarding);
        assert_eq!(
            onboarding.requirements_due,
            vec!["identity.verification_document"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn byo_provider_configuration_and_disconnect_are_atomic()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        crate::control_plane_migrations::migrator()
            .run(&pool)
            .await?;
        let project_id = create_test_project(&pool).await?;
        let account_id = format!("acct_{}", Uuid::now_v7().simple());
        let (stripe_base, requests, server) = spawn_mock_stripe(account_id.clone()).await?;
        let service = test_service(pool.clone(), stripe_base, None)?;
        let summary = service
            .configure_byo(
                project_id,
                "sk_test_1234567890123456", // gitleaks:allow -- synthetic Stripe test fixture
                "whsec_1234567890123456",
            )
            .await?;
        assert_eq!(
            summary.provider_account_id.as_deref(),
            Some(account_id.as_str())
        );
        assert!(summary.secrets_configured);
        assert_eq!(summary.mode, CommerceProviderMode::BringYourOwnKeys);
        assert!(
            requests
                .lock()
                .map_err(|_| "mock request mutex poisoned")?
                .contains(&(Method::GET, "/v1/account".to_owned()))
        );
        service.disconnect_account(project_id).await?;
        let account_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM project_commerce_accounts WHERE project_id=$1",
        )
        .bind(project_id.0)
        .fetch_one(&pool)
        .await?;
        let secret_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM project_commerce_secrets WHERE project_id=$1")
                .bind(project_id.0)
                .fetch_one(&pool)
                .await?;
        assert_eq!((account_count, secret_count), (0, 0));

        service
            .configure_byo(
                project_id,
                "sk_test_1234567890123456", // gitleaks:allow -- synthetic Stripe test fixture
                "whsec_1234567890123456",
            )
            .await?;
        sqlx::query(
            "INSERT INTO commerce_products (id,project_id,name,metadata) \
             VALUES ($1,$2,'Bound product','{}'::jsonb)",
        )
        .bind(Uuid::now_v7())
        .bind(project_id.0)
        .execute(&pool)
        .await?;
        assert_eq!(
            service.disconnect_account(project_id).await,
            Err(CommerceServiceError::AccountInUse)
        );
        let preserved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM project_commerce_accounts WHERE project_id=$1",
        )
        .bind(project_id.0)
        .fetch_one(&pool)
        .await?;
        assert_eq!(preserved, 1);
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn connect_v2_lifecycle_and_global_webhook_route_by_account()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        crate::control_plane_migrations::migrator()
            .run(&pool)
            .await?;
        let project_id = create_test_project(&pool).await?;
        let account_id = format!("acct_{}", Uuid::now_v7().simple());
        let (stripe_base, requests, server) = spawn_mock_stripe(account_id.clone()).await?;
        let connect_secret = "whsec_project_connect_123456789";
        let service = test_service(
            pool.clone(),
            stripe_base,
            Some(CommerceConnectConfig {
                secret_key: ProtectedString::from("sk_test_connect_123456789".to_owned()),
                webhook_secret: ProtectedString::from(connect_secret.to_owned()),
            }),
        )?;
        let onboarding = service
            .connect_onboarding(
                project_id,
                &CreateCommerceConnectOnboardingRequest {
                    country: "US".to_owned(),
                    email: "merchant@example.test".to_owned(),
                    return_url: "https://merchant.example.test/stripe/return".to_owned(),
                    refresh_url: "https://merchant.example.test/stripe/refresh".to_owned(),
                },
                "connect-test-key-123456",
            )
            .await?;
        assert_eq!(onboarding.account.mode, CommerceProviderMode::StripeConnect);
        assert_eq!(
            onboarding.account.webhook_url,
            "https://ffdb.example.test/v1/commerce/webhooks/stripe-connect"
        );
        service.refresh_account(project_id).await?;
        let recorded = requests
            .lock()
            .map_err(|_| "mock request mutex poisoned")?
            .clone();
        assert!(recorded.contains(&(Method::POST, "/v2/core/accounts".to_owned())));
        assert!(recorded.contains(&(Method::POST, "/v2/core/account_links".to_owned())));
        assert!(recorded.contains(&(Method::GET, format!("/v2/core/accounts/{account_id}"))));
        assert!(
            !recorded
                .iter()
                .any(|(_, path)| path.starts_with("/v1/accounts"))
        );

        let timestamp = 2_000_000_000;
        let payload = serde_json::to_vec(&json!({
            "id": format!("evt_{}", Uuid::now_v7().simple()),
            "type": "ffdb.test.noop",
            "account": account_id,
            "livemode": false,
            "created": timestamp,
            "data": {"object": {}}
        }))?;
        let signature = stripe_signature(connect_secret, &payload, timestamp);
        assert_eq!(
            service
                .apply_connect_webhook(&payload, &signature, timestamp)
                .await?,
            WebhookOutcome::Processed
        );
        assert_eq!(
            service
                .apply_connect_webhook(&payload, &signature, timestamp)
                .await?,
            WebhookOutcome::Duplicate
        );
        assert_eq!(
            service
                .apply_byo_webhook(project_id, &payload, &signature, timestamp)
                .await,
            Err(CommerceServiceError::Conflict)
        );
        let unknown_payload = serde_json::to_vec(&json!({
            "id": format!("evt_{}", Uuid::now_v7().simple()),
            "type": "ffdb.test.noop",
            "account": "acct_unknown_123456789",
            "livemode": false,
            "created": timestamp,
            "data": {"object": {}}
        }))?;
        let unknown_signature = stripe_signature(connect_secret, &unknown_payload, timestamp);
        assert_eq!(
            service
                .apply_connect_webhook(&unknown_payload, &unknown_signature, timestamp)
                .await,
            Err(CommerceServiceError::InvalidSignature)
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn webhook_state_machine_is_deduplicated_ordered_and_fulfillment_safe()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        crate::control_plane_migrations::migrator()
            .run(&pool)
            .await?;
        let project_id = create_test_project(&pool).await?;
        let account_id = format!("acct_{}", Uuid::now_v7().simple());
        let (stripe_base, _requests, server) = spawn_mock_stripe(account_id).await?;
        let service = test_service(pool.clone(), stripe_base, None)?;
        let webhook_secret = "whsec_1234567890123456"; // gitleaks:allow -- synthetic webhook fixture
        service
            .configure_byo(project_id, "sk_test_1234567890123456", webhook_secret) // gitleaks:allow -- synthetic Stripe test fixture
            .await?;

        let product_id = Uuid::now_v7();
        let one_time_price_id = Uuid::now_v7();
        let recurring_price_id = Uuid::now_v7();
        let customer_id = Uuid::now_v7();
        let order_id = Uuid::now_v7();
        let subscription_id = Uuid::now_v7();
        let checkout_id = format!("cs_{}", Uuid::now_v7().simple());
        let payment_intent_id = format!("pi_{}", Uuid::now_v7().simple());
        let subscription_provider_id = format!("sub_{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO commerce_products (id,project_id,name,metadata) \
             VALUES ($1,$2,'Integration product','{}'::jsonb)",
        )
        .bind(product_id)
        .bind(project_id.0)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_prices \
             (id,project_id,product_id,currency,unit_amount_minor,billing_type,recurring_interval,recurring_interval_count,provider_price_id,entitlements) \
             VALUES ($1,$3,$2,'usd',2000,'one_time',NULL,NULL,$4,'{}'::jsonb), \
                    ($5,$3,$2,'usd',1500,'recurring','month',1,$6,$7)",
        )
        .bind(one_time_price_id)
        .bind(product_id)
        .bind(project_id.0)
        .bind(format!("price_{}", Uuid::now_v7().simple()))
        .bind(recurring_price_id)
        .bind(format!("price_{}", Uuid::now_v7().simple()))
        .bind(json!({"seats": {"type": "quantity", "value": 5}}))
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_customers \
             (id,project_id,subject_kind,subject_id) VALUES ($1,$2,'team','team-1')",
        )
        .bind(customer_id)
        .bind(project_id.0)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_orders \
             (id,project_id,customer_id,status,currency,subtotal_minor,total_minor,provider_checkout_session_id) \
             VALUES ($1,$2,$3,'checkout_created','usd',2000,2000,$4)",
        )
        .bind(order_id)
        .bind(project_id.0)
        .bind(customer_id)
        .bind(&checkout_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_order_lines \
             (id,project_id,order_id,product_id,price_id,product_name,currency,unit_amount_minor,quantity,line_total_minor) \
             VALUES ($1,$2,$3,$4,$5,'Integration product','usd',2000,1,2000)",
        )
        .bind(Uuid::now_v7())
        .bind(project_id.0)
        .bind(order_id)
        .bind(product_id)
        .bind(one_time_price_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO commerce_subscriptions \
             (id,project_id,customer_id,price_id,subject_kind,subject_id,status,provider_subscription_id) \
             VALUES ($1,$2,$3,$4,'team','team-1','checkout_pending',$5)",
        )
        .bind(subscription_id)
        .bind(project_id.0)
        .bind(customer_id)
        .bind(recurring_price_id)
        .bind(&subscription_provider_id)
        .execute(&pool)
        .await?;

        let created = now_ms() / 1_000;
        let checkout_event_id = format!("evt_{}", Uuid::now_v7().simple());
        let checkout_object = json!({
            "id": checkout_id,
            "metadata": {"ffdb_order_id": order_id.to_string()},
            "payment_status": "paid",
            "payment_intent": payment_intent_id,
            "amount_total": 2000,
            "currency": "usd"
        });
        assert_eq!(
            apply_byo_test_event(
                &service,
                project_id,
                webhook_secret,
                &checkout_event_id,
                "checkout.session.completed",
                created,
                checkout_object.clone(),
            )
            .await?,
            WebhookOutcome::Processed
        );
        assert_eq!(
            apply_byo_test_event(
                &service,
                project_id,
                webhook_secret,
                &checkout_event_id,
                "checkout.session.completed",
                created,
                checkout_object,
            )
            .await?,
            WebhookOutcome::Duplicate
        );
        apply_byo_test_event(
            &service,
            project_id,
            webhook_secret,
            &format!("evt_{}", Uuid::now_v7().simple()),
            "payment_intent.payment_failed",
            created - 60,
            json!({
                "id": payment_intent_id,
                "metadata": {"ffdb_order_id": order_id.to_string()},
                "status": "requires_payment_method",
                "amount": 2000,
                "amount_received": 0,
                "currency": "usd"
            }),
        )
        .await?;
        let order_status: String =
            sqlx::query_scalar("SELECT status FROM commerce_orders WHERE project_id=$1 AND id=$2")
                .bind(project_id.0)
                .bind(order_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(order_status, "paid");
        service
            .update_fulfillment(
                project_id,
                CommerceOrderId(order_id),
                CommerceFulfillmentStatus::Processing,
                Some("packed"),
            )
            .await?;
        service
            .update_fulfillment(
                project_id,
                CommerceOrderId(order_id),
                CommerceFulfillmentStatus::Fulfilled,
                Some("shipped"),
            )
            .await?;

        let payment_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM commerce_payments WHERE project_id=$1 AND provider_payment_intent_id=$2",
        )
        .bind(project_id.0)
        .bind(&payment_intent_id)
        .fetch_one(&pool)
        .await?;
        let refund_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO commerce_refunds \
             (id,project_id,order_id,payment_id,status,amount_minor,currency,reason) \
             VALUES ($1,$2,$3,$4,'pending',500,'usd','requested_by_customer')",
        )
        .bind(refund_id)
        .bind(project_id.0)
        .bind(order_id)
        .bind(payment_id)
        .execute(&pool)
        .await?;
        apply_byo_test_event(
            &service,
            project_id,
            webhook_secret,
            &format!("evt_{}", Uuid::now_v7().simple()),
            "refund.updated",
            created + 10,
            json!({
                "id": format!("re_{}", Uuid::now_v7().simple()),
                "status": "succeeded",
                "metadata": {"ffdb_refund_id": refund_id.to_string()}
            }),
        )
        .await?;

        let subscription_event_id = format!("evt_{}", Uuid::now_v7().simple());
        let subscription_object = json!({
            "id": subscription_provider_id,
            "metadata": {"ffdb_subscription_id": subscription_id.to_string()},
            "status": "active",
            "current_period_start": created - 10,
            "current_period_end": created + 3600,
            "cancel_at_period_end": false
        });
        assert_eq!(
            apply_byo_test_event(
                &service,
                project_id,
                webhook_secret,
                &subscription_event_id,
                "customer.subscription.updated",
                created + 20,
                subscription_object.clone(),
            )
            .await?,
            WebhookOutcome::Processed
        );
        assert_eq!(
            apply_byo_test_event(
                &service,
                project_id,
                webhook_secret,
                &subscription_event_id,
                "customer.subscription.updated",
                created + 20,
                subscription_object,
            )
            .await?,
            WebhookOutcome::Duplicate
        );
        apply_byo_test_event(
            &service,
            project_id,
            webhook_secret,
            &format!("evt_{}", Uuid::now_v7().simple()),
            "customer.subscription.deleted",
            created - 30,
            json!({
                "id": subscription_provider_id,
                "metadata": {"ffdb_subscription_id": subscription_id.to_string()},
                "status": "canceled",
                "current_period_start": created - 100,
                "current_period_end": created + 100,
                "cancel_at_period_end": false
            }),
        )
        .await?;

        let final_row = sqlx::query(
            "SELECT status,fulfillment_status,refunded_minor FROM commerce_orders \
             WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(order_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            final_row.try_get::<String, _>("status")?,
            "partially_refunded"
        );
        assert_eq!(
            final_row.try_get::<String, _>("fulfillment_status")?,
            "fulfilled"
        );
        assert_eq!(final_row.try_get::<i64, _>("refunded_minor")?, 500);
        let payment_state =
            sqlx::query("SELECT status,refunded_minor FROM commerce_payments WHERE id=$1")
                .bind(payment_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            payment_state.try_get::<String, _>("status")?,
            "partially_refunded"
        );
        assert_eq!(payment_state.try_get::<i64, _>("refunded_minor")?, 500);
        let subscription_status: String =
            sqlx::query_scalar("SELECT status FROM commerce_subscriptions WHERE id=$1")
                .bind(subscription_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(subscription_status, "active");
        let entitlement = sqlx::query(
            "SELECT status,entitlement_value FROM commerce_entitlements \
             WHERE project_id=$1 AND subscription_id=$2 AND entitlement_key='seats'",
        )
        .bind(project_id.0)
        .bind(subscription_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(entitlement.try_get::<String, _>("status")?, "active");
        assert_eq!(
            entitlement.try_get::<Value, _>("entitlement_value")?,
            json!({"type":"quantity","value":5})
        );
        let fulfillment_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM commerce_fulfillment_events WHERE project_id=$1 AND order_id=$2",
        )
        .bind(project_id.0)
        .bind(order_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(fulfillment_events, 2);
        server.abort();
        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct CatalogQuery {
    #[serde(default)]
    include_inactive: bool,
}

#[derive(Deserialize)]
pub(crate) struct EntitlementQuery {
    subject_kind: CommerceMembershipSubjectKind,
    subject_id: String,
    at_ms: Option<i64>,
}

pub(crate) async fn account(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    match service.account_summary(project_id).await {
        Ok(summary) => Json(Some(summary)).into_response(),
        // Account discovery is a read model used by setup UIs. An unconfigured
        // project is a valid empty state, not a conflicting request.
        Err(CommerceServiceError::AccountNotConfigured) => {
            Json(Option::<CommerceAccountSummary>::None).into_response()
        }
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn configure_byo(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<ConfigureCommerceByoRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    // Bind idempotency to secret fingerprints without ever persisting or
    // logging the credentials themselves.
    let payload = json!({
        "secret_key_sha256": sha256_hex(input.secret_key.expose().as_bytes()),
        "webhook_secret_sha256": sha256_hex(input.webhook_secret.expose().as_bytes()),
    });
    let admission = match admit_mutation(
        &state,
        project_id,
        &headers,
        "commerce.account.byo.rotate",
        &payload,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let result = service
        .configure_byo(
            project_id,
            input.secret_key.expose(),
            input.webhook_secret.expose(),
        )
        .await;
    finish_mutation(&state, admission, result, StatusCode::OK, request_id).await
}

pub(crate) async fn connect_onboarding(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateCommerceConnectOnboardingRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let payload = match serde_json::to_value(&input) {
        Ok(value) => value,
        Err(_) => return service_error(CommerceServiceError::InvalidRequest, request_id),
    };
    let admission = match admit_mutation(
        &state,
        project_id,
        &headers,
        "commerce.account.connect.onboard",
        &payload,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let result = service
        .connect_onboarding(project_id, &input, admission.provider_key())
        .await;
    finish_mutation(&state, admission, result, StatusCode::OK, request_id).await
}

pub(crate) async fn refresh_account(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    json_result(service.refresh_account(project_id).await, request_id)
}

pub(crate) async fn disconnect_account(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id, actor) =
        match authorized_service_with_actor(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let payload = json!({});
    let admission = match admit_mutation(
        &state,
        project_id,
        &headers,
        "commerce.account.disconnect",
        &payload,
        request_id,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    if append_commerce_audit(
        &state,
        project_id,
        request_id,
        actor,
        "commerce.account.disconnect.requested",
        AuditOutcome::Success,
    )
    .await
    .is_err()
    {
        let _ = idempotency::abandon(&service.pool, &admission.claim).await;
        return service_error(CommerceServiceError::Unavailable, request_id);
    }
    let result = service.disconnect_account(project_id).await;
    let outcome = if result.is_ok() {
        AuditOutcome::Success
    } else {
        AuditOutcome::Failure
    };
    if append_commerce_audit(
        &state,
        project_id,
        request_id,
        actor,
        "commerce.account.disconnect.completed",
        outcome,
    )
    .await
    .is_err()
    {
        tracing::error!(%request_id, %project_id, "commerce disconnect terminal audit failed");
    }
    finish_mutation(
        &state,
        admission,
        result.map(|()| json!({})),
        StatusCode::NO_CONTENT,
        request_id,
    )
    .await
}

pub(crate) async fn products(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Query(query): Query<CatalogQuery>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if query.include_inactive
        && authorize_commerce_admin(&state, project_id, &headers)
            .await
            .is_err()
    {
        return credential_error(CredentialError::InsufficientScope, request_id).into_response();
    }
    json_result(
        service.products(project_id, query.include_inactive).await,
        request_id,
    )
}

pub(crate) async fn create_product(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateCommerceProductRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let create_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.product.create",
        &input,
        StatusCode::CREATED,
        request_id,
        |key| async move {
            service
                .create_product(project_id, &create_input, &key)
                .await
        },
    )
    .await
}

pub(crate) async fn archive_product(
    State(state): State<ApiState>,
    Path((project, product)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let product_id = match parse_wire_id(&product).map(CommerceProductId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    let input = json!({"product_id": product_id});
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.product.archive",
        &input,
        StatusCode::NO_CONTENT,
        request_id,
        |key| async move {
            service
                .archive_product(project_id, product_id, &key)
                .await
                .map(|_| json!({}))
        },
    )
    .await
}

pub(crate) async fn prices(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Query(query): Query<CatalogQuery>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if query.include_inactive
        && authorize_commerce_admin(&state, project_id, &headers)
            .await
            .is_err()
    {
        return credential_error(CredentialError::InsufficientScope, request_id).into_response();
    }
    json_result(
        service.prices(project_id, query.include_inactive).await,
        request_id,
    )
}

pub(crate) async fn create_price(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateCommercePriceRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let create_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.price.create",
        &input,
        StatusCode::CREATED,
        request_id,
        |key| async move { service.create_price(project_id, &create_input, &key).await },
    )
    .await
}

pub(crate) async fn retire_price(
    State(state): State<ApiState>,
    Path((project, price)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let price_id = match parse_wire_id(&price).map(CommercePriceId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    let input = json!({"price_id": price_id});
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.price.retire",
        &input,
        StatusCode::NO_CONTENT,
        request_id,
        |key| async move {
            service
                .retire_price(project_id, price_id, &key)
                .await
                .map(|_| json!({}))
        },
    )
    .await
}

pub(crate) async fn one_time_checkout(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateOneTimeCommerceCheckoutRequest>,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(subject) = input.subject.as_ref()
        && authorize_subject(&state, project_id, &headers, subject)
            .await
            .is_err()
    {
        return credential_error(CredentialError::InsufficientScope, request_id).into_response();
    }
    let checkout_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.checkout.one_time",
        &input,
        StatusCode::CREATED,
        request_id,
        |key| async move {
            service
                .one_time_checkout(project_id, &checkout_input, &key)
                .await
        },
    )
    .await
}

pub(crate) async fn recurring_checkout(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateRecurringCommerceCheckoutRequest>,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if authorize_subject(&state, project_id, &headers, &input.subject)
        .await
        .is_err()
    {
        return credential_error(CredentialError::InsufficientScope, request_id).into_response();
    }
    let checkout_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.checkout.recurring",
        &input,
        StatusCode::CREATED,
        request_id,
        |key| async move {
            service
                .recurring_checkout(project_id, &checkout_input, &key)
                .await
        },
    )
    .await
}

pub(crate) async fn customer_portal(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateCommerceCustomerPortalRequest>,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if authorize_subject(&state, project_id, &headers, &input.subject)
        .await
        .is_err()
    {
        return credential_error(CredentialError::InsufficientScope, request_id).into_response();
    }
    let portal_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.customer_portal.create",
        &input,
        StatusCode::CREATED,
        request_id,
        |key| async move {
            service
                .customer_portal(project_id, &portal_input, &key)
                .await
        },
    )
    .await
}

pub(crate) async fn orders(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    json_result(service.orders(project_id).await, request_id)
}

pub(crate) async fn order(
    State(state): State<ApiState>,
    Path((project, order)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let order_id = match parse_wire_id(&order).map(CommerceOrderId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    json_result(service.order(project_id, order_id).await, request_id)
}

pub(crate) async fn payments(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    json_result(service.payments(project_id).await, request_id)
}

pub(crate) async fn refunds(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CreateCommerceRefundRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let refund_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.refund.create",
        &input,
        StatusCode::CREATED,
        request_id,
        |key| async move { service.create_refund(project_id, &refund_input, &key).await },
    )
    .await
}

pub(crate) async fn subscriptions(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    json_result(service.subscriptions(project_id).await, request_id)
}

pub(crate) async fn cancel_subscription(
    State(state): State<ApiState>,
    Path((project, subscription)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<CancelCommerceSubscriptionRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let subscription_id = match parse_wire_id(&subscription).map(CommerceSubscriptionId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.subscription.cancel",
        &json!({"subscription_id": subscription_id, "input": input}),
        StatusCode::OK,
        request_id,
        |key| async move {
            service
                .cancel_subscription(project_id, subscription_id, input.at_period_end, &key)
                .await
        },
    )
    .await
}

pub(crate) async fn entitlements(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Query(query): Query<EntitlementQuery>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let subject = CommerceMembershipSubject {
        kind: query.subject_kind,
        id: query.subject_id,
    };
    if authorize_subject(&state, project_id, &headers, &subject)
        .await
        .is_err()
    {
        return credential_error(CredentialError::InsufficientScope, request_id).into_response();
    }
    json_result(
        service
            .entitlements(project_id, &subject, query.at_ms.unwrap_or_else(now_ms))
            .await,
        request_id,
    )
}

pub(crate) async fn fulfillment(
    State(state): State<ApiState>,
    Path((project, order)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(input): Json<ffdb_protocol::UpdateCommerceFulfillmentRequest>,
) -> Response {
    let (service, project_id) =
        match authorized_service(&state, &project, &headers, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let order_id = match parse_wire_id(&order).map(CommerceOrderId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    let fulfillment_input = input.clone();
    mutation_handler(
        &state,
        project_id,
        &headers,
        "commerce.fulfillment.update",
        &input,
        StatusCode::OK,
        request_id,
        |_key| async move {
            service
                .update_fulfillment(
                    project_id,
                    order_id,
                    fulfillment_input.status,
                    fulfillment_input.note.as_deref(),
                )
                .await
        },
    )
    .await
}

pub(crate) async fn stripe_webhook(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (service, project_id) = match public_service(&state, &project, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(signature) = headers
        .get("stripe-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return service_error(CommerceServiceError::InvalidSignature, request_id);
    };
    match service
        .apply_byo_webhook(project_id, &body, signature, now_ms() / 1_000)
        .await
    {
        Ok(WebhookOutcome::Processed) => Json(json!({"received": true})).into_response(),
        Ok(WebhookOutcome::Duplicate) => {
            Json(json!({"received": true, "duplicate": true})).into_response()
        }
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn stripe_connect_webhook(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(service) = state.commerce.as_ref() else {
        return service_error(CommerceServiceError::Unavailable, request_id);
    };
    let Some(signature) = headers
        .get("stripe-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return service_error(CommerceServiceError::InvalidSignature, request_id);
    };
    match service
        .apply_connect_webhook(&body, signature, now_ms() / 1_000)
        .await
    {
        Ok(WebhookOutcome::Processed) => Json(json!({"received": true})).into_response(),
        Ok(WebhookOutcome::Duplicate) => {
            Json(json!({"received": true, "duplicate": true})).into_response()
        }
        Err(error) => service_error(error, request_id),
    }
}

struct MutationAdmission {
    claim: idempotency::Claim,
    provider_key: String,
}

impl MutationAdmission {
    fn provider_key(&self) -> &str {
        &self.provider_key
    }
}

async fn admit_mutation(
    state: &ApiState,
    project_id: ProjectId,
    headers: &HeaderMap,
    operation: &'static str,
    payload: &Value,
    request_id: RequestId,
) -> Result<MutationAdmission, Response> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| service_error(CommerceServiceError::InvalidRequest, request_id))?;
    validate_idempotency_key(key).map_err(|error| service_error(error, request_id))?;
    let service = state
        .commerce
        .as_ref()
        .ok_or_else(|| service_error(CommerceServiceError::Unavailable, request_id))?;
    let hash = idempotency::request_hash(payload)
        .map_err(|_| service_error(CommerceServiceError::InvalidRequest, request_id))?;
    match idempotency::admit(
        &service.pool,
        idempotency::Scope::Project(project_id),
        operation,
        key,
        hash,
    )
    .await
    {
        Ok(idempotency::Admission::Owner(claim)) => Ok(MutationAdmission {
            claim,
            provider_key: format!("{operation}:{project_id}:{key}"),
        }),
        Ok(idempotency::Admission::Replay { status, body }) => {
            Err((status, Json(body)).into_response())
        }
        Ok(idempotency::Admission::Conflict) => Err(service_error(
            CommerceServiceError::WebhookHashConflict,
            request_id,
        )),
        Ok(idempotency::Admission::InProgress) => Err((
            StatusCode::CONFLICT,
            Json(json!({"error": "request_in_progress"})),
        )
            .into_response()),
        Err(_) => Err(service_error(CommerceServiceError::Unavailable, request_id)),
    }
}

async fn finish_mutation<T: serde::Serialize>(
    state: &ApiState,
    admission: MutationAdmission,
    result: Result<T, CommerceServiceError>,
    success_status: StatusCode,
    request_id: RequestId,
) -> Response {
    let service = match state.commerce.as_ref() {
        Some(value) => value,
        None => return service_error(CommerceServiceError::Unavailable, request_id),
    };
    match result {
        Ok(value) => {
            let body = match serde_json::to_value(&value) {
                Ok(value) => value,
                Err(_) => {
                    let _ = idempotency::abandon(&service.pool, &admission.claim).await;
                    return service_error(CommerceServiceError::Unavailable, request_id);
                }
            };
            if idempotency::complete(&service.pool, &admission.claim, success_status, &body)
                .await
                .is_err()
            {
                return service_error(CommerceServiceError::Unavailable, request_id);
            }
            if success_status == StatusCode::NO_CONTENT {
                success_status.into_response()
            } else {
                (success_status, Json(body)).into_response()
            }
        }
        Err(error) => {
            let _ = idempotency::abandon(&service.pool, &admission.claim).await;
            service_error(error, request_id)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn mutation_handler<I, T, F, Fut>(
    state: &ApiState,
    project_id: ProjectId,
    headers: &HeaderMap,
    operation: &'static str,
    input: &I,
    success_status: StatusCode,
    request_id: RequestId,
    run: F,
) -> Response
where
    I: serde::Serialize,
    T: serde::Serialize,
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, CommerceServiceError>>,
{
    let payload = match serde_json::to_value(input) {
        Ok(value) => value,
        Err(_) => return service_error(CommerceServiceError::InvalidRequest, request_id),
    };
    let admission =
        match admit_mutation(state, project_id, headers, operation, &payload, request_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let result = run(admission.provider_key.clone()).await;
    finish_mutation(state, admission, result, success_status, request_id).await
}

#[allow(clippy::result_large_err)]
fn public_service(
    state: &ApiState,
    project: &str,
    request_id: RequestId,
) -> Result<(Arc<CommerceService>, ProjectId), Response> {
    let project_id = parse_wire_id(project)
        .map(ProjectId)
        .map_err(|error| service_error(error, request_id))?;
    let service = state
        .commerce
        .clone()
        .ok_or_else(|| service_error(CommerceServiceError::Unavailable, request_id))?;
    Ok((service, project_id))
}

async fn authorized_service(
    state: &ApiState,
    project: &str,
    headers: &HeaderMap,
    request_id: RequestId,
) -> Result<(Arc<CommerceService>, ProjectId), Response> {
    let (service, project_id) = public_service(state, project, request_id)?;
    authorize_commerce_admin(state, project_id, headers)
        .await
        .map_err(|error| credential_error(error, request_id).into_response())?;
    Ok((service, project_id))
}

#[derive(Clone, Copy)]
struct CommerceAuditActor {
    organization_id: Option<OrganizationId>,
    kind: ActorKind,
    id: Uuid,
}

async fn authorized_service_with_actor(
    state: &ApiState,
    project: &str,
    headers: &HeaderMap,
    request_id: RequestId,
) -> Result<(Arc<CommerceService>, ProjectId, CommerceAuditActor), Response> {
    let (service, project_id) = public_service(state, project, request_id)?;
    if let Ok(principal) =
        developer(state, project_id, headers, DeveloperScope::CommerceManage).await
    {
        return Ok((
            service,
            project_id,
            CommerceAuditActor {
                organization_id: Some(principal.organization_id),
                kind: ActorKind::ApiKey,
                id: principal.api_key_id.0,
            },
        ));
    }
    let (management, identity) = super::management::authenticated(state, headers, request_id)
        .await
        .map_err(|_| credential_error(CredentialError::Invalid, request_id).into_response())?;
    let organization_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.organization_id FROM projects p JOIN organization_memberships m \
         ON m.organization_id=p.organization_id WHERE p.id=$1 AND m.user_id=$2 \
         AND p.lifecycle_state <> 'deleted' AND m.role IN ('owner','admin')",
    )
    .bind(project_id.0)
    .bind(identity.user_id.0)
    .fetch_optional(&management.pool)
    .await
    .map_err(|_| credential_error(CredentialError::Unavailable, request_id).into_response())?;
    let organization_id = organization_id.ok_or_else(|| {
        credential_error(CredentialError::InsufficientScope, request_id).into_response()
    })?;
    Ok((
        service,
        project_id,
        CommerceAuditActor {
            organization_id: Some(OrganizationId(organization_id)),
            kind: ActorKind::User,
            id: identity.user_id.0,
        },
    ))
}

async fn append_commerce_audit(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    actor: CommerceAuditActor,
    action: &str,
    outcome: AuditOutcome,
) -> Result<(), ()> {
    state
        .audit
        .append(AuditDraft {
            occurred_at_ms: now_ms(),
            organization_id: actor.organization_id,
            project_id: Some(project_id),
            request_id,
            actor_kind: actor.kind,
            actor_id: Some(actor.id),
            action: action.to_owned(),
            resource_kind: "commerce_account".to_owned(),
            resource_id: None,
            outcome,
            source_ip: super::trusted_source_ip(),
            metadata: json!({"protocol_version": PROTOCOL_VERSION}),
        })
        .await
        .map(|_| ())
        .map_err(|_| ())
}

async fn authorize_commerce_admin(
    state: &ApiState,
    project_id: ProjectId,
    headers: &HeaderMap,
) -> Result<(), CredentialError> {
    if developer(state, project_id, headers, DeveloperScope::CommerceManage)
        .await
        .is_ok()
    {
        return Ok(());
    }
    let (management, identity) = super::management::authenticated(state, headers, RequestId::new())
        .await
        .map_err(|_| CredentialError::Invalid)?;
    let allowed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM projects p JOIN organization_memberships m \
         ON m.organization_id=p.organization_id WHERE p.id=$1 AND m.user_id=$2 \
         AND p.lifecycle_state <> 'deleted' AND m.role IN ('owner','admin'))",
    )
    .bind(project_id.0)
    .bind(identity.user_id.0)
    .fetch_one(&management.pool)
    .await
    .map_err(|_| CredentialError::Unavailable)?;
    if allowed {
        Ok(())
    } else {
        Err(CredentialError::InsufficientScope)
    }
}

async fn authorize_subject(
    state: &ApiState,
    project_id: ProjectId,
    headers: &HeaderMap,
    subject: &CommerceMembershipSubject,
) -> Result<(), CredentialError> {
    domain_subject(subject).map_err(|_| CredentialError::Invalid)?;
    if subject.kind == CommerceMembershipSubjectKind::Individual
        && let Ok(context) = end_user(state, project_id, headers).await
    {
        let expected = Uuid::parse_str(&subject.id).map_err(|_| CredentialError::Invalid)?;
        if context.subject.0 == expected {
            return Ok(());
        }
    }
    authorize_commerce_admin(state, project_id, headers).await
}

fn json_result<T: serde::Serialize>(
    result: Result<T, CommerceServiceError>,
    request_id: RequestId,
) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

fn service_error(error: CommerceServiceError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        CommerceServiceError::InvalidConfiguration | CommerceServiceError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "commerce.unavailable",
            "commerce service is unavailable",
        ),
        CommerceServiceError::InvalidRequest | CommerceServiceError::ProviderResponseInvalid => (
            StatusCode::BAD_REQUEST,
            "commerce.invalid_request",
            "commerce request is invalid",
        ),
        CommerceServiceError::NotFound => (
            StatusCode::NOT_FOUND,
            "commerce.not_found",
            "commerce resource was not found",
        ),
        CommerceServiceError::Forbidden => (
            StatusCode::FORBIDDEN,
            "commerce.forbidden",
            "commerce operation is not permitted",
        ),
        CommerceServiceError::AccountNotConfigured => (
            StatusCode::CONFLICT,
            "commerce.account_not_configured",
            "commerce account is not configured",
        ),
        CommerceServiceError::AccountInUse => (
            StatusCode::CONFLICT,
            "commerce.account_in_use",
            "commerce account cannot be disconnected while catalog, customer, order, or subscription data is bound to it",
        ),
        CommerceServiceError::AccountRestricted | CommerceServiceError::CapabilityUnavailable => (
            StatusCode::CONFLICT,
            "commerce.account_restricted",
            "commerce account cannot perform this operation",
        ),
        CommerceServiceError::Conflict | CommerceServiceError::WebhookHashConflict => (
            StatusCode::CONFLICT,
            "commerce.conflict",
            "commerce state conflicts with the request",
        ),
        CommerceServiceError::Encryption => (
            StatusCode::SERVICE_UNAVAILABLE,
            "commerce.secret_unavailable",
            "commerce credentials are unavailable",
        ),
        CommerceServiceError::ProviderUnavailable => (
            StatusCode::BAD_GATEWAY,
            "commerce.provider_unavailable",
            "payment provider is unavailable",
        ),
        CommerceServiceError::ProviderRejected => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "commerce.provider_rejected",
            "payment provider rejected the operation",
        ),
        CommerceServiceError::InvalidSignature => (
            StatusCode::BAD_REQUEST,
            "commerce.invalid_webhook_signature",
            "webhook signature is invalid",
        ),
    };
    (
        status,
        Json(ffdb_protocol::ErrorEnvelope {
            error: ffdb_protocol::PlatformError::safe(code, message, request_id),
        }),
    )
        .into_response()
}

fn account_summary_from_row(
    project_id: ProjectId,
    row: &sqlx::postgres::PgRow,
    webhook_url: String,
) -> Result<CommerceAccountSummary, CommerceServiceError> {
    let capabilities: Vec<String> = row
        .try_get("capabilities")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(CommerceAccountSummary {
        project_id,
        mode: parse_mode(
            row.try_get("mode")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        status: parse_account_status(
            row.try_get("status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        livemode: row
            .try_get("livemode")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        provider_account_id: row
            .try_get("provider_account_id")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        capabilities: parse_capabilities(capabilities),
        requirements_due: row
            .try_get("requirements_due")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        disabled_reason: row
            .try_get("disabled_reason")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        webhook_url,
        secrets_configured: row
            .try_get("secrets_configured")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn product_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommerceProductSummary, CommerceServiceError> {
    let metadata: Value = row
        .try_get("metadata")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    let mut metadata = metadata
        .as_object()
        .cloned()
        .ok_or(CommerceServiceError::Unavailable)?;
    metadata.retain(|key, _| !key.starts_with("ffdb_"));
    let active: bool = row
        .try_get("active")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(CommerceProductSummary {
        id: CommerceProductId(
            row.try_get("id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        name: row
            .try_get("name")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        description: row
            .try_get("description")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        tax_code: row
            .try_get("tax_code")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        status: if active {
            CommerceProductStatus::Active
        } else {
            CommerceProductStatus::Archived
        },
        metadata,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn price_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommercePriceSummary, CommerceServiceError> {
    let billing_type: String = row
        .try_get("billing_type")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    let billing = match billing_type.as_str() {
        "one_time" => CommercePriceBilling::OneTime,
        "recurring" => {
            let interval: String = row
                .try_get("recurring_interval")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let count: i32 = row
                .try_get("recurring_interval_count")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            CommercePriceBilling::Recurring {
                interval: parse_wire_interval(&interval)?,
                interval_count: u16::try_from(count)
                    .map_err(|_| CommerceServiceError::Unavailable)?,
            }
        }
        _ => return Err(CommerceServiceError::Unavailable),
    };
    let entitlements: Value = row
        .try_get("entitlements")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(CommercePriceSummary {
        id: CommercePriceId(
            row.try_get("id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        product_id: CommerceProductId(
            row.try_get("product_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        lookup_key: row
            .try_get("lookup_key")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        currency: uppercase_currency(
            row.try_get("currency")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        unit_amount_minor: positive_row_u64(row, "unit_amount_minor")?,
        billing,
        entitlements: serde_json::from_value(entitlements)
            .map_err(|_| CommerceServiceError::Unavailable)?,
        active: row
            .try_get("active")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn order_summary_from_row(
    row: &sqlx::postgres::PgRow,
    project_id: ProjectId,
    order_id: CommerceOrderId,
    lines: Vec<CommerceOrderLineSummary>,
) -> Result<CommerceOrderSummary, CommerceServiceError> {
    Ok(CommerceOrderSummary {
        id: order_id,
        project_id,
        customer_id: row
            .try_get::<Option<Uuid>, _>("customer_id")
            .map_err(|_| CommerceServiceError::Unavailable)?
            .map(CommerceCustomerId),
        client_reference: row
            .try_get("client_reference")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        status: parse_order_status(
            row.try_get("status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        fulfillment_status: parse_fulfillment_status(
            row.try_get("fulfillment_status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        currency: uppercase_currency(
            row.try_get("currency")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        subtotal_minor: positive_or_zero_row_u64(row, "subtotal_minor")?,
        discount_minor: positive_or_zero_row_u64(row, "discount_minor")?,
        tax_minor: positive_or_zero_row_u64(row, "tax_minor")?,
        shipping_minor: positive_or_zero_row_u64(row, "shipping_minor")?,
        total_minor: positive_or_zero_row_u64(row, "total_minor")?,
        refunded_minor: positive_or_zero_row_u64(row, "refunded_minor")?,
        lines,
        paid_at_ms: row
            .try_get("paid_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn order_line_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommerceOrderLineSummary, CommerceServiceError> {
    let quantity: i32 = row
        .try_get("quantity")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(CommerceOrderLineSummary {
        product_id: CommerceProductId(
            row.try_get("product_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        price_id: CommercePriceId(
            row.try_get("price_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        product_name: row
            .try_get("product_name")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        currency: uppercase_currency(
            row.try_get("currency")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        unit_amount_minor: positive_row_u64(row, "unit_amount_minor")?,
        quantity: u32::try_from(quantity).map_err(|_| CommerceServiceError::Unavailable)?,
        line_total_minor: positive_row_u64(row, "line_total_minor")?,
    })
}

fn payment_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommercePaymentSummary, CommerceServiceError> {
    Ok(CommercePaymentSummary {
        id: CommercePaymentId(
            row.try_get("id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        order_id: row
            .try_get::<Option<Uuid>, _>("order_id")
            .map_err(|_| CommerceServiceError::Unavailable)?
            .map(CommerceOrderId),
        subscription_id: row
            .try_get::<Option<Uuid>, _>("subscription_id")
            .map_err(|_| CommerceServiceError::Unavailable)?
            .map(CommerceSubscriptionId),
        status: parse_payment_status(
            row.try_get("status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        currency: uppercase_currency(
            row.try_get("currency")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        authorized_minor: positive_or_zero_row_u64(row, "authorized_minor")?,
        captured_minor: positive_or_zero_row_u64(row, "captured_minor")?,
        refunded_minor: positive_or_zero_row_u64(row, "refunded_minor")?,
        provider_created_at_ms: row
            .try_get("provider_created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn subscription_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommerceSubscriptionSummary, CommerceServiceError> {
    Ok(CommerceSubscriptionSummary {
        id: CommerceSubscriptionId(
            row.try_get("id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        project_id: ProjectId(
            row.try_get("project_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        customer_id: CommerceCustomerId(
            row.try_get("customer_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        price_id: CommercePriceId(
            row.try_get("price_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        subject: CommerceMembershipSubject {
            kind: parse_subject_kind(
                row.try_get("subject_kind")
                    .map_err(|_| CommerceServiceError::Unavailable)?,
            )?,
            id: row
                .try_get("subject_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        },
        quantity: u32::try_from(
            row.try_get::<i32, _>("quantity")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )
        .map_err(|_| CommerceServiceError::Unavailable)?,
        status: parse_subscription_status(
            row.try_get("status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        current_period_start_ms: row
            .try_get("current_period_start_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        current_period_end_ms: row
            .try_get("current_period_end_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        cancel_at_period_end: row
            .try_get("cancel_at_period_end")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn entitlement_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommerceEntitlementSummary, CommerceServiceError> {
    let value: Value = row
        .try_get("entitlement_value")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(CommerceEntitlementSummary {
        subject: CommerceMembershipSubject {
            kind: parse_subject_kind(
                row.try_get("subject_kind")
                    .map_err(|_| CommerceServiceError::Unavailable)?,
            )?,
            id: row
                .try_get("subject_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        },
        key: row
            .try_get("entitlement_key")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        value: serde_json::from_value(value).map_err(|_| CommerceServiceError::Unavailable)?,
        subscription_id: row
            .try_get::<Option<Uuid>, _>("subscription_id")
            .map_err(|_| CommerceServiceError::Unavailable)?
            .map(CommerceSubscriptionId),
        order_id: row
            .try_get::<Option<Uuid>, _>("order_id")
            .map_err(|_| CommerceServiceError::Unavailable)?
            .map(CommerceOrderId),
        valid_from_ms: row
            .try_get("valid_from_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        valid_until_ms: row
            .try_get("valid_until_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn refund_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CommerceRefundSummary, CommerceServiceError> {
    let reason: Option<String> = row
        .try_get("reason")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(CommerceRefundSummary {
        id: CommerceRefundId(
            row.try_get("id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        payment_id: CommercePaymentId(
            row.try_get("payment_id")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        ),
        status: parse_refund_status(
            row.try_get("status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        amount_minor: positive_row_u64(row, "amount_minor")?,
        currency: uppercase_currency(
            row.try_get("currency")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?,
        reason: reason.as_deref().map(parse_refund_reason).transpose()?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|_| CommerceServiceError::Unavailable)?,
    })
}

fn parse_mode(value: &str) -> Result<CommerceProviderMode, CommerceServiceError> {
    match value {
        "byo_keys" => Ok(CommerceProviderMode::BringYourOwnKeys),
        "stripe_connect" => Ok(CommerceProviderMode::StripeConnect),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn parse_account_status(value: &str) -> Result<CommerceAccountStatus, CommerceServiceError> {
    match value {
        "configuring" => Ok(CommerceAccountStatus::Configuring),
        "onboarding" => Ok(CommerceAccountStatus::Onboarding),
        "enabled" => Ok(CommerceAccountStatus::Enabled),
        "restricted" => Ok(CommerceAccountStatus::Restricted),
        "disconnected" => Ok(CommerceAccountStatus::Disconnected),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

const fn account_status_name(value: CommerceAccountStatus) -> &'static str {
    match value {
        CommerceAccountStatus::Configuring => "configuring",
        CommerceAccountStatus::Onboarding => "onboarding",
        CommerceAccountStatus::Enabled => "enabled",
        CommerceAccountStatus::Restricted => "restricted",
        CommerceAccountStatus::Disconnected => "disconnected",
    }
}

fn parse_capabilities(values: Vec<String>) -> CommerceAccountCapabilities {
    CommerceAccountCapabilities {
        one_time_payments: values.iter().any(|value| value == "one_time_payments"),
        recurring_payments: values.iter().any(|value| value == "recurring_payments"),
        refunds: values.iter().any(|value| value == "refunds"),
        customer_portal: values.iter().any(|value| value == "customer_portal"),
    }
}

fn capability_names(value: &CommerceAccountCapabilities) -> Vec<String> {
    let mut result = Vec::new();
    if value.one_time_payments {
        result.push("one_time_payments".to_owned());
    }
    if value.recurring_payments {
        result.push("recurring_payments".to_owned());
    }
    if value.refunds {
        result.push("refunds".to_owned());
    }
    if value.customer_portal {
        result.push("customer_portal".to_owned());
    }
    result
}

fn require_capability(
    context: &ProviderContext,
    capability: MerchantCapability,
) -> Result<(), CommerceServiceError> {
    let available = match capability {
        MerchantCapability::OneTimePayments => context.capabilities.one_time_payments,
        MerchantCapability::RecurringPayments => context.capabilities.recurring_payments,
        MerchantCapability::Refunds => context.capabilities.refunds,
    };
    if available {
        Ok(())
    } else {
        Err(CommerceServiceError::CapabilityUnavailable)
    }
}

fn merchant_domain(
    project_id: ProjectId,
    context: &ProviderContext,
) -> Result<MerchantAccount, CommerceServiceError> {
    let mode = match context.mode {
        CommerceProviderMode::BringYourOwnKeys => MerchantProviderMode::BringYourOwnCredentials {
            credential_reference: SecretReference::new(format!(
                "secret://commerce/{project_id}/stripe"
            ))
            .map_err(|_| CommerceServiceError::Unavailable)?,
        },
        CommerceProviderMode::StripeConnect => MerchantProviderMode::ConnectedAccount {
            account_reference: ProviderReference::new(
                context
                    .provider_account_id
                    .clone()
                    .ok_or(CommerceServiceError::Unavailable)?,
            )
            .map_err(|_| CommerceServiceError::Unavailable)?,
            charge_model: ffdb_commerce::ConnectedChargeModel::Direct,
        },
    };
    let mut merchant = MerchantAccount::new(MerchantAccountId::new(), project_id, mode);
    let status = match context.status {
        CommerceAccountStatus::Enabled => MerchantAccountStatus::Active,
        CommerceAccountStatus::Restricted => MerchantAccountStatus::Restricted,
        CommerceAccountStatus::Disconnected => MerchantAccountStatus::Disabled,
        CommerceAccountStatus::Configuring | CommerceAccountStatus::Onboarding => {
            MerchantAccountStatus::Pending
        }
    };
    let mut capabilities = Vec::new();
    if context.capabilities.one_time_payments {
        capabilities.push(MerchantCapability::OneTimePayments);
    }
    if context.capabilities.recurring_payments {
        capabilities.push(MerchantCapability::RecurringPayments);
    }
    if context.capabilities.refunds {
        capabilities.push(MerchantCapability::Refunds);
    }
    merchant.set_provider_state(status, capabilities);
    Ok(merchant)
}

fn parse_v1_account(
    payload: &Value,
    livemode: bool,
) -> Result<ProviderAccountState, CommerceServiceError> {
    let id = provider_id(payload, "id", "acct_")?;
    let charges_enabled = payload
        .get("charges_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let details_submitted = payload
        .get("details_submitted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let card_payments = payload
        .pointer("/capabilities/card_payments")
        .and_then(Value::as_str)
        == Some("active");
    let requirements_due = payload
        .pointer("/requirements/currently_due")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| value.len() <= 255)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let disabled_reason = payload
        .pointer("/requirements/disabled_reason")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(255).collect::<String>());
    let status = if charges_enabled && card_payments {
        CommerceAccountStatus::Enabled
    } else if !details_submitted {
        CommerceAccountStatus::Onboarding
    } else {
        CommerceAccountStatus::Restricted
    };
    Ok(ProviderAccountState {
        id,
        status,
        livemode,
        capabilities: CommerceAccountCapabilities {
            one_time_payments: charges_enabled && card_payments,
            recurring_payments: charges_enabled && card_payments,
            // Refunds reverse card payments and don't depend on payout
            // capability being enabled for the merchant account.
            refunds: charges_enabled && card_payments,
            customer_portal: charges_enabled && card_payments,
        },
        requirements_due,
        disabled_reason,
    })
}

fn parse_v2_account(
    payload: &Value,
    livemode: bool,
) -> Result<ProviderAccountState, CommerceServiceError> {
    let id = provider_id(payload, "id", "acct_")?;
    let card_payments = payload
        .pointer("/configuration/merchant/capabilities/card_payments/status")
        .and_then(Value::as_str)
        == Some("active");
    let requirements = payload
        .pointer("/requirements/entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut requirements_due = Vec::new();
    for entry in &requirements {
        let status = entry
            .pointer("/minimum_deadline/status")
            .and_then(Value::as_str)
            .or_else(|| entry.get("status").and_then(Value::as_str));
        if matches!(status, Some("eventually_due" | "pending" | "satisfied")) {
            continue;
        }
        let reference = entry
            .get("id")
            .or_else(|| entry.get("reference"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 255)
            .unwrap_or("account_requirement");
        requirements_due.push(reference.to_owned());
        if requirements_due.len() == 100 {
            break;
        }
    }
    requirements_due.sort();
    requirements_due.dedup();
    let disabled_reason = payload
        .pointer("/configuration/merchant/capabilities/card_payments/status_details/code")
        .or_else(|| {
            payload
                .pointer("/configuration/merchant/capabilities/card_payments/status_details/reason")
        })
        .and_then(Value::as_str)
        .map(|value| value.chars().take(255).collect::<String>());
    let status = if card_payments {
        CommerceAccountStatus::Enabled
    } else if requirements_due.is_empty() {
        CommerceAccountStatus::Restricted
    } else {
        CommerceAccountStatus::Onboarding
    };
    Ok(ProviderAccountState {
        id,
        status,
        livemode,
        capabilities: CommerceAccountCapabilities {
            one_time_payments: card_payments,
            recurring_payments: card_payments,
            refunds: card_payments,
            customer_portal: card_payments,
        },
        requirements_due,
        disabled_reason,
    })
}

fn domain_entitlements(
    values: &BTreeMap<String, CommerceEntitlementValue>,
) -> Result<BTreeMap<EntitlementKey, EntitlementValue>, CommerceServiceError> {
    values
        .iter()
        .map(|(key, value)| {
            let key = EntitlementKey::new(key.clone())
                .map_err(|_| CommerceServiceError::InvalidRequest)?;
            let value = match value {
                CommerceEntitlementValue::Enabled(value) => EntitlementValue::Enabled(*value),
                CommerceEntitlementValue::Quantity(value) => EntitlementValue::Quantity(*value),
                CommerceEntitlementValue::Text(value) => {
                    if value.is_empty() || value.len() > 1_000 {
                        return Err(CommerceServiceError::InvalidRequest);
                    }
                    EntitlementValue::Text(value.clone())
                }
            };
            Ok((key, value))
        })
        .collect()
}

fn domain_subject(
    subject: &CommerceMembershipSubject,
) -> Result<ffdb_commerce::MembershipSubject, CommerceServiceError> {
    let id = parse_uuid(&subject.id)?;
    Ok(match subject.kind {
        CommerceMembershipSubjectKind::Individual => {
            ffdb_commerce::MembershipSubject::Individual(ffdb_commerce::IndividualId::from_uuid(id))
        }
        CommerceMembershipSubjectKind::Team => {
            ffdb_commerce::MembershipSubject::Team(ffdb_commerce::TeamId::from_uuid(id))
        }
        CommerceMembershipSubjectKind::Organization => {
            ffdb_commerce::MembershipSubject::Organization(
                ffdb_commerce::SubjectOrganizationId::from_uuid(id),
            )
        }
    })
}

const fn domain_interval_unit(value: CommerceBillingIntervalUnit) -> BillingIntervalUnit {
    match value {
        CommerceBillingIntervalUnit::Day => BillingIntervalUnit::Day,
        CommerceBillingIntervalUnit::Week => BillingIntervalUnit::Week,
        CommerceBillingIntervalUnit::Month => BillingIntervalUnit::Month,
        CommerceBillingIntervalUnit::Year => BillingIntervalUnit::Year,
    }
}

fn parse_domain_interval(value: &str) -> Result<BillingIntervalUnit, CommerceServiceError> {
    match value {
        "day" => Ok(BillingIntervalUnit::Day),
        "week" => Ok(BillingIntervalUnit::Week),
        "month" => Ok(BillingIntervalUnit::Month),
        "year" => Ok(BillingIntervalUnit::Year),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn parse_wire_interval(value: &str) -> Result<CommerceBillingIntervalUnit, CommerceServiceError> {
    match value {
        "day" => Ok(CommerceBillingIntervalUnit::Day),
        "week" => Ok(CommerceBillingIntervalUnit::Week),
        "month" => Ok(CommerceBillingIntervalUnit::Month),
        "year" => Ok(CommerceBillingIntervalUnit::Year),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

const fn interval_name(value: CommerceBillingIntervalUnit) -> &'static str {
    match value {
        CommerceBillingIntervalUnit::Day => "day",
        CommerceBillingIntervalUnit::Week => "week",
        CommerceBillingIntervalUnit::Month => "month",
        CommerceBillingIntervalUnit::Year => "year",
    }
}

fn price_db_values(
    billing: &CommercePriceBilling,
) -> (&'static str, Option<&'static str>, Option<i32>) {
    match *billing {
        CommercePriceBilling::OneTime => ("one_time", None, None),
        CommercePriceBilling::Recurring {
            interval,
            interval_count,
        } => (
            "recurring",
            Some(interval_name(interval)),
            Some(i32::from(interval_count)),
        ),
    }
}

const fn subject_kind_name(value: CommerceMembershipSubjectKind) -> &'static str {
    match value {
        CommerceMembershipSubjectKind::Individual => "individual",
        CommerceMembershipSubjectKind::Team => "team",
        CommerceMembershipSubjectKind::Organization => "organization",
    }
}

fn parse_subject_kind(value: &str) -> Result<CommerceMembershipSubjectKind, CommerceServiceError> {
    match value {
        "individual" => Ok(CommerceMembershipSubjectKind::Individual),
        "team" => Ok(CommerceMembershipSubjectKind::Team),
        "organization" => Ok(CommerceMembershipSubjectKind::Organization),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn parse_order_status(value: &str) -> Result<CommerceOrderStatus, CommerceServiceError> {
    match value {
        "pending" => Ok(CommerceOrderStatus::Pending),
        "checkout_created" => Ok(CommerceOrderStatus::CheckoutCreated),
        "processing" => Ok(CommerceOrderStatus::Processing),
        "paid" => Ok(CommerceOrderStatus::Paid),
        "payment_failed" => Ok(CommerceOrderStatus::PaymentFailed),
        "canceled" => Ok(CommerceOrderStatus::Canceled),
        "partially_refunded" => Ok(CommerceOrderStatus::PartiallyRefunded),
        "refunded" => Ok(CommerceOrderStatus::Refunded),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn parse_fulfillment_status(
    value: &str,
) -> Result<CommerceFulfillmentStatus, CommerceServiceError> {
    match value {
        "unfulfilled" => Ok(CommerceFulfillmentStatus::Unfulfilled),
        "processing" => Ok(CommerceFulfillmentStatus::Processing),
        "fulfilled" => Ok(CommerceFulfillmentStatus::Fulfilled),
        "canceled" => Ok(CommerceFulfillmentStatus::Canceled),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

const fn fulfillment_status_name(value: CommerceFulfillmentStatus) -> &'static str {
    match value {
        CommerceFulfillmentStatus::Unfulfilled => "unfulfilled",
        CommerceFulfillmentStatus::Processing => "processing",
        CommerceFulfillmentStatus::Fulfilled => "fulfilled",
        CommerceFulfillmentStatus::Canceled => "canceled",
    }
}

fn parse_payment_status(value: &str) -> Result<CommercePaymentStatus, CommerceServiceError> {
    match value {
        "requires_payment_method" => Ok(CommercePaymentStatus::RequiresPaymentMethod),
        "requires_action" => Ok(CommercePaymentStatus::RequiresAction),
        "processing" => Ok(CommercePaymentStatus::Processing),
        "authorized" => Ok(CommercePaymentStatus::Authorized),
        "captured" => Ok(CommercePaymentStatus::Captured),
        "partially_refunded" => Ok(CommercePaymentStatus::PartiallyRefunded),
        "refunded" => Ok(CommercePaymentStatus::Refunded),
        "failed" => Ok(CommercePaymentStatus::Failed),
        "canceled" => Ok(CommercePaymentStatus::Canceled),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn parse_subscription_status(
    value: &str,
) -> Result<CommerceSubscriptionStatus, CommerceServiceError> {
    match value {
        "checkout_pending" => Ok(CommerceSubscriptionStatus::CheckoutPending),
        "trialing" => Ok(CommerceSubscriptionStatus::Trialing),
        "active" => Ok(CommerceSubscriptionStatus::Active),
        "past_due" => Ok(CommerceSubscriptionStatus::PastDue),
        "unpaid" => Ok(CommerceSubscriptionStatus::Unpaid),
        "paused" => Ok(CommerceSubscriptionStatus::Paused),
        "canceled" => Ok(CommerceSubscriptionStatus::Canceled),
        "incomplete" => Ok(CommerceSubscriptionStatus::Incomplete),
        "expired" => Ok(CommerceSubscriptionStatus::Expired),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn parse_provider_subscription_status(value: &str) -> Result<&'static str, CommerceServiceError> {
    match value {
        "trialing" => Ok("trialing"),
        "active" => Ok("active"),
        "past_due" => Ok("past_due"),
        "unpaid" => Ok("unpaid"),
        "paused" => Ok("paused"),
        "canceled" => Ok("canceled"),
        "incomplete" => Ok("incomplete"),
        "incomplete_expired" => Ok("expired"),
        _ => Err(CommerceServiceError::InvalidRequest),
    }
}

fn parse_refund_status(value: &str) -> Result<CommerceRefundStatus, CommerceServiceError> {
    match value {
        "pending" => Ok(CommerceRefundStatus::Pending),
        "succeeded" => Ok(CommerceRefundStatus::Succeeded),
        "failed" => Ok(CommerceRefundStatus::Failed),
        "canceled" => Ok(CommerceRefundStatus::Canceled),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

const fn refund_reason_name(value: CommerceRefundReason) -> &'static str {
    match value {
        CommerceRefundReason::Duplicate => "duplicate",
        CommerceRefundReason::Fraudulent => "fraudulent",
        CommerceRefundReason::RequestedByCustomer => "requested_by_customer",
        CommerceRefundReason::Other => "other",
    }
}

fn parse_refund_reason(value: &str) -> Result<CommerceRefundReason, CommerceServiceError> {
    match value {
        "duplicate" => Ok(CommerceRefundReason::Duplicate),
        "fraudulent" => Ok(CommerceRefundReason::Fraudulent),
        "requested_by_customer" => Ok(CommerceRefundReason::RequestedByCustomer),
        "other" => Ok(CommerceRefundReason::Other),
        _ => Err(CommerceServiceError::Unavailable),
    }
}

fn validate_stripe_secret(value: &str, prefix: &str) -> Result<(), CommerceServiceError> {
    if !value.starts_with(prefix)
        || value.len() < prefix.len() + 12
        || value.len() > 512
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), CommerceServiceError> {
    if !(8..=200).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',' && byte != b';')
    {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(())
}

fn validate_country(value: &str) -> Result<(), CommerceServiceError> {
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(())
}

fn validate_email(value: &str) -> Result<(), CommerceServiceError> {
    if value.len() > 320
        || value.trim() != value
        || !value.contains('@')
        || value.chars().any(char::is_control)
    {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(())
}

fn validate_lookup_key(value: &str) -> Result<(), CommerceServiceError> {
    if value.is_empty()
        || value.len() > 100
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(())
}

fn validate_return_url(value: &str) -> Result<Url, CommerceServiceError> {
    let url = Url::parse(value).map_err(|_| CommerceServiceError::InvalidRequest)?;
    let local_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
        });
    if url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || local_http)
    {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(url)
}

fn validate_checkout_urls(success: &str, cancel: &str) -> Result<(), CommerceServiceError> {
    validate_return_url(success)?;
    validate_return_url(cancel)?;
    Ok(())
}

fn metadata_scalar(value: &Value) -> Result<String, CommerceServiceError> {
    let rendered = match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => {
            return Err(CommerceServiceError::InvalidRequest);
        }
    };
    if rendered.len() > 500 {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(rendered)
}

fn validate_product_metadata_key(value: &str) -> Result<(), CommerceServiceError> {
    if value.is_empty()
        || value.len() > 40
        || value.starts_with("ffdb_")
        || value.contains(['[', ']'])
        || value.chars().any(char::is_control)
    {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(())
}

fn parse_wire_id(value: &str) -> Result<Uuid, CommerceServiceError> {
    parse_uuid(value)
}

fn parse_uuid(value: &str) -> Result<Uuid, CommerceServiceError> {
    Uuid::parse_str(value).map_err(|_| CommerceServiceError::InvalidRequest)
}

fn valid_provider_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && (prefix.len() + 4..=255).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn provider_id(payload: &Value, field: &str, prefix: &str) -> Result<String, CommerceServiceError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_provider_id(value, prefix))
        .map(str::to_owned)
        .ok_or(CommerceServiceError::ProviderResponseInvalid)
}

fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str, CommerceServiceError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 255)
        .ok_or(CommerceServiceError::InvalidRequest)
}

fn required_i64(payload: &Value, field: &str) -> Result<i64, CommerceServiceError> {
    payload
        .get(field)
        .and_then(Value::as_i64)
        .ok_or(CommerceServiceError::ProviderResponseInvalid)
}

fn required_currency_lower(payload: &Value, field: &str) -> Result<String, CommerceServiceError> {
    let value = required_str(payload, field)?;
    if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return Err(CommerceServiceError::InvalidRequest);
    }
    Ok(value.to_owned())
}

fn verified_https_url(payload: &Value, field: &str) -> Result<String, CommerceServiceError> {
    let value = payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or(CommerceServiceError::ProviderResponseInvalid)?;
    let url = Url::parse(value).map_err(|_| CommerceServiceError::ProviderResponseInvalid)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(CommerceServiceError::ProviderResponseInvalid);
    }
    Ok(value.to_owned())
}

fn verified_stripe_url(payload: &Value, field: &str) -> Result<String, CommerceServiceError> {
    let value = verified_https_url(payload, field)?;
    let url = Url::parse(&value).map_err(|_| CommerceServiceError::ProviderResponseInvalid)?;
    let host = url
        .host_str()
        .ok_or(CommerceServiceError::ProviderResponseInvalid)?;
    if host != "stripe.com" && !host.ends_with(".stripe.com") {
        return Err(CommerceServiceError::ProviderResponseInvalid);
    }
    Ok(value)
}

fn seconds_to_ms(value: i64) -> Result<i64, CommerceServiceError> {
    value
        .checked_mul(1_000)
        .ok_or(CommerceServiceError::ProviderResponseInvalid)
}

fn u64_to_i64(value: u64) -> Result<i64, CommerceServiceError> {
    i64::try_from(value).map_err(|_| CommerceServiceError::InvalidRequest)
}

fn positive_row_u64(row: &sqlx::postgres::PgRow, field: &str) -> Result<u64, CommerceServiceError> {
    let value: i64 = row
        .try_get(field)
        .map_err(|_| CommerceServiceError::Unavailable)?;
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(CommerceServiceError::Unavailable)
}

fn positive_or_zero_row_u64(
    row: &sqlx::postgres::PgRow,
    field: &str,
) -> Result<u64, CommerceServiceError> {
    let value: i64 = row
        .try_get(field)
        .map_err(|_| CommerceServiceError::Unavailable)?;
    u64::try_from(value).map_err(|_| CommerceServiceError::Unavailable)
}

fn uppercase_currency(value: String) -> Result<String, CommerceServiceError> {
    Currency::new(value.to_ascii_uppercase())
        .map(|currency| currency.as_str().to_owned())
        .map_err(|_| CommerceServiceError::Unavailable)
}

fn sha256_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(value);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn stable_entity_uuid(project_id: ProjectId, kind: &str, idempotency_key: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.commerce.entity.v1\0");
    digest.update(project_id.0.as_bytes());
    digest.update([0]);
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(idempotency_key.as_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 9562 UUIDv8: application-defined bytes with the standard variant.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn verify_stripe_signature(
    webhook_secret: &str,
    payload: &[u8],
    signature_header: &str,
    now_seconds: i64,
) -> Result<(), CommerceServiceError> {
    if signature_header.len() > 4096 || webhook_secret.is_empty() {
        return Err(CommerceServiceError::InvalidSignature);
    }
    let mut timestamp = None;
    let mut v1_signatures = Vec::new();
    for component in signature_header.split(',') {
        let Some((name, value)) = component.trim().split_once('=') else {
            return Err(CommerceServiceError::InvalidSignature);
        };
        match name {
            "t" => {
                if timestamp.is_some() {
                    return Err(CommerceServiceError::InvalidSignature);
                }
                timestamp = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| CommerceServiceError::InvalidSignature)?,
                );
            }
            "v1" => {
                let decoded = decode_hex_32(value)?;
                v1_signatures.push(decoded);
            }
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(CommerceServiceError::InvalidSignature)?;
    if timestamp > now_seconds.saturating_add(WEBHOOK_TOLERANCE_SECONDS)
        || timestamp < now_seconds.saturating_sub(WEBHOOK_TOLERANCE_SECONDS)
        || v1_signatures.is_empty()
    {
        return Err(CommerceServiceError::InvalidSignature);
    }
    let mut signed_payload = timestamp.to_string().into_bytes();
    signed_payload.push(b'.');
    signed_payload.extend_from_slice(payload);
    let key = hmac::Key::new(hmac::HMAC_SHA256, webhook_secret.as_bytes());
    if v1_signatures
        .iter()
        .any(|signature| hmac::verify(&key, &signed_payload, signature).is_ok())
    {
        Ok(())
    } else {
        Err(CommerceServiceError::InvalidSignature)
    }
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], CommerceServiceError> {
    if value.len() != 64 {
        return Err(CommerceServiceError::InvalidSignature);
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_digit(pair[0]).ok_or(CommerceServiceError::InvalidSignature)?;
        let low = decode_hex_digit(pair[1]).ok_or(CommerceServiceError::InvalidSignature)?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn decode_hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn provider_event_is_newer(
    row: &sqlx::postgres::PgRow,
    provider_created_ms: i64,
    event_id: &str,
) -> Result<bool, CommerceServiceError> {
    let previous_at: Option<chrono::DateTime<chrono::Utc>> = row
        .try_get("last_provider_event_created_at")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    let previous_id: Option<String> = row
        .try_get("last_provider_event_id")
        .map_err(|_| CommerceServiceError::Unavailable)?;
    Ok(match (previous_at, previous_id) {
        (None, _) => true,
        (Some(previous_at), previous_id) => {
            let previous_ms = previous_at.timestamp_millis();
            provider_created_ms > previous_ms
                || (provider_created_ms == previous_ms
                    && previous_id.as_deref().is_none_or(|value| event_id > value))
        }
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WebhookOutcome {
    Processed,
    Duplicate,
}

impl CommerceService {
    async fn apply_byo_webhook(
        &self,
        project_id: ProjectId,
        payload: &[u8],
        signature: &str,
        now_seconds: i64,
    ) -> Result<WebhookOutcome, CommerceServiceError> {
        let context = self.provider_context(project_id, false).await?;
        if context.mode != CommerceProviderMode::BringYourOwnKeys {
            return Err(CommerceServiceError::Conflict);
        }
        verify_stripe_signature(
            context.webhook_secret.expose_secret(),
            payload,
            signature,
            now_seconds,
        )?;
        let event: Value =
            serde_json::from_slice(payload).map_err(|_| CommerceServiceError::InvalidRequest)?;
        if event.get("account").is_some() {
            return Err(CommerceServiceError::InvalidSignature);
        }
        self.apply_verified_webhook(project_id, &context, payload, &event)
            .await
    }

    async fn apply_connect_webhook(
        &self,
        payload: &[u8],
        signature: &str,
        now_seconds: i64,
    ) -> Result<WebhookOutcome, CommerceServiceError> {
        let connect = self
            .connect
            .as_ref()
            .ok_or(CommerceServiceError::InvalidConfiguration)?;
        // The global Connect endpoint secret is verified before parsing or
        // performing an account lookup, so untrusted JSON cannot select a
        // project or disclose whether a connected account exists.
        verify_stripe_signature(
            connect.webhook_secret.expose_secret(),
            payload,
            signature,
            now_seconds,
        )?;
        let event: Value =
            serde_json::from_slice(payload).map_err(|_| CommerceServiceError::InvalidRequest)?;
        let account_id = required_str(&event, "account")?;
        if !valid_provider_id(account_id, "acct_") {
            return Err(CommerceServiceError::InvalidSignature);
        }
        let project_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT project_id FROM project_commerce_accounts \
             WHERE provider='stripe' AND mode='stripe_connect' AND provider_account_id=$1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .map(ProjectId)
        .ok_or(CommerceServiceError::InvalidSignature)?;
        let context = self.provider_context(project_id, false).await?;
        if context.mode != CommerceProviderMode::StripeConnect
            || context.provider_account_id.as_deref() != Some(account_id)
        {
            return Err(CommerceServiceError::InvalidSignature);
        }
        self.apply_verified_webhook(project_id, &context, payload, &event)
            .await
    }

    async fn apply_verified_webhook(
        &self,
        project_id: ProjectId,
        context: &ProviderContext,
        payload: &[u8],
        event: &Value,
    ) -> Result<WebhookOutcome, CommerceServiceError> {
        let event_id = provider_id(event, "id", "evt_")?;
        let event_type = required_str(event, "type")?;
        if event_type.len() > 255 {
            return Err(CommerceServiceError::InvalidRequest);
        }
        let livemode = event
            .get("livemode")
            .and_then(Value::as_bool)
            .ok_or(CommerceServiceError::InvalidRequest)?;
        if livemode != context.livemode {
            return Err(CommerceServiceError::InvalidSignature);
        }
        if context.mode == CommerceProviderMode::StripeConnect
            && event.get("account").and_then(Value::as_str)
                != context.provider_account_id.as_deref()
        {
            return Err(CommerceServiceError::InvalidSignature);
        }
        let provider_created_seconds = required_i64(event, "created")?;
        let provider_created_ms = seconds_to_ms(provider_created_seconds)?;
        let hash: [u8; 32] = Sha256::digest(payload).into();
        let provider_account = context
            .provider_account_id
            .as_deref()
            .unwrap_or("byo_project_account");
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO commerce_webhook_events \
             (project_id,provider,provider_account_id,provider_event_id,event_type,livemode,\
              payload_sha256,provider_created_at) \
             VALUES ($1,'stripe',$2,$3,$4,$5,$6,to_timestamp($7::double precision/1000)) \
             ON CONFLICT (project_id,provider,provider_event_id) DO NOTHING",
        )
        .bind(project_id.0)
        .bind(provider_account)
        .bind(&event_id)
        .bind(event_type)
        .bind(livemode)
        .bind(hash.as_slice())
        .bind(provider_created_ms)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if inserted.rows_affected() == 0 {
            let existing = sqlx::query(
                "SELECT payload_sha256,processed_at IS NOT NULL processed \
                 FROM commerce_webhook_events WHERE project_id=$1 AND provider='stripe' \
                  AND provider_event_id=$2 FOR UPDATE",
            )
            .bind(project_id.0)
            .bind(&event_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            let existing_hash: Vec<u8> = existing
                .try_get("payload_sha256")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            if existing_hash.as_slice() != hash {
                return Err(CommerceServiceError::WebhookHashConflict);
            }
            let processed: bool = existing
                .try_get("processed")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            if processed {
                transaction
                    .commit()
                    .await
                    .map_err(|_| CommerceServiceError::Unavailable)?;
                return Ok(WebhookOutcome::Duplicate);
            }
        }
        let object = event
            .pointer("/data/object")
            .ok_or(CommerceServiceError::InvalidRequest)?;
        let process_result = self
            .process_webhook_event(
                &mut transaction,
                project_id,
                &event_id,
                event_type,
                provider_created_ms,
                object,
            )
            .await;
        if let Err(error) = process_result {
            let message = error.to_string();
            sqlx::query(
                "UPDATE commerce_webhook_events SET processing_error=$3 \
                 WHERE project_id=$1 AND provider='stripe' AND provider_event_id=$2",
            )
            .bind(project_id.0)
            .bind(&event_id)
            .bind(message.chars().take(2_000).collect::<String>())
            .execute(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            transaction
                .commit()
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
            return Err(error);
        }
        sqlx::query(
            "UPDATE commerce_webhook_events SET processed_at=now(),processing_error=NULL \
             WHERE project_id=$1 AND provider='stripe' AND provider_event_id=$2",
        )
        .bind(project_id.0)
        .bind(&event_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(WebhookOutcome::Processed)
    }

    async fn process_webhook_event(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        event_id: &str,
        event_type: &str,
        provider_created_ms: i64,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        match event_type {
            "checkout.session.completed" => {
                self.apply_checkout_completed(
                    transaction,
                    project_id,
                    event_id,
                    provider_created_ms,
                    object,
                )
                .await
            }
            "checkout.session.expired" => {
                self.apply_checkout_expired(transaction, project_id, object)
                    .await
            }
            "payment_intent.created"
            | "payment_intent.processing"
            | "payment_intent.amount_capturable_updated"
            | "payment_intent.succeeded"
            | "payment_intent.payment_failed"
            | "payment_intent.canceled" => {
                self.apply_payment_intent(
                    transaction,
                    project_id,
                    event_id,
                    event_type,
                    provider_created_ms,
                    object,
                )
                .await
            }
            "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted"
            | "customer.subscription.paused"
            | "customer.subscription.resumed" => {
                self.apply_subscription(
                    transaction,
                    project_id,
                    event_id,
                    provider_created_ms,
                    object,
                )
                .await
            }
            "invoice.paid" | "invoice.payment_failed" | "invoice.payment_action_required" => {
                self.apply_subscription_invoice(
                    transaction,
                    project_id,
                    event_id,
                    event_type,
                    provider_created_ms,
                    object,
                )
                .await
            }
            "refund.created" | "refund.updated" | "refund.failed" => {
                self.apply_refund(
                    transaction,
                    project_id,
                    event_id,
                    provider_created_ms,
                    object,
                )
                .await
            }
            _ => Ok(()),
        }
    }

    async fn apply_checkout_completed(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        event_id: &str,
        provider_created_ms: i64,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        let session_id = provider_id(object, "id", "cs_")?;
        let metadata = object
            .get("metadata")
            .and_then(Value::as_object)
            .ok_or(CommerceServiceError::InvalidRequest)?;
        if let Some(order_id) = metadata.get("ffdb_order_id").and_then(Value::as_str) {
            let order_id = parse_uuid(order_id)?;
            let stored: Option<String> = sqlx::query_scalar(
                "SELECT provider_checkout_session_id FROM commerce_orders \
                 WHERE project_id=$1 AND id=$2 FOR UPDATE",
            )
            .bind(project_id.0)
            .bind(order_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            if stored.as_deref() != Some(session_id.as_str()) {
                return Err(CommerceServiceError::Conflict);
            }
            let payment_status = required_str(object, "payment_status")?;
            if payment_status == "paid" {
                let payment_intent = object
                    .get("payment_intent")
                    .and_then(Value::as_str)
                    .filter(|value| valid_provider_id(value, "pi_"))
                    .ok_or(CommerceServiceError::InvalidRequest)?;
                let amount = object
                    .get("amount_total")
                    .and_then(Value::as_u64)
                    .ok_or(CommerceServiceError::InvalidRequest)?;
                let currency = required_currency_lower(object, "currency")?;
                let order_amount: i64 = sqlx::query_scalar(
                    "SELECT total_minor FROM commerce_orders WHERE project_id=$1 AND id=$2",
                )
                .bind(project_id.0)
                .bind(order_id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
                if u64::try_from(order_amount).map_err(|_| CommerceServiceError::Unavailable)?
                    != amount
                {
                    return Err(CommerceServiceError::Conflict);
                }
                if !self
                    .upsert_payment(
                        transaction,
                        project_id,
                        Some(order_id),
                        None,
                        payment_intent,
                        None,
                        "captured",
                        &currency,
                        amount,
                        amount,
                        provider_created_ms,
                        event_id,
                    )
                    .await?
                {
                    return Ok(());
                }
                sqlx::query(
                    "UPDATE commerce_orders SET status='paid',provider_payment_intent_id=$3,\
                     paid_at=coalesce(paid_at,now()),updated_at=now() WHERE project_id=$1 AND id=$2",
                )
                .bind(project_id.0)
                .bind(order_id)
                .bind(payment_intent)
                .execute(&mut **transaction)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
            } else {
                sqlx::query(
                    "UPDATE commerce_orders SET status='processing',updated_at=now() \
                     WHERE project_id=$1 AND id=$2 AND status='checkout_created'",
                )
                .bind(project_id.0)
                .bind(order_id)
                .execute(&mut **transaction)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
            }
        } else if let Some(subscription_id) =
            metadata.get("ffdb_subscription_id").and_then(Value::as_str)
        {
            let subscription_id = parse_uuid(subscription_id)?;
            let provider_subscription = object
                .get("subscription")
                .and_then(Value::as_str)
                .filter(|value| valid_provider_id(value, "sub_"))
                .ok_or(CommerceServiceError::InvalidRequest)?;
            let customer_id: Option<Uuid> = sqlx::query_scalar(
                "UPDATE commerce_subscriptions SET provider_subscription_id=$3,updated_at=now() \
                 WHERE project_id=$1 AND id=$2 AND provider_checkout_session_id=$4 \
                 RETURNING customer_id",
            )
            .bind(project_id.0)
            .bind(subscription_id)
            .bind(provider_subscription)
            .bind(session_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            let Some(customer_id) = customer_id else {
                return Err(CommerceServiceError::Conflict);
            };
            if let Some(provider_customer_id) = object
                .get("customer")
                .and_then(Value::as_str)
                .filter(|value| valid_provider_id(value, "cus_"))
            {
                sqlx::query(
                    "UPDATE commerce_customers SET provider_customer_id=$3,updated_at=now() \
                     WHERE project_id=$1 AND id=$2",
                )
                .bind(project_id.0)
                .bind(customer_id)
                .bind(provider_customer_id)
                .execute(&mut **transaction)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
            }
        }
        Ok(())
    }

    async fn apply_checkout_expired(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        let session_id = provider_id(object, "id", "cs_")?;
        sqlx::query(
            "UPDATE commerce_orders SET status='canceled',canceled_at=now(),updated_at=now() \
             WHERE project_id=$1 AND provider_checkout_session_id=$2 \
              AND status IN ('pending','checkout_created','processing','payment_failed')",
        )
        .bind(project_id.0)
        .bind(&session_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query(
            "UPDATE commerce_subscriptions SET status='expired',updated_at=now() \
             WHERE project_id=$1 AND provider_checkout_session_id=$2 AND status='checkout_pending'",
        )
        .bind(project_id.0)
        .bind(session_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(())
    }

    async fn apply_payment_intent(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        event_id: &str,
        event_type: &str,
        provider_created_ms: i64,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        let payment_intent = provider_id(object, "id", "pi_")?;
        let metadata = object.get("metadata").and_then(Value::as_object);
        let order_id = metadata
            .and_then(|value| value.get("ffdb_order_id"))
            .and_then(Value::as_str)
            .map(parse_uuid)
            .transpose()?;
        let subscription_id = metadata
            .and_then(|value| value.get("ffdb_subscription_id"))
            .and_then(Value::as_str)
            .map(parse_uuid)
            .transpose()?;
        if order_id.is_none() == subscription_id.is_none() {
            // Ignore unrelated provider payments without allowing them to bind
            // to a project commerce aggregate.
            return Ok(());
        }
        let currency = required_currency_lower(object, "currency")?;
        let amount = object
            .get("amount")
            .and_then(Value::as_u64)
            .ok_or(CommerceServiceError::InvalidRequest)?;
        let captured = object
            .get("amount_received")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if captured > amount {
            return Err(CommerceServiceError::Conflict);
        }
        let status = if event_type == "payment_intent.payment_failed" {
            "failed"
        } else {
            match required_str(object, "status")? {
                "requires_payment_method" => "requires_payment_method",
                "requires_action" => "requires_action",
                "processing" => "processing",
                "requires_capture" => "authorized",
                "succeeded" => "captured",
                "canceled" => "canceled",
                _ => return Err(CommerceServiceError::InvalidRequest),
            }
        };
        let charge = object
            .get("latest_charge")
            .and_then(Value::as_str)
            .filter(|value| valid_provider_id(value, "ch_"));
        let applied = self
            .upsert_payment(
                transaction,
                project_id,
                order_id,
                subscription_id,
                &payment_intent,
                charge,
                status,
                &currency,
                amount,
                captured,
                provider_created_ms,
                event_id,
            )
            .await?;
        if !applied {
            return Ok(());
        }
        if let Some(order_id) = order_id {
            let order_status = match status {
                "captured" => "paid",
                "failed" => "payment_failed",
                "canceled" => "canceled",
                _ => "processing",
            };
            sqlx::query(
                "UPDATE commerce_orders SET status=$3,provider_payment_intent_id=$4,\
                  provider_charge_id=$5,paid_at=CASE WHEN $3='paid' THEN coalesce(paid_at,now()) \
                  ELSE paid_at END,updated_at=now() WHERE project_id=$1 AND id=$2 \
                  AND status NOT IN ('refunded','partially_refunded')",
            )
            .bind(project_id.0)
            .bind(order_id)
            .bind(order_status)
            .bind(payment_intent)
            .bind(charge)
            .execute(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_payment(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        order_id: Option<Uuid>,
        subscription_id: Option<Uuid>,
        payment_intent: &str,
        charge: Option<&str>,
        status: &str,
        currency: &str,
        authorized: u64,
        captured: u64,
        provider_created_ms: i64,
        event_id: &str,
    ) -> Result<bool, CommerceServiceError> {
        let result = sqlx::query(
            "INSERT INTO commerce_payments \
             (id,project_id,order_id,subscription_id,status,currency,authorized_minor,captured_minor,\
              provider_payment_intent_id,provider_charge_id,provider_created_at,\
              last_provider_event_created_at,last_provider_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,to_timestamp($11::double precision/1000),\
              to_timestamp($11::double precision/1000),$12) \
             ON CONFLICT (provider_payment_intent_id) DO UPDATE SET status=$5,\
              authorized_minor=$7,captured_minor=$8,provider_charge_id=coalesce($10,\
              commerce_payments.provider_charge_id),last_provider_event_created_at=\
              to_timestamp($11::double precision/1000),last_provider_event_id=$12,updated_at=now() \
             WHERE commerce_payments.project_id=$2 \
              AND (commerce_payments.last_provider_event_created_at < to_timestamp($11::double precision/1000) \
               OR (commerce_payments.last_provider_event_created_at = to_timestamp($11::double precision/1000) \
                AND commerce_payments.last_provider_event_id < $12))",
        )
        .bind(Uuid::now_v7())
        .bind(project_id.0)
        .bind(order_id)
        .bind(subscription_id)
        .bind(status)
        .bind(currency)
        .bind(u64_to_i64(authorized)?)
        .bind(u64_to_i64(captured)?)
        .bind(payment_intent)
        .bind(charge)
        .bind(provider_created_ms)
        .bind(event_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(result.rows_affected() > 0)
    }

    async fn apply_subscription(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        event_id: &str,
        provider_created_ms: i64,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        let provider_subscription_id = provider_id(object, "id", "sub_")?;
        let metadata = object.get("metadata").and_then(Value::as_object);
        let local_id = metadata
            .and_then(|value| value.get("ffdb_subscription_id"))
            .and_then(Value::as_str)
            .map(parse_uuid)
            .transpose()?;
        let row = if let Some(local_id) = local_id {
            sqlx::query(
                "SELECT id,last_provider_event_created_at,last_provider_event_id FROM commerce_subscriptions \
                 WHERE project_id=$1 AND id=$2 FOR UPDATE",
            )
            .bind(project_id.0)
            .bind(local_id)
            .fetch_optional(&mut **transaction)
            .await
        } else {
            sqlx::query(
                "SELECT id,last_provider_event_created_at,last_provider_event_id FROM commerce_subscriptions \
                 WHERE project_id=$1 AND provider_subscription_id=$2 FOR UPDATE",
            )
            .bind(project_id.0)
            .bind(&provider_subscription_id)
            .fetch_optional(&mut **transaction)
            .await
        }
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let Some(row) = row else {
            // Stripe subscriptions without FFDB metadata cannot bind to a
            // local membership subject, so they cannot grant entitlements.
            return Ok(());
        };
        if !provider_event_is_newer(&row, provider_created_ms, event_id)? {
            return Ok(());
        }
        let subscription_id: Uuid = row
            .try_get("id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let status = parse_provider_subscription_status(required_str(object, "status")?)?;
        let period_start_ms = object
            .get("current_period_start")
            .and_then(Value::as_i64)
            .map(seconds_to_ms)
            .transpose()?;
        let period_end_ms = object
            .get("current_period_end")
            .and_then(Value::as_i64)
            .map(seconds_to_ms)
            .transpose()?;
        if period_start_ms
            .zip(period_end_ms)
            .is_some_and(|(start, end)| start >= end)
        {
            return Err(CommerceServiceError::Conflict);
        }
        let cancel_at_period_end = object
            .get("cancel_at_period_end")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        sqlx::query(
            "UPDATE commerce_subscriptions SET provider_subscription_id=$3,status=$4,\
             current_period_start=CASE WHEN $5::bigint IS NULL THEN NULL ELSE \
              to_timestamp($5::double precision/1000) END,current_period_end=CASE WHEN $6::bigint \
              IS NULL THEN NULL ELSE to_timestamp($6::double precision/1000) END,\
             cancel_at_period_end=$7,last_provider_event_created_at=\
              to_timestamp($8::double precision/1000),last_provider_event_id=$9,updated_at=now() \
             WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(subscription_id)
        .bind(provider_subscription_id)
        .bind(status)
        .bind(period_start_ms)
        .bind(period_end_ms)
        .bind(cancel_at_period_end)
        .bind(provider_created_ms)
        .bind(event_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        self.refresh_subscription_entitlements(
            transaction,
            project_id,
            subscription_id,
            status,
            period_start_ms,
            period_end_ms,
        )
        .await
    }

    async fn refresh_subscription_entitlements(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        subscription_id: Uuid,
        status: &str,
        period_start_ms: Option<i64>,
        period_end_ms: Option<i64>,
    ) -> Result<(), CommerceServiceError> {
        if matches!(status, "active" | "trialing") {
            let (start, end) = period_start_ms
                .zip(period_end_ms)
                .ok_or(CommerceServiceError::Conflict)?;
            let row = sqlx::query(
                "SELECT s.subject_kind,s.subject_id,p.entitlements FROM commerce_subscriptions s \
                 JOIN commerce_prices p ON p.project_id=s.project_id AND p.id=s.price_id \
                 WHERE s.project_id=$1 AND s.id=$2",
            )
            .bind(project_id.0)
            .bind(subscription_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            let subject_kind: String = row
                .try_get("subject_kind")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let subject_id: String = row
                .try_get("subject_id")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let values: Value = row
                .try_get("entitlements")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let values = values
                .as_object()
                .ok_or(CommerceServiceError::Unavailable)?;
            for (key, value) in values {
                EntitlementKey::new(key.clone()).map_err(|_| CommerceServiceError::Unavailable)?;
                let wire: CommerceEntitlementValue = serde_json::from_value(value.clone())
                    .map_err(|_| CommerceServiceError::Unavailable)?;
                let stored =
                    serde_json::to_value(wire).map_err(|_| CommerceServiceError::Unavailable)?;
                sqlx::query(
                    "INSERT INTO commerce_entitlements \
                     (id,project_id,subscription_id,subject_kind,subject_id,entitlement_key,\
                      entitlement_value,status,valid_from,valid_until) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,'active',\
                      to_timestamp($8::double precision/1000),to_timestamp($9::double precision/1000)) \
                     ON CONFLICT (project_id,subject_kind,subject_id,entitlement_key) DO UPDATE SET \
                      subscription_id=$3,order_id=NULL,entitlement_value=$7,status='active',\
                      valid_from=to_timestamp($8::double precision/1000),\
                      valid_until=to_timestamp($9::double precision/1000),updated_at=now()",
                )
                .bind(Uuid::now_v7())
                .bind(project_id.0)
                .bind(subscription_id)
                .bind(&subject_kind)
                .bind(&subject_id)
                .bind(key)
                .bind(stored)
                .bind(start)
                .bind(end)
                .execute(&mut **transaction)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
            }
        } else {
            let entitlement_status = if matches!(status, "canceled" | "expired") {
                "expired"
            } else {
                "revoked"
            };
            sqlx::query(
                "UPDATE commerce_entitlements SET status=$3,valid_until=coalesce(valid_until,now()),\
                 updated_at=now() WHERE project_id=$1 AND subscription_id=$2 AND status='active'",
            )
            .bind(project_id.0)
            .bind(subscription_id)
            .bind(entitlement_status)
            .execute(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        }
        Ok(())
    }

    async fn apply_subscription_invoice(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        event_id: &str,
        event_type: &str,
        provider_created_ms: i64,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        provider_id(object, "id", "in_")?;
        let provider_subscription_id = object
            .pointer("/parent/subscription_details/subscription")
            .or_else(|| object.get("subscription"))
            .and_then(Value::as_str)
            .filter(|value| valid_provider_id(value, "sub_"));
        let metadata_id = object
            .pointer("/parent/subscription_details/metadata/ffdb_subscription_id")
            .and_then(Value::as_str)
            .map(parse_uuid)
            .transpose()?;
        let row = if let Some(local_id) = metadata_id {
            sqlx::query(
                "SELECT id FROM commerce_subscriptions WHERE project_id=$1 AND id=$2 \
                 AND ($3::text IS NULL OR provider_subscription_id=$3)",
            )
            .bind(project_id.0)
            .bind(local_id)
            .bind(provider_subscription_id)
            .fetch_optional(&mut **transaction)
            .await
        } else if let Some(provider_subscription_id) = provider_subscription_id {
            sqlx::query(
                "SELECT id FROM commerce_subscriptions WHERE project_id=$1 \
                 AND provider_subscription_id=$2",
            )
            .bind(project_id.0)
            .bind(provider_subscription_id)
            .fetch_optional(&mut **transaction)
            .await
        } else {
            return Ok(());
        }
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let Some(row) = row else {
            return Ok(());
        };
        let subscription_id: Uuid = row
            .try_get("id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let payment_intent = object
            .pointer("/payments/data")
            .and_then(Value::as_array)
            .and_then(|payments| {
                payments.iter().find_map(|payment| {
                    payment
                        .pointer("/payment/payment_intent")
                        .and_then(Value::as_str)
                        .filter(|value| valid_provider_id(value, "pi_"))
                })
            })
            .or_else(|| {
                object
                    .get("payment_intent")
                    .and_then(Value::as_str)
                    .filter(|value| valid_provider_id(value, "pi_"))
            });
        let Some(payment_intent) = payment_intent else {
            // Zero-value and externally settled invoices have no PaymentIntent
            // to record; subscription webhooks remain lifecycle authority.
            return Ok(());
        };
        let currency = required_currency_lower(object, "currency")?;
        let amount = object
            .get("total")
            .or_else(|| object.get("amount_due"))
            .and_then(Value::as_u64)
            .ok_or(CommerceServiceError::InvalidRequest)?;
        let captured = object
            .get("amount_paid")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if captured > amount {
            return Err(CommerceServiceError::Conflict);
        }
        let status = match event_type {
            "invoice.paid" => "captured",
            "invoice.payment_action_required" => "requires_action",
            "invoice.payment_failed" => "failed",
            _ => return Err(CommerceServiceError::InvalidRequest),
        };
        self.upsert_payment(
            transaction,
            project_id,
            None,
            Some(subscription_id),
            payment_intent,
            None,
            status,
            &currency,
            amount,
            captured,
            provider_created_ms,
            event_id,
        )
        .await
        .map(|_| ())
    }

    async fn apply_refund(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
        event_id: &str,
        provider_created_ms: i64,
        object: &Value,
    ) -> Result<(), CommerceServiceError> {
        let provider_refund_id = provider_id(object, "id", "re_")?;
        let local_id = object
            .pointer("/metadata/ffdb_refund_id")
            .and_then(Value::as_str)
            .map(parse_uuid)
            .transpose()?;
        let row = if let Some(local_id) = local_id {
            sqlx::query(
                "SELECT id,payment_id,last_provider_event_created_at,last_provider_event_id \
                 FROM commerce_refunds WHERE project_id=$1 AND id=$2 FOR UPDATE",
            )
            .bind(project_id.0)
            .bind(local_id)
            .fetch_optional(&mut **transaction)
            .await
        } else {
            sqlx::query(
                "SELECT id,payment_id,last_provider_event_created_at,last_provider_event_id \
                 FROM commerce_refunds WHERE project_id=$1 AND provider_refund_id=$2 FOR UPDATE",
            )
            .bind(project_id.0)
            .bind(&provider_refund_id)
            .fetch_optional(&mut **transaction)
            .await
        }
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let Some(row) = row else {
            return Ok(());
        };
        if !provider_event_is_newer(&row, provider_created_ms, event_id)? {
            return Ok(());
        }
        let refund_id: Uuid = row
            .try_get("id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let payment_id: Uuid = row
            .try_get("payment_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let status = match required_str(object, "status")? {
            "pending" | "requires_action" => "pending",
            "succeeded" => "succeeded",
            "failed" => "failed",
            "canceled" => "canceled",
            _ => return Err(CommerceServiceError::InvalidRequest),
        };
        sqlx::query(
            "UPDATE commerce_refunds SET provider_refund_id=$3,status=$4,\
             failure_reason=CASE WHEN $4='failed' THEN coalesce($5,'provider_failed') ELSE NULL END,\
             last_provider_event_created_at=to_timestamp($6::double precision/1000),\
             last_provider_event_id=$7,updated_at=now() WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(refund_id)
        .bind(provider_refund_id)
        .bind(status)
        .bind(object
            .get("failure_reason")
            .and_then(Value::as_str)
            .map(|value| value.chars().take(255).collect::<String>()))
        .bind(provider_created_ms)
        .bind(event_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let succeeded: i64 = sqlx::query_scalar(
            "SELECT coalesce(sum(amount_minor),0)::bigint FROM commerce_refunds \
             WHERE project_id=$1 AND payment_id=$2 AND status='succeeded'",
        )
        .bind(project_id.0)
        .bind(payment_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let payment = sqlx::query(
            "UPDATE commerce_payments SET refunded_minor=$3,status=CASE WHEN $3=0 THEN 'captured' \
              WHEN $3=captured_minor THEN 'refunded' ELSE 'partially_refunded' END,updated_at=now() \
             WHERE project_id=$1 AND id=$2 RETURNING order_id,captured_minor",
        )
        .bind(project_id.0)
        .bind(payment_id)
        .bind(succeeded)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if let Some(order_id) = payment
            .try_get::<Option<Uuid>, _>("order_id")
            .map_err(|_| CommerceServiceError::Unavailable)?
        {
            let totals = sqlx::query(
                "SELECT o.total_minor,coalesce(sum(p.refunded_minor),0)::bigint refunded \
                 FROM commerce_orders o LEFT JOIN commerce_payments p \
                  ON p.project_id=o.project_id AND p.order_id=o.id \
                 WHERE o.project_id=$1 AND o.id=$2 GROUP BY o.total_minor",
            )
            .bind(project_id.0)
            .bind(order_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            let total: i64 = totals
                .try_get("total_minor")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let refunded: i64 = totals
                .try_get("refunded")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let order_status = if refunded == 0 {
                "paid"
            } else if refunded >= total {
                "refunded"
            } else {
                "partially_refunded"
            };
            sqlx::query(
                "UPDATE commerce_orders SET refunded_minor=$3,status=$4,updated_at=now() \
                 WHERE project_id=$1 AND id=$2",
            )
            .bind(project_id.0)
            .bind(order_id)
            .bind(refunded)
            .bind(order_status)
            .execute(&mut **transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        }
        Ok(())
    }
}

impl CommerceService {
    async fn orders(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<CommerceOrderSummary>, CommerceServiceError> {
        let rows = sqlx::query(
            "SELECT id,project_id,customer_id,client_reference,status,fulfillment_status,currency,\
              subtotal_minor,discount_minor,tax_minor,shipping_minor,total_minor,refunded_minor,\
              (extract(epoch FROM paid_at)*1000)::bigint paid_at_ms,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
             FROM commerce_orders WHERE project_id=$1 ORDER BY created_at DESC,id",
        )
        .bind(project_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;

        let line_rows = sqlx::query(
            "SELECT order_id,product_id,price_id,product_name,currency,unit_amount_minor,quantity,line_total_minor \
             FROM commerce_order_lines WHERE project_id=$1 ORDER BY order_id,id",
        )
        .bind(project_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let mut lines_by_order = BTreeMap::<Uuid, Vec<CommerceOrderLineSummary>>::new();
        for row in &line_rows {
            let order_id: Uuid = row
                .try_get("order_id")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            lines_by_order
                .entry(order_id)
                .or_default()
                .push(order_line_from_row(row)?);
        }

        rows.iter()
            .map(|row| {
                let order_id: Uuid = row
                    .try_get("id")
                    .map_err(|_| CommerceServiceError::Unavailable)?;
                order_summary_from_row(
                    row,
                    project_id,
                    CommerceOrderId(order_id),
                    lines_by_order.remove(&order_id).unwrap_or_default(),
                )
            })
            .collect()
    }

    async fn order(
        &self,
        project_id: ProjectId,
        order_id: CommerceOrderId,
    ) -> Result<CommerceOrderSummary, CommerceServiceError> {
        let row = sqlx::query(
            "SELECT id,project_id,customer_id,client_reference,status,fulfillment_status,currency,\
              subtotal_minor,discount_minor,tax_minor,shipping_minor,total_minor,refunded_minor,\
              (extract(epoch FROM paid_at)*1000)::bigint paid_at_ms,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
             FROM commerce_orders WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(order_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let line_rows = sqlx::query(
            "SELECT product_id,price_id,product_name,currency,unit_amount_minor,quantity,line_total_minor \
             FROM commerce_order_lines WHERE project_id=$1 AND order_id=$2 ORDER BY id",
        )
        .bind(project_id.0)
        .bind(order_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let lines = line_rows
            .iter()
            .map(order_line_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        order_summary_from_row(&row, project_id, order_id, lines)
    }

    async fn payments(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<CommercePaymentSummary>, CommerceServiceError> {
        let rows = sqlx::query(
            "SELECT id,project_id,order_id,subscription_id,status,currency,authorized_minor,\
              captured_minor,refunded_minor,\
              (extract(epoch FROM provider_created_at)*1000)::bigint provider_created_at_ms,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms \
             FROM commerce_payments WHERE project_id=$1 ORDER BY created_at DESC,id",
        )
        .bind(project_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        rows.iter().map(payment_summary_from_row).collect()
    }

    async fn subscriptions(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<CommerceSubscriptionSummary>, CommerceServiceError> {
        let rows = sqlx::query(
            "SELECT id,project_id,customer_id,price_id,subject_kind,subject_id,quantity,status,\
              (extract(epoch FROM current_period_start)*1000)::bigint current_period_start_ms,\
              (extract(epoch FROM current_period_end)*1000)::bigint current_period_end_ms,\
              cancel_at_period_end,(extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
             FROM commerce_subscriptions WHERE project_id=$1 ORDER BY created_at DESC,id",
        )
        .bind(project_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        rows.iter().map(subscription_summary_from_row).collect()
    }

    async fn subscription(
        &self,
        project_id: ProjectId,
        subscription_id: CommerceSubscriptionId,
    ) -> Result<CommerceSubscriptionSummary, CommerceServiceError> {
        let row = sqlx::query(
            "SELECT id,project_id,customer_id,price_id,subject_kind,subject_id,quantity,status,\
              (extract(epoch FROM current_period_start)*1000)::bigint current_period_start_ms,\
              (extract(epoch FROM current_period_end)*1000)::bigint current_period_end_ms,\
              cancel_at_period_end,(extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
             FROM commerce_subscriptions WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(subscription_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        subscription_summary_from_row(&row)
    }

    async fn cancel_subscription(
        &self,
        project_id: ProjectId,
        subscription_id: CommerceSubscriptionId,
        at_period_end: bool,
        idempotency_key: &str,
    ) -> Result<CommerceSubscriptionSummary, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        let row = sqlx::query(
            "SELECT provider_subscription_id,status FROM commerce_subscriptions \
             WHERE project_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(project_id.0)
        .bind(subscription_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let provider_subscription_id: Option<String> = row
            .try_get("provider_subscription_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let provider_subscription_id = provider_subscription_id
            .filter(|value| valid_provider_id(value, "sub_"))
            .ok_or(CommerceServiceError::Conflict)?;
        let current_status: String = row
            .try_get("status")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        if matches!(current_status.as_str(), "canceled" | "expired") {
            return self.subscription(project_id, subscription_id).await;
        }
        if at_period_end {
            self.stripe
                .request(
                    &context,
                    Method::POST,
                    &format!("v1/subscriptions/{provider_subscription_id}"),
                    &[("cancel_at_period_end".to_owned(), "true".to_owned())],
                    Some(idempotency_key),
                )
                .await?;
            sqlx::query(
                "UPDATE commerce_subscriptions SET cancel_at_period_end=true,updated_at=now() \
                 WHERE project_id=$1 AND id=$2 AND status NOT IN ('canceled','expired')",
            )
            .bind(project_id.0)
            .bind(subscription_id.0)
            .execute(&self.pool)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        } else {
            self.stripe
                .request(
                    &context,
                    Method::DELETE,
                    &format!("v1/subscriptions/{provider_subscription_id}"),
                    &[],
                    Some(idempotency_key),
                )
                .await?;
            // Stripe's response confirms cancellation; the ordered webhook
            // will reconcile period bounds and entitlement expiry.
            sqlx::query(
                "UPDATE commerce_subscriptions SET status='canceled',cancel_at_period_end=false,\
                 updated_at=now() WHERE project_id=$1 AND id=$2",
            )
            .bind(project_id.0)
            .bind(subscription_id.0)
            .execute(&self.pool)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            sqlx::query(
                "UPDATE commerce_entitlements SET status='revoked',valid_until=coalesce(valid_until,now()),\
                 updated_at=now() WHERE project_id=$1 AND subscription_id=$2 AND status='active'",
            )
            .bind(project_id.0)
            .bind(subscription_id.0)
            .execute(&self.pool)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        }
        self.subscription(project_id, subscription_id).await
    }

    async fn entitlements(
        &self,
        project_id: ProjectId,
        subject: &CommerceMembershipSubject,
        at_ms: i64,
    ) -> Result<Vec<CommerceEntitlementSummary>, CommerceServiceError> {
        domain_subject(subject)?;
        let rows = sqlx::query(
            "SELECT entitlement_key,entitlement_value,subscription_id,order_id,subject_kind,subject_id,\
              (extract(epoch FROM valid_from)*1000)::bigint valid_from_ms,\
              (extract(epoch FROM valid_until)*1000)::bigint valid_until_ms \
             FROM commerce_entitlements WHERE project_id=$1 AND subject_kind=$2 AND subject_id=$3 \
              AND status='active' AND valid_from <= to_timestamp($4::double precision/1000) \
              AND (valid_until IS NULL OR valid_until > to_timestamp($4::double precision/1000)) \
             ORDER BY entitlement_key",
        )
        .bind(project_id.0)
        .bind(subject_kind_name(subject.kind))
        .bind(&subject.id)
        .bind(at_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        rows.iter().map(entitlement_summary_from_row).collect()
    }

    async fn create_refund(
        &self,
        project_id: ProjectId,
        input: &CreateCommerceRefundRequest,
        idempotency_key: &str,
    ) -> Result<CommerceRefundSummary, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        require_capability(&context, MerchantCapability::Refunds)?;
        let refund_id = CommerceRefundId(stable_entity_uuid(project_id, "refund", idempotency_key));
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let payment = sqlx::query(
            "SELECT order_id,subscription_id,currency,captured_minor,provider_payment_intent_id \
             FROM commerce_payments WHERE project_id=$1 AND id=$2 \
               AND status IN ('captured','partially_refunded') FOR UPDATE",
        )
        .bind(project_id.0)
        .bind(input.payment_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let captured = positive_row_u64(&payment, "captured_minor")?;
        let reserved: i64 = sqlx::query_scalar(
            "SELECT coalesce(sum(amount_minor),0)::bigint FROM commerce_refunds \
             WHERE project_id=$1 AND payment_id=$2 AND status IN ('pending','succeeded')",
        )
        .bind(project_id.0)
        .bind(input.payment_id.0)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let reserved = u64::try_from(reserved).map_err(|_| CommerceServiceError::Unavailable)?;
        let available = captured
            .checked_sub(reserved)
            .ok_or(CommerceServiceError::Unavailable)?;
        let amount = input.amount_minor.unwrap_or(available);
        if amount == 0 || amount > available {
            return Err(CommerceServiceError::InvalidRequest);
        }
        let currency: String = payment
            .try_get("currency")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let order_id: Option<Uuid> = payment
            .try_get("order_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let subscription_id: Option<Uuid> = payment
            .try_get("subscription_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let reserved_refund = sqlx::query(
            "INSERT INTO commerce_refunds \
             (id,project_id,order_id,subscription_id,payment_id,status,amount_minor,currency,reason) \
             VALUES ($1,$2,$3,$4,$5,'pending',$6,$7,$8) \
             ON CONFLICT (id) DO UPDATE SET status='pending',failure_reason=NULL,updated_at=now() \
             WHERE commerce_refunds.project_id=EXCLUDED.project_id \
               AND commerce_refunds.payment_id=EXCLUDED.payment_id \
               AND commerce_refunds.order_id IS NOT DISTINCT FROM EXCLUDED.order_id \
               AND commerce_refunds.subscription_id IS NOT DISTINCT FROM EXCLUDED.subscription_id \
               AND commerce_refunds.amount_minor=EXCLUDED.amount_minor \
               AND commerce_refunds.currency=EXCLUDED.currency \
               AND commerce_refunds.reason IS NOT DISTINCT FROM EXCLUDED.reason \
               AND commerce_refunds.status='failed'",
        )
        .bind(refund_id.0)
        .bind(project_id.0)
        .bind(order_id)
        .bind(subscription_id)
        .bind(input.payment_id.0)
        .bind(u64_to_i64(amount)?)
        .bind(&currency)
        .bind(input.reason.map(refund_reason_name))
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if reserved_refund.rows_affected() != 1 {
            return Err(CommerceServiceError::Conflict);
        }
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;

        let payment_intent: String = payment
            .try_get("provider_payment_intent_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let mut form = vec![
            ("payment_intent".to_owned(), payment_intent),
            ("amount".to_owned(), amount.to_string()),
            (
                "metadata[ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            ("metadata[ffdb_refund_id]".to_owned(), refund_id.to_string()),
        ];
        if let Some(reason) = input
            .reason
            .filter(|value| *value != CommerceRefundReason::Other)
        {
            form.push(("reason".to_owned(), refund_reason_name(reason).to_owned()));
        }
        let provider = self
            .stripe
            .request(
                &context,
                Method::POST,
                "v1/refunds",
                &form,
                Some(idempotency_key),
            )
            .await;
        match provider {
            Ok(payload) => {
                let provider_refund_id = provider_id(&payload, "id", "re_")?;
                sqlx::query(
                    "UPDATE commerce_refunds SET provider_refund_id=$3,updated_at=now() \
                     WHERE project_id=$1 AND id=$2",
                )
                .bind(project_id.0)
                .bind(refund_id.0)
                .bind(provider_refund_id)
                .execute(&self.pool)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?;
            }
            Err(error) => {
                let _ = sqlx::query(
                    "UPDATE commerce_refunds SET status='failed',failure_reason='provider_rejected',\
                     updated_at=now() WHERE project_id=$1 AND id=$2 AND status='pending'",
                )
                .bind(project_id.0)
                .bind(refund_id.0)
                .execute(&self.pool)
                .await;
                return Err(error);
            }
        }
        self.refund(project_id, refund_id).await
    }

    async fn refund(
        &self,
        project_id: ProjectId,
        refund_id: CommerceRefundId,
    ) -> Result<CommerceRefundSummary, CommerceServiceError> {
        let row = sqlx::query(
            "SELECT id,payment_id,status,amount_minor,currency,reason,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
             FROM commerce_refunds WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(refund_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        refund_summary_from_row(&row)
    }

    async fn update_fulfillment(
        &self,
        project_id: ProjectId,
        order_id: CommerceOrderId,
        status: CommerceFulfillmentStatus,
        note: Option<&str>,
    ) -> Result<CommerceOrderSummary, CommerceServiceError> {
        if note.is_some_and(|value| value.len() > 2_000)
            || status == CommerceFulfillmentStatus::Unfulfilled
        {
            return Err(CommerceServiceError::InvalidRequest);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let order = sqlx::query(
            "SELECT total_minor,status,fulfillment_status FROM commerce_orders \
             WHERE project_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(project_id.0)
        .bind(order_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let order_status: String = order
            .try_get("status")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let current: String = order
            .try_get("fulfillment_status")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let next = fulfillment_status_name(status);
        let valid_transition = matches!(
            (current.as_str(), next),
            ("unfulfilled", "processing")
                | ("processing", "fulfilled")
                | ("unfulfilled", "canceled")
                | ("processing", "canceled")
        );
        if !valid_transition {
            return Err(CommerceServiceError::Conflict);
        }
        if matches!(
            status,
            CommerceFulfillmentStatus::Processing | CommerceFulfillmentStatus::Fulfilled
        ) {
            if !matches!(order_status.as_str(), "paid" | "partially_refunded") {
                return Err(CommerceServiceError::Conflict);
            }
            let total = positive_row_u64(&order, "total_minor")?;
            let available: i64 = sqlx::query_scalar(
                "SELECT (coalesce(sum(p.captured_minor),0)-coalesce(sum(r.reserved),0))::bigint \
                 FROM commerce_payments p LEFT JOIN (SELECT payment_id,sum(amount_minor) reserved \
                   FROM commerce_refunds WHERE project_id=$1 AND status IN ('pending','succeeded') \
                   GROUP BY payment_id) r ON r.payment_id=p.id \
                 WHERE p.project_id=$1 AND p.order_id=$2 \
                   AND p.status IN ('captured','partially_refunded')",
            )
            .bind(project_id.0)
            .bind(order_id.0)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
            if u64::try_from(available).map_err(|_| CommerceServiceError::Unavailable)? < total {
                return Err(CommerceServiceError::Conflict);
            }
        }
        sqlx::query(
            "UPDATE commerce_orders SET fulfillment_status=$3,updated_at=now() \
             WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(order_id.0)
        .bind(next)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO commerce_fulfillment_events (id,project_id,order_id,state,note) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(project_id.0)
        .bind(order_id.0)
        .bind(next)
        .bind(note)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        self.order(project_id, order_id).await
    }
}

impl CommerceService {
    async fn one_time_checkout(
        &self,
        project_id: ProjectId,
        input: &CreateOneTimeCommerceCheckoutRequest,
        idempotency_key: &str,
    ) -> Result<CommerceCheckoutResponse, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        require_capability(&context, MerchantCapability::OneTimePayments)?;
        validate_checkout_urls(&input.success_url, &input.cancel_url)?;
        if input.lines.is_empty() || input.lines.len() > 100 {
            return Err(CommerceServiceError::InvalidRequest);
        }
        if input
            .client_reference
            .as_ref()
            .is_some_and(|value| value.len() > 255)
        {
            return Err(CommerceServiceError::InvalidRequest);
        }
        if let Some(email) = &input.customer_email {
            validate_email(email)?;
        }
        let merchant = merchant_domain(project_id, &context)?;
        let mut domain_lines = Vec::with_capacity(input.lines.len());
        let mut provider_lines = Vec::with_capacity(input.lines.len());
        for requested in &input.lines {
            let row = sqlx::query(
                "SELECT p.id product_id,p.name,p.description,r.id price_id,r.currency,\
                  r.unit_amount_minor,r.provider_price_id,r.active \
                 FROM commerce_prices r JOIN commerce_products p \
                   ON p.project_id=r.project_id AND p.id=r.product_id \
                 WHERE r.project_id=$1 AND r.id=$2 AND r.active=true AND p.active=true \
                   AND r.billing_type='one_time'",
            )
            .bind(project_id.0)
            .bind(requested.price_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?
            .ok_or(CommerceServiceError::NotFound)?;
            let product_id: Uuid = row
                .try_get("product_id")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let mut product = Product::new(
                ProductId::from_uuid(product_id),
                project_id,
                row.try_get::<String, _>("name")
                    .map_err(|_| CommerceServiceError::Unavailable)?,
                row.try_get("description")
                    .map_err(|_| CommerceServiceError::Unavailable)?,
            )
            .map_err(|_| CommerceServiceError::Unavailable)?;
            product
                .activate()
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let currency: String = row
                .try_get("currency")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            let minor = positive_row_u64(&row, "unit_amount_minor")?;
            let price = Price::new(
                PriceId::from_uuid(requested.price_id.0),
                &product,
                PriceTerms::one_time(
                    Money::positive(
                        Currency::new(currency.to_ascii_uppercase())
                            .map_err(|_| CommerceServiceError::Unavailable)?,
                        minor,
                    )
                    .map_err(|_| CommerceServiceError::Unavailable)?,
                )
                .map_err(|_| CommerceServiceError::Unavailable)?,
            )
            .map_err(|_| CommerceServiceError::Unavailable)?;
            domain_lines.push(
                OrderLineSnapshot::from_price(&product, &price, requested.quantity)
                    .map_err(|_| CommerceServiceError::InvalidRequest)?,
            );
            let provider_price_id: String = row
                .try_get("provider_price_id")
                .map_err(|_| CommerceServiceError::Unavailable)?;
            provider_lines.push((provider_price_id, requested.quantity));
        }
        let order_id = CommerceOrderId(stable_entity_uuid(project_id, "order", idempotency_key));
        let order = Order::new(
            OrderId::from_uuid(order_id.0),
            &merchant,
            domain_lines,
            now_ms(),
        )
        .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let customer_id = self
            .ensure_customer(
                project_id,
                input.subject.as_ref(),
                input.customer_email.as_deref(),
            )
            .await?;
        let provider_customer_id = match customer_id {
            Some(customer_id) => self.provider_customer_id(project_id, customer_id).await?,
            None => None,
        };
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let reserved_order = sqlx::query(
            "INSERT INTO commerce_orders \
             (id,project_id,customer_id,client_reference,status,fulfillment_status,currency,\
              subtotal_minor,total_minor) VALUES ($1,$2,$3,$4,'pending','unfulfilled',$5,$6,$6) \
             ON CONFLICT (id) DO UPDATE SET status='pending',updated_at=now() \
             WHERE commerce_orders.project_id=EXCLUDED.project_id \
               AND commerce_orders.customer_id IS NOT DISTINCT FROM EXCLUDED.customer_id \
               AND commerce_orders.client_reference IS NOT DISTINCT FROM EXCLUDED.client_reference \
               AND commerce_orders.currency=EXCLUDED.currency \
               AND commerce_orders.subtotal_minor=EXCLUDED.subtotal_minor \
               AND commerce_orders.total_minor=EXCLUDED.total_minor \
               AND commerce_orders.status='payment_failed'",
        )
        .bind(order_id.0)
        .bind(project_id.0)
        .bind(customer_id.map(|id| id.0))
        .bind(&input.client_reference)
        .bind(order.total().currency().as_str().to_ascii_lowercase())
        .bind(u64_to_i64(order.total().minor())?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if reserved_order.rows_affected() != 1 {
            return Err(CommerceServiceError::Conflict);
        }
        for (index, line) in order.lines().iter().enumerate() {
            sqlx::query(
                "INSERT INTO commerce_order_lines \
                 (id,project_id,order_id,product_id,price_id,product_name,currency,\
                  unit_amount_minor,quantity,line_total_minor) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(stable_entity_uuid(
                project_id,
                &format!("order-line-{index}"),
                idempotency_key,
            ))
            .bind(project_id.0)
            .bind(order_id.0)
            .bind(line.product_id().as_uuid())
            .bind(line.price_id().as_uuid())
            .bind(line.product_name())
            .bind(line.total().currency().as_str().to_ascii_lowercase())
            .bind(u64_to_i64(line.unit_amount().minor())?)
            .bind(i32::try_from(line.quantity()).map_err(|_| CommerceServiceError::InvalidRequest)?)
            .bind(u64_to_i64(line.total().minor())?)
            .execute(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;

        let mut form = vec![
            ("mode".to_owned(), "payment".to_owned()),
            ("success_url".to_owned(), input.success_url.clone()),
            ("cancel_url".to_owned(), input.cancel_url.clone()),
            (
                "metadata[ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            ("metadata[ffdb_order_id]".to_owned(), order_id.to_string()),
            (
                "payment_intent_data[metadata][ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            (
                "payment_intent_data[metadata][ffdb_order_id]".to_owned(),
                order_id.to_string(),
            ),
        ];
        if let Some(provider_customer_id) = provider_customer_id {
            form.push(("customer".to_owned(), provider_customer_id));
        } else if let Some(email) = &input.customer_email {
            form.push(("customer_email".to_owned(), email.trim().to_owned()));
        }
        if customer_id.is_some() && !form.iter().any(|(key, _)| key == "customer") {
            form.push(("customer_creation".to_owned(), "always".to_owned()));
        }
        if let Some(reference) = &input.client_reference {
            form.push(("client_reference_id".to_owned(), reference.clone()));
        }
        for (index, (price, quantity)) in provider_lines.iter().enumerate() {
            form.push((format!("line_items[{index}][price]"), price.clone()));
            form.push((
                format!("line_items[{index}][quantity]"),
                quantity.to_string(),
            ));
        }
        let checkout = self
            .stripe
            .request(
                &context,
                Method::POST,
                "v1/checkout/sessions",
                &form,
                Some(idempotency_key),
            )
            .await;
        let checkout = match checkout {
            Ok(value) => value,
            Err(error) => {
                let _ = sqlx::query(
                    "UPDATE commerce_orders SET status='payment_failed',updated_at=now() \
                     WHERE project_id=$1 AND id=$2 AND status='pending'",
                )
                .bind(project_id.0)
                .bind(order_id.0)
                .execute(&self.pool)
                .await;
                return Err(error);
            }
        };
        let session_id = provider_id(&checkout, "id", "cs_")?;
        let checkout_url = verified_https_url(&checkout, "url")?;
        let expires_at_ms = seconds_to_ms(required_i64(&checkout, "expires_at")?)?;
        sqlx::query(
            "UPDATE commerce_orders SET status='checkout_created',provider_checkout_session_id=$3,\
             checkout_expires_at=to_timestamp($4::double precision/1000),updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND status IN ('pending','payment_failed')",
        )
        .bind(project_id.0)
        .bind(order_id.0)
        .bind(session_id)
        .bind(expires_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(CommerceCheckoutResponse {
            url: checkout_url,
            expires_at_ms,
            order_id: Some(order_id),
            subscription_id: None,
        })
    }

    async fn recurring_checkout(
        &self,
        project_id: ProjectId,
        input: &CreateRecurringCommerceCheckoutRequest,
        idempotency_key: &str,
    ) -> Result<CommerceCheckoutResponse, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        require_capability(&context, MerchantCapability::RecurringPayments)?;
        validate_checkout_urls(&input.success_url, &input.cancel_url)?;
        if let Some(email) = &input.customer_email {
            validate_email(email)?;
        }
        let subject = domain_subject(&input.subject)?;
        let row = sqlx::query(
            "SELECT r.product_id,r.currency,r.unit_amount_minor,r.provider_price_id,\
              r.recurring_interval,r.recurring_interval_count,r.entitlements,p.name,p.description \
             FROM commerce_prices r JOIN commerce_products p \
               ON p.project_id=r.project_id AND p.id=r.product_id \
             WHERE r.project_id=$1 AND r.id=$2 AND r.active=true AND p.active=true \
               AND r.billing_type='recurring'",
        )
        .bind(project_id.0)
        .bind(input.price_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let product_id: Uuid = row
            .try_get("product_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let mut product = Product::new(
            ProductId::from_uuid(product_id),
            project_id,
            row.try_get::<String, _>("name")
                .map_err(|_| CommerceServiceError::Unavailable)?,
            row.try_get("description")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )
        .map_err(|_| CommerceServiceError::Unavailable)?;
        product
            .activate()
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let interval_name: String = row
            .try_get("recurring_interval")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let interval_count: i32 = row
            .try_get("recurring_interval_count")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let entitlements_json: Value = row
            .try_get("entitlements")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let wire_entitlements: BTreeMap<String, CommerceEntitlementValue> =
            serde_json::from_value(entitlements_json)
                .map_err(|_| CommerceServiceError::Unavailable)?;
        let price = Price::new(
            PriceId::from_uuid(input.price_id.0),
            &product,
            PriceTerms::recurring(
                Money::positive(
                    Currency::new(
                        row.try_get::<String, _>("currency")
                            .map_err(|_| CommerceServiceError::Unavailable)?
                            .to_ascii_uppercase(),
                    )
                    .map_err(|_| CommerceServiceError::Unavailable)?,
                    positive_row_u64(&row, "unit_amount_minor")?,
                )
                .map_err(|_| CommerceServiceError::Unavailable)?,
                BillingInterval::new(
                    parse_domain_interval(&interval_name)?,
                    u16::try_from(interval_count).map_err(|_| CommerceServiceError::Unavailable)?,
                )
                .map_err(|_| CommerceServiceError::Unavailable)?,
                domain_entitlements(&wire_entitlements)?,
            )
            .map_err(|_| CommerceServiceError::Unavailable)?,
        )
        .map_err(|_| CommerceServiceError::Unavailable)?;
        let merchant = merchant_domain(project_id, &context)?;
        let subscription_id = CommerceSubscriptionId(stable_entity_uuid(
            project_id,
            "subscription",
            idempotency_key,
        ));
        Subscription::new(
            SubscriptionId::from_uuid(subscription_id.0),
            &merchant,
            &price,
            subject,
            input.quantity,
            now_ms(),
        )
        .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let customer_id = self
            .ensure_customer(
                project_id,
                Some(&input.subject),
                input.customer_email.as_deref(),
            )
            .await?
            .ok_or(CommerceServiceError::InvalidRequest)?;
        let provider_customer_id = self.provider_customer_id(project_id, customer_id).await?;
        let reserved_subscription = sqlx::query(
            "INSERT INTO commerce_subscriptions \
             (id,project_id,customer_id,price_id,subject_kind,subject_id,quantity,status) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,'checkout_pending') \
             ON CONFLICT (id) DO UPDATE SET updated_at=now() \
             WHERE commerce_subscriptions.project_id=EXCLUDED.project_id \
               AND commerce_subscriptions.customer_id=EXCLUDED.customer_id \
               AND commerce_subscriptions.price_id=EXCLUDED.price_id \
               AND commerce_subscriptions.subject_kind=EXCLUDED.subject_kind \
               AND commerce_subscriptions.subject_id=EXCLUDED.subject_id \
               AND commerce_subscriptions.quantity=EXCLUDED.quantity \
               AND commerce_subscriptions.status='checkout_pending'",
        )
        .bind(subscription_id.0)
        .bind(project_id.0)
        .bind(customer_id.0)
        .bind(input.price_id.0)
        .bind(subject_kind_name(input.subject.kind))
        .bind(&input.subject.id)
        .bind(i32::try_from(input.quantity).map_err(|_| CommerceServiceError::InvalidRequest)?)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if reserved_subscription.rows_affected() != 1 {
            return Err(CommerceServiceError::Conflict);
        }
        let provider_price_id: String = row
            .try_get("provider_price_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let mut form = vec![
            ("mode".to_owned(), "subscription".to_owned()),
            ("success_url".to_owned(), input.success_url.clone()),
            ("cancel_url".to_owned(), input.cancel_url.clone()),
            ("line_items[0][price]".to_owned(), provider_price_id),
            (
                "line_items[0][quantity]".to_owned(),
                input.quantity.to_string(),
            ),
            (
                "metadata[ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            (
                "metadata[ffdb_subscription_id]".to_owned(),
                subscription_id.to_string(),
            ),
            (
                "subscription_data[metadata][ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            (
                "subscription_data[metadata][ffdb_subscription_id]".to_owned(),
                subscription_id.to_string(),
            ),
        ];
        if let Some(provider_customer_id) = provider_customer_id {
            form.push(("customer".to_owned(), provider_customer_id));
        } else if let Some(email) = &input.customer_email {
            form.push(("customer_email".to_owned(), email.trim().to_owned()));
        }
        let checkout = self
            .stripe
            .request(
                &context,
                Method::POST,
                "v1/checkout/sessions",
                &form,
                Some(idempotency_key),
            )
            .await?;
        let session_id = provider_id(&checkout, "id", "cs_")?;
        let checkout_url = verified_https_url(&checkout, "url")?;
        let expires_at_ms = seconds_to_ms(required_i64(&checkout, "expires_at")?)?;
        sqlx::query(
            "UPDATE commerce_subscriptions SET provider_checkout_session_id=$3,updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND status='checkout_pending'",
        )
        .bind(project_id.0)
        .bind(subscription_id.0)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(CommerceCheckoutResponse {
            url: checkout_url,
            expires_at_ms,
            order_id: None,
            subscription_id: Some(subscription_id),
        })
    }

    async fn customer_portal(
        &self,
        project_id: ProjectId,
        input: &CreateCommerceCustomerPortalRequest,
        idempotency_key: &str,
    ) -> Result<BillingRedirect, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        if !context.capabilities.customer_portal {
            return Err(CommerceServiceError::CapabilityUnavailable);
        }
        domain_subject(&input.subject)?;
        let return_url = validate_return_url(&input.return_url)?;
        let provider_customer_id: Option<String> = sqlx::query_scalar(
            "SELECT provider_customer_id FROM commerce_customers \
             WHERE project_id=$1 AND subject_kind=$2 AND subject_id=$3",
        )
        .bind(project_id.0)
        .bind(subject_kind_name(input.subject.kind))
        .bind(&input.subject.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .flatten();
        let provider_customer_id = provider_customer_id
            .filter(|value| valid_provider_id(value, "cus_"))
            .ok_or(CommerceServiceError::Conflict)?;
        let payload = self
            .stripe
            .request(
                &context,
                Method::POST,
                "v1/billing_portal/sessions",
                &[
                    ("customer".to_owned(), provider_customer_id),
                    ("return_url".to_owned(), return_url.to_string()),
                ],
                Some(idempotency_key),
            )
            .await?;
        Ok(BillingRedirect {
            url: verified_https_url(&payload, "url")?,
        })
    }

    async fn ensure_customer(
        &self,
        project_id: ProjectId,
        subject: Option<&CommerceMembershipSubject>,
        email: Option<&str>,
    ) -> Result<Option<CommerceCustomerId>, CommerceServiceError> {
        let (kind, subject_id) = if let Some(subject) = subject {
            domain_subject(subject)?;
            (subject_kind_name(subject.kind), subject.id.clone())
        } else if let Some(email) = email {
            validate_email(email)?;
            let digest = Sha256::digest(email.trim().to_ascii_lowercase().as_bytes());
            ("guest", format!("email:{}", hex::encode(digest)))
        } else {
            return Ok(None);
        };
        let id = CommerceCustomerId::new();
        let stored: Uuid = sqlx::query_scalar(
            "INSERT INTO commerce_customers (id,project_id,subject_kind,subject_id) \
             VALUES ($1,$2,$3,$4) ON CONFLICT (project_id,subject_kind,subject_id) \
             DO UPDATE SET updated_at=now() RETURNING id",
        )
        .bind(id.0)
        .bind(project_id.0)
        .bind(kind)
        .bind(subject_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(Some(CommerceCustomerId(stored)))
    }

    async fn provider_customer_id(
        &self,
        project_id: ProjectId,
        customer_id: CommerceCustomerId,
    ) -> Result<Option<String>, CommerceServiceError> {
        let value: Option<String> = sqlx::query_scalar(
            "SELECT provider_customer_id FROM commerce_customers \
             WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(customer_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .flatten();
        match value {
            Some(value) if valid_provider_id(&value, "cus_") => Ok(Some(value)),
            Some(_) => Err(CommerceServiceError::Unavailable),
            None => Ok(None),
        }
    }
}

impl CommerceService {
    async fn create_product(
        &self,
        project_id: ProjectId,
        input: &CreateCommerceProductRequest,
        idempotency_key: &str,
    ) -> Result<CommerceProductSummary, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        require_capability(&context, MerchantCapability::OneTimePayments)?;
        if input.metadata.len() > 50 {
            return Err(CommerceServiceError::InvalidRequest);
        }
        let product_id =
            CommerceProductId(stable_entity_uuid(project_id, "product", idempotency_key));
        let mut domain = Product::new(
            ProductId::from_uuid(product_id.0),
            project_id,
            input.name.clone(),
            input.description.clone(),
        )
        .map_err(|_| CommerceServiceError::InvalidRequest)?;
        domain
            .activate()
            .map_err(|_| CommerceServiceError::InvalidRequest)?;
        if input
            .tax_code
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 64)
        {
            return Err(CommerceServiceError::InvalidRequest);
        }
        let mut form = vec![
            ("name".to_owned(), input.name.clone()),
            ("active".to_owned(), "true".to_owned()),
            (
                "metadata[ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            (
                "metadata[ffdb_product_id]".to_owned(),
                product_id.to_string(),
            ),
        ];
        if let Some(description) = &input.description {
            form.push(("description".to_owned(), description.clone()));
        }
        if let Some(tax_code) = &input.tax_code {
            form.push(("tax_code".to_owned(), tax_code.clone()));
        }
        for (key, value) in &input.metadata {
            validate_product_metadata_key(key)?;
            let value = metadata_scalar(value)?;
            form.push((format!("metadata[{key}]"), value));
        }
        let payload = self
            .stripe
            .request(
                &context,
                Method::POST,
                "v1/products",
                &form,
                Some(idempotency_key),
            )
            .await?;
        let provider_product_id = provider_id(&payload, "id", "prod_")?;
        let row = sqlx::query(
            "INSERT INTO commerce_products \
             (id,project_id,name,description,tax_code,active,metadata) \
             VALUES ($1,$2,$3,$4,$5,true,$6) \
             ON CONFLICT (id) DO UPDATE SET updated_at=commerce_products.updated_at \
             RETURNING id,project_id,name,description,tax_code,active,metadata,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms",
        )
        .bind(product_id.0)
        .bind(project_id.0)
        .bind(&input.name)
        .bind(&input.description)
        .bind(&input.tax_code)
        .bind(Value::Object(input.metadata.clone()))
        .fetch_one(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        // Provider identifiers are not needed to create prices because Stripe
        // Price accepts provider product id. Retain it in protected metadata,
        // never caller-controlled metadata.
        sqlx::query(
            "UPDATE commerce_products SET metadata=jsonb_set(metadata,'{ffdb_provider_product_id}',\
             to_jsonb($3::text),true) WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id.0)
        .bind(product_id.0)
        .bind(provider_product_id)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        product_summary_from_row(&row)
    }

    async fn products(
        &self,
        project_id: ProjectId,
        include_archived: bool,
    ) -> Result<Vec<CommerceProductSummary>, CommerceServiceError> {
        let rows = sqlx::query(
            "SELECT id,project_id,name,description,tax_code,active,metadata,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms,\
              (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
             FROM commerce_products WHERE project_id=$1 AND ($2 OR active=true) \
             ORDER BY created_at,id",
        )
        .bind(project_id.0)
        .bind(include_archived)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        rows.iter().map(product_summary_from_row).collect()
    }

    async fn archive_product(
        &self,
        project_id: ProjectId,
        product_id: CommerceProductId,
        idempotency_key: &str,
    ) -> Result<(), CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        let metadata: Value = sqlx::query_scalar(
            "SELECT metadata FROM commerce_products WHERE project_id=$1 AND id=$2 AND active=true",
        )
        .bind(project_id.0)
        .bind(product_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let provider_product_id = metadata
            .get("ffdb_provider_product_id")
            .and_then(Value::as_str)
            .filter(|value| valid_provider_id(value, "prod_"))
            .ok_or(CommerceServiceError::Unavailable)?;
        self.stripe
            .request(
                &context,
                Method::POST,
                &format!("v1/products/{provider_product_id}"),
                &[("active".to_owned(), "false".to_owned())],
                Some(idempotency_key),
            )
            .await?;
        let result = sqlx::query(
            "UPDATE commerce_products SET active=false,updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND active=true",
        )
        .bind(project_id.0)
        .bind(product_id.0)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(CommerceServiceError::Conflict)
        }
    }

    async fn create_price(
        &self,
        project_id: ProjectId,
        input: &CreateCommercePriceRequest,
        idempotency_key: &str,
    ) -> Result<CommercePriceSummary, CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        let required = match input.billing {
            CommercePriceBilling::OneTime => MerchantCapability::OneTimePayments,
            CommercePriceBilling::Recurring { .. } => MerchantCapability::RecurringPayments,
        };
        require_capability(&context, required)?;
        let product_row = sqlx::query(
            "SELECT id,name,description,metadata FROM commerce_products \
             WHERE project_id=$1 AND id=$2 AND active=true",
        )
        .bind(project_id.0)
        .bind(input.product_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        let mut product = Product::new(
            ProductId::from_uuid(input.product_id.0),
            project_id,
            product_row
                .try_get::<String, _>("name")
                .map_err(|_| CommerceServiceError::Unavailable)?,
            product_row
                .try_get("description")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )
        .map_err(|_| CommerceServiceError::Unavailable)?;
        product
            .activate()
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let currency = Currency::new(input.currency.to_ascii_uppercase())
            .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let amount = Money::positive(currency, input.unit_amount_minor)
            .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let entitlements = domain_entitlements(&input.entitlements)?;
        let terms = match input.billing {
            CommercePriceBilling::OneTime => {
                if !entitlements.is_empty() {
                    return Err(CommerceServiceError::InvalidRequest);
                }
                PriceTerms::one_time(amount)
            }
            CommercePriceBilling::Recurring {
                interval,
                interval_count,
            } => PriceTerms::recurring(
                amount,
                BillingInterval::new(domain_interval_unit(interval), interval_count)
                    .map_err(|_| CommerceServiceError::InvalidRequest)?,
                entitlements,
            ),
        }
        .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let price_id = CommercePriceId(stable_entity_uuid(project_id, "price", idempotency_key));
        Price::new(PriceId::from_uuid(price_id.0), &product, terms)
            .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let metadata: Value = product_row
            .try_get("metadata")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let provider_product_id = metadata
            .get("ffdb_provider_product_id")
            .and_then(Value::as_str)
            .filter(|value| valid_provider_id(value, "prod_"))
            .ok_or(CommerceServiceError::Unavailable)?;
        let currency_lower = input.currency.to_ascii_lowercase();
        let mut form = vec![
            ("product".to_owned(), provider_product_id.to_owned()),
            ("currency".to_owned(), currency_lower.clone()),
            (
                "unit_amount".to_owned(),
                input.unit_amount_minor.to_string(),
            ),
            (
                "metadata[ffdb_project_id]".to_owned(),
                project_id.to_string(),
            ),
            ("metadata[ffdb_price_id]".to_owned(), price_id.to_string()),
        ];
        if let Some(lookup_key) = &input.lookup_key {
            validate_lookup_key(lookup_key)?;
            form.push(("lookup_key".to_owned(), lookup_key.clone()));
        }
        if let CommercePriceBilling::Recurring {
            interval,
            interval_count,
        } = input.billing
        {
            form.push((
                "recurring[interval]".to_owned(),
                interval_name(interval).to_owned(),
            ));
            form.push((
                "recurring[interval_count]".to_owned(),
                interval_count.to_string(),
            ));
        }
        let payload = self
            .stripe
            .request(
                &context,
                Method::POST,
                "v1/prices",
                &form,
                Some(idempotency_key),
            )
            .await?;
        let provider_price_id = provider_id(&payload, "id", "price_")?;
        let (billing_type, interval, interval_count) = price_db_values(&input.billing);
        let entitlements_json = serde_json::to_value(&input.entitlements)
            .map_err(|_| CommerceServiceError::InvalidRequest)?;
        let row = sqlx::query(
            "INSERT INTO commerce_prices \
             (id,project_id,product_id,lookup_key,currency,unit_amount_minor,billing_type,\
              recurring_interval,recurring_interval_count,provider_price_id,entitlements,active) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,true) \
             RETURNING id,project_id,product_id,lookup_key,currency,unit_amount_minor,\
              billing_type,recurring_interval,recurring_interval_count,entitlements,active,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms",
        )
        .bind(price_id.0)
        .bind(project_id.0)
        .bind(input.product_id.0)
        .bind(&input.lookup_key)
        .bind(currency_lower)
        .bind(u64_to_i64(input.unit_amount_minor)?)
        .bind(billing_type)
        .bind(interval)
        .bind(interval_count)
        .bind(provider_price_id)
        .bind(entitlements_json)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        price_summary_from_row(&row)
    }

    async fn prices(
        &self,
        project_id: ProjectId,
        include_inactive: bool,
    ) -> Result<Vec<CommercePriceSummary>, CommerceServiceError> {
        let rows = sqlx::query(
            "SELECT id,project_id,product_id,lookup_key,currency,unit_amount_minor,billing_type,\
              recurring_interval,recurring_interval_count,entitlements,active,\
              (extract(epoch FROM created_at)*1000)::bigint created_at_ms \
             FROM commerce_prices WHERE project_id=$1 AND ($2 OR active=true) ORDER BY created_at,id",
        )
        .bind(project_id.0)
        .bind(include_inactive)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        rows.iter().map(price_summary_from_row).collect()
    }

    async fn retire_price(
        &self,
        project_id: ProjectId,
        price_id: CommercePriceId,
        idempotency_key: &str,
    ) -> Result<(), CommerceServiceError> {
        let context = self.provider_context(project_id, true).await?;
        let provider_price_id: String = sqlx::query_scalar(
            "SELECT provider_price_id FROM commerce_prices WHERE project_id=$1 AND id=$2 AND active=true",
        )
        .bind(project_id.0)
        .bind(price_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::NotFound)?;
        self.stripe
            .request(
                &context,
                Method::POST,
                &format!("v1/prices/{provider_price_id}"),
                &[("active".to_owned(), "false".to_owned())],
                Some(idempotency_key),
            )
            .await?;
        let result = sqlx::query(
            "UPDATE commerce_prices SET active=false,updated_at=now() \
             WHERE project_id=$1 AND id=$2 AND active=true",
        )
        .bind(project_id.0)
        .bind(price_id.0)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(CommerceServiceError::Conflict)
        }
    }
}

/// Namespace bound to an encrypted payment-provider secret.
///
/// Keeping the scope in AEAD associated data prevents a ciphertext copied
/// between project commerce and instance billing from decrypting successfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderSecretScope {
    ProjectCommerce(ProjectId),
    PlatformInstanceBilling(Uuid),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SealedProviderSecret {
    pub key_version: i32,
    pub nonce: [u8; NONCE_BYTES],
    /// AES-GCM ciphertext followed by its authentication tag.
    pub ciphertext: Vec<u8>,
}

impl SealedProviderSecret {
    pub(crate) fn to_packed(&self) -> Vec<u8> {
        let mut packed = Vec::with_capacity(NONCE_BYTES + self.ciphertext.len());
        packed.extend_from_slice(&self.nonce);
        packed.extend_from_slice(&self.ciphertext);
        packed
    }

    pub(crate) fn from_packed(
        key_version: i32,
        packed: &[u8],
    ) -> Result<Self, CommerceServiceError> {
        if key_version <= 0 || packed.len() <= NONCE_BYTES + TAG_BYTES {
            return Err(CommerceServiceError::Encryption);
        }
        let nonce = packed[..NONCE_BYTES]
            .try_into()
            .map_err(|_| CommerceServiceError::Encryption)?;
        Ok(Self {
            key_version,
            nonce,
            ciphertext: packed[NONCE_BYTES..].to_vec(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct ProviderSecretEnvelope {
    key: Arc<Zeroizing<[u8; 32]>>,
    pub(crate) key_version: i32,
}

impl std::fmt::Debug for ProviderSecretEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderSecretEnvelope")
            .field("key", &"[REDACTED]")
            .field("key_version", &self.key_version)
            .finish()
    }
}

impl ProviderSecretEnvelope {
    pub(crate) fn new(key: Vec<u8>, key_version: i32) -> Result<Self, CommerceServiceError> {
        let key = Zeroizing::new(key);
        let key: [u8; 32] = key
            .as_slice()
            .try_into()
            .map_err(|_| CommerceServiceError::InvalidConfiguration)?;
        if key_version <= 0 {
            return Err(CommerceServiceError::InvalidConfiguration);
        }
        Ok(Self {
            key: Arc::new(Zeroizing::new(key)),
            key_version,
        })
    }

    fn aead(&self) -> Result<LessSafeKey, CommerceServiceError> {
        UnboundKey::new(&AES_256_GCM, self.key.as_ref().as_ref())
            .map(LessSafeKey::new)
            .map_err(|_| CommerceServiceError::Encryption)
    }

    pub(crate) fn seal(
        &self,
        scope: ProviderSecretScope,
        field: &'static str,
        plaintext: &str,
    ) -> Result<SealedProviderSecret, CommerceServiceError> {
        let mut nonce = [0_u8; NONCE_BYTES];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| CommerceServiceError::Encryption)?;
        let mut sealed = Zeroizing::new(plaintext.as_bytes().to_vec());
        self.aead()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(secret_aad(scope, field, self.key_version).as_slice()),
                sealed.deref_mut(),
            )
            .map_err(|_| CommerceServiceError::Encryption)?;
        Ok(SealedProviderSecret {
            key_version: self.key_version,
            nonce,
            ciphertext: sealed.to_vec(),
        })
    }

    pub(crate) fn open(
        &self,
        scope: ProviderSecretScope,
        field: &'static str,
        sealed_secret: &SealedProviderSecret,
    ) -> Result<ProtectedString, CommerceServiceError> {
        if sealed_secret.key_version != self.key_version
            || sealed_secret.ciphertext.len() <= TAG_BYTES
        {
            return Err(CommerceServiceError::Encryption);
        }
        let mut sealed = Zeroizing::new(sealed_secret.ciphertext.clone());
        let plaintext = self
            .aead()?
            .open_in_place(
                Nonce::assume_unique_for_key(sealed_secret.nonce),
                Aad::from(secret_aad(scope, field, sealed_secret.key_version).as_slice()),
                sealed.as_mut_slice(),
            )
            .map_err(|_| CommerceServiceError::Encryption)?;
        let value = std::str::from_utf8(plaintext)
            .map_err(|_| CommerceServiceError::Encryption)?
            .to_owned();
        Ok(ProtectedString::from(value))
    }
}

fn secret_aad(scope: ProviderSecretScope, field: &str, key_version: i32) -> Vec<u8> {
    let (namespace, id) = match scope {
        ProviderSecretScope::ProjectCommerce(project_id) => ("project-commerce", project_id.0),
        ProviderSecretScope::PlatformInstanceBilling(instance_id) => {
            ("platform-instance-billing", instance_id)
        }
    };
    format!("ffdb.provider-secret.v1|{key_version}|{namespace}|{id}|{field}").into_bytes()
}

#[derive(Clone, Debug)]
pub(crate) struct StripeRequestClient {
    client: reqwest::Client,
    api_base: Url,
}

#[derive(Clone)]
pub(crate) struct StripeRequestAuth {
    pub secret_key: ProtectedString,
    pub connected_account: Option<String>,
}

impl std::fmt::Debug for StripeRequestAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StripeRequestAuth")
            .field("secret_key", &"[REDACTED]")
            .field("connected_account", &self.connected_account)
            .finish()
    }
}

enum StripeRequestBody<'a> {
    Form(&'a [(String, String)]),
    Json(&'a Value),
}

impl StripeRequestClient {
    pub(crate) fn production() -> Result<Self, CommerceServiceError> {
        Self::new(
            Url::parse("https://api.stripe.com/")
                .map_err(|_| CommerceServiceError::InvalidConfiguration)?,
        )
    }

    pub(crate) fn new(api_base: Url) -> Result<Self, CommerceServiceError> {
        if api_base.host_str().is_none() || !matches!(api_base.scheme(), "http" | "https") {
            return Err(CommerceServiceError::InvalidConfiguration);
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|_| CommerceServiceError::InvalidConfiguration)?,
            api_base,
        })
    }

    pub(crate) async fn form(
        &self,
        auth: &StripeRequestAuth,
        method: Method,
        path: &str,
        form: &[(String, String)],
        idempotency_key: Option<&str>,
    ) -> Result<Value, CommerceServiceError> {
        self.execute(
            auth,
            method,
            path,
            StripeRequestBody::Form(form),
            idempotency_key,
        )
        .await
    }

    pub(crate) async fn json(
        &self,
        auth: &StripeRequestAuth,
        method: Method,
        path: &str,
        payload: &Value,
        idempotency_key: Option<&str>,
    ) -> Result<Value, CommerceServiceError> {
        self.execute(
            auth,
            method,
            path,
            StripeRequestBody::Json(payload),
            idempotency_key,
        )
        .await
    }

    async fn request(
        &self,
        context: &ProviderContext,
        method: Method,
        path: &str,
        form: &[(String, String)],
        idempotency_key: Option<&str>,
    ) -> Result<Value, CommerceServiceError> {
        self.form(
            &StripeRequestAuth {
                secret_key: context.secret_key.clone(),
                connected_account: context.connected_account.clone(),
            },
            method,
            path,
            form,
            idempotency_key,
        )
        .await
    }

    async fn execute(
        &self,
        auth: &StripeRequestAuth,
        method: Method,
        path: &str,
        body: StripeRequestBody<'_>,
        idempotency_key: Option<&str>,
    ) -> Result<Value, CommerceServiceError> {
        let is_get = method == Method::GET;
        let endpoint = self
            .api_base
            .join(path)
            .map_err(|_| CommerceServiceError::InvalidConfiguration)?;
        let mut request = self
            .client
            .request(method, endpoint)
            .header(
                AUTHORIZATION,
                format!("Bearer {}", auth.secret_key.expose_secret()),
            )
            .header("Stripe-Version", STRIPE_API_VERSION);
        match body {
            StripeRequestBody::Form(form) if !form.is_empty() => {
                request = if is_get {
                    request.query(form)
                } else {
                    request
                        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .form(form)
                };
            }
            StripeRequestBody::Form(_) => {}
            StripeRequestBody::Json(payload) => {
                request = request
                    .header(CONTENT_TYPE, "application/json")
                    .json(payload);
            }
        }
        if let Some(account_id) = auth.connected_account.as_deref() {
            request = request.header("Stripe-Account", account_id);
        }
        if let Some(key) = idempotency_key {
            validate_idempotency_key(key)?;
            request = request.header("Idempotency-Key", key);
        }
        let response = request
            .send()
            .await
            .map_err(|_| CommerceServiceError::ProviderUnavailable)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| CommerceServiceError::ProviderUnavailable)?;
        if bytes.len() > MAX_PROVIDER_BODY_BYTES {
            return Err(CommerceServiceError::ProviderResponseInvalid);
        }
        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| CommerceServiceError::ProviderResponseInvalid)?;
        if status.is_success() {
            Ok(payload)
        } else if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            Err(CommerceServiceError::ProviderUnavailable)
        } else {
            Err(CommerceServiceError::ProviderRejected)
        }
    }
}

#[derive(Clone)]
struct ProviderContext {
    mode: CommerceProviderMode,
    secret_key: ProtectedString,
    webhook_secret: ProtectedString,
    connected_account: Option<String>,
    provider_account_id: Option<String>,
    livemode: bool,
    status: CommerceAccountStatus,
    capabilities: CommerceAccountCapabilities,
}

impl std::fmt::Debug for ProviderContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderContext")
            .field("mode", &self.mode)
            .field("secret_key", &"[REDACTED]")
            .field("webhook_secret", &"[REDACTED]")
            .field("connected_account", &self.connected_account)
            .field("provider_account_id", &self.provider_account_id)
            .field("livemode", &self.livemode)
            .field("status", &self.status)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[derive(Clone, Debug)]
struct ProviderAccountState {
    id: String,
    status: CommerceAccountStatus,
    livemode: bool,
    capabilities: CommerceAccountCapabilities,
    requirements_due: Vec<String>,
    disabled_reason: Option<String>,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum CommerceServiceError {
    #[error("commerce configuration is invalid")]
    InvalidConfiguration,
    #[error("commerce request is invalid")]
    InvalidRequest,
    #[error("commerce resource was not found")]
    NotFound,
    #[error("commerce operation is forbidden")]
    Forbidden,
    #[error("commerce account is not configured")]
    AccountNotConfigured,
    #[error("commerce account has provider-bound state and cannot be disconnected")]
    AccountInUse,
    #[error("commerce account is not enabled")]
    AccountRestricted,
    #[error("commerce capability is unavailable")]
    CapabilityUnavailable,
    #[error("commerce conflict")]
    Conflict,
    #[error("commerce secret encryption failed")]
    Encryption,
    #[error("commerce provider is unavailable")]
    ProviderUnavailable,
    #[error("commerce provider rejected the request")]
    ProviderRejected,
    #[error("commerce provider response is invalid")]
    ProviderResponseInvalid,
    #[error("commerce webhook signature is invalid")]
    InvalidSignature,
    #[error("commerce webhook payload conflicts with an accepted event")]
    WebhookHashConflict,
    #[error("commerce datastore is unavailable")]
    Unavailable,
}

impl CommerceService {
    async fn disconnect_account(&self, project_id: ProjectId) -> Result<(), CommerceServiceError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let exists = sqlx::query_scalar::<_, String>(
            "SELECT mode FROM project_commerce_accounts \
             WHERE project_id=$1 AND provider='stripe' FOR UPDATE",
        )
        .bind(project_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .is_some();
        if !exists {
            return Err(CommerceServiceError::AccountNotConfigured);
        }
        let in_use: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM commerce_products WHERE project_id=$1) \
             OR EXISTS(SELECT 1 FROM commerce_customers WHERE project_id=$1) \
             OR EXISTS(SELECT 1 FROM commerce_orders WHERE project_id=$1) \
             OR EXISTS(SELECT 1 FROM commerce_subscriptions WHERE project_id=$1)",
        )
        .bind(project_id.0)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if in_use {
            return Err(CommerceServiceError::AccountInUse);
        }
        sqlx::query("DELETE FROM project_commerce_secrets WHERE project_id=$1")
            .bind(project_id.0)
            .execute(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query("DELETE FROM project_commerce_accounts WHERE project_id=$1")
            .bind(project_id.0)
            .execute(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)
    }

    async fn ensure_account_rebinding_safe(
        &self,
        project_id: ProjectId,
        next_provider_account_id: Option<&str>,
    ) -> Result<(), CommerceServiceError> {
        let current: Option<Option<String>> = sqlx::query_scalar(
            "SELECT provider_account_id FROM project_commerce_accounts \
             WHERE project_id=$1 AND provider='stripe'",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if current.as_ref().and_then(Option::as_deref) == next_provider_account_id {
            return Ok(());
        }
        let has_provider_bound_state: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM commerce_products WHERE project_id=$1) \
             OR EXISTS(SELECT 1 FROM commerce_customers WHERE project_id=$1) \
             OR EXISTS(SELECT 1 FROM commerce_orders WHERE project_id=$1) \
             OR EXISTS(SELECT 1 FROM commerce_subscriptions WHERE project_id=$1)",
        )
        .bind(project_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        if has_provider_bound_state {
            Err(CommerceServiceError::Conflict)
        } else {
            Ok(())
        }
    }

    async fn account_summary(
        &self,
        project_id: ProjectId,
    ) -> Result<CommerceAccountSummary, CommerceServiceError> {
        let row = sqlx::query(
            "SELECT mode,status,livemode,provider_account_id,capabilities,requirements_due,\
                    disabled_reason,EXISTS(SELECT 1 FROM project_commerce_secrets s \
                    WHERE s.project_id=a.project_id) secrets_configured \
             FROM project_commerce_accounts a WHERE project_id=$1 AND provider='stripe'",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::AccountNotConfigured)?;
        let mode = parse_mode(
            row.try_get("mode")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?;
        account_summary_from_row(project_id, &row, self.webhook_url(project_id, mode)?)
    }

    async fn configure_byo(
        &self,
        project_id: ProjectId,
        secret_key: &str,
        webhook_secret: &str,
    ) -> Result<CommerceAccountSummary, CommerceServiceError> {
        validate_stripe_secret(secret_key, "sk_")?;
        validate_stripe_secret(webhook_secret, "whsec_")?;
        let context = ProviderContext {
            mode: CommerceProviderMode::BringYourOwnKeys,
            secret_key: ProtectedString::from(secret_key.to_owned()),
            webhook_secret: ProtectedString::from(webhook_secret.to_owned()),
            connected_account: None,
            provider_account_id: None,
            livemode: secret_key.starts_with("sk_live_"),
            status: CommerceAccountStatus::Configuring,
            capabilities: CommerceAccountCapabilities {
                one_time_payments: false,
                recurring_payments: false,
                refunds: false,
                customer_portal: false,
            },
        };
        let payload = self
            .stripe
            .request(&context, Method::GET, "v1/account", &[], None)
            .await?;
        let account = parse_v1_account(&payload, context.livemode)?;
        self.ensure_account_rebinding_safe(project_id, Some(&account.id))
            .await?;
        let secret_ciphertext = self
            .cipher
            .seal(
                ProviderSecretScope::ProjectCommerce(project_id),
                "secret_key",
                secret_key,
            )?
            .to_packed();
        let webhook_ciphertext = self
            .cipher
            .seal(
                ProviderSecretScope::ProjectCommerce(project_id),
                "webhook_secret",
                webhook_secret,
            )?
            .to_packed();
        let fingerprint: [u8; 32] = Sha256::digest(secret_key.as_bytes()).into();
        let capabilities = capability_names(&account.capabilities);
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO project_commerce_accounts \
             (project_id,provider,provider_account_id,status,charge_model,capabilities,\
              controller_configuration,mode,livemode,requirements_due,disabled_reason) \
             VALUES ($1,'stripe',$2,$3,'direct',$4,'{}'::jsonb,'byo_keys',$5,$6,$7) \
             ON CONFLICT (project_id) DO UPDATE SET provider='stripe',provider_account_id=$2,\
              status=$3,charge_model='direct',capabilities=$4,controller_configuration='{}'::jsonb,\
              mode='byo_keys',livemode=$5,requirements_due=$6,disabled_reason=$7,updated_at=now()",
        )
        .bind(project_id.0)
        .bind(&account.id)
        .bind(account_status_name(account.status))
        .bind(&capabilities)
        .bind(account.livemode)
        .bind(&account.requirements_due)
        .bind(&account.disabled_reason)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO project_commerce_secrets \
             (project_id,provider,key_version,secret_key_ciphertext,webhook_secret_ciphertext,\
              secret_key_fingerprint,rotated_at) VALUES ($1,'stripe',$2,$3,$4,$5,now()) \
             ON CONFLICT (project_id) DO UPDATE SET provider='stripe',key_version=$2,\
              secret_key_ciphertext=$3,webhook_secret_ciphertext=$4,secret_key_fingerprint=$5,\
              rotated_at=now()",
        )
        .bind(project_id.0)
        .bind(self.cipher.key_version)
        .bind(secret_ciphertext)
        .bind(webhook_ciphertext)
        .bind(fingerprint.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        self.account_summary(project_id).await
    }

    async fn connect_onboarding(
        &self,
        project_id: ProjectId,
        input: &CreateCommerceConnectOnboardingRequest,
        idempotency_key: &str,
    ) -> Result<CommerceOnboardingResponse, CommerceServiceError> {
        let connect = self
            .connect
            .as_ref()
            .ok_or(CommerceServiceError::InvalidConfiguration)?;
        validate_country(&input.country)?;
        validate_email(&input.email)?;
        let return_url = validate_return_url(&input.return_url)?;
        let refresh_url = validate_return_url(&input.refresh_url)?;
        let platform_context = ProviderContext {
            mode: CommerceProviderMode::StripeConnect,
            secret_key: connect.secret_key.clone(),
            webhook_secret: connect.webhook_secret.clone(),
            connected_account: None,
            provider_account_id: None,
            livemode: connect.secret_key.expose_secret().starts_with("sk_live_"),
            status: CommerceAccountStatus::Configuring,
            capabilities: CommerceAccountCapabilities {
                one_time_payments: false,
                recurring_payments: false,
                refunds: false,
                customer_portal: false,
            },
        };
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT provider_account_id FROM project_commerce_accounts \
             WHERE project_id=$1 AND provider='stripe' AND mode='stripe_connect'",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .flatten();
        if existing.is_none() {
            self.ensure_account_rebinding_safe(project_id, None).await?;
        }
        let account_payload = if let Some(account_id) = existing.as_deref() {
            let include = vec![
                ("include[]".to_owned(), "configuration.merchant".to_owned()),
                ("include[]".to_owned(), "identity".to_owned()),
                ("include[]".to_owned(), "defaults".to_owned()),
                ("include[]".to_owned(), "requirements".to_owned()),
            ];
            self.stripe
                .form(
                    &StripeRequestAuth {
                        secret_key: platform_context.secret_key.clone(),
                        connected_account: None,
                    },
                    Method::GET,
                    &format!("v2/core/accounts/{account_id}"),
                    &include,
                    None,
                )
                .await?
        } else {
            // Accounts v2 makes liability, fee collection and dashboard
            // access explicit. Stripe is merchant-risk/fee liable and the
            // project owner receives full Dashboard access; FFDB uses only
            // direct charges on this account.
            let account_payload = json!({
                "contact_email": input.email.trim().to_ascii_lowercase(),
                "display_name": input.email.split('@').next().unwrap_or("FFDB merchant"),
                "identity": {
                    "country": input.country.to_ascii_lowercase(),
                },
                "configuration": {
                    "merchant": {
                        "capabilities": {
                            "card_payments": {"requested": true}
                        }
                    }
                },
                "defaults": {
                    "responsibilities": {
                        "fees_collector": "stripe",
                        "losses_collector": "stripe"
                    }
                },
                "dashboard": "full",
                "metadata": {"ffdb_project_id": project_id.to_string()},
                "include": ["configuration.merchant", "identity", "defaults"]
            });
            self.stripe
                .json(
                    &StripeRequestAuth {
                        secret_key: platform_context.secret_key.clone(),
                        connected_account: None,
                    },
                    Method::POST,
                    "v2/core/accounts",
                    &account_payload,
                    Some(&format!("{idempotency_key}:account")),
                )
                .await?
        };
        let account = parse_v2_account(&account_payload, platform_context.livemode)?;
        let account_id = account.id.clone();
        self.ensure_account_rebinding_safe(project_id, Some(&account_id))
            .await?;
        let link = self
            .stripe
            .json(
                &StripeRequestAuth {
                    secret_key: platform_context.secret_key.clone(),
                    connected_account: None,
                },
                Method::POST,
                "v2/core/account_links",
                &json!({
                    "account": account_id,
                    "use_case": {
                        "type": "account_onboarding",
                        "account_onboarding": {
                            "configurations": ["merchant"],
                            "return_url": return_url,
                            "refresh_url": refresh_url,
                            "collection_options": {
                                "fields": "eventually_due",
                                "future_requirements": "include"
                            }
                        }
                    }
                }),
                Some(&format!("{idempotency_key}:link")),
            )
            .await?;
        let onboarding_url = verified_stripe_url(&link, "url")?;
        let expires_at_ms = link
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
            .ok_or(CommerceServiceError::ProviderResponseInvalid)?;
        let capabilities = capability_names(&account.capabilities);
        let controller = json!({
            "merchant_of_record": "project_owner",
            "charge_model": "direct"
        });
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO project_commerce_accounts \
             (project_id,provider,provider_account_id,status,charge_model,capabilities,\
              controller_configuration,mode,livemode,onboarding_url_expires_at,\
              requirements_due,disabled_reason) \
             VALUES ($1,'stripe',$2,$3,'direct',$4,$5,'stripe_connect',$6,\
                     to_timestamp($7::double precision/1000),$8,$9) \
             ON CONFLICT (project_id) DO UPDATE SET provider='stripe',provider_account_id=$2,\
              status=$3,charge_model='direct',capabilities=$4,controller_configuration=$5,\
              mode='stripe_connect',livemode=$6,onboarding_url_expires_at=\
              to_timestamp($7::double precision/1000),requirements_due=$8,disabled_reason=$9,\
              updated_at=now()",
        )
        .bind(project_id.0)
        .bind(&account_id)
        .bind(account_status_name(
            if account.status == CommerceAccountStatus::Enabled {
                account.status
            } else {
                CommerceAccountStatus::Onboarding
            },
        ))
        .bind(&capabilities)
        .bind(controller)
        .bind(account.livemode)
        .bind(expires_at_ms)
        .bind(&account.requirements_due)
        .bind(&account.disabled_reason)
        .execute(&mut *transaction)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        sqlx::query("DELETE FROM project_commerce_secrets WHERE project_id=$1")
            .bind(project_id.0)
            .execute(&mut *transaction)
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| CommerceServiceError::Unavailable)?;
        Ok(CommerceOnboardingResponse {
            account: self.account_summary(project_id).await?,
            onboarding_url,
            expires_at_ms,
        })
    }

    async fn refresh_account(
        &self,
        project_id: ProjectId,
    ) -> Result<CommerceAccountSummary, CommerceServiceError> {
        let context = self.provider_context(project_id, false).await?;
        let account = match context.mode {
            CommerceProviderMode::BringYourOwnKeys => {
                let payload = self
                    .stripe
                    .request(&context, Method::GET, "v1/account", &[], None)
                    .await?;
                parse_v1_account(&payload, context.livemode)?
            }
            CommerceProviderMode::StripeConnect => {
                let account_id = context
                    .provider_account_id
                    .as_deref()
                    .ok_or(CommerceServiceError::Unavailable)?;
                let include = vec![
                    ("include[]".to_owned(), "configuration.merchant".to_owned()),
                    ("include[]".to_owned(), "identity".to_owned()),
                    ("include[]".to_owned(), "defaults".to_owned()),
                    ("include[]".to_owned(), "requirements".to_owned()),
                ];
                let payload = self
                    .stripe
                    .form(
                        &StripeRequestAuth {
                            secret_key: context.secret_key.clone(),
                            connected_account: None,
                        },
                        Method::GET,
                        &format!("v2/core/accounts/{account_id}"),
                        &include,
                        None,
                    )
                    .await?;
                let account = parse_v2_account(&payload, context.livemode)?;
                if account.id != account_id {
                    return Err(CommerceServiceError::ProviderResponseInvalid);
                }
                account
            }
        };
        sqlx::query(
            "UPDATE project_commerce_accounts SET status=$2,capabilities=$3,livemode=$4,\
             requirements_due=$5,disabled_reason=$6,updated_at=now() \
             WHERE project_id=$1 AND provider='stripe'",
        )
        .bind(project_id.0)
        .bind(account_status_name(account.status))
        .bind(capability_names(&account.capabilities))
        .bind(account.livemode)
        .bind(account.requirements_due)
        .bind(account.disabled_reason)
        .execute(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?;
        self.account_summary(project_id).await
    }

    async fn provider_context(
        &self,
        project_id: ProjectId,
        require_enabled: bool,
    ) -> Result<ProviderContext, CommerceServiceError> {
        let row = sqlx::query(
            "SELECT mode,status,livemode,provider_account_id,capabilities \
             FROM project_commerce_accounts WHERE project_id=$1 AND provider='stripe'",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CommerceServiceError::Unavailable)?
        .ok_or(CommerceServiceError::AccountNotConfigured)?;
        let mode = parse_mode(
            row.try_get("mode")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?;
        let status = parse_account_status(
            row.try_get("status")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        )?;
        let capabilities = parse_capabilities(
            row.try_get("capabilities")
                .map_err(|_| CommerceServiceError::Unavailable)?,
        );
        if require_enabled && status != CommerceAccountStatus::Enabled {
            return Err(CommerceServiceError::AccountRestricted);
        }
        let livemode: bool = row
            .try_get("livemode")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        let provider_account_id: Option<String> = row
            .try_get("provider_account_id")
            .map_err(|_| CommerceServiceError::Unavailable)?;
        match mode {
            CommerceProviderMode::BringYourOwnKeys => {
                let secret = sqlx::query(
                    "SELECT key_version,secret_key_ciphertext,webhook_secret_ciphertext \
                     FROM project_commerce_secrets WHERE project_id=$1 AND provider='stripe'",
                )
                .bind(project_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| CommerceServiceError::Unavailable)?
                .ok_or(CommerceServiceError::AccountNotConfigured)?;
                let key_version: i32 = secret
                    .try_get("key_version")
                    .map_err(|_| CommerceServiceError::Unavailable)?;
                let secret_key_ciphertext: Vec<u8> = secret
                    .try_get("secret_key_ciphertext")
                    .map_err(|_| CommerceServiceError::Unavailable)?;
                let webhook_secret_ciphertext: Vec<u8> = secret
                    .try_get("webhook_secret_ciphertext")
                    .map_err(|_| CommerceServiceError::Unavailable)?;
                Ok(ProviderContext {
                    mode,
                    secret_key: self.cipher.open(
                        ProviderSecretScope::ProjectCommerce(project_id),
                        "secret_key",
                        &SealedProviderSecret::from_packed(key_version, &secret_key_ciphertext)?,
                    )?,
                    webhook_secret: self.cipher.open(
                        ProviderSecretScope::ProjectCommerce(project_id),
                        "webhook_secret",
                        &SealedProviderSecret::from_packed(
                            key_version,
                            &webhook_secret_ciphertext,
                        )?,
                    )?,
                    connected_account: None,
                    provider_account_id,
                    livemode,
                    status,
                    capabilities,
                })
            }
            CommerceProviderMode::StripeConnect => {
                let connect = self
                    .connect
                    .as_ref()
                    .ok_or(CommerceServiceError::InvalidConfiguration)?;
                let account = provider_account_id
                    .clone()
                    .ok_or(CommerceServiceError::Unavailable)?;
                Ok(ProviderContext {
                    mode,
                    secret_key: connect.secret_key.clone(),
                    webhook_secret: connect.webhook_secret.clone(),
                    connected_account: Some(account),
                    provider_account_id,
                    livemode,
                    status,
                    capabilities,
                })
            }
        }
    }
}
