use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use ffdb_audit::AuditOutcome;
use ffdb_database_router::{DatabaseExecutor, DatabaseRouter, ExecutionError, RoutingError};
use ffdb_object_storage::{
    AuthorizationRequest, AuthorizationToken, MetadataAuthorization, MetadataAuthorizer,
    S3Provider, SignedObjectRequest as ProviderSignedRequest, StorageAction, StorageCommitResult,
    StorageError, StorageGateway, StorageLimits, StorageMetadataCommit,
    StorageReceiptRequest as MetadataReceiptRequest, StorageReservationRequest,
};
use ffdb_protocol::{
    AuthContext, DeveloperPrincipal, DeveloperScope, ExecutionMode, PROTOCOL_VERSION, ProjectId,
    RequestId, ResourceLimits, SensitiveString, StorageAuthorization, StorageAuthorizeRequest,
    StorageCleanupAckRequest, StorageCleanupBatch, StorageCleanupClaimRequest,
    StorageCleanupDisposition, StorageCleanupOutcome, StorageCommitReceipt, StorageCommitRequest,
    StorageCreateBucketRequest, StorageListRequest, StorageListResponse, StorageReceiptRequest,
    StorageReleaseRequest, StorageReserveRequest, WorkerOperation, WorkerRequest, WorkerResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ApiError, ApiState, append_audit, append_audit_best_effort, audit_unavailable,
    credential_error, developer, end_user, enforce_execution_rate_limit, now_ms, parse_project,
};

const DEFAULT_MAX_OBJECT_BYTES: u64 = 50 * 1024 * 1024;
const DEFAULT_PROJECT_QUOTA_BYTES: u64 = 1_000_000_000;

#[derive(Clone)]
pub struct WorkerMetadataAuthorizer {
    router: Arc<dyn DatabaseRouter>,
    executor: Arc<dyn DatabaseExecutor>,
    limits: ResourceLimits,
    usage_metering: Option<Arc<crate::metering::UsageMeteringService>>,
}

impl std::fmt::Debug for WorkerMetadataAuthorizer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerMetadataAuthorizer")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl WorkerMetadataAuthorizer {
    #[must_use]
    pub fn new(
        router: Arc<dyn DatabaseRouter>,
        executor: Arc<dyn DatabaseExecutor>,
        limits: ResourceLimits,
    ) -> Self {
        Self {
            router,
            executor,
            limits,
            usage_metering: None,
        }
    }

    #[must_use]
    pub fn with_usage_metering(
        mut self,
        usage_metering: Arc<crate::metering::UsageMeteringService>,
    ) -> Self {
        self.usage_metering = Some(usage_metering);
        self
    }

    async fn execute_end_user(
        &self,
        auth: &AuthContext,
        operation: WorkerOperation,
    ) -> Result<WorkerResponse, StorageError> {
        self.execute(
            auth.project_id,
            ExecutionMode::EndUser(auth.clone()),
            operation,
        )
        .await
    }

    async fn execute(
        &self,
        project_id: ProjectId,
        mode: ExecutionMode,
        operation: WorkerOperation,
    ) -> Result<WorkerResponse, StorageError> {
        let route = self.router.resolve(project_id).await.map_err(map_routing)?;
        if route.project_id != project_id {
            return Err(StorageError::Internal);
        }
        let request = WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            route: route.clone(),
            mode,
            deadline_epoch_ms: now_ms().saturating_add(self.limits.transaction_timeout_ms as i64),
            limits: self.limits.clone(),
            expected_schema_version: None,
            operation_receipt_id: None,
            operation,
        };
        self.executor
            .execute(&route, request)
            .await
            .map(|execution| execution.response)
            .map_err(map_execution)
    }

    async fn list(
        &self,
        auth: &AuthContext,
        request: StorageListRequest,
    ) -> Result<StorageListResponse, StorageError> {
        match self
            .execute_end_user(auth, WorkerOperation::StorageList(request))
            .await?
        {
            WorkerResponse::StorageObjects(response) => Ok(response),
            _ => Err(StorageError::Internal),
        }
    }

    async fn current_bytes(&self, auth: &AuthContext) -> Result<u64, StorageError> {
        match self
            .execute_end_user(auth, WorkerOperation::StorageUsage)
            .await?
        {
            WorkerResponse::StorageUsage(usage) => Ok(usage.current_bytes),
            _ => Err(StorageError::Internal),
        }
    }

    async fn buckets(
        &self,
        project_id: ProjectId,
        principal: DeveloperPrincipal,
    ) -> Result<Vec<ffdb_protocol::StorageBucket>, StorageError> {
        match self
            .execute(
                project_id,
                ExecutionMode::Developer(principal),
                WorkerOperation::StorageBuckets,
            )
            .await?
        {
            WorkerResponse::StorageBuckets(response) => Ok(response),
            _ => Err(StorageError::Internal),
        }
    }

    async fn create_bucket(
        &self,
        project_id: ProjectId,
        principal: DeveloperPrincipal,
        request: StorageCreateBucketRequest,
    ) -> Result<ffdb_protocol::StorageBucket, StorageError> {
        match self
            .execute(
                project_id,
                ExecutionMode::Developer(principal),
                WorkerOperation::StorageCreateBucket(request),
            )
            .await?
        {
            WorkerResponse::StorageBucket(response) => Ok(response),
            _ => Err(StorageError::Internal),
        }
    }

    async fn cleanup_claim(
        &self,
        project_id: ProjectId,
        principal: DeveloperPrincipal,
        now_ms: i64,
    ) -> Result<StorageCleanupBatch, StorageError> {
        match self
            .execute(
                project_id,
                ExecutionMode::Developer(principal),
                WorkerOperation::StorageCleanupClaim(StorageCleanupClaimRequest {
                    now_ms,
                    limit: 100,
                }),
            )
            .await?
        {
            WorkerResponse::StorageCleanupBatch(batch) => Ok(batch),
            _ => Err(StorageError::Internal),
        }
    }

    async fn cleanup_ack(
        &self,
        project_id: ProjectId,
        principal: DeveloperPrincipal,
        now_ms: i64,
        items: Vec<StorageCleanupDisposition>,
    ) -> Result<(u64, u64), StorageError> {
        match self
            .execute(
                project_id,
                ExecutionMode::Developer(principal),
                WorkerOperation::StorageCleanupAck(StorageCleanupAckRequest { now_ms, items }),
            )
            .await?
        {
            WorkerResponse::StorageCleanupAck { removed, retried } => Ok((removed, retried)),
            _ => Err(StorageError::Internal),
        }
    }

    async fn settle_storage_metering(
        &self,
        auth: &AuthContext,
        nonce: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let Some(metering) = &self.usage_metering else {
            return Ok(());
        };
        let current_bytes = self.current_bytes(auth).await?;
        metering
            .settle_object_storage(auth.project_id, nonce, auth.subject, current_bytes, now_ms)
            .await
            .map(|_| ())
            .map_err(map_storage_metering)
    }
}

