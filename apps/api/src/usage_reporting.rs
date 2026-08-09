//! Durable organization-usage reporting and provider reconciliation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ffdb_billing::{
    BillingError, PlatformBillingProvider, STORAGE_BILLING_UNIT_BYTES, UsageMeterEvent,
    UsageMetric, UsageSummaryInput,
};
use ffdb_org_metrics::{
    BillingPeriod, BillingPeriodStatus, ClaimedReport, IngestOutcome, MetricsConfig, MetricsError,
    OrganizationMetricsStore, ReconciliationStatus, ReportRequest, UsageDimension, UsageSummary,
};
use ffdb_protocol::OrganizationId;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row as _};
use thiserror::Error;
use uuid::Uuid;

const HOUR_MS: i64 = 60 * 60 * 1_000;
const DAY_MS: i64 = 24 * HOUR_MS;

#[derive(Clone, Debug)]
pub struct UsageReportingConfig {
    pub metrics_root: PathBuf,
    pub subject_key: [u8; 32],
    pub report_lag_ms: i64,
    pub provider_consistency_delay_ms: i64,
    pub period_cutoff_grace_ms: i64,
    pub outbox_lease_ms: i64,
    pub retry_base_ms: i64,
    pub max_periods_per_cycle: u32,
    pub max_reports_per_period: u32,
}

impl UsageReportingConfig {
    #[must_use]
    pub fn production(metrics_root: PathBuf, subject_key: [u8; 32]) -> Self {
        Self {
            metrics_root,
            subject_key,
            report_lag_ms: 5 * 60 * 1_000,
            provider_consistency_delay_ms: 15 * 60 * 1_000,
            period_cutoff_grace_ms: 2 * DAY_MS,
            outbox_lease_ms: 60_000,
            retry_base_ms: 30_000,
            max_periods_per_cycle: 1_000,
            max_reports_per_period: 100,
        }
    }

