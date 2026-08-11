use std::sync::Arc;

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

use crate::context::{ExecutionMode, SharedContext};

pub(crate) fn install(connection: &rusqlite::Connection, state: &SharedContext) {
    let state = Arc::clone(state);
    connection.authorizer(Some(move |context: AuthContext<'_>| {
        authorize(&state, &context)
    }));
}

fn authorize(state: &SharedContext, context: &AuthContext<'_>) -> Authorization {
    let Ok(state) = state.lock() else {
        return Authorization::Deny;
    };
    if state.internal_depth > 0 {
        return Authorization::Allow;
    }
    let Some(mode) = state.active.as_deref() else {
        return Authorization::Deny;
    };
    let source = context.accessor;
    match context.action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => Authorization::Deny,
        AuthAction::Function { function_name } if dangerous_function(function_name) => {
            Authorization::Deny
        }
        AuthAction::Function { function_name } if is_internal(function_name) => {
            if (state.public_auth_depth > 0
                && source.is_none()
                && is_public_auth_implementation(function_name))
                || source.is_some_and(|source| {
                    approved_generated_source(source, &state.approved_sources)
                })
            {
                Authorization::Allow
            } else {
                tracing::debug!(
                    object_kind = "internal_function",
                    object_name = function_name,
                    accessor = source.unwrap_or("direct"),
                    "SQLite authorizer denied an operation"
                );
                Authorization::Deny
            }
        }
        action if creates_or_drops_protected(&action, &state.approved_sources) => {
            Authorization::Deny
        }
        AuthAction::Read { table_name, .. }
        | AuthAction::Insert { table_name }
        | AuthAction::Delete { table_name }
        | AuthAction::Update { table_name, .. }
            if is_internal(table_name) =>
        {
            if source
                .is_some_and(|source| approved_generated_source(source, &state.approved_sources))
            {
                Authorization::Allow
            } else {
                tracing::debug!(
                    object_kind = "internal_table",
                    object_name = table_name,
                    accessor = source.unwrap_or("direct"),
                    "SQLite authorizer denied an operation"
                );
                Authorization::Deny
            }
        }
        AuthAction::Read { table_name, .. }
        | AuthAction::Insert { table_name }
        | AuthAction::Delete { table_name }
        | AuthAction::Update { table_name, .. }
            if sqlite_schema_name(table_name) =>
        {
            match mode {
                ExecutionMode::Developer(_) => Authorization::Allow,
                ExecutionMode::EndUser(_) => Authorization::Deny,
            }
        }
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => authorize_pragma(mode, pragma_name, pragma_value),
        AuthAction::CreateVtable { module_name, .. } => match mode {
            ExecutionMode::Developer(_) if module_name.eq_ignore_ascii_case("fts5") => {
                Authorization::Allow
            }
            _ => Authorization::Deny,
        },
        action if is_schema_action(&action) => match mode {
            ExecutionMode::Developer(_) => Authorization::Allow,
            ExecutionMode::EndUser(_) => Authorization::Deny,
        },
        _ => Authorization::Allow,
    }
}

fn authorize_pragma(mode: &ExecutionMode, name: &str, value: Option<&str>) -> Authorization {
    let name = name.to_ascii_lowercase();
    let always_denied = matches!(
        name.as_str(),
        "writable_schema"
            | "trusted_schema"
            | "temp_store_directory"
            | "data_store_directory"
            | "load_extension"
            | "journal_mode"
            | "locking_mode"
    );
    if always_denied {
        return Authorization::Deny;
    }
    match mode {
        ExecutionMode::EndUser(_) => Authorization::Deny,
        ExecutionMode::Developer(_) if value.is_none() => Authorization::Allow,
        ExecutionMode::Developer(_) => match name.as_str() {
            "foreign_keys" | "defer_foreign_keys" | "recursive_triggers" => Authorization::Allow,
            _ => Authorization::Deny,
        },
    }
}

fn is_schema_action(action: &AuthAction<'_>) -> bool {
    matches!(
        action,
        AuthAction::AlterTable { .. }
            | AuthAction::CreateIndex { .. }
            | AuthAction::CreateTable { .. }
            | AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::CreateTrigger { .. }
            | AuthAction::CreateView { .. }
            | AuthAction::DropIndex { .. }
            | AuthAction::DropTable { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::DropTrigger { .. }
            | AuthAction::DropView { .. }
            | AuthAction::CreateVtable { .. }
            | AuthAction::DropVtable { .. }
    )
}

fn dangerous_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "load_extension"
            | "readfile"
            | "writefile"
            | "edit"
            | "fts3_tokenizer"
            | "zipfile"
            | "shell_add_schema"
            | "shell_module_schema"
    )
}

fn sqlite_schema_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("sqlite_schema") || name.eq_ignore_ascii_case("sqlite_master")
}

fn is_internal(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("__ffdb_")
}

fn is_public_auth_implementation(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "__ffdb_auth_uid" | "__ffdb_auth_role" | "__ffdb_auth_jwt" | "__ffdb_auth_claim"
    )
}

fn approved_generated_source(
    source: &str,
    approved_sources: &std::collections::BTreeSet<String>,
) -> bool {
    let source = source.to_ascii_lowercase();
    approved_sources.contains(&source)
}

fn creates_or_drops_protected(
    action: &AuthAction<'_>,
    approved_sources: &std::collections::BTreeSet<String>,
) -> bool {
    let protected =
        |name: &str| is_internal(name) || approved_sources.contains(&name.to_ascii_lowercase());
    match action {
        AuthAction::CreateTable { table_name }
        | AuthAction::CreateTempTable { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::DropTempTable { table_name } => protected(table_name),
        AuthAction::CreateView { view_name }
        | AuthAction::CreateTempView { view_name }
        | AuthAction::DropView { view_name }
        | AuthAction::DropTempView { view_name } => protected(view_name),
        AuthAction::CreateTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::CreateTempTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::DropTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::DropTempTrigger {
            trigger_name,
            table_name,
        } => protected(trigger_name) || protected(table_name),
        AuthAction::CreateIndex {
            index_name,
            table_name,
        }
        | AuthAction::CreateTempIndex {
            index_name,
            table_name,
        }
        | AuthAction::DropIndex {
            index_name,
            table_name,
        }
        | AuthAction::DropTempIndex {
            index_name,
            table_name,
        } => protected(index_name) || protected(table_name),
        AuthAction::CreateVtable { table_name, .. }
        | AuthAction::DropVtable { table_name, .. }
        | AuthAction::AlterTable { table_name, .. } => protected(table_name),
        _ => false,
    }
}
