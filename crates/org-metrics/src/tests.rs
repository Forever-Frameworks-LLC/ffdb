#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use tempfile::TempDir;

use super::*;

const START: i64 = 1_700_000_000_000;

fn period() -> BillingPeriod {
    BillingPeriod {
        start_ms: START,
        end_ms: START + HOUR_MS,
        cutoff_ms: START + HOUR_MS + 60_000,
    }
}

fn config(directory: &TempDir) -> MetricsConfig {
    MetricsConfig::new(directory.path().to_path_buf())
}

fn organization() -> OrganizationId {
    OrganizationId(Uuid::parse_str("018f0000-0000-7000-8000-000000000001").unwrap())
}

fn project() -> ProjectId {
    ProjectId(Uuid::parse_str("018f0000-0000-7000-8000-000000000002").unwrap())
}

fn key() -> [u8; 32] {
    [0x5a; 32]
}

fn store(directory: &TempDir) -> OrganizationMetricsStore {
    OrganizationMetricsStore::open(&config(directory), organization(), key()).unwrap()
}

fn event(id: &str, reads: u64, writes: u64) -> UsageEvent {
    UsageEvent {
        event_id: id.to_owned(),
        project_id: project(),
        period: period(),
        occurred_at_ms: START + 1_000,
        recorded_at_ms: START + 2_000,
        read_units: reads,
        write_units: writes,
        active_subject_hash: None,
        storage_snapshots: Vec::new(),
    }
}

fn latency_summary(samples: &mut [Duration]) -> (Duration, Duration, Duration) {
    samples.sort_unstable();
    let percentile = |percent: usize| {
        let index = samples
            .len()
            .saturating_mul(percent)
            .div_ceil(100)
            .saturating_sub(1)
            .min(samples.len().saturating_sub(1));
        samples[index]
    };
    (percentile(50), percentile(95), percentile(99))
}

/// Manual host-dependent probe for the synchronous=FULL durability boundary
/// on the API request path.
#[test]
#[ignore = "manual organization metrics latency profile"]
fn organization_metrics_ingest_latency_profile() {
    const SAMPLES: usize = 500;
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let mut samples = Vec::with_capacity(SAMPLES);
    for ordinal in 0..SAMPLES {
        let usage = event(&format!("profile_{ordinal}"), 1, 0);
        let started = Instant::now();
        assert_eq!(store.record_event(&usage).unwrap(), IngestOutcome::Inserted);
        samples.push(started.elapsed());
    }
    let summary = latency_summary(&mut samples);
    eprintln!(
        "organization_metrics_ingest_us p50={} p95={} p99={}",
        summary.0.as_micros(),
        summary.1.as_micros(),
        summary.2.as_micros(),
    );
}

fn strict_limits(mau: u64) -> UsageLimits {
    UsageLimits {
        read_units: 10,
        write_units: 10,
        monthly_active_users: mau,
        storage_bytes: 1_000,
        enforce_read_limit: true,
        enforce_write_limit: true,
        enforce_mau_limit: true,
        enforce_storage_limit: true,
    }
}

fn reservation(id: &str, subject: Option<[u8; 32]>) -> ReservationRequest {
    ReservationRequest {
        reservation_id: id.to_owned(),
        project_id: project(),
        period: period(),
        created_at_ms: START + 1_000,
        expires_at_ms: START + 30_000,
        read_units: 0,
        write_units: 1,
        storage_growth_bytes: 10,
        active_subject_hash: subject,
    }
}

#[test]
fn creates_private_wal_full_org_database() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    assert_eq!(store.organization_id(), organization());
    assert_eq!(
        store.path().file_name().unwrap().to_str().unwrap(),
        "018f0000-0000-7000-8000-000000000001.sqlite3"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::metadata(directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let connection = store.lock().unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(synchronous, 2);
}

#[test]
fn refuses_to_open_database_as_another_organization() {
    let directory = tempfile::tempdir().unwrap();
    let first = store(&directory);
    let wrong_path = directory.path().join(format!("{}.sqlite3", Uuid::now_v7()));
    first.backup_to(&wrong_path, START).unwrap();
    drop(first);
    let wrong_org =
        OrganizationId(Uuid::parse_str(wrong_path.file_stem().unwrap().to_str().unwrap()).unwrap());
    assert_eq!(
        OrganizationMetricsStore::open(&config(&directory), wrong_org, key()).unwrap_err(),
        MetricsError::OrganizationMismatch
    );
}

#[test]
fn event_ingest_is_idempotent_and_detects_hash_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let original = event("evt_1", 3, 2);
    assert_eq!(
        store.record_event(&original).unwrap(),
        IngestOutcome::Inserted
    );
    assert_eq!(
        store.record_event(&original).unwrap(),
        IngestOutcome::Duplicate
    );
    let mut conflict = original;
    conflict.write_units = 3;
    assert_eq!(
        store.record_event(&conflict).unwrap_err(),
        MetricsError::HashConflict
    );
    let summary = store.summary(period()).unwrap();
    assert_eq!(summary.read_units, 3);
    assert_eq!(summary.write_units, 2);
}

