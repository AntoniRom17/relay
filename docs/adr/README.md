# Architecture decision records

Relay records decisions that shape more than one subsystem or impose a durable constraint. ADRs are
immutable once accepted: a later decision supersedes an earlier record instead of rewriting its
historical context.

| Record | Decision |
| --- | --- |
| [ADR-001](001-rust-and-tokio.md) | Rust and Tokio |
| [ADR-002](002-hyper-data-plane.md) | Hyper data plane and Axum control plane |
| [ADR-003](003-typed-configuration.md) | Typed, validated configuration |
| [ADR-004](004-route-matching.md) | Compiled route matching and precedence |
| [ADR-005](005-load-balancing.md) | Pluggable load balancing with RAII leases |
| [ADR-006](006-rate-limiting.md) | Layered token-bucket rate limiting |
| [ADR-007](007-runtime-reload.md) | Immutable runtime snapshots and atomic reload |
| [ADR-008](008-observability.md) | Metrics, traces, and structured logs |
| [ADR-009](009-redis-limits.md) | Redis/Lua distributed enforcement |
| [ADR-010](010-circuit-breaker.md) | Explicit circuit-breaker state machine |
