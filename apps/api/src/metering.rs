use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{Datelike as _, TimeZone as _, Utc};
use ffdb_org_metrics::{
    BillingPeriod, IngestOutcome, MetricsConfig, MetricsError, OrganizationMetricsStore,
    ReservationAdmission, ReservationRequest, StorageSnapshot, UsageEvent, UsageLimits,
};
use ffdb_protocol::{
    ExecutionMode, OrganizationId, PlatformUsageSummary, ProjectId, UsageReceipt,
    UsageReportingStatus, UserId, WorkerOperation,
};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

const RESERVATION_TTL_MS: i64 = 60_000;
const PERIOD_CUTOFF_GRACE_MS: i64 = 2 * 24 * 60 * 60 * 1_000;
const MAX_CACHED_STORES: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UsagePlan {
    pub read_units: u64,
    pub write_units: u64,
}

impl UsagePlan {
    pub(crate) fn for_operation(
        operation: &WorkerOperation,
    ) -> Result<Option<Self>, MeteringError> {
        let plan = match operation {
            WorkerOperation::Query(request) => {
                let class = ffdb_sql_parser::classify_statement(&request.sql)
                    .map_err(|_| MeteringError::InvalidOperation)?;
                Self {
                    read_units: u64::from(class.read_only),
                    write_units: u64::from(!class.read_only),
                }
            }
            WorkerOperation::Transaction(request) => {
                let mut reads = 0_u64;
                let mut writes = 0_u64;
                for statement in &request.statements {
                    let class = ffdb_sql_parser::classify_statement(&statement.sql)
                        .map_err(|_| MeteringError::InvalidOperation)?;
                    reads = reads.saturating_add(u64::from(class.read_only));
                    writes = writes.saturating_add(u64::from(!class.read_only));
                }
                Self {
                    read_units: reads,
                    write_units: writes,
                }
            }
            WorkerOperation::Snapshot(_) | WorkerOperation::SyncPull(_) => Self {
                read_units: 1,
                write_units: 0,
            },
            WorkerOperation::SyncPush(request) => Self {
                read_units: 0,
                write_units: request.mutations.len() as u64,
            },
            _ => return Ok(None),
        };
        Ok(Some(plan))
    }