#[async_trait]
impl MetadataAuthorizer for WorkerMetadataAuthorizer {
    async fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<MetadataAuthorization, StorageError> {
        match self
            .execute_end_user(
                &request.auth,
                WorkerOperation::StorageAuthorize(StorageAuthorizeRequest {
                    bucket: request.bucket.clone(),
                    object_key: request.object_key.clone(),
                    action: to_protocol_action(request.action),
                    content_length: request.content_length,
                    checksum_sha256: request.checksum_sha256.clone(),
                    content_type: request.content_type.clone(),
                    upload_id: request.upload_id.clone(),
                    part_number: request.part_number,
                }),
            )
            .await?
        {
            WorkerResponse::StorageAuthorization(StorageAuthorization {
                provider_key,
                scope_fingerprint,
                project_quota_bytes,
                current_project_bytes,
                max_object_bytes,
                reservation_bytes,
                replacement_fingerprint,
            }) => Ok(MetadataAuthorization {
                provider_key,
                scope_fingerprint,
                project_quota_bytes,
                current_project_bytes,
                max_object_bytes,
                reservation_bytes,
                replacement_fingerprint,
            }),
            _ => Err(StorageError::Internal),
        }
    }

    async fn reserve(&self, request: &StorageReservationRequest) -> Result<(), StorageError> {
        if let Some(metering) = &self.usage_metering {
            metering
                .reserve_object_storage(
                    request.auth.project_id,
                    &request.nonce,
                    request.auth.subject,
                    request.bytes,
                    now_ms(),
                    request.expires_at_ms,
                )
                .await
                .map_err(map_storage_metering)?;
        }
        let result = match self
            .execute_end_user(
                &request.auth,
                WorkerOperation::StorageReserve(StorageReserveRequest {
                    nonce: request.nonce.clone(),
                    bytes: request.bytes,
                    expires_at_ms: request.expires_at_ms,
                    provider_key: SensitiveString::new(request.provider_key.clone()),
                    action: to_protocol_action(request.action),
                    upload_id: request.upload_id.clone(),
                }),
            )
            .await
        {
            Ok(response) => expect_ack(response),
            Err(error) => Err(error),
        };
        if result.is_err()
            && let Some(metering) = &self.usage_metering
        {
            let _ignored = metering
                .release_object_storage(request.auth.project_id, &request.nonce, now_ms())
                .await;
        }
        result
    }

