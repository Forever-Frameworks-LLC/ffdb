use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ffdb_sync_worker::{DirectorySyncStore, WorkerError};
use serde_json::json;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), WorkerError> {
    tracing_subscriber::fmt().json().init();
    let interval_seconds = read_positive_u64("FFDB_SYNC_MAINTENANCE_INTERVAL_SECONDS", 60)?;
    let stale_temporary_seconds = read_positive_u64("FFDB_SYNC_STALE_TEMPORARY_SECONDS", 3_600)?;
    if stale_temporary_seconds < 60 {
        return Err(WorkerError::InvalidConfiguration);
    }
    let state_directory =
        std::env::var("FFDB_SYNC_STATE_DIR").map_err(|_| WorkerError::InvalidConfiguration)?;
    let store = DirectorySyncStore::open(state_directory).await?;
    info!(
        event = "sync_worker.ready",
        mode = "maintenance_only",
        interval_seconds,
        "sync maintenance worker ready; user sync remains in database-worker RLS sessions"
    );
    println!(
        "{}",
        json!({"status":"ready","service":"ffdb-sync-worker","mode":"maintenance_only"})
    );
    let mut interval = tokio::time::interval(Duration::from_secs(interval_seconds));
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|_| WorkerError::Unavailable)?;
                break;
            }
            _ = interval.tick() => {
                match store.maintenance_pass(
                    epoch_ms()?,
                    i64::try_from(stale_temporary_seconds)
                        .ok()
                        .and_then(|seconds| seconds.checked_mul(1_000))
                        .ok_or(WorkerError::InvalidConfiguration)?,
                ).await {
                    Ok(report) => info!(
                        event = "sync_worker.maintenance",
                        checkpoints_scanned = report.checkpoints_scanned,
                        stale_temporary_files_removed = report.stale_temporary_files_removed,
                        "sync state maintenance completed"
                    ),
                    Err(error) => error!(
                        event = "sync_worker.maintenance_failed",
                        error = %error,
                        "sync state maintenance failed"
                    ),
                }
            }
        }
    }
    info!(event = "sync_worker.shutdown", "sync worker stopped");
    Ok(())
}

fn read_positive_u64(name: &str, default: u64) -> Result<u64, WorkerError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|parsed| *parsed > 0)
            .ok_or(WorkerError::InvalidConfiguration),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(WorkerError::InvalidConfiguration),
    }
}

fn epoch_ms() -> Result<i64, WorkerError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or(WorkerError::Unavailable)
}
