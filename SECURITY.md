# Security policy

## Reporting a vulnerability

Use GitHub's private vulnerability reporting for this repository. Do not open a public issue with
exploit details, credentials, certificates, request captures, or an unpatched proof of concept.

Include the affected commit or version, deployment assumptions, reproduction steps, impact, and any
suggested mitigation. You should receive an acknowledgement within seven days. Coordinated
disclosure timing depends on severity, fix complexity, and downstream exposure.

## Supported versions

Until Relay reaches 1.0, security fixes target the current `main` branch. There are no supported
maintenance branches. Consumers should pin a reviewed commit or immutable image digest and monitor
the security workflow and repository advisories.

## Deployment responsibilities

- Keep the admin listener on loopback unless it is isolated and authenticated externally.
- Replace all demonstration API keys, TLS material, hostnames, and Redis settings.
- Terminate public traffic with a valid certificate and restrict upstream network access.
- Set per-route request, concurrency, cache, and rate limits appropriate to payloads and tenants.
- Protect Redis with network policy and credentials; Relay's example URL is not a production secret.
- Export logs and traces only to trusted systems because identity subjects and request paths may be
  operationally sensitive.
- Review [the threat model](docs/security/threat-model.md) and run the RustSec audit before release.

The project forbids unsafe Rust at workspace lint level. Cryptographic primitives, HTTP parsing,
TLS, and JWT signatures are delegated to maintained libraries rather than implemented locally.
