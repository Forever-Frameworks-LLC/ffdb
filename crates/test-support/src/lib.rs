//! Deterministic cross-crate fixtures. This crate is never a production dependency.

use ffdb_protocol::{AuthContext, ProjectId, TokenId, UserId};
use serde_json::{Map, Value};

#[must_use]
pub fn auth_context(project_id: ProjectId, user_id: UserId, organization_id: &str) -> AuthContext {
    let claims = Map::from_iter([(
        "organization_id".into(),
        Value::String(organization_id.into()),
    )]);
    AuthContext {
        project_id,
        subject: user_id,
        role: "authenticated".into(),
        claims,
        token_id: TokenId::new(),
    }
}
