//! PostgreSQL-backed control-plane registry.
//!
//! Project provisioning is deliberately one transaction: deferred schema
//! constraints make a project, its single database, and its single current route
//! an indivisible committed unit.

mod developer_auth;

pub use developer_auth::{
    PgPlatformSessionStore, PgPlatformUserRepository, PlatformAuthError, PlatformAuthService,
    PlatformSessionIdentity, PlatformSessionIssue, PlatformSessionRecord, PlatformSessionRotation,
    PlatformSessionStore, PlatformUserRecord, PlatformUserRepository,
};

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ffdb_database_router::{DatabaseRouter, RoutingError};
use ffdb_protocol::{
    CreateOrganizationRequest, CreateProjectRequest, DatabaseId, DatabaseRoute, NodeId,
    OrganizationId, OrganizationRole, OrganizationSummary, ProjectId, ProjectLifecycleState,
    ProjectSummary, UserId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ProvisionedProject {
    pub project: ProjectSummary,
    pub route: DatabaseRoute,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("registry input is invalid")]
    InvalidInput,
    #[error("registry resource was not found")]
    NotFound,
    #[error("registry resource already exists")]
    Conflict,
    #[error("project is unavailable")]
    Unavailable,
    #[error("registry operation is forbidden")]
    Forbidden,
    #[error("organization project allowance is exhausted")]
    BillingLimit,
    #[error("database route generation is stale")]
    StaleGeneration,
    #[error("control-plane metadata is inconsistent")]
    Inconsistent,
    #[error("control-plane datastore is unavailable")]
    DatastoreUnavailable,
}

#[async_trait]
pub trait Registry: Send + Sync {
    async fn create_organization(
        &self,
        request: CreateOrganizationRequest,
        owner: UserId,
        now_ms: i64,
    ) -> Result<OrganizationSummary, RegistryError>;

    async fn register_node(&self, node_id: NodeId, name: &str) -> Result<(), RegistryError>;

    async fn list_organizations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<OrganizationSummary>, RegistryError>;

    async fn list_projects(
        &self,
        organization_id: OrganizationId,
        user_id: UserId,
    ) -> Result<Vec<ProjectSummary>, RegistryError>;

