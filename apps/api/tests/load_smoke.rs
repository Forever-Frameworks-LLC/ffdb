use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use ffdb_api::{ApiState, CredentialError, CredentialVerifier, router};
use ffdb_database_router::{DatabaseExecutor, DatabaseRouter, ExecutionError, RoutingError};
use ffdb_protocol::{
    AuthContext, DatabaseRoute, DeveloperPrincipal, DeveloperScope, ExecutionMode, ProjectId,
    ResourceLimits, SessionId, WorkerExecution, WorkerRequest,
};
use tower::ServiceExt as _;

#[derive(Debug)]
struct UnusedRouter;

#[async_trait]
impl DatabaseRouter for UnusedRouter {
    async fn resolve(&self, _project_id: ProjectId) -> Result<DatabaseRoute, RoutingError> {
        Err(RoutingError::NotFound)
    }
}

#[derive(Debug)]
struct UnusedExecutor;

#[async_trait]
impl DatabaseExecutor for UnusedExecutor {
    async fn execute(
        &self,
        _route: &DatabaseRoute,
        _request: WorkerRequest,
    ) -> Result<WorkerExecution, ExecutionError> {
        Err(ExecutionError::Unavailable)
    }
}

#[derive(Debug)]
struct UnusedCredentials;

#[async_trait]
impl CredentialVerifier for UnusedCredentials {
    async fn verify_query_credential(
        &self,
        _project_id: ProjectId,
        _bearer_token: &str,
    ) -> Result<ExecutionMode, CredentialError> {
        Err(CredentialError::Invalid)
    }

    async fn verify_developer_credential(
        &self,
        _project_id: ProjectId,
        _bearer_token: &str,
        _required_scope: DeveloperScope,
    ) -> Result<DeveloperPrincipal, CredentialError> {
        Err(CredentialError::Invalid)
    }

    async fn verify_end_user_credential(
        &self,
        _project_id: ProjectId,
        _bearer_token: &str,
    ) -> Result<AuthContext, CredentialError> {
        Err(CredentialError::Invalid)
    }

    async fn verify_end_user_session_credential(
        &self,
        _project_id: ProjectId,
        _bearer_token: &str,
    ) -> Result<(AuthContext, Option<SessionId>), CredentialError> {
        Err(CredentialError::Invalid)
    }
}

fn test_state() -> ApiState {
    ApiState {
        router: Arc::new(UnusedRouter),
        executor: Arc::new(UnusedExecutor),
        credentials: Arc::new(UnusedCredentials),
        limits: ResourceLimits::default(),
        metrics: None,
        observability: None,
        management: None,
        project_auth: None,
        storage: None,
        email: None,
        usage_metering: None,
        commerce: None,
        instance: None,
        host_updates: None,
        cors_allowed_origins: vec!["https://portal.example.test".to_owned()],
        trusted_proxy_cidrs: Vec::new(),
        rate_limiter: None,
        audit: Arc::new(ffdb_audit::InMemoryAuditSink::default()),
        readiness_pool: None,
    }
}

/// Bounded parallel load smoke for the real Axum middleware stack. This is not
/// a capacity benchmark; it catches accidental serialization, request-ID reuse,
/// and middleware failures under concurrent admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_path_handles_parallel_load_with_unique_request_ids()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    const REQUESTS: usize = 512;
    let application = router(test_state());
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..REQUESTS {
        let service = application.clone();
        tasks.spawn(async move {
            service
                .oneshot(Request::get("/healthz").body(Body::empty())?)
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })
        });
    }

    let mut request_ids = HashSet::with_capacity(REQUESTS);
    while let Some(result) = tasks.join_next().await {
        let response = result??;
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response
            .headers()
            .get("x-request-id")
            .ok_or("missing request ID")?
            .to_str()?
            .to_owned();
        assert!(request_ids.insert(request_id), "request ID was reused");
    }
    assert_eq!(request_ids.len(), REQUESTS);
    Ok(())
}
