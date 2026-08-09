use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs as _};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest as _, Sha256};
use url::{Host, Url};
use zeroize::Zeroizing;

use crate::{
    ObjectProvider, ProviderOperation, S3Presigner, SignedObjectRequest, StorageAction,
    StorageCommitResult, StorageError, validate_provider_key, validate_s3_endpoint,
};

type HmacSha256 = Hmac<Sha256>;
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

#[derive(Clone)]
pub struct S3ProviderConfig {
    pub endpoint: Url,
    pub public_endpoint: Url,
    pub region: String,
    pub bucket: String,
    pub allow_insecure_localhost: bool,
    insecure_development_service_host: Option<String>,
    insecure_development_public_host: Option<String>,
    private_network_service_host: Option<String>,
    access_key_id: Zeroizing<String>,
    secret_access_key: Zeroizing<String>,
    session_token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for S3ProviderConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3ProviderConfig")
            .field("endpoint", &self.endpoint)
            .field("public_endpoint", &self.public_endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("allow_insecure_localhost", &self.allow_insecure_localhost)
            .field(
                "insecure_development_service_host",
                &self.insecure_development_service_host,
            )
            .field(
                "insecure_development_public_host",
                &self.insecure_development_public_host,
            )
            .field(
                "private_network_service_host",
                &self.private_network_service_host,
            )
            .field("access_key_id", &"[REDACTED]")
            .field("secret_access_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl S3ProviderConfig {
    #[must_use]
    pub fn new(
        endpoint: Url,
        region: impl Into<String>,
        bucket: impl Into<String>,
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
    ) -> Self {
        Self {
            public_endpoint: endpoint.clone(),
            endpoint,
            region: region.into(),
            bucket: bucket.into(),
            allow_insecure_localhost: false,
            insecure_development_service_host: None,
            insecure_development_public_host: None,
            private_network_service_host: None,
            access_key_id: Zeroizing::new(access_key_id.into()),
            secret_access_key: Zeroizing::new(secret_access_key.into()),
            session_token: None,
        }
    }

    #[must_use]
    pub fn allow_insecure_localhost(mut self, allow: bool) -> Self {
        self.allow_insecure_localhost = allow;
        self
    }

    /// Allows HTTP only for this exact operator-configured Development/Test
    /// service name and only when DNS resolves to private or loopback addresses.
    #[must_use]
    pub fn allow_insecure_development_service(mut self, host: impl Into<String>) -> Self {
        self.insecure_development_service_host = Some(host.into());
        self
    }

    #[must_use]
    pub fn with_public_endpoint(mut self, endpoint: Url) -> Self {
        self.public_endpoint = endpoint;
        self
    }

    /// Allows HTTP signed URLs for this exact Development/Test browser-facing
    /// host. It has no effect on the provider verification connection.
    #[must_use]
    pub fn allow_insecure_development_public_host(mut self, host: impl Into<String>) -> Self {
        self.insecure_development_public_host = Some(host.into());
        self
    }

    /// Allows private RFC1918/ULA addresses only for the exact configured
    /// internal HTTPS endpoint hostname. It never affects public signed URLs.
    #[must_use]
    pub fn allow_private_network_service(mut self, host: impl Into<String>) -> Self {
        self.private_network_service_host = Some(host.into());
        self
    }

    #[must_use]
    pub fn with_session_token(mut self, token: impl Into<String>) -> Self {
        self.session_token = Some(Zeroizing::new(token.into()));
        self
    }
}

#[derive(Clone)]
pub struct S3Provider {
    config: S3ProviderConfig,
    client: reqwest::Client,
}

impl std::fmt::Debug for S3Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3Provider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl S3Provider {
    const MAX_CONTROL_RESPONSE_BYTES: usize = 128 * 1024;

    /// Resolve, validate, and pin the configured endpoint. The endpoint is
    /// operator configuration, never request input. Redirects stay disabled.
    pub fn new(config: S3ProviderConfig) -> Result<Self, StorageError> {
        let host = config
            .endpoint
            .host_str()
            .ok_or(StorageError::InvalidConfiguration)?;
        let port = config
            .endpoint
            .port_or_known_default()
            .ok_or(StorageError::InvalidConfiguration)?;
        let socket_addresses: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|_| StorageError::UnsafeProviderEndpoint)?
            .collect();
        let public_host = config
            .public_endpoint
            .host_str()
            .ok_or(StorageError::InvalidConfiguration)?;
        let public_port = config
            .public_endpoint
            .port_or_known_default()
            .ok_or(StorageError::InvalidConfiguration)?;
        let public_addresses: Vec<SocketAddr> = (public_host, public_port)
            .to_socket_addrs()
            .map_err(|_| StorageError::UnsafeProviderEndpoint)?
            .collect();
        Self::with_resolved_endpoint_addresses(config, socket_addresses, public_addresses)
    }

    pub fn with_resolved_addresses(
        config: S3ProviderConfig,
        socket_addresses: Vec<SocketAddr>,
    ) -> Result<Self, StorageError> {
        Self::with_resolved_endpoint_addresses(config, socket_addresses.clone(), socket_addresses)
    }

    pub fn with_resolved_endpoint_addresses(
        config: S3ProviderConfig,
        socket_addresses: Vec<SocketAddr>,
        public_socket_addresses: Vec<SocketAddr>,
    ) -> Result<Self, StorageError> {
        validate_config(&config)?;
        let host = config
            .endpoint
            .host_str()
            .ok_or(StorageError::InvalidConfiguration)?;
        let allowed_hosts = vec![host.to_owned()];
        let resolved_addresses: Vec<IpAddr> =
            socket_addresses.iter().map(|value| value.ip()).collect();
        validate_s3_endpoint(
            &config.endpoint,
            &allowed_hosts,
            &resolved_addresses,
            config.allow_insecure_localhost,
            config.insecure_development_service_host.as_deref(),
            config.private_network_service_host.as_deref(),
        )?;
        let public_host = config
            .public_endpoint
            .host_str()
            .ok_or(StorageError::InvalidConfiguration)?;
        let public_addresses = public_socket_addresses
            .iter()
            .map(|value| value.ip())
            .collect::<Vec<_>>();
        validate_s3_endpoint(
            &config.public_endpoint,
            &[public_host.to_owned()],
            &public_addresses,
            config.allow_insecure_localhost,
            config.insecure_development_public_host.as_deref(),
            None,
        )?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .resolve_to_addrs(host, &socket_addresses)
            .build()
            .map_err(|_| StorageError::InvalidConfiguration)?;
        Ok(Self { config, client })
    }

    pub fn presign_at(
        &self,
        operation: &ProviderOperation,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<SignedObjectRequest, StorageError> {
        let seconds = ttl_ms / 1_000;
        if !(1..=604_800).contains(&seconds) {
            return Err(StorageError::InvalidTtl);
        }
        validate_operation(operation)?;
        let timestamp = timestamp(now_ms)?;
        let (method, canonical_uri, mut query) = target(operation, &self.config.bucket)?;
        let mut required_headers = required_headers(operation);
        let host = canonical_host(&self.config.public_endpoint)?;
        let mut canonical_headers = BTreeMap::from([("host".to_owned(), host)]);
        for (name, value) in &required_headers {
            canonical_headers.insert(name.to_ascii_lowercase(), normalize_header(value));
        }
        let signed_headers = canonical_headers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(";");
        let credential_scope = format!("{}/{}/s3/aws4_request", timestamp.date, self.config.region);
        query.insert("X-Amz-Algorithm".to_owned(), "AWS4-HMAC-SHA256".to_owned());
        query.insert(
            "X-Amz-Credential".to_owned(),
            format!(
                "{}/{}",
                self.config.access_key_id.as_str(),
                credential_scope
            ),
        );
        query.insert("X-Amz-Date".to_owned(), timestamp.amz_date.clone());
        query.insert("X-Amz-Expires".to_owned(), seconds.to_string());
        query.insert("X-Amz-SignedHeaders".to_owned(), signed_headers.clone());
        if let Some(token) = &self.config.session_token {
            query.insert("X-Amz-Security-Token".to_owned(), token.to_string());
        }
        let canonical_query_without_signature = canonical_query(&query);
        let header_block = canonical_header_block(&canonical_headers);
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query_without_signature}\n{header_block}\n{signed_headers}\n{UNSIGNED_PAYLOAD}"
        );
        let string_to_sign =
            string_to_sign(&timestamp.amz_date, &credential_scope, &canonical_request);
        let signature = signature(
            self.config.secret_access_key.as_bytes(),
            &timestamp.date,
            &self.config.region,
            &string_to_sign,
        )?;
        query.insert("X-Amz-Signature".to_owned(), signature);
        let mut url = self.config.public_endpoint.clone();
        url.set_path(&canonical_uri);
        url.set_query(Some(&canonical_query(&query)));
        required_headers.sort_by(|left, right| left.0.cmp(&right.0));
        let expires_at_ms = now_ms
            .div_euclid(1_000)
            .saturating_add(seconds)
            .saturating_mul(1_000);
        Ok(SignedObjectRequest {
            url,
            method,
            expires_at_ms,
            required_headers,
        })
    }

