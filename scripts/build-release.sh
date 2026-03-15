#!/usr/bin/env bash
# Build release binaries for all supported platforms
# Usage: ./scripts/build-release.sh [VERSION]
set -euo pipefail

VERSION="${1:-0.1.0}"
DIST_DIR="dist/v${VERSION}"
mkdir -p "$DIST_DIR"

echo "Building DuDuClaw v${VERSION}..."

# Build frontend first
echo "Building frontend..."
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# Build for current platform
echo "Building Rust binary..."
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# Determine target triple
TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
BINARY="duduclaw"
if [[ "$TARGET" == *"windows"* ]]; then
  BINARY="duduclaw.exe"
fi

echo "Target: ${TARGET}"

# Copy binary to dist
cp "target/release/${BINARY}" "${DIST_DIR}/"

# Create checksum and archive
cd "${DIST_DIR}"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${BINARY}" > "${BINARY}.sha256"
else
  shasum -a 256 "${BINARY}" > "${BINARY}.sha256"
fi

if [[ "$TARGET" == *"windows"* ]]; then
  zip "duduclaw-${VERSION}-${TARGET}.zip" "${BINARY}" "${BINARY}.sha256"
else
  tar czf "duduclaw-${VERSION}-${TARGET}.tar.gz" "${BINARY}" "${BINARY}.sha256"
fi
cd ../..

echo ""
echo "Release artifacts in ${DIST_DIR}/:"
ls -lh "${DIST_DIR}/"