    async fn commit(&self, request: &StorageMetadataCommit) -> Result<(), StorageError> {
        expect_ack(
            self.execute_end_user(
                &request.auth,
                WorkerOperation::StorageCommit(StorageCommitRequest {
                    bucket: request.bucket.clone(),
                    object_key: request.object_key.clone(),
                    provider_key: request.provider_key.clone(),
                    action: to_protocol_action(request.action),
                    content_length: request.content_length,
                    checksum_sha256: request.checksum_sha256.clone(),
                    content_type: request.content_type.clone(),
                    upload_id: request.upload_id.clone(),
                    part_number: request.part_number,
                    etag: request.etag.clone(),
                    version_id: request.version_id.clone(),
                    reservation_nonce: request.reservation_nonce.clone(),
                    reservation_bytes: request.reservation_bytes,
                    reservation_expires_at_ms: request.reservation_expires_at_ms,
                    replacement_fingerprint: request.replacement_fingerprint.clone(),
                }),
            )
            .await?,
        )?;
        self.settle_storage_metering(&request.auth, &request.reservation_nonce, now_ms())
            .await
    }

    async fn receipt(
        &self,
        request: &MetadataReceiptRequest,
    ) -> Result<Option<StorageCommitResult>, StorageError> {
        match self
            .execute_end_user(
                &request.auth,
                WorkerOperation::StorageReceipt(StorageReceiptRequest {
                    bucket: request.bucket.clone(),
                    object_key: request.object_key.clone(),
                    provider_key: request.provider_key.clone(),
                    action: to_protocol_action(request.action),
                    content_length: request.content_length,
                    checksum_sha256: request.checksum_sha256.clone(),
                    content_type: request.content_type.clone(),
                    upload_id: request.upload_id.clone(),
                    part_number: request.part_number,
                    reservation_nonce: request.reservation_nonce.clone(),
                    reservation_bytes: request.reservation_bytes,
                    reservation_expires_at_ms: request.reservation_expires_at_ms,
                    replacement_fingerprint: request.replacement_fingerprint.clone(),
                }),
            )
            .await?
        {
            WorkerResponse::StorageReceipt(receipt) => {
                if receipt.is_some() {
                    self.settle_storage_metering(
                        &request.auth,
                        &request.reservation_nonce,
                        now_ms(),
                    )
                    .await?;
                }
                Ok(receipt.map(
                    |StorageCommitReceipt {
                         content_length,
                         checksum_sha256,
                         etag,
                         version_id,
                     }| StorageCommitResult {
                        content_length,
                        checksum_sha256,
                        etag,
                        version_id,
                    },
                ))
            }
            _ => Err(StorageError::Internal),
        }
    }

    async fn release_reservation(
        &self,
        auth: &AuthContext,
        nonce: &str,
        reservation_bytes: u64,
        reservation_expires_at_ms: i64,
    ) -> Result<(), StorageError> {
        expect_ack(
            self.execute_end_user(
                auth,
                WorkerOperation::StorageRelease(StorageReleaseRequest {
                    nonce: nonce.to_owned(),
                    reservation_bytes,
                    reservation_expires_at_ms,
                }),
            )
            .await?,
        )?;
        if let Some(metering) = &self.usage_metering {
            metering
                .release_object_storage(auth.project_id, nonce, now_ms())
                .await
                .map_err(map_storage_metering)?;
        }
        Ok(())
    }

    async fn cleanup_expired_reservations(&self, _now_ms: i64) -> Result<usize, StorageError> {
        // Cleanup is a trusted developer maintenance operation, while this trait
        // method deliberately has no principal. API maintenance dispatches the
        // worker operation directly rather than manufacturing a principal.
        Err(StorageError::Internal)
    }
}

pub struct StorageService {
    gateway: StorageGateway<WorkerMetadataAuthorizer, S3Provider>,
    worker: WorkerMetadataAuthorizer,
    provider: S3Provider,
}

impl std::fmt::Debug for StorageService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageService")
            .finish_non_exhaustive()
    }
}

