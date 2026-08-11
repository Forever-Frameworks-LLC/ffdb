//! Length-bounded worker protocol and stale-route-safe request dispatch.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

mod backup_crypto;
mod storage;

pub use storage::SqliteMetadataAuthorizer;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ffdb_migration_engine::{
    DurableOperationReceipt, MigrationEngine, MigrationOutcome,
    MigrationSpec as EngineMigrationSpec,
};
use ffdb_protocol as protocol;
use ffdb_sqlite_runtime as runtime;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

use backup_crypto::{BackupCrypto, BackupCryptoError, remove_file_if_present};

pub const MAX_FRAME_BYTES: usize = 9 * 1024 * 1024;
const MAX_OPERATION_RECEIPT_BYTES: u64 = 1024 * 1024;
const OPERATION_RECEIPT_RETENTION: Duration = Duration::from_secs(48 * 60 * 60);

#[derive(Debug)]
pub struct DatabaseWorker {
    route: protocol::DatabaseRoute,
    database: Arc<runtime::Database>,
    backup_root: PathBuf,
    transient_root: PathBuf,
    receipt_root: PathBuf,
    receipt_maintenance_counter: AtomicU64,
    backup_crypto: BackupCrypto,
    storage_cursor_codec: storage::StorageCursorCodec,
    migrations: MigrationEngine,
}

#[derive(Clone, Debug, Error, Eq, PartialEq, Serialize, Deserialize)]
#[error("{message}")]
pub struct WorkerFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
// This response crosses a Unix socket, so preserving the unboxed router contract matters more
// than reducing the enum's stack size.
#[allow(clippy::large_enum_variant)]
pub enum WireResponse {
    Ok(protocol::WorkerExecution),
    Error(protocol::PlatformError),
}

impl DatabaseWorker {
    pub fn open(
        route: protocol::DatabaseRoute,
        database_root: &Path,
        backup_root: &Path,
        config: runtime::RuntimeConfig,
        backup_master_key: impl AsRef<[u8]>,
    ) -> Result<Self, WorkerFailure> {
        fs::create_dir_all(database_root).map_err(|_| {
            failure(
                "internal.database_open",
                "database worker could not initialize storage",
            )
        })?;
        fs::create_dir_all(backup_root).map_err(|_| {
            failure(
                "internal.backup_open",
                "database worker could not initialize backup storage",
            )
        })?;
        let transient_root = initialize_transient_root(backup_root, route.database_id)?;
        let receipt_root = initialize_receipt_root(backup_root, route.database_id)?;
        let path = runtime::TrustedDatabasePath::for_database(
            database_root,
            &route.database_id.to_string(),
        )
        .map_err(map_runtime)?;
        let database = Arc::new(runtime::Database::open(path, config).map_err(map_runtime)?);
        let backup_crypto = BackupCrypto::new(backup_master_key).map_err(map_backup_crypto)?;
        let mut cursor_secret = backup_crypto
            .derive_storage_cursor_secret(&route)
            .map_err(map_backup_crypto)?;
        let storage_cursor_codec =
            storage::StorageCursorCodec::new(cursor_secret).map_err(map_storage)?;
        cursor_secret.zeroize();
        Ok(Self {
            route,
            database,
            backup_root: backup_root.to_owned(),
            transient_root,
            receipt_root,
            receipt_maintenance_counter: AtomicU64::new(0),
            backup_crypto,
            storage_cursor_codec,
            migrations: MigrationEngine,
        })
    }

    #[must_use]
    pub fn metadata_authorizer(&self) -> SqliteMetadataAuthorizer {
        SqliteMetadataAuthorizer::new(
            Arc::clone(&self.database),
            self.route.project_id,
            self.storage_cursor_codec.clone(),
        )
    }

    fn create_encrypted_backup(
        &self,
        backup_id: protocol::BackupId,
        cancellation: &runtime::CancellationToken,
        deadline: Instant,
    ) -> Result<protocol::BackupResult, WorkerFailure> {
        let destination = self.backup_root.join(format!("{backup_id}.sqlite3"));
        if destination.exists() {
            return Err(failure(
                "backup.already_exists",
                "backup identifier already exists",
            ));
        }
        let temporary_id = Uuid::now_v7();
        let plaintext = runtime::TrustedDatabasePath::for_database(
            &self.transient_root,
            &temporary_id.to_string(),
        )
        .map_err(map_runtime)?;
        let ciphertext = self
            .backup_root
            .join(format!(".{backup_id}.{temporary_id}.encrypted"));
        let result = (|| {
            self.database
                .backup_to_bounded(&plaintext, cancellation, deadline)
                .map_err(map_runtime)?;
            let (size_bytes, sha256) = self
                .backup_crypto
                .encrypt_file(
                    plaintext.as_path(),
                    &ciphertext,
                    &self.route,
                    backup_id,
                    cancellation,
                    deadline,
                )
                .map_err(map_backup_crypto)?;
            fs::rename(&ciphertext, &destination)
                .map_err(|_| failure("internal.backup_failed", "backup could not be finalized"))?;
            fs::File::open(&self.backup_root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| failure("internal.backup_failed", "backup could not be finalized"))?;
            Ok(protocol::BackupResult {
                backup_id,
                size_bytes,
                sha256,
            })
        })();
        let cleanup = remove_transient_database_files(&self.transient_root, temporary_id);
        remove_file_if_present(&ciphertext);
        cleanup?;
        result
    }

    fn restore_encrypted_backup(
        &self,
        backup_id: protocol::BackupId,
        cancellation: &runtime::CancellationToken,
        deadline: Instant,
        receipt: Option<&OperationReceipt>,
    ) -> Result<protocol::RestoreResult, WorkerFailure> {
        let source = self.backup_root.join(format!("{backup_id}.sqlite3"));
        if !source.is_file() {
            return Err(failure("backup.not_found", "backup does not exist"));
        }
        let temporary_id = Uuid::now_v7();
        let plaintext = runtime::TrustedDatabasePath::for_database(
            &self.transient_root,
            &temporary_id.to_string(),
        )
        .map_err(map_runtime)?;
        if let Err(error) = self.backup_crypto.decrypt_file(
            &source,
            plaintext.as_path(),
            &self.route,
            backup_id,
            cancellation,
            deadline,
        ) {
            remove_transient_database_files(&self.transient_root, temporary_id)?;
            return Err(map_backup_crypto(error));
        }
        let prepared_schema_version = match receipt {
            Some(receipt) => match runtime::Database::prepare_restore_receipt(
                &plaintext,
                &receipt.receipt_id.to_string(),
                &receipt.request_digest,
                &backup_id.to_string(),
            ) {
                Ok(value) => Some(value),
                Err(error) => {
                    remove_transient_database_files(&self.transient_root, temporary_id)?;
                    return Err(map_runtime(error));
                }
            },
            None => None,
        };

        let recovery_id = protocol::BackupId::new();
        let recovery = self.create_encrypted_backup(recovery_id, cancellation, deadline);
        if let Err(error) = recovery {
            remove_transient_database_files(&self.transient_root, temporary_id)?;
            return Err(error);
        }
        let restore = self
            .database
            .restore_from_bounded(&plaintext, cancellation, deadline)
            .map_err(map_runtime);
        remove_transient_database_files(&self.transient_root, temporary_id)?;
        match restore {
            Ok(()) => {
                remove_file_if_present(&self.backup_root.join(format!("{recovery_id}.sqlite3")));
                let schema_version = match prepared_schema_version {
                    Some(value) => value,
                    None => self
                        .database
                        .with_context(
                            runtime::ExecutionMode::Developer(runtime::DeveloperPrincipal {
                                actor_id: "ffdb-restore".to_owned(),
                                api_key_id: "ffdb-restore".to_owned(),
                            }),
                            cancellation,
                            |session| session.schema_version(),
                        )
                        .map_err(map_runtime)?,
                };
                Ok(protocol::RestoreResult {
                    backup_id,
                    integrity_ok: true,
                    schema_version,
                })
            }
            Err(error) => {
                // The encrypted pre-restore snapshot remains under its recovery UUID.
                Err(error)
            }
        }
    }

