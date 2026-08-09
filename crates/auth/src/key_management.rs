//! Envelope encryption and PostgreSQL lifecycle management for project JWT keys.

use async_trait::async_trait;
use ffdb_protocol::ProjectId;
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom as _, SystemRandom},
};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    EncryptedSigningKey, JwtError, ProjectSigner, SigningKeyDecryptor, VerificationKeyStatus,
};

const NONCE_BYTES: usize = 12;
const PRIVATE_KEY_BYTES: usize = 32;
const TAG_BYTES: usize = 16;
const MIN_GRACE_SECONDS: i64 = 60;
const MAX_GRACE_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum SigningKeyManagementError {
    #[error("signing-key configuration is invalid")]
    InvalidConfiguration,
    #[error("signing-key input is invalid")]
    InvalidInput,
    #[error("project was not found")]
    ProjectNotFound,
    #[error("an active signing key already exists")]
    ActiveKeyExists,
    #[error("no active signing key exists")]
    NoActiveKey,
    #[error("signing-key encryption failed")]
    Encryption,
    #[error("signing-key datastore is unavailable")]
    Unavailable,
}

pub trait SigningKeyEncryptor: Send + Sync {
    fn encrypt(
        &self,
        project_id: ProjectId,
        kid: &str,
        private_key: &[u8; PRIVATE_KEY_BYTES],
    ) -> Result<EncryptedSigningKey, SigningKeyManagementError>;
}

/// AES-256-GCM envelope using a configured deployment master key. Ciphertexts
/// are bound to key version, project id, and `kid` through authenticated AAD.
#[derive(Clone)]
pub struct AeadSigningKeyEnvelope {
    master_key: Zeroizing<[u8; 32]>,
    encryption_key_version: i32,
}

impl std::fmt::Debug for AeadSigningKeyEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AeadSigningKeyEnvelope")
            .field("master_key", &"[REDACTED]")
            .field("encryption_key_version", &self.encryption_key_version)
            .finish()
    }
}

impl AeadSigningKeyEnvelope {
    pub fn new(
        master_key: Vec<u8>,
        encryption_key_version: i32,
    ) -> Result<Self, SigningKeyManagementError> {
        let master_key = Zeroizing::new(master_key);
        let master_key: [u8; 32] = master_key
            .as_slice()
            .try_into()
            .map_err(|_| SigningKeyManagementError::InvalidConfiguration)?;
        if encryption_key_version <= 0 {
            return Err(SigningKeyManagementError::InvalidConfiguration);
        }
        Ok(Self {
            master_key: Zeroizing::new(master_key),
            encryption_key_version,
        })
    }

    fn key(&self) -> Result<LessSafeKey, SigningKeyManagementError> {
        UnboundKey::new(&AES_256_GCM, self.master_key.as_ref())
            .map(LessSafeKey::new)
            .map_err(|_| SigningKeyManagementError::Encryption)
    }
}

impl SigningKeyEncryptor for AeadSigningKeyEnvelope {
    fn encrypt(
        &self,
        project_id: ProjectId,
        kid: &str,
        private_key: &[u8; PRIVATE_KEY_BYTES],
    ) -> Result<EncryptedSigningKey, SigningKeyManagementError> {
        let aad = signing_key_aad(project_id, kid, self.encryption_key_version)?;
        let mut nonce_bytes = [0_u8; NONCE_BYTES];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| SigningKeyManagementError::Encryption)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut sealed = Zeroizing::new(private_key.to_vec());
        self.key()?
            .seal_in_place_append_tag(nonce, Aad::from(aad.as_slice()), &mut *sealed)
            .map_err(|_| SigningKeyManagementError::Encryption)?;
        let mut ciphertext = Vec::with_capacity(NONCE_BYTES + sealed.len());
        ciphertext.extend_from_slice(&nonce_bytes);
        ciphertext.extend_from_slice(&sealed);
        Ok(EncryptedSigningKey {
            project_id,
            kid: kid.to_owned(),
            ciphertext,
            encryption_key_version: self.encryption_key_version,
        })
    }
}

