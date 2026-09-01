# ADR-010: Explicit circuit-breaker state machine

- Status: accepted
- Date: 2026-09-01

## Context

Repeated requests to a failing pool amplify load and delay clients. Recovery must be probed without
immediately releasing all traffic, and concurrent completions must not produce invalid combinations
of open flags, counters, and probe counts.

## Decision

Represent the breaker as one enum under a short standard mutex:

- `Closed { consecutive_failures }` admits normal work and opens at the threshold;
- `Open { opened_at }` rejects until the monotonic recovery timeout;
- `HalfOpen { in_flight, successes }` admits only the configured probe limit, closes after the
  success threshold, and reopens on any failure.

Admission returns a `CircuitPermit`. The caller must consume it as success or failure; dropping an
unresolved permit, including cancellation, conservatively records failure. The mutex is never held
across an await. State snapshots drive metrics and the read-only admin API.

## Alternatives considered

- Independent atomic booleans and counters avoid a mutex but allow combinations that do not map to
  a valid breaker state and complicate transitions.
- A rolling error-rate breaker is more sensitive to intermittent failure but requires a bounded
  time-window data structure and minimum sample policy.
- One breaker per endpoint isolates failures more precisely; a pool-level breaker better matches the
  current route-to-pool reliability contract while active health handles endpoint ejection.
- Retrying until an endpoint succeeds hides failure at the cost of unbounded amplification and is
  rejected.

## Tradeoffs

The small mutex serializes state transitions per pool, although no I/O occurs inside the critical
section. Consecutive failures can open on a brief burst and successes reset the count. Pool-level
state can open even when a remaining endpoint might recover independently. Cancelled operations are
classified pessimistically.

## Consequences

Invalid state combinations are unrepresentable and half-open admission is bounded. The metric uses
stable values closed=0, half-open=1, open=2. Tests cover recovery, probe limits, failure reopening,
and cancelled permits. Moving to error-rate windows or endpoint-scoped breakers requires an explicit
compatibility and observability decision.
