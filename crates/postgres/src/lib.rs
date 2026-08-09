//! Narrow PostgreSQL-only SQLx facade.
//!
//! The upstream `sqlx` facade records optional drivers in Cargo.lock. FFDB uses
//! only PostgreSQL, so this crate exposes the small API surface used by the
//! workspace while depending directly on the pinned core and Postgres crates.

pub use sqlx_core::error::{Error, Result};
pub use sqlx_core::migrate;
pub use sqlx_core::query::query;
pub use sqlx_core::query_scalar::query_scalar;
pub use sqlx_core::row::Row;
pub use sqlx_core::sql_str::{AssertSqlSafe, SqlSafeStr, SqlStr};
pub use sqlx_core::transaction::Transaction;
pub use sqlx_core::types;
pub use sqlx_postgres::{PgPool, Postgres};

pub mod postgres {
    pub use sqlx_postgres::{PgPoolOptions, PgRow};
}
