use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use ffdb_protocol::{OrganizationId, ProjectId};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use uuid::Uuid;

const LEASE_SECONDS: i64 = 30;
const RETENTION_HOURS: i64 = 24;
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope {
    Project(ProjectId),
    Organization(OrganizationId),
}

#[derive(Clone, Debug)]
pub(crate) struct Claim {
    scope: Scope,
    operation: &'static str,
    key_hash: [u8; 32],
    owner_token: Uuid,
}

#[derive(Debug)]
pub(crate) enum Admission {
    Owner(Claim),
    Replay { status: StatusCode, body: Value },
    Conflict,
    InProgress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    InvalidKey,
    InvalidRequest,
    InvalidStoredResponse,
    ResponseTooLarge,
    Unavailable,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "idempotency error: {self:?}")
    }
}

impl std::error::Error for Error {}

pub(crate) fn request_hash(value: &Value) -> Result<[u8; 32], Error> {
    serde_json::to_vec(value)
        .map(|encoded| Sha256::digest(encoded).into())
        .map_err(|_| Error::InvalidRequest)
}

fn key_hash(key: &str) -> Result<[u8; 32], Error> {
    if !(8..=256).contains(&key.len())
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',' && byte != b';')
    {
        return Err(Error::InvalidKey);
    }
    Ok(Sha256::digest(key.as_bytes()).into())
}

pub(crate) async fn admit(
    pool: &PgPool,
    scope: Scope,
    operation: &'static str,
    key: &str,
    request_hash: [u8; 32],
) -> Result<Admission, Error> {
    if operation.is_empty() || operation.len() > 128 {
        return Err(Error::InvalidRequest);
    }
    let key_hash = key_hash(key)?;
    // Opportunistic, index-backed maintenance keeps request-driven retention
    // bounded even if the scheduled production maintenance task is delayed.
    purge_expired(pool, 64).await?;
    let owner_token = Uuid::now_v7();
    let mut transaction = pool.begin().await.map_err(|_| Error::Unavailable)?;
    insert_candidate(
        &mut transaction,
        scope,
        operation,
        &key_hash,
        &request_hash,
        owner_token,
    )
    .await?;

    let row = lock_row(&mut transaction, scope, operation, &key_hash).await?;
    let expired: bool = row.try_get("expired").map_err(|_| Error::Unavailable)?;
    let stored_request_hash: Vec<u8> = row
        .try_get("request_hash")
        .map_err(|_| Error::Unavailable)?;
    let stored_status: Option<i32> = row
        .try_get("response_status")
        .map_err(|_| Error::Unavailable)?;
    let stored_body: Option<Value> = row
        .try_get("response_body")
        .map_err(|_| Error::Unavailable)?;
    let stored_owner: Option<Uuid> = row.try_get("owner_token").map_err(|_| Error::Unavailable)?;
    let lease_expired: bool = row
        .try_get("lease_expired")
        .map_err(|_| Error::Unavailable)?;

    let admission = if stored_owner == Some(owner_token) {
        Admission::Owner(Claim {
            scope,
            operation,
            key_hash,
            owner_token,
        })
    } else if expired && (lease_expired || stored_owner.is_none()) {
        reset_row(
            &mut transaction,
            scope,
            operation,
            &key_hash,
            &request_hash,
            owner_token,
        )
        .await?;
        Admission::Owner(Claim {
            scope,
            operation,
            key_hash,
            owner_token,
        })
    } else if stored_request_hash.as_slice() != request_hash {
        Admission::Conflict
    } else if let (Some(status), Some(body)) = (stored_status, stored_body) {
        let status = u16::try_from(status)
            .ok()
            .and_then(|value| StatusCode::from_u16(value).ok())
            .filter(|status| !status.is_server_error())
            .ok_or(Error::InvalidStoredResponse)?;
        Admission::Replay { status, body }
    } else if lease_expired || stored_owner.is_none() {
        take_over_row(&mut transaction, scope, operation, &key_hash, owner_token).await?;
        Admission::Owner(Claim {
            scope,
            operation,
            key_hash,
            owner_token,
        })
    } else {
        Admission::InProgress
    };
    transaction.commit().await.map_err(|_| Error::Unavailable)?;
    Ok(admission)
}

