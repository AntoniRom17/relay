//! Configurable HTTP upstream used by Relay's demos and operations exercises.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context as _;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    response::{IntoResponse as _, Response},
    routing::get,
};
use clap::Parser;
use http::{HeaderMap, Request, StatusCode};
use serde_json::json;
use tracing::info;

const MAX_ECHO_BODY: usize = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "relay-demo",
    about = "Mock upstream with latency and failure injection"
)]
struct Args {
    /// Address on which the upstream listens.
    #[arg(long, default_value = "127.0.0.1:9001")]
    listen: SocketAddr,
    /// Stable name returned in every response.
    #[arg(long, default_value = "upstream-a")]
    name: String,
    /// Delay applied before every non-health response.
    #[arg(long, default_value_t = 0)]
    delay_ms: u64,
    /// Fail every Nth non-health request; zero disables failures.
    #[arg(long, default_value_t = 0)]
    fail_every: u64,
    /// Status emitted for injected failures.
    #[arg(long, default_value_t = 503)]
    failure_status: u16,
    /// Make the active health endpoint return 503.
    #[arg(long, default_value_t = false)]
    unhealthy: bool,
}

#[derive(Debug)]
struct AppState {
    name: String,
    delay: Duration,
    fail_every: u64,
    failure_status: StatusCode,
    unhealthy: bool,
    requests: AtomicU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .compact()
        .with_target(false)
        .init();
    let args = Args::parse();
    let failure_status = StatusCode::from_u16(args.failure_status)
        .context("--failure-status must be a valid HTTP status")?;
    let state = Arc::new(AppState {
        name: args.name,
        delay: Duration::from_millis(args.delay_ms),
        fail_every: args.fail_every,
        failure_status,
        unhealthy: args.unhealthy,
        requests: AtomicU64::new(0),
    });
    let app = Router::new()
        .route("/health", get(health))
        .fallback(echo)
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind demo upstream {}", args.listen))?;
    info!(address = %args.listen, upstream = %state.name, "demo upstream started");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("demo upstream server failed")
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    if state.unhealthy {
        (StatusCode::SERVICE_UNAVAILABLE, "unhealthy").into_response()
    } else {
        (StatusCode::OK, "healthy").into_response()
    }
}

async fn echo(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }
    let sequence = state.requests.fetch_add(1, Ordering::AcqRel) + 1;
    if state.fail_every != 0 && sequence.is_multiple_of(state.fail_every) {
        return (
            state.failure_status,
            axum::Json(json!({
                "upstream": state.name,
                "sequence": sequence,
                "injected_failure": true,
            })),
        )
            .into_response();
    }

    let (parts, body) = request.into_parts();
    let request_id = header(&parts.headers, "x-request-id");
    let traceparent = header(&parts.headers, "traceparent");
    match to_bytes(body, MAX_ECHO_BODY).await {
        Ok(body) => axum::Json(json!({
            "upstream": state.name,
            "sequence": sequence,
            "method": parts.method.as_str(),
            "uri": parts.uri.to_string(),
            "body": String::from_utf8_lossy(&body),
            "request_id": request_id,
            "traceparent": traceparent,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            axum::Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
