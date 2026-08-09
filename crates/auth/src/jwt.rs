use std::fmt;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use ffdb_protocol::{AuthContext, ProjectId, SessionId, TokenId, UserId};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{MAX_BEARER_TOKEN_BYTES, SecretToken};

const MAX_CLOCK_SKEW_SECONDS: i64 = 30;
const MAX_ACCESS_TOKEN_LIFETIME_SECONDS: i64 = 15 * 60;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JwtHeader {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessTokenClaims {
    pub iss: String,
    pub aud: String,
    pub sub: UserId,
    pub project_id: ProjectId,
    pub jti: TokenId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<SessionId>,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub role: String,
    #[serde(default)]
    pub claims: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct ExpectedAccessToken<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    /// Trusted project selected from the request path/configuration, not from
    /// the unverified JWT payload.
    pub project_id: ProjectId,
    pub now_seconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationKeyStatus {
    Active,
    Grace,
    Revoked,
}

#[derive(Clone)]
pub struct VerificationKey {
    pub project_id: ProjectId,
    pub kid: String,
    pub public_key: VerifyingKey,
    pub status: VerificationKeyStatus,
    pub valid_from_seconds: i64,
    pub valid_until_seconds: Option<i64>,
}

impl fmt::Debug for VerificationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationKey")
            .field("project_id", &self.project_id)
            .field("kid", &self.kid)
            .field("status", &self.status)
            .field("valid_from_seconds", &self.valid_from_seconds)
            .field("valid_until_seconds", &self.valid_until_seconds)
            .finish_non_exhaustive()
    }
}

pub struct ProjectSigner {
    project_id: ProjectId,
    kid: String,
    key: SigningKey,
}

impl fmt::Debug for ProjectSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectSigner")
            .field("project_id", &self.project_id)
            .field("kid", &self.kid)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl ProjectSigner {
    pub fn generate(project_id: ProjectId, kid: String) -> Result<Self, JwtError> {
        validate_kid(&kid)?;
        Ok(Self {
            project_id,
            kid,
            key: SigningKey::generate(&mut OsRng),
        })
    }

    pub fn from_bytes(
        project_id: ProjectId,
        kid: String,
        private_key: &[u8; 32],
    ) -> Result<Self, JwtError> {
        validate_kid(&kid)?;
        Ok(Self {
            project_id,
            kid,
            key: SigningKey::from_bytes(private_key),
        })
    }

    pub(crate) fn export_private_key(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.key.to_bytes())
    }

    #[must_use]
    pub fn verification_key(
        &self,
        status: VerificationKeyStatus,
        valid_from_seconds: i64,
        valid_until_seconds: Option<i64>,
    ) -> VerificationKey {
        VerificationKey {
            project_id: self.project_id,
            kid: self.kid.clone(),
            public_key: self.key.verifying_key(),
            status,
            valid_from_seconds,
            valid_until_seconds,
        }
    }

    pub fn sign(&self, claims: &AccessTokenClaims) -> Result<SecretToken, JwtError> {
        if claims.project_id != self.project_id
            || claims.exp <= claims.iat
            || claims.nbf > claims.exp
            || claims.exp - claims.iat > MAX_ACCESS_TOKEN_LIFETIME_SECONDS
        {
            return Err(JwtError::InvalidClaims);
        }
        validate_claim_strings(claims)?;
        let header = JwtHeader {
            alg: "EdDSA".into(),
            typ: "JWT".into(),
            kid: self.kid.clone(),
        };
        let header =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).map_err(|_| JwtError::Encoding)?);
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(|_| JwtError::Encoding)?);
        let signing_input = format!("{header}.{payload}");
        let signature = self.key.sign(signing_input.as_bytes());
        Ok(SecretToken::new(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )))
    }
}

#[async_trait]
pub trait SigningKeyStore: Send + Sync {
    async fn active_signer(&self, project_id: ProjectId) -> Result<ProjectSigner, JwtError>;
    async fn verification_keys(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<VerificationKey>, JwtError>;
}

#[derive(Clone, Debug)]
pub struct JwtIssuer {
    issuer: String,
    audience: String,
}

#[derive(Clone, Copy, Debug)]
pub struct AccessTokenSessionPolicy {
    pub session_id: Option<SessionId>,
    pub now_seconds: i64,
    pub ttl_seconds: i64,
}

impl JwtIssuer {
    pub fn new(issuer: String, audience: String) -> Result<Self, JwtError> {
        if issuer.is_empty() || issuer.len() > 256 || audience.is_empty() || audience.len() > 128 {
            return Err(JwtError::InvalidClaims);
        }
        Ok(Self { issuer, audience })
    }