pub(crate) async fn complete(
    pool: &PgPool,
    claim: &Claim,
    status: StatusCode,
    body: &Value,
) -> Result<(), Error> {
    if status.is_server_error()
        || matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    {
        return Err(Error::InvalidStoredResponse);
    }
    let encoded = serde_json::to_vec(body).map_err(|_| Error::InvalidStoredResponse)?;
    if encoded.len() > MAX_RESPONSE_BYTES {
        return Err(Error::ResponseTooLarge);
    }
    let result = match claim.scope {
        Scope::Project(project_id) => sqlx::query(
            "UPDATE idempotency_keys SET response_status=$5,response_body=$6,completed_at=now(),\
             owner_token=NULL,lease_expires_at=NULL WHERE project_id=$1 AND organization_id IS NULL \
             AND operation=$2 AND key_hash=$3 AND owner_token=$4 AND response_body IS NULL",
        )
        .bind(project_id.0)
        .bind(claim.operation)
        .bind(claim.key_hash.as_slice())
        .bind(claim.owner_token)
        .bind(i32::from(status.as_u16()))
        .bind(body)
        .execute(pool)
        .await,
        Scope::Organization(organization_id) => sqlx::query(
            "UPDATE idempotency_keys SET response_status=$5,response_body=$6,completed_at=now(),\
             owner_token=NULL,lease_expires_at=NULL WHERE organization_id=$1 AND project_id IS NULL \
             AND operation=$2 AND key_hash=$3 AND owner_token=$4 AND response_body IS NULL",
        )
        .bind(organization_id.0)
        .bind(claim.operation)
        .bind(claim.key_hash.as_slice())
        .bind(claim.owner_token)
        .bind(i32::from(status.as_u16()))
        .bind(body)
        .execute(pool)
        .await,
    }
    .map_err(|_| Error::Unavailable)?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(Error::Unavailable)
    }
}

pub(crate) async fn abandon(pool: &PgPool, claim: &Claim) -> Result<(), Error> {
    let result = match claim.scope {
        Scope::Project(project_id) => {
            sqlx::query(
                "DELETE FROM idempotency_keys WHERE project_id=$1 AND organization_id IS NULL \
             AND operation=$2 AND key_hash=$3 AND owner_token=$4 AND response_body IS NULL",
            )
            .bind(project_id.0)
            .bind(claim.operation)
            .bind(claim.key_hash.as_slice())
            .bind(claim.owner_token)
            .execute(pool)
            .await
        }
        Scope::Organization(organization_id) => {
            sqlx::query(
                "DELETE FROM idempotency_keys WHERE organization_id=$1 AND project_id IS NULL \
             AND operation=$2 AND key_hash=$3 AND owner_token=$4 AND response_body IS NULL",
            )
            .bind(organization_id.0)
            .bind(claim.operation)
            .bind(claim.key_hash.as_slice())
            .bind(claim.owner_token)
            .execute(pool)
            .await
        }
    };
    result.map(|_| ()).map_err(|_| Error::Unavailable)
}

/// Renew only the current owner's incomplete claim. Returning false means the
/// caller has lost ownership and must not publish a response for this claim.
pub(crate) async fn renew(pool: &PgPool, claim: &Claim) -> Result<bool, Error> {
    let result = match claim.scope {
        Scope::Project(project_id) => {
            sqlx::query(
                "UPDATE idempotency_keys SET lease_expires_at=now()+$5*interval '1 second' \
             WHERE project_id=$1 AND organization_id IS NULL AND operation=$2 \
             AND key_hash=$3 AND owner_token=$4 AND response_body IS NULL",
            )
            .bind(project_id.0)
            .bind(claim.operation)
            .bind(claim.key_hash.as_slice())
            .bind(claim.owner_token)
            .bind(LEASE_SECONDS as f64)
            .execute(pool)
            .await
        }
        Scope::Organization(organization_id) => {
            sqlx::query(
                "UPDATE idempotency_keys SET lease_expires_at=now()+$5*interval '1 second' \
             WHERE organization_id=$1 AND project_id IS NULL AND operation=$2 \
             AND key_hash=$3 AND owner_token=$4 AND response_body IS NULL",
            )
            .bind(organization_id.0)
            .bind(claim.operation)
            .bind(claim.key_hash.as_slice())
            .bind(claim.owner_token)
            .bind(LEASE_SECONDS as f64)
            .execute(pool)
            .await
        }
    }
    .map_err(|_| Error::Unavailable)?;
    Ok(result.rows_affected() == 1)
}