#[async_trait]
impl SigningKeyDecryptor for AeadSigningKeyEnvelope {
    async fn decrypt(
        &self,
        key: &EncryptedSigningKey,
    ) -> Result<Zeroizing<[u8; PRIVATE_KEY_BYTES]>, JwtError> {
        if key.encryption_key_version != self.encryption_key_version
            || key.ciphertext.len() != NONCE_BYTES + PRIVATE_KEY_BYTES + TAG_BYTES
        {
            return Err(JwtError::KeyStoreUnavailable);
        }
        let nonce_bytes: [u8; NONCE_BYTES] = key.ciphertext[..NONCE_BYTES]
            .try_into()
            .map_err(|_| JwtError::KeyStoreUnavailable)?;
        let aad = signing_key_aad(key.project_id, &key.kid, key.encryption_key_version)
            .map_err(|_| JwtError::KeyStoreUnavailable)?;
        let mut sealed = Zeroizing::new(key.ciphertext[NONCE_BYTES..].to_vec());
        let plaintext = self
            .key()
            .map_err(|_| JwtError::KeyStoreUnavailable)?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(aad.as_slice()),
                &mut sealed,
            )
            .map_err(|_| JwtError::KeyStoreUnavailable)?;
        let plaintext: [u8; PRIVATE_KEY_BYTES] = plaintext
            .try_into()
            .map_err(|_| JwtError::KeyStoreUnavailable)?;
        Ok(Zeroizing::new(plaintext))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SigningKeyDescriptor {
    pub project_id: ProjectId,
    pub kid: String,
    pub public_key: [u8; 32],
    pub status: VerificationKeyStatus,
    pub valid_from_seconds: i64,
    pub valid_until_seconds: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SigningKeyRotation {
    pub previous_kid: String,
    pub previous_valid_until_seconds: i64,
    pub active: SigningKeyDescriptor,
}

#[derive(Clone, Debug)]
pub struct PgSigningKeyManager<E> {
    pool: PgPool,
    encryptor: E,
}

impl<E: SigningKeyEncryptor> PgSigningKeyManager<E> {
    #[must_use]
    pub fn new(pool: PgPool, encryptor: E) -> Self {
        Self { pool, encryptor }
    }

    pub async fn bootstrap(
        &self,
        project_id: ProjectId,
        now_seconds: i64,
    ) -> Result<SigningKeyDescriptor, SigningKeyManagementError> {
        validate_time(now_seconds)?;
        let generated = self.generate_key(project_id, now_seconds)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        lock_project(&mut transaction, project_id).await?;
        ensure_project(&mut transaction, project_id).await?;
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM jwt_signing_keys WHERE project_id=$1 AND status='active')",
        )
        .bind(project_id.0)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if active {
            return Err(SigningKeyManagementError::ActiveKeyExists);
        }
        insert_key(&mut transaction, &generated).await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(generated.descriptor)
    }

    pub async fn rotate(
        &self,
        project_id: ProjectId,
        now_seconds: i64,
        grace_period_seconds: i64,
    ) -> Result<SigningKeyRotation, SigningKeyManagementError> {
        validate_time(now_seconds)?;
        if !(MIN_GRACE_SECONDS..=MAX_GRACE_SECONDS).contains(&grace_period_seconds) {
            return Err(SigningKeyManagementError::InvalidInput);
        }
        let grace_until = now_seconds
            .checked_add(grace_period_seconds)
            .ok_or(SigningKeyManagementError::InvalidInput)?;
        let generated = self.generate_key(project_id, now_seconds)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        lock_project(&mut transaction, project_id).await?;
        ensure_project(&mut transaction, project_id).await?;
        let active = sqlx::query(
            "SELECT kid FROM jwt_signing_keys WHERE project_id=$1 AND status='active' FOR UPDATE",
        )
        .bind(project_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx)?
        .ok_or(SigningKeyManagementError::NoActiveKey)?;
        let previous_kid: String = active
            .try_get("kid")
            .map_err(|_| SigningKeyManagementError::Unavailable)?;
        sqlx::query(
            "UPDATE jwt_signing_keys SET status='grace', \
             valid_until=GREATEST(valid_from + interval '1 second',to_timestamp($2::double precision)) \
             WHERE project_id=$1 AND status='active'",
        )
        .bind(project_id.0)
        .bind(grace_until)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        insert_key(&mut transaction, &generated).await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(SigningKeyRotation {
            previous_kid,
            previous_valid_until_seconds: grace_until,
            active: generated.descriptor,
        })
    }

    fn generate_key(
        &self,
        project_id: ProjectId,
        now_seconds: i64,
    ) -> Result<GeneratedKey, SigningKeyManagementError> {
        let kid = format!("key_{}", Uuid::now_v7().simple());
        let signer = ProjectSigner::generate(project_id, kid.clone())
            .map_err(|_| SigningKeyManagementError::Encryption)?;
        let public_key = signer
            .verification_key(VerificationKeyStatus::Active, now_seconds, None)
            .public_key
            .to_bytes();
        let private_key = signer.export_private_key();
        let encrypted = self.encryptor.encrypt(project_id, &kid, &private_key)?;
        Ok(GeneratedKey {
            descriptor: SigningKeyDescriptor {
                project_id,
                kid,
                public_key,
                status: VerificationKeyStatus::Active,
                valid_from_seconds: now_seconds,
                valid_until_seconds: None,
            },
            encrypted,
        })
    }
}

