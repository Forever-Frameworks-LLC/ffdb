use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ffdb_object_storage::{
    AuthorizationRequest, MetadataAuthorization, MetadataAuthorizer, StorageAction,
    StorageCommitResult, StorageError, StorageMetadataCommit, StorageReceiptRequest,
    StorageReservationRequest,
};
use ffdb_protocol as protocol;
use ffdb_sqlite_runtime as runtime;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;
const MAX_LIST_CURSOR_BYTES: usize = 4_096;
const COMMIT_RECEIPT_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
const CLEANUP_LEASE_MS: i64 = 5 * 60 * 1_000;

#[derive(Clone)]
pub(crate) struct StorageCursorCodec {
    secret: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for StorageCursorCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageCursorCodec")
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct SqliteMetadataAuthorizer {
    database: Arc<runtime::Database>,
    project_id: protocol::ProjectId,
    cursor_codec: StorageCursorCodec,
}

#[derive(Debug)]
struct BucketMetadata {
    id: String,
    max_object_bytes: u64,
    project_quota_bytes: u64,
}

#[derive(Debug)]
struct ObjectMetadata {
    id: String,
    size_bytes: u64,
}

#[derive(Debug)]
struct UploadMetadata {
    id: String,
    owner_id: String,
    expected_size_bytes: Option<u64>,
    checksum_sha256: Option<String>,
    content_type: Option<String>,
    status: String,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ListCursor {
    project_id: String,
    subject: String,
    token_id: String,
    scope_fingerprint: String,
    bucket: String,
    prefix: String,
    object_key: String,
    id: String,
}

impl StorageCursorCodec {
    pub(crate) fn new(secret: impl AsRef<[u8]>) -> Result<Self, StorageError> {
        if secret.as_ref().len() < 32 {
            return Err(StorageError::Internal);
        }
        Ok(Self {
            secret: Zeroizing::new(secret.as_ref().to_vec()),
        })
    }

    fn encode(&self, cursor: &ListCursor) -> Result<String, StorageError> {
        let payload = serde_json::to_vec(cursor).map_err(|_| StorageError::Internal)?;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(self.secret.as_slice())
            .map_err(|_| StorageError::Internal)?;
        mac.update(&payload);
        let encoded = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        if encoded.len() > MAX_LIST_CURSOR_BYTES {
            return Err(StorageError::Internal);
        }
        Ok(encoded)
    }

    fn decode(&self, cursor: &str) -> Result<ListCursor, StorageError> {
        if cursor.is_empty() || cursor.len() > MAX_LIST_CURSOR_BYTES {
            return Err(StorageError::InvalidGrant);
        }
        let (payload, signature) = cursor
            .split_once('.')
            .filter(|(_, signature)| !signature.contains('.'))
            .ok_or(StorageError::InvalidGrant)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| StorageError::InvalidGrant)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| StorageError::InvalidGrant)?;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(self.secret.as_slice())
            .map_err(|_| StorageError::Internal)?;
        mac.update(&payload);
        mac.verify_slice(&signature)
            .map_err(|_| StorageError::InvalidGrant)?;
        serde_json::from_slice(&payload).map_err(|_| StorageError::InvalidGrant)
    }

