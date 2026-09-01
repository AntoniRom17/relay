use std::{collections::HashSet, fs, path::Path};

use http::{HeaderName, HeaderValue, Method};
use thiserror::Error;

use crate::{
    AuthPolicyConfig, GatewayConfig, LoadBalancingPolicy, RateLimitKey, RouteConfig, UpstreamConfig,
};

const MAX_LISTENERS: usize = 64;
const MAX_ROUTES: usize = 10_000;
const MAX_UPSTREAMS: usize = 10_000;
const MAX_ENDPOINTS_PER_POOL: usize = 1_024;
const MAX_WEIGHTED_SCHEDULE_SLOTS: u64 = 65_536;
const MAX_API_KEYS: usize = 10_000;
const MAX_JWT_PROVIDERS: usize = 1_000;
const MAX_CACHE_ENTRIES: u64 = 10_000;
const MAX_ROUTE_CACHE_BODY_BYTES: u64 = 512 * 1024 * 1024;

/// Failure to read, decode, or semantically validate configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Configuration could not be read from disk.
    #[error("failed to read configuration {path}: {source}")]
    Read {
        /// Requested file path.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// YAML did not match the typed configuration schema.
    #[error("invalid configuration syntax: {0}")]
    Parse(#[from] serde_yaml::Error),
    /// One or more cross-field constraints failed.
    #[error("configuration validation failed:\n  - {}", .0.join("\n  - "))]
    Validation(Vec<String>),
}

/// Reads and validates a YAML configuration file.
///
/// # Errors
///
/// Returns [`ConfigError::Read`] when the file cannot be read, or the same parse and validation
/// errors returned by [`parse`].
pub fn load_from_path(path: impl AsRef<Path>) -> Result<GatewayConfig, ConfigError> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse(&contents)
}

/// Parses and validates YAML configuration text.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] for schema or YAML failures and [`ConfigError::Validation`] for
/// semantic or cross-field violations.
pub fn parse(contents: &str) -> Result<GatewayConfig, ConfigError> {
    let config: GatewayConfig = serde_yaml::from_str(contents)?;
    validate(&config)?;
    Ok(config)
}

/// Validates cross-field invariants not expressible through Serde types.
///
/// # Errors
///
/// Returns [`ConfigError::Validation`] containing every independently discovered violation.
pub fn validate(config: &GatewayConfig) -> Result<(), ConfigError> {
    let mut problems = Vec::new();
    validate_top_level(config, &mut problems);
    validate_listeners(config, &mut problems);
    validate_routes(config, &mut problems);
    validate_upstreams(config, &mut problems);
    validate_authentication(config, &mut problems);

    if problems.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Validation(problems))
    }
}

fn validate_top_level(config: &GatewayConfig, problems: &mut Vec<String>) {
    if config.listeners.is_empty() {
        problems.push("listeners: at least one data-plane listener is required".to_owned());
    }
    if config.routes.is_empty() {
        problems.push("routes: at least one route is required".to_owned());
    }
    if config.listeners.len() > MAX_LISTENERS {
        problems.push(format!(
            "listeners: must not contain more than {MAX_LISTENERS} entries"
        ));
    }
    if config.routes.len() > MAX_ROUTES {
        problems.push(format!(
            "routes: must not contain more than {MAX_ROUTES} entries"
        ));
    }
    if config.upstreams.len() > MAX_UPSTREAMS {
        problems.push(format!(
            "upstreams: must not contain more than {MAX_UPSTREAMS} pools"
        ));
    }
    if config.shutdown_grace_ms == 0 {
        problems.push("shutdown_grace_ms: must be greater than zero".to_owned());
    }
    if let Some(limit) = &config.gateway_rate_limit {
        if limit.requests_per_second == 0 || limit.burst_capacity == 0 {
            problems.push(
                "gateway_rate_limit: rate and burst capacity must be greater than zero".to_owned(),
            );
        }
        if limit.distributed && config.redis.is_none() {
            problems.push(
                "gateway_rate_limit.distributed: requires a top-level redis section".to_owned(),
            );
        }
        if limit.key != RateLimitKey::Route {
            problems.push(
                "gateway_rate_limit.key: only route is valid for the aggregate gateway bucket"
                    .to_owned(),
            );
        }
    }
}