    /// Initiates multipart upload through the pinned internal endpoint. This
    /// control-plane operation is never exposed as a browser presigned URL.
    pub async fn initiate_multipart_internal(
        &self,
        provider_key: &str,
        content_type: Option<&str>,
        checksum_sha256: bool,
        now_ms: i64,
    ) -> Result<String, StorageError> {
        let mut query = BTreeMap::new();
        query.insert("uploads".to_owned(), String::new());
        let mut control_headers = BTreeMap::new();
        if let Some(content_type) = content_type {
            control_headers.insert("content-type".to_owned(), content_type.to_owned());
        }
        if checksum_sha256 {
            control_headers.insert("x-amz-checksum-algorithm".to_owned(), "SHA256".to_owned());
        }
        let mut response = self
            .send_internal("POST", provider_key, &query, &control_headers, now_ms)
            .await?;
        if !response.status().is_success() {
            return Err(StorageError::Provider);
        }
        let body = read_bounded_body(&mut response, Self::MAX_CONTROL_RESPONSE_BYTES).await?;
        parse_upload_id(&body)
    }

    /// Deletes an opaque provider key using the redirect-disabled, DNS-pinned
    /// internal endpoint. The key comes only from trusted project metadata.
    pub async fn delete_internal(
        &self,
        provider_key: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let response = self
            .send_internal(
                "DELETE",
                provider_key,
                &BTreeMap::new(),
                &BTreeMap::new(),
                now_ms,
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(StorageError::Provider)
        }
    }

