# ADR-008: Metrics, traces, and structured logs

- Status: accepted
- Date: 2026-09-01

## Context

A gateway needs enough information to distinguish client policy rejection, internal overload,
backend failure, and latency. Observability must not create unbounded label cardinality, leak
credentials, or require synchronous network export on the request path.

## Decision

Use `tracing` spans and events for structured diagnostics, the Prometheus Rust client for local
pull-based metrics, and OpenTelemetry's batched OTLP/gRPC exporter for distributed traces. Generate
or preserve a request ID, extract W3C trace context from incoming headers, attach a Relay span, and
inject the current context upstream.

Record stable configured route, pool, and endpoint identities but never API keys, bearer tokens,
arbitrary paths, peer identities, or error strings as metric labels. Expose metrics only through the
separate admin listener. JSON logs are the production default; human formatting remains available.
The telemetry guard flushes the tracer provider during normal process teardown.

## Alternatives considered

- Logs alone are flexible but poor for alerting and percentile aggregation.
- Push-only metrics couple request observability to a remote system; Prometheus scraping keeps the
  gateway independent.
- Per-request dynamic metric labels make ad hoc debugging easy but permit attacker-controlled memory
  growth.
- A custom trace protocol would add incompatibility; W3C Trace Context and OTLP are standard.

## Tradeoffs

Metrics add atomic updates and histogram work to request handling. Batch tracing can lose recent
spans on forced termination and consumes bounded background resources. Route names can still create
cardinality if operators continuously rename them across deployments. Incoming trace identifiers
are not authenticated.

## Consequences

New metrics require a bounded label-set review. Sensitive headers and secrets are excluded from
normal fields, and internal errors remain server-side. Operators must secure the admin socket and
size scrape/trace retention externally. Performance tests should compare representative telemetry
settings rather than disabling all observation and calling that production capacity.