fn validate_listeners(config: &GatewayConfig, problems: &mut Vec<String>) {
    let mut listener_addresses = HashSet::new();
    for (index, listener) in config.listeners.iter().enumerate() {
        if !listener_addresses.insert(listener.address) {
            problems.push(format!(
                "listeners[{index}].address: duplicate listener address {}",
                listener.address
            ));
        }
        if listener.max_connections == 0 {
            problems.push(format!(
                "listeners[{index}].max_connections: must be greater than zero"
            ));
        }
        if listener.request_timeout_ms == 0 {
            problems.push(format!(
                "listeners[{index}].request_timeout_ms: must be greater than zero"
            ));
        }
        if listener.max_header_bytes < 1_024 {
            problems.push(format!(
                "listeners[{index}].max_header_bytes: must be at least 1024"
            ));
        }
    }
    if config.admin.enabled && listener_addresses.contains(&config.admin.address) {
        problems.push("admin.address: must not overlap a data-plane listener".to_owned());
    }
    if config.admin.enabled
        && !config.admin.address.ip().is_loopback()
        && !config.admin.allow_remote
    {
        problems.push(
            "admin.address: must use loopback unless admin.allow_remote is explicitly true"
                .to_owned(),
        );
    }
}

fn validate_routes(config: &GatewayConfig, problems: &mut Vec<String>) {
    let mut route_names = HashSet::new();
    for (index, route) in config.routes.iter().enumerate() {
        validate_route(config, index, route, &mut route_names, problems);
    }
}

fn validate_route(
    config: &GatewayConfig,
    index: usize,
    route: &RouteConfig,
    route_names: &mut HashSet<String>,
    problems: &mut Vec<String>,
) {
    let field = format!("routes[{index}]");
    if route.name.trim().is_empty() {
        problems.push(format!("{field}.name: must not be empty"));
    } else if !route_names.insert(route.name.clone()) {
        problems.push(format!(
            "{field}.name: duplicate route name `{}`",
            route.name
        ));
    }
    if route.path_exact.is_some() && route.path_prefix.is_some() {
        problems.push(format!(
            "{field}: path_exact and path_prefix are mutually exclusive"
        ));
    }
    validate_route_matchers(route, &field, problems);
    if !config.upstreams.contains_key(&route.upstream) {
        problems.push(format!(
            "{field}.upstream: unknown upstream pool `{}`",
            route.upstream
        ));
    }
    validate_retry(route, &field, problems);
    validate_route_policies(config, route, &field, problems);
    if route.max_body_bytes == 0 {
        problems.push(format!("{field}.max_body_bytes: must be greater than zero"));
    }
    if route.concurrency_limit == 0 {
        problems.push(format!(
            "{field}.concurrency_limit: must be greater than zero"
        ));
    }
}

fn validate_route_matchers(route: &RouteConfig, field: &str, problems: &mut Vec<String>) {
    for (path_field, path) in [
        ("path_exact", route.path_exact.as_deref()),
        ("path_prefix", route.path_prefix.as_deref()),
    ] {
        if let Some(path) = path
            && !path.starts_with('/')
        {
            problems.push(format!("{field}.{path_field}: must begin with `/`"));
        }
    }
    for method in &route.methods {
        if method.parse::<Method>().is_err() {
            problems.push(format!("{field}.methods: `{method}` is not an HTTP method"));
        }
    }
    for (name, value) in &route.headers {
        if name.parse::<HeaderName>().is_err() {
            problems.push(format!(
                "{field}.headers: `{name}` is not a valid header name"
            ));
        }
        if value.parse::<HeaderValue>().is_err() {
            problems.push(format!(
                "{field}.headers.{name}: value contains invalid header bytes"
            ));
        }
    }
}

