//! Real-socket integration tests for Relay's streaming data plane.

use std::{
    fmt::Write as _,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body as AxumBody, to_bytes},
    extract::State,
    response::Response as AxumResponse,
};
use bytes::Bytes;
use http::{Request, StatusCode, header};
use http_body_util::{BodyExt as _, Full};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use relay_config::{GatewayConfig, parse};
use relay_core::Shutdown;
use relay_limits::LocalRateLimiter;
use relay_observability::Metrics;
use relay_proxy::{ProxyService, RuntimeState, RuntimeStore, serve_http_on};
use serde_json::json;
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Debug)]
struct UpstreamState {
    name: &'static str,
    fail: AtomicBool,
    requests: AtomicUsize,
}

async fn upstream(
    State(state): State<Arc<UpstreamState>>,
    request: Request<AxumBody>,
) -> AxumResponse {
    state.requests.fetch_add(1, Ordering::AcqRel);
    if state.fail.load(Ordering::Acquire) {
        return AxumResponse::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(AxumBody::from("injected failure"))
            .expect("static response");
    }
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1024 * 1024)
        .await
        .expect("integration request body");
    let payload = json!({
        "upstream": state.name,
        "method": parts.method.as_str(),
        "uri": parts.uri.to_string(),
        "body": String::from_utf8_lossy(&body),
        "request_id": parts.headers.get("x-request-id").and_then(|value| value.to_str().ok()),
        "traceparent": parts.headers.get("traceparent").and_then(|value| value.to_str().ok()),
    });
    AxumResponse::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "public, max-age=60")
        .body(AxumBody::from(payload.to_string()))
        .expect("static response")
}

async fn spawn_upstream(
    name: &'static str,
    fail: bool,
) -> (SocketAddr, Arc<UpstreamState>, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind upstream");
    let address = listener.local_addr().expect("upstream address");
    let state = Arc::new(UpstreamState {
        name,
        fail: AtomicBool::new(fail),
        requests: AtomicUsize::new(0),
    });
    let app = Router::new()
        .fallback(upstream)
        .with_state(Arc::clone(&state));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("upstream server");
    });
    (address, state, task)
}

struct RunningRelay {
    address: SocketAddr,
    shutdown: Shutdown,
    task: JoinHandle<Result<(), relay_proxy::ServeError>>,
}

impl RunningRelay {
    async fn start(endpoints: &[SocketAddr], cache: bool, attempts: u32) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind Relay");
        let address = listener.local_addr().expect("Relay address");
        let mut endpoint_yaml = String::new();
        for endpoint in endpoints {
            writeln!(endpoint_yaml, "      - url: http://{endpoint}")
                .expect("writing to a String cannot fail");
        }
        let cache_yaml = if cache {
            "    cache:\n      ttl_seconds: 60\n      max_entries: 32\n      max_body_bytes: 4096\n"
        } else {
            ""
        };
        let yaml = format!(
            "listeners:\n  - address: {address}\n    max_connections: 64\n    request_timeout_ms: 2000\n    max_header_bytes: 16384\nadmin:\n  enabled: false\n  address: 127.0.0.1:0\nroutes:\n  - name: integration\n    path_prefix: /\n    upstream: api\n    max_body_bytes: 1024\n    concurrency_limit: 32\n    retry:\n      max_attempts: {attempts}\n      base_backoff_ms: 0\n      max_backoff_ms: 0\n      statuses: [503]\n{cache_yaml}upstreams:\n  api:\n    load_balancing: round_robin\n    connect_timeout_ms: 500\n    concurrency_limit: 32\n    endpoints:\n{endpoint_yaml}redis: null\nshutdown_grace_ms: 1000\n"
        );
        let config: GatewayConfig = parse(&yaml).expect("valid test configuration");
        let listener_config = config.listeners[0].clone();
        let runtime = Arc::new(RuntimeStore::new(
            RuntimeState::build(config).expect("compiled runtime"),
        ));
        let metrics = Metrics::new().expect("metrics");
        let shutdown = Shutdown::new(Duration::from_secs(1));
        let proxy = ProxyService::new(
            runtime,
            metrics.clone(),
            Duration::from_secs(2),
            Duration::from_millis(500),
            Arc::new(LocalRateLimiter::new(1024)),
            None,
        );
        let server_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            serve_http_on(listener, listener_config, proxy, metrics, server_shutdown).await
        });
        Self {
            address,
            shutdown,
            task,
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .expect("Relay should drain")
            .expect("Relay task should join")
            .expect("Relay server should stop cleanly");
    }
}

