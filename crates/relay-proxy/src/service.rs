use std::{
    convert::Infallible,
    net::IpAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, Uri, header};
use http_body::{Body, Frame};
use http_body_util::{BodyExt as _, Full, combinators::UnsyncBoxBody};
use hyper::body::Incoming;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use relay_core::GatewayError;
use relay_observability::Metrics;
use relay_upstream::{PoolSelectionError, UpstreamLease};
use serde_json::json;
use tokio::time;
use tracing::{Instrument as _, info, info_span, warn};
use url::Url;
use uuid::Uuid;

use crate::{
    headers::{prepare_downstream_response, prepare_upstream_request},
    runtime::{RequestPlan, RuntimeState},
};

const REQUEST_ID: &str = "x-request-id";

/// Error type carried by streaming proxy response bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Unified streaming response body returned for upstream and local responses.
pub type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;

type HttpsClient = Client<HttpsConnector<HttpConnector>, Incoming>;

/// Cloneable Hyper service implementing Relay's core proxy path.
#[derive(Clone)]
pub struct ProxyService {
    runtime: Arc<RuntimeState>,
    client: HttpsClient,
    metrics: Metrics,
    request_timeout: Duration,
}

impl ProxyService {
    /// Builds an HTTP/1.1 and HTTP/2 client with pooled HTTP and HTTPS connections.
    pub fn new(
        runtime: Arc<RuntimeState>,
        metrics: Metrics,
        request_timeout: Duration,
        connect_timeout: Duration,
    ) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        connector.set_connect_timeout(Some(connect_timeout));
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(32)
            .build(https);
        Self {
            runtime,
            client,
            metrics,
            request_timeout,
        }
    }

    /// Routes and streams one downstream request to an upstream endpoint.
    pub async fn handle(
        &self,
        mut request: Request<Incoming>,
        client_ip: IpAddr,
        downstream_protocol: &'static str,
    ) -> Response<ProxyBody> {
        let request_id = request_id(request.headers().get(REQUEST_ID));
        let method = request.method().clone();
        let path = request
            .uri()
            .path_and_query()
            .map_or_else(|| "/".to_owned(), ToString::to_string);
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if let Ok(value) = HeaderValue::from_str(&request_id) {
            request.headers_mut().insert(REQUEST_ID, value);
        }

        let span = info_span!(
            "proxy.request",
            request_id,
            method = %method,
            host,
            path,
            route = tracing::field::Empty,
            upstream = tracing::field::Empty,
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        );

        async move {
            let started = Instant::now();
            let result = self
                .forward(request, client_ip, downstream_protocol, started)
                .await;
            let mut response = match result {
                Ok(response) => response,
                Err(error) => {
                    let status = error.status_code();
                    tracing::Span::current().record("status", status.as_u16());
                    self.metrics.observe_request(
                        "unmatched",
                        method.as_str(),
                        status.as_u16(),
                        started.elapsed().as_secs_f64(),
                    );
                    gateway_error_response(&error, &request_id)
                }
            };
            if let Ok(value) = HeaderValue::from_str(&request_id) {
                response.headers_mut().insert(REQUEST_ID, value);
            }
            response
        }
        .instrument(span)
        .await
    }

    async fn forward(
        &self,
        mut request: Request<Incoming>,
        client_ip: IpAddr,
        downstream_protocol: &'static str,
        started: Instant,
    ) -> Result<Response<ProxyBody>, GatewayError> {
        let plan = self.runtime.plan(&request)?;
        let request_method = request.method().as_str().to_owned();
        tracing::Span::current().record("route", plan.route.name.as_str());
        let circuit_permit = plan
            .circuit
            .try_acquire()
            .map_err(|_| GatewayError::CircuitOpen)?;
        let upstream = plan.pool.try_select().map_err(|error| match error {
            PoolSelectionError::NoHealthyEndpoint => GatewayError::NoHealthyUpstream,
            PoolSelectionError::Overloaded => GatewayError::Overloaded,
        })?;
        let endpoint = upstream.endpoint();
        let endpoint_name = endpoint.url().to_string();
        tracing::Span::current().record("upstream", endpoint_name.as_str());

        let authority = upstream_authority(endpoint.url()).ok_or(GatewayError::Internal)?;
        let uri =
            upstream_uri(endpoint.url(), request.uri()).map_err(|_| GatewayError::Internal)?;
        *request.uri_mut() = uri;
        prepare_upstream_request(
            request.headers_mut(),
            &authority,
            client_ip,
            downstream_protocol,
        )
        .map_err(|_| GatewayError::Internal)?;

        let upstream_started = Instant::now();
        let response = match time::timeout(self.request_timeout, self.client.request(request)).await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                circuit_permit.failure();
                endpoint.record_passive_failure(3);
                self.metrics
                    .upstream_failure(plan.pool.name(), &endpoint_name, "transport");
                warn!(%error, "upstream transport failed");
                return Err(GatewayError::UpstreamTransport);
            }
            Err(_) => {
                circuit_permit.failure();
                endpoint.record_passive_failure(3);
                self.metrics
                    .upstream_failure(plan.pool.name(), &endpoint_name, "timeout");
                warn!("upstream request timed out");
                return Err(GatewayError::UpstreamTimeout);
            }
        };
        let status = response.status();
        self.metrics.observe_upstream(
            plan.pool.name(),
            &endpoint_name,
            status.as_u16(),
            upstream_started.elapsed().as_secs_f64(),
        );
        if status.is_server_error() {
            circuit_permit.failure();
        } else {
            circuit_permit.success();
        }

        let (mut parts, body) = response.into_parts();
        prepare_downstream_response(&mut parts.headers);
        tracing::Span::current().record("status", status.as_u16());
        info!(status = status.as_u16(), "request headers completed");
        let body = ObservedBody::new(
            body,
            plan,
            upstream,
            self.metrics.clone(),
            started,
            status,
            request_method,
        )
        .map_err(|error| -> BoxError { Box::new(error) })
        .boxed_unsync();
        Ok(Response::from_parts(parts, body))
    }
}

