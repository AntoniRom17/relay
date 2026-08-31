use relay_config::{ConfigError, LoadBalancingPolicy, parse};

const MINIMAL: &str = r#"
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
"#;

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
