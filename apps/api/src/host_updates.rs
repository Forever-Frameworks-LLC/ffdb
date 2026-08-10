//! Privilege-separated host update control.
//!
//! The API never accepts or constructs a shell command. It invokes one
//! root-owned executable with a closed set of argv forms. The updater queues
//! work in root-owned durable state before returning, so an install job remains
//! observable while this API process is restarted.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use axum::Json;
use axum::extract::{Extension, Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use chrono::DateTime;
use ffdb_audit::AuditOutcome;
use ffdb_protocol::{
    HostUpdateCapabilities, HostUpdateChannel, HostUpdateJob, HostUpdateJobState,
    HostUpdateOperation, HostUpdateRelease, HostUpdateSettings, HostUpdateStatus,
    HostUpdateVersionRequest, InstanceAdministratorRole, RequestId,
};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use uuid::Uuid;

use super::instance::InstanceServiceError;
use super::management::{
    authenticated, enforce_platform_user_rate, require_management_audit, terminal_management_audit,
};
use super::{ApiError, ApiState, now_ms};

const MAX_UPDATER_OUTPUT_BYTES: u64 = 64 * 1024;
const UPDATER_TIMEOUT: Duration = Duration::from_secs(30);
const RECENT_AUTHENTICATION_MS: i64 = 15 * 60 * 1_000;

#[async_trait]
pub trait HostUpdater: Send + Sync {
    async fn inspect(&self) -> Result<HostUpdateStatus, HostUpdateError>;
    async fn check(&self) -> Result<HostUpdateJob, HostUpdateError>;
    async fn install(&self, version: &str) -> Result<HostUpdateJob, HostUpdateError>;
    async fn rollback(&self, version: &str) -> Result<HostUpdateJob, HostUpdateError>;
    async fn job(&self, job_id: Uuid) -> Result<HostUpdateJob, HostUpdateError>;
    async fn configure(
        &self,
        settings: &HostUpdateSettings,
    ) -> Result<HostUpdateJob, HostUpdateError>;
}

#[derive(Clone, Debug)]
pub struct CommandHostUpdater {
    executable: PathBuf,
}

impl CommandHostUpdater {
    pub fn new(executable: impl Into<PathBuf>) -> Result<Self, HostUpdateError> {
        let executable = executable.into();
        if !executable.is_absolute() || executable.as_os_str().is_empty() {
            return Err(HostUpdateError::InvalidConfiguration);
        }
        Ok(Self { executable })
    }

    pub fn production() -> Result<Self, HostUpdateError> {
        Self::new(Path::new("/usr/local/bin/ffdb-update"))
    }

    async fn invoke(&self, arguments: &[String]) -> Result<String, HostUpdateError> {
        let mut command = Command::new(&self.executable);
        command
            .args(arguments)
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(map_spawn_error)?;
        let stdout = child.stdout.take().ok_or(HostUpdateError::Unavailable)?;
        let stderr = child.stderr.take().ok_or(HostUpdateError::Unavailable)?;
        let stdout_task = tokio::spawn(read_bounded(stdout));
        let stderr_task = tokio::spawn(read_bounded(stderr));
        let status = match tokio::time::timeout(UPDATER_TIMEOUT, child.wait()).await {
            Ok(result) => result.map_err(|_| HostUpdateError::Unavailable)?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(HostUpdateError::Timeout);
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|_| HostUpdateError::Unavailable)??;
        let stderr = stderr_task
            .await
            .map_err(|_| HostUpdateError::Unavailable)??;
        if !status.success() {
            return Err(parse_updater_failure(&stderr));
        }
        String::from_utf8(stdout).map_err(|_| HostUpdateError::InvalidResponse)
    }

    async fn submit(
        &self,
        operation: &str,
        version: Option<&str>,
    ) -> Result<HostUpdateJob, HostUpdateError> {
        if let Some(value) = version {
            validate_release_version(value)?;
        }
        let mut arguments = vec!["submit".to_owned(), operation.to_owned()];
        if let Some(value) = version {
            arguments.push(value.to_owned());
        }
        let output = self.invoke(&arguments).await?;
        if let Ok(job) = parse_job(&output) {
            return Ok(job);
        }
        let job_id = parse_job_id(output.trim())?;
        self.job(job_id).await
    }
}

#[async_trait]
impl HostUpdater for CommandHostUpdater {
    async fn inspect(&self) -> Result<HostUpdateStatus, HostUpdateError> {
        let output = self.invoke(&["inspect".to_owned()]).await?;
        let value: WireStatus =
            serde_json::from_str(&output).map_err(|_| HostUpdateError::InvalidResponse)?;
        value.try_into()
    }

    async fn check(&self) -> Result<HostUpdateJob, HostUpdateError> {
        self.submit("check", None).await
    }

    async fn install(&self, version: &str) -> Result<HostUpdateJob, HostUpdateError> {
        self.submit("install", Some(version)).await
    }

    async fn rollback(&self, version: &str) -> Result<HostUpdateJob, HostUpdateError> {
        self.submit("rollback", Some(version)).await
    }

    async fn job(&self, job_id: Uuid) -> Result<HostUpdateJob, HostUpdateError> {
        let output = self
            .invoke(&["job".to_owned(), job_id.hyphenated().to_string()])
            .await?;
        parse_job(&output)
    }

    async fn configure(
        &self,
        settings: &HostUpdateSettings,
    ) -> Result<HostUpdateJob, HostUpdateError> {
        validate_settings(settings)?;
        let window = settings.maintenance_window_start.as_ref().map_or_else(
            || "disabled".to_owned(),
            |start| format!("{start}/{}", settings.maintenance_window_duration_minutes),
        );
        let arguments = vec![
            "submit".to_owned(),
            "configure".to_owned(),
            "--channel".to_owned(),
            channel_name(settings.channel).to_owned(),
            "--automatic-checks".to_owned(),
            settings.automatic_checks.to_string(),
            "--check-interval-hours".to_owned(),
            settings.check_interval_hours.to_string(),
            "--automatic-apply".to_owned(),
            settings.automatic_apply.to_string(),
            "--maintenance-window".to_owned(),
            window,
        ];
        let output = self.invoke(&arguments).await?;
        if let Ok(job) = parse_job(&output) {
            return Ok(job);
        }
        self.job(parse_job_id(output.trim())?).await
    }
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, HostUpdateError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_UPDATER_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| HostUpdateError::Unavailable)?;
    if bytes.len() as u64 > MAX_UPDATER_OUTPUT_BYTES {
        return Err(HostUpdateError::InvalidResponse);
    }
    Ok(bytes)
}