    fn validate(&self) -> Result<(), ReportingCycleError> {
        if !self.metrics_root.is_absolute()
            || self.subject_key.iter().all(|byte| *byte == 0)
            || !(1_000..=HOUR_MS).contains(&self.report_lag_ms)
            || !(1_000..=DAY_MS).contains(&self.provider_consistency_delay_ms)
            || !(HOUR_MS..=7 * DAY_MS).contains(&self.period_cutoff_grace_ms)
            || !(1_000..=10 * 60 * 1_000).contains(&self.outbox_lease_ms)
            || !(1_000..=HOUR_MS).contains(&self.retry_base_ms)
            || !(1..=10_000).contains(&self.max_periods_per_cycle)
            || !(1..=100).contains(&self.max_reports_per_period)
        {
            return Err(ReportingCycleError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReportingCycleSummary {
    pub organizations: u32,
    pub periods: u32,
    pub reports_enqueued: u32,
    pub reports_sent: u32,
    pub reports_failed: u32,
    pub periods_reconciled: u32,
    pub organizations_blocked: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportingHealth {
    Healthy,
    Degraded,
    Reconciling,
    Blocked,
}

impl ReportingHealth {
    const fn rank(self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::Degraded => 1,
            Self::Reconciling => 2,
            Self::Blocked => 3,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Reconciling => "reconciling",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug)]
struct PaidBillingPeriod {
    organization_id: OrganizationId,
    customer_id: String,
    period: BillingPeriod,
}

#[derive(Clone, Debug)]
struct UsageCatalog {
    event_names: HashMap<UsageMetric, String>,
}

impl UsageCatalog {
    fn event_name(&self, metric: UsageMetric) -> Result<&str, ReportingCycleError> {
        self.event_names
            .get(&metric)
            .map(String::as_str)
            .ok_or(ReportingCycleError::InvalidCatalog)
    }

    fn validate(&self) -> Result<(), ReportingCycleError> {
        if self.event_names.len() != UsageMetric::ALL.len()
            || UsageMetric::ALL
                .iter()
                .any(|metric| self.event_name(*metric).is_err())
        {
            return Err(ReportingCycleError::InvalidCatalog);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PeriodCycleOutcome {
    health: Option<ReportingHealth>,
    reports_enqueued: u32,
    reports_sent: u32,
    reports_failed: u32,
    reconciled: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ReportingCycleError {
    #[error("usage reporting configuration is invalid")]
    InvalidConfiguration,
    #[error("usage reporting catalog is incomplete or invalid")]
    InvalidCatalog,
    #[error("usage reporting repository is unavailable")]
    RepositoryUnavailable,
    #[error("organization metrics datastore is unavailable")]
    MetricsUnavailable,
    #[error("usage reporting totals are inconsistent")]
    InconsistentTotals,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderUsageError {
    Retryable,
    Rejected,
}

#[async_trait]
trait UsageProvider: Send + Sync {
    async fn report(&self, event: &UsageMeterEvent) -> Result<(), ProviderUsageError>;

    async fn summary(&self, input: &UsageSummaryInput) -> Result<u64, ProviderUsageError>;
}

#[derive(Clone)]
struct BillingProviderAdapter {
    provider: Arc<dyn PlatformBillingProvider>,
}

impl std::fmt::Debug for BillingProviderAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("BillingProviderAdapter").finish()
    }
}

#[async_trait]
impl UsageProvider for BillingProviderAdapter {
    async fn report(&self, event: &UsageMeterEvent) -> Result<(), ProviderUsageError> {
        self.provider
            .report_usage(event)
            .await
            .map_err(map_provider_error)
    }

    async fn summary(&self, input: &UsageSummaryInput) -> Result<u64, ProviderUsageError> {
        self.provider
            .usage_summary(input)
            .await
            .map(|summary| summary.aggregated_value)
            .map_err(map_provider_error)
    }
}

#[async_trait]
trait UsageReportingRepository: Send + Sync {
    async fn usage_catalog(&self) -> Result<UsageCatalog, ReportingCycleError>;

    async fn paid_periods(
        &self,
        now_ms: i64,
        cutoff_grace_ms: i64,
        limit: u32,
    ) -> Result<Vec<PaidBillingPeriod>, ReportingCycleError>;

    async fn update_health(
        &self,
        organization_id: OrganizationId,
        health: ReportingHealth,
        now_ms: i64,
    ) -> Result<(), ReportingCycleError>;
}

#[derive(Clone, Debug)]
struct PgUsageReportingRepository {
    pool: PgPool,
}

#[async_trait]
impl UsageReportingRepository for PgUsageReportingRepository {
    async fn usage_catalog(&self) -> Result<UsageCatalog, ReportingCycleError> {
        let rows = sqlx::query(
            "SELECT metric,event_name FROM billing_usage_catalog WHERE active=true ORDER BY metric",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| ReportingCycleError::RepositoryUnavailable)?;
        let mut event_names = HashMap::new();
        for row in rows {
            let metric = parse_metric(
                row.try_get("metric")
                    .map_err(|_| ReportingCycleError::InvalidCatalog)?,
            )?;
            let event_name: String = row
                .try_get("event_name")
                .map_err(|_| ReportingCycleError::InvalidCatalog)?;
            if event_name.is_empty() || event_names.insert(metric, event_name).is_some() {
                return Err(ReportingCycleError::InvalidCatalog);
            }
        }
        let catalog = UsageCatalog { event_names };
        catalog.validate()?;
        Ok(catalog)
    }

    async fn paid_periods(
        &self,
        now_ms: i64,
        cutoff_grace_ms: i64,
        limit: u32,
    ) -> Result<Vec<PaidBillingPeriod>, ReportingCycleError> {
        let rows = sqlx::query(
            "WITH candidate_periods AS ( \
               SELECT a.organization_id,a.provider_customer_id customer_id, \
                      (extract(epoch FROM a.current_period_start)*1000)::bigint period_start_ms, \
                      (extract(epoch FROM a.current_period_end)*1000)::bigint period_end_ms, \
                      a.usage_reporting_last_success_at reporting_priority \
               FROM organization_billing_accounts a \
               WHERE a.provider='stripe' AND a.tier IN ('pay_as_you_go','pro') \
                 AND a.current_period_start IS NOT NULL AND a.current_period_end IS NOT NULL \
               UNION \
               SELECT i.organization_id,a.provider_customer_id customer_id, \
                      (extract(epoch FROM i.period_start)*1000)::bigint period_start_ms, \
                      (extract(epoch FROM i.period_end)*1000)::bigint period_end_ms, \
                      a.usage_reporting_last_success_at reporting_priority \
               FROM organization_billing_invoices i \
               JOIN organization_billing_accounts a ON a.organization_id=i.organization_id \
               WHERE i.provider='stripe' AND a.provider='stripe' \
                 AND i.period_start IS NOT NULL AND i.period_end IS NOT NULL \
             ) \
             SELECT DISTINCT organization_id,customer_id,period_start_ms,period_end_ms,reporting_priority \
             FROM candidate_periods WHERE period_start_ms>=0 AND period_end_ms>period_start_ms \
               AND period_start_ms<=$1 \
             ORDER BY reporting_priority NULLS FIRST,period_end_ms DESC,organization_id LIMIT $2",
        )
        .bind(now_ms)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| ReportingCycleError::RepositoryUnavailable)?;
        rows.into_iter()
            .map(|row| {
                let start_ms: i64 = row
                    .try_get("period_start_ms")
                    .map_err(|_| ReportingCycleError::RepositoryUnavailable)?;
                let end_ms: i64 = row
                    .try_get("period_end_ms")
                    .map_err(|_| ReportingCycleError::RepositoryUnavailable)?;
                Ok(PaidBillingPeriod {
                    organization_id: OrganizationId(
                        row.try_get::<Uuid, _>("organization_id")
                            .map_err(|_| ReportingCycleError::RepositoryUnavailable)?,
                    ),
                    customer_id: row
                        .try_get("customer_id")
                        .map_err(|_| ReportingCycleError::RepositoryUnavailable)?,
                    period: BillingPeriod {
                        start_ms,
                        end_ms,
                        cutoff_ms: end_ms.saturating_add(cutoff_grace_ms),
                    },
                })
            })
            .collect()
    }

    async fn update_health(
        &self,
        organization_id: OrganizationId,
        health: ReportingHealth,
        now_ms: i64,
    ) -> Result<(), ReportingCycleError> {
        let query = match health {
            ReportingHealth::Healthy => {
                "UPDATE organization_billing_accounts SET usage_reporting_status=$2, \
                 usage_reporting_last_success_at=to_timestamp($3::double precision/1000), \
                 usage_reporting_hard_cutoff_at=NULL,updated_at=now() WHERE organization_id=$1"
            }
            ReportingHealth::Blocked => {
                "UPDATE organization_billing_accounts SET usage_reporting_status=$2, \
                 usage_reporting_hard_cutoff_at=COALESCE(usage_reporting_hard_cutoff_at, \
                   to_timestamp($3::double precision/1000)),updated_at=now() WHERE organization_id=$1"
            }
            ReportingHealth::Degraded | ReportingHealth::Reconciling => {
                "UPDATE organization_billing_accounts SET usage_reporting_status=$2,updated_at=now() \
                 WHERE organization_id=$1"
            }
        };
        let mut statement = sqlx::query(query)
            .bind(organization_id.0)
            .bind(health.as_str());
        if matches!(health, ReportingHealth::Healthy | ReportingHealth::Blocked) {
            statement = statement.bind(now_ms);
        }
        statement
            .execute(&self.pool)
            .await
            .map_err(|_| ReportingCycleError::RepositoryUnavailable)?;
        Ok(())
    }
}

pub struct UsageReportingService {
    repository: Arc<dyn UsageReportingRepository>,
    provider: Arc<dyn UsageProvider>,
    config: UsageReportingConfig,
}

#[derive(Debug)]
pub struct UsageReportingWorkerHandle {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl UsageReportingWorkerHandle {
    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

impl std::fmt::Debug for UsageReportingService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UsageReportingService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl UsageReportingService {
    pub fn new(
        pool: PgPool,
        provider: Arc<dyn PlatformBillingProvider>,
        config: UsageReportingConfig,
    ) -> Result<Self, ReportingCycleError> {
        config.validate()?;
        Ok(Self {
            repository: Arc::new(PgUsageReportingRepository { pool }),
            provider: Arc::new(BillingProviderAdapter { provider }),
            config,
        })
    }

    pub fn spawn(
        self: Arc<Self>,
        interval: Duration,
    ) -> Result<UsageReportingWorkerHandle, ReportingCycleError> {
        if !(Duration::from_secs(1)..=Duration::from_secs(60 * 60)).contains(&interval) {
            return Err(ReportingCycleError::InvalidConfiguration);
        }
        let (shutdown, mut shutdown_receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = &mut shutdown_receiver => break,
                    _ = ticker.tick() => {
                        let now_ms = epoch_ms();
                        match now_ms {
                            Some(now_ms) => match self.run_cycle(now_ms).await {
                                Ok(summary) => tracing::info!(
                                    organizations = summary.organizations,
                                    periods = summary.periods,
                                    reports_enqueued = summary.reports_enqueued,
                                    reports_sent = summary.reports_sent,
                                    reports_failed = summary.reports_failed,
                                    periods_reconciled = summary.periods_reconciled,
                                    organizations_blocked = summary.organizations_blocked,
                                    "usage reporting cycle completed"
                                ),
                                Err(error) => tracing::error!(error = %error, "usage reporting cycle failed"),
                            },
                            None => tracing::error!("system time is unavailable for usage reporting"),
                        }
                    }
                }
            }
        });
        Ok(UsageReportingWorkerHandle {
            shutdown: Some(shutdown),
            task,
        })
    }

    /// Executes one bounded reporting pass. Scheduling is deliberately outside
    /// this type so shutdown, jitter, and process supervision remain owned by
    /// the API composition root.
    pub async fn run_cycle(
        &self,
        now_ms: i64,
    ) -> Result<ReportingCycleSummary, ReportingCycleError> {
        if now_ms < 0 {
            return Err(ReportingCycleError::InvalidConfiguration);
        }
        let catalog = self.repository.usage_catalog().await?;
        catalog.validate()?;
        let periods = self
            .repository
            .paid_periods(
                now_ms,
                self.config.period_cutoff_grace_ms,
                self.config.max_periods_per_cycle,
            )
            .await?;
        let mut cycle = ReportingCycleSummary {
            periods: u32::try_from(periods.len())
                .map_err(|_| ReportingCycleError::RepositoryUnavailable)?,
            ..ReportingCycleSummary::default()
        };
        let mut health_by_organization = HashMap::new();
        for paid_period in periods {
            let fallback = if now_ms
                >= paid_period
                    .period
                    .cutoff_ms
                    .saturating_add(self.config.provider_consistency_delay_ms)
            {
                ReportingHealth::Blocked
            } else {
                ReportingHealth::Degraded
            };
            let outcome = match self.process_period(&paid_period, &catalog, now_ms).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::error!(
                        organization_id = %paid_period.organization_id,
                        period_start_ms = paid_period.period.start_ms,
                        error = %error,
                        "organization usage reporting period failed"
                    );
                    PeriodCycleOutcome {
                        health: Some(fallback),
                        ..PeriodCycleOutcome::default()
                    }
                }
            };
            cycle.reports_enqueued = cycle
                .reports_enqueued
                .saturating_add(outcome.reports_enqueued);
            cycle.reports_sent = cycle.reports_sent.saturating_add(outcome.reports_sent);
            cycle.reports_failed = cycle.reports_failed.saturating_add(outcome.reports_failed);
            cycle.periods_reconciled = cycle
                .periods_reconciled
                .saturating_add(u32::from(outcome.reconciled));
            if let Some(health) = outcome.health {
                health_by_organization
                    .entry(paid_period.organization_id)
                    .and_modify(|current: &mut ReportingHealth| {
                        if health.rank() > current.rank() {
                            *current = health;
                        }
                    })
                    .or_insert(health);
            }
        }
        cycle.organizations = u32::try_from(health_by_organization.len())
            .map_err(|_| ReportingCycleError::RepositoryUnavailable)?;
        for (organization_id, health) in health_by_organization {
            self.repository
                .update_health(organization_id, health, now_ms)
                .await?;
            cycle.organizations_blocked = cycle
                .organizations_blocked
                .saturating_add(u32::from(health == ReportingHealth::Blocked));
        }
        Ok(cycle)
    }

    async fn process_period(
        &self,
        paid: &PaidBillingPeriod,
        catalog: &UsageCatalog,
        now_ms: i64,
    ) -> Result<PeriodCycleOutcome, ReportingCycleError> {
        let store = OrganizationMetricsStore::open(
            &MetricsConfig::new(self.config.metrics_root.clone()),
            paid.organization_id,
            self.config.subject_key,
        )
        .map_err(map_metrics_error)?;
        let mut status = store
            .period_status(paid.period)
            .map_err(map_metrics_error)?;
        if status == BillingPeriodStatus::Open && now_ms >= paid.period.cutoff_ms {
            store
                .seal_period(paid.period, now_ms)
                .map_err(map_metrics_error)?;
            status = BillingPeriodStatus::Sealed;
        }
        if status == BillingPeriodStatus::Finalized {
            return Ok(PeriodCycleOutcome {
                health: Some(ReportingHealth::Healthy),
                reconciled: true,
                ..PeriodCycleOutcome::default()
            });
        }

        let mut outcome = PeriodCycleOutcome::default();
        if let Some(through_ms) = reportable_through(paid.period, now_ms, self.config.report_lag_ms)
        {
            let local = store
                .summary_through(paid.period, through_ms)
                .map_err(map_metrics_error)?;
            for (metric, dimension, total) in metric_totals(&local)? {
                let progress = store
                    .reporting_progress(paid.period, dimension)
                    .map_err(map_metrics_error)?;
                let delta = total
                    .checked_sub(progress.enqueued_quantity)
                    .ok_or(ReportingCycleError::InconsistentTotals)?;
                if delta == 0 {
                    continue;
                }
                let report = ReportRequest {
                    identifier: stable_report_identifier(
                        paid.organization_id,
                        paid.period,
                        metric,
                        total,
                        through_ms,
                    ),
                    event_name: catalog.event_name(metric)?.to_owned(),
                    customer_id: paid.customer_id.clone(),
                    period: paid.period,
                    dimension,
                    window_start_ms: paid.period.start_ms,
                    window_end_ms: through_ms,
                    quantity: delta,
                    provider_timestamp_ms: through_ms,
                    now_ms,
                };
                if store.enqueue_report(&report).map_err(map_metrics_error)?
                    == IngestOutcome::Inserted
                {
                    outcome.reports_enqueued = outcome.reports_enqueued.saturating_add(1);
                }
            }
        }

        let claims = store
            .claim_reports_for_period(
                paid.period,
                now_ms,
                self.config.outbox_lease_ms,
                self.config.max_reports_per_period,
            )
            .map_err(map_metrics_error)?;
        let mut permanent_provider_failure = false;
        for claim in claims {
            match self.report_claim(&claim).await {
                Ok(()) => {
                    store
                        .acknowledge_report(
                            &claim.identifier,
                            &claim.lease_token,
                            &claim.identifier,
                            now_ms,
                        )
                        .map_err(map_metrics_error)?;
                    outcome.reports_sent = outcome.reports_sent.saturating_add(1);
                }
                Err(error) => {
                    let retry_at = now_ms
                        .saturating_add(retry_delay_ms(self.config.retry_base_ms, claim.attempt));
                    store
                        .fail_report(
                            &claim.identifier,
                            &claim.lease_token,
                            retry_at,
                            match error {
                                ProviderUsageError::Retryable => "provider_unavailable",
                                ProviderUsageError::Rejected => "provider_rejected",
                            },
                        )
                        .map_err(map_metrics_error)?;
                    outcome.reports_failed = outcome.reports_failed.saturating_add(1);
                    permanent_provider_failure |= error == ProviderUsageError::Rejected;
                }
            }
        }

        let checkpoint = store
            .outbox_checkpoint(paid.period)
            .map_err(map_metrics_error)?;
        if permanent_provider_failure {
            outcome.health = Some(ReportingHealth::Blocked);
            return Ok(outcome);
        }
        if status == BillingPeriodStatus::Open {
            outcome.health = Some(if checkpoint.outstanding == 0 {
                ReportingHealth::Healthy
            } else {
                ReportingHealth::Degraded
            });
            return Ok(outcome);
        }
        if checkpoint.outstanding != 0 {
            outcome.health = Some(
                if now_ms
                    >= paid
                        .period
                        .cutoff_ms
                        .saturating_add(self.config.provider_consistency_delay_ms)
                {
                    ReportingHealth::Blocked
                } else {
                    ReportingHealth::Reconciling
                },
            );
            return Ok(outcome);
        }
        let consistency_base = checkpoint
            .last_acknowledged_at_ms
            .unwrap_or(paid.period.cutoff_ms)
            .max(paid.period.cutoff_ms);
        if now_ms < consistency_base.saturating_add(self.config.provider_consistency_delay_ms) {
            outcome.health = Some(ReportingHealth::Reconciling);
            return Ok(outcome);
        }

        let local = store.summary(paid.period).map_err(map_metrics_error)?;
        let mut all_matched = true;
        for (metric, dimension, local_quantity) in metric_totals(&local)? {
            let provider_quantity = match self
                .provider
                .summary(&UsageSummaryInput {
                    customer_id: paid.customer_id.clone(),
                    metric,
                    start_ms: paid.period.start_ms,
                    end_ms: paid.period.end_ms,
                })
                .await
            {
                Ok(quantity) => quantity,
                Err(_) => {
                    outcome.health = Some(ReportingHealth::Blocked);
                    return Ok(outcome);
                }
            };
            let reconciled = store
                .record_reconciliation(
                    paid.period,
                    dimension,
                    local_quantity,
                    provider_quantity,
                    now_ms,
                )
                .map_err(map_metrics_error)?;
            all_matched &= reconciled == ReconciliationStatus::Matched;
        }
        if !all_matched {
            outcome.health = Some(ReportingHealth::Blocked);
            return Ok(outcome);
        }
        store
            .finalize_period(paid.period, now_ms)
            .map_err(map_metrics_error)?;
        outcome.health = Some(ReportingHealth::Healthy);
        outcome.reconciled = true;
        Ok(outcome)
    }

    async fn report_claim(&self, claim: &ClaimedReport) -> Result<(), ProviderUsageError> {
        self.provider
            .report(&UsageMeterEvent {
                customer_id: claim.customer_id.clone(),
                metric: provider_metric(claim.dimension),
                identifier: claim.identifier.clone(),
                value: claim.quantity,
                timestamp_ms: claim.provider_timestamp_ms,
            })
            .await
    }
}

fn reportable_through(period: BillingPeriod, now_ms: i64, lag_ms: i64) -> Option<i64> {
    let stable_ms = now_ms.checked_sub(lag_ms)?;
    let through_ms = if stable_ms >= period.end_ms {
        period.end_ms
    } else {
        stable_ms - stable_ms.rem_euclid(HOUR_MS)
    };
    (through_ms > period.start_ms).then_some(through_ms)
}

fn epoch_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

fn metric_totals(
    summary: &UsageSummary,
) -> Result<[(UsageMetric, UsageDimension, u64); 4], ReportingCycleError> {
    let storage_kilobyte_hours = u64::try_from(
        summary.storage_byte_milliseconds
            / u128::try_from(HOUR_MS).map_err(|_| ReportingCycleError::InconsistentTotals)?,
    )
    .map_err(|_| ReportingCycleError::InconsistentTotals)?
        / STORAGE_BILLING_UNIT_BYTES;
    Ok([
        (
            UsageMetric::Reads,
            UsageDimension::ReadUnits,
            summary.read_units,
        ),
        (
            UsageMetric::Writes,
            UsageDimension::WriteUnits,
            summary.write_units,
        ),
        (
            UsageMetric::StorageByteHours,
            UsageDimension::StorageByteMilliseconds,
            storage_kilobyte_hours,
        ),
        (
            UsageMetric::MonthlyActiveUsers,
            UsageDimension::MonthlyActiveUsers,
            summary.monthly_active_users,
        ),
    ])
}

const fn provider_metric(dimension: UsageDimension) -> UsageMetric {
    match dimension {
        UsageDimension::ReadUnits => UsageMetric::Reads,
        UsageDimension::WriteUnits => UsageMetric::Writes,
        UsageDimension::StorageByteMilliseconds => UsageMetric::StorageByteHours,
        UsageDimension::MonthlyActiveUsers => UsageMetric::MonthlyActiveUsers,
    }
}

fn stable_report_identifier(
    organization_id: OrganizationId,
    period: BillingPeriod,
    metric: UsageMetric,
    cumulative_quantity: u64,
    through_ms: i64,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.stripe.usage-report.v1\0");
    digest.update(organization_id.0.as_bytes());
    digest.update(period.start_ms.to_be_bytes());
    digest.update(period.end_ms.to_be_bytes());
    digest.update(metric.name().as_bytes());
    digest.update(cumulative_quantity.to_be_bytes());
    digest.update(through_ms.to_be_bytes());
    let bytes: [u8; 32] = digest.finalize().into();
    let mut identifier = String::with_capacity(69);
    identifier.push_str("ffdb_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(identifier, "{byte:02x}");
    }
    identifier
}

fn retry_delay_ms(base_ms: i64, attempt: u32) -> i64 {
    let exponent = attempt.saturating_sub(1).min(10);
    base_ms
        .saturating_mul(1_i64.checked_shl(exponent).unwrap_or(i64::MAX))
        .min(HOUR_MS)
}

fn parse_metric(value: &str) -> Result<UsageMetric, ReportingCycleError> {
    match value {
        "reads" => Ok(UsageMetric::Reads),
        "writes" => Ok(UsageMetric::Writes),
        "storage_byte_hours" => Ok(UsageMetric::StorageByteHours),
        "monthly_active_users" => Ok(UsageMetric::MonthlyActiveUsers),
        _ => Err(ReportingCycleError::InvalidCatalog),
    }
}

const fn map_provider_error(error: BillingError) -> ProviderUsageError {
    match error {
        BillingError::ProviderUnavailable => ProviderUsageError::Retryable,
        BillingError::InvalidConfiguration
        | BillingError::InvalidRequest
        | BillingError::InvalidWebhookSignature
        | BillingError::InvalidWebhookPayload
        | BillingError::ProviderRejected => ProviderUsageError::Rejected,
    }
}

const fn map_metrics_error(error: MetricsError) -> ReportingCycleError {
    match error {
        MetricsError::HashConflict
        | MetricsError::OrganizationMismatch
        | MetricsError::ReservationMismatch => ReportingCycleError::InconsistentTotals,
        MetricsError::InvalidConfiguration | MetricsError::InvalidInput => {
            ReportingCycleError::InvalidConfiguration
        }
        MetricsError::PeriodSealed
        | MetricsError::LimitExceeded(_)
        | MetricsError::ReservationNotFound
        | MetricsError::StaleLease
        | MetricsError::ReconciliationPending
        | MetricsError::Unavailable => ReportingCycleError::MetricsUnavailable,
    }
}

#[cfg(test)]
#[path = "usage_reporting_tests.rs"]
mod usage_reporting_tests;
