# Upstreams and load balancing

An upstream pool owns immutable endpoint references, one scheduling policy, and a Tokio semaphore.
Selection is fail-fast: capacity exhaustion returns overload, and a pool with no healthy endpoint
returns bad gateway. `UpstreamLease` combines the pool permit with an endpoint RAII lease so active
counts cannot be forgotten on errors or disconnects.

Available schedulers:

- Round robin uses a relaxed atomic cursor and health-aware wraparound.
- Weighted round robin builds a compact interleaved index schedule after dividing weights by their
  greatest common divisor. Runtime selection is an atomic increment plus healthy lookup.
- Least connections scans healthy endpoints by relaxed active-request count. Ties preserve endpoint
  order.

Active checks use the same pooled HTTP/HTTPS client style as proxy traffic but run under supervised
cancellation. Consecutive success and failure thresholds damp flapping. Passive transport failures
increment the same endpoint counters; ordinary 5xx responses affect the circuit but do not
immediately declare an endpoint unreachable.

Pool and endpoint state belongs to one runtime generation. A reload starts a new health-check
generation and cancels and joins the previous generation. Requests holding the previous runtime
retain valid pool references until their bodies finish.
