#!/usr/bin/env bash
# macOS codesign + notarize + staple for a DuDuClaw .app/.dmg (TODO §D4.1).
# Shared by both desktop shells — the Tauri app (src-tauri) and the
# native-gui app (crates/duduclaw-native-gui, WP-C-M01); pass a different
# entitlements file for the latter (see usage below).
#
# Requires (inject via CI secrets, never commit):
#   APPLE_SIGNING_IDENTITY   e.g. "Developer ID Application: Acme (TEAMID)"
#   APPLE_ID                 Apple account email used for notarytool
#   APPLE_PASSWORD           app-specific password
#   APPLE_TEAM_ID            10-char team id
#
# Usage: sign-notarize-macos.sh <path-to-.app-or-.dmg> [entitlements-path]
#
#   entitlements-path defaults to src-tauri/entitlements.plist (the
#   original, production-verified path) — existing callers are unaffected.
#   Pass an alternate file (e.g.
#   crates/duduclaw-native-gui/entitlements.plist) to sign a
#   differently-scoped app.
#
# `xcrun notarytool submit --help` documents ZIP / DMG / PKG as the only
# accepted upload shapes — a raw .app directory is rejected outright. When
# ARTIFACT is a .app, this script transparently wraps it in a throwaway UDZO
# DMG (via `hdiutil create`, no external create-dmg dependency) purely to
# satisfy that upload requirement, submits the DMG, and staples the
# resulting ticket back onto the ORIGINAL .app. A notarization ticket is
# keyed by the binary's code signature, not the container it was uploaded
# in, so stapling the .app directly (rather than the scratch DMG) is
# standard practice. A .dmg passed in directly skips the wrap step entirely
# — byte-identical to the previous behavior.
set -euo pipefail

ARTIFACT="${1:?usage: sign-notarize-macos.sh <artifact.app|artifact.dmg> [entitlements-path]}"
ENTITLEMENTS="${2:-"$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/src-tauri/entitlements.plist"}"

: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"
: "${APPLE_ID:?APPLE_ID is required}"
: "${APPLE_PASSWORD:?APPLE_PASSWORD is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"

echo "==> codesign (hardened runtime, timestamped)"
codesign --force --deep --options runtime --timestamp \
  --entitlements "$ENTITLEMENTS" \
  --sign "$APPLE_SIGNING_IDENTITY" \
  "$ARTIFACT"

echo "==> verify signature"
codesign --verify --strict --verbose=2 "$ARTIFACT"

# notarytool only accepts zip/dmg/pkg — wrap a raw .app in a scratch DMG.
NOTARIZE_TARGET="$ARTIFACT"
SCRATCH_DMG=""
if [[ "$ARTIFACT" == *.app ]]; then
  SCRATCH_DMG="$(mktemp -u "${TMPDIR:-/tmp}/notarize-XXXXXX").dmg"
  echo "==> wrapping .app in scratch DMG for notarytool submission: $SCRATCH_DMG"
  hdiutil create -volname "$(basename "$ARTIFACT" .app)" \
    -srcfolder "$ARTIFACT" -ov -format UDZO "$SCRATCH_DMG"
  NOTARIZE_TARGET="$SCRATCH_DMG"
fi

echo "==> notarize (notarytool, wait)"
xcrun notarytool submit "$NOTARIZE_TARGET" \
  --apple-id "$APPLE_ID" \
  --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" \
  --wait

[[ -n "$SCRATCH_DMG" ]] && rm -f "$SCRATCH_DMG"

# Stapling only applies to a container (.dmg / .pkg / .app), not raw binaries.
# Always staple the ORIGINAL artifact (the .app, even when a scratch .dmg
# was used just to submit it) — see the shape note above.
echo "==> staple"
xcrun stapler staple "$ARTIFACT"
xcrun stapler validate "$ARTIFACT"
echo "==> done: $ARTIFACT"
