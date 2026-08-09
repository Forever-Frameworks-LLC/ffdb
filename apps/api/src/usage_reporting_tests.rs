#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Mutex;

use ffdb_org_metrics::{StorageSnapshot, UsageEvent};
use ffdb_protocol::ProjectId;
use tempfile::TempDir;

use super::*;

const START: i64 = 1_800_000_000_000;

#[derive(Debug)]
struct FakeRepository {
    catalog: UsageCatalog,
    periods: Vec<PaidBillingPeriod>,
    health: Mutex<Vec<(OrganizationId, ReportingHealth, i64)>>,
}

#[async_trait]
impl UsageReportingRepository for FakeRepository {
    async fn usage_catalog(&self) -> Result<UsageCatalog, ReportingCycleError> {
        Ok(self.catalog.clone())
    }

    async fn paid_periods(
        &self,
        _now_ms: i64,
        _cutoff_grace_ms: i64,
        _limit: u32,
    ) -> Result<Vec<PaidBillingPeriod>, ReportingCycleError> {
        Ok(self.periods.clone())
    }

    async fn update_health(
        &self,
        organization_id: OrganizationId,
        health: ReportingHealth,
        now_ms: i64,
    ) -> Result<(), ReportingCycleError> {
        self.health
            .lock()
            .unwrap()
            .push((organization_id, health, now_ms));
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FakeProvider {
    accepted: Mutex<Vec<UsageMeterEvent>>,
    failures_remaining: Mutex<u32>,
    summary_offsets: Mutex<HashMap<UsageMetric, u64>>,
}

#[async_trait]
impl UsageProvider for FakeProvider {
    async fn report(&self, event: &UsageMeterEvent) -> Result<(), ProviderUsageError> {
        let mut failures = self.failures_remaining.lock().unwrap();
        if *failures > 0 {
            *failures -= 1;
            return Err(ProviderUsageError::Retryable);
        }
        drop(failures);
        let mut accepted = self.accepted.lock().unwrap();
        if !accepted
            .iter()
            .any(|existing| existing.identifier == event.identifier)
        {
            accepted.push(event.clone());
        }
        Ok(())
    }

    async fn summary(&self, input: &UsageSummaryInput) -> Result<u64, ProviderUsageError> {
        let reported = self
            .accepted
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                event.customer_id == input.customer_id
                    && event.metric == input.metric
                    && event.timestamp_ms >= input.start_ms
                    && event.timestamp_ms <= input.end_ms
            })
            .map(|event| event.value)
            .sum::<u64>();
        Ok(reported.saturating_add(
            *self
                .summary_offsets
                .lock()
                .unwrap()
                .get(&input.metric)
                .unwrap_or(&0),
        ))
    }
}

fn organization() -> OrganizationId {
    OrganizationId(Uuid::parse_str("018f0000-0000-7000-8000-000000000011").unwrap())
}

fn project() -> ProjectId {
    ProjectId(Uuid::parse_str("018f0000-0000-7000-8000-000000000012").unwrap())
}

fn period() -> BillingPeriod {
    let aligned = START - START.rem_euclid(HOUR_MS);
    BillingPeriod {
        start_ms: aligned,
        end_ms: aligned + HOUR_MS,
        cutoff_ms: aligned + 2 * HOUR_MS,
    }
}

fn catalog() -> UsageCatalog {
    UsageCatalog {
        event_names: HashMap::from([
            (UsageMetric::Reads, "ffdb_reads".to_owned()),
            (UsageMetric::Writes, "ffdb_writes".to_owned()),
            (
                UsageMetric::StorageByteHours,
                "ffdb_storage_byte_hours".to_owned(),
            ),
            (
                UsageMetric::MonthlyActiveUsers,
                "ffdb_monthly_active_users".to_owned(),
            ),
        ]),
    }
}

fn config(directory: &TempDir) -> UsageReportingConfig {
    UsageReportingConfig {
        metrics_root: directory.path().to_path_buf(),
        subject_key: [0x33; 32],
        report_lag_ms: 1_000,
        provider_consistency_delay_ms: 1_000,
        period_cutoff_grace_ms: HOUR_MS,
        outbox_lease_ms: 1_000,
        retry_base_ms: 1_000,
        max_periods_per_cycle: 10,
        max_reports_per_period: 10,
    }
}

fn repository() -> Arc<FakeRepository> {
    Arc::new(FakeRepository {
        catalog: catalog(),
        periods: vec![PaidBillingPeriod {
            organization_id: organization(),
            customer_id: "cus_reporting_test".to_owned(),
            period: period(),
        }],
        health: Mutex::new(Vec::new()),
    })
}

fn service(
    directory: &TempDir,
    repository: Arc<FakeRepository>,
    provider: Arc<FakeProvider>,
) -> UsageReportingService {
    let config = config(directory);
    config.validate().unwrap();
    UsageReportingService {
        repository,
        provider,
        config,
    }
}

fn seed_usage(directory: &TempDir) {
    let config = MetricsConfig::new(directory.path().to_path_buf());
    let store = OrganizationMetricsStore::open(&config, organization(), [0x33; 32]).unwrap();
    let subject = store.hash_active_subject(period(), "active-user").unwrap();
    store
        .record_event(&UsageEvent {
            event_id: "usage_cycle_seed".to_owned(),
            project_id: project(),
            period: period(),
            occurred_at_ms: period().start_ms,
            recorded_at_ms: period().start_ms,
            read_units: 3,
            write_units: 2,
            active_subject_hash: Some(subject),
            storage_snapshots: vec![StorageSnapshot {
                resource_id: format!("database:{}", project().0),
                logical_bytes: 100_000,
            }],
        })
        .unwrap();
}

