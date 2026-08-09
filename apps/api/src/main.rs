use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use ffdb_api::{
    ApiState, CommerceConnectConfig, CommerceService, CommerceServiceConfig, DurableRateLimiter,
    InstanceService, InstanceServiceConfig, InstanceStripeBillingConfig,
    InstanceStripeProviderCatalog, InstanceStripeUsageEventConfig, ManagementState,
    ManagementStateConfig, ObservabilityService, OutboxAuthEmailDispatcher, ProjectAuthState,
    StorageService, UsageMeteringService, UsageReportingConfig, UsageReportingService,
    WorkerMetadataAuthorizer, router, spawn_security_state_maintenance,
};
use ffdb_audit::PgAuditSink;
use ffdb_auth::{AeadSigningKeyEnvelope, PgCredentialVerifier, PgSigningKeyStore, SigningKeyStore};
use ffdb_billing::{
    PlatformBillingProvider, StripeUsageMeterConfig as ProviderStripeUsageMeterConfig, UsageMetric,
};
use ffdb_config::{AppConfig, BillingConfig, CommerceConfig, EmailTransportConfig, Environment};
use ffdb_control_plane::{PgRegistry, Registry as _};
use ffdb_database_router::{DatabaseExecutor, DatabaseRouter, ProcessWorkerExecutor};
use ffdb_email::{
    EmailMessageCipher, EmailTransport, OutboxWorkerHandle, PgEmailService, ResendTransport,
    SmtpTransport,
};
use ffdb_object_storage::{S3Provider, S3ProviderConfig, StorageLimits};
use ffdb_observability::Metrics;
use ffdb_rate_limits::{PgTokenBucketLimiter, TokenBucketConfig};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing::{error, info};
use zeroize::Zeroizing;

