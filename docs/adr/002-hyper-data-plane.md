# ADR-002: Hyper data plane and Axum control plane

- Status: accepted
- Date: 2026-09-01

## Context

The main proxy must preserve HTTP methods, URIs, headers, streaming bodies, HTTP/2 trailers, client
disconnects, and pooled upstream connections. The administrative API instead needs a small set of
JSON/text endpoints where routing ergonomics matter more than per-request proxy overhead.

## Decision

Build the public data plane directly on Hyper 1, hyper-util, `http-body`, Tokio, and rustls. Adapt
incoming bodies into boxed streaming bodies and pass frames through without general-purpose full
buffering. Use hyper-util's legacy client pool for HTTP/1.1 and HTTP/2 upstream reuse, and remove
hop-by-hop headers at the proxy boundary.

Use Tower-compatible service concepts where they clarify policy composition, but keep the request
lifecycle explicit because retries, endpoint/circuit leases, cache completion, and response-body
RAII must coordinate. Use Axum only for the isolated, read-only control plane.

## Alternatives considered

- An Axum application for both planes would be productive but obscure the exact connection and
  body lifecycle this project needs to control.
- A fully generic Tower middleware stack makes individual policies reusable, but response-body
  ownership and retry replay constraints become harder to see and test across many layers.
- Writing HTTP parsing and pooling directly would add risk without differentiating Relay; Hyper is
  the mature protocol foundation.
- Buffering bodies simplifies retries and caching but turns untrusted payload size into routine
  memory consumption and weakens streaming latency.

## Tradeoffs

Direct Hyper code is more verbose and protocol details remain Relay's responsibility. Some policies
cannot be represented as a simple request/response middleware boundary. Transparent gRPC support
does not provide protobuf-aware routing or status synthesis.

## Consequences

Request and response bodies stream by default. Features that retain payloads require explicit size
bounds and completion semantics. HTTP/2 and TLS behavior must be exercised as protocol behavior,
not inferred from an application framework. The admin listener remains operationally separate and
must not share public routing.