fn validate_retry(route: &RouteConfig, field: &str, problems: &mut Vec<String>) {
    if route.retry.max_attempts == 0 {
        problems.push(format!("{field}.retry.max_attempts: must be at least 1"));
    }
    if route.retry.base_backoff_ms > route.retry.max_backoff_ms {
        problems.push(format!(
            "{field}.retry: base_backoff_ms must not exceed max_backoff_ms"
        ));
    }
    for status in &route.retry.statuses {
        if !(100..=599).contains(status) {
            problems.push(format!(
                "{field}.retry.statuses: {status} is outside the HTTP status range"
            ));
        }
    }
}

fn validate_route_policies(
    config: &GatewayConfig,
    route: &RouteConfig,
    field: &str,
    problems: &mut Vec<String>,
) {
    if let Some(limit) = &route.rate_limit {
        if limit.requests_per_second == 0 || limit.burst_capacity == 0 {
            problems.push(format!(
                "{field}.rate_limit: rate and burst capacity must be greater than zero"
            ));
        }
        if limit.distributed && config.redis.is_none() {
            problems.push(format!(
                "{field}.rate_limit.distributed: requires a top-level redis section"
            ));
        }
        if limit.key == RateLimitKey::ApiKey
            && !matches!(route.authentication, Some(AuthPolicyConfig::ApiKey { .. }))
        {
            problems.push(format!(
                "{field}.rate_limit.key: api_key scope requires API-key authentication on the route"
            ));
        }
    }
    if let Some(AuthPolicyConfig::Jwt { provider }) = &route.authentication
        && !config.authentication.jwt_providers.contains_key(provider)
    {
        problems.push(format!(
            "{field}.authentication.provider: unknown JWT provider `{provider}`"
        ));
    }
    validate_cache(route, field, problems);
}

fn validate_cache(route: &RouteConfig, field: &str, problems: &mut Vec<String>) {
    let Some(cache) = &route.cache else { return };
    if cache.ttl_seconds == 0 || cache.max_entries == 0 || cache.max_body_bytes == 0 {
        problems.push(format!(
            "{field}.cache: TTL, entry capacity, and body limit must be greater than zero"
        ));
    }
    if route.authentication.is_some() {
        problems.push(format!(
            "{field}.cache: authenticated routes are not cacheable without an explicit private-cache policy"
        ));
    }
    if cache.max_entries > MAX_CACHE_ENTRIES {
        problems.push(format!(
            "{field}.cache.max_entries: must not exceed {MAX_CACHE_ENTRIES}"
        ));
    }
    if cache.max_body_bytes > 16 * 1024 * 1024 {
        problems.push(format!(
            "{field}.cache.max_body_bytes: must not exceed 16777216"
        ));
    }
    let max_body_bytes = u64::try_from(cache.max_body_bytes).unwrap_or(u64::MAX);
    if cache.max_entries.saturating_mul(max_body_bytes) > MAX_ROUTE_CACHE_BODY_BYTES {
        problems.push(format!(
            "{field}.cache: max_entries multiplied by max_body_bytes must not exceed {MAX_ROUTE_CACHE_BODY_BYTES}"
        ));
    }
}

fn validate_upstreams(config: &GatewayConfig, problems: &mut Vec<String>) {
    for (name, upstream) in &config.upstreams {
        validate_upstream(name, upstream, problems);
    }
}

