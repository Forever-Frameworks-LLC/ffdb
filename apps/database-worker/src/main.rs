use std::{env, io, path::PathBuf, process::ExitCode};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ffdb_database_worker::{DatabaseWorker, serve_frames};
use ffdb_protocol::{DatabaseId, DatabaseRoute, NodeId, ProjectId};
use ffdb_sqlite_runtime::RuntimeConfig;
use uuid::Uuid;

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ffdb=warn".into()),
        )
        .try_init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("database worker failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let project_id = required_uuid("FFDB_PROJECT_ID")?;
    let database_id = required_uuid("FFDB_DATABASE_ID")?;
    let node_id = required_uuid("FFDB_NODE_ID")?;
    let generation = env::var("FFDB_ROUTE_GENERATION")
        .map_err(|_| "FFDB_ROUTE_GENERATION is required".to_owned())?
        .parse::<u64>()
        .map_err(|_| "FFDB_ROUTE_GENERATION is invalid".to_owned())?;
    let database_root = absolute_path("FFDB_DATABASE_ROOT")?;
    let backup_root = absolute_path("FFDB_BACKUP_ROOT")?;
    let backup_master_key = STANDARD
        .decode(
            env::var("FFDB_BACKUP_MASTER_KEY")
                .map_err(|_| "FFDB_BACKUP_MASTER_KEY is required".to_owned())?,
        )
        .map_err(|_| "FFDB_BACKUP_MASTER_KEY is invalid".to_owned())?;
    if backup_master_key.len() < 32 {
        return Err("FFDB_BACKUP_MASTER_KEY must decode to at least 32 bytes".to_owned());
    }
    let worker = DatabaseWorker::open(
        DatabaseRoute {
            project_id: ProjectId(project_id),
            database_id: DatabaseId(database_id),
            node_id: NodeId(node_id),
            generation,
        },
        &database_root,
        &backup_root,
        RuntimeConfig::default(),
        &backup_master_key,
    )
    .map_err(|error| error.to_string())?;
    serve_frames(&worker, io::stdin().lock(), io::stdout().lock())
        .map_err(|error| error.to_string())
}

fn required_uuid(name: &str) -> Result<Uuid, String> {
    env::var(name)
        .map_err(|_| format!("{name} is required"))?
        .parse()
        .map_err(|_| format!("{name} is invalid"))
}

fn absolute_path(name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(env::var(name).map_err(|_| format!("{name} is required"))?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{name} must be absolute"))
    }
}
