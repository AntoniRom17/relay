//! Configuration API defaults and semantic validation regressions.

use relay_config::{ConfigError, LoadBalancingPolicy, parse};

const MINIMAL: &str = r"
listeners:
  - address: 127.0.0.1:8080
routes:
  - name: api
    host: api.example.test
    path_prefix: /v1
    upstream: api
upstreams:
  api:
    endpoints:
      - url: http://127.0.0.1:9001
redis: null
";

#[test]
fn minimal_configuration_applies_safe_defaults() {
    let config = parse(MINIMAL).expect("minimal configuration should be valid");

    assert_eq!(config.listeners[0].max_connections, 10_000);
    assert_eq!(config.routes[0].retry.max_attempts, 1);
    assert_eq!(
        config.upstreams["api"].load_balancing,
        LoadBalancingPolicy::RoundRobin
    );
    assert_eq!(config.admin.address.to_string(), "127.0.0.1:9090");
}

#[test]
fn validation_reports_multiple_actionable_paths() {
    let invalid = MINIMAL
        .replace("path_prefix: /v1", "path_prefix: v1\n    max_body_bytes: 0")
        .replace("url: http://127.0.0.1:9001", "url: ftp://127.0.0.1:9001");

    let error = parse(&invalid).expect_err("semantic violations should fail");
    let ConfigError::Validation(problems) = error else {
        panic!("expected semantic validation error");
    };

    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("path_prefix"))
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("max_body_bytes"))
    );
    assert!(problems.iter().any(|problem| problem.contains("scheme")));
}

#[test]
fn unknown_fields_are_rejected() {
    let invalid = MINIMAL.replace(
        "address: 127.0.0.1:8080",
        "address: 127.0.0.1:8080\n    surprise: true",
    );

    let error = parse(&invalid).expect_err("unknown keys should not be silently ignored");

    assert!(matches!(error, ConfigError::Parse(_)));
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn routes_must_reference_existing_upstreams() {
    let invalid = MINIMAL.replace("upstream: api", "upstream: missing");

    let error = parse(&invalid).expect_err("dangling references should fail");

    assert!(
        error
            .to_string()
            .contains("unknown upstream pool `missing`")
    );
}

#[test]
fn authenticated_and_unbounded_cache_policies_are_rejected() {
    let invalid = MINIMAL.replace(
        "upstream: api",
        "upstream: api\n    authentication:\n      type: api_key\n    cache:\n      ttl_seconds: 60\n      max_entries: 100001\n      max_body_bytes: 16777217",
    );

    let error = parse(&invalid).expect_err("unsafe cache policy should fail");
    let message = error.to_string();

    assert!(message.contains("authenticated routes are not cacheable"));
    assert!(message.contains("max_entries"));
    assert!(message.contains("max_body_bytes"));
}

#[test]
fn cache_memory_envelope_is_bounded() {
    let invalid = MINIMAL.replace(
        "upstream: api",
        "upstream: api\n    cache:\n      ttl_seconds: 60\n      max_entries: 10000\n      max_body_bytes: 65536",
    );

    let error = parse(&invalid).expect_err("aggregate cache memory should be bounded");

    assert!(error.to_string().contains("multiplied by max_body_bytes"));
}

#[test]
fn pathological_weighted_schedules_are_rejected() {
    let invalid = MINIMAL.replace(
        "api:\n    endpoints:\n      - url: http://127.0.0.1:9001",
        "api:\n    load_balancing: weighted_round_robin\n    endpoints:\n      - url: http://127.0.0.1:9001\n        weight: 100000\n      - url: http://127.0.0.1:9002\n        weight: 1",
    );

    let error = parse(&invalid).expect_err("weighted schedule allocation should be bounded");

    assert!(error.to_string().contains("normalized weighted schedule"));
}

#[test]
fn gateway_rate_limit_is_an_aggregate_bucket() {
    let invalid = MINIMAL.replace(
        "redis: null",
        "gateway_rate_limit:\n  requests_per_second: 0\n  burst_capacity: 10\n  key: client_ip\n  distributed: true\nredis: null",
    );

    let error = parse(&invalid).expect_err("invalid gateway limit should fail");
    let message = error.to_string();

    assert!(message.contains("rate and burst capacity"));
    assert!(message.contains("only route is valid"));
    assert!(message.contains("requires a top-level redis section"));
}

#[test]
fn api_key_routes_require_keys_and_a_valid_header() {
    let invalid = MINIMAL.replace(
        "upstream: api",
        "upstream: api\n    authentication:\n      type: api_key\n      header: invalid header",
    );

    let error = parse(&invalid).expect_err("unusable authentication must fail at validation");
    let message = error.to_string();

    assert!(message.contains("not a valid header name"));
    assert!(message.contains("requires at least one"));
}

#[test]
fn operational_bounds_prevent_runtime_panics_and_pathological_values() {
    let invalid = MINIMAL
        .replace(
            "address: 127.0.0.1:8080",
            "address: 127.0.0.1:8080\n    max_connections: 1000001\n    max_header_bytes: 1048577",
        )
        .replace("name: api", "name: 'invalid route name'")
        .replace("host: api.example.test", "host: example.com:443")
        .replace(
            "upstream: api",
            "upstream: api\n    concurrency_limit: 1000001\n    retry:\n      max_attempts: 11\n      base_backoff_ms: 60001\n      max_backoff_ms: 60001",
        )
        .replace(
            "api:\n    endpoints:",
            "api:\n    connect_timeout_ms: 60001\n    concurrency_limit: 1000001\n    endpoints:",
        );

    let error = parse(&invalid).expect_err("unsafe operational bounds must be rejected");
    let message = error.to_string();

    assert!(message.contains("max_connections"));
    assert!(message.contains("max_header_bytes"));
    assert!(message.contains("identifier"));
    assert!(message.contains("without a port"));
    assert!(message.contains("max_attempts"));
    assert!(message.contains("connect timeout"));
}

#[test]
fn telemetry_endpoints_must_be_safe_urls() {
    let invalid = MINIMAL.replace(
        "redis: null",
        "observability:\n  otlp_endpoint: https://user:secret@collector.example.test:4317\nredis: null",
    );

    let error = parse(&invalid).expect_err("credential-bearing telemetry URL must be rejected");

    assert!(error.to_string().contains("observability.otlp_endpoint"));
}

#[test]
fn remote_admin_binding_requires_explicit_opt_in() {
    let unsafe_default = MINIMAL.replace("routes:", "admin:\n  address: 0.0.0.0:9090\nroutes:");
    assert!(
        parse(&unsafe_default)
            .expect_err("remote admin should be rejected")
            .to_string()
            .contains("allow_remote")
    );

    let explicit = unsafe_default.replace(
        "address: 0.0.0.0:9090",
        "address: 0.0.0.0:9090\n  allow_remote: true",
    );
    parse(&explicit).expect("explicit remote admin opt-in should validate");
}