    fn replacement_fingerprint(
        &self,
        project_id: protocol::ProjectId,
        bucket_id: &str,
        object: &ObjectMetadata,
        provider_key: &str,
    ) -> Result<String, runtime::RuntimeError> {
        let payload = serde_json::to_vec(&json!({
            "domain": "ffdb.storage.replacement.v1",
            "project_id": project_id,
            "bucket_id": bucket_id,
            "object_id": object.id,
            "size_bytes": object.size_bytes,
            "provider_key": provider_key,
        }))
        .map_err(|_| runtime::RuntimeError::Database)?;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(self.secret.as_slice())
            .map_err(|_| runtime::RuntimeError::Database)?;
        mac.update(&payload);
        Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

impl SqliteMetadataAuthorizer {
    pub(crate) fn new(
        database: Arc<runtime::Database>,
        project_id: protocol::ProjectId,
        cursor_codec: StorageCursorCodec,
    ) -> Self {
        Self {
            database,
            project_id,
            cursor_codec,
        }
    }

    fn check_project(&self, auth: &protocol::AuthContext) -> Result<(), StorageError> {
        if auth.project_id != self.project_id {
            tracing::warn!("storage auth context did not match the worker project");
            return Err(StorageError::RlsDenied);
        }
        Ok(())
    }

    fn with_user<T>(
        &self,
        auth: &protocol::AuthContext,
        callback: impl FnOnce(&mut runtime::Session<'_>) -> Result<T, runtime::RuntimeError>,
    ) -> Result<T, StorageError> {
        self.check_project(auth)?;
        self.database
            .with_context(
                runtime::ExecutionMode::EndUser(runtime::AuthContext {
                    project_id: auth.project_id.to_string(),
                    subject: auth.subject.to_string(),
                    role: auth.role.clone(),
                    claims: auth.claims.clone(),
                    token_id: auth.token_id.to_string(),
                }),
                &runtime::CancellationToken::default(),
                callback,
            )
            .map_err(map_runtime)
    }

    fn with_maintenance<T>(
        &self,
        callback: impl FnOnce(&mut runtime::Session<'_>) -> Result<T, runtime::RuntimeError>,
    ) -> Result<T, StorageError> {
        self.database
            .with_context(
                runtime::ExecutionMode::Developer(runtime::DeveloperPrincipal {
                    actor_id: "storage-maintenance".to_owned(),
                    api_key_id: "storage-maintenance".to_owned(),
                }),
                &runtime::CancellationToken::default(),
                callback,
            )
            .map_err(map_runtime)
    }

    pub(crate) fn list_sync(
        &self,
        auth: &protocol::AuthContext,
        request: &protocol::StorageListRequest,
    ) -> Result<protocol::StorageListResponse, StorageError> {
        if request.limit == 0
            || request.limit > 200
            || request.prefix.len() > 1_024
            || request.prefix.contains('\0')
        {
            return Err(StorageError::InvalidObjectKey);
        }
        let scope = scope_fingerprint(auth).map_err(map_runtime)?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| self.cursor_codec.decode(cursor))
            .transpose()?;
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.project_id != auth.project_id.to_string()
                || cursor.subject != auth.subject.to_string()
                || cursor.token_id != auth.token_id.to_string()
                || cursor.scope_fingerprint != scope
                || cursor.bucket != request.bucket
                || cursor.prefix != request.prefix
        }) {
            return Err(StorageError::InvalidGrant);
        }
        self.with_user(auth, |session| {
            let cursor_key = cursor
                .as_ref()
                .map_or("", |cursor| cursor.object_key.as_str());
            let cursor_id = cursor.as_ref().map_or("", |cursor| cursor.id.as_str());
            let limit = i64::from(request.limit) + 1;
            let result = session.execute(&runtime::StatementRequest {
                sql: "SELECT o.id,o.object_key,o.owner_id,o.size_bytes,o.content_type,\
                      o.checksum_sha256,o.etag,o.version_id,o.created_at_ms,o.updated_at_ms \
                      FROM storage_objects AS o JOIN storage_buckets AS b ON b.id=o.bucket_id \
                      WHERE b.name=?1 AND substr(o.object_key,1,length(?2))=?2 \
                      AND (o.object_key>?3 OR (o.object_key=?3 AND o.id>?4)) \
                      ORDER BY o.object_key,o.id LIMIT ?5"
                    .to_owned(),
                parameters: vec![
                    runtime::SqlParameter::Text(request.bucket.clone()),
                    runtime::SqlParameter::Text(request.prefix.clone()),
                    runtime::SqlParameter::Text(cursor_key.to_owned()),
                    runtime::SqlParameter::Text(cursor_id.to_owned()),
                    runtime::SqlParameter::Integer(limit),
                ],
            })?;
            let mut items = result
                .rows
                .iter()
                .map(|row| storage_item(row))
                .collect::<Result<Vec<_>, _>>()?;
            let has_more = items.len() > usize::try_from(request.limit).unwrap_or(usize::MAX);
            if has_more {
                items.truncate(usize::try_from(request.limit).unwrap_or(usize::MAX));
            }
            let next_cursor = if has_more {
                items
                    .last()
                    .map(|item| {
                        self.cursor_codec.encode(&ListCursor {
                            project_id: auth.project_id.to_string(),
                            subject: auth.subject.to_string(),
                            token_id: auth.token_id.to_string(),
                            scope_fingerprint: scope.clone(),
                            bucket: request.bucket.clone(),
                            prefix: request.prefix.clone(),
                            object_key: item.object_key.clone(),
                            id: item.id.clone(),
                        })
                    })
                    .transpose()
                    .map_err(|_| runtime::RuntimeError::Database)?
            } else {
                None
            };
            Ok(protocol::StorageListResponse { items, next_cursor })
        })
    }
}

impl SqliteMetadataAuthorizer {
    pub(crate) fn authorize_sync(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<MetadataAuthorization, StorageError> {
        let now_ms = epoch_ms()?;
        self.with_user(&request.auth, |session| {
            let bucket = load_bucket(session, &request.bucket)?;
            let object = load_object(session, &bucket.id, &request.object_key)?;
            let mut reservation_bytes = 0_u64;
            let mut replacement_fingerprint = None;
            let provider_key = match request.action {
                StorageAction::Upload => {
                    let size = request
                        .content_length
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    if size > bucket.max_object_bytes {
                        return Err(runtime::RuntimeError::StorageQuotaExceeded);
                    }
                    if let Some(object) = &object {
                        probe_object_update(session, object, request, now_ms)?;
                        let old_provider_key = session
                            .storage_provider_key(&object.id)?
                            .ok_or(runtime::RuntimeError::Database)?;
                        replacement_fingerprint = Some(self.cursor_codec.replacement_fingerprint(
                            self.project_id,
                            &bucket.id,
                            object,
                            &old_provider_key,
                        )?);
                        reservation_bytes = size.saturating_sub(object.size_bytes);
                    } else {
                        let object_id = Uuid::now_v7().to_string();
                        probe_object_insert(session, &object_id, &bucket.id, request, now_ms)?;
                        reservation_bytes = size;
                    }
                    provider_object_key(self.project_id, &bucket.id, &Uuid::now_v7().to_string())
                }
                StorageAction::Download => {
                    let object = object.ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    session
                        .storage_provider_key(&object.id)?
                        .ok_or(runtime::RuntimeError::Database)?
                }
                StorageAction::Delete => {
                    let object = object.ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    session.storage_probe_object_delete(&object.id)?;
                    session
                        .storage_provider_key(&object.id)?
                        .ok_or(runtime::RuntimeError::Database)?
                }
                StorageAction::CreateMultipart => {
                    let size = request
                        .content_length
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    if size > bucket.max_object_bytes {
                        return Err(runtime::RuntimeError::StorageQuotaExceeded);
                    }
                    if let Some(object) = &object {
                        probe_object_update(session, object, request, now_ms)?;
                        let old_provider_key = session
                            .storage_provider_key(&object.id)?
                            .ok_or(runtime::RuntimeError::Database)?;
                        replacement_fingerprint = Some(self.cursor_codec.replacement_fingerprint(
                            self.project_id,
                            &bucket.id,
                            object,
                            &old_provider_key,
                        )?);
                    } else {
                        let object_id = Uuid::now_v7().to_string();
                        probe_object_insert(session, &object_id, &bucket.id, request, now_ms)?;
                    }
                    let metadata_id = Uuid::now_v7().to_string();
                    probe_upload_insert(session, &metadata_id, &bucket.id, request, now_ms)?;
                    provider_object_key(self.project_id, &bucket.id, &Uuid::now_v7().to_string())
                }
                StorageAction::UploadPart => {
                    let upload_id = request
                        .upload_id
                        .as_deref()
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    let upload = load_upload(session, upload_id, &bucket.id, &request.object_key)?
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    if upload.owner_id != request.auth.subject.to_string()
                        || upload.status != "active"
                        || request.part_number.is_none()
                        || request.content_length.is_none()
                        || request
                            .content_length
                            .is_some_and(|size| size > bucket.max_object_bytes)
                    {
                        return Err(runtime::RuntimeError::StatementNotAllowed);
                    }
                    probe_upload_update(session, &upload.id, "active")?;
                    session
                        .storage_upload_provider_binding(&upload.id)?
                        .ok_or(runtime::RuntimeError::Database)?
                        .provider_key
                }
                StorageAction::CompleteMultipart => {
                    let upload_id = request
                        .upload_id
                        .as_deref()
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    let upload = load_upload(session, upload_id, &bucket.id, &request.object_key)?
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    if upload.owner_id != request.auth.subject.to_string()
                        || upload.status != "active"
                        || upload.expected_size_bytes != request.content_length
                        || request.content_length.is_none()
                        || request
                            .content_length
                            .is_some_and(|size| size > bucket.max_object_bytes)
                        || upload.checksum_sha256 != request.checksum_sha256
                        || upload.content_type != request.content_type
                    {
                        return Err(runtime::RuntimeError::StatementNotAllowed);
                    }
                    probe_upload_update(session, &upload.id, "active")?;
                    let binding = session
                        .storage_upload_provider_binding(&upload.id)?
                        .ok_or(runtime::RuntimeError::Database)?;
                    verify_replacement_state(
                        session,
                        &self.cursor_codec,
                        self.project_id,
                        &bucket,
                        object.as_ref(),
                        binding.replacement_fingerprint.as_deref(),
                    )?;
                    reservation_bytes = request
                        .content_length
                        .unwrap_or(0)
                        .saturating_sub(object.as_ref().map_or(0, |value| value.size_bytes));
                    replacement_fingerprint = binding.replacement_fingerprint;
                    binding.provider_key
                }
                StorageAction::AbortMultipart => {
                    let upload_id = request
                        .upload_id
                        .as_deref()
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    let upload = load_upload(session, upload_id, &bucket.id, &request.object_key)?
                        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                    if upload.owner_id != request.auth.subject.to_string()
                        || upload.status != "active"
                    {
                        return Err(runtime::RuntimeError::StatementNotAllowed);
                    }
                    session.probe_write(&runtime::StatementRequest {
                        sql: "DELETE FROM storage_uploads WHERE id=?1".to_owned(),
                        parameters: vec![runtime::SqlParameter::Text(upload.id.clone())],
                    })?;
                    session
                        .storage_upload_provider_binding(&upload.id)?
                        .ok_or(runtime::RuntimeError::Database)?
                        .provider_key
                }
                StorageAction::List => {
                    // Logical keys are unrelated to opaque provider keys. Issuing a broad
                    // provider list grant would bypass per-object RLS; listing stays mediated
                    // by SQLite metadata rather than presigned at this boundary.
                    return Err(runtime::RuntimeError::StatementNotAllowed);
                }
            };
            let usage = session.storage_usage(now_ms)?;
            Ok(MetadataAuthorization {
                provider_key,
                scope_fingerprint: scope_fingerprint(&request.auth)?,
                project_quota_bytes: bucket.project_quota_bytes,
                // The gateway adds its own process-local pending counter. Durable reservations
                // are enforced authoritatively and atomically by `storage_reserve`; including
                // them here would double-count this gateway's reservations during admission.
                current_project_bytes: usage.current_bytes,
                max_object_bytes: bucket.max_object_bytes,
                reservation_bytes,
                replacement_fingerprint,
            })
        })
    }

    pub(crate) fn reserve_sync(
        &self,
        request: &StorageReservationRequest,
    ) -> Result<(), StorageError> {
        let now_ms = epoch_ms()?;
        self.with_user(&request.auth, |session| {
            session.atomic(|session| {
                session.storage_reserve(
                    &request.auth.project_id.to_string(),
                    &request.auth.subject.to_string(),
                    &request.auth.token_id.to_string(),
                    &request.nonce,
                    request.bytes,
                    request.expires_at_ms,
                    now_ms,
                    &request.provider_key,
                    storage_action_name(request.action),
                    request.upload_id.as_deref(),
                )
            })
        })
    }

    pub(crate) fn commit_sync(&self, request: &StorageMetadataCommit) -> Result<(), StorageError> {
        let now_ms = epoch_ms()?;
        self.with_user(&request.auth, |session| {
            session.atomic(|session| {
                let binding_digest = commit_binding_digest(request)?;
                let result = commit_result(request);
                let commit_digest = commit_result_digest(&binding_digest, &result)?;
                if let Some(receipt) = session.storage_commit_receipt(
                    &request.auth.project_id.to_string(),
                    &request.reservation_nonce,
                    &request.auth.subject.to_string(),
                    &request.auth.token_id.to_string(),
                    &binding_digest,
                    now_ms,
                )? {
                    return if receipt.commit_digest == commit_digest {
                        Ok(())
                    } else {
                        Err(runtime::RuntimeError::StorageReservationMismatch)
                    };
                }
                if requires_durable_reservation(request.action) {
                    session.storage_consume_reservation(
                        &request.auth.project_id.to_string(),
                        &request.auth.subject.to_string(),
                        &request.auth.token_id.to_string(),
                        &request.reservation_nonce,
                        request.reservation_bytes,
                        request.reservation_expires_at_ms,
                        now_ms,
                        &request.provider_key,
                        storage_action_name(request.action),
                        request.upload_id.as_deref(),
                    )?;
                }
                let bucket = load_bucket(session, &request.bucket)?;
                if matches!(
                    request.action,
                    StorageAction::Upload
                        | StorageAction::CreateMultipart
                        | StorageAction::CompleteMultipart
                ) && request
                    .content_length
                    .is_none_or(|size| size > bucket.max_object_bytes)
                {
                    return Err(runtime::RuntimeError::StorageQuotaExceeded);
                }
                match request.action {
                    StorageAction::Upload => {
                        commit_object_upload(
                            session,
                            &self.cursor_codec,
                            self.project_id,
                            &bucket,
                            request,
                            now_ms,
                        )?;
                    }
                    StorageAction::Delete => {
                        let object = load_object(session, &bucket.id, &request.object_key)?
                            .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
                        let expected = session
                            .storage_provider_key(&object.id)?
                            .ok_or(runtime::RuntimeError::Database)?;
                        if expected != request.provider_key {
                            return Err(runtime::RuntimeError::StatementNotAllowed);
                        }
                        session.storage_delete_object(&object.id)?;
                        session.storage_remove_provider_key(&object.id)?;
                    }
                    StorageAction::CreateMultipart => {
                        commit_create_multipart(
                            session,
                            &self.cursor_codec,
                            self.project_id,
                            &bucket,
                            request,
                            now_ms,
                        )?;
                    }
                    StorageAction::UploadPart => {
                        let upload = load_committed_upload(session, &bucket, request)?;
                        verify_upload_provider(session, &upload, &request.provider_key)?;
                        session.storage_store_upload_part(
                            &upload.id,
                            request
                                .part_number
                                .ok_or(runtime::RuntimeError::StatementNotAllowed)?,
                            request.content_length,
                            request.checksum_sha256.as_deref(),
                            request.etag.as_deref(),
                        )?;
                        let _ = session.execute(&runtime::StatementRequest {
                            sql: "UPDATE storage_uploads SET status='active' WHERE id=?1"
                                .to_owned(),
                            parameters: vec![runtime::SqlParameter::Text(upload.id)],
                        })?;
                    }
                    StorageAction::CompleteMultipart => {
                        let upload = load_committed_upload(session, &bucket, request)?;
                        let binding =
                            verify_upload_provider(session, &upload, &request.provider_key)?;
                        if binding.replacement_fingerprint != request.replacement_fingerprint
                            || upload.status != "active"
                            || upload.expected_size_bytes != request.content_length
                            || upload.checksum_sha256 != request.checksum_sha256
                            || upload.content_type != request.content_type
                        {
                            return Err(runtime::RuntimeError::StorageReservationMismatch);
                        }
                        commit_object_upload(
                            session,
                            &self.cursor_codec,
                            self.project_id,
                            &bucket,
                            request,
                            now_ms,
                        )?;
                        let _ = session.execute(&runtime::StatementRequest {
                            sql: "DELETE FROM storage_uploads WHERE id=?1".to_owned(),
                            parameters: vec![runtime::SqlParameter::Text(upload.id.clone())],
                        })?;
                        session.storage_remove_upload_parts(&upload.id)?;
                        session.storage_remove_upload_provider_key(&upload.id)?;
                    }
                    StorageAction::AbortMultipart => {
                        let upload = load_committed_upload(session, &bucket, request)?;
                        verify_upload_provider(session, &upload, &request.provider_key)?;
                        let _ = session.execute(&runtime::StatementRequest {
                            sql: "DELETE FROM storage_uploads WHERE id=?1".to_owned(),
                            parameters: vec![runtime::SqlParameter::Text(upload.id.clone())],
                        })?;
                        session.storage_remove_upload_parts(&upload.id)?;
                        session.storage_remove_upload_provider_key(&upload.id)?;
                    }
                    StorageAction::Download => {}
                    StorageAction::List => {
                        return Err(runtime::RuntimeError::StatementNotAllowed);
                    }
                }
                let result_json = serde_json::to_string(&protocol::StorageCommitReceipt {
                    content_length: result.content_length,
                    checksum_sha256: result.checksum_sha256.clone(),
                    etag: result.etag.clone(),
                    version_id: result.version_id.clone(),
                })
                .map_err(|_| runtime::RuntimeError::Database)?;
                session.storage_record_commit_receipt(
                    &request.auth.project_id.to_string(),
                    &request.reservation_nonce,
                    &request.auth.subject.to_string(),
                    &request.auth.token_id.to_string(),
                    &binding_digest,
                    &commit_digest,
                    &result_json,
                    now_ms,
                    now_ms.saturating_add(COMMIT_RECEIPT_TTL_MS),
                )?;
                Ok(())
            })
        })
    }

    pub(crate) fn receipt_sync(
        &self,
        request: &StorageReceiptRequest,
    ) -> Result<Option<StorageCommitResult>, StorageError> {
        let now_ms = epoch_ms()?;
        self.with_user(&request.auth, |session| {
            let binding_digest = receipt_binding_digest(request)?;
            session
                .storage_commit_receipt(
                    &request.auth.project_id.to_string(),
                    &request.reservation_nonce,
                    &request.auth.subject.to_string(),
                    &request.auth.token_id.to_string(),
                    &binding_digest,
                    now_ms,
                )?
                .map(|receipt| {
                    serde_json::from_str::<protocol::StorageCommitReceipt>(&receipt.result_json)
                        .map(|receipt| StorageCommitResult {
                            content_length: receipt.content_length,
                            checksum_sha256: receipt.checksum_sha256,
                            etag: receipt.etag,
                            version_id: receipt.version_id,
                        })
                        .map_err(|_| runtime::RuntimeError::Database)
                })
                .transpose()
        })
    }

    pub(crate) fn release_reservation_sync(
        &self,
        auth: &protocol::AuthContext,
        nonce: &str,
        reservation_bytes: u64,
        reservation_expires_at_ms: i64,
    ) -> Result<(), StorageError> {
        let now_ms = epoch_ms()?;
        self.with_user(auth, |session| {
            session.atomic(|session| {
                session.storage_release_reservation_bound(
                    &auth.project_id.to_string(),
                    &auth.subject.to_string(),
                    &auth.token_id.to_string(),
                    nonce,
                    reservation_bytes,
                    reservation_expires_at_ms,
                    now_ms,
                )
            })
        })
    }

    pub(crate) fn cleanup_expired_reservations_sync(
        &self,
        now_ms: i64,
    ) -> Result<usize, StorageError> {
        self.with_maintenance(|session| {
            session.atomic(|session| session.storage_cleanup_expired_reservations(now_ms))
        })
    }

    pub(crate) fn cleanup_claim_sync(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<protocol::StorageCleanupBatch, StorageError> {
        let limit = usize::try_from(limit).map_err(|_| StorageError::Internal)?;
        self.with_maintenance(|session| {
            session.atomic(|session| {
                let removed = session.storage_prepare_expired_cleanup(now_ms, limit)?;
                let items = session
                    .storage_claim_cleanup(now_ms, limit, CLEANUP_LEASE_MS)?
                    .into_iter()
                    .map(|item| {
                        Ok(protocol::StorageCleanupItem {
                            id: item.id,
                            provider_key: protocol::SensitiveString::new(item.provider_key),
                            action: protocol_storage_action(&item.action)?,
                            upload_id: item.upload_id,
                            lease_token: protocol::SensitiveString::new(item.lease_token),
                            attempt: item.attempt,
                            lease_expires_at_ms: item.lease_expires_at_ms,
                        })
                    })
                    .collect::<Result<Vec<_>, runtime::RuntimeError>>()?;
                Ok(protocol::StorageCleanupBatch {
                    removed_reservations: u64::try_from(removed).unwrap_or(u64::MAX),
                    items,
                })
            })
        })
    }

    pub(crate) fn cleanup_ack_sync(
        &self,
        now_ms: i64,
        items: Vec<protocol::StorageCleanupDisposition>,
    ) -> Result<(u64, u64), StorageError> {
        let dispositions = items
            .into_iter()
            .map(|item| {
                (
                    item.id,
                    item.lease_token.into_inner(),
                    match item.outcome {
                        protocol::StorageCleanupOutcome::Deleted => {
                            runtime::StorageCleanupDisposition::Deleted
                        }
                        protocol::StorageCleanupOutcome::Retry => {
                            runtime::StorageCleanupDisposition::Retry
                        }
                    },
                )
            })
            .collect::<Vec<_>>();
        self.with_maintenance(|session| {
            session.atomic(|session| {
                let (removed, retried) = session.storage_ack_cleanup(now_ms, &dispositions)?;
                Ok((
                    u64::try_from(removed).unwrap_or(u64::MAX),
                    u64::try_from(retried).unwrap_or(u64::MAX),
                ))
            })
        })
    }
}

#[async_trait]
impl MetadataAuthorizer for SqliteMetadataAuthorizer {
    async fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<MetadataAuthorization, StorageError> {
        self.authorize_sync(request)
    }

    async fn reserve(&self, request: &StorageReservationRequest) -> Result<(), StorageError> {
        self.reserve_sync(request)
    }

    async fn commit(&self, request: &StorageMetadataCommit) -> Result<(), StorageError> {
        self.commit_sync(request)
    }

    async fn receipt(
        &self,
        request: &StorageReceiptRequest,
    ) -> Result<Option<StorageCommitResult>, StorageError> {
        self.receipt_sync(request)
    }

    async fn release_reservation(
        &self,
        auth: &protocol::AuthContext,
        nonce: &str,
        reservation_bytes: u64,
        reservation_expires_at_ms: i64,
    ) -> Result<(), StorageError> {
        self.release_reservation_sync(auth, nonce, reservation_bytes, reservation_expires_at_ms)
    }

    async fn cleanup_expired_reservations(&self, now_ms: i64) -> Result<usize, StorageError> {
        self.cleanup_expired_reservations_sync(now_ms)
    }
}

fn load_bucket(
    session: &mut runtime::Session<'_>,
    name: &str,
) -> Result<BucketMetadata, runtime::RuntimeError> {
    let result = session.execute(&runtime::StatementRequest {
        sql: "SELECT id,max_object_bytes,project_quota_bytes FROM storage_buckets WHERE name=?1"
            .to_owned(),
        parameters: vec![runtime::SqlParameter::Text(name.to_owned())],
    })?;
    let row = result
        .rows
        .first()
        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
    Ok(BucketMetadata {
        id: text(row.first())?,
        max_object_bytes: unsigned(row.get(1))?,
        project_quota_bytes: unsigned(row.get(2))?,
    })
}

fn load_object(
    session: &mut runtime::Session<'_>,
    bucket_id: &str,
    object_key: &str,
) -> Result<Option<ObjectMetadata>, runtime::RuntimeError> {
    let result = session.execute(&runtime::StatementRequest {
        sql: "SELECT id,size_bytes FROM storage_objects WHERE bucket_id=?1 AND object_key=?2"
            .to_owned(),
        parameters: vec![
            runtime::SqlParameter::Text(bucket_id.to_owned()),
            runtime::SqlParameter::Text(object_key.to_owned()),
        ],
    })?;
    result
        .rows
        .first()
        .map(|row| {
            Ok(ObjectMetadata {
                id: text(row.first())?,
                size_bytes: unsigned(row.get(1))?,
            })
        })
        .transpose()
}

fn load_upload(
    session: &mut runtime::Session<'_>,
    upload_id: &str,
    bucket_id: &str,
    object_key: &str,
) -> Result<Option<UploadMetadata>, runtime::RuntimeError> {
    let result = session.execute(&runtime::StatementRequest {
        sql: "SELECT id,owner_id,expected_size_bytes,checksum_sha256,content_type,status \
              FROM storage_uploads WHERE id=?1 AND bucket_id=?2 AND object_key=?3"
            .to_owned(),
        parameters: vec![
            runtime::SqlParameter::Text(upload_id.to_owned()),
            runtime::SqlParameter::Text(bucket_id.to_owned()),
            runtime::SqlParameter::Text(object_key.to_owned()),
        ],
    })?;
    result
        .rows
        .first()
        .map(|row| {
            Ok(UploadMetadata {
                id: text(row.first())?,
                owner_id: text(row.get(1))?,
                expected_size_bytes: nullable_unsigned(row.get(2))?,
                checksum_sha256: nullable_text(row.get(3))?,
                content_type: nullable_text(row.get(4))?,
                status: text(row.get(5))?,
            })
        })
        .transpose()
}

fn load_committed_upload(
    session: &mut runtime::Session<'_>,
    bucket: &BucketMetadata,
    request: &StorageMetadataCommit,
) -> Result<UploadMetadata, runtime::RuntimeError> {
    let upload_id = request
        .upload_id
        .as_deref()
        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
    load_upload(session, upload_id, &bucket.id, &request.object_key)?
        .filter(|upload| upload.owner_id == request.auth.subject.to_string())
        .ok_or(runtime::RuntimeError::StatementNotAllowed)
}

fn verify_upload_provider(
    session: &mut runtime::Session<'_>,
    upload: &UploadMetadata,
    provider_key: &str,
) -> Result<runtime::StorageProviderUploadBinding, runtime::RuntimeError> {
    let expected = session
        .storage_upload_provider_binding(&upload.id)?
        .ok_or(runtime::RuntimeError::Database)?;
    if expected.provider_key != provider_key {
        return Err(runtime::RuntimeError::StatementNotAllowed);
    }
    Ok(expected)
}

fn probe_upload_insert(
    session: &mut runtime::Session<'_>,
    upload_id: &str,
    bucket_id: &str,
    request: &AuthorizationRequest,
    now_ms: i64,
) -> Result<(), runtime::RuntimeError> {
    session.probe_write(&runtime::StatementRequest {
        sql: "INSERT INTO storage_uploads \
              (id,bucket_id,object_key,owner_id,expected_size_bytes,checksum_sha256,content_type,\
               status,created_at_ms,expires_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'authorizing',?8,?9)"
            .to_owned(),
        parameters: vec![
            runtime::SqlParameter::Text(upload_id.to_owned()),
            runtime::SqlParameter::Text(bucket_id.to_owned()),
            runtime::SqlParameter::Text(request.object_key.clone()),
            runtime::SqlParameter::Text(request.auth.subject.to_string()),
            optional_integer(request.content_length)?,
            optional_text(request.checksum_sha256.as_deref()),
            optional_text(request.content_type.as_deref()),
            runtime::SqlParameter::Integer(now_ms),
            runtime::SqlParameter::Integer(now_ms.saturating_add(24 * 60 * 60 * 1_000)),
        ],
    })
}

fn probe_upload_update(
    session: &mut runtime::Session<'_>,
    upload_id: &str,
    status: &str,
) -> Result<(), runtime::RuntimeError> {
    session.probe_write(&runtime::StatementRequest {
        sql: "UPDATE storage_uploads SET status=?1 WHERE id=?2".to_owned(),
        parameters: vec![
            runtime::SqlParameter::Text(status.to_owned()),
            runtime::SqlParameter::Text(upload_id.to_owned()),
        ],
    })
}

fn commit_create_multipart(
    session: &mut runtime::Session<'_>,
    cursor_codec: &StorageCursorCodec,
    project_id: protocol::ProjectId,
    bucket: &BucketMetadata,
    request: &StorageMetadataCommit,
    now_ms: i64,
) -> Result<(), runtime::RuntimeError> {
    if request.upload_id.is_some() || request.content_length.is_none() {
        return Err(runtime::RuntimeError::StorageReservationMismatch);
    }
    let provider_upload_id = request
        .version_id
        .as_deref()
        .ok_or(runtime::RuntimeError::StatementNotAllowed)?;
    let object = load_object(session, &bucket.id, &request.object_key)?;
    verify_replacement_state(
        session,
        cursor_codec,
        project_id,
        bucket,
        object.as_ref(),
        request.replacement_fingerprint.as_deref(),
    )?;
    let _ = session.execute(&runtime::StatementRequest {
        sql: "INSERT INTO storage_uploads \
              (id,bucket_id,object_key,owner_id,expected_size_bytes,checksum_sha256,content_type,\
               status,created_at_ms,expires_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'active',?8,?9)"
            .to_owned(),
        parameters: vec![
            runtime::SqlParameter::Text(provider_upload_id.to_owned()),
            runtime::SqlParameter::Text(bucket.id.clone()),
            runtime::SqlParameter::Text(request.object_key.clone()),
            runtime::SqlParameter::Text(request.auth.subject.to_string()),
            optional_integer(request.content_length)?,
            optional_text(request.checksum_sha256.as_deref()),
            optional_text(request.content_type.as_deref()),
            runtime::SqlParameter::Integer(now_ms),
            runtime::SqlParameter::Integer(now_ms.saturating_add(24 * 60 * 60 * 1_000)),
        ],
    })?;
    session.storage_set_upload_provider_binding(
        provider_upload_id,
        &request.provider_key,
        request.reservation_bytes,
        request.replacement_fingerprint.as_deref(),
    )
}

fn probe_object_insert(
    session: &mut runtime::Session<'_>,
    object_id: &str,
    bucket_id: &str,
    request: &AuthorizationRequest,
    now_ms: i64,
) -> Result<(), runtime::RuntimeError> {
    session.probe_write(&runtime::StatementRequest {
        sql: "INSERT INTO storage_objects \
              (id,bucket_id,object_key,owner_id,size_bytes,content_type,checksum_sha256,etag,\
               version_id,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,NULL,?8,?8)"
            .to_owned(),
        parameters: vec![
            runtime::SqlParameter::Text(object_id.to_owned()),
            runtime::SqlParameter::Text(bucket_id.to_owned()),
            runtime::SqlParameter::Text(request.object_key.clone()),
            runtime::SqlParameter::Text(request.auth.subject.to_string()),
            integer(request.content_length)?,
            optional_text(request.content_type.as_deref()),
            optional_text(request.checksum_sha256.as_deref()),
            runtime::SqlParameter::Integer(now_ms),
        ],
    })
}

fn probe_object_update(
    session: &mut runtime::Session<'_>,
    object: &ObjectMetadata,
    request: &AuthorizationRequest,
    now_ms: i64,
) -> Result<(), runtime::RuntimeError> {
    session.probe_write(&runtime::StatementRequest {
        sql: "UPDATE storage_objects SET size_bytes=?1,content_type=?2,checksum_sha256=?3,\
              updated_at_ms=?4 WHERE id=?5"
            .to_owned(),
        parameters: vec![
            integer(request.content_length)?,
            optional_text(request.content_type.as_deref()),
            optional_text(request.checksum_sha256.as_deref()),
            runtime::SqlParameter::Integer(now_ms),
            runtime::SqlParameter::Text(object.id.clone()),
        ],
    })
}

fn commit_object_upload(
    session: &mut runtime::Session<'_>,
    cursor_codec: &StorageCursorCodec,
    project_id: protocol::ProjectId,
    bucket: &BucketMetadata,
    request: &StorageMetadataCommit,
    now_ms: i64,
) -> Result<(), runtime::RuntimeError> {
    let object = load_object(session, &bucket.id, &request.object_key)?;
    verify_replacement_state(
        session,
        cursor_codec,
        project_id,
        bucket,
        object.as_ref(),
        request.replacement_fingerprint.as_deref(),
    )?;
    let object_id = if let Some(object) = object {
        let old_provider_key = session
            .storage_provider_key(&object.id)?
            .ok_or(runtime::RuntimeError::Database)?;
        let _ = session.execute(&runtime::StatementRequest {
            sql: "UPDATE storage_objects SET size_bytes=?1,content_type=?2,checksum_sha256=?3,\
                  etag=?4,version_id=?5,updated_at_ms=?6 WHERE id=?7"
                .to_owned(),
            parameters: vec![
                integer(request.content_length)?,
                optional_text(request.content_type.as_deref()),
                optional_text(request.checksum_sha256.as_deref()),
                optional_text(request.etag.as_deref()),
                optional_text(request.version_id.as_deref()),
                runtime::SqlParameter::Integer(now_ms),
                runtime::SqlParameter::Text(object.id.clone()),
            ],
        })?;
        session.storage_set_provider_key(&object.id, &request.provider_key)?;
        if old_provider_key != request.provider_key {
            session.storage_enqueue_provider_cleanup(&old_provider_key, "upload", None, now_ms)?;
        }
        object.id
    } else {
        let object_id = request
            .provider_key
            .rsplit('/')
            .next()
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or(runtime::RuntimeError::StatementNotAllowed)?
            .to_string();
        let _ = session.execute(&runtime::StatementRequest {
            sql: "INSERT INTO storage_objects \
                  (id,bucket_id,object_key,owner_id,size_bytes,content_type,checksum_sha256,etag,\
                   version_id,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)"
                .to_owned(),
            parameters: vec![
                runtime::SqlParameter::Text(object_id.clone()),
                runtime::SqlParameter::Text(bucket.id.clone()),
                runtime::SqlParameter::Text(request.object_key.clone()),
                runtime::SqlParameter::Text(request.auth.subject.to_string()),
                integer(request.content_length)?,
                optional_text(request.content_type.as_deref()),
                optional_text(request.checksum_sha256.as_deref()),
                optional_text(request.etag.as_deref()),
                optional_text(request.version_id.as_deref()),
                runtime::SqlParameter::Integer(now_ms),
            ],
        })?;
        session.storage_set_provider_key(&object_id, &request.provider_key)?;
        object_id
    };
    if let Some(provider_version_id) = request.version_id.as_deref() {
        let _ = session.execute(&runtime::StatementRequest {
            sql: "INSERT INTO storage_versions \
                  (id,object_id,owner_id,size_bytes,checksum_sha256,etag,provider_version_id,created_at_ms) \
                  VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"
                .to_owned(),
            parameters: vec![
                runtime::SqlParameter::Text(Uuid::now_v7().to_string()),
                runtime::SqlParameter::Text(object_id),
                runtime::SqlParameter::Text(request.auth.subject.to_string()),
                integer(request.content_length)?,
                optional_text(request.checksum_sha256.as_deref()),
                optional_text(request.etag.as_deref()),
                runtime::SqlParameter::Text(provider_version_id.to_owned()),
                runtime::SqlParameter::Integer(now_ms),
            ],
        })?;
    }
    Ok(())
}

fn verify_replacement_state(
    session: &mut runtime::Session<'_>,
    cursor_codec: &StorageCursorCodec,
    project_id: protocol::ProjectId,
    bucket: &BucketMetadata,
    object: Option<&ObjectMetadata>,
    expected_fingerprint: Option<&str>,
) -> Result<(), runtime::RuntimeError> {
    match (object, expected_fingerprint) {
        (None, None) => Ok(()),
        (Some(object), Some(expected)) => {
            let provider_key = session
                .storage_provider_key(&object.id)?
                .ok_or(runtime::RuntimeError::Database)?;
            let actual = cursor_codec.replacement_fingerprint(
                project_id,
                &bucket.id,
                object,
                &provider_key,
            )?;
            if actual == expected {
                Ok(())
            } else {
                Err(runtime::RuntimeError::StorageReservationMismatch)
            }
        }
        _ => Err(runtime::RuntimeError::StorageReservationMismatch),
    }
}

fn provider_object_key(
    project_id: protocol::ProjectId,
    bucket_id: &str,
    object_id: &str,
) -> String {
    format!("projects/{project_id}/buckets/{bucket_id}/objects/{object_id}")
}

fn text(value: Option<&runtime::ResultValue>) -> Result<String, runtime::RuntimeError> {
    match value {
        Some(runtime::ResultValue::Text(value)) => Ok(value.clone()),
        _ => Err(runtime::RuntimeError::Database),
    }
}

fn unsigned(value: Option<&runtime::ResultValue>) -> Result<u64, runtime::RuntimeError> {
    match value {
        Some(runtime::ResultValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| runtime::RuntimeError::Database)
        }
        Some(runtime::ResultValue::IntegerString(value)) => {
            value.parse().map_err(|_| runtime::RuntimeError::Database)
        }
        _ => Err(runtime::RuntimeError::Database),
    }
}

fn nullable_unsigned(
    value: Option<&runtime::ResultValue>,
) -> Result<Option<u64>, runtime::RuntimeError> {
    match value {
        Some(runtime::ResultValue::Null) => Ok(None),
        value => unsigned(value).map(Some),
    }
}

fn signed(value: Option<&runtime::ResultValue>) -> Result<i64, runtime::RuntimeError> {
    match value {
        Some(runtime::ResultValue::Integer(value)) => Ok(*value),
        Some(runtime::ResultValue::IntegerString(value)) => {
            value.parse().map_err(|_| runtime::RuntimeError::Database)
        }
        _ => Err(runtime::RuntimeError::Database),
    }
}

fn nullable_text(
    value: Option<&runtime::ResultValue>,
) -> Result<Option<String>, runtime::RuntimeError> {
    match value {
        Some(runtime::ResultValue::Null) => Ok(None),
        Some(runtime::ResultValue::Text(value)) => Ok(Some(value.clone())),
        _ => Err(runtime::RuntimeError::Database),
    }
}

fn storage_item(
    row: &[runtime::ResultValue],
) -> Result<protocol::StorageObjectItem, runtime::RuntimeError> {
    Ok(protocol::StorageObjectItem {
        id: text(row.first())?,
        object_key: text(row.get(1))?,
        owner_id: text(row.get(2))?,
        size_bytes: unsigned(row.get(3))?,
        content_type: nullable_text(row.get(4))?,
        checksum_sha256: nullable_text(row.get(5))?,
        etag: nullable_text(row.get(6))?,
        version_id: nullable_text(row.get(7))?,
        created_at_ms: signed(row.get(8))?,
        updated_at_ms: signed(row.get(9))?,
    })
}

fn integer(value: Option<u64>) -> Result<runtime::SqlParameter, runtime::RuntimeError> {
    value
        .ok_or(runtime::RuntimeError::StatementNotAllowed)
        .and_then(|value| {
            i64::try_from(value)
                .map(runtime::SqlParameter::Integer)
                .map_err(|_| runtime::RuntimeError::StorageQuotaExceeded)
        })
}

fn optional_integer(value: Option<u64>) -> Result<runtime::SqlParameter, runtime::RuntimeError> {
    value.map_or(Ok(runtime::SqlParameter::Null), |value| {
        i64::try_from(value)
            .map(runtime::SqlParameter::Integer)
            .map_err(|_| runtime::RuntimeError::StorageQuotaExceeded)
    })
}

fn optional_text(value: Option<&str>) -> runtime::SqlParameter {
    value.map_or(runtime::SqlParameter::Null, |value| {
        runtime::SqlParameter::Text(value.to_owned())
    })
}

fn scope_fingerprint(auth: &protocol::AuthContext) -> Result<String, runtime::RuntimeError> {
    let bytes = serde_json::to_vec(&json!({"role": auth.role, "claims": auth.claims}))
        .map_err(|_| runtime::RuntimeError::Database)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn storage_action_name(action: StorageAction) -> &'static str {
    match action {
        StorageAction::Upload => "upload",
        StorageAction::Download => "download",
        StorageAction::Delete => "delete",
        StorageAction::List => "list",
        StorageAction::CreateMultipart => "create_multipart",
        StorageAction::UploadPart => "upload_part",
        StorageAction::CompleteMultipart => "complete_multipart",
        StorageAction::AbortMultipart => "abort_multipart",
    }
}

fn protocol_storage_action(action: &str) -> Result<protocol::StorageAction, runtime::RuntimeError> {
    match action {
        "upload" => Ok(protocol::StorageAction::Upload),
        "download" => Ok(protocol::StorageAction::Download),
        "delete" => Ok(protocol::StorageAction::Delete),
        "list" => Ok(protocol::StorageAction::List),
        "create_multipart" => Ok(protocol::StorageAction::CreateMultipart),
        "upload_part" => Ok(protocol::StorageAction::UploadPart),
        "complete_multipart" => Ok(protocol::StorageAction::CompleteMultipart),
        "abort_multipart" => Ok(protocol::StorageAction::AbortMultipart),
        _ => Err(runtime::RuntimeError::Database),
    }
}

fn commit_binding_digest(
    request: &StorageMetadataCommit,
) -> Result<Vec<u8>, runtime::RuntimeError> {
    binding_digest(
        &request.auth,
        &request.bucket,
        &request.object_key,
        &request.provider_key,
        request.action,
        request.content_length,
        request.checksum_sha256.as_deref(),
        request.content_type.as_deref(),
        request.upload_id.as_deref(),
        request.part_number,
        &request.reservation_nonce,
        request.reservation_bytes,
        request.reservation_expires_at_ms,
        request.replacement_fingerprint.as_deref(),
    )
}

fn receipt_binding_digest(
    request: &StorageReceiptRequest,
) -> Result<Vec<u8>, runtime::RuntimeError> {
    binding_digest(
        &request.auth,
        &request.bucket,
        &request.object_key,
        &request.provider_key,
        request.action,
        request.content_length,
        request.checksum_sha256.as_deref(),
        request.content_type.as_deref(),
        request.upload_id.as_deref(),
        request.part_number,
        &request.reservation_nonce,
        request.reservation_bytes,
        request.reservation_expires_at_ms,
        request.replacement_fingerprint.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn binding_digest(
    auth: &protocol::AuthContext,
    bucket: &str,
    object_key: &str,
    provider_key: &str,
    action: StorageAction,
    content_length: Option<u64>,
    checksum_sha256: Option<&str>,
    content_type: Option<&str>,
    upload_id: Option<&str>,
    part_number: Option<u32>,
    reservation_nonce: &str,
    reservation_bytes: u64,
    reservation_expires_at_ms: i64,
    replacement_fingerprint: Option<&str>,
) -> Result<Vec<u8>, runtime::RuntimeError> {
    let encoded = serde_json::to_vec(&json!({
        "domain": "ffdb.storage.commit-binding.v1",
        "project_id": auth.project_id,
        "subject": auth.subject,
        "token_id": auth.token_id,
        "bucket": bucket,
        "object_key": object_key,
        "provider_key": provider_key,
        "action": storage_action_name(action),
        "content_length": content_length,
        "checksum_sha256": checksum_sha256,
        "content_type": content_type,
        "upload_id": upload_id,
        "part_number": part_number,
        "reservation_nonce": reservation_nonce,
        "reservation_bytes": reservation_bytes,
        "reservation_expires_at_ms": reservation_expires_at_ms,
        "replacement_fingerprint": replacement_fingerprint,
    }))
    .map_err(|_| runtime::RuntimeError::Database)?;
    Ok(Sha256::digest(encoded).to_vec())
}

fn commit_result(request: &StorageMetadataCommit) -> StorageCommitResult {
    StorageCommitResult {
        content_length: request.content_length,
        checksum_sha256: request.checksum_sha256.clone(),
        etag: request.etag.clone(),
        version_id: request.version_id.clone(),
    }
}

fn commit_result_digest(
    binding_digest: &[u8],
    result: &StorageCommitResult,
) -> Result<Vec<u8>, runtime::RuntimeError> {
    let encoded = serde_json::to_vec(&json!({
        "domain": "ffdb.storage.commit-result.v1",
        "binding_digest": URL_SAFE_NO_PAD.encode(binding_digest),
        "content_length": result.content_length,
        "checksum_sha256": result.checksum_sha256,
        "etag": result.etag,
        "version_id": result.version_id,
    }))
    .map_err(|_| runtime::RuntimeError::Database)?;
    Ok(Sha256::digest(encoded).to_vec())
}

fn epoch_ms() -> Result<i64, StorageError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StorageError::Internal)?;
    i64::try_from(duration.as_millis()).map_err(|_| StorageError::Internal)
}

fn map_runtime(error: runtime::RuntimeError) -> StorageError {
    match error {
        runtime::RuntimeError::StorageQuotaExceeded => StorageError::ProjectQuotaExceeded,
        runtime::RuntimeError::StorageReservationDuplicate => StorageError::DuplicateReservation,
        runtime::RuntimeError::StorageReservationMismatch => StorageError::Internal,
        runtime::RuntimeError::StatementNotAllowed
        | runtime::RuntimeError::ConstraintViolation
        | runtime::RuntimeError::Database => StorageError::RlsDenied,
        error => {
            tracing::warn!(runtime_error = ?error, "storage metadata operation failed");
            StorageError::Internal
        }
    }
}

const fn requires_durable_reservation(action: StorageAction) -> bool {
    !matches!(action, StorageAction::Download | StorageAction::List)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::{Duration, Instant};

    use ffdb_migration_engine::{MigrationEngine, MigrationSpec};
    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn metadata_authorization_uses_rls_and_durable_scoped_reservations() {
        let directory = TempDir::new().unwrap();
        let database_id = protocol::DatabaseId::new();
        let project_id = protocol::ProjectId::new();
        let path =
            runtime::TrustedDatabasePath::for_database(directory.path(), &database_id.to_string())
                .unwrap();
        let database =
            Arc::new(runtime::Database::open(path, runtime::RuntimeConfig::default()).unwrap());
        let cancellation = runtime::CancellationToken::default();
        let budget = runtime::RequestBudget {
            limits: runtime::ExecutionLimits::default(),
            deadline: Instant::now() + Duration::from_secs(30),
        };
        let developer = runtime::DeveloperPrincipal {
            actor_id: "test".to_owned(),
            api_key_id: "test-key".to_owned(),
        };
        let mut migration = MigrationSpec {
            id: "storage_owner_policies".to_owned(),
            name: "storage owner policies".to_owned(),
            up_sql: "CREATE POLICY storage_bucket_authenticated ON storage_buckets FOR SELECT TO authenticated \
                     USING (1); \
                     CREATE POLICY storage_object_owner ON storage_objects FOR ALL TO authenticated \
                     USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid()); \
                     CREATE POLICY storage_upload_owner ON storage_uploads FOR ALL TO authenticated \
                     USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid()); \
                     CREATE POLICY storage_version_owner ON storage_versions FOR ALL TO authenticated \
                     USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())"
                .to_owned(),
            down_sql: "DROP POLICY storage_object_owner ON storage_objects; \
                       DROP POLICY storage_upload_owner ON storage_uploads; \
                       DROP POLICY storage_version_owner ON storage_versions; \
                       DROP POLICY storage_bucket_authenticated ON storage_buckets"
                .to_owned(),
            checksum: String::new(),
            created_at_ms: 1,
        };
        migration.checksum = migration.calculate_checksum();
        MigrationEngine
            .apply(
                &database,
                developer.clone(),
                &migration,
                1,
                &cancellation,
                &budget,
            )
            .unwrap();
        let owner = protocol::UserId::new();
        database
            .with_context(
                runtime::ExecutionMode::Developer(developer),
                &cancellation,
                |session| {
                    let _ = session.execute(&runtime::StatementRequest {
                        sql: "INSERT INTO storage_buckets \
                              (id,name,owner_id,public,max_object_bytes,project_quota_bytes,created_at_ms) \
                              VALUES ('bucket-1','private',?1,0,1000,10000,1)"
                            .to_owned(),
                        parameters: vec![runtime::SqlParameter::Text(Uuid::nil().to_string())],
                    })?;
                    Ok(())
                },
            )
            .unwrap();

        let adapter = SqliteMetadataAuthorizer::new(
            Arc::clone(&database),
            project_id,
            StorageCursorCodec::new([7_u8; 32]).unwrap(),
        );
        let alice = protocol::AuthContext {
            project_id,
            subject: owner,
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: protocol::TokenId::new(),
        };
        let upload = AuthorizationRequest {
            auth: alice.clone(),
            bucket: "private".to_owned(),
            object_key: "docs/a.txt".to_owned(),
            action: StorageAction::Upload,
            content_length: Some(25),
            checksum_sha256: Some("abc".to_owned()),
            content_type: Some("text/plain".to_owned()),
            upload_id: None,
            part_number: None,
        };
        let authorization = adapter.authorize(&upload).await.unwrap();
        let nonce = "server-nonce-0000001";
        let reservation_expires_at_ms = epoch_ms().unwrap() + 60_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: nonce.to_owned(),
                bytes: authorization.reservation_bytes,
                expires_at_ms: reservation_expires_at_ms,
                provider_key: authorization.provider_key.clone(),
                action: StorageAction::Upload,
                upload_id: None,
            })
            .await
            .unwrap();
        let commit = StorageMetadataCommit {
            auth: alice.clone(),
            bucket: upload.bucket.clone(),
            object_key: upload.object_key.clone(),
            provider_key: authorization.provider_key.clone(),
            action: StorageAction::Upload,
            content_length: upload.content_length,
            checksum_sha256: upload.checksum_sha256.clone(),
            content_type: upload.content_type.clone(),
            upload_id: None,
            part_number: None,
            etag: Some("etag".to_owned()),
            version_id: None,
            reservation_nonce: nonce.to_owned(),
            reservation_bytes: 25,
            reservation_expires_at_ms,
            replacement_fingerprint: authorization.replacement_fingerprint.clone(),
        };
        let mut rejected_commit = commit.clone();
        rejected_commit.provider_key = "not-a-provider-key".to_owned();
        assert_eq!(
            adapter.commit(&rejected_commit).await.unwrap_err(),
            StorageError::Internal
        );
        adapter.commit(&commit).await.unwrap();
        adapter.commit(&commit).await.unwrap();
        let mut changed_replay = commit.clone();
        changed_replay.etag = Some("different-etag".to_owned());
        assert_eq!(
            adapter.commit(&changed_replay).await.unwrap_err(),
            StorageError::Internal
        );
        let download = adapter
            .authorize(&AuthorizationRequest {
                auth: alice.clone(),
                action: StorageAction::Download,
                content_length: None,
                checksum_sha256: None,
                content_type: None,
                upload_id: None,
                ..upload.clone()
            })
            .await
            .unwrap();
        assert_eq!(download.provider_key, authorization.provider_key);

