//! Conservative SQL inspection and the custom PostgreSQL-style RLS grammar.
//!
//! This crate is deliberately not the sole security boundary. Classification is
//! a fail-closed first gate; SQLite preparation and an authorizer are authoritative.

mod classifier;
mod identifier;
mod rls;
mod scanner;

pub use classifier::{ClassificationError, StatementClass, StatementKind, classify_statement};
pub use identifier::{Identifier, IdentifierError};
pub use rls::{
    AlterPolicy, AlterTableRls, CreatePolicy, DropPolicy, PolicyCommand, PolicyMode, Predicate,
    RlsParseError, RlsStatement, RoleName, parse_rls, parse_rls_statement,
    rewrite_auth_functions_for_execution,
};
pub use scanner::{SqlScanError, split_sql_statements};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The result of conservatively classifying one statement and, for the custom
/// RLS grammar, parsing it into typed metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ParsedStatement {
    Sql(StatementClass),
    Rls(RlsStatement),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StatementParseError {
    #[error(transparent)]
    Classification(#[from] ClassificationError),
    #[error(transparent)]
    Rls(#[from] RlsParseError),
}

/// A narrow, total entrypoint for untrusted UTF-8. It requires exactly one SQL
/// statement, classifies ordinary SQLite syntax, and fully parses custom RLS
/// statements. SQLite preparation and the runtime authorizer remain the
/// authoritative execution boundary.
pub fn parse_and_classify_statement(sql: &str) -> Result<ParsedStatement, StatementParseError> {
    let class = classify_statement(sql)?;
    if class.kind == StatementKind::Rls {
        parse_rls_statement(sql)
            .map(ParsedStatement::Rls)
            .map_err(Into::into)
    } else {
        Ok(ParsedStatement::Sql(class))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn public_entrypoint_is_total_and_deterministic_for_arbitrary_utf8(input in any::<String>()) {
            let first = parse_and_classify_statement(&input);
            let second = parse_and_classify_statement(&input);
            prop_assert_eq!(first, second);
        }

        #[test]
        fn quoted_semicolons_never_create_extra_statements(payload in "[a-zA-Z0-9 ;/_-]{0,96}") {
            let sql = format!("SELECT '{payload}' AS payload");
            let statements = split_sql_statements(&sql).unwrap();
            prop_assert_eq!(statements.len(), 1);
            let is_select = matches!(
                parse_and_classify_statement(&sql).unwrap(),
                ParsedStatement::Sql(StatementClass { kind: StatementKind::Select, read_only: true })
            );
            prop_assert!(is_select);
        }
    }

    #[test]
    fn rls_classification_always_routes_through_the_typed_parser() {
        let parsed = parse_and_classify_statement(
            "CREATE POLICY own ON documents FOR SELECT USING (owner_id = auth.uid())",
        )
        .unwrap();
        assert!(matches!(
            parsed,
            ParsedStatement::Rls(RlsStatement::CreatePolicy(_))
        ));

        let error = parse_and_classify_statement("CREATE POLICY incomplete").unwrap_err();
        assert!(matches!(error, StatementParseError::Rls(_)));
    }
}
