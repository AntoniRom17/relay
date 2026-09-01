# Observability

Relay emits compact text or flattened JSON logs through `tracing-subscriber`. Request spans include
request ID, method, host, path, matched route, selected upstream, status, attempts, policy outcomes,
and end-to-end latency where applicable. Credentials and raw API keys are never span fields.

Prometheus metrics use bounded dimensions:

- `relay_requests_total` and `relay_request_duration_seconds`
- `relay_upstream_requests_total` and `relay_upstream_request_duration_seconds`
- `relay_upstream_failures_total`
- `relay_active_connections`
- `relay_rate_limit_rejections_total`
- `relay_retry_attempts_total`
- `relay_circuit_breaker_state` (`closed=0`, `half_open=1`, `open=2`)
- `relay_upstream_health`
- `relay_config_reload_total`
- `relay_cache_events_total`

Endpoint URLs are configuration-controlled metric labels. Request IDs, paths, client IPs, subjects,
and token-bucket keys are not labels, preventing attacker-driven cardinality. Standard HTTP methods
have individual labels; every extension method is collapsed into `OTHER`.

When `observability.otlp_endpoint` is set, Relay builds a batched OTLP/gRPC span exporter with
`service.name=relay`. The W3C Trace Context propagator extracts an incoming `traceparent` and injects
the current context into every upstream attempt. Prometheus remains the metrics source of truth;
OTLP export currently covers traces, not metrics or logs.
