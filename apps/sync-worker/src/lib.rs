//! Bounded actor host for per-project logical sync engines.
//!
//! Every mutating command commits a versioned engine checkpoint through a
//! compare-and-swap [`SyncStore`] before success is returned. This makes an
//! implementation backed by the project SQLite database possible without
//! exposing a raw connection to the sync engine.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ffdb_sync_engine::{
    OpaqueCursor, PullResult, PushBatch, PushResult, ResnapshotReason, Snapshot, SyncAuthorizer,
    SyncCheckpoint, SyncConfig, SyncContext, SyncEngine, SyncError,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, mpsc, oneshot},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSyncCheckpoint {
    pub revision: u64,
    pub checkpoint: SyncCheckpoint,
}

/// Durable compare-and-swap boundary. A project-SQLite implementation should
/// store the checkpoint and revision in an internal table inside the same
/// trusted transaction used for logical change capture.
#[async_trait]
pub trait SyncStore: Send + Sync {
    async fn load(&self, project_id: &str) -> Result<Option<StoredSyncCheckpoint>, SyncStoreError>;

    async fn compare_and_swap(
        &self,
        project_id: &str,
        expected_revision: u64,
        checkpoint: &SyncCheckpoint,
    ) -> Result<u64, SyncStoreError>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SyncStoreError {
    #[error("durable sync state changed concurrently")]
    Conflict,
    #[error("durable sync state is corrupt")]
    Corrupt,
    #[error("durable sync state is unavailable")]
    Unavailable,
}

#[derive(Debug)]
pub struct DirectorySyncStore {
    root: PathBuf,
    operation_lock: Mutex<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub checkpoints_scanned: u64,
    pub stale_temporary_files_removed: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEnvelope {
    format_version: u16,
    revision: u64,
    checkpoint_base64: String,
}

impl DirectorySyncStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, SyncStoreError> {
        tokio::fs::create_dir_all(root.as_ref())
            .await
            .map_err(|_| SyncStoreError::Unavailable)?;
        let root = tokio::fs::canonicalize(root.as_ref())
            .await
            .map_err(|_| SyncStoreError::Unavailable)?;
        Ok(Self {
            root,
            operation_lock: Mutex::new(0),
        })
    }

    fn project_path(&self, project_id: &str) -> Result<PathBuf, SyncStoreError> {
        if project_id.is_empty() || project_id.len() > 128 {
            return Err(SyncStoreError::Corrupt);
        }
        let digest = hex::encode(Sha256::digest(project_id.as_bytes()));
        Ok(self.root.join(format!("{digest}.sync")))
    }

    async fn load_unlocked(
        &self,
        project_id: &str,
    ) -> Result<Option<StoredSyncCheckpoint>, SyncStoreError> {
        const MAX_FILE_BYTES: u64 = 350 * 1024 * 1024;
        let path = self.project_path(project_id)?;
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SyncStoreError::Unavailable),
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err(SyncStoreError::Corrupt);
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| SyncStoreError::Unavailable)?;
        let envelope: FileEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| SyncStoreError::Corrupt)?;
        if envelope.format_version != 1 || envelope.revision == 0 {
            return Err(SyncStoreError::Corrupt);
        }
        let checkpoint = STANDARD
            .decode(envelope.checkpoint_base64)
            .map_err(|_| SyncStoreError::Corrupt)
            .and_then(|bytes| {
                SyncCheckpoint::from_bytes(bytes).map_err(|_| SyncStoreError::Corrupt)
            })?;
        Ok(Some(StoredSyncCheckpoint {
            revision: envelope.revision,
            checkpoint,
        }))
    }

    /// Performs storage durability maintenance without accepting user sync
    /// commands. Stale temporary files are removed only after the caller's
    /// conservative age threshold; checkpoint envelopes are structurally read
    /// and bounded but never interpreted as authorization state.
    pub async fn maintenance_pass(
        &self,
        now_ms: i64,
        stale_after_ms: i64,
    ) -> Result<MaintenanceReport, SyncStoreError> {
        if now_ms < 0 || stale_after_ms < 60_000 {
            return Err(SyncStoreError::Unavailable);
        }
        let _guard = self.operation_lock.lock().await;
        let mut entries = tokio::fs::read_dir(&self.root)
            .await
            .map_err(|_| SyncStoreError::Unavailable)?;
        let mut report = MaintenanceReport {
            checkpoints_scanned: 0,
            stale_temporary_files_removed: 0,
        };
        let mut scanned = 0_u64;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| SyncStoreError::Unavailable)?
        {
            scanned = scanned.saturating_add(1);
            if scanned > 1_000_000 {
                return Err(SyncStoreError::Unavailable);
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if name.ends_with(".sync") {
                let metadata = entry
                    .metadata()
                    .await
                    .map_err(|_| SyncStoreError::Unavailable)?;
                if !metadata.is_file() || metadata.len() > 350 * 1024 * 1024 {
                    return Err(SyncStoreError::Corrupt);
                }
                let bytes = tokio::fs::read(&path)
                    .await
                    .map_err(|_| SyncStoreError::Unavailable)?;
                let envelope: FileEnvelope =
                    serde_json::from_slice(&bytes).map_err(|_| SyncStoreError::Corrupt)?;
                if envelope.format_version != 1
                    || envelope.revision == 0
                    || STANDARD.decode(envelope.checkpoint_base64).is_err()
                {
                    return Err(SyncStoreError::Corrupt);
                }
                report.checkpoints_scanned = report.checkpoints_scanned.saturating_add(1);
            } else if name.starts_with('.') && name.ends_with(".tmp") {
                let modified_ms = entry
                    .metadata()
                    .await
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|duration| i64::try_from(duration.as_millis()).ok());
                if modified_ms
                    .is_some_and(|modified| modified.saturating_add(stale_after_ms) <= now_ms)
                {
                    tokio::fs::remove_file(&path)
                        .await
                        .map_err(|_| SyncStoreError::Unavailable)?;
                    report.stale_temporary_files_removed =
                        report.stale_temporary_files_removed.saturating_add(1);
                }
            }
        }
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || std::fs::File::open(root)?.sync_all())
            .await
            .map_err(|_| SyncStoreError::Unavailable)?
            .map_err(|_| SyncStoreError::Unavailable)?;
        Ok(report)
    }
}

