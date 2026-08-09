//! RLS-gated contracts for S3-compatible object storage.
//!
//! Authorization happens against object metadata in a project SQLite session.
//! The provider adapter accepts only a short-lived, method/key-bound grant signed
//! by trusted service code; it never accepts an unchecked bucket or key.

use std::{collections::HashMap, net::IpAddr, sync::Mutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ffdb_protocol::AuthContext;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use url::Url;
use zeroize::{Zeroize, Zeroizing};

mod s3;

pub use s3::{S3Provider, S3ProviderConfig};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAction {
    Upload,
    Download,
    Delete,
    List,
    CreateMultipart,
    UploadPart,
    CompleteMultipart,
    AbortMultipart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    /// Full context produced by verified project JWT middleware. Metadata RLS
    /// adapters must pass the claims through to the immutable SQLite session so
    /// `auth.jwt()` and `auth.claim()` have identical semantics to SQL queries.
    pub auth: AuthContext,
    pub bucket: String,
    pub object_key: String,
    pub action: StorageAction,
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataAuthorization {
    /// Provider-side key resolved from protected metadata, never caller input.
    pub provider_key: String,
    pub scope_fingerprint: String,
    pub project_quota_bytes: u64,
    /// Committed object bytes only. The gateway adds its in-process pending
    /// reservations; the durable authorizer remains the cross-process quota gate.
    pub current_project_bytes: u64,
    pub max_object_bytes: u64,
    /// Net quota growth after replacing an existing logical object.
    pub reservation_bytes: u64,
    /// Opaque binding to the object state observed during authorization.
    pub replacement_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReservationRequest {
    pub auth: AuthContext,
    pub nonce: String,
    pub bytes: u64,
    pub expires_at_ms: i64,
    pub provider_key: String,
    pub action: StorageAction,
    pub upload_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMetadataCommit {
    pub auth: AuthContext,
    pub bucket: String,
    pub object_key: String,
    pub provider_key: String,
    pub action: StorageAction,
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
    pub etag: Option<String>,
    pub version_id: Option<String>,
    pub reservation_nonce: String,
    pub reservation_bytes: u64,
    pub reservation_expires_at_ms: i64,
    pub replacement_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReceiptRequest {
    pub auth: AuthContext,
    pub bucket: String,
    pub object_key: String,
    pub provider_key: String,
    pub action: StorageAction,
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
    pub reservation_nonce: String,
    pub reservation_bytes: u64,
    pub reservation_expires_at_ms: i64,
    pub replacement_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageCommitResult {
    pub content_length: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub etag: Option<String>,
    pub version_id: Option<String>,
}

#[async_trait]
pub trait MetadataAuthorizer: Send + Sync {
    /// Evaluate the matching metadata read/write using the project's RLS session.
    async fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<MetadataAuthorization, StorageError>;

    /// Atomically re-check quota and persist a cross-process reservation in
    /// project SQLite. This is authoritative; the gateway's bounded in-memory
    /// counter is only a local admission-control layer.
    async fn reserve(&self, request: &StorageReservationRequest) -> Result<(), StorageError>;

    /// Commit object/upload/version metadata through the authenticated RLS
    /// session after the provider operation succeeds.
    async fn commit(&self, request: &StorageMetadataCommit) -> Result<(), StorageError>;

    /// Returns a prior exact commit result for replay, or `None` when this
    /// grant has not committed. Implementations fail closed on binding drift.
    async fn receipt(
        &self,
        request: &StorageReceiptRequest,
    ) -> Result<Option<StorageCommitResult>, StorageError>;

    /// Release one durable reservation after completion, failure, or abort.
    async fn release_reservation(
        &self,
        auth: &AuthContext,
        nonce: &str,
        reservation_bytes: u64,
        reservation_expires_at_ms: i64,
    ) -> Result<(), StorageError>;

    /// Trusted maintenance hook for abandoned durable reservations.
    async fn cleanup_expired_reservations(&self, now_ms: i64) -> Result<usize, StorageError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GrantClaims {
    project_id: String,
    subject: String,
    token_id: String,
    bucket: String,
    object_key: String,
    provider_key: String,
    action: StorageAction,
    max_bytes: Option<u64>,
    checksum_sha256: Option<String>,
    content_type: Option<String>,
    upload_id: Option<String>,
    part_number: Option<u32>,
    scope_fingerprint: String,
    issued_at_ms: i64,
    expires_at_ms: i64,
    nonce: String,
    reservation_bytes: u64,
    reservation_expires_at_ms: i64,
    replacement_fingerprint: Option<String>,
}

/// Opaque, authenticated proof that a single provider operation passed RLS.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationToken(String);

impl AuthorizationToken {
    pub fn parse(value: impl Into<String>) -> Result<Self, StorageError> {
        let value = value.into();
        if value.is_empty() || value.len() > 8_192 || value.chars().any(char::is_control) {
            return Err(StorageError::InvalidGrant);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AuthorizationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthorizationToken([REDACTED])")
    }
}

impl Drop for AuthorizationToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone)]
pub struct GrantCodec {
    secret: Zeroizing<Vec<u8>>,
    max_ttl_ms: i64,
}

impl std::fmt::Debug for GrantCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrantCodec")
            .field("secret", &"[REDACTED]")
            .field("max_ttl_ms", &self.max_ttl_ms)
            .finish()
    }
}

impl GrantCodec {
    pub fn new(secret: impl AsRef<[u8]>, max_ttl_ms: i64) -> Result<Self, StorageError> {
        let secret = secret.as_ref();
        if secret.len() < 32 || max_ttl_ms <= 0 {
            return Err(StorageError::InvalidConfiguration);
        }
        Ok(Self {
            secret: Zeroizing::new(secret.to_vec()),
            max_ttl_ms,
        })
    }

    fn issue(&self, claims: &GrantClaims) -> Result<AuthorizationToken, StorageError> {
        let payload = serde_json::to_vec(claims).map_err(|_| StorageError::InvalidGrant)?;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(self.secret.as_slice())
            .map_err(|_| StorageError::InvalidConfiguration)?;
        mac.update(&payload);
        let signature = mac.finalize().into_bytes();
        Ok(AuthorizationToken(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(signature)
        )))
    }

    fn verify(&self, token: &AuthorizationToken, now_ms: i64) -> Result<GrantClaims, StorageError> {
        let claims = self.verify_authenticated(token)?;
        if now_ms < claims.issued_at_ms
            || now_ms >= claims.expires_at_ms
            || claims.expires_at_ms - claims.issued_at_ms > self.max_ttl_ms
        {
            return Err(StorageError::ExpiredGrant);
        }
        Ok(claims)
    }

    fn verify_authenticated(
        &self,
        token: &AuthorizationToken,
    ) -> Result<GrantClaims, StorageError> {
        const MAX_TOKEN_BYTES: usize = 8_192;
        const MAX_PAYLOAD_ENCODED_BYTES: usize = 7_000;
        const MAX_PAYLOAD_BYTES: usize = 5_120;
        const SIGNATURE_ENCODED_BYTES: usize = 43;
        if token.0.len() > MAX_TOKEN_BYTES {
            return Err(StorageError::InvalidGrant);
        }
        let (payload, signature) = token.0.split_once('.').ok_or(StorageError::InvalidGrant)?;
        if payload.len() > MAX_PAYLOAD_ENCODED_BYTES || signature.len() != SIGNATURE_ENCODED_BYTES {
            return Err(StorageError::InvalidGrant);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| StorageError::InvalidGrant)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(StorageError::InvalidGrant);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| StorageError::InvalidGrant)?;
        if signature.len() != 32 {
            return Err(StorageError::InvalidGrant);
        }
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(self.secret.as_slice())
            .map_err(|_| StorageError::InvalidConfiguration)?;
        mac.update(&payload);
        mac.verify_slice(&signature)
            .map_err(|_| StorageError::InvalidGrant)?;
        let claims: GrantClaims =
            serde_json::from_slice(&payload).map_err(|_| StorageError::InvalidGrant)?;
        Ok(claims)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SignedObjectRequest {
    pub url: Url,
    pub method: String,
    pub expires_at_ms: i64,
    pub required_headers: Vec<(String, String)>,
}

impl std::fmt::Debug for SignedObjectRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignedObjectRequest")
            .field("url", &"[REDACTED]")
            .field("method", &self.method)
            .field("expires_at_ms", &self.expires_at_ms)
            .field(
                "required_header_names",
                &self
                    .required_headers
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOperation {
    pub action: StorageAction,
    pub bucket: String,
    pub provider_key: String,
    pub max_bytes: Option<u64>,
    pub checksum_sha256: Option<String>,
    pub content_type: Option<String>,
    pub upload_id: Option<String>,
    pub part_number: Option<u32>,
}

#[async_trait]
pub trait S3Presigner: Send + Sync {
    async fn presign(
        &self,
        operation: &ProviderOperation,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<SignedObjectRequest, StorageError>;
}

#[async_trait]
pub trait ObjectProvider: S3Presigner {
    /// Verify provider state after a client performed a signed upload or delete.
    /// Implementations must derive metadata from the provider, never the client.
    async fn verify_commit(
        &self,
        operation: &ProviderOperation,
        now_ms: i64,
    ) -> Result<StorageCommitResult, StorageError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLimits {
    pub signed_url_ttl_ms: i64,
    pub grant_ttl_ms: i64,
    pub max_pending_reservations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reservation {
    project_id: String,
    bytes: u64,
    expires_at_ms: i64,
}

#[derive(Debug, Default)]
struct Reservations {
    by_nonce: HashMap<String, Reservation>,
    by_project: HashMap<String, u64>,
}

/// Coordinates an RLS metadata decision, quota reservation, and S3 presign.
pub struct StorageGateway<A, P> {
    authorizer: A,
    presigner: P,
    codec: GrantCodec,
    limits: StorageLimits,
    reservations: Mutex<Reservations>,
}

impl<A: std::fmt::Debug, P: std::fmt::Debug> std::fmt::Debug for StorageGateway<A, P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageGateway")
            .field("authorizer", &self.authorizer)
            .field("presigner", &self.presigner)
            .field("codec", &self.codec)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl<A, P> StorageGateway<A, P>
where
    A: MetadataAuthorizer,
    P: S3Presigner,
{
    pub fn new(
        authorizer: A,
        presigner: P,
        secret: impl AsRef<[u8]>,
        limits: StorageLimits,
    ) -> Result<Self, StorageError> {
        if limits.signed_url_ttl_ms <= 0
            || limits.grant_ttl_ms <= 0
            || limits.max_pending_reservations == 0
        {
            return Err(StorageError::InvalidConfiguration);
        }
        Ok(Self {
            authorizer,
            presigner,
            codec: GrantCodec::new(secret, limits.grant_ttl_ms)?,
            limits,
            reservations: Mutex::new(Reservations::default()),
        })
    }

    pub async fn authorize(
        &self,
        request: &AuthorizationRequest,
        now_ms: i64,
    ) -> Result<AuthorizationToken, StorageError> {
        let mut nonce_bytes = [0_u8; 24];
        getrandom::fill(&mut nonce_bytes).map_err(|_| StorageError::Internal)?;
        self.authorize_with_nonce(request, now_ms, URL_SAFE_NO_PAD.encode(nonce_bytes))
            .await
    }

    async fn authorize_with_nonce(
        &self,
        request: &AuthorizationRequest,
        now_ms: i64,
        nonce: String,
    ) -> Result<AuthorizationToken, StorageError> {
        validate_request(request)?;
        if nonce.len() < 16 || nonce.len() > 128 {
            return Err(StorageError::InvalidGrant);
        }
        let authorization = self.authorizer.authorize(request).await?;
        validate_provider_key(&authorization.provider_key)?;
        if authorization.scope_fingerprint.is_empty() || authorization.scope_fingerprint.len() > 256
        {
            return Err(StorageError::InvalidAuthorizationDecision);
        }

        let object_bytes = request.content_length.unwrap_or(0);
        let bytes = authorization.reservation_bytes;
        if matches!(
            request.action,
            StorageAction::Upload
                | StorageAction::CreateMultipart
                | StorageAction::UploadPart
                | StorageAction::CompleteMultipart
        ) && (request.content_length.is_none() || object_bytes > authorization.max_object_bytes)
        {
            return Err(StorageError::ObjectQuotaExceeded);
        }
        if bytes > 0 {
            {
                let mut reservations = self
                    .reservations
                    .lock()
                    .map_err(|_| StorageError::Internal)?;
                sweep_expired(&mut reservations, now_ms);
                if reservations.by_nonce.contains_key(&nonce) {
                    return Err(StorageError::DuplicateReservation);
                }
                if reservations.by_nonce.len() >= self.limits.max_pending_reservations {
                    return Err(StorageError::TooManyReservations);
                }
                let reserved = reservations
                    .by_project
                    .get(&request.auth.project_id.to_string())
                    .copied()
                    .unwrap_or(0);
                if authorization
                    .current_project_bytes
                    .saturating_add(reserved)
                    .saturating_add(bytes)
                    > authorization.project_quota_bytes
                {
                    return Err(StorageError::ProjectQuotaExceeded);
                }
                reservations.by_nonce.insert(
                    nonce.clone(),
                    Reservation {
                        project_id: request.auth.project_id.to_string(),
                        bytes,
                        expires_at_ms: now_ms.saturating_add(self.limits.grant_ttl_ms),
                    },
                );
                reservations.by_project.insert(
                    request.auth.project_id.to_string(),
                    reserved.saturating_add(bytes),
                );
            }
        }
        if requires_durable_reservation(request.action)
            && let Err(error) = self
                .authorizer
                .reserve(&StorageReservationRequest {
                    auth: request.auth.clone(),
                    nonce: nonce.clone(),
                    bytes,
                    expires_at_ms: now_ms.saturating_add(self.limits.grant_ttl_ms),
                    provider_key: authorization.provider_key.clone(),
                    action: request.action,
                    upload_id: request.upload_id.clone(),
                })
                .await
        {
            if bytes > 0 {
                self.release_local_reservation(
                    &request.auth.project_id.to_string(),
                    &nonce,
                    bytes,
                    now_ms.saturating_add(self.limits.grant_ttl_ms),
                    now_ms,
                )?;
            }
            return Err(error);
        }

        self.codec.issue(&GrantClaims {
            project_id: request.auth.project_id.to_string(),
            subject: request.auth.subject.to_string(),
            token_id: request.auth.token_id.to_string(),
            bucket: request.bucket.clone(),
            object_key: request.object_key.clone(),
            provider_key: authorization.provider_key,
            action: request.action,
            max_bytes: request.content_length,
            checksum_sha256: request.checksum_sha256.clone(),
            content_type: request.content_type.clone(),
            upload_id: request.upload_id.clone(),
            part_number: request.part_number,
            scope_fingerprint: authorization.scope_fingerprint,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(self.limits.grant_ttl_ms),
            nonce,
            reservation_bytes: bytes,
            reservation_expires_at_ms: now_ms.saturating_add(self.limits.grant_ttl_ms),
            replacement_fingerprint: authorization.replacement_fingerprint,
        })
    }

    pub async fn presign(
        &self,
        token: &AuthorizationToken,
        requested_ttl_ms: i64,
        now_ms: i64,
    ) -> Result<SignedObjectRequest, StorageError> {
        let claims = self.codec.verify(token, now_ms)?;
        if requested_ttl_ms <= 0 || requested_ttl_ms > self.limits.signed_url_ttl_ms {
            return Err(StorageError::InvalidTtl);
        }
        let grant_remaining = claims.expires_at_ms.saturating_sub(now_ms);
        let ttl = requested_ttl_ms.min(grant_remaining);
        self.presigner
            .presign(
                &ProviderOperation {
                    action: claims.action,
                    bucket: claims.bucket,
                    provider_key: claims.provider_key,
                    max_bytes: claims.max_bytes,
                    checksum_sha256: claims.checksum_sha256,
                    content_type: claims.content_type,
                    upload_id: claims.upload_id,
                    part_number: claims.part_number,
                },
                ttl,
                now_ms,
            )
            .await
    }

    /// Release a reservation after metadata commit, failed upload, or expiry.
    pub async fn release_reservation(
        &self,
        token: &AuthorizationToken,
        auth: &AuthContext,
        now_ms: i64,
    ) -> Result<u64, StorageError> {
        let claims = self.codec.verify_authenticated(token)?;
        if claims.project_id != auth.project_id.to_string()
            || claims.subject != auth.subject.to_string()
            || claims.token_id != auth.token_id.to_string()
        {
            return Err(StorageError::InvalidGrant);
        }
        self.validate_local_reservation(&claims, now_ms)?;
        if requires_durable_reservation(claims.action) {
            self.authorizer
                .release_reservation(
                    auth,
                    &claims.nonce,
                    claims.reservation_bytes,
                    claims.reservation_expires_at_ms,
                )
                .await?;
        }
        self.release_local_reservation(
            &claims.project_id,
            &claims.nonce,
            claims.reservation_bytes,
            claims.reservation_expires_at_ms,
            now_ms,
        )
    }

    fn validate_local_reservation(
        &self,
        claims: &GrantClaims,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        if claims.reservation_bytes == 0 {
            return Ok(());
        }
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| StorageError::Internal)?;
        sweep_expired(&mut reservations, now_ms);
        if let Some(reservation) = reservations.by_nonce.get(&claims.nonce)
            && (reservation.project_id != claims.project_id
                || reservation.bytes != claims.reservation_bytes
                || reservation.expires_at_ms != claims.reservation_expires_at_ms)
        {
            return Err(StorageError::InvalidGrant);
        }
        Ok(())
    }

    fn release_local_reservation(
        &self,
        project_id: &str,
        nonce: &str,
        expected_bytes: u64,
        expected_expires_at_ms: i64,
        now_ms: i64,
    ) -> Result<u64, StorageError> {
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| StorageError::Internal)?;
        sweep_expired(&mut reservations, now_ms);
        let Some(reservation) = reservations.by_nonce.get(nonce) else {
            return Ok(0);
        };
        if reservation.project_id != project_id
            || reservation.bytes != expected_bytes
            || reservation.expires_at_ms != expected_expires_at_ms
        {
            return Err(StorageError::InvalidGrant);
        }
        let released = reservation.bytes;
        reservations.by_nonce.remove(nonce);
        let project = reservations
            .by_project
            .get(project_id)
            .copied()
            .unwrap_or(0);
        let remaining = project.saturating_sub(released);
        if remaining == 0 {
            reservations.by_project.remove(project_id);
        } else {
            reservations
                .by_project
                .insert(project_id.to_owned(), remaining);
        }
        Ok(released)
    }

    /// Re-authorize and atomically persist provider results in project SQLite.
    /// The caller supplies a freshly verified auth context; grant identity,
    /// size, and checksum bindings are checked before the metadata transaction.
    pub async fn commit_metadata(
        &self,
        token: &AuthorizationToken,
        auth: AuthContext,
        provider_result: StorageCommitResult,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let claims = self.codec.verify(token, now_ms)?;
        if claims.project_id != auth.project_id.to_string()
            || claims.subject != auth.subject.to_string()
            || claims.token_id != auth.token_id.to_string()
            || provider_result
                .content_length
                .zip(claims.max_bytes)
                .is_some_and(|(actual, maximum)| actual > maximum)
            || provider_result
                .checksum_sha256
                .as_ref()
                .zip(claims.checksum_sha256.as_ref())
                .is_some_and(|(actual, expected)| actual != expected)
        {
            return Err(StorageError::InvalidGrant);
        }
        if let Some(committed) = self
            .authorizer
            .receipt(&receipt_request(&claims, auth.clone()))
            .await?
        {
            if committed != provider_result {
                return Err(StorageError::InvalidGrant);
            }
            self.release_local_reservation(
                &claims.project_id,
                &claims.nonce,
                claims.reservation_bytes,
                claims.reservation_expires_at_ms,
                now_ms,
            )?;
            return Ok(());
        }
        self.validate_local_reservation(&claims, now_ms)?;
        self.authorizer
            .commit(&StorageMetadataCommit {
                auth: auth.clone(),
                bucket: claims.bucket,
                object_key: claims.object_key,
                provider_key: claims.provider_key,
                action: claims.action,
                content_length: provider_result.content_length.or(claims.max_bytes),
                checksum_sha256: provider_result.checksum_sha256.or(claims.checksum_sha256),
                content_type: claims.content_type,
                upload_id: claims.upload_id,
                part_number: claims.part_number,
                etag: provider_result.etag,
                version_id: provider_result.version_id,
                reservation_nonce: claims.nonce.clone(),
                reservation_bytes: claims.reservation_bytes,
                reservation_expires_at_ms: claims.reservation_expires_at_ms,
                replacement_fingerprint: claims.replacement_fingerprint,
            })
            .await?;
        self.release_local_reservation(
            &claims.project_id,
            &claims.nonce,
            claims.reservation_bytes,
            claims.reservation_expires_at_ms,
            now_ms,
        )?;
        Ok(())
    }

    async fn prior_receipt(
        &self,
        token: &AuthorizationToken,
        auth: AuthContext,
        expected_action: StorageAction,
        now_ms: i64,
    ) -> Result<Option<StorageCommitResult>, StorageError> {
        let claims = self.codec.verify(token, now_ms)?;
        if claims.project_id != auth.project_id.to_string()
            || claims.subject != auth.subject.to_string()
            || claims.token_id != auth.token_id.to_string()
            || claims.action != expected_action
        {
            return Err(StorageError::InvalidGrant);
        }
        self.authorizer
            .receipt(&receipt_request(&claims, auth))
            .await
    }

    /// Verify the provider result server-side before committing RLS metadata.
    /// This prevents callers from inventing an object size, checksum, or delete
    /// result after receiving a signed URL.
    pub async fn verify_provider_and_commit(
        &self,
        token: &AuthorizationToken,
        auth: AuthContext,
        now_ms: i64,
    ) -> Result<StorageCommitResult, StorageError>
    where
        P: ObjectProvider,
    {
        let claims = self.codec.verify(token, now_ms)?;
        if claims.project_id != auth.project_id.to_string()
            || claims.subject != auth.subject.to_string()
            || claims.token_id != auth.token_id.to_string()
        {
            return Err(StorageError::InvalidGrant);
        }
        let result = self
            .presigner
            .verify_commit(
                &ProviderOperation {
                    action: claims.action,
                    bucket: claims.bucket,
                    provider_key: claims.provider_key,
                    max_bytes: claims.max_bytes,
                    checksum_sha256: claims.checksum_sha256,
                    content_type: claims.content_type,
                    upload_id: claims.upload_id,
                    part_number: claims.part_number,
                },
                now_ms,
            )
            .await?;
        self.commit_metadata(token, auth, result.clone(), now_ms)
            .await?;
        Ok(result)
    }

    /// Persists multipart staging results that S3 does not expose through HEAD.
    /// The caller must invoke this only after a successful same-origin provider
    /// response. Grants remain identity/action/key/size bound; final completion
    /// is independently verified through provider HEAD before object commit.
    pub async fn commit_multipart_stage(
        &self,
        token: &AuthorizationToken,
        auth: AuthContext,
        expected_action: StorageAction,
        provider_result: StorageCommitResult,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let claims = self.codec.verify(token, now_ms)?;
        if claims.action != expected_action
            || !matches!(
                expected_action,
                StorageAction::CreateMultipart
                    | StorageAction::UploadPart
                    | StorageAction::AbortMultipart
            )
            || matches!(expected_action, StorageAction::CreateMultipart)
                && provider_result.version_id.as_ref().is_none_or(|upload_id| {
                    upload_id.is_empty()
                        || upload_id.len() > 256
                        || upload_id.chars().any(char::is_control)
                })
            || matches!(expected_action, StorageAction::UploadPart)
                && provider_result
                    .etag
                    .as_ref()
                    .is_none_or(|etag| etag.is_empty() || etag.len() > 256)
        {
            return Err(StorageError::InvalidMultipartRequest);
        }
        self.commit_metadata(token, auth, provider_result, now_ms)
            .await
    }

    /// Trusted maintenance path independent of the lifetime of a bearer grant.
    pub async fn cleanup_expired_reservations(&self, now_ms: i64) -> Result<usize, StorageError> {
        let durable = self.authorizer.cleanup_expired_reservations(now_ms).await?;
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| StorageError::Internal)?;
        Ok(durable.saturating_add(sweep_expired(&mut reservations, now_ms)))
    }

    /// Sweeps only the process-local cache. Durable cleanup is dispatched by a
    /// separately authenticated maintenance principal at the API boundary.
    pub fn cleanup_local_reservations(&self, now_ms: i64) -> Result<usize, StorageError> {
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| StorageError::Internal)?;
        Ok(sweep_expired(&mut reservations, now_ms))
    }
}

impl<A> StorageGateway<A, S3Provider>
where
    A: MetadataAuthorizer,
{
    /// Executes the S3-generated multipart-create control operation through the
    /// pinned internal endpoint and durably records its upload ID before the
    /// caller receives it. An exact replay returns the stored provider result
    /// without creating another multipart upload.
    pub async fn initiate_multipart_and_commit(
        &self,
        token: &AuthorizationToken,
        auth: AuthContext,
        now_ms: i64,
    ) -> Result<String, StorageError> {
        if let Some(receipt) = self
            .prior_receipt(token, auth.clone(), StorageAction::CreateMultipart, now_ms)
            .await?
        {
            return valid_upload_id(receipt.version_id);
        }
        let claims = self.codec.verify(token, now_ms)?;
        let upload_id = match self
            .presigner
            .recover_multipart_for_key_internal(&claims.provider_key, now_ms)
            .await?
        {
            Some(upload_id) => upload_id,
            None => {
                self.presigner
                    .initiate_multipart_internal(
                        &claims.provider_key,
                        claims.content_type.as_deref(),
                        claims.checksum_sha256.is_some(),
                        now_ms,
                    )
                    .await?
            }
        };
        let result = StorageCommitResult {
            version_id: Some(upload_id.clone()),
            ..StorageCommitResult::default()
        };
        if let Err(error) = self
            .commit_multipart_stage(token, auth, StorageAction::CreateMultipart, result, now_ms)
            .await
        {
            let _ignored = self
                .presigner
                .abort_multipart_internal(&claims.provider_key, &upload_id, now_ms)
                .await;
            return Err(error);
        }
        Ok(upload_id)
    }
}

fn valid_upload_id(value: Option<String>) -> Result<String, StorageError> {
    value
        .filter(|upload_id| {
            !upload_id.is_empty()
                && upload_id.len() <= 256
                && !upload_id.chars().any(char::is_control)
        })
        .ok_or(StorageError::InvalidMultipartRequest)
}

fn sweep_expired(reservations: &mut Reservations, now_ms: i64) -> usize {
    let expired: Vec<String> = reservations
        .by_nonce
        .iter()
        .filter(|(_, reservation)| reservation.expires_at_ms <= now_ms)
        .map(|(nonce, _)| nonce.clone())
        .collect();
    for nonce in &expired {
        if let Some(reservation) = reservations.by_nonce.remove(nonce) {
            let remaining = reservations
                .by_project
                .get(&reservation.project_id)
                .copied()
                .unwrap_or(0)
                .saturating_sub(reservation.bytes);
            if remaining == 0 {
                reservations.by_project.remove(&reservation.project_id);
            } else {
                reservations
                    .by_project
                    .insert(reservation.project_id, remaining);
            }
        }
    }
    expired.len()
}

fn receipt_request(claims: &GrantClaims, auth: AuthContext) -> StorageReceiptRequest {
    StorageReceiptRequest {
        auth,
        bucket: claims.bucket.clone(),
        object_key: claims.object_key.clone(),
        provider_key: claims.provider_key.clone(),
        action: claims.action,
        content_length: claims.max_bytes,
        checksum_sha256: claims.checksum_sha256.clone(),
        content_type: claims.content_type.clone(),
        upload_id: claims.upload_id.clone(),
        part_number: claims.part_number,
        reservation_nonce: claims.nonce.clone(),
        reservation_bytes: claims.reservation_bytes,
        reservation_expires_at_ms: claims.reservation_expires_at_ms,
        replacement_fingerprint: claims.replacement_fingerprint.clone(),
    }
}

const fn requires_durable_reservation(action: StorageAction) -> bool {
    !matches!(action, StorageAction::Download | StorageAction::List)
}

fn validate_request(request: &AuthorizationRequest) -> Result<(), StorageError> {
    let claims_bytes =
        serde_json::to_vec(&request.auth.claims).map_err(|_| StorageError::InvalidObjectKey)?;
    if request.auth.role.is_empty()
        || request.auth.role.len() > 128
        || request.auth.claims.len() > 128
        || claims_bytes.len() > 16_384
        || request.bucket.is_empty()
        || request.object_key.is_empty()
        || request.bucket.len() > 63
        || request.object_key.len() > 1024
        || request.object_key.starts_with('/')
        || request.object_key.contains('\0')
        || request.content_type.as_ref().is_some_and(|value| {
            value.is_empty() || value.len() > 255 || value.chars().any(char::is_control)
        })
        || request.checksum_sha256.as_ref().is_some_and(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_whitespace)
        })
        || request.upload_id.as_ref().is_some_and(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
        })
        || request
            .object_key
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(StorageError::InvalidObjectKey);
    }
    if matches!(request.action, StorageAction::UploadPart)
        && (request.upload_id.is_none()
            || request.content_length.is_none()
            || request.part_number.is_none())
    {
        return Err(StorageError::InvalidMultipartRequest);
    }
    if matches!(
        request.action,
        StorageAction::CompleteMultipart | StorageAction::AbortMultipart
    ) && request.upload_id.is_none()
    {
        return Err(StorageError::InvalidMultipartRequest);
    }
    if request
        .part_number
        .is_some_and(|part_number| !(1..=10_000).contains(&part_number))
        || request.part_number.is_some() && !matches!(request.action, StorageAction::UploadPart)
    {
        return Err(StorageError::InvalidMultipartRequest);
    }
    Ok(())
}

