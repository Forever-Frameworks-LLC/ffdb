use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_database_router::ProcessWorkerExecutor;
use ffdb_protocol::{
    ObservabilityHttpTotals, ObservabilityQueryMetric, ObservabilityRouteMetric,
    ObservabilityRuntimeSnapshot, ObservabilityStorageSnapshot, ObservabilitySummary,
    ObservabilityTimePoint, ProjectId, RequestId, WorkerOperation, WorkerStatementTelemetry,
};
use ffdb_sql_parser::{StatementKind, classify_statement};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row as _};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::instance::InstanceServiceError;
use super::management::{authenticated, authorized_project_member};
use super::{ApiError, ApiState};

const RETENTION_DAYS: u16 = 30;
const EVENT_BUFFER: usize = 8_192;
const MAX_PENDING_KEYS: usize = 50_000;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const LATENCY_BOUNDS_MS: [f64; 12] = [
    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1_000.0, 2_500.0, 5_000.0, 15_000.0, 60_000.0,
];

#[derive(Clone)]
pub struct ObservabilityService {
    pool: PgPool,
    sender: mpsc::Sender<Event>,
    dropped: Arc<AtomicU64>,
    executor: Arc<ProcessWorkerExecutor>,
    database_root: PathBuf,
    backup_root: PathBuf,
}

impl std::fmt::Debug for ObservabilityService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservabilityService")
            .field("retention_days", &RETENTION_DAYS)
            .field("dropped", &self.dropped.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct ObservabilityWorkerHandle {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    retention_task: tokio::task::JoinHandle<()>,
}

impl ObservabilityWorkerHandle {
    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ignored = shutdown.send(());
        }
        let _ignored = self.task.await;
        self.retention_task.abort();
        let _ignored = self.retention_task.await;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct QueryProfile {
    fingerprint: String,
    shape: String,
    statement_kind: String,
    read_only: bool,
}

pub(crate) struct ExecutionObservation<'a> {
    pub project_id: ProjectId,
    pub profiles: &'a [QueryProfile],
    pub telemetry: &'a [WorkerStatementTelemetry],
    pub fallback_duration: Duration,
    pub failed: bool,
    pub logical_database_bytes: Option<u64>,
    pub now_ms: i64,
}

#[derive(Debug)]
enum Event {
    Http(HttpEvent),
    Query(QueryEvent),
    Storage(StorageEvent),
}

#[derive(Debug)]
struct HttpEvent {
    bucket_start_ms: i64,
    project_id: Uuid,
    method: String,
    route: String,
    status_class: i16,
    duration_ms: f64,
}

#[derive(Debug)]
struct QueryEvent {
    bucket_start_ms: i64,
    project_id: Uuid,
    profile: QueryProfile,
    duration_ms: f64,
    failed: bool,
    rows_returned: u64,
    rows_affected: u64,
}