    async fn create_project(
        &self,
        request: CreateProjectRequest,
        node_id: NodeId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError>;

    /// Public management path. The registry verifies an owner/admin membership
    /// before provisioning in the same control-plane operation.
    async fn create_project_authorized(
        &self,
        request: CreateProjectRequest,
        node_id: NodeId,
        actor: UserId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError>;

    async fn resolve_route(&self, project_id: ProjectId) -> Result<DatabaseRoute, RegistryError>;

    async fn reroute(
        &self,
        project_id: ProjectId,
        expected_generation: u64,
        node_id: NodeId,
    ) -> Result<DatabaseRoute, RegistryError>;

    async fn set_project_state(
        &self,
        project_id: ProjectId,
        state: ProjectLifecycleState,
    ) -> Result<(), RegistryError>;
}

#[derive(Clone, Debug)]
struct ProjectRecord {
    summary: ProjectSummary,
    database_id: DatabaseId,
}

#[derive(Debug, Default)]
struct MemoryState {
    organizations: HashMap<OrganizationId, OrganizationSummary>,
    organization_slugs: HashMap<String, OrganizationId>,
    memberships: HashMap<(OrganizationId, UserId), OrganizationRole>,
    projects: HashMap<ProjectId, ProjectRecord>,
    project_slugs: HashMap<(OrganizationId, String), ProjectId>,
    database_by_project: HashMap<ProjectId, DatabaseId>,
    routes: HashMap<ProjectId, DatabaseRoute>,
    nodes: HashMap<NodeId, String>,
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryRegistry {
    state: Arc<RwLock<MemoryState>>,
}

#[async_trait]
impl Registry for InMemoryRegistry {
    async fn create_organization(
        &self,
        request: CreateOrganizationRequest,
        owner: UserId,
        now_ms: i64,
    ) -> Result<OrganizationSummary, RegistryError> {
        validate_slug(&request.slug)?;
        validate_name(&request.name)?;
        let mut state = self.state.write().await;
        if state.organization_slugs.contains_key(&request.slug) {
            return Err(RegistryError::Conflict);
        }
        let summary = OrganizationSummary {
            id: OrganizationId::new(),
            name: request.name,
            slug: request.slug.clone(),
            role: OrganizationRole::Owner,
            created_at_ms: now_ms,
        };
        state.organization_slugs.insert(request.slug, summary.id);
        state
            .memberships
            .insert((summary.id, owner), OrganizationRole::Owner);
        state.organizations.insert(summary.id, summary.clone());
        Ok(summary)
    }

    async fn register_node(&self, node_id: NodeId, name: &str) -> Result<(), RegistryError> {
        validate_name(name)?;
        let mut state = self.state.write().await;
        if state
            .nodes
            .get(&node_id)
            .is_some_and(|current| current == name)
        {
            return Ok(());
        }
        if state.nodes.values().any(|current| current == name) {
            return Err(RegistryError::Conflict);
        }
        if state.nodes.contains_key(&node_id) {
            return Err(RegistryError::Conflict);
        }
        state.nodes.insert(node_id, name.to_owned());
        Ok(())
    }

    async fn list_organizations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<OrganizationSummary>, RegistryError> {
        let state = self.state.read().await;
        let mut organizations = state
            .memberships
            .iter()
            .filter_map(|((organization_id, member_id), role)| {
                if *member_id != user_id {
                    return None;
                }
                let mut organization = state.organizations.get(organization_id)?.clone();
                organization.role = *role;
                Some(organization)
            })
            .collect::<Vec<_>>();
        organizations.sort_by(|left, right| left.slug.cmp(&right.slug));
        Ok(organizations)
    }

    async fn list_projects(
        &self,
        organization_id: OrganizationId,
        user_id: UserId,
    ) -> Result<Vec<ProjectSummary>, RegistryError> {
        let state = self.state.read().await;
        if !state.memberships.contains_key(&(organization_id, user_id)) {
            return Err(RegistryError::Forbidden);
        }
        let mut projects = state
            .projects
            .values()
            .filter(|project| project.summary.organization_id == organization_id)
            .map(|project| project.summary.clone())
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| left.slug.cmp(&right.slug));
        Ok(projects)
    }

    async fn create_project(
        &self,
        request: CreateProjectRequest,
        node_id: NodeId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError> {
        validate_slug(&request.slug)?;
        validate_name(&request.name)?;
        let region = validate_region(request.region.as_deref().unwrap_or("local"))?.to_owned();
        let mut state = self.state.write().await;
        if !state.organizations.contains_key(&request.organization_id)
            || !state.nodes.contains_key(&node_id)
        {
            return Err(RegistryError::NotFound);
        }
        let project_count = state
            .projects
            .values()
            .filter(|project| {
                project.summary.organization_id == request.organization_id
                    && project.summary.state != ProjectLifecycleState::Deleted
            })
            .count();
        if project_count >= 2 {
            return Err(RegistryError::BillingLimit);
        }
        let slug_key = (request.organization_id, request.slug.clone());
        if state.project_slugs.contains_key(&slug_key) {
            return Err(RegistryError::Conflict);
        }

        let project_id = ProjectId::new();
        let database_id = DatabaseId::new();
        let summary = ProjectSummary {
            id: project_id,
            organization_id: request.organization_id,
            name: request.name,
            slug: request.slug,
            region,
            state: ProjectLifecycleState::Provisioning,
            schema_version: 0,
            created_at_ms: now_ms,
        };
        let route = DatabaseRoute {
            project_id,
            database_id,
            node_id,
            generation: 1,
        };
        state.project_slugs.insert(slug_key, project_id);
        state.projects.insert(
            project_id,
            ProjectRecord {
                summary: summary.clone(),
                database_id,
            },
        );
        state.database_by_project.insert(project_id, database_id);
        state.routes.insert(project_id, route.clone());
        Ok(ProvisionedProject {
            project: summary,
            route,
        })
    }

    async fn create_project_authorized(
        &self,
        request: CreateProjectRequest,
        node_id: NodeId,
        actor: UserId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError> {
        let permitted = {
            let state = self.state.read().await;
            matches!(
                state.memberships.get(&(request.organization_id, actor)),
                Some(OrganizationRole::Owner | OrganizationRole::Admin)
            )
        };
        if !permitted {
            return Err(RegistryError::Forbidden);
        }
        self.create_project(request, node_id, now_ms).await
    }

    async fn resolve_route(&self, project_id: ProjectId) -> Result<DatabaseRoute, RegistryError> {
        let state = self.state.read().await;
        let project = state
            .projects
            .get(&project_id)
            .ok_or(RegistryError::NotFound)?;
        if !matches!(
            project.summary.state,
            ProjectLifecycleState::Provisioning | ProjectLifecycleState::Active
        ) {
            return Err(RegistryError::Unavailable);
        }
        let database_id = state
            .database_by_project
            .get(&project_id)
            .ok_or(RegistryError::Inconsistent)?;
        let route = state
            .routes
            .get(&project_id)
            .ok_or(RegistryError::Inconsistent)?;
        if database_id != &project.database_id || route.database_id != *database_id {
            return Err(RegistryError::Inconsistent);
        }
        Ok(route.clone())
    }

    async fn reroute(
        &self,
        project_id: ProjectId,
        expected_generation: u64,
        node_id: NodeId,
    ) -> Result<DatabaseRoute, RegistryError> {
        let mut state = self.state.write().await;
        if !state.nodes.contains_key(&node_id) {
            return Err(RegistryError::NotFound);
        }
        let route = state
            .routes
            .get_mut(&project_id)
            .ok_or(RegistryError::NotFound)?;
        if route.generation != expected_generation {
            return Err(RegistryError::StaleGeneration);
        }
        route.generation = route
            .generation
            .checked_add(1)
            .ok_or(RegistryError::Inconsistent)?;
        route.node_id = node_id;
        Ok(route.clone())
    }

    async fn set_project_state(
        &self,
        project_id: ProjectId,
        new_state: ProjectLifecycleState,
    ) -> Result<(), RegistryError> {
        let mut state = self.state.write().await;
        let project = state
            .projects
            .get_mut(&project_id)
            .ok_or(RegistryError::NotFound)?;
        if !valid_transition(project.summary.state, new_state) {
            return Err(RegistryError::InvalidInput);
        }
        project.summary.state = new_state;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PgRegistry {
    pool: PgPool,
}

impl PgRegistry {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn create_project_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        request: CreateProjectRequest,
        node_id: NodeId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError> {
        validate_slug(&request.slug)?;
        validate_name(&request.name)?;
        let region = validate_region(request.region.as_deref().unwrap_or("local"))?.to_owned();
        let project_id = ProjectId::new();
        let database_id = DatabaseId::new();
        let route_id = Uuid::now_v7();
        sqlx::query("SET CONSTRAINTS ALL DEFERRED")
            .execute(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO projects \
             (id, organization_id, database_id, slug, display_name, region, lifecycle_state, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'provisioning', to_timestamp($7::double precision / 1000), to_timestamp($7::double precision / 1000))",
        )
        .bind(project_id.0)
        .bind(request.organization_id.0)
        .bind(database_id.0)
        .bind(&request.slug)
        .bind(&request.name)
        .bind(&region)
        .bind(now_ms)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO project_databases \
             (id, project_id, route_id, lifecycle_state, created_at, updated_at) \
             VALUES ($1, $2, $3, 'provisioning', to_timestamp($4::double precision / 1000), to_timestamp($4::double precision / 1000))",
        )
        .bind(database_id.0)
        .bind(project_id.0)
        .bind(route_id)
        .bind(now_ms)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO database_routes (id, project_id, database_id, node_id, generation, assigned_at) \
             VALUES ($1, $2, $3, $4, 1, to_timestamp($5::double precision / 1000))",
        )
        .bind(route_id)
        .bind(project_id.0)
        .bind(database_id.0)
        .bind(node_id.0)
        .bind(now_ms)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        Ok(ProvisionedProject {
            project: ProjectSummary {
                id: project_id,
                organization_id: request.organization_id,
                name: request.name,
                slug: request.slug,
                region,
                state: ProjectLifecycleState::Provisioning,
                schema_version: 0,
                created_at_ms: now_ms,
            },
            route: DatabaseRoute {
                project_id,
                database_id,
                node_id,
                generation: 1,
            },
        })
    }

    async fn project_lock(
        transaction: &mut Transaction<'_, Postgres>,
        project_id: ProjectId,
    ) -> Result<(), RegistryError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(project_id.0)
            .execute(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        Ok(())
    }

    async fn enforce_project_allowance(
        transaction: &mut Transaction<'_, Postgres>,
        organization_id: OrganizationId,
    ) -> Result<(), RegistryError> {
        let organization_exists: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM organizations WHERE id=$1 AND disabled_at IS NULL FOR UPDATE",
        )
        .bind(organization_id.0)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if organization_exists.is_none() {
            return Err(RegistryError::NotFound);
        }
        let billing_enforced: bool = sqlx::query_scalar(
            "SELECT COALESCE((SELECT billing_enforcement_enabled FROM instance_settings \
             WHERE singleton=true),false)",
        )
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if !billing_enforced {
            return Ok(());
        }
        let exempt: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM organization_billing_exemptions \
             WHERE organization_id=$1)",
        )
        .bind(organization_id.0)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if exempt {
            return Ok(());
        }
        let paid_entitlement: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM organization_billing_accounts \
             WHERE organization_id=$1 AND tier IN ('pay_as_you_go','pro') \
             AND status IN ('active','trialing'))",
        )
        .bind(organization_id.0)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if paid_entitlement {
            return Ok(());
        }
        let free_limit: i32 = sqlx::query_scalar(
            "SELECT project_limit FROM billing_price_catalog WHERE tier='free' AND active=true",
        )
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?
        .ok_or(RegistryError::Inconsistent)?;
        let project_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM projects WHERE organization_id=$1 \
             AND lifecycle_state <> 'deleted'",
        )
        .bind(organization_id.0)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if project_count >= i64::from(free_limit) {
            return Err(RegistryError::BillingLimit);
        }
        Ok(())
    }
}

