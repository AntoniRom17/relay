# ADR-001: Rust and Tokio

- Status: accepted
- Date: 2026-09-01

## Context

The gateway handles many concurrent, long-lived streams and shares route, health, and reliability
state across them. Memory growth, accidental blocking, cancellation, and resource accounting are
part of correctness. The implementation also needs mature HTTP/TLS libraries and one binary that
can be deployed without a language runtime.

## Decision

Implement Relay as a Rust 2024 Cargo workspace, pin the minimum supported compiler to Rust 1.88,
and use Tokio as the asynchronous runtime. Forbid unsafe code workspace-wide. Express resource
ownership through `Arc`, immutable snapshots, atomics, semaphores, cancellation tokens, and RAII
leases. Use typed errors in libraries and contextual `anyhow` errors only at process boundaries.

Keep each crate aligned with a real responsibility—protocol data plane, routing, upstream state,
policies, configuration, observability, control plane, TLS, and orchestration—without splitting
single types into cosmetic crates.

## Alternatives considered

- Go offers a strong networking standard library and simpler builds, but gives less compile-time
  control over ownership and resource lifetime in the streaming path.
- C++ can achieve equivalent low-level control, but its memory-safety and dependency costs are poor
  fits for a new security boundary.
- Async-std and smol are capable runtimes; Tokio has the broadest compatibility with Hyper, rustls,
  Redis, OpenTelemetry, and the intended operational ecosystem.
- A single crate would start smaller, but would entangle reusable state machines with process and
  protocol concerns as the gateway grows.

## Tradeoffs

Rust has a steeper contributor learning curve and longer clean builds. The AWS-LC rustls provider
requires native build tooling. Async cancellation still demands explicit ownership design; the
language does not make operational shutdown automatic.

## Consequences

The workspace carries strict format, Clippy, documentation, test, and lockfile gates. Blocking work
must not occur on Tokio workers. A newer language edition can only be adopted with an explicit
minimum-supported-Rust-version change. Unsafe optimizations require a new decision because the
current lint forbids them rather than merely warning.