    pub fn claims(
        &self,
        project_id: ProjectId,
        subject: UserId,
        role: String,
        custom_claims: Map<String, Value>,
        now_seconds: i64,
    ) -> Result<AccessTokenClaims, JwtError> {
        self.claims_for_session(project_id, subject, role, custom_claims, None, now_seconds)
    }

    pub fn claims_for_session(
        &self,
        project_id: ProjectId,
        subject: UserId,
        role: String,
        custom_claims: Map<String, Value>,
        session_id: Option<SessionId>,
        now_seconds: i64,
    ) -> Result<AccessTokenClaims, JwtError> {
        self.claims_with_session_policy(
            project_id,
            subject,
            role,
            custom_claims,
            AccessTokenSessionPolicy {
                session_id,
                now_seconds,
                ttl_seconds: MAX_ACCESS_TOKEN_LIFETIME_SECONDS,
            },
        )
    }

    pub fn claims_with_session_policy(
        &self,
        project_id: ProjectId,
        subject: UserId,
        role: String,
        custom_claims: Map<String, Value>,
        policy: AccessTokenSessionPolicy,
    ) -> Result<AccessTokenClaims, JwtError> {
        if !(60..=MAX_ACCESS_TOKEN_LIFETIME_SECONDS).contains(&policy.ttl_seconds) {
            return Err(JwtError::InvalidClaims);
        }
        let claims = AccessTokenClaims {
            iss: self.issuer.clone(),
            aud: self.audience.clone(),
            sub: subject,
            project_id,
            jti: TokenId::new(),
            sid: policy.session_id,
            iat: policy.now_seconds,
            nbf: policy.now_seconds,
            exp: policy
                .now_seconds
                .checked_add(policy.ttl_seconds)
                .ok_or(JwtError::InvalidClaims)?,
            role,
            claims: custom_claims,
        };
        validate_claim_strings(&claims)?;
        Ok(claims)
    }

