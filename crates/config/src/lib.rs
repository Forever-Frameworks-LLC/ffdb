//! Validated configuration. Secrets deliberately do not implement serialization.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use ffdb_protocol::{NodeId, PlatformBillingUnit, ResourceLimits};
use ipnet::IpNet;
use secrecy::SecretString;
use thiserror::Error;
use url::Url;

#[derive(Debug)]
pub struct AppConfig {
    pub environment: Environment,
    pub http: HttpConfig,
    pub postgres: PostgresConfig,
    pub workers: WorkerConfig,
    pub security: SecurityConfig,
    pub rate_limits: RateLimitConfig,
    pub limits: ResourceLimits,
    pub storage: StorageConfig,
    pub email: EmailConfig,
    pub billing: BillingConfig,
    pub instance_connect: Option<Box<StripeConnectConfig>>,
    pub commerce: CommerceConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Development,
    Test,
    Production,
}

#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub bind_address: String,
    pub public_base_url: Url,
    pub allowed_origins: Vec<Url>,
    /// Immediate reverse-proxy networks allowed to supply X-Forwarded-For.
    /// An empty list makes the transport peer the only client identity.
    pub trusted_proxy_cidrs: Vec<IpNet>,
}

#[derive(Debug)]
pub struct PostgresConfig {
    pub database_url: SecretString,
    pub min_connections: u32,
    pub max_connections: u32,
    pub acquire_timeout_ms: u64,
    pub idle_timeout_seconds: u64,
    pub max_lifetime_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub node_id: NodeId,
    pub node_name: String,
    pub database_root: PathBuf,
    pub backup_root: PathBuf,
    pub metrics_root: PathBuf,
    pub binary: PathBuf,
    pub max_processes: u16,
    pub queue_capacity_per_worker: u16,
}

#[derive(Debug)]
pub struct SecurityConfig {
    pub master_key_base64: SecretString,
    pub backup_master_key_base64: SecretString,
    pub cursor_hmac_key: SecretString,
    pub bootstrap_token: SecretString,
}

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    /// Conservative source-IP admission policy used before authentication.
    pub pre_auth_capacity: u32,
    pub pre_auth_refill_tokens_per_second: f64,
    /// Independent project/user/API-key policy for authenticated work.
    pub execution_capacity: u32,
    pub execution_refill_tokens_per_second: f64,
    /// Shared bounds for durable PostgreSQL bucket state.
    pub idle_ttl_seconds: u64,
    pub max_entries: usize,
}

#[derive(Debug)]
pub struct StorageConfig {
    pub endpoint: Url,
    pub public_endpoint: Url,
    pub region: String,
    pub bucket: String,
    pub access_key_id: SecretString,
    pub secret_access_key: SecretString,
    pub allow_private_network: bool,
}

#[derive(Debug)]
pub struct EmailConfig {
    pub transport: EmailTransportConfig,
    pub from_address: String,
}

#[derive(Debug)]
pub enum EmailTransportConfig {
    Resend { api_key: SecretString },
    Smtp { host: String, port: u16 },
}

#[derive(Debug)]
pub enum BillingConfig {
    Disabled,
    Stripe(Box<StripeBillingConfig>),
}

#[derive(Debug)]
pub enum CommerceConfig {
    ByoOnly,
    StripeConnect(Box<StripeConnectConfig>),
}

pub struct StripeConnectConfig {
    pub secret_key: SecretString,
    pub webhook_secret: SecretString,
}

impl std::fmt::Debug for StripeConnectConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StripeConnectConfig")
            .field("secret_key", &"[REDACTED]")
            .field("webhook_secret", &"[REDACTED]")
            .finish()
    }
}

pub struct StripeBillingConfig {
    /// Optional legacy bootstrap credentials. First-run instance setup stores
    /// the selected BYO credentials encrypted in PostgreSQL; catalog and meter
    /// configuration remain non-secret runtime configuration.
    pub secret_key: Option<SecretString>,
    pub webhook_secret: Option<SecretString>,
    pub pro_base_price_id: String,
    pub reads_meter: StripeUsageMeterConfig,
    pub writes_meter: StripeUsageMeterConfig,
    pub storage_meter: StripeUsageMeterConfig,
    pub mau_meter: StripeUsageMeterConfig,
    pub pro_billing_unit: PlatformBillingUnit,
    pub success_url: Url,
    pub cancel_url: Url,
    pub portal_return_url: Url,
}

#[derive(Clone, Debug)]
pub struct StripeUsageMeterConfig {
    pub event_name: String,
    pub meter_id: String,
    pub payg_price_id: String,
    pub pro_price_id: String,
}

impl std::fmt::Debug for StripeBillingConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StripeBillingConfig")
            .field(
                "secret_key",
                &self.secret_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "webhook_secret",
                &self.webhook_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("pro_base_price_id", &self.pro_base_price_id)
            .field("reads_meter", &self.reads_meter)
            .field("writes_meter", &self.writes_meter)
            .field("storage_meter", &self.storage_meter)
            .field("mau_meter", &self.mau_meter)
            .field("pro_billing_unit", &self.pro_billing_unit)
            .field("success_url", &self.success_url)
            .field("cancel_url", &self.cancel_url)
            .field("portal_return_url", &self.portal_return_url)
            .finish()
    }
}

impl AppConfig {
    pub fn from_environment() -> Result<Self, ConfigError> {
        let values: HashMap<String, String> = std::env::vars()
            .filter(|(key, _)| key.starts_with("FFDB_"))
            .collect();
        Self::from_map(&values)
    }

