#!/usr/bin/env bash
# macOS codesign + notarize + staple for the DuDuClaw .app/.dmg (TODO §D4.1).
#
# Requires (inject via CI secrets, never commit):
#   APPLE_SIGNING_IDENTITY   e.g. "Developer ID Application: Acme (TEAMID)"
#   APPLE_ID                 Apple account email used for notarytool
#   APPLE_PASSWORD           app-specific password
#   APPLE_TEAM_ID            10-char team id
#
# Usage: sign-notarize-macos.sh <path-to-.app-or-.dmg>
set -euo pipefail

ARTIFACT="${1:?usage: sign-notarize-macos.sh <artifact.app|artifact.dmg>}"
ENTITLEMENTS="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/src-tauri/entitlements.plist"

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

echo "==> notarize (notarytool, wait)"
xcrun notarytool submit "$ARTIFACT" \
  --apple-id "$APPLE_ID" \
  --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" \
  --wait

# Stapling only applies to a container (.dmg / .pkg / .app), not raw binaries.
echo "==> staple"
xcrun stapler staple "$ARTIFACT"
xcrun stapler validate "$ARTIFACT"
echo "==> done: $ARTIFACT"