/// Bounded, lease-safe retention cleanup. An expired record with a live owner
/// is preserved; completed records and abandoned expired leases are eligible.
pub(crate) async fn purge_expired(pool: &PgPool, limit: u32) -> Result<u64, Error> {
    if limit == 0 || limit > 10_000 {
        return Err(Error::InvalidRequest);
    }
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE ctid IN (\
           SELECT ctid FROM idempotency_keys \
           WHERE expires_at<=now() \
             AND (owner_token IS NULL OR lease_expires_at IS NULL OR lease_expires_at<=now()) \
           ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED)",
    )
    .bind(i64::from(limit))
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
    .map_err(|_| Error::Unavailable)
}

/// Background renewal for requests which own an idempotency lease. Dropping the
/// guard stops renewal. Database outages are retried; a definitive owner mismatch
/// marks the guard unhealthy and terminates it.
#[derive(Debug)]
pub(crate) struct LeaseHeartbeat {
    task: Option<tokio::task::JoinHandle<()>>,
    healthy: Arc<AtomicBool>,
}

impl LeaseHeartbeat {
    pub(crate) fn start(pool: PgPool, claim: Claim) -> Self {
        let healthy = Arc::new(AtomicBool::new(true));
        let task_health = Arc::clone(&healthy);
        let task = tokio::spawn(async move {
            let period = Duration::from_secs((LEASE_SECONDS / 3).max(1) as u64);
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                match renew(&pool, &claim).await {
                    Ok(true) => task_health.store(true, Ordering::Release),
                    Ok(false) => {
                        task_health.store(false, Ordering::Release);
                        break;
                    }
                    Err(_) => {
                        task_health.store(false, Ordering::Release);
                        // Keep retrying: while PostgreSQL is unavailable no new
                        // claimant can take over, and recovery may renew before
                        // the lease is observed by another request.
                    }
                }
            }
        });
        Self {
            task: Some(task),
            healthy,
        }
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }
}

pub(crate) async fn confirm_owner(
    pool: &PgPool,
    claim: &Claim,
    heartbeat: &LeaseHeartbeat,
) -> bool {
    heartbeat.is_healthy() || renew(pool, claim).await == Ok(true)
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) fn deterministic_uuid(claim: &Claim) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.idempotency.resource.v1\0");
    match claim.scope {
        Scope::Project(id) => digest.update(id.0.as_bytes()),
        Scope::Organization(id) => digest.update(id.0.as_bytes()),
    }
    digest.update(claim.operation.as_bytes());
    digest.update(claim.key_hash);
    let hash = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

pub(crate) fn receipt_uuid(claim: &Claim) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"ffdb.idempotency.worker-receipt.v1\0");
    match claim.scope {
        Scope::Project(id) => digest.update(id.0.as_bytes()),
        Scope::Organization(id) => digest.update(id.0.as_bytes()),
    }
    digest.update(claim.operation.as_bytes());
    digest.update(claim.key_hash);
    let hash = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

async fn insert_candidate(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    operation: &str,
    key_hash: &[u8; 32],
    request_hash: &[u8; 32],
    owner_token: Uuid,
) -> Result<(), Error> {
    let result = match scope {
        Scope::Project(project_id) => sqlx::query(
            "INSERT INTO idempotency_keys \
             (project_id,organization_id,operation,key_hash,request_hash,owner_token,lease_expires_at,expires_at) \
             VALUES ($1,NULL,$2,$3,$4,$5,now()+$6*interval '1 second',now()+$7*interval '1 hour') \
             ON CONFLICT (project_id,operation,key_hash) WHERE project_id IS NOT NULL DO NOTHING",
        )
        .bind(project_id.0)
        .bind(operation)
        .bind(key_hash.as_slice())
        .bind(request_hash.as_slice())
        .bind(owner_token)
        .bind(LEASE_SECONDS as f64)
        .bind(RETENTION_HOURS as f64)
        .execute(&mut **transaction)
        .await,
        Scope::Organization(organization_id) => sqlx::query(
            "INSERT INTO idempotency_keys \
             (project_id,organization_id,operation,key_hash,request_hash,owner_token,lease_expires_at,expires_at) \
             VALUES (NULL,$1,$2,$3,$4,$5,now()+$6*interval '1 second',now()+$7*interval '1 hour') \
             ON CONFLICT (organization_id,operation,key_hash) WHERE organization_id IS NOT NULL DO NOTHING",
        )
        .bind(organization_id.0)
        .bind(operation)
        .bind(key_hash.as_slice())
        .bind(request_hash.as_slice())
        .bind(owner_token)
        .bind(LEASE_SECONDS as f64)
        .bind(RETENTION_HOURS as f64)
        .execute(&mut **transaction)
        .await,
    };
    result.map(|_| ()).map_err(|_| Error::Unavailable)
}