fn client() -> Client<HttpConnector, Full<Bytes>> {
    Client::builder(TokioExecutor::new()).build(HttpConnector::new())
}

#[tokio::test]
async fn forwards_method_query_body_request_id_and_trace_context() {
    let (upstream_address, upstream, upstream_task) = spawn_upstream("echo", false).await;
    let relay = RunningRelay::start(&[upstream_address], false, 1).await;
    let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let request = Request::post(format!("http://{}/v1/items?limit=2", relay.address))
        .header("traceparent", traceparent)
        .body(Full::new(Bytes::from_static(b"stream me")))
        .expect("request");

    let response = client().request(request).await.expect("proxy response");
    let request_id = response.headers()["x-request-id"].clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON echo");

    assert_eq!(payload["method"], "POST");
    assert_eq!(payload["uri"], "/v1/items?limit=2");
    assert_eq!(payload["body"], "stream me");
    assert_eq!(payload["traceparent"], traceparent);
    assert_eq!(
        payload["request_id"],
        request_id.to_str().expect("request ID")
    );
    assert_eq!(upstream.requests.load(Ordering::Acquire), 1);

    relay.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn complete_get_responses_are_served_from_route_cache() {
    let (upstream_address, upstream, upstream_task) = spawn_upstream("cached", false).await;
    let relay = RunningRelay::start(&[upstream_address], true, 1).await;
    let uri = format!("http://{}/catalog?page=1", relay.address);

    let first = client()
        .request(
            Request::get(&uri)
                .body(Full::new(Bytes::new()))
                .expect("request"),
        )
        .await
        .expect("first response");
    assert_eq!(first.headers()["x-relay-cache"], "MISS");
    let first_body = first.into_body().collect().await.expect("body").to_bytes();

    let second = client()
        .request(
            Request::get(&uri)
                .body(Full::new(Bytes::new()))
                .expect("request"),
        )
        .await
        .expect("second response");
    assert_eq!(second.headers()["x-relay-cache"], "HIT");
    let second_body = second.into_body().collect().await.expect("body").to_bytes();

    assert_eq!(first_body, second_body);
    assert_eq!(upstream.requests.load(Ordering::Acquire), 1);

    relay.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn retryable_failures_move_to_the_next_pool_endpoint() {
    let (failed_address, failed, failed_task) = spawn_upstream("failed", true).await;
    let (healthy_address, healthy, healthy_task) = spawn_upstream("healthy", false).await;
    let relay = RunningRelay::start(&[failed_address, healthy_address], false, 2).await;
    let request = Request::get(format!("http://{}/retry", relay.address))
        .body(Full::new(Bytes::new()))
        .expect("request");

    let response = client().request(request).await.expect("proxy response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON echo");

    assert_eq!(payload["upstream"], "healthy");
    assert_eq!(failed.requests.load(Ordering::Acquire), 1);
    assert_eq!(healthy.requests.load(Ordering::Acquire), 1);

    relay.stop().await;
    failed_task.abort();
    healthy_task.abort();
}

#[tokio::test]
async fn route_body_limits_reject_before_contacting_the_upstream() {
    let (upstream_address, upstream, upstream_task) = spawn_upstream("unused", false).await;
    let relay = RunningRelay::start(&[upstream_address], false, 1).await;
    let request = Request::post(format!("http://{}/upload", relay.address))
        .body(Full::new(Bytes::from(vec![b'x'; 1025])))
        .expect("request");

    let response = client().request(request).await.expect("proxy response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("error body")
        .to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON error");

    assert_eq!(payload["error"]["code"], "request_too_large");
    assert_eq!(upstream.requests.load(Ordering::Acquire), 0);

    relay.stop().await;
    upstream_task.abort();
}
