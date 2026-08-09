use ffdb_sql_parser::Identifier;
use ffdb_sqlite_rls::backing_table_name;
use rusqlite::OptionalExtension as _;
use uuid::Uuid;

use crate::{InternalLease, RuntimeError, Session, SqlParameter, StatementRequest};

const MAX_CLEANUP_ATTEMPTS: i64 = 12;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageUsage {
    pub current_bytes: u64,
    pub reserved_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageProviderUploadBinding {
    pub provider_key: String,
    pub reserved_bytes: u64,
    pub replacement_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageCommitReceiptRecord {
    pub commit_digest: Vec<u8>,
    pub result_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageCleanupQueueItem {
    pub id: String,
    pub provider_key: String,
    pub action: String,
    pub upload_id: Option<String>,
    pub lease_token: String,
    pub attempt: u32,
    pub lease_expires_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageCleanupDisposition {
    Deleted,
    Retry,
}

impl Session<'_> {
    pub fn storage_probe_object_delete(&mut self, object_id: &str) -> Result<(), RuntimeError> {
        validate_object_id(object_id)?;
        let _internal = InternalLease::enter(&self.context)?;
        self.probe_write(&StatementRequest {
            sql: "DELETE FROM storage_objects WHERE id=?1".to_owned(),
            parameters: vec![SqlParameter::Text(object_id.to_owned())],
        })
    }

    pub fn storage_delete_object(&mut self, object_id: &str) -> Result<(), RuntimeError> {
        validate_object_id(object_id)?;
        let _internal = InternalLease::enter(&self.context)?;
        let _ = self.execute(&StatementRequest {
            sql: "DELETE FROM storage_objects WHERE id=?1".to_owned(),
            parameters: vec![SqlParameter::Text(object_id.to_owned())],
        })?;
        Ok(())
    }

    pub fn storage_usage(&mut self, now_ms: i64) -> Result<StorageUsage, RuntimeError> {
        let objects = storage_backing("storage_objects")?;
        let _internal = InternalLease::enter(&self.context)?;
        let current: i64 = self.connection.query_row(
            &format!(
                "SELECT COALESCE(SUM(size_bytes),0) FROM {}",
                objects.quoted()
            ),
            [],
            |row| row.get(0),
        )?;
        let pending: i64 = self.connection.query_row(
            "SELECT COALESCE(SUM(bytes),0) FROM __ffdb_storage_reservations \
             WHERE expires_at_ms>?1",
            [now_ms],
            |row| row.get(0),
        )?;
        let uploads: i64 = self.connection.query_row(
            "SELECT COALESCE(SUM(reserved_bytes),0) FROM __ffdb_storage_provider_uploads",
            [],
            |row| row.get(0),
        )?;
        Ok(StorageUsage {
            current_bytes: u64::try_from(current).map_err(|_| RuntimeError::Database)?,
            reserved_bytes: u64::try_from(
                pending.checked_add(uploads).ok_or(RuntimeError::Database)?,
            )
            .map_err(|_| RuntimeError::Database)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn storage_reserve(
        &mut self,
        project_id: &str,
        subject: &str,
        token_id: &str,
        nonce: &str,
        bytes: u64,
        expires_at_ms: i64,
        now_ms: i64,
        provider_key: &str,
        action: &str,
        upload_id: Option<&str>,
    ) -> Result<(), RuntimeError> {
        if project_id.is_empty()
            || subject.is_empty()
            || token_id.is_empty()
            || nonce.len() < 16
            || nonce.len() > 128
            || expires_at_ms <= now_ms
            || provider_key.is_empty()
            || !valid_storage_action(action)
        {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let buckets = storage_backing("storage_buckets")?;
        let objects = storage_backing("storage_objects")?;
        let bytes = i64::try_from(bytes).map_err(|_| RuntimeError::StorageQuotaExceeded)?;
        let _internal = InternalLease::enter(&self.context)?;
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM __ffdb_storage_reservations \
             WHERE project_id=?1 AND nonce=?2)",
            [project_id, nonce],
            |row| row.get(0),
        )?;
        if exists {
            return Err(RuntimeError::StorageReservationDuplicate);
        }
        let quota: Option<i64> = self.connection.query_row(
            &format!("SELECT MIN(project_quota_bytes) FROM {}", buckets.quoted()),
            [],
            |row| row.get(0),
        )?;
        let quota = quota.ok_or(RuntimeError::StorageQuotaExceeded)?;
        let used: i64 = self.connection.query_row(
            &format!(
                "SELECT COALESCE(SUM(size_bytes),0) FROM {}",
                objects.quoted()
            ),
            [],
            |row| row.get(0),
        )?;
        let reserved: i64 = self.connection.query_row(
            "SELECT \
             (SELECT COALESCE(SUM(bytes),0) FROM __ffdb_storage_reservations \
              WHERE project_id=?1 AND expires_at_ms>?2) + \
             (SELECT COALESCE(SUM(reserved_bytes),0) FROM __ffdb_storage_provider_uploads)",
            rusqlite::params![project_id, now_ms],
            |row| row.get(0),
        )?;
        if used
            .checked_add(reserved)
            .and_then(|value| value.checked_add(bytes))
            .is_none_or(|total| total > quota)
        {
            return Err(RuntimeError::StorageQuotaExceeded);
        }
        self.connection.execute(
            "INSERT INTO __ffdb_storage_reservations \
             (project_id,nonce,subject,token_id,bytes,expires_at_ms,provider_key,action,upload_id) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                project_id,
                nonce,
                subject,
                token_id,
                bytes,
                expires_at_ms,
                provider_key,
                action,
                upload_id
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn storage_consume_reservation(
        &mut self,
        project_id: &str,
        subject: &str,
        token_id: &str,
        nonce: &str,
        expected_bytes: u64,
        expected_expires_at_ms: i64,
        now_ms: i64,
        expected_provider_key: &str,
        expected_action: &str,
        expected_upload_id: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let expected_bytes =
            i64::try_from(expected_bytes).map_err(|_| RuntimeError::StorageReservationMismatch)?;
        let _internal = InternalLease::enter(&self.context)?;
        let consumed = self.connection.execute(
            "DELETE FROM __ffdb_storage_reservations \
             WHERE project_id=?1 AND nonce=?2 AND subject=?3 AND token_id=?4 \
             AND bytes=?5 AND expires_at_ms=?6 AND expires_at_ms>?7 \
             AND provider_key=?8 AND action=?9 AND upload_id IS ?10",
            rusqlite::params![
                project_id,
                nonce,
                subject,
                token_id,
                expected_bytes,
                expected_expires_at_ms,
                now_ms,
                expected_provider_key,
                expected_action,
                expected_upload_id
            ],
        )?;
        if consumed == 1 {
            Ok(())
        } else {
            Err(RuntimeError::StorageReservationMismatch)
        }
    }

    pub fn storage_cleanup_expired_reservations(
        &mut self,
        now_ms: i64,
    ) -> Result<usize, RuntimeError> {
        self.storage_prepare_expired_cleanup(now_ms, 100)
    }

    pub fn storage_prepare_expired_cleanup(
        &mut self,
        now_ms: i64,
        limit: usize,
    ) -> Result<usize, RuntimeError> {
        if limit == 0 || limit > 100 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let uploads = storage_backing("storage_uploads")?;
        let _internal = InternalLease::enter(&self.context)?;
        let mut expired = self.connection.prepare(
            "SELECT project_id,nonce,provider_key,action,upload_id \
             FROM __ffdb_storage_reservations WHERE expires_at_ms<=?1 \
             ORDER BY expires_at_ms,nonce LIMIT ?2",
        )?;
        let reservations = expired
            .query_map(rusqlite::params![now_ms, limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(expired);
        for (project_id, nonce, provider_key, action, upload_id) in &reservations {
            if let (Some(provider_key), Some(action)) = (provider_key, action)
                && cleanup_target_present(action, upload_id.as_deref())
            {
                enqueue_cleanup(
                    self.connection,
                    provider_key,
                    action,
                    upload_id.as_deref(),
                    now_ms,
                )?;
            }
            let removed = self.connection.execute(
                "DELETE FROM __ffdb_storage_reservations WHERE project_id=?1 AND nonce=?2 \
                 AND expires_at_ms<=?3",
                rusqlite::params![project_id, nonce, now_ms],
            )?;
            if removed != 1 {
                return Err(RuntimeError::StorageReservationMismatch);
            }
        }

        let mut expired_upload_statement = self.connection.prepare(&format!(
            "SELECT u.id,p.provider_key FROM {} AS u \
             JOIN __ffdb_storage_provider_uploads AS p ON p.upload_id=u.id \
             WHERE u.expires_at_ms<=?1 ORDER BY u.expires_at_ms,u.id LIMIT ?2",
            uploads.quoted()
        ))?;
        let expired_uploads = expired_upload_statement
            .query_map(rusqlite::params![now_ms, limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(expired_upload_statement);
        for (upload_id, provider_key) in expired_uploads {
            enqueue_cleanup(
                self.connection,
                &provider_key,
                "abort_multipart",
                Some(&upload_id),
                now_ms,
            )?;
            self.connection.execute(
                "DELETE FROM __ffdb_storage_upload_parts WHERE upload_id=?1",
                [&upload_id],
            )?;
            self.connection.execute(
                "DELETE FROM __ffdb_storage_provider_uploads WHERE upload_id=?1",
                [&upload_id],
            )?;
            self.connection.execute(
                &format!("DELETE FROM {} WHERE id=?1", uploads.quoted()),
                [&upload_id],
            )?;
        }
        self.connection.execute(
            "DELETE FROM __ffdb_storage_commit_receipts WHERE expires_at_ms<=?1",
            [now_ms],
        )?;
        Ok(reservations.len())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn storage_release_reservation_bound(
        &mut self,
        project_id: &str,
        subject: &str,
        token_id: &str,
        nonce: &str,
        expected_bytes: u64,
        expected_expires_at_ms: i64,
        now_ms: i64,
    ) -> Result<(), RuntimeError> {
        let expected_bytes =
            i64::try_from(expected_bytes).map_err(|_| RuntimeError::StorageReservationMismatch)?;
        let _internal = InternalLease::enter(&self.context)?;
        let binding = self
            .connection
            .query_row(
                "SELECT provider_key,action,upload_id FROM __ffdb_storage_reservations \
                 WHERE project_id=?1 AND nonce=?2 AND subject=?3 AND token_id=?4 \
                 AND bytes=?5 AND expires_at_ms=?6 AND expires_at_ms>?7",
                rusqlite::params![
                    project_id,
                    nonce,
                    subject,
                    token_id,
                    expected_bytes,
                    expected_expires_at_ms,
                    now_ms
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(RuntimeError::StorageReservationMismatch)?;
        if cleanup_target_present(&binding.1, binding.2.as_deref()) {
            enqueue_cleanup(
                self.connection,
                &binding.0,
                &binding.1,
                binding.2.as_deref(),
                now_ms,
            )?;
        }
        let removed = self.connection.execute(
            "DELETE FROM __ffdb_storage_reservations WHERE project_id=?1 AND nonce=?2 \
             AND subject=?3 AND token_id=?4 AND bytes=?5 AND expires_at_ms=?6",
            rusqlite::params![
                project_id,
                nonce,
                subject,
                token_id,
                expected_bytes,
                expected_expires_at_ms
            ],
        )?;
        if removed == 1 {
            Ok(())
        } else {
            Err(RuntimeError::StorageReservationMismatch)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn storage_record_commit_receipt(
        &mut self,
        project_id: &str,
        nonce: &str,
        subject: &str,
        token_id: &str,
        binding_digest: &[u8],
        commit_digest: &[u8],
        result_json: &str,
        committed_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<(), RuntimeError> {
        if expires_at_ms <= committed_at_ms || result_json.len() > 8_192 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_storage_commit_receipts \
             (project_id,nonce,subject,token_id,binding_digest,commit_digest,result_json,committed_at_ms,expires_at_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                project_id,
                nonce,
                subject,
                token_id,
                binding_digest,
                commit_digest,
                result_json,
                committed_at_ms,
                expires_at_ms
            ],
        )?;
        Ok(())
    }

    pub fn storage_commit_receipt(
        &mut self,
        project_id: &str,
        nonce: &str,
        subject: &str,
        token_id: &str,
        binding_digest: &[u8],
        now_ms: i64,
    ) -> Result<Option<StorageCommitReceiptRecord>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let receipt = self
            .connection
            .query_row(
                "SELECT subject,token_id,binding_digest,commit_digest,result_json,expires_at_ms \
                 FROM __ffdb_storage_commit_receipts WHERE project_id=?1 AND nonce=?2",
                rusqlite::params![project_id, nonce],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            stored_subject,
            stored_token,
            stored_binding,
            commit_digest,
            result_json,
            expiry,
        )) = receipt
        else {
            return Ok(None);
        };
        if stored_subject != subject
            || stored_token != token_id
            || stored_binding.as_slice() != binding_digest
            || expiry <= now_ms
        {
            return Err(RuntimeError::StorageReservationMismatch);
        }
        Ok(Some(StorageCommitReceiptRecord {
            commit_digest,
            result_json,
        }))
    }

    pub fn storage_claim_cleanup(
        &mut self,
        now_ms: i64,
        limit: usize,
        lease_duration_ms: i64,
    ) -> Result<Vec<StorageCleanupQueueItem>, RuntimeError> {
        if limit == 0 || limit > 100 || !(1_000..=15 * 60 * 1_000).contains(&lease_duration_ms) {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let lease_expires_at_ms = now_ms.saturating_add(lease_duration_ms);
        let _internal = InternalLease::enter(&self.context)?;
        let mut statement = self.connection.prepare(
            "SELECT id,provider_key,action,upload_id,attempts \
             FROM __ffdb_storage_provider_cleanup \
             WHERE available_at_ms<=?1 AND (lease_token IS NULL OR lease_expires_at_ms<=?1) \
             AND attempts<?3 \
             ORDER BY available_at_ms,created_at_ms,id LIMIT ?2",
        )?;
        let candidates = statement
            .query_map(
                rusqlite::params![now_ms, limit, MAX_CLEANUP_ATTEMPTS],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut claimed = Vec::with_capacity(candidates.len());
        for (id, provider_key, action, upload_id, attempts) in candidates {
            let lease_token = Uuid::now_v7().to_string();
            let next_attempt = attempts.saturating_add(1).min(MAX_CLEANUP_ATTEMPTS);
            let updated = self.connection.execute(
                "UPDATE __ffdb_storage_provider_cleanup \
                 SET lease_token=?1,lease_expires_at_ms=?2,attempts=?3 \
                 WHERE id=?4 AND available_at_ms<=?5 \
                 AND (lease_token IS NULL OR lease_expires_at_ms<=?5)",
                rusqlite::params![lease_token, lease_expires_at_ms, next_attempt, id, now_ms],
            )?;
            if updated != 1 {
                return Err(RuntimeError::StorageReservationMismatch);
            }
            claimed.push(StorageCleanupQueueItem {
                id,
                provider_key,
                action,
                upload_id,
                lease_token,
                attempt: u32::try_from(next_attempt).unwrap_or(u32::MAX),
                lease_expires_at_ms,
            });
        }
        Ok(claimed)
    }

    pub fn storage_ack_cleanup(
        &mut self,
        now_ms: i64,
        dispositions: &[(String, String, StorageCleanupDisposition)],
    ) -> Result<(usize, usize), RuntimeError> {
        if dispositions.is_empty() || dispositions.len() > 100 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let _internal = InternalLease::enter(&self.context)?;
        let mut removed = 0_usize;
        let mut retried = 0_usize;
        for (id, lease_token, outcome) in dispositions {
            match outcome {
                StorageCleanupDisposition::Deleted => {
                    let changed = self.connection.execute(
                        "DELETE FROM __ffdb_storage_provider_cleanup WHERE id=?1 AND lease_token=?2",
                        rusqlite::params![id, lease_token],
                    )?;
                    if changed != 1 {
                        return Err(RuntimeError::StorageReservationMismatch);
                    }
                    removed += 1;
                }
                StorageCleanupDisposition::Retry => {
                    let attempts = self
                        .connection
                        .query_row(
                            "SELECT attempts FROM __ffdb_storage_provider_cleanup \
                             WHERE id=?1 AND lease_token=?2",
                            rusqlite::params![id, lease_token],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?
                        .ok_or(RuntimeError::StorageReservationMismatch)?;
                    let exponent = u32::try_from(attempts.saturating_sub(1))
                        .unwrap_or(u32::MAX)
                        .min(12);
                    let backoff_ms = 1_000_i64.saturating_mul(1_i64 << exponent).min(3_600_000);
                    let changed = self.connection.execute(
                        "UPDATE __ffdb_storage_provider_cleanup \
                         SET available_at_ms=?1,lease_token=NULL,lease_expires_at_ms=NULL \
                         WHERE id=?2 AND lease_token=?3",
                        rusqlite::params![now_ms.saturating_add(backoff_ms), id, lease_token],
                    )?;
                    if changed != 1 {
                        return Err(RuntimeError::StorageReservationMismatch);
                    }
                    retried += 1;
                }
            }
        }
        Ok((removed, retried))
    }

    pub fn storage_enqueue_provider_cleanup(
        &mut self,
        provider_key: &str,
        action: &str,
        upload_id: Option<&str>,
        now_ms: i64,
    ) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        enqueue_cleanup(self.connection, provider_key, action, upload_id, now_ms)
    }

    pub fn storage_provider_key(
        &mut self,
        object_id: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT provider_key FROM __ffdb_storage_provider_objects WHERE object_id=?1",
                [object_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn storage_set_provider_key(
        &mut self,
        object_id: &str,
        provider_key: &str,
    ) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_storage_provider_objects(object_id,provider_key) VALUES (?1,?2) \
             ON CONFLICT(object_id) DO UPDATE SET provider_key=excluded.provider_key",
            [object_id, provider_key],
        )?;
        Ok(())
    }

    pub fn storage_remove_provider_key(&mut self, object_id: &str) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "DELETE FROM __ffdb_storage_provider_objects WHERE object_id=?1",
            [object_id],
        )?;
        Ok(())
    }

    pub fn storage_upload_provider_key(
        &mut self,
        upload_id: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT provider_key FROM __ffdb_storage_provider_uploads WHERE upload_id=?1",
                [upload_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn storage_upload_provider_binding(
        &mut self,
        upload_id: &str,
    ) -> Result<Option<StorageProviderUploadBinding>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT provider_key,reserved_bytes,replacement_fingerprint \
                 FROM __ffdb_storage_provider_uploads WHERE upload_id=?1",
                [upload_id],
                |row| {
                    let reserved = row.get::<_, i64>(1)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        reserved,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .map(|(provider_key, reserved, replacement_fingerprint)| {
                Ok(StorageProviderUploadBinding {
                    provider_key,
                    reserved_bytes: u64::try_from(reserved).map_err(|_| RuntimeError::Database)?,
                    replacement_fingerprint,
                })
            })
            .transpose()
    }

    pub fn storage_set_upload_provider_key(
        &mut self,
        upload_id: &str,
        provider_key: &str,
    ) -> Result<(), RuntimeError> {
        self.storage_set_upload_provider_binding(upload_id, provider_key, 0, None)
    }

    pub fn storage_set_upload_provider_binding(
        &mut self,
        upload_id: &str,
        provider_key: &str,
        reserved_bytes: u64,
        replacement_fingerprint: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let reserved_bytes =
            i64::try_from(reserved_bytes).map_err(|_| RuntimeError::StorageQuotaExceeded)?;
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_storage_provider_uploads \
             (upload_id,provider_key,reserved_bytes,replacement_fingerprint) VALUES (?1,?2,?3,?4) \
             ON CONFLICT(upload_id) DO UPDATE SET provider_key=excluded.provider_key, \
             reserved_bytes=excluded.reserved_bytes, \
             replacement_fingerprint=excluded.replacement_fingerprint",
            rusqlite::params![
                upload_id,
                provider_key,
                reserved_bytes,
                replacement_fingerprint
            ],
        )?;
        Ok(())
    }

    pub fn storage_remove_upload_provider_key(
        &mut self,
        upload_id: &str,
    ) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "DELETE FROM __ffdb_storage_provider_uploads WHERE upload_id=?1",
            [upload_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn storage_store_upload_part(
        &mut self,
        upload_id: &str,
        part_number: u32,
        size_bytes: Option<u64>,
        checksum_sha256: Option<&str>,
        etag: Option<&str>,
    ) -> Result<(), RuntimeError> {
        if !(1..=10_000).contains(&part_number) {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let size_bytes = size_bytes
            .map(i64::try_from)
            .transpose()
            .map_err(|_| RuntimeError::StorageQuotaExceeded)?;
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_storage_upload_parts \
             (upload_id,part_number,size_bytes,checksum_sha256,etag) VALUES (?1,?2,?3,?4,?5) \
             ON CONFLICT(upload_id,part_number) DO UPDATE SET size_bytes=excluded.size_bytes,\
             checksum_sha256=excluded.checksum_sha256,etag=excluded.etag",
            rusqlite::params![upload_id, part_number, size_bytes, checksum_sha256, etag],
        )?;
        Ok(())
    }

    pub fn storage_remove_upload_parts(&mut self, upload_id: &str) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "DELETE FROM __ffdb_storage_upload_parts WHERE upload_id=?1",
            [upload_id],
        )?;
        Ok(())
    }
}

fn validate_object_id(object_id: &str) -> Result<(), RuntimeError> {
    Uuid::parse_str(object_id)
        .map(|_| ())
        .map_err(|_| RuntimeError::StatementNotAllowed)
}

fn storage_backing(name: &str) -> Result<Identifier, RuntimeError> {
    let name = Identifier::new(name).map_err(|_| RuntimeError::Database)?;
    backing_table_name(&name).map_err(|_| RuntimeError::Database)
}

fn valid_storage_action(action: &str) -> bool {
    matches!(
        action,
        "upload"
            | "download"
            | "delete"
            | "list"
            | "create_multipart"
            | "upload_part"
            | "complete_multipart"
            | "abort_multipart"
    )
}

fn cleanup_required(action: &str) -> bool {
    matches!(
        action,
        "upload" | "create_multipart" | "upload_part" | "complete_multipart" | "abort_multipart"
    )
}

fn cleanup_target_present(action: &str, upload_id: Option<&str>) -> bool {
    match action {
        "upload" | "create_multipart" | "complete_multipart" => true,
        "upload_part" | "abort_multipart" => upload_id.is_some(),
        _ => false,
    }
}

fn enqueue_cleanup(
    connection: &rusqlite::Connection,
    provider_key: &str,
    action: &str,
    upload_id: Option<&str>,
    now_ms: i64,
) -> Result<(), RuntimeError> {
    if provider_key.is_empty() || provider_key.len() > 1_024 || !cleanup_required(action) {
        return Err(RuntimeError::StatementNotAllowed);
    }
    connection.execute(
        "INSERT INTO __ffdb_storage_provider_cleanup \
         (id,provider_key,action,upload_id,created_at_ms,available_at_ms) \
         VALUES (?1,?2,?3,?4,?5,?5)",
        rusqlite::params![
            Uuid::now_v7().to_string(),
            provider_key,
            action,
            upload_id,
            now_ms
        ],
    )?;
    Ok(())
}
