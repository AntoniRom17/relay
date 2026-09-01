# Failure-injection exercises

`relay-demo` is a deterministic Rust upstream for validating gateway failure behavior. It exposes
`/health`, echoes request metadata and bodies, can delay responses, and can fail every Nth request.

## Retry and balancing

```console
cargo run -p relay-demo -- --listen 127.0.0.1:9001 --name app-a --fail-every 3
cargo run -p relay-demo -- --listen 127.0.0.1:9002 --name app-b
cargo run --bin relay -- run --config configs/load-balanced.yaml
```

Send bodyless GET requests and observe backend names, request IDs, retry counters, and endpoint
in-flight gauges. Then send a POST body: Relay must not replay it even if the backend fails.

## Health ejection and recovery

Start one demo with `--unhealthy`, or stop it after Relay begins. Wait for the configured consecutive
failure threshold and confirm its health gauge falls while traffic continues to the other endpoint.
Restore it and wait for the healthy threshold before expecting selection again.

## Circuit transition

Run the only endpoint with `--fail-every 1`. After the failure threshold, requests should fail fast
while the circuit is open. After the recovery timeout, only the bounded half-open probe population
is admitted. Replace the upstream with a healthy process and confirm the required successful probes
close the circuit.

## Timeout and graceful drain

Set demo delay above `request_timeout_ms` to exercise the response-header deadline. For drain tests,
set it below the request timeout but long enough to send SIGINT while a request is active. The
request should finish within `shutdown_grace_ms`; new accepts stop. Repeat above the grace period and
confirm the orchestrator, client, and Relay failure behavior is understood before production.

Automated real-socket versions of core retry, body-limit, cache, trace propagation, and drain cases
live in `crates/relay-proxy/tests/end_to_end.rs`.
