# Troubleshooting

Start with the redacted config, process logs, admin endpoints, and the exact status returned to the
client:

```console
relay validate --config /etc/relay/relay.yaml
relay config print --config /etc/relay/relay.yaml
curl --fail http://127.0.0.1:9090/health
curl --fail http://127.0.0.1:9090/ready
curl http://127.0.0.1:9090/metrics
```

`/health` means the process and control plane are alive. `/ready` additionally reflects whether the
compiled runtime has usable upstream capacity. A healthy process can therefore be unready.

## Startup failures

**Configuration validation lists several errors.** Relay aggregates independent semantic failures.
Fix every bullet, then rerun `validate`; do not bypass validation by editing generated YAML after a
deployment check.

**Address already in use.** Confirm the public and admin addresses do not overlap, identify the
owning process, and ensure only one Relay replica binds a host-network socket.

**TLS identity cannot be loaded.** Check that the certificate chain and unencrypted PKCS#8 or RSA
private key are valid PEM, readable by the Relay user, and correspond to one another. The container
must mount both paths named by the config.

**Redis connection fails.** Verify URL, credentials, DNS, TLS/network policy, and Redis readiness.
Relay deliberately refuses to weaken a distributed limiter into local enforcement.

**Crypto-provider or linker error during build.** Use the pinned Rust toolchain and install the
platform C/C++ build tools required by the rustls crypto provider. On Windows use the x64 Visual
Studio developer environment.

## Request failures

**404 / no route.** Check the host without its port, exact versus prefix path, case-sensitive path,
method, and exact header constraints. Query strings do not participate in path matching but are
preserved upstream.

**401.** Confirm the route's auth type, header name or bearer format, configured API-key digest, and
JWT issuer/audience/algorithm/time claims. Relay removes a consumed credential before proxying.

**413.** Increase the route body bound only after verifying upstream capacity. Chunked requests are
counted while streaming even without `Content-Length`.

**429.** Inspect the configured bucket key and whether the policy is local or Redis-backed. A
gateway-wide limiter is applied in addition to any route limiter. Many requests behind one direct
proxy share its socket IP under `client_ip` policy.

**503 or other gateway 5xx.** Check endpoint health gauges, circuit state, route and pool limits,
connectivity, DNS, and upstream status. Relay returns generic client text; the structured log and
metrics carry the safe diagnosis. An open circuit or exhausted semaphore is intended fail-fast
behavior, not a deadlock.

**Only large or long responses fail.** Relay's request timeout governs upstream response-header
acquisition in v0.1; streaming bodies are not subject to one total body deadline. Check upstream,
client, ingress, and orchestrator idle timeouts as well as body stream errors.

**gRPC call fails.** Use an HTTP/2-capable TLS listener or cleartext h2 client behavior supported by
the client. Verify `content-type: application/grpc`, ALPN, upstream HTTP/2 support, and trailers.
Relay transparently proxies gRPC; it does not synthesize application-level gRPC statuses.

## Reload failures

Look at `relay_config_reloads_total` and the error log. A `validation_error` or `compile_error` means
the old generation is still active. `restart_required` means an immutable listener, admin, Redis,
observability, TLS, or shutdown field changed. Revert the file or schedule a process restart.

Some editors replace a file more than once while saving. Relay may log a rejected intermediate
candidate and accept the complete file on the next two-second poll. Prefer an atomic same-directory
rename in deployment tooling.

## Cache surprises

A response is deliberately bypassed when the request is authenticated, has authorization or cookie
headers, uses a non-GET/HEAD method, or when the response is non-successful, too large, incomplete,
sets cookies, varies, or declares private/no-store. Inspect `x-relay-cache` and the cache metrics.
Entries expire lazily; there is no v0.1 purge API.

## Escalation bundle

Before escalating, capture the Relay version, redacted canonical config, a UTC time window, request
ID, route and pool names, client protocol, status, relevant logs/metrics, and a minimal reproduction.
Never attach raw API keys, bearer tokens, Redis credentials, private keys, or unredacted source YAML.