    pub fn handle(
        &self,
        request: protocol::WorkerRequest,
        cancellation: &runtime::CancellationToken,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        self.validate_envelope(&request)?;
        let mode = convert_mode(&request.mode);
        let budget = request_budget(&request)?;
        if let Some(expected) = request.expected_schema_version {
            let actual = self
                .database
                .with_context_budget(mode.clone(), cancellation, &budget, |session| {
                    session.schema_version()
                })
                .map_err(map_runtime)?;
            if actual != expected {
                return Err(failure(
                    "query.schema_version_mismatch",
                    "database schema version does not match the request",
                ));
            }
        }
        match &request.operation {
            protocol::WorkerOperation::Query(query) => {
                return self.execute_metered_query(&request, query, cancellation, &budget);
            }
            protocol::WorkerOperation::Transaction(transaction) => {
                return self.execute_metered_transaction(
                    &request,
                    transaction,
                    cancellation,
                    &budget,
                );
            }
            protocol::WorkerOperation::Snapshot(snapshot) => {
                return self.execute_metered_snapshot(&request, snapshot, cancellation, &budget);
            }
            protocol::WorkerOperation::SyncPull(pull) => {
                return self.execute_metered_sync_pull(&request, pull, cancellation, &budget);
            }
            protocol::WorkerOperation::SyncPush(push) => {
                return self.execute_metered_sync_push(&request, push, cancellation, &budget);
            }
            _ => {}
        }
        let receipt = match request.operation_receipt_id {
            Some(receipt_id) => {
                require_developer(&request.mode)?;
                match self.begin_operation_receipt(
                    receipt_id,
                    &request.mode,
                    &request.operation,
                    cancellation,
                    &budget,
                )? {
                    ReceiptAdmission::Replay(response) => {
                        return self.unmetered_execution(
                            request.request_id,
                            &request.mode,
                            response,
                            cancellation,
                            &budget,
                        );
                    }
                    ReceiptAdmission::Owner(receipt) => Some(receipt),
                }
            }
            None => None,
        };
        let response = match request.operation {
            protocol::WorkerOperation::Query(_)
            | protocol::WorkerOperation::Transaction(_)
            | protocol::WorkerOperation::Snapshot(_)
            | protocol::WorkerOperation::SyncPull(_)
            | protocol::WorkerOperation::SyncPush(_) => {
                unreachable!("metered operation dispatched")
            }
            protocol::WorkerOperation::ApplyMigration(specification) => {
                let principal = require_developer(&request.mode)?;
                let engine_spec = convert_migration_spec(&specification);
                let now = epoch_ms()?;
                let durable_receipt = receipt.as_ref().map(durable_migration_receipt);
                let result = self
                    .migrations
                    .apply_with_receipt(
                        &self.database,
                        principal,
                        &engine_spec,
                        now,
                        cancellation,
                        &budget,
                        durable_receipt.as_ref(),
                    )
                    .map_err(map_migration)?;
                Ok(protocol::WorkerResponse::Migration(
                    protocol::MigrationRecord {
                        spec: specification,
                        status: protocol::MigrationStatus::Applied,
                        schema_version_before: result.schema_version_before,
                        schema_version_after: result.schema_version_after,
                        applied_at_ms: Some(result.applied_at_ms),
                        duration_ms: Some(result.duration_ms),
                        actor_api_key_id: developer_api_key(&request.mode)?,
                        execution_log: result.execution_log,
                    },
                ))
            }
            protocol::WorkerOperation::RollbackMigration { migration_id } => {
                let developer = match &request.mode {
                    protocol::ExecutionMode::Developer(developer) => developer.clone(),
                    protocol::ExecutionMode::EndUser(_) => {
                        return Err(failure(
                            "auth.developer_required",
                            "developer credentials are required",
                        ));
                    }
                };
                let principal = require_developer(&request.mode)?;
                let now = epoch_ms()?;
                let stored = self
                    .database
                    .with_context_budget(
                        convert_mode(&request.mode),
                        cancellation,
                        &budget,
                        |session| session.migration_record(&migration_id),
                    )
                    .map_err(map_runtime)?
                    .ok_or_else(|| failure("migration.not_found", "migration does not exist"))?;
                let durable_receipt = receipt.as_ref().map(durable_migration_receipt);
                let outcome = self
                    .migrations
                    .rollback_with_receipt(
                        &self.database,
                        principal,
                        &migration_id,
                        now,
                        cancellation,
                        &budget,
                        durable_receipt.as_ref(),
                    )
                    .map_err(map_migration)?;
                let spec = protocol::MigrationSpec {
                    id: stored.id,
                    name: stored.name,
                    up_sql: stored.up_sql,
                    down_sql: stored.down_sql,
                    checksum: stored
                        .checksum
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    created_at_ms: stored.created_at_ms,
                };
                Ok(protocol::WorkerResponse::Migration(
                    protocol::MigrationRecord {
                        spec,
                        status: protocol::MigrationStatus::RolledBack,
                        schema_version_before: outcome.schema_version_before,
                        schema_version_after: outcome.schema_version_after,
                        applied_at_ms: Some(outcome.applied_at_ms),
                        duration_ms: Some(outcome.duration_ms),
                        actor_api_key_id: developer.api_key_id,
                        execution_log: outcome.execution_log,
                    },
                ))
            }
            protocol::WorkerOperation::MigrationHistory {
                limit,
                before_version,
            } => {
                require_developer(&request.mode)?;
                let records = self
                    .database
                    .with_context_budget(mode, cancellation, &budget, |session| {
                        session.migration_history(
                            usize::try_from(limit).unwrap_or(usize::MAX),
                            before_version,
                        )
                    })
                    .map_err(map_runtime)?;
                Ok(protocol::WorkerResponse::MigrationHistory(
                    records
                        .into_iter()
                        .map(convert_migration_history)
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            protocol::WorkerOperation::Schema => {
                let schema = self
                    .database
                    .with_context_budget(mode, cancellation, &budget, |session| {
                        let catalog = session.load_rls_catalog()?;
                        let snapshot = session.schema_snapshot(&catalog)?;
                        let definitions = session.logical_schema_sql(&catalog)?;
                        let version = session.schema_version()?;
                        Ok((catalog, snapshot, definitions, version))
                    })
                    .map_err(map_runtime)?;
                Ok(protocol::WorkerResponse::Schema(protocol::SchemaSnapshot {
                    version: schema.3,
                    tables: schema
                        .1
                        .tables
                        .into_iter()
                        .map(|table| {
                            let policies = schema.0.tables().get(&table.name);
                            let name = table.name.as_str().to_owned();
                            protocol::TableDefinition {
                                sql: schema.2.get(&name).cloned().unwrap_or_default(),
                                name,
                                rls_enabled: policies.is_some_and(|policies| policies.enabled),
                                rls_forced: policies.is_some_and(|policies| policies.forced),
                            }
                        })
                        .collect(),
                }))
            }
            protocol::WorkerOperation::Policies => {
                let catalog = self
                    .database
                    .with_context_budget(mode, cancellation, &budget, |session| {
                        session.load_rls_catalog()
                    })
                    .map_err(map_runtime)?;
                let mut policies = Vec::new();
                for (table, state) in catalog.tables() {
                    for policy in state.policies.values() {
                        policies.push(protocol::PolicyDefinition {
                            name: policy.name.as_str().to_owned(),
                            table: table.as_str().to_owned(),
                            kind: match policy.mode {
                                ffdb_sql_parser::PolicyMode::Permissive => {
                                    protocol::PolicyKind::Permissive
                                }
                                ffdb_sql_parser::PolicyMode::Restrictive => {
                                    protocol::PolicyKind::Restrictive
                                }
                            },
                            command: match policy.command {
                                ffdb_sql_parser::PolicyCommand::All => protocol::PolicyCommand::All,
                                ffdb_sql_parser::PolicyCommand::Select => {
                                    protocol::PolicyCommand::Select
                                }
                                ffdb_sql_parser::PolicyCommand::Insert => {
                                    protocol::PolicyCommand::Insert
                                }
                                ffdb_sql_parser::PolicyCommand::Update => {
                                    protocol::PolicyCommand::Update
                                }
                                ffdb_sql_parser::PolicyCommand::Delete => {
                                    protocol::PolicyCommand::Delete
                                }
                            },
                            roles: policy
                                .roles
                                .iter()
                                .map(|role| role.as_str().to_owned())
                                .collect(),
                            using_expression: policy
                                .using
                                .as_ref()
                                .map(|predicate| predicate.as_sql().to_owned()),
                            check_expression: policy
                                .with_check
                                .as_ref()
                                .map(|predicate| predicate.as_sql().to_owned()),
                            enabled: state.enabled,
                            forced: state.forced,
                        });
                    }
                }
                Ok(protocol::WorkerResponse::Policies(policies))
            }
            protocol::WorkerOperation::Backup { backup_id } => {
                require_developer(&request.mode)?;
                self.create_encrypted_backup(backup_id, cancellation, budget.deadline)
                    .map(protocol::WorkerResponse::Backup)
            }
            protocol::WorkerOperation::Restore { backup_id } => {
                require_developer(&request.mode)?;
                self.restore_encrypted_backup(
                    backup_id,
                    cancellation,
                    budget.deadline,
                    receipt.as_ref().map(|receipt| &receipt.record),
                )
                .map(protocol::WorkerResponse::Restore)
            }
            protocol::WorkerOperation::IntegrityCheck => {
                require_developer(&request.mode)?;
                let ok = self
                    .database
                    .integrity_check_bounded(cancellation, budget.deadline)
                    .map_err(map_runtime)?;
                Ok(protocol::WorkerResponse::Integrity(
                    protocol::IntegrityResult {
                        ok,
                        messages: if ok {
                            Vec::new()
                        } else {
                            vec!["integrity check failed".to_owned()]
                        },
                    },
                ))
            }
            protocol::WorkerOperation::StorageAuthorize(storage) => {
                let auth = require_end_user(&request.mode)?;
                let authorization = self
                    .metadata_authorizer()
                    .authorize_sync(&ffdb_object_storage::AuthorizationRequest {
                        auth,
                        bucket: storage.bucket,
                        object_key: storage.object_key,
                        action: convert_storage_action(storage.action),
                        content_length: storage.content_length,
                        checksum_sha256: storage.checksum_sha256,
                        content_type: storage.content_type,
                        upload_id: storage.upload_id,
                        part_number: storage.part_number,
                    })
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageAuthorization(
                    protocol::StorageAuthorization {
                        provider_key: authorization.provider_key,
                        scope_fingerprint: authorization.scope_fingerprint,
                        project_quota_bytes: authorization.project_quota_bytes,
                        current_project_bytes: authorization.current_project_bytes,
                        max_object_bytes: authorization.max_object_bytes,
                        reservation_bytes: authorization.reservation_bytes,
                        replacement_fingerprint: authorization.replacement_fingerprint,
                    },
                ))
            }
            protocol::WorkerOperation::StorageReserve(storage) => {
                let auth = require_end_user(&request.mode)?;
                self.metadata_authorizer()
                    .reserve_sync(&ffdb_object_storage::StorageReservationRequest {
                        auth,
                        nonce: storage.nonce,
                        bytes: storage.bytes,
                        expires_at_ms: storage.expires_at_ms,
                        provider_key: storage.provider_key.into_inner(),
                        action: convert_storage_action(storage.action),
                        upload_id: storage.upload_id,
                    })
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageAck)
            }
            protocol::WorkerOperation::StorageCommit(storage) => {
                let auth = require_end_user(&request.mode)?;
                self.metadata_authorizer()
                    .commit_sync(&ffdb_object_storage::StorageMetadataCommit {
                        auth,
                        bucket: storage.bucket,
                        object_key: storage.object_key,
                        provider_key: storage.provider_key,
                        action: convert_storage_action(storage.action),
                        content_length: storage.content_length,
                        checksum_sha256: storage.checksum_sha256,
                        content_type: storage.content_type,
                        upload_id: storage.upload_id,
                        part_number: storage.part_number,
                        etag: storage.etag,
                        version_id: storage.version_id,
                        reservation_nonce: storage.reservation_nonce,
                        reservation_bytes: storage.reservation_bytes,
                        reservation_expires_at_ms: storage.reservation_expires_at_ms,
                        replacement_fingerprint: storage.replacement_fingerprint,
                    })
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageAck)
            }
            protocol::WorkerOperation::StorageReceipt(storage) => {
                let auth = require_end_user(&request.mode)?;
                let receipt = self
                    .metadata_authorizer()
                    .receipt_sync(&ffdb_object_storage::StorageReceiptRequest {
                        auth,
                        bucket: storage.bucket,
                        object_key: storage.object_key,
                        provider_key: storage.provider_key,
                        action: convert_storage_action(storage.action),
                        content_length: storage.content_length,
                        checksum_sha256: storage.checksum_sha256,
                        content_type: storage.content_type,
                        upload_id: storage.upload_id,
                        part_number: storage.part_number,
                        reservation_nonce: storage.reservation_nonce,
                        reservation_bytes: storage.reservation_bytes,
                        reservation_expires_at_ms: storage.reservation_expires_at_ms,
                        replacement_fingerprint: storage.replacement_fingerprint,
                    })
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageReceipt(receipt.map(
                    |receipt| protocol::StorageCommitReceipt {
                        content_length: receipt.content_length,
                        checksum_sha256: receipt.checksum_sha256,
                        etag: receipt.etag,
                        version_id: receipt.version_id,
                    },
                )))
            }
            protocol::WorkerOperation::StorageUsage => {
                require_end_user(&request.mode)?;
                let now_ms = epoch_ms()?;
                self.database
                    .with_context_budget(mode.clone(), cancellation, &budget, |session| {
                        session.storage_usage(now_ms)
                    })
                    .map(|usage| {
                        protocol::WorkerResponse::StorageUsage(protocol::StorageUsageSnapshot {
                            current_bytes: usage.current_bytes,
                        })
                    })
                    .map_err(map_runtime)
            }
            protocol::WorkerOperation::StorageRelease(storage) => {
                let auth = require_end_user(&request.mode)?;
                self.metadata_authorizer()
                    .release_reservation_sync(
                        &auth,
                        &storage.nonce,
                        storage.reservation_bytes,
                        storage.reservation_expires_at_ms,
                    )
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageAck)
            }
            protocol::WorkerOperation::StorageList(storage) => {
                let auth = require_end_user(&request.mode)?;
                self.metadata_authorizer()
                    .list_sync(&auth, &storage)
                    .map(protocol::WorkerResponse::StorageObjects)
                    .map_err(map_storage)
            }
            protocol::WorkerOperation::StorageCleanup { now_ms } => {
                require_developer(&request.mode)?;
                let removed = self
                    .metadata_authorizer()
                    .cleanup_expired_reservations_sync(now_ms)
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageCleanup {
                    removed: u64::try_from(removed).unwrap_or(u64::MAX),
                })
            }
            protocol::WorkerOperation::StorageCleanupClaim(storage) => {
                require_developer(&request.mode)?;
                self.metadata_authorizer()
                    .cleanup_claim_sync(storage.now_ms, storage.limit)
                    .map(protocol::WorkerResponse::StorageCleanupBatch)
                    .map_err(map_storage)
            }
            protocol::WorkerOperation::StorageCleanupAck(storage) => {
                require_developer(&request.mode)?;
                let (removed, retried) = self
                    .metadata_authorizer()
                    .cleanup_ack_sync(storage.now_ms, storage.items)
                    .map_err(map_storage)?;
                Ok(protocol::WorkerResponse::StorageCleanupAck { removed, retried })
            }
            protocol::WorkerOperation::StorageBuckets => {
                require_developer(&request.mode)?;
                let result = self
                    .database
                    .with_context_budget(mode, cancellation, &budget, |session| {
                        session.execute(&runtime::StatementRequest {
                            sql: "SELECT id,name,owner_id,public,max_object_bytes,\
                                  project_quota_bytes,created_at_ms FROM storage_buckets ORDER BY name,id"
                                .to_owned(),
                            parameters: Vec::new(),
                        })
                    })
                    .map_err(map_runtime)?;
                convert_storage_buckets(&result).map(protocol::WorkerResponse::StorageBuckets)
            }
            protocol::WorkerOperation::StorageCreateBucket(bucket) => {
                require_developer(&request.mode)?;
                if Uuid::parse_str(&bucket.id).is_err()
                    || !valid_bucket_name(&bucket.name)
                    || bucket.max_object_bytes == 0
                    || bucket.project_quota_bytes < bucket.max_object_bytes
                {
                    return Err(failure(
                        "storage.invalid_bucket",
                        "storage bucket metadata is invalid",
                    ));
                }
                let owner_id = bucket
                    .owner_id
                    .map_or_else(|| Uuid::nil().to_string(), |owner| owner.to_string());
                let max_object_bytes = i64::try_from(bucket.max_object_bytes).map_err(|_| {
                    failure("storage.invalid_bucket", "storage bucket quota is invalid")
                })?;
                let project_quota_bytes =
                    i64::try_from(bucket.project_quota_bytes).map_err(|_| {
                        failure("storage.invalid_bucket", "storage bucket quota is invalid")
                    })?;
                self.database
                    .with_context_budget(mode, cancellation, &budget, |session| {
                        let _ = session.execute(&runtime::StatementRequest {
                            sql: "INSERT INTO storage_buckets \
                                  (id,name,owner_id,public,max_object_bytes,project_quota_bytes,created_at_ms) \
                                  VALUES (?1,?2,?3,?4,?5,?6,?7)"
                                .to_owned(),
                            parameters: vec![
                                runtime::SqlParameter::Text(bucket.id.clone()),
                                runtime::SqlParameter::Text(bucket.name.clone()),
                                runtime::SqlParameter::Text(owner_id.clone()),
                                runtime::SqlParameter::Integer(i64::from(bucket.public)),
                                runtime::SqlParameter::Integer(max_object_bytes),
                                runtime::SqlParameter::Integer(project_quota_bytes),
                                runtime::SqlParameter::Integer(bucket.created_at_ms),
                            ],
                        })?;
                        Ok(())
                    })
                    .map_err(map_runtime)?;
                Ok(protocol::WorkerResponse::StorageBucket(
                    protocol::StorageBucket {
                        id: bucket.id,
                        name: bucket.name,
                        owner_id,
                        public: bucket.public,
                        max_object_bytes: bucket.max_object_bytes,
                        project_quota_bytes: bucket.project_quota_bytes,
                        created_at_ms: bucket.created_at_ms,
                    },
                ))
            }
        }?;
        if let Some(receipt) = receipt {
            self.complete_operation_receipt(receipt, &response)?;
        }
        self.unmetered_execution(
            request.request_id,
            &request.mode,
            response,
            cancellation,
            &budget,
        )
    }

    fn execute_metered_query(
        &self,
        request: &protocol::WorkerRequest,
        query: &protocol::QueryRequest,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        query.validate(&request.limits).map_err(map_validation)?;
        let runtime_request = convert_query(query)?;
        let (reads, writes) = statement_usage_units(&query.sql)?;
        self.execute_metered_operation(request, writes > 0, cancellation, budget, move |session| {
            let started = Instant::now();
            let result = session.execute(&runtime_request)?;
            let telemetry = protocol::WorkerStatementTelemetry {
                ordinal: 0,
                duration_ms: started.elapsed().as_secs_f64() * 1_000.0,
                rows_returned: u64::try_from(result.rows.len()).unwrap_or(u64::MAX),
                rows_affected: result.affected_rows,
            };
            Ok((
                protocol::WorkerResponse::Query(convert_result(result, query.options.max_rows)),
                reads,
                writes,
                vec![telemetry],
            ))
        })
    }

    fn execute_metered_transaction(
        &self,
        request: &protocol::WorkerRequest,
        transaction: &protocol::TransactionRequest,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        transaction
            .validate(&request.limits)
            .map_err(map_validation)?;
        let statements = transaction
            .statements
            .iter()
            .map(convert_query)
            .collect::<Result<Vec<_>, _>>()?;
        let (reads, writes) =
            transaction
                .statements
                .iter()
                .try_fold((0_u64, 0_u64), |(reads, writes), query| {
                    let (next_reads, next_writes) = statement_usage_units(&query.sql)?;
                    Ok::<_, WorkerFailure>((reads + next_reads, writes + next_writes))
                })?;
        self.execute_metered_operation(request, writes > 0, cancellation, budget, move |session| {
            let (results, durations) =
                session.transaction_in_current_atomic_observed(&statements)?;
            let statement_telemetry = results
                .iter()
                .zip(&durations)
                .enumerate()
                .map(
                    |(ordinal, (result, duration))| protocol::WorkerStatementTelemetry {
                        ordinal: u16::try_from(ordinal).unwrap_or(u16::MAX),
                        duration_ms: duration.as_secs_f64() * 1_000.0,
                        rows_returned: u64::try_from(result.rows.len()).unwrap_or(u64::MAX),
                        rows_affected: result.affected_rows,
                    },
                )
                .collect();
            let response = results
                .into_iter()
                .zip(&transaction.statements)
                .map(|(result, query)| convert_result(result, query.options.max_rows))
                .collect();
            Ok((
                protocol::WorkerResponse::Transaction(response),
                reads,
                writes,
                statement_telemetry,
            ))
        })
    }

    fn execute_metered_snapshot(
        &self,
        request: &protocol::WorkerRequest,
        snapshot: &protocol::SnapshotRequest,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        self.execute_metered_operation(request, false, cancellation, budget, move |session| {
            let result = session.sync_snapshot(snapshot.tables.as_deref())?;
            Ok((
                protocol::WorkerResponse::Snapshot(protocol::SnapshotResponse {
                    schema_version: result.schema_version,
                    cursor: result.cursor,
                    tables: result
                        .tables
                        .into_iter()
                        .map(|(table, result)| (table, convert_result(result, u32::MAX)))
                        .collect(),
                }),
                1,
                0,
                Vec::new(),
            ))
        })
    }

    fn execute_metered_sync_pull(
        &self,
        request: &protocol::WorkerRequest,
        pull: &protocol::SyncPullRequest,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        let limit = usize::try_from(pull.limit).unwrap_or(usize::MAX);
        self.execute_metered_operation(request, false, cancellation, budget, move |session| {
            let result = match session.sync_pull(pull.cursor.as_deref(), limit) {
                Ok(result) => Ok(result),
                Err(runtime::RuntimeError::SyncCursorInvalid) => {
                    Err((session.sync_current_cursor()?, session.schema_version()?))
                }
                Err(error) => return Err(error),
            };
            let response = match result {
                Ok(result) => protocol::SyncPullResponse {
                    changes: result
                        .changes
                        .into_iter()
                        .map(convert_sync_change)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| runtime::RuntimeError::UsageReceiptInvalid)?,
                    cursor: result.cursor,
                    has_more: result.has_more,
                    control: None,
                },
                Err((cursor, schema_version)) => protocol::SyncPullResponse {
                    changes: Vec::new(),
                    cursor,
                    has_more: false,
                    control: Some(protocol::SyncControl::ResnapshotRequired {
                        reason: "cursor_expired_or_authorization_changed".to_owned(),
                        minimum_schema_version: schema_version,
                    }),
                },
            };
            Ok((protocol::WorkerResponse::Sync(response), 1, 0, Vec::new()))
        })
    }

    fn execute_metered_sync_push(
        &self,
        request: &protocol::WorkerRequest,
        push: &protocol::SyncPushRequest,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        let mutations = push
            .mutations
            .iter()
            .map(convert_sync_mutation)
            .collect::<Result<Vec<_>, _>>()?;
        if mutations.is_empty() || mutations.len() > 100 {
            return Err(failure(
                "sync.invalid_batch",
                "sync mutation batch size is invalid",
            ));
        }
        self.execute_metered_operation(request, true, cancellation, budget, move |session| {
            let (receipts, cursor) = session.sync_apply_mutations_individually_in_current_atomic(
                push.schema_version,
                &mutations,
            )?;
            let writes = receipts
                .iter()
                .filter(|receipt| receipt.as_ref().is_ok_and(|receipt| !receipt.duplicate))
                .count() as u64;
            let results = receipts
                .into_iter()
                .zip(&push.mutations)
                .map(|(receipt, mutation)| match receipt {
                    Ok(receipt) => protocol::SyncMutationResult {
                        mutation_id: receipt.mutation_id,
                        status: if receipt.duplicate {
                            protocol::MutationStatus::Duplicate
                        } else {
                            protocol::MutationStatus::Applied
                        },
                        server_sequence: Some(receipt.sequence),
                        row_version: Some(receipt.row_version),
                        error_code: None,
                    },
                    Err(error) => protocol::SyncMutationResult {
                        mutation_id: mutation.mutation_id.clone(),
                        status: protocol::MutationStatus::Rejected,
                        server_sequence: None,
                        row_version: None,
                        error_code: Some(sync_rejection_code(&error).to_owned()),
                    },
                })
                .collect();
            Ok((
                protocol::WorkerResponse::SyncPush(protocol::SyncPushResponse { results, cursor }),
                0,
                writes,
                Vec::new(),
            ))
        })
    }

    fn execute_metered_operation(
        &self,
        request: &protocol::WorkerRequest,
        mutating: bool,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
        execute: impl FnOnce(
            &mut runtime::Session<'_>,
        ) -> Result<
            (
                protocol::WorkerResponse,
                u64,
                u64,
                Vec<protocol::WorkerStatementTelemetry>,
            ),
            runtime::RuntimeError,
        >,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        let request_id = request.request_id;
        let receipt_id = request.operation_receipt_id.unwrap_or(request_id.0);
        let request_id_text = request_id.to_string();
        let receipt_id_text = receipt_id.to_string();
        let request_digest =
            usage_receipt_digest(&request.route, &request.mode, &request.operation)?;
        let subject = usage_subject(&request.mode);
        let subject_text = subject.map(|subject| subject.to_string());
        let recorded_at_ms = epoch_ms()?;
        self.database
            .with_context_budget(
                convert_mode(&request.mode),
                cancellation,
                budget,
                move |session| {
                    session.atomic(move |session| {
                        if let Some(stored) = session.usage_receipt(&receipt_id_text)? {
                            if stored.request_digest != request_digest {
                                return Err(runtime::RuntimeError::UsageReceiptConflict);
                            }
                            let usage = protocol_usage_from_stored(receipt_id, &stored)?;
                            if mutating {
                                let response = stored
                                    .response_json
                                    .as_deref()
                                    .ok_or(runtime::RuntimeError::UsageReceiptInvalid)
                                    .and_then(|json| {
                                        serde_json::from_str(json)
                                            .map_err(|_| runtime::RuntimeError::UsageReceiptInvalid)
                                    })?;
                                return Ok(protocol::WorkerExecution {
                                    response,
                                    usage,
                                    statement_telemetry: Vec::new(),
                                });
                            }
                            let (response, _, _, statement_telemetry) = execute(session)?;
                            return Ok(protocol::WorkerExecution {
                                response,
                                usage,
                                statement_telemetry,
                            });
                        }

                        let (response, reads, writes, statement_telemetry) = execute(session)?;
                        let response_json = if mutating {
                            Some(
                                serde_json::to_string(&response)
                                    .map_err(|_| runtime::RuntimeError::UsageReceiptInvalid)?,
                            )
                        } else {
                            None
                        };
                        let logical_database_bytes =
                            session.store_usage_receipt(&runtime::UsageReceiptInsert {
                                receipt_id: &receipt_id_text,
                                request_id: &request_id_text,
                                request_digest: &request_digest,
                                response_json: response_json.as_deref(),
                                reads,
                                writes,
                                subject: subject_text.as_deref(),
                                recorded_at_ms,
                            })?;
                        Ok(protocol::WorkerExecution {
                            response,
                            usage: protocol::UsageReceipt {
                                receipt_id,
                                request_id,
                                reads,
                                writes,
                                logical_database_bytes,
                                subject,
                                recorded_at_ms,
                            },
                            statement_telemetry,
                        })
                    })
                },
            )
            .map_err(map_runtime)
    }

    fn unmetered_execution(
        &self,
        request_id: protocol::RequestId,
        mode: &protocol::ExecutionMode,
        response: protocol::WorkerResponse,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<protocol::WorkerExecution, WorkerFailure> {
        let logical_database_bytes = self
            .database
            .with_context_budget(convert_mode(mode), cancellation, budget, |session| {
                session.logical_database_bytes()
            })
            .map_err(map_runtime)?;
        Ok(protocol::WorkerExecution {
            response,
            usage: protocol::UsageReceipt {
                receipt_id: request_id.0,
                request_id,
                reads: 0,
                writes: 0,
                logical_database_bytes,
                subject: usage_subject(mode),
                recorded_at_ms: epoch_ms()?,
            },
            statement_telemetry: Vec::new(),
        })
    }

    fn begin_operation_receipt(
        &self,
        receipt_id: Uuid,
        mode: &protocol::ExecutionMode,
        operation: &protocol::WorkerOperation,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<ReceiptAdmission, WorkerFailure> {
        let operation_name = receipt_operation_name(operation).ok_or_else(|| {
            failure(
                "idempotency.receipt_not_supported",
                "operation does not support a durable worker receipt",
            )
        })?;
        if self
            .receipt_maintenance_counter
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(256)
        {
            cleanup_stale_receipts(&self.receipt_root, 256)?;
        }
        let request_digest = operation_receipt_digest(&self.route, mode, operation)?;
        let path = self.receipt_root.join(format!("{receipt_id}.json"));
        if !path.exists() {
            self.validate_new_receipt_preconditions(operation, cancellation, budget)?;
        }
        let record = OperationReceipt {
            version: 1,
            receipt_id,
            project_id: self.route.project_id,
            database_id: self.route.database_id,
            operation: operation_name.to_owned(),
            request_digest,
            state: OperationReceiptState::Started,
        };
        let encoded = serde_json::to_vec(&record).map_err(|_| receipt_unavailable())?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&encoded)
                    .map_err(|_| receipt_unavailable())?;
                file.sync_all().map_err(|_| receipt_unavailable())?;
                sync_directory(&self.receipt_root)?;
                self.initialize_operation_receipt(&record)?;
                Ok(ReceiptAdmission::Owner(ReceiptOwner { path, record }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stored = read_operation_receipt(&path)?;
                if stored.version != record.version
                    || stored.receipt_id != record.receipt_id
                    || stored.project_id != record.project_id
                    || stored.database_id != record.database_id
                    || stored.operation != record.operation
                    || stored.request_digest != record.request_digest
                {
                    return Err(failure(
                        "idempotency.receipt_conflict",
                        "worker operation receipt belongs to another request",
                    ));
                }
                match stored.state {
                    OperationReceiptState::Completed { response } => {
                        Ok(ReceiptAdmission::Replay(*response))
                    }
                    OperationReceiptState::Started => {
                        let owner = ReceiptOwner {
                            path,
                            record: stored,
                        };
                        match self.reconcile_started_receipt(
                            &owner.record,
                            mode,
                            operation,
                            cancellation,
                            budget,
                        )? {
                            Some(response) => {
                                self.complete_operation_receipt(owner, &response)?;
                                Ok(ReceiptAdmission::Replay(response))
                            }
                            None => Ok(ReceiptAdmission::Owner(owner)),
                        }
                    }
                }
            }
            Err(_) => Err(receipt_unavailable()),
        }
    }

    fn complete_operation_receipt(
        &self,
        mut owner: ReceiptOwner,
        response: &protocol::WorkerResponse,
    ) -> Result<(), WorkerFailure> {
        owner.record.state = OperationReceiptState::Completed {
            response: Box::new(response.clone()),
        };
        let encoded = serde_json::to_vec(&owner.record).map_err(|_| receipt_unavailable())?;
        if encoded.len() as u64 > MAX_OPERATION_RECEIPT_BYTES {
            return Err(receipt_unavailable());
        }
        let temporary = self.receipt_root.join(format!(
            ".{}.{}.tmp",
            owner.record.receipt_id,
            Uuid::now_v7()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|_| receipt_unavailable())?;
            file.write_all(&encoded)
                .map_err(|_| receipt_unavailable())?;
            file.sync_all().map_err(|_| receipt_unavailable())?;
            fs::rename(&temporary, &owner.path).map_err(|_| receipt_unavailable())?;
            sync_directory(&self.receipt_root)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn initialize_operation_receipt(
        &self,
        receipt: &OperationReceipt,
    ) -> Result<(), WorkerFailure> {
        if matches!(
            receipt.operation.as_str(),
            "migration.apply" | "migration.rollback"
        ) {
            self.database
                .start_worker_operation_receipt(
                    &receipt.receipt_id.to_string(),
                    &receipt.request_digest,
                    &receipt.operation,
                    epoch_ms()?,
                )
                .map_err(map_runtime)?;
        }
        Ok(())
    }

    fn validate_new_receipt_preconditions(
        &self,
        operation: &protocol::WorkerOperation,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<(), WorkerFailure> {
        match operation {
            protocol::WorkerOperation::ApplyMigration(specification) => {
                specification.validate().map_err(map_validation)?;
            }
            protocol::WorkerOperation::RollbackMigration { migration_id } => {
                let stored = self
                    .database
                    .with_context_budget(
                        runtime::ExecutionMode::Developer(runtime::DeveloperPrincipal {
                            actor_id: "ffdb-receipt".to_owned(),
                            api_key_id: "ffdb-receipt".to_owned(),
                        }),
                        cancellation,
                        budget,
                        |session| session.migration_record(migration_id),
                    )
                    .map_err(map_runtime)?;
                if stored
                    .as_ref()
                    .is_none_or(|record| record.status != "applied")
                {
                    return Err(failure("migration.not_applied", "migration is not applied"));
                }
            }
            protocol::WorkerOperation::Backup { backup_id } => {
                if self
                    .backup_root
                    .join(format!("{backup_id}.sqlite3"))
                    .exists()
                {
                    return Err(failure(
                        "backup.already_exists",
                        "backup identifier already exists",
                    ));
                }
            }
            protocol::WorkerOperation::Restore { backup_id } => {
                if !self
                    .backup_root
                    .join(format!("{backup_id}.sqlite3"))
                    .is_file()
                {
                    return Err(failure("backup.not_found", "backup does not exist"));
                }
            }
            _ => {
                return Err(failure(
                    "idempotency.receipt_not_supported",
                    "operation does not support a durable worker receipt",
                ));
            }
        }
        Ok(())
    }

    fn reconcile_started_receipt(
        &self,
        receipt: &OperationReceipt,
        mode: &protocol::ExecutionMode,
        operation: &protocol::WorkerOperation,
        cancellation: &runtime::CancellationToken,
        budget: &runtime::RequestBudget,
    ) -> Result<Option<protocol::WorkerResponse>, WorkerFailure> {
        self.initialize_operation_receipt(receipt)?;
        match operation {
            protocol::WorkerOperation::ApplyMigration(specification) => {
                let Some(outcome) = self.reconcile_migration_outcome(receipt)? else {
                    return Ok(None);
                };
                Ok(Some(protocol::WorkerResponse::Migration(
                    migration_response_from_outcome(
                        specification.clone(),
                        protocol::MigrationStatus::Applied,
                        outcome,
                        developer_api_key(mode)?,
                    )?,
                )))
            }
            protocol::WorkerOperation::RollbackMigration { migration_id } => {
                let Some(outcome) = self.reconcile_migration_outcome(receipt)? else {
                    return Ok(None);
                };
                let stored = self
                    .database
                    .with_context_budget(
                        runtime::ExecutionMode::Developer(runtime::DeveloperPrincipal {
                            actor_id: "ffdb-receipt".to_owned(),
                            api_key_id: "ffdb-receipt".to_owned(),
                        }),
                        cancellation,
                        budget,
                        |session| session.migration_record(migration_id),
                    )
                    .map_err(map_runtime)?
                    .ok_or_else(receipt_outcome_unknown)?;
                Ok(Some(protocol::WorkerResponse::Migration(
                    migration_response_from_outcome(
                        protocol_migration_spec(stored),
                        protocol::MigrationStatus::RolledBack,
                        outcome,
                        developer_api_key(mode)?,
                    )?,
                )))
            }
            protocol::WorkerOperation::Backup { backup_id } => {
                let path = self.backup_root.join(format!("{backup_id}.sqlite3"));
                if !path.exists() {
                    return Ok(None);
                }
                let (size_bytes, sha256) = hash_file_bounded(&path, cancellation, budget.deadline)?;
                Ok(Some(protocol::WorkerResponse::Backup(
                    protocol::BackupResult {
                        backup_id: *backup_id,
                        size_bytes,
                        sha256,
                    },
                )))
            }
            protocol::WorkerOperation::Restore { backup_id } => {
                let marker = self.database.restore_receipt().map_err(map_runtime)?;
                match marker {
                    Some(marker)
                        if marker.receipt_id == receipt.receipt_id.to_string()
                            && marker.request_digest == receipt.request_digest
                            && marker.backup_id == backup_id.to_string() =>
                    {
                        Ok(Some(protocol::WorkerResponse::Restore(
                            protocol::RestoreResult {
                                backup_id: *backup_id,
                                integrity_ok: true,
                                schema_version: marker.schema_version,
                            },
                        )))
                    }
                    Some(_) => Err(receipt_outcome_unknown()),
                    None => Ok(None),
                }
            }
            _ => Err(failure(
                "idempotency.receipt_not_supported",
                "operation does not support a durable worker receipt",
            )),
        }
    }

    fn reconcile_migration_outcome(
        &self,
        receipt: &OperationReceipt,
    ) -> Result<Option<MigrationOutcome>, WorkerFailure> {
        let stored = self
            .database
            .worker_operation_receipt(&receipt.receipt_id.to_string())
            .map_err(map_runtime)?
            .ok_or_else(receipt_outcome_unknown)?;
        if stored.request_digest != receipt.request_digest || stored.operation != receipt.operation
        {
            return Err(receipt_outcome_unknown());
        }
        stored
            .result_json
            .map(|result| serde_json::from_str(&result).map_err(|_| receipt_outcome_unknown()))
            .transpose()
    }

    fn validate_envelope(&self, request: &protocol::WorkerRequest) -> Result<(), WorkerFailure> {
        if request.protocol_version != protocol::PROTOCOL_VERSION {
            return Err(failure(
                "internal.protocol_version",
                "worker protocol version is not supported",
            ));
        }
        if request.route != self.route {
            return Err(failure(
                "project.stale_route",
                "database route is stale or belongs to another worker",
            ));
        }
        if let protocol::ExecutionMode::EndUser(context) = &request.mode
            && context.project_id != request.route.project_id
        {
            return Err(failure(
                "auth.project_mismatch",
                "verified auth context does not belong to the routed project",
            ));
        }
        request.limits.validate().map_err(map_validation)?;
        if request.deadline_epoch_ms <= epoch_ms()? {
            return Err(failure(
                "query.deadline_exceeded",
                "request deadline was exceeded",
            ));
        }
        Ok(())
    }
}

fn request_budget(
    request: &protocol::WorkerRequest,
) -> Result<runtime::RequestBudget, WorkerFailure> {
    let remaining_ms = request.deadline_epoch_ms.saturating_sub(epoch_ms()?);
    let remaining_ms = u64::try_from(remaining_ms)
        .map_err(|_| failure("query.deadline_exceeded", "request deadline was exceeded"))?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(remaining_ms))
        .ok_or_else(|| failure("query.deadline_exceeded", "request deadline is invalid"))?;
    Ok(runtime::RequestBudget {
        limits: runtime::ExecutionLimits {
            max_sql_bytes: usize::try_from(request.limits.max_sql_bytes).unwrap_or(usize::MAX),
            max_variables: usize::try_from(request.limits.max_variables).unwrap_or(usize::MAX),
            max_rows: usize::try_from(request.limits.max_result_rows).unwrap_or(usize::MAX),
            max_response_bytes: usize::try_from(request.limits.max_response_bytes)
                .unwrap_or(usize::MAX),
            statement_timeout: Duration::from_millis(request.limits.statement_timeout_ms),
            transaction_timeout: Duration::from_millis(request.limits.transaction_timeout_ms),
            max_database_bytes: request.limits.max_database_bytes,
            progress_ops: 1_000,
        },
        deadline,
    })
}

pub fn serve_frames(
    worker: &DatabaseWorker,
    mut input: impl Read,
    mut output: impl Write,
) -> Result<(), WorkerFailure> {
    loop {
        let mut length = [0_u8; 4];
        match input.read_exact(&mut length) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(_) => return Err(failure("internal.ipc_read", "worker IPC read failed")),
        }
        let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(failure(
                "internal.ipc_frame_too_large",
                "worker IPC frame is invalid",
            ));
        }
        let mut frame = vec![0_u8; length];
        input
            .read_exact(&mut frame)
            .map_err(|_| failure("internal.ipc_read", "worker IPC read failed"))?;
        let request: protocol::WorkerRequest = serde_json::from_slice(&frame)
            .map_err(|_| failure("internal.ipc_decode", "worker IPC request is invalid"))?;
        let request_id = request.request_id;
        let response = match worker.handle(request, &runtime::CancellationToken::default()) {
            Ok(response) => WireResponse::Ok(response),
            Err(error) => WireResponse::Error(protocol::PlatformError::safe(
                error.code,
                error.message,
                request_id,
            )),
        };
        let payload = serde_json::to_vec(&response)
            .map_err(|_| failure("internal.ipc_encode", "worker IPC response failed"))?;
        if payload.len() > MAX_FRAME_BYTES {
            return Err(failure(
                "internal.ipc_frame_too_large",
                "worker response is too large",
            ));
        }
        let length = u32::try_from(payload.len()).map_err(|_| {
            failure(
                "internal.ipc_frame_too_large",
                "worker response is too large",
            )
        })?;
        output
            .write_all(&length.to_be_bytes())
            .map_err(|_| failure("internal.ipc_write", "worker IPC write failed"))?;
        output
            .write_all(&payload)
            .map_err(|_| failure("internal.ipc_write", "worker IPC write failed"))?;
        output
            .flush()
            .map_err(|_| failure("internal.ipc_write", "worker IPC write failed"))?;
    }
}

