//! Hardened SQLite sessions with immutable auth context and no raw-connection API.

mod authorizer;
mod context;
mod limits;
mod storage;

pub use storage::{StorageCleanupDisposition, StorageProviderUploadBinding};
mod sync;
mod value;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use ffdb_sql_parser::{
    StatementKind, classify_statement, parse_rls_statement, rewrite_auth_functions_for_execution,
};
use ffdb_sqlite_rls::{
    ColumnSchema, CompiledRlsPlan, Compiler, RlsCatalog, SchemaSnapshot, TableSchema,
    backing_table_name, generated_source_names,
};
use rusqlite::{Connection, OpenFlags, config::DbConfig, limits::Limit};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use context::{AuthContext, DeveloperPrincipal, ExecutionMode};
use context::{ContextLease, InternalLease, PublicAuthLease, SharedContext};
pub use limits::{CancellationToken, ExecutionLimits};
pub use sync::{
    SyncChange, SyncChangeOperation, SyncMutation, SyncMutationOperation, SyncMutationReceipt,
    SyncPullResult, SyncSnapshot,
};
pub use value::{QueryResult, ResultColumn, ResultValue, SqlParameter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedDatabasePath(PathBuf);

impl TrustedDatabasePath {
    /// Constructs a database path from a trusted absolute root and an opaque UUID.
    pub fn for_database(root: &Path, database_id: &str) -> Result<Self, RuntimeError> {
        if !root.is_absolute()
            || database_id.len() != 36
            || !database_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            || database_id.contains("..")
        {
            return Err(RuntimeError::InvalidDatabaseRoute);
        }
        Ok(Self(root.join(format!("{database_id}.sqlite3"))))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub limits: ExecutionLimits,
    pub busy_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct RequestBudget {
    pub limits: ExecutionLimits,
    pub deadline: Instant,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            limits: ExecutionLimits::default(),
            busy_timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Debug)]
pub struct Database {
    connection: Mutex<Connection>,
    context: SharedContext,
    limits: ExecutionLimits,
    path: TrustedDatabasePath,
    poisoned: AtomicBool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatementRequest {
    pub sql: String,
    pub parameters: Vec<SqlParameter>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredMigration {
    pub id: String,
    pub name: String,
    pub checksum: Vec<u8>,
    pub up_sql: String,
    pub down_sql: String,
    pub created_at_ms: i64,
    pub applied_at_ms: i64,
    pub actor_id: String,
    pub duration_ms: i64,
    pub version_before: u64,
    pub version_after: u64,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreReceiptMarker {
    pub receipt_id: String,
    pub request_digest: Vec<u8>,
    pub backup_id: String,
    pub schema_version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerOperationReceipt {
    pub request_digest: Vec<u8>,
    pub operation: String,
    pub result_json: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredUsageReceipt {
    pub request_id: String,
    pub request_digest: Vec<u8>,
    pub response_json: Option<String>,
    pub reads: u64,
    pub writes: u64,
    pub logical_database_bytes: u64,
    pub subject: Option<String>,
    pub recorded_at_ms: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct UsageReceiptInsert<'a> {
    pub receipt_id: &'a str,
    pub request_id: &'a str,
    pub request_digest: &'a [u8],
    pub response_json: Option<&'a str>,
    pub reads: u64,
    pub writes: u64,
    pub subject: Option<&'a str>,
    pub recorded_at_ms: i64,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RuntimeError {
    #[error("database route is invalid")]
    InvalidDatabaseRoute,
    #[error("execution limits are invalid")]
    InvalidLimits,
    #[error("request context is already installed")]
    ContextAlreadyInstalled,
    #[error("database connection is poisoned")]
    Poisoned,
    #[error("SQL exceeds the configured length limit")]
    SqlTooLarge,
    #[error("statement contains too many bound variables")]
    TooManyVariables,
    #[error("statement kind is not allowed in this execution mode")]
    StatementNotAllowed,
    #[error("response exceeds the configured byte limit")]
    ResponseTooLarge,
    #[error("database exceeds its configured size limit")]
    DatabaseTooLarge,
    #[error("statement was cancelled")]
    Cancelled,
    #[error("statement deadline was exceeded")]
    DeadlineExceeded,
    #[error("constraint violation")]
    ConstraintViolation,
    #[error("sync cursor is invalid or expired")]
    SyncCursorInvalid,
    #[error("sync schema version does not match")]
    SyncSchemaMismatch,
    #[error("sync mutation is invalid")]
    SyncMutationInvalid,
    #[error("storage quota would be exceeded")]
    StorageQuotaExceeded,
    #[error("storage reservation already exists")]
    StorageReservationDuplicate,
    #[error("storage reservation does not match the authenticated generation")]
    StorageReservationMismatch,
    #[error("usage receipt belongs to another request")]
    UsageReceiptConflict,
    #[error("usage receipt is invalid")]
    UsageReceiptInvalid,
    #[error("database operation failed")]
    Database,
}

impl From<rusqlite::Error> for RuntimeError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Self::ConstraintViolation
            }
            rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ErrorCode::AuthorizationForStatementDenied =>
            {
                Self::StatementNotAllowed
            }
            rusqlite::Error::SqliteFailure(code, _) => {
                tracing::warn!(
                    sqlite_code = ?code.code,
                    sqlite_extended_code = code.extended_code,
                    "SQLite operation failed"
                );
                Self::Database
            }
            rusqlite::Error::UserFunctionError(_) => {
                tracing::warn!(
                    sqlite_error_kind = "user_function",
                    "SQLite operation failed"
                );
                Self::Database
            }
            rusqlite::Error::ExecuteReturnedResults => {
                tracing::warn!(
                    sqlite_error_kind = "execute_returned_results",
                    "SQLite operation failed"
                );
                Self::Database
            }
            _ => {
                tracing::warn!(sqlite_error_kind = "other", "SQLite operation failed");
                Self::Database
            }
        }
    }
}

fn ensure_internal_columns(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str)],
) -> Result<(), RuntimeError> {
    for (column, declaration) in expected {
        if !matches!(
            (table, *column, *declaration),
            ("__ffdb_storage_reservations", "provider_key", "TEXT")
                | (
                    "__ffdb_usage_receipts",
                    "transport_request_id",
                    "TEXT NOT NULL DEFAULT ''"
                )
                | ("__ffdb_storage_reservations", "action", "TEXT")
                | ("__ffdb_storage_reservations", "upload_id", "TEXT")
                | (
                    "__ffdb_storage_provider_uploads",
                    "reserved_bytes",
                    "INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes >= 0)"
                )
                | (
                    "__ffdb_storage_provider_uploads",
                    "replacement_fingerprint",
                    "TEXT"
                )
        ) {
            return Err(RuntimeError::Database);
        }
    }
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    let existing = columns.collect::<Result<std::collections::HashSet<_>, _>>()?;
    drop(statement);
    for (column, declaration) in expected {
        if !existing.contains(*column) {
            connection.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
            ))?;
        }
    }
    Ok(())
}