async fn lock_row(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    operation: &str,
    key_hash: &[u8; 32],
) -> Result<sqlx::postgres::PgRow, Error> {
    let result = match scope {
        Scope::Project(project_id) => sqlx::query(
            "SELECT request_hash,response_status,response_body,owner_token,expires_at <= now() AS expired,\
             COALESCE(lease_expires_at <= now(),true) AS lease_expired FROM idempotency_keys \
             WHERE project_id=$1 AND organization_id IS NULL AND operation=$2 AND key_hash=$3 FOR UPDATE",
        )
        .bind(project_id.0)
        .bind(operation)
        .bind(key_hash.as_slice())
        .fetch_one(&mut **transaction)
        .await,
        Scope::Organization(organization_id) => sqlx::query(
            "SELECT request_hash,response_status,response_body,owner_token,expires_at <= now() AS expired,\
             COALESCE(lease_expires_at <= now(),true) AS lease_expired FROM idempotency_keys \
             WHERE organization_id=$1 AND project_id IS NULL AND operation=$2 AND key_hash=$3 FOR UPDATE",
        )
        .bind(organization_id.0)
        .bind(operation)
        .bind(key_hash.as_slice())
        .fetch_one(&mut **transaction)
        .await,
    };
    result.map_err(|_| Error::Unavailable)
}

async fn reset_row(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    operation: &str,
    key_hash: &[u8; 32],
    request_hash: &[u8; 32],
    owner_token: Uuid,
) -> Result<(), Error> {
    update_owner(
        transaction,
        scope,
        operation,
        key_hash,
        Some(request_hash),
        owner_token,
    )
    .await
}

async fn take_over_row(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    operation: &str,
    key_hash: &[u8; 32],
    owner_token: Uuid,
) -> Result<(), Error> {
    update_owner(transaction, scope, operation, key_hash, None, owner_token).await
}

