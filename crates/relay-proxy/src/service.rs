use std::{
    convert::Infallible,
    net::IpAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use http::{HeaderValue, Request, Response, StatusCode, Uri, header};
use http_body::{Body, Frame};
use http_body_util::{BodyExt as _, Empty, Full, Limited, combinators::UnsyncBoxBody};
use hyper::body::Incoming;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use rand::Rng as _;
use relay_cache::{CacheRegistry, CachedResponse, ResponseCache};
use relay_config::{AuthPolicyConfig, CacheConfig, RateLimitConfig, RateLimitKey, RetryConfig};
use relay_core::GatewayError;
use relay_limits::{LocalRateLimiter, RedisRateLimiter};
use relay_observability::{Metrics, inject_context, set_parent_from_headers};
use relay_upstream::{CircuitState, PoolSelectionError, UpstreamLease};
use serde_json::json;
use tokio::time;
use tracing::{Instrument as _, info, info_span, warn};
use url::Url;
use uuid::Uuid;

use crate::{
    headers::{prepare_downstream_response, prepare_upstream_request},
    runtime::{RequestPlan, RuntimeStore},
};

const REQUEST_ID: &str = "x-request-id";

/// Error type carried by streaming proxy response bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Unified streaming response body returned for upstream and local responses.
pub type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;

type HttpsClient = Client<HttpsConnector<HttpConnector>, ProxyBody>;

/// Cloneable Hyper service implementing Relay's core proxy path.
#[derive(Clone)]
pub struct ProxyService {
    runtime: Arc<RuntimeStore>,
    client: HttpsClient,
    metrics: Metrics,
    request_timeout: Duration,
    local_limiter: Arc<LocalRateLimiter>,
    redis_limiter: Option<RedisRateLimiter>,
    cache_registry: Arc<CacheRegistry>,
}

impl ProxyService {
    /// Builds an HTTP/1.1 and HTTP/2 client with pooled HTTP and HTTPS connections.
    pub fn new(
        runtime: Arc<RuntimeStore>,
        metrics: Metrics,
        request_timeout: Duration,
        connect_timeout: Duration,
        local_limiter: Arc<LocalRateLimiter>,
        redis_limiter: Option<RedisRateLimiter>,
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
            local_limiter,
            redis_limiter,
            cache_registry: Arc::new(CacheRegistry::default()),
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
            identity = tracing::field::Empty,
            retries = tracing::field::Empty,
            rate_limit = tracing::field::Empty,
            circuit_state = tracing::field::Empty,
        );
        set_parent_from_headers(&span, request.headers());

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

    #[expect(
        clippy::too_many_lines,
        reason = "the linear request lifecycle keeps retry ownership and RAII permits explicit"
    )]
    async fn forward(
        &self,
        mut request: Request<Incoming>,
        client_ip: IpAddr,
        downstream_protocol: &'static str,
        started: Instant,
    ) -> Result<Response<ProxyBody>, GatewayError> {
        let runtime = self.runtime.load();
        if let Some(limit) = &runtime.config().gateway_rate_limit {
            self.enforce_rate_limit("gateway", limit, client_ip, None)
                .await?;
        }
        let plan = runtime.plan(&request)?;
        let request_method = request.method().as_str().to_owned();
        let original_uri = request.uri().clone();
        tracing::Span::current().record("route", plan.route.name.as_str());
        if request_body_exceeds_limit(&request, plan.route.max_body_bytes) {
            return Err(GatewayError::RequestTooLarge);
        }
        let mut api_key_limit_key = None;
        if let Some(policy) = &plan.route.authentication {
            let identity = runtime
                .authenticator()
                .authenticate(request.headers(), policy)
                .map_err(|_| GatewayError::Unauthorized)?;
            if let Some(subject) = identity.subject.as_deref() {
                tracing::Span::current().record("identity", subject);
            } else if let Some(fingerprint) = identity.api_key_fingerprint.as_deref() {
                tracing::Span::current().record("identity", fingerprint);
            }
            api_key_limit_key = identity.rate_limit_key();
            remove_consumed_credential(request.headers_mut(), policy);
        }
        if let Some(limit) = &plan.route.rate_limit {
            self.enforce_rate_limit(
                &plan.route.name,
                limit,
                client_ip,
                api_key_limit_key.as_deref(),
            )
            .await?;
        }
        let cache_read = cache_read_plan(&request, &plan, &original_uri, &self.cache_registry);
        if let Some((cache, key)) = &cache_read {
            if let Some(cached) = cache.get(key) {
                self.metrics.cache_event(&plan.route.name, "hit");
                return Ok(cached_response(
                    cached,
                    plan,
                    self.metrics.clone(),
                    started,
                    request_method,
                ));
            }
            self.metrics.cache_event(&plan.route.name, "miss");
        }
        let replayable = is_replayable(&request);
        let template = RequestTemplate::from_request(&request);
        let max_attempts = if replayable {
            plan.route.retry.max_attempts
        } else {
            1
        };
        let mut streaming_request = if replayable {
            None
        } else {
            Some(box_incoming_request(request, plan.route.max_body_bytes))
        };
        let mut attempt = 0;

        loop {
            attempt += 1;
            tracing::Span::current().record("retries", attempt - 1);
            let upstream = plan.pool.try_select().map_err(|error| match error {
                PoolSelectionError::NoHealthyEndpoint => GatewayError::NoHealthyUpstream,
                PoolSelectionError::Overloaded => GatewayError::Overloaded,
            })?;
            let endpoint = upstream.endpoint();
            let endpoint_name = endpoint.url().to_string();
            tracing::Span::current().record("upstream", endpoint_name.as_str());
            let mut upstream_request = if replayable {
                template.build()
            } else {
                streaming_request.take().ok_or(GatewayError::Internal)?
            };
            let authority = upstream_authority(endpoint.url()).ok_or(GatewayError::Internal)?;
            let uri = upstream_uri(endpoint.url(), upstream_request.uri())
                .map_err(|_| GatewayError::Internal)?;
            *upstream_request.uri_mut() = uri;
            prepare_upstream_request(
                upstream_request.headers_mut(),
                &authority,
                client_ip,
                downstream_protocol,
            )
            .map_err(|_| GatewayError::Internal)?;
            inject_context(upstream_request.headers_mut());
            tracing::Span::current().record(
                "circuit_state",
                circuit_state_name(plan.circuit.snapshot().state),
            );
            let circuit_permit = plan
                .circuit
                .try_acquire()
                .map_err(|_| GatewayError::CircuitOpen)?;
            tracing::Span::current().record(
                "circuit_state",
                circuit_state_name(plan.circuit.snapshot().state),
            );
            let upstream_started = Instant::now();
            let response =
                match time::timeout(self.request_timeout, self.client.request(upstream_request))
                    .await
                {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        circuit_permit.failure();
                        endpoint.record_passive_failure(3);
                        if error_chain_contains_length_limit(&error) {
                            return Err(GatewayError::RequestTooLarge);
                        }
                        self.metrics.upstream_failure(
                            plan.pool.name(),
                            &endpoint_name,
                            "transport",
                        );
                        warn!(attempt, %error, "upstream transport failed");
                        if attempt < max_attempts {
                            self.metrics.retry(&plan.route.name, "transport");
                            time::sleep(retry_delay(&plan.route.retry, attempt)).await;
                            continue;
                        }
                        return Err(GatewayError::UpstreamTransport);
                    }
                    Err(_) => {
                        circuit_permit.failure();
                        endpoint.record_passive_failure(3);
                        self.metrics
                            .upstream_failure(plan.pool.name(), &endpoint_name, "timeout");
                        warn!(attempt, "upstream request timed out");
                        if attempt < max_attempts {
                            self.metrics.retry(&plan.route.name, "timeout");
                            time::sleep(retry_delay(&plan.route.retry, attempt)).await;
                            continue;
                        }
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
            let retryable_status = plan.route.retry.statuses.contains(&status.as_u16());
            if retryable_status && attempt < max_attempts {
                circuit_permit.failure();
                self.metrics.retry(&plan.route.name, "status");
                drop(response);
                drop(upstream);
                time::sleep(retry_delay(&plan.route.retry, attempt)).await;
                continue;
            }
            if status.is_server_error() {
                circuit_permit.failure();
            } else {
                circuit_permit.success();
            }

            let (mut parts, body) = response.into_parts();
            prepare_downstream_response(&mut parts.headers);
            tracing::Span::current().record("status", status.as_u16());
            info!(
                status = status.as_u16(),
                attempt, "request headers completed"
            );
            let cache_write = cache_read.and_then(|(cache, key)| {
                cache_write_plan(
                    &plan.route.name,
                    cache,
                    key,
                    plan.route.cache.as_ref()?,
                    &parts,
                    &self.metrics,
                )
            });
            if plan.route.cache.is_some() {
                parts.headers.insert(
                    "x-relay-cache",
                    HeaderValue::from_static(if cache_write.is_some() {
                        "MISS"
                    } else {
                        "BYPASS"
                    }),
                );
            }
            let body = ObservedBody::new(
                body,
                plan,
                Some(upstream),
                self.metrics.clone(),
                started,
                status,
                request_method,
            );
            let body = if let Some(write) = cache_write {
                CachingBody::new(body, write)
                    .map_err(|error| -> BoxError { Box::new(error) })
                    .boxed_unsync()
            } else {
                body.map_err(|error| -> BoxError { Box::new(error) })
                    .boxed_unsync()
            };
            return Ok(Response::from_parts(parts, body));
        }
    }

    async fn enforce_rate_limit(
        &self,
        route: &str,
        config: &RateLimitConfig,
        client_ip: IpAddr,
        api_key: Option<&str>,
    ) -> Result<(), GatewayError> {
        let (scope, identity) = match config.key {
            RateLimitKey::Route => (
                if route == "gateway" {
                    "gateway"
                } else {
                    "route"
                },
                "global".to_owned(),
            ),
            RateLimitKey::ClientIp => ("client_ip", client_ip.to_string()),
            RateLimitKey::ApiKey => (
                "api_key",
                api_key.ok_or(GatewayError::Unauthorized)?.to_owned(),
            ),
        };
        let decision = if config.distributed {
            self.redis_limiter
                .as_ref()
                .ok_or(GatewayError::DependencyUnavailable)?
                .check(route, &identity, config)
                .await
                .map_err(|error| {
                    warn!(%error, "distributed rate limiter unavailable");
                    GatewayError::DependencyUnavailable
                })?
        } else {
            self.local_limiter
                .check(format!("{route}:{identity}"), config)
                .map_err(|_| GatewayError::Overloaded)?
        };
        if decision.allowed {
            tracing::Span::current().record("rate_limit", "allowed");
            Ok(())
        } else {
            tracing::Span::current().record("rate_limit", "rejected");
            self.metrics.rate_limited(route, scope);
            Err(GatewayError::RateLimited)
        }
    }
}

