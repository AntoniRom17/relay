# ADR-009: Redis/Lua distributed rate enforcement

- Status: accepted
- Date: 2026-09-01

## Context

Local token buckets cannot enforce one allowance across several Relay replicas. A distributed
decision needs atomic read/refill/consume/write behavior, a clock consistent across clients, bounded
key lifetime, and clear behavior when the store is unavailable.

## Decision

Use Redis only when a policy explicitly sets `distributed: true`. Execute one Lua script per
decision. The script obtains Redis server time, refills and caps the bucket, conditionally consumes
one token, stores fractional tokens and update time in a hash, and assigns a TTL derived from twice
the fill duration with a one-second floor.

Namespace keys as `<prefix>:rate:<route>:<non-secret identity>`. Use Redis's multiplexed reconnecting
connection manager. Require an initial connection at process startup and return a gateway error on
evaluation failure; there is no silent local fallback.

## Alternatives considered

- Redis `INCR` fixed windows need fewer fields but have boundary bursts and weaker burst-capacity
  behavior.
- Client-side read/modify/write is subject to races between replicas even with a shared clock.
- Redis transactions with `WATCH` can retry under contention and require additional round trips.
- A dedicated quota service could centralize richer policy, but adds another protocol, deployment,
  and availability boundary that Relay does not yet need.

## Tradeoffs

Distributed enforcement adds one network round trip and makes Redis availability part of request
availability. Cluster slotting and cross-region latency are deployment concerns. Identity text is
part of Redis keys; current API-key use is a SHA-256-derived identifier, not the raw secret, but key
access still requires infrastructure controls.

## Consequences

Operators choose availability versus global strictness deliberately. Redis latency and errors must
be monitored alongside gateway latency. Keys expire after inactivity to bound persistent state.
Changing script semantics needs compatibility tests against a real Redis instance and must preserve
server-time and atomicity guarantees.