fn validate_upstream(name: &str, upstream: &UpstreamConfig, problems: &mut Vec<String>) {
    let field = format!("upstreams.{name}");
    if upstream.endpoints.is_empty() {
        problems.push(format!(
            "{field}.endpoints: at least one endpoint is required"
        ));
    }
    if upstream.endpoints.len() > MAX_ENDPOINTS_PER_POOL {
        problems.push(format!(
            "{field}.endpoints: must not contain more than {MAX_ENDPOINTS_PER_POOL} endpoints"
        ));
    }
    if upstream.connect_timeout_ms == 0 || upstream.concurrency_limit == 0 {
        problems.push(format!(
            "{field}: connect timeout and concurrency limit must be greater than zero"
        ));
    }
    let mut urls = HashSet::new();
    for (index, endpoint) in upstream.endpoints.iter().enumerate() {
        let endpoint_field = format!("{field}.endpoints[{index}]");
        if !matches!(endpoint.url.scheme(), "http" | "https") {
            problems.push(format!(
                "{endpoint_field}.url: scheme must be http or https"
            ));
        }
        if endpoint.url.host().is_none() {
            problems.push(format!("{endpoint_field}.url: host is required"));
        }
        if !endpoint.url.username().is_empty() || endpoint.url.password().is_some() {
            problems.push(format!(
                "{endpoint_field}.url: credentials are not permitted"
            ));
        }
        if endpoint.url.query().is_some() || endpoint.url.fragment().is_some() {
            problems.push(format!(
                "{endpoint_field}.url: query strings and fragments are not permitted"
            ));
        }
        if !urls.insert(endpoint.url.as_str()) {
            problems.push(format!("{endpoint_field}.url: duplicate endpoint URL"));
        }
        if endpoint.weight == 0 {
            problems.push(format!(
                "{endpoint_field}.weight: must be greater than zero"
            ));
        }
    }
    validate_weighted_schedule(upstream, &field, problems);
    validate_upstream_health(upstream, &field, problems);
    let circuit = &upstream.circuit_breaker;
    if circuit.failure_threshold == 0
        || circuit.recovery_timeout_ms == 0
        || circuit.half_open_probe_limit == 0
        || circuit.success_threshold == 0
    {
        problems.push(format!(
            "{field}.circuit_breaker: thresholds and timeout must be greater than zero"
        ));
    }
}

fn validate_weighted_schedule(upstream: &UpstreamConfig, field: &str, problems: &mut Vec<String>) {
    if upstream.load_balancing != LoadBalancingPolicy::WeightedRoundRobin
        || upstream.endpoints.is_empty()
    {
        return;
    }
    let divisor = upstream
        .endpoints
        .iter()
        .map(|endpoint| endpoint.weight)
        .reduce(greatest_common_divisor)
        .unwrap_or(1);
    let slots = upstream
        .endpoints
        .iter()
        .map(|endpoint| u64::from(endpoint.weight / divisor))
        .sum::<u64>();
    if slots > MAX_WEIGHTED_SCHEDULE_SLOTS {
        problems.push(format!(
            "{field}.endpoints: normalized weighted schedule requires {slots} slots; maximum is {MAX_WEIGHTED_SCHEDULE_SLOTS}"
        ));
    }
}

const fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn validate_upstream_health(upstream: &UpstreamConfig, field: &str, problems: &mut Vec<String>) {
    let Some(health) = &upstream.health_check else {
        return;
    };
    if !health.path.starts_with('/') {
        problems.push(format!("{field}.health_check.path: must begin with `/`"));
    }
    if health.interval_ms == 0
        || health.timeout_ms == 0
        || health.healthy_threshold == 0
        || health.unhealthy_threshold == 0
    {
        problems.push(format!(
            "{field}.health_check: durations and thresholds must be greater than zero"
        ));
    }
    if health.expected_status_min > health.expected_status_max
        || health.expected_status_max > 599
        || health.expected_status_min < 100
    {
        problems.push(format!(
            "{field}.health_check: expected status range must be within 100..=599"
        ));
    }
}

fn validate_authentication(config: &GatewayConfig, problems: &mut Vec<String>) {
    if config.authentication.api_key_sha256.len() > MAX_API_KEYS {
        problems.push(format!(
            "authentication.api_key_sha256: must not contain more than {MAX_API_KEYS} digests"
        ));
    }
    if config.authentication.jwt_providers.len() > MAX_JWT_PROVIDERS {
        problems.push(format!(
            "authentication.jwt_providers: must not contain more than {MAX_JWT_PROVIDERS} providers"
        ));
    }
    for (index, digest) in config.authentication.api_key_sha256.iter().enumerate() {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            problems.push(format!(
                "authentication.api_key_sha256[{index}]: expected a 64-character SHA-256 hex digest"
            ));
        }
    }
}