impl StorageService {
    pub fn new(
        worker: WorkerMetadataAuthorizer,
        provider: S3Provider,
        grant_secret: impl AsRef<[u8]>,
        limits: StorageLimits,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            gateway: StorageGateway::new(worker.clone(), provider.clone(), grant_secret, limits)?,
            worker,
            provider,
        })
    }

    async fn sign(
        &self,
        auth: AuthContext,
        request: &SignRequest,
        now: i64,
    ) -> Result<SignResponse, StorageError> {
        let action = match request.operation {
            ObjectOperation::Upload => StorageAction::Upload,
            ObjectOperation::Download => StorageAction::Download,
            ObjectOperation::Delete => StorageAction::Delete,
            ObjectOperation::CreateMultipart => {
                return Err(StorageError::InvalidAuthorizationDecision);
            }
            ObjectOperation::UploadPart => StorageAction::UploadPart,
            ObjectOperation::CompleteMultipart => StorageAction::CompleteMultipart,
            ObjectOperation::AbortMultipart => StorageAction::AbortMultipart,
        };
        let checksum = request
            .checksum_sha256
            .as_deref()
            .map(normalize_checksum)
            .transpose()?;
        let authorization = AuthorizationRequest {
            auth: auth.clone(),
            bucket: request.bucket.clone(),
            object_key: request.key.clone(),
            action,
            content_length: request.size_bytes,
            checksum_sha256: checksum,
            content_type: request.content_type.clone(),
            upload_id: request.upload_id.clone(),
            part_number: request.part_number,
        };
        let token = self.gateway.authorize(&authorization, now).await?;
        let signed = match self.gateway.presign(&token, 5 * 60 * 1_000, now).await {
            Ok(value) => value,
            Err(error) => {
                let _ignored = self.gateway.release_reservation(&token, &auth, now).await;
                return Err(error);
            }
        };
        Ok(SignResponse::new(
            signed,
            (!matches!(action, StorageAction::Download)).then(|| token.as_str().to_owned()),
        ))
    }

    async fn authorize_multipart_create(
        &self,
        auth: AuthContext,
        request: &MultipartAuthorizeRequest,
        now_ms: i64,
    ) -> Result<String, StorageError> {
        self.gateway
            .authorize(
                &AuthorizationRequest {
                    auth,
                    bucket: request.bucket.clone(),
                    object_key: request.key.clone(),
                    action: StorageAction::CreateMultipart,
                    content_length: Some(request.size_bytes),
                    checksum_sha256: request
                        .checksum_sha256
                        .as_deref()
                        .map(normalize_checksum)
                        .transpose()?,
                    content_type: request.content_type.clone(),
                    upload_id: None,
                    part_number: None,
                },
                now_ms,
            )
            .await
            .map(|token| token.as_str().to_owned())
    }

    async fn create_multipart(
        &self,
        auth: AuthContext,
        authorization_token: String,
        now_ms: i64,
    ) -> Result<String, StorageError> {
        let token = AuthorizationToken::parse(authorization_token)?;
        self.gateway
            .initiate_multipart_and_commit(&token, auth, now_ms)
            .await
    }

    async fn commit(
        &self,
        auth: AuthContext,
        authorization_token: String,
        now: i64,
    ) -> Result<(), StorageError> {
        let token = AuthorizationToken::parse(authorization_token)?;
        self.gateway
            .verify_provider_and_commit(&token, auth, now)
            .await
            .map(|_| ())
    }

    async fn release(
        &self,
        auth: &AuthContext,
        authorization_token: String,
        now: i64,
    ) -> Result<(), StorageError> {
        let token = AuthorizationToken::parse(authorization_token)?;
        self.gateway
            .release_reservation(&token, auth, now)
            .await
            .map(|_| ())
    }

    async fn current_bytes(&self, auth: &AuthContext) -> Result<u64, StorageError> {
        self.worker.current_bytes(auth).await
    }

    async fn cleanup(
        &self,
        project_id: ProjectId,
        principal: DeveloperPrincipal,
        now_ms: i64,
    ) -> Result<(u64, u64), StorageError> {
        let batch = self
            .worker
            .cleanup_claim(project_id, principal.clone(), now_ms)
            .await?;
        let mut dispositions = Vec::with_capacity(batch.items.len());
        for item in batch.items {
            let provider_result = match from_protocol_action(item.action) {
                StorageAction::Upload
                | StorageAction::Delete
                | StorageAction::CompleteMultipart => {
                    self.provider
                        .delete_internal(item.provider_key.expose(), now_ms)
                        .await
                }
                StorageAction::CreateMultipart
                | StorageAction::UploadPart
                | StorageAction::AbortMultipart => match item.upload_id.as_deref() {
                    Some(upload_id) => {
                        self.provider
                            .abort_multipart_internal(item.provider_key.expose(), upload_id, now_ms)
                            .await
                    }
                    None if matches!(
                        from_protocol_action(item.action),
                        StorageAction::CreateMultipart
                    ) =>
                    {
                        self.provider
                            .abort_multipart_for_key_internal(item.provider_key.expose(), now_ms)
                            .await
                    }
                    None => Err(StorageError::InvalidMultipartRequest),
                },
                StorageAction::Download | StorageAction::List => {
                    Err(StorageError::InvalidAuthorizationDecision)
                }
            };
            dispositions.push(StorageCleanupDisposition {
                id: item.id,
                lease_token: item.lease_token,
                outcome: if provider_result.is_ok() {
                    StorageCleanupOutcome::Deleted
                } else {
                    StorageCleanupOutcome::Retry
                },
            });
        }
        let (provider_removed, retried) = self
            .worker
            .cleanup_ack(project_id, principal, now_ms, dispositions)
            .await?;
        let local = self.gateway.cleanup_local_reservations(now_ms)?;
        Ok((
            batch
                .removed_reservations
                .saturating_add(provider_removed)
                .saturating_add(u64::try_from(local).unwrap_or(u64::MAX)),
            retried,
        ))
    }

    async fn commit_multipart(
        &self,
        auth: AuthContext,
        request: MultipartCommitRequest,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let token = AuthorizationToken::parse(request.authorization_token.into_inner())?;
        let action = match request.operation {
            MultipartCommitOperation::Create => {
                return Err(StorageError::InvalidAuthorizationDecision);
            }
            MultipartCommitOperation::UploadPart => StorageAction::UploadPart,
            MultipartCommitOperation::Complete => {
                return self
                    .gateway
                    .verify_provider_and_commit(&token, auth, now_ms)
                    .await
                    .map(|_| ());
            }
            MultipartCommitOperation::Abort => StorageAction::AbortMultipart,
        };
        self.gateway
            .commit_multipart_stage(
                &token,
                auth,
                action,
                ffdb_object_storage::StorageCommitResult {
                    content_length: None,
                    checksum_sha256: None,
                    etag: request.etag,
                    version_id: request.upload_id,
                },
                now_ms,
            )
            .await
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SignRequest {
    bucket: String,
    key: String,
    operation: ObjectOperation,
    content_type: Option<String>,
    size_bytes: Option<u64>,
    checksum_sha256: Option<String>,
    upload_id: Option<String>,
    part_number: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ObjectOperation {
    Upload,
    Download,
    Delete,
    CreateMultipart,
    UploadPart,
    CompleteMultipart,
    AbortMultipart,
}

#[derive(Debug, Serialize)]
struct SignResponse {
    url: SensitiveString,
    method: String,
    headers: Vec<(String, String)>,
    expires_at_ms: i64,
    authorization_token: Option<SensitiveString>,
}

impl SignResponse {
    fn new(request: ProviderSignedRequest, authorization_token: Option<String>) -> Self {
        Self {
            url: SensitiveString::new(request.url.to_string()),
            method: request.method,
            headers: request.required_headers,
            expires_at_ms: request.expires_at_ms,
            authorization_token: authorization_token.map(SensitiveString::new),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CommitRequest {
    authorization_token: SensitiveString,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MultipartCommitRequest {
    authorization_token: SensitiveString,
    operation: MultipartCommitOperation,
    upload_id: Option<String>,
    etag: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MultipartAuthorizeRequest {
    bucket: String,
    key: String,
    content_type: Option<String>,
    size_bytes: u64,
    checksum_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct MultipartAuthorizeResponse {
    authorization_token: SensitiveString,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MultipartCreateRequest {
    authorization_token: SensitiveString,
}

#[derive(Debug, Serialize)]
struct MultipartCreateResponse {
    upload_id: SensitiveString,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MultipartCommitOperation {
    Create,
    UploadPart,
    Complete,
    Abort,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    bucket: String,
    #[serde(default)]
    prefix: String,
    limit: Option<u32>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateBucketRequest {
    name: String,
    #[serde(default)]
    public: bool,
    max_object_bytes: Option<u64>,
    #[serde(default)]
    versioning: bool,
}

#[derive(Debug, Serialize)]
struct BucketResponse {
    id: String,
    name: String,
    public: bool,
    max_object_bytes: u64,
    project_quota_bytes: u64,
    versioning: bool,
    created_at_ms: i64,
}

impl From<ffdb_protocol::StorageBucket> for BucketResponse {
    fn from(value: ffdb_protocol::StorageBucket) -> Self {
        Self {
            id: value.id,
            name: value.name,
            public: value.public,
            max_object_bytes: value.max_object_bytes,
            project_quota_bytes: value.project_quota_bytes,
            versioning: false,
            created_at_ms: value.created_at_ms,
        }
    }
}

pub(crate) async fn sign(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<SignRequest>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) =
        begin_audited(&state, project_id, request_id, &mode, "storage.sign").await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => service.sign(auth, &payload, now_ms()).await,
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.sign",
        result,
        |signed| (StatusCode::OK, Json(signed)).into_response(),
    )
    .await
}

pub(crate) async fn commit(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CommitRequest>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) =
        begin_audited(&state, project_id, request_id, &mode, "storage.commit").await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => {
            match service
                .commit(
                    auth.clone(),
                    payload.authorization_token.into_inner(),
                    now_ms(),
                )
                .await
            {
                Ok(()) => service.current_bytes(&auth).await,
                Err(error) => Err(error),
            }
        }
        None => Err(StorageError::Internal),
    };
    let result = match result {
        Ok(current_bytes) => {
            if let Some(metering) = &state.usage_metering
                && let Err(error) = metering
                    .record_object_storage(
                        project_id,
                        request_id.0,
                        auth.subject,
                        current_bytes,
                        now_ms(),
                    )
                    .await
            {
                append_audit_best_effort(
                    &state,
                    project_id,
                    request_id,
                    &mode,
                    "storage.commit",
                    "object",
                    AuditOutcome::Failure,
                )
                .await;
                return super::metering_error(error, request_id).into_response();
            }
            Ok(())
        }
        Err(error) => Err(error),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.commit",
        result,
        |_| StatusCode::NO_CONTENT.into_response(),
    )
    .await
}

pub(crate) async fn release(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CommitRequest>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) =
        begin_audited(&state, project_id, request_id, &mode, "storage.release").await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => {
            service
                .release(&auth, payload.authorization_token.into_inner(), now_ms())
                .await
        }
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.release",
        result,
        |_| StatusCode::NO_CONTENT.into_response(),
    )
    .await
}

pub(crate) async fn multipart_commit(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<MultipartCommitRequest>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) = begin_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.multipart.commit",
    )
    .await
    {
        return response;
    }
    let changes_storage = matches!(
        payload.operation,
        MultipartCommitOperation::Complete | MultipartCommitOperation::Abort
    );
    let result = match &state.storage {
        Some(service) => match service
            .commit_multipart(auth.clone(), payload, now_ms())
            .await
        {
            Ok(()) if changes_storage => service.current_bytes(&auth).await.map(Some),
            Ok(()) => Ok(None),
            Err(error) => Err(error),
        },
        None => Err(StorageError::Internal),
    };
    let result = match result {
        Ok(Some(current_bytes)) => {
            if let Some(metering) = &state.usage_metering
                && let Err(error) = metering
                    .record_object_storage(
                        project_id,
                        request_id.0,
                        auth.subject,
                        current_bytes,
                        now_ms(),
                    )
                    .await
            {
                append_audit_best_effort(
                    &state,
                    project_id,
                    request_id,
                    &mode,
                    "storage.multipart.commit",
                    "object",
                    AuditOutcome::Failure,
                )
                .await;
                return super::metering_error(error, request_id).into_response();
            }
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.multipart.commit",
        result,
        |_| StatusCode::NO_CONTENT.into_response(),
    )
    .await
}

pub(crate) async fn multipart_authorize(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<MultipartAuthorizeRequest>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) = begin_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.multipart.authorize",
    )
    .await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => {
            service
                .authorize_multipart_create(auth, &payload, now_ms())
                .await
        }
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.multipart.authorize",
        result,
        |authorization_token| {
            (
                StatusCode::OK,
                Json(MultipartAuthorizeResponse {
                    authorization_token: SensitiveString::new(authorization_token),
                }),
            )
                .into_response()
        },
    )
    .await
}

pub(crate) async fn multipart_create(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<MultipartCreateRequest>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) = begin_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.multipart.create",
    )
    .await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => {
            service
                .create_multipart(auth, payload.authorization_token.into_inner(), now_ms())
                .await
        }
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.multipart.create",
        result,
        |upload_id| {
            (
                StatusCode::CREATED,
                Json(MultipartCreateResponse {
                    upload_id: SensitiveString::new(upload_id),
                }),
            )
                .into_response()
        },
    )
    .await
}

