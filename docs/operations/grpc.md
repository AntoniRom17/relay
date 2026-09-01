# gRPC proxying

Relay handles gRPC as transparent HTTP/2 traffic. It preserves the path and query, content type,
streaming data frames, response headers, and trailers while applying the same route, auth, limit,
health, balancing, circuit, and tracing pipeline as other requests.

Use a TLS listener for conventional gRPC clients; Relay advertises `h2` and `http/1.1` through ALPN.
Cleartext HTTP/2 depends on the client using prior knowledge because Relay does not implement an
HTTP/1.1 `h2c` upgrade. Each backend URL must identify an HTTP/2-capable HTTP or HTTPS origin.

Routes match the gRPC HTTP path, commonly `/package.Service/Method`. Do not enable retries for
streaming or body-carrying calls: Relay's replay guard already prevents them, irrespective of the
configured attempt count. Response caching is not applicable to gRPC calls.

Relay does not parse protobuf messages, perform transcoding, understand method descriptors, or
synthesize application gRPC statuses. An application error remains the upstream's trailers. A
transport failure before headers becomes a gateway HTTP error; after headers it surfaces as a body
stream failure, which clients must treat as an incomplete RPC.
