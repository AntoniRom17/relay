# Routing

Routes compile HTTP methods, header names, header values, lowercase host matchers, and path matchers
once per runtime generation. Matching then scans an immutable specificity-sorted slice without a
lock or per-route parsing.

Precedence is lexicographic, highest first:

1. A host-constrained route precedes a host-agnostic route.
2. Exact path precedes prefix path, which precedes no path constraint.
3. A longer prefix or exact path precedes a shorter one.
4. More configured methods precede fewer methods.
5. More required headers precede fewer headers.
6. Original declaration order breaks an otherwise exact tie.

All constraints on a route must match. Hosts are case-insensitive and the port is removed from the
Host header; absolute-form URI authority takes precedence when present. Paths use the parsed URI
path and do not include query parameters. Prefix matching is literal: `/api` also matches
`/apiv2`, so use a trailing slash or an exact route when segment boundaries matter.

The sorted linear table favors predictable behavior and compact immutable state for hundreds of
routes. The checked-in Criterion baseline includes a late match among 256 host-and-prefix routes.
A radix tree may become justified only after representative route distributions show the scan to
be a material end-to-end cost.