pub(crate) async fn objects(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(parameters): Query<ListQuery>,
) -> Response {
    let (project_id, auth) = match authenticated_user(&state, &project, request_id, &headers).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let limit = parameters.limit.unwrap_or(100);
    if limit == 0 || limit > 1_000 || parameters.prefix.len() > 1_024 {
        return storage_error(StorageError::InvalidObjectKey, request_id).into_response();
    }
    let mode = ExecutionMode::EndUser(auth.clone());
    if let Err(response) =
        begin_audited(&state, project_id, request_id, &mode, "storage.list").await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => {
            service
                .worker
                .list(
                    &auth,
                    StorageListRequest {
                        bucket: parameters.bucket,
                        prefix: parameters.prefix,
                        limit,
                        cursor: parameters.cursor,
                    },
                )
                .await
        }
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.list",
        result,
        |objects| (StatusCode::OK, Json(objects)).into_response(),
    )
    .await
}

pub(crate) async fn buckets(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (project_id, principal) = match authenticated_developer(
        &state,
        &project,
        request_id,
        &headers,
        DeveloperScope::StorageManage,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::Developer(principal.clone());
    if let Err(response) =
        begin_audited(&state, project_id, request_id, &mode, "storage.buckets").await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => service.worker.buckets(project_id, principal).await,
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.buckets",
        result,
        |buckets| {
            (
                StatusCode::OK,
                Json(
                    buckets
                        .into_iter()
                        .map(BucketResponse::from)
                        .collect::<Vec<_>>(),
                ),
            )
                .into_response()
        },
    )
    .await
}

