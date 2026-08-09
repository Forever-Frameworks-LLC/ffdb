//! Precompiled transactional email rendering and Resend delivery.
//!
//! React/JavaScript compilation is deliberately outside this crate and outside
//! request handling. Runtime code accepts a validated artifact and performs only
//! bounded, allowlisted scalar substitution.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use async_trait::async_trait;
use regex::Regex;
use reqwest::{Client, StatusCode, header::HeaderValue, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

mod outbox;
mod smtp;

pub use outbox::{
    EmailEnqueueRequest, EmailMessageCipher, EmailTemplateRecord, OrganizationInvitationRequest,
    OutboxWorkerHandle, PgEmailService, TemplateArtifactInput, kind_name, parse_kind,
};
pub use smtp::SmtpTransport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    EmailVerification,
    PasswordReset,
    EmailChange,
    Invitation,
    MagicLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationJob {
    pub project_id: String,
    pub template_id: String,
    pub kind: TemplateKind,
    pub version: u64,
    pub source: String,
    pub allowed_variables: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationFailure {
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrecompiledTemplate {
    pub project_id: String,
    pub template_id: String,
    pub kind: TemplateKind,
    pub version: u64,
    pub source_sha256: String,
    pub subject_template: String,
    pub html_template: String,
    pub text_template: String,
    pub allowed_variables: BTreeSet<String>,
    pub compiled_at_ms: i64,
}

impl PrecompiledTemplate {
    pub fn validate(&self) -> Result<(), EmailError> {
        if self.project_id.is_empty()
            || self.template_id.is_empty()
            || self.version == 0
            || self.source_sha256.len() != 64
            || self.subject_template.is_empty()
            || self.subject_template.len() > 998
            || self.html_template.is_empty()
            || self.html_template.len() > 1_000_000
            || self.text_template.len() > 500_000
            || self.allowed_variables.len() > 64
        {
            return Err(EmailError::InvalidArtifact);
        }
        let forbidden = Regex::new(
            r#"(?is)<\s*(script|iframe|object|embed|form|base|meta)\b|javascript\s*:|\son[a-z]+\s*="#,
        )
        .map_err(|_| EmailError::InvalidConfiguration)?;
        if forbidden.is_match(&self.html_template)
            || self.html_template.contains("{{{")
            || self.subject_template.contains(['\r', '\n'])
        {
            return Err(EmailError::UnsafeArtifact);
        }
        for template in [
            &self.subject_template,
            &self.html_template,
            &self.text_template,
        ] {
            for variable in variables_in(template)? {
                if !self.allowed_variables.contains(&variable) {
                    return Err(EmailError::UndeclaredVariable(variable));
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
pub trait IsolatedTemplateCompiler: Send + Sync {
    /// This implementation belongs in a locked-down build job, never the API.
    async fn compile(
        &self,
        job: CompilationJob,
    ) -> Result<PrecompiledTemplate, Vec<CompilationFailure>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedEmail {
    pub to: String,
    pub from: String,
    pub reply_to: Option<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub template_id: String,
    pub template_version: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    String(String),
    Number(i64),
    Boolean(bool),
}

impl std::fmt::Display for ScalarValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String(value) => formatter.write_str(value),
            Self::Number(value) => write!(formatter, "{value}"),
            Self::Boolean(value) => write!(formatter, "{value}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeRenderer {
    template: PrecompiledTemplate,
}

impl RuntimeRenderer {
    pub fn new(template: PrecompiledTemplate) -> Result<Self, EmailError> {
        template.validate()?;
        Ok(Self { template })
    }

    pub fn render(
        &self,
        to: impl Into<String>,
        from: impl Into<String>,
        reply_to: Option<String>,
        variables: &BTreeMap<String, ScalarValue>,
        idempotency_key: impl Into<String>,
    ) -> Result<RenderedEmail, EmailError> {
        validate_variable_set(&self.template.allowed_variables, variables)?;
        let subject = substitute(
            &self.template.subject_template,
            variables,
            EscapeMode::Header,
        )?;
        let html = substitute(&self.template.html_template, variables, EscapeMode::Html)?;
        let text = substitute(&self.template.text_template, variables, EscapeMode::Text)?;
        let to = to.into();
        let from = from.into();
        let idempotency_key = idempotency_key.into();
        validate_mailbox(&to)?;
        validate_mailbox(&from)?;
        if let Some(address) = &reply_to {
            validate_mailbox(address)?;
        }
        if idempotency_key.len() < 8 || idempotency_key.len() > 256 {
            return Err(EmailError::InvalidIdempotencyKey);
        }
        Ok(RenderedEmail {
            to,
            from,
            reply_to,
            subject,
            html,
            text,
            template_id: self.template.template_id.clone(),
            template_version: self.template.version,
            idempotency_key,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMessageId(pub String);

#[async_trait]
pub trait EmailTransport: Send + Sync {
    async fn send(&self, message: &RenderedEmail) -> Result<ProviderMessageId, EmailError>;
}

#[derive(Clone)]
pub struct ResendTransport {
    client: Client,
    endpoint: Url,
    api_key: String,
}

impl std::fmt::Debug for ResendTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResendTransport")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl ResendTransport {
    pub async fn new(
        endpoint: Url,
        api_key: impl Into<String>,
        allow_insecure_localhost: bool,
    ) -> Result<Self, EmailError> {
        let host = validate_resend_url(&endpoint, allow_insecure_localhost)?;
        let port = endpoint
            .port_or_known_default()
            .ok_or(EmailError::UnsafeProviderEndpoint)?;
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| EmailError::ProviderDnsFailed)?
            .collect();
        Self::new_with_resolved_addresses(endpoint, api_key, allow_insecure_localhost, &addresses)
    }

    /// Builds a redirect-disabled client pinned to addresses that were validated
    /// immediately before construction, closing the DNS-rebinding gap.
    pub fn new_with_resolved_addresses(
        endpoint: Url,
        api_key: impl Into<String>,
        allow_insecure_localhost: bool,
        resolved_addresses: &[SocketAddr],
    ) -> Result<Self, EmailError> {
        let host = validate_resend_url(&endpoint, allow_insecure_localhost)?;
        let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
        if resolved_addresses.is_empty()
            || resolved_addresses.iter().any(|address| {
                !(is_public_ip(address.ip())
                    || allow_insecure_localhost && local && address.ip().is_loopback())
            })
        {
            return Err(EmailError::UnsafeProviderEndpoint);
        }
        let api_key = api_key.into();
        if api_key.len() < 16 || HeaderValue::from_str(&format!("Bearer {api_key}")).is_err() {
            return Err(EmailError::InvalidConfiguration);
        }
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .resolve_to_addrs(&host, resolved_addresses);
        if endpoint.scheme() == "https" {
            builder = builder.https_only(true);
        }
        let client = builder
            .build()
            .map_err(|_| EmailError::InvalidConfiguration)?;
        Ok(Self {
            client,
            endpoint,
            api_key,
        })
    }
}

fn validate_resend_url(
    endpoint: &Url,
    allow_insecure_localhost: bool,
) -> Result<String, EmailError> {
    let host = endpoint
        .host_str()
        .ok_or(EmailError::UnsafeProviderEndpoint)?
        .to_owned();
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    if endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
        || (host != "api.resend.com" && !(allow_insecure_localhost && local))
        || (endpoint.scheme() != "https" && !(allow_insecure_localhost && local))
    {
        return Err(EmailError::UnsafeProviderEndpoint);
    }
    Ok(host)
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

#[derive(Serialize)]
struct ResendRequest<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'a str,
    html: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<&'a str>,
    headers: BTreeMap<&'static str, String>,
}

#[derive(Deserialize)]
struct ResendResponse {
    id: String,
}

#[async_trait]
impl EmailTransport for ResendTransport {
    async fn send(&self, message: &RenderedEmail) -> Result<ProviderMessageId, EmailError> {
        let url = self
            .endpoint
            .join("emails")
            .map_err(|_| EmailError::InvalidConfiguration)?;
        let request = ResendRequest {
            from: &message.from,
            to: [&message.to],
            subject: &message.subject,
            html: &message.html,
            text: &message.text,
            reply_to: message.reply_to.as_deref(),
            headers: BTreeMap::from([(
                "X-FFDB-Template",
                format!("{}:v{}", message.template_id, message.template_version),
            )]),
        };
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("Idempotency-Key", &message.idempotency_key)
            .json(&request)
            .send()
            .await
            .map_err(|_| EmailError::ProviderUnavailable)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(EmailError::ProviderRateLimited);
        }
        if !response.status().is_success() {
            return Err(EmailError::ProviderRejected);
        }
        let response: ResendResponse = response
            .json()
            .await
            .map_err(|_| EmailError::InvalidProviderResponse)?;
        if response.id.is_empty() || response.id.len() > 256 {
            return Err(EmailError::InvalidProviderResponse);
        }
        Ok(ProviderMessageId(response.id))
    }
}

pub fn default_template(
    project_id: impl Into<String>,
    kind: TemplateKind,
    compiled_at_ms: i64,
) -> PrecompiledTemplate {
    let (template_id, subject, heading, action, explanatory) = match kind {
        TemplateKind::EmailVerification => (
            "email-verification",
            "Verify your {{project_name}} email",
            "Verify your email",
            "Verify email",
            "Confirm this address to finish creating your account.",
        ),
        TemplateKind::PasswordReset => (
            "password-reset",
            "Reset your {{project_name}} password",
            "Reset your password",
            "Reset password",
            "Use this secure link to choose a new password.",
        ),
        TemplateKind::EmailChange => (
            "email-change",
            "Confirm your new {{project_name}} email",
            "Confirm your new email",
            "Confirm email",
            "Confirm this address to complete your email change.",
        ),
        TemplateKind::Invitation => (
            "invitation",
            "You were invited to {{project_name}}",
            "Join {{project_name}}",
            "Accept invitation",
            "You have been invited to collaborate.",
        ),
        TemplateKind::MagicLink => (
            "magic-link",
            "Sign in to {{project_name}}",
            "Sign in securely",
            "Sign in",
            "Use this one-time link to continue.",
        ),
    };
    let html_template = format!(
        "<!doctype html><html><body style=\"margin:0;background:#f7f8fa;font-family:Arial,sans-serif;color:#101828\"><main style=\"max-width:560px;margin:40px auto;padding:32px;background:#fff;border:1px solid #d6dce5\"><h1>{heading}</h1><p>{explanatory}</p><p><a href=\"{{{{action_url}}}}\" style=\"display:inline-block;padding:12px 18px;background:#0868e8;color:#fff;text-decoration:none;border-radius:5px\">{action}</a></p><p style=\"color:#4b5565;font-size:13px\">This link expires in {{{{expires_in}}}}.</p></main></body></html>"
    );
    let text_template = format!(
        "{heading}\n\n{explanatory}\n\n{{{{action_url}}}}\n\nThis link expires in {{{{expires_in}}}}."
    );
    let source_sha256 = hex::encode(Sha256::digest(template_id.as_bytes()));
    PrecompiledTemplate {
        project_id: project_id.into(),
        template_id: template_id.to_owned(),
        kind,
        version: 1,
        source_sha256,
        subject_template: subject.to_owned(),
        html_template,
        text_template,
        allowed_variables: BTreeSet::from([
            "action_url".to_owned(),
            "expires_in".to_owned(),
            "project_name".to_owned(),
        ]),
        compiled_at_ms,
    }
}

#[derive(Clone, Copy)]
enum EscapeMode {
    Header,
    Html,
    Text,
}

fn substitute(
    template: &str,
    variables: &BTreeMap<String, ScalarValue>,
    mode: EscapeMode,
) -> Result<String, EmailError> {
    let marker = Regex::new(r"\{\{([a-zA-Z][a-zA-Z0-9_]*)\}\}")
        .map_err(|_| EmailError::InvalidConfiguration)?;
    let mut rendered = String::with_capacity(template.len());
    let mut offset = 0;
    for capture in marker.captures_iter(template) {
        let whole = capture.get(0).ok_or(EmailError::InvalidArtifact)?;
        let name = capture.get(1).ok_or(EmailError::InvalidArtifact)?.as_str();
        rendered.push_str(&template[offset..whole.start()]);
        let value = variables
            .get(name)
            .ok_or_else(|| EmailError::MissingVariable(name.to_owned()))?
            .to_string();
        rendered.push_str(&escape(&value, mode)?);
        offset = whole.end();
    }
    rendered.push_str(&template[offset..]);
    Ok(rendered)
}

fn variables_in(template: &str) -> Result<BTreeSet<String>, EmailError> {
    let marker = Regex::new(r"\{\{([a-zA-Z][a-zA-Z0-9_]*)\}\}")
        .map_err(|_| EmailError::InvalidConfiguration)?;
    Ok(marker
        .captures_iter(template)
        .filter_map(|capture| capture.get(1).map(|name| name.as_str().to_owned()))
        .collect())
}

fn validate_variable_set(
    allowed: &BTreeSet<String>,
    variables: &BTreeMap<String, ScalarValue>,
) -> Result<(), EmailError> {
    if let Some(unknown) = variables.keys().find(|name| !allowed.contains(*name)) {
        return Err(EmailError::UndeclaredVariable(unknown.clone()));
    }
    if let Some(missing) = allowed.iter().find(|name| !variables.contains_key(*name)) {
        return Err(EmailError::MissingVariable(missing.clone()));
    }
    Ok(())
}

fn escape(value: &str, mode: EscapeMode) -> Result<String, EmailError> {
    if value.len() > 16_384 || value.contains('\0') {
        return Err(EmailError::InvalidVariableValue);
    }
    match mode {
        EscapeMode::Html => Ok(value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")),
        EscapeMode::Header => {
            if value.contains(['\r', '\n']) {
                Err(EmailError::HeaderInjection)
            } else {
                Ok(value.to_owned())
            }
        }
        EscapeMode::Text => Ok(value.to_owned()),
    }
}

fn validate_mailbox(address: &str) -> Result<(), EmailError> {
    if address.len() > 320
        || address.contains(['\r', '\n', '\0'])
        || !address
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
    {
        return Err(EmailError::InvalidMailbox);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EmailError {
    #[error("compiled template artifact is invalid")]
    InvalidArtifact,
    #[error("compiled template artifact contains unsafe markup")]
    UnsafeArtifact,
    #[error("template references undeclared variable {0}")]
    UndeclaredVariable(String),
    #[error("template variable {0} is missing")]
    MissingVariable(String),
    #[error("template variable value is invalid")]
    InvalidVariableValue,
    #[error("header value contains a line break")]
    HeaderInjection,
    #[error("email mailbox is invalid")]
    InvalidMailbox,
    #[error("email idempotency key is invalid")]
    InvalidIdempotencyKey,
    #[error("provider endpoint is unsafe")]
    UnsafeProviderEndpoint,
    #[error("email provider DNS resolution failed")]
    ProviderDnsFailed,
    #[error("email configuration is invalid")]
    InvalidConfiguration,
    #[error("email provider is unavailable")]
    ProviderUnavailable,
    #[error("email provider rate limited the request")]
    ProviderRateLimited,
    #[error("email provider rejected the request")]
    ProviderRejected,
    #[error("email provider returned an invalid response")]
    InvalidProviderResponse,
    #[error("email template version already exists")]
    TemplateVersionExists,
    #[error("email template was not found")]
    TemplateNotFound,
    #[error("email outbox encryption failed")]
    Encryption,
    #[error("email control-plane repository is unavailable")]
    RepositoryUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variables() -> BTreeMap<String, ScalarValue> {
        BTreeMap::from([
            (
                "project_name".to_owned(),
                ScalarValue::String("Atlas".to_owned()),
            ),
            (
                "action_url".to_owned(),
                ScalarValue::String("https://example.test/a?x=1&y=<unsafe>".to_owned()),
            ),
            (
                "expires_in".to_owned(),
                ScalarValue::String("30 minutes".to_owned()),
            ),
        ])
    }

    #[test]
    fn runtime_renders_only_precompiled_artifacts_and_html_escapes_values() -> Result<(), EmailError>
    {
        let artifact = default_template("project-1", TemplateKind::PasswordReset, 1_000);
        let renderer = RuntimeRenderer::new(artifact)?;
        let email = renderer.render(
            "user@example.test",
            "Atlas <auth@example.test>",
            None,
            &variables(),
            "reset-0123456789",
        )?;
        assert!(email.subject.contains("Atlas"));
        assert!(email.html.contains("&amp;y=&lt;unsafe&gt;"));
        assert!(!email.html.contains("<unsafe>"));
        assert!(email.text.contains("&y=<unsafe>"));
        Ok(())
    }

    #[test]
    fn unsafe_compiler_output_is_rejected() {
        let mut artifact = default_template("project-1", TemplateKind::Invitation, 1_000);
        artifact.html_template.push_str("<script>alert(1)</script>");
        assert!(matches!(
            RuntimeRenderer::new(artifact),
            Err(EmailError::UnsafeArtifact)
        ));
    }

    #[test]
    fn unknown_and_missing_variables_fail_closed() -> Result<(), EmailError> {
        let renderer = RuntimeRenderer::new(default_template(
            "project-1",
            TemplateKind::EmailVerification,
            1_000,
        ))?;
        let mut values = variables();
        values.insert("admin".to_owned(), ScalarValue::Boolean(true));
        assert_eq!(
            renderer.render(
                "user@example.test",
                "auth@example.test",
                None,
                &values,
                "verify-0123456789"
            ),
            Err(EmailError::UndeclaredVariable("admin".to_owned()))
        );
        values.remove("admin");
        values.remove("action_url");
        assert_eq!(
            renderer.render(
                "user@example.test",
                "auth@example.test",
                None,
                &values,
                "verify-0123456789"
            ),
            Err(EmailError::MissingVariable("action_url".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn remote_provider_is_allowlisted_pinned_and_debug_redacts_secret() -> Result<(), EmailError> {
        let http = Url::parse("http://resend.example.test/")
            .map_err(|_| EmailError::InvalidConfiguration)?;
        assert!(matches!(
            ResendTransport::new_with_resolved_addresses(
                http,
                "re_0123456789abcdef",
                false,
                &["8.8.8.8:443"
                    .parse()
                    .map_err(|_| EmailError::InvalidConfiguration)?]
            ),
            Err(EmailError::UnsafeProviderEndpoint)
        ));
        let https =
            Url::parse("https://api.resend.com/").map_err(|_| EmailError::InvalidConfiguration)?;
        assert!(matches!(
            ResendTransport::new_with_resolved_addresses(
                https.clone(),
                "re_0123456789abcdef",
                false,
                &["169.254.169.254:443"
                    .parse()
                    .map_err(|_| EmailError::InvalidConfiguration)?]
            ),
            Err(EmailError::UnsafeProviderEndpoint)
        ));
        let transport = ResendTransport::new_with_resolved_addresses(
            https,
            "re_0123456789abcdef",
            false,
            &["8.8.8.8:443"
                .parse()
                .map_err(|_| EmailError::InvalidConfiguration)?],
        )?;
        let debug = format!("{transport:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("re_0123456789abcdef"));
        Ok(())
    }
}
