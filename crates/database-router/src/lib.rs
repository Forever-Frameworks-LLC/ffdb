//! Opaque project routing and worker execution boundaries.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ffdb_protocol::{
    DatabaseId, DatabaseRoute, PlatformError, ProjectId, WorkerExecution, WorkerRequest,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Semaphore};

// Keep this aligned with the worker's authenticated IPC envelope ceiling. The
// protocol permits an 8 MiB response body plus JSON framing and metadata.
const MAX_WORKER_FRAME_BYTES: usize = 9 * 1024 * 1024;
const WORKER_IDLE_TTL: Duration = Duration::from_secs(5 * 60);

#[async_trait]
pub trait DatabaseRouter: Send + Sync {
    async fn resolve(&self, project_id: ProjectId) -> Result<DatabaseRoute, RoutingError>;
}

#[async_trait]
pub trait DatabaseExecutor: Send + Sync {
    async fn execute(
        &self,
        route: &DatabaseRoute,
        request: WorkerRequest,
    ) -> Result<WorkerExecution, ExecutionError>;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RoutingError {
    #[error("project was not found")]
    NotFound,
    #[error("project is not available")]
    Unavailable,
    #[error("database route is stale")]
    StaleGeneration,
    #[error("routing metadata is inconsistent")]
    Inconsistent,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExecutionError {
    #[error("worker queue is full")]
    QueueFull,
    #[error("worker request timed out")]
    DeadlineExceeded,
    #[error("worker rejected a stale route")]
    StaleGeneration,
    #[error("worker protocol failure")]
    Protocol,
    #[error("worker execution failed: {code}")]
    Rejected { code: String },
    #[error("worker is unavailable")]
    Unavailable,
}

/// Bounded subprocess executor for the framed database-worker protocol.
///
/// Paths are trusted node configuration accepted only at construction. Requests
/// carry opaque routes and can never select a path or executable.
#[derive(Debug)]
pub struct ProcessWorkerExecutor {
    worker_binary: PathBuf,
    database_root: PathBuf,
    backup_root: PathBuf,
    backup_master_key_base64: SecretString,
    node_id: ffdb_protocol::NodeId,
    max_workers: usize,
    workers: Mutex<HashMap<(DatabaseId, u64), WorkerEntry>>,
    queue_slots: Arc<Semaphore>,
    queue_capacity: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerPoolSnapshot {
    pub active_workers: usize,
    pub max_workers: usize,
    pub execution_slots_in_use: usize,
    pub queue_capacity: usize,
}

impl ProcessWorkerExecutor {
    pub fn new(
        worker_binary: PathBuf,
        database_root: PathBuf,
        backup_root: PathBuf,
        backup_master_key_base64: SecretString,
        node_id: ffdb_protocol::NodeId,
        max_workers: usize,
        queue_capacity: usize,
    ) -> Result<Self, ExecutionError> {
        if !specific_absolute_path(&worker_binary)
            || !specific_absolute_path(&database_root)
            || !specific_absolute_path(&backup_root)
            || max_workers == 0
            || queue_capacity == 0
        {
            return Err(ExecutionError::Protocol);
        }
        Ok(Self {
            worker_binary,
            database_root,
            backup_root,
            backup_master_key_base64,
            node_id,
            max_workers,
            workers: Mutex::new(HashMap::new()),
            queue_slots: Arc::new(Semaphore::new(queue_capacity)),
            queue_capacity,
        })
    }

    async fn worker_for(
        &self,
        route: &DatabaseRoute,
    ) -> Result<Arc<Mutex<WorkerProcess>>, ExecutionError> {
        if route.node_id != self.node_id {
            return Err(ExecutionError::StaleGeneration);
        }
        let key = (route.database_id, route.generation);
        let mut workers = self.workers.lock().await;
        let now = Instant::now();
        workers.retain(|_, entry| {
            Arc::strong_count(&entry.process) > 1
                || now.saturating_duration_since(entry.last_used) < WORKER_IDLE_TTL
        });
        if let Some(entry) = workers.get_mut(&key) {
            entry.last_used = now;
            return Ok(Arc::clone(&entry.process));
        }
        // Retire prior generations before enforcing the cap. In-flight requests
        // keep their Arc and complete against the now-fenced old process.
        workers.retain(|(database_id, _), _| *database_id != route.database_id);
        if workers.len() >= self.max_workers {
            return Err(ExecutionError::QueueFull);
        }
        let process = WorkerProcess::spawn(
            &self.worker_binary,
            &self.database_root,
            &self.backup_root,
            &self.backup_master_key_base64,
            route,
        )?;
        let process = Arc::new(Mutex::new(process));
        workers.insert(
            key,
            WorkerEntry {
                process: Arc::clone(&process),
                last_used: now,
            },
        );
        Ok(process)
    }

    /// Evicts an idle worker generation. In-flight work retains its process until
    /// the request guard drops.
    pub async fn evict(&self, database_id: DatabaseId, generation: u64) -> bool {
        self.workers
            .lock()
            .await
            .remove(&(database_id, generation))
            .is_some()
    }

    /// A point-in-time bounded worker-pool snapshot for the operator dashboard.
    pub async fn snapshot(&self) -> WorkerPoolSnapshot {
        let active_workers = self.workers.lock().await.len();
        let available = self.queue_slots.available_permits();
        WorkerPoolSnapshot {
            active_workers,
            max_workers: self.max_workers,
            execution_slots_in_use: self.queue_capacity.saturating_sub(available),
            queue_capacity: self.queue_capacity,
        }
    }
}

#[derive(Debug)]
struct WorkerEntry {
    process: Arc<Mutex<WorkerProcess>>,
    last_used: Instant,
}

#[async_trait]
impl DatabaseExecutor for ProcessWorkerExecutor {
    async fn execute(
        &self,
        route: &DatabaseRoute,
        request: WorkerRequest,
    ) -> Result<WorkerExecution, ExecutionError> {
        if request.route != *route {
            return Err(ExecutionError::StaleGeneration);
        }
        let _queue_permit = Arc::clone(&self.queue_slots)
            .try_acquire_owned()
            .map_err(|_| ExecutionError::QueueFull)?;
        let worker = self.worker_for(route).await?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ExecutionError::Unavailable)?
            .as_millis();
        let deadline_ms = u128::try_from(request.deadline_epoch_ms)
            .map_err(|_| ExecutionError::DeadlineExceeded)?;
        let remaining_ms = deadline_ms
            .checked_sub(now_ms)
            .ok_or(ExecutionError::DeadlineExceeded)?;
        let remaining_ms = u64::try_from(remaining_ms).unwrap_or(u64::MAX);
        let result = tokio::time::timeout(Duration::from_millis(remaining_ms), async {
            worker.lock().await.exchange(&request).await
        })
        .await
        .map_err(|_| ExecutionError::DeadlineExceeded)
        .and_then(std::convert::identity);
        if result.is_err() {
            let _ = self.evict(route.database_id, route.generation).await;
        }
        result
    }
}

struct WorkerProcess {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl std::fmt::Debug for WorkerProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerProcess")
            .finish_non_exhaustive()
    }
}

impl WorkerProcess {
    fn spawn(
        binary: &Path,
        database_root: &Path,
        backup_root: &Path,
        backup_master_key_base64: &SecretString,
        route: &DatabaseRoute,
    ) -> Result<Self, ExecutionError> {
        let mut child = Command::new(binary)
            .env_clear()
            .env("FFDB_PROJECT_ID", route.project_id.to_string())
            .env("FFDB_DATABASE_ID", route.database_id.to_string())
            .env("FFDB_NODE_ID", route.node_id.to_string())
            .env("FFDB_ROUTE_GENERATION", route.generation.to_string())
            .env("FFDB_DATABASE_ROOT", database_root)
            .env("FFDB_BACKUP_ROOT", backup_root)
            .env(
                "FFDB_BACKUP_MASTER_KEY",
                backup_master_key_base64.expose_secret(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ExecutionError::Unavailable)?;
        let stdin = child.stdin.take().ok_or(ExecutionError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(ExecutionError::Unavailable)?;
        Ok(Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    async fn exchange(
        &mut self,
        request: &WorkerRequest,
    ) -> Result<WorkerExecution, ExecutionError> {
        let payload = serde_json::to_vec(request).map_err(|_| ExecutionError::Protocol)?;
        if payload.is_empty() || payload.len() > MAX_WORKER_FRAME_BYTES {
            return Err(ExecutionError::Protocol);
        }
        let length = u32::try_from(payload.len()).map_err(|_| ExecutionError::Protocol)?;
        self.stdin
            .write_all(&length.to_be_bytes())
            .await
            .map_err(|_| ExecutionError::Unavailable)?;
        self.stdin
            .write_all(&payload)
            .await
            .map_err(|_| ExecutionError::Unavailable)?;
        self.stdin
            .flush()
            .await
            .map_err(|_| ExecutionError::Unavailable)?;

        let length = self
            .stdout
            .read_u32()
            .await
            .map_err(|_| ExecutionError::Unavailable)?;
        let length = usize::try_from(length).map_err(|_| ExecutionError::Protocol)?;
        if length == 0 || length > MAX_WORKER_FRAME_BYTES {
            return Err(ExecutionError::Protocol);
        }
        let mut response = vec![0_u8; length];
        self.stdout
            .read_exact(&mut response)
            .await
            .map_err(|_| ExecutionError::Unavailable)?;
        match serde_json::from_slice::<WireResponse>(&response)
            .map_err(|_| ExecutionError::Protocol)?
        {
            WireResponse::Ok(response) => Ok(*response),
            WireResponse::Error(error) => Err(map_platform_error(error)),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
enum WireResponse {
    Ok(Box<WorkerExecution>),
    Error(PlatformError),
}

fn map_platform_error(error: PlatformError) -> ExecutionError {
    if !error.code.starts_with("query.")
        && !error.code.starts_with("rls.")
        && !error.code.starts_with("migration.")
        && !error.code.starts_with("sync.")
        && !error.code.starts_with("auth.")
        && !error.code.starts_with("storage.")
        && error.code != "project.stale_route"
    {
        tracing::warn!(code = %error.code, "worker returned an unmapped error code");
    }
    match error.code.as_str() {
        "project.stale_route" => ExecutionError::StaleGeneration,
        "query.deadline_exceeded" => ExecutionError::DeadlineExceeded,
        code if code.starts_with("query.")
            || code.starts_with("rls.")
            || code.starts_with("migration.")
            || code.starts_with("sync.")
            || code.starts_with("auth.")
            || code.starts_with("storage.") =>
        {
            ExecutionError::Rejected {
                code: code.to_owned(),
            }
        }
        _ => ExecutionError::Protocol,
    }
}

fn specific_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_executor_rejects_relative_or_broad_paths() {
        let result = ProcessWorkerExecutor::new(
            PathBuf::from("worker"),
            PathBuf::from("/"),
            PathBuf::from("/var/lib/ffdb/backups"),
            SecretString::from("test-only-secret"),
            ffdb_protocol::NodeId::new(),
            1,
            1,
        );
        assert!(matches!(result, Err(ExecutionError::Protocol)));
    }
}
