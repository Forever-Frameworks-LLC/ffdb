//! Deterministic, bounded rate and quota enforcement.
//!
//! Callers should enforce independent keys for source IP, project, user, and API
//! key. Identifiers are HMACed before storage so limiter snapshots do not contain
//! raw IP addresses or user identifiers.

use std::collections::HashMap;

use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{PgPool, Row};
use sqlx::{Postgres, Transaction};
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RateDimension {
    Ip,
    /// Project-scoped authentication admission (registration, sign-in,
    /// verification, refresh, and recovery), intentionally separate from the
    /// higher-throughput authenticated execution bucket.
    AuthProject,
    /// User-scoped authentication admission.
    AuthUser,
    /// API-key authentication lifecycle admission.
    AuthApiKey,
    Project,
    User,
    ApiKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RateLimitKey {
    pub dimension: RateDimension,
    digest: [u8; 32],
}

impl RateLimitKey {
    pub fn derive(
        dimension: RateDimension,
        namespace_secret: &[u8],
        identifier: &[u8],
    ) -> Result<Self, RateLimitError> {
        if namespace_secret.len() < 32 || identifier.is_empty() || identifier.len() > 1024 {
            return Err(RateLimitError::InvalidConfiguration);
        }
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(namespace_secret)
            .map_err(|_| RateLimitError::InvalidConfiguration)?;
        mac.update(dimension_name(dimension).as_bytes());
        mac.update(&[0]);
        mac.update(identifier);
        Ok(Self {
            dimension,
            digest: mac.finalize().into_bytes().into(),
        })
    }

    #[must_use]
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TokenBucketConfig {
    pub capacity: u32,
    pub refill_tokens_per_second: f64,
    pub idle_ttl_ms: i64,
    pub max_entries: usize,
}

impl TokenBucketConfig {
    pub fn validate(self) -> Result<Self, RateLimitError> {
        if self.capacity == 0
            || !self.refill_tokens_per_second.is_finite()
            || self.refill_tokens_per_second <= 0.0
            || self.idle_ttl_ms <= 0
            || self.max_entries == 0
            || i64::try_from(self.max_entries).is_err()
        {
            return Err(RateLimitError::InvalidConfiguration);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateLimitDecision {
    Allowed { remaining: f64 },
    Denied { retry_after_ms: u64 },
}

#[derive(Clone, Copy, Debug)]
struct BucketState {
    tokens: f64,
    last_refill_ms: i64,
    last_seen_ms: i64,
}

#[derive(Debug)]
pub struct TokenBucketLimiter {
    config: TokenBucketConfig,
    buckets: Mutex<HashMap<RateLimitKey, BucketState>>,
}

impl TokenBucketLimiter {
    pub fn new(config: TokenBucketConfig) -> Result<Self, RateLimitError> {
        Ok(Self {
            config: config.validate()?,
            buckets: Mutex::new(HashMap::new()),
        })
    }

    pub fn check(
        &self,
        key: RateLimitKey,
        cost: u32,
        now_ms: i64,
    ) -> Result<RateLimitDecision, RateLimitError> {
        if cost == 0 || cost > self.config.capacity {
            return Err(RateLimitError::InvalidCost);
        }
        let mut buckets = self.buckets.lock();
        if !buckets.contains_key(&key) && buckets.len() >= self.config.max_entries {
            let idle_ttl_ms = self.config.idle_ttl_ms;
            buckets.retain(|_, bucket| now_ms.saturating_sub(bucket.last_seen_ms) <= idle_ttl_ms);
            if buckets.len() >= self.config.max_entries {
                // A full limiter cannot silently skip enforcement.
                return Err(RateLimitError::Saturated);
            }
        }
        let bucket = buckets.entry(key).or_insert(BucketState {
            tokens: f64::from(self.config.capacity),
            last_refill_ms: now_ms,
            last_seen_ms: now_ms,
        });
        // Wall-clock rollback never mints tokens.
        let elapsed_ms = now_ms.saturating_sub(bucket.last_refill_ms).max(0);
        let refill = (elapsed_ms as f64 / 1000.0) * self.config.refill_tokens_per_second;
        bucket.tokens = (bucket.tokens + refill).min(f64::from(self.config.capacity));
        bucket.last_refill_ms = bucket.last_refill_ms.max(now_ms);
        bucket.last_seen_ms = bucket.last_seen_ms.max(now_ms);

        let cost = f64::from(cost);
        if bucket.tokens >= cost {
            bucket.tokens -= cost;
            return Ok(RateLimitDecision::Allowed {
                remaining: bucket.tokens,
            });
        }
        let missing = cost - bucket.tokens;
        let retry_ms = ((missing / self.config.refill_tokens_per_second) * 1000.0).ceil();
        let retry_after_ms = if retry_ms.is_finite() && retry_ms >= 1.0 {
            retry_ms.min(u64::MAX as f64) as u64
        } else {
            1
        };
        Ok(RateLimitDecision::Denied { retry_after_ms })
    }

    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        self.buckets.lock().len()
    }
}

/// PostgreSQL-backed token bucket for consistent enforcement across API
/// replicas. Each derived key is serialized with an advisory transaction lock;
/// raw identifiers are never persisted.
#[derive(Clone, Debug)]
pub struct PgTokenBucketLimiter {
    pool: PgPool,
    config: TokenBucketConfig,
}

impl PgTokenBucketLimiter {
    pub fn new(pool: PgPool, config: TokenBucketConfig) -> Result<Self, RateLimitError> {
        Ok(Self {
            pool,
            config: config.validate()?,
        })
    }

    pub async fn check(
        &self,
        key: RateLimitKey,
        cost: u32,
        now_ms: i64,
    ) -> Result<RateLimitDecision, RateLimitError> {
        self.check_many(&[(key, cost)], now_ms)
            .await?
            .into_iter()
            .next()
            .ok_or(RateLimitError::Unavailable)
    }

    /// Check an ordered set of independent dimensions in one PostgreSQL
    /// transaction. Evaluation short-circuits after the first denial, matching
    /// sequential enforcement while avoiding a BEGIN/COMMIT pair per dimension.
    pub async fn check_many(
        &self,
        checks: &[(RateLimitKey, u32)],
        now_ms: i64,
    ) -> Result<Vec<RateLimitDecision>, RateLimitError> {
        if checks.is_empty()
            || checks.len() > 16
            || now_ms < 0
            || checks
                .iter()
                .any(|(_, cost)| *cost == 0 || *cost > self.config.capacity)
        {
            return Err(RateLimitError::InvalidCost);
        }

        let mut lock_digests = checks
            .iter()
            .map(|(key, _)| key.digest.to_vec())
            .collect::<Vec<_>>();
        lock_digests.sort_unstable();
        lock_digests.dedup();
        if lock_digests.len() != checks.len() {
            return Err(RateLimitError::InvalidCost);
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| RateLimitError::Unavailable)?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(encode(bucket_key,'hex'),23)) \
             FROM (SELECT unnest($1::bytea[]) AS bucket_key ORDER BY bucket_key) ordered",
        )
        .bind(&lock_digests)
        .execute(&mut *transaction)
        .await
        .map_err(|_| RateLimitError::Unavailable)?;

        let rows = sqlx::query(
            "SELECT bucket_key,dimension,tokens, \
                    (extract(epoch FROM last_refill_at)*1000)::bigint last_refill_ms \
             FROM rate_limit_state WHERE bucket_key=ANY($1) FOR UPDATE",
        )
        .bind(&lock_digests)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| RateLimitError::Unavailable)?;
        let mut states = HashMap::<Vec<u8>, (String, f64, i64)>::with_capacity(rows.len());
        for row in rows {
            let digest: Vec<u8> = row
                .try_get("bucket_key")
                .map_err(|_| RateLimitError::Unavailable)?;
            let state = (
                row.try_get("dimension")
                    .map_err(|_| RateLimitError::Unavailable)?,
                row.try_get("tokens")
                    .map_err(|_| RateLimitError::Unavailable)?,
                row.try_get("last_refill_ms")
                    .map_err(|_| RateLimitError::Unavailable)?,
            );
            states.insert(digest, state);
        }

        let mut decisions = Vec::with_capacity(checks.len());
        let mut persisted_digests = Vec::with_capacity(checks.len());
        let mut persisted_dimensions = Vec::with_capacity(checks.len());
        let mut persisted_tokens = Vec::with_capacity(checks.len());
        let mut persisted_refills = Vec::with_capacity(checks.len());
        let mut persisted_expiries = Vec::with_capacity(checks.len());
        let mut new_entries = 0_i64;

        for (key, raw_cost) in checks {
            let (mut tokens, last_refill_ms) = if let Some((dimension, tokens, last_refill_ms)) =
                states.get(key.digest.as_slice())
            {
                if dimension != dimension_name(key.dimension) {
                    return Err(RateLimitError::Unavailable);
                }
                (*tokens, *last_refill_ms)
            } else {
                new_entries += 1;
                (f64::from(self.config.capacity), now_ms)
            };
            if !tokens.is_finite() || tokens < 0.0 {
                return Err(RateLimitError::Unavailable);
            }

            let elapsed_ms = now_ms.saturating_sub(last_refill_ms).max(0);
            let refill = (elapsed_ms as f64 / 1_000.0) * self.config.refill_tokens_per_second;
            tokens = (tokens + refill).min(f64::from(self.config.capacity));
            let cost = f64::from(*raw_cost);
            let decision = if tokens >= cost {
                tokens -= cost;
                RateLimitDecision::Allowed { remaining: tokens }
            } else {
                let retry_ms =
                    (((cost - tokens) / self.config.refill_tokens_per_second) * 1_000.0).ceil();
                let retry_after_ms = if retry_ms.is_finite() && retry_ms >= 1.0 {
                    retry_ms.min(u64::MAX as f64) as u64
                } else {
                    1
                };
                RateLimitDecision::Denied { retry_after_ms }
            };

            let persisted_refill_ms = last_refill_ms.max(now_ms);
            let expires_at_ms = persisted_refill_ms
                .checked_add(self.config.idle_ttl_ms)
                .ok_or(RateLimitError::InvalidCost)?;
            persisted_digests.push(key.digest.to_vec());
            persisted_dimensions.push(dimension_name(key.dimension).to_owned());
            persisted_tokens.push(tokens);
            persisted_refills.push(persisted_refill_ms);
            persisted_expiries.push(expires_at_ms);
            decisions.push(decision);
            if matches!(decision, RateLimitDecision::Denied { .. }) {
                break;
            }
        }

        if new_entries > 0 {
            cleanup_expired_in_transaction(&mut transaction, now_ms, 256).await?;
            let entries: i64 = sqlx::query_scalar(
                "SELECT entry_count FROM rate_limit_capacity WHERE singleton=true FOR UPDATE",
            )
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| RateLimitError::Unavailable)?;
            let max_entries = i64::try_from(self.config.max_entries)
                .map_err(|_| RateLimitError::InvalidConfiguration)?;
            if entries.saturating_add(new_entries) > max_entries {
                return Err(RateLimitError::Saturated);
            }
        }

        sqlx::query(
            "INSERT INTO rate_limit_state \
             (bucket_key,dimension,tokens,last_refill_at,expires_at) \
             SELECT bucket_key,dimension,tokens, \
                    to_timestamp(last_refill_ms::double precision/1000), \
                    to_timestamp(expires_at_ms::double precision/1000) \
             FROM unnest($1::bytea[],$2::text[],$3::float8[],$4::bigint[],$5::bigint[]) \
                  AS batch(bucket_key,dimension,tokens,last_refill_ms,expires_at_ms) \
             ON CONFLICT (bucket_key) DO UPDATE SET tokens=EXCLUDED.tokens, \
                 last_refill_at=EXCLUDED.last_refill_at,expires_at=EXCLUDED.expires_at",
        )
        .bind(&persisted_digests)
        .bind(&persisted_dimensions)
        .bind(&persisted_tokens)
        .bind(&persisted_refills)
        .bind(&persisted_expiries)
        .execute(&mut *transaction)
        .await
        .map_err(|_| RateLimitError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RateLimitError::Unavailable)?;
        Ok(decisions)
    }

    /// Bounded cleanup suitable for a recurring maintenance job.
    pub async fn cleanup_expired(&self, now_ms: i64, limit: u32) -> Result<u64, RateLimitError> {
        if now_ms < 0 || limit == 0 || limit > 10_000 {
            return Err(RateLimitError::InvalidCost);
        }
        let result = sqlx::query(
            "DELETE FROM rate_limit_state WHERE ctid IN \
             (SELECT ctid FROM rate_limit_state \
              WHERE expires_at<=to_timestamp($1::double precision/1000) \
              ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
        )
        .bind(now_ms)
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await
        .map_err(|_| RateLimitError::Unavailable)?;
        Ok(result.rows_affected())
    }
}

