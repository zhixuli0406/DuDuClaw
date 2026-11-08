#!/usr/bin/env bash
# Sign the gateway bundle and legacy binary, notarize together, then staple.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENTITLEMENTS="$SCRIPT_DIR/../../packaging/macos/gateway.entitlements"
DRY_RUN=0
APP= EXE= OUT= PROFILE="${DUDUCLAW_NOTARY_PROFILE:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --app) APP="$2"; shift 2 ;;
        --exe) EXE="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --keychain-profile) PROFILE="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done
[[ -d "$APP" && -f "$EXE" && -n "$OUT" ]] || { echo 'Required: --app PATH --exe PATH --out FILE.app.zip' >&2; exit 1; }
: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"
if [[ -z "$PROFILE" ]]; then
    : "${APPLE_ID:?APPLE_ID is required}"
    : "${APPLE_PASSWORD:?APPLE_PASSWORD is required}"
    : "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
fi
run() {
    if [[ "$DRY_RUN" == 1 ]]; then
        printf '%q ' "$@"; printf '\n'
    else
        "$@"
    fi
}
submit() {
    if [[ "$DRY_RUN" == 1 ]]; then
        run "$@" --output-format json
    else
        "$@" --output-format json | python3 -c 'import json, sys; result = json.load(sys.stdin); sys.exit(0 if result.get("status") == "Accepted" else "Notarization was not accepted")'
    fi
}
# No --deep: the bundle contains only one executable and no nested code.
for artifact in "$EXE" "$APP"; do
    run codesign --force --options runtime --timestamp --identifier ai.duduclaw.gateway \
        --entitlements "$ENTITLEMENTS" --sign "$APPLE_SIGNING_IDENTITY" "$artifact"
done
if [[ "$DRY_RUN" == 1 ]]; then
    STAGE='/tmp/duduclaw-notary.DRY-RUN'
else
    STAGE="$(mktemp -d)"
    trap 'rm -rf "$STAGE"' EXIT
    mkdir "$STAGE/payload"
    ditto "$APP" "$STAGE/payload/DuDuClaw Gateway.app"
    ditto "$EXE" "$STAGE/payload/$(basename "$EXE")"
fi
if [[ "$DRY_RUN" == 1 ]]; then
    run mkdir -p "$STAGE/payload"
    run ditto "$APP" "$STAGE/payload/DuDuClaw Gateway.app"
    run ditto "$EXE" "$STAGE/payload/$(basename "$EXE")"
fi
run ditto -c -k --keepParent "$STAGE/payload" "$STAGE/notary.zip"
if [[ -n "$PROFILE" ]]; then
    submit xcrun notarytool submit "$STAGE/notary.zip" --wait --keychain-profile "$PROFILE"
elif [[ "$DRY_RUN" == 1 ]]; then
    # Never expose credentials in dry-run output.
    submit xcrun notarytool submit "$STAGE/notary.zip" --wait --apple-id '<APPLE_ID>' --password '<APPLE_PASSWORD>' --team-id '<APPLE_TEAM_ID>'
else
    submit xcrun notarytool submit "$STAGE/notary.zip" --wait --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID"
fi
run xcrun stapler staple "$APP"
run ditto -c -k --norsrc --noextattr --noqtn --keepParent "$APP" "$OUT"