    pub fn from_map(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let environment = match optional(values, "FFDB_ENVIRONMENT", "development").as_str() {
            "development" => Environment::Development,
            "test" => Environment::Test,
            "production" => Environment::Production,
            value => return Err(ConfigError::InvalidValue("FFDB_ENVIRONMENT", value.into())),
        };
        let email_transport = match optional(
            values,
            "FFDB_EMAIL_TRANSPORT",
            if environment == Environment::Production {
                "resend"
            } else {
                "smtp"
            },
        )
        .as_str()
        {
            "resend" => EmailTransportConfig::Resend {
                api_key: SecretString::from(required(values, "FFDB_RESEND_API_KEY")?),
            },
            "smtp" if environment != Environment::Production => EmailTransportConfig::Smtp {
                host: optional(values, "FFDB_SMTP_HOST", "localhost"),
                port: parse(values, "FFDB_SMTP_PORT", 1025)?,
            },
            value => {
                return Err(ConfigError::InvalidValue(
                    "FFDB_EMAIL_TRANSPORT",
                    value.into(),
                ));
            }
        };
        let allowed_origins = parse_origins(
            values,
            "FFDB_CORS_ALLOWED_ORIGINS",
            if environment == Environment::Production {
                ""
            } else {
                "http://localhost:5173"
            },
        )?;
        let trusted_proxy_cidrs = parse_cidrs(values, "FFDB_TRUSTED_PROXY_CIDRS", "")?;
        let billing = parse_billing(values)?;
        let instance_connect = parse_connect_credentials(
            values,
            "FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY",
            "FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET",
        )?;
        let commerce = parse_commerce(values)?;
        let config = Self {
            environment,
            http: HttpConfig {
                bind_address: optional(values, "FFDB_HTTP_BIND", "0.0.0.0:8080"),
                public_base_url: parse_url(
                    values,
                    "FFDB_PUBLIC_BASE_URL",
                    "http://localhost:8080",
                )?,
                allowed_origins,
                trusted_proxy_cidrs,
            },
            postgres: PostgresConfig {
                database_url: SecretString::from(required(values, "FFDB_DATABASE_URL")?),
                min_connections: parse(values, "FFDB_POSTGRES_MIN_CONNECTIONS", 2)?,
                max_connections: parse(values, "FFDB_POSTGRES_MAX_CONNECTIONS", 20)?,
                acquire_timeout_ms: parse(values, "FFDB_POSTGRES_ACQUIRE_TIMEOUT_MS", 5_000)?,
                idle_timeout_seconds: parse(values, "FFDB_POSTGRES_IDLE_TIMEOUT_SECONDS", 600)?,
                max_lifetime_seconds: parse(values, "FFDB_POSTGRES_MAX_LIFETIME_SECONDS", 1_800)?,
            },
            workers: WorkerConfig {
                node_id: NodeId(
                    uuid::Uuid::parse_str(&optional(
                        values,
                        "FFDB_NODE_ID",
                        "01965555-0000-7000-8000-000000000001",
                    ))
                    .map_err(|_| {
                        ConfigError::InvalidValue(
                            "FFDB_NODE_ID",
                            optional(values, "FFDB_NODE_ID", "invalid"),
                        )
                    })?,
                ),
                node_name: optional(values, "FFDB_NODE_NAME", "ffdb-local-01"),
                database_root: PathBuf::from(optional(
                    values,
                    "FFDB_DATABASE_ROOT",
                    "./data/projects",
                )),
                backup_root: PathBuf::from(optional(values, "FFDB_BACKUP_ROOT", "./data/backups")),
                metrics_root: PathBuf::from(optional(
                    values,
                    "FFDB_METRICS_ROOT",
                    "./data/metrics",
                )),
                binary: PathBuf::from(optional(
                    values,
                    "FFDB_DATABASE_WORKER",
                    "ffdb-database-worker",
                )),
                max_processes: parse(values, "FFDB_WORKER_MAX_PROCESSES", 8)?,
                queue_capacity_per_worker: parse(values, "FFDB_WORKER_QUEUE_CAPACITY", 32)?,
            },
            security: SecurityConfig {
                master_key_base64: SecretString::from(required(values, "FFDB_MASTER_KEY")?),
                backup_master_key_base64: SecretString::from(required(
                    values,
                    "FFDB_BACKUP_MASTER_KEY",
                )?),
                cursor_hmac_key: SecretString::from(required(values, "FFDB_CURSOR_HMAC_KEY")?),
                bootstrap_token: SecretString::from(optional(
                    values,
                    "FFDB_BOOTSTRAP_TOKEN",
                    "local-bootstrap-token-change-before-production",
                )),
            },
            rate_limits: RateLimitConfig {
                pre_auth_capacity: parse(values, "FFDB_RATE_LIMIT_PRE_AUTH_CAPACITY", 120)?,
                pre_auth_refill_tokens_per_second: parse(
                    values,
                    "FFDB_RATE_LIMIT_PRE_AUTH_REFILL_PER_SECOND",
                    2.0,
                )?,
                execution_capacity: parse(values, "FFDB_RATE_LIMIT_EXECUTION_CAPACITY", 2_000)?,
                execution_refill_tokens_per_second: parse(
                    values,
                    "FFDB_RATE_LIMIT_EXECUTION_REFILL_PER_SECOND",
                    200.0,
                )?,
                idle_ttl_seconds: parse(values, "FFDB_RATE_LIMIT_IDLE_TTL_SECONDS", 3_600)?,
                max_entries: parse(values, "FFDB_RATE_LIMIT_MAX_ENTRIES", 1_000_000)?,
            },
            limits: ResourceLimits::default(),
            storage: StorageConfig {
                endpoint: parse_url(values, "FFDB_S3_ENDPOINT", "http://localhost:9000")?,
                public_endpoint: parse_url(
                    values,
                    "FFDB_S3_PUBLIC_ENDPOINT",
                    "http://localhost:9000",
                )?,
                region: optional(values, "FFDB_S3_REGION", "us-east-1"),
                bucket: optional(values, "FFDB_S3_BUCKET", "ffdb"),
                access_key_id: SecretString::from(required(values, "FFDB_S3_ACCESS_KEY_ID")?),
                secret_access_key: SecretString::from(required(
                    values,
                    "FFDB_S3_SECRET_ACCESS_KEY",
                )?),
                allow_private_network: parse(values, "FFDB_S3_ALLOW_PRIVATE_NETWORK", false)?,
            },
            email: EmailConfig {
                transport: email_transport,
                from_address: required(values, "FFDB_EMAIL_FROM")?,
            },
            billing,
            instance_connect,
            commerce,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.limits
            .validate()
            .map_err(|error| ConfigError::Limits(error.to_string()))?;
        if self.workers.max_processes == 0
            || self.workers.queue_capacity_per_worker == 0
            || self.postgres.min_connections > self.postgres.max_connections
            || self.postgres.max_connections == 0
            || self.postgres.max_connections > 1_000
            || !(100..=120_000).contains(&self.postgres.acquire_timeout_ms)
            || !(1..=86_400).contains(&self.postgres.idle_timeout_seconds)
            || !(30..=86_400).contains(&self.postgres.max_lifetime_seconds)
            || self.rate_limits.pre_auth_capacity == 0
            || self.rate_limits.pre_auth_capacity > 1_000_000
            || !self
                .rate_limits
                .pre_auth_refill_tokens_per_second
                .is_finite()
            || self.rate_limits.pre_auth_refill_tokens_per_second <= 0.0
            || self.rate_limits.pre_auth_refill_tokens_per_second > 100_000.0
            || self.rate_limits.execution_capacity == 0
            || self.rate_limits.execution_capacity > 1_000_000
            || !self
                .rate_limits
                .execution_refill_tokens_per_second
                .is_finite()
            || self.rate_limits.execution_refill_tokens_per_second <= 0.0
            || self.rate_limits.execution_refill_tokens_per_second > 100_000.0
            || !(60..=86_400).contains(&self.rate_limits.idle_ttl_seconds)
            || self.rate_limits.max_entries == 0
            || self.rate_limits.max_entries > 10_000_000
        {
            return Err(ConfigError::InvalidLimit);
        }
        if !trusted_database_root(&self.workers.database_root)
            || !trusted_database_root(&self.workers.backup_root)
            || !trusted_database_root(&self.workers.metrics_root)
        {
            return Err(ConfigError::UnsafeDatabaseRoot);
        }
        if self.workers.node_name.is_empty() || self.workers.node_name.len() > 128 {
            return Err(ConfigError::InvalidValue(
                "FFDB_NODE_NAME",
                self.workers.node_name.clone(),
            ));
        }
        validate_internal_provider_url(
            &self.storage.endpoint,
            self.environment,
            self.storage.allow_private_network,
        )?;
        validate_provider_url(&self.storage.public_endpoint, self.environment)?;
        if self.environment == Environment::Production
            && self.http.public_base_url.scheme() != "https"
        {
            return Err(ConfigError::HttpsRequired("FFDB_PUBLIC_BASE_URL"));
        }
        for origin in &self.http.allowed_origins {
            if !matches!(origin.scheme(), "http" | "https")
                || origin.host_str().is_none()
                || origin.username() != ""
                || origin.password().is_some()
                || origin.query().is_some()
                || origin.fragment().is_some()
                || origin.path() != "/"
                || self.environment == Environment::Production && origin.scheme() != "https"
            {
                return Err(ConfigError::InvalidValue(
                    "FFDB_CORS_ALLOWED_ORIGINS",
                    origin.as_str().to_owned(),
                ));
            }
        }
        let master_key = base64::engine::general_purpose::STANDARD
            .decode(secrecy::ExposeSecret::expose_secret(
                &self.security.master_key_base64,
            ))
            .map_err(|_| ConfigError::InvalidMasterKey)?;
        if master_key.len() != 32 {
            return Err(ConfigError::InvalidMasterKey);
        }
        let backup_master_key = base64::engine::general_purpose::STANDARD
            .decode(secrecy::ExposeSecret::expose_secret(
                &self.security.backup_master_key_base64,
            ))
            .map_err(|_| ConfigError::InvalidBackupMasterKey)?;
        if backup_master_key.len() != 32 {
            return Err(ConfigError::InvalidBackupMasterKey);
        }
        if secrecy::ExposeSecret::expose_secret(&self.security.cursor_hmac_key).len() < 32 {
            return Err(ConfigError::WeakCursorKey);
        }
        let bootstrap_token = secrecy::ExposeSecret::expose_secret(&self.security.bootstrap_token);
        if bootstrap_token.len() < 32
            || (self.environment == Environment::Production
                && bootstrap_token == "local-bootstrap-token-change-before-production")
        {
            return Err(ConfigError::WeakBootstrapToken);
        }
        if !self.email.from_address.contains('@') {
            return Err(ConfigError::InvalidValue(
                "FFDB_EMAIL_FROM",
                self.email.from_address.clone(),
            ));
        }
        match &self.email.transport {
            EmailTransportConfig::Resend { api_key } => {
                if secrecy::ExposeSecret::expose_secret(api_key).len() < 16 {
                    return Err(ConfigError::InvalidValue(
                        "FFDB_RESEND_API_KEY",
                        "[REDACTED]".to_owned(),
                    ));
                }
            }
            EmailTransportConfig::Smtp { host, port } => {
                if self.environment == Environment::Production
                    || *port == 0
                    || !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "mailpit")
                {
                    return Err(ConfigError::InvalidValue(
                        "FFDB_EMAIL_TRANSPORT",
                        "smtp".to_owned(),
                    ));
                }
            }
        }
        if let BillingConfig::Stripe(stripe) = &self.billing {
            if let (Some(secret), Some(webhook)) = (&stripe.secret_key, &stripe.webhook_secret) {
                let secret = secrecy::ExposeSecret::expose_secret(secret);
                let webhook = secrecy::ExposeSecret::expose_secret(webhook);
                if !secret.starts_with("sk_") || secret.len() < 16 {
                    return Err(ConfigError::InvalidValue(
                        "FFDB_STRIPE_SECRET_KEY",
                        "[REDACTED]".into(),
                    ));
                }
                if !webhook.starts_with("whsec_") || webhook.len() < 16 {
                    return Err(ConfigError::InvalidValue(
                        "FFDB_STRIPE_WEBHOOK_SECRET",
                        "[REDACTED]".into(),
                    ));
                }
            } else if stripe.secret_key.is_some() || stripe.webhook_secret.is_some() {
                return Err(ConfigError::Missing(if stripe.secret_key.is_none() {
                    "FFDB_STRIPE_SECRET_KEY"
                } else {
                    "FFDB_STRIPE_WEBHOOK_SECRET"
                }));
            }
            for (key, price) in stripe_price_values(stripe) {
                if !price.starts_with("price_")
                    || price.len() < 10
                    || !price
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err(ConfigError::InvalidValue(key, price.clone()));
                }
            }
            for (event_key, meter_key, meter) in [
                (
                    "FFDB_STRIPE_READS_EVENT_NAME",
                    "FFDB_STRIPE_READS_METER_ID",
                    &stripe.reads_meter,
                ),
                (
                    "FFDB_STRIPE_WRITES_EVENT_NAME",
                    "FFDB_STRIPE_WRITES_METER_ID",
                    &stripe.writes_meter,
                ),
                (
                    "FFDB_STRIPE_STORAGE_EVENT_NAME",
                    "FFDB_STRIPE_STORAGE_METER_ID",
                    &stripe.storage_meter,
                ),
                (
                    "FFDB_STRIPE_MAU_EVENT_NAME",
                    "FFDB_STRIPE_MAU_METER_ID",
                    &stripe.mau_meter,
                ),
            ] {
                if !(3..=100).contains(&meter.event_name.len())
                    || !meter.event_name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
                {
                    return Err(ConfigError::InvalidValue(
                        event_key,
                        meter.event_name.clone(),
                    ));
                }
                if !valid_stripe_identifier(&meter.meter_id, "mtr_") {
                    return Err(ConfigError::InvalidValue(meter_key, meter.meter_id.clone()));
                }
            }
            for (key, url) in [
                ("FFDB_BILLING_SUCCESS_URL", &stripe.success_url),
                ("FFDB_BILLING_CANCEL_URL", &stripe.cancel_url),
                ("FFDB_BILLING_PORTAL_RETURN_URL", &stripe.portal_return_url),
            ] {
                validate_browser_return_url(url, self.environment)
                    .map_err(|_| ConfigError::InvalidValue(key, url.to_string()))?;
            }
        }
        if let CommerceConfig::StripeConnect(connect) = &self.commerce {
            validate_connect_credentials(
                connect,
                "FFDB_COMMERCE_STRIPE_CONNECT_SECRET_KEY",
                "FFDB_COMMERCE_STRIPE_CONNECT_WEBHOOK_SECRET",
            )?;
        }
        if let Some(connect) = &self.instance_connect {
            validate_connect_credentials(
                connect,
                "FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY",
                "FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET",
            )?;
        }
        Ok(())
    }
}