const fn circuit_state_name(state: CircuitState) -> &'static str {
    match state {
        CircuitState::Closed => "closed",
        CircuitState::Open => "open",
        CircuitState::HalfOpen => "half_open",
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

struct CacheWrite {
    cache: Arc<ResponseCache>,
    key: String,
    route: String,
    status: StatusCode,
    version: http::Version,
    headers: http::HeaderMap,
    ttl: Duration,
    max_body_bytes: usize,
    metrics: Metrics,
}

struct CachingBody<B> {
    inner: Pin<Box<B>>,
    write: Option<CacheWrite>,
    buffer: BytesMut,
}

impl<B> CachingBody<B> {
    fn new(body: B, write: CacheWrite) -> Self {
        Self {
            inner: Box::pin(body),
            buffer: BytesMut::with_capacity(write.max_body_bytes.min(64 * 1024)),
            write: Some(write),
        }
    }

    fn abandon(&mut self, outcome: &str) {
        if let Some(write) = self.write.take() {
            write.metrics.cache_event(&write.route, outcome);
        }
        self.buffer.clear();
    }

    fn finish(&mut self) {
        if let Some(write) = self.write.take() {
            let cached = CachedResponse::new(
                write.status,
                write.version,
                write.headers,
                self.buffer.split().freeze(),
                write.ttl,
            );
            let outcome = if write.cache.insert(write.key, cached) {
                "stored"
            } else {
                "capacity"
            };
            write.metrics.cache_event(&write.route, outcome);
        }
    }
}

impl<B> Body for CachingBody<B>
where
    B: Body<Data = Bytes>,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let polled = self.inner.as_mut().poll_frame(context);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if frame.trailers_ref().is_some() {
                    self.abandon("trailers");
                } else if let (Some(data), Some(write)) = (frame.data_ref(), self.write.as_ref()) {
                    if self.buffer.len().saturating_add(data.len()) > write.max_body_bytes {
                        self.abandon("oversized");
                    } else {
                        self.buffer.extend_from_slice(data);
                    }
                }
                if self.inner.is_end_stream() {
                    self.finish();
                }
            }
            Poll::Ready(None) => {
                self.finish();
            }
            Poll::Pending | Poll::Ready(Some(Err(_))) => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for CachingBody<B> {
    fn drop(&mut self) {
        if let Some(write) = self.write.take() {
            write.metrics.cache_event(&write.route, "aborted");
        }
    }
}

