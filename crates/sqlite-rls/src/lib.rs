//! Compiles normalized RLS metadata into SQLite views and generated triggers.
//!
//! The compiler always quotes identifiers and only accepts predicates validated
//! by `ffdb-sql-parser`. Its output still requires the runtime authorizer: views
//! and triggers enforce row predicates while the authorizer protects backing and
//! metadata objects from direct access.

use std::collections::{BTreeMap, btree_map::Entry};

use ffdb_sql_parser::{
    AlterTableRls, CreatePolicy, Identifier, PolicyCommand, PolicyMode, Predicate, RlsStatement,
    RoleName,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const INTERNAL_PREFIX: &str = "__ffdb_";

pub fn backing_table_name(table: &Identifier) -> Result<Identifier, RlsCompileError> {
    generated_identifier("data", table)
}

pub fn generated_source_names(table: &Identifier) -> Result<Vec<Identifier>, RlsCompileError> {
    Ok(vec![
        table.clone(),
        generated_identifier("insert", table)?,
        generated_identifier("update", table)?,
        generated_identifier("delete", table)?,
    ])
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub tables: Vec<TableSchema>,
}

impl SchemaSnapshot {
    #[must_use]
    pub fn table(&self, name: &Identifier) -> Option<&TableSchema> {
        self.tables.iter().find(|table| table.name == *name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: Identifier,
    pub columns: Vec<ColumnSchema>,
    /// True once the logical table has been converted to a public view and an
    /// internal physical table.
    pub protected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: Identifier,
    pub primary_key_ordinal: Option<u32>,
    pub generated: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RlsCatalog {
    tables: BTreeMap<Identifier, TablePolicies>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TablePolicies {
    pub enabled: bool,
    pub forced: bool,
    pub policies: BTreeMap<Identifier, Policy>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub name: Identifier,
    pub mode: PolicyMode,
    pub command: PolicyCommand,
    pub roles: Vec<RoleName>,
    pub using: Option<Predicate>,
    pub with_check: Option<Predicate>,
}

impl From<CreatePolicy> for Policy {
    fn from(policy: CreatePolicy) -> Self {
        Self {
            name: policy.name,
            mode: policy.mode,
            command: policy.command,
            roles: policy.roles,
            using: policy.using,
            with_check: policy.with_check,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CombinedPredicates {
    pub using_sql: Option<String>,
    pub check_sql: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledRlsPlan {
    tables: Vec<CompiledTablePlan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledTablePlan {
    table: Identifier,
    backing_table: Identifier,
    /// Exact view/trigger names accepted as trusted authorizer origins.
    generated_sources: Vec<Identifier>,
    /// Present only when an ordinary table must first be renamed to its backing name.
    rename_sql: Option<String>,
    drop_generated_sql: Vec<String>,
    create_generated_sql: Vec<String>,
}

impl CompiledRlsPlan {
    #[must_use]
    pub fn tables(&self) -> &[CompiledTablePlan] {
        &self.tables
    }
}

impl CompiledTablePlan {
    #[must_use]
    pub fn table(&self) -> &Identifier {
        &self.table
    }

    #[must_use]
    pub fn backing_table(&self) -> &Identifier {
        &self.backing_table
    }

    #[must_use]
    pub fn generated_sources(&self) -> &[Identifier] {
        &self.generated_sources
    }

    #[must_use]
    pub fn rename_sql(&self) -> Option<&str> {
        self.rename_sql.as_deref()
    }

    #[must_use]
    pub fn drop_generated_sql(&self) -> &[String] {
        &self.drop_generated_sql
    }

    #[must_use]
    pub fn create_generated_sql(&self) -> &[String] {
        &self.create_generated_sql
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RlsCompileError {
    #[error("table {0} does not exist in the schema snapshot")]
    UnknownTable(String),
    #[error("policy {policy} already exists on table {table}")]
    DuplicatePolicy { table: String, policy: String },
    #[error("policy {policy} does not exist on table {table}")]
    UnknownPolicy { table: String, policy: String },
    #[error("RLS-protected table {0} must have a primary key")]
    MissingPrimaryKey(String),
    #[error("RLS-protected table {0} must have at least one column")]
    MissingColumns(String),
    #[error("generated internal identifier is invalid")]
    InternalIdentifier,
    #[error(
        "ALTER POLICY cannot change a policy command; {0} is incompatible with the stored command"
    )]
    IncompatibleAlter(&'static str),
}

impl RlsCatalog {
    #[must_use]
    pub fn tables(&self) -> &BTreeMap<Identifier, TablePolicies> {
        &self.tables
    }

    pub fn apply(
        &mut self,
        schema: &SchemaSnapshot,
        statement: RlsStatement,
    ) -> Result<(), RlsCompileError> {
        match statement {
            RlsStatement::AlterTable { table, action } => {
                ensure_table(schema, &table)?;
                let metadata = self.tables.entry(table).or_default();
                match action {
                    AlterTableRls::Enable => metadata.enabled = true,
                    AlterTableRls::Disable => metadata.enabled = false,
                    AlterTableRls::Force => metadata.forced = true,
                    AlterTableRls::NoForce => metadata.forced = false,
                }
            }
            RlsStatement::CreatePolicy(statement) => {
                ensure_table(schema, &statement.table)?;
                let table_name = statement.table.clone();
                let metadata = self.tables.entry(table_name.clone()).or_default();
                match metadata.policies.entry(statement.name.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(statement.into());
                    }
                    Entry::Occupied(_) => {
                        return Err(RlsCompileError::DuplicatePolicy {
                            table: table_name.as_str().to_owned(),
                            policy: statement.name.as_str().to_owned(),
                        });
                    }
                }
            }
            RlsStatement::AlterPolicy(statement) => {
                ensure_table(schema, &statement.table)?;
                let table_name = statement.table.as_str().to_owned();
                let metadata = self.tables.entry(statement.table.clone()).or_default();
                let mut policy = metadata.policies.remove(&statement.name).ok_or_else(|| {
                    RlsCompileError::UnknownPolicy {
                        table: table_name.clone(),
                        policy: statement.name.as_str().to_owned(),
                    }
                })?;
                if statement.using.is_some() && policy.command == PolicyCommand::Insert {
                    return Err(RlsCompileError::IncompatibleAlter("USING"));
                }
                if statement.with_check.is_some()
                    && matches!(
                        policy.command,
                        PolicyCommand::Select | PolicyCommand::Delete
                    )
                {
                    return Err(RlsCompileError::IncompatibleAlter("WITH CHECK"));
                }
                if let Some(roles) = statement.roles {
                    policy.roles = roles;
                }
                if let Some(using) = statement.using {
                    policy.using = Some(using);
                }
                if let Some(check) = statement.with_check {
                    policy.with_check = Some(check);
                }
                let new_name = statement.rename_to.unwrap_or_else(|| policy.name.clone());
                policy.name = new_name.clone();
                if metadata.policies.insert(new_name.clone(), policy).is_some() {
                    return Err(RlsCompileError::DuplicatePolicy {
                        table: table_name,
                        policy: new_name.as_str().to_owned(),
                    });
                }
            }
            RlsStatement::DropPolicy(statement) => {
                ensure_table(schema, &statement.table)?;
                let removed = self
                    .tables
                    .entry(statement.table.clone())
                    .or_default()
                    .policies
                    .remove(&statement.name);
                if removed.is_none() && !statement.if_exists {
                    return Err(RlsCompileError::UnknownPolicy {
                        table: statement.table.as_str().to_owned(),
                        policy: statement.name.as_str().to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn combined_predicates(
        &self,
        table: &Identifier,
        operation: Operation,
    ) -> Result<CombinedPredicates, RlsCompileError> {
        let metadata = self.tables.get(table).cloned().unwrap_or_default();
        if !metadata.enabled {
            return Ok(CombinedPredicates {
                using_sql: operation_uses_existing(operation).then(|| "1".to_owned()),
                check_sql: operation_checks_new(operation).then(|| "1".to_owned()),
            });
        }

        let using_sql = operation_uses_existing(operation).then(|| {
            combine(
                metadata
                    .policies
                    .values()
                    .filter(|policy| applies(policy.command, operation)),
                PredicatePurpose::Using,
                metadata.forced,
            )
        });
        let check_sql = operation_checks_new(operation).then(|| {
            combine(
                metadata
                    .policies
                    .values()
                    .filter(|policy| applies(policy.command, operation)),
                PredicatePurpose::Check,
                metadata.forced,
            )
        });
        Ok(CombinedPredicates {
            using_sql,
            check_sql,
        })
    }
}

#[derive(Clone, Copy)]
enum PredicatePurpose {
    Using,
    Check,
}

fn combine<'a>(
    policies: impl Iterator<Item = &'a Policy>,
    purpose: PredicatePurpose,
    forced: bool,
) -> String {
    let mut permissive = Vec::new();
    let mut restrictive = Vec::new();
    for policy in policies {
        let guard = role_guard(&policy.roles);
        let predicate = match purpose {
            PredicatePurpose::Using => policy
                .using
                .as_ref()
                .map_or_else(|| "1".to_owned(), Predicate::sqlite_sql),
            PredicatePurpose::Check => policy
                .with_check
                .as_ref()
                .or(policy.using.as_ref())
                .map_or_else(|| "1".to_owned(), Predicate::sqlite_sql),
        };
        match policy.mode {
            PolicyMode::Permissive => permissive.push(format!("(({guard}) AND ({predicate}))")),
            PolicyMode::Restrictive => {
                restrictive.push(format!("((NOT ({guard})) OR ({predicate}))"));
            }
        }
    }
    let permissive = if permissive.is_empty() {
        "0".to_owned()
    } else {
        permissive.join(" OR ")
    };
    let restrictive = if restrictive.is_empty() {
        "1".to_owned()
    } else {
        restrictive.join(" AND ")
    };
    let policy_result = format!("(({permissive}) AND ({restrictive}))");
    if forced {
        policy_result
    } else {
        format!("(__ffdb_is_developer() OR {policy_result})")
    }
}

fn role_guard(roles: &[RoleName]) -> String {
    if roles
        .iter()
        .any(|role| role.as_str().eq_ignore_ascii_case("public"))
    {
        return "1".to_owned();
    }
    roles
        .iter()
        .map(|role| format!("__ffdb_auth_role() = {}", quote_literal(role.as_str())))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn applies(command: PolicyCommand, operation: Operation) -> bool {
    command == PolicyCommand::All
        || matches!(
            (command, operation),
            (PolicyCommand::Select, Operation::Select)
                | (PolicyCommand::Insert, Operation::Insert)
                | (PolicyCommand::Update, Operation::Update)
                | (PolicyCommand::Delete, Operation::Delete)
        )
}

const fn operation_uses_existing(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::Select | Operation::Update | Operation::Delete
    )
}

const fn operation_checks_new(operation: Operation) -> bool {
    matches!(operation, Operation::Insert | Operation::Update)
}

#[derive(Clone, Debug, Default)]
pub struct Compiler;

impl Compiler {
    pub fn compile(
        &self,
        schema: &SchemaSnapshot,
        catalog: &RlsCatalog,
    ) -> Result<CompiledRlsPlan, RlsCompileError> {
        let mut tables = Vec::new();
        for (table_name, metadata) in catalog.tables() {
            let table = schema
                .table(table_name)
                .ok_or_else(|| RlsCompileError::UnknownTable(table_name.as_str().to_owned()))?;
            if !metadata.enabled && !table.protected {
                continue;
            }
            tables.push(compile_table(table, catalog)?);
        }
        Ok(CompiledRlsPlan { tables })
    }
}

fn compile_table(
    table: &TableSchema,
    catalog: &RlsCatalog,
) -> Result<CompiledTablePlan, RlsCompileError> {
    if table.columns.is_empty() {
        return Err(RlsCompileError::MissingColumns(
            table.name.as_str().to_owned(),
        ));
    }
    let primary_key: Vec<_> = table
        .columns
        .iter()
        .filter(|column| column.primary_key_ordinal.is_some())
        .collect();
    if primary_key.is_empty() {
        return Err(RlsCompileError::MissingPrimaryKey(
            table.name.as_str().to_owned(),
        ));
    }
    let backing_table = backing_identifier(&table.name)?;
    let trigger_insert = generated_identifier("insert", &table.name)?;
    let trigger_update = generated_identifier("update", &table.name)?;
    let trigger_delete = generated_identifier("delete", &table.name)?;
    let columns = table
        .columns
        .iter()
        .map(|column| column.name.quoted())
        .collect::<Vec<_>>()
        .join(", ");
    let select = catalog.combined_predicates(&table.name, Operation::Select)?;
    let insert = catalog.combined_predicates(&table.name, Operation::Insert)?;
    let update = catalog.combined_predicates(&table.name, Operation::Update)?;
    let delete = catalog.combined_predicates(&table.name, Operation::Delete)?;
    let table_alias = table.name.quoted();
    let view_sql = format!(
        "CREATE VIEW {} AS SELECT {} FROM {} AS {} WHERE ({})",
        table.name.quoted(),
        columns,
        backing_table.quoted(),
        table_alias,
        select.using_sql.as_deref().unwrap_or("0")
    );

    let new_candidate = candidate_row(table, "NEW");
    let old_candidate = candidate_row(table, "OLD");
    let writable: Vec<_> = table
        .columns
        .iter()
        .filter(|column| !column.generated)
        .collect();
    let writable_names = writable
        .iter()
        .map(|column| column.name.quoted())
        .collect::<Vec<_>>()
        .join(", ");
    let new_values = writable
        .iter()
        .map(|column| format!("NEW.{}", column.name.quoted()))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_check = insert.check_sql.as_deref().unwrap_or("0");
    let insert_sql = format!(
        "CREATE TRIGGER {} INSTEAD OF INSERT ON {} BEGIN \
         SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM (SELECT {}) AS {} WHERE ({})) \
         THEN RAISE(ABORT, 'rls.with_check_violation') END; \
         INSERT INTO {} ({}) VALUES ({}); END",
        trigger_insert.quoted(),
        table.name.quoted(),
        new_candidate,
        table_alias,
        insert_check,
        backing_table.quoted(),
        writable_names,
        new_values
    );
    let assignments = writable
        .iter()
        .map(|column| format!("{} = NEW.{}", column.name.quoted(), column.name.quoted()))
        .collect::<Vec<_>>()
        .join(", ");
    let key_match = primary_key
        .iter()
        .map(|column| format!("{} IS OLD.{}", column.name.quoted(), column.name.quoted()))
        .collect::<Vec<_>>()
        .join(" AND ");
    let update_check = update.check_sql.as_deref().unwrap_or("0");
    let update_using = update.using_sql.as_deref().unwrap_or("0");
    let update_sql = format!(
        "CREATE TRIGGER {} INSTEAD OF UPDATE ON {} BEGIN \
         SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM (SELECT {}) AS {} WHERE ({})) \
         THEN RAISE(ABORT, 'rls.using_violation') END; \
         SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM (SELECT {}) AS {} WHERE ({})) \
         THEN RAISE(ABORT, 'rls.with_check_violation') END; \
         UPDATE {} SET {} WHERE {}; END",
        trigger_update.quoted(),
        table.name.quoted(),
        old_candidate,
        table_alias,
        update_using,
        new_candidate,
        table_alias,
        update_check,
        backing_table.quoted(),
        assignments,
        key_match
    );
    let delete_using = delete.using_sql.as_deref().unwrap_or("0");
    let delete_sql = format!(
        "CREATE TRIGGER {} INSTEAD OF DELETE ON {} BEGIN \
         SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM (SELECT {}) AS {} WHERE ({})) \
         THEN RAISE(ABORT, 'rls.using_violation') END; \
         DELETE FROM {} WHERE {}; END",
        trigger_delete.quoted(),
        table.name.quoted(),
        old_candidate,
        table_alias,
        delete_using,
        backing_table.quoted(),
        key_match
    );

    Ok(CompiledTablePlan {
        table: table.name.clone(),
        backing_table: backing_table.clone(),
        generated_sources: generated_source_names(&table.name)?,
        rename_sql: (!table.protected).then(|| {
            format!(
                "ALTER TABLE {} RENAME TO {}",
                table.name.quoted(),
                backing_table.quoted()
            )
        }),
        drop_generated_sql: vec![
            format!("DROP TRIGGER IF EXISTS {}", trigger_insert.quoted()),
            format!("DROP TRIGGER IF EXISTS {}", trigger_update.quoted()),
            format!("DROP TRIGGER IF EXISTS {}", trigger_delete.quoted()),
            format!("DROP VIEW IF EXISTS {}", table.name.quoted()),
        ],
        create_generated_sql: vec![view_sql, insert_sql, update_sql, delete_sql],
    })
}

fn candidate_row(table: &TableSchema, row_alias: &str) -> String {
    table
        .columns
        .iter()
        .map(|column| {
            format!(
                "{row_alias}.{} AS {}",
                column.name.quoted(),
                column.name.quoted()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn backing_identifier(table: &Identifier) -> Result<Identifier, RlsCompileError> {
    backing_table_name(table)
}

fn generated_identifier(kind: &str, table: &Identifier) -> Result<Identifier, RlsCompileError> {
    let encoded = Sha256::digest(table.as_str().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Identifier::new(format!("__ffdb_{kind}_{encoded}"))
        .map_err(|_| RlsCompileError::InternalIdentifier)
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn ensure_table(schema: &SchemaSnapshot, table: &Identifier) -> Result<(), RlsCompileError> {
    schema
        .table(table)
        .map(|_| ())
        .ok_or_else(|| RlsCompileError::UnknownTable(table.as_str().to_owned()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use ffdb_sql_parser::{CreatePolicy, parse_rls_statement};
    use proptest::prelude::*;
    use rusqlite::{Connection, functions::FunctionFlags};

    use super::*;

    fn schema(protected: bool) -> SchemaSnapshot {
        SchemaSnapshot {
            tables: vec![TableSchema {
                name: Identifier::new("documents").unwrap(),
                columns: vec![
                    ColumnSchema {
                        name: Identifier::new("tenant_id").unwrap(),
                        primary_key_ordinal: Some(1),
                        generated: false,
                    },
                    ColumnSchema {
                        name: Identifier::new("payload").unwrap(),
                        primary_key_ordinal: None,
                        generated: false,
                    },
                ],
                protected,
            }],
        }
    }

    fn apply(catalog: &mut RlsCatalog, sql: &str) {
        catalog
            .apply(&schema(false), parse_rls_statement(sql).unwrap())
            .unwrap();
    }

    #[test]
    fn default_deny_without_a_permissive_policy() {
        let mut catalog = RlsCatalog::default();
        apply(
            &mut catalog,
            "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
        );
        apply(
            &mut catalog,
            "CREATE POLICY limit_rows ON documents AS RESTRICTIVE FOR SELECT USING (tenant_id > 0)",
        );
        let sql = catalog
            .combined_predicates(&Identifier::new("documents").unwrap(), Operation::Select)
            .unwrap()
            .using_sql
            .unwrap();
        assert!(sql.contains("((0) AND"));
    }

    #[test]
    fn combines_permissive_or_and_restrictive_and_with_role_guards() {
        let mut catalog = RlsCatalog::default();
        apply(
            &mut catalog,
            "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
        );
        apply(
            &mut catalog,
            "CREATE POLICY own ON documents FOR SELECT TO authenticated USING (tenant_id = auth.uid())",
        );
        apply(
            &mut catalog,
            "CREATE POLICY positive ON documents AS RESTRICTIVE FOR SELECT TO authenticated USING (tenant_id > 0)",
        );
        let sql = catalog
            .combined_predicates(&Identifier::new("documents").unwrap(), Operation::Select)
            .unwrap()
            .using_sql
            .unwrap();
        assert!(sql.contains(" OR ") || sql.contains("__ffdb_is_developer() OR"));
        assert!(sql.contains(" AND "));
        assert!(sql.contains("__ffdb_auth_role() = 'authenticated'"));
        assert!(sql.contains("__ffdb_auth_uid()"));
    }

    #[test]
    fn force_removes_developer_bypass() {
        let mut catalog = RlsCatalog::default();
        apply(
            &mut catalog,
            "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
        );
        apply(
            &mut catalog,
            "ALTER TABLE documents FORCE ROW LEVEL SECURITY",
        );
        let sql = catalog
            .combined_predicates(&Identifier::new("documents").unwrap(), Operation::Select)
            .unwrap()
            .using_sql
            .unwrap();
        assert!(!sql.contains("__ffdb_is_developer"));
    }

    #[test]
    fn compiler_renames_and_generates_view_and_write_triggers() {
        let mut catalog = RlsCatalog::default();
        apply(
            &mut catalog,
            "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
        );
        apply(
            &mut catalog,
            "CREATE POLICY own ON documents FOR ALL USING (tenant_id = auth.uid()) WITH CHECK (tenant_id = auth.uid())",
        );
        let plan = Compiler.compile(&schema(false), &catalog).unwrap();
        let table = &plan.tables[0];
        assert!(table.rename_sql.as_ref().unwrap().contains("__ffdb_data_"));
        assert_eq!(table.create_generated_sql.len(), 4);
        assert!(table.create_generated_sql[0].contains("CREATE VIEW"));
        assert!(table.create_generated_sql[1].contains("rls.with_check_violation"));
    }

    #[test]
    fn rejects_duplicate_policy_without_mutating_existing() {
        let mut catalog = RlsCatalog::default();
        let statement =
            parse_rls_statement("CREATE POLICY own ON documents FOR SELECT USING (tenant_id = 1)")
                .unwrap();
        catalog.apply(&schema(false), statement.clone()).unwrap();
        assert!(matches!(
            catalog.apply(&schema(false), statement),
            Err(RlsCompileError::DuplicatePolicy { .. })
        ));
        let policies = &catalog.tables().values().next().unwrap().policies;
        assert_eq!(policies.len(), 1);
        let _: &Policy = policies.values().next().unwrap();
        let _: Option<&CreatePolicy> = None;
    }

    #[test]
    fn generated_views_and_triggers_are_accepted_and_enforced_by_sqlite() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .create_scalar_function(
                "__ffdb_auth_uid",
                0,
                FunctionFlags::SQLITE_DETERMINISTIC,
                |_| Ok("alice".to_owned()),
            )
            .unwrap();
        connection
            .create_scalar_function(
                "__ffdb_auth_role",
                0,
                FunctionFlags::SQLITE_DETERMINISTIC,
                |_| Ok("authenticated".to_owned()),
            )
            .unwrap();
        connection
            .create_scalar_function(
                "__ffdb_is_developer",
                0,
                FunctionFlags::SQLITE_DETERMINISTIC,
                |_| Ok(0_i64),
            )
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE documents(tenant_id TEXT PRIMARY KEY, payload TEXT NOT NULL)",
            )
            .unwrap();

        let mut catalog = RlsCatalog::default();
        apply(
            &mut catalog,
            "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
        );
        apply(
            &mut catalog,
            "CREATE POLICY own ON documents FOR ALL TO authenticated \
             USING (tenant_id = auth.uid()) WITH CHECK (tenant_id = auth.uid())",
        );
        let plan = Compiler.compile(&schema(false), &catalog).unwrap();
        let table = &plan.tables()[0];
        connection
            .execute_batch(table.rename_sql().unwrap())
            .unwrap();
        for statement in table.drop_generated_sql() {
            connection.execute_batch(statement).unwrap();
        }
        for statement in table.create_generated_sql() {
            connection.execute_batch(statement).unwrap();
        }

        connection
            .execute(
                "INSERT INTO documents(tenant_id,payload) VALUES ('alice','visible')",
                [],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "INSERT INTO documents(tenant_id,payload) VALUES ('bob','hidden')",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE documents SET tenant_id='bob' WHERE tenant_id='alice'",
                    []
                )
                .is_err()
        );
        let visible: Vec<String> = connection
            .prepare("SELECT payload FROM documents ORDER BY tenant_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(visible, ["visible"]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn compiled_policy_algebra_matches_boolean_reference_model(
            permissive in prop::collection::vec(any::<bool>(), 0..6),
            restrictive in prop::collection::vec(any::<bool>(), 0..6),
        ) {
            let mut catalog = RlsCatalog::default();
            apply(&mut catalog, "ALTER TABLE documents ENABLE ROW LEVEL SECURITY");
            apply(&mut catalog, "ALTER TABLE documents FORCE ROW LEVEL SECURITY");
            for (index, value) in permissive.iter().enumerate() {
                apply(
                    &mut catalog,
                    &format!(
                        "CREATE POLICY p{index} ON documents AS PERMISSIVE FOR SELECT USING ({})",
                        i32::from(*value)
                    ),
                );
            }
            for (index, value) in restrictive.iter().enumerate() {
                apply(
                    &mut catalog,
                    &format!(
                        "CREATE POLICY r{index} ON documents AS RESTRICTIVE FOR SELECT USING ({})",
                        i32::from(*value)
                    ),
                );
            }
            let expression = catalog
                .combined_predicates(
                    &Identifier::new("documents").unwrap(),
                    Operation::Select,
                )
                .unwrap()
                .using_sql
                .unwrap();
            let actual: bool = Connection::open_in_memory()
                .unwrap()
                .query_row(&format!("SELECT ({expression})"), [], |row| row.get(0))
                .unwrap();
            let expected = permissive.iter().any(|value| *value)
                && restrictive.iter().all(|value| *value);
            prop_assert_eq!(actual, expected);
        }

        #[test]
        fn generated_names_are_stable_distinct_and_internal(
            left in "[a-z][a-z0-9_]{0,40}",
            right in "[a-z][a-z0-9_]{0,40}",
        ) {
            let left = Identifier::new(left).unwrap();
            let right = Identifier::new(right).unwrap();
            let left_backing = backing_table_name(&left).unwrap();
            prop_assert_eq!(left_backing.clone(), backing_table_name(&left).unwrap());
            prop_assert!(left_backing.is_internal());
            let sources = generated_source_names(&left).unwrap();
            prop_assert_eq!(sources.len(), 4);
            for (index, source) in sources.iter().enumerate() {
                prop_assert!(sources.iter().skip(index + 1).all(|other| other != source));
            }
            if left != right {
                prop_assert_ne!(left_backing, backing_table_name(&right).unwrap());
            }
        }
    }
}