fn validate_connect_credentials(
    connect: &StripeConnectConfig,
    secret_key: &'static str,
    webhook_key: &'static str,
) -> Result<(), ConfigError> {
    let secret = secrecy::ExposeSecret::expose_secret(&connect.secret_key);
    let webhook = secrecy::ExposeSecret::expose_secret(&connect.webhook_secret);
    if !secret.starts_with("sk_") || secret.len() < 16 {
        return Err(ConfigError::InvalidValue(secret_key, "[REDACTED]".into()));
    }
    if !webhook.starts_with("whsec_") || webhook.len() < 16 {
        return Err(ConfigError::InvalidValue(webhook_key, "[REDACTED]".into()));
    }
    Ok(())
}

fn parse_connect_credentials(
    values: &HashMap<String, String>,
    secret_key: &'static str,
    webhook_key: &'static str,
) -> Result<Option<Box<StripeConnectConfig>>, ConfigError> {
    let secret_present = values
        .get(secret_key)
        .is_some_and(|value| !value.is_empty());
    let webhook_present = values
        .get(webhook_key)
        .is_some_and(|value| !value.is_empty());
    if !secret_present && !webhook_present {
        return Ok(None);
    }
    if secret_present != webhook_present {
        return Err(ConfigError::Missing(if secret_present {
            webhook_key
        } else {
            secret_key
        }));
    }
    Ok(Some(Box::new(StripeConnectConfig {
        secret_key: SecretString::from(required(values, secret_key)?),
        webhook_secret: SecretString::from(required(values, webhook_key)?),
    })))
}

