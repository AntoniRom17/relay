# Reliability and overload behavior

Relay deliberately rejects excess work rather than hiding it in an unbounded queue. Listener,
route, and pool semaphores bound progressively smaller scopes. Local rate-limit key cardinality,
cache entries, cache body size, headers, request bodies, and connection tasks all have explicit
limits. Rejections map to 413, 429, or 503 with stable error codes.

Expired cache entries are reclaimed on reads and when insertion meets capacity. Local token-bucket
identities are pruned after ten idle minutes by a once-per-minute blocking-pool maintenance job, so
bounded state can recover without scanning the map on the request path.

## Retries and deadlines

The listener request timeout currently forms the complete upstream-attempt deadline. Connect
timeout is configured separately on the connector. Retries apply only when the method is idempotent
and the request body reports an exact size of zero. This avoids replaying partially consumed bodies.
Backoff doubles from the configured base, caps before jitter, and adds up to 50 percent jitter.

## Circuit breaker

Each pool has an explicit closed, open, or half-open state. Consecutive failures open the circuit.
After the recovery deadline, a bounded number of probes enter half-open. One failed probe reopens;
the configured number of successful probes closes and clears counters. A permit dropped unresolved
counts as failure, making cancellation conservative.

## Health and shutdown

Active health transitions remove or restore endpoints. Readiness fails when any configured pool has
no healthy member. On SIGINT or SIGTERM, Relay cancels all listeners, stops accepting connections,
cancels health and reload workers, requests graceful Hyper connection shutdown, and waits up to the
configured grace period before aborting connection tasks. The OTLP provider is flushed when the
composition root exits.