mod control_plane_migrations;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ffdb=info,tower_http=info".into()),
        )
        .try_init()
        .map_err(|error| anyhow!("failed to initialize tracing: {error}"))?;

    let config = AppConfig::from_environment().context("invalid FFDB configuration")?;
    let pool = PgPoolOptions::new()
        .min_connections(config.postgres.min_connections)
        .max_connections(config.postgres.max_connections)
        .acquire_timeout(Duration::from_millis(config.postgres.acquire_timeout_ms))
        .idle_timeout(Some(Duration::from_secs(
            config.postgres.idle_timeout_seconds,
        )))
        .max_lifetime(Some(Duration::from_secs(
            config.postgres.max_lifetime_seconds,
        )))
        .connect(config.postgres.database_url.expose_secret())
        .await
        .context("PostgreSQL is unavailable")?;
    control_plane_migrations::migrator()
        .run(&pool)
        .await
        .context("control-plane migrations failed")?;

    let registry = Arc::new(PgRegistry::new(pool.clone()));
    registry
        .register_node(config.workers.node_id, &config.workers.node_name)
        .await
        .context("worker node registration failed")?;

    let worker_binary = canonical_existing(&config.workers.binary)
        .await
        .context("database worker binary is unavailable")?;
    let database_root = canonical_directory(&config.workers.database_root)
        .await
        .context("database root is unavailable")?;
    let backup_root = canonical_directory(&config.workers.backup_root)
        .await
        .context("backup root is unavailable")?;
    let metrics_root = canonical_directory(&config.workers.metrics_root)
        .await
        .context("organization metrics root is unavailable")?;
    let process_executor = Arc::new(
        ProcessWorkerExecutor::new(
            worker_binary,
            database_root.clone(),
            backup_root.clone(),
            secrecy::SecretString::from(
                config
                    .security
                    .backup_master_key_base64
                    .expose_secret()
                    .to_owned(),
            ),
            config.workers.node_id,
            usize::from(config.workers.max_processes),
            usize::from(config.workers.queue_capacity_per_worker)
                .saturating_mul(usize::from(config.workers.max_processes)),
        )
        .map_err(|error| anyhow!("invalid worker executor configuration: {error}"))?,
    );
    let executor: Arc<dyn DatabaseExecutor> = process_executor.clone();
    let (observability, observability_worker) =
        ObservabilityService::spawn(pool.clone(), process_executor, database_root, backup_root);

    let master_key = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(config.security.master_key_base64.expose_secret())
            .context("master key decoding failed")?,
    );
    let envelope = AeadSigningKeyEnvelope::new(master_key.as_slice().to_vec(), 1)
        .map_err(|error| anyhow!("signing-key envelope setup failed: {error}"))?;
    let api_key_pepper = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.api-key-pepper.v1",
    ));
    let platform_session_pepper = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.platform-session-pepper.v1",
    ));
    let invitation_pepper = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.organization-invitation-pepper.v1",
    ));
    let usage_subject_key: [u8; 32] = derive_secret(
        master_key.as_slice(),
        b"ffdb.organization-metrics-subject.v1",
    )
    .try_into()
    .map_err(|_| anyhow!("organization metrics key derivation failed"))?;
    let (billing_template, pro_billing_unit) = match &config.billing {
        BillingConfig::Disabled => {
            let usage_events = default_usage_events();
            sync_usage_event_catalog(&pool, &usage_events)
                .await
                .context("billing usage event catalog synchronization failed")?;
            (
                Some(InstanceStripeBillingConfig {
                    byo_catalog: None,
                    usage_events,
                    pro_billing_unit: ffdb_protocol::PlatformBillingUnit::Organization,
                    success_url: config.http.public_base_url.join("/app/billing/success")?,
                    cancel_url: config.http.public_base_url.join("/app/billing/cancel")?,
                    portal_return_url: config.http.public_base_url.join("/app/billing")?,
                }),
                ffdb_protocol::PlatformBillingUnit::Organization,
            )
        }
        BillingConfig::Stripe(stripe) => {
            sync_usage_catalog(&pool, stripe)
                .await
                .context("billing usage catalog synchronization failed")?;
            (
                Some(InstanceStripeBillingConfig {
                    byo_catalog: Some(InstanceStripeProviderCatalog {
                        product_id: None,
                        pro_base_price_id: stripe.pro_base_price_id.clone(),
                        usage_meters: vec![
                            provider_meter(UsageMetric::Reads, &stripe.reads_meter),
                            provider_meter(UsageMetric::Writes, &stripe.writes_meter),
                            provider_meter(UsageMetric::StorageByteHours, &stripe.storage_meter),
                            provider_meter(UsageMetric::MonthlyActiveUsers, &stripe.mau_meter),
                        ],
                    }),
                    usage_events: vec![
                        usage_event(UsageMetric::Reads, &stripe.reads_meter),
                        usage_event(UsageMetric::Writes, &stripe.writes_meter),
                        usage_event(UsageMetric::StorageByteHours, &stripe.storage_meter),
                        usage_event(UsageMetric::MonthlyActiveUsers, &stripe.mau_meter),
                    ],
                    pro_billing_unit: stripe.pro_billing_unit,
                    success_url: stripe.success_url.clone(),
                    cancel_url: stripe.cancel_url.clone(),
                    portal_return_url: stripe.portal_return_url.clone(),
                }),
                stripe.pro_billing_unit,
            )
        }
    };
    let (connect_secret_key, connect_webhook_secret) = match &config.instance_connect {
        None => (None, None),
        Some(connect) => (
            Some(secrecy::SecretString::from(
                connect.secret_key.expose_secret().to_owned(),
            )),
            Some(secrecy::SecretString::from(
                connect.webhook_secret.expose_secret().to_owned(),
            )),
        ),
    };
    let instance = Arc::new(
        InstanceService::new(
            pool.clone(),
            InstanceServiceConfig {
                master_key: master_key.as_slice().to_vec(),
                key_version: 1,
                connect_secret_key,
                connect_webhook_secret,
                billing: billing_template,
            },
        )
        .map_err(|error| anyhow!("instance service setup failed: {error:?}"))?,
    );
    instance
        .reload_billing_provider()
        .await
        .map_err(|error| anyhow!("instance billing activation failed: {error:?}"))?;
    let billing_provider: Option<Arc<dyn PlatformBillingProvider>> = instance
        .billing_provider()
        .map(|provider| provider as Arc<dyn PlatformBillingProvider>);
    let commerce_connect = match &config.commerce {
        CommerceConfig::ByoOnly => None,
        CommerceConfig::StripeConnect(stripe) => Some(CommerceConnectConfig {
            secret_key: secrecy::SecretString::from(stripe.secret_key.expose_secret().to_owned()),
            webhook_secret: secrecy::SecretString::from(
                stripe.webhook_secret.expose_secret().to_owned(),
            ),
        }),
    };
    let commerce = Arc::new(
        CommerceService::new(
            pool.clone(),
            CommerceServiceConfig {
                master_key: master_key.as_slice().to_vec(),
                key_version: 1,
                public_base_url: config.http.public_base_url.clone(),
                connect: commerce_connect,
            },
        )
        .map_err(|error| anyhow!("project commerce setup failed: {error}"))?,
    );
    let management = Arc::new(
        ManagementState::new(
            pool.clone(),
            ManagementStateConfig {
                platform_session_pepper: platform_session_pepper.as_slice().to_vec(),
                api_key_pepper: api_key_pepper.as_slice().to_vec(),
                invitation_pepper: invitation_pepper.as_slice().to_vec(),
                signing_key_envelope: envelope.clone(),
                bootstrap_token: config.security.bootstrap_token.expose_secret().to_owned(),
                node_id: config.workers.node_id,
                public_base_url: config.http.public_base_url.clone(),
                email_from_address: config.email.from_address.clone(),
                billing_provider: billing_provider.clone(),
                pro_billing_unit,
            },
        )
        .map_err(|error| anyhow!("management service setup failed: {error}"))?,
    );
    let signing_keys: Arc<dyn SigningKeyStore> =
        Arc::new(PgSigningKeyStore::new(pool.clone(), envelope));
    let issuer = config.http.public_base_url.join("v1/auth/")?.to_string();
    let one_time_pepper = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.one-time-token-pepper.v1",
    ));
    let refresh_pepper = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.refresh-token-pepper.v1",
    ));
    let email_cipher_key = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.email-outbox.v1",
    ));
    let email_service = Arc::new(PgEmailService::new(
        pool.clone(),
        EmailMessageCipher::new(email_cipher_key.as_slice(), 1)
            .map_err(|error| anyhow!("email outbox encryption setup failed: {error}"))?,
    ));
    let email_transport: Arc<dyn EmailTransport> = match &config.email.transport {
        EmailTransportConfig::Resend { api_key } => Arc::new(
            ResendTransport::new(
                url::Url::parse("https://api.resend.com/")?,
                api_key.expose_secret().to_owned(),
                false,
            )
            .await
            .map_err(|error| anyhow!("Resend transport setup failed: {error}"))?,
        ),
        EmailTransportConfig::Smtp { host, port } => Arc::new(
            SmtpTransport::development(host.clone(), *port)
                .await
                .map_err(|error| anyhow!("development SMTP transport setup failed: {error}"))?,
        ),
    };
    let email_outbox = OutboxWorkerHandle::spawn(email_service.clone(), email_transport);
    let project_auth = Arc::new(
        ProjectAuthState::new(
            pool.clone(),
            one_time_pepper.as_slice().to_vec(),
            refresh_pepper.as_slice().to_vec(),
            signing_keys.clone(),
            issuer.clone(),
            "ffdb".into(),
            Arc::new(OutboxAuthEmailDispatcher::new(
                email_service.clone(),
                config.email.from_address.clone(),
                config.http.public_base_url.clone(),
            )),
        )
        .map_err(|error| anyhow!("project authentication setup failed: {error}"))?,
    );
    let credentials = Arc::new(
        PgCredentialVerifier::new(
            pool.clone(),
            signing_keys,
            api_key_pepper.as_slice().to_vec(),
            issuer,
            "ffdb".into(),
        )
        .map_err(|error| anyhow!("credential verifier setup failed: {error}"))?,
    );
    let rate_namespace_secret = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.rate-limit-namespace.v1",
    ));
    let rate_limit_idle_ttl_ms = i64::try_from(config.rate_limits.idle_ttl_seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .ok_or_else(|| anyhow!("rate limiter idle TTL is out of range"))?;
    let pre_auth_rate_limiter = PgTokenBucketLimiter::new(
        pool.clone(),
        TokenBucketConfig {
            capacity: config.rate_limits.pre_auth_capacity,
            refill_tokens_per_second: config.rate_limits.pre_auth_refill_tokens_per_second,
            idle_ttl_ms: rate_limit_idle_ttl_ms,
            max_entries: config.rate_limits.max_entries,
        },
    )
    .map_err(|error| anyhow!("pre-auth rate limiter setup failed: {error}"))?;
    let execution_rate_limiter = PgTokenBucketLimiter::new(
        pool.clone(),
        TokenBucketConfig {
            capacity: config.rate_limits.execution_capacity,
            refill_tokens_per_second: config.rate_limits.execution_refill_tokens_per_second,
            idle_ttl_ms: rate_limit_idle_ttl_ms,
            max_entries: config.rate_limits.max_entries,
        },
    )
    .map_err(|error| anyhow!("execution rate limiter setup failed: {error}"))?;
    let security_state_maintenance =
        spawn_security_state_maintenance(pool.clone(), pre_auth_rate_limiter.clone());
    let durable_rate_limiter = DurableRateLimiter::new(
        pre_auth_rate_limiter,
        execution_rate_limiter,
        rate_namespace_secret.as_slice().to_vec(),
    )
    .map_err(|error| anyhow!("rate limiter setup failed: {error}"))?;

    let usage_metering = Arc::new(UsageMeteringService::new(
        pool.clone(),
        metrics_root.clone(),
        usage_subject_key,
    ));

    let storage_config = S3ProviderConfig::new(
        config.storage.endpoint.clone(),
        config.storage.region.clone(),
        config.storage.bucket.clone(),
        config.storage.access_key_id.expose_secret().to_owned(),
        config.storage.secret_access_key.expose_secret().to_owned(),
    )
    .with_public_endpoint(config.storage.public_endpoint.clone());
    let storage_config = if matches!(
        config.environment,
        Environment::Development | Environment::Test
    ) {
        storage_config
            .allow_insecure_development_service(
                config
                    .storage
                    .endpoint
                    .host_str()
                    .ok_or_else(|| anyhow!("S3 endpoint host is missing"))?,
            )
            .allow_insecure_development_public_host(
                config
                    .storage
                    .public_endpoint
                    .host_str()
                    .ok_or_else(|| anyhow!("S3 public endpoint host is missing"))?,
            )
    } else {
        storage_config
    };
    let storage_config = if config.storage.allow_private_network {
        storage_config.allow_private_network_service(
            config
                .storage
                .endpoint
                .host_str()
                .ok_or_else(|| anyhow!("S3 endpoint host is missing"))?,
        )
    } else {
        storage_config
    };
    let storage_provider = S3Provider::new(storage_config)
        .map_err(|error| anyhow!("S3 storage provider setup failed: {error}"))?;
    let storage_authorizer = WorkerMetadataAuthorizer::new(
        registry.clone() as Arc<dyn DatabaseRouter>,
        executor.clone(),
        config.limits.clone(),
    )
    .with_usage_metering(usage_metering.clone());
    let storage_grant_secret = Zeroizing::new(derive_secret(
        master_key.as_slice(),
        b"ffdb.storage-grant.v1",
    ));
    let storage = Arc::new(
        StorageService::new(
            storage_authorizer,
            storage_provider,
            storage_grant_secret.as_slice(),
            StorageLimits {
                signed_url_ttl_ms: 5 * 60 * 1_000,
                grant_ttl_ms: 10 * 60 * 1_000,
                max_pending_reservations: 4_096,
            },
        )
        .map_err(|error| anyhow!("storage gateway setup failed: {error}"))?,
    );

    let usage_reporting = match billing_provider {
        Some(provider) => Some(
            Arc::new(
                UsageReportingService::new(
                    pool.clone(),
                    provider,
                    UsageReportingConfig::production(metrics_root.clone(), usage_subject_key),
                )
                .map_err(|error| anyhow!("usage reporting setup failed: {error}"))?,
            )
            .spawn(std::time::Duration::from_secs(60))
            .map_err(|error| anyhow!("usage reporting worker failed to start: {error}"))?,
        ),
        None => None,
    };
    let state = ApiState {
        router: registry as Arc<dyn DatabaseRouter>,
        executor,
        credentials,
        limits: config.limits,
        metrics: Some(Arc::new(
            Metrics::new().map_err(|error| anyhow!("metrics setup failed: {error}"))?,
        )),
        observability: Some(observability),
        management: Some(management),
        project_auth: Some(project_auth),
        storage: Some(storage),
        email: Some(email_service),
        usage_metering: Some(usage_metering),
        commerce: Some(commerce),
        instance: Some(instance),
        cors_allowed_origins: config
            .http
            .allowed_origins
            .iter()
            .map(|origin| origin.origin().ascii_serialization())
            .collect(),
        trusted_proxy_cidrs: config.http.trusted_proxy_cidrs.clone(),
        rate_limiter: Some(Arc::new(durable_rate_limiter)),
        audit: Arc::new(PgAuditSink::new(pool.clone())),
        readiness_pool: Some(pool),
    };
    let listener = TcpListener::bind(&config.http.bind_address)
        .await
        .with_context(|| format!("failed to bind {}", config.http.bind_address))?;
    info!(address = %config.http.bind_address, "FFDB API listening");
    let server_result = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("HTTP server failed");
    email_outbox.shutdown().await;
    if let Some(worker) = usage_reporting {
        worker.shutdown().await;
    }
    observability_worker.shutdown().await;
    security_state_maintenance.abort();
    server_result?;
    Ok(())
}

