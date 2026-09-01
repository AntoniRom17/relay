# ADR-004: Compiled route matching and deterministic precedence

- Status: accepted
- Date: 2026-09-01

## Context

Routes may combine host, exact or prefix path, methods, and exact headers. Declaration-only priority
is easy to misunderstand when broad and narrow rules overlap, while ambiguous runtime selection is
unacceptable for authentication and traffic policy.

## Decision

Compile configuration strings once into HTTP methods, header names/values, lowercase host values,
and boxed path data. Sort an immutable route array by this precedence tuple:

1. host-constrained before host-wildcard;
2. exact path before prefix before path-wildcard;
3. longer paths before shorter paths;
4. more method constraints before fewer;
5. more required headers before fewer;
6. earlier declaration as the final tie-breaker.

Match through the sorted array and return the first complete match. Hosts compare without case and
without a port; paths and header values preserve HTTP comparison semantics. Query parameters are not
route selectors but remain part of the forwarded URI and cache key.

## Alternatives considered

- Declaration order alone is simple but lets a distant broad rule shadow a narrow security rule.
- User-supplied integer priority is flexible but makes every configuration review a global ranking
  exercise and still needs tie behavior.
- A prefix trie can reduce lookup work for very large tables, but mixed host/method/header predicates
  require additional indexes and complexity. The measured 256-route linear baseline is acceptable
  for v0.1.
- Regular expressions add expressiveness at the cost of performance predictability and ambiguity.

## Tradeoffs

The precedence is opinionated and adding a new matcher dimension can change ordering. Lookup remains
linear after the one-time sort. Prefix matching is lexical, so `/api` also matches `/apix`; operators
needing a segment boundary should use `/api/` plus an exact `/api` route.

## Consequences

The precedence contract is documented and covered by example, unit, and property tests. Route names
remain stable observability identities, not ordering controls. A future indexed matcher must produce
identical selections against the property-test oracle before replacing this representation.