    pub(crate) const fn mutating(self) -> bool {
        self.write_units > 0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedUsage {
    store: Arc<OrganizationMetricsStore>,
    period: BillingPeriod,
    project_id: ProjectId,
    reservation_id: Option<String>,
    subject_hash: Option<[u8; 32]>,
    pub max_database_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedRegistration(PreparedUsage);

#[derive(Debug)]
pub struct UsageMeteringService {
    pool: PgPool,
    config: MetricsConfig,
    subject_key: [u8; 32],
    stores: Mutex<HashMap<OrganizationId, Arc<OrganizationMetricsStore>>>,
}

impl UsageMeteringService {
    pub fn new(pool: PgPool, root: PathBuf, subject_key: [u8; 32]) -> Self {
        Self {
            pool,
            config: MetricsConfig::new(root),
            subject_key,
            stores: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn prepare(
        &self,
        project_id: ProjectId,
        receipt_id: Uuid,
        mode: &ExecutionMode,
        plan: UsagePlan,
        now_ms: i64,
        configured_max_database_bytes: u64,
    ) -> Result<PreparedUsage, MeteringError> {
        let context = self.billing_context(project_id, now_ms).await?;
        if context.reporting_blocked && plan.mutating() {
            return Err(MeteringError::ReportingBlocked);
        }
        let store = self.store(context.organization_id)?;
        let subject_hash = match mode {
            ExecutionMode::EndUser(auth) => Some(
                store
                    .hash_active_subject(context.period, &auth.subject.to_string())
                    .map_err(MeteringError::Store)?,
            ),
            ExecutionMode::Developer(_) => None,
        };
        let resource_id = database_resource(project_id);
        let mut max_database_bytes = configured_max_database_bytes;
        let reservation_id = if plan.mutating() {
            let summary = store
                .summary(context.period)
                .map_err(MeteringError::Store)?;
            let current_project_bytes = store
                .storage_resource_bytes(&resource_id)
                .map_err(MeteringError::Store)?;
            let storage_growth_bytes = if context.enforce_storage {
                let remaining = context
                    .storage_bytes
                    .saturating_sub(summary.storage_bytes_at_end);
                max_database_bytes = max_database_bytes.min(
                    current_project_bytes
                        .checked_add(remaining)
                        .ok_or(MeteringError::Unavailable)?,
                );
                remaining
            } else {
                0
            };
            let reservation_id = receipt_id.to_string();
            let request = ReservationRequest {
                reservation_id: reservation_id.clone(),
                project_id,
                period: context.period,
                created_at_ms: now_ms,
                expires_at_ms: now_ms.saturating_add(RESERVATION_TTL_MS),
                read_units: plan.read_units,
                write_units: plan.write_units,
                storage_growth_bytes,
                active_subject_hash: subject_hash,
            };
            let admission = store
                .reserve(
                    &request,
                    UsageLimits {
                        read_units: context.monthly_reads,
                        write_units: context.monthly_writes,
                        monthly_active_users: context.monthly_active_users,
                        storage_bytes: context.storage_bytes,
                        // Free reads continue after the included quantity. Paid
                        // tiers meter overage instead of blocking reads/writes.
                        enforce_read_limit: false,
                        enforce_write_limit: context.enforce_write,
                        enforce_mau_limit: false,
                        enforce_storage_limit: context.enforce_storage,
                    },
                )
                .map_err(MeteringError::Store)?;
            if !matches!(
                admission,
                ReservationAdmission::Reserved | ReservationAdmission::Duplicate(_)
            ) {
                return Err(MeteringError::Unavailable);
            }
            Some(reservation_id)
        } else {
            None
        };
        Ok(PreparedUsage {
            store,
            period: context.period,
            project_id,
            reservation_id,
            subject_hash,
            max_database_bytes,
        })
    }

    pub(crate) async fn prepare_registration(
        &self,
        project_id: ProjectId,
        reservation_id: Uuid,
        normalized_subject: &str,
        now_ms: i64,
    ) -> Result<PreparedRegistration, MeteringError> {
        let context = self.billing_context(project_id, now_ms).await?;
        let store = self.store(context.organization_id)?;
        let subject_hash = store
            .hash_active_subject(context.period, normalized_subject)
            .map_err(MeteringError::Store)?;
        let reservation_id = reservation_id.to_string();
        store
            .reserve(
                &ReservationRequest {
                    reservation_id: reservation_id.clone(),
                    project_id,
                    period: context.period,
                    created_at_ms: now_ms,
                    expires_at_ms: now_ms.saturating_add(RESERVATION_TTL_MS),
                    read_units: 0,
                    write_units: 0,
                    storage_growth_bytes: 0,
                    active_subject_hash: Some(subject_hash),
                },
                UsageLimits {
                    read_units: context.monthly_reads,
                    write_units: context.monthly_writes,
                    monthly_active_users: context.monthly_active_users,
                    storage_bytes: context.storage_bytes,
                    enforce_read_limit: false,
                    enforce_write_limit: false,
                    enforce_mau_limit: context.enforce_write,
                    enforce_storage_limit: false,
                },
            )
            .map_err(MeteringError::Store)?;
        Ok(PreparedRegistration(PreparedUsage {
            store,
            period: context.period,
            project_id,
            reservation_id: Some(reservation_id),
            subject_hash: Some(subject_hash),
            max_database_bytes: 0,
        }))
    }

    pub(crate) fn settle_registration(
        &self,
        prepared: &PreparedRegistration,
        now_ms: i64,
    ) -> Result<IngestOutcome, MeteringError> {
        let usage = &prepared.0;
        let reservation_id = usage
            .reservation_id
            .as_deref()
            .ok_or(MeteringError::Unavailable)?;
        usage
            .store
            .settle_reservation(
                reservation_id,
                &UsageEvent {
                    event_id: reservation_id.to_owned(),
                    project_id: usage.project_id,
                    period: usage.period,
                    occurred_at_ms: now_ms
                        .clamp(usage.period.start_ms, usage.period.end_ms.saturating_sub(1)),
                    recorded_at_ms: now_ms,
                    read_units: 0,
                    write_units: 0,
                    active_subject_hash: usage.subject_hash,
                    storage_snapshots: Vec::new(),
                },
            )
            .map_err(MeteringError::Store)
    }

    pub(crate) fn release_registration(&self, prepared: &PreparedRegistration, now_ms: i64) {
        self.release(&prepared.0, now_ms);
    }

    pub(crate) fn ingest(
        &self,
        prepared: &PreparedUsage,
        receipt: &UsageReceipt,
        now_ms: i64,
    ) -> Result<IngestOutcome, MeteringError> {
        let occurred_at_ms = receipt.recorded_at_ms.clamp(
            prepared.period.start_ms,
            prepared.period.end_ms.saturating_sub(1),
        );
        let event = UsageEvent {
            event_id: receipt.receipt_id.to_string(),
            project_id: prepared.project_id,
            period: prepared.period,
            occurred_at_ms,
            recorded_at_ms: now_ms,
            read_units: receipt.reads,
            write_units: receipt.writes,
            active_subject_hash: prepared.subject_hash,
            storage_snapshots: vec![StorageSnapshot {
                resource_id: database_resource(prepared.project_id),
                logical_bytes: receipt.logical_database_bytes,
            }],
        };
        match &prepared.reservation_id {
            Some(reservation_id) => prepared
                .store
                .settle_reservation(reservation_id, &event)
                .map_err(MeteringError::Store),
            None => prepared
                .store
                .record_event(&event)
                .map_err(MeteringError::Store),
        }
    }

    pub(crate) fn release(&self, prepared: &PreparedUsage, now_ms: i64) {
        if let Some(reservation_id) = &prepared.reservation_id
            && prepared
                .store
                .release_reservation(reservation_id, now_ms)
                .is_err()
        {
            tracing::error!(reservation_id, "failed to release usage reservation");
        }
    }

    pub(crate) async fn organization_summary(
        &self,
        organization_id: OrganizationId,
        now_ms: i64,
    ) -> Result<PlatformUsageSummary, MeteringError> {
        let account = sqlx::query(
            "SELECT tier,status,usage_reporting_status, \
                    (extract(epoch FROM current_period_start)*1000)::bigint period_start_ms, \
                    (extract(epoch FROM current_period_end)*1000)::bigint period_end_ms, \
                    (extract(epoch FROM usage_reporting_last_success_at)*1000)::bigint reporting_last_success_ms \
             FROM organization_billing_accounts WHERE organization_id=$1",
        )
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| MeteringError::Unavailable)?;
        let (period, reporting_status, reporting_last_success_ms) = match account {
            Some(row) => {
                let status: String = row
                    .try_get("status")
                    .map_err(|_| MeteringError::Unavailable)?;
                let entitled = matches!(status.as_str(), "active" | "trialing");
                let period_start_ms: Option<i64> = row
                    .try_get("period_start_ms")
                    .map_err(|_| MeteringError::Unavailable)?;
                let period_end_ms: Option<i64> = row
                    .try_get("period_end_ms")
                    .map_err(|_| MeteringError::Unavailable)?;
                let period = if entitled {
                    match (period_start_ms, period_end_ms) {
                        (Some(start_ms), Some(end_ms)) if start_ms <= now_ms && now_ms < end_ms => {
                            BillingPeriod {
                                start_ms,
                                end_ms,
                                cutoff_ms: end_ms.saturating_add(PERIOD_CUTOFF_GRACE_MS),
                            }
                        }
                        _ => calendar_month(now_ms)?,
                    }
                } else {
                    calendar_month(now_ms)?
                };
                let raw_reporting_status: String = row
                    .try_get("usage_reporting_status")
                    .map_err(|_| MeteringError::Unavailable)?;
                let reporting_status = match raw_reporting_status.as_str() {
                    "healthy" => UsageReportingStatus::Healthy,
                    "degraded" => UsageReportingStatus::Degraded,
                    "reconciling" => UsageReportingStatus::Reconciling,
                    "blocked" => UsageReportingStatus::Blocked,
                    _ => return Err(MeteringError::Unavailable),
                };
                let reporting_last_success_ms = row
                    .try_get("reporting_last_success_ms")
                    .map_err(|_| MeteringError::Unavailable)?;
                (period, reporting_status, reporting_last_success_ms)
            }
            None => (calendar_month(now_ms)?, UsageReportingStatus::Healthy, None),
        };
        let summary = self
            .store(organization_id)?
            .summary(period)
            .map_err(MeteringError::Store)?;
        let storage_byte_hours = summary.storage_byte_milliseconds / 3_600_000;
        Ok(PlatformUsageSummary {
            organization_id,
            period_start_ms: period.start_ms,
            period_end_ms: period.end_ms,
            reads: summary.read_units,
            writes: summary.write_units,
            storage_bytes: summary.storage_bytes_at_end,
            storage_byte_hours: u64::try_from(storage_byte_hours).unwrap_or(u64::MAX),
            monthly_active_users: summary.monthly_active_users,
            reporting_status,
            reporting_last_success_ms,
            as_of_ms: now_ms,
        })
    }

    pub(crate) async fn record_object_storage(
        &self,
        project_id: ProjectId,
        event_id: Uuid,
        subject: UserId,
        current_bytes: u64,
        now_ms: i64,
    ) -> Result<IngestOutcome, MeteringError> {
        let context = self.billing_context(project_id, now_ms).await?;
        let store = self.store(context.organization_id)?;
        let subject_hash = store
            .hash_active_subject(context.period, &subject.to_string())
            .map_err(MeteringError::Store)?;
        store
            .record_event(&UsageEvent {
                event_id: format!("storage:{event_id}"),
                project_id,
                period: context.period,
                occurred_at_ms: now_ms.clamp(
                    context.period.start_ms,
                    context.period.end_ms.saturating_sub(1),
                ),
                recorded_at_ms: now_ms,
                read_units: 0,
                write_units: 0,
                active_subject_hash: Some(subject_hash),
                storage_snapshots: vec![StorageSnapshot {
                    resource_id: object_storage_resource(project_id),
                    logical_bytes: current_bytes,
                }],
            })
            .map_err(MeteringError::Store)
    }

    pub(crate) async fn reserve_object_storage(
        &self,
        project_id: ProjectId,
        nonce: &str,
        subject: UserId,
        storage_growth_bytes: u64,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<(), MeteringError> {
        let context = self.billing_context(project_id, created_at_ms).await?;
        if context.reporting_blocked {
            return Err(MeteringError::ReportingBlocked);
        }
        let store = self.store(context.organization_id)?;
        let subject_hash = store
            .hash_active_subject(context.period, &subject.to_string())
            .map_err(MeteringError::Store)?;
        let request = ReservationRequest {
            reservation_id: storage_reservation_id(nonce),
            project_id,
            period: context.period,
            created_at_ms,
            expires_at_ms,
            read_units: 0,
            write_units: 0,
            storage_growth_bytes,
            active_subject_hash: Some(subject_hash),
        };
        store
            .reserve(
                &request,
                UsageLimits {
                    read_units: context.monthly_reads,
                    write_units: context.monthly_writes,
                    monthly_active_users: context.monthly_active_users,
                    storage_bytes: context.storage_bytes,
                    enforce_read_limit: false,
                    enforce_write_limit: false,
                    enforce_mau_limit: false,
                    enforce_storage_limit: context.enforce_storage,
                },
            )
            .map(|_| ())
            .map_err(MeteringError::Store)
    }

    pub(crate) async fn settle_object_storage(
        &self,
        project_id: ProjectId,
        nonce: &str,
        subject: UserId,
        current_bytes: u64,
        now_ms: i64,
    ) -> Result<IngestOutcome, MeteringError> {
        let organization_id = self.organization_for_project(project_id).await?;
        let store = self.store(organization_id)?;
        let reservation_id = storage_reservation_id(nonce);
        let reservation = store
            .reservation_context(&reservation_id)
            .map_err(MeteringError::Store)?
            .ok_or(MeteringError::Store(MetricsError::ReservationNotFound))?;
        if reservation.project_id != project_id {
            return Err(MeteringError::Store(MetricsError::ReservationMismatch));
        }
        let period = BillingPeriod {
            start_ms: reservation.period_start_ms,
            end_ms: reservation.period_end_ms,
            cutoff_ms: reservation
                .period_end_ms
                .saturating_add(PERIOD_CUTOFF_GRACE_MS),
        };
        let subject_hash = store
            .hash_active_subject(period, &subject.to_string())
            .map_err(MeteringError::Store)?;
        store
            .settle_reservation(
                &reservation_id,
                &UsageEvent {
                    event_id: reservation_id.clone(),
                    project_id,
                    period,
                    occurred_at_ms: now_ms.clamp(period.start_ms, period.end_ms.saturating_sub(1)),
                    recorded_at_ms: now_ms,
                    read_units: 0,
                    write_units: 0,
                    active_subject_hash: Some(subject_hash),
                    storage_snapshots: vec![StorageSnapshot {
                        resource_id: object_storage_resource(project_id),
                        logical_bytes: current_bytes,
                    }],
                },
            )
            .map_err(MeteringError::Store)
    }

    pub(crate) async fn release_object_storage(
        &self,
        project_id: ProjectId,
        nonce: &str,
        now_ms: i64,
    ) -> Result<(), MeteringError> {
        let organization_id = self.organization_for_project(project_id).await?;
        self.store(organization_id)?
            .release_reservation(&storage_reservation_id(nonce), now_ms)
            .map(|_| ())
            .map_err(MeteringError::Store)
    }

    async fn organization_for_project(
        &self,
        project_id: ProjectId,
    ) -> Result<OrganizationId, MeteringError> {
        let value = sqlx::query_scalar::<_, Uuid>(
            "SELECT organization_id FROM projects WHERE id=$1 AND lifecycle_state <> 'deleted'",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| MeteringError::Unavailable)?
        .ok_or(MeteringError::Unavailable)?;
        Ok(OrganizationId(value))
    }

    fn store(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Arc<OrganizationMetricsStore>, MeteringError> {
        let mut stores = self.stores.lock().map_err(|_| MeteringError::Unavailable)?;
        if let Some(store) = stores.get(&organization_id) {
            return Ok(store.clone());
        }
        if stores.len() >= MAX_CACHED_STORES {
            stores.clear();
        }
        let store = Arc::new(
            OrganizationMetricsStore::open(&self.config, organization_id, self.subject_key)
                .map_err(MeteringError::Store)?,
        );
        stores.insert(organization_id, store.clone());
        Ok(store)
    }

    async fn billing_context(
        &self,
        project_id: ProjectId,
        now_ms: i64,
    ) -> Result<BillingContext, MeteringError> {
        let row = sqlx::query(
            "SELECT p.organization_id,a.tier,a.status, \
                    (extract(epoch FROM a.current_period_start)*1000)::bigint period_start_ms, \
                    (extract(epoch FROM a.current_period_end)*1000)::bigint period_end_ms, \
                    a.usage_reporting_hard_cutoff_at IS NOT NULL reporting_blocked, \
                    COALESCE(s.billing_enforcement_enabled,false) billing_enforced, \
                    c.storage_bytes,c.monthly_reads,c.monthly_writes,c.monthly_active_users, \
                    EXISTS(SELECT 1 FROM organization_billing_exemptions e \
                           WHERE e.organization_id=p.organization_id) billing_exempt \
             FROM projects p LEFT JOIN organization_billing_accounts a ON a.organization_id=p.organization_id \
             LEFT JOIN instance_settings s ON s.singleton=true \
             LEFT JOIN billing_price_catalog c \
                    ON c.tier=CASE WHEN a.status IN ('active','trialing') \
                                        THEN COALESCE(a.tier,'free') ELSE 'free' END \
                   AND c.active=true \
             WHERE p.id=$1 AND p.lifecycle_state <> 'deleted'",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| MeteringError::Unavailable)?
        .ok_or(MeteringError::Unavailable)?;
        let organization_id = OrganizationId(
            row.try_get("organization_id")
                .map_err(|_| MeteringError::Unavailable)?,
        );
        let tier: Option<String> = row
            .try_get("tier")
            .map_err(|_| MeteringError::Unavailable)?;
        let status: Option<String> = row
            .try_get("status")
            .map_err(|_| MeteringError::Unavailable)?;
        let entitled = matches!(status.as_deref(), Some("active" | "trialing"));
        let effective_tier = if entitled {
            tier.as_deref().unwrap_or("free")
        } else {
            "free"
        };
        let period_start_ms: Option<i64> = row
            .try_get("period_start_ms")
            .map_err(|_| MeteringError::Unavailable)?;
        let period_end_ms: Option<i64> = row
            .try_get("period_end_ms")
            .map_err(|_| MeteringError::Unavailable)?;
        let period = if entitled {
            match (period_start_ms, period_end_ms) {
                (Some(start_ms), Some(end_ms)) if start_ms <= now_ms && now_ms < end_ms => {
                    BillingPeriod {
                        start_ms,
                        end_ms,
                        cutoff_ms: end_ms.saturating_add(PERIOD_CUTOFF_GRACE_MS),
                    }
                }
                _ => calendar_month(now_ms)?,
            }
        } else {
            calendar_month(now_ms)?
        };
        let billing_enforced: bool = row
            .try_get("billing_enforced")
            .map_err(|_| MeteringError::Unavailable)?;
        let billing_exempt: bool = row
            .try_get("billing_exempt")
            .map_err(|_| MeteringError::Unavailable)?;
        let enforce_allowances = billing_enforced && !billing_exempt;
        Ok(BillingContext {
            organization_id,
            period,
            storage_bytes: positive(&row, "storage_bytes")?,
            monthly_reads: positive(&row, "monthly_reads")?,
            monthly_writes: positive(&row, "monthly_writes")?,
            monthly_active_users: positive(&row, "monthly_active_users")?,
            enforce_write: enforce_allowances && effective_tier == "free",
            enforce_storage: enforce_allowances && effective_tier == "free",
            reporting_blocked: row
                .try_get("reporting_blocked")
                .map_err(|_| MeteringError::Unavailable)?
                && enforce_allowances,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct BillingContext {
    organization_id: OrganizationId,
    period: BillingPeriod,
    storage_bytes: u64,
    monthly_reads: u64,
    monthly_writes: u64,
    monthly_active_users: u64,
    enforce_write: bool,
    enforce_storage: bool,
    reporting_blocked: bool,
}

#[derive(Debug)]
pub(crate) enum MeteringError {
    InvalidOperation,
    ReportingBlocked,
    Store(MetricsError),
    Unavailable,
}

fn positive(row: &sqlx::postgres::PgRow, name: &str) -> Result<u64, MeteringError> {
    let value: i64 = row.try_get(name).map_err(|_| MeteringError::Unavailable)?;
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(MeteringError::Unavailable)
}

fn calendar_month(now_ms: i64) -> Result<BillingPeriod, MeteringError> {
    let now = Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .ok_or(MeteringError::Unavailable)?;
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .ok_or(MeteringError::Unavailable)?;
    let (next_year, next_month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    let end = Utc
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()
        .ok_or(MeteringError::Unavailable)?;
    let start_ms = start.timestamp_millis();
    let end_ms = end.timestamp_millis();
    Ok(BillingPeriod {
        start_ms,
        end_ms,
        cutoff_ms: end_ms.saturating_add(PERIOD_CUTOFF_GRACE_MS),
    })
}

fn database_resource(project_id: ProjectId) -> String {
    format!("database:{}", project_id.0)
}

fn object_storage_resource(project_id: ProjectId) -> String {
    format!("objects:{}", project_id.0)
}

fn storage_reservation_id(nonce: &str) -> String {
    let digest = Sha256::digest(nonce.as_bytes());
    format!("storage:{}", hex::encode(digest))
}