#[async_trait]
impl SyncStore for DirectorySyncStore {
    async fn load(&self, project_id: &str) -> Result<Option<StoredSyncCheckpoint>, SyncStoreError> {
        let _guard = self.operation_lock.lock().await;
        self.load_unlocked(project_id).await
    }

    async fn compare_and_swap(
        &self,
        project_id: &str,
        expected_revision: u64,
        checkpoint: &SyncCheckpoint,
    ) -> Result<u64, SyncStoreError> {
        let mut counter = self.operation_lock.lock().await;
        let current = self.load_unlocked(project_id).await?;
        if current.as_ref().map_or(0, |stored| stored.revision) != expected_revision {
            return Err(SyncStoreError::Conflict);
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or(SyncStoreError::Corrupt)?;
        let path = self.project_path(project_id)?;
        *counter = counter.saturating_add(1);
        let temporary = self.root.join(format!(
            ".{}.{}.tmp",
            path.file_stem()
                .and_then(|name| name.to_str())
                .ok_or(SyncStoreError::Corrupt)?,
            *counter
        ));
        let bytes = serde_json::to_vec(&FileEnvelope {
            format_version: 1,
            revision,
            checkpoint_base64: STANDARD.encode(checkpoint.as_bytes()),
        })
        .map_err(|_| SyncStoreError::Corrupt)?;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
            .map_err(|_| SyncStoreError::Unavailable)?;
        if file.write_all(&bytes).await.is_err() || file.sync_all().await.is_err() {
            let _ignored = tokio::fs::remove_file(&temporary).await;
            return Err(SyncStoreError::Unavailable);
        }
        drop(file);
        if tokio::fs::rename(&temporary, &path).await.is_err() {
            let _ignored = tokio::fs::remove_file(&temporary).await;
            return Err(SyncStoreError::Unavailable);
        }
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || std::fs::File::open(root)?.sync_all())
            .await
            .map_err(|_| SyncStoreError::Unavailable)?
            .map_err(|_| SyncStoreError::Unavailable)?;
        Ok(revision)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    pub queue_capacity: usize,
    pub max_open_projects: usize,
}

#[derive(Clone)]
pub struct WorkerHandle {
    sender: mpsc::Sender<Command>,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerHandle")
            .finish_non_exhaustive()
    }
}

impl WorkerHandle {
    pub fn spawn<A, S>(
        config: WorkerConfig,
        authorizer: Arc<A>,
        store: Arc<S>,
    ) -> Result<Self, WorkerError>
    where
        A: SyncAuthorizer + Send + Sync + 'static,
        S: SyncStore + 'static,
    {
        if config.queue_capacity == 0 || config.max_open_projects == 0 {
            return Err(WorkerError::InvalidConfiguration);
        }
        let (sender, receiver) = mpsc::channel(config.queue_capacity);
        tokio::spawn(run(receiver, authorizer, store, config.max_open_projects));
        Ok(Self { sender })
    }