fn convert_mode(mode: &protocol::ExecutionMode) -> runtime::ExecutionMode {
    match mode {
        protocol::ExecutionMode::Developer(developer) => {
            runtime::ExecutionMode::Developer(runtime::DeveloperPrincipal {
                actor_id: developer.actor_label.clone(),
                api_key_id: developer.api_key_id.to_string(),
            })
        }
        protocol::ExecutionMode::EndUser(context) => {
            runtime::ExecutionMode::EndUser(runtime::AuthContext {
                project_id: context.project_id.to_string(),
                subject: context.subject.to_string(),
                role: context.role.clone(),
                claims: context.claims.clone(),
                token_id: context.token_id.to_string(),
            })
        }
    }
}

fn require_developer(
    mode: &protocol::ExecutionMode,
) -> Result<runtime::DeveloperPrincipal, WorkerFailure> {
    match convert_mode(mode) {
        runtime::ExecutionMode::Developer(principal) => Ok(principal),
        runtime::ExecutionMode::EndUser(_) => Err(failure(
            "auth.developer_required",
            "developer credentials are required",
        )),
    }
}

fn require_end_user(
    mode: &protocol::ExecutionMode,
) -> Result<protocol::AuthContext, WorkerFailure> {
    match mode {
        protocol::ExecutionMode::EndUser(auth) => Ok(auth.clone()),
        protocol::ExecutionMode::Developer(_) => Err(failure(
            "auth.end_user_required",
            "verified end-user context is required",
        )),
    }
}

