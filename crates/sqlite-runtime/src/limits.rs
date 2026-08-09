use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionLimits {
    pub max_sql_bytes: usize,
    pub max_variables: usize,
    pub max_rows: usize,
    pub max_response_bytes: usize,
    pub statement_timeout: Duration,
    pub transaction_timeout: Duration,
    pub max_database_bytes: u64,
    pub progress_ops: i32,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_sql_bytes: 256 * 1024,
            max_variables: 999,
            max_rows: 10_000,
            max_response_bytes: 8 * 1024 * 1024,
            statement_timeout: Duration::from_secs(5),
            transaction_timeout: Duration::from_secs(15),
            max_database_bytes: 1024 * 1024 * 1024,
            progress_ops: 1_000,
        }
    }
}

impl ExecutionLimits {
    pub(crate) fn validate(&self) -> bool {
        self.max_sql_bytes > 0
            && self.max_variables > 0
            && self.max_rows > 0
            && self.max_response_bytes > 0
            && !self.statement_timeout.is_zero()
            && !self.transaction_timeout.is_zero()
            && self.max_database_bytes > 0
            && self.progress_ops > 0
    }

    pub(crate) fn restricted_by(&self, requested: &Self) -> Self {
        Self {
            max_sql_bytes: self.max_sql_bytes.min(requested.max_sql_bytes),
            max_variables: self.max_variables.min(requested.max_variables),
            max_rows: self.max_rows.min(requested.max_rows),
            max_response_bytes: self.max_response_bytes.min(requested.max_response_bytes),
            statement_timeout: self.statement_timeout.min(requested.statement_timeout),
            transaction_timeout: self.transaction_timeout.min(requested.transaction_timeout),
            max_database_bytes: self.max_database_bytes.min(requested.max_database_bytes),
            progress_ops: self.progress_ops.min(requested.progress_ops),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