#[async_trait]
impl DatabaseRouter for PgRegistry {
    async fn resolve(&self, project_id: ProjectId) -> Result<DatabaseRoute, RoutingError> {
        Registry::resolve_route(self, project_id)
            .await
            .map_err(map_routing_error)
    }
}

#[async_trait]
impl Registry for PgRegistry {
    async fn create_organization(
        &self,
        request: CreateOrganizationRequest,
        owner: UserId,
        now_ms: i64,
    ) -> Result<OrganizationSummary, RegistryError> {
        validate_slug(&request.slug)?;
        validate_name(&request.name)?;
        let id = OrganizationId::new();
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO organizations (id, slug, display_name, created_at, updated_at) \
             VALUES ($1, $2, $3, to_timestamp($4::double precision / 1000), to_timestamp($4::double precision / 1000))",
        )
        .bind(id.0)
        .bind(&request.slug)
        .bind(&request.name)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO organization_memberships (organization_id, user_id, role, created_at) \
             VALUES ($1, $2, 'owner', to_timestamp($3::double precision / 1000))",
        )
        .bind(id.0)
        .bind(owner.0)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(OrganizationSummary {
            id,
            name: request.name,
            slug: request.slug,
            role: OrganizationRole::Owner,
            created_at_ms: now_ms,
        })
    }

    async fn register_node(&self, node_id: NodeId, name: &str) -> Result<(), RegistryError> {
        validate_name(name)?;
        let registered = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO nodes (id, name) VALUES ($1, $2) \
             ON CONFLICT (id) DO UPDATE SET name=nodes.name WHERE nodes.name=EXCLUDED.name \
             RETURNING id",
        )
        .bind(node_id.0)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        registered.map(|_| ()).ok_or(RegistryError::Conflict)
    }

    async fn list_organizations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<OrganizationSummary>, RegistryError> {
        let rows = sqlx::query(
            "SELECT o.id,o.display_name,o.slug,m.role, \
                    (extract(epoch FROM o.created_at)*1000)::bigint created_at_ms \
             FROM organization_memberships m JOIN organizations o ON o.id=m.organization_id \
             WHERE m.user_id=$1 AND o.disabled_at IS NULL ORDER BY o.slug",
        )
        .bind(user_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.into_iter()
            .map(|row| {
                let role: String = row
                    .try_get("role")
                    .map_err(|_| RegistryError::Inconsistent)?;
                Ok(OrganizationSummary {
                    id: OrganizationId(row.try_get("id").map_err(|_| RegistryError::Inconsistent)?),
                    name: row
                        .try_get("display_name")
                        .map_err(|_| RegistryError::Inconsistent)?,
                    slug: row
                        .try_get("slug")
                        .map_err(|_| RegistryError::Inconsistent)?,
                    role: parse_organization_role(&role)?,
                    created_at_ms: row
                        .try_get("created_at_ms")
                        .map_err(|_| RegistryError::Inconsistent)?,
                })
            })
            .collect()
    }

    async fn list_projects(
        &self,
        organization_id: OrganizationId,
        user_id: UserId,
    ) -> Result<Vec<ProjectSummary>, RegistryError> {
        let membership: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM organization_memberships \
             WHERE organization_id=$1 AND user_id=$2)",
        )
        .bind(organization_id.0)
        .bind(user_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx)?;
        if !membership {
            return Err(RegistryError::Forbidden);
        }
        let rows = sqlx::query(
            "SELECT p.id,p.organization_id,p.display_name,p.slug,p.region,p.lifecycle_state, \
                    d.schema_version,(extract(epoch FROM p.created_at)*1000)::bigint created_at_ms \
             FROM projects p JOIN project_databases d ON d.id=p.database_id AND d.project_id=p.id \
             JOIN organizations o ON o.id=p.organization_id \
             WHERE p.organization_id=$1 AND o.disabled_at IS NULL ORDER BY p.slug",
        )
        .bind(organization_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.into_iter().map(project_summary_from_row).collect()
    }

    async fn create_project(
        &self,
        request: CreateProjectRequest,
        node_id: NodeId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        Self::enforce_project_allowance(&mut transaction, request.organization_id).await?;
        let provisioned =
            Self::create_project_in_transaction(&mut transaction, request, node_id, now_ms).await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(provisioned)
    }

    async fn create_project_authorized(
        &self,
        request: CreateProjectRequest,
        node_id: NodeId,
        actor: UserId,
        now_ms: i64,
    ) -> Result<ProvisionedProject, RegistryError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        Self::enforce_project_allowance(&mut transaction, request.organization_id).await?;
        let role: Option<String> = sqlx::query_scalar(
            "SELECT m.role FROM organization_memberships m \
             JOIN organizations o ON o.id=m.organization_id \
             WHERE m.organization_id=$1 AND m.user_id=$2 AND o.disabled_at IS NULL \
             FOR SHARE OF m,o",
        )
        .bind(request.organization_id.0)
        .bind(actor.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        if !matches!(role.as_deref(), Some("owner" | "admin")) {
            return Err(RegistryError::Forbidden);
        }
        let provisioned =
            Self::create_project_in_transaction(&mut transaction, request, node_id, now_ms).await?;
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(provisioned)
    }

    async fn resolve_route(&self, project_id: ProjectId) -> Result<DatabaseRoute, RegistryError> {
        let row = sqlx::query(
            "SELECT p.database_id, p.lifecycle_state, d.lifecycle_state AS database_state, \
                    r.node_id, r.generation, n.lifecycle_state AS node_state, \
                    o.disabled_at IS NOT NULL AS organization_disabled \
             FROM projects p \
             JOIN organizations o ON o.id = p.organization_id \
             JOIN project_databases d ON d.project_id = p.id AND d.id = p.database_id \
             JOIN database_routes r ON r.id = d.route_id AND r.database_id = d.id \
             JOIN nodes n ON n.id = r.node_id \
             WHERE p.id = $1",
        )
        .bind(project_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or(RegistryError::NotFound)?;
        let project_state: String = row
            .try_get("lifecycle_state")
            .map_err(|_| RegistryError::Inconsistent)?;
        let database_state: String = row
            .try_get("database_state")
            .map_err(|_| RegistryError::Inconsistent)?;
        let node_state: String = row
            .try_get("node_state")
            .map_err(|_| RegistryError::Inconsistent)?;
        let organization_disabled: bool = row
            .try_get("organization_disabled")
            .map_err(|_| RegistryError::Inconsistent)?;
        if organization_disabled
            || !matches!(project_state.as_str(), "provisioning" | "active")
            || !matches!(database_state.as_str(), "provisioning" | "active")
            || node_state != "active"
        {
            return Err(RegistryError::Unavailable);
        }
        let generation: i64 = row
            .try_get("generation")
            .map_err(|_| RegistryError::Inconsistent)?;
        Ok(DatabaseRoute {
            project_id,
            database_id: DatabaseId(
                row.try_get("database_id")
                    .map_err(|_| RegistryError::Inconsistent)?,
            ),
            node_id: NodeId(
                row.try_get("node_id")
                    .map_err(|_| RegistryError::Inconsistent)?,
            ),
            generation: u64::try_from(generation).map_err(|_| RegistryError::Inconsistent)?,
        })
    }

    async fn reroute(
        &self,
        project_id: ProjectId,
        expected_generation: u64,
        node_id: NodeId,
    ) -> Result<DatabaseRoute, RegistryError> {
        let expected =
            i64::try_from(expected_generation).map_err(|_| RegistryError::InvalidInput)?;
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        Self::project_lock(&mut transaction, project_id).await?;
        let row = sqlx::query(
            "UPDATE database_routes r SET node_id = $1, generation = r.generation + 1, assigned_at = now() \
             FROM nodes n WHERE r.project_id = $2 AND r.generation = $3 \
               AND n.id = $1 AND n.lifecycle_state = 'active' \
             RETURNING r.database_id, r.generation",
        )
        .bind(node_id.0)
        .bind(project_id.0)
        .bind(expected)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else {
            transaction.rollback().await.map_err(map_sqlx)?;
            return Err(RegistryError::StaleGeneration);
        };
        transaction.commit().await.map_err(map_sqlx)?;
        let generation: i64 = row
            .try_get("generation")
            .map_err(|_| RegistryError::Inconsistent)?;
        Ok(DatabaseRoute {
            project_id,
            database_id: DatabaseId(
                row.try_get("database_id")
                    .map_err(|_| RegistryError::Inconsistent)?,
            ),
            node_id,
            generation: u64::try_from(generation).map_err(|_| RegistryError::Inconsistent)?,
        })
    }

    async fn set_project_state(
        &self,
        project_id: ProjectId,
        state: ProjectLifecycleState,
    ) -> Result<(), RegistryError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        Self::project_lock(&mut transaction, project_id).await?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT lifecycle_state FROM projects WHERE id = $1 FOR UPDATE")
                .bind(project_id.0)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(map_sqlx)?;
        let current = current.ok_or(RegistryError::NotFound)?;
        let current = parse_project_state(&current)?;
        if !valid_transition(current, state) {
            return Err(RegistryError::InvalidInput);
        }
        sqlx::query("UPDATE projects SET lifecycle_state = $1, updated_at = now() WHERE id = $2")
            .bind(project_state_name(state))
            .bind(project_id.0)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        if let Some(database_state) = database_state_for_project(state) {
            sqlx::query(
                "UPDATE project_databases SET lifecycle_state=$1,updated_at=now() WHERE project_id=$2",
            )
            .bind(database_state)
            .bind(project_id.0)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
        }
        transaction.commit().await.map_err(map_sqlx)?;
        Ok(())
    }
}

