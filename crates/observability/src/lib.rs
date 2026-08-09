//! Bounded-cardinality metrics, request tracing, and structured secret redaction.

use std::collections::BTreeMap;
use std::fmt;

use prometheus::{
    Encoder as _, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};
use serde_json::Value;
use tracing::Span;
use zeroize::Zeroizing;

const REDACTED: &str = "[REDACTED]";

#[derive(Clone)]
pub struct SecretText(Zeroizing<String>);

impl SecretText {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretText([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalRequestId(String);

impl ExternalRequestId {
    pub fn parse(value: &str) -> Result<Self, ObservabilityError> {
        if !(16..=64).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(ObservabilityError::InvalidRequestId);
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct SafeFields(BTreeMap<String, Value>);

impl SafeFields {
    pub fn from_value(value: Value) -> Result<Self, ObservabilityError> {
        let Value::Object(mut fields) = value else {
            return Err(ObservabilityError::InvalidFields);
        };
        let mut nodes = 0_usize;
        for (key, value) in &mut fields {
            redact_value(Some(key), value, 0, &mut nodes)?;
        }
        let encoded = serde_json::to_vec(&fields).map_err(|_| ObservabilityError::InvalidFields)?;
        if encoded.len() > 32 * 1024 {
            return Err(ObservabilityError::InvalidFields);
        }
        Ok(Self(fields.into_iter().collect()))
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, Value> {
        &self.0
    }
}

/// Returns a safe URL representation for logs. Credentials, query parameters,
/// and fragments are removed without attempting network access.
#[must_use]
pub fn redact_url(raw: &str) -> String {
    let without_fragment = raw.split('#').next().unwrap_or_default();
    let without_query = without_fragment.split('?').next().unwrap_or_default();
    if let Some((scheme, rest)) = without_query.split_once("://") {
        let authority_and_path = rest.rsplit_once('@').map_or(rest, |(_, safe)| safe);
        format!("{scheme}://{authority_and_path}")
    } else {
        without_query.to_owned()
    }
}

/// Header allowlist for structured logs. Values of sensitive headers are always
/// replaced even if a future caller accidentally includes them.
#[must_use]
pub fn safe_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeMap<String, String> {
    let mut safe = BTreeMap::new();
    for (name, value) in headers {
        let normalized = name.to_ascii_lowercase();
        if is_sensitive_key(&normalized) {
            safe.insert(normalized, REDACTED.into());
        } else if matches!(
            normalized.as_str(),
            "user-agent" | "content-type" | "x-request-id"
        ) {
            safe.insert(normalized, truncate(value, 512));
        }
    }
    safe
}

#[derive(Clone, Debug)]
pub struct RequestTrace {
    pub request_id: String,
    pub method: String,
    /// A static route template (for example `/v1/projects/:id/query`), never a
    /// raw URI containing IDs or query parameters.
    pub route: String,
}

/// Injection seam for deployments that compose `tracing` spans with their own
/// subscriber/layer stack (including an OTLP layer maintained by the deployer).
/// FFDB itself does not claim an OpenTelemetry exporter unless such a layer is
/// installed by the process composition.
pub trait RequestSpanFactory: Send + Sync {
    fn request_span(&self, trace: &RequestTrace) -> Span;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultSubscriberSpanFactory;

impl RequestSpanFactory for DefaultSubscriberSpanFactory {
    fn request_span(&self, trace: &RequestTrace) -> Span {
        tracing::info_span!(
            "http.request",
            request_id = %trace.request_id,
            method = %trace.method,
            route = %trace.route,
        )
    }
}

impl RequestTrace {
    #[must_use]
    pub fn span(&self) -> Span {
        self.span_with(&DefaultSubscriberSpanFactory)
    }

    #[must_use]
    pub fn span_with(&self, factory: &dyn RequestSpanFactory) -> Span {
        factory.request_span(self)
    }
}

#[derive(Clone, Debug)]
pub struct Metrics {
    registry: Registry,
    requests: IntCounterVec,
    duration: HistogramVec,
    inflight: IntGauge,
    auth_failures: IntCounterVec,
    rate_limit_denials: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Result<Self, ObservabilityError> {
        let registry = Registry::new();
        let requests = IntCounterVec::new(
            Opts::new(
                "ffdb_http_requests_total",
                "HTTP requests by stable route/status class",
            ),
            &["method", "route", "status_class"],
        )
        .map_err(|_| ObservabilityError::Metrics)?;
        let duration = HistogramVec::new(
            HistogramOpts::new("ffdb_http_request_duration_seconds", "HTTP request latency")
                .buckets(vec![
                    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0,
                ]),
            &["method", "route"],
        )
        .map_err(|_| ObservabilityError::Metrics)?;
        let inflight = IntGauge::new(
            "ffdb_http_requests_inflight",
            "Current in-flight HTTP requests",
        )
        .map_err(|_| ObservabilityError::Metrics)?;
        let auth_failures = IntCounterVec::new(
            Opts::new(
                "ffdb_auth_failures_total",
                "Authentication failures by safe reason",
            ),
            &["kind", "reason"],
        )
        .map_err(|_| ObservabilityError::Metrics)?;
        let rate_limit_denials = IntCounterVec::new(
            Opts::new(
                "ffdb_rate_limit_denials_total",
                "Rate-limit denials by dimension",
            ),
            &["dimension"],
        )
        .map_err(|_| ObservabilityError::Metrics)?;
        registry
            .register(Box::new(requests.clone()))
            .and_then(|()| registry.register(Box::new(duration.clone())))
            .and_then(|()| registry.register(Box::new(inflight.clone())))
            .and_then(|()| registry.register(Box::new(auth_failures.clone())))
            .and_then(|()| registry.register(Box::new(rate_limit_denials.clone())))
            .map_err(|_| ObservabilityError::Metrics)?;
        Ok(Self {
            registry,
            requests,
            duration,
            inflight,
            auth_failures,
            rate_limit_denials,
        })
    }

    pub fn observe_request(
        &self,
        method: &'static str,
        route: &'static str,
        status: u16,
        duration_seconds: f64,
    ) -> Result<(), ObservabilityError> {
        if !duration_seconds.is_finite() || duration_seconds < 0.0 {
            return Err(ObservabilityError::Metrics);
        }
        let status_class = match status {
            100..=199 => "1xx",
            200..=299 => "2xx",
            300..=399 => "3xx",
            400..=499 => "4xx",
            500..=599 => "5xx",
            _ => return Err(ObservabilityError::Metrics),
        };
        self.requests
            .with_label_values(&[method, route, status_class])
            .inc();
        self.duration
            .with_label_values(&[method, route])
            .observe(duration_seconds);
        Ok(())
    }

    pub fn request_started(&self) {
        self.inflight.inc();
    }

    pub fn request_finished(&self) {
        self.inflight.dec();
    }

    #[must_use]
    pub fn inflight(&self) -> u64 {
        u64::try_from(self.inflight.get()).unwrap_or_default()
    }

    pub fn authentication_failed(&self, kind: AuthKind, reason: AuthFailureReason) {
        self.auth_failures
            .with_label_values(&[kind.name(), reason.name()])
            .inc();
    }

    pub fn rate_limit_denied(&self, dimension: RateLimitDimension) {
        self.rate_limit_denials
            .with_label_values(&[dimension.name()])
            .inc();
    }

    pub fn encode_prometheus(&self) -> Result<Vec<u8>, ObservabilityError> {
        let families = self.registry.gather();
        let mut output = Vec::new();
        TextEncoder::new()
            .encode(&families, &mut output)
            .map_err(|_| ObservabilityError::Metrics)?;
        Ok(output)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AuthKind {
    ApiKey,
    AccessToken,
    RefreshToken,
    Password,
}

impl AuthKind {
    fn name(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::AccessToken => "access_token",
            Self::RefreshToken => "refresh_token",
            Self::Password => "password",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AuthFailureReason {
    Missing,
    Invalid,
    Expired,
    WrongProject,
    InsufficientScope,
    Disabled,
}

impl AuthFailureReason {
    fn name(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::WrongProject => "wrong_project",
            Self::InsufficientScope => "insufficient_scope",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RateLimitDimension {
    Ip,
    Project,
    User,
    ApiKey,
}

impl RateLimitDimension {
    fn name(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Project => "project",
            Self::User => "user",
            Self::ApiKey => "api_key",
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ObservabilityError {
    #[error("request id is invalid")]
    InvalidRequestId,
    #[error("structured fields are invalid")]
    InvalidFields,
    #[error("metrics are unavailable")]
    Metrics,
}

fn redact_value(
    key: Option<&str>,
    value: &mut Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), ObservabilityError> {
    *nodes = nodes
        .checked_add(1)
        .ok_or(ObservabilityError::InvalidFields)?;
    if depth > 8 || *nodes > 512 {
        return Err(ObservabilityError::InvalidFields);
    }
    if key.is_some_and(is_sensitive_key) {
        *value = Value::String(REDACTED.into());
        return Ok(());
    }
    match value {
        Value::Array(values) => {
            for value in values {
                redact_value(None, value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                redact_value(Some(key), value, depth + 1, nodes)?;
            }
        }
        Value::String(value) if value.len() > 2048 => {
            return Err(ObservabilityError::InvalidFields);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    normalized == "authorization"
        || normalized == "cookie"
        || normalized == "set_cookie"
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("api_key")
        || normalized.contains("private_key")
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &value[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn structured_fields_recursively_redact_secrets() -> Result<(), ObservabilityError> {
        let fields = SafeFields::from_value(json!({
            "request_id": "safe",
            "Authorization": "Bearer plaintext",
            "nested": {"refresh-token": "plaintext", "visible": 1}
        }))?;
        assert_eq!(
            fields.get("Authorization"),
            Some(&Value::String(REDACTED.into()))
        );
        assert_eq!(
            fields
                .get("nested")
                .and_then(|value| value.get("refresh-token")),
            Some(&Value::String(REDACTED.into()))
        );
        assert!(!format!("{fields:?}").contains("plaintext"));
        Ok(())
    }

    #[test]
    fn url_and_headers_do_not_leak_bearers() {
        assert_eq!(
            redact_url("https://user:pass@example.test/path?access_token=plaintext#fragment"),
            "https://example.test/path"
        );
        let headers = safe_headers([
            ("Authorization", "Bearer plaintext"),
            ("User-Agent", "test"),
            ("X-Untrusted", "not logged"),
        ]);
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some(REDACTED)
        );
        assert!(!headers.values().any(|value| value.contains("plaintext")));
    }

    #[test]
    fn metrics_use_only_bounded_labels() -> Result<(), ObservabilityError> {
        let metrics = Metrics::new()?;
        metrics.observe_request("POST", "/v1/projects/:id/query", 429, 0.01)?;
        metrics.authentication_failed(AuthKind::ApiKey, AuthFailureReason::Invalid);
        metrics.rate_limit_denied(RateLimitDimension::Project);
        let output = String::from_utf8(metrics.encode_prometheus()?)
            .map_err(|_| ObservabilityError::Metrics)?;
        assert!(output.contains("ffdb_http_requests_total"));
        assert!(output.contains("status_class=\"4xx\""));
        Ok(())
    }

    #[test]
    fn request_span_factory_is_an_explicit_subscriber_extension_seam() {
        struct TestFactory(Arc<AtomicBool>);

        impl RequestSpanFactory for TestFactory {
            fn request_span(&self, trace: &RequestTrace) -> Span {
                assert_eq!(trace.route, "/v1/projects/:id/query");
                self.0.store(true, Ordering::Relaxed);
                Span::none()
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let trace = RequestTrace {
            request_id: "request_123456789".into(),
            method: "POST".into(),
            route: "/v1/projects/:id/query".into(),
        };
        let _span = trace.span_with(&TestFactory(called.clone()));
        assert!(called.load(Ordering::Relaxed));
    }
}
