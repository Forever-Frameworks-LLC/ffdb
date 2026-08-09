//! Append-only, tamper-evident audit events.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ffdb_protocol::{OrganizationId, ProjectId, RequestId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

const REDACTED: &str = "[REDACTED]";
const MAX_METADATA_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Anonymous,
    User,
    ApiKey,
    Operator,
    Service,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Success,
    Denied,
    Failure,
}

#[derive(Clone, Debug)]
pub struct AuditDraft {
    pub occurred_at_ms: i64,
    pub organization_id: Option<OrganizationId>,
    pub project_id: Option<ProjectId>,
    pub request_id: RequestId,
    pub actor_kind: ActorKind,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub outcome: AuditOutcome,
    pub source_ip: Option<IpAddr>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub occurred_at_ms: i64,
    pub organization_id: Option<OrganizationId>,
    pub project_id: Option<ProjectId>,
    pub request_id: RequestId,
    pub actor_kind: ActorKind,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub outcome: AuditOutcome,
    pub source_ip: Option<IpAddr>,
    pub metadata: Value,
    pub previous_hash: Option<[u8; 32]>,
    pub event_hash: [u8; 32],
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum AuditError {
    #[error("audit event is invalid")]
    InvalidEvent,
    #[error("audit sink is unavailable")]
    Unavailable,
    #[error("audit stream integrity check failed")]
    Integrity,
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn append(&self, draft: AuditDraft) -> Result<AuditEvent, AuditError>;
}

#[derive(Debug, Default)]
struct AuditState {
    events: Vec<AuditEvent>,
    heads: HashMap<AuditStream, [u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AuditStream {
    Project(ProjectId),
    Organization(OrganizationId),
    Global,
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryAuditSink {
    state: Arc<Mutex<AuditState>>,
}

#[async_trait]
impl AuditSink for InMemoryAuditSink {
    async fn append(&self, draft: AuditDraft) -> Result<AuditEvent, AuditError> {
        let draft = validate_and_redact(draft)?;
        let stream = stream_for(&draft);
        let mut state = self.state.lock().await;
        let previous_hash = state.heads.get(&stream).copied();
        let event = build_event(draft, previous_hash)?;
        state.heads.insert(stream, event.event_hash);
        state.events.push(event.clone());
        Ok(event)
    }
}

impl InMemoryAuditSink {
    pub async fn events(&self) -> Vec<AuditEvent> {
        self.state.lock().await.events.clone()
    }

    pub async fn verify(&self) -> Result<(), AuditError> {
        let state = self.state.lock().await;
        let mut heads = HashMap::<AuditStream, [u8; 32]>::new();
        for event in &state.events {
            let stream = stream_for_event(event);
            if event.previous_hash != heads.get(&stream).copied()
                || compute_hash(event)? != event.event_hash
            {
                return Err(AuditError::Integrity);
            }
            heads.insert(stream, event.event_hash);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PgAuditSink {
    pool: PgPool,
}

impl PgAuditSink {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditSink for PgAuditSink {
    async fn append(&self, draft: AuditDraft) -> Result<AuditEvent, AuditError> {
        let draft = validate_and_redact(draft)?;
        let stream_key = stream_lock_key(&draft);
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AuditError::Unavailable)?;
        // Keep lock acquisition as a distinct statement. A data-modifying
        // append must not read the chain head until PostgreSQL confirms that it
        // owns this stream's transaction-scoped advisory lock.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,17))")
            .bind(&stream_key)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AuditError::Unavailable)?;
        let previous_hash: Option<Vec<u8>> = if let Some(project_id) = draft.project_id {
            sqlx::query_scalar(
                "SELECT event_hash FROM audit_events \
                 WHERE project_id=$1 ORDER BY append_sequence DESC LIMIT 1",
            )
            .bind(project_id.0)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| AuditError::Unavailable)?
        } else if let Some(organization_id) = draft.organization_id {
            sqlx::query_scalar(
                "SELECT event_hash FROM audit_events \
                 WHERE project_id IS NULL AND organization_id=$1 \
                 ORDER BY append_sequence DESC LIMIT 1",
            )
            .bind(organization_id.0)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| AuditError::Unavailable)?
        } else {
            sqlx::query_scalar(
                "SELECT event_hash FROM audit_events \
                 WHERE project_id IS NULL AND organization_id IS NULL \
                 ORDER BY append_sequence DESC LIMIT 1",
            )
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| AuditError::Unavailable)?
        };
        let previous_hash = previous_hash
            .map(|hash| hash.try_into().map_err(|_| AuditError::Integrity))
            .transpose()?;
        let event = build_event(draft, previous_hash)?;
        sqlx::query(
            "INSERT INTO audit_events \
             (id, occurred_at, organization_id, project_id, request_id, actor_kind, actor_id, action, \
              resource_kind, resource_id, outcome, source_ip, metadata, prev_hash, event_hash) \
             VALUES ($1, to_timestamp($2::double precision / 1000), $3, $4, $5, $6, $7, $8, $9, $10, $11, \
                     $12::inet, $13, $14, $15)",
        )
        .bind(event.id)
        .bind(event.occurred_at_ms)
        .bind(event.organization_id.map(|id| id.0))
        .bind(event.project_id.map(|id| id.0))
        .bind(event.request_id.to_string())
        .bind(actor_kind_name(event.actor_kind))
        .bind(event.actor_id)
        .bind(&event.action)
        .bind(&event.resource_kind)
        .bind(event.resource_id)
        .bind(outcome_name(event.outcome))
        .bind(event.source_ip.map(|ip| ip.to_string()))
        .bind(&event.metadata)
        .bind(event.previous_hash.map(|hash| hash.to_vec()))
        .bind(event.event_hash.to_vec())
        .execute(&mut *transaction)
        .await
        .map_err(|_| AuditError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| AuditError::Unavailable)?;
        Ok(event)
    }
}

fn validate_and_redact(mut draft: AuditDraft) -> Result<AuditDraft, AuditError> {
    if draft.occurred_at_ms < 0
        || !safe_identifier(&draft.action)
        || !safe_identifier(&draft.resource_kind)
        || matches!(draft.actor_kind, ActorKind::Anonymous) && draft.actor_id.is_some()
    {
        return Err(AuditError::InvalidEvent);
    }
    let mut nodes = 0_usize;
    redact_value(None, &mut draft.metadata, 0, &mut nodes)?;
    let encoded = serde_json::to_vec(&draft.metadata).map_err(|_| AuditError::InvalidEvent)?;
    if encoded.len() > MAX_METADATA_BYTES {
        return Err(AuditError::InvalidEvent);
    }
    Ok(draft)
}

fn redact_value(
    key: Option<&str>,
    value: &mut Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), AuditError> {
    *nodes = nodes.checked_add(1).ok_or(AuditError::InvalidEvent)?;
    if depth > 8 || *nodes > 512 {
        return Err(AuditError::InvalidEvent);
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
        Value::String(value) if value.len() > 2048 => return Err(AuditError::InvalidEvent),
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
        || normalized.contains("private_key")
        || normalized.contains("api_key")
}

fn safe_identifier(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn build_event(
    draft: AuditDraft,
    previous_hash: Option<[u8; 32]>,
) -> Result<AuditEvent, AuditError> {
    let mut event = AuditEvent {
        id: Uuid::now_v7(),
        occurred_at_ms: draft.occurred_at_ms,
        organization_id: draft.organization_id,
        project_id: draft.project_id,
        request_id: draft.request_id,
        actor_kind: draft.actor_kind,
        actor_id: draft.actor_id,
        action: draft.action,
        resource_kind: draft.resource_kind,
        resource_id: draft.resource_id,
        outcome: draft.outcome,
        source_ip: draft.source_ip,
        metadata: draft.metadata,
        previous_hash,
        event_hash: [0; 32],
    };
    event.event_hash = compute_hash(&event)?;
    Ok(event)
}

fn compute_hash(event: &AuditEvent) -> Result<[u8; 32], AuditError> {
    let canonical = serde_json::to_vec(&json!({
        "id": event.id,
        "occurred_at_ms": event.occurred_at_ms,
        "organization_id": event.organization_id,
        "project_id": event.project_id,
        "request_id": event.request_id,
        "actor_kind": event.actor_kind,
        "actor_id": event.actor_id,
        "action": event.action,
        "resource_kind": event.resource_kind,
        "resource_id": event.resource_id,
        "outcome": event.outcome,
        "source_ip": event.source_ip,
        "metadata": event.metadata,
        "previous_hash": event.previous_hash,
    }))
    .map_err(|_| AuditError::InvalidEvent)?;
    Ok(Sha256::digest(canonical).into())
}

fn stream_for(draft: &AuditDraft) -> AuditStream {
    draft
        .project_id
        .map(AuditStream::Project)
        .or_else(|| draft.organization_id.map(AuditStream::Organization))
        .unwrap_or(AuditStream::Global)
}

fn stream_for_event(event: &AuditEvent) -> AuditStream {
    event
        .project_id
        .map(AuditStream::Project)
        .or_else(|| event.organization_id.map(AuditStream::Organization))
        .unwrap_or(AuditStream::Global)
}

fn stream_lock_key(draft: &AuditDraft) -> String {
    match stream_for(draft) {
        AuditStream::Project(id) => format!("project:{id}"),
        AuditStream::Organization(id) => format!("organization:{id}"),
        AuditStream::Global => "global".into(),
    }
}

fn actor_kind_name(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Anonymous => "anonymous",
        ActorKind::User => "user",
        ActorKind::ApiKey => "api_key",
        ActorKind::Operator => "operator",
        ActorKind::Service => "service",
    }
}

fn outcome_name(outcome: AuditOutcome) -> &'static str {
    match outcome {
        AuditOutcome::Success => "success",
        AuditOutcome::Denied => "denied",
        AuditOutcome::Failure => "failure",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row as _;

    fn percentile_ms(samples: &mut [f64], percentile: f64) -> f64 {
        samples.sort_by(f64::total_cmp);
        let index = ((samples.len() - 1) as f64 * percentile).ceil() as usize;
        samples[index]
    }

    fn draft(project_id: ProjectId, metadata: Value) -> AuditDraft {
        AuditDraft {
            occurred_at_ms: 100,
            organization_id: None,
            project_id: Some(project_id),
            request_id: RequestId::new(),
            actor_kind: ActorKind::ApiKey,
            actor_id: Some(Uuid::now_v7()),
            action: "project.update".into(),
            resource_kind: "project".into(),
            resource_id: Some(project_id.0),
            outcome: AuditOutcome::Success,
            source_ip: Some(IpAddr::from([192, 0, 2, 1])),
            metadata,
        }
    }

    #[tokio::test]
    async fn redacts_secrets_and_chains_events() -> Result<(), AuditError> {
        let sink = InMemoryAuditSink::default();
        let project = ProjectId::new();
        let first = sink
            .append(draft(
                project,
                json!({
                    "safe": "visible",
                    "authorization": "Bearer credential",
                    "nested": {"refresh_token": "plaintext"}
                }),
            ))
            .await?;
        let second = sink.append(draft(project, json!({}))).await?;
        assert_eq!(first.metadata["authorization"], REDACTED);
        assert_eq!(first.metadata["nested"]["refresh_token"], REDACTED);
        assert_eq!(second.previous_hash, Some(first.event_hash));
        sink.verify().await?;
        Ok(())
    }

    #[tokio::test]
    async fn independent_projects_have_independent_chains() -> Result<(), AuditError> {
        let sink = InMemoryAuditSink::default();
        let first = sink.append(draft(ProjectId::new(), json!({}))).await?;
        let second = sink.append(draft(ProjectId::new(), json!({}))).await?;
        assert!(first.previous_hash.is_none());
        assert!(second.previous_hash.is_none());
        Ok(())
    }

    #[test]
    fn postgres_chain_head_uses_append_order_not_caller_timestamp() {
        let migration =
            include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql");
        assert!(migration.contains("append_sequence bigint GENERATED ALWAYS AS IDENTITY UNIQUE"));
        let source = include_str!("lib.rs");
        assert!(source.contains("ORDER BY append_sequence DESC LIMIT 1"));
        assert!(source.contains("SELECT pg_advisory_xact_lock"));
    }

    /// Profiles the same-stream hash-chain append that authenticated worker
    /// dispatches pay before and after execution. It creates and drops an
    /// isolated schema rather than writing into the real append-only ledger.
    #[ignore = "requires a local migrated PostgreSQL database and is not a CI capacity claim"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn postgres_append_latency_profile()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        const SEQUENTIAL: usize = 250;
        const CONCURRENT: usize = 400;
        const CONCURRENCY: usize = 8;
        let database_url = std::env::var("TEST_DATABASE_URL")?;
        let admin_pool = PgPool::connect(&database_url).await?;
        let schema = format!("audit_profile_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin_pool)
            .await?;
        sqlx::query(&format!(
            "CREATE TABLE {schema}.audit_events (\
                append_sequence bigint GENERATED ALWAYS AS IDENTITY UNIQUE,\
                id uuid PRIMARY KEY, occurred_at timestamptz NOT NULL,\
                organization_id uuid, project_id uuid, request_id text NOT NULL,\
                actor_kind text NOT NULL, actor_id uuid, action text NOT NULL,\
                resource_kind text NOT NULL, resource_id uuid, outcome text NOT NULL,\
                source_ip inet, metadata jsonb NOT NULL, prev_hash bytea, event_hash bytea NOT NULL\
            )"
        ))
        .execute(&admin_pool)
        .await?;
        sqlx::query(&format!(
            "CREATE INDEX audit_profile_org_append_idx ON {schema}.audit_events \
             (organization_id,append_sequence DESC) \
             WHERE project_id IS NULL AND organization_id IS NOT NULL"
        ))
        .execute(&admin_pool)
        .await?;
        let search_path = schema.clone();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(16)
            .after_connect(move |connection, _metadata| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path',$1,false)")
                        .bind(search_path)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await?;
        let sink = PgAuditSink::new(pool.clone());
        let organization_id = OrganizationId::new();
        let profile_draft = || AuditDraft {
            occurred_at_ms: 100,
            organization_id: Some(organization_id),
            project_id: None,
            request_id: RequestId::new(),
            actor_kind: ActorKind::Service,
            actor_id: None,
            action: "performance.profile".into(),
            resource_kind: "audit".into(),
            resource_id: None,
            outcome: AuditOutcome::Success,
            source_ip: None,
            metadata: json!({"profile": true}),
        };

        sink.append(profile_draft()).await?;
        let mut sequential = Vec::with_capacity(SEQUENTIAL);
        for _ in 0..SEQUENTIAL {
            let started = std::time::Instant::now();
            sink.append(profile_draft()).await?;
            sequential.push(started.elapsed().as_secs_f64() * 1_000.0);
        }
        let sequential_p50 = percentile_ms(&mut sequential, 0.50);
        let sequential_p95 = percentile_ms(&mut sequential, 0.95);
        let sequential_p99 = percentile_ms(&mut sequential, 0.99);

        let samples = Arc::new(Mutex::new(Vec::with_capacity(CONCURRENT)));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..CONCURRENCY {
            let sink = sink.clone();
            let samples = samples.clone();
            tasks.spawn(async move {
                for _ in 0..(CONCURRENT / CONCURRENCY) {
                    let started = std::time::Instant::now();
                    sink.append(AuditDraft {
                        occurred_at_ms: 100,
                        organization_id: Some(organization_id),
                        project_id: None,
                        request_id: RequestId::new(),
                        actor_kind: ActorKind::Service,
                        actor_id: None,
                        action: "performance.profile".into(),
                        resource_kind: "audit".into(),
                        resource_id: None,
                        outcome: AuditOutcome::Success,
                        source_ip: None,
                        metadata: json!({"profile": true}),
                    })
                    .await?;
                    samples
                        .lock()
                        .await
                        .push(started.elapsed().as_secs_f64() * 1_000.0);
                }
                Ok::<(), AuditError>(())
            });
        }
        while let Some(result) = tasks.join_next().await {
            result??;
        }
        let chain = sqlx::query(
            "SELECT count(*), \
                        count(*) FILTER (WHERE prev_hash IS NULL), \
                        count(DISTINCT prev_hash) FILTER (WHERE prev_hash IS NOT NULL), \
                        count(*) FILTER (\
                            WHERE prev_hash IS NOT NULL AND NOT EXISTS (\
                                SELECT 1 FROM audit_events parent \
                                WHERE parent.event_hash=audit_events.prev_hash\
                            )\
                        ) \
                 FROM audit_events",
        )
        .fetch_one(&pool)
        .await?;
        let event_count: i64 = chain.try_get(0)?;
        let root_count: i64 = chain.try_get(1)?;
        let distinct_links: i64 = chain.try_get(2)?;
        let missing_links: i64 = chain.try_get(3)?;
        assert_eq!(event_count, i64::try_from(1 + SEQUENTIAL + CONCURRENT)?);
        assert_eq!(root_count, 1, "the stream must have exactly one chain root");
        assert_eq!(
            distinct_links,
            event_count - 1,
            "every non-root event must reference a distinct predecessor"
        );
        assert_eq!(missing_links, 0, "every previous hash must exist in-stream");
        let mut concurrent = Arc::try_unwrap(samples)
            .map_err(|_| "profile sample collection still shared")?
            .into_inner();
        let concurrent_p50 = percentile_ms(&mut concurrent, 0.50);
        let concurrent_p95 = percentile_ms(&mut concurrent, 0.95);
        let concurrent_p99 = percentile_ms(&mut concurrent, 0.99);
        println!(
            "postgres audit append sequential: n={SEQUENTIAL} p50={sequential_p50:.3}ms p95={sequential_p95:.3}ms p99={sequential_p99:.3}ms"
        );
        println!(
            "postgres audit append same-project c={CONCURRENCY}: n={CONCURRENT} p50={concurrent_p50:.3}ms p95={concurrent_p95:.3}ms p99={concurrent_p99:.3}ms"
        );

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin_pool)
            .await?;
        Ok(())
    }
}
