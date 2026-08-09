use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use base64::Engine as _;
use rusqlite::{Connection, functions::FunctionFlags, types::Value};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};

use crate::RuntimeError;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeveloperPrincipal {
    pub actor_id: String,
    pub api_key_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthContext {
    pub project_id: String,
    pub subject: String,
    pub role: String,
    pub claims: Map<String, JsonValue>,
    pub token_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ExecutionMode {
    Developer(DeveloperPrincipal),
    EndUser(AuthContext),
}

#[derive(Debug, Default)]
pub(crate) struct ContextState {
    pub active: Option<Arc<ExecutionMode>>,
    pub internal_depth: u32,
    pub approved_sources: BTreeSet<String>,
    pub request_transaction_id: Option<String>,
    pub client_mutation_id: Option<String>,
}

pub(crate) type SharedContext = Arc<Mutex<ContextState>>;

pub(crate) struct ContextLease {
    state: SharedContext,
}

impl ContextLease {
    pub(crate) fn install(
        state: &SharedContext,
        mode: ExecutionMode,
    ) -> Result<Self, RuntimeError> {
        let mut guard = state.lock().map_err(|_| RuntimeError::Poisoned)?;
        if guard.active.is_some() || guard.internal_depth != 0 {
            return Err(RuntimeError::ContextAlreadyInstalled);
        }
        guard.active = Some(Arc::new(mode));
        guard.request_transaction_id = Some(next_transaction_id());
        guard.client_mutation_id = None;
        drop(guard);
        Ok(Self {
            state: Arc::clone(state),
        })
    }
}

impl Drop for ContextLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = None;
            state.internal_depth = 0;
            state.request_transaction_id = None;
            state.client_mutation_id = None;
        }
    }
}

pub(crate) struct InternalLease {
    state: SharedContext,
}

pub(crate) struct MutationLease {
    state: SharedContext,
}

impl MutationLease {
    pub(crate) fn install(state: &SharedContext, mutation_id: &str) -> Result<Self, RuntimeError> {
        let mut guard = state.lock().map_err(|_| RuntimeError::Poisoned)?;
        if guard.client_mutation_id.is_some() {
            return Err(RuntimeError::ContextAlreadyInstalled);
        }
        guard.client_mutation_id = Some(mutation_id.to_owned());
        drop(guard);
        Ok(Self {
            state: Arc::clone(state),
        })
    }
}

impl Drop for MutationLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.client_mutation_id = None;
        }
    }
}

impl InternalLease {
    pub(crate) fn enter(state: &SharedContext) -> Result<Self, RuntimeError> {
        let mut guard = state.lock().map_err(|_| RuntimeError::Poisoned)?;
        guard.internal_depth = guard
            .internal_depth
            .checked_add(1)
            .ok_or(RuntimeError::Poisoned)?;
        drop(guard);
        Ok(Self {
            state: Arc::clone(state),
        })
    }
}

impl Drop for InternalLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.internal_depth = state.internal_depth.saturating_sub(1);
        }
    }
}

pub(crate) fn install_auth_functions(
    connection: &Connection,
    state: &SharedContext,
) -> Result<(), RuntimeError> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS;
    let uid_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_auth_uid", 0, flags, move |_| {
        with_end_user(&uid_state, |context| Value::Text(context.subject.clone()))
    })?;
    let role_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_auth_role", 0, flags, move |_| {
        let state = role_state.lock().map_err(function_error)?;
        let role = match state.active.as_deref() {
            Some(ExecutionMode::EndUser(context)) => context.role.clone(),
            Some(ExecutionMode::Developer(_)) => "service_role".to_owned(),
            None => return Err(function_error("request context is not installed")),
        };
        Ok(Value::Text(role))
    })?;
    let jwt_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_auth_jwt", 0, flags, move |_| {
        with_end_user(&jwt_state, |context| {
            let mut claims = context.claims.clone();
            claims.insert("sub".to_owned(), JsonValue::String(context.subject.clone()));
            claims.insert("role".to_owned(), JsonValue::String(context.role.clone()));
            claims.insert(
                "project_id".to_owned(),
                JsonValue::String(context.project_id.clone()),
            );
            claims.insert(
                "jti".to_owned(),
                JsonValue::String(context.token_id.clone()),
            );
            Value::Text(JsonValue::Object(claims).to_string())
        })
    })?;
    let claim_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_auth_claim", 1, flags, move |function| {
        let name = function.get::<String>(0)?;
        with_end_user(&claim_state, |context| {
            context
                .claims
                .get(&name)
                .map_or(Value::Null, json_to_sqlite)
        })
    })?;
    let developer_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_is_developer", 0, flags, move |_| {
        let state = developer_state.lock().map_err(function_error)?;
        match state.active.as_deref() {
            Some(ExecutionMode::Developer(_)) => Ok(1_i64),
            Some(ExecutionMode::EndUser(_)) => Ok(0_i64),
            None => Err(function_error("request context is not installed")),
        }
    })?;
    let actor_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_actor", 0, flags, move |_| {
        let state = actor_state.lock().map_err(function_error)?;
        match state.active.as_deref() {
            Some(ExecutionMode::EndUser(context)) => Ok(context.subject.clone()),
            Some(ExecutionMode::Developer(context)) => Ok(context.actor_id.clone()),
            None => Err(function_error("request context is not installed")),
        }
    })?;
    let transaction_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_transaction_id", 0, flags, move |_| {
        let state = transaction_state.lock().map_err(function_error)?;
        state
            .request_transaction_id
            .clone()
            .ok_or_else(|| function_error("request context is not installed"))
    })?;
    let mutation_state = Arc::clone(state);
    connection.create_scalar_function("__ffdb_client_mutation_id", 0, flags, move |_| {
        let state = mutation_state.lock().map_err(function_error)?;
        Ok(state.client_mutation_id.clone())
    })?;
    connection.create_scalar_function("__ffdb_sync_json", 1, flags, move |function| {
        let value = function.get::<Value>(0)?;
        let json = match value {
            Value::Null => JsonValue::Null,
            Value::Integer(value) => JsonValue::from(value),
            Value::Real(value) => serde_json::Number::from_f64(value)
                .map(JsonValue::Number)
                .unwrap_or(JsonValue::Null),
            Value::Text(value) => JsonValue::String(value),
            Value::Blob(value) => JsonValue::Object(Map::from_iter([(
                "$blob".to_owned(),
                JsonValue::String(base64::engine::general_purpose::STANDARD.encode(value)),
            )])),
        };
        Ok(json.to_string())
    })?;
    Ok(())
}

fn next_transaction_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn with_end_user(
    state: &SharedContext,
    callback: impl FnOnce(&AuthContext) -> Value,
) -> rusqlite::Result<Value> {
    let state = state.lock().map_err(function_error)?;
    match state.active.as_deref() {
        Some(ExecutionMode::EndUser(context)) => Ok(callback(context)),
        Some(ExecutionMode::Developer(_)) => Ok(Value::Null),
        None => Err(function_error("request context is not installed")),
    }
}

fn json_to_sqlite(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Integer(i64::from(*value)),
        JsonValue::Number(value) => value.as_i64().map_or_else(
            || value.as_f64().map_or(Value::Null, Value::Real),
            Value::Integer,
        ),
        JsonValue::String(value) => Value::Text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => Value::Text(value.to_string()),
    }
}

fn function_error(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(error.to_string())))
}