fn convert_storage_action(action: protocol::StorageAction) -> ffdb_object_storage::StorageAction {
    match action {
        protocol::StorageAction::Upload => ffdb_object_storage::StorageAction::Upload,
        protocol::StorageAction::Download => ffdb_object_storage::StorageAction::Download,
        protocol::StorageAction::Delete => ffdb_object_storage::StorageAction::Delete,
        protocol::StorageAction::List => ffdb_object_storage::StorageAction::List,
        protocol::StorageAction::CreateMultipart => {
            ffdb_object_storage::StorageAction::CreateMultipart
        }
        protocol::StorageAction::UploadPart => ffdb_object_storage::StorageAction::UploadPart,
        protocol::StorageAction::CompleteMultipart => {
            ffdb_object_storage::StorageAction::CompleteMultipart
        }
        protocol::StorageAction::AbortMultipart => {
            ffdb_object_storage::StorageAction::AbortMultipart
        }
    }
}

fn developer_api_key(mode: &protocol::ExecutionMode) -> Result<protocol::ApiKeyId, WorkerFailure> {
    match mode {
        protocol::ExecutionMode::Developer(principal) => Ok(principal.api_key_id),
        protocol::ExecutionMode::EndUser(_) => Err(failure(
            "auth.developer_required",
            "developer credentials are required",
        )),
    }
}

fn convert_query(
    query: &protocol::QueryRequest,
) -> Result<runtime::StatementRequest, WorkerFailure> {
    let parameters = query
        .parameters
        .iter()
        .map(|parameter| match parameter {
            protocol::SqlParameter::Null => Ok(runtime::SqlParameter::Null),
            protocol::SqlParameter::Integer(value) => value
                .as_i64()
                .map(runtime::SqlParameter::Integer)
                .map_err(map_validation),
            protocol::SqlParameter::Real(value) if value.is_finite() => {
                Ok(runtime::SqlParameter::Real(*value))
            }
            protocol::SqlParameter::Real(_) => Err(failure(
                "query.invalid_parameter",
                "floating-point parameter must be finite",
            )),
            protocol::SqlParameter::Text(value) => Ok(runtime::SqlParameter::Text(value.clone())),
            protocol::SqlParameter::Blob(value) => BASE64
                .decode(value)
                .map(runtime::SqlParameter::Blob)
                .map_err(|_| {
                    failure(
                        "query.invalid_parameter",
                        "blob parameter is not valid base64",
                    )
                }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(runtime::StatementRequest {
        sql: query.sql.clone(),
        parameters,
    })
}

fn convert_result(mut result: runtime::QueryResult, max_rows: u32) -> protocol::QueryResult {
    let max_rows = usize::try_from(max_rows).unwrap_or(usize::MAX);
    if result.rows.len() > max_rows {
        result.rows.truncate(max_rows);
        result.truncated = true;
    }
    protocol::QueryResult {
        columns: result
            .columns
            .into_iter()
            .map(|column| protocol::ColumnMetadata {
                name: column.name,
                declared_type: declared_type(column.declared_type.as_deref()),
                origin_table: None,
            })
            .collect(),
        rows: result
            .rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|value| match value {
                        runtime::ResultValue::Null => protocol::ResultCell::Null(()),
                        runtime::ResultValue::Integer(value) => {
                            protocol::ResultCell::SafeInteger(value)
                        }
                        runtime::ResultValue::IntegerString(value)
                        | runtime::ResultValue::Text(value) => protocol::ResultCell::Text(value),
                        runtime::ResultValue::Real(value) => protocol::ResultCell::Real(value),
                        runtime::ResultValue::Blob { data } => {
                            protocol::ResultCell::Blob(protocol::BlobValue { base64: data })
                        }
                    })
                    .collect()
            })
            .collect(),
        affected_rows: result.affected_rows,
        last_insert_rowid: result.last_insert_rowid,
        truncated: result.truncated,
    }
}

fn convert_sync_mutation(
    mutation: &protocol::SyncMutation,
) -> Result<runtime::SyncMutation, WorkerFailure> {
    Ok(runtime::SyncMutation {
        mutation_id: mutation.mutation_id.clone(),
        table: mutation.table.clone(),
        primary_key: mutation.primary_key.clone(),
        operation: match mutation.operation {
            protocol::ChangeOperation::Insert | protocol::ChangeOperation::Update => {
                runtime::SyncMutationOperation::Upsert
            }
            protocol::ChangeOperation::Delete => runtime::SyncMutationOperation::Delete,
        },
        values: mutation.values.clone().unwrap_or_default(),
        base_row_version: mutation.base_row_version,
        client_timestamp_ms: mutation.client_timestamp_ms,
    })
}

fn convert_sync_change(
    change: runtime::SyncChange,
) -> Result<protocol::LogicalChange, WorkerFailure> {
    let transaction = uuid::Uuid::parse_str(&change.transaction_id).map_err(|_| {
        failure(
            "internal.sync_log_corrupt",
            "sync change log contains an invalid transaction identifier",
        )
    })?;
    let actor = uuid::Uuid::parse_str(&change.actor)
        .ok()
        .map(protocol::UserId);
    Ok(protocol::LogicalChange {
        sequence: change.sequence,
        transaction_id: protocol::TransactionId(transaction),
        table: change.table,
        primary_key: change.primary_key,
        operation: match change.operation {
            runtime::SyncChangeOperation::Insert => protocol::ChangeOperation::Insert,
            runtime::SyncChangeOperation::Update => protocol::ChangeOperation::Update,
            runtime::SyncChangeOperation::Delete => protocol::ChangeOperation::Delete,
        },
        row_version: change.row_version,
        values: change.values,
        tombstone: change.tombstone,
        actor,
        schema_version: change.schema_version,
        committed_at_ms: change.committed_at_ms,
        client_mutation_id: change.client_mutation_id,
    })
}

fn declared_type(value: Option<&str>) -> protocol::DeclaredColumnType {
    let value = value.unwrap_or_default().to_ascii_uppercase();
    if value.contains("TIMESTAMP") {
        protocol::DeclaredColumnType::Timestamp
    } else if value == "DATE" {
        protocol::DeclaredColumnType::Date
    } else if value.contains("INT") {
        protocol::DeclaredColumnType::Integer
    } else if value.contains("CHAR") || value.contains("CLOB") || value.contains("TEXT") {
        protocol::DeclaredColumnType::Text
    } else if value.contains("BLOB") {
        protocol::DeclaredColumnType::Blob
    } else if value.contains("REAL") || value.contains("FLOA") || value.contains("DOUB") {
        protocol::DeclaredColumnType::Real
    } else {
        protocol::DeclaredColumnType::Unknown
    }
}

fn convert_storage_buckets(
    result: &runtime::QueryResult,
) -> Result<Vec<protocol::StorageBucket>, WorkerFailure> {
    result
        .rows
        .iter()
        .map(|row| {
            Ok(protocol::StorageBucket {
                id: runtime_text(row.first())?,
                name: runtime_text(row.get(1))?,
                owner_id: runtime_text(row.get(2))?,
                public: runtime_i64(row.get(3))? != 0,
                max_object_bytes: u64::try_from(runtime_i64(row.get(4))?)
                    .map_err(|_| failure("internal.storage", "storage bucket quota is invalid"))?,
                project_quota_bytes: u64::try_from(runtime_i64(row.get(5))?)
                    .map_err(|_| failure("internal.storage", "storage bucket quota is invalid"))?,
                created_at_ms: runtime_i64(row.get(6))?,
            })
        })
        .collect()
}