fn parse_commerce(values: &HashMap<String, String>) -> Result<CommerceConfig, ConfigError> {
    const SECRET: &str = "FFDB_COMMERCE_STRIPE_CONNECT_SECRET_KEY";
    const WEBHOOK: &str = "FFDB_COMMERCE_STRIPE_CONNECT_WEBHOOK_SECRET";
    match parse_connect_credentials(values, SECRET, WEBHOOK)? {
        Some(connect) => Ok(CommerceConfig::StripeConnect(connect)),
        None => Ok(CommerceConfig::ByoOnly),
    }
}

fn parse_billing(values: &HashMap<String, String>) -> Result<BillingConfig, ConfigError> {
    const REQUIRED: [&str; 20] = [
        "FFDB_STRIPE_PRO_BASE_PRICE_ID",
        "FFDB_STRIPE_READS_EVENT_NAME",
        "FFDB_STRIPE_READS_METER_ID",
        "FFDB_STRIPE_PAYG_READS_PRICE_ID",
        "FFDB_STRIPE_PRO_READS_PRICE_ID",
        "FFDB_STRIPE_WRITES_EVENT_NAME",
        "FFDB_STRIPE_WRITES_METER_ID",
        "FFDB_STRIPE_PAYG_WRITES_PRICE_ID",
        "FFDB_STRIPE_PRO_WRITES_PRICE_ID",
        "FFDB_STRIPE_STORAGE_EVENT_NAME",
        "FFDB_STRIPE_STORAGE_METER_ID",
        "FFDB_STRIPE_PAYG_STORAGE_PRICE_ID",
        "FFDB_STRIPE_PRO_STORAGE_PRICE_ID",
        "FFDB_STRIPE_MAU_EVENT_NAME",
        "FFDB_STRIPE_MAU_METER_ID",
        "FFDB_STRIPE_PAYG_MAU_PRICE_ID",
        "FFDB_STRIPE_PRO_MAU_PRICE_ID",
        "FFDB_BILLING_SUCCESS_URL",
        "FFDB_BILLING_CANCEL_URL",
        "FFDB_BILLING_PORTAL_RETURN_URL",
    ];
    let secret_present = values
        .get("FFDB_STRIPE_SECRET_KEY")
        .is_some_and(|value| !value.is_empty());
    let webhook_present = values
        .get("FFDB_STRIPE_WEBHOOK_SECRET")
        .is_some_and(|value| !value.is_empty());
    if REQUIRED
        .iter()
        .all(|key| values.get(*key).is_none_or(String::is_empty))
        && !secret_present
        && !webhook_present
    {
        return Ok(BillingConfig::Disabled);
    }
    if secret_present != webhook_present {
        return Err(ConfigError::Missing(if secret_present {
            "FFDB_STRIPE_WEBHOOK_SECRET"
        } else {
            "FFDB_STRIPE_SECRET_KEY"
        }));
    }
    let pro_billing_unit =
        match optional(values, "FFDB_STRIPE_PRO_BILLING_UNIT", "organization").as_str() {
            "organization" => PlatformBillingUnit::Organization,
            "seat" => PlatformBillingUnit::Seat,
            value => {
                return Err(ConfigError::InvalidValue(
                    "FFDB_STRIPE_PRO_BILLING_UNIT",
                    value.into(),
                ));
            }
        };
    Ok(BillingConfig::Stripe(Box::new(StripeBillingConfig {
        secret_key: secret_present
            .then(|| required(values, "FFDB_STRIPE_SECRET_KEY"))
            .transpose()?
            .map(SecretString::from),
        webhook_secret: webhook_present
            .then(|| required(values, "FFDB_STRIPE_WEBHOOK_SECRET"))
            .transpose()?
            .map(SecretString::from),
        pro_base_price_id: required(values, "FFDB_STRIPE_PRO_BASE_PRICE_ID")?,
        reads_meter: parse_stripe_meter(
            values,
            "FFDB_STRIPE_READS_EVENT_NAME",
            "FFDB_STRIPE_READS_METER_ID",
            "FFDB_STRIPE_PAYG_READS_PRICE_ID",
            "FFDB_STRIPE_PRO_READS_PRICE_ID",
        )?,
        writes_meter: parse_stripe_meter(
            values,
            "FFDB_STRIPE_WRITES_EVENT_NAME",
            "FFDB_STRIPE_WRITES_METER_ID",
            "FFDB_STRIPE_PAYG_WRITES_PRICE_ID",
            "FFDB_STRIPE_PRO_WRITES_PRICE_ID",
        )?,
        storage_meter: parse_stripe_meter(
            values,
            "FFDB_STRIPE_STORAGE_EVENT_NAME",
            "FFDB_STRIPE_STORAGE_METER_ID",
            "FFDB_STRIPE_PAYG_STORAGE_PRICE_ID",
            "FFDB_STRIPE_PRO_STORAGE_PRICE_ID",
        )?,
        mau_meter: parse_stripe_meter(
            values,
            "FFDB_STRIPE_MAU_EVENT_NAME",
            "FFDB_STRIPE_MAU_METER_ID",
            "FFDB_STRIPE_PAYG_MAU_PRICE_ID",
            "FFDB_STRIPE_PRO_MAU_PRICE_ID",
        )?,
        pro_billing_unit,
        success_url: parse_url(values, "FFDB_BILLING_SUCCESS_URL", "")?,
        cancel_url: parse_url(values, "FFDB_BILLING_CANCEL_URL", "")?,
        portal_return_url: parse_url(values, "FFDB_BILLING_PORTAL_RETURN_URL", "")?,
    })))
}