impl Database {
    pub fn open(path: TrustedDatabasePath, config: RuntimeConfig) -> Result<Self, RuntimeError> {
        if !config.limits.validate() {
            return Err(RuntimeError::InvalidLimits);
        }
        let connection = Connection::open_with_flags(
            path.as_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(config.busy_timeout)?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; \
             PRAGMA trusted_schema=OFF; PRAGMA recursive_triggers=ON; \
             CREATE TABLE IF NOT EXISTS __ffdb_rls_catalog \
             (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), catalog_json TEXT NOT NULL); \
             CREATE TABLE IF NOT EXISTS __ffdb_schema_state \
             (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), schema_version INTEGER NOT NULL); \
             INSERT OR IGNORE INTO __ffdb_schema_state(singleton, schema_version) VALUES (1, 0); \
             CREATE TABLE IF NOT EXISTS __ffdb_migrations \
             (id TEXT PRIMARY KEY, name TEXT NOT NULL, checksum BLOB NOT NULL, up_sql TEXT NOT NULL, \
              down_sql TEXT NOT NULL, created_at_ms INTEGER NOT NULL, applied_at_ms INTEGER NOT NULL, \
              actor_id TEXT NOT NULL, duration_ms INTEGER NOT NULL, version_before INTEGER NOT NULL, \
              version_after INTEGER NOT NULL, status TEXT NOT NULL CHECK(status IN ('applied','rolled_back'))); \
             CREATE TABLE IF NOT EXISTS __ffdb_restore_receipt \
             (singleton INTEGER PRIMARY KEY CHECK(singleton=1), receipt_id TEXT NOT NULL, \
              request_digest BLOB NOT NULL, backup_id TEXT NOT NULL, schema_version INTEGER NOT NULL); \
             CREATE TABLE IF NOT EXISTS __ffdb_worker_operation_receipts \
             (receipt_id TEXT PRIMARY KEY, request_digest BLOB NOT NULL, operation TEXT NOT NULL, \
              result_json TEXT, recorded_at_ms INTEGER NOT NULL); \
             CREATE TABLE IF NOT EXISTS __ffdb_usage_receipts \
             (request_id TEXT PRIMARY KEY, transport_request_id TEXT NOT NULL, \
              request_digest BLOB NOT NULL, response_json TEXT, \
              reads INTEGER NOT NULL CHECK(reads >= 0), writes INTEGER NOT NULL CHECK(writes >= 0), \
              logical_database_bytes INTEGER NOT NULL CHECK(logical_database_bytes >= 0), \
              subject TEXT, recorded_at_ms INTEGER NOT NULL); \
             CREATE TABLE IF NOT EXISTS __ffdb_sync_state \
             (singleton INTEGER PRIMARY KEY CHECK(singleton=1), next_sequence INTEGER NOT NULL, \
              minimum_sequence INTEGER NOT NULL, cursor_secret BLOB NOT NULL); \
             INSERT OR IGNORE INTO __ffdb_sync_state(singleton,next_sequence,minimum_sequence,cursor_secret) \
             VALUES (1,1,0,randomblob(32)); \
             CREATE TABLE IF NOT EXISTS __ffdb_sync_changes \
             (sequence INTEGER PRIMARY KEY, transaction_id TEXT NOT NULL, table_name TEXT NOT NULL, \
              primary_key_json TEXT NOT NULL, operation TEXT NOT NULL CHECK(operation IN ('insert','update','delete')), \
              row_version INTEGER NOT NULL, values_json TEXT, tombstone_json TEXT, actor TEXT NOT NULL, \
              schema_version INTEGER NOT NULL, committed_at_ms INTEGER NOT NULL, client_mutation_id TEXT); \
             CREATE TABLE IF NOT EXISTS __ffdb_sync_versions \
             (table_name TEXT NOT NULL, primary_key_json TEXT NOT NULL, row_version INTEGER NOT NULL, \
              last_sequence INTEGER NOT NULL, deleted INTEGER NOT NULL, \
              PRIMARY KEY(table_name,primary_key_json)); \
             CREATE TABLE IF NOT EXISTS __ffdb_sync_mutations \
             (subject TEXT NOT NULL, client_id TEXT NOT NULL, mutation_id TEXT NOT NULL, \
              payload_hash BLOB NOT NULL, sequence INTEGER NOT NULL, row_version INTEGER NOT NULL, \
              PRIMARY KEY(subject,client_id,mutation_id)); \
             CREATE TABLE IF NOT EXISTS storage_buckets \
             (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, owner_id TEXT NOT NULL, \
              public INTEGER NOT NULL DEFAULT 0 CHECK(public IN (0,1)), \
              max_object_bytes INTEGER NOT NULL DEFAULT 52428800 CHECK(max_object_bytes >= 0), \
              project_quota_bytes INTEGER NOT NULL DEFAULT 1000000000 CHECK(project_quota_bytes >= 0), \
              created_at_ms INTEGER NOT NULL); \
             CREATE TABLE IF NOT EXISTS storage_objects \
             (id TEXT PRIMARY KEY, bucket_id TEXT NOT NULL, object_key TEXT NOT NULL, \
              owner_id TEXT NOT NULL, size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0), content_type TEXT, \
              checksum_sha256 TEXT, etag TEXT, version_id TEXT, created_at_ms INTEGER NOT NULL, \
              updated_at_ms INTEGER NOT NULL, UNIQUE(bucket_id,object_key), \
              FOREIGN KEY(bucket_id) REFERENCES storage_buckets(id) ON DELETE CASCADE); \
             CREATE TABLE IF NOT EXISTS storage_uploads \
             (id TEXT PRIMARY KEY, bucket_id TEXT NOT NULL, object_key TEXT NOT NULL, \
              owner_id TEXT NOT NULL, expected_size_bytes INTEGER CHECK(expected_size_bytes >= 0), checksum_sha256 TEXT, \
              content_type TEXT, status TEXT NOT NULL, created_at_ms INTEGER NOT NULL, \
              expires_at_ms INTEGER NOT NULL, \
              FOREIGN KEY(bucket_id) REFERENCES storage_buckets(id) ON DELETE CASCADE); \
             CREATE TABLE IF NOT EXISTS storage_versions \
             (id TEXT PRIMARY KEY, object_id TEXT NOT NULL, owner_id TEXT NOT NULL, \
              size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0), checksum_sha256 TEXT, etag TEXT, \
              provider_version_id TEXT, created_at_ms INTEGER NOT NULL, \
              FOREIGN KEY(object_id) REFERENCES storage_objects(id) ON DELETE CASCADE); \
             CREATE TABLE IF NOT EXISTS __ffdb_storage_provider_objects \
             (object_id TEXT PRIMARY KEY, provider_key TEXT NOT NULL UNIQUE); \
             CREATE TABLE IF NOT EXISTS __ffdb_storage_reservations \
             (project_id TEXT NOT NULL, nonce TEXT NOT NULL, subject TEXT NOT NULL, \
              token_id TEXT NOT NULL, bytes INTEGER NOT NULL CHECK(bytes >= 0), \
              expires_at_ms INTEGER NOT NULL, provider_key TEXT NOT NULL, action TEXT NOT NULL, \
              upload_id TEXT, PRIMARY KEY(project_id,nonce)); \
             CREATE TABLE IF NOT EXISTS __ffdb_storage_provider_uploads \
             (upload_id TEXT PRIMARY KEY, provider_key TEXT NOT NULL UNIQUE, \
              reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes >= 0), \
              replacement_fingerprint TEXT); \
             CREATE TABLE IF NOT EXISTS __ffdb_storage_upload_parts \
             (upload_id TEXT NOT NULL, part_number INTEGER NOT NULL CHECK(part_number BETWEEN 1 AND 10000), \
              size_bytes INTEGER CHECK(size_bytes >= 0), checksum_sha256 TEXT, etag TEXT, \
              PRIMARY KEY(upload_id,part_number)); \
             CREATE TABLE IF NOT EXISTS __ffdb_storage_commit_receipts \
             (project_id TEXT NOT NULL, nonce TEXT NOT NULL, subject TEXT NOT NULL, token_id TEXT NOT NULL, \
              binding_digest BLOB NOT NULL, commit_digest BLOB NOT NULL, result_json TEXT NOT NULL, \
              committed_at_ms INTEGER NOT NULL, expires_at_ms INTEGER NOT NULL, \
              PRIMARY KEY(project_id,nonce)); \
             CREATE TABLE IF NOT EXISTS __ffdb_storage_provider_cleanup \
             (id TEXT PRIMARY KEY, provider_key TEXT NOT NULL, action TEXT NOT NULL, upload_id TEXT, \
              created_at_ms INTEGER NOT NULL, available_at_ms INTEGER NOT NULL, \
              lease_token TEXT, lease_expires_at_ms INTEGER, \
              attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0));",
        )?;
        ensure_internal_columns(
            &connection,
            "__ffdb_usage_receipts",
            &[("transport_request_id", "TEXT NOT NULL DEFAULT ''")],
        )?;
        ensure_internal_columns(
            &connection,
            "__ffdb_storage_reservations",
            &[
                ("provider_key", "TEXT"),
                ("action", "TEXT"),
                ("upload_id", "TEXT"),
            ],
        )?;
        ensure_internal_columns(
            &connection,
            "__ffdb_storage_provider_uploads",
            &[
                (
                    "reserved_bytes",
                    "INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes >= 0)",
                ),
                ("replacement_fingerprint", "TEXT"),
            ],
        )?;
        // The connection also executes fixed trusted catalog/capture SQL. Caller SQL is
        // validated against the tighter configured bounds before prepare.
        let max_variables = i32::try_from(config.limits.max_variables.max(64)).unwrap_or(i32::MAX);
        let max_sql = i32::try_from(config.limits.max_sql_bytes.max(1_048_576)).unwrap_or(i32::MAX);
        connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, max_variables);
        connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, max_sql);
        connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
        connection.set_limit(Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32);
        connection.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 1_000);

        let mut context_state = context::ContextState::default();
        if let Ok(json) = connection.query_row(
            "SELECT catalog_json FROM __ffdb_rls_catalog WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        ) && let Ok(catalog) = serde_json::from_str::<RlsCatalog>(&json)
        {
            for table in catalog.tables().keys() {
                if let Ok(sources) = generated_source_names(table) {
                    context_state.approved_sources.extend(
                        sources
                            .into_iter()
                            .map(|source| source.as_str().to_ascii_lowercase()),
                    );
                }
            }
        }
        let context = Arc::new(Mutex::new(context_state));
        context::install_auth_functions(&connection, &context)?;
        authorizer::install(&connection, &context);
        let database = Self {
            connection: Mutex::new(connection),
            context,
            limits: config.limits,
            path,
            poisoned: AtomicBool::new(false),
        };
        database.with_context(
            ExecutionMode::Developer(DeveloperPrincipal {
                actor_id: "ffdb-bootstrap".to_owned(),
                api_key_id: "ffdb-bootstrap".to_owned(),
            }),
            &CancellationToken::default(),
            |session| {
                let catalog = session.bootstrap_storage_rls()?;
                session.refresh_change_capture(&catalog)
            },
        )?;
        Ok(database)
    }

    pub fn with_context<T>(
        &self,
        mode: ExecutionMode,
        cancellation: &CancellationToken,
        callback: impl FnOnce(&mut Session<'_>) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        self.with_context_budget(
            mode,
            cancellation,
            &RequestBudget {
                limits: self.limits.clone(),
                deadline: Instant::now()
                    .checked_add(self.limits.transaction_timeout)
                    .ok_or(RuntimeError::DeadlineExceeded)?,
            },
            callback,
        )
    }

    pub fn with_context_budget<T>(
        &self,
        mode: ExecutionMode,
        cancellation: &CancellationToken,
        budget: &RequestBudget,
        callback: impl FnOnce(&mut Session<'_>) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(RuntimeError::Poisoned);
        }
        if !budget.limits.validate() {
            return Err(RuntimeError::InvalidLimits);
        }
        if budget.deadline <= Instant::now() {
            return Err(RuntimeError::DeadlineExceeded);
        }
        let effective_limits = self.limits.restricted_by(&budget.limits);
        let mut connection = self.connection.lock().map_err(|_| {
            self.poisoned.store(true, Ordering::Release);
            RuntimeError::Poisoned
        })?;
        let lease = ContextLease::install(&self.context, mode.clone())?;
        let mut session = Session {
            connection: &mut connection,
            context: Arc::clone(&self.context),
            mode,
            limits: &effective_limits,
            cancellation,
            transaction_deadline: None,
            request_deadline: budget.deadline,
        };
        let result = callback(&mut session);
        drop(session);
        drop(lease);
        let clean = self.context.lock().is_ok_and(|state| {
            state.active.is_none() && state.internal_depth == 0 && state.public_auth_depth == 0
        });
        if !clean {
            self.poisoned.store(true, Ordering::Release);
            return Err(RuntimeError::Poisoned);
        }
        result
    }

    pub fn backup_to(&self, destination: &TrustedDatabasePath) -> Result<(), RuntimeError> {
        self.backup_to_bounded(
            destination,
            &CancellationToken::default(),
            Instant::now()
                .checked_add(Duration::from_secs(24 * 60 * 60))
                .ok_or(RuntimeError::DeadlineExceeded)?,
        )
    }

    pub fn backup_to_bounded(
        &self,
        destination: &TrustedDatabasePath,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        let source = self.connection.lock().map_err(|_| RuntimeError::Poisoned)?;
        let mut destination = Connection::open(destination.as_path())?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
        loop {
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(RuntimeError::DeadlineExceeded);
            }
            match backup.step(128)? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => {}
                rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return Err(RuntimeError::Database),
            }
        }
        drop(backup);
        let token = cancellation.clone();
        destination.progress_handler(
            self.limits.progress_ops,
            Some(move || token.is_cancelled() || Instant::now() >= deadline),
        );
        let integrity =
            destination.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0));
        destination.progress_handler(0, None::<fn() -> bool>);
        match integrity {
            Ok(result) if result == "ok" => Ok(()),
            Ok(_) => Err(RuntimeError::Database),
            Err(_) if cancellation.is_cancelled() => Err(RuntimeError::Cancelled),
            Err(_) if Instant::now() >= deadline => Err(RuntimeError::DeadlineExceeded),
            Err(error) => Err(error.into()),
        }
    }

    pub fn restore_from_bounded(
        &self,
        source: &TrustedDatabasePath,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(RuntimeError::DeadlineExceeded);
        }
        let source = Connection::open_with_flags(
            source.as_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let token = cancellation.clone();
        source.progress_handler(
            self.limits.progress_ops,
            Some(move || token.is_cancelled() || Instant::now() >= deadline),
        );
        let integrity =
            source.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0));
        source.progress_handler(0, None::<fn() -> bool>);
        match integrity {
            Ok(result) if result == "ok" => {}
            Ok(_) => return Err(RuntimeError::Database),
            Err(_) if cancellation.is_cancelled() => return Err(RuntimeError::Cancelled),
            Err(_) if Instant::now() >= deadline => return Err(RuntimeError::DeadlineExceeded),
            Err(error) => return Err(error.into()),
        }
        {
            let mut destination = self.connection.lock().map_err(|_| RuntimeError::Poisoned)?;
            let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
            loop {
                if cancellation.is_cancelled() {
                    return Err(RuntimeError::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(RuntimeError::DeadlineExceeded);
                }
                match backup.step(128)? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More => {}
                    rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => return Err(RuntimeError::Database),
                }
            }
        }
        self.with_context(
            ExecutionMode::Developer(DeveloperPrincipal {
                actor_id: "ffdb-restore".to_owned(),
                api_key_id: "ffdb-restore".to_owned(),
            }),
            cancellation,
            |session| {
                let catalog = session.bootstrap_storage_rls()?;
                session.refresh_change_capture(&catalog)
            },
        )?;
        if self.integrity_check_bounded(cancellation, deadline)? {
            Ok(())
        } else {
            Err(RuntimeError::Database)
        }
    }

    /// Installs a singleton receipt marker into the trusted plaintext restore
    /// image. SQLite's backup transaction copies this marker with the restored
    /// state, allowing a retry to distinguish a completed restore from one that
    /// never committed without replaying over intervening writes.
    pub fn prepare_restore_receipt(
        source: &TrustedDatabasePath,
        receipt_id: &str,
        request_digest: &[u8],
        backup_id: &str,
    ) -> Result<u64, RuntimeError> {
        let mut connection = Connection::open_with_flags(
            source.as_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS __ffdb_restore_receipt \
             (singleton INTEGER PRIMARY KEY CHECK(singleton=1), receipt_id TEXT NOT NULL, \
              request_digest BLOB NOT NULL, backup_id TEXT NOT NULL, schema_version INTEGER NOT NULL)",
        )?;
        let schema_version: u64 = transaction.query_row(
            "SELECT schema_version FROM __ffdb_schema_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO __ffdb_restore_receipt \
             (singleton,receipt_id,request_digest,backup_id,schema_version) VALUES (1,?1,?2,?3,?4) \
             ON CONFLICT(singleton) DO UPDATE SET receipt_id=excluded.receipt_id, \
             request_digest=excluded.request_digest,backup_id=excluded.backup_id, \
             schema_version=excluded.schema_version",
            rusqlite::params![receipt_id, request_digest, backup_id, schema_version],
        )?;
        transaction.commit()?;
        Ok(schema_version)
    }

    pub fn restore_receipt(&self) -> Result<Option<RestoreReceiptMarker>, RuntimeError> {
        let connection = self.connection.lock().map_err(|_| RuntimeError::Poisoned)?;
        let _internal = InternalLease::enter(&self.context)?;
        let result = connection.query_row(
            "SELECT receipt_id,request_digest,backup_id,schema_version \
             FROM __ffdb_restore_receipt WHERE singleton=1",
            [],
            |row| {
                Ok(RestoreReceiptMarker {
                    receipt_id: row.get(0)?,
                    request_digest: row.get(1)?,
                    backup_id: row.get(2)?,
                    schema_version: row.get(3)?,
                })
            },
        );
        match result {
            Ok(marker) => Ok(Some(marker)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn worker_operation_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<WorkerOperationReceipt>, RuntimeError> {
        let connection = self.connection.lock().map_err(|_| RuntimeError::Poisoned)?;
        let _internal = InternalLease::enter(&self.context)?;
        let result = connection.query_row(
            "SELECT request_digest,operation,result_json \
             FROM __ffdb_worker_operation_receipts WHERE receipt_id=?1",
            [receipt_id],
            |row| {
                Ok(WorkerOperationReceipt {
                    request_digest: row.get(0)?,
                    operation: row.get(1)?,
                    result_json: row.get(2)?,
                })
            },
        );
        match result {
            Ok(receipt) => Ok(Some(receipt)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn start_worker_operation_receipt(
        &self,
        receipt_id: &str,
        request_digest: &[u8],
        operation: &str,
        recorded_at_ms: i64,
    ) -> Result<(), RuntimeError> {
        let mut connection = self.connection.lock().map_err(|_| RuntimeError::Poisoned)?;
        let _internal = InternalLease::enter(&self.context)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO __ffdb_worker_operation_receipts \
             (receipt_id,request_digest,operation,result_json,recorded_at_ms) \
             VALUES (?1,?2,?3,NULL,?4) ON CONFLICT(receipt_id) DO NOTHING",
            rusqlite::params![receipt_id, request_digest, operation, recorded_at_ms],
        )?;
        // The filesystem receipt is retained for 48 hours. Keeping transaction
        // markers longer means a live Started receipt is never made ambiguous by
        // this bounded, age-based cleanup.
        transaction.execute(
            "DELETE FROM __ffdb_worker_operation_receipts WHERE recorded_at_ms<?1",
            [recorded_at_ms.saturating_sub(72 * 60 * 60 * 1_000)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn integrity_check(&self) -> Result<bool, RuntimeError> {
        self.integrity_check_bounded(
            &CancellationToken::default(),
            Instant::now()
                .checked_add(Duration::from_secs(24 * 60 * 60))
                .ok_or(RuntimeError::DeadlineExceeded)?,
        )
    }

    pub fn integrity_check_bounded(
        &self,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<bool, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(RuntimeError::DeadlineExceeded);
        }
        let connection = self.connection.lock().map_err(|_| RuntimeError::Poisoned)?;
        let _internal = InternalLease::enter(&self.context)?;
        let token = cancellation.clone();
        connection.progress_handler(
            self.limits.progress_ops,
            Some(move || token.is_cancelled() || Instant::now() >= deadline),
        );
        let result =
            connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0));
        connection.progress_handler(0, None::<fn() -> bool>);
        match result {
            Ok(result) => Ok(result == "ok"),
            Err(_) if cancellation.is_cancelled() => Err(RuntimeError::Cancelled),
            Err(_) if Instant::now() >= deadline => Err(RuntimeError::DeadlineExceeded),
            Err(error) => Err(error.into()),
        }
    }

    #[must_use]
    pub fn path(&self) -> &TrustedDatabasePath {
        &self.path
    }
}

pub struct Session<'a> {
    connection: &'a mut Connection,
    context: SharedContext,
    mode: ExecutionMode,
    limits: &'a ExecutionLimits,
    cancellation: &'a CancellationToken,
    transaction_deadline: Option<Instant>,
    request_deadline: Instant,
}

impl std::fmt::Debug for Session<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session<'_> {
    fn bootstrap_storage_rls(&mut self) -> Result<RlsCatalog, RuntimeError> {
        const STORAGE_TABLES: [&str; 4] = [
            "storage_buckets",
            "storage_objects",
            "storage_uploads",
            "storage_versions",
        ];
        let mut catalog = self.load_rls_catalog()?;
        let mut missing = Vec::new();
        for table in STORAGE_TABLES {
            let identifier =
                ffdb_sql_parser::Identifier::new(table).map_err(|_| RuntimeError::Database)?;
            if !catalog.tables().contains_key(&identifier) {
                missing.push(identifier);
            }
        }
        if !missing.is_empty() {
            // Enabling RLS mutates only the policy catalog, not the physical
            // SQLite schema. Reuse one trusted snapshot for all missing storage
            // tables instead of re-reading sqlite_schema once per table.
            let schema = self.schema_snapshot(&catalog)?;
            for identifier in missing {
                let statement = parse_rls_statement(&format!(
                    "ALTER TABLE {} ENABLE ROW LEVEL SECURITY",
                    identifier.quoted()
                ))
                .map_err(|_| RuntimeError::Database)?;
                catalog
                    .apply(&schema, statement)
                    .map_err(|_| RuntimeError::Database)?;
            }
            let schema = self.schema_snapshot(&catalog)?;
            let plan = Compiler
                .compile(&schema, &catalog)
                .map_err(|_| RuntimeError::Database)?;
            self.apply_rls_plan(&plan)?;
            self.store_rls_catalog(&catalog)?;
        }
        Ok(catalog)
    }

    pub fn execute(&mut self, request: &StatementRequest) -> Result<QueryResult, RuntimeError> {
        self.validate(request)?;
        let deadline = Instant::now()
            .checked_add(self.limits.statement_timeout)
            .ok_or(RuntimeError::DeadlineExceeded)?;
        let deadline = self
            .transaction_deadline
            .map_or(deadline, |transaction| transaction.min(deadline))
            .min(self.request_deadline);
        self.execute_bounded(request, deadline)
    }

    /// Executes one data mutation through the caller's RLS context and always rolls it back.
    /// Used by trusted services to authorize a later external side effect without persisting a
    /// speculative metadata row.
    pub fn probe_write(&mut self, request: &StatementRequest) -> Result<(), RuntimeError> {
        let class =
            classify_statement(&request.sql).map_err(|_| RuntimeError::StatementNotAllowed)?;
        if !matches!(
            class.kind,
            StatementKind::Insert | StatementKind::Update | StatementKind::Delete
        ) {
            return Err(RuntimeError::StatementNotAllowed);
        }
        self.connection
            .execute_batch("SAVEPOINT __ffdb_write_probe")?;
        let result = self.execute(request).map(|_| ());
        let rollback = self
            .connection
            .execute_batch("ROLLBACK TO __ffdb_write_probe; RELEASE __ffdb_write_probe");
        match (result, rollback) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error.into()),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub fn transaction(
        &mut self,
        requests: &[StatementRequest],
    ) -> Result<Vec<QueryResult>, RuntimeError> {
        self.atomic(|session| session.transaction_in_current_atomic(requests))
    }

    /// Executes a bounded SQL transaction inside an already-open `atomic`
    /// block. This lets the worker append its usage receipt before the same
    /// SQLite commit without exposing the raw connection.
    pub fn transaction_in_current_atomic(
        &mut self,
        requests: &[StatementRequest],
    ) -> Result<Vec<QueryResult>, RuntimeError> {
        self.transaction_in_current_atomic_observed(requests)
            .map(|(results, _)| results)
    }

    /// Executes a bounded transaction and returns one precise execution time per
    /// statement. The timings contain no SQL text and are intended for the
    /// privacy-safe operator telemetry pipeline.
    pub fn transaction_in_current_atomic_observed(
        &mut self,
        requests: &[StatementRequest],
    ) -> Result<(Vec<QueryResult>, Vec<Duration>), RuntimeError> {
        if self.transaction_deadline.is_none() || requests.is_empty() || requests.len() > 100 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let mut results = Vec::with_capacity(requests.len());
        let mut durations = Vec::with_capacity(requests.len());
        let mut response_bytes = 0_usize;
        for request in requests {
            let started = Instant::now();
            let result = self.execute(request)?;
            durations.push(started.elapsed());
            response_bytes = response_bytes.saturating_add(result.encoded_size());
            if response_bytes > self.limits.max_response_bytes {
                return Err(RuntimeError::ResponseTooLarge);
            }
            results.push(result);
        }
        Ok((results, durations))
    }

    pub fn atomic<T>(
        &mut self,
        callback: impl FnOnce(&mut Self) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        if self.transaction_deadline.is_some() {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let deadline = Instant::now()
            .checked_add(self.limits.transaction_timeout)
            .ok_or(RuntimeError::DeadlineExceeded)?
            .min(self.request_deadline);
        let approved_before = self
            .context
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .approved_sources
            .clone();
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        self.transaction_deadline = Some(deadline);
        let result = callback(self);
        match result {
            Ok(value) => {
                if let Err(error) = self.check_database_size() {
                    let _ = self.connection.execute_batch("ROLLBACK");
                    self.restore_approved_sources(approved_before)?;
                    self.transaction_deadline = None;
                    return Err(error);
                }
                if let Err(error) = self.connection.execute_batch("COMMIT") {
                    let _ = self.connection.execute_batch("ROLLBACK");
                    self.restore_approved_sources(approved_before)?;
                    self.transaction_deadline = None;
                    return Err(error.into());
                }
                self.transaction_deadline = None;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                self.restore_approved_sources(approved_before)?;
                self.transaction_deadline = None;
                Err(error)
            }
        }
    }

    pub fn apply_rls_plan(&mut self, plan: &CompiledRlsPlan) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute_batch("SAVEPOINT __ffdb_rls_plan")?;
        let apply_result = (|| {
            for table in plan.tables() {
                if let Some(rename) = table.rename_sql() {
                    self.connection.execute_batch(rename)?;
                }
                for sql in table.drop_generated_sql() {
                    self.connection.execute_batch(sql)?;
                }
                for sql in table.create_generated_sql() {
                    self.connection.execute_batch(sql)?;
                }
            }
            Ok::<(), rusqlite::Error>(())
        })();
        if let Err(error) = apply_result {
            let _ = self.connection.execute_batch("ROLLBACK TO __ffdb_rls_plan");
            let _ = self.connection.execute_batch("RELEASE __ffdb_rls_plan");
            return Err(error.into());
        }
        self.connection.execute_batch("RELEASE __ffdb_rls_plan")?;
        let mut context = self.context.lock().map_err(|_| RuntimeError::Poisoned)?;
        for table in plan.tables() {
            context.approved_sources.extend(
                table
                    .generated_sources()
                    .iter()
                    .map(|source| source.as_str().to_ascii_lowercase()),
            );
        }
        Ok(())
    }

    pub fn load_rls_catalog(&mut self) -> Result<RlsCatalog, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let result = self.connection.query_row(
            "SELECT catalog_json FROM __ffdb_rls_catalog WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(json) => serde_json::from_str(&json).map_err(|_| RuntimeError::Database),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(RlsCatalog::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn store_rls_catalog(&mut self, catalog: &RlsCatalog) -> Result<(), RuntimeError> {
        let json = serde_json::to_string(catalog).map_err(|_| RuntimeError::Database)?;
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_rls_catalog(singleton, catalog_json) VALUES (1, ?1) \
             ON CONFLICT(singleton) DO UPDATE SET catalog_json=excluded.catalog_json",
            [json],
        )?;
        Ok(())
    }

    pub fn schema_snapshot(
        &mut self,
        catalog: &RlsCatalog,
    ) -> Result<SchemaSnapshot, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let mut names = Vec::<(String, bool)>::new();
        {
            let mut statement = self.connection.prepare(
                "SELECT name FROM sqlite_schema WHERE type = 'table' \
                 AND name NOT LIKE '__ffdb_%' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                names.push((row?, false));
            }
        }
        for table in catalog.tables().keys() {
            let backing = backing_table_name(table).map_err(|_| RuntimeError::Database)?;
            let exists: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [backing.as_str()],
                |row| row.get(0),
            )?;
            if exists && !names.iter().any(|(name, _)| name == table.as_str()) {
                names.push((table.as_str().to_owned(), true));
            }
        }
        let mut tables = Vec::with_capacity(names.len());
        for (logical_name, protected) in names {
            let logical = ffdb_sql_parser::Identifier::new(logical_name)
                .map_err(|_| RuntimeError::Database)?;
            let physical = if protected {
                backing_table_name(&logical).map_err(|_| RuntimeError::Database)?
            } else {
                logical.clone()
            };
            let pragma = format!("PRAGMA table_xinfo({})", physical.quoted());
            let mut statement = self.connection.prepare(&pragma)?;
            let rows = statement.query_map([], |row| {
                let name: String = row.get(1)?;
                let primary_key: u32 = row.get(5)?;
                let hidden: u32 = row.get(6)?;
                Ok((name, primary_key, hidden))
            })?;
            let mut columns = Vec::new();
            for row in rows {
                let (name, primary_key, hidden) = row?;
                columns.push(ColumnSchema {
                    name: ffdb_sql_parser::Identifier::new(name)
                        .map_err(|_| RuntimeError::Database)?,
                    primary_key_ordinal: (primary_key > 0).then_some(primary_key),
                    generated: matches!(hidden, 2 | 3),
                });
            }
            tables.push(TableSchema {
                name: logical,
                columns,
                protected,
            });
        }
        tables.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(SchemaSnapshot { tables })
    }

    pub fn logical_schema_sql(
        &mut self,
        catalog: &RlsCatalog,
    ) -> Result<BTreeMap<String, String>, RuntimeError> {
        let snapshot = self.schema_snapshot(catalog)?;
        let _internal = InternalLease::enter(&self.context)?;
        let mut definitions = BTreeMap::new();
        for table in snapshot.tables {
            let physical = if table.protected {
                backing_table_name(&table.name).map_err(|_| RuntimeError::Database)?
            } else {
                table.name.clone()
            };
            let sql: String = self.connection.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
                [physical.as_str()],
                |row| row.get(0),
            )?;
            let logical_sql = if table.protected {
                let suffix = sql.find('(').map_or("", |offset| &sql[offset..]);
                format!("CREATE TABLE {}{suffix}", table.name.quoted())
            } else {
                sql
            };
            let logical_sql = rewrite_logical_schema_names(logical_sql, catalog)?;
            definitions.insert(table.name.as_str().to_owned(), logical_sql);
        }
        Ok(definitions)
    }

    pub fn schema_version(&mut self) -> Result<u64, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT schema_version FROM __ffdb_schema_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn set_schema_version(&mut self, version: u64) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "UPDATE __ffdb_schema_state SET schema_version=?1 WHERE singleton=1",
            [version],
        )?;
        Ok(())
    }

    pub fn migration_record(&mut self, id: &str) -> Result<Option<StoredMigration>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let result = self.connection.query_row(
            "SELECT id,name,checksum,up_sql,down_sql,created_at_ms,applied_at_ms,actor_id,\
             duration_ms,version_before,version_after,status FROM __ffdb_migrations WHERE id=?1",
            [id],
            |row| {
                Ok(StoredMigration {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    checksum: row.get(2)?,
                    up_sql: row.get(3)?,
                    down_sql: row.get(4)?,
                    created_at_ms: row.get(5)?,
                    applied_at_ms: row.get(6)?,
                    actor_id: row.get(7)?,
                    duration_ms: row.get(8)?,
                    version_before: row.get(9)?,
                    version_after: row.get(10)?,
                    status: row.get(11)?,
                })
            },
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn migration_history(
        &mut self,
        limit: usize,
        before_version: Option<u64>,
    ) -> Result<Vec<StoredMigration>, RuntimeError> {
        if limit == 0 || limit > 1_000 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let before_version = before_version.map_or(i64::MAX, |version| {
            i64::try_from(version).unwrap_or(i64::MAX)
        });
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let _internal = InternalLease::enter(&self.context)?;
        let mut statement = self.connection.prepare(
            "SELECT id,name,checksum,up_sql,down_sql,created_at_ms,applied_at_ms,actor_id,\
             duration_ms,version_before,version_after,status FROM __ffdb_migrations \
             WHERE version_after<?1 ORDER BY version_after DESC,id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(rusqlite::params![before_version, limit], |row| {
            Ok(StoredMigration {
                id: row.get(0)?,
                name: row.get(1)?,
                checksum: row.get(2)?,
                up_sql: row.get(3)?,
                down_sql: row.get(4)?,
                created_at_ms: row.get(5)?,
                applied_at_ms: row.get(6)?,
                actor_id: row.get(7)?,
                duration_ms: row.get(8)?,
                version_before: row.get(9)?,
                version_after: row.get(10)?,
                status: row.get(11)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn store_migration(&mut self, record: &StoredMigration) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_migrations(id,name,checksum,up_sql,down_sql,created_at_ms,applied_at_ms,\
             actor_id,duration_ms,version_before,version_after,status) VALUES \
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, checksum=excluded.checksum, \
             up_sql=excluded.up_sql, down_sql=excluded.down_sql, applied_at_ms=excluded.applied_at_ms, \
             actor_id=excluded.actor_id, duration_ms=excluded.duration_ms, \
             version_before=excluded.version_before, version_after=excluded.version_after, status=excluded.status",
            rusqlite::params![record.id, record.name, record.checksum, record.up_sql, record.down_sql,
                record.created_at_ms, record.applied_at_ms, record.actor_id, record.duration_ms,
                record.version_before, record.version_after, record.status],
        )?;
        Ok(())
    }

    /// Stores a worker receipt in the caller's current SQLite transaction.
    /// Keeping the receipt and the mutation in one commit removes the
    /// post-effect/pre-receipt crash window for migration operations.
    pub fn store_worker_operation_receipt(
        &mut self,
        receipt_id: &str,
        request_digest: &[u8],
        operation: &str,
        result_json: &str,
        recorded_at_ms: i64,
    ) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let updated = self.connection.execute(
            "INSERT INTO __ffdb_worker_operation_receipts \
             (receipt_id,request_digest,operation,result_json,recorded_at_ms) \
             VALUES (?1,?2,?3,?4,?5) ON CONFLICT(receipt_id) DO UPDATE SET \
             result_json=excluded.result_json,recorded_at_ms=excluded.recorded_at_ms \
             WHERE request_digest=excluded.request_digest AND operation=excluded.operation",
            rusqlite::params![
                receipt_id,
                request_digest,
                operation,
                result_json,
                recorded_at_ms
            ],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(RuntimeError::ConstraintViolation)
        }
    }

    /// Loads the durable usage record for a request. Internal access is leased
    /// so caller-authored SQL can never select the receipt table directly.
    pub fn usage_receipt(
        &mut self,
        receipt_id: &str,
    ) -> Result<Option<StoredUsageReceipt>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let result = self.connection.query_row(
            "SELECT transport_request_id,request_digest,response_json,reads,writes,logical_database_bytes,subject,recorded_at_ms \
             FROM __ffdb_usage_receipts WHERE request_id=?1",
            [receipt_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        );
        match result {
            Ok((
                request_id,
                request_digest,
                response_json,
                reads,
                writes,
                bytes,
                subject,
                recorded_at_ms,
            )) => Ok(Some(StoredUsageReceipt {
                request_id,
                request_digest,
                response_json,
                reads: u64::try_from(reads).map_err(|_| RuntimeError::UsageReceiptInvalid)?,
                writes: u64::try_from(writes).map_err(|_| RuntimeError::UsageReceiptInvalid)?,
                logical_database_bytes: u64::try_from(bytes)
                    .map_err(|_| RuntimeError::UsageReceiptInvalid)?,
                subject,
                recorded_at_ms,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns the logical main-database size (`page_count * page_size`). WAL
    /// file allocation is intentionally excluded because it is transient
    /// physical overhead rather than logical project data.
    pub fn logical_database_bytes(&mut self) -> Result<u64, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let page_count: i64 = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        page_count
            .checked_mul(page_size)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(RuntimeError::UsageReceiptInvalid)
    }

    /// Inserts a receipt in the caller's current transaction and records the
    /// logical SQLite size after the receipt payload has allocated its pages.
    /// The returned byte count is therefore the same value committed in the
    /// receipt row.
    pub fn store_usage_receipt(
        &mut self,
        receipt: &UsageReceiptInsert<'_>,
    ) -> Result<u64, RuntimeError> {
        if self.transaction_deadline.is_none() {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let reads = i64::try_from(receipt.reads).map_err(|_| RuntimeError::UsageReceiptInvalid)?;
        let writes =
            i64::try_from(receipt.writes).map_err(|_| RuntimeError::UsageReceiptInvalid)?;
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_usage_receipts \
             (request_id,transport_request_id,request_digest,response_json,reads,writes,logical_database_bytes,subject,recorded_at_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,0,?7,?8)",
            rusqlite::params![
                receipt.receipt_id,
                receipt.request_id,
                receipt.request_digest,
                receipt.response_json,
                reads,
                writes,
                receipt.subject,
                receipt.recorded_at_ms
            ],
        )?;
        let logical_database_bytes = self.logical_database_bytes()?;
        let stored_bytes =
            i64::try_from(logical_database_bytes).map_err(|_| RuntimeError::UsageReceiptInvalid)?;
        let updated = self.connection.execute(
            "UPDATE __ffdb_usage_receipts SET logical_database_bytes=?2 WHERE request_id=?1",
            rusqlite::params![receipt.receipt_id, stored_bytes],
        )?;
        if updated != 1 {
            return Err(RuntimeError::UsageReceiptInvalid);
        }
        Ok(logical_database_bytes)
    }

    fn validate(&self, request: &StatementRequest) -> Result<(), RuntimeError> {
        if request.sql.len() > self.limits.max_sql_bytes {
            return Err(RuntimeError::SqlTooLarge);
        }
        if request.parameters.len() > self.limits.max_variables {
            return Err(RuntimeError::TooManyVariables);
        }
        let class =
            classify_statement(&request.sql).map_err(|_| RuntimeError::StatementNotAllowed)?;
        match &self.mode {
            ExecutionMode::EndUser(_) if !class.allowed_for_end_user() => {
                Err(RuntimeError::StatementNotAllowed)
            }
            ExecutionMode::Developer(_)
                if matches!(class.kind, StatementKind::Attach | StatementKind::Detach) =>
            {
                Err(RuntimeError::StatementNotAllowed)
            }
            ExecutionMode::Developer(_)
                if class.kind == StatementKind::Vacuum
                    && request.sql.to_ascii_uppercase().contains("INTO") =>
            {
                Err(RuntimeError::StatementNotAllowed)
            }
            ExecutionMode::Developer(_) if class.kind == StatementKind::Rls => {
                Err(RuntimeError::StatementNotAllowed)
            }
            _ => Ok(()),
        }
    }

    fn execute_bounded(
        &mut self,
        request: &StatementRequest,
        deadline: Instant,
    ) -> Result<QueryResult, RuntimeError> {
        if self.cancellation.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        let cancellation = self.cancellation.clone();
        self.connection.progress_handler(
            self.limits.progress_ops,
            Some(move || cancellation.is_cancelled() || Instant::now() >= deadline),
        );
        let class = classify_statement(&request.sql).ok();
        let write = class.as_ref().is_none_or(|class| !class.read_only);
        if write {
            self.connection
                .execute_batch("SAVEPOINT __ffdb_statement_limit")?;
        }
        if class
            .as_ref()
            .is_some_and(|class| class.kind == StatementKind::Ddl)
            && let Err(error) = self.suspend_change_capture()
        {
            let _ = self
                .connection
                .execute_batch("ROLLBACK TO __ffdb_statement_limit");
            let _ = self
                .connection
                .execute_batch("RELEASE __ffdb_statement_limit");
            self.connection.progress_handler(0, None::<fn() -> bool>);
            return Err(error);
        }
        let mut result = self.execute_inner(request);
        if result.is_ok()
            && class
                .as_ref()
                .is_some_and(|class| class.kind == StatementKind::Ddl)
            && let Err(error) = self
                .load_rls_catalog()
                .and_then(|catalog| self.refresh_change_capture(&catalog))
        {
            result = Err(error);
        }
        if write {
            if result.is_ok() {
                result = self.check_database_size().and(result);
            }
            if result.is_ok() {
                if let Err(error) = self
                    .connection
                    .execute_batch("RELEASE __ffdb_statement_limit")
                {
                    result = Err(error.into());
                }
            } else {
                let _ = self
                    .connection
                    .execute_batch("ROLLBACK TO __ffdb_statement_limit");
                let _ = self
                    .connection
                    .execute_batch("RELEASE __ffdb_statement_limit");
            }
        }
        self.connection.progress_handler(0, None::<fn() -> bool>);
        match result {
            Err(RuntimeError::Database) if self.cancellation.is_cancelled() => {
                Err(RuntimeError::Cancelled)
            }
            Err(RuntimeError::Database) if Instant::now() >= deadline => {
                Err(RuntimeError::DeadlineExceeded)
            }
            result => result,
        }
    }

    fn execute_inner(&mut self, request: &StatementRequest) -> Result<QueryResult, RuntimeError> {
        let statement_kind = classify_statement(&request.sql)
            .ok()
            .map(|class| class.kind);
        let sqlite_sql = rewrite_auth_functions_for_execution(&request.sql)
            .map_err(|_| RuntimeError::StatementNotAllowed)?;
        let uses_public_auth = sqlite_sql != request.sql;
        let _public_auth = uses_public_auth
            .then(|| PublicAuthLease::enter(&self.context))
            .transpose()?;
        let mut statement = self.connection.prepare(&sqlite_sql)?;
        if statement.parameter_count() > self.limits.max_variables {
            return Err(RuntimeError::TooManyVariables);
        }
        let columns = statement
            .columns()
            .into_iter()
            .map(|column| ResultColumn {
                name: column.name().to_owned(),
                declared_type: column.decl_type().map(str::to_owned),
            })
            .collect::<Vec<_>>();
        if columns.is_empty() {
            let affected_rows =
                statement.execute(rusqlite::params_from_iter(&request.parameters))?;
            let last_insert_rowid = (statement_kind == Some(StatementKind::Insert))
                .then(|| self.connection.last_insert_rowid());
            return Ok(QueryResult {
                columns,
                rows: Vec::new(),
                affected_rows: affected_rows as u64,
                last_insert_rowid,
                truncated: false,
            });
        }
        let mut rows = statement.query(rusqlite::params_from_iter(&request.parameters))?;
        let mut output_rows = Vec::new();
        let mut encoded_bytes = columns
            .iter()
            .map(|column| column.name.len() + 24)
            .sum::<usize>();
        let mut truncated = false;
        while let Some(row) = rows.next()? {
            if output_rows.len() == self.limits.max_rows {
                truncated = true;
                break;
            }
            let mut output = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                let value = ResultValue::from_value_ref(row.get_ref(index)?);
                encoded_bytes = encoded_bytes.saturating_add(value.encoded_size() + 1);
                if encoded_bytes > self.limits.max_response_bytes {
                    return Err(RuntimeError::ResponseTooLarge);
                }
                output.push(value);
            }
            output_rows.push(output);
        }
        drop(rows);
        drop(statement);
        Ok(QueryResult {
            columns,
            rows: output_rows,
            affected_rows: if matches!(
                statement_kind,
                Some(StatementKind::Insert | StatementKind::Update | StatementKind::Delete)
            ) {
                self.connection.changes()
            } else {
                0
            },
            last_insert_rowid: None,
            truncated,
        })
    }

    fn check_database_size(&mut self) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let page_count: u64 = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: u64 = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        if page_count.saturating_mul(page_size) > self.limits.max_database_bytes {
            Err(RuntimeError::DatabaseTooLarge)
        } else {
            Ok(())
        }
    }

    fn restore_approved_sources(
        &self,
        approved_sources: std::collections::BTreeSet<String>,
    ) -> Result<(), RuntimeError> {
        self.context
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .approved_sources = approved_sources;
        Ok(())
    }
}

fn rewrite_logical_schema_names(
    mut sql: String,
    catalog: &RlsCatalog,
) -> Result<String, RuntimeError> {
    for logical in catalog.tables().keys() {
        let physical = backing_table_name(logical).map_err(|_| RuntimeError::Database)?;
        sql = sql.replace(physical.as_str(), logical.as_str());
    }
    if sql.to_ascii_lowercase().contains("__ffdb_data_") {
        return Err(RuntimeError::Database);
    }
    Ok(sql)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use ffdb_sql_parser::parse_rls_statement;
    use ffdb_sqlite_rls::Compiler;
    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;

    const DATABASE_ID: &str = "019fc39c-ddbd-7d12-9849-e4ee35310132";

    fn database(mut limits: ExecutionLimits) -> (TempDir, Database) {
        limits.max_database_bytes = limits.max_database_bytes.max(64 * 1024);
        let directory = TempDir::new().unwrap();
        let path = TrustedDatabasePath::for_database(directory.path(), DATABASE_ID).unwrap();
        let database = Database::open(
            path,
            RuntimeConfig {
                limits,
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        (directory, database)
    }

    fn developer() -> ExecutionMode {
        ExecutionMode::Developer(DeveloperPrincipal {
            actor_id: "operator".to_owned(),
            api_key_id: "key-1".to_owned(),
        })
    }

    fn user(subject: &str) -> ExecutionMode {
        ExecutionMode::EndUser(AuthContext {
            project_id: "project".to_owned(),
            subject: subject.to_owned(),
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: format!("token-{subject}"),
        })
    }

    fn statement(sql: &str, parameters: Vec<SqlParameter>) -> StatementRequest {
        StatementRequest {
            sql: sql.to_owned(),
            parameters,
        }
    }

    fn install_owner_rls(database: &Database) {
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "CREATE TABLE documents(id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, body TEXT)",
                    Vec::new(),
                ))?;
                let mut catalog = session.load_rls_catalog()?;
                for sql in [
                    "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
                    "CREATE POLICY documents_owner ON documents FOR ALL TO authenticated USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())",
                ] {
                    let schema = session.schema_snapshot(&catalog)?;
                    catalog
                        .apply(&schema, parse_rls_statement(sql).map_err(|_| RuntimeError::Database)?)
                        .map_err(|_| RuntimeError::Database)?;
                }
                let schema = session.schema_snapshot(&catalog)?;
                let plan = Compiler.compile(&schema, &catalog).map_err(|_| RuntimeError::Database)?;
                session.apply_rls_plan(&plan)?;
                session.store_rls_catalog(&catalog)
            })
            .unwrap();
    }

    #[test]
    fn legacy_internal_column_migrations_are_batched_by_table() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE __ffdb_usage_receipts(request_id TEXT PRIMARY KEY); \
                 CREATE TABLE __ffdb_storage_reservations(project_id TEXT, nonce TEXT); \
                 CREATE TABLE __ffdb_storage_provider_uploads(upload_id TEXT PRIMARY KEY);",
            )
            .unwrap();
        ensure_internal_columns(
            &connection,
            "__ffdb_usage_receipts",
            &[("transport_request_id", "TEXT NOT NULL DEFAULT ''")],
        )
        .unwrap();
        ensure_internal_columns(
            &connection,
            "__ffdb_storage_reservations",
            &[
                ("provider_key", "TEXT"),
                ("action", "TEXT"),
                ("upload_id", "TEXT"),
            ],
        )
        .unwrap();
        ensure_internal_columns(
            &connection,
            "__ffdb_storage_provider_uploads",
            &[
                (
                    "reserved_bytes",
                    "INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes >= 0)",
                ),
                ("replacement_fingerprint", "TEXT"),
            ],
        )
        .unwrap();
        for (table, expected) in [
            ("__ffdb_usage_receipts", &["transport_request_id"][..]),
            (
                "__ffdb_storage_reservations",
                &["provider_key", "action", "upload_id"][..],
            ),
            (
                "__ffdb_storage_provider_uploads",
                &["reserved_bytes", "replacement_fingerprint"][..],
            ),
        ] {
            let mut statement = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<std::collections::HashSet<_>, _>>()
                .unwrap();
            for column in expected {
                assert!(
                    columns.contains(*column),
                    "{table}.{column} was not restored"
                );
            }
        }
    }

    #[test]
    fn rls_separates_users_and_rejects_direct_backing_access() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (?1, ?2, ?3)",
                    vec![
                        SqlParameter::Integer(1),
                        SqlParameter::Text("alice".to_owned()),
                        SqlParameter::Text("secret-a".to_owned()),
                    ],
                ))?;
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (?1, ?2, ?3)",
                    vec![
                        SqlParameter::Integer(2),
                        SqlParameter::Text("bob".to_owned()),
                        SqlParameter::Text("secret-b".to_owned()),
                    ],
                ))?;
                Ok(())
            })
            .unwrap();

        for (subject, expected) in [("alice", "secret-a"), ("bob", "secret-b")] {
            let result = database
                .with_context(user(subject), &cancellation, |session| {
                    session.execute(&statement(
                        "SELECT body FROM documents ORDER BY id",
                        Vec::new(),
                    ))
                })
                .unwrap();
            assert_eq!(
                result.rows,
                vec![vec![ResultValue::Text(expected.to_owned())]]
            );
        }

        let backing = ffdb_sqlite_rls::backing_table_name(
            &ffdb_sql_parser::Identifier::new("documents").unwrap(),
        )
        .unwrap();
        let error = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement(
                    &format!("SELECT * FROM {}", backing.quoted()),
                    Vec::new(),
                ))
            })
            .unwrap_err();
        assert_eq!(error, RuntimeError::StatementNotAllowed);
    }

    #[test]
    fn application_sql_can_use_public_auth_functions_without_exposing_private_names() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();

        let inserted = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) \
                     VALUES (10, auth.uid(), auth.role()) RETURNING owner_id, body",
                    Vec::new(),
                ))
            })
            .unwrap();
        assert_eq!(
            inserted.rows,
            vec![vec![
                ResultValue::Text("alice".to_owned()),
                ResultValue::Text("authenticated".to_owned()),
            ]]
        );

        let selected = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement(
                    "SELECT id FROM documents WHERE owner_id = auth.uid()",
                    Vec::new(),
                ))
            })
            .unwrap();
        assert_eq!(selected.rows, vec![vec![ResultValue::Integer(10)]]);

        for sql in ["SELECT __ffdb_auth_uid()", "SELECT auth.set_uid('bob')"] {
            let error = database
                .with_context(user("alice"), &cancellation, |session| {
                    session.execute(&statement(sql, Vec::new()))
                })
                .unwrap_err();
            assert_eq!(error, RuntimeError::StatementNotAllowed);
        }
    }

    #[test]
    fn with_check_denies_cross_user_insert_and_context_does_not_leak() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        let error = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (1, 'bob', 'stolen') RETURNING id",
                    Vec::new(),
                ))
            })
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::ConstraintViolation | RuntimeError::Database
        ));

        let inserted = database
            .with_context(user("bob"), &cancellation, |session| {
                session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (2, 'bob', 'ok') RETURNING id",
                    Vec::new(),
                ))
            })
            .unwrap();
        assert_eq!(inserted.rows, vec![vec![ResultValue::Integer(2)]]);
        let alice = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement("SELECT count(*) FROM documents", Vec::new()))
            })
            .unwrap();
        assert_eq!(alice.rows, vec![vec![ResultValue::Integer(0)]]);
    }

    #[test]
    fn hidden_and_visible_unique_collisions_fail_with_the_same_generic_error() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id,owner_id,body) VALUES \
                     (1,'alice','hidden'),(2,'bob','visible')",
                    Vec::new(),
                ))?;
                Ok(())
            })
            .unwrap();

        let collision = |id| {
            database
                .with_context(user("bob"), &cancellation, |session| {
                    session.execute(&statement(
                        "INSERT INTO documents(id,owner_id,body) VALUES (?1,'bob','probe')",
                        vec![SqlParameter::Integer(id)],
                    ))
                })
                .unwrap_err()
        };
        let hidden = collision(1);
        let visible = collision(2);
        assert_eq!(hidden, RuntimeError::ConstraintViolation);
        assert_eq!(visible, RuntimeError::ConstraintViolation);
        assert_eq!(hidden, visible);
        assert_eq!(hidden.to_string(), "constraint violation");
    }

    #[test]
    fn select_affected_rows_is_zero_and_does_not_leak_connection_history() {
        let (_directory, database) = database(ExecutionLimits::default());
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "CREATE TABLE request_local(id INTEGER PRIMARY KEY, value TEXT)",
                    Vec::new(),
                ))?;
                let inserted = session.execute(&statement(
                    "INSERT INTO request_local(value) VALUES ('one'),('two')",
                    Vec::new(),
                ))?;
                assert_eq!(inserted.affected_rows, 2);
                Ok(())
            })
            .unwrap();

        let selected = database
            .with_context(developer(), &cancellation, |session| {
                session.execute(&statement(
                    "SELECT value FROM request_local ORDER BY id",
                    Vec::new(),
                ))
            })
            .unwrap();
        assert_eq!(selected.affected_rows, 0);
        assert_eq!(selected.last_insert_rowid, None);
        assert_eq!(selected.rows.len(), 2);

        let returning = database
            .with_context(developer(), &cancellation, |session| {
                session.execute(&statement(
                    "INSERT INTO request_local(value) VALUES ('three') RETURNING id",
                    Vec::new(),
                ))
            })
            .unwrap();
        assert_eq!(returning.affected_rows, 1);
    }

    #[test]
    fn ordinary_rls_write_is_atomically_visible_to_sync_pull_and_policy_filtered() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        let cursor = database
            .with_context(user("alice"), &cancellation, |session| {
                session.sync_snapshot(None).map(|snapshot| snapshot.cursor)
            })
            .unwrap();
        database
            .with_context(user("alice"), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (7, 'alice', 'captured')",
                    Vec::new(),
                ))?;
                Ok(())
            })
            .unwrap();

        let populated_snapshot = database
            .with_context(user("alice"), &cancellation, |session| {
                session.sync_snapshot(Some(&["documents".to_owned()]))
            })
            .unwrap();
        let documents = populated_snapshot.tables.get("documents").unwrap();
        assert_eq!(documents.rows.len(), 1);
        assert_eq!(
            documents
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "id",
                "owner_id",
                "body",
                "__ffdb_primary_key",
                "__ffdb_row_version",
                "__ffdb_server_sequence",
            ]
        );
        assert_eq!(documents.rows[0][3], ResultValue::Text("7".to_owned()));
        assert_eq!(documents.rows[0][4], ResultValue::Integer(1));
        assert_eq!(documents.rows[0][5], ResultValue::Integer(1));

        let alice = database
            .with_context(user("alice"), &cancellation, |session| {
                session.sync_pull(Some(&cursor), 10)
            })
            .unwrap();
        assert_eq!(alice.changes.len(), 1);
        assert_eq!(alice.changes[0].operation, SyncChangeOperation::Insert);
        assert_eq!(alice.changes[0].table, "documents");
        assert_eq!(
            alice.changes[0]
                .values
                .as_ref()
                .and_then(|values| values.get("body")),
            Some(&serde_json::json!("captured"))
        );

        let bob = database
            .with_context(user("bob"), &cancellation, |session| {
                session.sync_pull(None, 10)
            })
            .unwrap();
        assert!(bob.changes.is_empty());
    }

    #[test]
    fn end_users_and_developers_cannot_use_escape_hatches() {
        let (_directory, database) = database(ExecutionLimits::default());
        let cancellation = CancellationToken::default();
        let end_user_ddl = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement("CREATE TABLE escaped(id)", Vec::new()))
            })
            .unwrap_err();
        assert_eq!(end_user_ddl, RuntimeError::StatementNotAllowed);
        for sql in [
            "ATTACH DATABASE '/tmp/escape.sqlite3' AS escape",
            "PRAGMA writable_schema=ON",
            "VACUUM INTO '/tmp/copy.sqlite3'",
        ] {
            assert!(
                database
                    .with_context(developer(), &cancellation, |session| {
                        session.execute(&statement(sql, Vec::new()))
                    })
                    .is_err()
            );
        }
    }

    #[test]
    fn storage_metadata_tables_bootstrap_as_public_default_deny_rls_surfaces() {
        let (_directory, database) = database(ExecutionLimits::default());
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let catalog = session.load_rls_catalog()?;
                for name in [
                    "storage_buckets",
                    "storage_objects",
                    "storage_uploads",
                    "storage_versions",
                ] {
                    let identifier = ffdb_sql_parser::Identifier::new(name)
                        .map_err(|_| RuntimeError::Database)?;
                    assert!(
                        catalog
                            .tables()
                            .get(&identifier)
                            .is_some_and(|state| state.enabled)
                    );
                }
                let definitions = session.logical_schema_sql(&catalog)?;
                assert!(
                    definitions
                        .values()
                        .all(|sql| !sql.to_ascii_lowercase().contains("__ffdb_data_"))
                );
                assert!(
                    definitions
                        .get("storage_objects")
                        .is_some_and(|sql| sql.contains("storage_buckets"))
                );
                Ok(())
            })
            .unwrap();
        let rows = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement("SELECT * FROM storage_buckets", Vec::new()))
            })
            .unwrap();
        assert!(rows.rows.is_empty());
        let insert = database.with_context(user("alice"), &cancellation, |session| {
            session.execute(&statement(
                "INSERT INTO storage_buckets \
                 (id,name,owner_id,public,max_object_bytes,project_quota_bytes,created_at_ms) \
                 VALUES ('b1','private','alice',0,100,1000,1)",
                Vec::new(),
            ))
        });
        assert!(insert.is_err());
    }

    #[test]
    fn developer_trigger_cannot_impersonate_generated_source() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        let backing = ffdb_sqlite_rls::backing_table_name(
            &ffdb_sql_parser::Identifier::new("documents").unwrap(),
        )
        .unwrap();
        let result = database.with_context(developer(), &cancellation, |session| {
            let _ = session.execute(&statement("CREATE TABLE bridge(id INTEGER)", Vec::new()))?;
            session.execute(&statement(
                &format!(
                    "CREATE TRIGGER __ffdb_insert_evil AFTER INSERT ON bridge BEGIN \
                     INSERT INTO {}(id, owner_id, body) VALUES (NEW.id, 'attacker', 'bypass'); END",
                    backing.quoted()
                ),
                Vec::new(),
            ))
        });
        assert!(
            result.is_err(),
            "reserved trigger impersonation must be denied"
        );
    }

    #[test]
    fn developer_trigger_cannot_reuse_approved_view_name_as_accessor() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        let backing = ffdb_sqlite_rls::backing_table_name(
            &ffdb_sql_parser::Identifier::new("documents").unwrap(),
        )
        .unwrap();
        let result = database.with_context(developer(), &cancellation, |session| {
            let _ = session.execute(&statement("CREATE TABLE bridge(id INTEGER)", Vec::new()))?;
            session.execute(&statement(
                &format!(
                    "CREATE TRIGGER documents AFTER INSERT ON bridge BEGIN \
                     INSERT INTO {}(id, owner_id, body) VALUES (NEW.id, 'attacker', 'bypass'); END",
                    backing.quoted()
                ),
                Vec::new(),
            ))
        });
        assert!(
            result.is_err(),
            "an approved view name cannot be reused by a caller trigger"
        );
    }

    #[test]
    fn cte_auth_function_upsert_and_update_bypasses_fail_closed() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        database
            .with_context(user("alice"), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (1, 'alice', 'safe')",
                    Vec::new(),
                ))?;
                Ok(())
            })
            .unwrap();

        let backing = ffdb_sqlite_rls::backing_table_name(
            &ffdb_sql_parser::Identifier::new("documents").unwrap(),
        )
        .unwrap();
        for sql in [
            format!(
                "WITH leaked AS (SELECT * FROM {}) SELECT * FROM leaked",
                backing.quoted()
            ),
            "SELECT __ffdb_auth_uid()".to_owned(),
            "INSERT INTO documents(id, owner_id, body) VALUES (1, 'alice', 'replace') \
             ON CONFLICT(id) DO UPDATE SET body=excluded.body"
                .to_owned(),
            "UPDATE documents SET owner_id='bob' WHERE id=1 RETURNING body".to_owned(),
        ] {
            assert!(
                database
                    .with_context(user("alice"), &cancellation, |session| {
                        session.execute(&statement(&sql, Vec::new()))
                    })
                    .is_err(),
                "bypass form unexpectedly succeeded: {sql}"
            );
        }
        let result = database
            .with_context(user("alice"), &cancellation, |session| {
                session.execute(&statement(
                    "SELECT owner_id, body FROM documents WHERE id=1",
                    Vec::new(),
                ))
            })
            .unwrap();
        assert_eq!(
            result.rows,
            vec![vec![
                ResultValue::Text("alice".to_owned()),
                ResultValue::Text("safe".to_owned())
            ]]
        );
    }

    #[test]
    fn nested_developer_view_preserves_rls_filtering() {
        let (_directory, database) = database(ExecutionLimits::default());
        install_owner_rls(&database);
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (1, 'alice', 'a')",
                    Vec::new(),
                ))?;
                let _ = session.execute(&statement(
                    "INSERT INTO documents(id, owner_id, body) VALUES (2, 'bob', 'b')",
                    Vec::new(),
                ))?;
                let _ = session.execute(&statement(
                    "CREATE VIEW nested_documents AS SELECT * FROM documents",
                    Vec::new(),
                ))?;
                Ok(())
            })
            .unwrap();
        let result = database
            .with_context(user("bob"), &cancellation, |session| {
                session.execute(&statement("SELECT body FROM nested_documents", Vec::new()))
            })
            .unwrap();
        assert_eq!(result.rows, vec![vec![ResultValue::Text("b".to_owned())]]);
    }

    #[test]
    fn cancellation_interrupts_recursive_work_and_connection_is_reusable() {
        let limits = ExecutionLimits {
            statement_timeout: Duration::from_secs(30),
            ..ExecutionLimits::default()
        };
        let (_directory, database) = database(limits);
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let error = database
            .with_context(user("alice"), &cancelled, |session| {
                session.execute(&statement(
                    "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n",
                    Vec::new(),
                ))
            })
            .unwrap_err();
        assert_eq!(error, RuntimeError::Cancelled);

        let result = database
            .with_context(user("bob"), &CancellationToken::default(), |session| {
                session.execute(&statement("SELECT 1", Vec::new()))
            })
            .unwrap();
        assert_eq!(result.rows, vec![vec![ResultValue::Integer(1)]]);
    }

    #[test]
    fn usage_receipt_failure_rolls_back_the_data_effect_and_internal_table_is_hidden() {
        let (_directory, database) = database(ExecutionLimits::default());
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "CREATE TABLE usage_atomic_probe(id INTEGER PRIMARY KEY)",
                    Vec::new(),
                ))?;
                Ok(())
            })
            .unwrap();

        let error = database
            .with_context(developer(), &cancellation, |session| {
                session.atomic(|session| {
                    let _ = session.execute(&statement(
                        "INSERT INTO usage_atomic_probe(id) VALUES (1)",
                        Vec::new(),
                    ))?;
                    let _ = session.store_usage_receipt(&UsageReceiptInsert {
                        receipt_id: "019fc39c-ddbd-7d12-9849-e4ee35310134",
                        request_id: "019fc39c-ddbd-7d12-9849-e4ee35310134",
                        request_digest: &[9_u8; 32],
                        response_json: None,
                        reads: u64::MAX,
                        writes: 0,
                        subject: None,
                        recorded_at_ms: 1,
                    })?;
                    Ok(())
                })
            })
            .unwrap_err();
        assert_eq!(error, RuntimeError::UsageReceiptInvalid);

        database
            .with_context(developer(), &cancellation, |session| {
                let result = session.execute(&statement(
                    "SELECT count(*) FROM usage_atomic_probe",
                    Vec::new(),
                ))?;
                assert_eq!(result.rows, vec![vec![ResultValue::Integer(0)]]);
                assert!(
                    session
                        .usage_receipt("019fc39c-ddbd-7d12-9849-e4ee35310134")?
                        .is_none()
                );
                let hidden = session
                    .execute(&statement(
                        "SELECT * FROM __ffdb_usage_receipts",
                        Vec::new(),
                    ))
                    .unwrap_err();
                assert_eq!(hidden, RuntimeError::StatementNotAllowed);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn online_backup_is_consistent_and_passes_integrity_check() {
        let (directory, database) = database(ExecutionLimits::default());
        let cancellation = CancellationToken::default();
        database
            .with_context(developer(), &cancellation, |session| {
                let _ = session.execute(&statement(
                    "CREATE TABLE backup_probe(id INTEGER PRIMARY KEY, value TEXT)",
                    Vec::new(),
                ))?;
                let _ = session.execute(&statement(
                    "INSERT INTO backup_probe(value) VALUES ('durable')",
                    Vec::new(),
                ))?;
                Ok(())
            })
            .unwrap();
        let backup_id = "019fc39c-ddbd-7d12-9849-e4ee35310133";
        let backup_path = TrustedDatabasePath::for_database(directory.path(), backup_id).unwrap();
        database.backup_to(&backup_path).unwrap();
        let restored = Database::open(backup_path, RuntimeConfig::default()).unwrap();
        assert!(restored.integrity_check().unwrap());
        let result = restored
            .with_context(developer(), &cancellation, |session| {
                session.execute(&statement("SELECT value FROM backup_probe", Vec::new()))
            })
            .unwrap();
        assert_eq!(
            result.rows,
            vec![vec![ResultValue::Text("durable".to_owned())]]
        );
    }
}
