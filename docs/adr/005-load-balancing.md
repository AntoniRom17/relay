# ADR-005: Pluggable load balancing with RAII endpoint leases

- Status: accepted
- Date: 2026-09-01

## Context

Pools need multiple selection policies while honoring concurrent health state and tracking active
requests accurately through normal completion, streaming, errors, and cancellation. Selection runs
on every upstream attempt and must not serialize all requests.

## Decision

Define a synchronous, object-safe `LoadBalancer` trait over an immutable slice of shared endpoints.
Implement lock-free round robin with an atomic cursor, a precomputed interleaved weighted schedule,
and least-connections using atomic active counts. Each successful selection returns an
`EndpointLease`; construction increments and `Drop` decrements the endpoint count.

Endpoint health is atomic and all policies skip unhealthy endpoints. Pool concurrency is a separate
semaphore so endpoint choice remains cheap and policy-independent. A retry performs a new selection
and therefore can fail over.

## Alternatives considered

- Random choice avoids cursor coordination but adds RNG overhead and less deterministic tests.
- Smooth weighted round robin improves short-window distribution for large weights but normally
  needs synchronized mutable current weights; the compact schedule is simpler for configuration-time
  weights.
- Power-of-two-choices approximates least connections more cheaply for very large pools, but the
  configured pools expected in v0.1 are small.
- Increment/decrement calls at each return path are error-prone; a lease expresses lifetime in the
  type system.

## Tradeoffs

The weighted schedule consumes memory proportional to normalized weights, though validation requires
positive finite `u32` values and the greatest-common-divisor reduction limits common cases. Least
connections scans the pool and ties favor earlier endpoints. Atomics are eventually current during
concurrent selection, which is sufficient for balancing rather than accounting.

## Consequences

Response bodies retain their endpoint lease until completion or drop, so long streams correctly
remain active. New policies can implement one small trait without changing endpoint state. Very
large endpoint counts or pathological weight ratios would justify a later scheduling ADR and
benchmark, not an invisible algorithm substitution.