    /// Aborts a multipart upload using a provider-generated upload identifier.
    pub async fn abort_multipart_internal(
        &self,
        provider_key: &str,
        upload_id: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        if upload_id.is_empty() || upload_id.len() > 256 || upload_id.chars().any(char::is_control)
        {
            return Err(StorageError::InvalidMultipartRequest);
        }
        let query = BTreeMap::from([("uploadId".to_owned(), upload_id.to_owned())]);
        let response = self
            .send_internal("DELETE", provider_key, &query, &BTreeMap::new(), now_ms)
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(StorageError::Provider)
        }
    }

    /// Recovery path for a create response that reached S3 but could not be
    /// durably bound. Provider keys are unique per write, so an exact-prefix
    /// listing can safely find and abort only that abandoned upload.
    pub async fn abort_multipart_for_key_internal(
        &self,
        provider_key: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let (upload_ids, truncated) = self
            .multipart_upload_ids_for_key_internal(provider_key, now_ms)
            .await?;
        for upload_id in upload_ids {
            self.abort_multipart_internal(provider_key, &upload_id, now_ms)
                .await?;
        }
        if truncated {
            // Keep the cleanup item queued; the next retry drains another page.
            Err(StorageError::Provider)
        } else {
            Ok(())
        }
    }

    /// Finds an exact-key in-progress upload before initiating a new one. This
    /// recovers a provider-successful CreateMultipart whose response was lost
    /// before FFDB learned the upload id.
    pub async fn recover_multipart_for_key_internal(
        &self,
        provider_key: &str,
        now_ms: i64,
    ) -> Result<Option<String>, StorageError> {
        let (mut upload_ids, truncated) = self
            .multipart_upload_ids_for_key_internal(provider_key, now_ms)
            .await?;
        if truncated {
            return Err(StorageError::Provider);
        }
        upload_ids.sort_unstable();
        upload_ids.dedup();
        Ok(upload_ids.into_iter().next())
    }

    async fn multipart_upload_ids_for_key_internal(
        &self,
        provider_key: &str,
        now_ms: i64,
    ) -> Result<(Vec<String>, bool), StorageError> {
        validate_provider_key(provider_key)?;
        let query = BTreeMap::from([
            ("max-uploads".to_owned(), "100".to_owned()),
            ("prefix".to_owned(), provider_key.to_owned()),
            ("uploads".to_owned(), String::new()),
        ]);
        let request = self.build_internal_bucket_request("GET", &query, now_ms)?;
        let mut response = self
            .client
            .execute(request)
            .await
            .map_err(|_| StorageError::Provider)?;
        if !response.status().is_success() {
            return Err(StorageError::Provider);
        }
        let body = read_bounded_body(&mut response, Self::MAX_CONTROL_RESPONSE_BYTES).await?;
        let (uploads, truncated) = parse_multipart_uploads(&body)?;
        Ok((
            uploads
                .into_iter()
                .filter_map(|(key, upload_id)| (key == provider_key).then_some(upload_id))
                .collect(),
            truncated,
        ))
    }

    async fn send_internal(
        &self,
        method: &str,
        provider_key: &str,
        query: &BTreeMap<String, String>,
        control_headers: &BTreeMap<String, String>,
        now_ms: i64,
    ) -> Result<reqwest::Response, StorageError> {
        let request =
            self.build_internal_request(method, provider_key, query, control_headers, now_ms)?;
        self.client
            .execute(request)
            .await
            .map_err(|_| StorageError::Provider)
    }

    fn build_internal_request(
        &self,
        method: &str,
        provider_key: &str,
        query: &BTreeMap<String, String>,
        control_headers: &BTreeMap<String, String>,
        now_ms: i64,
    ) -> Result<reqwest::Request, StorageError> {
        validate_provider_key(provider_key)?;
        let canonical_uri = format!(
            "/{}/{}",
            aws_uri_encode(&self.config.bucket, true),
            aws_uri_encode(provider_key, false)
        );
        self.build_internal_request_at(method, &canonical_uri, query, control_headers, now_ms)
    }

    fn build_internal_bucket_request(
        &self,
        method: &str,
        query: &BTreeMap<String, String>,
        now_ms: i64,
    ) -> Result<reqwest::Request, StorageError> {
        let canonical_uri = format!("/{}", aws_uri_encode(&self.config.bucket, true));
        self.build_internal_request_at(method, &canonical_uri, query, &BTreeMap::new(), now_ms)
    }

    fn build_internal_request_at(
        &self,
        method: &str,
        canonical_uri: &str,
        query: &BTreeMap<String, String>,
        control_headers: &BTreeMap<String, String>,
        now_ms: i64,
    ) -> Result<reqwest::Request, StorageError> {
        let canonical_query = canonical_query(query);
        let timestamp = timestamp(now_ms)?;
        let host = canonical_host(&self.config.endpoint)?;
        let mut headers = BTreeMap::from([
            ("host".to_owned(), host),
            ("x-amz-content-sha256".to_owned(), EMPTY_SHA256.to_owned()),
            ("x-amz-date".to_owned(), timestamp.amz_date.clone()),
        ]);
        if let Some(token) = &self.config.session_token {
            headers.insert("x-amz-security-token".to_owned(), token.to_string());
        }
        for (name, value) in control_headers {
            headers.insert(name.clone(), normalize_header(value));
        }
        let signed_headers = headers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(";");
        let header_block = canonical_header_block(&headers);
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{header_block}\n{signed_headers}\n{EMPTY_SHA256}"
        );
        let scope = format!("{}/{}/s3/aws4_request", timestamp.date, self.config.region);
        let to_sign = string_to_sign(&timestamp.amz_date, &scope, &canonical_request);
        let signed = signature(
            self.config.secret_access_key.as_bytes(),
            &timestamp.date,
            &self.config.region,
            &to_sign,
        )?;
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signed}",
            self.config.access_key_id.as_str()
        );
        let mut request_headers = HeaderMap::new();
        for (name, value) in headers {
            if name == "host" {
                continue;
            }
            request_headers.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| StorageError::Internal)?,
                HeaderValue::from_str(&value).map_err(|_| StorageError::Internal)?,
            );
        }
        request_headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&authorization).map_err(|_| StorageError::Internal)?,
        );
        let mut url = self.config.endpoint.clone();
        url.set_path(canonical_uri);
        url.set_query((!canonical_query.is_empty()).then_some(canonical_query.as_str()));
        self.client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|_| StorageError::Internal)?,
                url,
            )
            .headers(request_headers)
            .build()
            .map_err(|_| StorageError::Internal)
    }

    async fn inspect_object(
        &self,
        operation: &ProviderOperation,
        now_ms: i64,
    ) -> Result<Option<StorageCommitResult>, StorageError> {
        let (_, canonical_uri, _) = target(operation, &self.config.bucket)?;
        let mut url = self.config.endpoint.clone();
        url.set_path(&canonical_uri);
        url.set_query(None);
        let timestamp = timestamp(now_ms)?;
        let host = canonical_host(&self.config.endpoint)?;
        let mut headers = BTreeMap::from([
            ("host".to_owned(), host),
            ("x-amz-content-sha256".to_owned(), EMPTY_SHA256.to_owned()),
            ("x-amz-date".to_owned(), timestamp.amz_date.clone()),
        ]);
        if let Some(token) = &self.config.session_token {
            headers.insert("x-amz-security-token".to_owned(), token.to_string());
        }
        let signed_headers = headers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(";");
        let header_block = canonical_header_block(&headers);
        let canonical_request =
            format!("HEAD\n{canonical_uri}\n\n{header_block}\n{signed_headers}\n{EMPTY_SHA256}");
        let scope = format!("{}/{}/s3/aws4_request", timestamp.date, self.config.region);
        let to_sign = string_to_sign(&timestamp.amz_date, &scope, &canonical_request);
        let signed = signature(
            self.config.secret_access_key.as_bytes(),
            &timestamp.date,
            &self.config.region,
            &to_sign,
        )?;
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signed}",
            self.config.access_key_id.as_str()
        );
        let mut request_headers = HeaderMap::new();
        for (name, value) in headers {
            if name == "host" {
                continue;
            }
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| StorageError::Internal)?;
            let value = HeaderValue::from_str(&value).map_err(|_| StorageError::Internal)?;
            request_headers.insert(name, value);
        }
        request_headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&authorization).map_err(|_| StorageError::Internal)?,
        );
        let response = self
            .client
            .head(url)
            .headers(request_headers)
            .send()
            .await
            .map_err(|_| StorageError::Provider)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(StorageError::Provider);
        }
        let headers = response.headers();
        let content_length = header_string(headers, CONTENT_LENGTH)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(StorageError::Provider)?;
        let content_type = header_string(headers, CONTENT_TYPE);
        if operation
            .max_bytes
            .is_some_and(|expected| expected != content_length)
            || operation
                .content_type
                .as_deref()
                .is_some_and(|expected| content_type.as_deref() != Some(expected))
        {
            return Err(StorageError::ProviderMetadataMismatch);
        }
        let checksum = optional_header(headers, "x-amz-checksum-sha256")?;
        if operation
            .checksum_sha256
            .as_deref()
            .is_some_and(|expected| checksum.as_deref() != Some(expected))
        {
            return Err(StorageError::ProviderMetadataMismatch);
        }
        Ok(Some(StorageCommitResult {
            content_length: Some(content_length),
            checksum_sha256: checksum,
            etag: header_string(headers, ETAG).map(|value| value.trim_matches('"').to_owned()),
            version_id: optional_header(headers, "x-amz-version-id")?,
        }))
    }
}

