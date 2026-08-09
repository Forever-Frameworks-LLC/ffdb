use std::borrow::Cow;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rusqlite::{
    ToSql,
    types::{ToSqlOutput, ValueRef},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SqlParameter {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl ToSql for SqlParameter {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Self::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Self::Integer(value) => ToSqlOutput::Borrowed(ValueRef::Integer(*value)),
            Self::Real(value) => ToSqlOutput::Borrowed(ValueRef::Real(*value)),
            Self::Text(value) => ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes())),
            Self::Blob(value) => ToSqlOutput::Borrowed(ValueRef::Blob(value)),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResultValue {
    Null,
    Integer(i64),
    IntegerString(String),
    Real(f64),
    Text(String),
    Blob {
        #[serde(rename = "$blob")]
        data: String,
    },
}

impl ResultValue {
    pub(crate) fn from_value_ref(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(value)
                if (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value) =>
            {
                Self::Integer(value)
            }
            ValueRef::Integer(value) => Self::IntegerString(value.to_string()),
            ValueRef::Real(value) => Self::Real(value),
            ValueRef::Text(value) => Self::Text(String::from_utf8_lossy(value).into_owned()),
            ValueRef::Blob(value) => Self::Blob {
                data: BASE64.encode(value),
            },
        }
    }

    pub(crate) fn encoded_size(&self) -> usize {
        match self {
            Self::Null => 4,
            Self::Integer(value) => decimal_len(*value),
            Self::IntegerString(value) | Self::Text(value) => value.len().saturating_add(2),
            Self::Real(_) => 24,
            Self::Blob { data } => data.len().saturating_add(12),
        }
    }
}

fn decimal_len(value: i64) -> usize {
    let rendered: Cow<'static, str> = Cow::Owned(value.to_string());
    rendered.len()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResultColumn {
    pub name: String,
    pub declared_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<ResultValue>>,
    pub affected_rows: u64,
    pub last_insert_rowid: Option<i64>,
    pub truncated: bool,
}

impl QueryResult {
    pub(crate) fn encoded_size(&self) -> usize {
        let columns = self
            .columns
            .iter()
            .map(|column| column.name.len().saturating_add(32))
            .sum::<usize>();
        let rows = self
            .rows
            .iter()
            .flatten()
            .map(|value| value.encoded_size().saturating_add(1))
            .sum::<usize>();
        columns.saturating_add(rows).saturating_add(128)
    }
}
