#![no_main]

use std::path::Path;

use ffdb_sql_parser::{StatementKind, classify_statement, parse_rls_statement};
use ffdb_sqlite_rls::Compiler;
use ffdb_sqlite_runtime::{
    AuthContext, CancellationToken, Database, DeveloperPrincipal, ExecutionMode, QueryResult,
    ResultValue, RuntimeConfig, SqlParameter, StatementRequest, TrustedDatabasePath,
};
use libfuzzer_sys::fuzz_target;
use serde_json::Map;

const SEEDED_DATABASE_ID: &str = "01965555-0000-7000-8000-000000000099";
const CONTROL_DATABASE_ID: &str = "01965555-0000-7000-8000-000000000100";

fn request(sql: &str) -> StatementRequest {
    StatementRequest {
        sql: sql.to_owned(),
        parameters: Vec::<SqlParameter>::new(),
    }
}

fn developer() -> ExecutionMode {
    ExecutionMode::Developer(DeveloperPrincipal {
        actor_id: "fuzzer".to_owned(),
        api_key_id: "fuzzer-key".to_owned(),
    })
}

fn user() -> ExecutionMode {
    ExecutionMode::EndUser(AuthContext {
        project_id: "fuzz-project".to_owned(),
        subject: "bob".to_owned(),
        role: "authenticated".to_owned(),
        claims: Map::new(),
        token_id: "fuzz-token".to_owned(),
    })
}

fn database(root: &Path, database_id: &str, seed_alice: bool) -> Database {
    let path = TrustedDatabasePath::for_database(root, database_id).expect("trusted path");
    let database = Database::open(path, RuntimeConfig::default()).expect("database");
    let cancellation = CancellationToken::default();
    database
        .with_context(developer(), &cancellation, |session| {
            session.execute(&request(
                "CREATE TABLE documents(id INTEGER PRIMARY KEY, owner_id TEXT NOT NULL, body TEXT NOT NULL)",
            ))?;
            let mut catalog = session.load_rls_catalog()?;
            for policy in [
                "ALTER TABLE documents ENABLE ROW LEVEL SECURITY",
                "CREATE POLICY documents_owner ON documents FOR ALL TO authenticated USING (owner_id = auth.uid()) WITH CHECK (owner_id = auth.uid())",
            ] {
                let schema = session.schema_snapshot(&catalog)?;
                catalog
                    .apply(
                        &schema,
                        parse_rls_statement(policy)
                            .map_err(|_| ffdb_sqlite_runtime::RuntimeError::Database)?,
                    )
                    .map_err(|_| ffdb_sqlite_runtime::RuntimeError::Database)?;
            }
            let schema = session.schema_snapshot(&catalog)?;
            let plan = Compiler
                .compile(&schema, &catalog)
                .map_err(|_| ffdb_sqlite_runtime::RuntimeError::Database)?;
            session.apply_rls_plan(&plan)?;
            session.store_rls_catalog(&catalog)?;
            if seed_alice {
                session.execute(&request(
                    "INSERT INTO documents(id, owner_id, body) \
                     VALUES (1, 'alice', 'ffdb-fuzz-secret')",
                ))?;
            }
            Ok(())
        })
        .expect("seeded RLS database");
    database
}

fn alice_row_is_intact(database: &Database, cancellation: &CancellationToken) -> bool {
    let invariant = database
        .with_context(developer(), cancellation, |session| {
            session.execute(&request(
                "SELECT count(*) FROM documents \
                 WHERE id=1 AND owner_id='alice' AND body='ffdb-fuzz-secret'",
            ))
        })
        .expect("invariant query");
    matches!(
        invariant.rows.as_slice(),
        [row] if matches!(row.as_slice(), [ResultValue::Integer(1)])
    )
}

fn normalize_select_metadata(result: &mut QueryResult) {
    // SQLite's changes()/last_insert_rowid() are connection history, not SELECT
    // output. The runtime currently includes that history in QueryResult, so it
    // must not make the seeded/control data comparison report a row-policy leak.
    result.affected_rows = 0;
    result.last_insert_rowid = None;
}

fuzz_target!(|bytes: &[u8]| {
    let Ok(input) = std::str::from_utf8(bytes) else {
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let seeded = database(directory.path(), SEEDED_DATABASE_ID, true);
    let cancellation = CancellationToken::default();

    let mut response = seeded.with_context(user(), &cancellation, |session| {
        session.execute(&request(input))
    });

    if classify_statement(input).is_ok_and(|class| class.kind == StatementKind::Select) {
        // A SELECT issued by Bob must have the same observable result whether or
        // not the otherwise-identical database contains Alice's protected row.
        // This avoids false positives when the input itself contains the canary
        // and catches projections/aggregates that transform the protected value.
        let control = database(directory.path(), CONTROL_DATABASE_ID, false);
        let mut control_response = control.with_context(user(), &cancellation, |session| {
            session.execute(&request(input))
        });
        if let Ok(result) = &mut response {
            normalize_select_metadata(result);
        }
        if let Ok(result) = &mut control_response {
            normalize_select_metadata(result);
        }
        assert_eq!(
            response, control_response,
            "end-user SELECT was influenced by another user's protected row"
        );
    }

    assert!(
        alice_row_is_intact(&seeded, &cancellation),
        "hostile SQL modified another user's protected row"
    );
});