fn map_sqlx(error: sqlx::Error) -> RegistryError {
    if let sqlx::Error::Database(database) = &error {
        return match database.code().as_deref() {
            Some("23505") => RegistryError::Conflict,
            Some("23503") => RegistryError::NotFound,
            Some("23514") | Some("22P02") => RegistryError::InvalidInput,
            _ => RegistryError::DatastoreUnavailable,
        };
    }
    RegistryError::DatastoreUnavailable
}

fn map_routing_error(error: RegistryError) -> RoutingError {
    match error {
        RegistryError::NotFound => RoutingError::NotFound,
        RegistryError::Unavailable | RegistryError::DatastoreUnavailable => {
            RoutingError::Unavailable
        }
        RegistryError::StaleGeneration => RoutingError::StaleGeneration,
        RegistryError::Inconsistent => RoutingError::Inconsistent,
        RegistryError::InvalidInput
        | RegistryError::Conflict
        | RegistryError::Forbidden
        | RegistryError::BillingLimit => RoutingError::Unavailable,
    }
}

fn validate_slug(slug: &str) -> Result<(), RegistryError> {
    if !(2..=63).contains(&slug.len())
        || !slug.starts_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(RegistryError::InvalidInput);
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), RegistryError> {
    if name.trim() != name || !(1..=128).contains(&name.len()) || name.chars().any(char::is_control)
    {
        return Err(RegistryError::InvalidInput);
    }
    Ok(())
}