#[test]
fn mau_hash_is_org_and_period_scoped_without_storing_subject() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let first = store.hash_active_subject(period(), "user-123").unwrap();
    let later_period = BillingPeriod {
        start_ms: period().end_ms,
        end_ms: period().end_ms + HOUR_MS,
        cutoff_ms: period().end_ms + HOUR_MS + 60_000,
    };
    let later = store.hash_active_subject(later_period, "user-123").unwrap();
    assert_ne!(first, later);
    assert_eq!(
        first,
        store.hash_active_subject(period(), "user-123").unwrap()
    );
    let bytes = std::fs::read(store.path()).unwrap();
    assert!(!bytes.windows(8).any(|window| window == b"user-123"));
}

#[test]
fn reservations_settle_release_and_replay_without_double_counting() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let subject = store.hash_active_subject(period(), "user-1").unwrap();
    let request = reservation("reservation_1", Some(subject));
    assert_eq!(
        store.reserve(&request, strict_limits(2)).unwrap(),
        ReservationAdmission::Reserved
    );
    assert_eq!(
        store.reserve(&request, strict_limits(2)).unwrap(),
        ReservationAdmission::Duplicate(ReservationStatus::Active)
    );
    let mut usage = event("usage_1", 0, 1);
    usage.active_subject_hash = Some(subject);
    assert_eq!(
        store.settle_reservation("reservation_1", &usage).unwrap(),
        IngestOutcome::Inserted
    );
    assert_eq!(
        store.settle_reservation("reservation_1", &usage).unwrap(),
        IngestOutcome::Duplicate
    );
    assert_eq!(
        store
            .release_reservation("reservation_1", START + 5_000)
            .unwrap(),
        ReservationStatus::Settled
    );
    let summary = store.summary(period()).unwrap();
    assert_eq!(summary.write_units, 1);
    assert_eq!(summary.monthly_active_users, 1);

    let released = reservation("reservation_2", None);
    store.reserve(&released, strict_limits(2)).unwrap();
    assert_eq!(
        store
            .release_reservation("reservation_2", START + 5_000)
            .unwrap(),
        ReservationStatus::Released
    );
    assert_eq!(
        store
            .release_reservation("reservation_2", START + 6_000)
            .unwrap(),
        ReservationStatus::Released
    );
}

#[test]
fn concurrent_mau_reservations_never_oversubscribe_limit() {
    let directory = tempfile::tempdir().unwrap();
    let initial = store(&directory);
    drop(initial);
    let root = directory.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(12));
    let mut threads = Vec::new();
    for index in 0..12 {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            let store =
                OrganizationMetricsStore::open(&MetricsConfig::new(root), organization(), key())
                    .unwrap();
            let subject = store
                .hash_active_subject(period(), &format!("user-{index}"))
                .unwrap();
            barrier.wait();
            store.reserve(
                &reservation(&format!("reservation_{index}"), Some(subject)),
                strict_limits(3),
            )
        }));
    }
    let outcomes = threads
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(ReservationAdmission::Reserved)))
            .count(),
        3
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    Err(MetricsError::LimitExceeded("monthly_active_users"))
                )
            })
            .count(),
        9
    );
}

#[test]
fn reservation_id_hash_conflicts_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let request = reservation("same_id", None);
    store.reserve(&request, strict_limits(2)).unwrap();
    let mut conflict = request;
    conflict.write_units = 2;
    assert_eq!(
        store.reserve(&conflict, strict_limits(2)).unwrap_err(),
        MetricsError::HashConflict
    );
}

