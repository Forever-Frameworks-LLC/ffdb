use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row};
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    EmailError, EmailTransport, PrecompiledTemplate, RenderedEmail, RuntimeRenderer, ScalarValue,
    TemplateKind, default_template,
};

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_ATTEMPTS: i32 = 8;

#[derive(Clone)]
pub struct EmailMessageCipher {
    key: Zeroizing<[u8; 32]>,
    key_version: i32,
}

impl std::fmt::Debug for EmailMessageCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmailMessageCipher")
            .field("key", &"[REDACTED]")
            .field("key_version", &self.key_version)
            .finish()
    }
}

impl EmailMessageCipher {
    pub fn new(key: impl AsRef<[u8]>, key_version: i32) -> Result<Self, EmailError> {
        let key: [u8; 32] = key
            .as_ref()
            .try_into()
            .map_err(|_| EmailError::InvalidConfiguration)?;
        if key_version <= 0 {
            return Err(EmailError::InvalidConfiguration);
        }
        Ok(Self {
            key: Zeroizing::new(key),
            key_version,
        })
    }

    fn aead(&self) -> Result<LessSafeKey, EmailError> {
        UnboundKey::new(&AES_256_GCM, self.key.as_ref())
            .map(LessSafeKey::new)
            .map_err(|_| EmailError::Encryption)
    }

    fn encrypt(
        &self,
        project_id: Uuid,
        job_id: Uuid,
        message: &RenderedEmail,
    ) -> Result<Vec<u8>, EmailError> {
        let mut nonce = [0_u8; NONCE_BYTES];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| EmailError::Encryption)?;
        let mut plaintext =
            Zeroizing::new(serde_json::to_vec(message).map_err(|_| EmailError::InvalidArtifact)?);
        self.aead()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(project_id, job_id, self.key_version).as_slice()),
                std::ops::DerefMut::deref_mut(&mut plaintext),
            )
            .map_err(|_| EmailError::Encryption)?;
        let mut ciphertext = Vec::with_capacity(NONCE_BYTES + plaintext.len());
        ciphertext.extend_from_slice(&nonce);
        ciphertext.extend_from_slice(&plaintext);
        Ok(ciphertext)
    }

    fn decrypt(
        &self,
        project_id: Uuid,
        job_id: Uuid,
        key_version: i32,
        encrypted: &[u8],
    ) -> Result<RenderedEmail, EmailError> {
        if key_version != self.key_version || encrypted.len() <= NONCE_BYTES + TAG_BYTES {
            return Err(EmailError::Encryption);
        }
        let nonce: [u8; NONCE_BYTES] = encrypted[..NONCE_BYTES]
            .try_into()
            .map_err(|_| EmailError::Encryption)?;
        let mut sealed = Zeroizing::new(encrypted[NONCE_BYTES..].to_vec());
        let plaintext = self
            .aead()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(project_id, job_id, key_version).as_slice()),
                sealed.as_mut_slice(),
            )
            .map_err(|_| EmailError::Encryption)?;
        serde_json::from_slice(plaintext).map_err(|_| EmailError::Encryption)
    }

    fn recipient_fingerprint(&self, recipient: &str) -> [u8; 32] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.key.as_ref());
        let tag = hmac::sign(&key, recipient.trim().to_ascii_lowercase().as_bytes());
        let mut fingerprint = [0_u8; 32];
        fingerprint.copy_from_slice(tag.as_ref());
        fingerprint
    }
}

fn aad(project_id: Uuid, job_id: Uuid, key_version: i32) -> Vec<u8> {
    format!("ffdb.email.job.v1|{key_version}|{project_id}|{job_id}").into_bytes()
}

#[derive(Debug, Clone)]
pub struct TemplateArtifactInput {
    pub project_id: Uuid,
    pub kind: TemplateKind,
    pub version: u64,
    pub source: String,
    pub subject_template: String,
    pub html_template: String,
    pub text_template: String,
    pub allowed_variables: BTreeSet<String>,
}

