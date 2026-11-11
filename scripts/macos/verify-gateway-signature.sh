#!/usr/bin/env bash
# Fail closed on signer identity, runtime, entitlements, notarization or version.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENTITLEMENTS="$SCRIPT_DIR/../../packaging/macos/gateway.entitlements"
PINNED_DR='identifier "ai.duduclaw.gateway" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "7469HYQ6HH"'
DRY_RUN=0
APP= EXE= TAG=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --app) APP="$2"; shift 2 ;;
        --exe) EXE="$2"; shift 2 ;;
        --tag) TAG="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done
[[ -d "$APP" && -f "$EXE" && "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Required: --app PATH --exe PATH --tag vX.Y.Z' >&2; exit 1; }
run() {
    if [[ "$DRY_RUN" == 1 ]]; then
        printf '%q ' "$@"; printf '\n'
    else
        "$@"
    fi
}
for artifact in "$APP" "$EXE"; do
    # A bare Mach-O is "not an app" to the exec assessment on current macOS
    # (rc 3), so the CLI binary is assessed via its primary signature instead.
    if [[ "$artifact" == "$EXE" ]]; then
        spctl_args=(-a -vv -t open --context context:primary-signature)
    else
        spctl_args=(-a -vv -t exec)
    fi
    run codesign --verify --strict --deep -vv "$artifact"
    run codesign --verify -R "=$PINNED_DR" "$artifact"
    if [[ "$DRY_RUN" == 1 ]]; then
        run codesign -d -r- "$artifact"
        run codesign -d --verbose=4 "$artifact"
        run codesign -d --entitlements :- "$artifact"
        run spctl "${spctl_args[@]}" "$artifact"
        continue
    fi
    requirement="$(codesign -d -r- "$artifact" 2>&1)"
    [[ "$requirement" == *'identifier "ai.duduclaw.gateway"'* && "$requirement" == *'subject.OU] = "7469HYQ6HH"'* ]] || { echo 'Designated requirement mismatch' >&2; exit 1; }
    details="$(codesign -d --verbose=4 "$artifact" 2>&1)"
    [[ "$details" =~ flags=.*\(.*runtime.*\) ]] || { echo 'Hardened runtime missing' >&2; exit 1; }
    codesign -d --entitlements :- "$artifact" | python3 -c '
import plistlib, sys
with open(sys.argv[1], "rb") as f:
    expected = plistlib.load(f)
if plistlib.loads(sys.stdin.buffer.read()) != expected:
    sys.exit("Entitlements mismatch")
' "$ENTITLEMENTS"
    assessment="$(spctl "${spctl_args[@]}" "$artifact" 2>&1 || true)"
    [[ "$assessment" == *'source=Notarized Developer ID'* ]] || { printf 'Notarization missing for %s:\n%s\n' "$artifact" "$assessment" >&2; exit 1; }
done
run xcrun stapler validate "$APP"
if [[ "$DRY_RUN" == 1 ]]; then
    run "$EXE" version
    printf 'Require version: %s\n' "${TAG#v}"
else
    actual="$("$EXE" version)"
    # The CLI prints either X.Y.Z or duduclaw[-pro] X.Y.Z.
    [[ "$actual" == "${TAG#v}" || "$actual" == "duduclaw ${TAG#v}" || "$actual" == "duduclaw-pro ${TAG#v}" ]] || { echo "Version mismatch: $actual != $TAG" >&2; exit 1; }
fi