fn parse_job(output: &str) -> Result<HostUpdateJob, HostUpdateError> {
    let value: WireJob =
        serde_json::from_str(output).map_err(|_| HostUpdateError::InvalidResponse)?;
    value.try_into()
}

fn parse_job_id(value: &str) -> Result<Uuid, HostUpdateError> {
    let normalized = if value.len() == 32 {
        format!(
            "{}-{}-{}-{}-{}",
            &value[..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..]
        )
    } else {
        value.to_owned()
    };
    Uuid::parse_str(&normalized).map_err(|_| HostUpdateError::InvalidResponse)
}

pub(crate) fn validate_release_version(value: &str) -> Result<(), HostUpdateError> {
    if value.len() > 64 || value.starts_with('v') || value.trim() != value {
        return Err(HostUpdateError::InvalidVersion);
    }
    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() != 3
        || segments.iter().any(|segment| {
            segment.is_empty()
                || !segment.bytes().all(|byte| byte.is_ascii_digit())
                || (segment.len() > 1 && segment.starts_with('0'))
        })
    {
        return Err(HostUpdateError::InvalidVersion);
    }
    Ok(())
}

pub(crate) fn validate_settings(value: &HostUpdateSettings) -> Result<(), HostUpdateError> {
    if !(1..=168).contains(&value.check_interval_hours)
        || !(15..=1_440).contains(&value.maintenance_window_duration_minutes)
        || value.automatic_apply && value.maintenance_window_start.is_none()
        || value
            .maintenance_window_start
            .as_deref()
            .is_some_and(|start| !valid_utc_time(start))
    {
        return Err(HostUpdateError::InvalidSettings);
    }
    Ok(())
}

fn valid_utc_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![0, 1, 3, 4]
            .iter()
            .all(|index| bytes[*index].is_ascii_digit())
    {
        return false;
    }
    let hour = (bytes[0] - b'0') * 10 + bytes[1] - b'0';
    let minute = (bytes[3] - b'0') * 10 + bytes[4] - b'0';
    hour < 24 && minute < 60
}

fn channel_name(value: HostUpdateChannel) -> &'static str {
    match value {
        HostUpdateChannel::Stable => "stable",
    }
}

fn timestamp_ms(value: &str) -> Result<i64, HostUpdateError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.timestamp_millis())
        .map_err(|_| HostUpdateError::InvalidResponse)
}

fn optional_timestamp_ms(value: Option<String>) -> Result<Option<i64>, HostUpdateError> {
    value.map(|value| timestamp_ms(&value)).transpose()
}

fn map_spawn_error(error: std::io::Error) -> HostUpdateError {
    if error.kind() == std::io::ErrorKind::NotFound {
        HostUpdateError::NotInstalled
    } else {
        HostUpdateError::Unavailable
    }
}

