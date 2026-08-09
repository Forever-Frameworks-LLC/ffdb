use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A decoded SQL identifier. Rendering always uses SQLite double-quote escaping.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Identifier(String);

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IdentifierError {
    #[error("identifier must not be empty")]
    Empty,
    #[error("identifier is longer than 128 characters")]
    TooLong,
    #[error("identifier contains a control character")]
    ControlCharacter,
}

impl Identifier {
    /// Constructs an identifier from its decoded value.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdentifierError::Empty);
        }
        if value.chars().count() > 128 {
            return Err(IdentifierError::TooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(IdentifierError::ControlCharacter);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_internal(&self) -> bool {
        self.0.to_ascii_lowercase().starts_with("__ffdb_")
    }

    /// Renders a safely quoted SQLite identifier.
    #[must_use]
    pub fn quoted(&self) -> String {
        format!("\"{}\"", self.0.replace('"', "\"\""))
    }
}

impl Display for Identifier {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.quoted())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn identifier_rendering_cannot_break_out_of_quotes() {
        let identifier = Identifier::new("docs\"; drop table users; --").unwrap();
        assert_eq!(identifier.quoted(), "\"docs\"\"; drop table users; --\"");
    }

    #[test]
    fn internal_prefix_is_case_insensitive() {
        assert!(Identifier::new("__FFDB_rows").unwrap().is_internal());
        assert!(!Identifier::new("ffdb_rows").unwrap().is_internal());
    }
}
