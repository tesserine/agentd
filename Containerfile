FROM docker.io/library/rust:1.88-bookworm AS builder

WORKDIR /workspace
COPY . .
RUN cargo build --release --locked --bin agentd

FROM docker.io/library/debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates podman \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/agentd/tmp \
    && chmod 1777 /var/lib/agentd/tmp

COPY --from=builder /workspace/target/release/agentd /usr/local/bin/agentd

ENV CONTAINER_HOST=unix:///run/podman/podman.sock
ENV TMPDIR=/var/lib/agentd/tmp

ENTRYPOINT ["/usr/local/bin/agentd"]
CMD ["daemon", "--config", "/etc/agentd/agentd.toml"]