fn parse_updater_failure(stderr: &[u8]) -> HostUpdateError {
    #[derive(Deserialize)]
    struct Failure {
        code: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        retryable: bool,
    }
    let text = std::str::from_utf8(stderr).unwrap_or_default().trim();
    if let Ok(failure) = serde_json::from_str::<Failure>(text) {
        if failure.code.is_empty()
            || failure.code.len() > 64
            || !failure
                .code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || failure.message.len() > 4_096
        {
            return HostUpdateError::InvalidResponse;
        }
        return HostUpdateError::Updater {
            code: failure.code,
            message: failure.message,
            retryable: failure.retryable,
        };
    }
    HostUpdateError::Unavailable
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum HostUpdateError {
    #[error("host updater path is invalid")]
    InvalidConfiguration,
    #[error("release version is invalid")]
    InvalidVersion,
    #[error("host update settings are invalid")]
    InvalidSettings,
    #[error("host update job was not found")]
    NotFound,
    #[error("host updater is not installed")]
    NotInstalled,
    #[error("host updater timed out")]
    Timeout,
    #[error("host updater response is invalid")]
    InvalidResponse,
    #[error("host updater is unavailable")]
    Unavailable,
    #[error("host updater rejected the operation: {code}")]
    Updater {
        code: String,
        message: String,
        retryable: bool,
    },
}

#[derive(Deserialize)]
struct WireStatus {
    #[serde(default = "default_capabilities")]
    capabilities: HostUpdateCapabilities,
    #[serde(default)]
    state_schema: u32,
    #[serde(default)]
    minimum_rollback_version: Option<String>,
    #[serde(default)]
    signature_identity: Option<String>,
    installed_version: Option<String>,
    available_version: Option<String>,
    update_available: bool,
    last_check_at: Option<String>,
    active_job: Option<WireJob>,
    #[serde(default)]
    releases: Vec<HostUpdateRelease>,
    settings: HostUpdateSettings,
}

impl TryFrom<WireStatus> for HostUpdateStatus {
    type Error = HostUpdateError;

    fn try_from(value: WireStatus) -> Result<Self, Self::Error> {
        validate_settings(&value.settings)?;
        if let Some(version) = value.installed_version.as_deref() {
            validate_release_version(version).map_err(|_| HostUpdateError::InvalidResponse)?;
        }
        if let Some(version) = value.available_version.as_deref() {
            validate_release_version(version).map_err(|_| HostUpdateError::InvalidResponse)?;
        }
        for release in &value.releases {
            validate_release_version(&release.version)
                .map_err(|_| HostUpdateError::InvalidResponse)?;
            if let Some(version) = release.minimum_rollback_version.as_deref() {
                validate_release_version(version).map_err(|_| HostUpdateError::InvalidResponse)?;
            }
            if release
                .signature_identity
                .as_deref()
                .is_some_and(|identity| identity.len() > 512)
                || release
                    .release_url
                    .as_deref()
                    .is_some_and(|url| !valid_release_url(&release.version, url))
            {
                return Err(HostUpdateError::InvalidResponse);
            }
        }
        if let Some(version) = value.minimum_rollback_version.as_deref() {
            validate_release_version(version).map_err(|_| HostUpdateError::InvalidResponse)?;
        }
        if value
            .signature_identity
            .as_deref()
            .is_some_and(|identity| identity.len() > 512)
        {
            return Err(HostUpdateError::InvalidResponse);
        }
        Ok(Self {
            supported: true,
            unavailable_reason: None,
            capabilities: value.capabilities,
            state_schema: value.state_schema,
            minimum_rollback_version: value.minimum_rollback_version,
            signature_identity: value.signature_identity,
            installed_version: value.installed_version,
            available_version: value.available_version,
            update_available: value.update_available,
            last_check_at_ms: optional_timestamp_ms(value.last_check_at)?,
            active_job: value.active_job.map(TryInto::try_into).transpose()?,
            releases: value.releases,
            settings: value.settings,
        })
    }
}

fn valid_release_url(version: &str, value: &str) -> bool {
    value == format!("https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v{version}")
}

#[derive(Deserialize)]
struct WireJob {
    job_id: String,
    operation: HostUpdateOperation,
    requested_version: Option<String>,
    state: HostUpdateJobState,
    phase: String,
    installed_version: Option<String>,
    available_version: Option<String>,
    previous_version: Option<String>,
    backup_path: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    retryable: bool,
    created_at: String,
    updated_at: String,
}

impl TryFrom<WireJob> for HostUpdateJob {
    type Error = HostUpdateError;

    fn try_from(value: WireJob) -> Result<Self, Self::Error> {
        let job_id = parse_job_id(&value.job_id)?.hyphenated().to_string();
        for version in [
            value.requested_version.as_deref(),
            value.installed_version.as_deref(),
            value.available_version.as_deref(),
            value.previous_version.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_release_version(version).map_err(|_| HostUpdateError::InvalidResponse)?;
        }
        if value.phase.len() > 128
            || value.message.len() > 4_096
            || value
                .error_code
                .as_deref()
                .is_some_and(|code| code.len() > 64)
            || value
                .backup_path
                .as_deref()
                .is_some_and(|path| path.len() > 4_096)
        {
            return Err(HostUpdateError::InvalidResponse);
        }
        Ok(Self {
            job_id,
            operation: value.operation,
            requested_version: value.requested_version,
            state: value.state,
            phase: value.phase,
            installed_version: value.installed_version,
            available_version: value.available_version,
            previous_version: value.previous_version,
            backup_path: value.backup_path,
            message: value.message,
            error_code: value.error_code,
            retryable: value.retryable,
            created_at_ms: timestamp_ms(&value.created_at)?,
            updated_at_ms: timestamp_ms(&value.updated_at)?,
        })
    }
}

fn default_capabilities() -> HostUpdateCapabilities {
    HostUpdateCapabilities {
        check: true,
        install: true,
        rollback: true,
        automatic_checks: true,
        automatic_apply: true,
    }
}

fn unsupported_status(reason: &str) -> HostUpdateStatus {
    HostUpdateStatus {
        supported: false,
        unavailable_reason: Some(reason.to_owned()),
        capabilities: HostUpdateCapabilities {
            check: false,
            install: false,
            rollback: false,
            automatic_checks: false,
            automatic_apply: false,
        },
        state_schema: 0,
        minimum_rollback_version: None,
        signature_identity: None,
        installed_version: option_env!("CARGO_PKG_VERSION").map(ToOwned::to_owned),
        available_version: None,
        update_available: false,
        last_check_at_ms: None,
        active_job: None,
        releases: Vec::new(),
        settings: HostUpdateSettings {
            channel: HostUpdateChannel::Stable,
            automatic_checks: false,
            check_interval_hours: 24,
            automatic_apply: false,
            maintenance_window_start: None,
            maintenance_window_duration_minutes: 60,
        },
    }
}

pub(crate) async fn status(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorized_actor(&state, &headers, request_id, false).await {
        return response;
    }
    let Some(updater) = state.host_updates.clone() else {
        return Json(unsupported_status("host updater is not installed")).into_response();
    };
    match updater.inspect().await {
        Ok(value) => Json(value).into_response(),
        Err(HostUpdateError::NotInstalled) => {
            Json(unsupported_status("host updater is not installed")).into_response()
        }
        Err(error) => update_error(error, request_id),
    }
}

async fn authorized_actor(
    state: &ApiState,
    headers: &HeaderMap,
    request_id: RequestId,
    recent_authentication: bool,
) -> Result<ffdb_protocol::UserId, Response> {
    let (management, identity) = authenticated(state, headers, request_id)
        .await
        .map_err(|error| error.into_response())?;
    enforce_platform_user_rate(state, identity.user_id, request_id).await?;
    let Some(instance) = state.instance.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "instance.unavailable",
            "instance administration is temporarily unavailable",
            request_id,
        )
        .into_response());
    };
    let role = instance
        .authorize_host_updates(identity.user_id)
        .await
        .map_err(|error| instance_auth_error(error, request_id))?;
    if recent_authentication
        && now_ms().saturating_sub(identity.authenticated_at_ms) > RECENT_AUTHENTICATION_MS
    {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "platform_auth.reauthentication_required",
            "sign in again before changing the host installation",
            request_id,
        )
        .into_response());
    }
    let _ = (management, role == InstanceAdministratorRole::Owner);
    Ok(identity.user_id)
}

