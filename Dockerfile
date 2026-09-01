# syntax=docker/dockerfile:1.7
FROM rust:1.98-bookworm AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends cmake ninja-build perl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --release --locked --package relay-cli --package relay-demo

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates dumb-init \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/relay relay

COPY --from=builder /src/target/release/relay /usr/local/bin/relay
COPY --from=builder /src/target/release/relay-demo /usr/local/bin/relay-demo
COPY --chown=relay:relay configs/basic.yaml /etc/relay/relay.yaml

USER 10001:10001
EXPOSE 8080 9090
ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["/usr/local/bin/relay", "run", "--config", "/etc/relay/relay.yaml"]
