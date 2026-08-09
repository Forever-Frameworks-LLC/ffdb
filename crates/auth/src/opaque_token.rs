use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct SecretToken(Zeroizing<String>);

impl SecretToken {
    #[must_use]
    pub(crate) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct CredentialDigest([u8; 32]);

impl CredentialDigest {
    pub fn from_slice(value: &[u8]) -> Result<Self, TokenError> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| TokenError::InvalidDigest)?;
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for CredentialDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialDigest([REDACTED])")
    }
}

impl Drop for CredentialDigest {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenParts {
    pub prefix: String,
    pub digest: CredentialDigest,
}

#[derive(Clone)]
pub struct OpaqueTokenCodec {
    label: &'static str,
    pepper: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for OpaqueTokenCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpaqueTokenCodec")
            .field("label", &self.label)
            .field("pepper", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TokenError {
    #[error("token key material is invalid")]
    InvalidKey,
    #[error("token is malformed")]
    Malformed,
    #[error("credential digest is invalid")]
    InvalidDigest,
}

impl OpaqueTokenCodec {
    pub fn new(label: &'static str, pepper: Vec<u8>) -> Result<Self, TokenError> {
        if label.is_empty()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            || pepper.len() < 32
        {
            return Err(TokenError::InvalidKey);
        }
        Ok(Self {
            label,
            pepper: Zeroizing::new(pepper),
        })
    }

    pub fn issue(&self) -> Result<(SecretToken, TokenParts), TokenError> {
        let mut prefix_bytes = [0_u8; 12];
        let mut secret_bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut prefix_bytes);
        OsRng.fill_bytes(&mut secret_bytes);
        let prefix = URL_SAFE_NO_PAD.encode(prefix_bytes);
        let secret = URL_SAFE_NO_PAD.encode(secret_bytes);
        secret_bytes.zeroize();
        let plaintext = format!("ffdb_{}_{}.{}", self.label, prefix, secret);
        let digest = self.digest(plaintext.as_bytes())?;
        Ok((
            SecretToken(Zeroizing::new(plaintext)),
            TokenParts { prefix, digest },
        ))
    }

    pub fn parse_and_digest(&self, token: &str) -> Result<TokenParts, TokenError> {
        if token.len() > 256 {
            return Err(TokenError::Malformed);
        }
        let expected_start = format!("ffdb_{}_", self.label);
        let remainder = token
            .strip_prefix(&expected_start)
            .ok_or(TokenError::Malformed)?;
        let (prefix, secret) = remainder.split_once('.').ok_or(TokenError::Malformed)?;
        if secret.contains('.') {
            return Err(TokenError::Malformed);
        }
        let prefix_bytes = URL_SAFE_NO_PAD
            .decode(prefix)
            .map_err(|_| TokenError::Malformed)?;
        let secret_bytes = URL_SAFE_NO_PAD
            .decode(secret)
            .map_err(|_| TokenError::Malformed)?;
        if prefix_bytes.len() != 12
            || secret_bytes.len() != 32
            || URL_SAFE_NO_PAD.encode(prefix_bytes) != prefix
            || URL_SAFE_NO_PAD.encode(secret_bytes) != secret
        {
            return Err(TokenError::Malformed);
        }
        Ok(TokenParts {
            prefix: prefix.to_owned(),
            digest: self.digest(token.as_bytes())?,
        })
    }

    #[must_use]
    pub fn verify_digest(&self, candidate: &CredentialDigest, expected: &CredentialDigest) -> bool {
        bool::from(candidate.as_bytes().ct_eq(expected.as_bytes()))
    }

    fn digest(&self, value: &[u8]) -> Result<CredentialDigest, TokenError> {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.pepper)
            .map_err(|_| TokenError::InvalidKey)?;
        mac.update(value);
        let bytes: [u8; 32] = mac.finalize().into_bytes().into();
        Ok(CredentialDigest(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip_is_canonical_and_redacted() -> Result<(), TokenError> {
        let codec = OpaqueTokenCodec::new("refresh", vec![7; 32])?;
        let (token, stored) = codec.issue()?;
        let parsed = codec.parse_and_digest(token.expose())?;
        assert_eq!(stored.prefix, parsed.prefix);
        assert!(codec.verify_digest(&stored.digest, &parsed.digest));
        assert_eq!(format!("{token:?}"), "SecretToken([REDACTED])");
        Ok(())
    }

    #[test]
    fn rejects_noncanonical_or_wrong_label() -> Result<(), TokenError> {
        let codec = OpaqueTokenCodec::new("refresh", vec![9; 32])?;
        assert!(codec.parse_and_digest("ffdb_dev_x.y").is_err());
        assert!(codec.parse_and_digest("ffdb_refresh_AA.AA").is_err());
        Ok(())
    }
}
