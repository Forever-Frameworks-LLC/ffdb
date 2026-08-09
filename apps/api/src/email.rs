use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json};
use ffdb_audit::AuditOutcome;
use ffdb_email::{
    EmailError, EmailTemplateRecord, RuntimeRenderer, ScalarValue, TemplateArtifactInput,
    parse_kind,
};
use ffdb_protocol::{DeveloperScope, ExecutionMode, ProjectId, RequestId};
use serde::{Deserialize, Serialize};

use crate::{
    ApiError, ApiState, append_audit, append_audit_best_effort, audit_unavailable,
    credential_error, developer, enforce_execution_rate_limit, now_ms, parse_project,
};

#[derive(Debug, Deserialize)]
pub(crate) struct TemplateQuery {
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImportArtifactRequest {
    kind: String,
    version: u64,
    source: String,
    source_sha256: String,
    subject_template: String,
    html_template: String,
    text_template: String,
    allowed_variables: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewRequest {
    variables: BTreeMap<String, ScalarValue>,
}

#[derive(Debug, Serialize)]
struct TemplateResponse {
    kind: &'static str,
    version: u64,
    source: String,
    source_sha256: String,
    subject_template: String,
    html_template: String,
    text_template: String,
    allowed_variables: BTreeSet<String>,
    artifact_status: String,
    compilation_errors: Vec<String>,
    compiled_at_ms: i64,
    published_at_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
struct PreviewResponse {
    subject: String,
    html: String,
    text: String,
}

impl From<EmailTemplateRecord> for TemplateResponse {
    fn from(record: EmailTemplateRecord) -> Self {
        let artifact = record.artifact;
        Self {
            kind: ffdb_email::kind_name(artifact.kind),
            version: artifact.version,
            source: record.source,
            source_sha256: artifact.source_sha256,
            subject_template: artifact.subject_template,
            html_template: artifact.html_template,
            text_template: artifact.text_template,
            allowed_variables: artifact.allowed_variables,
            artifact_status: record.artifact_status,
            compilation_errors: record.compilation_errors,
            compiled_at_ms: artifact.compiled_at_ms,
            published_at_ms: record.published_at_ms,
        }
    }
}

pub(crate) async fn templates(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<TemplateQuery>,
) -> Response {
    let (project_id, mode) = match authenticated(&state, &project, request_id, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let kind = match query.kind.as_deref().map(parse_kind).transpose() {
        Ok(value) => value,
        Err(error) => return email_error(error, request_id).into_response(),
    };
    if let Err(response) = begin(&state, project_id, request_id, &mode, "email.template.list").await
    {
        return response;
    }
    let result = match &state.email {
        Some(email) => email.templates(project_id.0, kind).await,
        None => Err(EmailError::RepositoryUnavailable),
    };
    finish(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.list",
        result,
        |templates| {
            (
                StatusCode::OK,
                Json(
                    templates
                        .into_iter()
                        .map(TemplateResponse::from)
                        .collect::<Vec<_>>(),
                ),
            )
                .into_response()
        },
    )
    .await
}

/// Imports the output of the isolated template compiler. The API never executes
/// `source`; it recomputes its SHA-256 and validates the bounded artifact again.
pub(crate) async fn import_artifact(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<ImportArtifactRequest>,
) -> Response {
    let (project_id, mode) = match authenticated(&state, &project, request_id, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let kind = match parse_kind(&payload.kind) {
        Ok(value) => value,
        Err(error) => return email_error(error, request_id).into_response(),
    };
    if let Err(response) = begin(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.artifact.import",
    )
    .await
    {
        return response;
    }
    let actor_api_key_id = match &mode {
        ExecutionMode::Developer(principal) => principal.api_key_id.0,
        ExecutionMode::EndUser(_) => {
            return email_error(EmailError::InvalidConfiguration, request_id).into_response();
        }
    };
    let result = match &state.email {
        Some(email) => {
            email
                .import_precompiled_artifact(
                    TemplateArtifactInput {
                        project_id: project_id.0,
                        kind,
                        version: payload.version,
                        source: payload.source,
                        subject_template: payload.subject_template,
                        html_template: payload.html_template,
                        text_template: payload.text_template,
                        allowed_variables: payload.allowed_variables,
                    },
                    &payload.source_sha256,
                    actor_api_key_id,
                    now_ms(),
                )
                .await
        }
        None => Err(EmailError::RepositoryUnavailable),
    };
    finish(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.artifact.import",
        result,
        |template| (StatusCode::CREATED, Json(TemplateResponse::from(template))).into_response(),
    )
    .await
}

pub(crate) async fn publish(
    State(state): State<ApiState>,
    Path((project, kind, version)): Path<(String, String, u64)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (project_id, mode) = match authenticated(&state, &project, request_id, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let kind = match parse_kind(&kind) {
        Ok(value) => value,
        Err(error) => return email_error(error, request_id).into_response(),
    };
    if let Err(response) = begin(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.publish",
    )
    .await
    {
        return response;
    }
    let result = match &state.email {
        Some(email) => email.publish_template(project_id.0, kind, version).await,
        None => Err(EmailError::RepositoryUnavailable),
    };
    finish(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.publish",
        result,
        |template| (StatusCode::OK, Json(TemplateResponse::from(template))).into_response(),
    )
    .await
}

pub(crate) async fn preview(
    State(state): State<ApiState>,
    Path((project, kind, version)): Path<(String, String, u64)>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<PreviewRequest>,
) -> Response {
    let (project_id, mode) = match authenticated(&state, &project, request_id, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let kind = match parse_kind(&kind) {
        Ok(value) => value,
        Err(error) => return email_error(error, request_id).into_response(),
    };
    if let Err(response) = begin(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.preview",
    )
    .await
    {
        return response;
    }
    let result = match &state.email {
        Some(email) => match email.template(project_id.0, kind, version).await {
            Ok(record) => RuntimeRenderer::new(record.artifact).and_then(|renderer| {
                renderer
                    .render(
                        "preview@example.test",
                        "FFDB Preview <preview@example.test>",
                        None,
                        &payload.variables,
                        format!("preview-{request_id}"),
                    )
                    .map(|rendered| PreviewResponse {
                        subject: rendered.subject,
                        html: rendered.html,
                        text: rendered.text,
                    })
            }),
            Err(error) => Err(error),
        },
        None => Err(EmailError::RepositoryUnavailable),
    };
    finish(
        &state,
        project_id,
        request_id,
        &mode,
        "email.template.preview",
        result,
        |preview| (StatusCode::OK, Json(preview)).into_response(),
    )
    .await
}

async fn authenticated(
    state: &ApiState,
    project: &str,
    request_id: RequestId,
    headers: &HeaderMap,
) -> Result<(ProjectId, ExecutionMode), Response> {
    let project_id = parse_project(project, request_id).map_err(|error| error.into_response())?;
    let principal = developer(state, project_id, headers, DeveloperScope::EmailManage)
        .await
        .map_err(|error| credential_error(error, request_id).into_response())?;
    Ok((project_id, ExecutionMode::Developer(principal)))
}

async fn begin(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    action: &str,
) -> Result<(), Response> {
    enforce_execution_rate_limit(state, project_id, request_id, mode, 2).await?;
    append_audit(
        state,
        project_id,
        request_id,
        mode,
        &format!("{action}.requested"),
        "email_template",
        AuditOutcome::Success,
    )
    .await
    .map_err(|()| audit_unavailable(request_id).into_response())
}

async fn finish<T, F>(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    action: &str,
    result: Result<T, EmailError>,
    success: F,
) -> Response
where
    F: FnOnce(T) -> Response,
{
    match result {
        Ok(value) => {
            append_audit_best_effort(
                state,
                project_id,
                request_id,
                mode,
                action,
                "email_template",
                AuditOutcome::Success,
            )
            .await;
            success(value)
        }
        Err(error) => {
            append_audit_best_effort(
                state,
                project_id,
                request_id,
                mode,
                action,
                "email_template",
                AuditOutcome::Failure,
            )
            .await;
            email_error(error, request_id).into_response()
        }
    }
}

fn email_error(error: EmailError, request_id: RequestId) -> ApiError {
    let (status, code, message) = match error {
        EmailError::TemplateVersionExists => (
            StatusCode::CONFLICT,
            "email.template_version_exists",
            "that template version already exists",
        ),
        EmailError::TemplateNotFound => (
            StatusCode::NOT_FOUND,
            "email.template_not_found",
            "email template version not found",
        ),
        EmailError::RepositoryUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "email.repository_unavailable",
            "email template repository is unavailable",
        ),
        EmailError::UnsafeArtifact => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "email.artifact_unsafe",
            "the precompiled artifact contains unsafe markup",
        ),
        _ => (
            StatusCode::BAD_REQUEST,
            "email.artifact_invalid",
            "the precompiled email artifact is invalid",
        ),
    };
    ApiError::new(status, code, message, request_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_name_does_not_claim_to_compile_source() {
        let request = ImportArtifactRequest {
            kind: "verification".to_owned(),
            version: 1,
            source: "export default function Email() {}".to_owned(),
            source_sha256: "0".repeat(64),
            subject_template: "Verify".to_owned(),
            html_template: "<p>Verify</p>".to_owned(),
            text_template: "Verify".to_owned(),
            allowed_variables: BTreeSet::new(),
        };
        assert_eq!(request.kind, "verification");
    }
}
