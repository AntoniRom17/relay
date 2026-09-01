use std::{net::SocketAddr, sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use relay_core::Shutdown;
use relay_observability::Metrics;
use relay_proxy::RuntimeStore;
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use tracing::info;

/// Dedicated administrative server state.
#[derive(Debug, Clone)]
pub struct ControlPlane {
    state: Arc<ControlState>,
}

#[derive(Debug)]
struct ControlState {
    runtime: Arc<RuntimeStore>,
    metrics: Metrics,
    started_at: Instant,
}

impl ControlPlane {
    /// Creates a control plane over the currently installed runtime state.
    pub fn new(runtime: Arc<RuntimeStore>, metrics: Metrics) -> Self {
        Self {
            state: Arc::new(ControlState {
                runtime,
                metrics,
                started_at: Instant::now(),
            }),
        }
    }

    /// Builds the Axum router without binding a socket, enabling integration tests.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/ready", get(ready))
            .route("/metrics", get(metrics))
            .route("/admin/status", get(status))
            .route("/admin/routes", get(routes))
            .route("/admin/upstreams", get(upstreams))
            .route("/admin/upstreams/{id}", get(upstream))
            .route("/admin/config", get(config))
            .with_state(Arc::clone(&self.state))
    }

    /// Binds and serves the administrative listener until shutdown begins.
    pub async fn serve(
        self,
        address: SocketAddr,
        shutdown: Shutdown,
    ) -> Result<(), ControlPlaneError> {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|source| ControlPlaneError::Bind { address, source })?;
        info!(%address, "control-plane listener started");
        axum::serve(listener, self.router())
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
            .map_err(ControlPlaneError::Serve)?;
        info!(%address, "control-plane listener stopped");
        Ok(())
    }
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<Arc<ControlState>>) -> Response {
    let runtime = state.runtime.load();
    let unavailable = runtime
        .pools()
        .filter(|(_, pool)| {
            !pool
                .endpoints()
                .iter()
                .any(|endpoint| endpoint.is_healthy())
        })
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    if unavailable.is_empty() {
        (StatusCode::OK, Json(json!({ "status": "ready" }))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "unavailable_upstreams": unavailable })),
        )
            .into_response()
    }
}

async fn metrics(State(state): State<Arc<ControlState>>) -> Response {
    match state.metrics.encode() {
        Ok(body) => {
            let mut response = body.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            response
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "metrics_encoding_failed", "detail": error.to_string() })),
        )
            .into_response(),
    }
}

async fn status(State(state): State<Arc<ControlState>>) -> impl IntoResponse {
    let runtime = state.runtime.load();
    Json(StatusView {
        status: "running",
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        routes: runtime.config().routes.len(),
        upstreams: runtime.config().upstreams.len(),
    })
}

async fn routes(State(state): State<Arc<ControlState>>) -> impl IntoResponse {
    let runtime = state.runtime.load();
    Json(
        runtime
            .config()
            .routes
            .iter()
            .map(|route| RouteView {
                name: route.name.clone(),
                host: route.host.clone(),
                path_exact: route.path_exact.clone(),
                path_prefix: route.path_prefix.clone(),
                methods: route.methods.clone(),
                upstream: route.upstream.clone(),
                authentication: route.authentication.is_some(),
                rate_limited: route.rate_limit.is_some(),
            })
            .collect::<Vec<_>>(),
    )
}

async fn upstreams(State(state): State<Arc<ControlState>>) -> impl IntoResponse {
    let runtime = state.runtime.load();
    Json(
        runtime
            .pools()
            .map(|(_, pool)| UpstreamView::from_pool(pool))
            .collect::<Vec<_>>(),
    )
}

async fn upstream(Path(id): Path<String>, State(state): State<Arc<ControlState>>) -> Response {
    let runtime = state.runtime.load();
    runtime.pool(&id).map_or_else(
        || {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "upstream_not_found" })),
            )
                .into_response()
        },
        |pool| Json(UpstreamView::from_pool(pool)).into_response(),
    )
}