pub(crate) async fn create_bucket(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateBucketRequest>,
) -> Response {
    let (project_id, principal) = match authenticated_developer(
        &state,
        &project,
        request_id,
        &headers,
        DeveloperScope::StorageManage,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    if payload.versioning {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "storage.versioning_not_configured",
            "bucket versioning is not configured for this provider",
            request_id,
        )
        .into_response();
    }
    let mode = ExecutionMode::Developer(principal.clone());
    if let Err(response) = begin_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.bucket.create",
    )
    .await
    {
        return response;
    }
    let created_at_ms = now_ms();
    let request = StorageCreateBucketRequest {
        id: Uuid::now_v7().to_string(),
        name: payload.name,
        owner_id: None,
        public: payload.public,
        max_object_bytes: payload.max_object_bytes.unwrap_or(DEFAULT_MAX_OBJECT_BYTES),
        project_quota_bytes: DEFAULT_PROJECT_QUOTA_BYTES,
        created_at_ms,
    };
    let result = match &state.storage {
        Some(service) => {
            service
                .worker
                .create_bucket(project_id, principal, request)
                .await
        }
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.bucket.create",
        result,
        |bucket| (StatusCode::CREATED, Json(BucketResponse::from(bucket))).into_response(),
    )
    .await
}

pub(crate) async fn cleanup(
    State(state): State<ApiState>,
    Path(project): Path<String>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let (project_id, principal) = match authenticated_developer(
        &state,
        &project,
        request_id,
        &headers,
        DeveloperScope::StorageManage,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mode = ExecutionMode::Developer(principal.clone());
    if let Err(response) =
        begin_audited(&state, project_id, request_id, &mode, "storage.cleanup").await
    {
        return response;
    }
    let result = match &state.storage {
        Some(service) => service.cleanup(project_id, principal, now_ms()).await,
        None => Err(StorageError::Internal),
    };
    finish_audited(
        &state,
        project_id,
        request_id,
        &mode,
        "storage.cleanup",
        result,
        |(removed, retried)| {
            (
                StatusCode::OK,
                Json(serde_json::json!({ "removed": removed, "retried": retried })),
            )
                .into_response()
        },
    )
    .await
}

async fn authenticated_user(
    state: &ApiState,
    project: &str,
    request_id: RequestId,
    headers: &HeaderMap,
) -> Result<(ProjectId, AuthContext), Response> {
    let project_id = parse_project(project, request_id).map_err(IntoResponse::into_response)?;
    let auth = end_user(state, project_id, headers)
        .await
        .map_err(|error| credential_error(error, request_id).into_response())?;
    if auth.project_id != project_id {
        return Err(
            credential_error(crate::CredentialError::WrongProject, request_id).into_response(),
        );
    }
    Ok((project_id, auth))
}

async fn authenticated_developer(
    state: &ApiState,
    project: &str,
    request_id: RequestId,
    headers: &HeaderMap,
    scope: DeveloperScope,
) -> Result<(ProjectId, DeveloperPrincipal), Response> {
    let project_id = parse_project(project, request_id).map_err(IntoResponse::into_response)?;
    let principal = developer(state, project_id, headers, scope)
        .await
        .map_err(|error| credential_error(error, request_id).into_response())?;
    Ok((project_id, principal))
}

async fn begin_audited(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    action: &str,
) -> Result<(), Response> {
    enforce_execution_rate_limit(state, project_id, request_id, mode, 1).await?;
    append_audit(
        state,
        project_id,
        request_id,
        mode,
        &format!("{action}.requested"),
        "storage",
        AuditOutcome::Success,
    )
    .await
    .map_err(|()| audit_unavailable(request_id).into_response())
}

async fn finish_audited<T, F>(
    state: &ApiState,
    project_id: ProjectId,
    request_id: RequestId,
    mode: &ExecutionMode,
    action: &str,
    result: Result<T, StorageError>,
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
                "storage",
                AuditOutcome::Success,
            )
            .await;
            success(value)
        }
        Err(error) => {
            let outcome = if matches!(error, StorageError::RlsDenied) {
                AuditOutcome::Denied
            } else {
                AuditOutcome::Failure
            };
            append_audit_best_effort(
                state, project_id, request_id, mode, action, "storage", outcome,
            )
            .await;
            storage_error(error, request_id).into_response()
        }
    }
}

