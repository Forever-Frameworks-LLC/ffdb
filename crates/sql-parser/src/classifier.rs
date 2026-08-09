use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::scanner::{
    SqlScanError, is_word_continue, is_word_start, skip_block_comment, skip_quoted,
};
use crate::split_sql_statements;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
    Ddl,
    Pragma,
    Attach,
    Detach,
    Vacuum,
    TransactionControl,
    Rls,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatementClass {
    pub kind: StatementKind,
    pub read_only: bool,
}

impl StatementClass {
    #[must_use]
    pub const fn allowed_for_end_user(&self) -> bool {
        matches!(
            self.kind,
            StatementKind::Select
                | StatementKind::Insert
                | StatementKind::Update
                | StatementKind::Delete
        )
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ClassificationError {
    #[error("expected exactly one SQL statement")]
    ExpectedOneStatement,
    #[error("could not safely classify the statement")]
    Ambiguous,
    #[error(transparent)]
    Scan(#[from] SqlScanError),
}

pub fn classify_statement(sql: &str) -> Result<StatementClass, ClassificationError> {
    let statements = split_sql_statements(sql)?;
    if statements.len() != 1 {
        return Err(ClassificationError::ExpectedOneStatement);
    }
    let tokens = top_level_words(statements[0])?;
    let first = tokens
        .first()
        .ok_or(ClassificationError::Ambiguous)?
        .as_str();
    let kind = match first {
        "SELECT" => StatementKind::Select,
        "INSERT" | "REPLACE" => StatementKind::Insert,
        "UPDATE" => StatementKind::Update,
        "DELETE" => StatementKind::Delete,
        "WITH" => classify_with(&tokens)?,
        "CREATE" | "DROP" => {
            if tokens.get(1).is_some_and(|word| word == "POLICY") {
                StatementKind::Rls
            } else {
                StatementKind::Ddl
            }
        }
        "ALTER" => {
            if tokens
                .get(1)
                .is_some_and(|word| matches!(word.as_str(), "POLICY" | "TABLE"))
                && tokens
                    .iter()
                    .any(|word| matches!(word.as_str(), "POLICY" | "SECURITY"))
            {
                StatementKind::Rls
            } else {
                StatementKind::Ddl
            }
        }
        "PRAGMA" => StatementKind::Pragma,
        "ATTACH" => StatementKind::Attach,
        "DETACH" => StatementKind::Detach,
        "VACUUM" => StatementKind::Vacuum,
        "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => {
            StatementKind::TransactionControl
        }
        _ => StatementKind::Other,
    };
    Ok(StatementClass {
        kind,
        read_only: kind == StatementKind::Select,
    })
}

fn classify_with(words: &[String]) -> Result<StatementKind, ClassificationError> {
    // The scanner emits only depth-zero words. Therefore the last top-level DML
    // keyword is the body after all parenthesized CTE definitions.
    words
        .iter()
        .skip(1)
        .rev()
        .find_map(|word| match word.as_str() {
            "SELECT" => Some(StatementKind::Select),
            "INSERT" | "REPLACE" => Some(StatementKind::Insert),
            "UPDATE" => Some(StatementKind::Update),
            "DELETE" => Some(StatementKind::Delete),
            _ => None,
        })
        .ok_or(ClassificationError::Ambiguous)
}

fn top_level_words(sql: &str) -> Result<Vec<String>, SqlScanError> {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut depth = 0_u32;
    let mut words = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b'\'' => index = skip_quoted(bytes, index, b'\'', SqlScanError::UnterminatedString)?,
            b'"' | b'`' => {
                index = skip_quoted(
                    bytes,
                    index,
                    bytes[index],
                    SqlScanError::UnterminatedIdentifier,
                )?;
            }
            b'[' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b']' {
                    index += 1;
                }
                if index == bytes.len() {
                    return Err(SqlScanError::UnterminatedIdentifier);
                }
                index += 1;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index)?
            }
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(SqlScanError::UnbalancedParenthesis)?;
                index += 1;
            }
            byte if depth == 0 && is_word_start(byte) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_word_continue(bytes[index]) {
                    index += 1;
                }
                words.push(sql[start..index].to_ascii_uppercase());
            }
            _ => index += 1,
        }
    }
    if depth == 0 {
        Ok(words)
    } else {
        Err(SqlScanError::UnbalancedParenthesis)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn classifies_recursive_cte_by_outer_statement() {
        let class = classify_statement(
            "WITH RECURSIVE x(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM x) SELECT * FROM x",
        )
        .unwrap();
        assert_eq!(class.kind, StatementKind::Select);
        assert!(class.allowed_for_end_user());
    }

    #[test]
    fn classifies_writable_cte_outer_update() {
        let class = classify_statement("WITH x AS (SELECT 1) UPDATE docs SET n = 2").unwrap();
        assert_eq!(class.kind, StatementKind::Update);
    }

    #[test]
    fn rejects_statement_smuggling() {
        assert!(matches!(
            classify_statement("SELECT 1; PRAGMA writable_schema=ON"),
            Err(ClassificationError::ExpectedOneStatement)
        ));
    }

    #[test]
    fn strings_and_comments_cannot_change_classification() {
        let class = classify_statement("/* DELETE */ SELECT 'ATTACH x' -- DROP").unwrap();
        assert_eq!(class.kind, StatementKind::Select);
    }

    #[test]
    fn supported_classification_agrees_with_sqlite_statement_readonly() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE docs(id INTEGER PRIMARY KEY, value TEXT)")
            .unwrap();
        for (sql, expected) in [
            ("SELECT id,value FROM docs", StatementKind::Select),
            (
                "WITH ids AS (SELECT id FROM docs) SELECT * FROM ids",
                StatementKind::Select,
            ),
            ("INSERT INTO docs(value) VALUES (?1)", StatementKind::Insert),
            (
                "REPLACE INTO docs(id,value) VALUES (1,?1)",
                StatementKind::Insert,
            ),
            (
                "UPDATE docs SET value=?1 WHERE id=?2",
                StatementKind::Update,
            ),
            ("DELETE FROM docs WHERE id=?1", StatementKind::Delete),
            (
                "WITH ids AS (SELECT id FROM docs) UPDATE docs SET value='x' WHERE id IN ids",
                StatementKind::Update,
            ),
        ] {
            let sqlite = connection.prepare(sql).unwrap();
            let ours = classify_statement(sql).unwrap();
            assert_eq!(ours.kind, expected, "classification differed for {sql}");
            assert_eq!(
                ours.read_only,
                sqlite.readonly(),
                "read-only decision differed for {sql}"
            );
        }
    }

    #[test]
    fn conservative_readonly_never_exceeds_sqlite_for_compatible_corpus() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE docs(id INTEGER PRIMARY KEY, value TEXT)")
            .unwrap();
        for sql in [
            "SELECT 1",
            "VALUES (1)",
            "EXPLAIN SELECT * FROM docs",
            "PRAGMA table_info(docs)",
            "CREATE INDEX docs_value ON docs(value)",
            "DROP TABLE docs",
        ] {
            let sqlite = connection.prepare(sql).unwrap();
            let ours = classify_statement(sql).unwrap();
            assert!(
                !ours.read_only || sqlite.readonly(),
                "classifier marked a SQLite write as read-only: {sql}"
            );
        }
    }
}
