use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ffdb_sql_parser::Identifier;
use ffdb_sqlite_rls::{Operation, RlsCatalog, TableSchema, backing_table_name};
use hmac::{Hmac, Mac};
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::{
    ExecutionMode, InternalLease, QueryResult, ResultColumn, ResultValue, RuntimeError, Session,
    SqlParameter, StatementRequest, context::MutationLease,
};

type HmacSha256 = Hmac<Sha256>;
const CURSOR_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const MAX_CURSOR_BYTES: usize = 2_048;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMutationOperation {
    Upsert,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncChangeOperation {
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncMutation {
    pub mutation_id: String,
    pub table: String,
    pub primary_key: JsonValue,
    pub operation: SyncMutationOperation,
    pub values: Map<String, JsonValue>,
    pub base_row_version: Option<u64>,
    pub client_timestamp_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncMutationReceipt {
    pub mutation_id: String,
    pub sequence: u64,
    pub row_version: u64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncChange {
    pub sequence: u64,
    pub transaction_id: String,
    pub table: String,
    pub primary_key: JsonValue,
    pub operation: SyncChangeOperation,
    pub row_version: u64,
    pub values: Option<Map<String, JsonValue>>,
    pub tombstone: Option<JsonValue>,
    pub actor: String,
    pub schema_version: u64,
    pub committed_at_ms: i64,
    pub client_mutation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncPullResult {
    pub changes: Vec<SyncChange>,
    pub cursor: String,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub schema_version: u64,
    pub cursor: String,
    pub tables: BTreeMap<String, QueryResult>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorClaims {
    project_id: String,
    subject: String,
    client_id: String,
    scope_fingerprint: String,
    sequence: u64,
    schema_version: u64,
    issued_at_ms: i64,
    expires_at_ms: i64,
}

impl Session<'_> {
    pub(crate) fn suspend_change_capture(&mut self) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        let names = {
            let mut statement = self.connection.prepare(
                "SELECT name FROM sqlite_schema WHERE type='trigger' \
                 AND name GLOB '__ffdb_sync_capture_*'",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for name in names {
            let name = Identifier::new(name).map_err(|_| RuntimeError::Database)?;
            self.connection
                .execute_batch(&format!("DROP TRIGGER {}", name.quoted()))?;
        }
        Ok(())
    }

    /// Rebuilds trusted row-change triggers for every logical table with a primary key.
    /// The triggers execute in the same SQLite statement/transaction as the application write.
    pub fn refresh_change_capture(&mut self, catalog: &RlsCatalog) -> Result<(), RuntimeError> {
        let schema = self.schema_snapshot(catalog)?;
        let mut generated_sources = Vec::new();
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .execute_batch("SAVEPOINT __ffdb_sync_capture_refresh")?;
        let result = (|| -> Result<(), RuntimeError> {
            for table in &schema.tables {
                let names = capture_source_names(table.name.as_str())?;
                for name in &names {
                    self.connection
                        .execute_batch(&format!("DROP TRIGGER IF EXISTS {}", name.quoted()))?;
                }
                if table
                    .columns
                    .iter()
                    .all(|column| column.primary_key_ordinal.is_none())
                {
                    continue;
                }
                let physical = if table.protected {
                    backing_table_name(&table.name).map_err(|_| RuntimeError::Database)?
                } else {
                    table.name.clone()
                };
                for sql in capture_trigger_sql(table, &physical, &names) {
                    self.connection.execute_batch(&sql)?;
                }
                generated_sources.extend(names);
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = self
                .connection
                .execute_batch("ROLLBACK TO __ffdb_sync_capture_refresh");
            let _ = self
                .connection
                .execute_batch("RELEASE __ffdb_sync_capture_refresh");
            return Err(error);
        }
        self.connection
            .execute_batch("RELEASE __ffdb_sync_capture_refresh")?;
        self.context
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .approved_sources
            .extend(
                generated_sources
                    .into_iter()
                    .map(|source| source.as_str().to_ascii_lowercase()),
            );
        Ok(())
    }

    pub fn sync_snapshot(
        &mut self,
        requested_tables: Option<&[String]>,
    ) -> Result<SyncSnapshot, RuntimeError> {
        let auth = self.sync_auth()?.clone();
        let catalog = self.load_rls_catalog()?;
        let schema = self.schema_snapshot(&catalog)?;
        let tables = if let Some(requested) = requested_tables {
            requested.to_vec()
        } else {
            schema
                .tables
                .iter()
                .filter(|table| {
                    table
                        .columns
                        .iter()
                        .any(|column| column.primary_key_ordinal.is_some())
                })
                .map(|table| table.name.as_str().to_owned())
                .collect()
        };
        let available = schema
            .tables
            .iter()
            .map(|table| table.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut output = BTreeMap::new();
        for table_name in tables {
            let table = Identifier::new(table_name.clone())
                .map_err(|_| RuntimeError::StatementNotAllowed)?;
            if table.is_internal() || !available.contains(table.as_str()) {
                return Err(RuntimeError::StatementNotAllowed);
            }
            let table_schema = schema
                .table(&table)
                .ok_or(RuntimeError::StatementNotAllowed)?;
            if table_schema
                .columns
                .iter()
                .all(|column| column.primary_key_ordinal.is_none())
            {
                return Err(RuntimeError::StatementNotAllowed);
            }
            let mut result = self.execute(&StatementRequest {
                sql: format!("SELECT * FROM {}", table.quoted()),
                parameters: Vec::new(),
            })?;
            if result.truncated {
                return Err(RuntimeError::ResponseTooLarge);
            }
            self.annotate_snapshot_rows(table_schema, &mut result)?;
            if result.encoded_size() > self.limits.max_response_bytes {
                return Err(RuntimeError::ResponseTooLarge);
            }
            output.insert(table_name, result);
        }
        let schema_version = self.schema_version()?;
        let sequence = self.latest_sync_sequence()?;
        let cursor = self.issue_sync_cursor(&auth, sequence, schema_version, epoch_ms()?)?;
        Ok(SyncSnapshot {
            schema_version,
            cursor,
            tables: output,
        })
    }

    fn annotate_snapshot_rows(
        &mut self,
        table: &TableSchema,
        result: &mut QueryResult,
    ) -> Result<(), RuntimeError> {
        let mut primary_key = table
            .columns
            .iter()
            .filter_map(|column| {
                column
                    .primary_key_ordinal
                    .map(|ordinal| (ordinal, column.name.as_str()))
            })
            .collect::<Vec<_>>();
        primary_key.sort_by_key(|(ordinal, _)| *ordinal);
        let primary_key_indices = primary_key
            .iter()
            .map(|(_, name)| {
                result
                    .columns
                    .iter()
                    .position(|column| column.name == *name)
                    .ok_or(RuntimeError::Database)
            })
            .collect::<Result<Vec<_>, _>>()?;

        for row in &mut result.rows {
            let key_values = primary_key
                .iter()
                .zip(&primary_key_indices)
                .map(|((_, name), index)| {
                    row.get(*index)
                        .ok_or(RuntimeError::Database)
                        .and_then(result_json)
                        .map(|value| ((*name).to_owned(), value))
                })
                .collect::<Result<Map<_, _>, _>>()?;
            let canonical_key = JsonValue::Object(key_values);
            let stored_key =
                serde_json::to_string(&canonical_key).map_err(|_| RuntimeError::Database)?;
            let public_key = if primary_key.len() == 1 {
                canonical_key
                    .as_object()
                    .and_then(|values| values.values().next())
                    .cloned()
                    .ok_or(RuntimeError::Database)?
            } else {
                canonical_key
            };
            let encoded_key =
                serde_json::to_string(&public_key).map_err(|_| RuntimeError::Database)?;
            let (row_version, server_sequence) =
                self.load_sync_version(table.name.as_str(), &stored_key)?;
            row.push(ResultValue::Text(encoded_key));
            row.push(ResultValue::Integer(
                i64::try_from(row_version).map_err(|_| RuntimeError::Database)?,
            ));
            row.push(ResultValue::Integer(
                i64::try_from(server_sequence).map_err(|_| RuntimeError::Database)?,
            ));
        }
        result.columns.extend([
            ResultColumn {
                name: "__ffdb_primary_key".to_owned(),
                declared_type: Some("TEXT".to_owned()),
            },
            ResultColumn {
                name: "__ffdb_row_version".to_owned(),
                declared_type: Some("INTEGER".to_owned()),
            },
            ResultColumn {
                name: "__ffdb_server_sequence".to_owned(),
                declared_type: Some("INTEGER".to_owned()),
            },
        ]);
        Ok(())
    }

    fn load_sync_version(&mut self, table: &str, key: &str) -> Result<(u64, u64), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT row_version,last_sequence FROM __ffdb_sync_versions \
                 WHERE table_name=?1 AND primary_key_json=?2 AND deleted=0",
                rusqlite::params![table, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map(|version| version.unwrap_or((0, 0)))
            .map_err(Into::into)
    }

    pub fn sync_current_cursor(&mut self) -> Result<String, RuntimeError> {
        let auth = self.sync_auth()?.clone();
        let schema_version = self.schema_version()?;
        let sequence = self.latest_sync_sequence()?;
        self.issue_sync_cursor(&auth, sequence, schema_version, epoch_ms()?)
    }

    pub fn sync_pull(
        &mut self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<SyncPullResult, RuntimeError> {
        if limit == 0 || limit > 1_000 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let auth = self.sync_auth()?.clone();
        let schema_version = self.schema_version()?;
        let now_ms = epoch_ms()?;
        let after = if let Some(cursor) = cursor {
            self.verify_sync_cursor(cursor, &auth, schema_version, now_ms)?
        } else {
            0
        };
        let scan_limit = self.limits.max_rows.min(10_000).max(limit + 1);
        let changes = self.load_sync_changes(after, scan_limit)?;
        let mut visible = Vec::new();
        let mut examined = after;
        let mut has_more = false;
        for mut change in changes {
            // Change capture stores a canonical object so the internal version
            // ledger has one unambiguous key format. The public snapshot
            // protocol has always represented a single-column key as its scalar
            // value, so pulls must expose the same shape or replicas retain the
            // snapshot row and insert an apparent duplicate for the update.
            change.primary_key = public_sync_primary_key(change.primary_key)?;
            let row = change
                .values
                .as_ref()
                .or_else(|| change.tombstone.as_ref().and_then(JsonValue::as_object));
            if let Some(row) = row
                && self.evaluate_select_policy(&change.table, row)?
            {
                if visible.len() == limit {
                    has_more = true;
                    break;
                }
                examined = change.sequence;
                visible.push(change);
            } else {
                examined = change.sequence;
            }
        }
        if examined < self.latest_sync_sequence()? {
            has_more = true;
        }
        Ok(SyncPullResult {
            changes: visible,
            cursor: self.issue_sync_cursor(&auth, examined, schema_version, now_ms)?,
            has_more,
        })
    }

    pub fn sync_apply_mutations(
        &mut self,
        expected_schema_version: u64,
        mutations: &[SyncMutation],
    ) -> Result<(Vec<SyncMutationReceipt>, String), RuntimeError> {
        if mutations.is_empty() || mutations.len() > 100 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let auth = self.sync_auth()?.clone();
        let schema_version = self.schema_version()?;
        if schema_version != expected_schema_version {
            return Err(RuntimeError::SyncSchemaMismatch);
        }
        let now_ms = epoch_ms()?;
        let receipts = self.atomic(|session| {
            let mut receipts = Vec::with_capacity(mutations.len());
            for mutation in mutations {
                receipts.push(session.apply_sync_mutation(&auth, mutation)?);
            }
            Ok(receipts)
        })?;
        let sequence = self.latest_sync_sequence()?;
        let cursor = self.issue_sync_cursor(&auth, sequence, schema_version, now_ms)?;
        Ok((receipts, cursor))
    }

    /// Applies a sync request inside an already-open `atomic` block while
    /// isolating each mutation behind a savepoint. Accepted mutations and the
    /// worker's operation-level usage receipt can therefore share one commit;
    /// rejected mutations leave no partial effects.
    pub fn sync_apply_mutations_individually_in_current_atomic(
        &mut self,
        expected_schema_version: u64,
        mutations: &[SyncMutation],
    ) -> Result<(Vec<Result<SyncMutationReceipt, RuntimeError>>, String), RuntimeError> {
        if self.transaction_deadline.is_none() || mutations.is_empty() || mutations.len() > 100 {
            return Err(RuntimeError::StatementNotAllowed);
        }
        let auth = self.sync_auth()?.clone();
        let schema_version = self.schema_version()?;
        if schema_version != expected_schema_version {
            return Err(RuntimeError::SyncSchemaMismatch);
        }
        let now_ms = epoch_ms()?;
        let mut receipts = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            self.connection
                .execute_batch("SAVEPOINT __ffdb_sync_usage_item")?;
            match self.apply_sync_mutation(&auth, mutation) {
                Ok(receipt) => {
                    self.connection
                        .execute_batch("RELEASE __ffdb_sync_usage_item")?;
                    receipts.push(Ok(receipt));
                }
                Err(error) => {
                    self.connection.execute_batch(
                        "ROLLBACK TO __ffdb_sync_usage_item; RELEASE __ffdb_sync_usage_item",
                    )?;
                    receipts.push(Err(error));
                }
            }
        }
        let sequence = self.latest_sync_sequence()?;
        let cursor = self.issue_sync_cursor(&auth, sequence, schema_version, now_ms)?;
        Ok((receipts, cursor))
    }

    fn apply_sync_mutation(
        &mut self,
        auth: &crate::AuthContext,
        mutation: &SyncMutation,
    ) -> Result<SyncMutationReceipt, RuntimeError> {
        validate_mutation(mutation)?;
        let payload =
            serde_json::to_vec(mutation).map_err(|_| RuntimeError::SyncMutationInvalid)?;
        let payload_hash = Sha256::digest(payload).to_vec();
        if let Some((stored_hash, sequence, row_version)) =
            self.load_sync_mutation(&auth.subject, &auth.token_id, &mutation.mutation_id)?
        {
            if stored_hash != payload_hash {
                return Err(RuntimeError::SyncMutationInvalid);
            }
            return Ok(SyncMutationReceipt {
                mutation_id: mutation.mutation_id.clone(),
                sequence,
                row_version,
                duplicate: true,
            });
        }

        let table = Identifier::new(mutation.table.clone())
            .map_err(|_| RuntimeError::SyncMutationInvalid)?;
        if table.is_internal() {
            return Err(RuntimeError::SyncMutationInvalid);
        }
        let (primary_key, primary_key_json) =
            self.resolve_primary_key(&table, &mutation.primary_key)?;
        let current = self.select_sync_row(&table, &primary_key)?;
        let (sequence, row_version) = {
            let _mutation = MutationLease::install(&self.context, &mutation.mutation_id)?;
            match mutation.operation {
                SyncMutationOperation::Upsert if current.is_some() => {
                    let _ = self.update_sync_row(&table, &primary_key, &mutation.values)?;
                }
                SyncMutationOperation::Upsert => {
                    let _ = self.insert_sync_row(&table, &primary_key, &mutation.values)?;
                }
                SyncMutationOperation::Delete => {
                    current.ok_or(RuntimeError::ConstraintViolation)?;
                    self.delete_sync_row(&table, &primary_key)?;
                }
            }
            self.load_captured_sync_change(
                table.as_str(),
                &primary_key_json,
                &mutation.mutation_id,
            )?
            .ok_or(RuntimeError::Database)?
        };
        self.store_sync_mutation(
            &auth.subject,
            &auth.token_id,
            &mutation.mutation_id,
            &payload_hash,
            sequence,
            row_version,
        )?;
        Ok(SyncMutationReceipt {
            mutation_id: mutation.mutation_id.clone(),
            sequence,
            row_version,
            duplicate: false,
        })
    }

    fn sync_auth(&self) -> Result<&crate::AuthContext, RuntimeError> {
        match &self.mode {
            ExecutionMode::EndUser(auth) => Ok(auth),
            ExecutionMode::Developer(_) => Err(RuntimeError::StatementNotAllowed),
        }
    }

    fn resolve_primary_key(
        &mut self,
        table: &Identifier,
        value: &JsonValue,
    ) -> Result<(Vec<(Identifier, JsonValue)>, String), RuntimeError> {
        let catalog = self.load_rls_catalog()?;
        let schema = self.schema_snapshot(&catalog)?;
        let table_schema = schema
            .table(table)
            .ok_or(RuntimeError::SyncMutationInvalid)?;
        let mut keys = table_schema
            .columns
            .iter()
            .filter_map(|column| {
                column
                    .primary_key_ordinal
                    .map(|ordinal| (ordinal, &column.name))
            })
            .collect::<Vec<_>>();
        keys.sort_by_key(|(ordinal, _)| *ordinal);
        if keys.is_empty() {
            return Err(RuntimeError::SyncMutationInvalid);
        }
        let resolved = if keys.len() == 1 && !value.is_object() {
            vec![(keys[0].1.clone(), value.clone())]
        } else {
            let object = value.as_object().ok_or(RuntimeError::SyncMutationInvalid)?;
            if object.len() != keys.len() {
                return Err(RuntimeError::SyncMutationInvalid);
            }
            keys.into_iter()
                .map(|(_, name)| {
                    object
                        .get(name.as_str())
                        .cloned()
                        .map(|value| (name.clone(), value))
                        .ok_or(RuntimeError::SyncMutationInvalid)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        for (_, value) in &resolved {
            let _ = json_parameter(value)?;
        }
        let canonical = JsonValue::Object(
            resolved
                .iter()
                .map(|(name, value)| (name.as_str().to_owned(), value.clone()))
                .collect(),
        );
        let json =
            serde_json::to_string(&canonical).map_err(|_| RuntimeError::SyncMutationInvalid)?;
        Ok((resolved, json))
    }

    fn select_sync_row(
        &mut self,
        table: &Identifier,
        primary_key: &[(Identifier, JsonValue)],
    ) -> Result<Option<Map<String, JsonValue>>, RuntimeError> {
        let (where_sql, parameters) = key_where(primary_key)?;
        let result = self.execute(&StatementRequest {
            sql: format!("SELECT * FROM {} WHERE {where_sql}", table.quoted()),
            parameters,
        })?;
        result
            .rows
            .first()
            .map(|row| result_row(&result, row))
            .transpose()
    }

    fn update_sync_row(
        &mut self,
        table: &Identifier,
        primary_key: &[(Identifier, JsonValue)],
        values: &Map<String, JsonValue>,
    ) -> Result<Map<String, JsonValue>, RuntimeError> {
        let (mut assignments, mut parameters) = mutation_assignments(values, primary_key)?;
        if assignments.is_empty() {
            let key = primary_key
                .first()
                .ok_or(RuntimeError::SyncMutationInvalid)?
                .0
                .quoted();
            assignments.push(format!("{key} = {key}"));
        }
        let (where_sql, key_parameters) = key_where_offset(primary_key, parameters.len())?;
        parameters.extend(key_parameters);
        let result = self.execute(&StatementRequest {
            sql: format!(
                "UPDATE {} SET {} WHERE {where_sql} RETURNING *",
                table.quoted(),
                assignments.join(", ")
            ),
            parameters,
        })?;
        let row = result
            .rows
            .first()
            .ok_or(RuntimeError::ConstraintViolation)?;
        result_row(&result, row)
    }

    fn insert_sync_row(
        &mut self,
        table: &Identifier,
        primary_key: &[(Identifier, JsonValue)],
        values: &Map<String, JsonValue>,
    ) -> Result<Map<String, JsonValue>, RuntimeError> {
        let mut columns = primary_key
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in values {
            let identifier =
                Identifier::new(name.clone()).map_err(|_| RuntimeError::SyncMutationInvalid)?;
            if identifier.is_internal() || columns.insert(identifier, value.clone()).is_some() {
                return Err(RuntimeError::SyncMutationInvalid);
            }
        }
        let names = columns.keys().map(Identifier::quoted).collect::<Vec<_>>();
        let parameters = columns
            .values()
            .map(json_parameter)
            .collect::<Result<Vec<_>, _>>()?;
        let placeholders = (1..=parameters.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>();
        let result = self.execute(&StatementRequest {
            sql: format!(
                "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
                table.quoted(),
                names.join(", "),
                placeholders.join(", ")
            ),
            parameters,
        })?;
        let row = result
            .rows
            .first()
            .ok_or(RuntimeError::ConstraintViolation)?;
        result_row(&result, row)
    }

    fn delete_sync_row(
        &mut self,
        table: &Identifier,
        primary_key: &[(Identifier, JsonValue)],
    ) -> Result<(), RuntimeError> {
        let (where_sql, parameters) = key_where(primary_key)?;
        let _ = self.execute(&StatementRequest {
            sql: format!("DELETE FROM {} WHERE {where_sql}", table.quoted()),
            parameters,
        })?;
        Ok(())
    }

    fn evaluate_select_policy(
        &mut self,
        table_name: &str,
        row: &Map<String, JsonValue>,
    ) -> Result<bool, RuntimeError> {
        let table = Identifier::new(table_name.to_owned()).map_err(|_| RuntimeError::Database)?;
        let catalog = self.load_rls_catalog()?;
        let predicate = catalog
            .combined_predicates(&table, Operation::Select)
            .map_err(|_| RuntimeError::Database)?
            .using_sql
            .unwrap_or_else(|| "0".to_owned());
        let mut columns = Vec::with_capacity(row.len());
        let mut parameters = Vec::with_capacity(row.len());
        for (index, (name, value)) in row.iter().enumerate() {
            let name = Identifier::new(name.clone()).map_err(|_| RuntimeError::Database)?;
            columns.push(format!("?{} AS {}", index + 1, name.quoted()));
            parameters.push(json_parameter(value)?);
        }
        let candidate = if columns.is_empty() {
            "SELECT 1".to_owned()
        } else {
            format!("SELECT {}", columns.join(", "))
        };
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM ({candidate}) AS {} WHERE ({predicate}))",
            table.quoted()
        );
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(&sql, rusqlite::params_from_iter(&parameters), |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    fn latest_sync_sequence(&mut self) -> Result<u64, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT next_sequence-1 FROM __ffdb_sync_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn load_captured_sync_change(
        &mut self,
        table: &str,
        key: &str,
        mutation_id: &str,
    ) -> Result<Option<(u64, u64)>, RuntimeError> {
        let transaction_id = self
            .context
            .lock()
            .map_err(|_| RuntimeError::Poisoned)?
            .request_transaction_id
            .clone()
            .ok_or(RuntimeError::Poisoned)?;
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT sequence,row_version FROM __ffdb_sync_changes \
                 WHERE transaction_id=?1 AND table_name=?2 AND primary_key_json=?3 \
                 AND client_mutation_id=?4 ORDER BY sequence DESC LIMIT 1",
                rusqlite::params![transaction_id, table, key, mutation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    fn load_sync_changes(
        &mut self,
        after: u64,
        limit: usize,
    ) -> Result<Vec<SyncChange>, RuntimeError> {
        let limit = u64::try_from(limit).unwrap_or(u64::MAX);
        let _internal = InternalLease::enter(&self.context)?;
        let mut statement = self.connection.prepare(
            "SELECT sequence,transaction_id,table_name,primary_key_json,operation,row_version,\
             values_json,tombstone_json,actor,schema_version,committed_at_ms,client_mutation_id \
             FROM __ffdb_sync_changes WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let rows = statement.query_map(rusqlite::params![after, limit], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, u64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, u64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        let mut changes = Vec::new();
        for row in rows {
            let (
                sequence,
                transaction_id,
                table,
                primary_key,
                operation,
                row_version,
                values,
                tombstone,
                actor,
                schema_version,
                committed_at_ms,
                client_mutation_id,
            ) = row?;
            changes.push(SyncChange {
                sequence,
                transaction_id,
                table,
                primary_key: serde_json::from_str(&primary_key)
                    .map_err(|_| RuntimeError::Database)?,
                operation: match operation.as_str() {
                    "insert" => SyncChangeOperation::Insert,
                    "update" => SyncChangeOperation::Update,
                    "delete" => SyncChangeOperation::Delete,
                    _ => return Err(RuntimeError::Database),
                },
                row_version,
                values: values
                    .map(|value| serde_json::from_str(&value))
                    .transpose()
                    .map_err(|_| RuntimeError::Database)?,
                tombstone: tombstone
                    .map(|value| serde_json::from_str(&value))
                    .transpose()
                    .map_err(|_| RuntimeError::Database)?,
                actor,
                schema_version,
                committed_at_ms,
                client_mutation_id,
            });
        }
        Ok(changes)
    }

    fn load_sync_mutation(
        &mut self,
        subject: &str,
        client_id: &str,
        mutation_id: &str,
    ) -> Result<Option<(Vec<u8>, u64, u64)>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT payload_hash,sequence,row_version FROM __ffdb_sync_mutations \
                 WHERE subject=?1 AND client_id=?2 AND mutation_id=?3",
                [subject, client_id, mutation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    fn store_sync_mutation(
        &mut self,
        subject: &str,
        client_id: &str,
        mutation_id: &str,
        payload_hash: &[u8],
        sequence: u64,
        row_version: u64,
    ) -> Result<(), RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection.execute(
            "INSERT INTO __ffdb_sync_mutations(subject,client_id,mutation_id,payload_hash,sequence,row_version) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![subject,client_id,mutation_id,payload_hash,sequence,row_version],
        )?;
        Ok(())
    }

    fn issue_sync_cursor(
        &mut self,
        auth: &crate::AuthContext,
        sequence: u64,
        schema_version: u64,
        now_ms: i64,
    ) -> Result<String, RuntimeError> {
        let claims = CursorClaims {
            project_id: auth.project_id.clone(),
            subject: auth.subject.clone(),
            client_id: auth.token_id.clone(),
            scope_fingerprint: scope_fingerprint(auth)?,
            sequence,
            schema_version,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(CURSOR_TTL_MS),
        };
        let payload = serde_json::to_vec(&claims).map_err(|_| RuntimeError::Database)?;
        let secret = self.sync_cursor_secret()?;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(&secret)
            .map_err(|_| RuntimeError::Database)?;
        mac.update(&payload);
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        ))
    }

    fn verify_sync_cursor(
        &mut self,
        cursor: &str,
        auth: &crate::AuthContext,
        schema_version: u64,
        now_ms: i64,
    ) -> Result<u64, RuntimeError> {
        if cursor.len() > MAX_CURSOR_BYTES {
            return Err(RuntimeError::SyncCursorInvalid);
        }
        let (payload, signature) = cursor
            .split_once('.')
            .ok_or(RuntimeError::SyncCursorInvalid)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| RuntimeError::SyncCursorInvalid)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| RuntimeError::SyncCursorInvalid)?;
        let secret = self.sync_cursor_secret()?;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(&secret)
            .map_err(|_| RuntimeError::Database)?;
        mac.update(&payload);
        mac.verify_slice(&signature)
            .map_err(|_| RuntimeError::SyncCursorInvalid)?;
        let claims: CursorClaims =
            serde_json::from_slice(&payload).map_err(|_| RuntimeError::SyncCursorInvalid)?;
        if claims.project_id != auth.project_id
            || claims.subject != auth.subject
            || claims.client_id != auth.token_id
            || claims.scope_fingerprint != scope_fingerprint(auth)?
            || claims.schema_version != schema_version
            || now_ms < claims.issued_at_ms
            || now_ms >= claims.expires_at_ms
        {
            return Err(RuntimeError::SyncCursorInvalid);
        }
        Ok(claims.sequence)
    }

    fn sync_cursor_secret(&mut self) -> Result<Vec<u8>, RuntimeError> {
        let _internal = InternalLease::enter(&self.context)?;
        self.connection
            .query_row(
                "SELECT cursor_secret FROM __ffdb_sync_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}

fn public_sync_primary_key(primary_key: JsonValue) -> Result<JsonValue, RuntimeError> {
    let JsonValue::Object(values) = primary_key else {
        return Err(RuntimeError::Database);
    };
    if values.is_empty() {
        return Err(RuntimeError::Database);
    }
    if values.len() == 1 {
        return values.into_values().next().ok_or(RuntimeError::Database);
    }
    Ok(JsonValue::Object(values))
}

fn capture_source_names(table: &str) -> Result<[Identifier; 4], RuntimeError> {
    let digest = hex::encode(Sha256::digest(table.as_bytes()));
    let suffix = &digest[..16];
    Ok([
        Identifier::new(format!("__ffdb_sync_capture_insert_{suffix}"))
            .map_err(|_| RuntimeError::Database)?,
        Identifier::new(format!("__ffdb_sync_capture_update_{suffix}"))
            .map_err(|_| RuntimeError::Database)?,
        Identifier::new(format!("__ffdb_sync_capture_delete_{suffix}"))
            .map_err(|_| RuntimeError::Database)?,
        Identifier::new(format!("__ffdb_sync_capture_guard_{suffix}"))
            .map_err(|_| RuntimeError::Database)?,
    ])
}

fn capture_trigger_sql(
    table: &TableSchema,
    physical: &Identifier,
    names: &[Identifier; 4],
) -> Vec<String> {
    let mut primary_key = table
        .columns
        .iter()
        .filter_map(|column| {
            column
                .primary_key_ordinal
                .map(|ordinal| (ordinal, &column.name))
        })
        .collect::<Vec<_>>();
    primary_key.sort_by_key(|(ordinal, _)| *ordinal);
    let primary_key = primary_key
        .into_iter()
        .map(|(_, name)| name)
        .collect::<Vec<_>>();
    let table_literal = sql_literal(table.name.as_str());
    let new_key = json_object_sql("NEW", &primary_key);
    let old_key = json_object_sql("OLD", &primary_key);
    let new_row = json_object_sql(
        "NEW",
        &table
            .columns
            .iter()
            .map(|column| &column.name)
            .collect::<Vec<_>>(),
    );
    let old_row = json_object_sql(
        "OLD",
        &table
            .columns
            .iter()
            .map(|column| &column.name)
            .collect::<Vec<_>>(),
    );
    let guard_columns = primary_key
        .iter()
        .map(|column| column.quoted())
        .collect::<Vec<_>>()
        .join(", ");
    let guard_when = primary_key
        .iter()
        .map(|column| format!("OLD.{} IS NOT NEW.{}", column.quoted(), column.quoted()))
        .collect::<Vec<_>>()
        .join(" OR ");
    vec![
        capture_one_trigger(
            &names[0],
            physical,
            "AFTER INSERT",
            &table_literal,
            &new_key,
            "insert",
            Some(&new_row),
            None,
            false,
        ),
        capture_one_trigger(
            &names[1],
            physical,
            "AFTER UPDATE",
            &table_literal,
            &new_key,
            "update",
            Some(&new_row),
            None,
            false,
        ),
        capture_one_trigger(
            &names[2],
            physical,
            "AFTER DELETE",
            &table_literal,
            &old_key,
            "delete",
            None,
            Some(&old_row),
            true,
        ),
        format!(
            "CREATE TRIGGER {} BEFORE UPDATE OF {guard_columns} ON {} \
             WHEN {guard_when} BEGIN SELECT RAISE(ABORT, 'primary key updates are not supported'); END",
            names[3].quoted(),
            physical.quoted(),
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn capture_one_trigger(
    name: &Identifier,
    physical: &Identifier,
    timing: &str,
    table_literal: &str,
    key_json: &str,
    operation: &str,
    values_json: Option<&str>,
    tombstone_json: Option<&str>,
    deleted: bool,
) -> String {
    let values_json = values_json.unwrap_or("NULL");
    let tombstone_json = tombstone_json.unwrap_or("NULL");
    let deleted = i64::from(deleted);
    format!(
        "CREATE TRIGGER {} {timing} ON {} BEGIN \
         UPDATE __ffdb_sync_state SET next_sequence=next_sequence+1 WHERE singleton=1; \
         INSERT INTO __ffdb_sync_changes \
         (sequence,transaction_id,table_name,primary_key_json,operation,row_version,values_json,\
          tombstone_json,actor,schema_version,committed_at_ms,client_mutation_id) \
         VALUES ((SELECT next_sequence-1 FROM __ffdb_sync_state WHERE singleton=1), \
          __ffdb_transaction_id(), {table_literal}, {key_json}, '{operation}', \
          COALESCE((SELECT row_version FROM __ffdb_sync_versions \
                    WHERE table_name={table_literal} AND primary_key_json={key_json}),0)+1, \
          {values_json}, {tombstone_json}, __ffdb_actor(), \
          (SELECT schema_version FROM __ffdb_schema_state WHERE singleton=1), \
          CAST((julianday('now')-2440587.5)*86400000 AS INTEGER), \
          __ffdb_client_mutation_id()); \
         INSERT INTO __ffdb_sync_versions \
         (table_name,primary_key_json,row_version,last_sequence,deleted) \
         SELECT {table_literal}, {key_json}, row_version, sequence, {deleted} \
         FROM __ffdb_sync_changes WHERE sequence=(SELECT next_sequence-1 FROM __ffdb_sync_state WHERE singleton=1) \
         ON CONFLICT(table_name,primary_key_json) DO UPDATE SET \
          row_version=excluded.row_version,last_sequence=excluded.last_sequence,deleted=excluded.deleted; \
         END",
        name.quoted(),
        physical.quoted(),
    )
}

fn json_object_sql(alias: &str, columns: &[&Identifier]) -> String {
    let arguments = columns
        .iter()
        .flat_map(|column| {
            [
                sql_literal(column.as_str()),
                format!("json(__ffdb_sync_json({alias}.{}))", column.quoted()),
            ]
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("json_object({arguments})")
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn validate_mutation(mutation: &SyncMutation) -> Result<(), RuntimeError> {
    if mutation.mutation_id.is_empty()
        || mutation.mutation_id.len() > 200
        || mutation.table.is_empty()
        || mutation.table.len() > 128
        || mutation.values.len() > 256
    {
        return Err(RuntimeError::SyncMutationInvalid);
    }
    Ok(())
}

fn key_where(
    primary_key: &[(Identifier, JsonValue)],
) -> Result<(String, Vec<SqlParameter>), RuntimeError> {
    key_where_offset(primary_key, 0)
}

fn key_where_offset(
    primary_key: &[(Identifier, JsonValue)],
    offset: usize,
) -> Result<(String, Vec<SqlParameter>), RuntimeError> {
    let sql = primary_key
        .iter()
        .enumerate()
        .map(|(index, (name, _))| format!("{} IS ?{}", name.quoted(), offset + index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let parameters = primary_key
        .iter()
        .map(|(_, value)| json_parameter(value))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sql, parameters))
}

fn mutation_assignments(
    values: &Map<String, JsonValue>,
    primary_key: &[(Identifier, JsonValue)],
) -> Result<(Vec<String>, Vec<SqlParameter>), RuntimeError> {
    let key_names = primary_key
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut assignments = Vec::new();
    let mut parameters = Vec::new();
    for (name, value) in values {
        let name = Identifier::new(name.clone()).map_err(|_| RuntimeError::SyncMutationInvalid)?;
        if name.is_internal() || key_names.contains(name.as_str()) {
            return Err(RuntimeError::SyncMutationInvalid);
        }
        parameters.push(json_parameter(value)?);
        assignments.push(format!("{} = ?{}", name.quoted(), parameters.len()));
    }
    Ok((assignments, parameters))
}

fn json_parameter(value: &JsonValue) -> Result<SqlParameter, RuntimeError> {
    match value {
        JsonValue::Null => Ok(SqlParameter::Null),
        JsonValue::Bool(value) => Ok(SqlParameter::Integer(i64::from(*value))),
        JsonValue::Number(value) => value
            .as_i64()
            .map(SqlParameter::Integer)
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(SqlParameter::Real)
            })
            .ok_or(RuntimeError::SyncMutationInvalid),
        JsonValue::String(value) => Ok(SqlParameter::Text(value.clone())),
        JsonValue::Object(value) if value.len() == 1 && value.contains_key("$blob") => {
            let encoded = value
                .get("$blob")
                .and_then(JsonValue::as_str)
                .ok_or(RuntimeError::SyncMutationInvalid)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| RuntimeError::SyncMutationInvalid)?;
            Ok(SqlParameter::Blob(bytes))
        }
        JsonValue::Array(_) | JsonValue::Object(_) => Ok(SqlParameter::Text(value.to_string())),
    }
}

fn result_row(
    result: &QueryResult,
    row: &[ResultValue],
) -> Result<Map<String, JsonValue>, RuntimeError> {
    if result.columns.len() != row.len() {
        return Err(RuntimeError::Database);
    }
    result
        .columns
        .iter()
        .zip(row)
        .map(|(column, value)| Ok((column.name.clone(), result_json(value)?)))
        .collect()
}

fn result_json(value: &ResultValue) -> Result<JsonValue, RuntimeError> {
    match value {
        ResultValue::Null => Ok(JsonValue::Null),
        ResultValue::Integer(value) => Ok(JsonValue::from(*value)),
        ResultValue::IntegerString(value) => Ok(JsonValue::String(value.clone())),
        ResultValue::Real(value) => serde_json::Number::from_f64(*value)
            .map(JsonValue::Number)
            .ok_or(RuntimeError::Database),
        ResultValue::Text(value) => Ok(JsonValue::String(value.clone())),
        ResultValue::Blob { data } => Ok(serde_json::json!({"$blob": data})),
    }
}

fn scope_fingerprint(auth: &crate::AuthContext) -> Result<String, RuntimeError> {
    let payload =
        serde_json::to_vec(&(&auth.role, &auth.claims)).map_err(|_| RuntimeError::Database)?;
    Ok(hex::encode(Sha256::digest(payload)))
}

fn epoch_ms() -> Result<i64, RuntimeError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeError::Database)?;
    i64::try_from(duration.as_millis()).map_err(|_| RuntimeError::Database)
}
