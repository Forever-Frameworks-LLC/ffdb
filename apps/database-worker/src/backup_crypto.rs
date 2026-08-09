use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::Instant,
};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use ffdb_protocol::{BackupId, DatabaseRoute};
use ffdb_sqlite_runtime::CancellationToken;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 8] = b"FFDBBK01";
const CHUNK_BYTES: usize = 64 * 1024;
const TAG_BYTES: usize = 16;
const MAX_CIPHERTEXT_CHUNK: usize = CHUNK_BYTES + TAG_BYTES;

#[derive(Clone)]
pub(crate) struct BackupCrypto {
    master_key: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for BackupCrypto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackupCrypto")
            .field("master_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub(crate) enum BackupCryptoError {
    #[error("backup encryption key is invalid")]
    InvalidKey,
    #[error("backup ciphertext is invalid")]
    InvalidCiphertext,
    #[error("backup I/O failed")]
    Io,
    #[error("backup operation was cancelled")]
    Cancelled,
    #[error("backup deadline was exceeded")]
    DeadlineExceeded,
}

impl BackupCrypto {
    pub(crate) fn new(master_key: impl AsRef<[u8]>) -> Result<Self, BackupCryptoError> {
        if master_key.as_ref().len() < 32 {
            return Err(BackupCryptoError::InvalidKey);
        }
        Ok(Self {
            master_key: Zeroizing::new(master_key.as_ref().to_vec()),
        })
    }

    pub(crate) fn encrypt_file(
        &self,
        plaintext: &Path,
        ciphertext: &Path,
        route: &DatabaseRoute,
        backup_id: BackupId,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<(u64, String), BackupCryptoError> {
        check_budget(cancellation, deadline)?;
        let mut input = File::open(plaintext).map_err(|_| BackupCryptoError::Io)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(ciphertext)
            .map_err(|_| BackupCryptoError::Io)?;
        let mut nonce_prefix = [0_u8; 8];
        getrandom::fill(&mut nonce_prefix).map_err(|_| BackupCryptoError::Io)?;
        let mut key = self.derive_key(route, backup_id)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| BackupCryptoError::InvalidKey)?;
        key.zeroize();
        let aad = associated_data(route, backup_id);
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        write_hashed(&mut output, MAGIC, &mut hasher, &mut size)?;
        write_hashed(&mut output, &nonce_prefix, &mut hasher, &mut size)?;

        let mut current = read_chunk(&mut input)?;
        let mut counter = 0_u32;
        loop {
            check_budget(cancellation, deadline)?;
            let next = read_chunk(&mut input)?;
            let final_chunk = next.is_empty();
            let nonce = chunk_nonce(nonce_prefix, counter);
            let cipher_nonce = Nonce::try_from(nonce.as_slice())
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            let chunk_aad = chunk_associated_data(&aad, counter, final_chunk);
            let encrypted = cipher
                .encrypt(
                    &cipher_nonce,
                    Payload {
                        msg: &current,
                        aad: &chunk_aad,
                    },
                )
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            let length =
                u32::try_from(encrypted.len()).map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            write_hashed(&mut output, &length.to_be_bytes(), &mut hasher, &mut size)?;
            write_hashed(
                &mut output,
                &[u8::from(final_chunk)],
                &mut hasher,
                &mut size,
            )?;
            write_hashed(&mut output, &encrypted, &mut hasher, &mut size)?;
            if final_chunk {
                break;
            }
            current = next;
            counter = counter
                .checked_add(1)
                .ok_or(BackupCryptoError::InvalidCiphertext)?;
        }
        output.sync_all().map_err(|_| BackupCryptoError::Io)?;
        Ok((size, hex::encode(hasher.finalize())))
    }

