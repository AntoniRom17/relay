# Control plane

The control plane is a separate Axum listener and never shares the public routing table. Endpoints:

| Endpoint | Purpose |
| --- | --- |
| `GET /health` | process liveness |
| `GET /ready` | 200 only when every pool has a healthy endpoint |
| `GET /metrics` | Prometheus text exposition |
| `GET /admin/status` | version, uptime, and object counts |
| `GET /admin/routes` | non-secret route summary |
| `GET /admin/upstreams` | capacity, endpoint health, and circuit state |
| `GET /admin/upstreams/{id}` | one pool snapshot |
| `GET /admin/config` | explicitly constructed, secret-redacted configuration view |

The default address is loopback. A non-loopback address is rejected unless `allow_remote: true` is
present, making exposure an auditable configuration decision. Relay does not provide operator
authentication or mutation endpoints. Container deployments must keep the port out of public
Services and restrict it with a firewall, NetworkPolicy, service mesh, or authenticated sidecar.

Handlers load one immutable runtime snapshot and serialize point-in-time views. They do not lock or
mutate the data plane, and the configuration endpoint never serializes the original secret-bearing
configuration object.
