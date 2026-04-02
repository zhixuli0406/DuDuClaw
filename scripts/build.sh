#!/usr/bin/env bash
set -euo pipefail

FEATURES="${1:-default}"

echo "Building DuDuClaw with features: $FEATURES"

# Build web dashboard first
(cd web && npm ci --legacy-peer-deps && npm run build)

# Build Rust binary
cargo build --release \
  -p duduclaw-cli \
  -p duduclaw-gateway \
  --features "duduclaw-gateway/dashboard,$FEATURES"

echo "Build complete: target/release/duduclaw"