fn validate_region(region: &str) -> Result<&str, RegistryError> {
    if region.is_empty()
        || region.len() > 64
        || !region.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(RegistryError::InvalidInput);
    }
    Ok(region)
}

fn valid_transition(from: ProjectLifecycleState, to: ProjectLifecycleState) -> bool {
    use ProjectLifecycleState as State;
    from == to
        || matches!(
            (from, to),
            (State::Provisioning, State::Active | State::Failed)
                | (
                    State::Active,
                    State::Suspended | State::Restoring | State::Deleting | State::Failed
                )
                | (State::Suspended, State::Active | State::Deleting)
                | (State::Restoring, State::Active | State::Failed)
                | (State::Deleting, State::Deleted | State::Failed)
                | (State::Failed, State::Provisioning | State::Deleting)
        )
}

fn project_state_name(state: ProjectLifecycleState) -> &'static str {
    match state {
        ProjectLifecycleState::Provisioning => "provisioning",
        ProjectLifecycleState::Active => "active",
        ProjectLifecycleState::Suspended => "suspended",
        ProjectLifecycleState::Restoring => "restoring",
        ProjectLifecycleState::Deleting => "deleting",
        ProjectLifecycleState::Deleted => "deleted",
        ProjectLifecycleState::Failed => "failed",
    }
}

