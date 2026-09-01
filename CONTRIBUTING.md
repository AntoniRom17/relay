# Contributing to Relay

Relay accepts focused fixes, tests, documentation, and carefully justified features. Open an issue
before a large protocol or configuration change so compatibility and operational consequences can
be discussed first.

## Development workflow

1. Use the Rust toolchain pinned in `rust-toolchain.toml`.
2. Add or update tests that demonstrate the behavior, including failure paths.
3. Keep configuration backward compatible unless an ADR explains the break.
4. Run the same gates as CI:

   ```console
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo test --workspace --all-features --locked
   cargo build --workspace --all-targets --all-features --locked
   ```

5. Update architecture, operations, security, or performance documentation when behavior changes.

Do not commit generated `target` output, fuzz corpora, credentials, certificates, benchmark HTML,
or machine-specific editor state. Never weaken a bound or security default without explaining the
new threat and resource model.

## Engineering expectations

- Keep the proxy path streaming; justify any full-body buffering with a strict bound.
- Never hold a blocking lock across `.await`.
- Prefer typed states and RAII permits over coordinated booleans and manual counters.
- Do not retry bodies unless replay safety is explicit and tested.
- Keep metric labels bounded; do not label with request IDs, client addresses, or raw URLs from
  untrusted input.
- Treat YAML fields and client-facing error codes as public interfaces.
- Include measured evidence for performance claims.

Use conventional, descriptive commits such as `feat:`, `fix:`, `test:`, `docs:`, `perf:`, and
`refactor:`. A pull request should explain its motivation, safety or compatibility impact, tests,
and operator-visible changes.