#[test]
fn storage_summary_is_time_weighted_across_resources() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let mut initial = event("storage_1", 0, 0);
    initial.occurred_at_ms = START;
    initial.recorded_at_ms = START;
    initial.storage_snapshots = vec![StorageSnapshot {
        resource_id: "database:project-1".to_owned(),
        logical_bytes: 100,
    }];
    store.record_event(&initial).unwrap();
    let mut change = event("storage_2", 0, 1);
    change.occurred_at_ms = START + HOUR_MS / 2;
    change.recorded_at_ms = change.occurred_at_ms;
    change.storage_snapshots = vec![StorageSnapshot {
        resource_id: "database:project-1".to_owned(),
        logical_bytes: 300,
    }];
    store.record_event(&change).unwrap();
    let summary = store.summary(period()).unwrap();
    assert_eq!(summary.storage_bytes_at_end, 300);
    assert_eq!(
        summary.storage_byte_milliseconds,
        u128::from(100_u64) * u128::try_from(HOUR_MS / 2).unwrap()
            + u128::from(300_u64) * u128::try_from(HOUR_MS / 2).unwrap()
    );
}

#[test]
fn cumulative_reporting_boundary_and_progress_are_durable() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    let aligned = START - START.rem_euclid(HOUR_MS);
    let reporting_period = BillingPeriod {
        start_ms: aligned,
        end_ms: aligned + 2 * HOUR_MS,
        cutoff_ms: aligned + 2 * HOUR_MS + 60_000,
    };
    let first_subject = store
        .hash_active_subject(reporting_period, "first-user")
        .unwrap();
    let second_subject = store
        .hash_active_subject(reporting_period, "second-user")
        .unwrap();
    for (id, at, reads, subject, bytes) in [
        ("boundary_1", aligned + 1_000, 3, first_subject, 100),
        (
            "boundary_2",
            aligned + HOUR_MS + 1_000,
            7,
            second_subject,
            300,
        ),
    ] {
        store
            .record_event(&UsageEvent {
                event_id: id.to_owned(),
                project_id: project(),
                period: reporting_period,
                occurred_at_ms: at,
                recorded_at_ms: at,
                read_units: reads,
                write_units: 0,
                active_subject_hash: Some(subject),
                storage_snapshots: vec![StorageSnapshot {
                    resource_id: "database:boundary".to_owned(),
                    logical_bytes: bytes,
                }],
            })
            .unwrap();
    }
    let first_hour = store
        .summary_through(reporting_period, aligned + HOUR_MS)
        .unwrap();
    assert_eq!(first_hour.read_units, 3);
    assert_eq!(first_hour.monthly_active_users, 1);
    assert_eq!(first_hour.storage_bytes_at_end, 100);

    let report = ReportRequest {
        identifier: "boundary_report_1".to_owned(),
        event_name: "ffdb_reads".to_owned(),
        customer_id: "cus_boundary".to_owned(),
        period: reporting_period,
        dimension: UsageDimension::ReadUnits,
        window_start_ms: aligned,
        window_end_ms: aligned + HOUR_MS,
        quantity: 3,
        provider_timestamp_ms: aligned + HOUR_MS,
        now_ms: aligned + HOUR_MS,
    };
    store.enqueue_report(&report).unwrap();
    assert_eq!(
        store
            .reporting_progress(reporting_period, UsageDimension::ReadUnits)
            .unwrap(),
        ReportingProgress {
            enqueued_quantity: 3,
            latest_window_end_ms: Some(aligned + HOUR_MS),
        }
    );
    assert_eq!(
        store.outbox_checkpoint(reporting_period).unwrap(),
        OutboxCheckpoint {
            outstanding: 1,
            failed: 0,
            last_acknowledged_at_ms: None,
        }
    );
    let claim = store
        .claim_reports(aligned + HOUR_MS, 1_000, 1)
        .unwrap()
        .pop()
        .unwrap();
    store
        .acknowledge_report(
            &claim.identifier,
            &claim.lease_token,
            "meter_boundary",
            aligned + HOUR_MS + 1,
        )
        .unwrap();
    assert_eq!(
        store.outbox_checkpoint(reporting_period).unwrap(),
        OutboxCheckpoint {
            outstanding: 0,
            failed: 0,
            last_acknowledged_at_ms: Some(aligned + HOUR_MS + 1),
        }
    );
}

fn report(identifier: &str, quantity: u64) -> ReportRequest {
    ReportRequest {
        identifier: identifier.to_owned(),
        event_name: "ffdb_reads".to_owned(),
        customer_id: "cus_123".to_owned(),
        period: period(),
        dimension: UsageDimension::ReadUnits,
        window_start_ms: START,
        window_end_ms: START + HOUR_MS,
        quantity,
        provider_timestamp_ms: START + HOUR_MS,
        now_ms: START + HOUR_MS,
    }
}