fn cache_read_plan<B>(
    request: &Request<B>,
    plan: &RequestPlan,
    original_uri: &Uri,
    registry: &CacheRegistry,
) -> Option<(Arc<ResponseCache>, String)> {
    let policy = plan.route.cache.as_ref()?;
    if !matches!(*request.method(), http::Method::GET | http::Method::HEAD)
        || request.headers().contains_key(header::AUTHORIZATION)
        || request.headers().contains_key(header::COOKIE)
        || header_has_directive(request.headers(), header::CACHE_CONTROL, "no-cache")
        || header_has_directive(request.headers(), header::CACHE_CONTROL, "no-store")
        || header_has_directive(request.headers(), header::PRAGMA, "no-cache")
    {
        return None;
    }
    let capacity = usize::try_from(policy.max_entries).ok()?;
    let cache = registry.route(&plan.route.name, capacity);
    let key = format!(
        "{}\n{}\n{}",
        plan.route.name,
        request.method(),
        original_uri
    );
    Some((cache, key))
}

fn cache_write_plan(
    route: &str,
    cache: Arc<ResponseCache>,
    key: String,
    policy: &CacheConfig,
    parts: &http::response::Parts,
    metrics: &Metrics,
) -> Option<CacheWrite> {
    if parts.status != StatusCode::OK
        || parts.headers.contains_key(header::SET_COOKIE)
        || parts.headers.contains_key(header::VARY)
        || header_has_directive(&parts.headers, header::CACHE_CONTROL, "private")
        || header_has_directive(&parts.headers, header::CACHE_CONTROL, "no-store")
    {
        metrics.cache_event(route, "uncacheable");
        return None;
    }
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > policy.max_body_bytes)
    {
        metrics.cache_event(route, "oversized");
        return None;
    }
    let mut headers = parts.headers.clone();
    headers.remove(header::AGE);
    headers.remove("x-relay-cache");
    Some(CacheWrite {
        cache,
        key,
        route: route.to_owned(),
        status: parts.status,
        version: parts.version,
        headers,
        ttl: Duration::from_secs(policy.ttl_seconds),
        max_body_bytes: policy.max_body_bytes,
        metrics: metrics.clone(),
    })
}