async fn cleanup_expired_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    now_ms: i64,
    limit: u32,
) -> Result<u64, RateLimitError> {
    let result = sqlx::query(
        "DELETE FROM rate_limit_state WHERE ctid IN \
         (SELECT ctid FROM rate_limit_state \
          WHERE expires_at<=to_timestamp($1::double precision/1000) \
          ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
    )
    .bind(now_ms)
    .bind(i64::from(limit))
    .execute(&mut **transaction)
    .await
    .map_err(|_| RateLimitError::Unavailable)?;
    Ok(result.rows_affected())
}

#[derive(Clone, Copy, Debug)]
pub struct QuotaConfig {
    pub limit: u64,
    pub window_ms: i64,
    pub max_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaDecision {
    Allowed { remaining: u64, reset_at_ms: i64 },
    Denied { reset_at_ms: i64 },
}

#[derive(Clone, Copy, Debug)]
struct QuotaState {
    window_start_ms: i64,
    consumed: u64,
}

#[derive(Debug)]
pub struct FixedWindowQuota {
    config: QuotaConfig,
    entries: Mutex<HashMap<RateLimitKey, QuotaState>>,
}

impl FixedWindowQuota {
    pub fn new(config: QuotaConfig) -> Result<Self, RateLimitError> {
        if config.limit == 0 || config.window_ms <= 0 || config.max_entries == 0 {
            return Err(RateLimitError::InvalidConfiguration);
        }
        Ok(Self {
            config,
            entries: Mutex::new(HashMap::new()),
        })
    }

