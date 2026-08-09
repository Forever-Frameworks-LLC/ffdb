//! Deterministic logical replication core.
//!
//! Server sequence is the only conflict clock. Cursors are opaque authenticated
//! capabilities bound to a project, schema, and authorization scope.

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncContext {
    pub project_id: String,
    pub subject: String,
    pub role: String,
    pub scope_fingerprint: String,
    /// Device/client id resolved from a verified session, not request JSON.
    pub trusted_client_id: String,
}

pub trait SyncAuthorizer {
    fn can_read(&self, context: &SyncContext, table: &str, row: &Map<String, Value>) -> bool;
    fn can_write(
        &self,
        context: &SyncContext,
        table: &str,
        operation: MutationOperation,
        row: &Map<String, Value>,
    ) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOperation {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientMutation {
    pub mutation_id: String,
    pub table: String,
    pub primary_key: Value,
    pub operation: MutationOperation,
    #[serde(default)]
    pub values: Map<String, Value>,
    pub base_row_version: Option<u64>,
    /// Diagnostic metadata only. It never participates in conflict ordering.
    pub client_timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushBatch {
    pub client_id: String,
    pub schema_version: u64,
    pub mode: BatchMode,
    pub mutations: Vec<ClientMutation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchMode {
    Atomic,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationReceipt {
    pub mutation_id: String,
    pub sequence: u64,
    pub row_version: u64,
    pub outcome: MutationOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOutcome {
    Applied,
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationFailure {
    pub mutation_id: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushResult {
    pub accepted: Vec<MutationReceipt>,
    pub rejected: Vec<MutationFailure>,
    pub cursor: OpaqueCursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplicaRow {
    pub table: String,
    pub primary_key: Value,
    pub values: Map<String, Value>,
    pub row_version: u64,
    pub server_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogicalEvent {
    Upsert {
        sequence: u64,
        transaction_id: String,
        table: String,
        primary_key: Value,
        values: Map<String, Value>,
        row_version: u64,
        actor: String,
        schema_version: u64,
        server_commit_ms: i64,
        client_mutation_id: Option<String>,
    },
    Delete {
        sequence: u64,
        transaction_id: String,
        table: String,
        primary_key: Value,
        tombstone: Map<String, Value>,
        row_version: u64,
        actor: String,
        schema_version: u64,
        server_commit_ms: i64,
        client_mutation_id: Option<String>,
    },
    ResnapshotRequired {
        sequence: u64,
        reason: ResnapshotReason,
        schema_version: u64,
        server_commit_ms: i64,
    },
}

impl LogicalEvent {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Upsert { sequence, .. }
            | Self::Delete { sequence, .. }
            | Self::ResnapshotRequired { sequence, .. } => *sequence,
        }
    }

    fn committed_at_ms(&self) -> i64 {
        match self {
            Self::Upsert {
                server_commit_ms, ..
            }
            | Self::Delete {
                server_commit_ms, ..
            }
            | Self::ResnapshotRequired {
                server_commit_ms, ..
            } => *server_commit_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResnapshotReason {
    CursorExpired,
    SchemaChanged,
    AuthorizationChanged,
    PolicyChanged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u64,
    pub rows: Vec<ReplicaRow>,
    pub cursor: OpaqueCursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullResult {
    pub events: Vec<LogicalEvent>,
    pub cursor: OpaqueCursor,
    pub has_more: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueCursor(String);

impl OpaqueCursor {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OpaqueCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OpaqueCursor([REDACTED])")
    }
}

impl Drop for OpaqueCursor {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub cursor_ttl_ms: i64,
    pub change_retention_ms: i64,
    pub tombstone_retention_ms: i64,
    pub max_pull_events: usize,
    pub max_push_mutations: usize,
    pub idempotency_retention_ms: i64,
    pub max_idempotency_records: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredRow {
    primary_key: Value,
    values: Map<String, Value>,
    row_version: u64,
    server_sequence: u64,
    deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct IdempotencyRecord {
    payload_hash: [u8; 32],
    receipt: MutationReceipt,
    committed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct EngineState {
    next_sequence: u64,
    minimum_available_sequence: u64,
    transaction_counter: u64,
    rows: HashMap<String, StoredRow>,
    events: Vec<LogicalEvent>,
    idempotency: HashMap<String, IdempotencyRecord>,
}

const CHECKPOINT_FORMAT_VERSION: u16 = 1;
const MAX_CHECKPOINT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointEnvelope {
    format_version: u16,
    project_id: String,
    schema_version: u64,
    state: EngineState,
}

/// Opaque durable engine state. The cursor signing key is deliberately omitted
/// and must be supplied again from secret storage when restoring.
#[derive(Clone, PartialEq, Eq)]
pub struct SyncCheckpoint(Vec<u8>);

impl SyncCheckpoint {
    /// Reconstruct a checkpoint read from a trusted durable store. Structural
    /// validation is completed by [`SyncEngine::restore`].
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, SyncError> {
        if bytes.is_empty() || bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(SyncError::InvalidCheckpoint);
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SyncCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncCheckpoint")
            .field("bytes", &"[REDACTED]")
            .field("length", &self.0.len())
            .finish()
    }
}

impl Drop for SyncCheckpoint {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorClaims {
    project_id: String,
    subject: String,
    trusted_client_id: String,
    sequence: u64,
    schema_version: u64,
    scope_fingerprint: String,
    issued_at_ms: i64,
    expires_at_ms: i64,
}

#[derive(Clone)]
struct CursorCodec {
    secret: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for CursorCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CursorCodec")
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl CursorCodec {
    fn encode(&self, claims: &CursorClaims) -> Result<OpaqueCursor, SyncError> {
        let payload = serde_json::to_vec(claims).map_err(|_| SyncError::InvalidCursor)?;
        let mut mac = HmacSha256::new_from_slice(self.secret.as_slice())
            .map_err(|_| SyncError::InvalidConfiguration)?;
        mac.update(&payload);
        Ok(OpaqueCursor(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        )))
    }

    fn decode(&self, cursor: &OpaqueCursor) -> Result<CursorClaims, SyncError> {
        const MAX_CURSOR_BYTES: usize = 2_048;
        const MAX_PAYLOAD_ENCODED_BYTES: usize = 1_900;
        const MAX_PAYLOAD_BYTES: usize = 1_024;
        if cursor.0.len() > MAX_CURSOR_BYTES {
            return Err(SyncError::InvalidCursor);
        }
        let (payload, signature) = cursor.0.split_once('.').ok_or(SyncError::InvalidCursor)?;
        if payload.len() > MAX_PAYLOAD_ENCODED_BYTES || signature.len() != 43 {
            return Err(SyncError::InvalidCursor);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| SyncError::InvalidCursor)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(SyncError::InvalidCursor);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| SyncError::InvalidCursor)?;
        if signature.len() != 32 {
            return Err(SyncError::InvalidCursor);
        }
        let mut mac = HmacSha256::new_from_slice(self.secret.as_slice())
            .map_err(|_| SyncError::InvalidConfiguration)?;
        mac.update(&payload);
        mac.verify_slice(&signature)
            .map_err(|_| SyncError::InvalidCursor)?;
        serde_json::from_slice(&payload).map_err(|_| SyncError::InvalidCursor)
    }
}

#[derive(Debug, Clone)]
pub struct SyncEngine {
    project_id: String,
    schema_version: u64,
    config: SyncConfig,
    cursor_codec: CursorCodec,
    state: EngineState,
}

impl SyncEngine {
    pub fn new(
        project_id: impl Into<String>,
        schema_version: u64,
        secret: impl AsRef<[u8]>,
        config: SyncConfig,
    ) -> Result<Self, SyncError> {
        let project_id = project_id.into();
        if project_id.is_empty()
            || schema_version == 0
            || secret.as_ref().len() < 32
            || config.cursor_ttl_ms <= 0
            || config.change_retention_ms <= 0
            || config.tombstone_retention_ms < config.change_retention_ms
            || config.max_pull_events == 0
            || config.max_push_mutations == 0
            || config.idempotency_retention_ms <= 0
            || config.max_idempotency_records == 0
        {
            return Err(SyncError::InvalidConfiguration);
        }
        Ok(Self {
            project_id,
            schema_version,
            config,
            cursor_codec: CursorCodec {
                secret: Zeroizing::new(secret.as_ref().to_vec()),
            },
            state: EngineState {
                next_sequence: 1,
                minimum_available_sequence: 0,
                transaction_counter: 1,
                rows: HashMap::new(),
                events: Vec::new(),
                idempotency: HashMap::new(),
            },
        })
    }

    pub fn schema_version(&self) -> u64 {
        self.schema_version
    }

    /// Serialize all logical rows, changes, tombstones, and idempotency
    /// receipts for an atomic durable-store commit.
    pub fn checkpoint(&self) -> Result<SyncCheckpoint, SyncError> {
        let bytes = serde_json::to_vec(&CheckpointEnvelope {
            format_version: CHECKPOINT_FORMAT_VERSION,
            project_id: self.project_id.clone(),
            schema_version: self.schema_version,
            state: self.state.clone(),
        })
        .map_err(|_| SyncError::InvalidCheckpoint)?;
        SyncCheckpoint::from_bytes(bytes)
    }

    /// Restore an engine from a versioned checkpoint while obtaining the
    /// cursor signing key from current secret storage rather than persisted
    /// engine state.
    pub fn restore(
        project_id: impl Into<String>,
        secret: impl AsRef<[u8]>,
        config: SyncConfig,
        checkpoint: &SyncCheckpoint,
    ) -> Result<Self, SyncError> {
        let project_id = project_id.into();
        let envelope: CheckpointEnvelope = serde_json::from_slice(checkpoint.as_bytes())
            .map_err(|_| SyncError::InvalidCheckpoint)?;
        if envelope.format_version != CHECKPOINT_FORMAT_VERSION
            || envelope.project_id != project_id
            || envelope.schema_version == 0
        {
            return Err(SyncError::InvalidCheckpoint);
        }
        validate_state(&envelope.state, &config)?;
        let mut engine = Self::new(project_id, envelope.schema_version, secret, config)?;
        engine.state = envelope.state;
        Ok(engine)
    }

    pub fn snapshot<A: SyncAuthorizer>(
        &self,
        context: &SyncContext,
        authorizer: &A,
        now_ms: i64,
    ) -> Result<Snapshot, SyncError> {
        self.verify_context(context)?;
        let mut rows = Vec::new();
        for (key, row) in &self.state.rows {
            if row.deleted_at_ms.is_some() {
                continue;
            }
            let table = key
                .split_once('\u{1f}')
                .map(|(table, _)| table)
                .unwrap_or("");
            if authorizer.can_read(context, table, &row.values) {
                rows.push(ReplicaRow {
                    table: table.to_owned(),
                    primary_key: row.primary_key.clone(),
                    values: row.values.clone(),
                    row_version: row.row_version,
                    server_sequence: row.server_sequence,
                });
            }
        }
        rows.sort_by(|left, right| {
            left.table
                .cmp(&right.table)
                .then(left.server_sequence.cmp(&right.server_sequence))
        });
        let sequence = self.latest_sequence();
        Ok(Snapshot {
            schema_version: self.schema_version,
            rows,
            cursor: self.cursor(context, sequence, now_ms)?,
        })
    }

    pub fn pull<A: SyncAuthorizer>(
        &self,
        context: &SyncContext,
        cursor: &OpaqueCursor,
        requested_limit: usize,
        authorizer: &A,
        now_ms: i64,
    ) -> Result<PullResult, SyncError> {
        self.verify_context(context)?;
        let claims = self.cursor_codec.decode(cursor)?;
        if claims.project_id != self.project_id {
            return Err(SyncError::WrongProject);
        }
        if claims.subject != context.subject
            || claims.trusted_client_id != context.trusted_client_id
        {
            return Err(SyncError::InvalidCursor);
        }
        if now_ms < claims.issued_at_ms {
            return Err(SyncError::InvalidCursor);
        }
        if now_ms >= claims.expires_at_ms {
            return Err(SyncError::ResnapshotRequired(
                ResnapshotReason::CursorExpired,
            ));
        }
        if claims.schema_version != self.schema_version {
            return Err(SyncError::ResnapshotRequired(
                ResnapshotReason::SchemaChanged,
            ));
        }
        if claims.scope_fingerprint != context.scope_fingerprint {
            return Err(SyncError::ResnapshotRequired(
                ResnapshotReason::AuthorizationChanged,
            ));
        }
        if claims.sequence < self.state.minimum_available_sequence {
            return Err(SyncError::ResnapshotRequired(
                ResnapshotReason::CursorExpired,
            ));
        }

        let limit = requested_limit.clamp(1, self.config.max_pull_events);
        let mut events = Vec::new();
        let mut examined_sequence = claims.sequence;
        let mut has_more = false;
        for event in self
            .state
            .events
            .iter()
            .filter(|event| event.sequence() > claims.sequence)
        {
            examined_sequence = event.sequence();
            if is_visible(event, context, authorizer) {
                if events.len() == limit {
                    has_more = true;
                    break;
                }
                events.push(event.clone());
            }
        }
        if !has_more {
            examined_sequence = self.latest_sequence();
        }
        Ok(PullResult {
            events,
            cursor: self.cursor(context, examined_sequence, now_ms)?,
            has_more,
        })
    }

    pub fn push<A: SyncAuthorizer>(
        &mut self,
        context: &SyncContext,
        batch: PushBatch,
        authorizer: &A,
        now_ms: i64,
    ) -> Result<PushResult, SyncError> {
        self.verify_context(context)?;
        validate_batch(
            &batch,
            context,
            self.schema_version,
            self.config.max_push_mutations,
        )?;
        match batch.mode {
            BatchMode::Atomic => {
                let original = self.state.clone();
                let result = self.apply_batch(context, &batch, authorizer, now_ms);
                if result.is_err() {
                    self.state = original;
                }
                result
            }
            BatchMode::Partial => self.apply_partial(context, &batch, authorizer, now_ms),
        }
    }

    pub fn invalidate(
        &mut self,
        reason: ResnapshotReason,
        new_schema_version: Option<u64>,
        now_ms: i64,
    ) -> Result<u64, SyncError> {
        if let Some(version) = new_schema_version {
            if version <= self.schema_version {
                return Err(SyncError::InvalidSchemaVersion);
            }
            self.schema_version = version;
        }
        let sequence = self.allocate_sequence();
        self.state.events.push(LogicalEvent::ResnapshotRequired {
            sequence,
            reason,
            schema_version: self.schema_version,
            server_commit_ms: now_ms,
        });
        Ok(sequence)
    }

    /// Drops expired change history, old idempotency records, and retained deletes.
    pub fn compact(&mut self, now_ms: i64) -> CompactionResult {
        let change_cutoff = now_ms.saturating_sub(self.config.change_retention_ms);
        let tombstone_cutoff = now_ms.saturating_sub(self.config.tombstone_retention_ms);
        let mut removed_events = 0;
        let mut minimum = self.state.minimum_available_sequence;
        self.state.events.retain(|event| {
            let retain = event.committed_at_ms() >= change_cutoff;
            if !retain {
                removed_events += 1;
                minimum = minimum.max(event.sequence());
            }
            retain
        });
        self.state.minimum_available_sequence = minimum;

        let before = self.state.rows.len();
        self.state.rows.retain(|_, row| {
            row.deleted_at_ms
                .is_none_or(|deleted_at| deleted_at >= tombstone_cutoff)
        });
        let idempotency_cutoff = now_ms.saturating_sub(self.config.idempotency_retention_ms);
        self.state
            .idempotency
            .retain(|_, record| record.committed_at_ms >= idempotency_cutoff);
        CompactionResult {
            removed_events,
            removed_tombstones: before.saturating_sub(self.state.rows.len()),
            minimum_available_sequence: self.state.minimum_available_sequence,
        }
    }

    fn apply_batch<A: SyncAuthorizer>(
        &mut self,
        context: &SyncContext,
        batch: &PushBatch,
        authorizer: &A,
        now_ms: i64,
    ) -> Result<PushResult, SyncError> {
        let transaction_id = self.allocate_transaction_id();
        let mut accepted = Vec::with_capacity(batch.mutations.len());
        for mutation in &batch.mutations {
            accepted.push(self.apply_mutation(
                context,
                &transaction_id,
                mutation,
                authorizer,
                now_ms,
            )?);
        }
        Ok(PushResult {
            accepted,
            rejected: Vec::new(),
            cursor: self.cursor(context, self.latest_sequence(), now_ms)?,
        })
    }

    fn apply_partial<A: SyncAuthorizer>(
        &mut self,
        context: &SyncContext,
        batch: &PushBatch,
        authorizer: &A,
        now_ms: i64,
    ) -> Result<PushResult, SyncError> {
        let transaction_id = self.allocate_transaction_id();
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for mutation in &batch.mutations {
            match self.apply_mutation(context, &transaction_id, mutation, authorizer, now_ms) {
                Ok(receipt) => accepted.push(receipt),
                Err(error) => rejected.push(MutationFailure {
                    mutation_id: mutation.mutation_id.clone(),
                    code: error.code().to_owned(),
                }),
            }
        }
        Ok(PushResult {
            accepted,
            rejected,
            cursor: self.cursor(context, self.latest_sequence(), now_ms)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_mutation<A: SyncAuthorizer>(
        &mut self,
        context: &SyncContext,
        transaction_id: &str,
        mutation: &ClientMutation,
        authorizer: &A,
        now_ms: i64,
    ) -> Result<MutationReceipt, SyncError> {
        validate_mutation(mutation)?;
        let idempotency_key = format!(
            "{}\u{1f}{}\u{1f}{}",
            context.subject, context.trusted_client_id, mutation.mutation_id
        );
        let payload = serde_json::to_vec(mutation).map_err(|_| SyncError::InvalidMutation)?;
        let payload_hash: [u8; 32] = Sha256::digest(payload).into();
        let row_key = row_key(&mutation.table, &mutation.primary_key)?;
        let prior = self.state.rows.get(&row_key).cloned();
        let authorization_row = if mutation.operation == MutationOperation::Delete {
            prior
                .as_ref()
                .map(|row| row.values.clone())
                .unwrap_or_else(|| mutation.values.clone())
        } else {
            mutation.values.clone()
        };
        if !authorizer.can_write(
            context,
            &mutation.table,
            mutation.operation,
            &authorization_row,
        ) {
            return Err(SyncError::RlsDenied);
        }
        if let Some(record) = self.state.idempotency.get(&idempotency_key) {
            if record.payload_hash != payload_hash {
                return Err(SyncError::MutationIdReused);
            }
            let mut receipt = record.receipt.clone();
            receipt.outcome = MutationOutcome::AlreadyApplied;
            return Ok(receipt);
        }
        let idempotency_cutoff = now_ms.saturating_sub(self.config.idempotency_retention_ms);
        self.state
            .idempotency
            .retain(|_, record| record.committed_at_ms >= idempotency_cutoff);
        if self.state.idempotency.len() >= self.config.max_idempotency_records {
            return Err(SyncError::IdempotencyCapacityReached);
        }

        let row_version = prior
            .as_ref()
            .map(|row| row.row_version.saturating_add(1))
            .unwrap_or(1);
        let sequence = self.allocate_sequence();
        let event = match mutation.operation {
            MutationOperation::Upsert => {
                self.state.rows.insert(
                    row_key,
                    StoredRow {
                        primary_key: mutation.primary_key.clone(),
                        values: mutation.values.clone(),
                        row_version,
                        server_sequence: sequence,
                        deleted_at_ms: None,
                    },
                );
                LogicalEvent::Upsert {
                    sequence,
                    transaction_id: transaction_id.to_owned(),
                    table: mutation.table.clone(),
                    primary_key: mutation.primary_key.clone(),
                    values: mutation.values.clone(),
                    row_version,
                    actor: context.subject.clone(),
                    schema_version: self.schema_version,
                    server_commit_ms: now_ms,
                    client_mutation_id: Some(mutation.mutation_id.clone()),
                }
            }
            MutationOperation::Delete => {
                let tombstone = prior.map(|row| row.values).unwrap_or_default();
                self.state.rows.insert(
                    row_key,
                    StoredRow {
                        primary_key: mutation.primary_key.clone(),
                        values: tombstone.clone(),
                        row_version,
                        server_sequence: sequence,
                        deleted_at_ms: Some(now_ms),
                    },
                );
                LogicalEvent::Delete {
                    sequence,
                    transaction_id: transaction_id.to_owned(),
                    table: mutation.table.clone(),
                    primary_key: mutation.primary_key.clone(),
                    tombstone,
                    row_version,
                    actor: context.subject.clone(),
                    schema_version: self.schema_version,
                    server_commit_ms: now_ms,
                    client_mutation_id: Some(mutation.mutation_id.clone()),
                }
            }
        };
        self.state.events.push(event);
        let receipt = MutationReceipt {
            mutation_id: mutation.mutation_id.clone(),
            sequence,
            row_version,
            outcome: MutationOutcome::Applied,
        };
        self.state.idempotency.insert(
            idempotency_key,
            IdempotencyRecord {
                payload_hash,
                receipt: receipt.clone(),
                committed_at_ms: now_ms,
            },
        );
        Ok(receipt)
    }

    fn verify_context(&self, context: &SyncContext) -> Result<(), SyncError> {
        if context.project_id != self.project_id {
            return Err(SyncError::WrongProject);
        }
        if context.subject.is_empty()
            || context.role.is_empty()
            || context.scope_fingerprint.is_empty()
            || context.trusted_client_id.is_empty()
            || context.trusted_client_id.len() > 200
        {
            return Err(SyncError::InvalidContext);
        }
        Ok(())
    }

    fn cursor(
        &self,
        context: &SyncContext,
        sequence: u64,
        now_ms: i64,
    ) -> Result<OpaqueCursor, SyncError> {
        self.cursor_codec.encode(&CursorClaims {
            project_id: self.project_id.clone(),
            subject: context.subject.clone(),
            trusted_client_id: context.trusted_client_id.clone(),
            sequence,
            schema_version: self.schema_version,
            scope_fingerprint: context.scope_fingerprint.clone(),
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(self.config.cursor_ttl_ms),
        })
    }

    fn allocate_sequence(&mut self) -> u64 {
        let sequence = self.state.next_sequence;
        self.state.next_sequence = self.state.next_sequence.saturating_add(1);
        sequence
    }

    fn latest_sequence(&self) -> u64 {
        self.state.next_sequence.saturating_sub(1)
    }

    fn allocate_transaction_id(&mut self) -> String {
        let id = format!("tx-{}", self.state.transaction_counter);
        self.state.transaction_counter = self.state.transaction_counter.saturating_add(1);
        id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub removed_events: usize,
    pub removed_tombstones: usize,
    pub minimum_available_sequence: u64,
}

fn validate_batch(
    batch: &PushBatch,
    context: &SyncContext,
    schema_version: u64,
    max_mutations: usize,
) -> Result<(), SyncError> {
    if batch.schema_version != schema_version {
        return Err(SyncError::SchemaMismatch {
            expected: schema_version,
            received: batch.schema_version,
        });
    }
    if batch.client_id.is_empty()
        || batch.client_id != context.trusted_client_id
        || batch.mutations.is_empty()
        || batch.mutations.len() > max_mutations
    {
        return Err(SyncError::InvalidBatch);
    }
    Ok(())
}

fn validate_mutation(mutation: &ClientMutation) -> Result<(), SyncError> {
    if mutation.mutation_id.is_empty()
        || mutation.mutation_id.len() > 200
        || mutation.table.is_empty()
        || mutation.table.starts_with("__ffdb_")
        || !mutation
            .table
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        || mutation.primary_key.is_null()
        || (mutation.operation == MutationOperation::Upsert && mutation.values.is_empty())
    {
        return Err(SyncError::InvalidMutation);
    }
    Ok(())
}

fn row_key(table: &str, primary_key: &Value) -> Result<String, SyncError> {
    let primary_key = serde_json::to_string(primary_key).map_err(|_| SyncError::InvalidMutation)?;
    Ok(format!("{table}\u{1f}{primary_key}"))
}

fn is_visible<A: SyncAuthorizer>(
    event: &LogicalEvent,
    context: &SyncContext,
    authorizer: &A,
) -> bool {
    match event {
        LogicalEvent::Upsert { table, values, .. } => authorizer.can_read(context, table, values),
        LogicalEvent::Delete {
            table, tombstone, ..
        } => authorizer.can_read(context, table, tombstone),
        LogicalEvent::ResnapshotRequired { .. } => true,
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SyncError {
    #[error("sync configuration is invalid")]
    InvalidConfiguration,
    #[error("sync context is invalid")]
    InvalidContext,
    #[error("cursor is invalid")]
    InvalidCursor,
    #[error("cursor belongs to another project")]
    WrongProject,
    #[error("client must discard its replica and fetch a snapshot: {0:?}")]
    ResnapshotRequired(ResnapshotReason),
    #[error("schema version mismatch: expected {expected}, received {received}")]
    SchemaMismatch { expected: u64, received: u64 },
    #[error("schema version is invalid")]
    InvalidSchemaVersion,
    #[error("push batch is invalid")]
    InvalidBatch,
    #[error("mutation is invalid")]
    InvalidMutation,
    #[error("row-level security denied the mutation")]
    RlsDenied,
    #[error("mutation id was reused with different content")]
    MutationIdReused,
    #[error("idempotency retention capacity reached")]
    IdempotencyCapacityReached,
    #[error("durable sync checkpoint is invalid")]
    InvalidCheckpoint,
}

impl SyncError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "sync.invalid_configuration",
            Self::InvalidContext => "sync.invalid_context",
            Self::InvalidCursor => "sync.invalid_cursor",
            Self::WrongProject => "sync.wrong_project",
            Self::ResnapshotRequired(_) => "sync.resnapshot_required",
            Self::SchemaMismatch { .. } => "sync.schema_mismatch",
            Self::InvalidSchemaVersion => "sync.invalid_schema_version",
            Self::InvalidBatch => "sync.invalid_batch",
            Self::InvalidMutation => "sync.invalid_mutation",
            Self::RlsDenied => "sync.rls_denied",
            Self::MutationIdReused => "sync.mutation_id_reused",
            Self::IdempotencyCapacityReached => "sync.idempotency_capacity_reached",
            Self::InvalidCheckpoint => "sync.invalid_checkpoint",
        }
    }
}

fn validate_state(state: &EngineState, config: &SyncConfig) -> Result<(), SyncError> {
    if state.next_sequence == 0
        || state.minimum_available_sequence > state.next_sequence.saturating_sub(1)
        || state.idempotency.len() > config.max_idempotency_records
    {
        return Err(SyncError::InvalidCheckpoint);
    }
    let mut prior_sequence = 0;
    for event in &state.events {
        let sequence = event.sequence();
        if sequence <= prior_sequence || sequence >= state.next_sequence {
            return Err(SyncError::InvalidCheckpoint);
        }
        prior_sequence = sequence;
    }
    if state.rows.iter().any(|(key, row)| {
        !key.contains('\u{1f}')
            || row.server_sequence == 0
            || row.server_sequence >= state.next_sequence
            || row.row_version == 0
    }) || state.idempotency.values().any(|record| {
        record.receipt.sequence == 0 || record.receipt.sequence >= state.next_sequence
    }) {
        return Err(SyncError::InvalidCheckpoint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OwnerOnly;

    impl SyncAuthorizer for OwnerOnly {
        fn can_read(&self, context: &SyncContext, _table: &str, row: &Map<String, Value>) -> bool {
            row.get("owner_id").and_then(Value::as_str) == Some(context.subject.as_str())
        }

        fn can_write(
            &self,
            context: &SyncContext,
            _table: &str,
            _operation: MutationOperation,
            row: &Map<String, Value>,
        ) -> bool {
            self.can_read(context, "", row)
        }
    }

    fn config() -> SyncConfig {
        SyncConfig {
            cursor_ttl_ms: 30_000,
            change_retention_ms: 10_000,
            tombstone_retention_ms: 20_000,
            max_pull_events: 100,
            max_push_mutations: 10,
            idempotency_retention_ms: 30_000,
            max_idempotency_records: 100,
        }
    }

    fn context(subject: &str, scope: &str) -> SyncContext {
        SyncContext {
            project_id: "project-1".to_owned(),
            subject: subject.to_owned(),
            role: "authenticated".to_owned(),
            scope_fingerprint: scope.to_owned(),
            trusted_client_id: "client-a".to_owned(),
        }
    }

    fn mutation(id: &str, owner: &str, title: &str) -> ClientMutation {
        ClientMutation {
            mutation_id: id.to_owned(),
            table: "documents".to_owned(),
            primary_key: Value::String("doc-1".to_owned()),
            operation: MutationOperation::Upsert,
            values: Map::from_iter([
                ("owner_id".to_owned(), Value::String(owner.to_owned())),
                ("title".to_owned(), Value::String(title.to_owned())),
            ]),
            base_row_version: None,
            client_timestamp_ms: Some(1),
        }
    }

    fn batch(mutations: Vec<ClientMutation>) -> PushBatch {
        PushBatch {
            client_id: "client-a".to_owned(),
            schema_version: 1,
            mode: BatchMode::Atomic,
            mutations,
        }
    }

    #[test]
    fn lww_uses_server_order_and_idempotency_is_stable() -> Result<(), SyncError> {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "alice:v1");
        let first = engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "first")]),
            &OwnerOnly,
            1_000,
        )?;
        let second = engine.push(
            &alice,
            batch(vec![mutation("m-2", "alice", "second")]),
            &OwnerOnly,
            900,
        )?;
        assert!(second.accepted[0].sequence > first.accepted[0].sequence);
        assert_eq!(second.accepted[0].row_version, 2);

        let replay = engine.push(
            &alice,
            batch(vec![mutation("m-2", "alice", "second")]),
            &OwnerOnly,
            2_000,
        )?;
        assert_eq!(replay.accepted[0].outcome, MutationOutcome::AlreadyApplied);
        assert_eq!(replay.accepted[0].sequence, second.accepted[0].sequence);
        assert_eq!(
            engine.snapshot(&alice, &OwnerOnly, 2_000)?.rows[0].values["title"],
            "second"
        );
        Ok(())
    }

    #[test]
    fn pull_and_snapshot_apply_read_rls() -> Result<(), SyncError> {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "alice:v1");
        let bob = context("bob", "bob:v1");
        let bob_start = engine.snapshot(&bob, &OwnerOnly, 900)?.cursor;
        engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "secret")]),
            &OwnerOnly,
            1_000,
        )?;
        assert!(engine.snapshot(&bob, &OwnerOnly, 1_500)?.rows.is_empty());
        assert!(
            engine
                .pull(&bob, &bob_start, 100, &OwnerOnly, 1_500)?
                .events
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn delete_tombstone_prevents_older_state_and_recreate_advances_version() -> Result<(), SyncError>
    {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "alice:v1");
        engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "one")]),
            &OwnerOnly,
            1_000,
        )?;
        let mut delete = mutation("m-2", "alice", "ignored");
        delete.operation = MutationOperation::Delete;
        delete.values.clear();
        let deleted = engine.push(&alice, batch(vec![delete]), &OwnerOnly, 2_000)?;
        assert!(engine.snapshot(&alice, &OwnerOnly, 2_100)?.rows.is_empty());
        let recreated = engine.push(
            &alice,
            batch(vec![mutation("m-3", "alice", "reborn")]),
            &OwnerOnly,
            3_000,
        )?;
        assert_eq!(deleted.accepted[0].row_version, 2);
        assert_eq!(recreated.accepted[0].row_version, 3);
        Ok(())
    }

    #[test]
    fn changed_scope_and_expired_history_require_resnapshot() -> Result<(), SyncError> {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "scope:v1");
        let cursor = engine.snapshot(&alice, &OwnerOnly, 1_000)?.cursor;
        let changed = context("alice", "scope:v2");
        assert_eq!(
            engine.pull(&changed, &cursor, 100, &OwnerOnly, 2_000),
            Err(SyncError::ResnapshotRequired(
                ResnapshotReason::AuthorizationChanged
            ))
        );
        engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "one")]),
            &OwnerOnly,
            2_000,
        )?;
        let compacted = engine.compact(20_000);
        assert_eq!(compacted.removed_events, 1);
        assert_eq!(
            engine.pull(&alice, &cursor, 100, &OwnerOnly, 20_000),
            Err(SyncError::ResnapshotRequired(
                ResnapshotReason::CursorExpired
            ))
        );
        Ok(())
    }

    #[test]
    fn atomic_failure_rolls_back_earlier_mutations() -> Result<(), SyncError> {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "scope:v1");
        let result = engine.push(
            &alice,
            batch(vec![
                mutation("m-1", "alice", "allowed"),
                mutation("m-2", "bob", "denied"),
            ]),
            &OwnerOnly,
            2_000,
        );
        assert_eq!(result, Err(SyncError::RlsDenied));
        assert!(engine.snapshot(&alice, &OwnerOnly, 2_000)?.rows.is_empty());
        Ok(())
    }

    struct DenyAll;

    impl SyncAuthorizer for DenyAll {
        fn can_read(
            &self,
            _context: &SyncContext,
            _table: &str,
            _row: &Map<String, Value>,
        ) -> bool {
            false
        }

        fn can_write(
            &self,
            _context: &SyncContext,
            _table: &str,
            _operation: MutationOperation,
            _row: &Map<String, Value>,
        ) -> bool {
            false
        }
    }

    #[test]
    fn duplicate_is_reauthorized_and_client_identity_is_trusted() -> Result<(), SyncError> {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "scope:v1");
        let original = batch(vec![mutation("m-1", "alice", "one")]);
        engine.push(&alice, original.clone(), &OwnerOnly, 1_000)?;
        assert_eq!(
            engine.push(&alice, original, &DenyAll, 2_000),
            Err(SyncError::RlsDenied)
        );
        let mut forged = batch(vec![mutation("m-2", "alice", "two")]);
        forged.client_id = "unregistered-device".to_owned();
        assert_eq!(
            engine.push(&alice, forged, &OwnerOnly, 2_000),
            Err(SyncError::InvalidBatch)
        );
        Ok(())
    }

