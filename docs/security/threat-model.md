# Threat model

This document describes Relay v0.1 as implemented. It is a deployment review aid, not a claim that
the gateway eliminates application risk.

## Assets and boundaries

Relay protects backend availability, routing integrity, credentials presented by clients, TLS
private keys, JWT trust material, Redis limiter state, and telemetry that may describe traffic.
There are four meaningful trust boundaries:

1. untrusted clients cross a public listener into Relay;
2. Relay crosses the backend boundary to operator-configured HTTP or HTTPS upstreams;
3. Relay crosses infrastructure boundaries to Redis and an optional OTLP collector;
4. operators and monitoring systems cross the separate admin listener.

The configuration author, host administrator, certificate provisioner, Redis administrator, and
telemetry operator are trusted. Upstream applications are trusted to enforce application-level
authorization and validate business inputs. Public clients, all public headers and bodies, and
arbitrary network peers are untrusted.

## Threats and controls

| Threat | Implemented control | Residual risk / operator action |
| --- | --- | --- |
| Connection or slow-header exhaustion | listener semaphore, HTTP/1 header read timeout, header-size bounds, HTTP/2 header-list bound | place an L4 defense in front for volumetric attacks; tune OS queues and timeouts |
| Oversized bodies | route-specific `Content-Length` fail-fast check and streaming byte limit | compressed application payload expansion is the upstream's responsibility |
| Work or memory growth | route/pool concurrency semaphores, bounded limiter keys, cache entry/body ceilings | config and reload cardinality must remain operator-controlled |
| Header smuggling / connection metadata forwarding | standard HTTP parsing plus removal of `Connection`-named and fixed hop-by-hop headers | terminate legacy or non-conforming intermediaries before Relay |
| Client identity spoofing | rate limiting uses the accepted peer socket, not `X-Forwarded-For`; Relay writes forwarding headers | direct client IP represents the trusted front proxy when one exists; design that topology explicitly |
| Credential disclosure to upstreams | consumed API-key headers and bearer authorization are removed before forwarding | other application credentials are intentionally forwarded unless the upstream contract removes them |
| Timing disclosure of API-key validity | SHA-256 digests are compared in constant time across configured candidates | key entropy, secure provisioning, rotation, and client transport remain operator responsibilities |
| Forged JWTs | signature, issuer, audience, algorithm, and temporal claims are checked with asymmetric keys | static keys have no JWKS fetch, revocation, or automatic rotation; deploy key changes deliberately |
| Retry amplification or duplicate mutation | attempts and backoff are bounded; only bodyless idempotent requests are replayed | upstream operations must honor HTTP method semantics; monitor retry amplification |
| Failed backend overload | active health ejection, passive circuit breaker, pool concurrency, fail-fast overload | bad thresholds can eject capacity or prolong failure; test them with realistic faults |
| Cache data leakage | cache is forbidden on authenticated routes and bypasses authorization, cookies, `private`, `no-store`, `set-cookie`, and `Vary` | cache keys are method/host/URI only; do not cache content personalized by any other input |
| Cache poisoning | only complete, bounded, successful public responses are inserted; incomplete streams are discarded | trusted upstreams still control cacheable content and headers |
| Control-plane disclosure or mutation | read-only, redacted API; loopback default; non-loopback requires explicit `allow_remote` | no built-in control-plane authentication—use firewall, NetworkPolicy, or authenticated sidecar |
| Configuration injection | strongly typed YAML, unknown-field denial, semantic validation, atomic candidate swap | configuration writers are trusted and can intentionally route traffic to arbitrary hosts |
| SSRF | upstream URLs can only come from validated operator configuration and allow only HTTP(S) origins | a compromised config writer can reach network destinations available to Relay; restrict egress |
| TLS interception | rustls termination and WebPKI validation for HTTPS upstreams; HTTP/2 ALPN | no upstream mTLS, certificate pinning, custom roots, or downstream client certificates in v0.1 |
| Secret exposure in diagnostics | client errors are generic; config print and admin state redact credentials | source YAML, process memory, crash dumps, and structured request fields still require access control |
| Trace spoofing | W3C context is parsed by the standard propagator and a Relay span is created | incoming trace IDs are untrusted correlation hints; never use them for authorization |
| Dependency compromise | lockfile, pinned toolchain, CI audit, Dependabot, minimal runtime image | pin container base images by digest and produce/sign SBOMs in the release environment |

## Protocol behavior

Relay accepts HTTP/1.1 and HTTP/2. TLS listeners advertise both with ALPN. gRPC works as transparent
HTTP/2 traffic: request and response data frames and trailers remain streaming. Relay does not
understand protobuf messages, enforce gRPC method authorization, or translate between gRPC and
JSON. Timeout failures that occur after response headers cannot be converted into a valid gRPC
status trailer.

Upstream TLS uses platform WebPKI roots. Plain HTTP upstreams are intentionally supported for
same-host or protected service-network deployments; they provide no confidentiality or server
authentication.

## Availability and failure semantics

Relay rejects overload instead of queuing it without bound. Authentication failures return 401,
rate exhaustion 429, oversized request bodies 413, and capacity/circuit/upstream failures a generic
5xx response. Exact mapping can evolve before 1.0; clients should handle standard status classes.

Redis-backed policies fail closed when Redis is unavailable. Startup also fails if a configured
Redis connection cannot be established. If availability is more important than global enforcement,
configure an explicitly local limiter instead of relying on an accidental fallback.

Graceful shutdown stops accepting new connections and grants in-flight work the configured drain
period. A process or orchestrator can still terminate streams when that budget expires. Client
retry policy and upstream idempotency remain necessary.

## Known v0.1 exclusions

- no operator authentication, authorization, or mutating admin API;
- no downstream mTLS or upstream client certificates;
- no dynamic JWKS discovery, JWT revocation, or per-route claim expression language;
- no WebSocket upgrade tunnel;
- no persistent or distributed response cache and no purge API;
- no dynamic TLS file reload in the process watcher;
- no WAF, schema validation, malware scanning, or application authorization;
- no built-in ACME, secret-manager, service-discovery, or DNS-refresh controller.

Report vulnerabilities according to `SECURITY.md`. Do not include active credentials or exploit
traffic against systems you do not own.