async fn update_owner(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    operation: &str,
    key_hash: &[u8; 32],
    request_hash: Option<&[u8; 32]>,
    owner_token: Uuid,
) -> Result<(), Error> {
    let result = match (scope, request_hash) {
        (Scope::Project(id), Some(request_hash)) => sqlx::query(
            "UPDATE idempotency_keys SET request_hash=$4,response_status=NULL,response_body=NULL,completed_at=NULL,\
             owner_token=$5,lease_expires_at=now()+$6*interval '1 second',\
             created_at=now(),expires_at=now()+$7*interval '1 hour' \
             WHERE project_id=$1 AND organization_id IS NULL AND operation=$2 AND key_hash=$3",
        )
        .bind(id.0).bind(operation).bind(key_hash.as_slice()).bind(request_hash.as_slice())
        .bind(owner_token).bind(LEASE_SECONDS as f64).bind(RETENTION_HOURS as f64)
        .execute(&mut **transaction).await,
        (Scope::Organization(id), Some(request_hash)) => sqlx::query(
            "UPDATE idempotency_keys SET request_hash=$4,response_status=NULL,response_body=NULL,completed_at=NULL,\
             owner_token=$5,lease_expires_at=now()+$6*interval '1 second',\
             created_at=now(),expires_at=now()+$7*interval '1 hour' \
             WHERE organization_id=$1 AND project_id IS NULL AND operation=$2 AND key_hash=$3",
        )
        .bind(id.0).bind(operation).bind(key_hash.as_slice()).bind(request_hash.as_slice())
        .bind(owner_token).bind(LEASE_SECONDS as f64).bind(RETENTION_HOURS as f64)
        .execute(&mut **transaction).await,
        (Scope::Project(id), None) => sqlx::query(
            "UPDATE idempotency_keys SET owner_token=$4,lease_expires_at=now()+$5*interval '1 second' \
             WHERE project_id=$1 AND organization_id IS NULL AND operation=$2 AND key_hash=$3",
        )
        .bind(id.0).bind(operation).bind(key_hash.as_slice()).bind(owner_token)
        .bind(LEASE_SECONDS as f64).execute(&mut **transaction).await,
        (Scope::Organization(id), None) => sqlx::query(
            "UPDATE idempotency_keys SET owner_token=$4,lease_expires_at=now()+$5*interval '1 second' \
             WHERE organization_id=$1 AND project_id IS NULL AND operation=$2 AND key_hash=$3",
        )
        .bind(id.0).bind(operation).bind(key_hash.as_slice()).bind(owner_token)
        .bind(LEASE_SECONDS as f64).execute(&mut **transaction).await,
    };
    result.map(|_| ()).map_err(|_| Error::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_keys_are_strictly_bounded() {
        assert!(key_hash("retry-key-123").is_ok());
        assert_eq!(key_hash("short"), Err(Error::InvalidKey));
        assert_eq!(key_hash("bad,key-123"), Err(Error::InvalidKey));
        assert_eq!(key_hash(&"x".repeat(257)), Err(Error::InvalidKey));
    }

    #[test]
    fn deterministic_resources_are_stable_and_key_specific() {
        let make = |byte| Claim {
            scope: Scope::Project(ProjectId(Uuid::nil())),
            operation: "backup.create",
            key_hash: [byte; 32],
            owner_token: Uuid::now_v7(),
        };
        assert_eq!(deterministic_uuid(&make(1)), deterministic_uuid(&make(1)));
        assert_ne!(deterministic_uuid(&make(1)), deterministic_uuid(&make(2)));
        assert_ne!(deterministic_uuid(&make(1)), receipt_uuid(&make(1)));
    }

    #[tokio::test]
    async fn postgres_claim_conflict_replay_and_failure_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        crate::control_plane_migrations::migrator()
            .run(&pool)
            .await?;
        let organization_id = OrganizationId::new();
        sqlx::query("INSERT INTO organizations (id,slug,display_name) VALUES ($1,$2,$3)")
            .bind(organization_id.0)
            .bind(format!(
                "idempotency-{}",
                &organization_id.to_string()[..12]
            ))
            .bind("Idempotency test")
            .execute(&pool)
            .await?;
        let scope = Scope::Organization(organization_id);
        let first_hash = request_hash(&serde_json::json!({"value": 1}))?;
        let second_hash = request_hash(&serde_json::json!({"value": 2}))?;
        let claim = match admit(&pool, scope, "test.create", "test-key-123", first_hash).await? {
            Admission::Owner(claim) => claim,
            other => return Err(format!("unexpected admission: {other:?}").into()),
        };
        assert!(matches!(
            admit(&pool, scope, "test.create", "test-key-123", second_hash).await?,
            Admission::Conflict
        ));
        assert!(matches!(
            admit(&pool, scope, "test.create", "test-key-123", first_hash).await?,
            Admission::InProgress
        ));
        let body = serde_json::json!({"created": true});
        complete(&pool, &claim, StatusCode::CREATED, &body).await?;
        match admit(&pool, scope, "test.create", "test-key-123", first_hash).await? {
            Admission::Replay {
                status,
                body: replay,
            } => {
                assert_eq!(status, StatusCode::CREATED);
                assert_eq!(replay, body);
            }
            other => return Err(format!("unexpected admission: {other:?}").into()),
        }
        let retry = match admit(&pool, scope, "test.retry", "retry-key-123", first_hash).await? {
            Admission::Owner(claim) => claim,
            other => return Err(format!("unexpected admission: {other:?}").into()),
        };
        abandon(&pool, &retry).await?;
        assert!(matches!(
            admit(&pool, scope, "test.retry", "retry-key-123", first_hash).await?,
            Admission::Owner(_)
        ));
        let live = match admit(
            &pool,
            scope,
            "test.lease-safe-purge",
            "live-lease-key-123",
            first_hash,
        )
        .await?
        {
            Admission::Owner(claim) => claim,
            other => return Err(format!("unexpected admission: {other:?}").into()),
        };
        sqlx::query(
            "UPDATE idempotency_keys SET expires_at=now()-interval '1 second' \
             WHERE organization_id=$1 AND operation=$2 AND key_hash=$3",
        )
        .bind(organization_id.0)
        .bind(live.operation)
        .bind(live.key_hash.as_slice())
        .execute(&pool)
        .await?;
        assert_eq!(purge_expired(&pool, 64).await?, 0);
        assert!(matches!(
            admit(
                &pool,
                scope,
                "test.lease-safe-purge",
                "live-lease-key-123",
                first_hash,
            )
            .await?,
            Admission::InProgress
        ));
        assert!(renew(&pool, &live).await?);
        complete(
            &pool,
            &live,
            StatusCode::OK,
            &serde_json::json!({"completed": true}),
        )
        .await?;
        assert_eq!(purge_expired(&pool, 64).await?, 1);
        sqlx::query("DELETE FROM idempotency_keys WHERE organization_id=$1")
            .bind(organization_id.0)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM organizations WHERE id=$1")
            .bind(organization_id.0)
            .execute(&pool)
            .await?;
        Ok(())
    }
}