fn cached_response(
    cached: CachedResponse,
    plan: RequestPlan,
    metrics: Metrics,
    started: Instant,
    method: String,
) -> Response<ProxyBody> {
    let age = cached.stored_at.elapsed().as_secs().to_string();
    let mut response = Response::new(
        ObservedBody::new(
            Full::new(cached.body),
            plan,
            None,
            metrics,
            started,
            cached.status,
            method,
        )
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed_unsync(),
    );
    *response.status_mut() = cached.status;
    *response.version_mut() = cached.version;
    *response.headers_mut() = cached.headers;
    response
        .headers_mut()
        .insert("x-relay-cache", HeaderValue::from_static("HIT"));
    if let Ok(value) = HeaderValue::from_str(&age) {
        response.headers_mut().insert(header::AGE, value);
    }
    response
}

fn header_has_directive(headers: &http::HeaderMap, name: header::HeaderName, needle: &str) -> bool {
    headers.get_all(name).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|directive| directive.eq_ignore_ascii_case(needle))
        })
    })
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
        upstream: Option<UpstreamLease>,
        metrics: Metrics,
        started: Instant,
        status: StatusCode,
        method: String,
    ) -> Self {
        Self {
            inner: Box::pin(body),
            plan: Some(plan),
            upstream,
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

#[derive(Clone)]
struct RequestTemplate {
    method: http::Method,
    uri: Uri,
    version: http::Version,
    headers: http::HeaderMap,
}

impl RequestTemplate {
    fn from_request<B>(request: &Request<B>) -> Self {
        Self {
            method: request.method().clone(),
            uri: request.uri().clone(),
            version: request.version(),
            headers: request.headers().clone(),
        }
    }

    fn build(&self) -> Request<ProxyBody> {
        let mut request = Request::new(empty_body());
        *request.method_mut() = self.method.clone();
        *request.uri_mut() = self.uri.clone();
        *request.version_mut() = self.version;
        *request.headers_mut() = self.headers.clone();
        request
    }
}

fn box_incoming_request(request: Request<Incoming>, max_body_bytes: u64) -> Request<ProxyBody> {
    let limit = usize::try_from(max_body_bytes).unwrap_or(usize::MAX);
    request.map(|body| Limited::new(body, limit).boxed_unsync())
}

fn request_body_exceeds_limit<B: Body>(request: &Request<B>, max_body_bytes: u64) -> bool {
    request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_body_bytes)
        || request
            .body()
            .size_hint()
            .upper()
            .is_some_and(|length| length > max_body_bytes)
}

fn error_chain_contains_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(current) = source {
        if current
            .downcast_ref::<http_body_util::LengthLimitError>()
            .is_some()
        {
            return true;
        }
        source = current.source();
    }
    false
}