    pub fn consume(
        &self,
        key: RateLimitKey,
        units: u64,
        now_ms: i64,
    ) -> Result<QuotaDecision, RateLimitError> {
        if units == 0 || units > self.config.limit || now_ms < 0 {
            return Err(RateLimitError::InvalidCost);
        }
        let window_start_ms = now_ms - (now_ms % self.config.window_ms);
        let reset_at_ms = window_start_ms
            .checked_add(self.config.window_ms)
            .ok_or(RateLimitError::InvalidCost)?;
        let mut entries = self.entries.lock();
        if !entries.contains_key(&key) && entries.len() >= self.config.max_entries {
            entries.retain(|_, entry| entry.window_start_ms >= window_start_ms);
            if entries.len() >= self.config.max_entries {
                return Err(RateLimitError::Saturated);
            }
        }
        let entry = entries.entry(key).or_insert(QuotaState {
            window_start_ms,
            consumed: 0,
        });
        if entry.window_start_ms != window_start_ms {
            entry.window_start_ms = window_start_ms;
            entry.consumed = 0;
        }
        let Some(next) = entry.consumed.checked_add(units) else {
            return Err(RateLimitError::InvalidCost);
        };
        if next > self.config.limit {
            return Ok(QuotaDecision::Denied { reset_at_ms });
        }
        entry.consumed = next;
        Ok(QuotaDecision::Allowed {
            remaining: self.config.limit - next,
            reset_at_ms,
        })
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum RateLimitError {
    #[error("rate limiter configuration is invalid")]
    InvalidConfiguration,
    #[error("rate limiter cost is invalid")]
    InvalidCost,
    #[error("rate limiter is saturated")]
    Saturated,
    #[error("rate limiter datastore is unavailable")]
    Unavailable,
}

fn dimension_name(dimension: RateDimension) -> &'static str {
    match dimension {
        RateDimension::Ip => "ip",
        RateDimension::AuthProject => "auth_project",
        RateDimension::AuthUser => "auth_user",
        RateDimension::AuthApiKey => "auth_api_key",
        RateDimension::Project => "project",
        RateDimension::User => "user",
        RateDimension::ApiKey => "api_key",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn percentile_ms(samples: &mut [f64], percentile: f64) -> f64 {
        samples.sort_by(f64::total_cmp);
        let index = ((samples.len() - 1) as f64 * percentile).ceil() as usize;
        samples[index]
    }

    fn key(dimension: RateDimension, value: &str) -> Result<RateLimitKey, RateLimitError> {
        RateLimitKey::derive(dimension, &[11; 32], value.as_bytes())
    }

    #[test]
    fn token_bucket_is_deterministic_and_dimension_scoped() -> Result<(), RateLimitError> {
        let limiter = TokenBucketLimiter::new(TokenBucketConfig {
            capacity: 2,
            refill_tokens_per_second: 1.0,
            idle_ttl_ms: 60_000,
            max_entries: 10,
        })?;
        let ip = key(RateDimension::Ip, "192.0.2.1")?;
        assert!(matches!(
            limiter.check(ip, 1, 1_000)?,
            RateLimitDecision::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check(ip, 1, 1_000)?,
            RateLimitDecision::Allowed { .. }
        ));
        assert_eq!(
            limiter.check(ip, 1, 1_000)?,
            RateLimitDecision::Denied {
                retry_after_ms: 1_000
            }
        );
        assert!(matches!(
            limiter.check(ip, 1, 2_000)?,
            RateLimitDecision::Allowed { .. }
        ));
        let project = key(RateDimension::Project, "192.0.2.1")?;
        assert!(matches!(
            limiter.check(project, 2, 2_000)?,
            RateLimitDecision::Allowed { .. }
        ));
        Ok(())
    }

    #[test]
    fn authentication_and_execution_dimensions_have_distinct_namespaces()
    -> Result<(), RateLimitError> {
        let secret = [11; 32];
        let identifier = b"same-project-or-actor";
        for (auth, execution) in [
            (RateDimension::AuthProject, RateDimension::Project),
            (RateDimension::AuthUser, RateDimension::User),
            (RateDimension::AuthApiKey, RateDimension::ApiKey),
        ] {
            let auth_key = RateLimitKey::derive(auth, &secret, identifier)?;
            let execution_key = RateLimitKey::derive(execution, &secret, identifier)?;
            assert_ne!(auth_key.digest(), execution_key.digest());
        }
        Ok(())
    }

    #[test]
    fn clock_rollback_does_not_refill() -> Result<(), RateLimitError> {
        let limiter = TokenBucketLimiter::new(TokenBucketConfig {
            capacity: 1,
            refill_tokens_per_second: 1.0,
            idle_ttl_ms: 60_000,
            max_entries: 10,
        })?;
        let key = key(RateDimension::User, "user")?;
        assert!(matches!(
            limiter.check(key, 1, 5_000)?,
            RateLimitDecision::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check(key, 1, 4_000)?,
            RateLimitDecision::Denied { .. }
        ));
        Ok(())
    }

    #[test]
    fn quota_fails_closed_at_limit() -> Result<(), RateLimitError> {
        let quota = FixedWindowQuota::new(QuotaConfig {
            limit: 5,
            window_ms: 1000,
            max_entries: 8,
        })?;
        let key = key(RateDimension::Project, "project")?;
        assert!(matches!(
            quota.consume(key, 5, 100)?,
            QuotaDecision::Allowed { remaining: 0, .. }
        ));
        assert!(matches!(
            quota.consume(key, 1, 101)?,
            QuotaDecision::Denied { .. }
        ));
        assert!(matches!(
            quota.consume(key, 1, 1000)?,
            QuotaDecision::Allowed { remaining: 4, .. }
        ));
        Ok(())
    }

    #[test]
    fn migration_has_durable_hashed_rate_state() {
        let sql = include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql");
        let bounded =
            include_str!("../../../infra/postgres/migrations/0005_bounded_security_state.up.sql");
        assert!(sql.contains("CREATE TABLE rate_limit_state"));
        assert!(sql.contains("bucket_key bytea PRIMARY KEY"));
        assert!(bounded.contains("CREATE TABLE rate_limit_capacity"));
        assert!(bounded.contains("rate_limit_state_expiry_idx"));
        assert!(bounded.contains("AFTER INSERT ON rate_limit_state"));
        assert!(bounded.contains("AFTER DELETE ON rate_limit_state"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_capacity_is_exact_under_concurrent_new_identifiers_and_recovers_by_cleanup()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        let schema_ready: bool =
            sqlx::query_scalar("SELECT to_regclass('rate_limit_capacity') IS NOT NULL")
                .fetch_one(&pool)
                .await?;
        if !schema_ready {
            return Ok(());
        }
        let baseline: i64 =
            sqlx::query_scalar("SELECT entry_count FROM rate_limit_capacity WHERE singleton=true")
                .fetch_one(&pool)
                .await?;
        let maximum = usize::try_from(baseline)?.saturating_add(1);
        let limiter = PgTokenBucketLimiter::new(
            pool.clone(),
            TokenBucketConfig {
                capacity: 1,
                refill_tokens_per_second: 1.0,
                idle_ttl_ms: 60_000,
                max_entries: maximum,
            },
        )?;
        let prefix = uuid::Uuid::now_v7().to_string();
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..8 {
            let limiter = limiter.clone();
            let identifier = format!("{prefix}-{index}");
            tasks.spawn(async move {
                let key =
                    RateLimitKey::derive(RateDimension::Ip, &[17; 32], identifier.as_bytes())?;
                limiter.check(key, 1, 1_000).await
            });
        }
        let mut allowed = 0;
        let mut saturated = 0;
        while let Some(result) = tasks.join_next().await {
            match result? {
                Ok(RateLimitDecision::Allowed { .. }) => allowed += 1,
                Ok(RateLimitDecision::Denied { .. }) => {}
                Err(RateLimitError::Saturated) => saturated += 1,
                Err(error) => {
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(error));
                }
            }
        }
        // Exactly one new row can cross the globally serialized capacity gate.
        // Verify the database invariant directly as the authoritative result.
        let after: i64 =
            sqlx::query_scalar("SELECT entry_count FROM rate_limit_capacity WHERE singleton=true")
                .fetch_one(&pool)
                .await?;
        assert_eq!(after, baseline + 1);
        assert_eq!(allowed, 1);
        assert_eq!(saturated, 7);

        let digests = (0..8)
            .map(|index| {
                RateLimitKey::derive(
                    RateDimension::Ip,
                    &[17; 32],
                    format!("{prefix}-{index}").as_bytes(),
                )
                .map(|key| key.digest().to_vec())
            })
            .collect::<Result<Vec<_>, _>>()?;
        sqlx::query(
            "UPDATE rate_limit_state SET expires_at=to_timestamp(0) WHERE bucket_key=ANY($1)",
        )
        .bind(&digests)
        .execute(&pool)
        .await?;
        assert_eq!(limiter.cleanup_expired(2_000, 32).await?, 1);
        let replacement = RateLimitKey::derive(
            RateDimension::Ip,
            &[17; 32],
            format!("{prefix}-replacement").as_bytes(),
        )?;
        assert!(matches!(
            limiter.check(replacement, 1, 2_000).await?,
            RateLimitDecision::Allowed { .. }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn postgres_batch_short_circuits_without_charging_later_dimensions()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        let schema_ready: bool =
            sqlx::query_scalar("SELECT to_regclass('rate_limit_capacity') IS NOT NULL")
                .fetch_one(&pool)
                .await?;
        if !schema_ready {
            return Ok(());
        }
        let limiter = PgTokenBucketLimiter::new(
            pool.clone(),
            TokenBucketConfig {
                capacity: 2,
                refill_tokens_per_second: 1.0,
                idle_ttl_ms: 60_000,
                max_entries: 1_000_000,
            },
        )?;
        let suffix = uuid::Uuid::now_v7().to_string();
        let project = RateLimitKey::derive(
            RateDimension::Project,
            &[23; 32],
            format!("project-{suffix}").as_bytes(),
        )?;
        let actor = RateLimitKey::derive(
            RateDimension::ApiKey,
            &[23; 32],
            format!("actor-{suffix}").as_bytes(),
        )?;

        assert_eq!(
            limiter
                .check_many(&[(project, 2), (actor, 1)], 10_000)
                .await?,
            vec![
                RateLimitDecision::Allowed { remaining: 0.0 },
                RateLimitDecision::Allowed { remaining: 1.0 },
            ]
        );
        assert_eq!(
            limiter
                .check_many(&[(project, 1), (actor, 1)], 10_000)
                .await?,
            vec![RateLimitDecision::Denied {
                retry_after_ms: 1_000,
            }]
        );
        let actor_tokens: f64 =
            sqlx::query_scalar("SELECT tokens FROM rate_limit_state WHERE bucket_key=$1")
                .bind(actor.digest().as_slice())
                .fetch_one(&pool)
                .await?;
        assert_eq!(actor_tokens, 1.0);

        sqlx::query("DELETE FROM rate_limit_state WHERE bucket_key=ANY($1)")
            .bind(vec![project.digest().to_vec(), actor.digest().to_vec()])
            .execute(&pool)
            .await?;
        Ok(())
    }

    /// Local diagnostic for the durable project + actor limiter paid by every
    /// authenticated worker dispatch. Run explicitly with a disposable local
    /// PostgreSQL database:
    ///
    /// `TEST_DATABASE_URL=postgres://... cargo test -p ffdb-rate-limits \
    ///   postgres_check_many_latency_profile -- --ignored --nocapture`
    #[ignore = "requires a local migrated PostgreSQL database and is not a CI capacity claim"]
    #[tokio::test]
    async fn postgres_check_many_latency_profile()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        const ITERATIONS: usize = 500;
        let database_url = std::env::var("TEST_DATABASE_URL")?;
        let pool = PgPool::connect(&database_url).await?;
        let limiter = PgTokenBucketLimiter::new(
            pool.clone(),
            TokenBucketConfig {
                capacity: 1_000_000,
                refill_tokens_per_second: 1_000_000.0,
                idle_ttl_ms: 60_000,
                max_entries: 1_000_000,
            },
        )?;
        let suffix = uuid::Uuid::now_v7().to_string();
        let project = RateLimitKey::derive(
            RateDimension::Project,
            &[31; 32],
            format!("profile-project-{suffix}").as_bytes(),
        )?;
        let actor = RateLimitKey::derive(
            RateDimension::ApiKey,
            &[31; 32],
            format!("profile-actor-{suffix}").as_bytes(),
        )?;
        let checks = [(project, 1), (actor, 1)];
        limiter.check_many(&checks, 1_000).await?;

        let mut samples = Vec::with_capacity(ITERATIONS);
        for index in 0..ITERATIONS {
            let started = std::time::Instant::now();
            limiter
                .check_many(&checks, 2_000 + i64::try_from(index)?)
                .await?;
            samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        }
        let p50 = percentile_ms(&mut samples, 0.50);
        let p95 = percentile_ms(&mut samples, 0.95);
        let p99 = percentile_ms(&mut samples, 0.99);
        println!(
            "postgres check_many project+actor: n={ITERATIONS} p50={p50:.3}ms p95={p95:.3}ms p99={p99:.3}ms"
        );

        sqlx::query("DELETE FROM rate_limit_state WHERE bucket_key=ANY($1)")
            .bind(vec![project.digest().to_vec(), actor.digest().to_vec()])
            .execute(&pool)
            .await?;
        Ok(())
    }
}