impl std::fmt::Debug for ProxyService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProxyService")
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

struct ObservedBody<B> {
    inner: Pin<Box<B>>,
    plan: Option<RequestPlan>,
    upstream: Option<UpstreamLease>,
    metrics: Metrics,
    started: Instant,
    status: StatusCode,
    method: String,
}

impl<B> ObservedBody<B> {
    fn new(
        body: B,
        plan: RequestPlan,
        upstream: UpstreamLease,
        metrics: Metrics,
        started: Instant,
        status: StatusCode,
        method: String,
    ) -> Self {
        Self {
            inner: Box::pin(body),
            plan: Some(plan),
            upstream: Some(upstream),
            metrics,
            started,
            status,
            method,
        }
    }
}

impl<B> Body for ObservedBody<B>
where
    B: Body<Data = Bytes>,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.inner.as_mut().poll_frame(context)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for ObservedBody<B> {
    fn drop(&mut self) {
        if let Some(plan) = self.plan.take() {
            self.metrics.observe_request(
                &plan.route.name,
                &self.method,
                self.status.as_u16(),
                self.started.elapsed().as_secs_f64(),
            );
        }
        drop(self.upstream.take());
        tracing::Span::current()
            .record("latency_ms", self.started.elapsed().as_secs_f64() * 1_000.0);
    }
}

fn request_id(value: Option<&HeaderValue>) -> String {
    value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(Uuid::new_v4)
        .to_string()
}

fn upstream_uri(endpoint: &Url, original: &Uri) -> Result<Uri, http::uri::InvalidUri> {
    let origin = endpoint.origin().ascii_serialization();
    let base = endpoint.path().trim_end_matches('/');
    let path_and_query = original
        .path_and_query()
        .map_or("/", |value| value.as_str());
    format!("{origin}{base}{path_and_query}").parse()
}

fn upstream_authority(endpoint: &Url) -> Option<String> {
    let host = endpoint.host_str()?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Some(
        endpoint
            .port()
            .map_or(host.clone(), |port| format!("{host}:{port}")),
    )
}

fn gateway_error_response(error: &GatewayError, request_id: &str) -> Response<ProxyBody> {
    let payload = json!({
        "error": {
            "code": error.code(),
            "message": error.to_string(),
            "request_id": request_id,
        }
    });
    let body = Full::new(Bytes::from(payload.to_string()))
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = error.status_code();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt as _;

    use super::*;

    #[test]
    fn upstream_uri_preserves_path_and_query_under_base_path() {
        let endpoint: Url = "https://upstream.example:8443/base/"
            .parse()
            .expect("valid endpoint");
        let original: Uri = "/v1/items?limit=10".parse().expect("valid request URI");

        let rewritten = upstream_uri(&endpoint, &original).expect("valid upstream URI");

        assert_eq!(
            rewritten,
            "https://upstream.example:8443/base/v1/items?limit=10"
        );
        assert_eq!(
            upstream_authority(&endpoint).as_deref(),
            Some("upstream.example:8443")
        );
    }

    #[tokio::test]
    async fn local_errors_are_stable_json_without_internal_details() {
        let response = gateway_error_response(&GatewayError::UpstreamTransport, "request-123");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("local body is infallible")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).expect("body should be JSON");

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(value["error"]["code"], "upstream_transport");
        assert_eq!(value["error"]["request_id"], "request-123");
        assert!(!String::from_utf8_lossy(&body).contains("127.0.0.1"));
    }

    #[test]
    fn only_uuid_request_ids_are_propagated() {
        let valid = HeaderValue::from_static("67e55044-10b1-426f-9247-bb680e5fe0c8");
        let invalid = HeaderValue::from_static("attacker-controlled-log-value");

        assert_eq!(request_id(Some(&valid)), valid.to_str().expect("ASCII"));
        assert!(Uuid::parse_str(&request_id(Some(&invalid))).is_ok());
    }
}
