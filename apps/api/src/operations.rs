use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_protocol::{
    AuditLogEntry, BackupId, BackupStatus, BackupSummary, DeveloperScope, ExecutionMode,
    MigrationSummary, QueryOptions, QueryRequest, RequestId, TransactionRequest, WorkerOperation,
};
use serde::Deserialize;
use sqlx::Row as _;

use super::{ApiError, ApiState, developer, dispatch};

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SeedRequest {
    sql: String,
}

pub(crate) async fn migration_history(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = developer(
        &state,
        project_id,
        &headers,
        DeveloperScope::DatabaseMigrate,
    )
    .await
    {
        return super::credential_error(error, request_id).into_response();
    }
    let limit = query.limit.unwrap_or(200);
    if !(1..=1_000).contains(&limit) {
        return invalid_limit(request_id);
    }
    let Some(pool) = &state.readiness_pool else {
        return unavailable(request_id);
    };
    let rows = match sqlx::query(
        "SELECT migration_id,name,encode(checksum,'hex') checksum,schema_version, \
         rolled_back_at IS NOT NULL rolled_back, \
         (extract(epoch FROM applied_at)*1000)::bigint applied_at_ms \
         FROM project_migrations WHERE project_id=$1 ORDER BY schema_version DESC LIMIT $2",
    )
    .bind(project_id.0)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    {
        Ok(value) => value,
        Err(_) => return unavailable(request_id),
    };
    let values = rows
        .into_iter()
        .map(|row| {
            let after = u64::try_from(row.get::<i64, _>("schema_version")).unwrap_or_default();
            MigrationSummary {
                id: row.get("migration_id"),
                name: row.get("name"),
                checksum: row.get("checksum"),
                status: if row.get::<bool, _>("rolled_back") {
                    "rolled_back".into()
                } else {
                    "applied".into()
                },
                schema_version_before: after.saturating_sub(1),
                schema_version_after: after,
                applied_at_ms: row.get("applied_at_ms"),
            }
        })
        .collect::<Vec<_>>();
    Json(values).into_response()
}

pub(crate) async fn seed(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<SeedRequest>,
) -> Response {
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let principal =
        match developer(&state, project_id, &headers, DeveloperScope::DatabaseQuery).await {
            Ok(value) => value,
            Err(error) => return super::credential_error(error, request_id).into_response(),
        };
    let statements = match ffdb_sql_parser::split_sql_statements(&payload.sql) {
        Ok(values) if !values.is_empty() => values,
        _ => {
            return ApiError::new(
                StatusCode::BAD_REQUEST,
                "seed.invalid_sql",
                "seed SQL is invalid",
                request_id,
            )
            .into_response();
        }
    };
    let transaction = TransactionRequest {
        statements: statements
            .into_iter()
            .map(|sql| QueryRequest {
                sql: sql.to_owned(),
                parameters: Vec::new(),
                options: QueryOptions::default(),
            })
            .collect(),
    };
    if transaction.validate(&state.limits).is_err() {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "seed.invalid_sql",
            "seed SQL exceeds configured limits",
            request_id,
        )
        .into_response();
    }
    dispatch(
        &state,
        project_id,
        request_id,
        ExecutionMode::Developer(principal),
        WorkerOperation::Transaction(transaction),
        None,
    )
    .await
}

pub(crate) async fn logs(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = developer(&state, project_id, &headers, DeveloperScope::LogsRead).await {
        return super::credential_error(error, request_id).into_response();
    }
    let limit = query.limit.unwrap_or(100);
    if !(1..=1_000).contains(&limit) {
        return invalid_limit(request_id);
    }
    let Some(pool) = &state.readiness_pool else {
        return unavailable(request_id);
    };
    let rows = match sqlx::query(
        "SELECT id,(extract(epoch FROM occurred_at)*1000)::bigint occurred_at_ms, \
         actor_kind,actor_id,action,resource_kind,resource_id,outcome,request_id \
         FROM audit_events WHERE project_id=$1 ORDER BY append_sequence DESC LIMIT $2",
    )
    .bind(project_id.0)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    {
        Ok(value) => value,
        Err(_) => return unavailable(request_id),
    };
    let values = rows
        .into_iter()
        .map(|row| {
            let actor_id: Option<uuid::Uuid> = row.get("actor_id");
            let resource_id: Option<uuid::Uuid> = row.get("resource_id");
            let actor_kind: String = row.get("actor_kind");
            let resource_kind: String = row.get("resource_kind");
            let outcome: String = row.get("outcome");
            AuditLogEntry {
                id: row.get::<uuid::Uuid, _>("id").to_string(),
                occurred_at_ms: row.get("occurred_at_ms"),
                actor: actor_id.map_or(actor_kind.clone(), |id| format!("{actor_kind}:{id}")),
                action: row.get("action"),
                resource: resource_id
                    .map_or(resource_kind.clone(), |id| format!("{resource_kind}:{id}")),
                outcome: if outcome == "failure" {
                    "failed".into()
                } else {
                    outcome
                },
                request_id: Some(row.get("request_id")),
            }
        })
        .collect::<Vec<_>>();
    Json(values).into_response()
}

pub(crate) async fn backups(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let project_id = match super::parse_project(&project, request_id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = developer(&state, project_id, &headers, DeveloperScope::BackupsManage).await
    {
        return super::credential_error(error, request_id).into_response();
    }
    let limit = query.limit.unwrap_or(200);
    if !(1..=1_000).contains(&limit) {
        return invalid_limit(request_id);
    }
    let Some(pool) = &state.readiness_pool else {
        return unavailable(request_id);
    };
    let rows = match sqlx::query(
        "SELECT id,state,size_bytes,encode(sha256,'hex') sha256, \
         (extract(epoch FROM created_at)*1000)::bigint created_at_ms, \
         (extract(epoch FROM verified_at)*1000)::bigint verified_at_ms \
         FROM backups WHERE project_id=$1 AND state<>'deleted' ORDER BY created_at DESC LIMIT $2",
    )
    .bind(project_id.0)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    {
        Ok(value) => value,
        Err(_) => return unavailable(request_id),
    };
    let values = rows
        .into_iter()
        .map(|row| {
            let state_name: String = row.get("state");
            let verified_at_ms: Option<i64> = row.get("verified_at_ms");
            BackupSummary {
                id: BackupId(row.get("id")),
                project_id,
                status: match state_name.as_str() {
                    "creating" => BackupStatus::Running,
                    "ready" => BackupStatus::Complete,
                    "restoring" => BackupStatus::Restoring,
                    "failed" | "deleted" => BackupStatus::Failed,
                    _ => BackupStatus::Failed,
                },
                size_bytes: row.get::<i64, _>("size_bytes").try_into().ok(),
                sha256: Some(row.get("sha256")),
                created_at_ms: row.get("created_at_ms"),
                completed_at_ms: verified_at_ms,
                last_restore_test_ms: None,
            }
        })
        .collect::<Vec<_>>();
    Json(values).into_response()
}

fn invalid_limit(request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "list.invalid_limit",
        "limit must be between 1 and 1000",
        request_id,
    )
    .into_response()
}

fn unavailable(request_id: RequestId) -> Response {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_plane.unavailable",
        "control plane is temporarily unavailable",
        request_id,
    )
    .into_response()
}