#[test]
fn outbox_claim_ack_fail_and_lease_recovery_are_durable() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    for index in 0..3 {
        store
            .enqueue_report(&report(&format!("report_{index}"), index + 1))
            .unwrap();
    }
    assert_eq!(
        store.enqueue_report(&report("report_0", 1)).unwrap(),
        IngestOutcome::Duplicate
    );
    assert_eq!(
        store.enqueue_report(&report("report_0", 9)).unwrap_err(),
        MetricsError::HashConflict
    );
    let claimed = store.claim_reports(START + HOUR_MS, 1_000, 2).unwrap();
    assert_eq!(claimed.len(), 2);
    store
        .acknowledge_report(
            &claimed[0].identifier,
            &claimed[0].lease_token,
            "meter_event_1",
            START + HOUR_MS + 100,
        )
        .unwrap();
    assert_eq!(
        store
            .acknowledge_report(
                &claimed[0].identifier,
                &claimed[0].lease_token,
                "meter_event_1",
                START + HOUR_MS + 200,
            )
            .unwrap_err(),
        MetricsError::StaleLease
    );
    store
        .fail_report(
            &claimed[1].identifier,
            &claimed[1].lease_token,
            START + HOUR_MS + 500,
            "transient provider failure",
        )
        .unwrap();
    let retried = store
        .claim_reports(START + HOUR_MS + 500, 1_000, 10)
        .unwrap();
    assert_eq!(retried.len(), 2);
    assert!(retried.iter().any(|item| item.attempt == 2));
    let identifiers = retried
        .iter()
        .map(|item| item.identifier.as_str())
        .collect::<HashSet<_>>();
    assert!(identifiers.contains("report_1"));
    assert!(identifiers.contains("report_2"));
}

#[test]
fn concurrent_outbox_claimers_never_receive_same_identifier() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    for index in 0..20 {
        store
            .enqueue_report(&report(&format!("parallel_{index}"), 1))
            .unwrap();
    }
    drop(store);
    let root = directory.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            let store =
                OrganizationMetricsStore::open(&MetricsConfig::new(root), organization(), key())
                    .unwrap();
            barrier.wait();
            store.claim_reports(START + HOUR_MS, 60_000, 20).unwrap()
        }));
    }
    let claims = threads
        .into_iter()
        .flat_map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    let unique = claims
        .iter()
        .map(|claim| claim.identifier.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(claims.len(), 20);
    assert_eq!(unique.len(), 20);
}

#[test]
fn cutoff_and_four_dimension_reconciliation_gate_finalization() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    store.record_event(&event("before_cutoff", 1, 0)).unwrap();
    assert_eq!(
        store
            .seal_period(period(), period().cutoff_ms - 1)
            .unwrap_err(),
        MetricsError::InvalidInput
    );
    store.seal_period(period(), period().cutoff_ms).unwrap();
    assert_eq!(
        store.record_event(&event("after_seal", 1, 0)).unwrap_err(),
        MetricsError::PeriodSealed
    );
    assert_eq!(
        store
            .finalize_period(period(), period().cutoff_ms + 1)
            .unwrap_err(),
        MetricsError::ReconciliationPending
    );
    for dimension in [
        UsageDimension::ReadUnits,
        UsageDimension::WriteUnits,
        UsageDimension::MonthlyActiveUsers,
        UsageDimension::StorageByteMilliseconds,
    ] {
        assert_eq!(
            store
                .record_reconciliation(period(), dimension, 4, 4, period().cutoff_ms)
                .unwrap(),
            ReconciliationStatus::Matched
        );
    }
    store
        .finalize_period(period(), period().cutoff_ms + 1)
        .unwrap();
}

#[test]
fn online_backup_has_manifest_integrity_and_org_binding() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory);
    store.record_event(&event("backup_event", 7, 2)).unwrap();
    let backup_directory = tempfile::tempdir().unwrap();
    let backup_path = backup_directory.path().join("metrics-backup.sqlite3");
    let manifest = store.backup_to(&backup_path, START + 10_000).unwrap();
    assert!(manifest.size_bytes > 0);
    OrganizationMetricsStore::verify_backup(&backup_path, &manifest).unwrap();
    let mut tampered = OpenOptions::new().append(true).open(&backup_path).unwrap();
    tampered.write_all(b"tampered").unwrap();
    drop(tampered);
    assert_eq!(
        OrganizationMetricsStore::verify_backup(&backup_path, &manifest).unwrap_err(),
        MetricsError::HashConflict
    );
}