    pub(crate) fn decrypt_file(
        &self,
        ciphertext: &Path,
        plaintext: &Path,
        route: &DatabaseRoute,
        backup_id: BackupId,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), BackupCryptoError> {
        check_budget(cancellation, deadline)?;
        let mut input = File::open(ciphertext).map_err(|_| BackupCryptoError::Io)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(plaintext)
            .map_err(|_| BackupCryptoError::Io)?;
        let mut magic = [0_u8; MAGIC.len()];
        input
            .read_exact(&mut magic)
            .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
        if &magic != MAGIC {
            return Err(BackupCryptoError::InvalidCiphertext);
        }
        let mut nonce_prefix = [0_u8; 8];
        input
            .read_exact(&mut nonce_prefix)
            .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
        let mut key = self.derive_key(route, backup_id)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| BackupCryptoError::InvalidKey)?;
        key.zeroize();
        let aad = associated_data(route, backup_id);
        let mut counter = 0_u32;
        loop {
            check_budget(cancellation, deadline)?;
            let mut length = [0_u8; 4];
            input
                .read_exact(&mut length)
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            let length = usize::try_from(u32::from_be_bytes(length))
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            if !(TAG_BYTES..=MAX_CIPHERTEXT_CHUNK).contains(&length) {
                return Err(BackupCryptoError::InvalidCiphertext);
            }
            let mut final_flag = [0_u8; 1];
            input
                .read_exact(&mut final_flag)
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            let final_chunk = match final_flag[0] {
                0 => false,
                1 => true,
                _ => return Err(BackupCryptoError::InvalidCiphertext),
            };
            let mut encrypted = vec![0_u8; length];
            input
                .read_exact(&mut encrypted)
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            let nonce = chunk_nonce(nonce_prefix, counter);
            let cipher_nonce = Nonce::try_from(nonce.as_slice())
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            let chunk_aad = chunk_associated_data(&aad, counter, final_chunk);
            let decrypted = cipher
                .decrypt(
                    &cipher_nonce,
                    Payload {
                        msg: &encrypted,
                        aad: &chunk_aad,
                    },
                )
                .map_err(|_| BackupCryptoError::InvalidCiphertext)?;
            output
                .write_all(&decrypted)
                .map_err(|_| BackupCryptoError::Io)?;
            if final_chunk {
                let mut trailing = [0_u8; 1];
                if input
                    .read(&mut trailing)
                    .map_err(|_| BackupCryptoError::Io)?
                    != 0
                {
                    return Err(BackupCryptoError::InvalidCiphertext);
                }
                break;
            }
            counter = counter
                .checked_add(1)
                .ok_or(BackupCryptoError::InvalidCiphertext)?;
        }
        output.sync_all().map_err(|_| BackupCryptoError::Io)
    }

    pub(crate) fn derive_storage_cursor_secret(
        &self,
        route: &DatabaseRoute,
    ) -> Result<[u8; 32], BackupCryptoError> {
        let mut salt = Vec::with_capacity(32);
        salt.extend_from_slice(route.project_id.0.as_bytes());
        salt.extend_from_slice(route.database_id.0.as_bytes());
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), self.master_key.as_slice());
        let mut key = [0_u8; 32];
        hkdf.expand(b"ffdb.storage.cursor.hmac.v1", &mut key)
            .map_err(|_| BackupCryptoError::InvalidKey)?;
        Ok(key)
    }

    fn derive_key(
        &self,
        route: &DatabaseRoute,
        backup_id: BackupId,
    ) -> Result<[u8; 32], BackupCryptoError> {
        let mut salt = Vec::with_capacity(48);
        salt.extend_from_slice(route.project_id.0.as_bytes());
        salt.extend_from_slice(route.database_id.0.as_bytes());
        salt.extend_from_slice(backup_id.0.as_bytes());
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), self.master_key.as_slice());
        let mut key = [0_u8; 32];
        hkdf.expand(b"ffdb.backup.aead.v1", &mut key)
            .map_err(|_| BackupCryptoError::InvalidKey)?;
        Ok(key)
    }
}

fn read_chunk(input: &mut File) -> Result<Vec<u8>, BackupCryptoError> {
    let mut chunk = vec![0_u8; CHUNK_BYTES];
    let mut filled = 0;
    while filled < chunk.len() {
        let read = input
            .read(&mut chunk[filled..])
            .map_err(|_| BackupCryptoError::Io)?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    chunk.truncate(filled);
    Ok(chunk)
}

fn write_hashed(
    output: &mut File,
    bytes: &[u8],
    hasher: &mut Sha256,
    size: &mut u64,
) -> Result<(), BackupCryptoError> {
    output.write_all(bytes).map_err(|_| BackupCryptoError::Io)?;
    hasher.update(bytes);
    *size = size.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    Ok(())
}

fn associated_data(route: &DatabaseRoute, backup_id: BackupId) -> Vec<u8> {
    let mut aad = Vec::with_capacity(80);
    aad.extend_from_slice(b"ffdb.backup.file.v1\0");
    aad.extend_from_slice(route.project_id.0.as_bytes());
    aad.extend_from_slice(route.database_id.0.as_bytes());
    aad.extend_from_slice(backup_id.0.as_bytes());
    aad
}

fn chunk_associated_data(prefix: &[u8], counter: u32, final_chunk: bool) -> Vec<u8> {
    let mut aad = Vec::with_capacity(prefix.len() + 5);
    aad.extend_from_slice(prefix);
    aad.extend_from_slice(&counter.to_be_bytes());
    aad.push(u8::from(final_chunk));
    aad
}

fn chunk_nonce(prefix: [u8; 8], counter: u32) -> [u8; 12] {
    let mut nonce = [0_u8; 12];
    nonce[..8].copy_from_slice(&prefix);
    nonce[8..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

fn check_budget(
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<(), BackupCryptoError> {
    if cancellation.is_cancelled() {
        Err(BackupCryptoError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(BackupCryptoError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

pub(crate) fn remove_file_if_present(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}