    pub async fn create_project(
        &self,
        project_id: String,
        schema_version: u64,
        cursor_secret: Vec<u8>,
        sync_config: SyncConfig,
    ) -> Result<(), WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::CreateProject {
            project_id,
            schema_version,
            cursor_secret,
            sync_config,
            reply,
        })
        .await?;
        receive.await.map_err(|_| WorkerError::Unavailable)?
    }

    pub async fn snapshot(
        &self,
        context: SyncContext,
        now_ms: i64,
    ) -> Result<Snapshot, WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Snapshot {
            context,
            now_ms,
            reply,
        })
        .await?;
        receive.await.map_err(|_| WorkerError::Unavailable)?
    }

    pub async fn pull(
        &self,
        context: SyncContext,
        cursor: OpaqueCursor,
        limit: usize,
        now_ms: i64,
    ) -> Result<PullResult, WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Pull {
            context,
            cursor,
            limit,
            now_ms,
            reply,
        })
        .await?;
        receive.await.map_err(|_| WorkerError::Unavailable)?
    }

    pub async fn push(
        &self,
        context: SyncContext,
        batch: PushBatch,
        now_ms: i64,
    ) -> Result<PushResult, WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Push {
            context,
            batch,
            now_ms,
            reply,
        })
        .await?;
        receive.await.map_err(|_| WorkerError::Unavailable)?
    }

    pub async fn invalidate(
        &self,
        project_id: String,
        reason: ResnapshotReason,
        new_schema_version: Option<u64>,
        now_ms: i64,
    ) -> Result<u64, WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Invalidate {
            project_id,
            reason,
            new_schema_version,
            now_ms,
            reply,
        })
        .await?;
        receive.await.map_err(|_| WorkerError::Unavailable)?
    }

    pub async fn compact(&self, project_id: String, now_ms: i64) -> Result<(), WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Compact {
            project_id,
            now_ms,
            reply,
        })
        .await?;
        receive.await.map_err(|_| WorkerError::Unavailable)?
    }

    pub async fn shutdown(&self) -> Result<(), WorkerError> {
        let (reply, receive) = oneshot::channel();
        self.send(Command::Shutdown { reply }).await?;
        receive.await.map_err(|_| WorkerError::Unavailable)
    }

    async fn send(&self, command: Command) -> Result<(), WorkerError> {
        self.sender
            .send(command)
            .await
            .map_err(|_| WorkerError::Unavailable)
    }
}

