# Request lifecycle

1. A listener admits a TCP connection only if its semaphore has capacity. TLS listeners snapshot
   the active rustls server configuration for each handshake.
2. Hyper enforces header-buffer and HTTP/2 header-list limits. Header read time is capped at ten
   seconds or the smaller listener request timeout.
3. Relay accepts only UUID request IDs; invalid or missing values are replaced. It extracts W3C
   trace context and starts a request span.
4. The runtime store yields an immutable snapshot. The compiled table selects one route and a
   non-blocking route semaphore admits or rejects the request.
5. A declared or known body larger than the route limit receives 413 before an upstream is touched.
   Chunked bodies are wrapped in a streaming length limiter.
6. Route authentication validates an API key or JWT. The consumed credential header is removed
   before forwarding. Gateway and route token buckets run after identity is established where
   needed.
7. Eligible GET/HEAD traffic checks its route cache. Personalized, explicitly bypassed, or
   unsupported variants skip it.
8. The selected pool immediately acquires a pool permit and endpoint lease. No request waits in an
   unbounded application queue. The circuit breaker must also issue a permit.
9. Relay rewrites the URI beneath the endpoint base path, replaces untrusted forwarding headers,
   injects trace context, and streams the body through a pooled HTTP/HTTPS Hyper client.
10. Bodyless idempotent requests may retry configured transport failures, timeouts, or statuses with
    exponential backoff and jitter. Streaming or unsafe bodies receive one attempt.
11. Response hop-by-hop headers are removed. A cacheable response is teed into a bounded buffer while
    still streaming; it is inserted only after an unambiguous successful end-of-stream.
12. Dropping or completing the response releases route, pool, endpoint, and circuit resources and
    records end-to-end metrics. Client disconnects therefore release capacity without a separate
    cleanup protocol.

Client-facing errors use a stable JSON envelope with an opaque code and request ID. Internal URLs,
transport causes, keys, and parser details stay in structured server logs.