#[tokio::test]
async fn cycle_reports_positive_deltas_once_then_reconciles_all_dimensions() {
    let directory = tempfile::tempdir().unwrap();
    seed_usage(&directory);
    let repository = repository();
    let provider = Arc::new(FakeProvider::default());
    let service = service(&directory, repository.clone(), provider.clone());

    let reported_at = period().end_ms + 1_000;
    let first = service.run_cycle(reported_at).await.unwrap();
    assert_eq!(first.reports_enqueued, 4);
    assert_eq!(first.reports_sent, 4);
    assert_eq!(first.reports_failed, 0);
    assert_eq!(provider.accepted.lock().unwrap().len(), 4);

    let duplicate = service.run_cycle(reported_at + 1).await.unwrap();
    assert_eq!(duplicate.reports_enqueued, 0);
    assert_eq!(duplicate.reports_sent, 0);
    assert_eq!(provider.accepted.lock().unwrap().len(), 4);

    let finalized = service.run_cycle(period().cutoff_ms + 1_000).await.unwrap();
    assert_eq!(finalized.periods_reconciled, 1);
    assert_eq!(finalized.organizations_blocked, 0);
    let store = OrganizationMetricsStore::open(
        &MetricsConfig::new(directory.path().to_path_buf()),
        organization(),
        [0x33; 32],
    )
    .unwrap();
    assert_eq!(
        store.period_status(period()).unwrap(),
        BillingPeriodStatus::Finalized
    );
    assert_eq!(
        repository.health.lock().unwrap().last().unwrap().1,
        ReportingHealth::Healthy
    );

    let accepted = provider.accepted.lock().unwrap();
    assert_eq!(
        accepted
            .iter()
            .find(|event| event.metric == UsageMetric::Reads)
            .unwrap()
            .value,
        3
    );
    assert_eq!(
        accepted
            .iter()
            .find(|event| event.metric == UsageMetric::Writes)
            .unwrap()
            .value,
        2
    );
    assert_eq!(
        accepted
            .iter()
            .find(|event| event.metric == UsageMetric::StorageByteHours)
            .unwrap()
            .value,
        100
    );
    assert_eq!(
        accepted
            .iter()
            .find(|event| event.metric == UsageMetric::MonthlyActiveUsers)
            .unwrap()
            .value,
        1
    );
}

#[tokio::test]
async fn retry_uses_existing_outbox_row_and_never_enqueues_usage_twice() {
    let directory = tempfile::tempdir().unwrap();
    seed_usage(&directory);
    let repository = repository();
    let provider = Arc::new(FakeProvider::default());
    *provider.failures_remaining.lock().unwrap() = 1;
    let service = service(&directory, repository.clone(), provider.clone());
    let first_at = period().end_ms + 1_000;

    let first = service.run_cycle(first_at).await.unwrap();
    assert_eq!(first.reports_enqueued, 4);
    assert_eq!(first.reports_failed, 1);
    assert_eq!(first.reports_sent, 3);
    assert_eq!(
        repository.health.lock().unwrap().last().unwrap().1,
        ReportingHealth::Degraded
    );

    let retry = service.run_cycle(first_at + 1_000).await.unwrap();
    assert_eq!(retry.reports_enqueued, 0);
    assert_eq!(retry.reports_sent, 1);
    assert_eq!(retry.reports_failed, 0);
    let accepted = provider.accepted.lock().unwrap();
    assert_eq!(accepted.len(), 4);
    assert_eq!(
        accepted
            .iter()
            .map(|event| event.identifier.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
}

#[tokio::test]
async fn provider_summary_mismatch_sets_hard_block_health() {
    let directory = tempfile::tempdir().unwrap();
    seed_usage(&directory);
    let repository = repository();
    let provider = Arc::new(FakeProvider::default());
    let service = service(&directory, repository.clone(), provider.clone());
    service.run_cycle(period().end_ms + 1_000).await.unwrap();
    provider
        .summary_offsets
        .lock()
        .unwrap()
        .insert(UsageMetric::Reads, 1);

    let cycle = service.run_cycle(period().cutoff_ms + 1_000).await.unwrap();
    assert_eq!(cycle.organizations_blocked, 1);
    assert_eq!(cycle.periods_reconciled, 0);
    assert_eq!(
        repository.health.lock().unwrap().last().unwrap().1,
        ReportingHealth::Blocked
    );
}

#[test]
fn report_identifier_is_stable_provider_safe_and_changes_with_target() {
    let first = stable_report_identifier(
        organization(),
        period(),
        UsageMetric::Reads,
        10,
        period().end_ms,
    );
    let duplicate = stable_report_identifier(
        organization(),
        period(),
        UsageMetric::Reads,
        10,
        period().end_ms,
    );
    let next = stable_report_identifier(
        organization(),
        period(),
        UsageMetric::Reads,
        11,
        period().end_ms,
    );
    assert_eq!(first, duplicate);
    assert_ne!(first, next);
    assert!((8..=100).contains(&first.len()));
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    );
}
