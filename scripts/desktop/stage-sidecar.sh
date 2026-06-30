#!/usr/bin/env bash
# Stage the release `duduclaw` binary into src-tauri/binaries/ with the
# host target-triple suffix Tauri's externalBin expects (TODO §D0).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

TRIPLE="$(rustc -vV | awk -F': ' '/^host:/{print $2}')"
EXT=""
case "$TRIPLE" in
  *windows*) EXT=".exe" ;;
esac

SRC="target/release/duduclaw${EXT}"
if [[ ! -f "$SRC" ]]; then
  echo "Release binary not found at $SRC — build it first:" >&2
  echo "  cargo build --release -p duduclaw-cli --bin duduclaw" >&2
  exit 1
fi

DEST_DIR="src-tauri/binaries"
mkdir -p "$DEST_DIR"
DEST="$DEST_DIR/duduclaw-${TRIPLE}${EXT}"
cp "$SRC" "$DEST"
chmod +x "$DEST"
echo "Staged sidecar: $DEST"