enum Command {
    CreateProject {
        project_id: String,
        schema_version: u64,
        cursor_secret: Vec<u8>,
        sync_config: SyncConfig,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Snapshot {
        context: SyncContext,
        now_ms: i64,
        reply: oneshot::Sender<Result<Snapshot, WorkerError>>,
    },
    Pull {
        context: SyncContext,
        cursor: OpaqueCursor,
        limit: usize,
        now_ms: i64,
        reply: oneshot::Sender<Result<PullResult, WorkerError>>,
    },
    Push {
        context: SyncContext,
        batch: PushBatch,
        now_ms: i64,
        reply: oneshot::Sender<Result<PushResult, WorkerError>>,
    },
    Invalidate {
        project_id: String,
        reason: ResnapshotReason,
        new_schema_version: Option<u64>,
        now_ms: i64,
        reply: oneshot::Sender<Result<u64, WorkerError>>,
    },
    Compact {
        project_id: String,
        now_ms: i64,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

async fn run<A>(
    mut receiver: mpsc::Receiver<Command>,
    authorizer: Arc<A>,
    store: Arc<impl SyncStore + 'static>,
    max_open_projects: usize,
) where
    A: SyncAuthorizer + Send + Sync + 'static,
{
    struct OpenEngine {
        engine: SyncEngine,
        revision: u64,
    }

    let mut engines: HashMap<String, OpenEngine> = HashMap::new();
    while let Some(command) = receiver.recv().await {
        match command {
            Command::CreateProject {
                project_id,
                schema_version,
                cursor_secret,
                sync_config,
                reply,
            } => {
                let result = if engines.contains_key(&project_id) {
                    Err(WorkerError::ProjectAlreadyOpen)
                } else if engines.len() >= max_open_projects {
                    Err(WorkerError::ProjectLimitReached)
                } else {
                    match store.load(&project_id).await.map_err(WorkerError::Store) {
                        Ok(Some(stored)) => SyncEngine::restore(
                            project_id.clone(),
                            cursor_secret,
                            sync_config,
                            &stored.checkpoint,
                        )
                        .and_then(|engine| {
                            if engine.schema_version() == schema_version {
                                Ok(engine)
                            } else {
                                Err(SyncError::SchemaMismatch {
                                    expected: engine.schema_version(),
                                    received: schema_version,
                                })
                            }
                        })
                        .map(|engine| {
                            engines.insert(
                                project_id,
                                OpenEngine {
                                    engine,
                                    revision: stored.revision,
                                },
                            );
                        })
                        .map_err(WorkerError::Sync),
                        Ok(None) => match SyncEngine::new(
                            project_id.clone(),
                            schema_version,
                            cursor_secret,
                            sync_config,
                        ) {
                            Ok(engine) => match engine.checkpoint() {
                                Ok(checkpoint) => store
                                    .compare_and_swap(&project_id, 0, &checkpoint)
                                    .await
                                    .map(|revision| {
                                        engines.insert(project_id, OpenEngine { engine, revision });
                                    })
                                    .map_err(WorkerError::Store),
                                Err(error) => Err(WorkerError::Sync(error)),
                            },
                            Err(error) => Err(WorkerError::Sync(error)),
                        },
                        Err(error) => Err(error),
                    }
                };
                let _ignored = reply.send(result);
            }
            Command::Snapshot {
                context,
                now_ms,
                reply,
            } => {
                let result = engines
                    .get(&context.project_id)
                    .ok_or(WorkerError::ProjectNotOpen)
                    .and_then(|open| {
                        open.engine
                            .snapshot(&context, authorizer.as_ref(), now_ms)
                            .map_err(WorkerError::Sync)
                    });
                let _ignored = reply.send(result);
            }
            Command::Pull {
                context,
                cursor,
                limit,
                now_ms,
                reply,
            } => {
                let result = engines
                    .get(&context.project_id)
                    .ok_or(WorkerError::ProjectNotOpen)
                    .and_then(|open| {
                        open.engine
                            .pull(&context, &cursor, limit, authorizer.as_ref(), now_ms)
                            .map_err(WorkerError::Sync)
                    });
                let _ignored = reply.send(result);
            }
            Command::Push {
                context,
                batch,
                now_ms,
                reply,
            } => {
                let result = match engines.get_mut(&context.project_id) {
                    Some(open) => {
                        let original = open.engine.clone();
                        match open
                            .engine
                            .push(&context, batch, authorizer.as_ref(), now_ms)
                        {
                            Ok(result) => match open.engine.checkpoint() {
                                Ok(checkpoint) => match store
                                    .compare_and_swap(
                                        &context.project_id,
                                        open.revision,
                                        &checkpoint,
                                    )
                                    .await
                                {
                                    Ok(revision) => {
                                        open.revision = revision;
                                        Ok(result)
                                    }
                                    Err(error) => {
                                        open.engine = original;
                                        Err(WorkerError::Store(error))
                                    }
                                },
                                Err(error) => {
                                    open.engine = original;
                                    Err(WorkerError::Sync(error))
                                }
                            },
                            Err(error) => Err(WorkerError::Sync(error)),
                        }
                    }
                    None => Err(WorkerError::ProjectNotOpen),
                };
                let _ignored = reply.send(result);
            }
            Command::Invalidate {
                project_id,
                reason,
                new_schema_version,
                now_ms,
                reply,
            } => {
                let result = match engines.get_mut(&project_id) {
                    Some(open) => {
                        let original = open.engine.clone();
                        match open.engine.invalidate(reason, new_schema_version, now_ms) {
                            Ok(sequence) => match open.engine.checkpoint() {
                                Ok(checkpoint) => match store
                                    .compare_and_swap(&project_id, open.revision, &checkpoint)
                                    .await
                                {
                                    Ok(revision) => {
                                        open.revision = revision;
                                        Ok(sequence)
                                    }
                                    Err(error) => {
                                        open.engine = original;
                                        Err(WorkerError::Store(error))
                                    }
                                },
                                Err(error) => {
                                    open.engine = original;
                                    Err(WorkerError::Sync(error))
                                }
                            },
                            Err(error) => Err(WorkerError::Sync(error)),
                        }
                    }
                    None => Err(WorkerError::ProjectNotOpen),
                };
                let _ignored = reply.send(result);
            }
            Command::Compact {
                project_id,
                now_ms,
                reply,
            } => {
                let result = match engines.get_mut(&project_id) {
                    Some(open) => {
                        let original = open.engine.clone();
                        open.engine.compact(now_ms);
                        match open.engine.checkpoint() {
                            Ok(checkpoint) => match store
                                .compare_and_swap(&project_id, open.revision, &checkpoint)
                                .await
                            {
                                Ok(revision) => {
                                    open.revision = revision;
                                    Ok(())
                                }
                                Err(error) => {
                                    open.engine = original;
                                    Err(WorkerError::Store(error))
                                }
                            },
                            Err(error) => {
                                open.engine = original;
                                Err(WorkerError::Sync(error))
                            }
                        }
                    }
                    None => Err(WorkerError::ProjectNotOpen),
                };
                let _ignored = reply.send(result);
            }
            Command::Shutdown { reply } => {
                let _ignored = reply.send(());
                break;
            }
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkerError {
    #[error("sync worker configuration is invalid")]
    InvalidConfiguration,
    #[error("sync worker is unavailable")]
    Unavailable,
    #[error("project sync engine is already open")]
    ProjectAlreadyOpen,
    #[error("project sync engine is not open")]
    ProjectNotOpen,
    #[error("open project limit reached")]
    ProjectLimitReached,
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Store(#[from] SyncStoreError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ffdb_sync_engine::{BatchMode, ClientMutation, MutationOperation};
    use serde_json::{Map, Value};

    use super::*;

    struct AllowAll;

    impl SyncAuthorizer for AllowAll {
        fn can_read(
            &self,
            _context: &SyncContext,
            _table: &str,
            _row: &Map<String, Value>,
        ) -> bool {
            true
        }

        fn can_write(
            &self,
            _context: &SyncContext,
            _table: &str,
            _operation: MutationOperation,
            _row: &Map<String, Value>,
        ) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn bounded_worker_hosts_project_engine_lifecycle() -> Result<(), WorkerError> {
        let directory = tempfile::tempdir().map_err(|_| WorkerError::Unavailable)?;
        let store = Arc::new(
            DirectorySyncStore::open(directory.path())
                .await
                .map_err(WorkerError::Store)?,
        );
        let worker_config = WorkerConfig {
            queue_capacity: 8,
            max_open_projects: 1,
        };
        let sync_config = SyncConfig {
            cursor_ttl_ms: 30_000,
            change_retention_ms: 10_000,
            tombstone_retention_ms: 20_000,
            max_pull_events: 100,
            max_push_mutations: 10,
            idempotency_retention_ms: 30_000,
            max_idempotency_records: 100,
        };
        let worker = WorkerHandle::spawn(worker_config.clone(), Arc::new(AllowAll), store)?;
        worker
            .create_project(
                "project-1".to_owned(),
                1,
                vec![4_u8; 32],
                sync_config.clone(),
            )
            .await?;
        let context = SyncContext {
            project_id: "project-1".to_owned(),
            subject: "user-1".to_owned(),
            role: "authenticated".to_owned(),
            scope_fingerprint: "scope:v1".to_owned(),
            trusted_client_id: "ios-device".to_owned(),
        };
        let values = Map::from_iter(BTreeMap::from([(
            "title".to_owned(),
            Value::String("offline".to_owned()),
        )]));
        worker
            .push(
                context.clone(),
                PushBatch {
                    client_id: "ios-device".to_owned(),
                    schema_version: 1,
                    mode: BatchMode::Atomic,
                    mutations: vec![ClientMutation {
                        mutation_id: "mutation-1".to_owned(),
                        table: "notes".to_owned(),
                        primary_key: Value::String("note-1".to_owned()),
                        operation: MutationOperation::Upsert,
                        values,
                        base_row_version: None,
                        client_timestamp_ms: None,
                    }],
                },
                1_000,
            )
            .await?;
        assert_eq!(worker.snapshot(context.clone(), 2_000).await?.rows.len(), 1);
        worker.shutdown().await?;

        let reopened_store = Arc::new(
            DirectorySyncStore::open(directory.path())
                .await
                .map_err(WorkerError::Store)?,
        );
        let reopened = WorkerHandle::spawn(worker_config, Arc::new(AllowAll), reopened_store)?;
        reopened
            .create_project("project-1".to_owned(), 1, vec![4_u8; 32], sync_config)
            .await?;
        assert_eq!(reopened.snapshot(context, 3_000).await?.rows.len(), 1);
        reopened.shutdown().await?;
        Ok(())
    }
}
