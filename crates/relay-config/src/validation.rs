use std::{collections::HashSet, fs, path::Path};

use http::{HeaderName, HeaderValue, Method};
use thiserror::Error;

use crate::{AuthPolicyConfig, GatewayConfig, RateLimitKey};

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
pub fn load_from_path(path: impl AsRef<Path>) -> Result<GatewayConfig, ConfigError> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse(&contents)
}

/// Parses and validates YAML configuration text.
pub fn parse(contents: &str) -> Result<GatewayConfig, ConfigError> {
    let config: GatewayConfig = serde_yaml::from_str(contents)?;
    validate(&config)?;
    Ok(config)
}

/// Validates cross-field invariants not expressible through Serde types.
pub fn validate(config: &GatewayConfig) -> Result<(), ConfigError> {
    let mut problems = Vec::new();

    if config.listeners.is_empty() {
        problems.push("listeners: at least one data-plane listener is required".to_owned());
    }
    if config.routes.is_empty() {
        problems.push("routes: at least one route is required".to_owned());
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
    if config.admin.enabled && !config.admin.address.ip().is_loopback() {
        problems.push(
            "admin.address: must use a loopback address; place authenticated infrastructure in front for remote access"
                .to_owned(),
        );
    }

    let mut route_names = HashSet::new();
    for (index, route) in config.routes.iter().enumerate() {
        let field = format!("routes[{index}]");
        if route.name.trim().is_empty() {
            problems.push(format!("{field}.name: must not be empty"));
        } else if !route_names.insert(route.name.as_str()) {
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
        if !config.upstreams.contains_key(&route.upstream) {
            problems.push(format!(
                "{field}.upstream: unknown upstream pool `{}`",
                route.upstream
            ));
        }
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
        if route.max_body_bytes == 0 {
            problems.push(format!("{field}.max_body_bytes: must be greater than zero"));
        }
        if route.concurrency_limit == 0 {
            problems.push(format!(
                "{field}.concurrency_limit: must be greater than zero"
            ));
        }
        if let Some(cache) = &route.cache {
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
            if cache.max_entries > 100_000 {
                problems.push(format!("{field}.cache.max_entries: must not exceed 100000"));
            }
            if cache.max_body_bytes > 16 * 1024 * 1024 {
                problems.push(format!(
                    "{field}.cache.max_body_bytes: must not exceed 16777216"
                ));
            }
        }
    }

    for (name, upstream) in &config.upstreams {
        let field = format!("upstreams.{name}");
        if upstream.endpoints.is_empty() {
            problems.push(format!(
                "{field}.endpoints: at least one endpoint is required"
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
        if let Some(health) = &upstream.health_check {
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

    for (index, digest) in config.authentication.api_key_sha256.iter().enumerate() {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            problems.push(format!(
                "authentication.api_key_sha256[{index}]: expected a 64-character SHA-256 hex digest"
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Validation(problems))
    }
}