    pub fn verify(
        &self,
        token: &str,
        expected_project: ProjectId,
        keys: &[VerificationKey],
        now_seconds: i64,
    ) -> Result<VerifiedAccessToken, JwtError> {
        verify_access_token(
            token,
            &ExpectedAccessToken {
                issuer: &self.issuer,
                audience: &self.audience,
                project_id: expected_project,
                now_seconds,
            },
            keys,
        )
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedAccessToken {
    context: AuthContext,
    session_id: Option<SessionId>,
    issued_at: i64,
    expires_at: i64,
    kid: String,
}

impl VerifiedAccessToken {
    #[must_use]
    pub fn context(&self) -> &AuthContext {
        &self.context
    }

    #[must_use]
    pub fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    #[must_use]
    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    #[must_use]
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum JwtError {
    #[error("access token is malformed")]
    Malformed,
    #[error("access token uses an unsupported algorithm")]
    UnsupportedAlgorithm,
    #[error("access token key id is unknown or inactive")]
    UnknownKey,
    #[error("access token signature is invalid")]
    InvalidSignature,
    #[error("access token claims are invalid")]
    InvalidClaims,
    #[error("access token is not currently valid")]
    TimeInvalid,
    #[error("access token encoding failed")]
    Encoding,
    #[error("signing key store is unavailable")]
    KeyStoreUnavailable,
}

pub fn verify_access_token(
    token: &str,
    expected: &ExpectedAccessToken<'_>,
    keys: &[VerificationKey],
) -> Result<VerifiedAccessToken, JwtError> {
    if token.is_empty() || token.len() > MAX_BEARER_TOKEN_BYTES {
        return Err(JwtError::Malformed);
    }
    let mut segments = token.split('.');
    let header_segment = segments.next().ok_or(JwtError::Malformed)?;
    let payload_segment = segments.next().ok_or(JwtError::Malformed)?;
    let signature_segment = segments.next().ok_or(JwtError::Malformed)?;
    if segments.next().is_some()
        || [header_segment, payload_segment, signature_segment].contains(&"")
    {
        return Err(JwtError::Malformed);
    }

    let header_bytes = decode_segment(header_segment, 1024)?;
    let header: JwtHeader =
        serde_json::from_slice(&header_bytes).map_err(|_| JwtError::Malformed)?;
    if header.alg != "EdDSA" || header.typ != "JWT" {
        return Err(JwtError::UnsupportedAlgorithm);
    }
    validate_kid(&header.kid)?;

    // The caller supplies keys already scoped by a trusted project path. The
    // unverified payload is never used to select either a project or key store.
    let key = keys
        .iter()
        .find(|key| key.project_id == expected.project_id && key.kid == header.kid)
        .ok_or(JwtError::UnknownKey)?;
    if key.status == VerificationKeyStatus::Revoked
        || expected.now_seconds + MAX_CLOCK_SKEW_SECONDS < key.valid_from_seconds
        || key
            .valid_until_seconds
            .is_some_and(|end| expected.now_seconds - MAX_CLOCK_SKEW_SECONDS > end)
    {
        return Err(JwtError::UnknownKey);
    }

    let signature_bytes = decode_segment(signature_segment, 64)?;
    let signature = Signature::from_slice(&signature_bytes).map_err(|_| JwtError::Malformed)?;
    let signing_input = format!("{header_segment}.{payload_segment}");
    key.public_key
        .verify(signing_input.as_bytes(), &signature)
        .map_err(|_| JwtError::InvalidSignature)?;

    // Claims are decoded only after signature verification.
    let payload_bytes = decode_segment(payload_segment, MAX_BEARER_TOKEN_BYTES)?;
    let claims: AccessTokenClaims =
        serde_json::from_slice(&payload_bytes).map_err(|_| JwtError::InvalidClaims)?;
    validate_claim_strings(&claims)?;
    if claims.iss != expected.issuer
        || claims.aud != expected.audience
        || claims.project_id != expected.project_id
        || claims.sub.0.is_nil()
        || claims.jti.0.is_nil()
    {
        return Err(JwtError::InvalidClaims);
    }
    if claims.exp <= claims.iat
        || claims.nbf > claims.exp
        || claims.exp - claims.iat > MAX_ACCESS_TOKEN_LIFETIME_SECONDS
        || claims.iat > expected.now_seconds + MAX_CLOCK_SKEW_SECONDS
        || claims.nbf > expected.now_seconds + MAX_CLOCK_SKEW_SECONDS
        || claims.exp <= expected.now_seconds - MAX_CLOCK_SKEW_SECONDS
    {
        return Err(JwtError::TimeInvalid);
    }

    Ok(VerifiedAccessToken {
        context: AuthContext {
            project_id: claims.project_id,
            subject: claims.sub,
            role: claims.role,
            claims: claims.claims,
            token_id: claims.jti,
        },
        session_id: claims.sid,
        issued_at: claims.iat,
        expires_at: claims.exp,
        kid: header.kid,
    })
}

fn validate_kid(kid: &str) -> Result<(), JwtError> {
    if !(8..=64).contains(&kid.len())
        || !kid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(JwtError::UnknownKey);
    }
    Ok(())
}

fn validate_claim_strings(claims: &AccessTokenClaims) -> Result<(), JwtError> {
    if claims.iss.is_empty()
        || claims.iss.len() > 256
        || claims.aud.is_empty()
        || claims.aud.len() > 128
        || claims.role.is_empty()
        || claims.role.len() > 64
        || !claims
            .role
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
        || claims.claims.len() > 64
    {
        return Err(JwtError::InvalidClaims);
    }
    for (name, value) in &claims.claims {
        if name.is_empty() || name.len() > 64 || !is_bounded_json(value, 0) {
            return Err(JwtError::InvalidClaims);
        }
    }
    Ok(())
}

fn is_bounded_json(value: &Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(value) => value.len() <= 1024,
        Value::Array(values) => {
            values.len() <= 32 && values.iter().all(|value| is_bounded_json(value, depth + 1))
        }
        Value::Object(values) => {
            values.len() <= 32
                && values
                    .iter()
                    .all(|(key, value)| key.len() <= 64 && is_bounded_json(value, depth + 1))
        }
    }
}

fn decode_segment(segment: &str, max_decoded_bytes: usize) -> Result<Vec<u8>, JwtError> {
    if segment.len() > max_decoded_bytes.saturating_mul(2) {
        return Err(JwtError::Malformed);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| JwtError::Malformed)?;
    if decoded.len() > max_decoded_bytes || URL_SAFE_NO_PAD.encode(&decoded) != segment {
        return Err(JwtError::Malformed);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issuer() -> Result<JwtIssuer, JwtError> {
        JwtIssuer::new("https://auth.ffdb.test".into(), "ffdb-project".into())
    }

    #[test]
    fn verifies_scoped_eddsa_token() -> Result<(), JwtError> {
        let project = ProjectId::new();
        let signer = ProjectSigner::generate(project, "key_2026_01".into())?;
        let claims = issuer()?.claims(
            project,
            UserId::new(),
            "authenticated".into(),
            Map::new(),
            1000,
        )?;
        let token = signer.sign(&claims)?;
        let key = signer.verification_key(VerificationKeyStatus::Active, 900, None);
        let verified = issuer()?.verify(token.expose(), project, &[key], 1001)?;
        assert_eq!(verified.context().project_id, project);
        assert_eq!(verified.kid(), "key_2026_01");
        assert!(!format!("{token:?}").contains("eyJ"));
        Ok(())
    }

    #[test]
    fn configured_ttl_and_signed_session_id_round_trip() -> Result<(), JwtError> {
        let project = ProjectId::new();
        let session_id = SessionId::new();
        let signer = ProjectSigner::generate(project, "session_key".into())?;
        let claims = issuer()?.claims_with_session_policy(
            project,
            UserId::new(),
            "authenticated".into(),
            Map::new(),
            AccessTokenSessionPolicy {
                session_id: Some(session_id),
                now_seconds: 1_000,
                ttl_seconds: 120,
            },
        )?;
        assert_eq!(claims.exp - claims.iat, 120);
        let token = signer.sign(&claims)?;
        let key = signer.verification_key(VerificationKeyStatus::Active, 900, None);
        let verified = issuer()?.verify(token.expose(), project, &[key], 1_001)?;
        assert_eq!(verified.session_id(), Some(session_id));
        assert_eq!(verified.expires_at() - verified.issued_at(), 120);
        assert!(matches!(
            issuer()?.claims_with_session_policy(
                project,
                UserId::new(),
                "authenticated".into(),
                Map::new(),
                AccessTokenSessionPolicy {
                    session_id: None,
                    now_seconds: 1_000,
                    ttl_seconds: 59,
                },
            ),
            Err(JwtError::InvalidClaims)
        ));
        Ok(())
    }

    #[test]
    fn rejects_cross_project_routing_and_key_confusion() -> Result<(), JwtError> {
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let signer = ProjectSigner::generate(project_a, "shared_kid".into())?;
        let claims = issuer()?.claims(
            project_a,
            UserId::new(),
            "authenticated".into(),
            Map::new(),
            1000,
        )?;
        let token = signer.sign(&claims)?;
        let key = signer.verification_key(VerificationKeyStatus::Active, 900, None);
        assert!(matches!(
            issuer()?.verify(token.expose(), project_b, &[key], 1001),
            Err(JwtError::UnknownKey)
        ));
        Ok(())
    }

    #[test]
    fn rejects_revoked_and_expired_keys() -> Result<(), JwtError> {
        let project = ProjectId::new();
        let signer = ProjectSigner::generate(project, "rotation_key".into())?;
        let claims = issuer()?.claims(
            project,
            UserId::new(),
            "authenticated".into(),
            Map::new(),
            1000,
        )?;
        let token = signer.sign(&claims)?;
        let revoked = signer.verification_key(VerificationKeyStatus::Revoked, 900, None);
        assert!(matches!(
            issuer()?.verify(token.expose(), project, &[revoked], 1001),
            Err(JwtError::UnknownKey)
        ));
        Ok(())
    }

    #[test]
    fn duplicate_registered_claim_is_rejected() -> Result<(), JwtError> {
        let project = ProjectId::new();
        let signer = ProjectSigner::generate(project, "duplicate_key".into())?;
        let header =
            URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","typ":"JWT","kid":"duplicate_key"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(
            r#"{{"iss":"a","iss":"b","aud":"ffdb-project","sub":"{}","project_id":"{}","jti":"{}","iat":1,"nbf":1,"exp":2,"role":"r","claims":{{}}}}"#,
            UserId::new(), project, TokenId::new()
        ));
        let input = format!("{header}.{payload}");
        let signature = signer.key.sign(input.as_bytes());
        let token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        let key = signer.verification_key(VerificationKeyStatus::Active, 0, None);
        assert!(matches!(
            verify_access_token(
                &token,
                &ExpectedAccessToken {
                    issuer: "a",
                    audience: "ffdb-project",
                    project_id: project,
                    now_seconds: 1,
                },
                &[key],
            ),
            Err(JwtError::InvalidClaims)
        ));
        Ok(())
    }
}
