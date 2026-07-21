# syntax=docker/dockerfile:1
#
# tael server/CLI image. Published to ghcr.io/thousandbirdsinc/tael by
# .github/workflows/docker.yml on every `v*` tag (plus `:latest`), so users
# get the bundled-DuckDB build precompiled instead of paying the ~1.4 GB C++
# amalgamation compile that `cargo install tael-cli` triggers.
#
#   docker run -p 7701:7701 -p 4317:4317 -p 8126:8126 -v tael-data:/data \
#     ghcr.io/thousandbirdsinc/tael:latest
#
# The same `tael` binary is the server (`serve`) and the query CLI, so you can
# also `docker exec <container> tael --format json services`.

# ---- builder ----
FROM rust:1-bookworm AS builder

# Bundled DuckDB compiles a large C++ amalgamation (g++ ships in the rust
# image); reqwest's default TLS links system OpenSSL, so add its headers.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Server/CLI only: the `gui` feature is opt-in (default = []), so a plain build
# already excludes the Tauri desktop app (webkit/GTK) — it can't open a window
# in a headless container anyway. `--no-default-features` is kept explicit to
# pin that intent even if defaults change. `--locked` keeps the image
# reproducible against Cargo.lock. The cargo registry and target dir are cache
# mounts, so the binary is copied out within the same RUN before the mount is
# unmounted.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p tael-cli --no-default-features \
    && cp target/release/tael /usr/local/bin/tael

# ---- runtime ----
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/tael /usr/local/bin/tael

# Bind on all interfaces inside the container (the binary defaults to
# 127.0.0.1, which is unreachable from the host) and keep WAL + storage on the
# /data volume so state survives container restarts.
ENV TAEL_OTLP_GRPC_ADDR=0.0.0.0:4317 \
    TAEL_REST_API_ADDR=0.0.0.0:7701 \
    TAEL_DD_AGENT_ADDR=0.0.0.0:8126 \
    TAEL_DATA_DIR=/data \
    TAEL_WAL_DIR=/data/wal_files

RUN mkdir -p /data
VOLUME ["/data"]

# 4317 = OTLP gRPC ingest, 7701 = REST API / CLI surface,
# 8126 = Datadog trace-agent (dd-trace) intake.
EXPOSE 4317 7701 8126

# `server status` prints {"status":"healthy"|"unreachable"} but always exits 0,
# so grep the JSON for a healthy verdict to drive the container health state.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD tael --format json server status | grep -q '"status":"healthy"'

ENTRYPOINT ["tael"]
CMD ["serve"]