fn expect_ack(response: WorkerResponse) -> Result<(), StorageError> {
    if matches!(response, WorkerResponse::StorageAck) {
        Ok(())
    } else {
        Err(StorageError::Internal)
    }
}

fn normalize_checksum(value: &str) -> Result<String, StorageError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let bytes = value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).map_err(|_| StorageError::InvalidObjectKey)?;
                u8::from_str_radix(text, 16).map_err(|_| StorageError::InvalidObjectKey)
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(STANDARD.encode(bytes));
    }
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| StorageError::InvalidObjectKey)?;
    if decoded.len() != 32 {
        return Err(StorageError::InvalidObjectKey);
    }
    Ok(value.to_owned())
}

fn to_protocol_action(action: StorageAction) -> ffdb_protocol::StorageAction {
    match action {
        StorageAction::Upload => ffdb_protocol::StorageAction::Upload,
        StorageAction::Download => ffdb_protocol::StorageAction::Download,
        StorageAction::Delete => ffdb_protocol::StorageAction::Delete,
        StorageAction::List => ffdb_protocol::StorageAction::List,
        StorageAction::CreateMultipart => ffdb_protocol::StorageAction::CreateMultipart,
        StorageAction::UploadPart => ffdb_protocol::StorageAction::UploadPart,
        StorageAction::CompleteMultipart => ffdb_protocol::StorageAction::CompleteMultipart,
        StorageAction::AbortMultipart => ffdb_protocol::StorageAction::AbortMultipart,
    }
}

