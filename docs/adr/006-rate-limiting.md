# ADR-006: Layered token-bucket rate limiting

- Status: accepted
- Date: 2026-09-01

## Context

Relay needs burst-tolerant limits at the gateway and route scopes, partitioned by route, accepted
client address, or API key. The common path must not take one process-wide mutex, identity growth
must be bounded, and operators need an explicit choice between per-process and cross-replica
semantics.

## Decision

Use token buckets with a configured refill rate and burst capacity. Apply the optional aggregate
gateway bucket before routing, then authentication, then the selected route bucket. API-key buckets
therefore use an already validated credential. Client-IP buckets use the peer socket address and do
not trust public forwarding headers.

The local implementation stores independently locked bucket entries in `DashMap`, uses a monotonic
clock, and caps identity cardinality at process construction. When the cap is reached, new identity
state is rejected instead of allocating without bound. Distributed mode delegates the same model to
the Redis design in ADR-009. Policy backend failure fails closed.

## Alternatives considered

- Fixed windows are simple but permit a two-window boundary burst.
- Sliding logs provide exact windows but retain one record per request and are inappropriate on the
  gateway hot path.
- One mutex-protected map is easy to reason about but creates contention across unrelated clients.
- Trusting `X-Forwarded-For` supports proxy chains but is unsafe without an explicit trusted-proxy
  topology, which v0.1 does not model.

## Tradeoffs

Per-process local limits multiply with replicas and reset on restart. The bounded identity map can
reject previously unseen identities during a cardinality attack even when their nominal bucket
would have capacity. Monotonic local time cannot coordinate independent hosts. Rate limits protect
work admission, not network bandwidth consumed before a request is parsed.

## Consequences

Configuration names local versus distributed semantics explicitly. Metrics distinguish rejections,
but bucket identities never become metric labels. A future trusted-proxy feature must define hop
validation before changing client identity. Eviction or admission-policy changes require tests that
preserve the hard memory bound.