pub(crate) async fn check(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (updater, actor) = match authorized(&state, &headers, request_id, false).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_job(
        &state,
        updater.check(),
        actor,
        request_id,
        "instance.host_update.check.enqueue",
        None,
    )
    .await
}

pub(crate) async fn install(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<HostUpdateVersionRequest>,
) -> Response {
    if let Err(error) = validate_release_version(&payload.version) {
        return update_error(error, request_id);
    }
    let (updater, actor) = match authorized(&state, &headers, request_id, true).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_job(
        &state,
        updater.install(&payload.version),
        actor,
        request_id,
        "instance.host_update.install.enqueue",
        None,
    )
    .await
}

pub(crate) async fn rollback(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<HostUpdateVersionRequest>,
) -> Response {
    if let Err(error) = validate_release_version(&payload.version) {
        return update_error(error, request_id);
    }
    let (updater, actor) = match authorized(&state, &headers, request_id, true).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_job(
        &state,
        updater.rollback(&payload.version),
        actor,
        request_id,
        "instance.host_update.rollback.enqueue",
        None,
    )
    .await
}

pub(crate) async fn job(
    State(state): State<ApiState>,
    AxumPath(job_id): AxumPath<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let job_id = match Uuid::parse_str(&job_id) {
        Ok(value) => value,
        Err(_) => {
            return ApiError::new(
                StatusCode::BAD_REQUEST,
                "host_update.invalid_job_id",
                "host update job id is invalid",
                request_id,
            )
            .into_response();
        }
    };
    let (updater, _) = match authorized(&state, &headers, request_id, false).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match updater.job(job_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => update_error(error, request_id),
    }
}

