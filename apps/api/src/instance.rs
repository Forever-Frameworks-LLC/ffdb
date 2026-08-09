//! First-run instance ownership, deployment policy, and global administration.
//!
//! The first platform user is installed as the immutable instance owner by the
//! control-plane repository. This module owns every later global mutation.

use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use chrono::DateTime;
use ffdb_audit::AuditOutcome;
use ffdb_billing::{
    BillingError, PlatformBillingProvider, PlatformCheckoutInput, PlatformPortalInput,
    STORAGE_BILLING_UNIT_BYTES, StripeBillingConfig, StripeBillingProvider, StripeUsageMeterConfig,
    UsageMeterEvent, UsageMetric, UsageSummary, UsageSummaryInput, VerifiedBillingEvent,
};
use ffdb_protocol::{
    BillingRedirect, CompleteInstanceSetupRequest, CompleteInstanceSetupResponse,
    CreateInstanceConnectOnboardingRequest, GrantInstanceAdministratorRequest,
    GrantOrganizationBillingExemptionRequest, InstanceAdministratorRole,
    InstanceAdministratorSummary, InstanceBillingAccountStatus, InstanceBillingAccountSummary,
    InstanceBillingMode, InstanceBillingOnboarding, InstanceDeploymentMode,
    InstanceOrganizationPage, InstanceOrganizationSummary, InstancePlanCatalogEntry,
    InstanceReadsAtLimit, InstanceSignupsAtLimit, InstanceStatus, InstanceUserPage,
    InstanceUserSummary, InstanceWritesAtLimit, OrganizationBillingExemptionSummary,
    OrganizationCreationPolicy, OrganizationId, PlatformBillingTier, PlatformBillingUnit,
    PublicInstanceSetupStatus, PutInstancePlanCatalogEntryRequest, RequestId,
    UpdateInstanceDisabledRequest, UpdateOrganizationCreationPolicyRequest, UserId,
};
use secrecy::SecretString as ProtectedString;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use url::Url;
use uuid::Uuid;

use super::commerce::{
    CommerceServiceError, ProviderSecretEnvelope, ProviderSecretScope, SealedProviderSecret,
    StripeRequestAuth, StripeRequestClient,
};
use super::management::{authenticated, require_management_audit, terminal_management_audit};
use super::{ApiError, ApiState};

const MAX_PAGE_SIZE: u32 = 500;
const DEFAULT_PAGE_SIZE: u32 = 100;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const STRIPE_CATALOG_VERSION: i32 = 1;
const CONNECT_SECRET_KIND: &str = "stripe_connect_secret_key";
const CONNECT_WEBHOOK_SECRET_KIND: &str = "stripe_connect_webhook_secret";

#[derive(Clone)]
pub struct InstanceService {
    pool: PgPool,
    secret_envelope: ProviderSecretEnvelope,
    stripe: StripeRequestClient,
    connect_auth: Option<StripeRequestAuth>,
    connect_webhook_secret: Option<ProtectedString>,
    billing_provider: Arc<InstanceBillingProvider>,
}

pub struct InstanceServiceConfig {
    pub master_key: Vec<u8>,
    pub key_version: i32,
    /// Legacy deployment-level fallback for installations configured before
    /// Connect credentials became encrypted instance state. Fresh onboarding
    /// persists owner-supplied credentials and does not require these values.
    pub connect_secret_key: Option<ProtectedString>,
    pub connect_webhook_secret: Option<ProtectedString>,
    pub billing: Option<InstanceStripeBillingConfig>,
}

#[derive(Clone, Debug)]
pub struct InstanceStripeBillingConfig {
    /// Optional backward-compatible catalog import. New BYO and Connect setup
    /// provisions an account-scoped catalog automatically.
    pub byo_catalog: Option<InstanceStripeProviderCatalog>,
    /// Stable event names used when FFDB provisions a Connect catalog.
    pub usage_events: Vec<InstanceStripeUsageEventConfig>,
    pub pro_billing_unit: PlatformBillingUnit,
    pub success_url: Url,
    pub cancel_url: Url,
    pub portal_return_url: Url,
}

#[derive(Clone, Debug)]
pub struct InstanceStripeProviderCatalog {
    pub product_id: Option<String>,
    pub pro_base_price_id: String,
    pub usage_meters: Vec<StripeUsageMeterConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceStripeUsageEventConfig {
    pub metric: UsageMetric,
    pub event_name: String,
}

/// Stable provider handle installed in `ManagementState`. Its delegate changes
/// only after a validated, committed owner setup, so checkout, webhook, and
/// metering calls immediately use the selected instance billing account.
pub struct InstanceBillingProvider {
    template: Option<InstanceStripeBillingConfig>,
    active: RwLock<Option<Arc<StripeBillingProvider>>>,
}

impl std::fmt::Debug for InstanceBillingProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstanceBillingProvider")
            .field("template", &self.template)
            .field(
                "active",
                &self.active.read().map_or("poisoned", |value| {
                    if value.is_some() {
                        "configured"
                    } else {
                        "disabled"
                    }
                }),
            )
            .finish()
    }
}

impl InstanceBillingProvider {
    fn new(template: Option<InstanceStripeBillingConfig>) -> Self {
        Self {
            template,
            active: RwLock::new(None),
        }
    }

    fn build(
        &self,
        secret_key: ProtectedString,
        webhook_secret: ProtectedString,
        connected_account: Option<String>,
        catalog: &InstanceStripeProviderCatalog,
    ) -> Result<Arc<StripeBillingProvider>, InstanceServiceError> {
        let template = self
            .template
            .as_ref()
            .ok_or(InstanceServiceError::InvalidConfiguration)?;
        StripeBillingProvider::new(StripeBillingConfig {
            secret_key,
            webhook_secret,
            connected_account,
            pro_base_price_id: catalog.pro_base_price_id.clone(),
            usage_meters: catalog.usage_meters.clone(),
            pro_billing_unit: template.pro_billing_unit,
            success_url: template.success_url.clone(),
            cancel_url: template.cancel_url.clone(),
            portal_return_url: template.portal_return_url.clone(),
        })
        .map(Arc::new)
        .map_err(map_billing_error)
    }

    fn activate(&self, provider: Arc<StripeBillingProvider>) -> Result<(), InstanceServiceError> {
        *self
            .active
            .write()
            .map_err(|_| InstanceServiceError::Unavailable)? = Some(provider);
        Ok(())
    }

    fn deactivate(&self) -> Result<(), InstanceServiceError> {
        *self
            .active
            .write()
            .map_err(|_| InstanceServiceError::Unavailable)? = None;
        Ok(())
    }

    fn current(&self) -> Result<Arc<StripeBillingProvider>, BillingError> {
        self.active
            .read()
            .map_err(|_| BillingError::ProviderUnavailable)?
            .clone()
            .ok_or(BillingError::InvalidConfiguration)
    }
}

#[async_trait::async_trait]
impl PlatformBillingProvider for InstanceBillingProvider {
    fn is_configured(&self) -> bool {
        self.active.read().is_ok_and(|provider| provider.is_some())
    }

    async fn create_checkout(
        &self,
        input: &PlatformCheckoutInput,
    ) -> Result<BillingRedirect, BillingError> {
        self.current()?.create_checkout(input).await
    }

    async fn create_portal(
        &self,
        input: &PlatformPortalInput,
    ) -> Result<BillingRedirect, BillingError> {
        self.current()?.create_portal(input).await
    }

    fn verify_webhook(
        &self,
        payload: &[u8],
        signature: &str,
        now_seconds: i64,
    ) -> Result<VerifiedBillingEvent, BillingError> {
        self.current()?
            .verify_webhook(payload, signature, now_seconds)
    }

    async fn report_usage(&self, input: &UsageMeterEvent) -> Result<(), BillingError> {
        self.current()?.report_usage(input).await
    }

    async fn usage_summary(&self, input: &UsageSummaryInput) -> Result<UsageSummary, BillingError> {
        self.current()?.usage_summary(input).await
    }
}

impl std::fmt::Debug for InstanceServiceConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstanceServiceConfig")
            .field("master_key", &"[REDACTED]")
            .field("key_version", &self.key_version)
            .field(
                "connect_secret_key",
                &self.connect_secret_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "connect_webhook_secret",
                &self.connect_webhook_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("billing", &self.billing)
            .finish()
    }
}

impl std::fmt::Debug for InstanceService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstanceService")
            .finish_non_exhaustive()
    }
}

impl InstanceService {
    pub fn new(pool: PgPool, config: InstanceServiceConfig) -> Result<Self, InstanceServiceError> {
        if config.connect_secret_key.is_some() != config.connect_webhook_secret.is_some() {
            return Err(InstanceServiceError::InvalidConfiguration);
        }
        if let Some(billing) = &config.billing {
            validate_usage_events(&billing.usage_events)?;
            if let Some(catalog) = &billing.byo_catalog {
                validate_provider_catalog_shape(catalog)?;
            }
        }
        let connect_webhook_secret = config.connect_webhook_secret.clone();
        let billing_provider = Arc::new(InstanceBillingProvider::new(config.billing));
        Ok(Self {
            pool,
            secret_envelope: ProviderSecretEnvelope::new(config.master_key, config.key_version)
                .map_err(map_commerce_error)?,
            stripe: StripeRequestClient::production().map_err(map_commerce_error)?,
            connect_auth: config
                .connect_secret_key
                .map(|secret_key| StripeRequestAuth {
                    secret_key,
                    connected_account: None,
                }),
            connect_webhook_secret,
            billing_provider,
        })
    }

    #[must_use]
    pub fn billing_provider(&self) -> Option<Arc<InstanceBillingProvider>> {
        self.billing_provider
            .template
            .as_ref()
            .map(|_| self.billing_provider.clone())
    }

    /// Rebuild the in-memory provider from durable encrypted instance state.
    /// Call once during startup after migrations and after any credential
    /// rotation. A configured platform mode fails closed if material is absent.
    pub async fn reload_billing_provider(&self) -> Result<(), InstanceServiceError> {
        let row = sqlx::query(
            "SELECT s.owner_user_id,s.deployment_mode,a.mode,a.provider_account_id,a.status \
             FROM instance_settings s LEFT JOIN instance_billing_accounts a ON a.singleton=true \
             WHERE s.singleton=true",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let Some(row) = row else {
            return Ok(());
        };
        let owner = UserId(row.try_get("owner_user_id")?);
        let mode = parse_deployment_mode(row.try_get::<String, _>("deployment_mode")?.as_str())?;
        if matches!(
            mode,
            InstanceDeploymentMode::PlatformByo | InstanceDeploymentMode::PlatformConnect
        ) && row.try_get::<Option<String>, _>("status")?.as_deref() != Some("enabled")
        {
            return self.billing_provider.deactivate();
        }
        let account_id = row
            .try_get::<Option<String>, _>("provider_account_id")?
            .filter(|value| valid_provider_id(value, "acct_"));
        let catalog = if matches!(
            mode,
            InstanceDeploymentMode::PlatformByo | InstanceDeploymentMode::PlatformConnect
        ) {
            Some(
                self.load_provider_catalog(
                    account_id
                        .as_deref()
                        .ok_or(InstanceServiceError::InvalidConfiguration)?,
                )
                .await?,
            )
        } else {
            None
        };
        let provider = match mode {
            InstanceDeploymentMode::Unconfigured
            | InstanceDeploymentMode::Private
            | InstanceDeploymentMode::Team => return self.billing_provider.deactivate(),
            InstanceDeploymentMode::PlatformByo => {
                if row.try_get::<Option<String>, _>("mode")?.as_deref() != Some("byo_keys") {
                    return Err(InstanceServiceError::InvalidConfiguration);
                }
                let secret_key = self
                    .load_instance_secret(owner, "stripe_secret_key")
                    .await?;
                let webhook_secret = self
                    .load_instance_secret(owner, "stripe_webhook_secret")
                    .await?;
                self.billing_provider.build(
                    secret_key,
                    webhook_secret,
                    None,
                    catalog
                        .as_ref()
                        .ok_or(InstanceServiceError::InvalidConfiguration)?,
                )?
            }
            InstanceDeploymentMode::PlatformConnect => {
                if row.try_get::<Option<String>, _>("mode")?.as_deref() != Some("stripe_connect") {
                    return Err(InstanceServiceError::InvalidConfiguration);
                }
                let account_id = account_id.ok_or(InstanceServiceError::InvalidConfiguration)?;
                let (auth, webhook_secret) = self.connect_credentials(owner).await?;
                self.billing_provider.build(
                    auth.secret_key,
                    webhook_secret,
                    Some(account_id),
                    catalog
                        .as_ref()
                        .ok_or(InstanceServiceError::InvalidConfiguration)?,
                )?
            }
        };
        self.billing_provider.activate(provider)
    }

    async fn load_instance_secret(
        &self,
        owner: UserId,
        kind: &'static str,
    ) -> Result<ProtectedString, InstanceServiceError> {
        self.load_instance_secret_optional(owner, kind)
            .await?
            .ok_or(InstanceServiceError::InvalidConfiguration)
    }

    async fn load_instance_secret_optional(
        &self,
        owner: UserId,
        kind: &'static str,
    ) -> Result<Option<ProtectedString>, InstanceServiceError> {
        let row = sqlx::query(
            "SELECT key_version,nonce,ciphertext FROM instance_billing_secrets \
             WHERE secret_kind=$1",
        )
        .bind(kind)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let nonce: Vec<u8> = row.try_get("nonce")?;
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_| InstanceServiceError::InvalidConfiguration)?;
        let sealed = SealedProviderSecret {
            key_version: row.try_get("key_version")?,
            nonce,
            ciphertext: row.try_get("ciphertext")?,
        };
        self.secret_envelope
            .open(
                ProviderSecretScope::PlatformInstanceBilling(owner.0),
                kind,
                &sealed,
            )
            .map(Some)
            .map_err(map_commerce_error)
    }

    async fn connect_credentials(
        &self,
        owner: UserId,
    ) -> Result<(StripeRequestAuth, ProtectedString), InstanceServiceError> {
        let secret_key = self
            .load_instance_secret_optional(owner, CONNECT_SECRET_KIND)
            .await?;
        let webhook_secret = self
            .load_instance_secret_optional(owner, CONNECT_WEBHOOK_SECRET_KIND)
            .await?;
        match (secret_key, webhook_secret) {
            (Some(secret_key), Some(webhook_secret)) => Ok((
                StripeRequestAuth {
                    secret_key,
                    connected_account: None,
                },
                webhook_secret,
            )),
            (None, None) => self
                .connect_auth
                .clone()
                .zip(self.connect_webhook_secret.clone())
                .ok_or(InstanceServiceError::InvalidConfiguration),
            _ => Err(InstanceServiceError::InvalidConfiguration),
        }
    }

