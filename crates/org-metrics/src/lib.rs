//! Durable, organization-isolated usage metering.
//!
//! Each organization owns one SQLite database. Callers must ingest the durable
//! receipt returned by a project database worker before exposing a successful
//! response to the client. The receipt identifier and payload hash make retries
//! idempotent and turn mismatched replays into a hard error.
//!
//! This crate deliberately does not call a billing provider. It owns the local
//! source of truth and a leased outbox. A provider adapter claims outbox rows,
//! sends them with the stable `identifier`, and acknowledges only after the
//! provider has accepted the event.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use ffdb_protocol::{OrganizationId, ProjectId};
use hmac::{Hmac, Mac as _};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const SCHEMA_VERSION: i64 = 1;
const HOUR_MS: i64 = 60 * 60 * 1_000;
const MAX_PERIOD_MS: i64 = 366 * 24 * HOUR_MS;
const MAX_IDENTIFIER_BYTES: usize = 100;
const MAX_RESOURCE_ID_BYTES: usize = 200;
const MAX_PROVIDER_ERROR_BYTES: usize = 1_024;

#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub root: PathBuf,
    pub busy_timeout: Duration,
}

impl MetricsConfig {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BillingPeriod {
    pub start_ms: i64,
    pub end_ms: i64,
    /// Usage can arrive while a period is closing, but never after this point.
    pub cutoff_ms: i64,
}

impl BillingPeriod {
    fn validate(self) -> Result<(), MetricsError> {
        if self.start_ms < 0
            || self.end_ms <= self.start_ms
            || self.end_ms.saturating_sub(self.start_ms) > MAX_PERIOD_MS
            || self.cutoff_ms < self.end_ms
            || self.cutoff_ms.saturating_sub(self.end_ms) > 7 * 24 * HOUR_MS
        {
            return Err(MetricsError::InvalidInput);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageLimits {
    pub read_units: u64,
    pub write_units: u64,
    pub monthly_active_users: u64,
    pub storage_bytes: u64,
    pub enforce_read_limit: bool,
    pub enforce_write_limit: bool,
    pub enforce_mau_limit: bool,
    pub enforce_storage_limit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StorageSnapshot {
    pub resource_id: String,
    pub logical_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageEvent {
    pub event_id: String,
    pub project_id: ProjectId,
    pub period: BillingPeriod,
    pub occurred_at_ms: i64,
    pub recorded_at_ms: i64,
    pub read_units: u64,
    pub write_units: u64,
    /// An HMAC returned by [`OrganizationMetricsStore::hash_active_subject`].
    pub active_subject_hash: Option<[u8; 32]>,
    /// Complete logical sizes for resources changed by this operation.
    pub storage_snapshots: Vec<StorageSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReservationRequest {
    pub reservation_id: String,
    pub project_id: ProjectId,
    pub period: BillingPeriod,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub read_units: u64,
    pub write_units: u64,
    /// Maximum additional logical bytes this operation can allocate.
    pub storage_growth_bytes: u64,
    pub active_subject_hash: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationStatus {
    Active,
    Settled,
    Released,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationAdmission {
    Reserved,
    Duplicate(ReservationStatus),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReservationContext {
    pub project_id: ProjectId,
    pub period_start_ms: i64,
    pub period_end_ms: i64,
    pub status: ReservationStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestOutcome {
    Inserted,
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UsageDimension {
    ReadUnits,
    WriteUnits,
    MonthlyActiveUsers,
    StorageByteMilliseconds,
}

impl UsageDimension {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadUnits => "read_units",
            Self::WriteUnits => "write_units",
            Self::MonthlyActiveUsers => "monthly_active_users",
            Self::StorageByteMilliseconds => "storage_byte_milliseconds",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageSummary {
    pub read_units: u64,
    pub write_units: u64,
    pub monthly_active_users: u64,
    pub storage_bytes_at_end: u64,
    pub storage_byte_milliseconds: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportingProgress {
    pub enqueued_quantity: u64,
    pub latest_window_end_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxCheckpoint {
    pub outstanding: u64,
    pub failed: u64,
    pub last_acknowledged_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingPeriodStatus {
    Open,
    Sealed,
    Finalized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportRequest {
    pub identifier: String,
    pub event_name: String,
    pub customer_id: String,
    pub period: BillingPeriod,
    pub dimension: UsageDimension,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub quantity: u64,
    pub provider_timestamp_ms: i64,
    pub now_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedReport {
    pub identifier: String,
    pub event_name: String,
    pub customer_id: String,
    pub dimension: UsageDimension,
    pub quantity: u64,
    pub provider_timestamp_ms: i64,
    pub lease_token: String,
    pub attempt: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationStatus {
    Matched,
    Mismatched,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupManifest {
    pub organization_id: OrganizationId,
    pub schema_version: u32,
    pub created_at_ms: i64,
    pub size_bytes: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum MetricsError {
    #[error("metrics configuration is invalid")]
    InvalidConfiguration,
    #[error("metrics input is invalid")]
    InvalidInput,
    #[error("metrics event identifier was reused with different content")]
    HashConflict,
    #[error("organization metrics database belongs to another organization")]
    OrganizationMismatch,
    #[error("billing period is sealed")]
    PeriodSealed,
    #[error("usage allowance is exhausted for {0}")]
    LimitExceeded(&'static str),
    #[error("usage reservation was not found")]
    ReservationNotFound,
    #[error("usage reservation cannot accept this event")]
    ReservationMismatch,
    #[error("outbox lease is stale")]
    StaleLease,
    #[error("billing period cannot be finalized")]
    ReconciliationPending,
    #[error("metrics datastore is unavailable")]
    Unavailable,
}

#[derive(Debug)]
pub struct OrganizationMetricsStore {
    organization_id: OrganizationId,
    path: PathBuf,
    connection: Mutex<Connection>,
    subject_key: [u8; 32],
}

impl OrganizationMetricsStore {
    pub fn open(
        config: &MetricsConfig,
        organization_id: OrganizationId,
        subject_key: [u8; 32],
    ) -> Result<Self, MetricsError> {
        if !config.root.is_absolute()
            || config.busy_timeout.is_zero()
            || subject_key.iter().all(|byte| *byte == 0)
        {
            return Err(MetricsError::InvalidConfiguration);
        }
        fs::create_dir_all(&config.root).map_err(|_| MetricsError::Unavailable)?;
        set_private_directory(&config.root)?;
        let path = config.root.join(format!("{}.sqlite3", organization_id.0));
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(map_sqlite)?;
        set_private_file(&path)?;
        connection
            .busy_timeout(config.busy_timeout)
            .map_err(map_sqlite)?;
        configure(&connection)?;
        migrate(&connection, organization_id)?;
        Ok(Self {
            organization_id,
            path,
            connection: Mutex::new(connection),
            subject_key,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn organization_id(&self) -> OrganizationId {
        self.organization_id
    }

    pub fn hash_active_subject(
        &self,
        period: BillingPeriod,
        subject: &str,
    ) -> Result<[u8; 32], MetricsError> {
        period.validate()?;
        if subject.is_empty() || subject.len() > 512 || subject.bytes().any(|byte| byte == 0) {
            return Err(MetricsError::InvalidInput);
        }
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(&self.subject_key)
            .map_err(|_| MetricsError::InvalidConfiguration)?;
        mac.update(b"ffdb.metrics.mau.v1\0");
        mac.update(self.organization_id.0.as_bytes());
        mac.update(&period.start_ms.to_be_bytes());
        mac.update(&period.end_ms.to_be_bytes());
        mac.update(subject.as_bytes());
        Ok(mac.finalize().into_bytes().into())
    }

    pub fn reserve(
        &self,
        request: &ReservationRequest,
        limits: UsageLimits,
    ) -> Result<ReservationAdmission, MetricsError> {
        validate_reservation(request)?;
        let digest = reservation_digest(request);
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        ensure_period(&transaction, request.period)?;
        ensure_accepting(&transaction, request.period, request.created_at_ms)?;
        expire_reservations(&transaction, request.created_at_ms)?;

        if let Some((stored, status)) = transaction
            .query_row(
                "SELECT request_sha256,status FROM usage_reservations WHERE reservation_id=?1",
                [&request.reservation_id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?
        {
            if stored.as_slice() != digest {
                return Err(MetricsError::HashConflict);
            }
            transaction.commit().map_err(map_sqlite)?;
            return Ok(ReservationAdmission::Duplicate(parse_reservation_status(
                &status,
            )?));
        }

        let (reads, writes): (i64, i64) = transaction
            .query_row(
                "SELECT COALESCE(SUM(read_units),0),COALESCE(SUM(write_units),0) \
                 FROM usage_hourly WHERE period_start_ms=?1",
                [request.period.start_ms],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite)?;
        let (reserved_reads, reserved_writes, reserved_storage): (i64, i64, i64) = transaction
            .query_row(
                "SELECT COALESCE(SUM(read_units),0),COALESCE(SUM(write_units),0), \
                        COALESCE(SUM(storage_growth_bytes),0) FROM usage_reservations \
                 WHERE period_start_ms=?1 AND status='active'",
                [request.period.start_ms],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(map_sqlite)?;
        let storage: i64 = transaction
            .query_row(
                "SELECT COALESCE(SUM(logical_bytes),0) FROM storage_current",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        if limits.enforce_read_limit {
            enforce_limit(
                as_u64(reads)?.saturating_add(as_u64(reserved_reads)?),
                request.read_units,
                limits.read_units,
                "read_units",
            )?;
        }
        if limits.enforce_write_limit {
            enforce_limit(
                as_u64(writes)?.saturating_add(as_u64(reserved_writes)?),
                request.write_units,
                limits.write_units,
                "write_units",
            )?;
        }
        if limits.enforce_storage_limit {
            enforce_limit(
                as_u64(storage)?.saturating_add(as_u64(reserved_storage)?),
                request.storage_growth_bytes,
                limits.storage_bytes,
                "storage_bytes",
            )?;
        }

        let mau_claimed = if let Some(subject_hash) = request.active_subject_hash {
            let existing = transaction
                .query_row(
                    "SELECT active,reservation_count FROM active_user_claims \
                     WHERE period_start_ms=?1 AND subject_hash=?2",
                    params![request.period.start_ms, subject_hash.as_slice()],
                    |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            if existing.is_none() && limits.enforce_mau_limit {
                let claimed: i64 = transaction
                    .query_row(
                        "SELECT count(*) FROM active_user_claims WHERE period_start_ms=?1 \
                         AND (active=1 OR reservation_count>0)",
                        [request.period.start_ms],
                        |row| row.get(0),
                    )
                    .map_err(map_sqlite)?;
                enforce_limit(
                    as_u64(claimed)?,
                    1,
                    limits.monthly_active_users,
                    "monthly_active_users",
                )?;
            }
            transaction
                .execute(
                    "INSERT INTO active_user_claims \
                     (period_start_ms,period_end_ms,subject_hash,active,reservation_count,first_seen_ms,last_seen_ms) \
                     VALUES (?1,?2,?3,0,1,NULL,NULL) \
                     ON CONFLICT(period_start_ms,subject_hash) DO UPDATE SET \
                       reservation_count=reservation_count+1",
                    params![
                        request.period.start_ms,
                        request.period.end_ms,
                        subject_hash.as_slice()
                    ],
                )
                .map_err(map_sqlite)?;
            true
        } else {
            false
        };

        transaction
            .execute(
                "INSERT INTO usage_reservations \
                 (reservation_id,request_sha256,project_id,period_start_ms,period_end_ms, \
                  created_at_ms,expires_at_ms,read_units,write_units,storage_growth_bytes, \
                  subject_hash,mau_claimed,status,event_id) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'active',NULL)",
                params![
                    request.reservation_id,
                    digest.as_slice(),
                    request.project_id.0.to_string(),
                    request.period.start_ms,
                    request.period.end_ms,
                    request.created_at_ms,
                    request.expires_at_ms,
                    to_i64(request.read_units)?,
                    to_i64(request.write_units)?,
                    to_i64(request.storage_growth_bytes)?,
                    request.active_subject_hash.map(|value| value.to_vec()),
                    mau_claimed,
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(ReservationAdmission::Reserved)
    }

    pub fn release_reservation(
        &self,
        reservation_id: &str,
        now_ms: i64,
    ) -> Result<ReservationStatus, MetricsError> {
        validate_identifier(reservation_id)?;
        if now_ms < 0 {
            return Err(MetricsError::InvalidInput);
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let row = reservation_row(&transaction, reservation_id)?
            .ok_or(MetricsError::ReservationNotFound)?;
        let status = parse_reservation_status(&row.status)?;
        if status == ReservationStatus::Active {
            release_mau_claim(&transaction, &row)?;
            transaction
                .execute(
                    "UPDATE usage_reservations SET status='released',resolved_at_ms=?2 \
                     WHERE reservation_id=?1 AND status='active'",
                    params![reservation_id, now_ms],
                )
                .map_err(map_sqlite)?;
        }
        transaction.commit().map_err(map_sqlite)?;
        Ok(if status == ReservationStatus::Active {
            ReservationStatus::Released
        } else {
            status
        })
    }

    pub fn settle_reservation(
        &self,
        reservation_id: &str,
        event: &UsageEvent,
    ) -> Result<IngestOutcome, MetricsError> {
        validate_identifier(reservation_id)?;
        validate_event(event)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let reservation = reservation_row(&transaction, reservation_id)?
            .ok_or(MetricsError::ReservationNotFound)?;
        let status = parse_reservation_status(&reservation.status)?;
        if status == ReservationStatus::Settled {
            if reservation.event_id.as_deref() == Some(event.event_id.as_str()) {
                let outcome = ingest_event(&transaction, event)?;
                transaction.commit().map_err(map_sqlite)?;
                return Ok(outcome);
            }
            return Err(MetricsError::ReservationMismatch);
        }
        if status == ReservationStatus::Released {
            return Err(MetricsError::ReservationMismatch);
        }
        if reservation.project_id != event.project_id.0.to_string()
            || reservation.period_start_ms != event.period.start_ms
            || reservation.period_end_ms != event.period.end_ms
            || event.read_units > as_u64(reservation.read_units)?
            || event.write_units > as_u64(reservation.write_units)?
            || reservation.subject_hash.as_deref()
                != event.active_subject_hash.as_ref().map(<[u8; 32]>::as_slice)
        {
            return Err(MetricsError::ReservationMismatch);
        }
        ensure_accepting(&transaction, event.period, event.recorded_at_ms)?;
        let outcome = ingest_event(&transaction, event)?;
        activate_mau_claim(&transaction, &reservation, event.recorded_at_ms)?;
        transaction
            .execute(
                "UPDATE usage_reservations SET status='settled',event_id=?2,resolved_at_ms=?3 \
                 WHERE reservation_id=?1 AND status IN ('active','expired')",
                params![reservation_id, event.event_id, event.recorded_at_ms],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(outcome)
    }

    pub fn reservation_context(
        &self,
        reservation_id: &str,
    ) -> Result<Option<ReservationContext>, MetricsError> {
        validate_identifier(reservation_id)?;
        let connection = self.lock()?;
        reservation_row(&connection, reservation_id)?
            .map(|row| {
                Ok(ReservationContext {
                    project_id: ProjectId(
                        Uuid::parse_str(&row.project_id).map_err(|_| MetricsError::Unavailable)?,
                    ),
                    period_start_ms: row.period_start_ms,
                    period_end_ms: row.period_end_ms,
                    status: parse_reservation_status(&row.status)?,
                })
            })
            .transpose()
    }

    pub fn record_event(&self, event: &UsageEvent) -> Result<IngestOutcome, MetricsError> {
        validate_event(event)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        ensure_period(&transaction, event.period)?;
        ensure_accepting(&transaction, event.period, event.recorded_at_ms)?;
        let outcome = ingest_event(&transaction, event)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(outcome)
    }

    pub fn summary(&self, period: BillingPeriod) -> Result<UsageSummary, MetricsError> {
        self.summary_through(period, period.end_ms)
    }

    /// Returns cumulative usage from the period start through an immutable
    /// reporting boundary. Callers use cumulative values and subtract the
    /// durable enqueued quantity, so a crash can never create a second delta.
    pub fn summary_through(
        &self,
        period: BillingPeriod,
        through_ms: i64,
    ) -> Result<UsageSummary, MetricsError> {
        period.validate()?;
        if through_ms <= period.start_ms || through_ms > period.end_ms {
            return Err(MetricsError::InvalidInput);
        }
        let connection = self.lock()?;
        let (reads, writes): (i64, i64) = connection
            .query_row(
                "SELECT COALESCE(SUM(read_units),0),COALESCE(SUM(write_units),0) \
                 FROM usage_hourly WHERE period_start_ms=?1 AND bucket_start_ms<?2",
                params![period.start_ms, through_ms],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite)?;
        let mau: i64 = connection
            .query_row(
                "SELECT count(*) FROM active_user_claims \
                 WHERE period_start_ms=?1 AND active=1 AND first_seen_ms<?2",
                params![period.start_ms, through_ms],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        let (storage_bytes_at_end, storage_byte_milliseconds) =
            storage_integral(&connection, period.start_ms, through_ms)?;
        Ok(UsageSummary {
            read_units: as_u64(reads)?,
            write_units: as_u64(writes)?,
            monthly_active_users: as_u64(mau)?,
            storage_bytes_at_end,
            storage_byte_milliseconds,
        })
    }

    pub fn reporting_progress(
        &self,
        period: BillingPeriod,
        dimension: UsageDimension,
    ) -> Result<ReportingProgress, MetricsError> {
        period.validate()?;
        let connection = self.lock()?;
        let (quantity, window_end): (i64, Option<i64>) = connection
            .query_row(
                "SELECT COALESCE(SUM(quantity),0),MAX(window_end_ms) FROM stripe_meter_outbox \
                 WHERE period_start_ms=?1 AND period_end_ms=?2 AND dimension=?3",
                params![period.start_ms, period.end_ms, dimension.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite)?;
        Ok(ReportingProgress {
            enqueued_quantity: as_u64(quantity)?,
            latest_window_end_ms: window_end,
        })
    }

    pub fn outbox_checkpoint(
        &self,
        period: BillingPeriod,
    ) -> Result<OutboxCheckpoint, MetricsError> {
        period.validate()?;
        let connection = self.lock()?;
        let (outstanding, failed, last_ack): (i64, i64, Option<i64>) = connection
            .query_row(
                "SELECT \
                   COALESCE(SUM(CASE WHEN state<>'acknowledged' THEN 1 ELSE 0 END),0), \
                   COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0), \
                   MAX(acknowledged_at_ms) FROM stripe_meter_outbox \
                 WHERE period_start_ms=?1 AND period_end_ms=?2",
                params![period.start_ms, period.end_ms],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(map_sqlite)?;
        Ok(OutboxCheckpoint {
            outstanding: as_u64(outstanding)?,
            failed: as_u64(failed)?,
            last_acknowledged_at_ms: last_ack,
        })
    }

    pub fn period_status(
        &self,
        period: BillingPeriod,
    ) -> Result<BillingPeriodStatus, MetricsError> {
        period.validate()?;
        let connection = self.lock()?;
        ensure_period(&connection, period)?;
        let status: String = connection
            .query_row(
                "SELECT status FROM billing_periods WHERE period_start_ms=?1 AND period_end_ms=?2",
                params![period.start_ms, period.end_ms],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        match status.as_str() {
            "open" | "closing" => Ok(BillingPeriodStatus::Open),
            "sealed" => Ok(BillingPeriodStatus::Sealed),
            "finalized" => Ok(BillingPeriodStatus::Finalized),
            _ => Err(MetricsError::Unavailable),
        }
    }

    pub fn storage_resource_bytes(&self, resource_id: &str) -> Result<u64, MetricsError> {
        validate_resource_id(resource_id)?;
        let connection = self.lock()?;
        let value: i64 = connection
            .query_row(
                "SELECT COALESCE((SELECT logical_bytes FROM storage_current WHERE resource_id=?1),0)",
                [resource_id],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        as_u64(value)
    }

    pub fn seal_period(&self, period: BillingPeriod, now_ms: i64) -> Result<(), MetricsError> {
        period.validate()?;
        if now_ms < period.cutoff_ms {
            return Err(MetricsError::InvalidInput);
        }
        let connection = self.lock()?;
        ensure_period(&connection, period)?;
        connection
            .execute(
                "UPDATE billing_periods SET status='sealed',sealed_at_ms=?2 \
                 WHERE period_start_ms=?1 AND status IN ('open','closing')",
                params![period.start_ms, now_ms],
            )
            .map_err(map_sqlite)?;
        Ok(())
    }

    pub fn enqueue_report(&self, report: &ReportRequest) -> Result<IngestOutcome, MetricsError> {
        validate_report(report)?;
        let digest = report_digest(report);
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        ensure_period(&transaction, report.period)?;
        if let Some(stored) = transaction
            .query_row(
                "SELECT payload_sha256 FROM stripe_meter_outbox WHERE identifier=?1",
                [&report.identifier],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(map_sqlite)?
        {
            if stored.as_slice() == digest {
                transaction.commit().map_err(map_sqlite)?;
                return Ok(IngestOutcome::Duplicate);
            }
            return Err(MetricsError::HashConflict);
        }
        transaction
            .execute(
                "INSERT INTO stripe_meter_outbox \
                 (identifier,payload_sha256,event_name,customer_id,period_start_ms,period_end_ms, \
                  dimension,window_start_ms,window_end_ms,quantity,provider_timestamp_ms,state, \
                  attempt_count,next_attempt_ms,lease_token,lease_expires_ms,provider_event_id, \
                  last_error,created_at_ms,acknowledged_at_ms) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',0,?12,NULL,NULL,NULL,NULL,?12,NULL)",
                params![
                    report.identifier,
                    digest.as_slice(),
                    report.event_name,
                    report.customer_id,
                    report.period.start_ms,
                    report.period.end_ms,
                    report.dimension.as_str(),
                    report.window_start_ms,
                    report.window_end_ms,
                    to_i64(report.quantity)?,
                    report.provider_timestamp_ms,
                    report.now_ms,
                ],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(IngestOutcome::Inserted)
    }

    pub fn claim_reports(
        &self,
        now_ms: i64,
        lease_duration_ms: i64,
        limit: u32,
    ) -> Result<Vec<ClaimedReport>, MetricsError> {
        self.claim_reports_inner(None, now_ms, lease_duration_ms, limit)
    }

    pub fn claim_reports_for_period(
        &self,
        period: BillingPeriod,
        now_ms: i64,
        lease_duration_ms: i64,
        limit: u32,
    ) -> Result<Vec<ClaimedReport>, MetricsError> {
        period.validate()?;
        self.claim_reports_inner(Some(period), now_ms, lease_duration_ms, limit)
    }

    fn claim_reports_inner(
        &self,
        period: Option<BillingPeriod>,
        now_ms: i64,
        lease_duration_ms: i64,
        limit: u32,
    ) -> Result<Vec<ClaimedReport>, MetricsError> {
        if now_ms < 0
            || !(1_000..=10 * 60 * 1_000).contains(&lease_duration_ms)
            || !(1..=100).contains(&limit)
        {
            return Err(MetricsError::InvalidInput);
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let mut statement = transaction
            .prepare(
                "SELECT identifier,event_name,customer_id,dimension,quantity,provider_timestamp_ms,attempt_count \
                 FROM stripe_meter_outbox WHERE \
                   (?3 IS NULL OR (period_start_ms=?3 AND period_end_ms=?4)) AND \
                   ((state IN ('pending','failed') AND next_attempt_ms<=?1) OR \
                   (state='in_flight' AND lease_expires_ms<=?1)) \
                 ORDER BY provider_timestamp_ms,identifier LIMIT ?2",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(
                params![
                    now_ms,
                    limit,
                    period.map(|value| value.start_ms),
                    period.map(|value| value.end_ms)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .map_err(map_sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        drop(statement);
        let mut claimed = Vec::with_capacity(rows.len());
        for (identifier, event_name, customer_id, dimension, quantity, timestamp, attempt) in rows {
            let lease_token = Uuid::now_v7().to_string();
            let updated = transaction
                .execute(
                    "UPDATE stripe_meter_outbox SET state='in_flight',attempt_count=attempt_count+1, \
                     lease_token=?2,lease_expires_ms=?3,last_error=NULL WHERE identifier=?1 AND \
                     ((state IN ('pending','failed') AND next_attempt_ms<=?4) OR \
                      (state='in_flight' AND lease_expires_ms<=?4))",
                    params![
                        identifier,
                        lease_token,
                        now_ms.saturating_add(lease_duration_ms),
                        now_ms
                    ],
                )
                .map_err(map_sqlite)?;
            if updated == 1 {
                claimed.push(ClaimedReport {
                    identifier,
                    event_name,
                    customer_id,
                    dimension: parse_dimension(&dimension)?,
                    quantity: as_u64(quantity)?,
                    provider_timestamp_ms: timestamp,
                    lease_token,
                    attempt: u32::try_from(attempt.saturating_add(1))
                        .map_err(|_| MetricsError::Unavailable)?,
                });
            }
        }
        transaction.commit().map_err(map_sqlite)?;
        Ok(claimed)
    }

    pub fn acknowledge_report(
        &self,
        identifier: &str,
        lease_token: &str,
        provider_event_id: &str,
        now_ms: i64,
    ) -> Result<(), MetricsError> {
        validate_identifier(identifier)?;
        validate_identifier(lease_token)?;
        validate_identifier(provider_event_id)?;
        let connection = self.lock()?;
        let updated = connection
            .execute(
                "UPDATE stripe_meter_outbox SET state='acknowledged',provider_event_id=?3, \
                 acknowledged_at_ms=?4,lease_token=NULL,lease_expires_ms=NULL,last_error=NULL \
                 WHERE identifier=?1 AND state='in_flight' AND lease_token=?2",
                params![identifier, lease_token, provider_event_id, now_ms],
            )
            .map_err(map_sqlite)?;
        if updated == 1 {
            Ok(())
        } else {
            Err(MetricsError::StaleLease)
        }
    }

    pub fn fail_report(
        &self,
        identifier: &str,
        lease_token: &str,
        retry_at_ms: i64,
        error: &str,
    ) -> Result<(), MetricsError> {
        validate_identifier(identifier)?;
        validate_identifier(lease_token)?;
        if retry_at_ms < 0 || error.is_empty() || error.len() > MAX_PROVIDER_ERROR_BYTES {
            return Err(MetricsError::InvalidInput);
        }
        let connection = self.lock()?;
        let updated = connection
            .execute(
                "UPDATE stripe_meter_outbox SET state='failed',next_attempt_ms=?3,last_error=?4, \
                 lease_token=NULL,lease_expires_ms=NULL \
                 WHERE identifier=?1 AND state='in_flight' AND lease_token=?2",
                params![identifier, lease_token, retry_at_ms, error],
            )
            .map_err(map_sqlite)?;
        if updated == 1 {
            Ok(())
        } else {
            Err(MetricsError::StaleLease)
        }
    }

    pub fn record_reconciliation(
        &self,
        period: BillingPeriod,
        dimension: UsageDimension,
        local_quantity: u64,
        provider_quantity: u64,
        checked_at_ms: i64,
    ) -> Result<ReconciliationStatus, MetricsError> {
        period.validate()?;
        let status = if local_quantity == provider_quantity {
            ReconciliationStatus::Matched
        } else {
            ReconciliationStatus::Mismatched
        };
        let connection = self.lock()?;
        ensure_period(&connection, period)?;
        connection
            .execute(
                "INSERT INTO stripe_reconciliations \
                 (period_start_ms,dimension,local_quantity,provider_quantity,status,checked_at_ms) \
                 VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(period_start_ms,dimension) DO UPDATE SET \
                   local_quantity=excluded.local_quantity,provider_quantity=excluded.provider_quantity, \
                   status=excluded.status,checked_at_ms=excluded.checked_at_ms",
                params![
                    period.start_ms,
                    dimension.as_str(),
                    to_i64(local_quantity)?,
                    to_i64(provider_quantity)?,
                    if status == ReconciliationStatus::Matched { "matched" } else { "mismatched" },
                    checked_at_ms,
                ],
            )
            .map_err(map_sqlite)?;
        Ok(status)
    }

    pub fn finalize_period(&self, period: BillingPeriod, now_ms: i64) -> Result<(), MetricsError> {
        period.validate()?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let status: String = transaction
            .query_row(
                "SELECT status FROM billing_periods WHERE period_start_ms=?1 AND period_end_ms=?2",
                params![period.start_ms, period.end_ms],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite)?
            .ok_or(MetricsError::ReconciliationPending)?;
        let outstanding: i64 = transaction
            .query_row(
                "SELECT count(*) FROM stripe_meter_outbox WHERE period_start_ms=?1 AND state<>'acknowledged'",
                [period.start_ms],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        let matched: i64 = transaction
            .query_row(
                "SELECT count(*) FROM stripe_reconciliations WHERE period_start_ms=?1 AND status='matched'",
                [period.start_ms],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        let mismatched: i64 = transaction
            .query_row(
                "SELECT count(*) FROM stripe_reconciliations WHERE period_start_ms=?1 AND status='mismatched'",
                [period.start_ms],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        if status != "sealed" || outstanding != 0 || matched != 4 || mismatched != 0 {
            return Err(MetricsError::ReconciliationPending);
        }
        transaction
            .execute(
                "UPDATE billing_periods SET status='finalized',finalized_at_ms=?2 \
                 WHERE period_start_ms=?1 AND status='sealed'",
                params![period.start_ms, now_ms],
            )
            .map_err(map_sqlite)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(())
    }

    pub fn backup_to(
        &self,
        destination: &Path,
        created_at_ms: i64,
    ) -> Result<BackupManifest, MetricsError> {
        if !destination.is_absolute() || destination.exists() || created_at_ms < 0 {
            return Err(MetricsError::InvalidInput);
        }
        let parent = destination.parent().ok_or(MetricsError::InvalidInput)?;
        fs::create_dir_all(parent).map_err(|_| MetricsError::Unavailable)?;
        let name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(MetricsError::InvalidInput)?;
        let temporary = parent.join(format!(".{name}.{}.tmp", Uuid::now_v7()));
        let result = (|| {
            let source = self.lock()?;
            let mut target = Connection::open(&temporary).map_err(map_sqlite)?;
            let backup = rusqlite::backup::Backup::new(&source, &mut target).map_err(map_sqlite)?;
            loop {
                match backup.step(128).map_err(map_sqlite)? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More => {}
                    rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                        std::thread::yield_now()
                    }
                    _ => return Err(MetricsError::Unavailable),
                }
            }
            drop(backup);
            let integrity: String = target
                .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
                .map_err(map_sqlite)?;
            if integrity != "ok" {
                return Err(MetricsError::Unavailable);
            }
            drop(target);
            set_private_file(&temporary)?;
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| MetricsError::Unavailable)?;
            file.flush().map_err(|_| MetricsError::Unavailable)?;
            file.sync_all().map_err(|_| MetricsError::Unavailable)?;
            drop(file);
            fs::rename(&temporary, destination).map_err(|_| MetricsError::Unavailable)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| MetricsError::Unavailable)?;
            let (size_bytes, sha256) = hash_file(destination)?;
            Ok(BackupManifest {
                organization_id: self.organization_id,
                schema_version: u32::try_from(SCHEMA_VERSION)
                    .map_err(|_| MetricsError::Unavailable)?,
                created_at_ms,
                size_bytes,
                sha256,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn verify_backup(path: &Path, expected: &BackupManifest) -> Result<(), MetricsError> {
        if !path.is_absolute() || expected.schema_version != SCHEMA_VERSION as u32 {
            return Err(MetricsError::InvalidInput);
        }
        let (size, digest) = hash_file(path)?;
        if size != expected.size_bytes || digest != expected.sha256 {
            return Err(MetricsError::HashConflict);
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(map_sqlite)?;
        let (organization, version): (String, i64) = connection
            .query_row(
                "SELECT organization_id,schema_version FROM metrics_metadata WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite)?;
        let integrity: String = connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(map_sqlite)?;
        if organization != expected.organization_id.0.to_string()
            || version != SCHEMA_VERSION
            || integrity != "ok"
        {
            return Err(MetricsError::OrganizationMismatch);
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, MetricsError> {
        self.connection
            .lock()
            .map_err(|_| MetricsError::Unavailable)
    }
}

fn configure(connection: &Connection) -> Result<(), MetricsError> {
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL; \
             PRAGMA synchronous=FULL; \
             PRAGMA foreign_keys=ON; \
             PRAGMA trusted_schema=OFF; \
             PRAGMA recursive_triggers=ON; \
             PRAGMA secure_delete=ON; \
             PRAGMA wal_autocheckpoint=1000; \
             PRAGMA journal_size_limit=67108864;",
        )
        .map_err(map_sqlite)
}

fn migrate(connection: &Connection, organization_id: OrganizationId) -> Result<(), MetricsError> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE; \
             CREATE TABLE IF NOT EXISTS metrics_metadata( \
               singleton INTEGER PRIMARY KEY CHECK(singleton=1), \
               organization_id TEXT NOT NULL, schema_version INTEGER NOT NULL); \
             CREATE TABLE IF NOT EXISTS billing_periods( \
               period_start_ms INTEGER PRIMARY KEY, period_end_ms INTEGER NOT NULL,cutoff_ms INTEGER NOT NULL, \
               status TEXT NOT NULL CHECK(status IN ('open','closing','sealed','finalized')), \
               sealed_at_ms INTEGER,finalized_at_ms INTEGER, \
               CHECK(period_end_ms>period_start_ms AND cutoff_ms>=period_end_ms)); \
             CREATE TABLE IF NOT EXISTS event_receipts( \
               event_id TEXT PRIMARY KEY,payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256)=32), \
               project_id TEXT NOT NULL,period_start_ms INTEGER NOT NULL,occurred_at_ms INTEGER NOT NULL, \
               recorded_at_ms INTEGER NOT NULL,FOREIGN KEY(period_start_ms) REFERENCES billing_periods(period_start_ms)); \
             CREATE TABLE IF NOT EXISTS usage_hourly( \
               period_start_ms INTEGER NOT NULL,bucket_start_ms INTEGER NOT NULL,project_id TEXT NOT NULL, \
               read_units INTEGER NOT NULL DEFAULT 0 CHECK(read_units>=0), \
               write_units INTEGER NOT NULL DEFAULT 0 CHECK(write_units>=0), \
               PRIMARY KEY(period_start_ms,bucket_start_ms,project_id), \
               FOREIGN KEY(period_start_ms) REFERENCES billing_periods(period_start_ms)); \
             CREATE TABLE IF NOT EXISTS active_user_claims( \
               period_start_ms INTEGER NOT NULL,period_end_ms INTEGER NOT NULL, \
               subject_hash BLOB NOT NULL CHECK(length(subject_hash)=32),active INTEGER NOT NULL CHECK(active IN (0,1)), \
               reservation_count INTEGER NOT NULL DEFAULT 0 CHECK(reservation_count>=0), \
               first_seen_ms INTEGER,last_seen_ms INTEGER,PRIMARY KEY(period_start_ms,subject_hash), \
               FOREIGN KEY(period_start_ms) REFERENCES billing_periods(period_start_ms)); \
             CREATE TABLE IF NOT EXISTS storage_transitions( \
               resource_id TEXT NOT NULL,occurred_at_ms INTEGER NOT NULL,event_id TEXT NOT NULL, \
               logical_bytes INTEGER NOT NULL CHECK(logical_bytes>=0), \
               PRIMARY KEY(resource_id,occurred_at_ms,event_id), \
               FOREIGN KEY(event_id) REFERENCES event_receipts(event_id)); \
             CREATE INDEX IF NOT EXISTS storage_transitions_time_idx \
               ON storage_transitions(occurred_at_ms,resource_id); \
             CREATE TABLE IF NOT EXISTS storage_current( \
               resource_id TEXT PRIMARY KEY,occurred_at_ms INTEGER NOT NULL,event_id TEXT NOT NULL, \
               logical_bytes INTEGER NOT NULL CHECK(logical_bytes>=0), \
               FOREIGN KEY(event_id) REFERENCES event_receipts(event_id)); \
             CREATE TABLE IF NOT EXISTS usage_reservations( \
               reservation_id TEXT PRIMARY KEY,request_sha256 BLOB NOT NULL CHECK(length(request_sha256)=32), \
               project_id TEXT NOT NULL,period_start_ms INTEGER NOT NULL,period_end_ms INTEGER NOT NULL, \
               created_at_ms INTEGER NOT NULL,expires_at_ms INTEGER NOT NULL, \
               read_units INTEGER NOT NULL CHECK(read_units>=0),write_units INTEGER NOT NULL CHECK(write_units>=0), \
               storage_growth_bytes INTEGER NOT NULL CHECK(storage_growth_bytes>=0),subject_hash BLOB, \
               mau_claimed INTEGER NOT NULL CHECK(mau_claimed IN (0,1)), \
               status TEXT NOT NULL CHECK(status IN ('active','settled','released','expired')), \
               event_id TEXT,resolved_at_ms INTEGER,FOREIGN KEY(period_start_ms) REFERENCES billing_periods(period_start_ms)); \
             CREATE INDEX IF NOT EXISTS usage_reservations_active_idx \
               ON usage_reservations(period_start_ms,status,expires_at_ms); \
             CREATE TABLE IF NOT EXISTS stripe_meter_outbox( \
               identifier TEXT PRIMARY KEY,payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256)=32), \
               event_name TEXT NOT NULL,customer_id TEXT NOT NULL,period_start_ms INTEGER NOT NULL, \
               period_end_ms INTEGER NOT NULL,dimension TEXT NOT NULL CHECK(dimension IN \
                 ('read_units','write_units','monthly_active_users','storage_byte_milliseconds')), \
               window_start_ms INTEGER NOT NULL,window_end_ms INTEGER NOT NULL,quantity INTEGER NOT NULL CHECK(quantity>=0), \
               provider_timestamp_ms INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN \
                 ('pending','in_flight','failed','acknowledged')),attempt_count INTEGER NOT NULL CHECK(attempt_count>=0), \
               next_attempt_ms INTEGER NOT NULL,lease_token TEXT,lease_expires_ms INTEGER,provider_event_id TEXT, \
               last_error TEXT,created_at_ms INTEGER NOT NULL,acknowledged_at_ms INTEGER, \
               FOREIGN KEY(period_start_ms) REFERENCES billing_periods(period_start_ms)); \
             CREATE INDEX IF NOT EXISTS stripe_meter_outbox_claim_idx \
               ON stripe_meter_outbox(state,next_attempt_ms,lease_expires_ms,provider_timestamp_ms); \
             CREATE TABLE IF NOT EXISTS stripe_reconciliations( \
               period_start_ms INTEGER NOT NULL,dimension TEXT NOT NULL,local_quantity INTEGER NOT NULL CHECK(local_quantity>=0), \
               provider_quantity INTEGER NOT NULL CHECK(provider_quantity>=0), \
               status TEXT NOT NULL CHECK(status IN ('matched','mismatched')),checked_at_ms INTEGER NOT NULL, \
               PRIMARY KEY(period_start_ms,dimension), \
               FOREIGN KEY(period_start_ms) REFERENCES billing_periods(period_start_ms)); \
             COMMIT;",
        )
        .map_err(map_sqlite)?;
    let existing = connection
        .query_row(
            "SELECT organization_id,schema_version FROM metrics_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    match existing {
        Some((organization, version))
            if organization == organization_id.0.to_string() && version == SCHEMA_VERSION => Ok(()),
        Some(_) => Err(MetricsError::OrganizationMismatch),
        None => connection
            .execute(
                "INSERT INTO metrics_metadata(singleton,organization_id,schema_version) VALUES (1,?1,?2)",
                params![organization_id.0.to_string(), SCHEMA_VERSION],
            )
            .map(|_| ())
            .map_err(map_sqlite),
    }
}

fn ensure_period(connection: &Connection, period: BillingPeriod) -> Result<(), MetricsError> {
    period.validate()?;
    let changed = connection
        .execute(
            "INSERT INTO billing_periods(period_start_ms,period_end_ms,cutoff_ms,status) \
             VALUES (?1,?2,?3,'open') ON CONFLICT(period_start_ms) DO NOTHING",
            params![period.start_ms, period.end_ms, period.cutoff_ms],
        )
        .map_err(map_sqlite)?;
    if changed == 0 {
        let valid: bool = connection
            .query_row(
                "SELECT period_end_ms=?2 AND cutoff_ms=?3 FROM billing_periods WHERE period_start_ms=?1",
                params![period.start_ms, period.end_ms, period.cutoff_ms],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        if !valid {
            return Err(MetricsError::HashConflict);
        }
    }
    Ok(())
}

fn ensure_accepting(
    connection: &Connection,
    period: BillingPeriod,
    recorded_at_ms: i64,
) -> Result<(), MetricsError> {
    let status: String = connection
        .query_row(
            "SELECT status FROM billing_periods WHERE period_start_ms=?1",
            [period.start_ms],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if matches!(status.as_str(), "sealed" | "finalized") || recorded_at_ms > period.cutoff_ms {
        Err(MetricsError::PeriodSealed)
    } else {
        Ok(())
    }
}

fn ingest_event(
    connection: &Connection,
    event: &UsageEvent,
) -> Result<IngestOutcome, MetricsError> {
    ensure_period(connection, event.period)?;
    let digest = event_digest(event);
    let inserted = connection
        .execute(
            "INSERT INTO event_receipts \
             (event_id,payload_sha256,project_id,period_start_ms,occurred_at_ms,recorded_at_ms) \
             VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(event_id) DO NOTHING",
            params![
                event.event_id,
                digest.as_slice(),
                event.project_id.0.to_string(),
                event.period.start_ms,
                event.occurred_at_ms,
                event.recorded_at_ms
            ],
        )
        .map_err(map_sqlite)?;
    if inserted == 0 {
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT payload_sha256 FROM event_receipts WHERE event_id=?1",
                [&event.event_id],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        return if stored.as_slice() == digest {
            Ok(IngestOutcome::Duplicate)
        } else {
            Err(MetricsError::HashConflict)
        };
    }
    let bucket = event.occurred_at_ms - event.occurred_at_ms.rem_euclid(HOUR_MS);
    connection
        .execute(
            "INSERT INTO usage_hourly(period_start_ms,bucket_start_ms,project_id,read_units,write_units) \
             VALUES (?1,?2,?3,?4,?5) ON CONFLICT(period_start_ms,bucket_start_ms,project_id) DO UPDATE SET \
               read_units=read_units+excluded.read_units,write_units=write_units+excluded.write_units",
            params![
                event.period.start_ms,
                bucket,
                event.project_id.0.to_string(),
                to_i64(event.read_units)?,
                to_i64(event.write_units)?
            ],
        )
        .map_err(map_sqlite)?;
    if let Some(subject_hash) = event.active_subject_hash {
        connection
            .execute(
                "INSERT INTO active_user_claims \
                 (period_start_ms,period_end_ms,subject_hash,active,reservation_count,first_seen_ms,last_seen_ms) \
                 VALUES (?1,?2,?3,1,0,?4,?4) ON CONFLICT(period_start_ms,subject_hash) DO UPDATE SET \
                   active=1,first_seen_ms=COALESCE(first_seen_ms,excluded.first_seen_ms), \
                   last_seen_ms=MAX(COALESCE(last_seen_ms,excluded.last_seen_ms),excluded.last_seen_ms)",
                params![
                    event.period.start_ms,
                    event.period.end_ms,
                    subject_hash.as_slice(),
                    event.occurred_at_ms
                ],
            )
            .map_err(map_sqlite)?;
    }
    for snapshot in &event.storage_snapshots {
        connection
            .execute(
                "INSERT INTO storage_transitions(resource_id,occurred_at_ms,event_id,logical_bytes) \
                 VALUES (?1,?2,?3,?4)",
                params![
                    snapshot.resource_id,
                    event.occurred_at_ms,
                    event.event_id,
                    to_i64(snapshot.logical_bytes)?
                ],
            )
            .map_err(map_sqlite)?;
        connection
            .execute(
                "INSERT INTO storage_current(resource_id,occurred_at_ms,event_id,logical_bytes) \
                 VALUES (?1,?2,?3,?4) ON CONFLICT(resource_id) DO UPDATE SET \
                   occurred_at_ms=excluded.occurred_at_ms,event_id=excluded.event_id,logical_bytes=excluded.logical_bytes \
                 WHERE (excluded.occurred_at_ms,excluded.event_id)>(storage_current.occurred_at_ms,storage_current.event_id)",
                params![
                    snapshot.resource_id,
                    event.occurred_at_ms,
                    event.event_id,
                    to_i64(snapshot.logical_bytes)?
                ],
            )
            .map_err(map_sqlite)?;
    }
    Ok(IngestOutcome::Inserted)
}

fn storage_integral(
    connection: &Connection,
    start_ms: i64,
    end_ms: i64,
) -> Result<(u64, u128), MetricsError> {
    let mut statement = connection
        .prepare(
            "SELECT resource_id,occurred_at_ms,event_id,logical_bytes FROM storage_transitions \
             WHERE occurred_at_ms<?1 ORDER BY resource_id,occurred_at_ms,event_id",
        )
        .map_err(map_sqlite)?;
    let transitions = statement
        .query_map([end_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    let mut resources: BTreeMap<String, Vec<(i64, String, u64)>> = BTreeMap::new();
    for (resource, at, event_id, bytes) in transitions {
        resources
            .entry(resource)
            .or_default()
            .push((at, event_id, as_u64(bytes)?));
    }
    let mut bytes_at_end = 0_u64;
    let mut byte_milliseconds = 0_u128;
    for transitions in resources.values() {
        let mut current = 0_u64;
        let mut cursor = start_ms;
        for (at, _, bytes) in transitions {
            if *at <= start_ms {
                current = *bytes;
                continue;
            }
            let duration = at.saturating_sub(cursor);
            byte_milliseconds = byte_milliseconds
                .checked_add(
                    u128::from(current)
                        * u128::try_from(duration).map_err(|_| MetricsError::Unavailable)?,
                )
                .ok_or(MetricsError::Unavailable)?;
            current = *bytes;
            cursor = *at;
        }
        let duration = end_ms.saturating_sub(cursor);
        byte_milliseconds = byte_milliseconds
            .checked_add(
                u128::from(current)
                    * u128::try_from(duration).map_err(|_| MetricsError::Unavailable)?,
            )
            .ok_or(MetricsError::Unavailable)?;
        bytes_at_end = bytes_at_end
            .checked_add(current)
            .ok_or(MetricsError::Unavailable)?;
    }
    Ok((bytes_at_end, byte_milliseconds))
}

#[derive(Debug)]
struct ReservationRow {
    project_id: String,
    period_start_ms: i64,
    period_end_ms: i64,
    read_units: i64,
    write_units: i64,
    subject_hash: Option<Vec<u8>>,
    mau_claimed: bool,
    status: String,
    event_id: Option<String>,
}

fn reservation_row(
    connection: &Connection,
    reservation_id: &str,
) -> Result<Option<ReservationRow>, MetricsError> {
    connection
        .query_row(
            "SELECT project_id,period_start_ms,period_end_ms,read_units,write_units,subject_hash, \
                    mau_claimed,status,event_id FROM usage_reservations WHERE reservation_id=?1",
            [reservation_id],
            |row| {
                Ok(ReservationRow {
                    project_id: row.get(0)?,
                    period_start_ms: row.get(1)?,
                    period_end_ms: row.get(2)?,
                    read_units: row.get(3)?,
                    write_units: row.get(4)?,
                    subject_hash: row.get(5)?,
                    mau_claimed: row.get(6)?,
                    status: row.get(7)?,
                    event_id: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn expire_reservations(connection: &Connection, now_ms: i64) -> Result<(), MetricsError> {
    let mut statement = connection
        .prepare(
            "SELECT reservation_id,project_id,period_start_ms,period_end_ms,read_units,write_units, \
                    subject_hash,mau_claimed,status,event_id FROM usage_reservations \
             WHERE status='active' AND expires_at_ms<=?1",
        )
        .map_err(map_sqlite)?;
    let rows = statement
        .query_map([now_ms], |row| {
            Ok(ReservationRow {
                project_id: row.get(1)?,
                period_start_ms: row.get(2)?,
                period_end_ms: row.get(3)?,
                read_units: row.get(4)?,
                write_units: row.get(5)?,
                subject_hash: row.get(6)?,
                mau_claimed: row.get(7)?,
                status: row.get(8)?,
                event_id: row.get(9)?,
            })
        })
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    for row in &rows {
        release_mau_claim(connection, row)?;
    }
    connection
        .execute(
            "UPDATE usage_reservations SET status='expired',resolved_at_ms=?1 \
             WHERE status='active' AND expires_at_ms<=?1",
            [now_ms],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn release_mau_claim(
    connection: &Connection,
    reservation: &ReservationRow,
) -> Result<(), MetricsError> {
    if !reservation.mau_claimed {
        return Ok(());
    }
    let Some(subject_hash) = &reservation.subject_hash else {
        return Err(MetricsError::Unavailable);
    };
    connection
        .execute(
            "UPDATE active_user_claims SET reservation_count=reservation_count-1 \
             WHERE period_start_ms=?1 AND subject_hash=?2 AND reservation_count>0",
            params![reservation.period_start_ms, subject_hash],
        )
        .map_err(map_sqlite)?;
    connection
        .execute(
            "DELETE FROM active_user_claims WHERE period_start_ms=?1 AND subject_hash=?2 \
             AND active=0 AND reservation_count=0",
            params![reservation.period_start_ms, subject_hash],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn activate_mau_claim(
    connection: &Connection,
    reservation: &ReservationRow,
    now_ms: i64,
) -> Result<(), MetricsError> {
    if !reservation.mau_claimed {
        return Ok(());
    }
    let Some(subject_hash) = &reservation.subject_hash else {
        return Err(MetricsError::Unavailable);
    };
    let changed = connection
        .execute(
            "UPDATE active_user_claims SET active=1,reservation_count=reservation_count-1, \
             first_seen_ms=COALESCE(first_seen_ms,?3),last_seen_ms=MAX(COALESCE(last_seen_ms,?3),?3) \
             WHERE period_start_ms=?1 AND subject_hash=?2 AND reservation_count>0",
            params![reservation.period_start_ms, subject_hash, now_ms],
        )
        .map_err(map_sqlite)?;
    if changed == 0 {
        connection
            .execute(
                "INSERT INTO active_user_claims \
                 (period_start_ms,period_end_ms,subject_hash,active,reservation_count,first_seen_ms,last_seen_ms) \
                 VALUES (?1,?2,?3,1,0,?4,?4) ON CONFLICT(period_start_ms,subject_hash) DO UPDATE SET \
                   active=1,first_seen_ms=COALESCE(first_seen_ms,excluded.first_seen_ms), \
                   last_seen_ms=MAX(COALESCE(last_seen_ms,excluded.last_seen_ms),excluded.last_seen_ms)",
                params![
                    reservation.period_start_ms,
                    reservation.period_end_ms,
                    subject_hash,
                    now_ms
                ],
            )
            .map_err(map_sqlite)?;
    }
    Ok(())
}

fn validate_event(event: &UsageEvent) -> Result<(), MetricsError> {
    validate_identifier(&event.event_id)?;
    event.period.validate()?;
    if event.occurred_at_ms < event.period.start_ms
        || event.occurred_at_ms >= event.period.end_ms
        || event.recorded_at_ms < event.occurred_at_ms
        || event.recorded_at_ms > event.period.cutoff_ms
        || event.storage_snapshots.len() > 64
        || event
            .storage_snapshots
            .iter()
            .any(|snapshot| validate_resource_id(&snapshot.resource_id).is_err())
    {
        return Err(MetricsError::InvalidInput);
    }
    let mut resources = event
        .storage_snapshots
        .iter()
        .map(|snapshot| snapshot.resource_id.as_str())
        .collect::<Vec<_>>();
    resources.sort_unstable();
    if resources.windows(2).any(|window| window[0] == window[1]) {
        return Err(MetricsError::InvalidInput);
    }
    to_i64(event.read_units)?;
    to_i64(event.write_units)?;
    for snapshot in &event.storage_snapshots {
        to_i64(snapshot.logical_bytes)?;
    }
    Ok(())
}

fn validate_reservation(request: &ReservationRequest) -> Result<(), MetricsError> {
    validate_identifier(&request.reservation_id)?;
    request.period.validate()?;
    if request.created_at_ms < request.period.start_ms
        || request.created_at_ms >= request.period.end_ms
        || request.expires_at_ms <= request.created_at_ms
        || request.expires_at_ms > request.period.cutoff_ms
        || request.expires_at_ms.saturating_sub(request.created_at_ms) > HOUR_MS
    {
        return Err(MetricsError::InvalidInput);
    }
    to_i64(request.read_units)?;
    to_i64(request.write_units)?;
    to_i64(request.storage_growth_bytes)?;
    Ok(())
}

fn validate_report(report: &ReportRequest) -> Result<(), MetricsError> {
    validate_identifier(&report.identifier)?;
    validate_identifier(&report.event_name)?;
    validate_identifier(&report.customer_id)?;
    report.period.validate()?;
    if report.window_start_ms < report.period.start_ms
        || report.window_end_ms <= report.window_start_ms
        || report.window_end_ms > report.period.end_ms
        || report.provider_timestamp_ms < report.window_start_ms
        || report.provider_timestamp_ms > report.window_end_ms
        || report.now_ms < report.provider_timestamp_ms
    {
        return Err(MetricsError::InvalidInput);
    }
    if report.quantity == 0 {
        return Err(MetricsError::InvalidInput);
    }
    to_i64(report.quantity)?;
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), MetricsError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        Err(MetricsError::InvalidInput)
    } else {
        Ok(())
    }
}

fn validate_resource_id(value: &str) -> Result<(), MetricsError> {
    if value.is_empty()
        || value.len() > MAX_RESOURCE_ID_BYTES
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        Err(MetricsError::InvalidInput)
    } else {
        Ok(())
    }
}

fn enforce_limit(
    current: u64,
    requested: u64,
    limit: u64,
    dimension: &'static str,
) -> Result<(), MetricsError> {
    if current
        .checked_add(requested)
        .is_none_or(|total| total > limit)
    {
        Err(MetricsError::LimitExceeded(dimension))
    } else {
        Ok(())
    }
}

fn event_digest(event: &UsageEvent) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.metrics.event.v1\0");
    digest.update(event.event_id.as_bytes());
    digest.update(event.project_id.0.as_bytes());
    digest.update(event.period.start_ms.to_be_bytes());
    digest.update(event.period.end_ms.to_be_bytes());
    digest.update(event.period.cutoff_ms.to_be_bytes());
    digest.update(event.occurred_at_ms.to_be_bytes());
    digest.update(event.recorded_at_ms.to_be_bytes());
    digest.update(event.read_units.to_be_bytes());
    digest.update(event.write_units.to_be_bytes());
    match event.active_subject_hash {
        Some(value) => {
            digest.update([1]);
            digest.update(value);
        }
        None => digest.update([0]),
    }
    let mut snapshots = event.storage_snapshots.clone();
    snapshots.sort();
    for snapshot in snapshots {
        digest.update((snapshot.resource_id.len() as u64).to_be_bytes());
        digest.update(snapshot.resource_id.as_bytes());
        digest.update(snapshot.logical_bytes.to_be_bytes());
    }
    digest.finalize().into()
}

fn reservation_digest(request: &ReservationRequest) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.metrics.reservation.v1\0");
    digest.update(request.reservation_id.as_bytes());
    digest.update(request.project_id.0.as_bytes());
    digest.update(request.period.start_ms.to_be_bytes());
    digest.update(request.period.end_ms.to_be_bytes());
    digest.update(request.period.cutoff_ms.to_be_bytes());
    digest.update(request.created_at_ms.to_be_bytes());
    digest.update(request.expires_at_ms.to_be_bytes());
    digest.update(request.read_units.to_be_bytes());
    digest.update(request.write_units.to_be_bytes());
    digest.update(request.storage_growth_bytes.to_be_bytes());
    if let Some(value) = request.active_subject_hash {
        digest.update(value);
    }
    digest.finalize().into()
}

fn report_digest(report: &ReportRequest) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.metrics.stripe-report.v1\0");
    digest.update(report.identifier.as_bytes());
    digest.update(report.event_name.as_bytes());
    digest.update(report.customer_id.as_bytes());
    digest.update(report.period.start_ms.to_be_bytes());
    digest.update(report.period.end_ms.to_be_bytes());
    digest.update(report.dimension.as_str().as_bytes());
    digest.update(report.window_start_ms.to_be_bytes());
    digest.update(report.window_end_ms.to_be_bytes());
    digest.update(report.quantity.to_be_bytes());
    digest.update(report.provider_timestamp_ms.to_be_bytes());
    digest.finalize().into()
}

fn parse_reservation_status(value: &str) -> Result<ReservationStatus, MetricsError> {
    match value {
        "active" => Ok(ReservationStatus::Active),
        "settled" => Ok(ReservationStatus::Settled),
        "released" => Ok(ReservationStatus::Released),
        "expired" => Ok(ReservationStatus::Expired),
        _ => Err(MetricsError::Unavailable),
    }
}

fn parse_dimension(value: &str) -> Result<UsageDimension, MetricsError> {
    match value {
        "read_units" => Ok(UsageDimension::ReadUnits),
        "write_units" => Ok(UsageDimension::WriteUnits),
        "monthly_active_users" => Ok(UsageDimension::MonthlyActiveUsers),
        "storage_byte_milliseconds" => Ok(UsageDimension::StorageByteMilliseconds),
        _ => Err(MetricsError::Unavailable),
    }
}

fn to_i64(value: u64) -> Result<i64, MetricsError> {
    i64::try_from(value).map_err(|_| MetricsError::InvalidInput)
}

fn as_u64(value: i64) -> Result<u64, MetricsError> {
    u64::try_from(value).map_err(|_| MetricsError::Unavailable)
}

fn map_sqlite(_: rusqlite::Error) -> MetricsError {
    MetricsError::Unavailable
}

fn hash_file(path: &Path) -> Result<(u64, [u8; 32]), MetricsError> {
    let mut file = File::open(path).map_err(|_| MetricsError::Unavailable)?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| MetricsError::Unavailable)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| MetricsError::Unavailable)?)
            .ok_or(MetricsError::Unavailable)?;
        digest.update(&buffer[..read]);
    }
    Ok((total, digest.finalize().into()))
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), MetricsError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| MetricsError::Unavailable)
}

#[cfg(not(unix))]
fn set_private_directory(_: &Path) -> Result<(), MetricsError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), MetricsError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| MetricsError::Unavailable)
}

#[cfg(not(unix))]
fn set_private_file(_: &Path) -> Result<(), MetricsError> {
    Ok(())
}

#[cfg(test)]
mod tests;