fn empty_body() -> ProxyBody {
    Empty::<Bytes>::new()
        .map_err(|never: Infallible| -> BoxError { match never {} })
        .boxed_unsync()
}

fn is_replayable<B: Body>(request: &Request<B>) -> bool {
    matches!(
        *request.method(),
        http::Method::GET
            | http::Method::HEAD
            | http::Method::PUT
            | http::Method::DELETE
            | http::Method::OPTIONS
            | http::Method::TRACE
    ) && request.body().size_hint().exact() == Some(0)
}

fn retry_delay(config: &RetryConfig, failed_attempt: u32) -> Duration {
    let multiplier = 1_u64
        .checked_shl(failed_attempt.saturating_sub(1))
        .unwrap_or(u64::MAX);
    let exponential = config
        .base_backoff_ms
        .saturating_mul(multiplier)
        .min(config.max_backoff_ms);
    let jitter = if exponential == 0 {
        0
    } else {
        rand::thread_rng().gen_range(0..=exponential / 2)
    };
    Duration::from_millis(exponential.saturating_add(jitter))
}

fn request_id(value: Option<&HeaderValue>) -> String {
    value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(Uuid::new_v4)
        .to_string()
}

fn remove_consumed_credential(headers: &mut http::HeaderMap, policy: &AuthPolicyConfig) {
    match policy {
        AuthPolicyConfig::ApiKey { header } => {
            headers.remove(header);
        }
        AuthPolicyConfig::Jwt { .. } => {
            headers.remove(header::AUTHORIZATION);
        }
    }
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
    if matches!(error, GatewayError::RateLimited) {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
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

    #[test]
    fn retries_require_both_idempotent_method_and_empty_body() {
        let get = Request::get("/")
            .body(Empty::<Bytes>::new())
            .expect("request");
        let put = Request::put("/")
            .body(Empty::<Bytes>::new())
            .expect("request");
        let post = Request::post("/")
            .body(Empty::<Bytes>::new())
            .expect("request");
        let get_with_body = Request::get("/")
            .body(Full::new(Bytes::from_static(b"payload")))
            .expect("request");

        assert!(is_replayable(&get));
        assert!(is_replayable(&put));
        assert!(!is_replayable(&post));
        assert!(!is_replayable(&get_with_body));
    }

    #[test]
    fn declared_or_known_request_bodies_over_the_route_limit_are_rejected() {
        let declared = Request::post("/")
            .header(header::CONTENT_LENGTH, "9")
            .body(Empty::<Bytes>::new())
            .expect("request");
        let known = Request::post("/")
            .body(Full::new(Bytes::from_static(b"123456789")))
            .expect("request");

        assert!(request_body_exceeds_limit(&declared, 8));
        assert!(request_body_exceeds_limit(&known, 8));
        assert!(!request_body_exceeds_limit(&known, 9));
    }

    #[tokio::test]
    async fn streaming_limit_errors_are_recognized_through_the_error_chain() {
        let error = Limited::new(Full::new(Bytes::from_static(b"oversized")), 3)
            .collect()
            .await
            .expect_err("body must exceed its limit");

        assert!(error_chain_contains_length_limit(error.as_ref()));
    }

    #[test]
    fn retry_backoff_is_exponential_capped_and_jittered() {
        let config = RetryConfig {
            max_attempts: 5,
            base_backoff_ms: 100,
            max_backoff_ms: 250,
            statuses: vec![503],
        };

        let first = retry_delay(&config, 1);
        let second = retry_delay(&config, 2);
        let capped = retry_delay(&config, 8);

        assert!((100..=150).contains(&first.as_millis()));
        assert!((200..=300).contains(&second.as_millis()));
        assert!((250..=375).contains(&capped.as_millis()));
    }

    #[test]
    fn rate_limit_errors_include_retry_guidance() {
        let response = gateway_error_response(&GatewayError::RateLimited, "request-123");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    }

    fn cache_write(cache: Arc<ResponseCache>, max_body_bytes: usize) -> CacheWrite {
        CacheWrite {
            cache,
            key: "route\nGET\n/items".to_owned(),
            route: "route".to_owned(),
            status: StatusCode::OK,
            version: http::Version::HTTP_11,
            headers: http::HeaderMap::new(),
            ttl: Duration::from_secs(60),
            max_body_bytes,
            metrics: Metrics::new().expect("metrics"),
        }
    }

    #[tokio::test]
    async fn cache_is_populated_only_after_a_complete_body() {
        let cache = Arc::new(ResponseCache::new(1));
        let body = CachingBody::new(
            Full::new(Bytes::from_static(b"complete")),
            cache_write(Arc::clone(&cache), 64),
        );

        let received = body.collect().await.expect("infallible body").to_bytes();
        let cached = cache.get("route\nGET\n/items").expect("cached response");

        assert_eq!(received, Bytes::from_static(b"complete"));
        assert_eq!(cached.body, received);
    }

    #[tokio::test]
    async fn oversized_streams_continue_downstream_without_being_cached() {
        let cache = Arc::new(ResponseCache::new(1));
        let body = CachingBody::new(
            Full::new(Bytes::from_static(b"too large")),
            cache_write(Arc::clone(&cache), 3),
        );

        let received = body.collect().await.expect("infallible body").to_bytes();

        assert_eq!(received, Bytes::from_static(b"too large"));
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_control_directives_are_case_insensitive_and_tokenized() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, No-Store"),
        );

        assert!(header_has_directive(
            &headers,
            header::CACHE_CONTROL,
            "no-store"
        ));
        assert!(!header_has_directive(
            &headers,
            header::CACHE_CONTROL,
            "private"
        ));
    }
}