async fn read_bounded_body(
    response: &mut reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, StorageError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(StorageError::InvalidMultipartRequest);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| StorageError::Provider)? {
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(StorageError::InvalidMultipartRequest);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_upload_id(body: &[u8]) -> Result<String, StorageError> {
    if body.len() > S3Provider::MAX_CONTROL_RESPONSE_BYTES {
        return Err(StorageError::InvalidMultipartRequest);
    }
    let xml = std::str::from_utf8(body).map_err(|_| StorageError::InvalidMultipartRequest)?;
    let start = xml
        .find("<UploadId>")
        .map(|index| index + "<UploadId>".len())
        .ok_or(StorageError::InvalidMultipartRequest)?;
    let end = xml[start..]
        .find("</UploadId>")
        .map(|index| start + index)
        .ok_or(StorageError::InvalidMultipartRequest)?;
    let value = decode_xml_text(&xml[start..end])?;
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(StorageError::InvalidMultipartRequest);
    }
    Ok(value)
}

fn parse_multipart_uploads(body: &[u8]) -> Result<(Vec<(String, String)>, bool), StorageError> {
    if body.len() > S3Provider::MAX_CONTROL_RESPONSE_BYTES {
        return Err(StorageError::InvalidMultipartRequest);
    }
    let xml = std::str::from_utf8(body).map_err(|_| StorageError::InvalidMultipartRequest)?;
    let mut uploads = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find("<Upload>") {
        let after_start = &remaining[start + "<Upload>".len()..];
        let end = after_start
            .find("</Upload>")
            .ok_or(StorageError::InvalidMultipartRequest)?;
        let upload = &after_start[..end];
        let key = parse_xml_element(upload, "Key")?;
        let upload_id = parse_xml_element(upload, "UploadId")?;
        if key.len() > 1_024
            || upload_id.is_empty()
            || upload_id.len() > 256
            || key.chars().chain(upload_id.chars()).any(char::is_control)
            || uploads.len() >= 100
        {
            return Err(StorageError::InvalidMultipartRequest);
        }
        uploads.push((key, upload_id));
        remaining = &after_start[end + "</Upload>".len()..];
    }
    let truncated = xml.contains("<IsTruncated>true</IsTruncated>");
    Ok((uploads, truncated))
}