fn validate_provider_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty()
        || key.len() > 1024
        || key.starts_with('/')
        || key.contains('\0')
        || key
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(StorageError::InvalidProviderKey);
    }
    Ok(())
}

/// Validates an S3 endpoint after DNS resolution. Provider clients must also
/// disable redirects and pin the validated addresses to prevent DNS rebinding.
pub fn validate_s3_endpoint(
    endpoint: &Url,
    allowed_hosts: &[String],
    resolved_addresses: &[IpAddr],
    allow_insecure_localhost: bool,
    insecure_development_service_host: Option<&str>,
    private_network_service_host: Option<&str>,
) -> Result<(), StorageError> {
    let host = endpoint
        .host_str()
        .ok_or(StorageError::UnsafeProviderEndpoint)?;
    let local_host = matches!(host, "localhost" | "127.0.0.1" | "::1");
    let development_service = insecure_development_service_host == Some(host);
    let private_network_service =
        private_network_service_host == Some(host) && endpoint.scheme() == "https";
    if endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !(allowed_hosts.iter().any(|allowed| allowed == host)
            || allow_insecure_localhost && local_host)
        || (endpoint.scheme() != "https"
            && !(allow_insecure_localhost && local_host || development_service))
        || resolved_addresses.is_empty()
        || resolved_addresses.iter().any(|address| {
            !(is_public_ip(*address)
                || allow_insecure_localhost && local_host && address.is_loopback()
                || development_service && is_private_or_loopback(*address)
                || private_network_service && is_private_network(*address))
        })
    {
        return Err(StorageError::UnsafeProviderEndpoint);
    }
    Ok(())
}