fn parse_stripe_meter(
    values: &HashMap<String, String>,
    event_key: &'static str,
    meter_key: &'static str,
    payg_price_key: &'static str,
    pro_price_key: &'static str,
) -> Result<StripeUsageMeterConfig, ConfigError> {
    Ok(StripeUsageMeterConfig {
        event_name: required(values, event_key)?,
        meter_id: required(values, meter_key)?,
        payg_price_id: required(values, payg_price_key)?,
        pro_price_id: required(values, pro_price_key)?,
    })
}

fn stripe_price_values(stripe: &StripeBillingConfig) -> [(&'static str, &String); 9] {
    [
        ("FFDB_STRIPE_PRO_BASE_PRICE_ID", &stripe.pro_base_price_id),
        (
            "FFDB_STRIPE_PAYG_READS_PRICE_ID",
            &stripe.reads_meter.payg_price_id,
        ),
        (
            "FFDB_STRIPE_PRO_READS_PRICE_ID",
            &stripe.reads_meter.pro_price_id,
        ),
        (
            "FFDB_STRIPE_PAYG_WRITES_PRICE_ID",
            &stripe.writes_meter.payg_price_id,
        ),
        (
            "FFDB_STRIPE_PRO_WRITES_PRICE_ID",
            &stripe.writes_meter.pro_price_id,
        ),
        (
            "FFDB_STRIPE_PAYG_STORAGE_PRICE_ID",
            &stripe.storage_meter.payg_price_id,
        ),
        (
            "FFDB_STRIPE_PRO_STORAGE_PRICE_ID",
            &stripe.storage_meter.pro_price_id,
        ),
        (
            "FFDB_STRIPE_PAYG_MAU_PRICE_ID",
            &stripe.mau_meter.payg_price_id,
        ),
        (
            "FFDB_STRIPE_PRO_MAU_PRICE_ID",
            &stripe.mau_meter.pro_price_id,
        ),
    ]
}

fn valid_stripe_identifier(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && (prefix.len() + 4..=255).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn required(values: &HashMap<String, String>, key: &'static str) -> Result<String, ConfigError> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(ConfigError::Missing(key))
}

fn optional(values: &HashMap<String, String>, key: &'static str, default: &str) -> String {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| default.into())
}

fn parse<T: std::str::FromStr>(
    values: &HashMap<String, String>,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError> {
    match values.get(key) {
        Some(value) => value
            .parse()
            .map_err(|_| ConfigError::InvalidValue(key, value.clone())),
        None => Ok(default),
    }
}

fn parse_url(
    values: &HashMap<String, String>,
    key: &'static str,
    default: &str,
) -> Result<Url, ConfigError> {
    let value = optional(values, key, default);
    Url::parse(&value).map_err(|_| ConfigError::InvalidValue(key, value))
}

fn parse_origins(
    values: &HashMap<String, String>,
    key: &'static str,
    default: &str,
) -> Result<Vec<Url>, ConfigError> {
    let raw = optional(values, key, default);
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            Url::parse(value).map_err(|_| ConfigError::InvalidValue(key, value.to_owned()))
        })
        .collect()
}

fn parse_cidrs(
    values: &HashMap<String, String>,
    key: &'static str,
    default: &str,
) -> Result<Vec<IpNet>, ConfigError> {
    let raw = optional(values, key, default);
    let cidrs = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<IpNet>()
                .map_err(|_| ConfigError::InvalidValue(key, value.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if cidrs.len() > 32 || cidrs.iter().any(|cidr| cidr.prefix_len() == 0) {
        return Err(ConfigError::InvalidValue(key, raw));
    }
    Ok(cidrs)
}

fn validate_browser_return_url(url: &Url, environment: Environment) -> Result<(), ConfigError> {
    if url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || !matches!(url.scheme(), "http" | "https")
        || environment == Environment::Production && url.scheme() != "https"
    {
        return Err(ConfigError::UnsafeProviderUrl);
    }
    Ok(())
}

fn validate_provider_url(url: &Url, environment: Environment) -> Result<(), ConfigError> {
    if url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(ConfigError::UnsafeProviderUrl);
    }
    if environment == Environment::Development && url.scheme() == "http" {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(ConfigError::HttpsRequired("FFDB_S3_ENDPOINT"));
    }
    if let Some(host) = url.host_str()
        && (host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(is_private_or_local))
    {
        return Err(ConfigError::UnsafeProviderUrl);
    }
    Ok(())
}

fn validate_internal_provider_url(
    url: &Url,
    environment: Environment,
    allow_private_network: bool,
) -> Result<(), ConfigError> {
    if environment != Environment::Production || !allow_private_network {
        return validate_provider_url(url, environment);
    }
    if url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.scheme() != "https"
    {
        return Err(ConfigError::UnsafeProviderUrl);
    }
    if url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|address| {
                address.is_loopback() || address.is_unspecified() || address.is_multicast()
            })
    }) {
        return Err(ConfigError::UnsafeProviderUrl);
    }
    Ok(())
}