fn provider_meter(
    metric: UsageMetric,
    meter: &ffdb_config::StripeUsageMeterConfig,
) -> ProviderStripeUsageMeterConfig {
    ProviderStripeUsageMeterConfig {
        metric,
        event_name: meter.event_name.clone(),
        meter_id: meter.meter_id.clone(),
        payg_price_id: meter.payg_price_id.clone(),
        pro_price_id: meter.pro_price_id.clone(),
    }
}

fn usage_event(
    metric: UsageMetric,
    meter: &ffdb_config::StripeUsageMeterConfig,
) -> InstanceStripeUsageEventConfig {
    InstanceStripeUsageEventConfig {
        metric,
        event_name: meter.event_name.clone(),
    }
}

fn default_usage_events() -> Vec<InstanceStripeUsageEventConfig> {
    vec![
        InstanceStripeUsageEventConfig {
            metric: UsageMetric::Reads,
            event_name: "ffdb_reads".into(),
        },
        InstanceStripeUsageEventConfig {
            metric: UsageMetric::Writes,
            event_name: "ffdb_writes".into(),
        },
        InstanceStripeUsageEventConfig {
            metric: UsageMetric::StorageByteHours,
            event_name: "ffdb_storage_kilobyte_hours".into(),
        },
        InstanceStripeUsageEventConfig {
            metric: UsageMetric::MonthlyActiveUsers,
            event_name: "ffdb_monthly_active_users".into(),
        },
    ]
}