fn database_state_for_project(state: ProjectLifecycleState) -> Option<&'static str> {
    match state {
        ProjectLifecycleState::Provisioning => Some("provisioning"),
        ProjectLifecycleState::Active => Some("active"),
        ProjectLifecycleState::Restoring => Some("restoring"),
        ProjectLifecycleState::Failed => Some("failed"),
        ProjectLifecycleState::Suspended
        | ProjectLifecycleState::Deleting
        | ProjectLifecycleState::Deleted => None,
    }
}

fn parse_project_state(value: &str) -> Result<ProjectLifecycleState, RegistryError> {
    match value {
        "provisioning" => Ok(ProjectLifecycleState::Provisioning),
        "active" => Ok(ProjectLifecycleState::Active),
        "suspended" => Ok(ProjectLifecycleState::Suspended),
        "restoring" => Ok(ProjectLifecycleState::Restoring),
        "deleting" => Ok(ProjectLifecycleState::Deleting),
        "deleted" => Ok(ProjectLifecycleState::Deleted),
        "failed" => Ok(ProjectLifecycleState::Failed),
        _ => Err(RegistryError::Inconsistent),
    }
}

fn parse_organization_role(value: &str) -> Result<OrganizationRole, RegistryError> {
    match value {
        "owner" => Ok(OrganizationRole::Owner),
        "admin" => Ok(OrganizationRole::Admin),
        "developer" => Ok(OrganizationRole::Developer),
        "viewer" => Ok(OrganizationRole::Viewer),
        _ => Err(RegistryError::Inconsistent),
    }
}