    pub async fn public_setup_status(
        &self,
    ) -> Result<PublicInstanceSetupStatus, InstanceServiceError> {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM platform_users) users_exist, \
                    EXISTS(SELECT 1 FROM instance_settings WHERE singleton=true \
                           AND setup_completed_at IS NOT NULL) setup_complete",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let users_exist: bool = row
            .try_get("users_exist")
            .map_err(|_| InstanceServiceError::Unavailable)?;
        let setup_complete: bool = row
            .try_get("setup_complete")
            .map_err(|_| InstanceServiceError::Unavailable)?;
        let (platform_byo_available, platform_connect_available) =
            billing_mode_capabilities(self.billing_provider.template.is_some());
        Ok(PublicInstanceSetupStatus {
            bootstrap_available: !users_exist,
            setup_required: users_exist && !setup_complete,
            platform_byo_available,
            platform_connect_available,
        })
    }

    pub async fn complete_setup(
        &self,
        actor: UserId,
        request: &CompleteInstanceSetupRequest,
        idempotency_key: &str,
    ) -> Result<CompleteInstanceSetupResponse, InstanceServiceError> {
        self.require_owner(actor).await?;
        validate_provider_idempotency_key(idempotency_key)?;
        let (requested_mode, policy) = setup_mode_and_policy(request);
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        let current = sqlx::query(
            "SELECT owner_user_id,deployment_mode,organization_creation_policy, \
                    setup_completed_at IS NOT NULL setup_complete \
             FROM instance_settings WHERE singleton=true FOR UPDATE",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotInitialized)?;
        let owner = UserId(current.try_get("owner_user_id")?);
        if owner != actor {
            return Err(InstanceServiceError::Forbidden);
        }
        let current_mode =
            parse_deployment_mode(current.try_get::<String, _>("deployment_mode")?.as_str())?;
        let current_policy = parse_creation_policy(
            current
                .try_get::<String, _>("organization_creation_policy")?
                .as_str(),
        )?;
        let setup_complete: bool = current.try_get("setup_complete")?;
        let billing_account = sqlx::query(
            "SELECT mode,provider_account_id FROM instance_billing_accounts \
             WHERE singleton=true FOR UPDATE",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let current_billing_mode = billing_account
            .as_ref()
            .map(|row| row.try_get::<String, _>("mode"))
            .transpose()?;
        let current_provider_account_id = billing_account
            .as_ref()
            .map(|row| row.try_get::<Option<String>, _>("provider_account_id"))
            .transpose()?
            .flatten();
        let tenant_billing_in_use: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM organization_billing_accounts \
             WHERE status <> 'canceled')",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        ensure_billing_reconfiguration_safe(
            current_mode,
            requested_mode,
            current_billing_mode.as_deref(),
            current_provider_account_id.as_deref(),
            tenant_billing_in_use,
        )?;
        if setup_complete
            && current_mode == requested_mode
            && current_policy == policy
            && matches!(
                request,
                CompleteInstanceSetupRequest::Private { .. }
                    | CompleteInstanceSetupRequest::Team { .. }
            )
        {
            transaction
                .rollback()
                .await
                .map_err(|_| InstanceServiceError::Unavailable)?;
            return Ok(CompleteInstanceSetupResponse {
                instance: self.status(actor).await?,
                onboarding: None,
            });
        }

        let (onboarding, setup_ready) = match request {
            CompleteInstanceSetupRequest::Private { .. }
            | CompleteInstanceSetupRequest::Team { .. } => {
                debug_assert!(deployment_mode_clears_billing(requested_mode));
                clear_instance_billing(&mut transaction).await?;
                (None, true)
            }
            CompleteInstanceSetupRequest::PlatformByo {
                secret_key,
                webhook_secret,
                ..
            } => {
                let setup_ready = self
                    .configure_byo(
                        &mut transaction,
                        actor,
                        secret_key.expose(),
                        webhook_secret.expose(),
                        tenant_billing_in_use.then_some(
                            current_provider_account_id
                                .as_deref()
                                .ok_or(InstanceServiceError::BillingInUse)?,
                        ),
                    )
                    .await?;
                sqlx::query(
                    "DELETE FROM instance_billing_secrets \
                     WHERE secret_kind IN \
                        ('stripe_connect_access_token','stripe_connect_secret_key', \
                         'stripe_connect_webhook_secret')",
                )
                .execute(&mut *transaction)
                .await
                .map_err(|_| InstanceServiceError::Unavailable)?;
                (None, setup_ready)
            }
            CompleteInstanceSetupRequest::PlatformConnect {
                secret_key,
                webhook_secret,
                country,
                email,
                return_url,
                refresh_url,
                ..
            } => {
                let existing_account = if current_mode == InstanceDeploymentMode::PlatformConnect
                    && current_billing_mode.as_deref() == Some("stripe_connect")
                {
                    current_provider_account_id.as_deref()
                } else {
                    None
                };
                (
                    Some(
                        self.configure_connect(
                            &mut transaction,
                            actor,
                            secret_key.expose(),
                            webhook_secret.expose(),
                            country,
                            email,
                            return_url,
                            refresh_url,
                            idempotency_key,
                            existing_account,
                        )
                        .await?,
                    ),
                    false,
                )
            }
        };
        sqlx::query(
            "UPDATE instance_settings SET deployment_mode=$2,organization_creation_policy=$3, \
                    billing_enforcement_enabled=$4, \
                    setup_completed_at=CASE WHEN $5 THEN COALESCE(setup_completed_at,now()) \
                                            ELSE NULL END,updated_at=now() \
             WHERE singleton=true AND owner_user_id=$1",
        )
        .bind(actor.0)
        .bind(deployment_mode_name(requested_mode))
        .bind(creation_policy_name(policy))
        .bind(matches!(
            requested_mode,
            InstanceDeploymentMode::PlatformByo | InstanceDeploymentMode::PlatformConnect
        ))
        .bind(setup_ready)
        .execute(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        if matches!(
            requested_mode,
            InstanceDeploymentMode::PlatformByo | InstanceDeploymentMode::PlatformConnect
        ) {
            self.reload_billing_provider().await?;
        } else {
            self.billing_provider.deactivate()?;
        }
        Ok(CompleteInstanceSetupResponse {
            instance: self.status(actor).await?,
            onboarding,
        })
    }

    pub async fn connect_onboarding(
        &self,
        actor: UserId,
        request: &CreateInstanceConnectOnboardingRequest,
        idempotency_key: &str,
    ) -> Result<InstanceBillingOnboarding, InstanceServiceError> {
        self.require_owner(actor).await?;
        validate_provider_idempotency_key(idempotency_key)?;
        let return_url = validate_return_url(&request.return_url)?;
        let refresh_url = validate_return_url(&request.refresh_url)?;
        let account_id: String = sqlx::query_scalar(
            "SELECT a.provider_account_id FROM instance_billing_accounts a \
             JOIN instance_settings s ON s.singleton=true \
             WHERE a.singleton=true AND a.mode='stripe_connect' \
               AND s.deployment_mode='platform_connect' AND s.owner_user_id=$1",
        )
        .bind(actor.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .flatten()
        .ok_or(InstanceServiceError::NotFound)?;
        let (auth, _) = self.connect_credentials(actor).await?;
        self.create_connect_account_link(
            &auth,
            &account_id,
            &return_url,
            &refresh_url,
            idempotency_key,
        )
        .await
    }

    pub async fn refresh_billing_account(
        &self,
        actor: UserId,
    ) -> Result<InstanceStatus, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let row = sqlx::query(
            "SELECT s.owner_user_id,s.deployment_mode,a.mode,a.provider_account_id \
             FROM instance_settings s JOIN instance_billing_accounts a ON a.singleton=true \
             WHERE s.singleton=true",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotFound)?;
        let owner = UserId(row.try_get("owner_user_id")?);
        let mode = parse_deployment_mode(row.try_get::<String, _>("deployment_mode")?.as_str())?;
        let mut connect_catalog = None;
        let account = match mode {
            InstanceDeploymentMode::PlatformByo => {
                let secret_key = self
                    .load_instance_secret(owner, "stripe_secret_key")
                    .await?;
                let payload = self
                    .stripe
                    .form(
                        &StripeRequestAuth {
                            secret_key,
                            connected_account: None,
                        },
                        reqwest::Method::GET,
                        "v1/account",
                        &[],
                        None,
                    )
                    .await
                    .map_err(map_commerce_error)?;
                parse_v1_account(&payload)?
            }
            InstanceDeploymentMode::PlatformConnect => {
                let account_id = row
                    .try_get::<Option<String>, _>("provider_account_id")?
                    .filter(|value| valid_provider_id(value, "acct_"))
                    .ok_or(InstanceServiceError::Unavailable)?;
                let (auth, _) = self.connect_credentials(owner).await?;
                let payload = self
                    .stripe
                    .json(
                        &auth,
                        reqwest::Method::POST,
                        &format!("v2/core/accounts/{account_id}"),
                        &json!({
                            "include": [
                                "configuration.merchant", "identity", "defaults", "requirements"
                            ]
                        }),
                        None,
                    )
                    .await
                    .map_err(map_commerce_error)?;
                let account = parse_v2_account(&payload)?;
                if account.status == "enabled" {
                    let direct_auth = StripeRequestAuth {
                        secret_key: auth.secret_key.clone(),
                        connected_account: Some(account.id.clone()),
                    };
                    connect_catalog = Some(
                        match self.load_provider_catalog_optional(&account.id).await? {
                            Some(catalog) => catalog,
                            None => {
                                self.provision_provider_catalog(&direct_auth, &account.id)
                                    .await?
                            }
                        },
                    );
                }
                account
            }
            InstanceDeploymentMode::Unconfigured
            | InstanceDeploymentMode::Private
            | InstanceDeploymentMode::Team => return Err(InstanceServiceError::Conflict),
        };
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        persist_billing_account(
            &mut transaction,
            actor,
            match mode {
                InstanceDeploymentMode::PlatformByo => "byo_keys",
                InstanceDeploymentMode::PlatformConnect => "stripe_connect",
                _ => return Err(InstanceServiceError::Conflict),
            },
            &account,
        )
        .await?;
        if let Some(catalog) = &connect_catalog {
            persist_provider_catalog(&mut transaction, &account.id, catalog).await?;
        }
        let setup_ready = setup_completion_ready(
            mode,
            account.status,
            mode == InstanceDeploymentMode::PlatformByo || connect_catalog.is_some(),
        );
        sqlx::query(
            "UPDATE instance_settings SET \
                setup_completed_at=CASE WHEN $1 THEN COALESCE(setup_completed_at,now()) \
                                        ELSE NULL END, \
                updated_at=now() WHERE singleton=true",
        )
        .bind(setup_ready)
        .execute(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        self.reload_billing_provider().await?;
        self.status(actor).await
    }

    async fn configure_byo(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        actor: UserId,
        secret_key: &str,
        webhook_secret: &str,
        required_account_id: Option<&str>,
    ) -> Result<bool, InstanceServiceError> {
        validate_stripe_secret_key(secret_key)?;
        validate_stripe_webhook_secret(webhook_secret)?;
        let auth = StripeRequestAuth {
            secret_key: ProtectedString::from(secret_key.to_owned()),
            connected_account: None,
        };
        let payload = self
            .stripe
            .form(&auth, reqwest::Method::GET, "v1/account", &[], None)
            .await
            .map_err(map_commerce_error)?;
        let account = parse_v1_account(&payload)?;
        if required_account_id.is_some_and(|expected| account.id != expected) {
            return Err(InstanceServiceError::BillingInUse);
        }
        let catalog = match self.load_provider_catalog_optional(&account.id).await? {
            Some(catalog) => catalog,
            None => {
                let legacy_catalog = self
                    .billing_provider
                    .template
                    .as_ref()
                    .and_then(|template| template.byo_catalog.clone());
                if let Some(legacy_catalog) = legacy_catalog {
                    if self
                        .validate_all_provider_prices(&auth, &legacy_catalog)
                        .await
                        .is_ok()
                    {
                        legacy_catalog
                    } else {
                        self.provision_provider_catalog(&auth, &account.id).await?
                    }
                } else {
                    self.provision_provider_catalog(&auth, &account.id).await?
                }
            }
        };
        self.billing_provider.build(
            auth.secret_key.clone(),
            ProtectedString::from(webhook_secret.to_owned()),
            None,
            &catalog,
        )?;
        self.validate_all_provider_prices(&auth, &catalog).await?;
        let scope = ProviderSecretScope::PlatformInstanceBilling(actor.0);
        let sealed_secret = self
            .secret_envelope
            .seal(scope, "stripe_secret_key", secret_key)
            .map_err(map_commerce_error)?;
        let sealed_webhook = self
            .secret_envelope
            .seal(scope, "stripe_webhook_secret", webhook_secret)
            .map_err(map_commerce_error)?;
        persist_instance_secret(transaction, actor, "stripe_secret_key", &sealed_secret).await?;
        persist_instance_secret(transaction, actor, "stripe_webhook_secret", &sealed_webhook)
            .await?;
        persist_billing_account(transaction, actor, "byo_keys", &account).await?;
        persist_provider_catalog(transaction, &account.id, &catalog).await?;
        Ok(account.status == "enabled")
    }

    #[allow(clippy::too_many_arguments)]
    async fn configure_connect(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        actor: UserId,
        secret_key: &str,
        webhook_secret: &str,
        country: &str,
        email: &str,
        return_url: &str,
        refresh_url: &str,
        idempotency_key: &str,
        existing_account: Option<&str>,
    ) -> Result<InstanceBillingOnboarding, InstanceServiceError> {
        validate_stripe_secret_key(secret_key)?;
        validate_stripe_webhook_secret(webhook_secret)?;
        let auth = StripeRequestAuth {
            secret_key: ProtectedString::from(secret_key.to_owned()),
            connected_account: None,
        };
        validate_country(country)?;
        validate_email(email)?;
        let return_url = validate_return_url(return_url)?;
        let refresh_url = validate_return_url(refresh_url)?;
        let (path, account_request) = existing_account.map_or_else(
            || {
                (
                    "v2/core/accounts".to_owned(),
                    json!({
                        "contact_email": email.trim().to_ascii_lowercase(),
                        "dashboard": "full",
                        "identity": { "country": country.to_ascii_lowercase() },
                        "configuration": {
                            "merchant": {
                                "capabilities": { "card_payments": { "requested": true } }
                            }
                        },
                        "defaults": {
                            "responsibilities": {
                                "fees_collector": "stripe",
                                "losses_collector": "stripe"
                            }
                        },
                        "include": [
                            "configuration.merchant", "identity", "defaults", "requirements"
                        ]
                    }),
                )
            },
            |account_id| {
                (
                    format!("v2/core/accounts/{account_id}"),
                    json!({
                        "include": [
                            "configuration.merchant", "identity", "defaults", "requirements"
                        ]
                    }),
                )
            },
        );
        let payload = self
            .stripe
            .json(
                &auth,
                reqwest::Method::POST,
                &path,
                &account_request,
                Some(&format!("{idempotency_key}:account")),
            )
            .await
            .map_err(map_commerce_error)?;
        let account = parse_v2_account(&payload)?;
        let onboarding = self
            .create_connect_account_link(
                &auth,
                &account.id,
                &return_url,
                &refresh_url,
                &format!("{idempotency_key}:link"),
            )
            .await?;
        let scope = ProviderSecretScope::PlatformInstanceBilling(actor.0);
        let sealed_secret = self
            .secret_envelope
            .seal(scope, CONNECT_SECRET_KIND, secret_key)
            .map_err(map_commerce_error)?;
        let sealed_webhook = self
            .secret_envelope
            .seal(scope, CONNECT_WEBHOOK_SECRET_KIND, webhook_secret)
            .map_err(map_commerce_error)?;
        clear_provider_catalog(transaction).await?;
        sqlx::query("DELETE FROM instance_billing_secrets")
            .execute(&mut **transaction)
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        persist_instance_secret(transaction, actor, CONNECT_SECRET_KIND, &sealed_secret).await?;
        persist_instance_secret(
            transaction,
            actor,
            CONNECT_WEBHOOK_SECRET_KIND,
            &sealed_webhook,
        )
        .await?;
        persist_billing_account(transaction, actor, "stripe_connect", &account).await?;
        Ok(onboarding)
    }

    async fn create_connect_account_link(
        &self,
        auth: &StripeRequestAuth,
        account_id: &str,
        return_url: &Url,
        refresh_url: &Url,
        idempotency_key: &str,
    ) -> Result<InstanceBillingOnboarding, InstanceServiceError> {
        if !valid_provider_id(account_id, "acct_") {
            return Err(InstanceServiceError::Unavailable);
        }
        let link = self
            .stripe
            .json(
                auth,
                reqwest::Method::POST,
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
                Some(idempotency_key),
            )
            .await
            .map_err(map_commerce_error)?;
        parse_account_link(&link)
    }

    async fn provision_provider_catalog(
        &self,
        auth: &StripeRequestAuth,
        account_id: &str,
    ) -> Result<InstanceStripeProviderCatalog, InstanceServiceError> {
        let template = self
            .billing_provider
            .template
            .as_ref()
            .ok_or(InstanceServiceError::InvalidConfiguration)?;
        validate_usage_events(&template.usage_events)?;
        let (payg, pro) = self.provisioning_plans().await?;
        if payg.currency != pro.currency {
            return Err(InstanceServiceError::InvalidConfiguration);
        }
        let product = self
            .stripe
            .form(
                auth,
                reqwest::Method::POST,
                "v1/products",
                &stripe_product_form(),
                Some(&catalog_idempotency_key(account_id, "product")?),
            )
            .await
            .map_err(map_commerce_error)?;
        let product_id = provider_payload_id(&product, "prod_")?;
        let base = self
            .stripe
            .form(
                auth,
                reqwest::Method::POST,
                "v1/prices",
                &stripe_base_price_form(&product_id, &pro)?,
                Some(&catalog_idempotency_key(account_id, "pro-base")?),
            )
            .await
            .map_err(map_commerce_error)?;
        let pro_base_price_id = provider_payload_id(&base, "price_")?;
        let mut usage_meters = Vec::with_capacity(UsageMetric::ALL.len());
        for usage in &template.usage_events {
            let metric_name = usage.metric.name();
            let meter = self
                .stripe
                .form(
                    auth,
                    reqwest::Method::POST,
                    "v1/billing/meters",
                    &stripe_meter_form(usage),
                    Some(&catalog_idempotency_key(
                        account_id,
                        &format!("meter-{metric_name}"),
                    )?),
                )
                .await
                .map_err(map_commerce_error)?;
            let meter_id = provider_payload_id(&meter, "mtr_")?;
            let payg_price = self
                .stripe
                .form(
                    auth,
                    reqwest::Method::POST,
                    "v1/prices",
                    &stripe_usage_price_form(&product_id, &meter_id, usage.metric, &payg)?,
                    Some(&catalog_idempotency_key(
                        account_id,
                        &format!("price-payg-{metric_name}"),
                    )?),
                )
                .await
                .map_err(map_commerce_error)?;
            let pro_price = self
                .stripe
                .form(
                    auth,
                    reqwest::Method::POST,
                    "v1/prices",
                    &stripe_usage_price_form(&product_id, &meter_id, usage.metric, &pro)?,
                    Some(&catalog_idempotency_key(
                        account_id,
                        &format!("price-pro-{metric_name}"),
                    )?),
                )
                .await
                .map_err(map_commerce_error)?;
            usage_meters.push(StripeUsageMeterConfig {
                metric: usage.metric,
                event_name: usage.event_name.clone(),
                meter_id,
                payg_price_id: provider_payload_id(&payg_price, "price_")?,
                pro_price_id: provider_payload_id(&pro_price, "price_")?,
            });
        }
        let catalog = InstanceStripeProviderCatalog {
            product_id: Some(product_id),
            pro_base_price_id,
            usage_meters,
        };
        self.validate_all_provider_prices(auth, &catalog).await?;
        Ok(catalog)
    }

    async fn provisioning_plans(
        &self,
    ) -> Result<(StripeProvisioningPlan, StripeProvisioningPlan), InstanceServiceError> {
        let rows = sqlx::query(
            "SELECT tier,currency,base_price_cents,storage_bytes,monthly_reads,monthly_writes, \
                    monthly_active_users,overage_enabled,active \
             FROM billing_price_catalog WHERE tier IN ('pay_as_you_go','pro') ORDER BY tier",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let mut payg = None;
        let mut pro = None;
        for row in rows {
            let plan = provisioning_plan_from_row(&row)?;
            let enabled =
                row.try_get::<bool, _>("active")? && row.try_get::<bool, _>("overage_enabled")?;
            if !enabled {
                return Err(InstanceServiceError::InvalidConfiguration);
            }
            match plan.tier {
                PlatformBillingTier::PayAsYouGo => payg = Some(plan),
                PlatformBillingTier::Pro => pro = Some(plan),
                PlatformBillingTier::Free => {
                    return Err(InstanceServiceError::InvalidConfiguration);
                }
            }
        }
        Ok((
            payg.ok_or(InstanceServiceError::InvalidConfiguration)?,
            pro.ok_or(InstanceServiceError::InvalidConfiguration)?,
        ))
    }

    pub async fn status(&self, actor: UserId) -> Result<InstanceStatus, InstanceServiceError> {
        let row = sqlx::query(
            "SELECT s.owner_user_id,s.deployment_mode,s.organization_creation_policy, \
                    s.billing_enforcement_enabled, \
                    (extract(epoch FROM s.setup_completed_at)*1000)::bigint setup_completed_at_ms, \
                    (extract(epoch FROM s.created_at)*1000)::bigint created_at_ms, \
                    (extract(epoch FROM s.updated_at)*1000)::bigint updated_at_ms, \
                    current_admin.role current_user_role, \
                    (SELECT count(*) FROM instance_administrators)::bigint administrator_count \
             FROM instance_settings s JOIN instance_administrators current_admin \
               ON current_admin.user_id=$1 WHERE s.singleton=true",
        )
        .bind(actor.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::Forbidden)?;
        let billing_account = self.billing_account_summary().await?;
        instance_status_from_row(&row, billing_account)
    }

    pub async fn authorize_organization_creation(
        &self,
        actor: UserId,
    ) -> Result<(), InstanceServiceError> {
        let row = sqlx::query(
            "SELECT s.organization_creation_policy,s.setup_completed_at IS NOT NULL setup_complete, \
                    a.role, \
                    EXISTS(SELECT 1 FROM organization_memberships m WHERE m.user_id=$1) \
                        has_organization_membership \
             FROM instance_settings s LEFT JOIN instance_administrators a ON a.user_id=$1 \
             WHERE s.singleton=true",
        )
        .bind(actor.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotInitialized)?;
        ensure_instance_setup_complete(row.try_get("setup_complete")?)?;
        let policy = parse_creation_policy(
            row.try_get::<String, _>("organization_creation_policy")?
                .as_str(),
        )?;
        let administrator = row.try_get::<Option<String>, _>("role")?.is_some();
        let invited_member = row.try_get::<bool, _>("has_organization_membership")?;
        if creation_policy_allows(policy, administrator, invited_member) {
            Ok(())
        } else {
            Err(InstanceServiceError::Forbidden)
        }
    }

    pub async fn require_setup_complete(&self) -> Result<(), InstanceServiceError> {
        let setup_complete: bool = sqlx::query_scalar(
            "SELECT setup_completed_at IS NOT NULL FROM instance_settings WHERE singleton=true",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotInitialized)?;
        ensure_instance_setup_complete(setup_complete)
    }

    pub async fn update_organization_creation_policy(
        &self,
        actor: UserId,
        policy: OrganizationCreationPolicy,
    ) -> Result<InstanceStatus, InstanceServiceError> {
        self.require_owner(actor).await?;
        let result = sqlx::query(
            "UPDATE instance_settings SET organization_creation_policy=$2,updated_at=now() \
             WHERE singleton=true AND owner_user_id=$1",
        )
        .bind(actor.0)
        .bind(creation_policy_name(policy))
        .execute(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        if result.rows_affected() != 1 {
            return Err(InstanceServiceError::Forbidden);
        }
        self.status(actor).await
    }

    pub async fn administrators(
        &self,
        actor: UserId,
    ) -> Result<Vec<InstanceAdministratorSummary>, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let rows = sqlx::query(
            "SELECT a.user_id,u.email,a.role,a.granted_by, \
                    (extract(epoch FROM a.created_at)*1000)::bigint created_at_ms \
             FROM instance_administrators a JOIN platform_users u ON u.id=a.user_id \
             ORDER BY CASE a.role WHEN 'owner' THEN 0 ELSE 1 END,a.created_at,a.user_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        rows.iter().map(administrator_from_row).collect()
    }

    pub async fn grant_administrator(
        &self,
        actor: UserId,
        user_id: UserId,
    ) -> Result<InstanceAdministratorSummary, InstanceServiceError> {
        self.require_owner(actor).await?;
        if actor == user_id {
            return Err(InstanceServiceError::InvalidRequest);
        }
        let row = sqlx::query(
            "WITH eligible AS ( \
                SELECT id FROM platform_users WHERE id=$1 AND disabled_at IS NULL \
                  AND email_verified_at IS NOT NULL \
             ), inserted AS ( \
                INSERT INTO instance_administrators (user_id,role,granted_by) \
                SELECT id,'admin',$2 FROM eligible \
                ON CONFLICT (user_id) DO UPDATE SET user_id=excluded.user_id \
                WHERE instance_administrators.role='admin' \
                RETURNING user_id,role,granted_by,created_at \
             ) \
             SELECT i.user_id,u.email,i.role,i.granted_by, \
                    (extract(epoch FROM i.created_at)*1000)::bigint created_at_ms \
             FROM inserted i JOIN platform_users u ON u.id=i.user_id",
        )
        .bind(user_id.0)
        .bind(actor.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotFound)?;
        administrator_from_row(&row)
    }

    pub async fn revoke_administrator(
        &self,
        actor: UserId,
        user_id: UserId,
    ) -> Result<(), InstanceServiceError> {
        self.require_owner(actor).await?;
        if actor == user_id {
            return Err(InstanceServiceError::InvalidRequest);
        }
        let result =
            sqlx::query("DELETE FROM instance_administrators WHERE user_id=$1 AND role='admin'")
                .bind(user_id.0)
                .execute(&self.pool)
                .await
                .map_err(|_| InstanceServiceError::Unavailable)?;
        if result.rows_affected() == 0 {
            return Err(InstanceServiceError::NotFound);
        }
        Ok(())
    }

    pub async fn organizations(
        &self,
        actor: UserId,
        page: PageQuery,
    ) -> Result<InstanceOrganizationPage, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let page = page.validated()?;
        let total: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM organizations")
            .fetch_one(&self.pool)
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        let rows = sqlx::query(
            "SELECT o.id,o.display_name,o.slug,o.disabled_at IS NOT NULL disabled, \
                    (SELECT count(*) FROM organization_memberships m \
                     WHERE m.organization_id=o.id)::bigint member_count, \
                    (SELECT count(*) FROM projects p \
                     WHERE p.organization_id=o.id AND p.lifecycle_state <> 'deleted')::bigint project_count, \
                    EXISTS(SELECT 1 FROM organization_billing_exemptions e \
                           WHERE e.organization_id=o.id) billing_exempt, \
                    (extract(epoch FROM o.created_at)*1000)::bigint created_at_ms \
             FROM organizations o ORDER BY o.created_at,o.id LIMIT $1 OFFSET $2",
        )
        .bind(i64::from(page.limit))
        .bind(i64::try_from(page.offset).map_err(|_| InstanceServiceError::InvalidRequest)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        Ok(InstanceOrganizationPage {
            organizations: rows
                .iter()
                .map(organization_from_row)
                .collect::<Result<_, _>>()?,
            total: u64::try_from(total).map_err(|_| InstanceServiceError::Unavailable)?,
            limit: page.limit,
            offset: page.offset,
        })
    }

    pub async fn users(
        &self,
        actor: UserId,
        page: PageQuery,
    ) -> Result<InstanceUserPage, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let page = page.validated()?;
        let total: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM platform_users")
            .fetch_one(&self.pool)
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        let rows = sqlx::query(
            "SELECT u.id,u.email,u.email_verified_at IS NOT NULL email_verified, \
                    u.disabled_at IS NOT NULL disabled,a.role instance_role, \
                    (SELECT count(*) FROM organization_memberships m \
                     WHERE m.user_id=u.id)::bigint organization_count, \
                    (extract(epoch FROM u.created_at)*1000)::bigint created_at_ms \
             FROM platform_users u LEFT JOIN instance_administrators a ON a.user_id=u.id \
             ORDER BY u.created_at,u.id LIMIT $1 OFFSET $2",
        )
        .bind(i64::from(page.limit))
        .bind(i64::try_from(page.offset).map_err(|_| InstanceServiceError::InvalidRequest)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        Ok(InstanceUserPage {
            users: rows.iter().map(user_from_row).collect::<Result<_, _>>()?,
            total: u64::try_from(total).map_err(|_| InstanceServiceError::Unavailable)?,
            limit: page.limit,
            offset: page.offset,
        })
    }

    pub async fn set_organization_disabled(
        &self,
        actor: UserId,
        organization_id: OrganizationId,
        disabled: bool,
    ) -> Result<InstanceOrganizationSummary, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let row = sqlx::query(
            "WITH changed AS ( \
                UPDATE organizations SET disabled_at=CASE WHEN $2 THEN \
                    COALESCE(disabled_at,now()) ELSE NULL END,updated_at=now() \
                WHERE id=$1 RETURNING id,display_name,slug,disabled_at,created_at \
             ) \
             SELECT o.id,o.display_name,o.slug,o.disabled_at IS NOT NULL disabled, \
                    (SELECT count(*) FROM organization_memberships m \
                     WHERE m.organization_id=o.id)::bigint member_count, \
                    (SELECT count(*) FROM projects p WHERE p.organization_id=o.id \
                     AND p.lifecycle_state <> 'deleted')::bigint project_count, \
                    EXISTS(SELECT 1 FROM organization_billing_exemptions e \
                           WHERE e.organization_id=o.id) billing_exempt, \
                    (extract(epoch FROM o.created_at)*1000)::bigint created_at_ms \
             FROM changed o",
        )
        .bind(organization_id.0)
        .bind(disabled)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotFound)?;
        organization_from_row(&row)
    }

    pub async fn set_user_disabled(
        &self,
        actor: UserId,
        user_id: UserId,
        disabled: bool,
    ) -> Result<InstanceUserSummary, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        let target = sqlx::query(
            "SELECT u.id,a.role FROM platform_users u \
             LEFT JOIN instance_administrators a ON a.user_id=u.id \
             WHERE u.id=$1 FOR UPDATE OF u",
        )
        .bind(user_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotFound)?;
        let target_role = target.try_get::<Option<String>, _>("role")?;
        let self_disable = disabled && actor == user_id;
        let other_enabled_administrators = if self_disable {
            let other_enabled_administrators: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM instance_administrators a \
                 JOIN platform_users u ON u.id=a.user_id \
                 WHERE a.user_id <> $1 AND u.disabled_at IS NULL",
            )
            .bind(user_id.0)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
            other_enabled_administrators
        } else {
            0
        };
        authorize_user_disable(
            disabled,
            target_role.as_deref(),
            self_disable,
            other_enabled_administrators,
        )?;
        let row = sqlx::query(
            "WITH changed AS ( \
                UPDATE platform_users SET disabled_at=CASE WHEN $2 THEN \
                    COALESCE(disabled_at,now()) ELSE NULL END,updated_at=now() \
                WHERE id=$1 RETURNING id,email,email_verified_at,disabled_at,created_at \
             ) \
             SELECT u.id,u.email,u.email_verified_at IS NOT NULL email_verified, \
                    u.disabled_at IS NOT NULL disabled,a.role instance_role, \
                    (SELECT count(*) FROM organization_memberships m \
                     WHERE m.user_id=u.id)::bigint organization_count, \
                    (extract(epoch FROM u.created_at)*1000)::bigint created_at_ms \
             FROM changed u LEFT JOIN instance_administrators a ON a.user_id=u.id",
        )
        .bind(user_id.0)
        .bind(disabled)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?;
        user_from_row(&row)
    }

    pub async fn billing_exemptions(
        &self,
        actor: UserId,
    ) -> Result<Vec<OrganizationBillingExemptionSummary>, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let rows = sqlx::query(
            "SELECT e.organization_id,o.display_name organization_name,e.reason,e.created_by, \
                    u.email created_by_email, \
                    (extract(epoch FROM e.created_at)*1000)::bigint created_at_ms \
             FROM organization_billing_exemptions e \
             JOIN organizations o ON o.id=e.organization_id \
             JOIN platform_users u ON u.id=e.created_by \
             ORDER BY e.created_at,e.organization_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        rows.iter().map(exemption_from_row).collect()
    }

    pub async fn grant_billing_exemption(
        &self,
        actor: UserId,
        organization_id: OrganizationId,
        reason: &str,
    ) -> Result<OrganizationBillingExemptionSummary, InstanceServiceError> {
        self.require_administrator(actor).await?;
        let reason = validate_reason(reason)?;
        let row = sqlx::query(
            "WITH upserted AS ( \
                INSERT INTO organization_billing_exemptions \
                    (organization_id,reason,created_by) \
                SELECT id,$2,$3 FROM organizations WHERE id=$1 \
                ON CONFLICT (organization_id) DO UPDATE SET \
                    reason=excluded.reason,created_by=excluded.created_by,created_at=now() \
                RETURNING organization_id,reason,created_by,created_at \
             ) \
             SELECT e.organization_id,o.display_name organization_name,e.reason,e.created_by, \
                    u.email created_by_email, \
                    (extract(epoch FROM e.created_at)*1000)::bigint created_at_ms \
             FROM upserted e JOIN organizations o ON o.id=e.organization_id \
             JOIN platform_users u ON u.id=e.created_by",
        )
        .bind(organization_id.0)
        .bind(reason)
        .bind(actor.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotFound)?;
        exemption_from_row(&row)
    }

    pub async fn revoke_billing_exemption(
        &self,
        actor: UserId,
        organization_id: OrganizationId,
    ) -> Result<(), InstanceServiceError> {
        self.require_administrator(actor).await?;
        let result =
            sqlx::query("DELETE FROM organization_billing_exemptions WHERE organization_id=$1")
                .bind(organization_id.0)
                .execute(&self.pool)
                .await
                .map_err(|_| InstanceServiceError::Unavailable)?;
        if result.rows_affected() == 0 {
            return Err(InstanceServiceError::NotFound);
        }
        Ok(())
    }

    pub async fn plans(
        &self,
        actor: UserId,
    ) -> Result<Vec<InstancePlanCatalogEntry>, InstanceServiceError> {
        self.require_administrator(actor).await?;
        // The base statement and ordering suffix are compile-time SQL with no
        // request data; AssertSqlSafe records that audit for SQLx 0.9.
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{} ORDER BY CASE tier WHEN 'free' THEN 0 WHEN 'pay_as_you_go' THEN 1 ELSE 2 END",
            plan_select()
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let provider_catalog_bound = self.provider_catalog_bound().await?;
        rows.iter()
            .map(|row| plan_from_row(row, provider_catalog_bound))
            .collect()
    }

    pub async fn put_plan(
        &self,
        actor: UserId,
        tier: PlatformBillingTier,
        input: &PutInstancePlanCatalogEntryRequest,
    ) -> Result<InstancePlanCatalogEntry, InstanceServiceError> {
        self.require_administrator(actor).await?;
        validate_plan(tier, input)?;
        let provider_catalog_bound = self.provider_catalog_bound().await?;
        if provider_catalog_bound && tier != PlatformBillingTier::Free {
            // plan_select() and this predicate are both compile-time SQL. The
            // tier remains a bind parameter below.
            let current = sqlx::query(sqlx::AssertSqlSafe(format!(
                "{} WHERE tier=$1",
                plan_select()
            )))
            .bind(tier_name(tier))
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?
            .ok_or(InstanceServiceError::NotFound)?;
            let current = plan_from_row(&current, true)?;
            if !provider_pricing_fields_match(&current, input) {
                return Err(InstanceServiceError::ProviderCatalogConflict);
            }
        }
        if input.active
            && let Some(auth) = self.current_billing_request_auth().await?
        {
            let account_id: String = sqlx::query_scalar(
                "SELECT provider_account_id FROM instance_billing_accounts WHERE singleton=true",
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| InstanceServiceError::Unavailable)?
            .flatten()
            .ok_or(InstanceServiceError::InvalidConfiguration)?;
            let catalog = self.load_provider_catalog(&account_id).await?;
            let plan = StripeProvisioningPlan {
                tier,
                currency: input.currency.clone(),
                base_price_cents: input.base_price_cents,
                storage_bytes: input.storage_bytes,
                monthly_reads: input.monthly_reads,
                monthly_writes: input.monthly_writes,
                monthly_active_users: input.monthly_active_users,
            };
            self.validate_provider_plan(&auth, &catalog, &plan).await?;
        }
        if !input.active {
            self.ensure_plan_can_retire(tier).await?;
        }
        let row = sqlx::query(
            "INSERT INTO billing_price_catalog \
                (tier,display_name,billing_unit,base_price_cents,currency,project_limit, \
                 storage_bytes,monthly_reads,monthly_writes,monthly_active_users,overage_enabled, \
                 reads_at_limit,writes_at_limit,signups_at_limit, \
                 requires_payment_method_for_overage,active,updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,now()) \
             ON CONFLICT (tier) DO UPDATE SET display_name=excluded.display_name, \
                 billing_unit=excluded.billing_unit,base_price_cents=excluded.base_price_cents, \
                 currency=excluded.currency,project_limit=excluded.project_limit, \
                 storage_bytes=excluded.storage_bytes,monthly_reads=excluded.monthly_reads, \
                 monthly_writes=excluded.monthly_writes, \
                 monthly_active_users=excluded.monthly_active_users, \
                 overage_enabled=excluded.overage_enabled,reads_at_limit=excluded.reads_at_limit, \
                 writes_at_limit=excluded.writes_at_limit,signups_at_limit=excluded.signups_at_limit, \
                 requires_payment_method_for_overage=excluded.requires_payment_method_for_overage, \
                 active=excluded.active,updated_at=now() \
             RETURNING tier,display_name,billing_unit,base_price_cents,currency,project_limit, \
                 storage_bytes,monthly_reads,monthly_writes,monthly_active_users,overage_enabled, \
                 reads_at_limit,writes_at_limit,signups_at_limit, \
                 requires_payment_method_for_overage,active, \
                 (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms",
        )
        .bind(tier_name(tier))
        .bind(input.display_name.trim())
        .bind(billing_unit_name(input.billing_unit))
        .bind(input.base_price_cents.map(i64::try_from).transpose().map_err(|_| InstanceServiceError::InvalidRequest)?)
        .bind(input.currency.as_str())
        .bind(input.project_limit.map(i32::try_from).transpose().map_err(|_| InstanceServiceError::InvalidRequest)?)
        .bind(i64::try_from(input.storage_bytes).map_err(|_| InstanceServiceError::InvalidRequest)?)
        .bind(i64::try_from(input.monthly_reads).map_err(|_| InstanceServiceError::InvalidRequest)?)
        .bind(i64::try_from(input.monthly_writes).map_err(|_| InstanceServiceError::InvalidRequest)?)
        .bind(i64::try_from(input.monthly_active_users).map_err(|_| InstanceServiceError::InvalidRequest)?)
        .bind(input.overage_enabled)
        .bind(reads_at_limit_name(input.reads_at_limit))
        .bind(writes_at_limit_name(input.writes_at_limit))
        .bind(signups_at_limit_name(input.signups_at_limit))
        .bind(input.requires_payment_method_for_overage)
        .bind(input.active)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        plan_from_row(&row, provider_catalog_bound)
    }

    pub async fn retire_plan(
        &self,
        actor: UserId,
        tier: PlatformBillingTier,
    ) -> Result<InstancePlanCatalogEntry, InstanceServiceError> {
        self.require_administrator(actor).await?;
        self.ensure_plan_can_retire(tier).await?;
        let row = sqlx::query(
            "UPDATE billing_price_catalog SET active=false,updated_at=now() WHERE tier=$1 \
             RETURNING tier,display_name,billing_unit,base_price_cents,currency,project_limit, \
                 storage_bytes,monthly_reads,monthly_writes,monthly_active_users,overage_enabled, \
                 reads_at_limit,writes_at_limit,signups_at_limit, \
                 requires_payment_method_for_overage,active, \
                 (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms",
        )
        .bind(tier_name(tier))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?
        .ok_or(InstanceServiceError::NotFound)?;
        plan_from_row(&row, self.provider_catalog_bound().await?)
    }

    async fn ensure_plan_can_retire(
        &self,
        tier: PlatformBillingTier,
    ) -> Result<(), InstanceServiceError> {
        if tier == PlatformBillingTier::Free {
            return Err(InstanceServiceError::Conflict);
        }
        let in_use: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM organization_billing_accounts \
             WHERE tier=$1 AND status <> 'canceled')",
        )
        .bind(tier_name(tier))
        .fetch_one(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        if in_use {
            Err(InstanceServiceError::Conflict)
        } else {
            Ok(())
        }
    }

    async fn require_administrator(
        &self,
        actor: UserId,
    ) -> Result<InstanceAdministratorRole, InstanceServiceError> {
        let role: Option<String> = sqlx::query_scalar(
            "SELECT a.role FROM instance_administrators a JOIN platform_users u ON u.id=a.user_id \
             WHERE a.user_id=$1 AND u.disabled_at IS NULL",
        )
        .bind(actor.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let role = role.as_deref().map(parse_administrator_role).transpose()?;
        authorize_instance_role(role, false)
    }

    pub(crate) async fn authorize_observability(
        &self,
        actor: UserId,
    ) -> Result<(), InstanceServiceError> {
        self.require_administrator(actor).await.map(|_| ())
    }

    async fn require_owner(&self, actor: UserId) -> Result<(), InstanceServiceError> {
        if self.require_administrator(actor).await? == InstanceAdministratorRole::Owner {
            Ok(())
        } else {
            Err(InstanceServiceError::Forbidden)
        }
    }

    async fn billing_account_summary(
        &self,
    ) -> Result<Option<InstanceBillingAccountSummary>, InstanceServiceError> {
        let row = sqlx::query(
            "SELECT a.mode,a.provider_account_id,a.status,a.charges_enabled,a.payouts_enabled, \
                    a.details_submitted,a.capabilities, \
                    EXISTS(SELECT 1 FROM instance_billing_secrets) credentials_configured, \
                    (SELECT count(*)=2 FROM instance_billing_secrets \
                     WHERE secret_kind IN ('stripe_connect_secret_key', \
                                           'stripe_connect_webhook_secret')) \
                        connect_credentials_configured, \
                    (extract(epoch FROM a.updated_at)*1000)::bigint updated_at_ms \
             FROM instance_billing_accounts a WHERE a.singleton=true",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let mut summary = row.as_ref().map(billing_account_from_row).transpose()?;
        if let Some(account) = &mut summary
            && account.mode == InstanceBillingMode::StripeConnect
        {
            let persisted: bool = row
                .as_ref()
                .ok_or(InstanceServiceError::Unavailable)?
                .try_get("connect_credentials_configured")?;
            account.credentials_configured = (persisted
                || (self.connect_auth.is_some() && self.connect_webhook_secret.is_some()))
                && self.billing_provider.template.is_some();
        }
        Ok(summary)
    }

    async fn current_billing_request_auth(
        &self,
    ) -> Result<Option<StripeRequestAuth>, InstanceServiceError> {
        let row = sqlx::query(
            "SELECT s.owner_user_id,s.deployment_mode,a.provider_account_id \
             FROM instance_settings s LEFT JOIN instance_billing_accounts a ON a.singleton=true \
             WHERE s.singleton=true",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let owner = UserId(row.try_get("owner_user_id")?);
        match parse_deployment_mode(row.try_get::<String, _>("deployment_mode")?.as_str())? {
            InstanceDeploymentMode::Unconfigured
            | InstanceDeploymentMode::Private
            | InstanceDeploymentMode::Team => Ok(None),
            InstanceDeploymentMode::PlatformByo => Ok(Some(StripeRequestAuth {
                secret_key: self
                    .load_instance_secret(owner, "stripe_secret_key")
                    .await?,
                connected_account: None,
            })),
            InstanceDeploymentMode::PlatformConnect => {
                let (mut auth, _) = self.connect_credentials(owner).await?;
                auth.connected_account = Some(
                    row.try_get::<Option<String>, _>("provider_account_id")?
                        .filter(|value| valid_provider_id(value, "acct_"))
                        .ok_or(InstanceServiceError::InvalidConfiguration)?,
                );
                Ok(Some(auth))
            }
        }
    }

    async fn provider_catalog_bound(&self) -> Result<bool, InstanceServiceError> {
        sqlx::query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM instance_settings s \
                JOIN instance_billing_accounts a ON a.singleton=true \
                JOIN instance_billing_catalog c ON c.singleton=true \
                WHERE s.singleton=true AND s.deployment_mode IN ('platform_byo','platform_connect') \
                  AND a.status='enabled' AND c.provider_account_id=a.provider_account_id \
             )",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)
    }

    async fn load_provider_catalog(
        &self,
        account_id: &str,
    ) -> Result<InstanceStripeProviderCatalog, InstanceServiceError> {
        self.load_provider_catalog_optional(account_id)
            .await?
            .ok_or(InstanceServiceError::InvalidConfiguration)
    }

    async fn load_provider_catalog_optional(
        &self,
        account_id: &str,
    ) -> Result<Option<InstanceStripeProviderCatalog>, InstanceServiceError> {
        if !valid_provider_id(account_id, "acct_") {
            return Err(InstanceServiceError::InvalidConfiguration);
        }
        let row = sqlx::query(
            "SELECT provider_account_id,product_id,pro_base_price_id FROM instance_billing_catalog \
             WHERE singleton=true AND provider_account_id=$1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        validate_catalog_account(
            account_id,
            row.try_get::<String, _>("provider_account_id")?.as_str(),
        )?;
        let usage_rows = sqlx::query(
            "SELECT metric,event_name,provider_meter_id,payg_price_id,pro_price_id \
             FROM instance_billing_usage_catalog WHERE provider_account_id=$1 ORDER BY metric",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let mut usage_meters = Vec::with_capacity(UsageMetric::ALL.len());
        for usage in usage_rows {
            usage_meters.push(StripeUsageMeterConfig {
                metric: parse_usage_metric(usage.try_get::<String, _>("metric")?.as_str())?,
                event_name: usage.try_get("event_name")?,
                meter_id: usage.try_get("provider_meter_id")?,
                payg_price_id: usage.try_get("payg_price_id")?,
                pro_price_id: usage.try_get("pro_price_id")?,
            });
        }
        if usage_meters.len() != UsageMetric::ALL.len()
            || UsageMetric::ALL
                .iter()
                .any(|metric| !usage_meters.iter().any(|meter| meter.metric == *metric))
        {
            return Err(InstanceServiceError::InvalidConfiguration);
        }
        Ok(Some(InstanceStripeProviderCatalog {
            product_id: row.try_get("product_id")?,
            pro_base_price_id: row.try_get("pro_base_price_id")?,
            usage_meters,
        }))
    }

    async fn validate_all_provider_prices(
        &self,
        auth: &StripeRequestAuth,
        catalog: &InstanceStripeProviderCatalog,
    ) -> Result<(), InstanceServiceError> {
        let rows = sqlx::query(
            "SELECT tier,currency,base_price_cents,storage_bytes,monthly_reads,monthly_writes, \
                    monthly_active_users \
             FROM billing_price_catalog \
             WHERE active=true AND tier IN ('pay_as_you_go','pro') ORDER BY tier",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        for row in rows {
            let plan = provisioning_plan_from_row(&row)?;
            self.validate_provider_plan(auth, catalog, &plan).await?;
        }
        Ok(())
    }

    async fn validate_provider_plan(
        &self,
        auth: &StripeRequestAuth,
        catalog: &InstanceStripeProviderCatalog,
        plan: &StripeProvisioningPlan,
    ) -> Result<(), InstanceServiceError> {
        if plan.tier == PlatformBillingTier::Free {
            return Ok(());
        }
        if plan.tier == PlatformBillingTier::Pro {
            let price = self.stripe_price(auth, &catalog.pro_base_price_id).await?;
            validate_stripe_price(
                &price,
                &catalog.pro_base_price_id,
                &plan.currency,
                plan.base_price_cents,
                None,
            )?;
        }
        for meter in &catalog.usage_meters {
            let price_id = match plan.tier {
                PlatformBillingTier::PayAsYouGo => &meter.payg_price_id,
                PlatformBillingTier::Pro => &meter.pro_price_id,
                PlatformBillingTier::Free => continue,
            };
            let price = self.stripe_price(auth, price_id).await?;
            validate_stripe_price(
                &price,
                price_id,
                &plan.currency,
                None,
                Some(&meter.meter_id),
            )?;
            validate_stripe_price_tiers(&price, &stripe_price_tiers(meter.metric, plan)?)?;
        }
        Ok(())
    }

    async fn stripe_price(
        &self,
        auth: &StripeRequestAuth,
        price_id: &str,
    ) -> Result<Value, InstanceServiceError> {
        if !valid_provider_id(price_id, "price_") {
            return Err(InstanceServiceError::InvalidConfiguration);
        }
        self.stripe
            .form(
                auth,
                reqwest::Method::GET,
                &format!("v1/prices/{price_id}?expand%5B%5D=tiers"),
                &[],
                None,
            )
            .await
            .map_err(map_commerce_error)
    }
}

fn authorize_instance_role(
    role: Option<InstanceAdministratorRole>,
    owner_only: bool,
) -> Result<InstanceAdministratorRole, InstanceServiceError> {
    match role {
        Some(InstanceAdministratorRole::Owner) => Ok(InstanceAdministratorRole::Owner),
        Some(InstanceAdministratorRole::Admin) if !owner_only => {
            Ok(InstanceAdministratorRole::Admin)
        }
        Some(InstanceAdministratorRole::Admin) | None => Err(InstanceServiceError::Forbidden),
    }
}

fn authorize_user_disable(
    disabled: bool,
    target_role: Option<&str>,
    self_disable: bool,
    other_enabled_administrators: i64,
) -> Result<(), InstanceServiceError> {
    if !disabled {
        return Ok(());
    }
    if target_role == Some("owner") {
        return Err(InstanceServiceError::Forbidden);
    }
    if self_disable && other_enabled_administrators == 0 {
        return Err(InstanceServiceError::Conflict);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstanceServiceError {
    InvalidRequest,
    Forbidden,
    NotFound,
    Conflict,
    BillingInUse,
    ProviderCatalogConflict,
    NotInitialized,
    SetupRequired,
    ProviderUnavailable,
    ProviderRejected,
    InvalidConfiguration,
    Unavailable,
}

impl From<sqlx::Error> for InstanceServiceError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default = "default_page_size")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u64,
}

impl PageQuery {
    fn validated(self) -> Result<Self, InstanceServiceError> {
        if self.limit == 0 || self.limit > MAX_PAGE_SIZE || self.offset > i64::MAX as u64 {
            return Err(InstanceServiceError::InvalidRequest);
        }
        Ok(self)
    }
}

const fn default_page_size() -> u32 {
    DEFAULT_PAGE_SIZE
}

fn instance_status_from_row(
    row: &sqlx::postgres::PgRow,
    billing_account: Option<InstanceBillingAccountSummary>,
) -> Result<InstanceStatus, InstanceServiceError> {
    Ok(InstanceStatus {
        owner_user_id: UserId(row.try_get("owner_user_id")?),
        current_user_role: parse_administrator_role(
            row.try_get::<String, _>("current_user_role")?.as_str(),
        )?,
        deployment_mode: parse_deployment_mode(
            row.try_get::<String, _>("deployment_mode")?.as_str(),
        )?,
        organization_creation_policy: parse_creation_policy(
            row.try_get::<String, _>("organization_creation_policy")?
                .as_str(),
        )?,
        billing_enforcement_enabled: row.try_get("billing_enforcement_enabled")?,
        setup_completed_at_ms: row.try_get("setup_completed_at_ms")?,
        billing_account,
        administrator_count: u32::try_from(row.try_get::<i64, _>("administrator_count")?)
            .map_err(|_| InstanceServiceError::Unavailable)?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

fn administrator_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<InstanceAdministratorSummary, InstanceServiceError> {
    Ok(InstanceAdministratorSummary {
        user_id: UserId(row.try_get("user_id")?),
        email: row.try_get("email")?,
        role: parse_administrator_role(row.try_get::<String, _>("role")?.as_str())?,
        granted_by: row.try_get::<Option<Uuid>, _>("granted_by")?.map(UserId),
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

fn organization_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<InstanceOrganizationSummary, InstanceServiceError> {
    Ok(InstanceOrganizationSummary {
        id: OrganizationId(row.try_get("id")?),
        name: row.try_get("display_name")?,
        slug: row.try_get("slug")?,
        disabled: row.try_get("disabled")?,
        member_count: u64::try_from(row.try_get::<i64, _>("member_count")?)
            .map_err(|_| InstanceServiceError::Unavailable)?,
        project_count: u64::try_from(row.try_get::<i64, _>("project_count")?)
            .map_err(|_| InstanceServiceError::Unavailable)?,
        billing_exempt: row.try_get("billing_exempt")?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

fn user_from_row(row: &sqlx::postgres::PgRow) -> Result<InstanceUserSummary, InstanceServiceError> {
    Ok(InstanceUserSummary {
        id: UserId(row.try_get("id")?),
        email: row.try_get("email")?,
        email_verified: row.try_get("email_verified")?,
        disabled: row.try_get("disabled")?,
        instance_role: row
            .try_get::<Option<String>, _>("instance_role")?
            .as_deref()
            .map(parse_administrator_role)
            .transpose()?,
        organization_count: u64::try_from(row.try_get::<i64, _>("organization_count")?)
            .map_err(|_| InstanceServiceError::Unavailable)?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

fn exemption_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<OrganizationBillingExemptionSummary, InstanceServiceError> {
    Ok(OrganizationBillingExemptionSummary {
        organization_id: OrganizationId(row.try_get("organization_id")?),
        organization_name: row.try_get("organization_name")?,
        reason: row.try_get("reason")?,
        created_by: UserId(row.try_get("created_by")?),
        created_by_email: row.try_get("created_by_email")?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

fn billing_account_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<InstanceBillingAccountSummary, InstanceServiceError> {
    Ok(InstanceBillingAccountSummary {
        mode: parse_billing_mode(row.try_get::<String, _>("mode")?.as_str())?,
        status: parse_billing_status(row.try_get::<String, _>("status")?.as_str())?,
        provider_account_id: row.try_get("provider_account_id")?,
        charges_enabled: row.try_get("charges_enabled")?,
        payouts_enabled: row.try_get("payouts_enabled")?,
        details_submitted: row.try_get("details_submitted")?,
        capabilities: row.try_get("capabilities")?,
        credentials_configured: row.try_get("credentials_configured")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

fn plan_from_row(
    row: &sqlx::postgres::PgRow,
    provider_catalog_bound: bool,
) -> Result<InstancePlanCatalogEntry, InstanceServiceError> {
    let tier = parse_tier(row.try_get::<String, _>("tier")?.as_str())?;
    Ok(InstancePlanCatalogEntry {
        tier,
        display_name: row.try_get("display_name")?,
        billing_unit: parse_billing_unit(row.try_get::<String, _>("billing_unit")?.as_str())?,
        base_price_cents: row
            .try_get::<Option<i32>, _>("base_price_cents")?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| InstanceServiceError::Unavailable)?,
        currency: row.try_get("currency")?,
        project_limit: row
            .try_get::<Option<i32>, _>("project_limit")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| InstanceServiceError::Unavailable)?,
        storage_bytes: positive_i64(row, "storage_bytes")?,
        monthly_reads: positive_i64(row, "monthly_reads")?,
        monthly_writes: positive_i64(row, "monthly_writes")?,
        monthly_active_users: positive_i64(row, "monthly_active_users")?,
        overage_enabled: row.try_get("overage_enabled")?,
        reads_at_limit: parse_reads_at_limit(row.try_get::<String, _>("reads_at_limit")?.as_str())?,
        writes_at_limit: parse_writes_at_limit(
            row.try_get::<String, _>("writes_at_limit")?.as_str(),
        )?,
        signups_at_limit: parse_signups_at_limit(
            row.try_get::<String, _>("signups_at_limit")?.as_str(),
        )?,
        requires_payment_method_for_overage: row.try_get("requires_payment_method_for_overage")?,
        active: row.try_get("active")?,
        provider_catalog_bound: provider_catalog_bound && tier != PlatformBillingTier::Free,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

fn positive_i64(row: &sqlx::postgres::PgRow, field: &str) -> Result<u64, InstanceServiceError> {
    u64::try_from(row.try_get::<i64, _>(field)?).map_err(|_| InstanceServiceError::Unavailable)
}

fn validate_reason(value: &str) -> Result<&str, InstanceServiceError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 500 {
        return Err(InstanceServiceError::InvalidRequest);
    }
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderAccountState {
    id: String,
    status: &'static str,
    charges_enabled: bool,
    payouts_enabled: bool,
    details_submitted: bool,
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StripeProvisioningPlan {
    tier: PlatformBillingTier,
    currency: String,
    base_price_cents: Option<u64>,
    storage_bytes: u64,
    monthly_reads: u64,
    monthly_writes: u64,
    monthly_active_users: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StripePriceTier {
    up_to: Option<u64>,
    unit_amount_decimal: &'static str,
}

fn provisioning_plan_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<StripeProvisioningPlan, InstanceServiceError> {
    Ok(StripeProvisioningPlan {
        tier: parse_tier(row.try_get::<String, _>("tier")?.as_str())?,
        currency: row.try_get("currency")?,
        base_price_cents: row
            .try_get::<Option<i32>, _>("base_price_cents")?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| InstanceServiceError::InvalidConfiguration)?,
        storage_bytes: positive_i64(row, "storage_bytes")?,
        monthly_reads: positive_i64(row, "monthly_reads")?,
        monthly_writes: positive_i64(row, "monthly_writes")?,
        monthly_active_users: positive_i64(row, "monthly_active_users")?,
    })
}

fn validate_usage_events(
    usage_events: &[InstanceStripeUsageEventConfig],
) -> Result<(), InstanceServiceError> {
    if usage_events.len() != UsageMetric::ALL.len()
        || UsageMetric::ALL
            .iter()
            .any(|metric| !usage_events.iter().any(|event| event.metric == *metric))
        || usage_events.iter().any(|event| {
            !(3..=100).contains(&event.event_name.len())
                || !event
                    .event_name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        return Err(InstanceServiceError::InvalidConfiguration);
    }
    let mut names = usage_events
        .iter()
        .map(|event| event.event_name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(InstanceServiceError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_provider_catalog_shape(
    catalog: &InstanceStripeProviderCatalog,
) -> Result<(), InstanceServiceError> {
    if catalog
        .product_id
        .as_deref()
        .is_some_and(|value| !valid_provider_id(value, "prod_"))
        || !valid_provider_id(&catalog.pro_base_price_id, "price_")
        || catalog.usage_meters.len() != UsageMetric::ALL.len()
        || UsageMetric::ALL.iter().any(|metric| {
            !catalog
                .usage_meters
                .iter()
                .any(|meter| meter.metric == *metric)
        })
        || catalog.usage_meters.iter().any(|meter| {
            !valid_provider_id(&meter.meter_id, "mtr_")
                || !valid_provider_id(&meter.payg_price_id, "price_")
                || !valid_provider_id(&meter.pro_price_id, "price_")
        })
    {
        Err(InstanceServiceError::InvalidConfiguration)
    } else {
        validate_usage_events(
            &catalog
                .usage_meters
                .iter()
                .map(|meter| InstanceStripeUsageEventConfig {
                    metric: meter.metric,
                    event_name: meter.event_name.clone(),
                })
                .collect::<Vec<_>>(),
        )
    }
}

fn provider_payload_id(payload: &Value, prefix: &str) -> Result<String, InstanceServiceError> {
    payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_provider_id(value, prefix))
        .map(str::to_owned)
        .ok_or(InstanceServiceError::ProviderUnavailable)
}

fn validate_catalog_account(
    requested_account_id: &str,
    stored_account_id: &str,
) -> Result<(), InstanceServiceError> {
    if requested_account_id == stored_account_id && valid_provider_id(requested_account_id, "acct_")
    {
        Ok(())
    } else {
        Err(InstanceServiceError::InvalidConfiguration)
    }
}

fn catalog_idempotency_key(
    account_id: &str,
    resource: &str,
) -> Result<String, InstanceServiceError> {
    let key = format!("ffdb-catalog-v{STRIPE_CATALOG_VERSION}:{account_id}:{resource}");
    if (8..=255).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        Ok(key)
    } else {
        Err(InstanceServiceError::InvalidConfiguration)
    }
}

fn stripe_product_form() -> Vec<(String, String)> {
    vec![
        ("name".into(), "FFDB Platform".into()),
        (
            "description".into(),
            "FFDB organization plans and measured usage".into(),
        ),
        (
            "metadata[ffdb_catalog_version]".into(),
            STRIPE_CATALOG_VERSION.to_string(),
        ),
    ]
}

fn stripe_base_price_form(
    product_id: &str,
    plan: &StripeProvisioningPlan,
) -> Result<Vec<(String, String)>, InstanceServiceError> {
    let amount = plan
        .base_price_cents
        .filter(|amount| *amount > 0)
        .ok_or(InstanceServiceError::InvalidConfiguration)?;
    Ok(vec![
        ("product".into(), product_id.into()),
        ("currency".into(), plan.currency.clone()),
        ("unit_amount".into(), amount.to_string()),
        ("recurring[interval]".into(), "month".into()),
        ("nickname".into(), "FFDB Pro base".into()),
        ("metadata[ffdb_tier]".into(), "pro".into()),
        (
            "metadata[ffdb_catalog_version]".into(),
            STRIPE_CATALOG_VERSION.to_string(),
        ),
    ])
}

fn stripe_meter_form(event: &InstanceStripeUsageEventConfig) -> Vec<(String, String)> {
    vec![
        (
            "display_name".into(),
            format!("FFDB {}", event.metric.name().replace('_', " ")),
        ),
        ("event_name".into(), event.event_name.clone()),
        ("default_aggregation[formula]".into(), "sum".into()),
        ("customer_mapping[type]".into(), "by_id".into()),
        (
            "customer_mapping[event_payload_key]".into(),
            "stripe_customer_id".into(),
        ),
        ("value_settings[event_payload_key]".into(), "value".into()),
    ]
}

fn stripe_usage_price_form(
    product_id: &str,
    meter_id: &str,
    metric: UsageMetric,
    plan: &StripeProvisioningPlan,
) -> Result<Vec<(String, String)>, InstanceServiceError> {
    let tiers = stripe_price_tiers(metric, plan)?;
    let mut form = vec![
        ("product".into(), product_id.into()),
        ("currency".into(), plan.currency.clone()),
        ("recurring[interval]".into(), "month".into()),
        ("recurring[usage_type]".into(), "metered".into()),
        ("recurring[meter]".into(), meter_id.into()),
        ("billing_scheme".into(), "tiered".into()),
        ("tiers_mode".into(), "graduated".into()),
        (
            "nickname".into(),
            format!("FFDB {} {} usage", tier_name(plan.tier), metric.name()),
        ),
        ("metadata[ffdb_tier]".into(), tier_name(plan.tier).into()),
        ("metadata[ffdb_metric]".into(), metric.name().into()),
        (
            "metadata[ffdb_catalog_version]".into(),
            STRIPE_CATALOG_VERSION.to_string(),
        ),
    ];
    for (index, tier) in tiers.iter().enumerate() {
        form.push((
            format!("tiers[{index}][up_to]"),
            tier.up_to
                .map_or_else(|| "inf".into(), |value| value.to_string()),
        ));
        form.push((
            format!("tiers[{index}][unit_amount_decimal]"),
            tier.unit_amount_decimal.into(),
        ));
    }
    Ok(form)
}

fn stripe_price_tiers(
    metric: UsageMetric,
    plan: &StripeProvisioningPlan,
) -> Result<Vec<StripePriceTier>, InstanceServiceError> {
    let (included, breakpoint, first_rate, later_rate) = match metric {
        UsageMetric::Reads => (plan.monthly_reads, None, "0.000025", "0.000025"),
        UsageMetric::Writes => (plan.monthly_writes, Some(1_000_000), "0.00015", "0.000225"),
        UsageMetric::StorageByteHours => (
            plan.storage_bytes
                .checked_div(STORAGE_BILLING_UNIT_BYTES)
                .ok_or(InstanceServiceError::InvalidConfiguration)?
                .checked_mul(730)
                .ok_or(InstanceServiceError::InvalidConfiguration)?,
            None,
            "0.000000027397",
            "0.000000027397",
        ),
        UsageMetric::MonthlyActiveUsers => (plan.monthly_active_users, Some(50_000), "0.5", "1.5"),
    };
    let mut tiers = vec![StripePriceTier {
        up_to: Some(included),
        unit_amount_decimal: "0",
    }];
    if let Some(breakpoint) = breakpoint
        && included < breakpoint
    {
        tiers.push(StripePriceTier {
            up_to: Some(breakpoint),
            unit_amount_decimal: first_rate,
        });
    }
    tiers.push(StripePriceTier {
        up_to: None,
        unit_amount_decimal: if breakpoint.is_some() {
            later_rate
        } else {
            first_rate
        },
    });
    Ok(tiers)
}

async fn persist_instance_secret(
    transaction: &mut Transaction<'_, Postgres>,
    actor: UserId,
    kind: &str,
    sealed: &SealedProviderSecret,
) -> Result<(), InstanceServiceError> {
    sqlx::query(
        "INSERT INTO instance_billing_secrets \
            (secret_kind,key_version,nonce,ciphertext,updated_by) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (secret_kind) DO UPDATE SET key_version=excluded.key_version, \
            nonce=excluded.nonce,ciphertext=excluded.ciphertext, \
            updated_by=excluded.updated_by,updated_at=now()",
    )
    .bind(kind)
    .bind(sealed.key_version)
    .bind(sealed.nonce.as_slice())
    .bind(&sealed.ciphertext)
    .bind(actor.0)
    .execute(&mut **transaction)
    .await
    .map_err(|_| InstanceServiceError::Unavailable)?;
    Ok(())
}

async fn clear_instance_billing(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), InstanceServiceError> {
    clear_provider_catalog(transaction).await?;
    sqlx::query("DELETE FROM instance_billing_secrets")
        .execute(&mut **transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
    sqlx::query("DELETE FROM instance_billing_accounts WHERE singleton=true")
        .execute(&mut **transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
    Ok(())
}

async fn clear_provider_catalog(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), InstanceServiceError> {
    sqlx::query("DELETE FROM instance_billing_usage_catalog")
        .execute(&mut **transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
    sqlx::query("DELETE FROM instance_billing_catalog WHERE singleton=true")
        .execute(&mut **transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
    sqlx::query(
        "UPDATE billing_usage_catalog SET provider_meter_id=NULL,payg_price_id=NULL, \
         pro_price_id=NULL,updated_at=now()",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|_| InstanceServiceError::Unavailable)?;
    Ok(())
}

async fn persist_provider_catalog(
    transaction: &mut Transaction<'_, Postgres>,
    account_id: &str,
    catalog: &InstanceStripeProviderCatalog,
) -> Result<(), InstanceServiceError> {
    if !valid_provider_id(account_id, "acct_") {
        return Err(InstanceServiceError::InvalidConfiguration);
    }
    validate_provider_catalog_shape(catalog)?;
    clear_provider_catalog(transaction).await?;
    sqlx::query(
        "INSERT INTO instance_billing_catalog \
            (singleton,provider_account_id,product_id,pro_base_price_id,catalog_version) \
         VALUES (true,$1,$2,$3,$4)",
    )
    .bind(account_id)
    .bind(&catalog.product_id)
    .bind(&catalog.pro_base_price_id)
    .bind(STRIPE_CATALOG_VERSION)
    .execute(&mut **transaction)
    .await
    .map_err(|_| InstanceServiceError::Unavailable)?;
    for meter in &catalog.usage_meters {
        sqlx::query(
            "INSERT INTO instance_billing_usage_catalog \
                (metric,provider_account_id,event_name,provider_meter_id,payg_price_id, \
                 pro_price_id,catalog_version) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(meter.metric.name())
        .bind(account_id)
        .bind(&meter.event_name)
        .bind(&meter.meter_id)
        .bind(&meter.payg_price_id)
        .bind(&meter.pro_price_id)
        .bind(STRIPE_CATALOG_VERSION)
        .execute(&mut **transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        let updated = sqlx::query(
            "UPDATE billing_usage_catalog SET event_name=$2,provider_meter_id=$3, \
             payg_price_id=$4,pro_price_id=$5,active=true,updated_at=now() WHERE metric=$1",
        )
        .bind(meter.metric.name())
        .bind(&meter.event_name)
        .bind(&meter.meter_id)
        .bind(&meter.payg_price_id)
        .bind(&meter.pro_price_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| InstanceServiceError::Unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(InstanceServiceError::InvalidConfiguration);
        }
    }
    Ok(())
}

async fn persist_billing_account(
    transaction: &mut Transaction<'_, Postgres>,
    actor: UserId,
    mode: &str,
    account: &ProviderAccountState,
) -> Result<(), InstanceServiceError> {
    sqlx::query(
        "INSERT INTO instance_billing_accounts \
            (singleton,provider,mode,provider_account_id,status,charges_enabled, \
             payouts_enabled,details_submitted,capabilities,updated_by) \
         VALUES (true,'stripe',$1,$2,$3,$4,$5,$6,$7,$8) \
         ON CONFLICT (singleton) DO UPDATE SET mode=excluded.mode, \
             provider_account_id=excluded.provider_account_id,status=excluded.status, \
             charges_enabled=excluded.charges_enabled,payouts_enabled=excluded.payouts_enabled, \
             details_submitted=excluded.details_submitted,capabilities=excluded.capabilities, \
             updated_by=excluded.updated_by,updated_at=now()",
    )
    .bind(mode)
    .bind(&account.id)
    .bind(account.status)
    .bind(account.charges_enabled)
    .bind(account.payouts_enabled)
    .bind(account.details_submitted)
    .bind(&account.capabilities)
    .bind(actor.0)
    .execute(&mut **transaction)
    .await
    .map_err(|_| InstanceServiceError::Unavailable)?;
    Ok(())
}

fn setup_mode_and_policy(
    request: &CompleteInstanceSetupRequest,
) -> (InstanceDeploymentMode, OrganizationCreationPolicy) {
    match request {
        CompleteInstanceSetupRequest::Private {
            organization_creation_policy,
        } => (
            InstanceDeploymentMode::Private,
            *organization_creation_policy,
        ),
        CompleteInstanceSetupRequest::Team {
            organization_creation_policy,
        } => (InstanceDeploymentMode::Team, *organization_creation_policy),
        CompleteInstanceSetupRequest::PlatformByo {
            organization_creation_policy,
            ..
        } => (
            InstanceDeploymentMode::PlatformByo,
            *organization_creation_policy,
        ),
        CompleteInstanceSetupRequest::PlatformConnect {
            organization_creation_policy,
            ..
        } => (
            InstanceDeploymentMode::PlatformConnect,
            *organization_creation_policy,
        ),
    }
}

const fn billing_mode_capabilities(template_configured: bool) -> (bool, bool) {
    (template_configured, template_configured)
}

fn setup_completion_ready(
    mode: InstanceDeploymentMode,
    account_status: &str,
    catalog_ready: bool,
) -> bool {
    match mode {
        InstanceDeploymentMode::Private | InstanceDeploymentMode::Team => true,
        InstanceDeploymentMode::PlatformByo | InstanceDeploymentMode::PlatformConnect => {
            catalog_ready && account_status == "enabled"
        }
        InstanceDeploymentMode::Unconfigured => false,
    }
}

fn ensure_instance_setup_complete(setup_complete: bool) -> Result<(), InstanceServiceError> {
    if setup_complete {
        Ok(())
    } else {
        Err(InstanceServiceError::SetupRequired)
    }
}

const fn deployment_mode_clears_billing(mode: InstanceDeploymentMode) -> bool {
    matches!(
        mode,
        InstanceDeploymentMode::Private | InstanceDeploymentMode::Team
    )
}

fn ensure_billing_reconfiguration_safe(
    current_mode: InstanceDeploymentMode,
    requested_mode: InstanceDeploymentMode,
    current_billing_mode: Option<&str>,
    current_provider_account_id: Option<&str>,
    tenant_billing_in_use: bool,
) -> Result<(), InstanceServiceError> {
    if !tenant_billing_in_use {
        return Ok(());
    }
    let binding_matches = match current_mode {
        InstanceDeploymentMode::PlatformByo => current_billing_mode == Some("byo_keys"),
        InstanceDeploymentMode::PlatformConnect => current_billing_mode == Some("stripe_connect"),
        InstanceDeploymentMode::Unconfigured
        | InstanceDeploymentMode::Private
        | InstanceDeploymentMode::Team => false,
    };
    if current_mode != requested_mode || !binding_matches || current_provider_account_id.is_none() {
        Err(InstanceServiceError::BillingInUse)
    } else {
        Ok(())
    }
}

fn deployment_mode_name(value: InstanceDeploymentMode) -> &'static str {
    match value {
        InstanceDeploymentMode::Unconfigured => "unconfigured",
        InstanceDeploymentMode::Private => "private",
        InstanceDeploymentMode::Team => "team",
        InstanceDeploymentMode::PlatformByo => "platform_byo",
        InstanceDeploymentMode::PlatformConnect => "platform_connect",
    }
}

fn validate_stripe_secret_key(value: &str) -> Result<(), InstanceServiceError> {
    if (value.starts_with("sk_test_") || value.starts_with("sk_live_"))
        && (16..=512).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(InstanceServiceError::InvalidRequest)
    }
}

fn validate_stripe_webhook_secret(value: &str) -> Result<(), InstanceServiceError> {
    if value.starts_with("whsec_")
        && (16..=512).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(InstanceServiceError::InvalidRequest)
    }
}

fn validate_provider_idempotency_key(value: &str) -> Result<(), InstanceServiceError> {
    if (8..=200).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        Ok(())
    } else {
        Err(InstanceServiceError::InvalidRequest)
    }
}

fn validate_country(value: &str) -> Result<(), InstanceServiceError> {
    if value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        Ok(())
    } else {
        Err(InstanceServiceError::InvalidRequest)
    }
}

fn validate_email(value: &str) -> Result<(), InstanceServiceError> {
    let value = value.trim();
    let Some((local, domain)) = value.rsplit_once('@') else {
        return Err(InstanceServiceError::InvalidRequest);
    };
    if value.len() <= 320
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Ok(())
    } else {
        Err(InstanceServiceError::InvalidRequest)
    }
}

fn validate_return_url(value: &str) -> Result<Url, InstanceServiceError> {
    let url = Url::parse(value).map_err(|_| InstanceServiceError::InvalidRequest)?;
    let local_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    if (url.scheme() == "https" || local_http)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some()
    {
        Ok(url)
    } else {
        Err(InstanceServiceError::InvalidRequest)
    }
}

fn valid_provider_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && (prefix.len() + 8..=255).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_stripe_price(
    payload: &Value,
    expected_id: &str,
    expected_currency: &str,
    expected_unit_amount: Option<u64>,
    expected_meter_id: Option<&str>,
) -> Result<(), InstanceServiceError> {
    if payload.get("id").and_then(Value::as_str) != Some(expected_id)
        || payload.get("active").and_then(Value::as_bool) != Some(true)
        || payload.get("currency").and_then(Value::as_str) != Some(expected_currency)
        || payload
            .get("recurring")
            .and_then(Value::as_object)
            .is_none()
    {
        return Err(InstanceServiceError::ProviderCatalogConflict);
    }
    if let Some(expected) = expected_unit_amount
        && payload.get("unit_amount").and_then(Value::as_u64) != Some(expected)
    {
        return Err(InstanceServiceError::ProviderCatalogConflict);
    }
    if let Some(expected) = expected_meter_id {
        let actual = payload.pointer("/recurring/meter").and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("id").and_then(Value::as_str))
        });
        if actual != Some(expected) {
            return Err(InstanceServiceError::ProviderCatalogConflict);
        }
    }
    Ok(())
}

fn validate_stripe_price_tiers(
    payload: &Value,
    expected: &[StripePriceTier],
) -> Result<(), InstanceServiceError> {
    if payload.get("billing_scheme").and_then(Value::as_str) != Some("tiered")
        || payload.get("tiers_mode").and_then(Value::as_str) != Some("graduated")
    {
        return Err(InstanceServiceError::ProviderCatalogConflict);
    }
    let actual = payload
        .get("tiers")
        .and_then(Value::as_array)
        .filter(|tiers| tiers.len() == expected.len())
        .ok_or(InstanceServiceError::ProviderCatalogConflict)?;
    for (actual, expected) in actual.iter().zip(expected) {
        let actual_up_to = actual.get("up_to").and_then(Value::as_u64);
        if actual_up_to != expected.up_to {
            return Err(InstanceServiceError::ProviderCatalogConflict);
        }
        let amount = actual
            .get("unit_amount_decimal")
            .and_then(Value::as_str)
            .or_else(|| {
                actual
                    .get("unit_amount")
                    .and_then(Value::as_i64)
                    .map(|value| if value == 0 { "0" } else { "" })
            })
            .ok_or(InstanceServiceError::ProviderCatalogConflict)?;
        if normalize_decimal(amount) != normalize_decimal(expected.unit_amount_decimal) {
            return Err(InstanceServiceError::ProviderCatalogConflict);
        }
        if actual
            .get("flat_amount_decimal")
            .and_then(Value::as_str)
            .is_some_and(|value| normalize_decimal(value) != "0")
            || actual
                .get("flat_amount")
                .and_then(Value::as_i64)
                .is_some_and(|value| value != 0)
        {
            return Err(InstanceServiceError::ProviderCatalogConflict);
        }
    }
    Ok(())
}

fn normalize_decimal(value: &str) -> &str {
    let value = value.trim();
    if let Some((whole, fractional)) = value.split_once('.') {
        let fractional = fractional.trim_end_matches('0');
        if fractional.is_empty() {
            whole
        } else {
            value.trim_end_matches('0')
        }
    } else {
        value
    }
}

fn parse_v1_account(payload: &Value) -> Result<ProviderAccountState, InstanceServiceError> {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_provider_id(value, "acct_"))
        .ok_or(InstanceServiceError::ProviderRejected)?
        .to_owned();
    let charges_enabled = payload
        .get("charges_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let payouts_enabled = payload
        .get("payouts_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let details_submitted = payload
        .get("details_submitted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut capabilities = payload
        .get("capabilities")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|values| values.iter())
        .filter(|(_, status)| status.as_str() == Some("active"))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    capabilities.sort();
    capabilities.dedup();
    Ok(ProviderAccountState {
        id,
        status: if charges_enabled && details_submitted {
            "enabled"
        } else {
            "restricted"
        },
        charges_enabled,
        payouts_enabled,
        details_submitted,
        capabilities,
    })
}

fn parse_v2_account(payload: &Value) -> Result<ProviderAccountState, InstanceServiceError> {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_provider_id(value, "acct_"))
        .ok_or(InstanceServiceError::ProviderRejected)?
        .to_owned();
    let capabilities_object = payload
        .pointer("/configuration/merchant/capabilities")
        .and_then(Value::as_object);
    let charges_enabled = capabilities_object
        .and_then(|values| values.get("card_payments"))
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        == Some("active");
    let payouts_enabled = capabilities_object
        .and_then(|values| values.get("stripe_balance"))
        .and_then(|value| value.get("payouts"))
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        == Some("active");
    let details_submitted = payload
        .pointer("/requirements/entries")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    let mut capabilities = Vec::new();
    if let Some(values) = capabilities_object {
        collect_active_capabilities(values, "", &mut capabilities);
    }
    capabilities.sort();
    capabilities.dedup();
    Ok(ProviderAccountState {
        id,
        status: if charges_enabled && details_submitted {
            "enabled"
        } else if details_submitted {
            "restricted"
        } else {
            "onboarding"
        },
        charges_enabled,
        payouts_enabled,
        details_submitted,
        capabilities,
    })
}

fn collect_active_capabilities(
    values: &serde_json::Map<String, Value>,
    prefix: &str,
    output: &mut Vec<String>,
) {
    for (name, value) in values {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if value.get("status").and_then(Value::as_str) == Some("active") {
            output.push(path.clone());
        }
        if let Some(nested) = value.as_object() {
            collect_active_capabilities(nested, &path, output);
        }
    }
}

fn parse_account_link(payload: &Value) -> Result<InstanceBillingOnboarding, InstanceServiceError> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .ok_or(InstanceServiceError::ProviderRejected)?;
    let url = Url::parse(url).map_err(|_| InstanceServiceError::ProviderRejected)?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || !(host == "stripe.com" || host.ends_with(".stripe.com"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(InstanceServiceError::ProviderRejected);
    }
    let expires = payload
        .get("expires_at")
        .and_then(Value::as_str)
        .ok_or(InstanceServiceError::ProviderRejected)?;
    let expires_at_ms = DateTime::parse_from_rfc3339(expires)
        .map_err(|_| InstanceServiceError::ProviderRejected)?
        .timestamp_millis();
    Ok(InstanceBillingOnboarding {
        url: url.into(),
        expires_at_ms,
    })
}

fn map_commerce_error(error: CommerceServiceError) -> InstanceServiceError {
    match error {
        CommerceServiceError::InvalidConfiguration
        | CommerceServiceError::Encryption
        | CommerceServiceError::AccountNotConfigured => InstanceServiceError::InvalidConfiguration,
        CommerceServiceError::InvalidRequest => InstanceServiceError::InvalidRequest,
        CommerceServiceError::ProviderUnavailable => InstanceServiceError::ProviderUnavailable,
        CommerceServiceError::ProviderRejected
        | CommerceServiceError::ProviderResponseInvalid
        | CommerceServiceError::AccountRestricted
        | CommerceServiceError::CapabilityUnavailable
        | CommerceServiceError::InvalidSignature => InstanceServiceError::ProviderRejected,
        CommerceServiceError::NotFound => InstanceServiceError::NotFound,
        CommerceServiceError::AccountInUse
        | CommerceServiceError::Conflict
        | CommerceServiceError::WebhookHashConflict => InstanceServiceError::Conflict,
        CommerceServiceError::Forbidden => InstanceServiceError::Forbidden,
        CommerceServiceError::Unavailable => InstanceServiceError::Unavailable,
    }
}

fn map_billing_error(error: BillingError) -> InstanceServiceError {
    match error {
        BillingError::InvalidConfiguration => InstanceServiceError::InvalidConfiguration,
        BillingError::InvalidRequest => InstanceServiceError::InvalidRequest,
        BillingError::InvalidWebhookSignature | BillingError::InvalidWebhookPayload => {
            InstanceServiceError::ProviderRejected
        }
        BillingError::ProviderUnavailable => InstanceServiceError::ProviderUnavailable,
        BillingError::ProviderRejected => InstanceServiceError::ProviderRejected,
    }
}

fn validate_plan(
    tier: PlatformBillingTier,
    input: &PutInstancePlanCatalogEntryRequest,
) -> Result<(), InstanceServiceError> {
    let display_name = input.display_name.trim();
    let limits = [
        input.storage_bytes,
        input.monthly_reads,
        input.monthly_writes,
        input.monthly_active_users,
    ];
    if display_name.is_empty()
        || display_name.chars().count() > 100
        || input.currency.len() != 3
        || !input.currency.bytes().all(|byte| byte.is_ascii_lowercase())
        || limits
            .iter()
            .any(|value| *value == 0 || *value > MAX_SAFE_INTEGER)
        || input.project_limit == Some(0)
        || !input
            .storage_bytes
            .is_multiple_of(STORAGE_BILLING_UNIT_BYTES)
        || input
            .base_price_cents
            .is_some_and(|value| value > MAX_SAFE_INTEGER)
    {
        return Err(InstanceServiceError::InvalidRequest);
    }
    let uses_overage = input.reads_at_limit == InstanceReadsAtLimit::Overage
        || input.writes_at_limit == InstanceWritesAtLimit::Overage
        || input.signups_at_limit == InstanceSignupsAtLimit::Overage;
    if uses_overage != input.overage_enabled {
        return Err(InstanceServiceError::InvalidRequest);
    }
    match tier {
        PlatformBillingTier::Free
            if input.billing_unit != PlatformBillingUnit::Organization
                || input.base_price_cents != Some(0)
                || input.project_limit.is_none()
                || input.overage_enabled
                || !input.active =>
        {
            Err(InstanceServiceError::InvalidRequest)
        }
        PlatformBillingTier::PayAsYouGo
            if input.base_price_cents.is_some() || !input.overage_enabled =>
        {
            Err(InstanceServiceError::InvalidRequest)
        }
        PlatformBillingTier::Pro
            if input.base_price_cents.is_none_or(|price| price == 0) || !input.overage_enabled =>
        {
            Err(InstanceServiceError::InvalidRequest)
        }
        _ => Ok(()),
    }
}

fn provider_pricing_fields_match(
    current: &InstancePlanCatalogEntry,
    requested: &PutInstancePlanCatalogEntryRequest,
) -> bool {
    current.billing_unit == requested.billing_unit
        && current.base_price_cents == requested.base_price_cents
        && current.currency == requested.currency
        && current.storage_bytes == requested.storage_bytes
        && current.monthly_reads == requested.monthly_reads
        && current.monthly_writes == requested.monthly_writes
        && current.monthly_active_users == requested.monthly_active_users
}

fn plan_select() -> &'static str {
    "SELECT tier,display_name,billing_unit,base_price_cents,currency,project_limit, \
            storage_bytes,monthly_reads,monthly_writes,monthly_active_users,overage_enabled, \
            reads_at_limit,writes_at_limit,signups_at_limit, \
            requires_payment_method_for_overage,active, \
            (extract(epoch FROM updated_at)*1000)::bigint updated_at_ms \
     FROM billing_price_catalog"
}

fn parse_deployment_mode(value: &str) -> Result<InstanceDeploymentMode, InstanceServiceError> {
    match value {
        "unconfigured" => Ok(InstanceDeploymentMode::Unconfigured),
        "private" => Ok(InstanceDeploymentMode::Private),
        "team" => Ok(InstanceDeploymentMode::Team),
        "platform_byo" => Ok(InstanceDeploymentMode::PlatformByo),
        "platform_connect" => Ok(InstanceDeploymentMode::PlatformConnect),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn parse_creation_policy(value: &str) -> Result<OrganizationCreationPolicy, InstanceServiceError> {
    match value {
        "owner_only" => Ok(OrganizationCreationPolicy::OwnerOnly),
        "authenticated" => Ok(OrganizationCreationPolicy::Authenticated),
        "invitation_only" => Ok(OrganizationCreationPolicy::InvitationOnly),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

const fn creation_policy_allows(
    policy: OrganizationCreationPolicy,
    administrator: bool,
    organization_member: bool,
) -> bool {
    match policy {
        OrganizationCreationPolicy::Authenticated => true,
        OrganizationCreationPolicy::OwnerOnly => administrator,
        OrganizationCreationPolicy::InvitationOnly => administrator || organization_member,
    }
}

fn creation_policy_name(value: OrganizationCreationPolicy) -> &'static str {
    match value {
        OrganizationCreationPolicy::OwnerOnly => "owner_only",
        OrganizationCreationPolicy::Authenticated => "authenticated",
        OrganizationCreationPolicy::InvitationOnly => "invitation_only",
    }
}

fn parse_administrator_role(
    value: &str,
) -> Result<InstanceAdministratorRole, InstanceServiceError> {
    match value {
        "owner" => Ok(InstanceAdministratorRole::Owner),
        "admin" => Ok(InstanceAdministratorRole::Admin),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn parse_billing_mode(value: &str) -> Result<InstanceBillingMode, InstanceServiceError> {
    match value {
        "byo_keys" => Ok(InstanceBillingMode::ByoKeys),
        "stripe_connect" => Ok(InstanceBillingMode::StripeConnect),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn parse_billing_status(value: &str) -> Result<InstanceBillingAccountStatus, InstanceServiceError> {
    match value {
        "pending" => Ok(InstanceBillingAccountStatus::Pending),
        "onboarding" => Ok(InstanceBillingAccountStatus::Onboarding),
        "enabled" => Ok(InstanceBillingAccountStatus::Enabled),
        "restricted" => Ok(InstanceBillingAccountStatus::Restricted),
        "disconnected" => Ok(InstanceBillingAccountStatus::Disconnected),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn tier_name(value: PlatformBillingTier) -> &'static str {
    match value {
        PlatformBillingTier::Free => "free",
        PlatformBillingTier::PayAsYouGo => "pay_as_you_go",
        PlatformBillingTier::Pro => "pro",
    }
}

fn parse_tier(value: &str) -> Result<PlatformBillingTier, InstanceServiceError> {
    match value {
        "free" => Ok(PlatformBillingTier::Free),
        "pay_as_you_go" => Ok(PlatformBillingTier::PayAsYouGo),
        "pro" => Ok(PlatformBillingTier::Pro),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn parse_usage_metric(value: &str) -> Result<UsageMetric, InstanceServiceError> {
    match value {
        "reads" => Ok(UsageMetric::Reads),
        "writes" => Ok(UsageMetric::Writes),
        "storage_byte_hours" => Ok(UsageMetric::StorageByteHours),
        "monthly_active_users" => Ok(UsageMetric::MonthlyActiveUsers),
        _ => Err(InstanceServiceError::InvalidConfiguration),
    }
}

fn billing_unit_name(value: PlatformBillingUnit) -> &'static str {
    match value {
        PlatformBillingUnit::Organization => "organization",
        PlatformBillingUnit::Seat => "seat",
    }
}

fn parse_billing_unit(value: &str) -> Result<PlatformBillingUnit, InstanceServiceError> {
    match value {
        "organization" => Ok(PlatformBillingUnit::Organization),
        "seat" => Ok(PlatformBillingUnit::Seat),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn reads_at_limit_name(value: InstanceReadsAtLimit) -> &'static str {
    match value {
        InstanceReadsAtLimit::Continue => "continue",
        InstanceReadsAtLimit::Overage => "overage",
    }
}

fn parse_reads_at_limit(value: &str) -> Result<InstanceReadsAtLimit, InstanceServiceError> {
    match value {
        "continue" => Ok(InstanceReadsAtLimit::Continue),
        "overage" => Ok(InstanceReadsAtLimit::Overage),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn writes_at_limit_name(value: InstanceWritesAtLimit) -> &'static str {
    match value {
        InstanceWritesAtLimit::Pause => "pause",
        InstanceWritesAtLimit::Overage => "overage",
    }
}

fn parse_writes_at_limit(value: &str) -> Result<InstanceWritesAtLimit, InstanceServiceError> {
    match value {
        "pause" => Ok(InstanceWritesAtLimit::Pause),
        "overage" => Ok(InstanceWritesAtLimit::Overage),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn signups_at_limit_name(value: InstanceSignupsAtLimit) -> &'static str {
    match value {
        InstanceSignupsAtLimit::Pause => "pause",
        InstanceSignupsAtLimit::Overage => "overage",
    }
}

fn parse_signups_at_limit(value: &str) -> Result<InstanceSignupsAtLimit, InstanceServiceError> {
    match value {
        "pause" => Ok(InstanceSignupsAtLimit::Pause),
        "overage" => Ok(InstanceSignupsAtLimit::Overage),
        _ => Err(InstanceServiceError::Unavailable),
    }
}

fn parse_tier_path(value: &str) -> Result<PlatformBillingTier, InstanceServiceError> {
    parse_tier(value)
}

fn parse_uuid_path<T>(
    value: &str,
    wrap: impl FnOnce(Uuid) -> T,
) -> Result<T, InstanceServiceError> {
    Uuid::parse_str(value)
        .map(wrap)
        .map_err(|_| InstanceServiceError::InvalidRequest)
}

pub(crate) fn service_error(error: InstanceServiceError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        InstanceServiceError::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "instance.invalid_request",
            "the instance administration request is invalid",
        ),
        InstanceServiceError::Forbidden => (
            StatusCode::FORBIDDEN,
            "instance.forbidden",
            "the operation is not permitted",
        ),
        InstanceServiceError::NotFound => (
            StatusCode::NOT_FOUND,
            "instance.not_found",
            "the requested instance resource was not found",
        ),
        InstanceServiceError::Conflict => (
            StatusCode::CONFLICT,
            "instance.conflict",
            "the requested change conflicts with current instance state",
        ),
        InstanceServiceError::BillingInUse => (
            StatusCode::CONFLICT,
            "instance.billing_in_use",
            "cancel and reconcile every organization subscription before changing the instance billing provider",
        ),
        InstanceServiceError::ProviderCatalogConflict => (
            StatusCode::CONFLICT,
            "instance.plan_provider_bound",
            "provider-priced plan fields are immutable while the Stripe catalog is active",
        ),
        InstanceServiceError::NotInitialized => (
            StatusCode::CONFLICT,
            "instance.not_initialized",
            "the instance owner has not been initialized",
        ),
        InstanceServiceError::SetupRequired => (
            StatusCode::CONFLICT,
            "instance.setup_required",
            "complete instance onboarding before creating organizations or projects",
        ),
        InstanceServiceError::ProviderUnavailable => (
            StatusCode::BAD_GATEWAY,
            "instance.provider_unavailable",
            "the billing provider is temporarily unavailable",
        ),
        InstanceServiceError::ProviderRejected => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "instance.provider_rejected",
            "the billing provider rejected the request",
        ),
        InstanceServiceError::InvalidConfiguration => (
            StatusCode::SERVICE_UNAVAILABLE,
            "instance.configuration_invalid",
            "instance billing is not configured for this operation",
        ),
        InstanceServiceError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "instance.unavailable",
            "instance administration is temporarily unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}

#[allow(clippy::result_large_err)]
fn required_service(
    service: Option<Arc<InstanceService>>,
    request_id: RequestId,
) -> Result<Arc<InstanceService>, Response> {
    service.ok_or_else(|| service_error(InstanceServiceError::Unavailable, request_id))
}

async fn identity(
    state: &ApiState,
    headers: &HeaderMap,
    request_id: RequestId,
) -> Result<UserId, Response> {
    authenticated(state, headers, request_id)
        .await
        .map(|(_, identity)| identity.user_id)
        .map_err(|error| error.into_response())
}

pub(crate) async fn public_status(
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.public_setup_status().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn status(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.status(actor).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn complete_setup(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CompleteInstanceSetupRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let idempotency_key = match headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value,
        None => return service_error(InstanceServiceError::InvalidRequest, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.setup.complete",
        "instance_settings",
        None,
        service.complete_setup(actor, &payload, idempotency_key),
    )
    .await
}

pub(crate) async fn connect_onboarding(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateInstanceConnectOnboardingRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let idempotency_key = match headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value,
        None => return service_error(InstanceServiceError::InvalidRequest, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.billing.connect_onboarding",
        "instance_billing_account",
        None,
        service.connect_onboarding(actor, &payload, idempotency_key),
    )
    .await
}

pub(crate) async fn refresh_billing_account(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.billing.refresh",
        "instance_billing_account",
        None,
        service.refresh_billing_account(actor),
    )
    .await
}

pub(crate) async fn update_policy(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<UpdateOrganizationCreationPolicyRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.policy.update",
        "instance_settings",
        None,
        service.update_organization_creation_policy(actor, payload.organization_creation_policy),
    )
    .await
}

pub(crate) async fn administrators(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.administrators(actor).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn grant_administrator(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<GrantInstanceAdministratorRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.administrator.grant",
        "instance_administrator",
        Some(payload.user_id.0),
        service.grant_administrator(actor, payload.user_id),
    )
    .await
}

pub(crate) async fn revoke_administrator(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let user_id = match parse_uuid_path(&user, UserId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.administrator.revoke",
        "instance_administrator",
        Some(user_id.0),
        service.revoke_administrator(actor, user_id),
    )
    .await
}

pub(crate) async fn organizations(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(page): Query<PageQuery>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.organizations(actor, page).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn set_organization_disabled(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(organization): Path<String>,
    Json(payload): Json<UpdateInstanceDisabledRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let organization_id = match parse_uuid_path(&organization, OrganizationId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        if payload.disabled {
            "instance.organization.disable"
        } else {
            "instance.organization.enable"
        },
        "organization",
        Some(organization_id.0),
        service.set_organization_disabled(actor, organization_id, payload.disabled),
    )
    .await
}

pub(crate) async fn users(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(page): Query<PageQuery>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.users(actor, page).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn set_user_disabled(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(user): Path<String>,
    Json(payload): Json<UpdateInstanceDisabledRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let user_id = match parse_uuid_path(&user, UserId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        if payload.disabled {
            "instance.user.disable"
        } else {
            "instance.user.enable"
        },
        "platform_user",
        Some(user_id.0),
        service.set_user_disabled(actor, user_id, payload.disabled),
    )
    .await
}

pub(crate) async fn billing_exemptions(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.billing_exemptions(actor).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn grant_billing_exemption(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(organization): Path<String>,
    Json(payload): Json<GrantOrganizationBillingExemptionRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let organization_id = match parse_uuid_path(&organization, OrganizationId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.billing_exemption.grant",
        "organization",
        Some(organization_id.0),
        service.grant_billing_exemption(actor, organization_id, &payload.reason),
    )
    .await
}

pub(crate) async fn revoke_billing_exemption(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(organization): Path<String>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let organization_id = match parse_uuid_path(&organization, OrganizationId) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.billing_exemption.revoke",
        "organization",
        Some(organization_id.0),
        service.revoke_billing_exemption(actor, organization_id),
    )
    .await
}

pub(crate) async fn plans(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service.plans(actor).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => service_error(error, request_id),
    }
}

pub(crate) async fn put_plan(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(tier): Path<String>,
    Json(payload): Json<PutInstancePlanCatalogEntryRequest>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let tier = match parse_tier_path(&tier) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.plan.put",
        "billing_plan",
        None,
        service.put_plan(actor, tier, &payload),
    )
    .await
}

pub(crate) async fn retire_plan(
    State(state): State<ApiState>,
    Extension(service): Extension<Option<Arc<InstanceService>>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(tier): Path<String>,
) -> Response {
    let service = match required_service(service, request_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let actor = match identity(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let tier = match parse_tier_path(&tier) {
        Ok(value) => value,
        Err(error) => return service_error(error, request_id),
    };
    audited_result(
        &state,
        actor,
        request_id,
        "instance.plan.retire",
        "billing_plan",
        None,
        service.retire_plan(actor, tier),
    )
    .await
}

async fn audited_result<T: serde::Serialize>(
    state: &ApiState,
    actor: UserId,
    request_id: RequestId,
    action: &str,
    resource_kind: &str,
    resource_id: Option<Uuid>,
    operation: impl std::future::Future<Output = Result<T, InstanceServiceError>>,
) -> Response {
    if let Err(response) = require_management_audit(
        state,
        None,
        None,
        Some(actor),
        request_id,
        action,
        resource_kind,
        resource_id,
    )
    .await
    {
        return response;
    }
    match operation.await {
        Ok(value) => {
            terminal_management_audit(
                state,
                None,
                None,
                Some(actor),
                request_id,
                action,
                resource_kind,
                resource_id,
                AuditOutcome::Success,
            )
            .await;
            Json(value).into_response()
        }
        Err(error) => {
            terminal_management_audit(
                state,
                None,
                None,
                Some(actor),
                request_id,
                action,
                resource_kind,
                resource_id,
                AuditOutcome::Failure,
            )
            .await;
            service_error(error, request_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffdb_protocol::SensitiveString;

    fn valid_plan(tier: PlatformBillingTier) -> PutInstancePlanCatalogEntryRequest {
        PutInstancePlanCatalogEntryRequest {
            display_name: "Plan".to_owned(),
            billing_unit: PlatformBillingUnit::Organization,
            base_price_cents: match tier {
                PlatformBillingTier::Free => Some(0),
                PlatformBillingTier::PayAsYouGo => None,
                PlatformBillingTier::Pro => Some(700),
            },
            currency: "usd".to_owned(),
            project_limit: (tier == PlatformBillingTier::Free).then_some(2),
            storage_bytes: 1_000_000_000,
            monthly_reads: 1_000_000,
            monthly_writes: 50_000,
            monthly_active_users: 5_000,
            overage_enabled: tier != PlatformBillingTier::Free,
            reads_at_limit: if tier == PlatformBillingTier::Free {
                InstanceReadsAtLimit::Continue
            } else {
                InstanceReadsAtLimit::Overage
            },
            writes_at_limit: if tier == PlatformBillingTier::Free {
                InstanceWritesAtLimit::Pause
            } else {
                InstanceWritesAtLimit::Overage
            },
            signups_at_limit: if tier == PlatformBillingTier::Free {
                InstanceSignupsAtLimit::Pause
            } else {
                InstanceSignupsAtLimit::Overage
            },
            requires_payment_method_for_overage: tier != PlatformBillingTier::Free,
            active: true,
        }
    }

    fn catalog_plan(tier: PlatformBillingTier) -> InstancePlanCatalogEntry {
        let plan = valid_plan(tier);
        InstancePlanCatalogEntry {
            tier,
            display_name: plan.display_name.clone(),
            billing_unit: plan.billing_unit,
            base_price_cents: plan.base_price_cents,
            currency: plan.currency.clone(),
            project_limit: plan.project_limit,
            storage_bytes: plan.storage_bytes,
            monthly_reads: plan.monthly_reads,
            monthly_writes: plan.monthly_writes,
            monthly_active_users: plan.monthly_active_users,
            overage_enabled: plan.overage_enabled,
            reads_at_limit: plan.reads_at_limit,
            writes_at_limit: plan.writes_at_limit,
            signups_at_limit: plan.signups_at_limit,
            requires_payment_method_for_overage: plan.requires_payment_method_for_overage,
            active: plan.active,
            provider_catalog_bound: true,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn catalog_validation_preserves_tier_invariants() {
        for tier in [
            PlatformBillingTier::Free,
            PlatformBillingTier::PayAsYouGo,
            PlatformBillingTier::Pro,
        ] {
            assert_eq!(validate_plan(tier, &valid_plan(tier)), Ok(()));
        }
        let mut free = valid_plan(PlatformBillingTier::Free);
        free.active = false;
        assert_eq!(
            validate_plan(PlatformBillingTier::Free, &free),
            Err(InstanceServiceError::InvalidRequest)
        );
        let mut payg = valid_plan(PlatformBillingTier::PayAsYouGo);
        payg.overage_enabled = false;
        assert_eq!(
            validate_plan(PlatformBillingTier::PayAsYouGo, &payg),
            Err(InstanceServiceError::InvalidRequest)
        );
    }

    #[test]
    fn plan_mutation_allows_owner_and_admin_but_rejects_ordinary_users() {
        assert_eq!(
            authorize_instance_role(Some(InstanceAdministratorRole::Owner), false),
            Ok(InstanceAdministratorRole::Owner)
        );
        assert_eq!(
            authorize_instance_role(Some(InstanceAdministratorRole::Admin), false),
            Ok(InstanceAdministratorRole::Admin)
        );
        assert_eq!(
            authorize_instance_role(None, false),
            Err(InstanceServiceError::Forbidden)
        );
        assert_eq!(
            authorize_instance_role(Some(InstanceAdministratorRole::Admin), true),
            Err(InstanceServiceError::Forbidden)
        );
    }

    #[test]
    fn organization_creation_policy_distinguishes_invited_members() {
        assert!(creation_policy_allows(
            OrganizationCreationPolicy::Authenticated,
            false,
            false
        ));
        assert!(creation_policy_allows(
            OrganizationCreationPolicy::OwnerOnly,
            true,
            false
        ));
        assert!(!creation_policy_allows(
            OrganizationCreationPolicy::OwnerOnly,
            false,
            true
        ));
        assert!(creation_policy_allows(
            OrganizationCreationPolicy::InvitationOnly,
            false,
            true
        ));
        assert!(!creation_policy_allows(
            OrganizationCreationPolicy::InvitationOnly,
            false,
            false
        ));
    }

    #[test]
    fn provider_bound_plan_rejects_local_pricing_and_billing_unit_drift() {
        let current = catalog_plan(PlatformBillingTier::Pro);
        let requested = valid_plan(PlatformBillingTier::Pro);
        assert!(provider_pricing_fields_match(&current, &requested));

        let mut changed_allowance = requested.clone();
        changed_allowance.monthly_reads += 1;
        assert!(!provider_pricing_fields_match(&current, &changed_allowance));

        let mut changed_billing_unit = requested;
        changed_billing_unit.billing_unit = PlatformBillingUnit::Seat;
        assert!(!provider_pricing_fields_match(
            &current,
            &changed_billing_unit
        ));
    }

    #[test]
    fn user_disable_protects_owner_and_last_enabled_administrator() {
        assert_eq!(
            authorize_user_disable(true, Some("owner"), false, 5),
            Err(InstanceServiceError::Forbidden)
        );
        assert_eq!(
            authorize_user_disable(true, Some("admin"), true, 0),
            Err(InstanceServiceError::Conflict)
        );
        assert_eq!(authorize_user_disable(true, Some("admin"), true, 1), Ok(()));
        assert_eq!(authorize_user_disable(true, None, false, 0), Ok(()));
        assert_eq!(
            authorize_user_disable(false, Some("owner"), false, 0),
            Ok(())
        );
    }

    #[test]
    fn reasons_are_trimmed_and_bounded() {
        assert_eq!(validate_reason("  incident  "), Ok("incident"));
        assert_eq!(
            validate_reason(""),
            Err(InstanceServiceError::InvalidRequest)
        );
        assert_eq!(
            validate_reason(&"x".repeat(501)),
            Err(InstanceServiceError::InvalidRequest)
        );
    }

    #[test]
    fn pagination_is_bounded() {
        assert!(
            PageQuery {
                limit: MAX_PAGE_SIZE,
                offset: i64::MAX as u64
            }
            .validated()
            .is_ok()
        );
        assert!(
            PageQuery {
                limit: MAX_PAGE_SIZE + 1,
                offset: 0
            }
            .validated()
            .is_err()
        );
    }

    #[test]
    fn provider_account_parsers_fail_closed_and_report_capabilities()
    -> Result<(), InstanceServiceError> {
        let v1 = parse_v1_account(&json!({
            "id": "acct_12345678",
            "charges_enabled": true,
            "payouts_enabled": true,
            "details_submitted": true,
            "capabilities": { "card_payments": "active", "transfers": "pending" }
        }))?;
        assert_eq!(v1.status, "enabled");
        assert_eq!(v1.capabilities, vec!["card_payments"]);

        let v2 = parse_v2_account(&json!({
            "id": "acct_abcdefgh",
            "configuration": { "merchant": { "capabilities": {
                "card_payments": { "status": "active" },
                "stripe_balance": { "payouts": { "status": "active" } }
            }}},
            "requirements": { "entries": [] }
        }))?;
        assert_eq!(v2.status, "enabled");
        assert!(v2.capabilities.contains(&"card_payments".to_owned()));
        assert!(
            v2.capabilities
                .contains(&"stripe_balance.payouts".to_owned())
        );
        assert!(parse_v2_account(&json!({ "id": "customer_123" })).is_err());
        Ok(())
    }

    #[test]
    fn account_links_require_stripe_https_and_rfc3339_expiry() -> Result<(), InstanceServiceError> {
        let link = parse_account_link(&json!({
            "url": "https://connect.stripe.com/setup/s/acct_12345678",
            "expires_at": "2026-08-03T14:00:00.000Z"
        }))?;
        assert_eq!(link.expires_at_ms, 1_785_765_600_000);
        assert!(
            parse_account_link(&json!({
                "url": "https://stripe.example.test/phish",
                "expires_at": "2026-08-03T14:00:00.000Z"
            }))
            .is_err()
        );
        Ok(())
    }

    fn provisioning_plan(tier: PlatformBillingTier) -> StripeProvisioningPlan {
        StripeProvisioningPlan {
            tier,
            currency: "usd".into(),
            base_price_cents: (tier == PlatformBillingTier::Pro).then_some(700),
            storage_bytes: if tier == PlatformBillingTier::Pro {
                10_000_000_000
            } else {
                1_000_000_000
            },
            monthly_reads: if tier == PlatformBillingTier::Pro {
                15_000_000
            } else {
                1_000_000
            },
            monthly_writes: if tier == PlatformBillingTier::Pro {
                750_000
            } else {
                50_000
            },
            monthly_active_users: if tier == PlatformBillingTier::Pro {
                50_000
            } else {
                5_000
            },
        }
    }

    #[test]
    fn provisioned_prices_encode_allowances_and_prototype_rates() {
        let payg = provisioning_plan(PlatformBillingTier::PayAsYouGo);
        assert_eq!(
            stripe_price_tiers(UsageMetric::Reads, &payg),
            Ok(vec![
                StripePriceTier {
                    up_to: Some(1_000_000),
                    unit_amount_decimal: "0",
                },
                StripePriceTier {
                    up_to: None,
                    unit_amount_decimal: "0.000025",
                },
            ])
        );
        assert_eq!(
            stripe_price_tiers(UsageMetric::Writes, &payg),
            Ok(vec![
                StripePriceTier {
                    up_to: Some(50_000),
                    unit_amount_decimal: "0",
                },
                StripePriceTier {
                    up_to: Some(1_000_000),
                    unit_amount_decimal: "0.00015",
                },
                StripePriceTier {
                    up_to: None,
                    unit_amount_decimal: "0.000225",
                },
            ])
        );
        assert_eq!(
            stripe_price_tiers(UsageMetric::MonthlyActiveUsers, &payg),
            Ok(vec![
                StripePriceTier {
                    up_to: Some(5_000),
                    unit_amount_decimal: "0",
                },
                StripePriceTier {
                    up_to: Some(50_000),
                    unit_amount_decimal: "0.5",
                },
                StripePriceTier {
                    up_to: None,
                    unit_amount_decimal: "1.5",
                },
            ])
        );
        let pro = provisioning_plan(PlatformBillingTier::Pro);
        assert_eq!(
            stripe_price_tiers(UsageMetric::MonthlyActiveUsers, &pro),
            Ok(vec![
                StripePriceTier {
                    up_to: Some(50_000),
                    unit_amount_decimal: "0",
                },
                StripePriceTier {
                    up_to: None,
                    unit_amount_decimal: "1.5",
                },
            ])
        );
        assert_eq!(
            stripe_price_tiers(UsageMetric::StorageByteHours, &payg),
            Ok(vec![
                StripePriceTier {
                    up_to: Some(1_000_000 * 730),
                    unit_amount_decimal: "0",
                },
                StripePriceTier {
                    up_to: None,
                    unit_amount_decimal: "0.000000027397",
                },
            ])
        );
    }

    #[test]
    fn capability_flags_and_non_billing_transitions_fail_closed() {
        assert_eq!(billing_mode_capabilities(false), (false, false));
        assert_eq!(billing_mode_capabilities(true), (true, true));
        assert!(setup_completion_ready(
            InstanceDeploymentMode::Private,
            "restricted",
            false,
        ));
        assert!(!setup_completion_ready(
            InstanceDeploymentMode::Unconfigured,
            "enabled",
            true,
        ));
        assert_eq!(ensure_instance_setup_complete(true), Ok(()));
        assert_eq!(
            ensure_instance_setup_complete(false),
            Err(InstanceServiceError::SetupRequired)
        );
        assert!(!setup_completion_ready(
            InstanceDeploymentMode::PlatformConnect,
            "onboarding",
            false,
        ));
        assert!(!setup_completion_ready(
            InstanceDeploymentMode::PlatformConnect,
            "enabled",
            false,
        ));
        assert!(setup_completion_ready(
            InstanceDeploymentMode::PlatformConnect,
            "enabled",
            true,
        ));
        assert!(deployment_mode_clears_billing(
            InstanceDeploymentMode::Private
        ));
        assert!(deployment_mode_clears_billing(InstanceDeploymentMode::Team));
        assert!(!deployment_mode_clears_billing(
            InstanceDeploymentMode::PlatformConnect
        ));
        let connect_request = CompleteInstanceSetupRequest::PlatformConnect {
            organization_creation_policy: OrganizationCreationPolicy::OwnerOnly,
            secret_key: SensitiveString::new("sk_test_do_not_log_this"),
            webhook_secret: SensitiveString::new("whsec_do_not_log_this"),
            country: "US".to_owned(),
            email: "owner@example.test".to_owned(),
            return_url: "https://portal.example.test/instance".to_owned(),
            refresh_url: "https://portal.example.test/instance?retry=1".to_owned(),
        };
        let debug = format!("{connect_request:?}");
        assert!(!debug.contains("sk_test_do_not_log_this"));
        assert!(!debug.contains("whsec_do_not_log_this"));
        assert!(debug.contains("[REDACTED]"));
        assert_eq!(
            ensure_billing_reconfiguration_safe(
                InstanceDeploymentMode::PlatformByo,
                InstanceDeploymentMode::Private,
                Some("byo_keys"),
                Some("acct_current"),
                true,
            ),
            Err(InstanceServiceError::BillingInUse)
        );
        assert_eq!(
            ensure_billing_reconfiguration_safe(
                InstanceDeploymentMode::PlatformByo,
                InstanceDeploymentMode::PlatformConnect,
                Some("byo_keys"),
                Some("acct_current"),
                true,
            ),
            Err(InstanceServiceError::BillingInUse)
        );
        assert_eq!(
            ensure_billing_reconfiguration_safe(
                InstanceDeploymentMode::PlatformByo,
                InstanceDeploymentMode::PlatformByo,
                Some("byo_keys"),
                Some("acct_current"),
                true,
            ),
            Ok(())
        );
        assert_eq!(
            ensure_billing_reconfiguration_safe(
                InstanceDeploymentMode::PlatformByo,
                InstanceDeploymentMode::Private,
                Some("byo_keys"),
                Some("acct_current"),
                false,
            ),
            Ok(())
        );
    }

    #[tokio::test]
    async fn billing_in_use_error_contract_is_stable() -> Result<(), Box<dyn std::error::Error>> {
        let response = service_error(InstanceServiceError::BillingInUse, RequestId::new());
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), 4_096).await?;
        let payload: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(
            payload.pointer("/error/code"),
            Some(&serde_json::Value::String(
                "instance.billing_in_use".to_owned()
            ))
        );
        assert_eq!(
            payload.pointer("/error/message"),
            Some(&serde_json::Value::String(
                "cancel and reconcile every organization subscription before changing the instance billing provider".to_owned()
            ))
        );
        assert!(
            payload
                .pointer("/error/request_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        );
        Ok(())
    }

    #[test]
    fn provider_deactivation_removes_the_runtime_delegate() -> Result<(), InstanceServiceError> {
        let portal_url = Url::parse("https://portal.example.test/app/billing")
            .map_err(|_| InstanceServiceError::InvalidConfiguration)?;
        let usage_meters = UsageMetric::ALL
            .iter()
            .map(|metric| StripeUsageMeterConfig {
                metric: *metric,
                event_name: format!("ffdb_{}", metric.name()),
                meter_id: format!("mtr_{}_12345678", metric.name()),
                payg_price_id: format!("price_payg_{}_12345678", metric.name()),
                pro_price_id: format!("price_pro_{}_12345678", metric.name()),
            })
            .collect::<Vec<_>>();
        let catalog = InstanceStripeProviderCatalog {
            product_id: Some("prod_ffdb_12345678".into()),
            pro_base_price_id: "price_pro_base_12345678".into(),
            usage_meters: usage_meters.clone(),
        };
        let handle = InstanceBillingProvider::new(Some(InstanceStripeBillingConfig {
            byo_catalog: Some(catalog.clone()),
            usage_events: usage_meters
                .iter()
                .map(|meter| InstanceStripeUsageEventConfig {
                    metric: meter.metric,
                    event_name: meter.event_name.clone(),
                })
                .collect(),
            pro_billing_unit: PlatformBillingUnit::Organization,
            success_url: portal_url.clone(),
            cancel_url: portal_url.clone(),
            portal_return_url: portal_url,
        }));
        let runtime = handle.build(
            ProtectedString::from("sk_test_1234567890123456".to_owned()), // gitleaks:allow -- synthetic Stripe test fixture
            ProtectedString::from("whsec_1234567890123456".to_owned()),
            None,
            &catalog,
        )?;
        assert!(!handle.is_configured());
        assert!(handle.activate(runtime).is_ok());
        assert!(handle.is_configured());
        assert!(handle.current().is_ok());
        assert!(handle.deactivate().is_ok());
        assert!(!handle.is_configured());
        assert!(handle.current().is_err());
        Ok(())
    }

    #[test]
    fn repeat_and_partial_byo_or_connect_provisioning_use_stable_resource_keys()
    -> Result<(), InstanceServiceError> {
        let account = "acct_catalog_12345678";
        let resources = [
            "product",
            "pro-base",
            "meter-reads",
            "price-payg-reads",
            "price-pro-reads",
            "meter-writes",
            "price-payg-writes",
            "price-pro-writes",
            "meter-storage_byte_hours",
            "price-payg-storage_byte_hours",
            "price-pro-storage_byte_hours",
            "meter-monthly_active_users",
            "price-payg-monthly_active_users",
            "price-pro-monthly_active_users",
        ];
        let first = resources
            .iter()
            .map(|resource| catalog_idempotency_key(account, resource))
            .collect::<Result<Vec<_>, _>>();
        let retry = resources
            .iter()
            .map(|resource| catalog_idempotency_key(account, resource))
            .collect::<Result<Vec<_>, _>>();
        assert_eq!(first, retry);
        let keys = first?;
        let retry_keys = retry?;
        assert_eq!(keys.len(), 14);
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len());
        assert_eq!(&keys[..5], &retry_keys[..5]);
        Ok(())
    }

    #[test]
    fn catalog_rejects_wrong_account_currency_and_price_tiers() -> Result<(), InstanceServiceError>
    {
        assert_eq!(
            validate_catalog_account("acct_first_12345678", "acct_second_12345678"),
            Err(InstanceServiceError::InvalidConfiguration)
        );
        let price = json!({
            "id": "price_reads_12345678",
            "active": true,
            "currency": "eur",
            "billing_scheme": "tiered",
            "tiers_mode": "graduated",
            "recurring": { "meter": "mtr_reads_12345678", "interval": "month" },
            "tiers": [
                { "up_to": 1_000_000, "unit_amount_decimal": "0" },
                { "up_to": null, "unit_amount_decimal": "0.000025" }
            ]
        });
        assert_eq!(
            validate_stripe_price(
                &price,
                "price_reads_12345678",
                "usd",
                None,
                Some("mtr_reads_12345678")
            ),
            Err(InstanceServiceError::ProviderCatalogConflict)
        );
        let expected = stripe_price_tiers(
            UsageMetric::Reads,
            &provisioning_plan(PlatformBillingTier::PayAsYouGo),
        );
        let expected = expected?;
        let mut wrong_tiers = price;
        wrong_tiers["currency"] = json!("usd");
        wrong_tiers["tiers"][1]["unit_amount_decimal"] = json!("0.000026");
        assert_eq!(
            validate_stripe_price_tiers(&wrong_tiers, &expected),
            Err(InstanceServiceError::ProviderCatalogConflict)
        );
        let current_price = json!({
            "billing_scheme": "tiered",
            "tiers_mode": "graduated",
            "tiers": [
                { "up_to": 1_000_000, "unit_amount_decimal": "0" },
                { "up_to": null, "unit_amount_decimal": "0.000025" }
            ]
        });
        assert_eq!(
            validate_stripe_price_tiers(&current_price, &expected),
            Ok(())
        );
        let mut changed_plan = provisioning_plan(PlatformBillingTier::PayAsYouGo);
        changed_plan.monthly_reads = 2_000_000;
        let changed_tiers = stripe_price_tiers(UsageMetric::Reads, &changed_plan)?;
        assert_eq!(
            validate_stripe_price_tiers(&current_price, &changed_tiers),
            Err(InstanceServiceError::ProviderCatalogConflict)
        );
        Ok(())
    }

    #[tokio::test]
    async fn paid_mode_cleanup_deletes_secrets_catalog_and_account_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        sqlx::query("CREATE TEMP TABLE instance_billing_usage_catalog (metric text)")
            .execute(&pool)
            .await?;
        sqlx::query("CREATE TEMP TABLE instance_billing_catalog (singleton boolean)")
            .execute(&pool)
            .await?;
        sqlx::query(
            "CREATE TEMP TABLE billing_usage_catalog (provider_meter_id text,payg_price_id text, \
             pro_price_id text,updated_at timestamptz)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("CREATE TEMP TABLE instance_billing_secrets (secret_kind text)")
            .execute(&pool)
            .await?;
        sqlx::query("CREATE TEMP TABLE instance_billing_accounts (singleton boolean)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO instance_billing_usage_catalog VALUES ('reads')")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO instance_billing_catalog VALUES (true)")
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO billing_usage_catalog VALUES ('mtr_12345678','price_a_12345678', \
             'price_b_12345678',now())",
        )
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO instance_billing_secrets VALUES ('stripe_secret_key')")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO instance_billing_accounts VALUES (true)")
            .execute(&pool)
            .await?;
        let mut transaction = pool.begin().await?;
        clear_instance_billing(&mut transaction)
            .await
            .map_err(|error| std::io::Error::other(format!("cleanup failed: {error:?}")))?;
        for table in [
            "instance_billing_usage_catalog",
            "instance_billing_catalog",
            "instance_billing_secrets",
            "instance_billing_accounts",
        ] {
            // `table` is selected exclusively from the fixed list above and
            // never contains request input.
            let count: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                    .fetch_one(&mut *transaction)
                    .await?;
            assert_eq!(count, 0, "{table} was not cleared");
        }
        let provider_ids_cleared: bool = sqlx::query_scalar(
            "SELECT provider_meter_id IS NULL AND payg_price_id IS NULL AND pro_price_id IS NULL \
             FROM billing_usage_catalog",
        )
        .fetch_one(&mut *transaction)
        .await?;
        assert!(provider_ids_cleared);
        transaction.rollback().await?;
        Ok(())
    }
}
