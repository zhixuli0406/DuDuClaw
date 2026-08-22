#!/usr/bin/env bash
# Bundle crates/duduclaw-native-gui (the gpui-based native GUI, not the Tauri
# shell) into a standard macOS `DuDuClaw.app` (WP-C-M01 / C-line S7).
#
# This is a plain hand-rolled bundle — no Tauri/cargo-bundle involved. The
# native-gui crate is deliberately excluded from the root workspace and from
# the src-tauri tree (see that crate's Cargo.toml comment), so it gets its
# own bundling script rather than piggy-backing on `cargo tauri build`.
#
# Usage:
#   scripts/desktop/bundle-native-gui-macos.sh [target-triple]
#
#   target-triple   defaults to the host triple (aarch64-apple-darwin on
#                   Apple Silicon, x86_64-apple-darwin on Intel). Pass the
#                   other Darwin triple to cross-build (requires
#                   `rustup target add <triple>`).
#
# Env overrides:
#   OUT_DIR       where DuDuClaw.app is written (default: <repo>/dist/native-gui-macos)
#   SKIP_BUILD=1  reuse an already-built release binary instead of invoking
#                 `cargo build` again (useful for repeated local bundle/sign
#                 iterations against the same binary).
#
# Output: $OUT_DIR/DuDuClaw.app (unsigned — see sign-notarize-macos.sh for
# the codesign + notarize + staple pass).
set -euo pipefail

# Prefer the rustup shim over any ambient Homebrew rustc. On a machine with
# both Apple Silicon and Intel Homebrew installed (e.g. /usr/local/bin from
# an old Rosetta `brew` prefix), `/usr/local/bin/rustc` can shadow
# `~/.cargo/bin/rustc` in PATH — and being a plain Homebrew binary (not a
# rustup proxy) it silently ignores this crate's `rust-toolchain.toml` pin
# (1.97.1, required — see main.rs's gpui-gotchas doc comment) AND reports
# the WRONG host triple (x86_64-apple-darwin on an aarch64 machine, running
# under Rosetta), which then cascades into bindgen/libclang architecture
# mismatches. Force the correct shim first regardless of ambient PATH.
export PATH="$HOME/.cargo/bin:$PATH"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CRATE_DIR="$ROOT/crates/duduclaw-native-gui"
CRATE_NAME="duduclaw-native-gui"

HOST_TRIPLE="$(rustc -vV | awk -F': ' '/^host:/{print $2}')"
TARGET_TRIPLE="${1:-$HOST_TRIPLE}"

case "$TARGET_TRIPLE" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *)
    echo "error: unsupported target triple '$TARGET_TRIPLE' (this script is macOS-only)" >&2
    exit 1
    ;;
esac

# Bundle metadata. Deliberately ai.duduclaw.native — the Tauri desktop shell
# already owns ai.duduclaw.desktop (src-tauri/tauri.conf.json); the two must
# never collide so both can be installed side by side during the transition.
APP_NAME="DuDuClaw"
BUNDLE_ID="ai.duduclaw.native"
BUNDLE_EXECUTABLE="$CRATE_NAME"
# D7: single product name everywhere the user sees it — "DuDuClaw", not
# "DuDuClaw Native" or the crate name.
DISPLAY_NAME="DuDuClaw"
MIN_MACOS="11.0"  # matches src-tauri/tauri.conf.json macOS.minimumSystemVersion

VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$CRATE_DIR/Cargo.toml")"
if [[ -z "$VERSION" ]]; then
  echo "error: could not read version from $CRATE_DIR/Cargo.toml" >&2
  exit 1
fi

ICON_SRC="$ROOT/src-tauri/icons/icon.icns"
if [[ ! -f "$ICON_SRC" ]]; then
  echo "error: icon not found at $ICON_SRC" >&2
  exit 1
fi

OUT_DIR="${OUT_DIR:-$ROOT/dist/native-gui-macos}"
APP_PATH="$OUT_DIR/$APP_NAME.app"

echo "==> duduclaw-native-gui v$VERSION -> $APP_PATH (target $TARGET_TRIPLE)"

if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
  echo "==> SKIP_BUILD=1 — reusing existing release binary"
  if [[ "$TARGET_TRIPLE" == "$HOST_TRIPLE" ]]; then
    BIN_SRC="$CRATE_DIR/target/release/$CRATE_NAME"
    [[ -f "$BIN_SRC" ]] || BIN_SRC="$CRATE_DIR/target/$TARGET_TRIPLE/release/$CRATE_NAME"
  else
    BIN_SRC="$CRATE_DIR/target/$TARGET_TRIPLE/release/$CRATE_NAME"
  fi
else
  echo "==> cargo build --release"
  if [[ "$TARGET_TRIPLE" == "$HOST_TRIPLE" ]]; then
    # No --target on the native host: reuses target/release/ directly (and
    # any cache already sitting there), matching how devs already build this
    # crate day to day (see crate CLAUDE.md / run scripts).
    ( cd "$CRATE_DIR" && cargo build --release )
    BIN_SRC="$CRATE_DIR/target/release/$CRATE_NAME"
  else
    ( cd "$CRATE_DIR" && cargo build --release --target "$TARGET_TRIPLE" )
    BIN_SRC="$CRATE_DIR/target/$TARGET_TRIPLE/release/$CRATE_NAME"
  fi
fi

if [[ ! -f "$BIN_SRC" ]]; then
  echo "error: release binary not found at $BIN_SRC" >&2
  echo "  (build it first: cd $CRATE_DIR && cargo build --release ${TARGET_TRIPLE:+--target $TARGET_TRIPLE})" >&2
  exit 1
fi

echo "==> assembling bundle"
rm -rf "$APP_PATH"
mkdir -p "$APP_PATH/Contents/MacOS" "$APP_PATH/Contents/Resources"

cp "$BIN_SRC" "$APP_PATH/Contents/MacOS/$BUNDLE_EXECUTABLE"
chmod +x "$APP_PATH/Contents/MacOS/$BUNDLE_EXECUTABLE"
cp "$ICON_SRC" "$APP_PATH/Contents/Resources/icon.icns"

cat > "$APP_PATH/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$DISPLAY_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$DISPLAY_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleExecutable</key>
    <string>$BUNDLE_EXECUTABLE</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleSupportedPlatforms</key>
    <array>
        <string>MacOSX</string>
    </array>
    <key>LSMinimumSystemVersion</key>
    <string>$MIN_MACOS</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright Dudu Technology Ltd.</string>
</dict>
</plist>
PLIST

echo "==> done: $APP_PATH"