    #[test]
    fn cursor_debug_is_redacted_and_decoder_caps_input() -> Result<(), SyncError> {
        let engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "scope:v1");
        let cursor = engine.snapshot(&alice, &OwnerOnly, 1_000)?.cursor;
        let debug = format!("{cursor:?}");
        assert_eq!(debug, "OpaqueCursor([REDACTED])");
        assert!(!debug.contains(cursor.as_str()));
        let oversized = OpaqueCursor("a".repeat(10_000));
        assert_eq!(
            engine.pull(&alice, &oversized, 100, &OwnerOnly, 2_000),
            Err(SyncError::InvalidCursor)
        );
        Ok(())
    }

    #[test]
    fn compaction_prunes_idempotency_records() -> Result<(), SyncError> {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "scope:v1");
        engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "one")]),
            &OwnerOnly,
            1_000,
        )?;
        engine.compact(40_000);
        let replay = engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "one")]),
            &OwnerOnly,
            40_001,
        )?;
        assert_eq!(replay.accepted[0].outcome, MutationOutcome::Applied);
        Ok(())
    }

    #[test]
    fn checkpoint_restores_logical_state_without_persisting_cursor_secret() -> Result<(), SyncError>
    {
        let mut engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let alice = context("alice", "scope:v1");
        engine.push(
            &alice,
            batch(vec![mutation("m-1", "alice", "durable")]),
            &OwnerOnly,
            1_000,
        )?;
        let checkpoint = engine.checkpoint()?;
        assert!(!format!("{checkpoint:?}").contains("durable"));

        let restored = SyncEngine::restore("project-1", [9_u8; 32], config(), &checkpoint)?;
        let snapshot = restored.snapshot(&alice, &OwnerOnly, 2_000)?;
        assert_eq!(snapshot.rows[0].values["title"], "durable");
        assert_eq!(restored.schema_version(), 1);
        Ok(())
    }

    #[test]
    fn checkpoint_is_bound_to_project_and_validated() -> Result<(), SyncError> {
        let engine = SyncEngine::new("project-1", 1, [3_u8; 32], config())?;
        let checkpoint = engine.checkpoint()?;
        assert!(matches!(
            SyncEngine::restore("project-2", [3_u8; 32], config(), &checkpoint),
            Err(SyncError::InvalidCheckpoint)
        ));
        assert_eq!(
            SyncCheckpoint::from_bytes(Vec::new()),
            Err(SyncError::InvalidCheckpoint)
        );
        Ok(())
    }
}