        let overwrite = AuthorizationRequest {
            content_length: Some(40),
            checksum_sha256: Some("def".to_owned()),
            ..upload.clone()
        };
        let overwrite_authorization = adapter.authorize(&overwrite).await.unwrap();
        assert_ne!(
            overwrite_authorization.provider_key, authorization.provider_key,
            "overwrite authorization must be copy-on-write"
        );
        assert_eq!(overwrite_authorization.reservation_bytes, 15);
        assert!(overwrite_authorization.replacement_fingerprint.is_some());
        let overwrite_nonce = "server-nonce-overwrite-1";
        let overwrite_expiry = epoch_ms().unwrap() + 60_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: overwrite_nonce.to_owned(),
                bytes: overwrite_authorization.reservation_bytes,
                expires_at_ms: overwrite_expiry,
                provider_key: overwrite_authorization.provider_key.clone(),
                action: StorageAction::Upload,
                upload_id: None,
            })
            .await
            .unwrap();
        let overwrite_commit = StorageMetadataCommit {
            auth: alice.clone(),
            bucket: overwrite.bucket.clone(),
            object_key: overwrite.object_key.clone(),
            provider_key: overwrite_authorization.provider_key.clone(),
            action: StorageAction::Upload,
            content_length: overwrite.content_length,
            checksum_sha256: overwrite.checksum_sha256.clone(),
            content_type: overwrite.content_type.clone(),
            upload_id: None,
            part_number: None,
            etag: Some("overwrite-etag".to_owned()),
            version_id: None,
            reservation_nonce: overwrite_nonce.to_owned(),
            reservation_bytes: overwrite_authorization.reservation_bytes,
            reservation_expires_at_ms: overwrite_expiry,
            replacement_fingerprint: overwrite_authorization.replacement_fingerprint.clone(),
        };
        let mut stale_overwrite = overwrite_commit.clone();
        stale_overwrite.replacement_fingerprint = Some("tampered-state".to_owned());
        assert_eq!(
            adapter.commit(&stale_overwrite).await.unwrap_err(),
            StorageError::Internal
        );
        adapter.commit(&overwrite_commit).await.unwrap();
        adapter.commit(&overwrite_commit).await.unwrap();
        let current_download = adapter
            .authorize(&AuthorizationRequest {
                action: StorageAction::Download,
                content_length: None,
                checksum_sha256: None,
                content_type: None,
                ..overwrite.clone()
            })
            .await
            .unwrap();
        assert_eq!(
            current_download.provider_key,
            overwrite_authorization.provider_key
        );

        let cleanup_now = epoch_ms().unwrap();
        let cleanup = adapter.cleanup_claim_sync(cleanup_now, 10).unwrap();
        assert_eq!(cleanup.items.len(), 1);
        assert_eq!(
            cleanup.items[0].provider_key.expose(),
            authorization.provider_key
        );
        assert_eq!(cleanup.items[0].action, protocol::StorageAction::Upload);
        let cleanup_id = cleanup.items[0].id.clone();
        let first_lease = cleanup.items[0].lease_token.expose().to_owned();
        assert_eq!(
            adapter
                .cleanup_ack_sync(
                    cleanup_now,
                    vec![protocol::StorageCleanupDisposition {
                        id: cleanup_id.clone(),
                        lease_token: protocol::SensitiveString::new("wrong-lease"),
                        outcome: protocol::StorageCleanupOutcome::Deleted,
                    }],
                )
                .unwrap_err(),
            StorageError::Internal
        );
        assert_eq!(
            adapter
                .cleanup_ack_sync(
                    cleanup_now,
                    vec![protocol::StorageCleanupDisposition {
                        id: cleanup_id.clone(),
                        lease_token: protocol::SensitiveString::new(first_lease.clone()),
                        outcome: protocol::StorageCleanupOutcome::Retry,
                    }],
                )
                .unwrap(),
            (0, 1)
        );
        assert!(
            adapter
                .cleanup_claim_sync(cleanup_now, 10)
                .unwrap()
                .items
                .is_empty()
        );
        let retry = adapter
            .cleanup_claim_sync(cleanup_now.saturating_add(1_000), 10)
            .unwrap();
        assert_eq!(retry.items.len(), 1);
        assert_eq!(retry.items[0].attempt, 2);
        let second_lease = retry.items[0].lease_token.expose().to_owned();
        assert_ne!(first_lease, second_lease);
        assert_eq!(
            adapter
                .cleanup_ack_sync(
                    cleanup_now.saturating_add(1_000),
                    vec![protocol::StorageCleanupDisposition {
                        id: cleanup_id.clone(),
                        lease_token: protocol::SensitiveString::new(first_lease),
                        outcome: protocol::StorageCleanupOutcome::Deleted,
                    }],
                )
                .unwrap_err(),
            StorageError::Internal
        );
        assert_eq!(
            adapter
                .cleanup_ack_sync(
                    cleanup_now.saturating_add(1_000),
                    vec![protocol::StorageCleanupDisposition {
                        id: cleanup_id.clone(),
                        lease_token: protocol::SensitiveString::new(second_lease.clone()),
                        outcome: protocol::StorageCleanupOutcome::Deleted,
                    }],
                )
                .unwrap(),
            (1, 0)
        );
        assert_eq!(
            adapter
                .cleanup_ack_sync(
                    cleanup_now.saturating_add(1_000),
                    vec![protocol::StorageCleanupDisposition {
                        id: cleanup_id,
                        lease_token: protocol::SensitiveString::new(second_lease),
                        outcome: protocol::StorageCleanupOutcome::Deleted,
                    }],
                )
                .unwrap_err(),
            StorageError::Internal,
            "cleanup acknowledgement replay is rejected"
        );

        let bob = protocol::AuthContext {
            project_id,
            subject: protocol::UserId::new(),
            role: "authenticated".to_owned(),
            claims: Map::new(),
            token_id: protocol::TokenId::new(),
        };
        let receipt_request = StorageReceiptRequest {
            auth: alice.clone(),
            bucket: commit.bucket.clone(),
            object_key: commit.object_key.clone(),
            provider_key: commit.provider_key.clone(),
            action: commit.action,
            content_length: commit.content_length,
            checksum_sha256: commit.checksum_sha256.clone(),
            content_type: commit.content_type.clone(),
            upload_id: commit.upload_id.clone(),
            part_number: commit.part_number,
            reservation_nonce: commit.reservation_nonce.clone(),
            reservation_bytes: commit.reservation_bytes,
            reservation_expires_at_ms: commit.reservation_expires_at_ms,
            replacement_fingerprint: commit.replacement_fingerprint.clone(),
        };
        assert_eq!(
            adapter.receipt(&receipt_request).await.unwrap(),
            Some(commit_result(&commit))
        );
        let mut cross_subject_receipt = receipt_request;
        cross_subject_receipt.auth = bob.clone();
        assert_eq!(
            adapter.receipt(&cross_subject_receipt).await.unwrap_err(),
            StorageError::Internal
        );
        let mut cross_subject_commit = commit.clone();
        cross_subject_commit.auth = bob.clone();
        assert_eq!(
            adapter.commit(&cross_subject_commit).await.unwrap_err(),
            StorageError::Internal
        );
        let cursor = adapter
            .cursor_codec
            .encode(&ListCursor {
                project_id: alice.project_id.to_string(),
                subject: alice.subject.to_string(),
                token_id: alice.token_id.to_string(),
                scope_fingerprint: scope_fingerprint(&alice).unwrap(),
                bucket: "private".to_owned(),
                prefix: "docs/".to_owned(),
                object_key: "docs/a.txt".to_owned(),
                id: "cursor-object".to_owned(),
            })
            .unwrap();
        let mut tampered = cursor.clone().into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert_eq!(
            adapter
                .cursor_codec
                .decode(std::str::from_utf8(&tampered).unwrap())
                .unwrap_err(),
            StorageError::InvalidGrant
        );
        assert!(
            adapter
                .list_sync(
                    &alice,
                    &protocol::StorageListRequest {
                        bucket: "private".to_owned(),
                        prefix: "docs/".to_owned(),
                        limit: 10,
                        cursor: Some(cursor.clone()),
                    },
                )
                .is_ok(),
            "the issuing operator can resume with an untampered cursor"
        );
        assert_eq!(
            adapter
                .list_sync(
                    &bob,
                    &protocol::StorageListRequest {
                        bucket: "private".to_owned(),
                        prefix: "docs/".to_owned(),
                        limit: 10,
                        cursor: Some(cursor),
                    },
                )
                .unwrap_err(),
            StorageError::InvalidGrant,
            "a valid cursor cannot cross operator/token boundaries"
        );
        let denied = adapter
            .authorize(&AuthorizationRequest {
                auth: bob.clone(),
                action: StorageAction::Download,
                content_length: None,
                checksum_sha256: None,
                content_type: None,
                upload_id: None,
                ..upload
            })
            .await
            .unwrap_err();
        assert_eq!(denied, StorageError::RlsDenied);

        let multipart = AuthorizationRequest {
            auth: alice.clone(),
            bucket: "private".to_owned(),
            object_key: "docs/a.txt".to_owned(),
            action: StorageAction::CreateMultipart,
            content_length: Some(55),
            checksum_sha256: Some("multipart-checksum".to_owned()),
            content_type: Some("text/plain".to_owned()),
            upload_id: None,
            part_number: None,
        };
        let multipart_authorization = adapter.authorize(&multipart).await.unwrap();
        assert_eq!(multipart_authorization.reservation_bytes, 0);
        assert!(multipart_authorization.replacement_fingerprint.is_some());
        assert_ne!(
            multipart_authorization.provider_key,
            overwrite_authorization.provider_key
        );
        let multipart_create_nonce = "server-multipart-create-1";
        let multipart_create_expiry = epoch_ms().unwrap() + 60_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: multipart_create_nonce.to_owned(),
                bytes: multipart_authorization.reservation_bytes,
                expires_at_ms: multipart_create_expiry,
                provider_key: multipart_authorization.provider_key.clone(),
                action: StorageAction::CreateMultipart,
                upload_id: None,
            })
            .await
            .unwrap();
        let provider_upload_id = "provider-upload-overwrite-1";
        let multipart_create_commit = StorageMetadataCommit {
            auth: alice.clone(),
            bucket: multipart.bucket.clone(),
            object_key: multipart.object_key.clone(),
            provider_key: multipart_authorization.provider_key.clone(),
            action: StorageAction::CreateMultipart,
            content_length: multipart.content_length,
            checksum_sha256: multipart.checksum_sha256.clone(),
            content_type: multipart.content_type.clone(),
            upload_id: None,
            part_number: None,
            etag: None,
            version_id: Some(provider_upload_id.to_owned()),
            reservation_nonce: multipart_create_nonce.to_owned(),
            reservation_bytes: multipart_authorization.reservation_bytes,
            reservation_expires_at_ms: multipart_create_expiry,
            replacement_fingerprint: multipart_authorization.replacement_fingerprint.clone(),
        };
        adapter.commit(&multipart_create_commit).await.unwrap();
        adapter.commit(&multipart_create_commit).await.unwrap();

        let multipart_complete = AuthorizationRequest {
            action: StorageAction::CompleteMultipart,
            upload_id: Some(provider_upload_id.to_owned()),
            ..multipart.clone()
        };
        let complete_authorization = adapter.authorize(&multipart_complete).await.unwrap();
        assert_eq!(complete_authorization.reservation_bytes, 15);
        assert_eq!(
            complete_authorization.provider_key,
            multipart_authorization.provider_key
        );
        assert_eq!(
            complete_authorization.replacement_fingerprint,
            multipart_authorization.replacement_fingerprint
        );
        let complete_nonce = "server-multipart-complete-1";
        let complete_expiry = epoch_ms().unwrap() + 60_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: complete_nonce.to_owned(),
                bytes: complete_authorization.reservation_bytes,
                expires_at_ms: complete_expiry,
                provider_key: complete_authorization.provider_key.clone(),
                action: StorageAction::CompleteMultipart,
                upload_id: Some(provider_upload_id.to_owned()),
            })
            .await
            .unwrap();
        let multipart_complete_commit = StorageMetadataCommit {
            auth: alice.clone(),
            bucket: multipart_complete.bucket.clone(),
            object_key: multipart_complete.object_key.clone(),
            provider_key: complete_authorization.provider_key.clone(),
            action: StorageAction::CompleteMultipart,
            content_length: multipart_complete.content_length,
            checksum_sha256: multipart_complete.checksum_sha256.clone(),
            content_type: multipart_complete.content_type.clone(),
            upload_id: Some(provider_upload_id.to_owned()),
            part_number: None,
            etag: Some("multipart-etag".to_owned()),
            version_id: Some("provider-object-version-1".to_owned()),
            reservation_nonce: complete_nonce.to_owned(),
            reservation_bytes: complete_authorization.reservation_bytes,
            reservation_expires_at_ms: complete_expiry,
            replacement_fingerprint: complete_authorization.replacement_fingerprint.clone(),
        };
        let mut cross_upload_commit = multipart_complete_commit.clone();
        cross_upload_commit.upload_id = Some("different-provider-upload".to_owned());
        assert_eq!(
            adapter.commit(&cross_upload_commit).await.unwrap_err(),
            StorageError::Internal
        );
        adapter.commit(&multipart_complete_commit).await.unwrap();
        adapter.commit(&multipart_complete_commit).await.unwrap();
        let mut changed_multipart_replay = multipart_complete_commit.clone();
        changed_multipart_replay.etag = Some("changed-multipart-etag".to_owned());
        assert_eq!(
            adapter.commit(&changed_multipart_replay).await.unwrap_err(),
            StorageError::Internal
        );
        let after_multipart = adapter
            .authorize(&AuthorizationRequest {
                action: StorageAction::Download,
                content_length: None,
                checksum_sha256: None,
                content_type: None,
                upload_id: None,
                ..multipart.clone()
            })
            .await
            .unwrap();
        assert_eq!(
            after_multipart.provider_key,
            multipart_authorization.provider_key
        );
        let multipart_cleanup = adapter.cleanup_claim_sync(epoch_ms().unwrap(), 10).unwrap();
        assert_eq!(multipart_cleanup.items.len(), 1);
        assert_eq!(
            multipart_cleanup.items[0].provider_key.expose(),
            overwrite_authorization.provider_key
        );
        let multipart_cleanup_item = &multipart_cleanup.items[0];
        assert_eq!(
            adapter
                .cleanup_ack_sync(
                    epoch_ms().unwrap(),
                    vec![protocol::StorageCleanupDisposition {
                        id: multipart_cleanup_item.id.clone(),
                        lease_token: multipart_cleanup_item.lease_token.clone(),
                        outcome: protocol::StorageCleanupOutcome::Deleted,
                    }],
                )
                .unwrap(),
            (1, 0)
        );
        adapter
            .with_maintenance(|session| {
                let usage = session.storage_usage(epoch_ms().unwrap()).unwrap();
                assert_eq!(usage.current_bytes, 55);
                assert_eq!(usage.reserved_bytes, 0);
                Ok(())
            })
            .unwrap();

        let delete = AuthorizationRequest {
            auth: alice.clone(),
            bucket: multipart.bucket.clone(),
            object_key: multipart.object_key.clone(),
            action: StorageAction::Delete,
            content_length: None,
            checksum_sha256: None,
            content_type: None,
            upload_id: None,
            part_number: None,
        };
        let delete_authorization = adapter.authorize(&delete).await.unwrap();
        assert_eq!(
            delete_authorization.provider_key,
            multipart_authorization.provider_key
        );
        let delete_nonce = "server-delete-object-1";
        let delete_expiry = epoch_ms().unwrap() + 60_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: delete_nonce.to_owned(),
                bytes: 0,
                expires_at_ms: delete_expiry,
                provider_key: delete_authorization.provider_key.clone(),
                action: StorageAction::Delete,
                upload_id: None,
            })
            .await
            .unwrap();
        adapter
            .commit(&StorageMetadataCommit {
                auth: alice.clone(),
                bucket: delete.bucket.clone(),
                object_key: delete.object_key.clone(),
                provider_key: delete_authorization.provider_key,
                action: StorageAction::Delete,
                content_length: None,
                checksum_sha256: None,
                content_type: None,
                upload_id: None,
                part_number: None,
                etag: None,
                version_id: None,
                reservation_nonce: delete_nonce.to_owned(),
                reservation_bytes: 0,
                reservation_expires_at_ms: delete_expiry,
                replacement_fingerprint: None,
            })
            .await
            .unwrap();
        adapter
            .with_maintenance(|session| {
                let usage = session.storage_usage(epoch_ms().unwrap()).unwrap();
                assert_eq!(usage.current_bytes, 0);
                assert_eq!(usage.reserved_bytes, 0);
                Ok(())
            })
            .unwrap();

        let scoped_nonce = "server-nonce-0000002";
        let scoped_expires_at_ms = epoch_ms().unwrap() + 60_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: scoped_nonce.to_owned(),
                bytes: 1,
                expires_at_ms: scoped_expires_at_ms,
                provider_key: "opaque-release-target".to_owned(),
                action: StorageAction::Delete,
                upload_id: None,
            })
            .await
            .unwrap();
        assert_eq!(
            adapter
                .release_reservation(&bob, scoped_nonce, 1, scoped_expires_at_ms)
                .await
                .unwrap_err(),
            StorageError::Internal
        );
        let duplicate = adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: scoped_nonce.to_owned(),
                bytes: 1,
                expires_at_ms: scoped_expires_at_ms,
                provider_key: "opaque-release-target".to_owned(),
                action: StorageAction::Delete,
                upload_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(duplicate, StorageError::DuplicateReservation);
        adapter
            .release_reservation(&alice, scoped_nonce, 1, scoped_expires_at_ms)
            .await
            .unwrap();
        assert_eq!(
            adapter
                .release_reservation(&alice, scoped_nonce, 1, scoped_expires_at_ms)
                .await
                .unwrap_err(),
            StorageError::Internal
        );

        let replacement_expires_at_ms = scoped_expires_at_ms + 1_000;
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: scoped_nonce.to_owned(),
                bytes: 1,
                expires_at_ms: replacement_expires_at_ms,
                provider_key: "opaque-release-target".to_owned(),
                action: StorageAction::Delete,
                upload_id: None,
            })
            .await
            .unwrap();
        assert_eq!(
            adapter
                .release_reservation(&alice, scoped_nonce, 1, scoped_expires_at_ms)
                .await
                .unwrap_err(),
            StorageError::Internal,
            "an old grant cannot consume a recycled nonce generation"
        );
        adapter
            .release_reservation(&alice, scoped_nonce, 1, replacement_expires_at_ms)
            .await
            .unwrap();

        let abandoned_upload = AuthorizationRequest {
            auth: alice.clone(),
            bucket: "private".to_owned(),
            object_key: "docs/abandoned.txt".to_owned(),
            action: StorageAction::Upload,
            content_length: Some(10),
            checksum_sha256: Some("abandoned".to_owned()),
            content_type: Some("text/plain".to_owned()),
            upload_id: None,
            part_number: None,
        };
        let abandoned_authorization = adapter.authorize(&abandoned_upload).await.unwrap();
        let abandoned_now = epoch_ms().unwrap();
        let abandoned_expiry = abandoned_now.saturating_add(100);
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: "server-abandoned-upload-1".to_owned(),
                bytes: abandoned_authorization.reservation_bytes,
                expires_at_ms: abandoned_expiry,
                provider_key: abandoned_authorization.provider_key.clone(),
                action: StorageAction::Upload,
                upload_id: None,
            })
            .await
            .unwrap();
        let abandoned_commit = StorageMetadataCommit {
            auth: alice.clone(),
            bucket: abandoned_upload.bucket.clone(),
            object_key: abandoned_upload.object_key.clone(),
            provider_key: abandoned_authorization.provider_key.clone(),
            action: StorageAction::Upload,
            content_length: abandoned_upload.content_length,
            checksum_sha256: abandoned_upload.checksum_sha256.clone(),
            content_type: abandoned_upload.content_type.clone(),
            upload_id: None,
            part_number: None,
            etag: Some("abandoned-etag".to_owned()),
            version_id: None,
            reservation_nonce: "server-abandoned-upload-1".to_owned(),
            reservation_bytes: abandoned_authorization.reservation_bytes,
            reservation_expires_at_ms: abandoned_expiry,
            replacement_fingerprint: abandoned_authorization.replacement_fingerprint.clone(),
        };
        let abandoned_create = adapter
            .authorize(&AuthorizationRequest {
                object_key: "docs/create-crash.txt".to_owned(),
                action: StorageAction::CreateMultipart,
                content_length: Some(12),
                checksum_sha256: Some("create-crash".to_owned()),
                ..abandoned_upload.clone()
            })
            .await
            .unwrap();
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: "server-abandoned-create-1".to_owned(),
                bytes: abandoned_create.reservation_bytes,
                expires_at_ms: abandoned_expiry,
                provider_key: abandoned_create.provider_key.clone(),
                action: StorageAction::CreateMultipart,
                upload_id: None,
            })
            .await
            .unwrap();
        let abandoned_batch = adapter.cleanup_claim_sync(abandoned_expiry, 10).unwrap();
        assert_eq!(abandoned_batch.removed_reservations, 2);
        assert_eq!(abandoned_batch.items.len(), 2);
        assert!(abandoned_batch.items.iter().any(|item| {
            item.provider_key.expose() == abandoned_authorization.provider_key
                && item.action == protocol::StorageAction::Upload
        }));
        assert!(abandoned_batch.items.iter().any(|item| {
            item.provider_key.expose() == abandoned_create.provider_key
                && item.action == protocol::StorageAction::CreateMultipart
                && item.upload_id.is_none()
        }));
        assert_eq!(
            adapter.commit(&abandoned_commit).await.unwrap_err(),
            StorageError::Internal,
            "cleanup winning the race removes the exact reservation before metadata commit"
        );
        let abandoned_dispositions = abandoned_batch
            .items
            .into_iter()
            .map(|item| protocol::StorageCleanupDisposition {
                id: item.id,
                lease_token: item.lease_token,
                outcome: protocol::StorageCleanupOutcome::Deleted,
            })
            .collect();
        assert_eq!(
            adapter
                .cleanup_ack_sync(abandoned_expiry, abandoned_dispositions)
                .unwrap(),
            (2, 0)
        );

        let race_winner = AuthorizationRequest {
            object_key: "docs/race-winner.txt".to_owned(),
            checksum_sha256: Some("race-winner".to_owned()),
            ..abandoned_upload.clone()
        };
        let race_authorization = adapter.authorize(&race_winner).await.unwrap();
        let race_expiry = epoch_ms().unwrap().saturating_add(60_000);
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: "server-race-winner-1".to_owned(),
                bytes: race_authorization.reservation_bytes,
                expires_at_ms: race_expiry,
                provider_key: race_authorization.provider_key.clone(),
                action: StorageAction::Upload,
                upload_id: None,
            })
            .await
            .unwrap();
        adapter
            .commit(&StorageMetadataCommit {
                auth: alice.clone(),
                bucket: race_winner.bucket.clone(),
                object_key: race_winner.object_key.clone(),
                provider_key: race_authorization.provider_key.clone(),
                action: StorageAction::Upload,
                content_length: race_winner.content_length,
                checksum_sha256: race_winner.checksum_sha256.clone(),
                content_type: race_winner.content_type.clone(),
                upload_id: None,
                part_number: None,
                etag: Some("race-winner-etag".to_owned()),
                version_id: None,
                reservation_nonce: "server-race-winner-1".to_owned(),
                reservation_bytes: race_authorization.reservation_bytes,
                reservation_expires_at_ms: race_expiry,
                replacement_fingerprint: race_authorization.replacement_fingerprint.clone(),
            })
            .await
            .unwrap();
        assert!(
            adapter
                .cleanup_claim_sync(race_expiry, 10)
                .unwrap()
                .items
                .is_empty()
        );

        let active_create = AuthorizationRequest {
            object_key: "docs/active-abandoned.txt".to_owned(),
            action: StorageAction::CreateMultipart,
            content_length: Some(20),
            checksum_sha256: Some("active-abandoned".to_owned()),
            ..abandoned_upload
        };
        let active_authorization = adapter.authorize(&active_create).await.unwrap();
        let active_expiry = epoch_ms().unwrap().saturating_add(60_000);
        adapter
            .reserve(&StorageReservationRequest {
                auth: alice.clone(),
                nonce: "server-active-create-1".to_owned(),
                bytes: active_authorization.reservation_bytes,
                expires_at_ms: active_expiry,
                provider_key: active_authorization.provider_key.clone(),
                action: StorageAction::CreateMultipart,
                upload_id: None,
            })
            .await
            .unwrap();
        let active_provider_upload_id = "provider-active-abandoned-1";
        adapter
            .commit(&StorageMetadataCommit {
                auth: alice.clone(),
                bucket: active_create.bucket.clone(),
                object_key: active_create.object_key.clone(),
                provider_key: active_authorization.provider_key.clone(),
                action: StorageAction::CreateMultipart,
                content_length: active_create.content_length,
                checksum_sha256: active_create.checksum_sha256.clone(),
                content_type: active_create.content_type.clone(),
                upload_id: None,
                part_number: None,
                etag: None,
                version_id: Some(active_provider_upload_id.to_owned()),
                reservation_nonce: "server-active-create-1".to_owned(),
                reservation_bytes: active_authorization.reservation_bytes,
                reservation_expires_at_ms: active_expiry,
                replacement_fingerprint: active_authorization.replacement_fingerprint.clone(),
            })
            .await
            .unwrap();
        let active_cleanup_at = epoch_ms().unwrap().saturating_add(25 * 60 * 60 * 1_000);
        let active_cleanup = adapter.cleanup_claim_sync(active_cleanup_at, 10).unwrap();
        assert_eq!(active_cleanup.items.len(), 1);
        assert_eq!(
            active_cleanup.items[0].provider_key.expose(),
            active_authorization.provider_key
        );
        assert_eq!(
            active_cleanup.items[0].action,
            protocol::StorageAction::AbortMultipart
        );
        assert_eq!(
            active_cleanup.items[0].upload_id.as_deref(),
            Some(active_provider_upload_id)
        );
        assert_eq!(
            adapter
                .authorize(&AuthorizationRequest {
                    action: StorageAction::CompleteMultipart,
                    upload_id: Some(active_provider_upload_id.to_owned()),
                    ..active_create
                })
                .await
                .unwrap_err(),
            StorageError::RlsDenied
        );
        let mut retry_now = active_cleanup_at;
        let mut retry_item = active_cleanup.items.into_iter().next().unwrap();
        for expected_attempt in 1_u32..=12 {
            assert_eq!(retry_item.attempt, expected_attempt);
            assert_eq!(
                adapter
                    .cleanup_ack_sync(
                        retry_now,
                        vec![protocol::StorageCleanupDisposition {
                            id: retry_item.id.clone(),
                            lease_token: retry_item.lease_token.clone(),
                            outcome: protocol::StorageCleanupOutcome::Retry,
                        }],
                    )
                    .unwrap(),
                (0, 1)
            );
            let exponent = expected_attempt.saturating_sub(1).min(12);
            let backoff = 1_000_i64.saturating_mul(1_i64 << exponent).min(3_600_000);
            retry_now = retry_now.saturating_add(backoff);
            let next = adapter.cleanup_claim_sync(retry_now, 10).unwrap();
            if expected_attempt < 12 {
                assert_eq!(next.items.len(), 1);
                retry_item = next.items.into_iter().next().unwrap();
            } else {
                assert!(
                    next.items.is_empty(),
                    "cleanup retries stop after the bounded maximum attempt"
                );
            }
        }
    }
}
