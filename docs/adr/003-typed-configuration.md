# ADR-003: Typed, validated configuration

- Status: accepted
- Date: 2026-09-01

## Context

Gateway configuration is an operator-facing API. A typo can silently weaken authentication,
overload a backend, expose administration, or route traffic incorrectly. Validation spans local
types and cross-references such as routes to pools, JWT providers, and distributed policies to
Redis.

## Decision

Use one Serde YAML document decoded into strongly typed Rust structures with unknown fields denied.
Separate parsing from semantic validation and aggregate independent semantic errors. Compile HTTP
methods, headers, routes, upstream pools, authenticators, and policies into an immutable runtime
only after validation succeeds.

Startup fails before binding sockets on any error. `relay validate` provides a deployment gate and
`relay config print` emits a canonical form while redacting API-key digests and Redis URLs. Examples
are versioned beside the schema and tested by CI.

## Alternatives considered

- Untyped YAML/JSON values make extensions easy but defer mistakes into live request paths.
- Environment variables alone are convenient for twelve-factor applications but poorly express
  nested routes, pools, and health policies.
- A database-backed dynamic control plane would support richer coordination, but requires
  authentication, consensus, schema migration, and failure semantics beyond the v0.1 scope.
- JSON and TOML are viable; YAML was selected for readable nested operator configuration despite
  its indentation sensitivity.

## Tradeoffs

Denying unknown fields makes forward migration explicit and can reject configuration prepared for a
newer binary. YAML has surprising scalar rules, so types and test fixtures must remain strict.
Canonical printing redacts known secrets but is not a general data-loss-prevention system.

## Consequences

Every new field needs a documented default or an intentional required migration, validation, an
example, and reload-compatibility classification. Runtime code can rely on validated invariants but
must still avoid panic-based logic. Deployment systems should validate with the exact binary they
will run.