fn runtime_text(value: Option<&runtime::ResultValue>) -> Result<String, WorkerFailure> {
    match value {
        Some(runtime::ResultValue::Text(value)) => Ok(value.clone()),
        _ => Err(failure("internal.storage", "storage metadata is invalid")),
    }
}

fn runtime_i64(value: Option<&runtime::ResultValue>) -> Result<i64, WorkerFailure> {
    match value {
        Some(runtime::ResultValue::Integer(value)) => Ok(*value),
        Some(runtime::ResultValue::IntegerString(value)) => value
            .parse()
            .map_err(|_| failure("internal.storage", "storage metadata is invalid")),
        _ => Err(failure("internal.storage", "storage metadata is invalid")),
    }
}

fn valid_bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !name.contains("..")
        && !name.contains(".-")
        && !name.contains("-.")
        && name.parse::<std::net::IpAddr>().is_err()
        && !name.starts_with("xn--")
        && !name.ends_with("-s3alias")
        && !name.ends_with("--ol-s3")
        && !name.ends_with(".mrap")
        && !name.ends_with("--x-s3")
        && !name.ends_with("--table-s3")
}

fn convert_migration_spec(specification: &protocol::MigrationSpec) -> EngineMigrationSpec {
    EngineMigrationSpec {
        id: specification.id.clone(),
        name: specification.name.clone(),
        up_sql: specification.up_sql.clone(),
        down_sql: specification.down_sql.clone(),
        checksum: specification.checksum.clone(),
        created_at_ms: specification.created_at_ms,
    }
}

fn durable_migration_receipt(receipt: &ReceiptOwner) -> DurableOperationReceipt {
    DurableOperationReceipt {
        receipt_id: receipt.record.receipt_id.to_string(),
        request_digest: receipt.record.request_digest.to_vec(),
    }
}

fn protocol_migration_spec(record: runtime::StoredMigration) -> protocol::MigrationSpec {
    protocol::MigrationSpec {
        id: record.id,
        name: record.name,
        up_sql: record.up_sql,
        down_sql: record.down_sql,
        checksum: record
            .checksum
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        created_at_ms: record.created_at_ms,
    }
}

fn migration_response_from_outcome(
    spec: protocol::MigrationSpec,
    status: protocol::MigrationStatus,
    outcome: MigrationOutcome,
    actor_api_key_id: protocol::ApiKeyId,
) -> Result<protocol::MigrationRecord, WorkerFailure> {
    let expected = match status {
        protocol::MigrationStatus::Applied => ffdb_migration_engine::MigrationStatus::Applied,
        protocol::MigrationStatus::RolledBack => ffdb_migration_engine::MigrationStatus::RolledBack,
        _ => return Err(receipt_outcome_unknown()),
    };
    if outcome.id != spec.id || outcome.status != expected {
        return Err(receipt_outcome_unknown());
    }
    Ok(protocol::MigrationRecord {
        spec,
        status,
        schema_version_before: outcome.schema_version_before,
        schema_version_after: outcome.schema_version_after,
        applied_at_ms: Some(outcome.applied_at_ms),
        duration_ms: Some(outcome.duration_ms),
        actor_api_key_id,
        execution_log: outcome.execution_log,
    })
}

fn hash_file_bounded(
    path: &Path,
    cancellation: &runtime::CancellationToken,
    deadline: Instant,
) -> Result<(u64, String), WorkerFailure> {
    let mut file = fs::File::open(path).map_err(|_| receipt_outcome_unknown())?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(failure("query.cancelled", "request was cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(failure(
                "query.deadline_exceeded",
                "request deadline exceeded",
            ));
        }
        let read = file
            .read(&mut buffer)
            .map_err(|_| receipt_outcome_unknown())?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).map_err(|_| receipt_outcome_unknown())?)
            .ok_or_else(receipt_outcome_unknown)?;
        digest.update(&buffer[..read]);
    }
    Ok((size, hex::encode(digest.finalize())))
}

fn receipt_outcome_unknown() -> WorkerFailure {
    failure(
        "idempotency.outcome_unknown",
        "a previous worker attempt has an unknown outcome",
    )
}

fn convert_migration_history(
    record: runtime::StoredMigration,
) -> Result<protocol::MigrationHistoryRecord, WorkerFailure> {
    let actor = Uuid::parse_str(&record.actor_id).map_err(|_| {
        failure(
            "internal.migration_history",
            "migration history contains an invalid actor identifier",
        )
    })?;
    let status = match record.status.as_str() {
        "applied" => protocol::MigrationStatus::Applied,
        "rolled_back" => protocol::MigrationStatus::RolledBack,
        _ => {
            return Err(failure(
                "internal.migration_history",
                "migration history contains an invalid status",
            ));
        }
    };
    Ok(protocol::MigrationHistoryRecord {
        spec: protocol::MigrationSpec {
            id: record.id,
            name: record.name,
            up_sql: record.up_sql,
            down_sql: record.down_sql,
            checksum: record
                .checksum
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            created_at_ms: record.created_at_ms,
        },
        status,
        schema_version_before: record.version_before,
        schema_version_after: record.version_after,
        applied_at_ms: record.applied_at_ms,
        duration_ms: u64::try_from(record.duration_ms).unwrap_or_default(),
        actor_api_key_id: protocol::ApiKeyId(actor),
    })
}

fn map_runtime(error: runtime::RuntimeError) -> WorkerFailure {
    match error {
        runtime::RuntimeError::StatementNotAllowed => {
            failure("query.statement_not_allowed", "statement is not allowed")
        }
        runtime::RuntimeError::SqlTooLarge => {
            failure("query.sql_too_large", "SQL exceeds the configured limit")
        }
        runtime::RuntimeError::TooManyVariables => failure(
            "query.too_many_variables",
            "statement has too many variables",
        ),
        runtime::RuntimeError::ResponseTooLarge => failure(
            "query.response_too_large",
            "query response exceeds the configured limit",
        ),
        runtime::RuntimeError::DatabaseTooLarge => {
            failure("quota.database_size", "database size quota was exceeded")
        }
        runtime::RuntimeError::Cancelled => failure("query.cancelled", "query was cancelled"),
        runtime::RuntimeError::DeadlineExceeded => {
            failure("query.deadline_exceeded", "query deadline was exceeded")
        }
        runtime::RuntimeError::ConstraintViolation => {
            failure("query.constraint_violation", "constraint violation")
        }
        runtime::RuntimeError::SyncCursorInvalid => {
            failure("sync.cursor_invalid", "sync cursor is invalid or expired")
        }
        runtime::RuntimeError::SyncSchemaMismatch => failure(
            "sync.schema_version_mismatch",
            "sync schema version does not match the database",
        ),
        runtime::RuntimeError::SyncMutationInvalid => {
            failure("sync.mutation_invalid", "sync mutation is invalid")
        }
        runtime::RuntimeError::UsageReceiptConflict => failure(
            "idempotency.receipt_conflict",
            "usage receipt belongs to another request",
        ),
        runtime::RuntimeError::UsageReceiptInvalid => {
            failure("internal.usage_receipt", "usage receipt is invalid")
        }
        internal => {
            tracing::warn!(runtime_error = ?internal, "database runtime returned an internal error");
            failure("internal.database", "database operation failed")
        }
    }
}

fn sync_rejection_code(error: &runtime::RuntimeError) -> &'static str {
    match error {
        runtime::RuntimeError::ConstraintViolation | runtime::RuntimeError::Database => {
            "sync.rls_or_constraint_rejected"
        }
        runtime::RuntimeError::SyncMutationInvalid => "sync.mutation_invalid",
        runtime::RuntimeError::StatementNotAllowed => "sync.statement_not_allowed",
        runtime::RuntimeError::Cancelled => "query.cancelled",
        runtime::RuntimeError::DeadlineExceeded => "query.deadline_exceeded",
        _ => "sync.mutation_rejected",
    }
}

fn map_migration(error: ffdb_migration_engine::MigrationError) -> WorkerFailure {
    match error {
        ffdb_migration_engine::MigrationError::ChecksumMismatch => failure(
            "migration.checksum_mismatch",
            "migration checksum does not match",
        ),
        ffdb_migration_engine::MigrationError::NotApplied => {
            failure("migration.not_applied", "migration is not applied")
        }
        ffdb_migration_engine::MigrationError::InvalidMetadata
        | ffdb_migration_engine::MigrationError::MissingDirection => {
            failure("migration.invalid", "migration is invalid")
        }
        _ => failure("migration.failed", "migration execution failed"),
    }
}

fn map_storage(error: ffdb_object_storage::StorageError) -> WorkerFailure {
    match error {
        ffdb_object_storage::StorageError::RlsDenied => failure(
            "storage.rls_denied",
            "storage metadata policy denied the operation",
        ),
        ffdb_object_storage::StorageError::ObjectQuotaExceeded => {
            failure("storage.object_quota", "object size quota was exceeded")
        }
        ffdb_object_storage::StorageError::ProjectQuotaExceeded => failure(
            "storage.project_quota",
            "project storage quota was exceeded",
        ),
        ffdb_object_storage::StorageError::DuplicateReservation => failure(
            "storage.duplicate_reservation",
            "storage reservation identifier already exists",
        ),
        ffdb_object_storage::StorageError::InvalidObjectKey
        | ffdb_object_storage::StorageError::InvalidMultipartRequest
        | ffdb_object_storage::StorageError::InvalidGrant => {
            failure("storage.invalid_request", "storage request is invalid")
        }
        error => {
            tracing::warn!(storage_error = ?error, "storage adapter returned an error");
            failure("internal.storage", "storage operation failed")
        }
    }
}

fn map_validation(_: protocol::ProtocolValidationError) -> WorkerFailure {
    failure("query.invalid_request", "request failed validation")
}

fn epoch_ms() -> Result<i64, WorkerFailure> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| failure("internal.clock", "system clock is invalid"))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| failure("internal.clock", "system clock is invalid"))
}

fn map_backup_crypto(error: BackupCryptoError) -> WorkerFailure {
    match error {
        BackupCryptoError::InvalidKey => failure(
            "internal.backup_key_invalid",
            "backup encryption key is invalid",
        ),
        BackupCryptoError::Cancelled => failure("query.cancelled", "query was cancelled"),
        BackupCryptoError::DeadlineExceeded => {
            failure("query.deadline_exceeded", "query deadline was exceeded")
        }
        BackupCryptoError::InvalidCiphertext => failure(
            "backup.authentication_failed",
            "backup ciphertext failed authentication",
        ),
        BackupCryptoError::Io => failure("internal.backup_failed", "backup operation failed"),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OperationReceipt {
    version: u8,
    receipt_id: Uuid,
    project_id: protocol::ProjectId,
    database_id: protocol::DatabaseId,
    operation: String,
    request_digest: [u8; 32],
    state: OperationReceiptState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OperationReceiptState {
    Started,
    Completed {
        response: Box<protocol::WorkerResponse>,
    },
}

#[derive(Debug)]
struct ReceiptOwner {
    path: PathBuf,
    record: OperationReceipt,
}

#[derive(Debug)]
enum ReceiptAdmission {
    Owner(ReceiptOwner),
    Replay(protocol::WorkerResponse),
}

fn receipt_operation_name(operation: &protocol::WorkerOperation) -> Option<&'static str> {
    match operation {
        protocol::WorkerOperation::ApplyMigration(_) => Some("migration.apply"),
        protocol::WorkerOperation::RollbackMigration { .. } => Some("migration.rollback"),
        protocol::WorkerOperation::Backup { .. } => Some("backup.create"),
        protocol::WorkerOperation::Restore { .. } => Some("backup.restore"),
        _ => None,
    }
}

fn operation_receipt_digest(
    route: &protocol::DatabaseRoute,
    mode: &protocol::ExecutionMode,
    operation: &protocol::WorkerOperation,
) -> Result<[u8; 32], WorkerFailure> {
    let encoded = serde_json::to_vec(&(mode, operation)).map_err(|_| receipt_unavailable())?;
    let mut digest = Sha256::new();
    digest.update(b"ffdb.worker-operation-receipt.v1\0");
    digest.update(route.project_id.0.as_bytes());
    digest.update(route.database_id.0.as_bytes());
    digest.update(encoded);
    Ok(digest.finalize().into())
}

fn usage_receipt_digest(
    route: &protocol::DatabaseRoute,
    mode: &protocol::ExecutionMode,
    operation: &protocol::WorkerOperation,
) -> Result<[u8; 32], WorkerFailure> {
    let encoded = serde_json::to_vec(&(mode, operation)).map_err(|_| {
        failure(
            "internal.usage_receipt",
            "usage receipt could not be created",
        )
    })?;
    let mut digest = Sha256::new();
    digest.update(b"ffdb.usage-receipt.v1\0");
    digest.update(route.project_id.0.as_bytes());
    digest.update(route.database_id.0.as_bytes());
    digest.update(encoded);
    Ok(digest.finalize().into())
}

fn statement_usage_units(sql: &str) -> Result<(u64, u64), WorkerFailure> {
    let class = ffdb_sql_parser::classify_statement(sql)
        .map_err(|_| failure("query.invalid_request", "request failed validation"))?;
    Ok(if class.read_only { (1, 0) } else { (0, 1) })
}

fn usage_subject(mode: &protocol::ExecutionMode) -> Option<protocol::UserId> {
    match mode {
        protocol::ExecutionMode::EndUser(auth) => Some(auth.subject),
        protocol::ExecutionMode::Developer(_) => None,
    }
}

fn protocol_usage_from_stored(
    receipt_id: Uuid,
    stored: &runtime::StoredUsageReceipt,
) -> Result<protocol::UsageReceipt, runtime::RuntimeError> {
    let request_id = Uuid::parse_str(&stored.request_id)
        .map(protocol::RequestId)
        .map_err(|_| runtime::RuntimeError::UsageReceiptInvalid)?;
    let subject = stored
        .subject
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| runtime::RuntimeError::UsageReceiptInvalid)?
        .map(protocol::UserId);
    Ok(protocol::UsageReceipt {
        receipt_id,
        request_id,
        reads: stored.reads,
        writes: stored.writes,
        logical_database_bytes: stored.logical_database_bytes,
        subject,
        recorded_at_ms: stored.recorded_at_ms,
    })
}

fn read_operation_receipt(path: &Path) -> Result<OperationReceipt, WorkerFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|_| receipt_unavailable())?;
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_OPERATION_RECEIPT_BYTES
    {
        return Err(failure(
            "idempotency.outcome_unknown",
            "worker operation receipt is incomplete or invalid",
        ));
    }
    let bytes = fs::read(path).map_err(|_| receipt_unavailable())?;
    serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "idempotency.outcome_unknown",
            "worker operation receipt is incomplete or invalid",
        )
    })
}

fn receipt_unavailable() -> WorkerFailure {
    failure(
        "idempotency.receipt_unavailable",
        "durable worker operation receipt is unavailable",
    )
}

