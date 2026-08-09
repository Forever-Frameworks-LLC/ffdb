use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use password_hash::{
    PasswordHash as ParsedPasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString,
    rand_core::OsRng,
};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

/// Secret text that zeroizes its allocation and never reveals itself via Debug.
#[derive(Clone)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

/// PHC password hash. Debug output is redacted to avoid accidental log leakage.
#[derive(Clone, Eq, PartialEq)]
pub struct PasswordHash(String);

impl PasswordHash {
    pub fn parse(value: String) -> Result<Self, PasswordError> {
        let parsed = ParsedPasswordHash::new(&value).map_err(|_| PasswordError::InvalidHash)?;
        if parsed.algorithm.as_str() != "argon2id" {
            return Err(PasswordError::UnsupportedHash);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_phc(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PasswordHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordHash([REDACTED])")
    }
}

impl Drop for PasswordHash {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyOutcome {
    Invalid,
    Valid,
    ValidNeedsRehash,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PasswordError {
    #[error("password does not satisfy the service policy")]
    Policy,
    #[error("password hash is invalid")]
    InvalidHash,
    #[error("password hash algorithm is unsupported")]
    UnsupportedHash,
    #[error("password hashing is unavailable")]
    HashingUnavailable,
}

pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: SecretString) -> Result<PasswordHash, PasswordError>;
    fn verify(
        &self,
        password: SecretString,
        hash: &PasswordHash,
    ) -> Result<VerifyOutcome, PasswordError>;
}

/// Versioned Argon2id policy. Existing hashes remain verifiable; a successful
/// verification reports when the stored parameters need an upgrade.
#[derive(Clone, Debug)]
pub struct Argon2PasswordHasher {
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
    output_len: usize,
    max_password_bytes: usize,
}

impl Default for Argon2PasswordHasher {
    fn default() -> Self {
        // OWASP's 19 MiB / t=2 / p=1 Argon2id baseline. Operators can raise
        // this after benchmarking, without invalidating existing PHC strings.
        Self {
            memory_kib: 19 * 1024,
            iterations: 2,
            lanes: 1,
            output_len: 32,
            max_password_bytes: 1024,
        }
    }
}

impl Argon2PasswordHasher {
    pub fn new(
        memory_kib: u32,
        iterations: u32,
        lanes: u32,
        max_password_bytes: usize,
    ) -> Result<Self, PasswordError> {
        Params::new(memory_kib, iterations, lanes, Some(32))
            .map_err(|_| PasswordError::HashingUnavailable)?;
        if max_password_bytes == 0 {
            return Err(PasswordError::Policy);
        }
        Ok(Self {
            memory_kib,
            iterations,
            lanes,
            output_len: 32,
            max_password_bytes,
        })
    }

    fn engine(&self) -> Result<Argon2<'static>, PasswordError> {
        let params = Params::new(
            self.memory_kib,
            self.iterations,
            self.lanes,
            Some(self.output_len),
        )
        .map_err(|_| PasswordError::HashingUnavailable)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    fn validate_password(&self, password: &SecretString) -> Result<(), PasswordError> {
        let length = password.expose().len();
        if length < 8 || length > self.max_password_bytes {
            return Err(PasswordError::Policy);
        }
        Ok(())
    }

    fn needs_rehash(&self, parsed: &ParsedPasswordHash<'_>) -> bool {
        parsed.version != Some(19)
            || parsed.params.get_decimal("m") != Some(self.memory_kib)
            || parsed.params.get_decimal("t") != Some(self.iterations)
            || parsed.params.get_decimal("p") != Some(self.lanes)
    }
}

impl PasswordHasher for Argon2PasswordHasher {
    fn hash(&self, password: SecretString) -> Result<PasswordHash, PasswordError> {
        self.validate_password(&password)?;
        let salt = SaltString::generate(&mut OsRng);
        let encoded = self
            .engine()?
            .hash_password(password.expose().as_bytes(), &salt)
            .map_err(|_| PasswordError::HashingUnavailable)?
            .to_string();
        PasswordHash::parse(encoded)
    }

    fn verify(
        &self,
        password: SecretString,
        hash: &PasswordHash,
    ) -> Result<VerifyOutcome, PasswordError> {
        self.validate_password(&password)?;
        let parsed =
            ParsedPasswordHash::new(hash.as_phc()).map_err(|_| PasswordError::InvalidHash)?;
        if parsed.algorithm.as_str() != "argon2id" {
            return Err(PasswordError::UnsupportedHash);
        }
        match self
            .engine()?
            .verify_password(password.expose().as_bytes(), &parsed)
        {
            Ok(()) if self.needs_rehash(&parsed) => Ok(VerifyOutcome::ValidNeedsRehash),
            Ok(()) => Ok(VerifyOutcome::Valid),
            Err(password_hash::Error::Password) => Ok(VerifyOutcome::Invalid),
            Err(_) => Err(PasswordError::InvalidHash),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_verifies_without_exposing_plaintext() -> Result<(), PasswordError> {
        let hasher = Argon2PasswordHasher::default();
        let hash = hasher.hash(SecretString::new("correct horse battery staple".into()))?;
        assert!(hash.as_phc().starts_with("$argon2id$v=19$"));
        assert_eq!(
            hasher.verify(
                SecretString::new("correct horse battery staple".into()),
                &hash
            )?,
            VerifyOutcome::Valid
        );
        assert_eq!(
            hasher.verify(SecretString::new("wrong password".into()), &hash)?,
            VerifyOutcome::Invalid
        );
        assert!(!format!("{hash:?}").contains("argon2"));
        Ok(())
    }

    #[test]
    fn reports_parameter_upgrade() -> Result<(), PasswordError> {
        let old = Argon2PasswordHasher::new(8 * 1024, 1, 1, 1024)?;
        let current = Argon2PasswordHasher::default();
        let hash = old.hash(SecretString::new("valid password".into()))?;
        assert_eq!(
            current.verify(SecretString::new("valid password".into()), &hash)?,
            VerifyOutcome::ValidNeedsRehash
        );
        Ok(())
    }

    #[test]
    fn rejects_trivially_short_passwords() {
        let hasher = Argon2PasswordHasher::default();
        assert_eq!(
            hasher.hash(SecretString::new("short".into())),
            Err(PasswordError::Policy)
        );
    }
}