fn project_summary_from_row(row: sqlx::postgres::PgRow) -> Result<ProjectSummary, RegistryError> {
    let state: String = row
        .try_get("lifecycle_state")
        .map_err(|_| RegistryError::Inconsistent)?;
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(|_| RegistryError::Inconsistent)?;
    Ok(ProjectSummary {
        id: ProjectId(row.try_get("id").map_err(|_| RegistryError::Inconsistent)?),
        organization_id: OrganizationId(
            row.try_get("organization_id")
                .map_err(|_| RegistryError::Inconsistent)?,
        ),
        name: row
            .try_get("display_name")
            .map_err(|_| RegistryError::Inconsistent)?,
        slug: row
            .try_get("slug")
            .map_err(|_| RegistryError::Inconsistent)?,
        region: row
            .try_get("region")
            .map_err(|_| RegistryError::Inconsistent)?,
        state: parse_project_state(&state)?,
        schema_version: u64::try_from(schema_version).map_err(|_| RegistryError::Inconsistent)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|_| RegistryError::Inconsistent)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn provisioned() -> Result<(InMemoryRegistry, ProvisionedProject, NodeId), RegistryError>
    {
        let registry = InMemoryRegistry::default();
        let organization = registry
            .create_organization(
                CreateOrganizationRequest {
                    name: "Example".into(),
                    slug: "example-org".into(),
                },
                UserId::new(),
                1,
            )
            .await?;
        let node = NodeId::new();
        registry.register_node(node, "node-a").await?;
        let project = registry
            .create_project(
                CreateProjectRequest {
                    organization_id: organization.id,
                    name: "Project".into(),
                    slug: "project-one".into(),
                    region: Some("us_east".into()),
                },
                node,
                2,
            )
            .await?;
        Ok((registry, project, node))
    }

    #[tokio::test]
    async fn project_has_exactly_one_stable_database() -> Result<(), RegistryError> {
        let (registry, project, _) = provisioned().await?;
        let first = registry.resolve_route(project.project.id).await?;
        let second = registry.resolve_route(project.project.id).await?;
        assert_eq!(first.database_id, second.database_id);
        assert_eq!(first.database_id, project.route.database_id);
        Ok(())
    }

    #[tokio::test]
    async fn reroute_fences_stale_generations() -> Result<(), RegistryError> {
        let (registry, project, _) = provisioned().await?;
        let node_b = NodeId::new();
        registry.register_node(node_b, "node-b").await?;
        let new_route = registry.reroute(project.project.id, 1, node_b).await?;
        assert_eq!(new_route.generation, 2);
        assert_eq!(
            registry.reroute(project.project.id, 1, node_b).await,
            Err(RegistryError::StaleGeneration)
        );
        Ok(())
    }

    #[tokio::test]
    async fn management_lists_memberships_and_authorizes_project_creation()
    -> Result<(), RegistryError> {
        let registry = InMemoryRegistry::default();
        let owner = UserId::new();
        let outsider = UserId::new();
        let organization = registry
            .create_organization(
                CreateOrganizationRequest {
                    name: "Secure Org".into(),
                    slug: "secure-org".into(),
                },
                owner,
                1,
            )
            .await?;
        let node = NodeId::new();
        registry.register_node(node, "node-a").await?;
        registry.register_node(node, "node-a").await?;
        assert_eq!(
            registry.register_node(node, "node-b").await,
            Err(RegistryError::Conflict)
        );
        assert_eq!(registry.list_organizations(owner).await?.len(), 1);
        let request = CreateProjectRequest {
            organization_id: organization.id,
            name: "Authorized".into(),
            slug: "authorized-project".into(),
            region: None,
        };
        assert!(matches!(
            registry
                .create_project_authorized(request.clone(), node, outsider, 2)
                .await,
            Err(RegistryError::Forbidden)
        ));
        registry
            .create_project_authorized(request, node, owner, 2)
            .await?;
        assert_eq!(
            registry.list_projects(organization.id, owner).await?.len(),
            1
        );
        assert_eq!(
            registry.list_projects(organization.id, outsider).await,
            Err(RegistryError::Forbidden)
        );
        Ok(())
    }

    #[tokio::test]
    async fn free_organizations_are_limited_to_two_projects() -> Result<(), RegistryError> {
        let registry = InMemoryRegistry::default();
        let owner = UserId::new();
        let organization = registry
            .create_organization(
                CreateOrganizationRequest {
                    name: "Free Org".into(),
                    slug: "free-org".into(),
                },
                owner,
                1,
            )
            .await?;
        let node = NodeId::new();
        registry.register_node(node, "node-a").await?;
        for (name, slug) in [("One", "project-one"), ("Two", "project-two")] {
            registry
                .create_project_authorized(
                    CreateProjectRequest {
                        organization_id: organization.id,
                        name: name.into(),
                        slug: slug.into(),
                        region: None,
                    },
                    node,
                    owner,
                    2,
                )
                .await?;
        }
        assert!(matches!(
            registry
                .create_project_authorized(
                    CreateProjectRequest {
                        organization_id: organization.id,
                        name: "Three".into(),
                        slug: "project-three".into(),
                        region: None,
                    },
                    node,
                    owner,
                    3,
                )
                .await,
            Err(RegistryError::BillingLimit)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn disabled_organization_blocks_existing_routes_and_new_projects_until_reenabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPool::connect(&database_url).await?;
        let registry = PgRegistry::new(pool.clone());
        let owner = UserId::new();
        sqlx::query(
            "INSERT INTO platform_users (id,email,password_phc,email_verified_at) \
             VALUES ($1,$2,'test-only',now())",
        )
        .bind(owner.0)
        .bind(format!("route-disable-{owner}@example.test"))
        .execute(&pool)
        .await?;
        let slug_suffix = &owner.to_string()[..12];
        let organization = registry
            .create_organization(
                CreateOrganizationRequest {
                    name: "Route disable test".into(),
                    slug: format!("route-disable-{slug_suffix}"),
                },
                owner,
                1,
            )
            .await?;
        let node = NodeId::new();
        registry
            .register_node(node, &format!("route-disable-{node}"))
            .await?;
        let project = registry
            .create_project_authorized(
                CreateProjectRequest {
                    organization_id: organization.id,
                    name: "Existing credentials".into(),
                    slug: "existing-credentials".into(),
                    region: None,
                },
                node,
                owner,
                2,
            )
            .await?;
        let existing_route = registry.resolve_route(project.project.id).await?;

        sqlx::query("UPDATE organizations SET disabled_at=now() WHERE id=$1")
            .bind(organization.id.0)
            .execute(&pool)
            .await?;
        assert_eq!(
            registry.resolve_route(project.project.id).await,
            Err(RegistryError::Unavailable)
        );
        assert_eq!(
            DatabaseRouter::resolve(&registry, project.project.id).await,
            Err(RoutingError::Unavailable)
        );
        assert!(matches!(
            registry
                .create_project_authorized(
                    CreateProjectRequest {
                        organization_id: organization.id,
                        name: "Blocked".into(),
                        slug: "blocked-while-disabled".into(),
                        region: None,
                    },
                    node,
                    owner,
                    3,
                )
                .await,
            Err(RegistryError::NotFound | RegistryError::Forbidden)
        ));

        sqlx::query("UPDATE organizations SET disabled_at=NULL WHERE id=$1")
            .bind(organization.id.0)
            .execute(&pool)
            .await?;
        assert_eq!(
            registry.resolve_route(project.project.id).await?,
            existing_route
        );
        Ok(())
    }

    #[test]
    fn migration_encodes_exactly_one_database_and_route() {
        let migration =
            include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql");
        assert!(migration.contains("database_id uuid NOT NULL UNIQUE"));
        assert!(migration.contains("project_id uuid NOT NULL UNIQUE"));
        assert!(migration.contains("projects_database_fk"));
        assert!(migration.contains("project_databases_route_fk"));
        assert!(migration.contains("DEFERRABLE INITIALLY DEFERRED"));
    }
}
