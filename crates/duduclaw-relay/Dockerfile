# DuDuClaw Cloud Relay — multi-stage build.
#
# This image is only the relay: webhook forwarding + LAN device discovery.
# It shares the main workspace's Cargo.toml/Cargo.lock (see the crate
# README), so the build context for this Dockerfile is the **repo root**,
# not this directory:
#
#   docker build -f crates/duduclaw-relay/Dockerfile -t duduclaw-relay .
#
# No TLS is terminated inside the container — deploy behind Cloud Run (or
# any reverse proxy) which terminates TLS and forwards plain HTTP/WS.

# ---- Stage 1: build ---------------------------------------------------
FROM rust:slim AS builder
WORKDIR /build

# rusqlite's "bundled" feature compiles SQLite from source (no system
# libsqlite3 needed at runtime), but still needs a C toolchain to build it.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config build-essential && \
    rm -rf /var/lib/apt/lists/*

# Workspace manifests/lockfile first (Cargo needs every member's Cargo.toml
# to resolve the workspace, even though only duduclaw-relay is compiled).
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build --release -p duduclaw-relay

# ---- Stage 2: runtime ---------------------------------------------------
# Distroless: the release binary is the only thing that needs to run, and
# "bundled" rusqlite means no system SQLite library is required.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

COPY --from=builder /build/target/release/duduclaw-relay /usr/local/bin/duduclaw-relay

# Cloud Run injects PORT; RELAY_BIND (if set) takes precedence — see
# src/config.rs. RELAY_DB_PATH defaults to a relative path, which resolves
# under WORKDIR below; point it at a mounted volume for real persistence
# (Cloud Run's local filesystem is ephemeral per instance — see README).
ENV RELAY_DB_PATH=/data/relay.db
WORKDIR /data

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/duduclaw-relay"]
