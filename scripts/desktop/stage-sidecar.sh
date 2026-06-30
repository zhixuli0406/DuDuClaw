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

SUFFIXED="duduclaw-${TRIPLE}${EXT}"

# 1) binaries/ — where Tauri's bundler picks up externalBin for `tauri build`.
DEST_DIR="src-tauri/binaries"
mkdir -p "$DEST_DIR"
cp "$SRC" "$DEST_DIR/$SUFFIXED"
chmod +x "$DEST_DIR/$SUFFIXED"
echo "Staged sidecar: $DEST_DIR/$SUFFIXED"

# 2) target/{debug,release}/ — `tauri dev` and a bare `tauri build` run the app
# binary in place and resolve the sidecar NEXT TO THE EXECUTABLE, not from
# binaries/. Without a copy here the dev app fails to spawn the gateway.
for profile in debug release; do
  PROFILE_DIR="src-tauri/target/$profile"
  if [[ -d "$PROFILE_DIR" ]]; then
    cp "$SRC" "$PROFILE_DIR/$SUFFIXED"
    chmod +x "$PROFILE_DIR/$SUFFIXED"
    echo "Staged sidecar: $PROFILE_DIR/$SUFFIXED"
  fi
done
