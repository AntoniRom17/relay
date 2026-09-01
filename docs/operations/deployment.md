# Deployment guide

Run Relay as a dedicated, unprivileged process with a validated, read-only configuration. Put the
admin listener on loopback or an isolated management network, give shutdown at least the configured
drain budget, and keep upstream and Redis DNS stable for the lifetime of a process.

## Build an artifact

```console
cargo build --workspace --release --locked
cargo test --workspace --all-features --locked
relay validate --config /etc/relay/relay.yaml
```

The release binary is `target/release/relay`. The multi-stage `Dockerfile` pins the Rust toolchain,
uses the lockfile, copies CA roots, sets UID/GID 10001, and wraps the process with `dumb-init`. Its
build context excludes Git state, local build products, documentation, and secret-like file types.
Build by digest-pinned base images in a release pipeline when supply-chain policy requires it.

## Linux service

`deploy/systemd/relay.service` is a hardened starting point. Install the binary at
`/usr/local/bin/relay`, configuration at `/etc/relay/relay.yaml`, and TLS material below
`/etc/relay/tls`. Validate the file before restarting:

```console
sudo -u relay /usr/local/bin/relay validate --config /etc/relay/relay.yaml
sudo systemctl restart relay
sudo systemctl status relay
```

The example's 35-second systemd stop timeout must stay longer than `shutdown_grace_ms`. Raise
`LimitNOFILE` in line with the listener and upstream connection budget, then enforce a corresponding
host-level limit.

## Containers and Kubernetes

For a local topology, `docker compose up --build` starts two failure-injectable upstreams, Redis,
Prometheus, and Grafana. It is a demonstration, not a high-availability deployment.

The manifests under `deploy/kubernetes` set probes, requests/limits, non-root security context,
rolling updates, a PodDisruptionBudget, and a NetworkPolicy. Review these operational assumptions:

- mount the named TLS Secret or remove the TLS volume/listener together;
- generate the ConfigMap from the deployment-specific Relay YAML;
- keep `/ready` reachable only from the kubelet and intended monitoring path;
- permit egress only to configured upstreams, DNS, Redis, and OTLP destinations;
- size `terminationGracePeriodSeconds` above Relay's drain budget;
- use at least two replicas only after selecting distributed limits where global enforcement is
  required;
- scrape each replica's counters and aggregate rates, not gauges that describe different pods.

## Rollout and rollback

1. Validate configuration with the exact release binary.
2. Start one canary and wait for `/ready` plus healthy upstream gauges.
3. Send representative HTTP/1.1, HTTP/2, streaming, auth, and failure-path traffic.
4. Check non-2xx rates, latency histograms, circuit transitions, memory, and open connections.
5. Expand gradually while preserving old replicas until the drain completes.
6. Roll back the binary and configuration together if schema or semantics changed.

A file reload is suitable for compatible route and pool policy updates. Use an orchestrated process
restart for listener, Redis, telemetry, admin, shutdown, or TLS changes. A malformed reload does not
damage the current generation, but automation should still alert on every non-success reload label.

## Capacity and failure policy

Begin with measured upstream capacity. Set the pool concurrency limit below the aggregate backend
safe concurrency, divide route limits intentionally, and leave connection headroom for health
checks and retries. A request may consume more than one upstream attempt, so size retry-enabled
capacity from observed amplification rather than client request rate alone.

Relay intentionally fails closed for authentication, distributed rate-limit backend errors, open
circuits, exhausted concurrency, oversized bodies, and unavailable upstreams. Account for those
responses in readiness and alerting instead of relying only on process liveness.