fn from_protocol_action(action: ffdb_protocol::StorageAction) -> StorageAction {
    match action {
        ffdb_protocol::StorageAction::Upload => StorageAction::Upload,
        ffdb_protocol::StorageAction::Download => StorageAction::Download,
        ffdb_protocol::StorageAction::Delete => StorageAction::Delete,
        ffdb_protocol::StorageAction::List => StorageAction::List,
        ffdb_protocol::StorageAction::CreateMultipart => StorageAction::CreateMultipart,
        ffdb_protocol::StorageAction::UploadPart => StorageAction::UploadPart,
        ffdb_protocol::StorageAction::CompleteMultipart => StorageAction::CompleteMultipart,
        ffdb_protocol::StorageAction::AbortMultipart => StorageAction::AbortMultipart,
    }
}

fn map_routing(error: RoutingError) -> StorageError {
    match error {
        RoutingError::NotFound
        | RoutingError::Unavailable
        | RoutingError::StaleGeneration
        | RoutingError::Inconsistent => StorageError::Internal,
    }
}

fn map_execution(error: ExecutionError) -> StorageError {
    match error {
        ExecutionError::Rejected { code } if code == "storage.rls_denied" => {
            StorageError::RlsDenied
        }
        ExecutionError::Rejected { code } if code == "storage.object_quota" => {
            StorageError::ObjectQuotaExceeded
        }
        ExecutionError::Rejected { code } if code == "storage.project_quota" => {
            StorageError::ProjectQuotaExceeded
        }
        ExecutionError::Rejected { code } if code == "storage.duplicate_reservation" => {
            StorageError::DuplicateReservation
        }
        ExecutionError::Rejected { code } if code == "storage.invalid_request" => {
            StorageError::InvalidObjectKey
        }
        ExecutionError::QueueFull => StorageError::TooManyReservations,
        ExecutionError::DeadlineExceeded
        | ExecutionError::StaleGeneration
        | ExecutionError::Protocol
        | ExecutionError::Rejected { .. }
        | ExecutionError::Unavailable => StorageError::Internal,
    }
}

fn storage_error(error: StorageError, request_id: RequestId) -> ApiError {
    let (status, code, message) = match error {
        StorageError::RlsDenied => (
            StatusCode::FORBIDDEN,
            "storage.rls_denied",
            "storage policy denied the operation",
        ),
        StorageError::InvalidObjectKey
        | StorageError::InvalidProviderKey
        | StorageError::InvalidMultipartRequest
        | StorageError::InvalidGrant
        | StorageError::InvalidTtl
        | StorageError::InvalidAuthorizationDecision => (
            StatusCode::BAD_REQUEST,
            "storage.invalid_request",
            "storage request is invalid",
        ),
        StorageError::ExpiredGrant => (
            StatusCode::UNAUTHORIZED,
            "storage.authorization_expired",
            "storage authorization expired",
        ),
        StorageError::ObjectQuotaExceeded => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "storage.object_quota",
            "object exceeds its configured size limit",
        ),
        StorageError::ProjectQuotaExceeded => (
            StatusCode::CONFLICT,
            "storage.project_quota",
            "project storage quota would be exceeded",
        ),
        StorageError::OrganizationQuotaExceeded => (
            StatusCode::PAYMENT_REQUIRED,
            "storage.organization_quota",
            "organization storage allowance would be exceeded",
        ),
        StorageError::DuplicateReservation => (
            StatusCode::CONFLICT,
            "storage.duplicate_reservation",
            "storage reservation already exists",
        ),
        StorageError::TooManyReservations => (
            StatusCode::TOO_MANY_REQUESTS,
            "storage.too_many_pending",
            "too many storage operations are pending",
        ),
        StorageError::Provider => (
            StatusCode::BAD_GATEWAY,
            "storage.provider_failed",
            "object provider operation failed",
        ),
        StorageError::ProviderMetadataMismatch => (
            StatusCode::CONFLICT,
            "storage.provider_metadata_mismatch",
            "provider object does not match the authorized upload",
        ),
        StorageError::InvalidConfiguration
        | StorageError::UnsafeProviderEndpoint
        | StorageError::Internal => (
            StatusCode::SERVICE_UNAVAILABLE,
            "storage.unavailable",
            "storage service is temporarily unavailable",
        ),
    };
    ApiError::new(status, code, message, request_id)
}

fn map_storage_metering(error: crate::metering::MeteringError) -> StorageError {
    match error {
        crate::metering::MeteringError::Store(ffdb_org_metrics::MetricsError::LimitExceeded(
            "storage_bytes",
        )) => StorageError::OrganizationQuotaExceeded,
        crate::metering::MeteringError::InvalidOperation
        | crate::metering::MeteringError::ReportingBlocked
        | crate::metering::MeteringError::Store(_)
        | crate::metering::MeteringError::Unavailable => StorageError::Internal,
    }
}
