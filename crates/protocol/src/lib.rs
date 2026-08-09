//! Versioned, provider-neutral contracts shared by FFDB services and workers.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub mod http;
pub use http::*;

/// Current public JSON protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Largest integer that can be represented exactly by JavaScript.
pub const MAX_SAFE_JSON_INTEGER: i64 = 9_007_199_254_740_991;
/// Smallest integer that can be represented exactly by JavaScript.
pub const MIN_SAFE_JSON_INTEGER: i64 = -9_007_199_254_740_991;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Deserialize,
            Eq,
            Hash,
            JsonSchema,
            Ord,
            PartialEq,
            PartialOrd,
            Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

opaque_id!(ProjectId);
opaque_id!(DatabaseId);
opaque_id!(OrganizationId);
opaque_id!(UserId);
opaque_id!(TokenId);
opaque_id!(ApiKeyId);
opaque_id!(RequestId);
opaque_id!(TransactionId);
opaque_id!(NodeId);
opaque_id!(SessionId);
opaque_id!(BackupId);
opaque_id!(CommerceProductId);
opaque_id!(CommercePriceId);
opaque_id!(CommerceCustomerId);
opaque_id!(CommerceOrderId);
opaque_id!(CommercePaymentId);
opaque_id!(CommerceRefundId);
opaque_id!(CommerceSubscriptionId);

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AuthContext {
    pub project_id: ProjectId,
    pub subject: UserId,
    pub role: String,
    #[serde(default)]
    pub claims: Map<String, Value>,
    pub token_id: TokenId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeveloperPrincipal {
    pub organization_id: OrganizationId,
    pub api_key_id: ApiKeyId,
    pub scopes: Vec<DeveloperScope>,
    pub actor_label: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeveloperScope {
    ProjectsRead,
    ProjectsWrite,
    DatabaseQuery,
    DatabaseMigrate,
    DatabaseSchema,
    AuthManage,
    StorageManage,
    EmailManage,
    KeysRotate,
    BackupsManage,
    LogsRead,
    CommerceManage,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "mode", content = "principal", rename_all = "snake_case")]
pub enum ExecutionMode {
    Developer(DeveloperPrincipal),
    EndUser(AuthContext),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DatabaseRoute {
    pub project_id: ProjectId,
    pub database_id: DatabaseId,
    pub node_id: NodeId,
    pub generation: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SqlParameter {
    Null,
    Integer(IntegerInput),
    Real(f64),
    Text(String),
    Blob(String),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum IntegerInput {
    Number(i64),
    Decimal(String),
}

impl IntegerInput {
    pub fn as_i64(&self) -> Result<i64, ProtocolValidationError> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::Decimal(value) => value
                .parse()
                .map_err(|_| ProtocolValidationError::InvalidInteger(value.clone())),
        }
    }
}

impl SqlParameter {
    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        match self {
            Self::Integer(value) => {
                value.as_i64()?;
                Ok(())
            }
            Self::Real(value) if !value.is_finite() => Err(ProtocolValidationError::NonFiniteReal),
            Self::Blob(value) => base64::engine::general_purpose::STANDARD
                .decode(value)
                .map(|_| ())
                .map_err(|_| ProtocolValidationError::InvalidBase64),
            Self::Null | Self::Real(_) | Self::Text(_) => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredColumnType {
    Null,
    Integer,
    Real,
    Text,
    Blob,
    Date,
    Timestamp,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ColumnMetadata {
    pub name: String,
    #[serde(rename = "type")]
    pub declared_type: DeclaredColumnType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_table: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResultCell {
    Null(()),
    SafeInteger(i64),
    Real(f64),
    Text(String),
    Blob(BlobValue),
}

impl ResultCell {
    #[must_use]
    pub fn integer(value: i64) -> Self {
        if (MIN_SAFE_JSON_INTEGER..=MAX_SAFE_JSON_INTEGER).contains(&value) {
            Self::SafeInteger(value)
        } else {
            Self::Text(value.to_string())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct BlobValue {
    #[serde(rename = "$blob")]
    pub base64: String,
}

impl BlobValue {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct QueryOptions {
    #[serde(default = "default_max_rows")]
    pub max_rows: u32,
}

const fn default_max_rows() -> u32 {
    1_000
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            max_rows: default_max_rows(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct QueryRequest {
    pub sql: String,
    #[serde(default)]
    pub parameters: Vec<SqlParameter>,
    #[serde(default)]
    pub options: QueryOptions,
}

impl QueryRequest {
    pub fn validate(&self, limits: &ResourceLimits) -> Result<(), ProtocolValidationError> {
        if self.sql.trim().is_empty() {
            return Err(ProtocolValidationError::EmptySql);
        }
        if self.sql.len() > limits.max_sql_bytes as usize {
            return Err(ProtocolValidationError::SqlTooLong);
        }
        if self.parameters.len() > limits.max_variables as usize {
            return Err(ProtocolValidationError::TooManyVariables);
        }
        if self.options.max_rows == 0 || self.options.max_rows > limits.max_result_rows {
            return Err(ProtocolValidationError::InvalidRowLimit);
        }
        self.parameters.iter().try_for_each(SqlParameter::validate)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnMetadata>,
    pub rows: Vec<Vec<ResultCell>>,
    pub affected_rows: u64,
    pub last_insert_rowid: Option<i64>,
    pub truncated: bool,
}

/// Retained, privacy-safe performance telemetry returned to portal operators.
/// Query shapes never contain identifiers, comments, literal values, or bound
/// parameter values; `fingerprint` is the SHA-256 digest of that redacted shape.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilitySummary {
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub generated_at_ms: i64,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub resolution_seconds: u32,
    pub retention_days: u16,
    pub current_inflight: u64,
    pub dropped_samples: u64,
    pub totals: ObservabilityHttpTotals,
    pub series: Vec<ObservabilityTimePoint>,
    pub busiest_routes: Vec<ObservabilityRouteMetric>,
    pub slowest_routes: Vec<ObservabilityRouteMetric>,
    pub frequent_queries: Vec<ObservabilityQueryMetric>,
    pub slow_queries: Vec<ObservabilityQueryMetric>,
    pub runtime: ObservabilityRuntimeSnapshot,
    pub storage: ObservabilityStorageSnapshot,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityHttpTotals {
    pub requests: u64,
    pub qps: f64,
    pub client_errors: u64,
    pub server_errors: u64,
    pub error_rate: f64,
    pub average_latency_ms: Option<f64>,
    pub p50_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
    pub p99_latency_ms: Option<f64>,
    pub max_latency_ms: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityTimePoint {
    pub timestamp_ms: i64,
    pub requests: u64,
    pub qps: f64,
    pub client_errors: u64,
    pub server_errors: u64,
    pub p50_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
    pub p99_latency_ms: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityRouteMetric {
    pub method: String,
    pub route: String,
    pub requests: u64,
    pub qps: f64,
    pub error_rate: f64,
    pub average_latency_ms: Option<f64>,
    pub p50_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
    pub p99_latency_ms: Option<f64>,
    pub max_latency_ms: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityQueryMetric {
    pub fingerprint: String,
    pub shape: String,
    pub statement_kind: String,
    pub read_only: bool,
    pub executions: u64,
    pub errors: u64,
    pub error_rate: f64,
    pub average_latency_ms: Option<f64>,
    pub p50_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
    pub p99_latency_ms: Option<f64>,
    pub max_latency_ms: Option<f64>,
    pub rows_returned: u64,
    pub rows_affected: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityRuntimeSnapshot {
    pub healthy: bool,
    pub active_workers: u32,
    pub max_workers: u32,
    pub worker_saturation: f64,
    pub execution_slots_in_use: u32,
    pub queue_capacity: u32,
    pub queue_saturation: f64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityStorageSnapshot {
    pub logical_database_bytes: u64,
    pub sampled_projects: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_disk_total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_disk_available_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_disk_used_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_disk_total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_disk_available_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_disk_used_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sample_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct TransactionRequest {
    pub statements: Vec<QueryRequest>,
}

impl TransactionRequest {
    pub fn validate(&self, limits: &ResourceLimits) -> Result<(), ProtocolValidationError> {
        if self.statements.is_empty()
            || self.statements.len() > limits.max_transaction_statements as usize
        {
            return Err(ProtocolValidationError::InvalidTransactionSize);
        }
        self.statements
            .iter()
            .try_for_each(|statement| statement.validate(limits))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ResourceLimits {
    pub max_sql_bytes: u32,
    pub max_variables: u32,
    pub max_result_rows: u32,
    pub max_response_bytes: u64,
    pub statement_timeout_ms: u64,
    pub transaction_timeout_ms: u64,
    pub max_transaction_statements: u16,
    pub max_database_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_sql_bytes: 256 * 1024,
            max_variables: 999,
            max_result_rows: 10_000,
            max_response_bytes: 8 * 1024 * 1024,
            statement_timeout_ms: 5_000,
            transaction_timeout_ms: 15_000,
            max_transaction_statements: 100,
            max_database_bytes: 1024 * 1024 * 1024,
        }
    }
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        if self.max_sql_bytes == 0
            || self.max_variables == 0
            || self.max_result_rows == 0
            || self.max_response_bytes == 0
            || self.statement_timeout_ms == 0
            || self.transaction_timeout_ms < self.statement_timeout_ms
            || self.max_transaction_statements == 0
            || self.max_database_bytes == 0
        {
            return Err(ProtocolValidationError::InvalidResourceLimits);
        }
        Ok(())
    }

    #[must_use]
    pub const fn statement_timeout(&self) -> Duration {
        Duration::from_millis(self.statement_timeout_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MigrationSpec {
    pub id: String,
    pub name: String,
    pub up_sql: String,
    pub down_sql: String,
    pub checksum: String,
    pub created_at_ms: i64,
}

impl MigrationSpec {
    #[must_use]
    pub fn calculate_checksum(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_bytes());
        hasher.update([0]);
        hasher.update(self.name.as_bytes());
        hasher.update([0]);
        hasher.update(self.up_sql.as_bytes());
        hasher.update([0]);
        hasher.update(self.down_sql.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.name.is_empty()
            || self.name.len() > 256
        {
            return Err(ProtocolValidationError::InvalidMigrationMetadata);
        }
        if self.up_sql.trim().is_empty() || self.down_sql.trim().is_empty() {
            return Err(ProtocolValidationError::MissingMigrationDirection);
        }
        if self.checksum != self.calculate_checksum() {
            return Err(ProtocolValidationError::ChecksumMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    Pending,
    Applying,
    Applied,
    RollingBack,
    RolledBack,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MigrationRecord {
    pub spec: MigrationSpec,
    pub status: MigrationStatus,
    pub schema_version_before: u64,
    pub schema_version_after: u64,
    pub applied_at_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    pub actor_api_key_id: ApiKeyId,
    pub execution_log: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCommand {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    Permissive,
    Restrictive,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PolicyDefinition {
    pub name: String,
    pub table: String,
    pub kind: PolicyKind,
    pub command: PolicyCommand,
    pub roles: Vec<String>,
    pub using_expression: Option<String>,
    pub check_expression: Option<String>,
    pub enabled: bool,
    pub forced: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SchemaSnapshot {
    pub version: u64,
    pub tables: Vec<TableDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TableDefinition {
    pub name: String,
    pub sql: String,
    pub rls_enabled: bool,
    pub rls_forced: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct WorkerRequest {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub route: DatabaseRoute,
    pub mode: ExecutionMode,
    pub deadline_epoch_ms: i64,
    pub limits: ResourceLimits,
    pub expected_schema_version: Option<u64>,
    /// Stable identifier for durable replay protection of state-changing worker
    /// operations. It is derived from the tenant-scoped idempotency claim and is
    /// never selected from unverified request data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_receipt_id: Option<Uuid>,
    pub operation: WorkerOperation,
}

/// Durable, operation-level usage evidence emitted by the database worker.
/// `receipt_id` is deterministically equal to the UUID carried by `request_id`;
/// keeping both fields makes deduplication explicit to downstream metering.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct UsageReceipt {
    pub receipt_id: Uuid,
    pub request_id: RequestId,
    pub reads: u64,
    pub writes: u64,
    pub logical_database_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<UserId>,
    pub recorded_at_ms: i64,
}

/// Internal worker envelope. Public HTTP handlers unwrap `response` unchanged
/// and forward `usage` to the organization metering pipeline.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct WorkerExecution {
    pub response: WorkerResponse,
    pub usage: UsageReceipt,
    /// Internal execution diagnostics. These contain counts and timings only;
    /// SQL text and parameter values never cross the worker boundary here.
    #[serde(default)]
    pub statement_telemetry: Vec<WorkerStatementTelemetry>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct WorkerStatementTelemetry {
    pub ordinal: u16,
    pub duration_ms: f64,
    pub rows_returned: u64,
    pub rows_affected: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WorkerOperation {
    Query(QueryRequest),
    Transaction(TransactionRequest),
    ApplyMigration(MigrationSpec),
    RollbackMigration {
        migration_id: String,
    },
    MigrationHistory {
        limit: u32,
        before_version: Option<u64>,
    },
    Schema,
    Policies,
    Snapshot(SnapshotRequest),
    SyncPull(SyncPullRequest),
    SyncPush(SyncPushRequest),
    Backup {
        backup_id: BackupId,
    },
    Restore {
        backup_id: BackupId,
    },
    IntegrityCheck,
    StorageAuthorize(StorageAuthorizeRequest),
    StorageReserve(StorageReserveRequest),
    StorageCommit(StorageCommitRequest),
    StorageReceipt(StorageReceiptRequest),
    StorageUsage,
    StorageRelease(StorageReleaseRequest),
    StorageList(StorageListRequest),
    StorageCleanup {
        now_ms: i64,
    },
    StorageCleanupClaim(StorageCleanupClaimRequest),
    StorageCleanupAck(StorageCleanupAckRequest),
    StorageBuckets,
    StorageCreateBucket(StorageCreateBucketRequest),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WorkerResponse {
    Query(QueryResult),
    Transaction(Vec<QueryResult>),
    Migration(MigrationRecord),
    MigrationHistory(Vec<MigrationHistoryRecord>),
    Schema(SchemaSnapshot),
    Policies(Vec<PolicyDefinition>),
    Snapshot(SnapshotResponse),
    Sync(SyncPullResponse),
    SyncPush(SyncPushResponse),
    Backup(BackupResult),
    Restore(RestoreResult),
    Integrity(IntegrityResult),
    StorageAuthorization(StorageAuthorization),
    StorageReceipt(Option<StorageCommitReceipt>),
    StorageUsage(StorageUsageSnapshot),
    StorageObjects(StorageListResponse),
    StorageAck,
    StorageCleanup { removed: u64 },
    StorageCleanupBatch(StorageCleanupBatch),
    StorageCleanupAck { removed: u64, retried: u64 },
    StorageBuckets(Vec<StorageBucket>),
    StorageBucket(StorageBucket),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MigrationHistoryRecord {
    pub spec: MigrationSpec,
    pub status: MigrationStatus,
    pub schema_version_before: u64,
    pub schema_version_after: u64,
    pub applied_at_ms: i64,
    pub duration_ms: u64,
    pub actor_api_key_id: ApiKeyId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SnapshotRequest {
    pub tables: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SnapshotResponse {
    pub schema_version: u64,
    pub cursor: String,
    pub tables: BTreeMap<String, QueryResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SyncPullRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SyncPullResponse {
    pub changes: Vec<LogicalChange>,
    pub cursor: String,
    pub has_more: bool,
    pub control: Option<SyncControl>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LogicalChange {
    pub sequence: u64,
    pub transaction_id: TransactionId,
    pub table: String,
    pub primary_key: Value,
    pub operation: ChangeOperation,
    pub row_version: u64,
    pub values: Option<Map<String, Value>>,
    pub tombstone: Option<Value>,
    pub actor: Option<UserId>,
    pub schema_version: u64,
    pub committed_at_ms: i64,
    pub client_mutation_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOperation {
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncControl {
    ResnapshotRequired {
        reason: String,
        minimum_schema_version: u64,
    },
    InvalidateScope {
        scope_fingerprint: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SyncMutation {
    pub mutation_id: String,
    pub table: String,
    pub primary_key: Value,
    pub operation: ChangeOperation,
    pub values: Option<Map<String, Value>>,
    pub base_row_version: Option<u64>,
    pub client_timestamp_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SyncPushRequest {
    pub schema_version: u64,
    pub mutations: Vec<SyncMutation>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SyncMutationResult {
    pub mutation_id: String,
    pub status: MutationStatus,
    pub server_sequence: Option<u64>,
    pub row_version: Option<u64>,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStatus {
    Applied,
    Duplicate,
    Rejected,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SyncPushResponse {
    pub results: Vec<SyncMutationResult>,
    pub cursor: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct BackupResult {
    pub backup_id: BackupId,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RestoreResult {
    pub backup_id: BackupId,
    pub integrity_ok: bool,
    pub schema_version: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAction {
    Upload,
    Download,
    Delete,
    List,
    CreateMultipart,
    UploadPart,
    CompleteMultipart,
    AbortMultipart,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageAuthorizeRequest {
    pub bucket: String,
    pub object_key: String,
    pub action: StorageAction,
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageAuthorization {
    pub provider_key: String,
    pub scope_fingerprint: String,
    pub project_quota_bytes: u64,
    pub current_project_bytes: u64,
    pub max_object_bytes: u64,
    pub reservation_bytes: u64,
    pub replacement_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageReserveRequest {
    pub nonce: String,
    pub bytes: u64,
    pub expires_at_ms: i64,
    pub provider_key: SensitiveString,
    pub action: StorageAction,
    pub upload_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageReleaseRequest {
    pub nonce: String,
    pub reservation_bytes: u64,
    pub reservation_expires_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCommitRequest {
    pub bucket: String,
    pub object_key: String,
    pub provider_key: String,
    pub action: StorageAction,
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
    pub etag: Option<String>,
    pub version_id: Option<String>,
    pub reservation_nonce: String,
    pub reservation_bytes: u64,
    pub reservation_expires_at_ms: i64,
    pub replacement_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageReceiptRequest {
    pub bucket: String,
    pub object_key: String,
    pub provider_key: String,
    pub action: StorageAction,
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
    pub reservation_nonce: String,
    pub reservation_bytes: u64,
    pub reservation_expires_at_ms: i64,
    pub replacement_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCommitReceipt {
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub etag: Option<String>,
    pub version_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageUsageSnapshot {
    pub current_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCleanupClaimRequest {
    pub now_ms: i64,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCleanupBatch {
    pub removed_reservations: u64,
    pub items: Vec<StorageCleanupItem>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCleanupItem {
    pub id: String,
    pub provider_key: SensitiveString,
    pub action: StorageAction,
    pub upload_id: Option<String>,
    pub lease_token: SensitiveString,
    pub attempt: u32,
    pub lease_expires_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupOutcome {
    Deleted,
    Retry,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCleanupAckRequest {
    pub now_ms: i64,
    pub items: Vec<StorageCleanupDisposition>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCleanupDisposition {
    pub id: String,
    pub lease_token: SensitiveString,
    pub outcome: StorageCleanupOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageListRequest {
    pub bucket: String,
    pub prefix: String,
    pub limit: u32,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageObjectItem {
    pub id: String,
    pub object_key: String,
    pub owner_id: String,
    pub size_bytes: u64,
    pub content_type: Option<String>,
    pub checksum_sha256: Option<String>,
    pub etag: Option<String>,
    pub version_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageListResponse {
    pub items: Vec<StorageObjectItem>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageCreateBucketRequest {
    pub id: String,
    pub name: String,
    pub owner_id: Option<UserId>,
    pub public: bool,
    pub max_object_bytes: u64,
    pub project_quota_bytes: u64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StorageBucket {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub public: bool,
    pub max_object_bytes: u64,
    pub project_quota_bytes: u64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct IntegrityResult {
    pub ok: bool,
    pub messages: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ErrorEnvelope {
    pub error: PlatformError,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct PlatformError {
    pub code: String,
    pub message: String,
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl PlatformError {
    #[must_use]
    pub fn safe(
        code: impl Into<String>,
        message: impl Into<String>,
        request_id: RequestId,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            request_id,
            details: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProtocolValidationError {
    #[error("SQL must not be empty")]
    EmptySql,
    #[error("SQL exceeds the configured limit")]
    SqlTooLong,
    #[error("too many bound variables")]
    TooManyVariables,
    #[error("result row limit is invalid")]
    InvalidRowLimit,
    #[error("transaction statement count is invalid")]
    InvalidTransactionSize,
    #[error("integer is not a signed 64-bit decimal: {0}")]
    InvalidInteger(String),
    #[error("floating-point parameter must be finite")]
    NonFiniteReal,
    #[error("blob parameter is not valid base64")]
    InvalidBase64,
    #[error("resource limits are invalid")]
    InvalidResourceLimits,
    #[error("migration metadata is invalid")]
    InvalidMigrationMetadata,
    #[error("both up and down SQL are required")]
    MissingMigrationDirection,
    #[error("migration checksum does not match its contents")]
    ChecksumMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_json_integer_is_encoded_as_a_decimal_string() {
        let value = ResultCell::integer(i64::MAX);
        let encoded = serde_json::to_value(value).unwrap_or(Value::Null);
        assert_eq!(encoded, Value::String(i64::MAX.to_string()));
    }

    #[test]
    fn blob_has_an_explicit_wire_tag() {
        let encoded = serde_json::to_value(ResultCell::Blob(BlobValue::from_bytes(&[1, 2])))
            .unwrap_or(Value::Null);
        assert_eq!(encoded, serde_json::json!({"$blob": "AQI="}));
    }

    #[test]
    fn migration_checksum_covers_both_directions() {
        let mut migration = MigrationSpec {
            id: "001".into(),
            name: "documents".into(),
            up_sql: "create table documents(id)".into(),
            down_sql: "drop table documents".into(),
            checksum: String::new(),
            created_at_ms: 0,
        };
        migration.checksum = migration.calculate_checksum();
        assert!(migration.validate().is_ok());
        migration.down_sql.push(';');
        assert_eq!(
            migration.validate(),
            Err(ProtocolValidationError::ChecksumMismatch)
        );
    }

    #[test]
    fn query_limits_are_fail_closed() {
        let limits = ResourceLimits {
            max_sql_bytes: 3,
            ..ResourceLimits::default()
        };
        let request = QueryRequest {
            sql: "select 1".into(),
            parameters: Vec::new(),
            options: QueryOptions::default(),
        };
        assert_eq!(
            request.validate(&limits),
            Err(ProtocolValidationError::SqlTooLong)
        );
    }

    #[test]
    fn worker_execution_keeps_usage_outside_the_public_response_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_id = RequestId::new();
        let execution = WorkerExecution {
            response: WorkerResponse::Query(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                affected_rows: 1,
                last_insert_rowid: Some(7),
                truncated: false,
            }),
            usage: UsageReceipt {
                receipt_id: request_id.0,
                request_id,
                reads: 0,
                writes: 1,
                logical_database_bytes: 4096,
                subject: Some(UserId::new()),
                recorded_at_ms: 123,
            },
            statement_telemetry: Vec::new(),
        };
        let encoded = serde_json::to_value(&execution)?;
        assert!(encoded.get("response").is_some());
        assert!(encoded.get("usage").is_some());
        assert!(encoded["response"].get("usage").is_none());
        assert_eq!(
            encoded["usage"]["receipt_id"],
            encoded["usage"]["request_id"]
        );
        let decoded = serde_json::from_value::<WorkerExecution>(encoded)?;
        assert_eq!(decoded, execution);
        Ok(())
    }
}