fn is_private_or_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value == Ipv4Addr::UNSPECIFIED
        }
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_unique_local()
                || value.is_unicast_link_local()
                || value == Ipv6Addr::UNSPECIFIED
        }
    }
}

fn trusted_database_root(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path != Path::new("/")
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required configuration: {0}")]
    Missing(&'static str),
    #[error("invalid value for {0}")]
    InvalidValue(&'static str, String),
    #[error("{0} must use HTTPS")]
    HttpsRequired(&'static str),
    #[error("provider URL is not allowed")]
    UnsafeProviderUrl,
    #[error("database root must be a specific trusted path without parent traversal")]
    UnsafeDatabaseRoot,
    #[error("one or more concurrency limits are zero")]
    InvalidLimit,
    #[error("master key must be 32 bytes of base64")]
    InvalidMasterKey,
    #[error("backup master key must be 32 bytes of base64")]
    InvalidBackupMasterKey,
    #[error("cursor HMAC key must contain at least 32 characters")]
    WeakCursorKey,
    #[error("bootstrap token must contain at least 32 characters")]
    WeakBootstrapToken,
    #[error("resource limits are invalid: {0}")]
    Limits(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> HashMap<String, String> {
        HashMap::from([
            (
                "FFDB_DATABASE_URL".into(),
                "postgres://ffdb:ffdb@localhost/ffdb".into(),
            ),
            (
                "FFDB_MASTER_KEY".into(),
                base64::engine::general_purpose::STANDARD.encode([7_u8; 32]),
            ),
            (
                "FFDB_BACKUP_MASTER_KEY".into(),
                base64::engine::general_purpose::STANDARD.encode([8_u8; 32]),
            ),
            (
                "FFDB_CURSOR_HMAC_KEY".into(),
                "a-very-long-cursor-key-that-is-not-short".into(),
            ),
            ("FFDB_S3_ACCESS_KEY_ID".into(), "minio".into()),
            ("FFDB_S3_SECRET_ACCESS_KEY".into(), "minio-secret".into()),
            (
                "FFDB_RESEND_API_KEY".into(),
                "re_test_01234567890123456789".into(),
            ),
            (
                "FFDB_EMAIL_FROM".into(),
                "FFDB <noreply@example.test>".into(),
            ),
        ])
    }

    fn production() -> HashMap<String, String> {
        let mut values = baseline();
        values.insert("FFDB_ENVIRONMENT".into(), "production".into());
        values.insert(
            "FFDB_PUBLIC_BASE_URL".into(),
            "https://api.example.test".into(),
        );
        values.insert(
            "FFDB_CORS_ALLOWED_ORIGINS".into(),
            "https://portal.example.test".into(),
        );
        values.insert(
            "FFDB_BOOTSTRAP_TOKEN".into(),
            "production-bootstrap-token-with-at-least-32-bytes".into(),
        );
        values.insert("FFDB_S3_ENDPOINT".into(), "https://s3.amazonaws.com".into());
        values.insert(
            "FFDB_S3_PUBLIC_ENDPOINT".into(),
            "https://objects.example.test".into(),
        );
        values
    }

    fn with_stripe(mut values: HashMap<String, String>) -> HashMap<String, String> {
        values.extend([
            (
                "FFDB_STRIPE_SECRET_KEY".into(),
                "sk_test_12345678901234567890".into(), // gitleaks:allow -- synthetic Stripe test fixture
            ),
            (
                "FFDB_STRIPE_WEBHOOK_SECRET".into(),
                "whsec_12345678901234567890".into(),
            ),
            (
                "FFDB_STRIPE_PRO_BASE_PRICE_ID".into(),
                "price_pro_base_1234".into(),
            ),
            ("FFDB_STRIPE_READS_EVENT_NAME".into(), "ffdb_reads".into()),
            ("FFDB_STRIPE_READS_METER_ID".into(), "mtr_reads_1234".into()),
            (
                "FFDB_STRIPE_PAYG_READS_PRICE_ID".into(),
                "price_payg_reads_1234".into(),
            ),
            (
                "FFDB_STRIPE_PRO_READS_PRICE_ID".into(),
                "price_pro_reads_1234".into(),
            ),
            ("FFDB_STRIPE_WRITES_EVENT_NAME".into(), "ffdb_writes".into()),
            (
                "FFDB_STRIPE_WRITES_METER_ID".into(),
                "mtr_writes_1234".into(),
            ),
            (
                "FFDB_STRIPE_PAYG_WRITES_PRICE_ID".into(),
                "price_payg_writes_1234".into(),
            ),
            (
                "FFDB_STRIPE_PRO_WRITES_PRICE_ID".into(),
                "price_pro_writes_1234".into(),
            ),
            (
                "FFDB_STRIPE_STORAGE_EVENT_NAME".into(),
                "ffdb_storage_byte_hours".into(),
            ),
            (
                "FFDB_STRIPE_STORAGE_METER_ID".into(),
                "mtr_storage_1234".into(),
            ),
            (
                "FFDB_STRIPE_PAYG_STORAGE_PRICE_ID".into(),
                "price_payg_storage_1234".into(),
            ),
            (
                "FFDB_STRIPE_PRO_STORAGE_PRICE_ID".into(),
                "price_pro_storage_1234".into(),
            ),
            ("FFDB_STRIPE_MAU_EVENT_NAME".into(), "ffdb_mau".into()),
            ("FFDB_STRIPE_MAU_METER_ID".into(), "mtr_mau_1234".into()),
            (
                "FFDB_STRIPE_PAYG_MAU_PRICE_ID".into(),
                "price_payg_mau_1234".into(),
            ),
            (
                "FFDB_STRIPE_PRO_MAU_PRICE_ID".into(),
                "price_pro_mau_1234".into(),
            ),
            (
                "FFDB_BILLING_SUCCESS_URL".into(),
                "http://localhost:5173/app/billing/success".into(),
            ),
            (
                "FFDB_BILLING_CANCEL_URL".into(),
                "http://localhost:5173/app/billing/cancel".into(),
            ),
            (
                "FFDB_BILLING_PORTAL_RETURN_URL".into(),
                "http://localhost:5173/app/billing".into(),
            ),
        ]);
        values
    }

    #[test]
    fn development_defaults_validate() -> Result<(), ConfigError> {
        let config = AppConfig::from_map(&baseline())?;
        assert!(matches!(
            config.email.transport,
            EmailTransportConfig::Smtp { port: 1025, .. }
        ));
        assert_eq!(config.postgres.min_connections, 2);
        assert_eq!(config.postgres.max_connections, 20);
        assert_eq!(config.postgres.acquire_timeout_ms, 5_000);
        assert_eq!(config.postgres.idle_timeout_seconds, 600);
        assert_eq!(config.postgres.max_lifetime_seconds, 1_800);
        assert_eq!(config.rate_limits.pre_auth_capacity, 120);
        assert_eq!(config.rate_limits.pre_auth_refill_tokens_per_second, 2.0);
        assert_eq!(config.rate_limits.execution_capacity, 2_000);
        assert_eq!(config.rate_limits.execution_refill_tokens_per_second, 200.0);
        assert_eq!(config.rate_limits.idle_ttl_seconds, 3_600);
        assert_eq!(config.rate_limits.max_entries, 1_000_000);
        assert!(config.http.trusted_proxy_cidrs.is_empty());
        Ok(())
    }

    #[test]
    fn trusted_proxy_cidrs_are_explicit_and_bounded() -> Result<(), ConfigError> {
        let mut values = baseline();
        values.insert(
            "FFDB_TRUSTED_PROXY_CIDRS".into(),
            "127.0.0.1/32, ::1/128".into(),
        );
        let config = AppConfig::from_map(&values)?;
        assert_eq!(
            config
                .http
                .trusted_proxy_cidrs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["127.0.0.1/32", "::1/128"]
        );

        values.insert("FFDB_TRUSTED_PROXY_CIDRS".into(), "0.0.0.0/0".into());
        assert!(AppConfig::from_map(&values).is_err());
        values.insert("FFDB_TRUSTED_PROXY_CIDRS".into(), "not-a-cidr".into());
        assert!(AppConfig::from_map(&values).is_err());
        Ok(())
    }

    #[test]
    fn durable_rate_limit_policies_are_independent_and_bounded() -> Result<(), ConfigError> {
        let mut values = baseline();
        values.insert("FFDB_RATE_LIMIT_PRE_AUTH_CAPACITY".into(), "60".into());
        values.insert(
            "FFDB_RATE_LIMIT_PRE_AUTH_REFILL_PER_SECOND".into(),
            "1.5".into(),
        );
        values.insert("FFDB_RATE_LIMIT_EXECUTION_CAPACITY".into(), "4000".into());
        values.insert(
            "FFDB_RATE_LIMIT_EXECUTION_REFILL_PER_SECOND".into(),
            "350".into(),
        );
        let config = AppConfig::from_map(&values)?;
        assert_eq!(config.rate_limits.pre_auth_capacity, 60);
        assert_eq!(config.rate_limits.pre_auth_refill_tokens_per_second, 1.5);
        assert_eq!(config.rate_limits.execution_capacity, 4_000);
        assert_eq!(config.rate_limits.execution_refill_tokens_per_second, 350.0);

        for (name, value) in [
            ("FFDB_RATE_LIMIT_PRE_AUTH_CAPACITY", "0"),
            ("FFDB_RATE_LIMIT_PRE_AUTH_REFILL_PER_SECOND", "0"),
            ("FFDB_RATE_LIMIT_EXECUTION_CAPACITY", "1000001"),
            ("FFDB_RATE_LIMIT_EXECUTION_REFILL_PER_SECOND", "NaN"),
            ("FFDB_RATE_LIMIT_IDLE_TTL_SECONDS", "59"),
            ("FFDB_RATE_LIMIT_MAX_ENTRIES", "10000001"),
        ] {
            let mut values = baseline();
            values.insert(name.into(), value.into());
            assert!(matches!(
                AppConfig::from_map(&values),
                Err(ConfigError::InvalidLimit)
            ));
        }
        Ok(())
    }

    #[test]
    fn postgres_pool_limits_are_bounded_and_consistent() -> Result<(), ConfigError> {
        let mut values = baseline();
        values.insert("FFDB_POSTGRES_MIN_CONNECTIONS".into(), "5".into());
        values.insert("FFDB_POSTGRES_MAX_CONNECTIONS".into(), "4".into());
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::InvalidLimit)
        ));

        let mut values = baseline();
        values.insert("FFDB_POSTGRES_ACQUIRE_TIMEOUT_MS".into(), "99".into());
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::InvalidLimit)
        ));

        let mut values = baseline();
        values.insert("FFDB_POSTGRES_IDLE_TIMEOUT_SECONDS".into(), "601".into());
        values.insert("FFDB_POSTGRES_MAX_LIFETIME_SECONDS".into(), "2400".into());
        let config = AppConfig::from_map(&values)?;
        assert_eq!(config.postgres.idle_timeout_seconds, 601);
        assert_eq!(config.postgres.max_lifetime_seconds, 2_400);
        Ok(())
    }

    #[test]
    fn stripe_configuration_is_all_or_none_and_redacted() -> Result<(), ConfigError> {
        let mut partial = baseline();
        partial.insert(
            "FFDB_STRIPE_SECRET_KEY".into(),
            "sk_test_12345678901234567890".into(), // gitleaks:allow -- synthetic Stripe test fixture
        );
        assert!(matches!(
            AppConfig::from_map(&partial),
            Err(ConfigError::Missing("FFDB_STRIPE_WEBHOOK_SECRET"))
        ));

        let config = AppConfig::from_map(&with_stripe(baseline()))?;
        assert!(matches!(
            &config.billing,
            BillingConfig::Stripe(stripe)
                if stripe.pro_billing_unit == PlatformBillingUnit::Organization
        ));
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk_test_12345678901234567890")); // gitleaks:allow -- verifies redaction
        assert!(!debug.contains("whsec_12345678901234567890"));
        Ok(())
    }

    #[test]
    fn commerce_connect_is_optional_all_or_none_and_redacted() -> Result<(), ConfigError> {
        let config = AppConfig::from_map(&baseline())?;
        assert!(matches!(config.commerce, CommerceConfig::ByoOnly));

        let mut partial = baseline();
        partial.insert(
            "FFDB_COMMERCE_STRIPE_CONNECT_SECRET_KEY".into(),
            "sk_test_connect_1234567890".into(),
        );
        assert!(matches!(
            AppConfig::from_map(&partial),
            Err(ConfigError::Missing(
                "FFDB_COMMERCE_STRIPE_CONNECT_WEBHOOK_SECRET"
            ))
        ));

        partial.insert(
            "FFDB_COMMERCE_STRIPE_CONNECT_WEBHOOK_SECRET".into(),
            "whsec_connect_1234567890".into(),
        );
        let config = AppConfig::from_map(&partial)?;
        assert!(matches!(config.commerce, CommerceConfig::StripeConnect(_)));
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk_test_connect_1234567890"));
        assert!(!debug.contains("whsec_connect_1234567890"));
        Ok(())
    }

    #[test]
    fn instance_connect_credentials_are_explicit_and_separate() -> Result<(), ConfigError> {
        let config = AppConfig::from_map(&baseline())?;
        assert!(config.instance_connect.is_none());

        let mut partial = baseline();
        partial.insert(
            "FFDB_INSTANCE_STRIPE_CONNECT_SECRET_KEY".into(),
            "sk_test_instance_1234567890".into(),
        );
        assert!(matches!(
            AppConfig::from_map(&partial),
            Err(ConfigError::Missing(
                "FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET"
            ))
        ));
        partial.insert(
            "FFDB_INSTANCE_STRIPE_CONNECT_WEBHOOK_SECRET".into(),
            "whsec_instance_1234567890".into(),
        );
        let config = AppConfig::from_map(&partial)?;
        assert!(config.instance_connect.is_some());
        assert!(matches!(config.commerce, CommerceConfig::ByoOnly));
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk_test_instance_1234567890"));
        assert!(!debug.contains("whsec_instance_1234567890"));
        Ok(())
    }

    #[test]
    fn production_billing_redirects_require_https() {
        let mut values = with_stripe(production());
        values.insert(
            "FFDB_BILLING_SUCCESS_URL".into(),
            "http://portal.example.test/app/billing/success".into(),
        );
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::InvalidValue("FFDB_BILLING_SUCCESS_URL", _))
        ));
    }

    #[test]
    fn production_rejects_plain_smtp() {
        let mut values = baseline();
        values.insert("FFDB_ENVIRONMENT".into(), "production".into());
        values.insert("FFDB_EMAIL_TRANSPORT".into(), "smtp".into());
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::InvalidValue("FFDB_EMAIL_TRANSPORT", _))
        ));
    }

    #[test]
    fn cors_origins_are_explicit_and_production_https_only() -> Result<(), ConfigError> {
        let development = AppConfig::from_map(&baseline())?;
        assert_eq!(
            development.http.allowed_origins[0]
                .origin()
                .ascii_serialization(),
            "http://localhost:5173"
        );
        let mut values = baseline();
        values.insert("FFDB_ENVIRONMENT".into(), "production".into());
        values.insert(
            "FFDB_PUBLIC_BASE_URL".into(),
            "https://api.example.test".into(),
        );
        values.insert("FFDB_S3_ENDPOINT".into(), "https://s3.amazonaws.com".into());
        values.insert(
            "FFDB_S3_PUBLIC_ENDPOINT".into(),
            "https://objects.example.test".into(),
        );
        values.insert(
            "FFDB_CORS_ALLOWED_ORIGINS".into(),
            "http://portal.example.test".into(),
        );
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::InvalidValue("FFDB_CORS_ALLOWED_ORIGINS", _))
        ));
        Ok(())
    }

    #[test]
    fn production_rejects_local_provider_and_http_public_url() {
        let mut values = baseline();
        values.insert("FFDB_ENVIRONMENT".into(), "production".into());
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::HttpsRequired(_))
        ));
    }

    #[test]
    fn production_private_s3_requires_explicit_internal_only_opt_in() -> Result<(), ConfigError> {
        let mut values = production();
        values.insert("FFDB_S3_ENDPOINT".into(), "https://10.12.0.8".into());
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::UnsafeProviderUrl)
        ));

        values.insert("FFDB_S3_ALLOW_PRIVATE_NETWORK".into(), "true".into());
        let config = AppConfig::from_map(&values)?;
        assert!(config.storage.allow_private_network);

        values.insert("FFDB_S3_ENDPOINT".into(), "http://10.12.0.8".into());
        assert!(AppConfig::from_map(&values).is_err());
        values.insert(
            "FFDB_S3_ENDPOINT".into(),
            "https://s3.internal.example".into(),
        );
        values.insert("FFDB_S3_PUBLIC_ENDPOINT".into(), "https://10.12.0.9".into());
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::UnsafeProviderUrl)
        ));
        Ok(())
    }

    #[test]
    fn database_root_with_parent_traversal_is_rejected() {
        let mut values = baseline();
        values.insert(
            "FFDB_DATABASE_ROOT".into(),
            "../../tmp/caller-controlled".into(),
        );
        assert!(matches!(
            AppConfig::from_map(&values),
            Err(ConfigError::UnsafeDatabaseRoot)
        ));
    }

    #[test]
    fn production_may_use_a_specific_absolute_root() {
        let mut values = baseline();
        values.insert("FFDB_DATABASE_ROOT".into(), "/var/lib/ffdb/projects".into());
        assert!(AppConfig::from_map(&values).is_ok());
    }
}