#[derive(Debug)]
struct StorageEvent {
    project_id: Uuid,
    logical_database_bytes: u64,
    sampled_at_ms: i64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HttpKey {
    bucket_start_ms: i64,
    project_id: Uuid,
    method: String,
    route: String,
    status_class: i16,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct QueryKey {
    bucket_start_ms: i64,
    project_id: Uuid,
    fingerprint: String,
}

#[derive(Clone, Debug, Default)]
struct LatencyAggregate {
    count: u64,
    sum_ms: f64,
    max_ms: f64,
    buckets: [u64; 12],
}

impl LatencyAggregate {
    fn observe(&mut self, duration_ms: f64) {
        let duration_ms = if duration_ms.is_finite() && duration_ms >= 0.0 {
            duration_ms.min(60_000.0)
        } else {
            60_000.0
        };
        self.count = self.count.saturating_add(1);
        self.sum_ms += duration_ms;
        self.max_ms = self.max_ms.max(duration_ms);
        for (index, bound) in LATENCY_BOUNDS_MS.iter().enumerate() {
            if duration_ms <= *bound {
                self.buckets[index] = self.buckets[index].saturating_add(1);
            }
        }
    }

    fn merge(&mut self, other: &Self) {
        self.count = self.count.saturating_add(other.count);
        self.sum_ms += other.sum_ms;
        self.max_ms = self.max_ms.max(other.max_ms);
        for (current, incoming) in self.buckets.iter_mut().zip(other.buckets) {
            *current = current.saturating_add(incoming);
        }
    }
}

#[derive(Clone, Debug)]
struct QueryAggregate {
    profile: QueryProfile,
    latency: LatencyAggregate,
    errors: u64,
    rows_returned: u64,
    rows_affected: u64,
}

#[derive(Default)]
struct Pending {
    http: HashMap<HttpKey, LatencyAggregate>,
    queries: HashMap<QueryKey, QueryAggregate>,
    storage: HashMap<Uuid, StorageEvent>,
}

impl Pending {
    fn key_count(&self) -> usize {
        self.http.len() + self.queries.len() + self.storage.len()
    }
}

impl ObservabilityService {
    pub fn spawn(
        pool: PgPool,
        executor: Arc<ProcessWorkerExecutor>,
        database_root: PathBuf,
        backup_root: PathBuf,
    ) -> (Arc<Self>, ObservabilityWorkerHandle) {
        let (sender, receiver) = mpsc::channel(EVENT_BUFFER);
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let service = Arc::new(Self {
            pool: pool.clone(),
            sender,
            dropped: Arc::clone(&dropped),
            executor,
            database_root,
            backup_root,
        });
        let retention_task = tokio::spawn(run_retention(pool.clone()));
        let task = tokio::spawn(run_recorder(pool, receiver, shutdown_receiver, dropped));
        (
            service,
            ObservabilityWorkerHandle {
                shutdown: Some(shutdown_sender),
                task,
                retention_task,
            },
        )
    }

    pub fn record_http(
        &self,
        project_id: Option<ProjectId>,
        method: &str,
        route: &str,
        status: u16,
        duration: Duration,
        now_ms: i64,
    ) {
        let status_class = i16::try_from(status / 100).unwrap_or(5).clamp(1, 5);
        self.send(Event::Http(HttpEvent {
            bucket_start_ms: minute_bucket(now_ms),
            project_id: project_id.map_or_else(Uuid::nil, |value| value.0),
            method: method.chars().take(12).collect(),
            route: route.chars().take(256).collect(),
            status_class,
            duration_ms: duration.as_secs_f64() * 1_000.0,
        }));
    }

    pub(crate) fn record_execution(&self, observation: ExecutionObservation<'_>) {
        if observation.failed {
            let divisor = observation.profiles.len().max(1) as f64;
            let duration_ms = observation.fallback_duration.as_secs_f64() * 1_000.0 / divisor;
            for profile in observation.profiles {
                self.record_query(QueryEvent {
                    bucket_start_ms: minute_bucket(observation.now_ms),
                    project_id: observation.project_id.0,
                    profile: profile.clone(),
                    duration_ms,
                    failed: true,
                    rows_returned: 0,
                    rows_affected: 0,
                });
            }
        } else if observation.profiles.len() == observation.telemetry.len() {
            for (profile, sample) in observation
                .profiles
                .iter()
                .cloned()
                .zip(observation.telemetry)
            {
                self.record_query(QueryEvent {
                    bucket_start_ms: minute_bucket(observation.now_ms),
                    project_id: observation.project_id.0,
                    profile,
                    duration_ms: sample.duration_ms,
                    failed: false,
                    rows_returned: sample.rows_returned,
                    rows_affected: sample.rows_affected,
                });
            }
        }
        if let Some(logical_database_bytes) = observation.logical_database_bytes {
            self.send(Event::Storage(StorageEvent {
                project_id: observation.project_id.0,
                logical_database_bytes,
                sampled_at_ms: observation.now_ms,
            }));
        }
    }

    fn record_query(&self, event: QueryEvent) {
        self.send(Event::Query(event));
    }

    fn send(&self, event: Event) {
        if self.sender.try_send(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    async fn summary(
        &self,
        project_id: Option<ProjectId>,
        range: RangeSpec,
        current_inflight: u64,
    ) -> Result<ObservabilitySummary, sqlx::Error> {
        let now = super::now_ms();
        let window_end_ms = now;
        let window_start_ms = now.saturating_sub(range.window_ms);
        let filter_project = project_id.is_some();
        let filter_id = project_id.map_or_else(Uuid::nil, |value| value.0);
        let rows = sqlx::query(HTTP_SERIES_SQL)
            .bind(window_start_ms)
            .bind(window_end_ms)
            .bind(filter_project)
            .bind(filter_id)
            .bind(range.resolution_ms)
            .fetch_all(&self.pool)
            .await?;
        let mut series_by_bucket = HashMap::new();
        for row in rows {
            let bucket = row.get::<i64, _>("bucket_start_ms");
            series_by_bucket.insert(bucket, stats_from_row(&row));
        }
        let mut series = Vec::new();
        let first_bucket = window_start_ms.div_euclid(range.resolution_ms) * range.resolution_ms;
        let last_bucket = window_end_ms.div_euclid(range.resolution_ms) * range.resolution_ms;
        let mut bucket = first_bucket;
        while bucket <= last_bucket {
            let stats = series_by_bucket.remove(&bucket).unwrap_or_default();
            series.push(time_point(bucket, &stats, range.resolution_ms));
            bucket = bucket.saturating_add(range.resolution_ms);
        }

        let total_row = sqlx::query(HTTP_TOTAL_SQL)
            .bind(window_start_ms)
            .bind(window_end_ms)
            .bind(filter_project)
            .bind(filter_id)
            .fetch_one(&self.pool)
            .await?;
        let total_stats = stats_from_row(&total_row);
        let totals = http_totals(&total_stats, range.window_ms);

        let route_rows = sqlx::query(HTTP_ROUTES_SQL)
            .bind(window_start_ms)
            .bind(window_end_ms)
            .bind(filter_project)
            .bind(filter_id)
            .fetch_all(&self.pool)
            .await?;
        let mut routes = route_rows
            .iter()
            .map(|row| route_metric(row, range.window_ms))
            .collect::<Vec<_>>();
        routes.sort_by_key(|route| std::cmp::Reverse(route.requests));
        let busiest_routes = routes.iter().take(20).cloned().collect();
        routes.sort_by(|left, right| {
            right
                .p95_latency_ms
                .partial_cmp(&left.p95_latency_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let slowest_routes = routes.into_iter().take(20).collect();

        let query_rows = sqlx::query(QUERY_METRICS_SQL)
            .bind(window_start_ms)
            .bind(window_end_ms)
            .bind(filter_project)
            .bind(filter_id)
            .fetch_all(&self.pool)
            .await?;
        let mut queries = query_rows.iter().map(query_metric).collect::<Vec<_>>();
        queries.sort_by_key(|query| std::cmp::Reverse(query.executions));
        let frequent_queries = queries.iter().take(20).cloned().collect();
        queries.sort_by(|left, right| {
            right
                .p95_latency_ms
                .partial_cmp(&left.p95_latency_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let slow_queries = queries.into_iter().take(20).collect();

        let storage_row = sqlx::query(STORAGE_SQL)
            .bind(filter_project)
            .bind(filter_id)
            .fetch_one(&self.pool)
            .await?;
        let storage = storage_snapshot(&storage_row, &self.database_root, &self.backup_root);
        let worker = self.executor.snapshot().await;
        let runtime = ObservabilityRuntimeSnapshot {
            healthy: true,
            active_workers: saturating_u32(worker.active_workers),
            max_workers: saturating_u32(worker.max_workers),
            worker_saturation: ratio(worker.active_workers as u64, worker.max_workers as u64),
            execution_slots_in_use: saturating_u32(worker.execution_slots_in_use),
            queue_capacity: saturating_u32(worker.queue_capacity),
            queue_saturation: ratio(
                worker.execution_slots_in_use as u64,
                worker.queue_capacity as u64,
            ),
        };

        Ok(ObservabilitySummary {
            scope: if project_id.is_some() {
                "project".into()
            } else {
                "instance".into()
            },
            project_id,
            generated_at_ms: now,
            window_start_ms,
            window_end_ms,
            resolution_seconds: u32::try_from(range.resolution_ms / 1_000).unwrap_or(u32::MAX),
            retention_days: RETENTION_DAYS,
            current_inflight,
            dropped_samples: self.dropped.load(Ordering::Relaxed),
            totals,
            series,
            busiest_routes,
            slowest_routes,
            frequent_queries,
            slow_queries,
            runtime,
            storage,
        })
    }
}

pub(crate) fn profiles_for_operation(operation: &WorkerOperation) -> Vec<QueryProfile> {
    match operation {
        WorkerOperation::Query(query) => vec![profile_sql(&query.sql)],
        WorkerOperation::Transaction(transaction) => transaction
            .statements
            .iter()
            .map(|statement| profile_sql(&statement.sql))
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn project_from_path(path: &str) -> Option<ProjectId> {
    let mut segments = path.trim_matches('/').split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("v1"), Some("projects"), Some(project)) => {
            Uuid::parse_str(project).ok().map(ProjectId)
        }
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ObservabilityQuery {
    range: Option<String>,
    project_id: Option<Uuid>,
}

pub(crate) async fn project_summary(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ObservabilityQuery>,
) -> Response {
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let (management, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) =
        authorized_project_member(&management, identity.user_id, project_id, request_id).await
    {
        return error.into_response();
    }
    let range = match RangeSpec::parse(query.range.as_deref()) {
        Ok(value) => value,
        Err(error) => return invalid_range_response(error, request_id),
    };
    respond(&state, Some(project_id), range, request_id).await
}

pub(crate) async fn instance_summary(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ObservabilityQuery>,
) -> Response {
    let (_, identity) = match authenticated(&state, &headers, request_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(instance) = &state.instance else {
        return error_response("observability service is unavailable", request_id);
    };
    if let Err(error) = instance.authorize_observability(identity.user_id).await {
        return instance_error(error, request_id);
    }
    let range = match RangeSpec::parse(query.range.as_deref()) {
        Ok(value) => value,
        Err(error) => return invalid_range_response(error, request_id),
    };
    respond(&state, query.project_id.map(ProjectId), range, request_id).await
}

async fn respond(
    state: &ApiState,
    project_id: Option<ProjectId>,
    range: RangeSpec,
    request_id: RequestId,
) -> Response {
    let Some(service) = &state.observability else {
        return error_response("observability service is unavailable", request_id);
    };
    let current_inflight = state
        .metrics
        .as_ref()
        .map_or(0, |metrics| metrics.inflight());
    match service.summary(project_id, range, current_inflight).await {
        Ok(summary) => Json(summary).into_response(),
        Err(error) => {
            tracing::error!(%error, "observability summary query failed");
            error_response("observability data is temporarily unavailable", request_id)
        }
    }
}

fn instance_error(error: InstanceServiceError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        InstanceServiceError::Forbidden => (
            StatusCode::FORBIDDEN,
            "observability.forbidden",
            "instance observability requires an instance administrator",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "observability.unavailable",
            "instance authorization is temporarily unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}

fn error_response(message: &str, request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "observability.unavailable",
        message,
        request_id,
    )
    .into_response()
}

fn invalid_range_response(message: &str, request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "observability.range_invalid",
        message,
        request_id,
    )
    .into_response()
}

#[derive(Clone, Copy, Debug)]
struct RangeSpec {
    window_ms: i64,
    resolution_ms: i64,
}

impl RangeSpec {
    fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        match value.unwrap_or("24h") {
            "1h" => Ok(Self {
                window_ms: 60 * 60 * 1_000,
                resolution_ms: 60 * 1_000,
            }),
            "6h" => Ok(Self {
                window_ms: 6 * 60 * 60 * 1_000,
                resolution_ms: 5 * 60 * 1_000,
            }),
            "24h" => Ok(Self {
                window_ms: 24 * 60 * 60 * 1_000,
                resolution_ms: 15 * 60 * 1_000,
            }),
            "7d" => Ok(Self {
                window_ms: 7 * 24 * 60 * 60 * 1_000,
                resolution_ms: 60 * 60 * 1_000,
            }),
            "30d" => Ok(Self {
                window_ms: 30 * 24 * 60 * 60 * 1_000,
                resolution_ms: 6 * 60 * 60 * 1_000,
            }),
            _ => Err("range must be one of 1h, 6h, 24h, 7d, or 30d"),
        }
    }
}

async fn run_recorder(
    pool: PgPool,
    mut receiver: mpsc::Receiver<Event>,
    mut shutdown: oneshot::Receiver<()>,
    dropped: Arc<AtomicU64>,
) {
    let mut pending = Pending::default();
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Some(event) => aggregate_event(&mut pending, event, &dropped),
                None => break,
            },
            _ = flush.tick() => flush_pending(&pool, &mut pending).await,
            _ = &mut shutdown => break,
        }
    }
    while let Ok(event) = receiver.try_recv() {
        aggregate_event(&mut pending, event, &dropped);
    }
    flush_pending(&pool, &mut pending).await;
}

async fn run_retention(pool: PgPool) {
    let mut cleanup = tokio::time::interval(Duration::from_secs(60 * 60));
    cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        cleanup.tick().await;
        cleanup_retention(&pool).await;
    }
}

fn aggregate_event(pending: &mut Pending, event: Event, dropped: &AtomicU64) {
    match event {
        Event::Http(event) => {
            let key = HttpKey {
                bucket_start_ms: event.bucket_start_ms,
                project_id: event.project_id,
                method: event.method,
                route: event.route,
                status_class: event.status_class,
            };
            if !pending.http.contains_key(&key) && pending.key_count() >= MAX_PENDING_KEYS {
                dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            pending
                .http
                .entry(key)
                .or_default()
                .observe(event.duration_ms);
        }
        Event::Query(event) => {
            let key = QueryKey {
                bucket_start_ms: event.bucket_start_ms,
                project_id: event.project_id,
                fingerprint: event.profile.fingerprint.clone(),
            };
            if !pending.queries.contains_key(&key) && pending.key_count() >= MAX_PENDING_KEYS {
                dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let aggregate = pending
                .queries
                .entry(key)
                .or_insert_with(|| QueryAggregate {
                    profile: event.profile,
                    latency: LatencyAggregate::default(),
                    errors: 0,
                    rows_returned: 0,
                    rows_affected: 0,
                });
            aggregate.latency.observe(event.duration_ms);
            aggregate.errors = aggregate.errors.saturating_add(u64::from(event.failed));
            aggregate.rows_returned = aggregate.rows_returned.saturating_add(event.rows_returned);
            aggregate.rows_affected = aggregate.rows_affected.saturating_add(event.rows_affected);
        }
        Event::Storage(event) => {
            if !pending.storage.contains_key(&event.project_id)
                && pending.key_count() >= MAX_PENDING_KEYS
            {
                dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            pending.storage.insert(event.project_id, event);
        }
    }
}

async fn flush_pending(pool: &PgPool, pending: &mut Pending) {
    if pending.http.is_empty() && pending.queries.is_empty() && pending.storage.is_empty() {
        return;
    }
    let batch = std::mem::take(pending);
    if let Err(error) = persist_batch(pool, &batch).await {
        tracing::error!(%error, "observability aggregate flush failed");
        merge_pending(pending, batch);
    }
}

fn merge_pending(target: &mut Pending, incoming: Pending) {
    for (key, aggregate) in incoming.http {
        target.http.entry(key).or_default().merge(&aggregate);
    }
    for (key, aggregate) in incoming.queries {
        let current = target.queries.entry(key).or_insert_with(|| QueryAggregate {
            profile: aggregate.profile.clone(),
            latency: LatencyAggregate::default(),
            errors: 0,
            rows_returned: 0,
            rows_affected: 0,
        });
        current.latency.merge(&aggregate.latency);
        current.errors = current.errors.saturating_add(aggregate.errors);
        current.rows_returned = current
            .rows_returned
            .saturating_add(aggregate.rows_returned);
        current.rows_affected = current
            .rows_affected
            .saturating_add(aggregate.rows_affected);
    }
    for (project, event) in incoming.storage {
        let replace = target
            .storage
            .get(&project)
            .is_none_or(|current| current.sampled_at_ms <= event.sampled_at_ms);
        if replace {
            target.storage.insert(project, event);
        }
    }
}

async fn persist_batch(pool: &PgPool, batch: &Pending) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    for (key, aggregate) in &batch.http {
        sqlx::query(HTTP_UPSERT_SQL)
            .bind(key.bucket_start_ms)
            .bind(key.project_id)
            .bind(&key.method)
            .bind(&key.route)
            .bind(key.status_class)
            .bind(as_i64(aggregate.count))
            .bind(aggregate.sum_ms)
            .bind(aggregate.max_ms)
            .bind(as_i64(aggregate.buckets[0]))
            .bind(as_i64(aggregate.buckets[1]))
            .bind(as_i64(aggregate.buckets[2]))
            .bind(as_i64(aggregate.buckets[3]))
            .bind(as_i64(aggregate.buckets[4]))
            .bind(as_i64(aggregate.buckets[5]))
            .bind(as_i64(aggregate.buckets[6]))
            .bind(as_i64(aggregate.buckets[7]))
            .bind(as_i64(aggregate.buckets[8]))
            .bind(as_i64(aggregate.buckets[9]))
            .bind(as_i64(aggregate.buckets[10]))
            .bind(as_i64(aggregate.buckets[11]))
            .execute(&mut *transaction)
            .await?;
    }
    for (key, aggregate) in &batch.queries {
        sqlx::query(QUERY_UPSERT_SQL)
            .bind(key.bucket_start_ms)
            .bind(key.project_id)
            .bind(&key.fingerprint)
            .bind(&aggregate.profile.shape)
            .bind(&aggregate.profile.statement_kind)
            .bind(aggregate.profile.read_only)
            .bind(as_i64(aggregate.latency.count))
            .bind(as_i64(aggregate.errors))
            .bind(aggregate.latency.sum_ms)
            .bind(aggregate.latency.max_ms)
            .bind(as_i64(aggregate.rows_returned))
            .bind(as_i64(aggregate.rows_affected))
            .bind(as_i64(aggregate.latency.buckets[0]))
            .bind(as_i64(aggregate.latency.buckets[1]))
            .bind(as_i64(aggregate.latency.buckets[2]))
            .bind(as_i64(aggregate.latency.buckets[3]))
            .bind(as_i64(aggregate.latency.buckets[4]))
            .bind(as_i64(aggregate.latency.buckets[5]))
            .bind(as_i64(aggregate.latency.buckets[6]))
            .bind(as_i64(aggregate.latency.buckets[7]))
            .bind(as_i64(aggregate.latency.buckets[8]))
            .bind(as_i64(aggregate.latency.buckets[9]))
            .bind(as_i64(aggregate.latency.buckets[10]))
            .bind(as_i64(aggregate.latency.buckets[11]))
            .execute(&mut *transaction)
            .await?;
    }
    for event in batch.storage.values() {
        sqlx::query(STORAGE_UPSERT_SQL)
            .bind(event.project_id)
            .bind(as_i64(event.logical_database_bytes))
            .bind(event.sampled_at_ms)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await
}

async fn cleanup_retention(pool: &PgPool) {
    let cutoff = super::now_ms().saturating_sub(i64::from(RETENTION_DAYS) * 24 * 60 * 60 * 1_000);
    for table in ["observability_http_buckets", "observability_query_buckets"] {
        // `table` comes only from this fixed internal allowlist. The cutoff is
        // still bound separately, so no request data enters the SQL string.
        let statement = format!(
            "DELETE FROM {table} WHERE ctid IN (SELECT ctid FROM {table} WHERE bucket_start_ms < $1 LIMIT 10000)"
        );
        loop {
            match sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
                .bind(cutoff)
                .execute(pool)
                .await
            {
                Ok(result) if result.rows_affected() == 10_000 => tokio::task::yield_now().await,
                Ok(_) => break,
                Err(error) => {
                    tracing::error!(%error, table, "observability retention cleanup failed");
                    break;
                }
            }
        }
    }
}

fn profile_sql(sql: &str) -> QueryProfile {
    let (statement_kind, read_only) = classify_statement(sql).map_or_else(
        |_| ("other".to_owned(), false),
        |class| (statement_kind_name(class.kind).to_owned(), class.read_only),
    );
    let shape = redact_sql_shape(sql);
    let fingerprint = hex::encode(Sha256::digest(shape.as_bytes()));
    QueryProfile {
        fingerprint,
        shape,
        statement_kind,
        read_only,
    }
}

fn statement_kind_name(kind: StatementKind) -> &'static str {
    match kind {
        StatementKind::Select => "select",
        StatementKind::Insert => "insert",
        StatementKind::Update => "update",
        StatementKind::Delete => "delete",
        StatementKind::Ddl => "ddl",
        StatementKind::Pragma => "pragma",
        StatementKind::Attach => "attach",
        StatementKind::Detach => "detach",
        StatementKind::Vacuum => "vacuum",
        StatementKind::TransactionControl => "transaction_control",
        StatementKind::Rls => "rls",
        StatementKind::Other => "other",
    }
}

fn redact_sql_shape(sql: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "ABORT",
        "ACTION",
        "ADD",
        "AFTER",
        "ALL",
        "ALTER",
        "ANALYZE",
        "AND",
        "AS",
        "ASC",
        "ATTACH",
        "BEFORE",
        "BEGIN",
        "BETWEEN",
        "BY",
        "CASCADE",
        "CASE",
        "CAST",
        "CHECK",
        "COLLATE",
        "COLUMN",
        "COMMIT",
        "CONFLICT",
        "CONSTRAINT",
        "CREATE",
        "CROSS",
        "CURRENT",
        "DEFAULT",
        "DELETE",
        "DESC",
        "DETACH",
        "DISTINCT",
        "DO",
        "DROP",
        "EACH",
        "ELSE",
        "END",
        "ESCAPE",
        "EXCEPT",
        "EXCLUDE",
        "EXISTS",
        "EXPLAIN",
        "FAIL",
        "FILTER",
        "FOLLOWING",
        "FOR",
        "FOREIGN",
        "FROM",
        "FULL",
        "GENERATED",
        "GLOB",
        "GROUP",
        "HAVING",
        "IF",
        "IGNORE",
        "IN",
        "INDEX",
        "INDEXED",
        "INITIALLY",
        "INNER",
        "INSERT",
        "INSTEAD",
        "INTERSECT",
        "INTO",
        "IS",
        "JOIN",
        "KEY",
        "LEFT",
        "LIKE",
        "LIMIT",
        "MATCH",
        "MATERIALIZED",
        "NATURAL",
        "NO",
        "NOT",
        "NOTHING",
        "NULL",
        "NULLS",
        "OF",
        "OFFSET",
        "ON",
        "OR",
        "ORDER",
        "OTHERS",
        "OUTER",
        "OVER",
        "PARTITION",
        "PLAN",
        "PRAGMA",
        "PRECEDING",
        "PRIMARY",
        "QUERY",
        "RAISE",
        "RANGE",
        "RECURSIVE",
        "REFERENCES",
        "REGEXP",
        "REINDEX",
        "RELEASE",
        "RENAME",
        "REPLACE",
        "RESTRICT",
        "RETURNING",
        "RIGHT",
        "ROLLBACK",
        "ROW",
        "ROWS",
        "SAVEPOINT",
        "SELECT",
        "SET",
        "TABLE",
        "TEMP",
        "TEMPORARY",
        "THEN",
        "TIES",
        "TO",
        "TRANSACTION",
        "TRIGGER",
        "UNBOUNDED",
        "UNION",
        "UNIQUE",
        "UPDATE",
        "USING",
        "VACUUM",
        "VALUES",
        "VIEW",
        "VIRTUAL",
        "WHEN",
        "WHERE",
        "WINDOW",
        "WITH",
        "WITHOUT",
    ];
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() && tokens.len() < 96 {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        if bytes.get(index + 1) == Some(&quote) {
                            index += 2;
                        } else {
                            index += 1;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
                tokens.push("?".to_owned());
            }
            b'[' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b']' {
                    index += 1;
                }
                index = (index + 1).min(bytes.len());
                tokens.push("?".to_owned());
            }
            b'?' | b':' | b'@' | b'$' => {
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                tokens.push("?".to_owned());
            }
            byte if byte.is_ascii_digit() => {
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric()
                        || matches!(bytes[index], b'.' | b'_' | b'+' | b'-'))
                {
                    index += 1;
                }
                tokens.push("?".to_owned());
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                let word = sql[start..index].to_ascii_uppercase();
                tokens.push(if KEYWORDS.binary_search(&word.as_str()).is_ok() {
                    word
                } else {
                    "?".to_owned()
                });
            }
            punctuation @ (b'(' | b')' | b',' | b';' | b'.') => {
                tokens.push(char::from(punctuation).to_string());
                index += 1;
            }
            operator @ (b'=' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'|') => {
                let mut rendered = char::from(operator).to_string();
                index += 1;
                if index < bytes.len() && matches!(bytes[index], b'=' | b'>' | b'<' | b'|') {
                    rendered.push(char::from(bytes[index]));
                    index += 1;
                }
                tokens.push(rendered);
            }
            _ => {
                tokens.push("?".to_owned());
                index += 1;
            }
        }
    }
    let mut shape = tokens.join(" ");
    if shape.is_empty() {
        shape = "UNKNOWN".into();
    }
    shape.truncate(320);
    shape
}

#[derive(Clone, Debug, Default)]
struct ReadStats {
    latency: LatencyAggregate,
    client_errors: u64,
    server_errors: u64,
}

fn stats_from_row(row: &sqlx::postgres::PgRow) -> ReadStats {
    ReadStats {
        latency: LatencyAggregate {
            count: as_u64(row.get::<i64, _>("request_count")),
            sum_ms: row.get("duration_sum_ms"),
            max_ms: row.get("duration_max_ms"),
            buckets: [
                as_u64(row.get("latency_le_5_ms")),
                as_u64(row.get("latency_le_10_ms")),
                as_u64(row.get("latency_le_25_ms")),
                as_u64(row.get("latency_le_50_ms")),
                as_u64(row.get("latency_le_100_ms")),
                as_u64(row.get("latency_le_250_ms")),
                as_u64(row.get("latency_le_500_ms")),
                as_u64(row.get("latency_le_1000_ms")),
                as_u64(row.get("latency_le_2500_ms")),
                as_u64(row.get("latency_le_5000_ms")),
                as_u64(row.get("latency_le_15000_ms")),
                as_u64(row.get("latency_le_60000_ms")),
            ],
        },
        client_errors: as_u64(row.get("client_errors")),
        server_errors: as_u64(row.get("server_errors")),
    }
}

fn http_totals(stats: &ReadStats, window_ms: i64) -> ObservabilityHttpTotals {
    ObservabilityHttpTotals {
        requests: stats.latency.count,
        qps: per_second(stats.latency.count, window_ms),
        client_errors: stats.client_errors,
        server_errors: stats.server_errors,
        error_rate: ratio(
            stats.client_errors.saturating_add(stats.server_errors),
            stats.latency.count,
        ),
        average_latency_ms: average(&stats.latency),
        p50_latency_ms: percentile(&stats.latency, 0.50),
        p95_latency_ms: percentile(&stats.latency, 0.95),
        p99_latency_ms: percentile(&stats.latency, 0.99),
        max_latency_ms: (stats.latency.count > 0).then_some(stats.latency.max_ms),
    }
}

fn time_point(timestamp_ms: i64, stats: &ReadStats, resolution_ms: i64) -> ObservabilityTimePoint {
    ObservabilityTimePoint {
        timestamp_ms,
        requests: stats.latency.count,
        qps: per_second(stats.latency.count, resolution_ms),
        client_errors: stats.client_errors,
        server_errors: stats.server_errors,
        p50_latency_ms: percentile(&stats.latency, 0.50),
        p95_latency_ms: percentile(&stats.latency, 0.95),
        p99_latency_ms: percentile(&stats.latency, 0.99),
    }
}

fn route_metric(row: &sqlx::postgres::PgRow, window_ms: i64) -> ObservabilityRouteMetric {
    let stats = stats_from_row(row);
    ObservabilityRouteMetric {
        method: row.get("method"),
        route: row.get("route"),
        requests: stats.latency.count,
        qps: per_second(stats.latency.count, window_ms),
        error_rate: ratio(
            stats.client_errors.saturating_add(stats.server_errors),
            stats.latency.count,
        ),
        average_latency_ms: average(&stats.latency),
        p50_latency_ms: percentile(&stats.latency, 0.50),
        p95_latency_ms: percentile(&stats.latency, 0.95),
        p99_latency_ms: percentile(&stats.latency, 0.99),
        max_latency_ms: (stats.latency.count > 0).then_some(stats.latency.max_ms),
    }
}

fn query_metric(row: &sqlx::postgres::PgRow) -> ObservabilityQueryMetric {
    let latency = LatencyAggregate {
        count: as_u64(row.get::<i64, _>("execution_count")),
        sum_ms: row.get("duration_sum_ms"),
        max_ms: row.get("duration_max_ms"),
        buckets: [
            as_u64(row.get("latency_le_5_ms")),
            as_u64(row.get("latency_le_10_ms")),
            as_u64(row.get("latency_le_25_ms")),
            as_u64(row.get("latency_le_50_ms")),
            as_u64(row.get("latency_le_100_ms")),
            as_u64(row.get("latency_le_250_ms")),
            as_u64(row.get("latency_le_500_ms")),
            as_u64(row.get("latency_le_1000_ms")),
            as_u64(row.get("latency_le_2500_ms")),
            as_u64(row.get("latency_le_5000_ms")),
            as_u64(row.get("latency_le_15000_ms")),
            as_u64(row.get("latency_le_60000_ms")),
        ],
    };
    let errors = as_u64(row.get("error_count"));
    ObservabilityQueryMetric {
        fingerprint: row.get("fingerprint"),
        shape: row.get("shape"),
        statement_kind: row.get("statement_kind"),
        read_only: row.get("read_only"),
        executions: latency.count,
        errors,
        error_rate: ratio(errors, latency.count),
        average_latency_ms: average(&latency),
        p50_latency_ms: percentile(&latency, 0.50),
        p95_latency_ms: percentile(&latency, 0.95),
        p99_latency_ms: percentile(&latency, 0.99),
        max_latency_ms: (latency.count > 0).then_some(latency.max_ms),
        rows_returned: as_u64(row.get("rows_returned")),
        rows_affected: as_u64(row.get("rows_affected")),
    }
}

fn storage_snapshot(
    row: &sqlx::postgres::PgRow,
    database_root: &FsPath,
    backup_root: &FsPath,
) -> ObservabilityStorageSnapshot {
    let database_disk = disk_capacity(database_root);
    let backup_disk = disk_capacity(backup_root);
    ObservabilityStorageSnapshot {
        logical_database_bytes: as_u64(row.get::<i64, _>("logical_database_bytes")),
        sampled_projects: saturating_u32(as_u64(row.get::<i64, _>("sampled_projects")) as usize),
        database_disk_total_bytes: database_disk.map(|value| value.0),
        database_disk_available_bytes: database_disk.map(|value| value.1),
        database_disk_used_percent: database_disk.map(|value| used_percent(value.0, value.1)),
        backup_disk_total_bytes: backup_disk.map(|value| value.0),
        backup_disk_available_bytes: backup_disk.map(|value| value.1),
        backup_disk_used_percent: backup_disk.map(|value| used_percent(value.0, value.1)),
        last_sample_at_ms: row.get("last_sample_at_ms"),
    }
}

fn disk_capacity(path: &FsPath) -> Option<(u64, u64)> {
    let total = fs2::total_space(path).ok()?;
    let available = fs2::available_space(path).ok()?;
    Some((total, available))
}

fn used_percent(total: u64, available: u64) -> f64 {
    ratio(total.saturating_sub(available), total) * 100.0
}

fn average(latency: &LatencyAggregate) -> Option<f64> {
    (latency.count > 0).then(|| latency.sum_ms / latency.count as f64)
}

fn percentile(latency: &LatencyAggregate, percentile: f64) -> Option<f64> {
    if latency.count == 0 {
        return None;
    }
    let target = (latency.count as f64 * percentile).ceil() as u64;
    let mut previous_count = 0_u64;
    let mut lower_bound = 0.0_f64;
    for (count, upper_bound) in latency.buckets.iter().zip(LATENCY_BOUNDS_MS) {
        if *count >= target {
            // The retained counters are cumulative histograms, not exact
            // samples. Returning the bucket ceiling made a 501 ms request look
            // like 1 s (and a 1.01 s request look like 2.5 s). Use the same
            // linear interpolation convention as Prometheus histograms, then
            // clamp to the observed maximum so a narrow/single-sample bucket
            // remains exact instead of being exaggerated.
            let bucket_count = count.saturating_sub(previous_count);
            let estimate = if bucket_count == 0 {
                upper_bound
            } else {
                let rank_in_bucket = target.saturating_sub(previous_count) as f64;
                let fraction = (rank_in_bucket / bucket_count as f64).clamp(0.0, 1.0);
                lower_bound + (upper_bound - lower_bound) * fraction
            };
            return Some(estimate.min(latency.max_ms));
        }
        previous_count = *count;
        lower_bound = upper_bound;
    }
    Some(latency.max_ms.min(60_000.0))
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn per_second(count: u64, duration_ms: i64) -> f64 {
    if duration_ms <= 0 {
        0.0
    } else {
        count as f64 / (duration_ms as f64 / 1_000.0)
    }
}

fn minute_bucket(now_ms: i64) -> i64 {
    now_ms.max(0).div_euclid(60_000) * 60_000
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

const HTTP_UPSERT_SQL: &str = "INSERT INTO observability_http_buckets (
    bucket_start_ms,project_id,method,route,status_class,request_count,duration_sum_ms,
    duration_max_ms,latency_le_5_ms,latency_le_10_ms,latency_le_25_ms,latency_le_50_ms,
    latency_le_100_ms,latency_le_250_ms,latency_le_500_ms,latency_le_1000_ms,
    latency_le_2500_ms,latency_le_5000_ms,latency_le_15000_ms,latency_le_60000_ms)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)
ON CONFLICT (bucket_start_ms,project_id,method,route,status_class) DO UPDATE SET
    request_count=observability_http_buckets.request_count+excluded.request_count,
    duration_sum_ms=observability_http_buckets.duration_sum_ms+excluded.duration_sum_ms,
    duration_max_ms=greatest(observability_http_buckets.duration_max_ms,excluded.duration_max_ms),
    latency_le_5_ms=observability_http_buckets.latency_le_5_ms+excluded.latency_le_5_ms,
    latency_le_10_ms=observability_http_buckets.latency_le_10_ms+excluded.latency_le_10_ms,
    latency_le_25_ms=observability_http_buckets.latency_le_25_ms+excluded.latency_le_25_ms,
    latency_le_50_ms=observability_http_buckets.latency_le_50_ms+excluded.latency_le_50_ms,
    latency_le_100_ms=observability_http_buckets.latency_le_100_ms+excluded.latency_le_100_ms,
    latency_le_250_ms=observability_http_buckets.latency_le_250_ms+excluded.latency_le_250_ms,
    latency_le_500_ms=observability_http_buckets.latency_le_500_ms+excluded.latency_le_500_ms,
    latency_le_1000_ms=observability_http_buckets.latency_le_1000_ms+excluded.latency_le_1000_ms,
    latency_le_2500_ms=observability_http_buckets.latency_le_2500_ms+excluded.latency_le_2500_ms,
    latency_le_5000_ms=observability_http_buckets.latency_le_5000_ms+excluded.latency_le_5000_ms,
    latency_le_15000_ms=observability_http_buckets.latency_le_15000_ms+excluded.latency_le_15000_ms,
    latency_le_60000_ms=observability_http_buckets.latency_le_60000_ms+excluded.latency_le_60000_ms";

const QUERY_UPSERT_SQL: &str = "INSERT INTO observability_query_buckets (
    bucket_start_ms,project_id,fingerprint,shape,statement_kind,read_only,execution_count,error_count,
    duration_sum_ms,duration_max_ms,rows_returned,rows_affected,latency_le_5_ms,latency_le_10_ms,
    latency_le_25_ms,latency_le_50_ms,latency_le_100_ms,latency_le_250_ms,latency_le_500_ms,
    latency_le_1000_ms,latency_le_2500_ms,latency_le_5000_ms,latency_le_15000_ms,latency_le_60000_ms)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)
ON CONFLICT (bucket_start_ms,project_id,fingerprint) DO UPDATE SET
    execution_count=observability_query_buckets.execution_count+excluded.execution_count,
    error_count=observability_query_buckets.error_count+excluded.error_count,
    duration_sum_ms=observability_query_buckets.duration_sum_ms+excluded.duration_sum_ms,
    duration_max_ms=greatest(observability_query_buckets.duration_max_ms,excluded.duration_max_ms),
    rows_returned=observability_query_buckets.rows_returned+excluded.rows_returned,
    rows_affected=observability_query_buckets.rows_affected+excluded.rows_affected,
    latency_le_5_ms=observability_query_buckets.latency_le_5_ms+excluded.latency_le_5_ms,
    latency_le_10_ms=observability_query_buckets.latency_le_10_ms+excluded.latency_le_10_ms,
    latency_le_25_ms=observability_query_buckets.latency_le_25_ms+excluded.latency_le_25_ms,
    latency_le_50_ms=observability_query_buckets.latency_le_50_ms+excluded.latency_le_50_ms,
    latency_le_100_ms=observability_query_buckets.latency_le_100_ms+excluded.latency_le_100_ms,
    latency_le_250_ms=observability_query_buckets.latency_le_250_ms+excluded.latency_le_250_ms,
    latency_le_500_ms=observability_query_buckets.latency_le_500_ms+excluded.latency_le_500_ms,
    latency_le_1000_ms=observability_query_buckets.latency_le_1000_ms+excluded.latency_le_1000_ms,
    latency_le_2500_ms=observability_query_buckets.latency_le_2500_ms+excluded.latency_le_2500_ms,
    latency_le_5000_ms=observability_query_buckets.latency_le_5000_ms+excluded.latency_le_5000_ms,
    latency_le_15000_ms=observability_query_buckets.latency_le_15000_ms+excluded.latency_le_15000_ms,
    latency_le_60000_ms=observability_query_buckets.latency_le_60000_ms+excluded.latency_le_60000_ms";

const STORAGE_UPSERT_SQL: &str = "INSERT INTO observability_project_storage
    (project_id,logical_database_bytes,sampled_at_ms) VALUES ($1,$2,$3)
ON CONFLICT (project_id) DO UPDATE SET
    logical_database_bytes=excluded.logical_database_bytes,
    sampled_at_ms=excluded.sampled_at_ms
WHERE observability_project_storage.sampled_at_ms <= excluded.sampled_at_ms";

const HTTP_SERIES_SQL: &str = "SELECT (bucket_start_ms/$5)*$5 bucket_start_ms,
    sum(request_count)::bigint request_count,
    coalesce(sum(duration_sum_ms),0)::double precision duration_sum_ms,
    coalesce(max(duration_max_ms),0)::double precision duration_max_ms,
    coalesce(sum(request_count) FILTER (WHERE status_class=4),0)::bigint client_errors,
    coalesce(sum(request_count) FILTER (WHERE status_class=5),0)::bigint server_errors,
    coalesce(sum(latency_le_5_ms),0)::bigint latency_le_5_ms,
    coalesce(sum(latency_le_10_ms),0)::bigint latency_le_10_ms,
    coalesce(sum(latency_le_25_ms),0)::bigint latency_le_25_ms,
    coalesce(sum(latency_le_50_ms),0)::bigint latency_le_50_ms,
    coalesce(sum(latency_le_100_ms),0)::bigint latency_le_100_ms,
    coalesce(sum(latency_le_250_ms),0)::bigint latency_le_250_ms,
    coalesce(sum(latency_le_500_ms),0)::bigint latency_le_500_ms,
    coalesce(sum(latency_le_1000_ms),0)::bigint latency_le_1000_ms,
    coalesce(sum(latency_le_2500_ms),0)::bigint latency_le_2500_ms,
    coalesce(sum(latency_le_5000_ms),0)::bigint latency_le_5000_ms,
    coalesce(sum(latency_le_15000_ms),0)::bigint latency_le_15000_ms,
    coalesce(sum(latency_le_60000_ms),0)::bigint latency_le_60000_ms
FROM observability_http_buckets WHERE bucket_start_ms >= $1 AND bucket_start_ms < $2
    AND (NOT $3 OR project_id=$4)
GROUP BY (bucket_start_ms/$5)*$5 ORDER BY bucket_start_ms";

const HTTP_TOTAL_SQL: &str = "SELECT
    coalesce(sum(request_count),0)::bigint request_count,
    coalesce(sum(duration_sum_ms),0)::double precision duration_sum_ms,
    coalesce(max(duration_max_ms),0)::double precision duration_max_ms,
    coalesce(sum(request_count) FILTER (WHERE status_class=4),0)::bigint client_errors,
    coalesce(sum(request_count) FILTER (WHERE status_class=5),0)::bigint server_errors,
    coalesce(sum(latency_le_5_ms),0)::bigint latency_le_5_ms,
    coalesce(sum(latency_le_10_ms),0)::bigint latency_le_10_ms,
    coalesce(sum(latency_le_25_ms),0)::bigint latency_le_25_ms,
    coalesce(sum(latency_le_50_ms),0)::bigint latency_le_50_ms,
    coalesce(sum(latency_le_100_ms),0)::bigint latency_le_100_ms,
    coalesce(sum(latency_le_250_ms),0)::bigint latency_le_250_ms,
    coalesce(sum(latency_le_500_ms),0)::bigint latency_le_500_ms,
    coalesce(sum(latency_le_1000_ms),0)::bigint latency_le_1000_ms,
    coalesce(sum(latency_le_2500_ms),0)::bigint latency_le_2500_ms,
    coalesce(sum(latency_le_5000_ms),0)::bigint latency_le_5000_ms,
    coalesce(sum(latency_le_15000_ms),0)::bigint latency_le_15000_ms,
    coalesce(sum(latency_le_60000_ms),0)::bigint latency_le_60000_ms
FROM observability_http_buckets WHERE bucket_start_ms >= $1 AND bucket_start_ms < $2
    AND (NOT $3 OR project_id=$4)";

const HTTP_ROUTES_SQL: &str = "SELECT method,route,
    sum(request_count)::bigint request_count,
    coalesce(sum(duration_sum_ms),0)::double precision duration_sum_ms,
    coalesce(max(duration_max_ms),0)::double precision duration_max_ms,
    coalesce(sum(request_count) FILTER (WHERE status_class=4),0)::bigint client_errors,
    coalesce(sum(request_count) FILTER (WHERE status_class=5),0)::bigint server_errors,
    coalesce(sum(latency_le_5_ms),0)::bigint latency_le_5_ms,
    coalesce(sum(latency_le_10_ms),0)::bigint latency_le_10_ms,
    coalesce(sum(latency_le_25_ms),0)::bigint latency_le_25_ms,
    coalesce(sum(latency_le_50_ms),0)::bigint latency_le_50_ms,
    coalesce(sum(latency_le_100_ms),0)::bigint latency_le_100_ms,
    coalesce(sum(latency_le_250_ms),0)::bigint latency_le_250_ms,
    coalesce(sum(latency_le_500_ms),0)::bigint latency_le_500_ms,
    coalesce(sum(latency_le_1000_ms),0)::bigint latency_le_1000_ms,
    coalesce(sum(latency_le_2500_ms),0)::bigint latency_le_2500_ms,
    coalesce(sum(latency_le_5000_ms),0)::bigint latency_le_5000_ms,
    coalesce(sum(latency_le_15000_ms),0)::bigint latency_le_15000_ms,
    coalesce(sum(latency_le_60000_ms),0)::bigint latency_le_60000_ms
FROM observability_http_buckets WHERE bucket_start_ms >= $1 AND bucket_start_ms < $2
    AND (NOT $3 OR project_id=$4)
GROUP BY method,route ORDER BY request_count DESC LIMIT 500";

const QUERY_METRICS_SQL: &str =
    "SELECT fingerprint,max(shape) shape,max(statement_kind) statement_kind,
    bool_and(read_only) read_only,sum(execution_count)::bigint execution_count,
    sum(error_count)::bigint error_count,
    coalesce(sum(duration_sum_ms),0)::double precision duration_sum_ms,
    coalesce(max(duration_max_ms),0)::double precision duration_max_ms,
    coalesce(sum(rows_returned),0)::bigint rows_returned,
    coalesce(sum(rows_affected),0)::bigint rows_affected,
    coalesce(sum(latency_le_5_ms),0)::bigint latency_le_5_ms,
    coalesce(sum(latency_le_10_ms),0)::bigint latency_le_10_ms,
    coalesce(sum(latency_le_25_ms),0)::bigint latency_le_25_ms,
    coalesce(sum(latency_le_50_ms),0)::bigint latency_le_50_ms,
    coalesce(sum(latency_le_100_ms),0)::bigint latency_le_100_ms,
    coalesce(sum(latency_le_250_ms),0)::bigint latency_le_250_ms,
    coalesce(sum(latency_le_500_ms),0)::bigint latency_le_500_ms,
    coalesce(sum(latency_le_1000_ms),0)::bigint latency_le_1000_ms,
    coalesce(sum(latency_le_2500_ms),0)::bigint latency_le_2500_ms,
    coalesce(sum(latency_le_5000_ms),0)::bigint latency_le_5000_ms,
    coalesce(sum(latency_le_15000_ms),0)::bigint latency_le_15000_ms,
    coalesce(sum(latency_le_60000_ms),0)::bigint latency_le_60000_ms
FROM observability_query_buckets WHERE bucket_start_ms >= $1 AND bucket_start_ms < $2
    AND (NOT $3 OR project_id=$4)
GROUP BY fingerprint ORDER BY execution_count DESC LIMIT 500";

const STORAGE_SQL: &str = "SELECT
    coalesce(sum(logical_database_bytes),0)::bigint logical_database_bytes,
    count(*)::bigint sampled_projects,
    max(sampled_at_ms)::bigint last_sample_at_ms
FROM observability_project_storage WHERE (NOT $1 OR project_id=$2)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_shape_removes_identifiers_literals_comments_and_parameters() {
        let profile = profile_sql(
            "SELECT secret_name, email FROM customer_accounts WHERE email = 'person@example.com' AND id = :account_id -- private",
        );
        assert_eq!(profile.statement_kind, "select");
        assert!(profile.read_only);
        assert!(!profile.shape.contains("secret_name"));
        assert!(!profile.shape.contains("customer_accounts"));
        assert!(!profile.shape.contains("person@example.com"));
        assert!(!profile.shape.contains("account_id"));
        assert_eq!(profile.fingerprint.len(), 64);
        assert!(
            profile
                .shape
                .starts_with("SELECT ? , ? FROM ? WHERE ? = ? AND ? = ?")
        );
    }

    #[test]
    fn fingerprints_group_value_and_identifier_variants() {
        let first = profile_sql("SELECT email FROM accounts WHERE id = 1");
        let second = profile_sql("select name from customers where customer_id = 999");
        assert_eq!(first.shape, second.shape);
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn path_project_attribution_is_strict() {
        let project = ProjectId::new();
        assert_eq!(
            project_from_path(&format!("/v1/projects/{project}/query")),
            Some(project)
        );
        assert_eq!(project_from_path("/v1/instance/observability"), None);
        assert_eq!(project_from_path("/v1/projects/not-a-uuid/query"), None);
    }

    #[test]
    fn invalid_ranges_fail_as_client_errors() {
        assert!(RangeSpec::parse(Some("31d")).is_err());
        let response = invalid_range_response("unsupported observability range", RequestId::new());
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn capacity_limit_keeps_aggregating_existing_keys() {
        let mut pending = Pending::default();
        for index in 1..=MAX_PENDING_KEYS {
            let project_id = Uuid::from_u128(index as u128);
            pending.storage.insert(
                project_id,
                StorageEvent {
                    project_id,
                    logical_database_bytes: 1,
                    sampled_at_ms: 1,
                },
            );
        }
        let dropped = AtomicU64::new(0);
        let existing_project = Uuid::from_u128(1);
        aggregate_event(
            &mut pending,
            Event::Storage(StorageEvent {
                project_id: existing_project,
                logical_database_bytes: 2,
                sampled_at_ms: 2,
            }),
            &dropped,
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert_eq!(
            pending
                .storage
                .get(&existing_project)
                .map(|sample| sample.logical_database_bytes),
            Some(2)
        );

        let new_project = Uuid::from_u128((MAX_PENDING_KEYS + 1) as u128);
        aggregate_event(
            &mut pending,
            Event::Storage(StorageEvent {
                project_id: new_project,
                logical_database_bytes: 1,
                sampled_at_ms: 2,
            }),
            &dropped,
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(!pending.storage.contains_key(&new_project));
    }

    #[test]
    fn percentile_interpolates_instead_of_reporting_bucket_ceiling() {
        let latency = LatencyAggregate {
            count: 20,
            sum_ms: 2_400.0,
            max_ms: 790.0,
            buckets: [0, 0, 0, 0, 0, 0, 18, 20, 20, 20, 20, 20],
        };

        assert_eq!(percentile(&latency, 0.95), Some(750.0));
        assert_eq!(percentile(&latency, 0.99), Some(790.0));
    }

    #[test]
    fn percentile_never_exceeds_observed_maximum() {
        let mut latency = LatencyAggregate::default();
        for _ in 0..100 {
            latency.observe(101.0);
        }

        assert_eq!(percentile(&latency, 0.95), Some(101.0));
    }
}