struct GeneratedKey {
    descriptor: SigningKeyDescriptor,
    encrypted: EncryptedSigningKey,
}

async fn lock_project(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: ProjectId,
) -> Result<(), SigningKeyManagementError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))")
        .bind(project_id.0)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
    Ok(())
}

async fn ensure_project(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: ProjectId,
) -> Result<(), SigningKeyManagementError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1)")
        .bind(project_id.0)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
    if !exists {
        return Err(SigningKeyManagementError::ProjectNotFound);
    }
    Ok(())
}

async fn insert_key(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    generated: &GeneratedKey,
) -> Result<(), SigningKeyManagementError> {
    sqlx::query(
        "INSERT INTO jwt_signing_keys \
         (id,project_id,kid,public_key,encrypted_private_key,encryption_key_version,status,valid_from) \
         VALUES ($1,$2,$3,$4,$5,$6,'active',to_timestamp($7::double precision))",
    )
    .bind(Uuid::now_v7())
    .bind(generated.descriptor.project_id.0)
    .bind(&generated.descriptor.kid)
    .bind(generated.descriptor.public_key.as_slice())
    .bind(generated.encrypted.ciphertext.as_slice())
    .bind(generated.encrypted.encryption_key_version)
    .bind(generated.descriptor.valid_from_seconds)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

fn signing_key_aad(
    project_id: ProjectId,
    kid: &str,
    encryption_key_version: i32,
) -> Result<Vec<u8>, SigningKeyManagementError> {
    if encryption_key_version <= 0
        || !(8..=64).contains(&kid.len())
        || !kid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SigningKeyManagementError::InvalidInput);
    }
    let mut aad = Vec::with_capacity(32 + kid.len());
    aad.extend_from_slice(b"ffdb.jwt-key.aes256gcm.v1\0");
    aad.extend_from_slice(&encryption_key_version.to_be_bytes());
    aad.extend_from_slice(project_id.0.as_bytes());
    aad.extend_from_slice(&(kid.len() as u16).to_be_bytes());
    aad.extend_from_slice(kid.as_bytes());
    Ok(aad)
}

fn validate_time(now_seconds: i64) -> Result<(), SigningKeyManagementError> {
    if now_seconds <= 0 {
        return Err(SigningKeyManagementError::InvalidInput);
    }
    Ok(())
}

fn map_sqlx(error: sqlx::Error) -> SigningKeyManagementError {
    if let sqlx::Error::Database(database) = &error
        && database.code().as_deref() == Some("23505")
    {
        return SigningKeyManagementError::ActiveKeyExists;
    }
    SigningKeyManagementError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn aead_round_trip_is_bound_to_project_kid_and_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let envelope = AeadSigningKeyEnvelope::new(vec![9; 32], 3)?;
        let project = ProjectId::new();
        let private_key = [7_u8; 32];
        let encrypted = envelope.encrypt(project, "current_key_01", &private_key)?;
        assert_ne!(
            &encrypted.ciphertext[NONCE_BYTES..NONCE_BYTES + 32],
            &private_key
        );
        assert_eq!(*envelope.decrypt(&encrypted).await?, private_key);

        let wrong_project = EncryptedSigningKey {
            project_id: ProjectId::new(),
            kid: encrypted.kid.clone(),
            ciphertext: encrypted.ciphertext.clone(),
            encryption_key_version: encrypted.encryption_key_version,
        };
        assert_eq!(
            envelope.decrypt(&wrong_project).await,
            Err(JwtError::KeyStoreUnavailable)
        );
        Ok(())
    }

    #[test]
    fn envelope_rejects_weak_master_key_and_invalid_version() {
        assert!(matches!(
            AeadSigningKeyEnvelope::new(vec![1; 31], 1),
            Err(SigningKeyManagementError::InvalidConfiguration)
        ));
        assert!(matches!(
            AeadSigningKeyEnvelope::new(vec![1; 32], 0),
            Err(SigningKeyManagementError::InvalidConfiguration)
        ));
    }

    #[test]
    fn migration_has_single_active_key_constraint() {
        let sql = include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql");
        assert!(sql.contains("jwt_one_active_key_per_project"));
        assert!(sql.contains("encrypted_private_key bytea"));
    }
}