async fn config(State(state): State<Arc<ControlState>>) -> impl IntoResponse {
    let runtime = state.runtime.load();
    Json(ConfigView {
        listeners: runtime
            .config()
            .listeners
            .iter()
            .map(|listener| listener.address)
            .collect(),
        admin: runtime.config().admin.address,
        routes: runtime
            .config()
            .routes
            .iter()
            .map(|route| route.name.clone())
            .collect(),
        upstreams: runtime.config().upstreams.keys().cloned().collect(),
        redis_configured: runtime.config().redis.is_some(),
        api_key_count: runtime.config().authentication.api_key_sha256.len(),
    })
}

#[derive(Serialize)]
struct StatusView {
    status: &'static str,
    version: &'static str,
    uptime_seconds: u64,
    routes: usize,
    upstreams: usize,
}

#[derive(Serialize)]
struct RouteView {
    name: String,
    host: Option<String>,
    path_exact: Option<String>,
    path_prefix: Option<String>,
    methods: Vec<String>,
    upstream: String,
    authentication: bool,
    rate_limited: bool,
}

#[derive(Serialize)]
struct UpstreamView {
    name: String,
    available_capacity: usize,
    endpoints: Vec<EndpointView>,
}

impl UpstreamView {
    fn from_pool(pool: &relay_upstream::Pool) -> Self {
        let snapshot = pool.snapshot();
        Self {
            name: snapshot.name,
            available_capacity: snapshot.available_capacity,
            endpoints: snapshot
                .endpoints
                .into_iter()
                .map(|endpoint| EndpointView {
                    url: endpoint.url,
                    healthy: endpoint.healthy,
                    active_requests: endpoint.active_requests,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct EndpointView {
    url: String,
    healthy: bool,
    active_requests: usize,
}

#[derive(Serialize)]
struct ConfigView {
    listeners: Vec<SocketAddr>,
    admin: SocketAddr,
    routes: Vec<String>,
    upstreams: Vec<String>,
    redis_configured: bool,
    api_key_count: usize,
}

/// Administrative listener startup or server failure.
#[derive(Debug, Error)]
pub enum ControlPlaneError {
    /// The configured administrative address could not be bound.
    #[error("failed to bind control-plane listener {address}: {source}")]
    Bind {
        /// Requested socket address.
        address: SocketAddr,
        /// Operating-system error.
        source: std::io::Error,
    },
    /// Axum stopped because of an I/O error.
    #[error("control-plane server failed: {0}")]
    Serve(std::io::Error),
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt as _;
    use relay_config::parse;
    use relay_proxy::RuntimeState;
    use tower::ServiceExt as _;

    use super::*;

    const CONFIG: &str = r#"
listeners:
  - address: 127.0.0.1:8080
admin:
  enabled: true
  address: 127.0.0.1:9090
routes:
  - name: api
    path_prefix: /
    upstream: api
upstreams:
  api:
    endpoints:
      - url: http://127.0.0.1:9001
authentication:
  api_key_sha256:
    - aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
redis:
  url: redis://:super-secret@127.0.0.1:6379/0
"#;

    fn control() -> ControlPlane {
        let runtime = Arc::new(RuntimeStore::new(
            RuntimeState::build(parse(CONFIG).expect("valid config"))
                .expect("runtime should build"),
        ));
        ControlPlane::new(runtime, Metrics::new().expect("metrics should register"))
    }

    #[tokio::test]
    async fn health_and_metrics_are_available() {
        let app = control().router();
        let health = app
            .clone()
            .oneshot(
                Request::get("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let metrics = app
            .oneshot(
                Request::get("/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(metrics.status(), StatusCode::OK);
        assert!(
            metrics.headers()[header::CONTENT_TYPE]
                .to_str()
                .expect("ASCII")
                .contains("text/plain")
        );
    }

    #[tokio::test]
    async fn configuration_endpoint_never_serializes_secrets() {
        let response = control()
            .router()
            .oneshot(
                Request::get("/admin/config")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let text = String::from_utf8_lossy(&body);

        assert!(text.contains("redis_configured"));
        assert!(text.contains("api_key_count"));
        assert!(!text.contains("super-secret"));
        assert!(!text.contains("aaaaaaaaaaaaaaaa"));
    }
}
