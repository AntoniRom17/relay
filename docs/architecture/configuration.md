# Configuration architecture

YAML is deserialized into deny-unknown-fields Serde types, then checked by semantic validators. The
validator aggregates independent failures so an operator can fix multiple fields in one edit. It
checks references, address conflicts, path and HTTP syntax, status ranges, positive limits,
authentication dependencies, Redis requirements, endpoint URL safety, cache bounds, TLS fields,
and circuit or health thresholds.

Startup fails before binding any listener when parsing, validation, runtime compilation,
authentication key compilation, TLS loading, metrics creation, or Redis initialization fails.

The watched-file reload loop compares a file modification/length stamp every two seconds and parses
in a blocking task. A candidate must validate and compile before publication. Listener/admin/Redis/
observability/TLS/shutdown settings are immutable because changing them requires rebuilding process-
owned resources. Routes, upstreams, auth material, rate policies, cache policies, health checks, and
circuits are replaced as one runtime generation.

Runtime compilation also creates one pooled HTTP/HTTPS client per upstream pool. This preserves
each pool's own `connect_timeout_ms` and ensures a successful reload swaps routing, policy, and
transport settings as the same generation.

There is no environment-variable interpolation inside YAML. Deployment systems should template or
mount a complete file, and protect it as a secret-bearing artifact. `relay config print` provides a
canonical representation while replacing Redis URLs and API-key digests.
