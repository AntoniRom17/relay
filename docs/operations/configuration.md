# Configuration operations

Relay reads one deny-unknown-fields YAML document at startup. Syntax and semantic validation finish
before any socket is bound. Run `relay validate --config <path>` in deployment automation and
`relay config print --config <path>` to inspect a canonical, secret-redacted form.

## Structure

- `listeners` defines public bind addresses, optional rustls certificate paths, connection limits,
  response-header deadlines, and header limits.
- `admin` controls the distinct health/metrics socket. Non-loopback binds require
  `allow_remote: true` because this API has no built-in authentication.
- `routes` contains ordered matchers and request policies. Names must be unique and are metric
  labels, so keep them stable and low-cardinality.
- `upstreams` maps route targets to backend URLs, balancing, health checks, concurrency, and circuit
  settings.
- `authentication` holds API-key SHA-256 digests and named asymmetric JWT providers.
- `redis` enables distributed token buckets. Relay fails startup if Redis is configured but cannot
  be reached; it does not silently weaken a distributed policy to a local one.
- `gateway_rate_limit` is an optional aggregate token bucket shared by the process or replicas.
- `observability` selects the tracing filter, JSON formatting, and optional OTLP/gRPC endpoint.
- `shutdown_grace_ms` controls the drain budget after SIGINT or SIGTERM.

Start from the examples in `configs/`. `configs/full.yaml` is a field-rich reference; it contains
demonstration addresses and digest material and is not a deployable secret bundle.

## Route matching

Every route may constrain host, one of `path_exact` or `path_prefix`, methods, and exact header
values. A missing constraint is a wildcard. Precedence is deterministic: exact paths outrank
prefixes, more-constrained routes outrank less-constrained routes, longer prefixes outrank shorter
ones, and source order is the final tie-breaker. The forwarded URI retains its path and query.

Authentication runs before per-key rate limiting. `key: api_key` is therefore valid only on an
API-key-authenticated route. Authenticated routes cannot enable the public response cache.

## Runtime reload

Relay polls the source file every two seconds. A changed file is parsed and compiled off the active
request path. It is installed with one atomic pointer swap only if every check succeeds. A rejected
candidate leaves the previous runtime untouched and increments `relay_config_reloads_total` with a
reason label.

| Reloaded without restart | Requires process restart |
| --- | --- |
| routes and their policies | listeners, TLS paths, and listener bounds |
| upstream endpoints and balancing | admin bind or exposure policy |
| health and circuit policies | Redis connection and namespace |
| API-key digests and JWT providers | log format/filter and OTLP endpoint |
| gateway aggregate rate policy | shutdown grace period |

In-flight requests keep the old immutable snapshot through response completion; new requests see
the new snapshot. Health-check generations are replaced after a successful swap. TLS identities
are represented by an atomically replaceable acceptor internally, but file-watcher wiring is a
post-v0.1 roadmap item, so certificate path or content changes currently require restart.

## Secrets

Store raw API keys outside YAML and configure lowercase or uppercase SHA-256 hex digests. Place JWT
public-key PEM text in the provider's `public_key` field. Do not put private signing keys in Relay.
Mount TLS private keys read-only with the narrowest filesystem permissions available.

YAML does not perform environment interpolation. Render deployment-specific values before Relay
starts, or mount a generated file. `config print` redacts Redis URLs and API-key digests, but the
source file remains sensitive because Redis URLs may contain credentials.

## Validation invariants

Validation aggregates independent failures so a deployment can fix them in one pass. It rejects,
among other cases, duplicate listeners or route names, unknown pools and JWT providers, invalid
paths/methods/headers, zero bounds, invalid retry statuses, credential-bearing upstream URLs,
distributed limits without Redis, cache bodies over 16 MiB, caches over 100,000 entries, and
remote admin binds without explicit opt-in.