fn is_private_network(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => ip.is_private(),
        IpAddr::V6(ip) => ip.is_unique_local(),
    }
}

fn is_private_or_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
    }
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.octets()[0] == 0)
        }
        IpAddr::V6(ip) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageError {
    #[error("storage metadata policy denied the operation")]
    RlsDenied,
    #[error("object key is invalid")]
    InvalidObjectKey,
    #[error("trusted provider key is invalid")]
    InvalidProviderKey,
    #[error("multipart request is invalid")]
    InvalidMultipartRequest,
    #[error("authorization grant is invalid")]
    InvalidGrant,
    #[error("authorization grant expired")]
    ExpiredGrant,
    #[error("object exceeds its configured size limit")]
    ObjectQuotaExceeded,
    #[error("project storage quota would be exceeded")]
    ProjectQuotaExceeded,
    #[error("organization storage allowance would be exceeded")]
    OrganizationQuotaExceeded,
    #[error("reservation identifier was already used")]
    DuplicateReservation,
    #[error("signed URL lifetime is invalid")]
    InvalidTtl,
    #[error("provider operation failed")]
    Provider,
    #[error("provider object metadata does not match the authorized operation")]
    ProviderMetadataMismatch,
    #[error("storage configuration is invalid")]
    InvalidConfiguration,
    #[error("storage authorization decision is invalid")]
    InvalidAuthorizationDecision,
    #[error("too many pending storage reservations")]
    TooManyReservations,
    #[error("object provider endpoint is unsafe")]
    UnsafeProviderEndpoint,
    #[error("storage state is unavailable")]
    Internal,
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[derive(Debug)]
    struct AllowMetadata;

    #[async_trait]
    impl MetadataAuthorizer for AllowMetadata {
        async fn authorize(
            &self,
            request: &AuthorizationRequest,
        ) -> Result<MetadataAuthorization, StorageError> {
            Ok(MetadataAuthorization {
                provider_key: format!(
                    "projects/{}/{}",
                    request.auth.project_id, request.object_key
                ),
                scope_fingerprint: "scope:v1".to_owned(),
                project_quota_bytes: 100,
                current_project_bytes: 30,
                max_object_bytes: 64,
                reservation_bytes: request.content_length.unwrap_or(0),
                replacement_fingerprint: None,
            })
        }

        async fn reserve(&self, _request: &StorageReservationRequest) -> Result<(), StorageError> {
            Ok(())
        }

        async fn commit(&self, _request: &StorageMetadataCommit) -> Result<(), StorageError> {
            Ok(())
        }

        async fn receipt(
            &self,
            _request: &StorageReceiptRequest,
        ) -> Result<Option<StorageCommitResult>, StorageError> {
            Ok(None)
        }

        async fn release_reservation(
            &self,
            _auth: &AuthContext,
            _nonce: &str,
            _reservation_bytes: u64,
            _reservation_expires_at_ms: i64,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn cleanup_expired_reservations(&self, _now_ms: i64) -> Result<usize, StorageError> {
            Ok(0)
        }
    }

    #[derive(Debug)]
    struct CapturingPresigner;

    #[derive(Debug)]
    struct CountingMetadata {
        reserves: Arc<AtomicUsize>,
        commits: Arc<AtomicUsize>,
        releases: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MetadataAuthorizer for CountingMetadata {
        async fn authorize(
            &self,
            request: &AuthorizationRequest,
        ) -> Result<MetadataAuthorization, StorageError> {
            AllowMetadata.authorize(request).await
        }

        async fn reserve(&self, _request: &StorageReservationRequest) -> Result<(), StorageError> {
            self.reserves.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn commit(&self, _request: &StorageMetadataCommit) -> Result<(), StorageError> {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn receipt(
            &self,
            _request: &StorageReceiptRequest,
        ) -> Result<Option<StorageCommitResult>, StorageError> {
            Ok(None)
        }

        async fn release_reservation(
            &self,
            _auth: &AuthContext,
            _nonce: &str,
            _reservation_bytes: u64,
            _reservation_expires_at_ms: i64,
        ) -> Result<(), StorageError> {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn cleanup_expired_reservations(&self, _now_ms: i64) -> Result<usize, StorageError> {
            Ok(0)
        }
    }

    #[async_trait]
    impl S3Presigner for CapturingPresigner {
        async fn presign(
            &self,
            operation: &ProviderOperation,
            ttl_ms: i64,
            _now_ms: i64,
        ) -> Result<SignedObjectRequest, StorageError> {
            let method = match operation.action {
                StorageAction::Download | StorageAction::List => "GET",
                StorageAction::Delete | StorageAction::AbortMultipart => "DELETE",
                _ => "PUT",
            };
            let url = Url::parse(&format!(
                "https://objects.example.test/{}/{}?ttl={ttl_ms}",
                operation.bucket, operation.provider_key
            ))
            .map_err(|_| StorageError::Provider)?;
            Ok(SignedObjectRequest {
                url,
                method: method.to_owned(),
                expires_at_ms: ttl_ms,
                required_headers: Vec::new(),
            })
        }
    }

    fn request(bytes: u64) -> AuthorizationRequest {
        AuthorizationRequest {
            auth: AuthContext {
                project_id: ffdb_protocol::ProjectId::new(),
                subject: ffdb_protocol::UserId::new(),
                role: "authenticated".to_owned(),
                claims: serde_json::Map::from_iter([(
                    "organization_id".to_owned(),
                    serde_json::json!("organization-1"),
                )]),
                token_id: ffdb_protocol::TokenId::new(),
            },
            bucket: "documents".to_owned(),
            object_key: "user-1/report.pdf".to_owned(),
            action: StorageAction::Upload,
            content_length: Some(bytes),
            checksum_sha256: Some("abc".to_owned()),
            content_type: Some("application/pdf".to_owned()),
            upload_id: None,
            part_number: None,
        }
    }

    fn gateway() -> Result<StorageGateway<AllowMetadata, CapturingPresigner>, StorageError> {
        StorageGateway::new(
            AllowMetadata,
            CapturingPresigner,
            [7_u8; 32],
            StorageLimits {
                signed_url_ttl_ms: 300_000,
                grant_ttl_ms: 60_000,
                max_pending_reservations: 64,
            },
        )
    }

    #[tokio::test]
    async fn grant_is_bound_to_authorized_provider_key_and_method() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let grant = gateway.authorize(&request(20), 1_000).await?;
        let signed = gateway.presign(&grant, 30_000, 2_000).await?;
        assert_eq!(signed.method, "PUT");
        assert!(signed.url.path().contains("/user-1/report.pdf"));
        assert!(!grant.as_str().contains("report.pdf"));
        Ok(())
    }

    #[tokio::test]
    async fn tampered_or_expired_grants_fail_closed() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let grant = gateway.authorize(&request(20), 1_000).await?;
        let mut tampered = grant.as_str().to_owned();
        tampered.push('x');
        assert_eq!(
            gateway
                .presign(&AuthorizationToken(tampered), 1_000, 2_000)
                .await,
            Err(StorageError::InvalidGrant)
        );
        assert_eq!(
            gateway.presign(&grant, 1_000, 61_000).await,
            Err(StorageError::ExpiredGrant)
        );
        Ok(())
    }

    #[tokio::test]
    async fn reservations_close_concurrent_quota_gap() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let first_request = request(50);
        let first = gateway.authorize(&first_request, 1_000).await?;
        let mut second_request = first_request.clone();
        second_request.content_length = Some(30);
        assert_eq!(
            gateway.authorize(&second_request, 1_000).await,
            Err(StorageError::ProjectQuotaExceeded)
        );
        assert_eq!(
            gateway
                .release_reservation(&first, &first_request.auth, 2_000)
                .await?,
            50
        );
        gateway.authorize(&second_request, 2_000).await?;
        Ok(())
    }

    #[tokio::test]
    async fn zero_byte_mutations_use_single_use_durable_reservations() -> Result<(), StorageError> {
        let reserves = Arc::new(AtomicUsize::new(0));
        let commits = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let gateway = StorageGateway::new(
            CountingMetadata {
                reserves: Arc::clone(&reserves),
                commits: Arc::clone(&commits),
                releases: Arc::clone(&releases),
            },
            CapturingPresigner,
            [7_u8; 32],
            StorageLimits {
                signed_url_ttl_ms: 300_000,
                grant_ttl_ms: 60_000,
                max_pending_reservations: 64,
            },
        )?;
        let mut delete = request(0);
        delete.action = StorageAction::Delete;
        delete.content_length = None;
        delete.checksum_sha256 = None;
        delete.content_type = None;

        let committed = gateway.authorize(&delete, 1_000).await?;
        assert_eq!(reserves.load(Ordering::SeqCst), 1);
        gateway
            .commit_metadata(
                &committed,
                delete.auth.clone(),
                StorageCommitResult::default(),
                2_000,
            )
            .await?;
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert_eq!(
            releases.load(Ordering::SeqCst),
            0,
            "metadata commit consumes its reservation atomically"
        );

        let released = gateway.authorize(&delete, 3_000).await?;
        assert_eq!(reserves.load(Ordering::SeqCst), 2);
        assert_eq!(
            gateway
                .release_reservation(&released, &delete.auth, 4_000)
                .await?,
            0
        );
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        let mut download = delete;
        download.action = StorageAction::Download;
        gateway.authorize(&download, 5_000).await?;
        assert_eq!(
            reserves.load(Ordering::SeqCst),
            2,
            "read-only grants do not create durable reservations"
        );
        Ok(())
    }

    #[tokio::test]
    async fn multipart_stage_commit_requires_matching_action_and_valid_provider_result()
    -> Result<(), StorageError> {
        let reserves = Arc::new(AtomicUsize::new(0));
        let commits = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let gateway = StorageGateway::new(
            CountingMetadata {
                reserves: Arc::clone(&reserves),
                commits: Arc::clone(&commits),
                releases,
            },
            CapturingPresigner,
            [7_u8; 32],
            StorageLimits {
                signed_url_ttl_ms: 300_000,
                grant_ttl_ms: 60_000,
                max_pending_reservations: 64,
            },
        )?;
        let mut create = request(0);
        create.action = StorageAction::CreateMultipart;
        create.content_length = Some(10);
        create.checksum_sha256 = None;

        let grant = gateway.authorize(&create, 1_000).await?;
        assert_eq!(reserves.load(Ordering::SeqCst), 1);
        assert_eq!(
            gateway
                .commit_multipart_stage(
                    &grant,
                    create.auth.clone(),
                    StorageAction::UploadPart,
                    StorageCommitResult {
                        etag: Some("part-etag".to_owned()),
                        ..StorageCommitResult::default()
                    },
                    2_000,
                )
                .await,
            Err(StorageError::InvalidMultipartRequest)
        );
        assert_eq!(
            gateway
                .commit_multipart_stage(
                    &grant,
                    create.auth.clone(),
                    StorageAction::CreateMultipart,
                    StorageCommitResult::default(),
                    2_000,
                )
                .await,
            Err(StorageError::InvalidMultipartRequest)
        );
        assert_eq!(commits.load(Ordering::SeqCst), 0);

        gateway
            .commit_multipart_stage(
                &grant,
                create.auth,
                StorageAction::CreateMultipart,
                StorageCommitResult {
                    version_id: Some("provider-upload-id".to_owned()),
                    ..StorageCommitResult::default()
                },
                2_000,
            )
            .await?;
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn path_escape_is_rejected_before_authorization() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let mut invalid = request(10);
        invalid.object_key = "users/../secrets".to_owned();
        assert_eq!(
            gateway.authorize(&invalid, 1_000).await,
            Err(StorageError::InvalidObjectKey)
        );
        Ok(())
    }

    #[tokio::test]
    async fn token_debug_is_redacted_and_oversized_input_is_rejected() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let grant = gateway.authorize(&request(20), 1_000).await?;
        let debug = format!("{grant:?}");
        assert_eq!(debug, "AuthorizationToken([REDACTED])");
        assert!(!debug.contains(grant.as_str()));
        let oversized = AuthorizationToken("a".repeat(10_000));
        assert_eq!(
            gateway.presign(&oversized, 1_000, 2_000).await,
            Err(StorageError::InvalidGrant)
        );
        Ok(())
    }

    #[tokio::test]
    async fn expired_reservations_are_swept_and_do_not_exhaust_quota() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let original_request = request(60);
        let expired = gateway.authorize(&original_request, 1_000).await?;
        assert_eq!(gateway.cleanup_expired_reservations(61_000).await?, 1);
        assert_eq!(
            gateway
                .release_reservation(&expired, &original_request.auth, 61_000)
                .await?,
            0
        );
        gateway.authorize(&original_request, 61_000).await?;
        Ok(())
    }

    #[tokio::test]
    async fn old_cross_project_grant_cannot_release_reused_nonce() -> Result<(), StorageError> {
        let gateway = gateway()?;
        let project_a = request(20);
        let project_b = request(20);
        let nonce = "forced-collision-0123456789".to_owned();
        let old_grant = gateway
            .authorize_with_nonce(&project_a, 1_000, nonce.clone())
            .await?;
        assert_eq!(
            gateway
                .release_reservation(&old_grant, &project_a.auth, 1_100)
                .await?,
            20
        );
        let current_grant = gateway
            .authorize_with_nonce(&project_b, 1_200, nonce)
            .await?;
        assert_eq!(
            gateway
                .release_reservation(&old_grant, &project_a.auth, 1_300)
                .await,
            Err(StorageError::InvalidGrant)
        );
        assert_eq!(
            gateway
                .release_reservation(&current_grant, &project_b.auth, 1_300)
                .await?,
            20
        );
        Ok(())
    }

    #[test]
    fn s3_endpoint_rejects_private_resolution_and_unlisted_hosts() -> Result<(), StorageError> {
        let endpoint = Url::parse("https://objects.example.test/")
            .map_err(|_| StorageError::InvalidConfiguration)?;
        let allowed = vec!["objects.example.test".to_owned()];
        let metadata_ip = "169.254.169.254"
            .parse()
            .map_err(|_| StorageError::InvalidConfiguration)?;
        assert_eq!(
            validate_s3_endpoint(&endpoint, &allowed, &[metadata_ip], false, None, None),
            Err(StorageError::UnsafeProviderEndpoint)
        );
        let public_ip = "8.8.8.8"
            .parse()
            .map_err(|_| StorageError::InvalidConfiguration)?;
        validate_s3_endpoint(&endpoint, &allowed, &[public_ip], false, None, None)?;
        let development =
            Url::parse("http://minio:9000/").map_err(|_| StorageError::InvalidConfiguration)?;
        let private_ip = "172.18.0.5"
            .parse()
            .map_err(|_| StorageError::InvalidConfiguration)?;
        validate_s3_endpoint(
            &development,
            &["minio".to_owned()],
            &[private_ip],
            false,
            Some("minio"),
            None,
        )?;
        assert_eq!(
            validate_s3_endpoint(
                &development,
                &["minio".to_owned()],
                &[metadata_ip],
                false,
                Some("minio"),
                None,
            ),
            Err(StorageError::UnsafeProviderEndpoint)
        );
        let private_https = Url::parse("https://s3.internal.example/")
            .map_err(|_| StorageError::InvalidConfiguration)?;
        assert_eq!(
            validate_s3_endpoint(
                &private_https,
                &["s3.internal.example".to_owned()],
                &[private_ip],
                false,
                None,
                None,
            ),
            Err(StorageError::UnsafeProviderEndpoint)
        );
        validate_s3_endpoint(
            &private_https,
            &["s3.internal.example".to_owned()],
            &[private_ip],
            false,
            None,
            Some("s3.internal.example"),
        )?;
        assert_eq!(
            validate_s3_endpoint(
                &private_https,
                &["s3.internal.example".to_owned()],
                &[private_ip],
                false,
                None,
                Some("different.internal.example"),
            ),
            Err(StorageError::UnsafeProviderEndpoint)
        );
        Ok(())
    }
}