pub(crate) async fn settings(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (updater, _) = match authorized(&state, &headers, request_id, false).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match updater.inspect().await {
        Ok(value) => Json(value.settings).into_response(),
        Err(error) => update_error(error, request_id),
    }
}

pub(crate) async fn configure(
    State(state): State<ApiState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<HostUpdateSettings>,
) -> Response {
    if let Err(error) = validate_settings(&payload) {
        return update_error(error, request_id);
    }
    let (updater, actor) = match authorized(&state, &headers, request_id, true).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    audited_job(
        &state,
        updater.configure(&payload),
        actor,
        request_id,
        "instance.host_update.configure.enqueue",
        None,
    )
    .await
}

async fn authorized(
    state: &ApiState,
    headers: &HeaderMap,
    request_id: RequestId,
    recent_authentication: bool,
) -> Result<(std::sync::Arc<dyn HostUpdater>, ffdb_protocol::UserId), Response> {
    let actor = authorized_actor(state, headers, request_id, recent_authentication).await?;
    let updater = state.host_updates.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "host_update.unavailable",
            "host updates are not available on this installation",
            request_id,
        )
        .into_response()
    })?;
    Ok((updater, actor))
}

fn instance_auth_error(error: InstanceServiceError, request_id: RequestId) -> Response {
    match error {
        InstanceServiceError::Forbidden => ApiError::new(
            StatusCode::FORBIDDEN,
            "instance.forbidden",
            "operation is not permitted",
            request_id,
        ),
        _ => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "instance.unavailable",
            "instance administration is temporarily unavailable",
            request_id,
        ),
    }
    .into_response()
}

async fn audited_job(
    state: &ApiState,
    operation: impl std::future::Future<Output = Result<HostUpdateJob, HostUpdateError>>,
    actor: ffdb_protocol::UserId,
    request_id: RequestId,
    action: &str,
    resource_id: Option<Uuid>,
) -> Response {
    if let Err(response) = require_management_audit(
        state,
        None,
        None,
        Some(actor),
        request_id,
        action,
        "host_update_job",
        resource_id,
    )
    .await
    {
        return response;
    }
    match operation.await {
        Ok(job) => {
            terminal_management_audit(
                state,
                None,
                None,
                Some(actor),
                request_id,
                action,
                "host_update_job",
                Uuid::parse_str(&job.job_id).ok(),
                AuditOutcome::Success,
            )
            .await;
            (StatusCode::ACCEPTED, Json(job)).into_response()
        }
        Err(error) => {
            terminal_management_audit(
                state,
                None,
                None,
                Some(actor),
                request_id,
                action,
                "host_update_job",
                resource_id,
                AuditOutcome::Failure,
            )
            .await;
            update_error(error, request_id)
        }
    }
}