fn sync_directory(path: &Path) -> Result<(), WorkerFailure> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| receipt_unavailable())
}

fn initialize_receipt_root(
    backup_root: &Path,
    database_id: protocol::DatabaseId,
) -> Result<PathBuf, WorkerFailure> {
    let parent = backup_root.join(".ffdb-operation-receipts");
    create_private_transient_directory(&parent)?;
    let root = parent.join(database_id.to_string());
    create_private_transient_directory(&root)?;
    cleanup_stale_receipts(&root, 10_000)?;
    Ok(root)
}

fn cleanup_stale_receipts(root: &Path, limit: usize) -> Result<usize, WorkerFailure> {
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in fs::read_dir(root).map_err(|_| receipt_unavailable())? {
        let entry = entry.map_err(|_| receipt_unavailable())?;
        if removed >= limit {
            break;
        }
        let metadata = entry.metadata().map_err(|_| receipt_unavailable())?;
        let is_expired = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= OPERATION_RECEIPT_RETENTION);
        if metadata.file_type().is_file()
            && is_operation_receipt_name(&entry.file_name())
            && is_expired
        {
            fs::remove_file(entry.path()).map_err(|_| receipt_unavailable())?;
            removed += 1;
        }
    }
    if removed > 0 {
        sync_directory(root)?;
    }
    Ok(removed)
}

fn is_operation_receipt_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_suffix(".json"))
        .is_some_and(|stem| Uuid::parse_str(stem).is_ok_and(|id| id.to_string() == stem))
}

fn initialize_transient_root(
    backup_root: &Path,
    database_id: protocol::DatabaseId,
) -> Result<PathBuf, WorkerFailure> {
    let parent = backup_root.join(".ffdb-transient");
    create_private_transient_directory(&parent)?;
    let root = parent.join(database_id.to_string());
    create_private_transient_directory(&root)?;
    cleanup_stale_transient_files(&root)?;
    Ok(root)
}

fn create_private_transient_directory(path: &Path) -> Result<(), WorkerFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(failure(
                "internal.backup_open",
                "database worker backup storage is not a directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| {
                failure(
                    "internal.backup_open",
                    "database worker could not initialize backup storage",
                )
            })?;
        }
        Err(_) => {
            return Err(failure(
                "internal.backup_open",
                "database worker could not inspect backup storage",
            ));
        }
    }
    secure_transient_directory(path)?;
    if !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err(failure(
            "internal.backup_open",
            "database worker backup storage is not a directory",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn secure_transient_directory(path: &Path) -> Result<(), WorkerFailure> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
        failure(
            "internal.backup_open",
            "database worker could not protect backup storage",
        )
    })
}

#[cfg(not(unix))]
fn secure_transient_directory(_path: &Path) -> Result<(), WorkerFailure> {
    Ok(())
}

fn cleanup_stale_transient_files(root: &Path) -> Result<(), WorkerFailure> {
    let entries = fs::read_dir(root).map_err(|_| {
        failure(
            "internal.backup_open",
            "database worker could not inspect backup storage",
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|_| {
            failure(
                "internal.backup_open",
                "database worker could not inspect backup storage",
            )
        })?;
        let file_type = entry.file_type().map_err(|_| {
            failure(
                "internal.backup_open",
                "database worker could not inspect backup storage",
            )
        })?;
        if file_type.is_file() && is_transient_database_artifact(&entry.file_name()) {
            fs::remove_file(entry.path()).map_err(|_| {
                failure(
                    "internal.backup_open",
                    "database worker could not clean backup storage",
                )
            })?;
        }
    }
    Ok(())
}

fn is_transient_database_artifact(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    [
        ".sqlite3-wal",
        ".sqlite3-shm",
        ".sqlite3-journal",
        ".sqlite3",
    ]
    .into_iter()
    .find_map(|suffix| name.strip_suffix(suffix))
    .is_some_and(|stem| Uuid::parse_str(stem).is_ok_and(|id| id.to_string() == stem))
}

fn remove_transient_database_files(root: &Path, id: Uuid) -> Result<(), WorkerFailure> {
    for suffix in [
        ".sqlite3-wal",
        ".sqlite3-shm",
        ".sqlite3-journal",
        ".sqlite3",
    ] {
        match fs::remove_file(root.join(format!("{id}{suffix}"))) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(failure(
                    "internal.backup_cleanup",
                    "database worker could not clean plaintext backup storage",
                ));
            }
        }
    }
    Ok(())
}