fn parse_xml_element(xml: &str, name: &str) -> Result<String, StorageError> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml
        .find(&open)
        .map(|index| index + open.len())
        .ok_or(StorageError::InvalidMultipartRequest)?;
    let end = xml[start..]
        .find(&close)
        .map(|index| start + index)
        .ok_or(StorageError::InvalidMultipartRequest)?;
    decode_xml_text(&xml[start..end])
}

fn decode_xml_text(value: &str) -> Result<String, StorageError> {
    let mut decoded = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('&') {
        decoded.push_str(&remaining[..start]);
        let entity = &remaining[start..];
        let end = entity
            .find(';')
            .filter(|end| *end <= 12)
            .ok_or(StorageError::InvalidMultipartRequest)?;
        let name = &entity[1..end];
        let character = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ if name.starts_with("#x") => u32::from_str_radix(&name[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or(StorageError::InvalidMultipartRequest)?,
            _ if name.starts_with('#') => name[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .ok_or(StorageError::InvalidMultipartRequest)?,
            _ => return Err(StorageError::InvalidMultipartRequest),
        };
        decoded.push(character);
        remaining = &entity[end + 1..];
    }
    decoded.push_str(remaining);
    Ok(decoded)
}

#[async_trait]
impl S3Presigner for S3Provider {
    async fn presign(
        &self,
        operation: &ProviderOperation,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<SignedObjectRequest, StorageError> {
        self.presign_at(operation, ttl_ms, now_ms)
    }
}

#[async_trait]
impl ObjectProvider for S3Provider {
    async fn verify_commit(
        &self,
        operation: &ProviderOperation,
        now_ms: i64,
    ) -> Result<StorageCommitResult, StorageError> {
        match operation.action {
            StorageAction::Upload | StorageAction::CompleteMultipart => self
                .inspect_object(operation, now_ms)
                .await?
                .ok_or(StorageError::ProviderMetadataMismatch),
            StorageAction::Delete => {
                if self.inspect_object(operation, now_ms).await?.is_some() {
                    return Err(StorageError::ProviderMetadataMismatch);
                }
                Ok(StorageCommitResult::default())
            }
            _ => Err(StorageError::InvalidAuthorizationDecision),
        }
    }
}

#[derive(Debug)]
struct SigningTimestamp {
    amz_date: String,
    date: String,
}

fn timestamp(now_ms: i64) -> Result<SigningTimestamp, StorageError> {
    let value: DateTime<Utc> =
        DateTime::from_timestamp_millis(now_ms).ok_or(StorageError::InvalidConfiguration)?;
    Ok(SigningTimestamp {
        amz_date: value.format("%Y%m%dT%H%M%SZ").to_string(),
        date: value.format("%Y%m%d").to_string(),
    })
}

fn validate_config(config: &S3ProviderConfig) -> Result<(), StorageError> {
    if config.endpoint.path() != "/"
        || config.endpoint.query().is_some()
        || config.public_endpoint.path() != "/"
        || config.public_endpoint.query().is_some()
        || config.public_endpoint.username() != ""
        || config.public_endpoint.password().is_some()
        || config.public_endpoint.fragment().is_some()
        || config.region.is_empty()
        || config.region.len() > 64
        || config.access_key_id.is_empty()
        || config.access_key_id.len() > 256
        || config.secret_access_key.len() < 8
        || config.secret_access_key.len() > 256
        || config
            .insecure_development_service_host
            .as_deref()
            .is_some_and(|host| Some(host) != config.endpoint.host_str())
        || config
            .insecure_development_public_host
            .as_deref()
            .is_some_and(|host| Some(host) != config.public_endpoint.host_str())
        || config
            .private_network_service_host
            .as_deref()
            .is_some_and(|host| {
                Some(host) != config.endpoint.host_str() || config.endpoint.scheme() != "https"
            })
        || !valid_bucket(&config.bucket)
        || config
            .session_token
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 4_096)
    {
        return Err(StorageError::InvalidConfiguration);
    }
    Ok(())
}

fn valid_bucket(bucket: &str) -> bool {
    let bytes = bucket.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
        && !bucket.contains("..")
        && bucket.parse::<Ipv4Addr>().is_err()
}

fn validate_operation(operation: &ProviderOperation) -> Result<(), StorageError> {
    if operation.bucket.is_empty()
        || operation.bucket.len() > 63
        || operation.provider_key.is_empty()
        || operation.provider_key.len() > 1_024
        || operation.provider_key.starts_with('/')
        || operation.provider_key.contains('\0')
        || matches!(operation.action, StorageAction::List)
    {
        return Err(StorageError::InvalidAuthorizationDecision);
    }
    if matches!(operation.action, StorageAction::UploadPart)
        && (operation.upload_id.is_none()
            || operation.part_number.is_none()
            || operation.max_bytes.is_none())
    {
        return Err(StorageError::InvalidMultipartRequest);
    }
    Ok(())
}

fn target(
    operation: &ProviderOperation,
    provider_bucket: &str,
) -> Result<(String, String, BTreeMap<String, String>), StorageError> {
    let path = format!(
        "/{}/{}",
        aws_uri_encode(provider_bucket, true),
        aws_uri_encode(&operation.provider_key, false)
    );
    let mut query = BTreeMap::new();
    let method = match operation.action {
        StorageAction::Upload => "PUT",
        StorageAction::Download => "GET",
        StorageAction::Delete => "DELETE",
        StorageAction::CreateMultipart => {
            query.insert("uploads".to_owned(), String::new());
            "POST"
        }
        StorageAction::UploadPart => {
            query.insert(
                "partNumber".to_owned(),
                operation
                    .part_number
                    .ok_or(StorageError::InvalidMultipartRequest)?
                    .to_string(),
            );
            query.insert(
                "uploadId".to_owned(),
                operation
                    .upload_id
                    .clone()
                    .ok_or(StorageError::InvalidMultipartRequest)?,
            );
            "PUT"
        }
        StorageAction::CompleteMultipart => {
            query.insert(
                "uploadId".to_owned(),
                operation
                    .upload_id
                    .clone()
                    .ok_or(StorageError::InvalidMultipartRequest)?,
            );
            "POST"
        }
        StorageAction::AbortMultipart => {
            query.insert(
                "uploadId".to_owned(),
                operation
                    .upload_id
                    .clone()
                    .ok_or(StorageError::InvalidMultipartRequest)?,
            );
            "DELETE"
        }
        StorageAction::List => return Err(StorageError::InvalidAuthorizationDecision),
    };
    Ok((method.to_owned(), path, query))
}

fn required_headers(operation: &ProviderOperation) -> Vec<(String, String)> {
    if !matches!(
        operation.action,
        StorageAction::Upload | StorageAction::UploadPart
    ) {
        return Vec::new();
    }
    let mut headers = Vec::new();
    if let Some(content_length) = operation.max_bytes {
        headers.push(("content-length".to_owned(), content_length.to_string()));
    }
    if let Some(content_type) = &operation.content_type {
        headers.push(("content-type".to_owned(), content_type.clone()));
    }
    if let Some(checksum) = &operation.checksum_sha256 {
        headers.push(("x-amz-checksum-sha256".to_owned(), checksum.clone()));
    }
    headers
}

fn canonical_host(endpoint: &Url) -> Result<String, StorageError> {
    let host = match endpoint.host().ok_or(StorageError::InvalidConfiguration)? {
        Host::Domain(value) => value.to_owned(),
        Host::Ipv4(value) => value.to_string(),
        Host::Ipv6(value) => format!("[{value}]"),
    };
    let include_port = endpoint
        .port()
        .is_some_and(|port| !matches!((endpoint.scheme(), port), ("http", 80) | ("https", 443)));
    Ok(if include_port {
        format!(
            "{host}:{}",
            endpoint.port().ok_or(StorageError::InvalidConfiguration)?
        )
    } else {
        host
    })
}

fn canonical_query(values: &BTreeMap<String, String>) -> String {
    let mut pairs = values
        .iter()
        .map(|(name, value)| (aws_uri_encode(name, true), aws_uri_encode(value, true)))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn aws_uri_encode(value: &str, encode_slash: bool) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || !encode_slash && byte == b'/'
        {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

fn normalize_header(value: &str) -> String {
    value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonical_header_block(headers: &BTreeMap<String, String>) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", normalize_header(value)))
        .collect()
}

fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    )
}

fn signature(
    secret: &[u8],
    date: &str,
    region: &str,
    string_to_sign: &str,
) -> Result<String, StorageError> {
    let mut prefixed = Zeroizing::new(Vec::with_capacity(secret.len().saturating_add(4)));
    prefixed.extend_from_slice(b"AWS4");
    prefixed.extend_from_slice(secret);
    let date_key = hmac(prefixed.as_slice(), date.as_bytes())?;
    let region_key = hmac(&date_key, region.as_bytes())?;
    let service_key = hmac(&region_key, b"s3")?;
    let signing_key = hmac(&service_key, b"aws4_request")?;
    Ok(hex(&hmac(&signing_key, string_to_sign.as_bytes())?))
}

fn hmac(key: &[u8], value: &[u8]) -> Result<Vec<u8>, StorageError> {
    let mut mac =
        <HmacSha256 as hmac::KeyInit>::new_from_slice(key).map_err(|_| StorageError::Internal)?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_sha256(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn header_string(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn optional_header(headers: &HeaderMap, name: &str) -> Result<Option<String>, StorageError> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| StorageError::Internal)?;
    Ok(header_string(headers, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn provider() -> Result<S3Provider, StorageError> {
        S3Provider::with_resolved_addresses(
            S3ProviderConfig::new(
                Url::parse("http://localhost:9000/")
                    .map_err(|_| StorageError::InvalidConfiguration)?,
                "us-east-1",
                "ffdb-test",
                "minio-access-key",
                "minio-secret-key",
            )
            .allow_insecure_localhost(true),
            vec![SocketAddr::from(([127, 0, 0, 1], 9_000))],
        )
    }

    fn upload() -> ProviderOperation {
        ProviderOperation {
            action: StorageAction::Upload,
            bucket: "documents".to_owned(),
            provider_key: "projects/019f/report final.pdf".to_owned(),
            max_bytes: Some(12),
            checksum_sha256: Some("YWJj".to_owned()),
            content_type: Some("application/pdf".to_owned()),
            upload_id: None,
            part_number: None,
        }
    }

    #[test]
    fn presign_is_deterministic_and_minio_path_style_compatible() -> Result<(), StorageError> {
        let signed = provider()?.presign_at(&upload(), 60_000, 1_774_166_400_000)?;
        assert_eq!(signed.method, "PUT");
        assert_eq!(
            signed.url.path(),
            "/ffdb-test/projects/019f/report%20final.pdf"
        );
        assert!(
            signed
                .url
                .query_pairs()
                .any(|(name, value)| { name == "X-Amz-Algorithm" && value == "AWS4-HMAC-SHA256" })
        );
        assert!(signed.url.query_pairs().any(|(name, value)| {
            name == "X-Amz-Credential" && value.starts_with("minio-access-key/")
        }));
        assert!(
            signed
                .url
                .query_pairs()
                .any(|(name, value)| { name == "X-Amz-Signature" && value.len() == 64 })
        );
        assert_eq!(
            signed.required_headers,
            vec![
                ("content-length".to_owned(), "12".to_owned()),
                ("content-type".to_owned(), "application/pdf".to_owned()),
                ("x-amz-checksum-sha256".to_owned(), "YWJj".to_owned()),
            ]
        );
        Ok(())
    }

    #[test]
    fn presign_uses_public_host_while_provider_client_stays_internal() -> Result<(), StorageError> {
        let config = S3ProviderConfig::new(
            Url::parse("http://minio:9000/").map_err(|_| StorageError::InvalidConfiguration)?,
            "us-east-1",
            "ffdb-test",
            "minio-access-key",
            "minio-secret-key",
        )
        .with_public_endpoint(
            Url::parse("http://localhost:9000/").map_err(|_| StorageError::InvalidConfiguration)?,
        )
        .allow_insecure_development_service("minio")
        .allow_insecure_development_public_host("localhost");
        let provider = S3Provider::with_resolved_endpoint_addresses(
            config,
            vec![SocketAddr::from(([172, 18, 0, 5], 9_000))],
            vec![SocketAddr::from(([127, 0, 0, 1], 9_000))],
        )?;
        let signed = provider.presign_at(&upload(), 60_000, 1_774_166_400_000)?;
        assert_eq!(signed.url.host_str(), Some("localhost"));
        assert_eq!(provider.config.endpoint.host_str(), Some("minio"));
        assert!(
            signed
                .url
                .query_pairs()
                .any(|(name, _)| name == "X-Amz-Signature")
        );
        Ok(())
    }

    #[test]
    fn debug_output_redacts_credentials_and_signed_url() -> Result<(), StorageError> {
        let provider = provider()?;
        let debug = format!("{provider:?}");
        assert!(!debug.contains("minio-access-key"));
        assert!(!debug.contains("minio-secret-key"));
        let signed = provider.presign_at(&upload(), 60_000, 1_774_166_400_000)?;
        let debug = format!("{signed:?}");
        assert!(!debug.contains("X-Amz-Signature"));
        assert!(!debug.contains("YWJj"));
        Ok(())
    }

    #[test]
    fn list_is_never_provider_presigned() -> Result<(), StorageError> {
        let mut operation = upload();
        operation.action = StorageAction::List;
        assert_eq!(
            provider()?.presign_at(&operation, 60_000, 1_774_166_400_000),
            Err(StorageError::InvalidAuthorizationDecision)
        );
        Ok(())
    }

    #[test]
    fn internal_delete_is_sigv4_signed_for_the_pinned_endpoint() -> Result<(), StorageError> {
        let request = provider()?.build_internal_request(
            "DELETE",
            "projects/project-1/retired-object",
            &BTreeMap::new(),
            &BTreeMap::new(),
            1_774_166_400_000,
        )?;
        assert_eq!(request.method(), reqwest::Method::DELETE);
        assert_eq!(request.url().host_str(), Some("localhost"));
        assert_eq!(
            request.url().path(),
            "/ffdb-test/projects/project-1/retired-object"
        );
        assert!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("AWS4-HMAC-SHA256 Credential="))
        );
        Ok(())
    }

    #[test]
    fn internal_multipart_control_is_bounded_and_uses_internal_endpoint() -> Result<(), StorageError>
    {
        let body = "<InitiateMultipartUploadResult><UploadId>provider&amp;upload</UploadId></InitiateMultipartUploadResult>";
        assert_eq!(parse_upload_id(body.as_bytes())?, "provider&upload");
        assert_eq!(
            parse_upload_id(&vec![b'a'; S3Provider::MAX_CONTROL_RESPONSE_BYTES + 1]),
            Err(StorageError::InvalidMultipartRequest)
        );
        let request = provider()?.build_internal_request(
            "POST",
            "projects/project-1/multipart-object",
            &BTreeMap::from([("uploads".to_owned(), String::new())]),
            &BTreeMap::from([(
                "content-type".to_owned(),
                "application/octet-stream".to_owned(),
            )]),
            1_774_166_400_000,
        )?;
        assert_eq!(request.url().query(), Some("uploads="));
        assert_eq!(request.url().host_str(), Some("localhost"));
        assert_eq!(
            request.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/octet-stream"))
        );
        let listing = b"<ListMultipartUploadsResult><Upload><Key>projects/project-1/object&amp;one</Key><UploadId>upload&amp;one</UploadId></Upload><IsTruncated>true</IsTruncated></ListMultipartUploadsResult>";
        assert_eq!(
            parse_multipart_uploads(listing)?,
            (
                vec![(
                    "projects/project-1/object&one".to_owned(),
                    "upload&one".to_owned(),
                )],
                true,
            )
        );
        let list_request = provider()?.build_internal_bucket_request(
            "GET",
            &BTreeMap::from([
                ("uploads".to_owned(), String::new()),
                ("prefix".to_owned(), "projects/project-1/object".to_owned()),
            ]),
            1_774_166_400_000,
        )?;
        assert_eq!(list_request.url().path(), "/ffdb-test");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires the repository MinIO service on localhost:9000"]
    async fn minio_round_trip_upload_download_verify_and_delete() -> Result<(), StorageError> {
        let provider = S3Provider::new(
            S3ProviderConfig::new(
                Url::parse("http://localhost:9000/")
                    .map_err(|_| StorageError::InvalidConfiguration)?,
                "us-east-1",
                "ffdb",
                "ffdb-local",
                "ffdb-local-secret-change-me",
            )
            .allow_insecure_localhost(true),
        )?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StorageError::Internal)?
            .as_millis()
            .try_into()
            .map_err(|_| StorageError::Internal)?;
        let body = b"ffdb-sigv4-minio";
        let mut operation = ProviderOperation {
            action: StorageAction::Upload,
            bucket: "integration".to_owned(),
            provider_key: format!("integration/sigv4-{now_ms}.txt"),
            max_bytes: Some(body.len().try_into().map_err(|_| StorageError::Internal)?),
            checksum_sha256: None,
            content_type: Some("text/plain".to_owned()),
            upload_id: None,
            part_number: None,
        };
        let signed = provider.presign_at(&operation, 60_000, now_ms)?;
        let response = reqwest::Client::new()
            .request(reqwest::Method::PUT, signed.url)
            .headers(signed.required_headers.iter().try_fold(
                HeaderMap::new(),
                |mut headers, (name, value)| {
                    headers.insert(
                        HeaderName::from_bytes(name.as_bytes())
                            .map_err(|_| StorageError::Internal)?,
                        HeaderValue::from_str(value).map_err(|_| StorageError::Internal)?,
                    );
                    Ok::<_, StorageError>(headers)
                },
            )?)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| StorageError::Provider)?;
        let upload_status = response.status();
        let upload_error = if upload_status.is_success() {
            String::new()
        } else {
            response.text().await.unwrap_or_default()
        };
        assert!(
            upload_status.is_success(),
            "MinIO upload failed with {upload_status}: {upload_error}"
        );
        let committed = provider.verify_commit(&operation, now_ms).await?;
        assert_eq!(
            committed.content_length,
            Some(body.len().try_into().map_err(|_| StorageError::Internal)?)
        );

        operation.action = StorageAction::Download;
        operation.max_bytes = None;
        operation.content_type = None;
        let signed = provider.presign_at(&operation, 60_000, now_ms)?;
        let downloaded = reqwest::Client::new()
            .get(signed.url)
            .send()
            .await
            .map_err(|_| StorageError::Provider)?;
        if !downloaded.status().is_success() {
            return Err(StorageError::Provider);
        }
        let downloaded = downloaded
            .bytes()
            .await
            .map_err(|_| StorageError::Provider)?;
        assert_eq!(downloaded.as_ref(), body);

        operation.action = StorageAction::Delete;
        let signed = provider.presign_at(&operation, 60_000, now_ms)?;
        let response = reqwest::Client::new()
            .delete(signed.url)
            .send()
            .await
            .map_err(|_| StorageError::Provider)?;
        if !response.status().is_success() {
            return Err(StorageError::Provider);
        }
        provider.verify_commit(&operation, now_ms).await?;
        Ok(())
    }
}