fn update_error(error: HostUpdateError, request_id: RequestId) -> Response {
    let (status, code, message) = match error {
        HostUpdateError::InvalidVersion => (
            StatusCode::BAD_REQUEST,
            "host_update.invalid_version",
            "version must be an exact stable release such as 0.3.3",
        ),
        HostUpdateError::InvalidSettings => (
            StatusCode::BAD_REQUEST,
            "host_update.invalid_settings",
            "host update settings are invalid",
        ),
        HostUpdateError::NotFound => (
            StatusCode::NOT_FOUND,
            "host_update.job_not_found",
            "host update job was not found",
        ),
        HostUpdateError::Updater { ref code, .. } if code == "busy" => (
            StatusCode::CONFLICT,
            "host_update.busy",
            "another host update operation is already active",
        ),
        HostUpdateError::Updater { ref code, .. } if code == "not_found" => (
            StatusCode::NOT_FOUND,
            "host_update.job_not_found",
            "host update job was not found",
        ),
        HostUpdateError::Updater { ref code, .. } if code == "incompatible_rollback" => (
            StatusCode::CONFLICT,
            "host_update.incompatible_rollback",
            "that release cannot safely read the current control-plane schema",
        ),
        HostUpdateError::Updater { ref code, .. }
            if matches!(
                code.as_str(),
                "invalid_request" | "invalid_version" | "invalid_job_id"
            ) =>
        {
            (
                StatusCode::BAD_REQUEST,
                "host_update.invalid_request",
                "host updater rejected the request",
            )
        }
        HostUpdateError::Updater { ref code, .. }
            if matches!(
                code.as_str(),
                "signature_verification_failed"
                    | "backup_failed"
                    | "install_failed"
                    | "health_check_failed"
            ) =>
        {
            (
                StatusCode::BAD_GATEWAY,
                "host_update.operation_failed",
                "host update safety checks failed",
            )
        }
        HostUpdateError::InvalidConfiguration
        | HostUpdateError::NotInstalled
        | HostUpdateError::Timeout
        | HostUpdateError::InvalidResponse
        | HostUpdateError::Unavailable
        | HostUpdateError::Updater { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "host_update.unavailable",
            "host updater is temporarily unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::to_bytes;
    use ffdb_audit::InMemoryAuditSink;
    use ffdb_auth::{
        AeadSigningKeyEnvelope, Argon2PasswordHasher, PasswordHasher as _, SecretString,
    };
    use ffdb_database_router::{DatabaseExecutor, DatabaseRouter, ExecutionError, RoutingError};
    use ffdb_protocol::{
        AuthContext, DatabaseRoute, DeveloperPrincipal, DeveloperScope, ExecutionMode, NodeId,
        PlatformBillingUnit, ProjectId, ResourceLimits, SessionId, WorkerExecution, WorkerRequest,
    };
    use sqlx::PgPool;
    use tokio::sync::Mutex;
    use url::Url;

    use crate::instance::{InstanceService, InstanceServiceConfig};
    use crate::management::{ManagementState, ManagementStateConfig};
    use crate::{CredentialError, CredentialVerifier};

    use super::*;

    #[derive(Debug)]
    struct NoopServices;

    #[async_trait]
    impl DatabaseRouter for NoopServices {
        async fn resolve(&self, _project_id: ProjectId) -> Result<DatabaseRoute, RoutingError> {
            Err(RoutingError::Unavailable)
        }
    }

    #[async_trait]
    impl DatabaseExecutor for NoopServices {
        async fn execute(
            &self,
            _route: &DatabaseRoute,
            _request: WorkerRequest,
        ) -> Result<WorkerExecution, ExecutionError> {
            Err(ExecutionError::Unavailable)
        }
    }

    #[async_trait]
    impl CredentialVerifier for NoopServices {
        async fn verify_query_credential(
            &self,
            _project_id: ProjectId,
            _bearer_token: &str,
        ) -> Result<ExecutionMode, CredentialError> {
            Err(CredentialError::Unavailable)
        }

        async fn verify_developer_credential(
            &self,
            _project_id: ProjectId,
            _bearer_token: &str,
            _required_scope: DeveloperScope,
        ) -> Result<DeveloperPrincipal, CredentialError> {
            Err(CredentialError::Unavailable)
        }

        async fn verify_end_user_credential(
            &self,
            _project_id: ProjectId,
            _bearer_token: &str,
        ) -> Result<AuthContext, CredentialError> {
            Err(CredentialError::Unavailable)
        }

        async fn verify_end_user_session_credential(
            &self,
            _project_id: ProjectId,
            _bearer_token: &str,
        ) -> Result<(AuthContext, Option<SessionId>), CredentialError> {
            Err(CredentialError::Unavailable)
        }
    }

    #[derive(Debug, Default)]
    struct RecordingUpdater {
        calls: Mutex<Vec<String>>,
    }

    impl RecordingUpdater {
        async fn calls(&self) -> Vec<String> {
            self.calls.lock().await.clone()
        }
    }

    #[async_trait]
    impl HostUpdater for RecordingUpdater {
        async fn inspect(&self) -> Result<HostUpdateStatus, HostUpdateError> {
            self.calls.lock().await.push("inspect".to_owned());
            Ok(unsupported_status("test"))
        }

        async fn check(&self) -> Result<HostUpdateJob, HostUpdateError> {
            self.calls.lock().await.push("check".to_owned());
            Ok(queued_job(HostUpdateOperation::Check, None))
        }

        async fn install(&self, version: &str) -> Result<HostUpdateJob, HostUpdateError> {
            self.calls.lock().await.push(format!("install:{version}"));
            Ok(queued_job(
                HostUpdateOperation::Install,
                Some(version.to_owned()),
            ))
        }

        async fn rollback(&self, version: &str) -> Result<HostUpdateJob, HostUpdateError> {
            self.calls.lock().await.push(format!("rollback:{version}"));
            Ok(queued_job(
                HostUpdateOperation::Rollback,
                Some(version.to_owned()),
            ))
        }

        async fn job(&self, job_id: Uuid) -> Result<HostUpdateJob, HostUpdateError> {
            self.calls.lock().await.push(format!("job:{job_id}"));
            Ok(queued_job(HostUpdateOperation::Check, None))
        }

        async fn configure(
            &self,
            _settings: &HostUpdateSettings,
        ) -> Result<HostUpdateJob, HostUpdateError> {
            self.calls.lock().await.push("configure".to_owned());
            Ok(queued_job(HostUpdateOperation::Configure, None))
        }
    }

    fn queued_job(
        operation: HostUpdateOperation,
        requested_version: Option<String>,
    ) -> HostUpdateJob {
        HostUpdateJob {
            job_id: Uuid::now_v7().to_string(),
            operation,
            requested_version,
            state: HostUpdateJobState::Queued,
            phase: "queued".to_owned(),
            installed_version: Some("0.3.2".to_owned()),
            available_version: Some("0.3.3".to_owned()),
            previous_version: None,
            backup_path: None,
            message: "update job accepted".to_owned(),
            error_code: None,
            retryable: false,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        }
    }

    struct HandlerFixture {
        state: ApiState,
        headers: HeaderMap,
        updater: Arc<RecordingUpdater>,
        audit: Arc<InMemoryAuditSink>,
    }

    async fn handler_fixture(
        authenticated_at_ms: i64,
        administrator: bool,
        updater_installed: bool,
    ) -> Result<Option<HandlerFixture>, Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(None);
        };
        let pool = PgPool::connect(&database_url).await?;
        let user_id = ffdb_protocol::UserId::new();
        let email = format!("host-update-{user_id}@example.test");
        let password = format!("Host-update-test-{user_id}");
        let password_hash =
            Argon2PasswordHasher::default().hash(SecretString::new(password.clone()))?;
        sqlx::query(
            "INSERT INTO platform_users \
             (id,email,password_phc,email_verified_at,created_at,updated_at) \
             VALUES ($1,$2,$3,now(),now(),now())",
        )
        .bind(user_id.0)
        .bind(&email)
        .bind(password_hash.as_phc())
        .execute(&pool)
        .await?;
        if administrator {
            sqlx::query(
                "INSERT INTO instance_administrators (user_id,role,granted_by) \
                 VALUES ($1,'admin',$1)",
            )
            .bind(user_id.0)
            .execute(&pool)
            .await?;
        }

        let management = Arc::new(ManagementState::new(
            pool.clone(),
            ManagementStateConfig {
                platform_session_pepper: vec![11; 32],
                api_key_pepper: vec![12; 32],
                invitation_pepper: vec![13; 32],
                signing_key_envelope: AeadSigningKeyEnvelope::new(vec![14; 32], 1)?,
                bootstrap_token: "test-bootstrap-token".to_owned(),
                node_id: NodeId::new(),
                public_base_url: Url::parse("https://ffdb.example.test/")?,
                email_from_address: "noreply@example.test".to_owned(),
                billing_provider: None,
                pro_billing_unit: PlatformBillingUnit::Organization,
            },
        )?);
        let session = management
            .platform_auth
            .sign_in(&email, SecretString::new(password), authenticated_at_ms)
            .await?;
        let instance = Arc::new(
            InstanceService::new(
                pool.clone(),
                InstanceServiceConfig {
                    master_key: vec![15; 32],
                    key_version: 1,
                    connect_secret_key: None,
                    connect_webhook_secret: None,
                    billing: None,
                },
            )
            .map_err(|error| std::io::Error::other(format!("instance setup failed: {error:?}")))?,
        );
        let updater = Arc::new(RecordingUpdater::default());
        let host_updates = updater_installed.then(|| updater.clone() as Arc<dyn HostUpdater>);
        let services = Arc::new(NoopServices);
        let audit = Arc::new(InMemoryAuditSink::default());
        let state = ApiState {
            router: services.clone(),
            executor: services.clone(),
            credentials: services,
            limits: ResourceLimits::default(),
            metrics: None,
            observability: None,
            management: Some(management),
            project_auth: None,
            storage: None,
            email: None,
            usage_metering: None,
            commerce: None,
            instance: Some(instance),
            host_updates,
            cors_allowed_origins: Vec::new(),
            trusted_proxy_cidrs: Vec::new(),
            rate_limiter: None,
            audit: audit.clone(),
            readiness_pool: Some(pool),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", session.plaintext.expose()).parse()?,
        );
        Ok(Some(HandlerFixture {
            state,
            headers,
            updater,
            audit,
        }))
    }

    fn settings() -> HostUpdateSettings {
        HostUpdateSettings {
            channel: HostUpdateChannel::Stable,
            automatic_checks: true,
            check_interval_hours: 24,
            automatic_apply: false,
            maintenance_window_start: None,
            maintenance_window_duration_minutes: 60,
        }
    }

    #[test]
    fn release_versions_are_exact_and_cannot_be_shell_fragments() {
        for valid in ["0.3.3", "1.0.0", "24.12.7"] {
            assert_eq!(validate_release_version(valid), Ok(()));
        }
        for invalid in [
            "v0.3.3",
            "0.3",
            "0.3.3-beta.1",
            "01.3.3",
            "0.3.3;id",
            "0.3.3\n--help",
            "../0.3.3",
        ] {
            assert_eq!(
                validate_release_version(invalid),
                Err(HostUpdateError::InvalidVersion)
            );
        }
    }

    #[test]
    fn automatic_apply_requires_a_bounded_utc_window() {
        let mut value = settings();
        value.automatic_apply = true;
        assert_eq!(
            validate_settings(&value),
            Err(HostUpdateError::InvalidSettings)
        );
        value.maintenance_window_start = Some("02:30".to_owned());
        assert_eq!(validate_settings(&value), Ok(()));
        value.maintenance_window_start = Some("24:00".to_owned());
        assert_eq!(
            validate_settings(&value),
            Err(HostUpdateError::InvalidSettings)
        );
    }

    #[test]
    fn updater_job_response_is_strictly_normalized() -> Result<(), HostUpdateError> {
        let value = parse_job(
            r#"{"job_id":"0191439c-37c4-70a1-8d88-1a81f5c0f461","operation":"install","requested_version":"0.3.3","state":"queued","phase":"queued","installed_version":"0.3.2","available_version":"0.3.3","previous_version":"0.3.2","backup_path":null,"message":"Queued","created_at":"2026-08-10T04:00:00Z","updated_at":"2026-08-10T04:00:01Z"}"#,
        )?;
        assert_eq!(value.operation, HostUpdateOperation::Install);
        assert_eq!(value.created_at_ms, 1_786_334_400_000);
        assert_eq!(value.updated_at_ms, 1_786_334_401_000);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_transport_passes_only_fixed_argv_without_a_shell()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("ffdb-update-test");
        let arguments = directory.path().join("arguments");
        let job = r#"{"job_id":"0191439c-37c4-70a1-8d88-1a81f5c0f461","operation":"install","requested_version":"0.3.3","state":"queued","phase":"queued","installed_version":"0.3.2","available_version":"0.3.3","previous_version":"0.3.2","backup_path":null,"message":"Queued","created_at":"2026-08-10T04:00:00Z","updated_at":"2026-08-10T04:00:01Z"}"#;
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{}'\n",
                arguments.display(),
                job
            ),
        )?;
        let mut permissions = std::fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions)?;

        let updater = CommandHostUpdater::new(&executable)?;
        let result = updater.install("0.3.3").await?;
        assert_eq!(result.requested_version.as_deref(), Some("0.3.3"));
        assert_eq!(
            std::fs::read_to_string(arguments)?,
            "submit\ninstall\n0.3.3\n"
        );
        assert_eq!(
            updater.install("0.3.3; touch /tmp/pwned").await,
            Err(HostUpdateError::InvalidVersion)
        );
        Ok(())
    }

    #[tokio::test]
    async fn non_administrator_cannot_read_host_update_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = handler_fixture(now_ms(), false, true).await? else {
            return Ok(());
        };
        let response = status(
            State(fixture.state),
            Extension(RequestId::new()),
            fixture.headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(fixture.updater.calls().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn absent_native_updater_is_a_supported_status_response()
    -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = handler_fixture(now_ms(), true, false).await? else {
            return Ok(());
        };
        let response = status(
            State(fixture.state),
            Extension(RequestId::new()),
            fixture.headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await?;
        let value: HostUpdateStatus = serde_json::from_slice(&body)?;
        assert!(!value.supported);
        assert_eq!(
            value.unavailable_reason.as_deref(),
            Some("host updater is not installed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_password_authentication_rejects_install_before_updater_dispatch()
    -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) =
            handler_fixture(now_ms() - RECENT_AUTHENTICATION_MS - 1_000, true, true).await?
        else {
            return Ok(());
        };
        let response = install(
            State(fixture.state),
            Extension(RequestId::new()),
            fixture.headers,
            Json(HostUpdateVersionRequest {
                version: "0.3.3".to_owned(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);
        assert!(fixture.updater.calls().await.is_empty());
        let body = to_bytes(response.into_body(), 1024 * 1024).await?;
        assert!(String::from_utf8_lossy(&body).contains("platform_auth.reauthentication_required"));
        Ok(())
    }

    #[tokio::test]
    async fn fresh_administrator_enqueues_exact_install_operation_and_audits_acceptance()
    -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = handler_fixture(now_ms(), true, true).await? else {
            return Ok(());
        };
        let response = install(
            State(fixture.state),
            Extension(RequestId::new()),
            fixture.headers,
            Json(HostUpdateVersionRequest {
                version: "0.3.3".to_owned(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(fixture.updater.calls().await, vec!["install:0.3.3"]);
        let events = fixture.audit.events().await;
        assert!(events.iter().any(|event| {
            event.action == "instance.host_update.install.enqueue.requested"
                && event.outcome == AuditOutcome::Success
        }));
        assert!(events.iter().any(|event| {
            event.action == "instance.host_update.install.enqueue"
                && event.outcome == AuditOutcome::Success
        }));
        assert!(
            !events
                .iter()
                .any(|event| event.action == "instance.host_update.install")
        );
        Ok(())
    }
}
