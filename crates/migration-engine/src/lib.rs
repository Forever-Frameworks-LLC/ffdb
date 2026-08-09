//! Atomic migration application and rollback for a single FFDB database.

use std::time::Instant;

use ffdb_sql_parser::{
    StatementKind, classify_statement, parse_rls_statement, split_sql_statements,
};
use ffdb_sqlite_rls::Compiler;
use ffdb_sqlite_runtime::{
    CancellationToken, Database, DeveloperPrincipal, ExecutionMode, RequestBudget, RuntimeError,
    Session, StatementRequest, StoredMigration,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationSpec {
    pub id: String,
    pub name: String,
    pub up_sql: String,
    pub down_sql: String,
    pub checksum: String,
    pub created_at_ms: i64,
}

impl MigrationSpec {
    #[must_use]
    pub fn calculate_checksum(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_bytes());
        hasher.update([0]);
        hasher.update(self.name.as_bytes());
        hasher.update([0]);
        hasher.update(self.up_sql.as_bytes());
        hasher.update([0]);
        hasher.update(self.down_sql.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn validate(&self) -> Result<(), MigrationError> {
        if self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || self.name.is_empty()
            || self.name.len() > 256
        {
            return Err(MigrationError::InvalidMetadata);
        }
        if self.up_sql.trim().is_empty() || self.down_sql.trim().is_empty() {
            return Err(MigrationError::MissingDirection);
        }
        if self.checksum.len() != 64 || self.checksum != self.calculate_checksum() {
            return Err(MigrationError::ChecksumMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    Applied,
    RolledBack,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationOutcome {
    pub id: String,
    pub status: MigrationStatus,
    pub schema_version_before: u64,
    pub schema_version_after: u64,
    pub applied_at_ms: i64,
    pub duration_ms: u64,
    pub execution_log: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableOperationReceipt {
    pub receipt_id: String,
    pub request_digest: Vec<u8>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MigrationError {
    #[error("migration metadata is invalid")]
    InvalidMetadata,
    #[error("both up and down SQL are required")]
    MissingDirection,
    #[error("migration checksum does not match its contents or stored history")]
    ChecksumMismatch,
    #[error("migration is already applied")]
    AlreadyApplied,
    #[error("migration is not applied")]
    NotApplied,
    #[error("migration SQL is invalid or not allowed")]
    InvalidSql,
    #[error("migration execution failed")]
    Execution,
}

impl From<RuntimeError> for MigrationError {
    fn from(_: RuntimeError) -> Self {
        Self::Execution
    }
}

#[derive(Clone, Debug, Default)]
pub struct MigrationEngine;

impl MigrationEngine {
    pub fn apply(
        &self,
        database: &Database,
        principal: DeveloperPrincipal,
        specification: &MigrationSpec,
        applied_at_ms: i64,
        cancellation: &CancellationToken,
        budget: &RequestBudget,
    ) -> Result<MigrationOutcome, MigrationError> {
        self.apply_with_receipt(
            database,
            principal,
            specification,
            applied_at_ms,
            cancellation,
            budget,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_with_receipt(
        &self,
        database: &Database,
        principal: DeveloperPrincipal,
        specification: &MigrationSpec,
        applied_at_ms: i64,
        cancellation: &CancellationToken,
        budget: &RequestBudget,
        receipt: Option<&DurableOperationReceipt>,
    ) -> Result<MigrationOutcome, MigrationError> {
        specification.validate()?;
        let started = Instant::now();
        database
            .with_context_budget(
                ExecutionMode::Developer(principal.clone()),
                cancellation,
                budget,
                |session| {
                    if let Some(existing) = session.migration_record(&specification.id)? {
                        if existing.checksum != decode_checksum(&specification.checksum)? {
                            return Err(RuntimeError::ConstraintViolation);
                        }
                        if existing.status == "applied" {
                            let outcome = outcome_from_record(&existing, MigrationStatus::Applied);
                            if let Some(receipt) = receipt {
                                return session.atomic(|session| {
                                    store_durable_receipt(
                                        session,
                                        receipt,
                                        "migration.apply",
                                        &outcome,
                                        applied_at_ms,
                                    )?;
                                    Ok(outcome)
                                });
                            }
                            return Ok(outcome);
                        }
                    }
                    session.atomic(|session| {
                        let version_before = session.schema_version()?;
                        execute_sql(session, &specification.up_sql)?;
                        let version_after = version_before
                            .checked_add(1)
                            .ok_or(RuntimeError::Database)?;
                        session.set_schema_version(version_after)?;
                        let duration_ms = elapsed_ms(started);
                        let stored = StoredMigration {
                            id: specification.id.clone(),
                            name: specification.name.clone(),
                            checksum: decode_checksum(&specification.checksum)?,
                            up_sql: specification.up_sql.clone(),
                            down_sql: specification.down_sql.clone(),
                            created_at_ms: specification.created_at_ms,
                            applied_at_ms,
                            actor_id: principal.api_key_id.clone(),
                            duration_ms: i64::try_from(duration_ms).unwrap_or(i64::MAX),
                            version_before,
                            version_after,
                            status: "applied".to_owned(),
                        };
                        session.store_migration(&stored)?;
                        let outcome = outcome_from_record(&stored, MigrationStatus::Applied);
                        if let Some(receipt) = receipt {
                            store_durable_receipt(
                                session,
                                receipt,
                                "migration.apply",
                                &outcome,
                                applied_at_ms,
                            )?;
                        }
                        Ok(outcome)
                    })
                },
            )
            .map_err(|error| match error {
                RuntimeError::ConstraintViolation => MigrationError::ChecksumMismatch,
                _ => MigrationError::Execution,
            })
    }

    pub fn rollback(
        &self,
        database: &Database,
        principal: DeveloperPrincipal,
        migration_id: &str,
        rolled_back_at_ms: i64,
        cancellation: &CancellationToken,
        budget: &RequestBudget,
    ) -> Result<MigrationOutcome, MigrationError> {
        self.rollback_with_receipt(
            database,
            principal,
            migration_id,
            rolled_back_at_ms,
            cancellation,
            budget,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rollback_with_receipt(
        &self,
        database: &Database,
        principal: DeveloperPrincipal,
        migration_id: &str,
        rolled_back_at_ms: i64,
        cancellation: &CancellationToken,
        budget: &RequestBudget,
        receipt: Option<&DurableOperationReceipt>,
    ) -> Result<MigrationOutcome, MigrationError> {
        let started = Instant::now();
        database
            .with_context_budget(
                ExecutionMode::Developer(principal.clone()),
                cancellation,
                budget,
                |session| {
                    let Some(existing) = session.migration_record(migration_id)? else {
                        return Err(RuntimeError::StatementNotAllowed);
                    };
                    if existing.status != "applied" {
                        return Err(RuntimeError::StatementNotAllowed);
                    }
                    session.atomic(|session| {
                        let version_before = session.schema_version()?;
                        execute_sql(session, &existing.down_sql)?;
                        let version_after = version_before
                            .checked_add(1)
                            .ok_or(RuntimeError::Database)?;
                        session.set_schema_version(version_after)?;
                        let mut stored = existing.clone();
                        stored.applied_at_ms = rolled_back_at_ms;
                        stored.actor_id = principal.api_key_id.clone();
                        stored.duration_ms = i64::try_from(elapsed_ms(started)).unwrap_or(i64::MAX);
                        stored.version_before = version_before;
                        stored.version_after = version_after;
                        stored.status = "rolled_back".to_owned();
                        session.store_migration(&stored)?;
                        let outcome = outcome_from_record(&stored, MigrationStatus::RolledBack);
                        if let Some(receipt) = receipt {
                            store_durable_receipt(
                                session,
                                receipt,
                                "migration.rollback",
                                &outcome,
                                rolled_back_at_ms,
                            )?;
                        }
                        Ok(outcome)
                    })
                },
            )
            .map_err(|error| match error {
                RuntimeError::StatementNotAllowed => MigrationError::NotApplied,
                _ => MigrationError::Execution,
            })
    }
}

fn store_durable_receipt(
    session: &mut Session<'_>,
    receipt: &DurableOperationReceipt,
    operation: &str,
    outcome: &MigrationOutcome,
    recorded_at_ms: i64,
) -> Result<(), RuntimeError> {
    let result_json = serde_json::to_string(outcome).map_err(|_| RuntimeError::Database)?;
    session.store_worker_operation_receipt(
        &receipt.receipt_id,
        &receipt.request_digest,
        operation,
        &result_json,
        recorded_at_ms,
    )
}

fn execute_sql(session: &mut Session<'_>, sql: &str) -> Result<(), RuntimeError> {
    let statements = split_sql_statements(sql).map_err(|_| RuntimeError::StatementNotAllowed)?;
    if statements.is_empty() {
        return Err(RuntimeError::StatementNotAllowed);
    }
    for sql in statements {
        let class = classify_statement(sql).map_err(|_| RuntimeError::StatementNotAllowed)?;
        if class.kind == StatementKind::Rls {
            let statement =
                parse_rls_statement(sql).map_err(|_| RuntimeError::StatementNotAllowed)?;
            let mut catalog = session.load_rls_catalog()?;
            let schema = session.schema_snapshot(&catalog)?;
            catalog
                .apply(&schema, statement)
                .map_err(|_| RuntimeError::StatementNotAllowed)?;
            let current_schema = session.schema_snapshot(&catalog)?;
            let plan = Compiler
                .compile(&current_schema, &catalog)
                .map_err(|_| RuntimeError::StatementNotAllowed)?;
            session.apply_rls_plan(&plan)?;
            session.store_rls_catalog(&catalog)?;
        } else {
            let _ = session.execute(&StatementRequest {
                sql: sql.to_owned(),
                parameters: Vec::new(),
            })?;
        }
    }
    let catalog = session.load_rls_catalog()?;
    session.refresh_change_capture(&catalog)?;
    Ok(())
}

fn decode_checksum(checksum: &str) -> Result<Vec<u8>, RuntimeError> {
    if checksum.len() != 64 {
        return Err(RuntimeError::ConstraintViolation);
    }
    (0..checksum.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&checksum[index..index + 2], 16)
                .map_err(|_| RuntimeError::ConstraintViolation)
        })
        .collect()
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn outcome_from_record(record: &StoredMigration, status: MigrationStatus) -> MigrationOutcome {
    MigrationOutcome {
        id: record.id.clone(),
        status,
        schema_version_before: record.version_before,
        schema_version_after: record.version_after,
        applied_at_ms: record.applied_at_ms,
        duration_ms: u64::try_from(record.duration_ms).unwrap_or_default(),
        execution_log: vec![match status {
            MigrationStatus::Applied => "migration applied atomically".to_owned(),
            MigrationStatus::RolledBack => {
                "developer-supplied down SQL executed atomically".to_owned()
            }
        }],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::{Duration, Instant};

    use ffdb_sqlite_runtime::{
        AuthContext, ExecutionLimits, QueryResult, ResultValue, RuntimeConfig, SqlParameter,
        StatementRequest, TrustedDatabasePath,
    };
    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;

    const DATABASE_ID: &str = "019fc39c-ddbd-7d12-9849-e4ee35310132";

    fn setup() -> (
        TempDir,
        Database,
        RequestBudget,
        CancellationToken,
        DeveloperPrincipal,
    ) {
        let directory = TempDir::new().unwrap();
        let path = TrustedDatabasePath::for_database(directory.path(), DATABASE_ID).unwrap();
        let database = Database::open(path, RuntimeConfig::default()).unwrap();
        let budget = RequestBudget {
            limits: ExecutionLimits::default(),
            deadline: Instant::now() + Duration::from_secs(30),
        };
        let cancellation = CancellationToken::default();
        let principal = DeveloperPrincipal {
            actor_id: "operator".to_owned(),
            api_key_id: "key-1".to_owned(),
        };
        (directory, database, budget, cancellation, principal)
    }

    fn specification(id: &str, up_sql: &str, down_sql: &str) -> MigrationSpec {
        let mut specification = MigrationSpec {
            id: id.to_owned(),
            name: format!("migration {id}"),
            up_sql: up_sql.to_owned(),
            down_sql: down_sql.to_owned(),
            checksum: String::new(),
            created_at_ms: 1,
        };
        specification.checksum = specification.calculate_checksum();
        specification
    }

    #[test]
    fn apply_is_idempotent_and_rollback_uses_stored_down_sql() {
        let (_directory, database, budget, cancellation, principal) = setup();
        let migration = specification(
            "001_documents",
            "CREATE TABLE documents(id INTEGER PRIMARY KEY)",
            "DROP TABLE documents",
        );
        let engine = MigrationEngine;
        let applied = engine
            .apply(
                &database,
                principal.clone(),
                &migration,
                10,
                &cancellation,
                &budget,
            )
            .unwrap();
        assert_eq!(applied.schema_version_after, 1);
        let repeated = engine
            .apply(
                &database,
                principal.clone(),
                &migration,
                11,
                &cancellation,
                &budget,
            )
            .unwrap();
        assert_eq!(repeated.schema_version_after, 1);
        let rolled_back = engine
            .rollback(
                &database,
                principal.clone(),
                &migration.id,
                20,
                &cancellation,
                &budget,
            )
            .unwrap();
        assert_eq!(rolled_back.schema_version_after, 2);
        let missing = database.with_context(
            ExecutionMode::Developer(principal),
            &cancellation,
            |session| {
                session.execute(&StatementRequest {
                    sql: "SELECT * FROM documents".to_owned(),
                    parameters: Vec::new(),
                })
            },
        );
        assert!(missing.is_err());
    }

    #[test]
    fn reused_identifier_with_different_checksum_is_rejected() {
        let (_directory, database, budget, cancellation, principal) = setup();
        let first = specification("001", "CREATE TABLE first(id)", "DROP TABLE first");
        MigrationEngine
            .apply(
                &database,
                principal.clone(),
                &first,
                10,
                &cancellation,
                &budget,
            )
            .unwrap();
        let second = specification("001", "CREATE TABLE second(id)", "DROP TABLE second");
        assert_eq!(
            MigrationEngine.apply(&database, principal, &second, 20, &cancellation, &budget),
            Err(MigrationError::ChecksumMismatch)
        );
    }

    #[test]
    fn migration_compiles_rls_and_two_users_receive_different_rows() {
        let (_directory, database, budget, cancellation, principal) = setup();
        let migration = specification(
            "001_rls",
            "CREATE TABLE documents(id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, body TEXT); \
             ALTER TABLE documents ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY owner_read ON documents FOR SELECT TO authenticated \
             USING (owner_id = auth.uid())",
            "SELECT 1",
        );
        MigrationEngine
            .apply(
                &database,
                principal.clone(),
                &migration,
                10,
                &cancellation,
                &budget,
            )
            .unwrap();
        database
            .with_context(
                ExecutionMode::Developer(principal),
                &cancellation,
                |session| {
                    let _ = session.execute(&StatementRequest {
                        sql: "INSERT INTO documents(id, owner_id, body) VALUES (?1, ?2, ?3)"
                            .to_owned(),
                        parameters: vec![
                            SqlParameter::Integer(1),
                            SqlParameter::Text("alice".to_owned()),
                            SqlParameter::Text("a".to_owned()),
                        ],
                    })?;
                    let _ = session.execute(&StatementRequest {
                        sql: "INSERT INTO documents(id, owner_id, body) VALUES (?1, ?2, ?3)"
                            .to_owned(),
                        parameters: vec![
                            SqlParameter::Integer(2),
                            SqlParameter::Text("bob".to_owned()),
                            SqlParameter::Text("b".to_owned()),
                        ],
                    })?;
                    Ok(())
                },
            )
            .unwrap();

        for (subject, expected) in [("alice", "a"), ("bob", "b")] {
            let context = ExecutionMode::EndUser(AuthContext {
                project_id: "project".to_owned(),
                subject: subject.to_owned(),
                role: "authenticated".to_owned(),
                claims: Map::new(),
                token_id: format!("token-{subject}"),
            });
            let result: QueryResult = database
                .with_context(context, &cancellation, |session| {
                    session.execute(&StatementRequest {
                        sql: "SELECT body FROM documents".to_owned(),
                        parameters: Vec::new(),
                    })
                })
                .unwrap();
            assert_eq!(
                result.rows,
                vec![vec![ResultValue::Text(expected.to_owned())]]
            );
        }
    }
}