fn failure(code: &str, message: &str) -> WorkerFailure {
    WorkerFailure {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{
        io::{Seek, SeekFrom},
        time::{Duration, Instant},
    };

    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;

    fn route() -> protocol::DatabaseRoute {
        protocol::DatabaseRoute {
            project_id: protocol::ProjectId::new(),
            database_id: protocol::DatabaseId::new(),
            node_id: protocol::NodeId::new(),
            generation: 7,
        }
    }

    fn worker(
        route: protocol::DatabaseRoute,
        limits: runtime::ExecutionLimits,
    ) -> (TempDir, DatabaseWorker) {
        let directory = TempDir::new().unwrap();
        let database_root = directory.path().join("databases");
        let backup_root = directory.path().join("backups");
        let worker = DatabaseWorker::open(
            route,
            &database_root,
            &backup_root,
            runtime::RuntimeConfig {
                limits,
                ..runtime::RuntimeConfig::default()
            },
            [7_u8; 32],
        )
        .unwrap();
        (directory, worker)
    }

    #[test]
    fn startup_cleanup_removes_only_private_sqlite_transients() {
        let directory = TempDir::new().unwrap();
        let backup_root = directory.path().join("backups");
        fs::create_dir_all(&backup_root).unwrap();
        let database_id = protocol::DatabaseId::new();
        let transient_root = initialize_transient_root(&backup_root, database_id).unwrap();
        let stale_id = Uuid::now_v7();
        let stale_database = transient_root.join(format!("{stale_id}.sqlite3"));
        let stale_wal = transient_root.join(format!("{stale_id}.sqlite3-wal"));
        let unknown = transient_root.join("operator-note.txt");
        let similar = transient_root.join(format!("{stale_id}.sqlite3.saved"));
        let directory_artifact = transient_root.join(format!("{}.sqlite3", Uuid::now_v7()));
        fs::write(&stale_database, b"plaintext").unwrap();
        fs::write(&stale_wal, b"plaintext-wal").unwrap();
        fs::write(&unknown, b"preserve").unwrap();
        fs::write(&similar, b"preserve").unwrap();
        fs::create_dir(&directory_artifact).unwrap();

        #[cfg(unix)]
        let symlink_artifact = {
            use std::os::unix::fs::symlink;

            let path = transient_root.join(format!("{}.sqlite3", Uuid::now_v7()));
            symlink(&unknown, &path).unwrap();
            path
        };

        let reopened = initialize_transient_root(&backup_root, database_id).unwrap();
        assert_eq!(reopened, transient_root);
        assert!(!stale_database.exists());
        assert!(!stale_wal.exists());
        assert!(unknown.exists());
        assert!(similar.exists());
        assert!(directory_artifact.is_dir());
        #[cfg(unix)]
        assert!(
            symlink_artifact
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        #[cfg(unix)]
        assert_eq!(
            transient_root.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_cleanup_refuses_a_symlinked_transient_root() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().unwrap();
        let backup_root = directory.path().join("backups");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&backup_root).unwrap();
        fs::create_dir(&outside).unwrap();
        let protected = outside.join(format!("{}.sqlite3", Uuid::now_v7()));
        fs::write(&protected, b"do-not-delete").unwrap();
        symlink(&outside, backup_root.join(".ffdb-transient")).unwrap();

        let error =
            initialize_transient_root(&backup_root, protocol::DatabaseId::new()).unwrap_err();
        assert_eq!(error.code, "internal.backup_open");
        assert_eq!(fs::read(&protected).unwrap(), b"do-not-delete");
    }

    fn end_user(
        route: &protocol::DatabaseRoute,
        project: protocol::ProjectId,
    ) -> protocol::WorkerRequest {
        protocol::WorkerRequest {
            protocol_version: protocol::PROTOCOL_VERSION,
            request_id: protocol::RequestId::new(),
            route: route.clone(),
            mode: protocol::ExecutionMode::EndUser(protocol::AuthContext {
                project_id: project,
                subject: protocol::UserId::new(),
                role: "authenticated".to_owned(),
                claims: Map::new(),
                token_id: protocol::TokenId::new(),
            }),
            deadline_epoch_ms: epoch_ms().unwrap() + 30_000,
            limits: protocol::ResourceLimits::default(),
            expected_schema_version: None,
            operation_receipt_id: None,
            operation: protocol::WorkerOperation::Query(protocol::QueryRequest {
                sql: "SELECT 1".to_owned(),
                parameters: Vec::new(),
                options: protocol::QueryOptions::default(),
            }),
        }
    }

    fn request_with_mode(
        route: &protocol::DatabaseRoute,
        mode: protocol::ExecutionMode,
        operation: protocol::WorkerOperation,
    ) -> protocol::WorkerRequest {
        protocol::WorkerRequest {
            protocol_version: protocol::PROTOCOL_VERSION,
            request_id: protocol::RequestId::new(),
            route: route.clone(),
            mode,
            deadline_epoch_ms: epoch_ms().unwrap() + 30_000,
            limits: protocol::ResourceLimits::default(),
            expected_schema_version: None,
            operation_receipt_id: None,
            operation,
        }
    }

    fn developer_mode() -> protocol::ExecutionMode {
        protocol::ExecutionMode::Developer(protocol::DeveloperPrincipal {
            organization_id: protocol::OrganizationId::new(),
            api_key_id: protocol::ApiKeyId::new(),
            scopes: vec![
                protocol::DeveloperScope::DatabaseQuery,
                protocol::DeveloperScope::DatabaseMigrate,
                protocol::DeveloperScope::BackupsManage,
            ],
            actor_label: "test".to_owned(),
        })
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

    /// Repeatable local diagnostic for separating SQLite cold-open work from
    /// the steady-state metered query path. This is ignored because absolute
    /// timings are host dependent; run it with `--ignored --nocapture` when
    /// evaluating runtime changes.
    #[test]
    #[ignore = "manual worker latency profile"]
    fn worker_latency_profile() {
        const FRESH_OPEN_SAMPLES: usize = 20;
        const REOPEN_SAMPLES: usize = 40;
        const QUERY_SAMPLES: usize = 500;

        let directory = TempDir::new().unwrap();
        let database_root = directory.path().join("databases");
        let backup_root = directory.path().join("backups");
        let config = runtime::RuntimeConfig::default();
        let mut fresh_opens = Vec::with_capacity(FRESH_OPEN_SAMPLES);
        for _ in 0..FRESH_OPEN_SAMPLES {
            let started = Instant::now();
            let opened = DatabaseWorker::open(
                route(),
                &database_root,
                &backup_root,
                config.clone(),
                [7_u8; 32],
            )
            .unwrap();
            fresh_opens.push(started.elapsed());
            drop(opened);
        }

        let persistent_route = route();
        let initial = DatabaseWorker::open(
            persistent_route.clone(),
            &database_root,
            &backup_root,
            config.clone(),
            [7_u8; 32],
        )
        .unwrap();
        drop(initial);
        let mut reopens = Vec::with_capacity(REOPEN_SAMPLES);
        for _ in 0..REOPEN_SAMPLES {
            let started = Instant::now();
            let opened = DatabaseWorker::open(
                persistent_route.clone(),
                &database_root,
                &backup_root,
                config.clone(),
                [7_u8; 32],
            )
            .unwrap();
            reopens.push(started.elapsed());
            drop(opened);
        }

        let worker = DatabaseWorker::open(
            persistent_route.clone(),
            &database_root,
            &backup_root,
            config,
            [7_u8; 32],
        )
        .unwrap();
        let cancellation = runtime::CancellationToken::default();
        let mut queries = Vec::with_capacity(QUERY_SAMPLES);
        for _ in 0..QUERY_SAMPLES {
            let request = request_with_mode(
                &persistent_route,
                developer_mode(),
                protocol::WorkerOperation::Query(protocol::QueryRequest {
                    sql: "SELECT 1".to_owned(),
                    parameters: Vec::new(),
                    options: protocol::QueryOptions::default(),
                }),
            );
            let started = Instant::now();
            worker.handle(request, &cancellation).unwrap();
            queries.push(started.elapsed());
        }

        let fresh = latency_summary(&mut fresh_opens);
        let reopen = latency_summary(&mut reopens);
        let query = latency_summary(&mut queries);
        eprintln!(
            "worker_latency_profile fresh_open_us p50={} p95={} p99={}; reopen_us p50={} p95={} p99={}; metered_select_us p50={} p95={} p99={}",
            fresh.0.as_micros(),
            fresh.1.as_micros(),
            fresh.2.as_micros(),
            reopen.0.as_micros(),
            reopen.1.as_micros(),
            reopen.2.as_micros(),
            query.0.as_micros(),
            query.1.as_micros(),
            query.2.as_micros(),
        );
    }

    #[test]
    fn sql_usage_receipts_are_durable_idempotent_and_hidden()
    -> Result<(), Box<dyn std::error::Error>> {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let cancellation = runtime::CancellationToken::default();
        let developer = developer_mode();
        worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql:
                            "CREATE TABLE usage_probe(id INTEGER PRIMARY KEY, value TEXT NOT NULL)"
                                .to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();

        let auth = protocol::AuthContext {
            project_id: route.project_id,
            subject: protocol::UserId::new(),
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: protocol::TokenId::new(),
        };
        let mut insert = request_with_mode(
            &route,
            protocol::ExecutionMode::EndUser(auth.clone()),
            protocol::WorkerOperation::Query(protocol::QueryRequest {
                sql: "INSERT INTO usage_probe(id,value) VALUES (1,'once')".to_owned(),
                parameters: Vec::new(),
                options: protocol::QueryOptions::default(),
            }),
        );
        let insert_receipt_id = Uuid::now_v7();
        insert.operation_receipt_id = Some(insert_receipt_id);
        let insert_id = insert.request_id;
        let first = worker.handle(insert.clone(), &cancellation).unwrap();
        assert_eq!(first.usage.receipt_id, insert_receipt_id);
        assert_eq!(first.usage.request_id, insert_id);
        assert_eq!((first.usage.reads, first.usage.writes), (0, 1));
        assert_eq!(first.usage.subject, Some(auth.subject));
        assert!(first.usage.logical_database_bytes > 0);
        let mut replay_request = insert.clone();
        replay_request.request_id = protocol::RequestId::new();
        let replay = worker.handle(replay_request, &cancellation).unwrap();
        assert_eq!(
            replay.response, first.response,
            "retry must return the stored response"
        );
        assert_eq!(
            replay.usage, first.usage,
            "retry must return the stored usage receipt"
        );
        assert_eq!(first.statement_telemetry.len(), 1);
        assert!(
            replay.statement_telemetry.is_empty(),
            "an idempotency replay must not be counted as another SQL execution"
        );

        insert.operation = protocol::WorkerOperation::Query(protocol::QueryRequest {
            sql: "INSERT INTO usage_probe(id,value) VALUES (2,'conflict')".to_owned(),
            parameters: Vec::new(),
            options: protocol::QueryOptions::default(),
        });
        assert_eq!(
            worker.handle(insert, &cancellation).unwrap_err().code,
            "idempotency.receipt_conflict"
        );

        let read = request_with_mode(
            &route,
            protocol::ExecutionMode::EndUser(auth.clone()),
            protocol::WorkerOperation::Query(protocol::QueryRequest {
                sql: "SELECT count(*) FROM usage_probe".to_owned(),
                parameters: Vec::new(),
                options: protocol::QueryOptions::default(),
            }),
        );
        let first_read = worker.handle(read.clone(), &cancellation).unwrap();
        assert_eq!((first_read.usage.reads, first_read.usage.writes), (1, 0));
        assert_eq!(first_read.usage.subject, Some(auth.subject));
        assert_eq!(
            worker.handle(read, &cancellation).unwrap().usage,
            first_read.usage
        );

        let mut transaction = request_with_mode(
            &route,
            protocol::ExecutionMode::EndUser(auth),
            protocol::WorkerOperation::Transaction(protocol::TransactionRequest {
                statements: vec![
                    protocol::QueryRequest {
                        sql: "INSERT INTO usage_probe(id,value) VALUES (2,'transaction')"
                            .to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    },
                    protocol::QueryRequest {
                        sql: "SELECT count(*) FROM usage_probe".to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    },
                ],
            }),
        );
        transaction.operation_receipt_id = Some(Uuid::now_v7());
        let first_transaction = worker.handle(transaction.clone(), &cancellation).unwrap();
        assert_eq!(
            (
                first_transaction.usage.reads,
                first_transaction.usage.writes
            ),
            (1, 1)
        );
        let mut transaction_retry = transaction;
        transaction_retry.request_id = protocol::RequestId::new();
        let replayed_transaction = worker.handle(transaction_retry, &cancellation).unwrap();
        assert_eq!(replayed_transaction.response, first_transaction.response);
        assert_eq!(replayed_transaction.usage, first_transaction.usage);
        assert_eq!(first_transaction.statement_telemetry.len(), 2);
        assert!(
            replayed_transaction.statement_telemetry.is_empty(),
            "a transaction replay must not insert the row or emit new execution telemetry"
        );

        let count = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "SELECT count(*) FROM usage_probe".to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        let protocol::WorkerResponse::Query(count) = count.response else {
            return Err("expected query response".into());
        };
        assert_eq!(count.rows, vec![vec![protocol::ResultCell::SafeInteger(2)]]);

        let hidden = worker
            .handle(
                request_with_mode(
                    &route,
                    developer,
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "SELECT * FROM __ffdb_usage_receipts".to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap_err();
        assert_eq!(hidden.code, "query.statement_not_allowed");
        Ok(())
    }

    #[test]
    fn sync_and_snapshot_usage_count_only_accepted_non_duplicate_mutations()
    -> Result<(), Box<dyn std::error::Error>> {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let cancellation = runtime::CancellationToken::default();
        let mut migration = protocol::MigrationSpec {
            id: "001_usage_sync".to_owned(),
            name: "usage sync".to_owned(),
            up_sql: "CREATE TABLE usage_sync(id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, body TEXT); \
                     ALTER TABLE usage_sync ENABLE ROW LEVEL SECURITY; \
                     CREATE POLICY usage_sync_owner ON usage_sync FOR ALL TO authenticated \
                     USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())"
                .to_owned(),
            down_sql: "DROP POLICY usage_sync_owner ON usage_sync; \
                       ALTER TABLE usage_sync DISABLE ROW LEVEL SECURITY; \
                       DROP TABLE usage_sync"
                .to_owned(),
            checksum: String::new(),
            created_at_ms: 1,
        };
        migration.checksum = migration.calculate_checksum();
        worker
            .handle(
                request_with_mode(
                    &route,
                    developer_mode(),
                    protocol::WorkerOperation::ApplyMigration(migration),
                ),
                &cancellation,
            )
            .unwrap();

        let auth = protocol::AuthContext {
            project_id: route.project_id,
            subject: protocol::UserId::new(),
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: protocol::TokenId::new(),
        };
        let snapshot = worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(auth.clone()),
                    protocol::WorkerOperation::Snapshot(protocol::SnapshotRequest {
                        tables: Some(vec!["usage_sync".to_owned()]),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        assert_eq!((snapshot.usage.reads, snapshot.usage.writes), (1, 0));
        assert_eq!(snapshot.usage.subject, Some(auth.subject));

        let mut accepted_values = Map::new();
        accepted_values.insert(
            "owner_id".to_owned(),
            serde_json::Value::String(auth.subject.to_string()),
        );
        accepted_values.insert(
            "body".to_owned(),
            serde_json::Value::String("accepted".to_owned()),
        );
        let mut rejected_values = accepted_values.clone();
        rejected_values.insert(
            "owner_id".to_owned(),
            serde_json::Value::String(protocol::UserId::new().to_string()),
        );
        let mut push = request_with_mode(
            &route,
            protocol::ExecutionMode::EndUser(auth.clone()),
            protocol::WorkerOperation::SyncPush(protocol::SyncPushRequest {
                schema_version: 1,
                mutations: vec![
                    protocol::SyncMutation {
                        mutation_id: "accepted-1".to_owned(),
                        table: "usage_sync".to_owned(),
                        primary_key: serde_json::json!(1),
                        operation: protocol::ChangeOperation::Insert,
                        values: Some(accepted_values),
                        base_row_version: None,
                        client_timestamp_ms: None,
                    },
                    protocol::SyncMutation {
                        mutation_id: "rejected-1".to_owned(),
                        table: "usage_sync".to_owned(),
                        primary_key: serde_json::json!(2),
                        operation: protocol::ChangeOperation::Insert,
                        values: Some(rejected_values),
                        base_row_version: None,
                        client_timestamp_ms: None,
                    },
                ],
            }),
        );
        push.operation_receipt_id = Some(Uuid::now_v7());
        let first_push = worker.handle(push.clone(), &cancellation).unwrap();
        assert_eq!((first_push.usage.reads, first_push.usage.writes), (0, 1));
        assert_eq!(first_push.usage.subject, Some(auth.subject));
        let protocol::WorkerResponse::SyncPush(first_result) = &first_push.response else {
            return Err("expected sync push response".into());
        };
        assert_eq!(
            first_result.results[0].status,
            protocol::MutationStatus::Applied
        );
        assert_eq!(
            first_result.results[1].status,
            protocol::MutationStatus::Rejected
        );
        let mut push_retry = push.clone();
        push_retry.request_id = protocol::RequestId::new();
        assert_eq!(
            worker.handle(push_retry, &cancellation).unwrap(),
            first_push
        );

        let mut duplicate_push = push;
        duplicate_push.request_id = protocol::RequestId::new();
        duplicate_push.operation_receipt_id = Some(Uuid::now_v7());
        let duplicate = worker.handle(duplicate_push, &cancellation).unwrap();
        assert_eq!((duplicate.usage.reads, duplicate.usage.writes), (0, 0));
        let protocol::WorkerResponse::SyncPush(duplicate_result) = duplicate.response else {
            return Err("expected sync push response".into());
        };
        assert_eq!(
            duplicate_result.results[0].status,
            protocol::MutationStatus::Duplicate
        );
        assert_eq!(
            duplicate_result.results[1].status,
            protocol::MutationStatus::Rejected
        );

        let pull = worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(auth.clone()),
                    protocol::WorkerOperation::SyncPull(protocol::SyncPullRequest {
                        cursor: None,
                        limit: 10,
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        assert_eq!((pull.usage.reads, pull.usage.writes), (1, 0));
        assert_eq!(pull.usage.subject, Some(auth.subject));
        let protocol::WorkerResponse::Sync(pull_result) = pull.response else {
            return Err("expected sync pull response".into());
        };
        assert_eq!(pull_result.changes.len(), 1);
        Ok(())
    }

    #[test]
    fn migration_history_is_typed_logical_and_paginated_by_schema_version() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let developer = developer_mode();
        let expected_actor = match &developer {
            protocol::ExecutionMode::Developer(principal) => principal.api_key_id,
            protocol::ExecutionMode::EndUser(_) => unreachable!(),
        };
        let mut migration = protocol::MigrationSpec {
            id: "001_history".to_owned(),
            name: "history probe".to_owned(),
            up_sql: "CREATE TABLE history_probe(id INTEGER PRIMARY KEY); \
                     ALTER TABLE history_probe ENABLE ROW LEVEL SECURITY"
                .to_owned(),
            down_sql: "ALTER TABLE history_probe DISABLE ROW LEVEL SECURITY; \
                       DROP TABLE history_probe"
                .to_owned(),
            checksum: String::new(),
            created_at_ms: 11,
        };
        migration.checksum = migration.calculate_checksum();
        worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::ApplyMigration(migration.clone()),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();

        let response = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::MigrationHistory {
                        limit: 10,
                        before_version: None,
                    },
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let history = match response.response {
            protocol::WorkerResponse::MigrationHistory(history) => Some(history),
            _ => None,
        }
        .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].spec, migration);
        assert_eq!(history[0].status, protocol::MigrationStatus::Applied);
        assert_eq!(history[0].schema_version_before, 0);
        assert_eq!(history[0].schema_version_after, 1);
        assert_eq!(history[0].actor_api_key_id, expected_actor);
        assert!(!history[0].spec.up_sql.contains("__ffdb_data_"));

        let response = worker
            .handle(
                request_with_mode(
                    &route,
                    developer,
                    protocol::WorkerOperation::MigrationHistory {
                        limit: 10,
                        before_version: Some(1),
                    },
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        assert!(matches!(
            response.response,
            protocol::WorkerResponse::MigrationHistory(history) if history.is_empty()
        ));
    }

    #[test]
    fn developer_bucket_lifecycle_validates_names_and_returns_typed_metadata() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let developer = developer_mode();
        let owner = protocol::UserId::new();
        let bucket_id = Uuid::now_v7().to_string();
        let bucket = protocol::StorageCreateBucketRequest {
            id: bucket_id.clone(),
            name: "project-assets".to_owned(),
            owner_id: Some(owner),
            public: false,
            max_object_bytes: 1_000,
            project_quota_bytes: 10_000,
            created_at_ms: 22,
        };
        let response = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::StorageCreateBucket(bucket.clone()),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let created = match response.response {
            protocol::WorkerResponse::StorageBucket(bucket) => Some(bucket),
            _ => None,
        }
        .unwrap();
        assert_eq!(created.id, bucket_id);
        assert_eq!(created.name, bucket.name);
        assert_eq!(created.owner_id, owner.to_string());
        assert_eq!(created.project_quota_bytes, 10_000);

        let response = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::StorageBuckets,
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        assert!(matches!(
            response.response,
            protocol::WorkerResponse::StorageBuckets(buckets)
                if buckets.len() == 1 && buckets[0] == created
        ));

        for invalid_name in ["192.168.1.1", "xn--reserved", "bad.-label"] {
            let error = worker
                .handle(
                    request_with_mode(
                        &route,
                        developer.clone(),
                        protocol::WorkerOperation::StorageCreateBucket(
                            protocol::StorageCreateBucketRequest {
                                id: Uuid::now_v7().to_string(),
                                name: invalid_name.to_owned(),
                                owner_id: None,
                                public: false,
                                max_object_bytes: 1,
                                project_quota_bytes: 1,
                                created_at_ms: 23,
                            },
                        ),
                    ),
                    &runtime::CancellationToken::default(),
                )
                .unwrap_err();
            assert_eq!(error.code, "storage.invalid_bucket");
        }
    }

    #[test]
    fn started_migration_receipt_reconciles_atomic_outcome_after_intervening_migration() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let cancellation = runtime::CancellationToken::default();
        let developer = developer_mode();
        let migration = |id: &str, table: &str| {
            let mut spec = protocol::MigrationSpec {
                id: id.to_owned(),
                name: id.to_owned(),
                up_sql: format!("CREATE TABLE {table}(id INTEGER PRIMARY KEY)"),
                down_sql: format!("DROP TABLE {table}"),
                checksum: String::new(),
                created_at_ms: 1,
            };
            spec.checksum = spec.calculate_checksum();
            spec
        };
        let receipt_id = Uuid::now_v7();
        let mut first_request = request_with_mode(
            &route,
            developer.clone(),
            protocol::WorkerOperation::ApplyMigration(migration("001_receipt", "receipt_one")),
        );
        first_request.operation_receipt_id = Some(receipt_id);
        let first_response = worker.handle(first_request.clone(), &cancellation).unwrap();
        let receipt_path = worker.receipt_root.join(format!("{receipt_id}.json"));
        let mut receipt = read_operation_receipt(&receipt_path).unwrap();
        receipt.state = OperationReceiptState::Started;
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();

        worker
            .handle(
                request_with_mode(
                    &route,
                    developer,
                    protocol::WorkerOperation::ApplyMigration(migration(
                        "002_intervening",
                        "receipt_two",
                    )),
                ),
                &cancellation,
            )
            .unwrap();
        let replay = worker.handle(first_request, &cancellation).unwrap();
        assert_eq!(replay.response, first_response.response);
        assert!(matches!(
            &replay.response,
            protocol::WorkerResponse::Migration(_)
        ));
        let protocol::WorkerResponse::Migration(record) = replay.response else {
            return;
        };
        assert_eq!(record.schema_version_after, 1);

        let rollback_receipt_id = Uuid::now_v7();
        let mut rollback_request = request_with_mode(
            &route,
            developer_mode(),
            protocol::WorkerOperation::RollbackMigration {
                migration_id: "001_receipt".to_owned(),
            },
        );
        rollback_request.operation_receipt_id = Some(rollback_receipt_id);
        let rollback_response = worker
            .handle(rollback_request.clone(), &cancellation)
            .unwrap();
        let rollback_receipt_path = worker
            .receipt_root
            .join(format!("{rollback_receipt_id}.json"));
        let mut rollback_receipt = read_operation_receipt(&rollback_receipt_path).unwrap();
        rollback_receipt.state = OperationReceiptState::Started;
        fs::write(
            &rollback_receipt_path,
            serde_json::to_vec(&rollback_receipt).unwrap(),
        )
        .unwrap();
        worker
            .handle(
                request_with_mode(
                    &route,
                    developer_mode(),
                    protocol::WorkerOperation::ApplyMigration(migration(
                        "003_after_rollback",
                        "receipt_three",
                    )),
                ),
                &cancellation,
            )
            .unwrap();
        assert_eq!(
            worker
                .handle(rollback_request, &cancellation)
                .unwrap()
                .response,
            rollback_response.response
        );
    }

    #[test]
    fn encrypted_backup_round_trip_rejects_tamper_and_preserves_failed_restore() {
        let route = route();
        let (directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let cancellation = runtime::CancellationToken::default();
        let developer = developer_mode();
        for sql in [
            "CREATE TABLE restore_probe(id INTEGER PRIMARY KEY,value TEXT)",
            "INSERT INTO restore_probe(id,value) VALUES (1,'original')",
        ] {
            worker
                .handle(
                    request_with_mode(
                        &route,
                        developer.clone(),
                        protocol::WorkerOperation::Query(protocol::QueryRequest {
                            sql: sql.to_owned(),
                            parameters: Vec::new(),
                            options: protocol::QueryOptions::default(),
                        }),
                    ),
                    &cancellation,
                )
                .unwrap();
        }
        let backup_id = protocol::BackupId::new();
        let backup_receipt_id = Uuid::now_v7();
        let mut backup_request = request_with_mode(
            &route,
            developer.clone(),
            protocol::WorkerOperation::Backup { backup_id },
        );
        backup_request.operation_receipt_id = Some(backup_receipt_id);
        let first_backup = worker
            .handle(backup_request.clone(), &cancellation)
            .unwrap();
        let backup_receipt_path = worker
            .receipt_root
            .join(format!("{backup_receipt_id}.json"));
        let mut backup_receipt = read_operation_receipt(&backup_receipt_path).unwrap();
        backup_receipt.state = OperationReceiptState::Started;
        fs::write(
            &backup_receipt_path,
            serde_json::to_vec(&backup_receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(
            worker
                .handle(backup_request, &cancellation)
                .unwrap()
                .response,
            first_backup.response
        );
        assert!(
            fs::read_dir(&worker.transient_root)
                .unwrap()
                .next()
                .is_none()
        );
        let backup_path = directory
            .path()
            .join("backups")
            .join(format!("{backup_id}.sqlite3"));
        let prefix = fs::read(&backup_path).unwrap();
        assert!(!prefix.starts_with(b"SQLite format 3\0"));
        assert!(prefix.starts_with(b"FFDBBK01"));
        let mut wrong_route = route.clone();
        wrong_route.project_id = protocol::ProjectId::new();
        let wrong_plaintext = directory.path().join("wrong-project.sqlite3");
        let cross_project = worker
            .backup_crypto
            .decrypt_file(
                &backup_path,
                &wrong_plaintext,
                &wrong_route,
                backup_id,
                &cancellation,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err();
        remove_file_if_present(&wrong_plaintext);
        assert_eq!(cross_project, BackupCryptoError::InvalidCiphertext);

        worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "UPDATE restore_probe SET value='changed' WHERE id=1".to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        let restore_receipt_id = Uuid::now_v7();
        let mut restore_request = request_with_mode(
            &route,
            developer.clone(),
            protocol::WorkerOperation::Restore { backup_id },
        );
        restore_request.operation_receipt_id = Some(restore_receipt_id);
        let first_restore = worker
            .handle(restore_request.clone(), &cancellation)
            .unwrap();
        assert!(
            fs::read_dir(&worker.transient_root)
                .unwrap()
                .next()
                .is_none()
        );
        let result = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "SELECT value FROM restore_probe WHERE id=1".to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        let result = match result.response {
            protocol::WorkerResponse::Query(result) => Some(result),
            _ => None,
        }
        .unwrap();
        assert_eq!(
            result.rows,
            vec![vec![protocol::ResultCell::Text("original".to_owned())]]
        );

        worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "UPDATE restore_probe SET value='after-response-loss' WHERE id=1"
                            .to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        let receipt_path = worker
            .receipt_root
            .join(format!("{restore_receipt_id}.json"));
        let mut receipt = read_operation_receipt(&receipt_path).unwrap();
        receipt.state = OperationReceiptState::Started;
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let replayed_restore = worker.handle(restore_request, &cancellation).unwrap();
        assert_eq!(replayed_restore.response, first_restore.response);
        let result = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "SELECT value FROM restore_probe WHERE id=1".to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &cancellation,
            )
            .unwrap();
        assert!(matches!(
            &result.response,
            protocol::WorkerResponse::Query(_)
        ));
        let protocol::WorkerResponse::Query(result) = result.response else {
            return;
        };
        assert_eq!(
            result.rows,
            vec![vec![protocol::ResultCell::Text(
                "after-response-loss".to_owned()
            )]],
            "reconciliation must not execute restore over intervening writes"
        );

        let tampered_id = protocol::BackupId::new();
        worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Backup {
                        backup_id: tampered_id,
                    },
                ),
                &cancellation,
            )
            .unwrap();
        let tampered_path = directory
            .path()
            .join("backups")
            .join(format!("{tampered_id}.sqlite3"));
        let mut tampered = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&tampered_path)
            .unwrap();
        tampered.seek(SeekFrom::End(-1)).unwrap();
        let mut final_byte = [0_u8; 1];
        tampered.read_exact(&mut final_byte).unwrap();
        final_byte[0] ^= 0xff;
        tampered.seek(SeekFrom::End(-1)).unwrap();
        tampered.write_all(&final_byte).unwrap();
        tampered.sync_all().unwrap();
        let error = worker
            .handle(
                request_with_mode(
                    &route,
                    developer.clone(),
                    protocol::WorkerOperation::Restore {
                        backup_id: tampered_id,
                    },
                ),
                &cancellation,
            )
            .unwrap_err();
        assert_eq!(error.code, "backup.authentication_failed");
        assert!(
            fs::read_dir(&worker.transient_root)
                .unwrap()
                .next()
                .is_none()
        );

        let invalid_id = protocol::BackupId::new();
        let invalid_plain = directory.path().join("invalid-plain.sqlite3");
        fs::write(&invalid_plain, b"not a database").unwrap();
        let invalid_cipher = directory
            .path()
            .join("backups")
            .join(format!("{invalid_id}.sqlite3"));
        worker
            .backup_crypto
            .encrypt_file(
                &invalid_plain,
                &invalid_cipher,
                &route,
                invalid_id,
                &cancellation,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        let before = fs::read_dir(directory.path().join("backups"))
            .unwrap()
            .count();
        assert!(
            worker
                .handle(
                    request_with_mode(
                        &route,
                        developer,
                        protocol::WorkerOperation::Restore {
                            backup_id: invalid_id,
                        },
                    ),
                    &cancellation,
                )
                .is_err()
        );
        let after = fs::read_dir(directory.path().join("backups"))
            .unwrap()
            .count();
        assert_eq!(after, before + 1, "failed restore keeps recovery backup");
        assert!(
            fs::read_dir(&worker.transient_root)
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn query_write_flows_to_policy_filtered_sync_pull() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let developer = protocol::ExecutionMode::Developer(protocol::DeveloperPrincipal {
            organization_id: protocol::OrganizationId::new(),
            api_key_id: protocol::ApiKeyId::new(),
            scopes: vec![protocol::DeveloperScope::DatabaseMigrate],
            actor_label: "test".to_owned(),
        });
        let mut migration = protocol::MigrationSpec {
            id: "001_sync_documents".to_owned(),
            name: "sync documents".to_owned(),
            up_sql: "CREATE TABLE sync_documents(id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, body TEXT); \
                     ALTER TABLE sync_documents ENABLE ROW LEVEL SECURITY; \
                     CREATE POLICY sync_documents_owner ON sync_documents FOR ALL TO authenticated \
                     USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())"
                .to_owned(),
            down_sql: "DROP POLICY sync_documents_owner ON sync_documents; \
                       ALTER TABLE sync_documents DISABLE ROW LEVEL SECURITY; \
                       DROP TABLE sync_documents"
                .to_owned(),
            checksum: String::new(),
            created_at_ms: 1,
        };
        migration.checksum = migration.calculate_checksum();
        worker
            .handle(
                request_with_mode(
                    &route,
                    developer,
                    protocol::WorkerOperation::ApplyMigration(migration),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();

        let alice = protocol::AuthContext {
            project_id: route.project_id,
            subject: protocol::UserId::new(),
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: protocol::TokenId::new(),
        };
        let bob = protocol::AuthContext {
            project_id: route.project_id,
            subject: protocol::UserId::new(),
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: protocol::TokenId::new(),
        };
        let snapshot = worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(alice.clone()),
                    protocol::WorkerOperation::Snapshot(protocol::SnapshotRequest {
                        tables: Some(vec!["sync_documents".to_owned()]),
                    }),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let snapshot = match snapshot.response {
            protocol::WorkerResponse::Snapshot(snapshot) => Some(snapshot),
            _ => None,
        }
        .unwrap();
        let insert = protocol::QueryRequest {
            sql: "INSERT INTO sync_documents(id,owner_id,body) VALUES (1,auth.uid(),'captured')"
                .to_owned(),
            parameters: Vec::new(),
            options: protocol::QueryOptions::default(),
        };
        worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(alice.clone()),
                    protocol::WorkerOperation::Query(insert),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let selected = worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(alice.clone()),
                    protocol::WorkerOperation::Query(protocol::QueryRequest {
                        sql: "SELECT body FROM sync_documents WHERE owner_id = auth.uid()"
                            .to_owned(),
                        parameters: Vec::new(),
                        options: protocol::QueryOptions::default(),
                    }),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let protocol::WorkerResponse::Query(selected) = selected.response else {
            panic!("expected query response");
        };
        assert_eq!(
            selected.rows,
            vec![vec![protocol::ResultCell::Text("captured".to_owned())]]
        );
        let alice_pull = worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(alice),
                    protocol::WorkerOperation::SyncPull(protocol::SyncPullRequest {
                        cursor: Some(snapshot.cursor),
                        limit: 10,
                    }),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let alice_pull = match alice_pull.response {
            protocol::WorkerResponse::Sync(pull) => Some(pull),
            _ => None,
        }
        .unwrap();
        assert_eq!(alice_pull.changes.len(), 1);
        assert_eq!(
            alice_pull.changes[0].operation,
            protocol::ChangeOperation::Insert
        );

        let bob_pull = worker
            .handle(
                request_with_mode(
                    &route,
                    protocol::ExecutionMode::EndUser(bob),
                    protocol::WorkerOperation::SyncPull(protocol::SyncPullRequest {
                        cursor: None,
                        limit: 10,
                    }),
                ),
                &runtime::CancellationToken::default(),
            )
            .unwrap();
        let bob_pull = match bob_pull.response {
            protocol::WorkerResponse::Sync(pull) => Some(pull),
            _ => None,
        }
        .unwrap();
        assert!(bob_pull.changes.is_empty());
    }

    #[test]
    fn rejects_cross_project_auth_context_before_sql_execution() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let request = end_user(&route, protocol::ProjectId::new());
        let error = worker
            .handle(request, &runtime::CancellationToken::default())
            .unwrap_err();
        assert_eq!(error.code, "auth.project_mismatch");
    }

    #[test]
    fn storage_cleanup_claim_and_ack_reject_cross_project_routes() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let mut wrong_route = route.clone();
        wrong_route.project_id = protocol::ProjectId::new();
        let claim = request_with_mode(
            &wrong_route,
            developer_mode(),
            protocol::WorkerOperation::StorageCleanupClaim(protocol::StorageCleanupClaimRequest {
                now_ms: epoch_ms().unwrap(),
                limit: 10,
            }),
        );
        assert_eq!(
            worker
                .handle(claim, &runtime::CancellationToken::default())
                .unwrap_err()
                .code,
            "project.stale_route"
        );
        let ack = request_with_mode(
            &wrong_route,
            developer_mode(),
            protocol::WorkerOperation::StorageCleanupAck(protocol::StorageCleanupAckRequest {
                now_ms: epoch_ms().unwrap(),
                items: vec![protocol::StorageCleanupDisposition {
                    id: Uuid::now_v7().to_string(),
                    lease_token: protocol::SensitiveString::new(Uuid::now_v7().to_string()),
                    outcome: protocol::StorageCleanupOutcome::Deleted,
                }],
            }),
        );
        assert_eq!(
            worker
                .handle(ack, &runtime::CancellationToken::default())
                .unwrap_err()
                .code,
            "project.stale_route"
        );
    }

    #[test]
    fn open_time_limit_remains_a_hard_ceiling_over_looser_request_limit() {
        let route = route();
        let limits = runtime::ExecutionLimits {
            max_sql_bytes: 8,
            ..runtime::ExecutionLimits::default()
        };
        let (_directory, worker) = worker(route.clone(), limits);
        let mut request = end_user(&route, route.project_id);
        request.limits.max_sql_bytes = 1_000;
        request.operation = protocol::WorkerOperation::Query(protocol::QueryRequest {
            sql: "SELECT 123".to_owned(),
            parameters: Vec::new(),
            options: protocol::QueryOptions::default(),
        });
        let error = worker
            .handle(request, &runtime::CancellationToken::default())
            .unwrap_err();
        assert_eq!(error.code, "query.sql_too_large");
    }

    #[test]
    fn request_statement_timeout_reaches_sqlite_progress_cancellation() {
        let route = route();
        let limits = runtime::ExecutionLimits {
            statement_timeout: Duration::from_secs(30),
            ..runtime::ExecutionLimits::default()
        };
        let (_directory, worker) = worker(route.clone(), limits);
        let mut request = end_user(&route, route.project_id);
        request.limits.statement_timeout_ms = 1;
        request.operation = protocol::WorkerOperation::Query(protocol::QueryRequest {
            sql:
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n"
                    .to_owned(),
            parameters: Vec::new(),
            options: protocol::QueryOptions::default(),
        });
        let error = worker
            .handle(request, &runtime::CancellationToken::default())
            .unwrap_err();
        assert_eq!(error.code, "query.deadline_exceeded");
    }

    #[test]
    fn backup_encryption_honors_cancellation_before_sparse_read() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("large-sparse-backup.sqlite3");
        let file = fs::File::create(&path).unwrap();
        file.set_len(512 * 1024 * 1024).unwrap();
        drop(file);
        let cancellation = runtime::CancellationToken::default();
        cancellation.cancel();
        let error = BackupCrypto::new([9_u8; 32])
            .unwrap()
            .encrypt_file(
                &path,
                &directory.path().join("ciphertext.sqlite3"),
                &route(),
                protocol::BackupId::new(),
                &cancellation,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err();
        assert_eq!(error, BackupCryptoError::Cancelled);
    }

    #[test]
    fn stale_generation_is_rejected() {
        let route = route();
        let (_directory, worker) = worker(route.clone(), runtime::ExecutionLimits::default());
        let mut request = end_user(&route, route.project_id);
        request.route.generation += 1;
        let error = worker
            .handle(request, &runtime::CancellationToken::default())
            .unwrap_err();
        assert_eq!(error.code, "project.stale_route");
    }
}
