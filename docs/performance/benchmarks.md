# Performance benchmarks

Relay reports measurements, not marketing estimates. The checked-in Criterion suites isolate the
two hot decision paths that can be measured without pretending a loopback benchmark represents a
production network: route matching and endpoint selection.

## Recorded baseline

This baseline was collected on 2026-09-01 with the following environment:

| Property | Value |
| --- | --- |
| CPU | Intel64 Family 6 Model 151 Stepping 2, GenuineIntel |
| Logical CPUs | 16 |
| OS | Microsoft Windows NT 10.0.26200.0 |
| Compiler | rustc 1.88.0 (6b00bc388 2025-06-23) |
| Profile | Cargo `bench`, Criterion `--quick` |

| Benchmark | Estimate interval |
| --- | ---: |
| Match the late prefix in a 256-route table | 4.9211–4.9418 µs |
| Round-robin endpoint choice | 28.941–29.370 ns |
| Weighted round-robin endpoint choice | 30.048–30.148 ns |
| Least-connections endpoint choice | 24.334–24.513 ns |

`--quick` is useful as a development smoke baseline and has deliberately lower statistical rigor
than Criterion's default run. These figures are not requests per second, do not include sockets,
TLS, upstream latency, or response streaming, and should not be extrapolated into capacity claims.

## Reproduce the microbenchmarks

```console
cargo bench -p relay-routing --bench route_matching -- --quick
cargo bench -p relay-upstream --bench load_balancing -- --quick
```

Remove `--quick` before publishing comparative results. Keep the host quiet, record the CPU,
compiler, operating system, power policy, and command, and compare Criterion distributions rather
than a single sample. Generated `target/criterion` reports are intentionally not committed.

## End-to-end load procedure

Relay does not check in a fabricated loopback throughput number. To measure a deployment, start the
container profile and run `hey` from a separate host so the load generator does not compete with
the gateway:

```console
docker compose up --build --detach
scripts/benchmark.sh http://relay-host:8080/demo
```

On Windows:

```powershell
.\scripts\benchmark.ps1 -Url http://relay-host:8080/demo
```

The scripts default to 30 seconds and 64 concurrent workers. Override `DURATION` and
`CONCURRENCY` on POSIX, or use the PowerShell parameters. Run a warm-up first, retain raw output,
and capture at least:

- response status counts and non-2xx bodies;
- throughput plus p50, p95, p99, and maximum latency;
- Relay process CPU, resident memory, file descriptors, and connection counts;
- upstream CPU and latency separately;
- `relay_requests_total`, `relay_request_duration_seconds`, and rejection counters;
- TLS mode, response size, HTTP version, connection reuse, and policy configuration.

Change one factor at a time. A useful matrix varies HTTP/1.1 versus HTTP/2, TLS termination,
payload size, cache hits, route count, concurrency, and an impaired upstream. Stop a run if errors
appear instead of reporting an inflated successful-request rate.

## Performance contracts

The implementation guards performance through invariants more durable than a single number:

- request and response bodies stream through Hyper and are not generally buffered;
- connection, route, and upstream work are bounded by semaphores;
- cache entries, cache body size, request body size, and limiter identity cardinality are bounded;
- route lookup and balancing avoid blocking I/O;
- overload is rejected promptly instead of entering an unbounded queue;
- retries are bounded, use exponential backoff, and are restricted to replay-safe requests.

See [request lifecycle](../architecture/request-lifecycle.md) for where each bound applies.
