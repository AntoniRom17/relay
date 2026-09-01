# ADR-007: Immutable runtime snapshots and atomic reload

- Status: accepted
- Date: 2026-09-01

## Context

Routes, pools, and policies must change without exposing partial generations to concurrent requests.
Locking a mutable global configuration for every policy lookup adds contention, and mutating objects
in place can make one request observe an impossible mixture of old routes and new pools.

## Decision

Compile each valid configuration into an immutable `RuntimeState` containing its route table, pools,
circuits, semaphores, and authenticators. Publish shared state through `ArcSwap`. A request loads one
`Arc` snapshot and keeps it—and its RAII permits—through response-body completion. Reload parses,
validates, and compiles a complete candidate before one atomic swap.

Watch the configured file at a two-second interval. Reconfigure health checks by cancelling and
joining the old generation after a successful swap. Treat listeners, admin, Redis, observability,
TLS listener definition, and shutdown budget as restart-only because their resources are owned by
the process orchestration layer.

## Alternatives considered

- `RwLock<RuntimeState>` makes publication straightforward but forces hot-path read locking and
  encourages in-place mutation.
- Per-component swaps reduce rebuild work but allow cross-component generation skew.
- SIGHUP is conventional but less portable and still needs file semantics; a polling watcher works
  on Windows and containers.
- A mutating control-plane API needs authentication, audit, persistence, and coordination and is
  intentionally outside v0.1.

## Tradeoffs

Old snapshots and their state remain alive while old streams exist, so a reload can temporarily
double runtime memory. Stateful policies reset in the new generation. Polling has up to two seconds
of latency and may observe an editor's intermediate save. Immutable resource fields require a
rollout rather than a hot change.

## Consequences

Reload failure leaves traffic on the last good generation and emits a reason-labelled metric. The
configuration schema must classify new fields as reloadable or restart-only. Deployment tools
should write complete files with atomic same-directory rename. Any future state migration must be
explicit; it cannot mutate the live snapshot behind existing requests.
