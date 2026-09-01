# Architecture overview

Relay separates a latency-sensitive data plane from a read-only administrative control plane. The
data plane accepts sockets with Tokio, negotiates plaintext HTTP or rustls, serves HTTP/1.1 and
HTTP/2 with Hyper, and forwards bodies as frames. Axum is intentionally limited to administration.

The Cargo workspace follows operational responsibilities rather than one crate per type. `proxy`
owns the request lifecycle; `routing`, `upstream`, `auth`, `limits`, `tls`, `cache`, and
`observability` provide independently tested policy mechanisms. `cli` is the composition root and
owns signals, background-task supervision, watched-file reload, and server lifetimes.

## Runtime ownership

`RuntimeState` contains an immutable configuration, compiled route table, pool and circuit objects,
per-route semaphores, and compiled authentication material. `RuntimeStore` publishes it through
`ArcSwap`. A request loads one `Arc<RuntimeState>`, then holds a `RequestPlan` and route permit until
its response stream is dropped. Endpoint and pool permits use the same RAII rule.

This creates a useful invariant: configuration and resource-accounting state needed by a request
cannot disappear while that request is alive. Reload does not mutate a collection piecemeal; it
builds a complete replacement and performs one atomic pointer store.

## Concurrency choices

- Atomics track endpoint health, active requests, circuit counters, and scheduler cursors.
- Semaphores enforce listener, route, and pool concurrency without unbounded queues.
- DashMap shards local rate-limit and cache state; both have explicit cardinality bounds.
- A mutex protects the small circuit state machine, and is never held across `.await`.
- Cancellation tokens link listener, health-check generation, watcher, and shutdown lifetimes.
- Join sets retain ownership of spawned server tasks and are drained during shutdown.

Relay forbids unsafe code at the workspace level. Streaming frame ownership and `Bytes` avoid
unnecessary full-body copies; correctness and bounds take priority over speculative micro-tuning.

## Data and control boundaries

Public traffic never reaches admin routes. The admin server receives the same atomic runtime store
but exposes snapshots and a secret-redacted configuration view only. It cannot mutate policies.
Remote admin binding requires `allow_remote: true` and deployment-level isolation.

Prometheus instruments are held in a private registry. Structured tracing spans follow the response
body lifetime, not merely the upstream header latency. OTLP is optional; when enabled, a batch
provider exports spans and W3C context is extracted and injected at HTTP boundaries.