async fn sync_usage_event_catalog(
    pool: &sqlx::PgPool,
    events: &[InstanceStripeUsageEventConfig],
) -> Result<()> {
    for event in events {
        let (display_name, unit_name) = match event.metric {
            UsageMetric::Reads => ("Successful SQL read statements", "statement"),
            UsageMetric::Writes => ("Successful SQL write statements", "statement"),
            UsageMetric::StorageByteHours => (
                "Logical storage usage reported in decimal kilobyte-hours",
                "kilobyte_hour",
            ),
            UsageMetric::MonthlyActiveUsers => ("Distinct monthly active users", "user"),
        };
        sqlx::query(
            "INSERT INTO billing_usage_catalog \
                (metric,display_name,event_name,provider_meter_id,payg_price_id,pro_price_id, \
                 aggregation,unit_name) VALUES ($1,$2,$3,NULL,NULL,NULL,'sum',$4) \
             ON CONFLICT (metric) DO UPDATE SET display_name=EXCLUDED.display_name, \
               event_name=EXCLUDED.event_name,aggregation='sum',unit_name=EXCLUDED.unit_name, \
               active=true,updated_at=now()",
        )
        .bind(event.metric.name())
        .bind(display_name)
        .bind(&event.event_name)
        .bind(unit_name)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn sync_usage_catalog(
    pool: &sqlx::PgPool,
    stripe: &ffdb_config::StripeBillingConfig,
) -> Result<()> {
    for (metric, display_name, aggregation, unit_name, meter) in [
        (
            "reads",
            "Successful SQL read statements",
            "sum",
            "statement",
            &stripe.reads_meter,
        ),
        (
            "writes",
            "Successful SQL write statements",
            "sum",
            "statement",
            &stripe.writes_meter,
        ),
        (
            "storage_byte_hours",
            "Logical storage usage reported in decimal kilobyte-hours",
            "sum",
            "kilobyte_hour",
            &stripe.storage_meter,
        ),
        (
            "monthly_active_users",
            "Distinct monthly active users",
            "sum",
            "user",
            &stripe.mau_meter,
        ),
    ] {
        sqlx::query(
            "INSERT INTO billing_usage_catalog \
             (metric,display_name,event_name,provider_meter_id,payg_price_id,pro_price_id,aggregation,unit_name) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             ON CONFLICT (metric) DO UPDATE SET display_name=EXCLUDED.display_name, \
               event_name=EXCLUDED.event_name,provider_meter_id=EXCLUDED.provider_meter_id, \
               payg_price_id=EXCLUDED.payg_price_id,pro_price_id=EXCLUDED.pro_price_id, \
               aggregation=EXCLUDED.aggregation,unit_name=EXCLUDED.unit_name,active=true,updated_at=now()",
        )
        .bind(metric)
        .bind(display_name)
        .bind(&meter.event_name)
        .bind(&meter.meter_id)
        .bind(&meter.payg_price_id)
        .bind(&meter.pro_price_id)
        .bind(aggregation)
        .bind(unit_name)
        .execute(pool)
        .await?;
    }
    Ok(())
}

fn derive_secret(master_key: &[u8], domain: &[u8]) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    digest.update(master_key);
    digest.finalize().to_vec()
}

async fn canonical_existing(path: &Path) -> std::io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    tokio::fs::canonicalize(path).await
}

async fn canonical_directory(path: &Path) -> std::io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    tokio::fs::create_dir_all(&path).await?;
    tokio::fs::canonicalize(path).await
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        error!("failed to install shutdown signal handler");
    }
    info!("graceful shutdown requested");
}
