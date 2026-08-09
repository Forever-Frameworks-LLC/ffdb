use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Identifier, IdentifierError, SqlScanError, split_sql_statements};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    #[default]
    Permissive,
    Restrictive,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCommand {
    #[default]
    All,
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleName(Identifier);

impl RoleName {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        Identifier::new(value).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Predicate {
    sql: String,
}

impl Predicate {
    pub fn new(sql: impl Into<String>) -> Result<Self, RlsParseError> {
        let sql = sql.into();
        validate_predicate(&sql)?;
        Ok(Self { sql })
    }

    #[must_use]
    pub fn as_sql(&self) -> &str {
        &self.sql
    }

    /// Produces SQLite SQL after replacing the only supported auth functions.
    /// The predicate was already checked for statement separators, comments,
    /// internal identifiers, and malformed auth calls.
    #[must_use]
    pub fn sqlite_sql(&self) -> String {
        match rewrite_auth_functions(&self.sql) {
            Ok(sql) => sql,
            Err(_) => "0".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatePolicy {
    pub name: Identifier,
    pub table: Identifier,
    pub mode: PolicyMode,
    pub command: PolicyCommand,
    pub roles: Vec<RoleName>,
    pub using: Option<Predicate>,
    pub with_check: Option<Predicate>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AlterPolicy {
    pub name: Identifier,
    pub table: Identifier,
    pub rename_to: Option<Identifier>,
    pub roles: Option<Vec<RoleName>>,
    pub using: Option<Predicate>,
    pub with_check: Option<Predicate>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DropPolicy {
    pub name: Identifier,
    pub table: Identifier,
    pub if_exists: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlterTableRls {
    Enable,
    Disable,
    Force,
    NoForce,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RlsStatement {
    AlterTable {
        table: Identifier,
        action: AlterTableRls,
    },
    CreatePolicy(CreatePolicy),
    AlterPolicy(AlterPolicy),
    DropPolicy(DropPolicy),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RlsParseError {
    #[error("empty RLS input")]
    Empty,
    #[error("unsupported RLS syntax at byte {offset}: {message}")]
    Syntax { offset: usize, message: String },
    #[error("an RLS identifier may not use the reserved __ffdb_ prefix")]
    InternalIdentifier,
    #[error("policy predicate must not contain SQL comments or statement separators")]
    UnsafePredicate,
    #[error("policy predicate contains an unsupported auth function")]
    UnsupportedAuthFunction,
    #[error("auth.claim requires exactly one non-empty string literal argument")]
    InvalidClaimArgument,
    #[error("policy predicate contains unbalanced parentheses or quoting")]
    MalformedPredicate,
    #[error("{command:?} policies do not support a {clause} clause")]
    InvalidPolicyClause {
        command: PolicyCommand,
        clause: &'static str,
    },
    #[error(transparent)]
    Identifier(#[from] IdentifierError),
    #[error(transparent)]
    Scan(#[from] SqlScanError),
}

pub fn parse_rls(sql: &str) -> Result<Vec<RlsStatement>, RlsParseError> {
    let statements = split_sql_statements(sql)?;
    if statements.is_empty() {
        return Err(RlsParseError::Empty);
    }
    statements.into_iter().map(parse_rls_statement).collect()
}

pub fn parse_rls_statement(sql: &str) -> Result<RlsStatement, RlsParseError> {
    let tokens = lex(sql, true)?;
    if tokens.is_empty() {
        return Err(RlsParseError::Empty);
    }
    let mut parser = Parser {
        sql,
        tokens,
        cursor: 0,
    };
    let statement = if parser.consume_keyword("CREATE") {
        parser.expect_keyword("POLICY")?;
        RlsStatement::CreatePolicy(parser.parse_create_policy()?)
    } else if parser.consume_keyword("ALTER") {
        if parser.consume_keyword("TABLE") {
            parser.parse_alter_table()?
        } else {
            parser.expect_keyword("POLICY")?;
            RlsStatement::AlterPolicy(parser.parse_alter_policy()?)
        }
    } else if parser.consume_keyword("DROP") {
        parser.expect_keyword("POLICY")?;
        RlsStatement::DropPolicy(parser.parse_drop_policy()?)
    } else {
        return Err(
            parser.syntax("expected CREATE POLICY, ALTER POLICY, DROP POLICY, or ALTER TABLE")
        );
    };
    parser.expect_end()?;
    Ok(statement)
}

struct Parser<'a> {
    sql: &'a str,
    tokens: Vec<Token>,
    cursor: usize,
}

impl Parser<'_> {
    fn parse_create_policy(&mut self) -> Result<CreatePolicy, RlsParseError> {
        let name = self.identifier()?;
        self.expect_keyword("ON")?;
        let table = self.identifier()?;
        let mut mode = PolicyMode::Permissive;
        let mut command = PolicyCommand::All;
        let mut roles = vec![RoleName::new("public")?];
        let mut using = None;
        let mut with_check = None;

        if self.consume_keyword("AS") {
            mode = if self.consume_keyword("PERMISSIVE") {
                PolicyMode::Permissive
            } else if self.consume_keyword("RESTRICTIVE") {
                PolicyMode::Restrictive
            } else {
                return Err(self.syntax("expected PERMISSIVE or RESTRICTIVE"));
            };
        }
        if self.consume_keyword("FOR") {
            command = self.policy_command()?;
        }
        if self.consume_keyword("TO") {
            roles = self.role_list()?;
        }
        if self.consume_keyword("USING") {
            using = Some(self.parenthesized_predicate()?);
        }
        if self.consume_keyword("WITH") {
            self.expect_keyword("CHECK")?;
            with_check = Some(self.parenthesized_predicate()?);
        }
        validate_policy_clauses(command, using.is_some(), with_check.is_some())?;
        Ok(CreatePolicy {
            name,
            table,
            mode,
            command,
            roles,
            using,
            with_check,
        })
    }

    fn parse_alter_table(&mut self) -> Result<RlsStatement, RlsParseError> {
        let table = self.identifier()?;
        let action = if self.consume_keyword("ENABLE") {
            self.expect_keyword("ROW")?;
            self.expect_keyword("LEVEL")?;
            self.expect_keyword("SECURITY")?;
            AlterTableRls::Enable
        } else if self.consume_keyword("DISABLE") {
            self.expect_keyword("ROW")?;
            self.expect_keyword("LEVEL")?;
            self.expect_keyword("SECURITY")?;
            AlterTableRls::Disable
        } else if self.consume_keyword("FORCE") {
            self.expect_keyword("ROW")?;
            self.expect_keyword("LEVEL")?;
            self.expect_keyword("SECURITY")?;
            AlterTableRls::Force
        } else if self.consume_keyword("NO") {
            self.expect_keyword("FORCE")?;
            self.expect_keyword("ROW")?;
            self.expect_keyword("LEVEL")?;
            self.expect_keyword("SECURITY")?;
            AlterTableRls::NoForce
        } else {
            return Err(self.syntax("expected ENABLE, DISABLE, FORCE, or NO FORCE"));
        };
        Ok(RlsStatement::AlterTable { table, action })
    }

    fn parse_alter_policy(&mut self) -> Result<AlterPolicy, RlsParseError> {
        let name = self.identifier()?;
        self.expect_keyword("ON")?;
        let table = self.identifier()?;
        if self.consume_keyword("RENAME") {
            self.expect_keyword("TO")?;
            return Ok(AlterPolicy {
                name,
                table,
                rename_to: Some(self.identifier()?),
                roles: None,
                using: None,
                with_check: None,
            });
        }
        let roles = self
            .consume_keyword("TO")
            .then(|| self.role_list())
            .transpose()?;
        let using = self
            .consume_keyword("USING")
            .then(|| self.parenthesized_predicate())
            .transpose()?;
        let with_check = if self.consume_keyword("WITH") {
            self.expect_keyword("CHECK")?;
            Some(self.parenthesized_predicate()?)
        } else {
            None
        };
        if roles.is_none() && using.is_none() && with_check.is_none() {
            return Err(self.syntax("ALTER POLICY requires RENAME, TO, USING, or WITH CHECK"));
        }
        Ok(AlterPolicy {
            name,
            table,
            rename_to: None,
            roles,
            using,
            with_check,
        })
    }

    fn parse_drop_policy(&mut self) -> Result<DropPolicy, RlsParseError> {
        let if_exists = if self.consume_keyword("IF") {
            self.expect_keyword("EXISTS")?;
            true
        } else {
            false
        };
        let name = self.identifier()?;
        self.expect_keyword("ON")?;
        let table = self.identifier()?;
        let _ = self.consume_keyword("CASCADE") || self.consume_keyword("RESTRICT");
        Ok(DropPolicy {
            name,
            table,
            if_exists,
        })
    }

    fn policy_command(&mut self) -> Result<PolicyCommand, RlsParseError> {
        for (keyword, command) in [
            ("ALL", PolicyCommand::All),
            ("SELECT", PolicyCommand::Select),
            ("INSERT", PolicyCommand::Insert),
            ("UPDATE", PolicyCommand::Update),
            ("DELETE", PolicyCommand::Delete),
        ] {
            if self.consume_keyword(keyword) {
                return Ok(command);
            }
        }
        Err(self.syntax("expected ALL, SELECT, INSERT, UPDATE, or DELETE"))
    }

    fn role_list(&mut self) -> Result<Vec<RoleName>, RlsParseError> {
        let mut roles = Vec::new();
        loop {
            roles.push(RoleName(self.identifier()?));
            if !self.consume_symbol(',') {
                break;
            }
        }
        Ok(roles)
    }

    fn identifier(&mut self) -> Result<Identifier, RlsParseError> {
        let token = self
            .tokens
            .get(self.cursor)
            .ok_or_else(|| self.syntax("expected identifier"))?;
        let value = match &token.kind {
            TokenKind::Word(value) | TokenKind::Identifier(value) => value.clone(),
            _ => return Err(self.syntax("expected identifier")),
        };
        self.cursor += 1;
        let identifier = Identifier::new(value)?;
        if identifier.is_internal() {
            return Err(RlsParseError::InternalIdentifier);
        }
        Ok(identifier)
    }

    fn parenthesized_predicate(&mut self) -> Result<Predicate, RlsParseError> {
        let open = self
            .tokens
            .get(self.cursor)
            .ok_or_else(|| self.syntax("expected '('"))?;
        if !matches!(open.kind, TokenKind::Symbol('(')) {
            return Err(self.syntax("expected '('"));
        }
        let content_start = open.end;
        self.cursor += 1;
        let mut depth = 1_u32;
        while let Some(token) = self.tokens.get(self.cursor) {
            match token.kind {
                TokenKind::Symbol('(') => depth += 1,
                TokenKind::Symbol(')') => {
                    depth -= 1;
                    if depth == 0 {
                        let content_end = token.start;
                        self.cursor += 1;
                        let value = self.sql[content_start..content_end].trim();
                        if value.is_empty() {
                            return Err(self.syntax("policy predicate must not be empty"));
                        }
                        return Predicate::new(value);
                    }
                }
                _ => {}
            }
            self.cursor += 1;
        }
        Err(RlsParseError::MalformedPredicate)
    }

    fn consume_keyword(&mut self, expected: &str) -> bool {
        if self.tokens.get(self.cursor).is_some_and(|token| {
            matches!(&token.kind, TokenKind::Word(word) if word.eq_ignore_ascii_case(expected))
        }) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, expected: &str) -> Result<(), RlsParseError> {
        if self.consume_keyword(expected) {
            Ok(())
        } else {
            Err(self.syntax(&format!("expected {expected}")))
        }
    }

    fn consume_symbol(&mut self, expected: char) -> bool {
        if self
            .tokens
            .get(self.cursor)
            .is_some_and(|token| token.kind == TokenKind::Symbol(expected))
        {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn expect_end(&self) -> Result<(), RlsParseError> {
        if self.cursor == self.tokens.len() {
            Ok(())
        } else {
            Err(self.syntax("unexpected trailing syntax"))
        }
    }

    fn syntax(&self, message: &str) -> RlsParseError {
        let offset = self
            .tokens
            .get(self.cursor)
            .map_or(self.sql.len(), |token| token.start);
        RlsParseError::Syntax {
            offset,
            message: message.to_owned(),
        }
    }
}

fn validate_policy_clauses(
    command: PolicyCommand,
    has_using: bool,
    has_check: bool,
) -> Result<(), RlsParseError> {
    if has_using && command == PolicyCommand::Insert {
        return Err(RlsParseError::InvalidPolicyClause {
            command,
            clause: "USING",
        });
    }
    if has_check && matches!(command, PolicyCommand::Select | PolicyCommand::Delete) {
        return Err(RlsParseError::InvalidPolicyClause {
            command,
            clause: "WITH CHECK",
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    Word(String),
    Identifier(String),
    StringLiteral(String),
    Symbol(char),
    Other,
}

fn lex(sql: &str, allow_comments: bool) -> Result<Vec<Token>, RlsParseError> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                if !allow_comments {
                    return Err(RlsParseError::UnsafePredicate);
                }
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                if !allow_comments {
                    return Err(RlsParseError::UnsafePredicate);
                }
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                if index + 1 == bytes.len() {
                    return Err(RlsParseError::MalformedPredicate);
                }
                index += 2;
            }
            b'\'' => {
                let (end, value) = decode_quoted(sql, index, '\'', '\'')?;
                index = end;
                tokens.push(Token {
                    kind: TokenKind::StringLiteral(value),
                    start,
                    end,
                });
            }
            b'"' => {
                let (end, value) = decode_quoted(sql, index, '"', '"')?;
                index = end;
                tokens.push(Token {
                    kind: TokenKind::Identifier(value),
                    start,
                    end,
                });
            }
            b'`' => {
                let (end, value) = decode_quoted(sql, index, '`', '`')?;
                index = end;
                tokens.push(Token {
                    kind: TokenKind::Identifier(value),
                    start,
                    end,
                });
            }
            b'[' => {
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != b']' {
                    index += 1;
                }
                if index == bytes.len() {
                    return Err(RlsParseError::MalformedPredicate);
                }
                let value = sql[value_start..index].to_owned();
                index += 1;
                tokens.push(Token {
                    kind: TokenKind::Identifier(value),
                    start,
                    end: index,
                });
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$'))
                {
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Word(sql[start..index].to_owned()),
                    start,
                    end: index,
                });
            }
            byte @ (b'(' | b')' | b',' | b'.' | b';') => {
                index += 1;
                tokens.push(Token {
                    kind: TokenKind::Symbol(char::from(byte)),
                    start,
                    end: index,
                });
            }
            _ => {
                index += 1;
                tokens.push(Token {
                    kind: TokenKind::Other,
                    start,
                    end: index,
                });
            }
        }
    }
    Ok(tokens)
}

fn decode_quoted(
    sql: &str,
    start: usize,
    quote: char,
    escape: char,
) -> Result<(usize, String), RlsParseError> {
    let mut value = String::new();
    let mut chars = sql[start + quote.len_utf8()..].char_indices().peekable();
    while let Some((relative, character)) = chars.next() {
        if character == quote {
            if chars.peek().is_some_and(|(_, next)| *next == escape) {
                let _ = chars.next();
                value.push(quote);
            } else {
                return Ok((
                    start + quote.len_utf8() + relative + quote.len_utf8(),
                    value,
                ));
            }
        } else {
            value.push(character);
        }
    }
    Err(RlsParseError::MalformedPredicate)
}

fn validate_predicate(sql: &str) -> Result<(), RlsParseError> {
    if sql.trim().is_empty() {
        return Err(RlsParseError::MalformedPredicate);
    }
    let tokens = lex(sql, false)?;
    if tokens
        .iter()
        .any(|token| token.kind == TokenKind::Symbol(';'))
    {
        return Err(RlsParseError::UnsafePredicate);
    }
    let mut depth = 0_i32;
    for token in &tokens {
        match token.kind {
            TokenKind::Symbol('(') => depth += 1,
            TokenKind::Symbol(')') => {
                depth -= 1;
                if depth < 0 {
                    return Err(RlsParseError::MalformedPredicate);
                }
            }
            TokenKind::Word(ref word) | TokenKind::Identifier(ref word)
                if word.to_ascii_lowercase().starts_with("__ffdb_") =>
            {
                return Err(RlsParseError::InternalIdentifier);
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(RlsParseError::MalformedPredicate);
    }
    let _ = rewrite_auth_functions(sql)?;
    Ok(())
}

fn rewrite_auth_functions(sql: &str) -> Result<String, RlsParseError> {
    let tokens = lex(sql, false)?;
    let mut output = String::with_capacity(sql.len());
    let mut source_cursor = 0;
    let mut index = 0;
    while index < tokens.len() {
        let is_auth = matches!(&tokens[index].kind, TokenKind::Word(word) if word.eq_ignore_ascii_case("auth"));
        if !is_auth {
            index += 1;
            continue;
        }
        if !matches!(
            tokens.get(index + 1).map(|token| &token.kind),
            Some(TokenKind::Symbol('.'))
        ) {
            index += 1;
            continue;
        }
        let Some(Token {
            kind: TokenKind::Word(function),
            ..
        }) = tokens.get(index + 2)
        else {
            return Err(RlsParseError::UnsupportedAuthFunction);
        };
        let replacement = match function.to_ascii_lowercase().as_str() {
            "uid" => "__ffdb_auth_uid",
            "role" => "__ffdb_auth_role",
            "jwt" => "__ffdb_auth_jwt",
            "claim" => "__ffdb_auth_claim",
            _ => return Err(RlsParseError::UnsupportedAuthFunction),
        };
        if !matches!(
            tokens.get(index + 3).map(|token| &token.kind),
            Some(TokenKind::Symbol('('))
        ) {
            return Err(RlsParseError::UnsupportedAuthFunction);
        }
        let close = if replacement == "__ffdb_auth_claim" {
            if !matches!(tokens.get(index + 4).map(|token| &token.kind), Some(TokenKind::StringLiteral(value)) if !value.is_empty() && value.chars().count() <= 128)
                || !matches!(
                    tokens.get(index + 5).map(|token| &token.kind),
                    Some(TokenKind::Symbol(')'))
                )
            {
                return Err(RlsParseError::InvalidClaimArgument);
            }
            index + 5
        } else {
            if !matches!(
                tokens.get(index + 4).map(|token| &token.kind),
                Some(TokenKind::Symbol(')'))
            ) {
                return Err(RlsParseError::UnsupportedAuthFunction);
            }
            index + 4
        };
        output.push_str(&sql[source_cursor..tokens[index].start]);
        output.push_str(replacement);
        source_cursor = tokens[index + 2].end;
        index = close + 1;
    }
    output.push_str(&sql[source_cursor..]);
    Ok(output)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn parses_documented_policy_and_rewrites_auth() {
        let statement = parse_rls_statement(
            "CREATE POLICY documents_read ON documents AS RESTRICTIVE FOR SELECT TO authenticated USING (organization_id = auth.claim('organization_id'))",
        )
        .unwrap();
        let RlsStatement::CreatePolicy(policy) = statement else {
            panic!("wrong statement")
        };
        assert_eq!(policy.mode, PolicyMode::Restrictive);
        assert_eq!(policy.command, PolicyCommand::Select);
        assert_eq!(policy.roles[0].as_str(), "authenticated");
        assert_eq!(
            policy.using.unwrap().sqlite_sql(),
            "organization_id = __ffdb_auth_claim('organization_id')"
        );
    }

    #[test]
    fn parses_rls_table_state_and_drop() {
        assert_eq!(
            parse_rls("ALTER TABLE docs ENABLE ROW LEVEL SECURITY; ALTER TABLE docs FORCE ROW LEVEL SECURITY; DROP POLICY IF EXISTS p ON docs").unwrap().len(),
            3
        );
    }

    #[test]
    fn predicate_cannot_smuggle_statement_or_internal_name() {
        assert!(matches!(
            Predicate::new("true); DROP TABLE users; --"),
            Err(RlsParseError::UnsafePredicate)
        ));
        assert!(matches!(
            Predicate::new("EXISTS (SELECT 1 FROM __ffdb_rows)"),
            Err(RlsParseError::InternalIdentifier)
        ));
    }

    #[test]
    fn literals_that_look_dangerous_remain_literals() {
        let predicate = Predicate::new("note = '; -- __ffdb_secret auth.uid()'").unwrap();
        assert_eq!(
            predicate.sqlite_sql(),
            "note = '; -- __ffdb_secret auth.uid()'"
        );
    }

    #[test]
    fn rejects_dynamic_claim_names_and_unknown_auth_functions() {
        assert!(matches!(
            Predicate::new("auth.claim(column_name) = 'x'"),
            Err(RlsParseError::InvalidClaimArgument)
        ));
        assert!(matches!(
            Predicate::new("auth.set_uid('x')"),
            Err(RlsParseError::UnsupportedAuthFunction)
        ));
    }

    #[test]
    fn quoted_identifiers_are_decoded_and_safely_rerendered() {
        let RlsStatement::CreatePolicy(policy) = parse_rls_statement(
            "CREATE POLICY \"read policy\" ON \"odd\"\"table\" FOR SELECT USING (true)",
        )
        .unwrap() else {
            panic!("wrong statement")
        };
        assert_eq!(policy.name.as_str(), "read policy");
        assert_eq!(policy.table.quoted(), "\"odd\"\"table\"");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn generated_select_policies_parse_and_rewrite_only_auth_calls(
            policy in "[a-z][a-z0-9_]{0,24}",
            table in "[a-z][a-z0-9_]{0,24}",
            role in "[a-z][a-z0-9_]{0,24}",
            claim in "[a-z][a-z0-9_]{0,24}",
        ) {
            let sql = format!(
                "CREATE POLICY {policy} ON {table} AS RESTRICTIVE FOR SELECT TO {role} \
                 USING (owner_id = auth.uid() AND auth.claim('{claim}') = '{claim}')"
            );
            let parsed = parse_rls_statement(&sql).unwrap();
            let RlsStatement::CreatePolicy(parsed) = parsed else {
                return Err(TestCaseError::fail("expected create policy"));
            };
            prop_assert_eq!(parsed.name.as_str(), policy);
            prop_assert_eq!(parsed.table.as_str(), table);
            prop_assert_eq!(parsed.roles[0].as_str(), role);
            let rewritten = parsed.using.unwrap().sqlite_sql();
            prop_assert!(rewritten.contains("__ffdb_auth_uid()"));
            prop_assert!(rewritten.contains("__ffdb_auth_claim"));
            prop_assert!(!rewritten.contains("auth.uid()"));
        }

        #[test]
        fn injected_predicate_separators_fail_closed(prefix in "[a-zA-Z0-9_ ()=]{0,64}") {
            let predicate = format!("{prefix}; DROP TABLE users");
            prop_assert!(matches!(
                Predicate::new(predicate),
                Err(RlsParseError::UnsafePredicate)
            ));
        }
    }
}
