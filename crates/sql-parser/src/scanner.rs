use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SqlScanError {
    #[error("SQL contains an unterminated single-quoted string")]
    UnterminatedString,
    #[error("SQL contains an unterminated quoted identifier")]
    UnterminatedIdentifier,
    #[error("SQL contains an unterminated block comment")]
    UnterminatedComment,
    #[error("SQL contains an unbalanced parenthesis")]
    UnbalancedParenthesis,
}

/// Splits SQL without treating semicolons in strings, identifiers, comments, or
/// trigger bodies as statement boundaries.
pub fn split_sql_statements(sql: &str) -> Result<Vec<&str>, SqlScanError> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut depth = 0_u32;
    let mut trigger_body_depth = 0_u32;
    let mut possible_trigger = false;
    let mut words: Vec<String> = Vec::new();

    while index < bytes.len() {
        match bytes[index] {
            b'\'' => index = skip_quoted(bytes, index, b'\'', SqlScanError::UnterminatedString)?,
            b'"' | b'`' => {
                let quote = bytes[index];
                index = skip_quoted(bytes, index, quote, SqlScanError::UnterminatedIdentifier)?;
            }
            b'[' => index = skip_bracket_identifier(bytes, index)?,
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
            byte if is_word_start(byte) => {
                let word_start = index;
                index += 1;
                while index < bytes.len() && is_word_continue(bytes[index]) {
                    index += 1;
                }
                let word = sql[word_start..index].to_ascii_uppercase();
                if depth == 0 {
                    if words.len() < 3 {
                        words.push(word.clone());
                        possible_trigger |= words.as_slice() == ["CREATE", "TRIGGER"]
                            || (words.len() == 3
                                && words[0] == "CREATE"
                                && matches!(words[1].as_str(), "TEMP" | "TEMPORARY")
                                && words[2] == "TRIGGER");
                    }
                    if (possible_trigger && word == "BEGIN")
                        || (trigger_body_depth > 0 && word == "CASE")
                    {
                        trigger_body_depth += 1;
                    } else if trigger_body_depth > 0 && word == "END" {
                        trigger_body_depth -= 1;
                    }
                }
            }
            b';' if depth == 0 && trigger_body_depth == 0 => {
                if !is_trivia(&sql[start..index]) {
                    statements.push(sql[start..index].trim());
                }
                index += 1;
                start = index;
                words.clear();
                possible_trigger = false;
            }
            _ => index += 1,
        }
    }

    if depth != 0 {
        return Err(SqlScanError::UnbalancedParenthesis);
    }
    if !is_trivia(&sql[start..]) {
        statements.push(sql[start..].trim());
    }
    Ok(statements)
}

fn is_trivia(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes[index] == b'-' && bytes.get(index + 1) == Some(&b'-') {
            index += 2;
            while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                index += 1;
            }
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let Ok(next) = skip_block_comment(bytes, index) else {
                return false;
            };
            index = next;
        } else {
            return false;
        }
    }
    true
}

pub(crate) fn skip_quoted(
    bytes: &[u8],
    mut index: usize,
    quote: u8,
    error: SqlScanError,
) -> Result<usize, SqlScanError> {
    index += 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
            } else {
                return Ok(index + 1);
            }
        } else {
            index += 1;
        }
    }
    Err(error)
}

fn skip_bracket_identifier(bytes: &[u8], mut index: usize) -> Result<usize, SqlScanError> {
    index += 1;
    while index < bytes.len() {
        if bytes[index] == b']' {
            return Ok(index + 1);
        }
        index += 1;
    }
    Err(SqlScanError::UnterminatedIdentifier)
}

pub(crate) fn skip_block_comment(bytes: &[u8], mut index: usize) -> Result<usize, SqlScanError> {
    index += 2;
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return Ok(index + 2);
        }
        index += 1;
    }
    Err(SqlScanError::UnterminatedComment)
}

pub(crate) const fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

pub(crate) const fn is_word_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn splits_around_literals_and_comments() {
        let sql = "select ';' /* ; */; -- ;\n select 2";
        assert_eq!(
            split_sql_statements(sql).unwrap(),
            ["select ';' /* ; */", "-- ;\n select 2"]
        );
    }

    #[test]
    fn does_not_split_a_trigger_body() {
        let sql = "CREATE TRIGGER t AFTER INSERT ON x BEGIN INSERT INTO y VALUES (1); UPDATE z SET n=2; END; SELECT 1";
        let parts = split_sql_statements(sql).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("CREATE TRIGGER"));
        assert_eq!(parts[1], "SELECT 1");
    }
}