impl TemplateArtifactInput {
    fn artifact(&self, compiled_at_ms: i64) -> PrecompiledTemplate {
        PrecompiledTemplate {
            project_id: self.project_id.to_string(),
            template_id: kind_name(self.kind).to_owned(),
            kind: self.kind,
            version: self.version,
            source_sha256: hex::encode(Sha256::digest(self.source.as_bytes())),
            subject_template: self.subject_template.clone(),
            html_template: self.html_template.clone(),
            text_template: self.text_template.clone(),
            allowed_variables: self.allowed_variables.clone(),
            compiled_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailTemplateRecord {
    pub artifact: PrecompiledTemplate,
    pub source: String,
    pub artifact_status: String,
    pub compilation_errors: Vec<String>,
    pub published_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct EmailEnqueueRequest {
    pub project_id: Uuid,
    pub kind: TemplateKind,
    pub recipient: String,
    pub from: String,
    pub reply_to: Option<String>,
    pub variables: BTreeMap<String, ScalarValue>,
    pub idempotency_key: String,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub struct OrganizationInvitationRequest {
    pub organization_id: Uuid,
    pub recipient: String,
    pub from: String,
    pub variables: BTreeMap<String, ScalarValue>,
    pub idempotency_key: String,
    pub now_ms: i64,
}

#[derive(Clone, Debug)]
pub struct PgEmailService {
    pool: PgPool,
    cipher: EmailMessageCipher,
}

impl PgEmailService {
    #[must_use]
    pub fn new(pool: PgPool, cipher: EmailMessageCipher) -> Self {
        Self { pool, cipher }
    }

    /// Imports an already-compiled template artifact and performs the bounded
    /// runtime validator again before persistence. This is deliberately not a
    /// React/JavaScript compiler: callers must provide output from the isolated
    /// compiler workflow and an independently computed source digest.
    pub async fn import_precompiled_artifact(
        &self,
        input: TemplateArtifactInput,
        expected_source_sha256: &str,
        actor_api_key_id: Uuid,
        compiled_at_ms: i64,
    ) -> Result<EmailTemplateRecord, EmailError> {
        if input.source.is_empty() || input.source.len() > 1_000_000 {
            return Err(EmailError::InvalidArtifact);
        }
        let artifact = input.artifact(compiled_at_ms);
        if expected_source_sha256.len() != 64
            || !expected_source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || artifact.source_sha256 != expected_source_sha256
        {
            return Err(EmailError::InvalidArtifact);
        }
        artifact.validate()?;
        let version = i64::try_from(input.version).map_err(|_| EmailError::InvalidArtifact)?;
        let variables: Vec<_> = input.allowed_variables.iter().cloned().collect();
        let compilation_job_id = Uuid::now_v7();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| EmailError::RepositoryUnavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO email_template_versions \
             (project_id,kind,version,source,source_sha256,subject_template,html_template,text_template,\
              allowed_variables,compilation_errors,artifact_status,compiled_at,created_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'[]'::jsonb,'validated',to_timestamp($10::double precision/1000),$11)",
        )
        .bind(input.project_id)
        .bind(kind_name(input.kind))
        .bind(version)
        .bind(&input.source)
        .bind(&artifact.source_sha256)
        .bind(&artifact.subject_template)
        .bind(&artifact.html_template)
        .bind(&artifact.text_template)
        .bind(serde_json::json!(variables))
        .bind(compiled_at_ms)
        .bind(actor_api_key_id)
        .execute(&mut *transaction)
        .await;
        if let Err(error) = inserted {
            return Err(
                if error
                    .as_database_error()
                    .is_some_and(|database| database.is_unique_violation())
                {
                    EmailError::TemplateVersionExists
                } else {
                    EmailError::RepositoryUnavailable
                },
            );
        }
        sqlx::query(
            "INSERT INTO email_template_compilation_jobs \
             (id,project_id,kind,version,state,source_sha256,errors,created_by,completed_at) \
             VALUES ($1,$2,$3,$4,'validated',$5,'[]'::jsonb,$6,now())",
        )
        .bind(compilation_job_id)
        .bind(input.project_id)
        .bind(kind_name(input.kind))
        .bind(version)
        .bind(&artifact.source_sha256)
        .bind(actor_api_key_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| EmailError::RepositoryUnavailable)?;
        Ok(EmailTemplateRecord {
            artifact,
            source: input.source,
            artifact_status: "validated".to_owned(),
            compilation_errors: Vec::new(),
            published_at_ms: None,
        })
    }

    pub async fn publish_template(
        &self,
        project_id: Uuid,
        kind: TemplateKind,
        version: u64,
    ) -> Result<EmailTemplateRecord, EmailError> {
        let version = i64::try_from(version).map_err(|_| EmailError::InvalidArtifact)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| EmailError::RepositoryUnavailable)?;
        sqlx::query(
            "UPDATE email_template_versions SET published_at=NULL \
             WHERE project_id=$1 AND kind=$2 AND published_at IS NOT NULL",
        )
        .bind(project_id)
        .bind(kind_name(kind))
        .execute(&mut *transaction)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        let updated = sqlx::query(
            "UPDATE email_template_versions SET published_at=now() \
             WHERE project_id=$1 AND kind=$2 AND version=$3 AND compiled_at IS NOT NULL \
                   AND artifact_status='validated' AND compilation_errors='[]'::jsonb \
             RETURNING project_id,kind,version,source,source_sha256,subject_template,html_template,\
                       text_template,allowed_variables,compilation_errors,artifact_status,\
                       (extract(epoch from compiled_at)*1000)::bigint AS compiled_at_ms,\
                       (extract(epoch from published_at)*1000)::bigint AS published_at_ms",
        )
        .bind(project_id)
        .bind(kind_name(kind))
        .bind(version)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?
        .ok_or(EmailError::TemplateNotFound)?;
        transaction
            .commit()
            .await
            .map_err(|_| EmailError::RepositoryUnavailable)?;
        template_record(&updated)
    }

    pub async fn templates(
        &self,
        project_id: Uuid,
        kind: Option<TemplateKind>,
    ) -> Result<Vec<EmailTemplateRecord>, EmailError> {
        let rows = sqlx::query(
            "SELECT project_id,kind,version,source,source_sha256,subject_template,html_template,\
                    text_template,allowed_variables,compilation_errors,artifact_status,\
                    (extract(epoch from compiled_at)*1000)::bigint AS compiled_at_ms,\
                    (extract(epoch from published_at)*1000)::bigint AS published_at_ms \
             FROM email_template_versions \
             WHERE project_id=$1 AND ($2::text IS NULL OR kind=$2) \
             ORDER BY kind,version DESC LIMIT 500",
        )
        .bind(project_id)
        .bind(kind.map(kind_name))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        rows.iter().map(template_record).collect()
    }

    pub async fn template(
        &self,
        project_id: Uuid,
        kind: TemplateKind,
        version: u64,
    ) -> Result<EmailTemplateRecord, EmailError> {
        let version = i64::try_from(version).map_err(|_| EmailError::InvalidArtifact)?;
        let row = sqlx::query(
            "SELECT project_id,kind,version,source,source_sha256,subject_template,html_template,\
                    text_template,allowed_variables,compilation_errors,artifact_status,\
                    (extract(epoch from compiled_at)*1000)::bigint AS compiled_at_ms,\
                    (extract(epoch from published_at)*1000)::bigint AS published_at_ms \
             FROM email_template_versions WHERE project_id=$1 AND kind=$2 AND version=$3",
        )
        .bind(project_id)
        .bind(kind_name(kind))
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?
        .ok_or(EmailError::TemplateNotFound)?;
        template_record(&row)
    }

    pub async fn enqueue(&self, request: EmailEnqueueRequest) -> Result<Uuid, EmailError> {
        let recipient_fingerprint = self.cipher.recipient_fingerprint(&request.recipient);
        let template = self
            .published_template(request.project_id, request.kind)
            .await?
            .unwrap_or_else(|| {
                default_template(request.project_id.to_string(), request.kind, request.now_ms)
            });
        let message = RuntimeRenderer::new(template.clone())?.render(
            &request.recipient,
            &request.from,
            request.reply_to,
            &request.variables,
            &request.idempotency_key,
        )?;
        let job_id = Uuid::now_v7();
        let encrypted = self.cipher.encrypt(request.project_id, job_id, &message)?;
        let version = i64::try_from(template.version).map_err(|_| EmailError::InvalidArtifact)?;
        let result = sqlx::query(
            "INSERT INTO email_delivery_jobs \
             (id,project_id,kind,template_version,recipient_fingerprint,encrypted_message,\
              encryption_key_version,idempotency_key,state,max_attempts,next_attempt_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'queued',$9,to_timestamp($10::double precision/1000)) \
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(job_id)
        .bind(request.project_id)
        .bind(kind_name(request.kind))
        .bind(version)
        .bind(recipient_fingerprint.as_slice())
        .bind(encrypted)
        .bind(self.cipher.key_version)
        .bind(&request.idempotency_key)
        .bind(MAX_ATTEMPTS)
        .bind(request.now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        if result.is_some() {
            return Ok(job_id);
        }
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM email_delivery_jobs WHERE project_id=$1 AND idempotency_key=$2",
        )
        .bind(request.project_id)
        .bind(&request.idempotency_key)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)
    }

    /// Queues an organization-scoped invitation without manufacturing a
    /// project. The tenant UUID binds ciphertext AAD and idempotency separately
    /// from project-scoped delivery jobs.
    pub async fn enqueue_organization_invitation(
        &self,
        request: OrganizationInvitationRequest,
    ) -> Result<Uuid, EmailError> {
        let template = default_template(
            request.organization_id.to_string(),
            TemplateKind::Invitation,
            request.now_ms,
        );
        let message = RuntimeRenderer::new(template.clone())?.render(
            &request.recipient,
            &request.from,
            None,
            &request.variables,
            &request.idempotency_key,
        )?;
        let job_id = Uuid::now_v7();
        let encrypted = self
            .cipher
            .encrypt(request.organization_id, job_id, &message)?;
        let recipient_fingerprint = self.cipher.recipient_fingerprint(&request.recipient);
        let version = i64::try_from(template.version).map_err(|_| EmailError::InvalidArtifact)?;
        let result = sqlx::query(
            "INSERT INTO email_delivery_jobs \
             (id,organization_id,kind,template_version,recipient_fingerprint,encrypted_message,\
              encryption_key_version,idempotency_key,state,max_attempts,next_attempt_at) \
             VALUES ($1,$2,'invitation',$3,$4,$5,$6,$7,'queued',$8,\
                     to_timestamp($9::double precision/1000)) \
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(job_id)
        .bind(request.organization_id)
        .bind(version)
        .bind(recipient_fingerprint.as_slice())
        .bind(encrypted)
        .bind(self.cipher.key_version)
        .bind(&request.idempotency_key)
        .bind(MAX_ATTEMPTS)
        .bind(request.now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        if result.is_some() {
            return Ok(job_id);
        }
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM email_delivery_jobs WHERE organization_id=$1 AND idempotency_key=$2",
        )
        .bind(request.organization_id)
        .bind(&request.idempotency_key)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)
    }

    async fn published_template(
        &self,
        project_id: Uuid,
        kind: TemplateKind,
    ) -> Result<Option<PrecompiledTemplate>, EmailError> {
        let row = sqlx::query(
            "SELECT project_id,kind,version,source,source_sha256,subject_template,html_template,\
                    text_template,allowed_variables,compilation_errors,artifact_status,\
                    (extract(epoch from compiled_at)*1000)::bigint AS compiled_at_ms,\
                    (extract(epoch from published_at)*1000)::bigint AS published_at_ms \
             FROM email_template_versions WHERE project_id=$1 AND kind=$2 AND published_at IS NOT NULL",
        )
        .bind(project_id)
        .bind(kind_name(kind))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        row.as_ref()
            .map(template_record)
            .transpose()
            .map(|record| record.map(|value| value.artifact))
    }

    pub async fn deliver_one(
        &self,
        transport: &dyn EmailTransport,
        now_ms: i64,
    ) -> Result<bool, EmailError> {
        let Some(job) = self.claim(now_ms).await? else {
            return Ok(false);
        };
        let message = match self.cipher.decrypt(
            job.tenant_id,
            job.id,
            job.encryption_key_version,
            &job.encrypted_message,
        ) {
            Ok(message) => message,
            Err(error) => {
                self.fail(&job, &error, now_ms, true).await?;
                return Ok(true);
            }
        };
        match transport.send(&message).await {
            Ok(provider_id) => {
                sqlx::query(
                    "UPDATE email_delivery_jobs SET state='delivered',provider_message_id=$2,\
                     delivered_at=now(),updated_at=now(),locked_at=NULL,last_error_code=NULL \
                     WHERE id=$1 AND state='processing'",
                )
                .bind(job.id)
                .bind(provider_id.0)
                .execute(&self.pool)
                .await
                .map_err(|_| EmailError::RepositoryUnavailable)?;
            }
            Err(error) => {
                let permanent = !matches!(
                    error,
                    EmailError::ProviderUnavailable
                        | EmailError::ProviderRateLimited
                        | EmailError::ProviderDnsFailed
                );
                self.fail(&job, &error, now_ms, permanent).await?;
            }
        }
        Ok(true)
    }

    async fn claim(&self, now_ms: i64) -> Result<Option<DeliveryJob>, EmailError> {
        let row = sqlx::query(
            "WITH candidate AS (\
               SELECT id FROM email_delivery_jobs \
               WHERE ((state='queued' AND next_attempt_at<=to_timestamp($1::double precision/1000)) \
                  OR (state='processing' AND locked_at<to_timestamp($1::double precision/1000)-interval '5 minutes')) \
               ORDER BY next_attempt_at,created_at FOR UPDATE SKIP LOCKED LIMIT 1\
             ) \
             UPDATE email_delivery_jobs AS jobs SET state='processing',locked_at=now(),\
                    attempt_count=attempt_count+1,updated_at=now() \
             FROM candidate WHERE jobs.id=candidate.id \
             RETURNING jobs.id,COALESCE(jobs.project_id,jobs.organization_id) AS tenant_id,\
                       jobs.encrypted_message,jobs.encryption_key_version,\
                       jobs.attempt_count,jobs.max_attempts",
        )
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        row.map(|row| {
            Ok(DeliveryJob {
                id: row
                    .try_get("id")
                    .map_err(|_| EmailError::RepositoryUnavailable)?,
                tenant_id: row
                    .try_get("tenant_id")
                    .map_err(|_| EmailError::RepositoryUnavailable)?,
                encrypted_message: row
                    .try_get("encrypted_message")
                    .map_err(|_| EmailError::RepositoryUnavailable)?,
                encryption_key_version: row
                    .try_get("encryption_key_version")
                    .map_err(|_| EmailError::RepositoryUnavailable)?,
                attempt_count: row
                    .try_get("attempt_count")
                    .map_err(|_| EmailError::RepositoryUnavailable)?,
                max_attempts: row
                    .try_get("max_attempts")
                    .map_err(|_| EmailError::RepositoryUnavailable)?,
            })
        })
        .transpose()
    }

    async fn fail(
        &self,
        job: &DeliveryJob,
        error: &EmailError,
        now_ms: i64,
        permanent: bool,
    ) -> Result<(), EmailError> {
        let dead = permanent || job.attempt_count >= job.max_attempts;
        let exponent = u32::try_from(job.attempt_count.saturating_sub(1))
            .unwrap_or(16)
            .min(16);
        let delay_ms = 30_000_i64
            .saturating_mul(2_i64.saturating_pow(exponent))
            .min(3_600_000);
        sqlx::query(
            "UPDATE email_delivery_jobs SET state=$2,last_error_code=$3,locked_at=NULL,\
             next_attempt_at=to_timestamp($4::double precision/1000),updated_at=now() \
             WHERE id=$1 AND state='processing'",
        )
        .bind(job.id)
        .bind(if dead { "dead" } else { "queued" })
        .bind(error_code(error))
        .bind(now_ms.saturating_add(delay_ms))
        .execute(&self.pool)
        .await
        .map_err(|_| EmailError::RepositoryUnavailable)?;
        Ok(())
    }
}

#[derive(Debug)]
struct DeliveryJob {
    id: Uuid,
    tenant_id: Uuid,
    encrypted_message: Vec<u8>,
    encryption_key_version: i32,
    attempt_count: i32,
    max_attempts: i32,
}

pub struct OutboxWorkerHandle {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for OutboxWorkerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutboxWorkerHandle")
            .finish_non_exhaustive()
    }
}

impl OutboxWorkerHandle {
    #[must_use]
    pub fn spawn(service: Arc<PgEmailService>, transport: Arc<dyn EmailTransport>) -> Self {
        let (send, mut receive) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut receive => break,
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        for _ in 0..25 {
                            match service.deliver_one(transport.as_ref(), epoch_ms()).await {
                                Ok(true) => {}
                                Ok(false) => break,
                                Err(_) => {
                                    tracing::warn!(event = "email.outbox.unavailable", "email outbox worker iteration failed");
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
        Self {
            shutdown: Some(send),
            task,
        }
    }

    pub async fn shutdown(mut self) {
        if let Some(send) = self.shutdown.take() {
            let _ignored = send.send(());
        }
        let _ignored = self.task.await;
    }
}

fn template_record(row: &sqlx::postgres::PgRow) -> Result<EmailTemplateRecord, EmailError> {
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|_| EmailError::RepositoryUnavailable)?;
    let kind: String = row
        .try_get("kind")
        .map_err(|_| EmailError::RepositoryUnavailable)?;
    let version: i64 = row
        .try_get("version")
        .map_err(|_| EmailError::RepositoryUnavailable)?;
    let variables: serde_json::Value = row
        .try_get("allowed_variables")
        .map_err(|_| EmailError::RepositoryUnavailable)?;
    let errors: serde_json::Value = row
        .try_get("compilation_errors")
        .map_err(|_| EmailError::RepositoryUnavailable)?;
    let artifact = PrecompiledTemplate {
        project_id: project_id.to_string(),
        template_id: kind.clone(),
        kind: parse_kind(&kind)?,
        version: u64::try_from(version).map_err(|_| EmailError::RepositoryUnavailable)?,
        source_sha256: row
            .try_get("source_sha256")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        subject_template: row
            .try_get("subject_template")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        html_template: row
            .try_get("html_template")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        text_template: row
            .try_get("text_template")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        allowed_variables: serde_json::from_value::<Vec<String>>(variables)
            .map_err(|_| EmailError::RepositoryUnavailable)?
            .into_iter()
            .collect(),
        compiled_at_ms: row
            .try_get("compiled_at_ms")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
    };
    artifact.validate()?;
    Ok(EmailTemplateRecord {
        artifact,
        source: row
            .try_get("source")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        artifact_status: row
            .try_get("artifact_status")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        compilation_errors: serde_json::from_value(errors)
            .map_err(|_| EmailError::RepositoryUnavailable)?,
        published_at_ms: row
            .try_get("published_at_ms")
            .map_err(|_| EmailError::RepositoryUnavailable)?,
    })
}

#[must_use]
pub fn kind_name(kind: TemplateKind) -> &'static str {
    match kind {
        TemplateKind::EmailVerification => "verification",
        TemplateKind::PasswordReset => "password_reset",
        TemplateKind::EmailChange => "email_change",
        TemplateKind::Invitation => "invitation",
        TemplateKind::MagicLink => "magic_link",
    }
}

pub fn parse_kind(value: &str) -> Result<TemplateKind, EmailError> {
    match value {
        "verification" => Ok(TemplateKind::EmailVerification),
        "password_reset" => Ok(TemplateKind::PasswordReset),
        "email_change" => Ok(TemplateKind::EmailChange),
        "invitation" => Ok(TemplateKind::Invitation),
        "magic_link" => Ok(TemplateKind::MagicLink),
        _ => Err(EmailError::InvalidArtifact),
    }
}

fn error_code(error: &EmailError) -> &'static str {
    match error {
        EmailError::ProviderUnavailable => "provider_unavailable",
        EmailError::ProviderRateLimited => "provider_rate_limited",
        EmailError::ProviderRejected => "provider_rejected",
        EmailError::InvalidProviderResponse => "provider_invalid_response",
        EmailError::Encryption => "message_decryption_failed",
        _ => "delivery_invalid",
    }
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_cipher_round_trips_and_binds_project_and_job() -> Result<(), EmailError> {
        let cipher = EmailMessageCipher::new([7_u8; 32], 1)?;
        let project_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let message = RenderedEmail {
            to: "user@example.test".to_owned(),
            from: "auth@example.test".to_owned(),
            reply_to: None,
            subject: "Verify".to_owned(),
            html: "<p>Verify</p>".to_owned(),
            text: "Verify".to_owned(),
            template_id: "verification".to_owned(),
            template_version: 1,
            idempotency_key: "verify-0123456789".to_owned(), // gitleaks:allow -- synthetic test key
        };
        let encrypted = cipher.encrypt(project_id, job_id, &message)?;
        assert!(!String::from_utf8_lossy(&encrypted).contains("user@example.test"));
        assert_eq!(cipher.decrypt(project_id, job_id, 1, &encrypted)?, message);
        assert_eq!(
            cipher.decrypt(Uuid::now_v7(), job_id, 1, &encrypted),
            Err(EmailError::Encryption)
        );
        Ok(())
    }
}
