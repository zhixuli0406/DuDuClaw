#!/usr/bin/env bash
# DuDuClaw Release Automation
# Usage:
#   ./scripts/release.sh <hotfix|minor|major> [--title "<theme>"] [--dry-run]
#                                                          # bump + sync all platforms
#   ./scripts/release.sh audit                              # show every platform's version + drift
#   ./scripts/release.sh verify [version]                  # confirm registries published <version>
#   ./scripts/release.sh linux-assets --target <triple> [--output-dir <dir>]
#                         [--manifest-template <json> --signing-key <offline-key>]
#                                                          # build/package, never publish
#   ./scripts/release.sh isolation-sign --help             # OFFLINE, owner present: sign the
#                                                          # native Linux isolation release
#
# isolation-sign (plan R2 §4.2) runs after CI has published the tag's Linux
# archives. It needs release/bootstrap-root.sha256 (added after the key
# ceremony; without it there is no production pin and nothing to sign). Per
# release it probes the released duduclaw-bootstrap (PRODUCTION <pin>), signs
# the isolation manifest with the target key, uploads the update-service assets
# (isolated-linux-<arch>-*) and writes SHA256SUMS-isolation.txt. Only the
# versions in ISOLATION_MIGRATION_BUNDLE_VERSIONS (owner decision Q6: v1.83.0)
# also build the migration bundle tar and need the root threshold
# (--migration-bundle --root-key R1 --root-key R2) plus the legacy minisign key.
#
# --title "<theme>" sets the one-line release theme in the CHANGELOG version
# header (house style: "## [1.36.0] - 2026-07-15 — <theme>"). On a bump the
# curated [Unreleased] section is RENAMED to the new version (its hand-written
# notes are preserved) and a fresh empty [Unreleased] is left on top.
#
# Why this exists: the version lives in MANY platform manifests (Cargo, PyPI's
# pyproject.toml, the npm wrapper + 5 platform sub-packages, README badges). When
# a bump is done by hand it's easy to update Cargo + README and silently forget
# pyproject.toml / npm — which then freezes PyPI/npm at the old version (the CI
# `pypi-publish` job builds the stale pyproject version and `skip-existing` makes
# the miss invisible). This script bumps EVERY manifest from one place and then
# ASSERTS they all reached the new version, so no platform can be left behind.
#
# Release lanes: minor/major retain the full signed delivery review; hotfix
# automatically signs five unchanged items from the preceding public assertion.
# patch is a deprecated alias for hotfix. Every lane publishes every channel.
#
# Steps (bump mode):
#   1. Validate arguments and clean working tree
#   2. Print platform version audit (minor/major only)
#   3. Preflight main macOS + Windows CI, enterprise prerequisites and branch
#   4. Verify/install signed delivery assertion (automatic for hotfix)
#   5. Bump every manifest, assert versions, move curated CHANGELOG notes
#   6. cargo check workspace + commercial; re-resolve desktop Cargo.lock
#   7. Generate distributor brief (minor/major only)
#   8. Commit, validate the actual bump, prepare core + desktop tags
#   9. Build all enterprise assets, sync the control-plane allowlist and publish
#      the distributor console manuals (publish-manuals.py; skip with
#      DUDUCLAW_SKIP_MANUALS=1, which leaves a gap that fails the release gate)
#  10. Print branch/PR/check/merge/tag/verify commands for the owner
#
# Runtime note (2026-09-08 incident): a full bump run takes roughly 20-45
# minutes end to end — `cargo check --workspace` right after the version
# bump, then the enterprise pro-image build and the bare-metal binary asset
# steps further down, are the bulk of that time. Always run this DETACHED,
# e.g.:
#   nohup ./scripts/release.sh minor --title "..." > release.log 2>&1 &
# Never invoke it from inside a tool/CI wrapper with a short (~10 min)
# foreground timeout — on 2026-09-08 exactly that killed a run right after
# the commit+tag but before the pro image / binary asset / allowlist steps
# ran, and those had to be replayed by hand afterward.
#
# Enterprise images default to CLOUD_BUILD=1 (native amd64 Cloud Build).
# Set CLOUD_BUILD=0 only for a deliberate local Docker build.
set -euo pipefail

# --- Config ---
WORKSPACE_TOML="Cargo.toml"
PYPI_PKG="duduclaw"   # PyPI + npm package name (used by `verify`)
NPM_PKG="duduclaw"
DRY_RUN=false
export CLOUD_BUILD="${CLOUD_BUILD:-1}"

# Extended-regex semver matcher (no leading anchor — reused in several patterns).
SEMVER='[0-9]+\.[0-9]+\.[0-9]+'

# Control-plane regions are independent of the artifact registry region.
# Both callers declare CONTROL_PLANE_REGIONS locally where applicable.
resolve_cp_regions() {
    local configured
    if [[ "${CP_REGIONS+x}" == "x" ]]; then
        configured="$CP_REGIONS"
    elif [[ -n "${CP_REGION:-}" ]]; then
        configured="$CP_REGION"
        echo "WARNING: CP_REGION-only fallback uses $CP_REGION; the other control-plane region is skipped. Set CP_REGIONS to sync both regions." >&2
    else
        configured="asia-east1 us-east4"
    fi
    # Split whitespace only; never expand region names as filesystem globs.
    configured="${configured//$'\n'/ }"
    read -r -a CONTROL_PLANE_REGIONS <<< "$configured"
    if [[ ${#CONTROL_PLANE_REGIONS[@]} -eq 0 ]]; then
        echo "ERROR: CP_REGIONS must contain at least one region." >&2
        return 1
    fi
}

# Read the revision actually serving traffic, never just the proposed template.
# Empty/unreadable or split traffic fails the caller's comparisons closed.
read_cp_serving_env() {
    local service="$1" project="$2" region="$3" env_json
    CP_SERVING_REV="$(gcloud run services describe "$service" --project "$project" --region "$region" \
        --format=json 2>/dev/null | python3 -c 'import json, sys
live = [t["revisionName"] for t in json.load(sys.stdin)["status"].get("traffic", []) if t.get("percent")]
print(live[0] if len(live) == 1 else "")' 2>/dev/null || true)"
    CP_SERVING_VERSIONS=""
    CP_SERVING_MANIFEST=""
    if [[ -n "$CP_SERVING_REV" ]]; then
        env_json="$(gcloud run revisions describe "$CP_SERVING_REV" --project "$project" --region "$region" \
            --format=json 2>/dev/null || true)"
        CP_SERVING_VERSIONS="$(printf '%s' "$env_json" | python3 -c 'import json, sys
env = json.load(sys.stdin)["spec"]["containers"][0].get("env", [])
print(next((e.get("value", "") for e in env if e["name"] == "DUDUCLAW_PRO_VERSIONS"), ""))' 2>/dev/null || true)"
        CP_SERVING_MANIFEST="$(printf '%s' "$env_json" | python3 -c 'import json, sys
env = json.load(sys.stdin)["spec"]["containers"][0].get("env", [])
print(next((e.get("value", "") for e in env if e["name"] == "DUDUCLAW_GENERAL_DELIVERY_MANIFEST"), ""))' 2>/dev/null || true)"
    fi
    echo "  Control-plane [$region] serving revision: ${CP_SERVING_REV:-unknown}"
}

verify_cp_regions() {
    local service="$1" project="$2" want="$3" cp_region rc=0 checks=0 passed=0
    local CP_SERVING_REV CP_SERVING_VERSIONS CP_SERVING_MANIFEST
    local first=1 baseline_versions="" baseline_manifest=""
    local -a CONTROL_PLANE_REGIONS=()
    resolve_cp_regions || return 1
    for cp_region in "${CONTROL_PLANE_REGIONS[@]}"; do
        read_cp_serving_env "$service" "$project" "$cp_region"
        checks=$((checks + 2))
        if [[ ",$CP_SERVING_VERSIONS," == *",v$want,"* ]]; then
            echo "  pro-offer [$cp_region] v$want OK ($CP_SERVING_REV)"
            passed=$((passed + 1))
        else
            echo "  pro-offer [$cp_region] v$want MISSING (revision ${CP_SERVING_REV:-unknown}, offers ${CP_SERVING_VERSIONS:-unknown})"
            rc=1
        fi
        if printf '%s' "$CP_SERVING_MANIFEST" | python3 -c 'import json, re, sys
doc = json.load(sys.stdin)
assert doc["schema_version"] == 1 and doc["profile"] == "general-distributor" and doc["platform"] == "linux/amd64"
assert any(a["version"] == sys.argv[1] and re.fullmatch(r"[0-9a-f]{64}", a["sha256"]) for a in doc["artifacts"])
' "v$want" 2>/dev/null && \
            { [[ "$first" == "1" ]] || [[ "$CP_SERVING_VERSIONS" == "$baseline_versions" && "$CP_SERVING_MANIFEST" == "$baseline_manifest" ]]; }; then
            echo "  delivery-manifest [$cp_region] v$want OK ($CP_SERVING_REV)"
            passed=$((passed + 1))
        else
            echo "  delivery-manifest [$cp_region] v$want MISSING/INVALID or differs between regions ($CP_SERVING_REV)"
            rc=1
        fi
        if [[ "$first" == "1" ]]; then
            baseline_versions="$CP_SERVING_VERSIONS"
            baseline_manifest="$CP_SERVING_MANIFEST"
            first=0
        fi
    done
    echo "Control-plane verification: $passed/$checks checks passed across ${#CONTROL_PLANE_REGIONS[@]} region(s)."
    return "$rc"
}

# --- Enumerate every version-bearing platform manifest as "kind|path" lines. ---
# Adding a new publish target = add it here, and audit/bump/assert all pick it up.
platform_manifests() {
    echo "cargo|$WORKSPACE_TOML"
    local t
    for t in crates/*/Cargo.toml; do
        # Only crates with a direct (non-workspace-inherited) version line.
        if [[ -f "$t" ]] && grep -qE "^version = \"$SEMVER\"" "$t"; then
            echo "cargo|$t"
        fi
    done
    if [[ -f pyproject.toml ]]; then echo "pyproject|pyproject.toml"; fi
    # Python SDK fallback version literal (__init__.py). pyproject is the real
    # publish version; this only matters for source-tree imports, but it drifts
    # silently if not synced (was stuck at 1.4.27 for many releases).
    if [[ -f python/duduclaw/__init__.py ]]; then echo "pyinit|python/duduclaw/__init__.py"; fi
    local p
    for p in npm/*/package.json; do
        if [[ -f "$p" ]]; then echo "npm|$p"; fi
    done
    local r
    for r in README.md README.zh-TW.md README.ja.md; do
        if [[ -f "$r" ]]; then echo "badge|$r"; fi
    done
    # Installer fallback versions (used only when the GitHub "latest release" API
    # is unreachable). These silently drifted to ancient v0.x for many releases,
    # causing a 404 → source-build fallback (MSVC + ~1.5h compile) on Windows.
    if [[ -f scripts/install.sh ]]; then echo "installer_sh|scripts/install.sh"; fi
    if [[ -f scripts/install.ps1 ]]; then echo "installer_ps1|scripts/install.ps1"; fi
    # Desktop shell (Tauri). Version must equal the core version or the updater
    # ships a shell/core mismatch (§D4.4). Enumerated here so the desktop app
    # can never silently freeze behind the core again (it sat at 1.33.0 for 11
    # releases because this file was bumped by hand and the desktop-v* tag was
    # a separate manual step nobody ran).
    if [[ -f src-tauri/tauri.conf.json ]]; then echo "tauri|src-tauri/tauri.conf.json"; fi
    # Official MCP Registry metadata. Both `version` fields (top level and
    # packages[0]) must name an npm version that is actually published, so it
    # moves with the npm wrapper. It sat at 1.56.0 for fourteen releases
    # because nothing bumped it.
    if [[ -f distribution/registries/mcp/server.json ]]; then
        echo "mcp_registry|distribution/registries/mcp/server.json"
    fi
}

# --- Read the current version out of a manifest, by kind. (Never fails: empty on miss.) ---
extract_version() {
    local file="$1" kind="$2"
    case "$kind" in
        cargo|pyproject)
            { grep -m1 -E "^version = \"$SEMVER\"" "$file" \
                | sed -E "s/^version = \"($SEMVER)\".*/\1/"; } 2>/dev/null || true
            ;;
        pyinit)
            { grep -m1 -E "^[[:space:]]*__version__ = \"$SEMVER\"" "$file" \
                | sed -E "s/^[[:space:]]*__version__ = \"($SEMVER)\".*/\1/"; } 2>/dev/null || true
            ;;
        npm|tauri)
            { grep -m1 -E "\"version\"[[:space:]]*:" "$file" \
                | sed -E "s/.*\"version\"[[:space:]]*:[[:space:]]*\"($SEMVER)\".*/\1/"; } 2>/dev/null || true
            ;;
        mcp_registry)
            # Two `version` fields (top level and packages[0]). Both must agree:
            # the registry checks packages[0].version against npm. One value
            # when they match; "a/b" (never equal to a release version, so the
            # audit flags it and the post-bump assert fails) when they do not;
            # empty when either is missing.
            { local vs n
              vs="$(grep -E "\"version\"[[:space:]]*:" "$file" \
                | sed -E "s/.*\"version\"[[:space:]]*:[[:space:]]*\"($SEMVER)\".*/\1/")"
              n="$(printf '%s\n' "$vs" | grep -c .)"
              if [[ "$n" -ge 2 ]]; then
                  printf '%s\n' "$vs" | sort -u | paste -sd/ -
              fi; } 2>/dev/null || true
            ;;
        badge)
            { grep -m1 -oE "badge/version-$SEMVER" "$file" \
                | sed -E "s|badge/version-($SEMVER)|\1|"; } 2>/dev/null || true
            ;;
        installer_sh)
            { grep -m1 -E "^FALLBACK_VERSION=\"$SEMVER\"" "$file" \
                | sed -E "s/^FALLBACK_VERSION=\"($SEMVER)\".*/\1/"; } 2>/dev/null || true
            ;;
        installer_ps1)
            { grep -m1 -E "^\\\$FallbackVersion = \"$SEMVER\"" "$file" \
                | sed -E "s/^\\\$FallbackVersion = \"($SEMVER)\".*/\1/"; } 2>/dev/null || true
            ;;
    esac
}

# --- Audit: print each platform's current version, flagging drift vs Cargo. ---
# Returns non-zero if any manifest disagrees with the Cargo workspace version.
run_audit() {
    local truth="$1" drift=0 kind file v flag
    echo "Platform version audit (source of truth: Cargo workspace = $truth)"
    echo "------------------------------------------------------------------"
    while IFS='|' read -r kind file; do
        v="$(extract_version "$file" "$kind")"
        flag=""
        if [[ "$v" != "$truth" ]]; then
            flag="   <-- DRIFT (publishes/freezes at $v)"
            drift=1
        fi
        printf "  %-34s %-10s [%s]%s\n" "$file" "${v:-?}" "$kind" "$flag"
    done < <(platform_manifests)
    echo "------------------------------------------------------------------"
    return $drift
}

# --- Darwin platform label, derived from the ACTUAL build host triple
# (`rustc -vV`), never assumed as darwin-arm64. A native `cargo build`
# produces a binary matching whatever toolchain is active — an x86_64 Rosetta
# toolchain running on Apple Silicon hardware still builds an x86_64 binary —
# so labeling the pro-bin asset by hardware alone would silently mislabel an
# x86_64 binary as darwin-arm64 (2026-08-17 live-verification finding: this
# dev machine's own `rustc -vV` host is x86_64-apple-darwin, and its deployed
# `/usr/local/bin/duduclaw-pro` is an x86_64 Mach-O binary). Shared by the
# pro-bin build/package step and `run_verify`'s pro-bin-mac check so both
# agree on which platform object they mean. Echoes "" (never guesses) on any
# host triple that is not a recognized darwin target — fail-closed: callers
# must skip the darwin asset rather than risk a wrong label.
darwin_platform_label() {
    local host
    host="$(rustc -vV 2>/dev/null | grep '^host:' | awk '{print $2}')"
    case "$host" in
        aarch64-apple-darwin) echo "darwin-arm64" ;;
        x86_64-apple-darwin) echo "darwin-x64" ;;
        *) echo "" ;;
    esac
}

# --- Verify: query the public registries for an actually-published version. ---
run_verify() {
    local want="$1" rc=0
    echo "Verifying public registries published version: $want"
    echo "------------------------------------------------------------------"

    # PyPI JSON API
    local pypi
    pypi="$(curl -fsSL "https://pypi.org/pypi/$PYPI_PKG/json" 2>/dev/null \
        | grep -oE "\"version\"[[:space:]]*:[[:space:]]*\"$SEMVER\"" | head -1 \
        | sed -E "s/.*\"($SEMVER)\"/\1/")" || true
    if [[ "$pypi" == "$want" ]]; then
        printf "  %-10s %-10s OK\n" "PyPI" "$pypi"
    else
        printf "  %-10s %-10s MISMATCH (expected %s)\n" "PyPI" "${pypi:-unreachable}" "$want"
        rc=1
    fi

    # npm registry
    local npm
    npm="$(curl -fsSL "https://registry.npmjs.org/$NPM_PKG/latest" 2>/dev/null \
        | grep -oE "\"version\"[[:space:]]*:[[:space:]]*\"$SEMVER\"" | head -1 \
        | sed -E "s/.*\"($SEMVER)\"/\1/")" || true
    if [[ "$npm" == "$want" ]]; then
        printf "  %-10s %-10s OK\n" "npm" "$npm"
    else
        printf "  %-10s %-10s MISMATCH (expected %s)\n" "npm" "${npm:-unreachable}" "$want"
        rc=1
    fi

    # Enterprise pro-image + offline OEM tar (commercial checkout only — the
    # build script is absent on public checkouts, and both artifacts are private).
    if [[ -x "commercial/duduclaw-pro-gateway/build-image.sh" ]] && command -v gcloud >/dev/null 2>&1; then
        local proj region bucket img tar
        proj="${DUDUCLAW_GCP_PROJECT:-$(gcloud config get-value project 2>/dev/null)}"
        region="${DUDUCLAW_GCP_REGION:-asia-east1}"
        bucket="${DUDUCLAW_IMAGE_TAR_BUCKET:-duduclaw-oem-images}"
        if [[ -n "$proj" && "$proj" != "(unset)" ]]; then
            img="${region}-docker.pkg.dev/${proj}/duduclaw/duduclaw-pro:v${want}"
            if gcloud artifacts docker images describe "$img" >/dev/null 2>&1; then
                printf "  %-10s %-10s OK\n" "pro-image" "v$want"
            else
                printf "  %-10s %-10s MISSING — re-run: commercial/duduclaw-pro-gateway/build-image.sh v%s\n" \
                    "pro-image" "v$want" "$want"
                rc=1
            fi
            tar="gs://${bucket}/duduclaw-pro/duduclaw-pro-v${want}.tar.gz"
            if gcloud storage objects describe "$tar" >/dev/null 2>&1; then
                printf "  %-10s %-10s OK\n" "oem-tar" "v$want"
            else
                printf "  %-10s %-10s MISSING (%s) — issued packs fall back to manual docker save\n" \
                    "oem-tar" "v$want" "$tar"
                rc=1
            fi

            # Bare-metal Pro binary assets (P0 control-plane auto-update channel).
            # darwin: which platform object to check for is decided by the
            # SAME host-triple detection the build/package step uses (see
            # darwin_platform_label above) — a verify run on a non-darwin host
            # (e.g. Linux CI) or an unrecognized triple can't know which
            # object to expect, so it skips this row rather than guessing.
            local darwin_label
            darwin_label="$(darwin_platform_label)"
            if [[ -n "$darwin_label" ]]; then
                tar="gs://${bucket}/duduclaw-pro-bin/v${want}/duduclaw-pro-${darwin_label}.tar.gz"
                if gcloud storage objects describe "$tar" >/dev/null 2>&1; then
                    printf "  %-10s %-10s OK\n" "pro-bin-mac" "v$want"
                else
                    printf "  %-10s %-10s MISSING (%s) — Pro auto-update has no %s asset\n" \
                        "pro-bin-mac" "v$want" "$tar" "$darwin_label"
                    rc=1
                fi
            else
                echo "  pro-bin-mac: skipped (could not determine a darwin host triple via rustc -vV)"
            fi
            tar="gs://${bucket}/duduclaw-pro-bin/v${want}/duduclaw-pro-linux-x64.tar.gz"
            if gcloud storage objects describe "$tar" >/dev/null 2>&1; then
                printf "  %-10s %-10s OK\n" "pro-bin-linux" "v$want"
            else
                printf "  %-10s %-10s MISSING (%s) — Pro auto-update has no linux-x64 asset\n" \
                    "pro-bin-linux" "v$want" "$tar"
                rc=1
            fi

            verify_cp_regions "${DUDUCLAW_CP_SERVICE:-duduclaw-control-plane}" "$proj" "$want" || rc=1
        else
            echo "  pro-image / oem-tar: NOT VERIFIED (no GCP project configured)"
            rc=1
        fi
    elif [[ "${DUDUCLAW_RELEASE_WITHOUT_PRO:-0}" == "1" ]]; then
        echo "  enterprise: skipped (DUDUCLAW_RELEASE_WITHOUT_PRO=1)"
    else
        # A public checkout cannot see the private assets; saying nothing here
        # is how v1.76.1 shipped with no enterprise channel at all.
        echo "  enterprise: NOT VERIFIED — needs the commercial checkout and gcloud"
        rc=1
    fi

    echo "------------------------------------------------------------------"
    if [[ $rc -ne 0 ]]; then
        echo "One or more registries are behind. The CI release.yml jobs for those"
        echo "platforms either skipped (stale manifest + skip-existing) or lack"
        echo "credentials (PYPI_TRUSTED_PUBLISHER / PYPI_TOKEN / NPM_TOKEN)."
    fi
    return $rc
}

# --- isolation-sign: offline signing of the native Linux isolation release (plan R2 §4.2) ---
# Owner decision Q6 (2026-11-17): ONLY v1.83.0 ships a migration bundle signed
# by the root threshold; later versions reach migrated hosts through the update
# service. Adding a version here is an owner decision, never a routine edit.
ISOLATION_MIGRATION_BUNDLE_VERSIONS="1.83.0"
ISOLATION_PIN_FILE="release/bootstrap-root.sha256"
ISOLATION_TEMPLATE="release/isolation/manifest-template.json"
ISOLATION_REPO="zhixuli0406/DuDuClaw"
ISOLATION_WORK=""

isolation_sign_usage() {
    cat <<'EOF'
Usage: scripts/release.sh isolation-sign \
         --target-key <target key handle> \
         --root-metadata <root.json> --root-signatures <root.json.signatures.json> \
         --output-dir <new directory> [--ssh-keygen <Homebrew ssh-keygen>] \
         [--manifest-valid-days <1-1830, default 365>] \
         [--migration-bundle --root-key <R1 key handle> --root-key <R2 key> \
          [--legacy-minisign-key <default ~/.minisign/duduclaw-release.key>] \
          [--authorization-valid-days <1-1830, default 730>]] \
         [--no-upload]

OFFLINE signing of the native Linux isolation release (plan R2 §4.2). Run it
attended, in an interactive terminal, on the release machine with the hardware
key attached, from a clean checkout of tag v<version>, after CI has published
that tag's Linux archives. Never in CI; never with stdin/stdout sent to a log.

Steps, in order (any failure stops everything; no output is ever overwritten):
  0. Preconditions: not CI, interactive terminal, clean tree at tag v<version>,
     release/bootstrap-root.sha256 present (absent = no production pin = nothing
     to sign), SHA-256 of --root-metadata equal to that pin, gh/minisign/ssh-keygen.
  For x86_64 and aarch64:
  1. Download duduclaw-linux-<arch>.tar.gz (+ .sha256, .minisig) of the tag; verify
     the CI minisign signature with the official release key and its exact
     trusted comment; extract duduclaw-bootstrap and execute --print-root-pin
     (natively on Linux of that arch, otherwise `docker run --network none`
     ubuntu:22.04): it must print exactly PRODUCTION <pin>.
  2. The target key signs the isolation manifest: release/isolation/manifest-template.json
     rendered for this version/arch (serial = major*10^6+minor*10^3+patch) over the
     binaries of that same released archive; ssh-keygen asks for touch + PIN.
  Only for ISOLATION_MIGRATION_BUNDLE_VERSIONS (owner decision Q6: ONLY v1.83.0),
  which also REQUIRE --migration-bundle (every other version refuses it):
  3. Assemble the bootstrap inbox and the plain-ustar migration bundle
     duduclaw-isolated-linux-<arch>-<version>.tar.
  4. The root threshold (R1 touch + PIN, R2 passphrase) signs its
     BundleAuthorization; the legacy release minisign key signs the tar
     (minisign asks for its password); both are verified before going on.
  Then:
  5. Write <output-dir>/SHA256SUMS-isolation.txt from the exact bytes to upload,
     plus "<pin>  root.json" (the root metadata fingerprint).
  6. After you type UPLOAD: upload the update-service assets isolated-linux-<arch>-*
     (and, for the bundle version, duduclaw-bootstrap-linux-<arch> and the four
     bundle files) to the GitHub release without replacing any asset, then
     download every uploaded asset again and compare its SHA-256.
Publishing SHA256SUMS-isolation.txt on the website and the licensed console is
the separate [external] gate; this script never publishes it there.

Secrets: key files and key handles are opened only by ssh-keygen / minisign,
which prompt on the terminal. This script never copies, prints or logs a key,
passphrase or PIN; its temporary directory holds public release data only and
is removed on exit.
EOF
}

isolation_fail() {
    echo "Error: $*" >&2
    return 1
}

isolation_sha256() {
    python3 -c 'import hashlib, sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as source:
    for chunk in iter(lambda: source.read(1 << 20), b""):
        digest.update(chunk)
print(digest.hexdigest())' "$1"
}

# Exact minisign text: four lines, prehashed (ED) signature, exact trusted comment.
isolation_check_minisig() {
    python3 - "$1" "$2" <<'PY'
import base64, binascii, sys
try:
    with open(sys.argv[1], "rb") as source:
        lines = source.read(4096).decode("utf-8").split("\n")
    ok = (len(lines) == 5 and lines[4] == "" and lines[0].startswith("untrusted comment: ")
          and base64.b64decode(lines[1], validate=True)[:2] == b"ED"
          and lines[2] == "trusted comment: " + sys.argv[2])
except (OSError, UnicodeDecodeError, binascii.Error):
    ok = False
sys.exit(0 if ok else 1)
PY
}

# Execute the verifier's read-only --print-root-pin; its stdout is the result.
# $1 is a private copy named duduclaw-bootstrap in its own directory.
isolation_probe_pin() {
    local binary="$1" arch="$2" host_arch platform
    host_arch="$(uname -m)"
    if [[ "$host_arch" == "arm64" ]]; then host_arch="aarch64"; fi
    if [[ "$(uname -s)" == "Linux" && "$host_arch" == "$arch" ]]; then
        env -i "$binary" --print-root-pin
        return
    fi
    case "$arch" in
        x86_64) platform="linux/amd64" ;;
        aarch64) platform="linux/arm64" ;;
        *) return 1 ;;
    esac
    command -v docker >/dev/null 2>&1 \
        || isolation_fail "executing the $arch verifier here needs docker (or run on Linux $arch)" || return 1
    docker run --rm --network none --platform "$platform" \
        -v "$(dirname -- "$binary"):/probe:ro" ubuntu:22.04 /probe/duduclaw-bootstrap --print-root-pin
}

run_isolation_sign() {
    local target_key="" root_metadata="" root_signatures="" output_dir="" ssh_keygen="ssh-keygen"
    local manifest_days=365 authorization_days=730 migration_bundle=0 upload=1
    local legacy_minisign_key="$HOME/.minisign/duduclaw-release.key"
    local -a root_keys=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help) isolation_sign_usage; return 0 ;;
            --target-key) target_key="${2:?--target-key needs a value}"; shift 2 ;;
            --root-metadata) root_metadata="${2:?--root-metadata needs a value}"; shift 2 ;;
            --root-signatures) root_signatures="${2:?--root-signatures needs a value}"; shift 2 ;;
            --output-dir) output_dir="${2:?--output-dir needs a value}"; shift 2 ;;
            --ssh-keygen) ssh_keygen="${2:?--ssh-keygen needs a value}"; shift 2 ;;
            --manifest-valid-days) manifest_days="${2:?--manifest-valid-days needs a value}"; shift 2 ;;
            --authorization-valid-days) authorization_days="${2:?--authorization-valid-days needs a value}"; shift 2 ;;
            --root-key) root_keys+=("${2:?--root-key needs a value}"); shift 2 ;;
            --legacy-minisign-key) legacy_minisign_key="${2:?--legacy-minisign-key needs a value}"; shift 2 ;;
            --migration-bundle) migration_bundle=1; shift ;;
            --no-upload) upload=0; shift ;;
            *) isolation_fail "unknown isolation-sign option '$1' (see --help)"; return 1 ;;
        esac
    done

    # 0. Preconditions, cheapest and most decisive first.
    if [[ -n "${CI:-}" || -n "${GITHUB_ACTIONS:-}" ]]; then
        isolation_fail "isolation-sign uses offline keys and never runs in CI"; return 1
    fi
    local bundle_release=0 listed
    local -a bundle_versions=()
    read -r -a bundle_versions <<< "$ISOLATION_MIGRATION_BUNDLE_VERSIONS"
    for listed in "${bundle_versions[@]}"; do
        if [[ "$listed" == "$CURRENT_VERSION" ]]; then bundle_release=1; fi
    done
    if [[ "$migration_bundle" == "1" && "$bundle_release" == "0" ]]; then
        isolation_fail "owner decision Q6: only v$ISOLATION_MIGRATION_BUNDLE_VERSIONS ships a migration bundle;" \
            "v$CURRENT_VERSION reaches migrated hosts through the update service. Drop --migration-bundle."
        return 1
    fi
    if [[ "$migration_bundle" == "0" && "$bundle_release" == "1" ]]; then
        isolation_fail "v$CURRENT_VERSION is the migration-bundle release (owner decision Q6);" \
            "pass --migration-bundle --root-key <R1> --root-key <R2>."
        return 1
    fi
    if [[ -z "$target_key" || -z "$root_metadata" || -z "$root_signatures" || -z "$output_dir" ]]; then
        isolation_fail "--target-key, --root-metadata, --root-signatures and --output-dir are required (see --help)"
        return 1
    fi
    if [[ "$migration_bundle" == "1" ]]; then
        if [[ ${#root_keys[@]} -ne 2 || "${root_keys[0]}" == "${root_keys[1]}" ]]; then
            isolation_fail "--migration-bundle needs exactly two distinct --root-key (the 2-of-3 threshold)"; return 1
        fi
    elif [[ ${#root_keys[@]} -ne 0 ]]; then
        isolation_fail "--root-key is only used with --migration-bundle"; return 1
    fi
    local days
    for days in "$manifest_days" "$authorization_days"; do
        if [[ ! "$days" =~ ^[1-9][0-9]{0,3}$ ]] || (( days > 1830 )); then
            isolation_fail "validity must be 1 to 1830 days"; return 1
        fi
    done
    if [[ ! -t 0 ]]; then
        isolation_fail "isolation-sign must run attended in an interactive terminal (keys prompt on the tty)"
        return 1
    fi
    if [[ ! -f "$ISOLATION_PIN_FILE" || -L "$ISOLATION_PIN_FILE" ]]; then
        isolation_fail "$ISOLATION_PIN_FILE is absent: there is no production root pin, so no isolation" \
            "release can be signed (the key ceremony adds it through a reviewed PR)."
        return 1
    fi
    local pin
    pin="$(python3 -c 'import importlib.util, sys
spec = importlib.util.spec_from_file_location("packager", "scripts/package-linux-release.py")
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)
pin = packager.read_pin_file(sys.argv[1])
if pin is None:
    sys.exit("production pin file is absent")
print(pin)' "$ISOLATION_PIN_FILE")"
    local tag="v$CURRENT_VERSION" head tag_commit
    head="$(git rev-parse HEAD)"
    tag_commit="$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}" || true)"
    if [[ -z "$tag_commit" || "$head" != "$tag_commit" ]]; then
        isolation_fail "check out tag $tag first: the template, pin file and released archives must belong to it"
        return 1
    fi
    if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
        isolation_fail "the working tree has uncommitted changes"; return 1
    fi
    if [[ ! -f "$root_metadata" || -L "$root_metadata" || ! -f "$root_signatures" || -L "$root_signatures" ]]; then
        isolation_fail "--root-metadata and --root-signatures must be regular files"; return 1
    fi
    if [[ "$(isolation_sha256 "$root_metadata")" != "$pin" ]]; then
        isolation_fail "SHA-256 of --root-metadata differs from $ISOLATION_PIN_FILE"; return 1
    fi
    local tool
    for tool in gh minisign "$ssh_keygen"; do
        if ! command -v "$tool" >/dev/null 2>&1; then isolation_fail "$tool is not on PATH"; return 1; fi
    done
    if [[ "$migration_bundle" == "1" && ! -f "$legacy_minisign_key" ]]; then
        isolation_fail "legacy release minisign key not found (--legacy-minisign-key)"; return 1
    fi
    local legacy_public
    legacy_public="$(sed -n 's|^pub const LEGACY_RELEASE_MINISIGN_KEY: &str = "\([A-Za-z0-9+/=]*\)";$|\1|p' \
        crates/duduclaw-isolation/src/update.rs)"
    if [[ ! "$legacy_public" =~ ^RW[A-Za-z0-9+/]{54}$ ]]; then
        isolation_fail "cannot read LEGACY_RELEASE_MINISIGN_KEY from duduclaw-isolation"; return 1
    fi
    if [[ -e "$output_dir" || -L "$output_dir" ]]; then
        isolation_fail "--output-dir must not exist yet (outputs are never overwritten)"; return 1
    fi
    mkdir -m 0700 -- "$output_dir"
    ISOLATION_WORK="$(mktemp -d "${TMPDIR:-/tmp}/duduclaw-isolation-sign.XXXXXX")"
    trap 'rm -rf -- "$ISOLATION_WORK"' EXIT

    local arch triple archive bundle_name verifier probe_output expires asset
    local -a uploads=()
    expires=$(( $(date +%s) + authorization_days * 86400 ))
    for arch in x86_64 aarch64; do
        if [[ "$arch" == "x86_64" ]]; then
            triple="x86_64-unknown-linux-gnu"; archive="duduclaw-linux-x64.tar.gz"
        else
            triple="aarch64-unknown-linux-gnu"; archive="duduclaw-linux-arm64.tar.gz"
        fi
        echo "== $arch: $archive of $tag"
        mkdir -m 0700 -- "$ISOLATION_WORK/$arch" "$ISOLATION_WORK/$arch/probe"
        gh release download "$tag" --repo "$ISOLATION_REPO" --dir "$ISOLATION_WORK/$arch" \
            --pattern "$archive" --pattern "$archive.sha256" --pattern "$archive.minisig"
        minisign -V -q -P "$legacy_public" -m "$ISOLATION_WORK/$arch/$archive" -x "$ISOLATION_WORK/$arch/$archive.minisig"
        if ! isolation_check_minisig "$ISOLATION_WORK/$arch/$archive.minisig" "duduclaw $tag $archive"; then
            isolation_fail "$archive.minisig is not the release job's prehashed signature for $tag"; return 1
        fi
        # 1. The released verifier must be a PRODUCTION build of exactly this pin.
        python3 scripts/package-linux-release.py --target "$triple" \
            --extract-bootstrap "$ISOLATION_WORK/$arch/$archive" --output-dir "$ISOLATION_WORK/$arch/verifier"
        verifier="$ISOLATION_WORK/$arch/verifier/duduclaw-bootstrap-linux-$arch"
        cp -- "$verifier" "$ISOLATION_WORK/$arch/probe/duduclaw-bootstrap"
        chmod 0755 "$ISOLATION_WORK/$arch/probe/duduclaw-bootstrap"
        probe_output="$(isolation_probe_pin "$ISOLATION_WORK/$arch/probe/duduclaw-bootstrap" "$arch")"
        if [[ "$probe_output" != "PRODUCTION $pin" ]]; then
            # The probe output itself is not echoed; only the expectation is named.
            isolation_fail "the released $arch duduclaw-bootstrap does not print PRODUCTION <$ISOLATION_PIN_FILE>"
            return 1
        fi
        # 2. Target key: isolation manifest over the binaries of this released archive.
        local -a package_args=(--target "$triple" --from-release-archive "$ISOLATION_WORK/$arch/$archive"
            --manifest-template "$ISOLATION_TEMPLATE" --manifest-valid-days "$manifest_days"
            --signing-key "$target_key" --ssh-keygen "$ssh_keygen" --output-dir "$output_dir")
        if [[ "$migration_bundle" == "1" ]]; then
            package_args+=(--bootstrap-root "$root_metadata" --bootstrap-root-signatures "$root_signatures")
        fi
        echo "Target key ($arch manifest): touch the key and enter its PIN when ssh-keygen asks."
        python3 scripts/package-linux-release.py "${package_args[@]}"
        for asset in "$output_dir/isolated-linux-$arch-"*; do
            if [[ -f "$asset" && ! -L "$asset" ]]; then uploads+=("$asset"); fi
        done
        if [[ "$migration_bundle" == "1" ]]; then
            # 3. Plain-ustar migration bundle (plan §3.2).
            python3 scripts/package-linux-release.py --target "$triple" \
                --migration-bundle-from "$output_dir/isolated-linux-$arch-bootstrap-inbox" --output-dir "$output_dir"
            bundle_name="duduclaw-isolated-linux-$arch-$CURRENT_VERSION.tar"
            # 4. Transition dual signature: root threshold + legacy release key.
            echo "Root threshold ($arch bundle): R1 touch + PIN, then the R2 passphrase."
            python3 scripts/sign-isolation-manifest.py --kind bundle --bundle "$output_dir/$bundle_name" \
                --expires-at-unix "$expires" --output "$output_dir/$bundle_name.authorization.json" \
                --key "${root_keys[0]}" --key "${root_keys[1]}" --ssh-keygen "$ssh_keygen"
            echo "Legacy release key ($arch bundle): minisign asks for its password."
            minisign -S -s "$legacy_minisign_key" -m "$output_dir/$bundle_name" \
                -t "duduclaw v$CURRENT_VERSION $bundle_name" -x "$output_dir/$bundle_name.minisig"
            minisign -V -q -P "$legacy_public" -m "$output_dir/$bundle_name" -x "$output_dir/$bundle_name.minisig"
            if ! isolation_check_minisig "$output_dir/$bundle_name.minisig" "duduclaw v$CURRENT_VERSION $bundle_name"; then
                isolation_fail "$bundle_name.minisig is not a prehashed signature with the verifier's trusted comment"
                return 1
            fi
            cp -- "$verifier" "$output_dir/duduclaw-bootstrap-linux-$arch"
            chmod 0755 "$output_dir/duduclaw-bootstrap-linux-$arch"
            uploads+=("$output_dir/duduclaw-bootstrap-linux-$arch" "$output_dir/$bundle_name"
                "$output_dir/$bundle_name.minisig" "$output_dir/$bundle_name.authorization.json"
                "$output_dir/$bundle_name.authorization.json.signatures.json")
        fi
    done

    # 5. Publication list from the exact bytes to upload (+ root fingerprint).
    local sums_file="$output_dir/SHA256SUMS-isolation.txt" index name
    local -a hashes=()
    for index in "${!uploads[@]}"; do
        hashes+=("$(isolation_sha256 "${uploads[$index]}")")
    done
    {
        for index in "${!uploads[@]}"; do
            printf '%s  %s\n' "${hashes[$index]}" "$(basename -- "${uploads[$index]}")"
        done
        printf '%s  %s\n' "$pin" "root.json"
    } > "$sums_file"
    echo ""
    echo "Assets for GitHub release $tag:"
    for index in "${!uploads[@]}"; do
        printf '  %s\n' "$(basename -- "${uploads[$index]}")"
    done
    if [[ "$upload" == "0" ]]; then
        echo "--no-upload: nothing uploaded; every output stays in $output_dir."
    else
        # 6. Upload exactly the recorded bytes, never replacing an asset, then read back.
        local answer=""
        read -r -p "Type UPLOAD to publish these ${#uploads[@]} assets to $tag: " answer
        if [[ "$answer" != "UPLOAD" ]]; then
            echo "Not uploaded; every output stays in $output_dir."
            return 1
        fi
        for index in "${!uploads[@]}"; do
            if [[ "$(isolation_sha256 "${uploads[$index]}")" != "${hashes[$index]}" ]]; then
                isolation_fail "$(basename -- "${uploads[$index]}") changed after it was recorded"; return 1
            fi
        done
        gh release upload "$tag" --repo "$ISOLATION_REPO" "${uploads[@]}"
        mkdir -m 0700 -- "$ISOLATION_WORK/readback"
        for index in "${!uploads[@]}"; do
            name="$(basename -- "${uploads[$index]}")"
            gh release download "$tag" --repo "$ISOLATION_REPO" --dir "$ISOLATION_WORK/readback" --pattern "$name"
            if [[ "$(isolation_sha256 "$ISOLATION_WORK/readback/$name")" != "${hashes[$index]}" ]]; then
                isolation_fail "uploaded $name differs from the signed bytes; investigate before publishing"; return 1
            fi
        done
        echo "Uploaded and read back ${#uploads[@]} assets."
    fi
    echo ""
    echo "SHA256SUMS-isolation.txt (sha256 $(isolation_sha256 "$sums_file")):"
    cat -- "$sums_file"
    echo "[external gate] Publish this list on the website AND the licensed console only after"
    echo "owner confirmation; both must show identical values (design §8 step 1)."
}

# --- Arg parsing / sub-commands ---
if [[ $# -lt 1 ]]; then
    echo "Usage:"
    echo "  $0 <hotfix|minor|major> [--title \"<theme>\"] [--dry-run]"
    echo "                                       bump + sync every platform manifest"
    echo "  $0 audit                             show each platform's version + drift"
    echo "  $0 verify [version]                  confirm PyPI/npm published <version>"
    echo "  $0 linux-assets --target <triple>     build/package all native Linux binaries"
    echo "  $0 isolation-sign --help              OFFLINE: sign/upload the Linux isolation release"
    exit 1
fi

# Read current version up-front (needed by all sub-commands).
CURRENT_VERSION="$(extract_version "$WORKSPACE_TOML" cargo)"
if [[ -z "$CURRENT_VERSION" ]]; then
    echo "Error: could not read version from $WORKSPACE_TOML"
    exit 1
fi

case "$1" in
    linux-assets)
        shift
        exec python3 "$(dirname "$0")/package-linux-release.py" "$@"
        ;;
    isolation-sign)
        shift
        run_isolation_sign "$@"
        exit $?
        ;;
    audit)
        run_audit "$CURRENT_VERSION" || {
            echo ""
            echo "DRIFT DETECTED: a manifest is behind the Cargo version. The next"
            echo "'$0 <hotfix|minor|major>' run re-syncs every manifest to the new version."
        }
        exit 0
        ;;
    verify)
        run_verify "${2:-$CURRENT_VERSION}"
        exit $?
        ;;
esac

BUMP_TYPE="$1"
if [[ "$BUMP_TYPE" == "patch" ]]; then
    echo "NOTICE: patch is deprecated; use hotfix (same patch bump and all-channel delivery)."
    BUMP_TYPE=hotfix
fi
shift
# Optional flags in any order: --dry-run, --title "<tagline>" (or --title=...).
# TITLE is the one-line release theme that goes after the date in the CHANGELOG
# version header (Keep-a-Changelog + this repo's convention:
#   "## [1.35.0] - 2026-07-07 — True auto-update, Ed25519-signed releases").
TITLE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            DRY_RUN=true
            echo "[DRY RUN] No files will be modified"
            ;;
        --title=*)
            TITLE="${1#--title=}"
            ;;
        --title)
            shift
            TITLE="${1:-}"
            ;;
        *)
            echo "Error: unknown option '$1'"
            exit 1
            ;;
    esac
    shift
done

if [[ "$BUMP_TYPE" != "hotfix" && "$BUMP_TYPE" != "minor" && "$BUMP_TYPE" != "major" ]]; then
    echo "Error: bump type must be 'hotfix', 'minor', 'major', or sub-command 'audit'/'verify'"
    exit 1
fi

# Check working tree is clean
if ! $DRY_RUN && ! git diff --quiet HEAD 2>/dev/null; then
    echo "Error: working tree has uncommitted changes"
    echo "Please commit or stash your changes first."
    exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"
case "$BUMP_TYPE" in
    hotfix) NEW_VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    minor) NEW_VERSION="$MAJOR.$((MINOR + 1)).0" ;;
    major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
esac

echo "Version: $CURRENT_VERSION -> $NEW_VERSION"
echo ""

# --- Pre-flight platform audit (informational; hotfix skips it) ---
if [[ "$BUMP_TYPE" != "hotfix" ]]; then
    run_audit "$CURRENT_VERSION" || echo "(drift above will be re-synced to $NEW_VERSION below)"
    echo ""
fi

if $DRY_RUN; then
    echo "[DRY RUN] Lane: $BUMP_TYPE; would bump all manifests, preserve CHANGELOG notes,"
    echo "          cargo check workspace + pro-gateway, re-resolve src-tauri lock, commit + tag."
    if [[ "$BUMP_TYPE" == "hotfix" ]]; then
        echo "[DRY RUN] Auto-sign five unchanged items from v$CURRENT_VERSION; doc drift requires a reason."
        echo "[DRY RUN] Skip informational audit and distributor technical brief; no manuals/sites rebuilt."
    else
        echo "[DRY RUN] Requires full signed two-manual / three-website delivery review."
        echo "[DRY RUN] Distributor output: commercial/marketing/distributor/releases/duduclaw-technology-brief-v$NEW_VERSION.html"
    fi
    echo "[DRY RUN] ALL channels: 5 GitHub targets + minisign/cosign, npm, PyPI, KB, GHCR,"
    echo "          computer-use image, desktop, Pro image/OEM tar/pro-bin/control-plane allowlist,"
    echo "          distributor console manuals (publish-manuals.py, dry run first)."
    echo "[DRY RUN] Linux archives include native isolation services; duduclaw-bootstrap only with"
    echo "          release/bootstrap-root.sha256. Isolation metadata is signed offline afterwards:"
    echo "          $0 isolation-sign --help (owner present; migration bundle only for: $ISOLATION_MIGRATION_BUNDLE_VERSIONS)."
    echo "[DRY RUN] Pro image CLOUD_BUILD=$CLOUD_BUILD; no signing/build/install/commit/tag/publish performed."
    echo "[DRY RUN] Confirm after publishing: $0 verify $NEW_VERSION"
fi
python3 "$(dirname "$0")/release_platform_ci.py" --lane "$BUMP_TYPE"
if $DRY_RUN; then
    exit 0
fi

# --- Enterprise release preflight (before any source mutation) ---
# The enterprise channel (duduclaw-pro image, offline tar, bare-metal binaries,
# control-plane allowlist) rides the same release train. Every one of those
# steps used to be best-effort and the whole section was skipped silently when
# no commercial checkout was present: v1.76.1 was cut from such a worktree, so
# enterprise installs stayed on v1.76.0 while the desktop app offered v1.76.1
# (2026-10-27). Missing prerequisites now stop the release before anything is
# changed. A CE-only release must say so with DUDUCLAW_RELEASE_WITHOUT_PRO=1.
PRO_RELEASE=1
PRO_GAPS=()
if [[ "${DUDUCLAW_RELEASE_WITHOUT_PRO:-0}" == "1" ]]; then
    PRO_RELEASE=0
    echo "WARNING: DUDUCLAW_RELEASE_WITHOUT_PRO=1 — this release ships NO enterprise assets."
    echo "         Enterprise installs stay on their current version until they are backfilled."
else
    PRO_MISSING=()
    [[ -x "commercial/duduclaw-pro-gateway/build-image.sh" ]] \
        || PRO_MISSING+=("commercial/duduclaw-pro-gateway checkout (private repository)")
    [[ -f "$HOME/.minisign/duduclaw-pro-release.key" ]] \
        || PRO_MISSING+=("Pro signing key ~/.minisign/duduclaw-pro-release.key")
    # Linux Pro is extracted from an image with its own Cargo profile. Resolve
    # the cross-format stripper before creating any release or publishing assets.
    PRO_STRIP_TOOL="${DUDUCLAW_LLVM_STRIP:-llvm-strip}"
    command -v "$PRO_STRIP_TOOL" >/dev/null 2>&1 \
        || PRO_MISSING+=("llvm-strip (install LLVM or set DUDUCLAW_LLVM_STRIP)")
    for PRO_TOOL in minisign gcloud docker; do
        command -v "$PRO_TOOL" >/dev/null 2>&1 || PRO_MISSING+=("$PRO_TOOL on PATH")
    done
    if command -v docker >/dev/null 2>&1 && ! docker info >/dev/null 2>&1; then
        PRO_MISSING+=("a running Docker daemon (linux-x64 is extracted from the pro image)")
    fi
    if command -v gcloud >/dev/null 2>&1 && [[ -z "$(gcloud auth list --filter=status:ACTIVE \
        --format='value(account)' 2>/dev/null)" ]]; then
        PRO_MISSING+=("an active gcloud account (gcloud auth activate-service-account ...)")
    fi
    # The image and the darwin binary embed the distributor notice fetched
    # from the control plane with the admin token. v1.76.2's first run had no
    # token in the shell, so the local image build stopped and the darwin
    # binary shipped without the notice. Read it from Secret Manager (the
    # deploy service account has access) unless the caller provides one or
    # opts out with DUDUCLAW_DISTRIBUTOR_NOTICE=skip. Kept in a variable and
    # handed only to the notice fetch and build-image.sh, never exported.
    PRO_NOTICE_TOKEN="${DUDUCLAW_CONTROL_ADMIN_TOKEN:-}"
    if [[ "${DUDUCLAW_DISTRIBUTOR_NOTICE:-fetch}" == "fetch" && -z "$PRO_NOTICE_TOKEN" ]] && \
        command -v gcloud >/dev/null 2>&1; then
        PRO_NOTICE_TOKEN="$(gcloud secrets versions access latest --secret=duduclaw-admin-token \
            --project "${DUDUCLAW_GCP_PROJECT:-louis-460302}" 2>/dev/null || true)"
    fi
    if [[ "${DUDUCLAW_DISTRIBUTOR_NOTICE:-fetch}" == "fetch" && -z "$PRO_NOTICE_TOKEN" ]]; then
        PRO_MISSING+=("the distributor-notice token (DUDUCLAW_CONTROL_ADMIN_TOKEN, read access to Secret Manager duduclaw-admin-token, or DUDUCLAW_DISTRIBUTOR_NOTICE=skip)")
    fi
    # The release commits the re-resolved enterprise Cargo.lock in the private
    # checkout (see the gate at the end), so that checkout must sit on a
    # branch that can go through a PR.
    if git -C commercial rev-parse --git-dir >/dev/null 2>&1; then
        PRO_COMMERCIAL_BRANCH="$(git -C commercial symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
        if [[ -z "$PRO_COMMERCIAL_BRANCH" || "$PRO_COMMERCIAL_BRANCH" == "main" ]]; then
            PRO_MISSING+=("the commercial checkout on its own branch, not main or a detached HEAD (the release commits duduclaw-pro-gateway/Cargo.lock there)")
        fi
    fi
    # Console manuals are published by this script (v1.80.0 shipped without
    # them because publishing was a separate manual step nobody ran). The
    # private summary and handbooks must already be merged in commercial/.
    if [[ "${DUDUCLAW_SKIP_MANUALS:-0}" != "1" ]]; then
        MANUALS_SUMMARY="commercial/docs/releases/manual-summaries/v$NEW_VERSION.json"
        MANUALS_HINT="write and merge it in the private repository, or set DUDUCLAW_SKIP_MANUALS=1"
        if [[ "$BUMP_TYPE" == "hotfix" ]]; then
            MANUALS_KIND="hotfix"
        else
            MANUALS_KIND="minor"
        fi
        [[ -f "commercial/scripts/publish-manuals.py" ]] \
            || PRO_MISSING+=("commercial/scripts/publish-manuals.py (private repository checkout)")
        if [[ ! -f "$MANUALS_SUMMARY" ]]; then
            PRO_MISSING+=("console manual summary $MANUALS_SUMMARY — $MANUALS_HINT")
        elif ! python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if d.get("version") == sys.argv[2] and d.get("kind") == sys.argv[3] else 1)
' "$MANUALS_SUMMARY" "$NEW_VERSION" "$MANUALS_KIND" >/dev/null 2>&1; then
            PRO_MISSING+=("valid console manual summary $MANUALS_SUMMARY (JSON with version=$NEW_VERSION, kind=$MANUALS_KIND) — $MANUALS_HINT")
        fi
        if [[ "$BUMP_TYPE" != "hotfix" && ! -d "commercial/docs/releases/v$NEW_VERSION/manuals" ]]; then
            PRO_MISSING+=("commercial/docs/releases/v$NEW_VERSION/manuals/ (handbooks + PDFs) — $MANUALS_HINT")
        fi
    fi
    if [[ ${#PRO_MISSING[@]} -gt 0 ]]; then
        echo "Error: enterprise release prerequisites are missing:"
        printf '  - %s\n' "${PRO_MISSING[@]}"
        echo "Fix them, or set DUDUCLAW_RELEASE_WITHOUT_PRO=1 for a deliberate CE-only release."
        exit 1
    fi
fi
# --- End enterprise release preflight ---

# --- Mandatory delivery review (before any source mutation) ---
# Only the neutral, signed assertion enters this public repository. The actual
# manuals, feature impact decisions and acceptance evidence stay private.
DELIVERY_ASSERTION="${RELEASE_DELIVERY_ASSERTION:-}"
DELIVERY_SIGNATURE="${RELEASE_DELIVERY_SIGNATURE:-}"
RELEASE_BRANCH="$(git branch --show-current)"
if [[ -z "$RELEASE_BRANCH" || "$RELEASE_BRANCH" == "main" || "$RELEASE_BRANCH" == "master" ]]; then
    echo "Error: create a release branch from latest origin/main; release commits must go through a pull request."
    exit 1
fi
if [[ -z "${RELEASE_DELIVERY_PUBLIC_KEY_PEM:-}" ]]; then
    RELEASE_DELIVERY_PUBLIC_KEY_PEM="$(gh variable get RELEASE_DELIVERY_PUBLIC_KEY_PEM --repo zhixuli0406/DuDuClaw)"
    export RELEASE_DELIVERY_PUBLIC_KEY_PEM
fi
if [[ "$BUMP_TYPE" == "hotfix" ]]; then
    commercial_git() (
        # Neither inherited Git overrides nor local replace objects may change
        # the signer checkout/history whose merged state we are certifying.
        for variable in ${!GIT_@}; do unset "$variable"; done
        git --no-replace-objects -C commercial "$@"
    )
    if [[ "$(commercial_git rev-parse --show-toplevel 2>/dev/null)" != "$(cd commercial 2>/dev/null && pwd -P)" ]]; then
        echo 'Error: commercial must be a separate private Git checkout; hotfix signer refused.'
        exit 1
    fi
    if ! COMMERCIAL_STATUS="$(commercial_git status --porcelain --untracked-files=all)"; then
        echo 'Error: cannot inspect commercial signer checkout; hotfix refused.'
        exit 1
    fi
    if [[ -n "$COMMERCIAL_STATUS" ]]; then
        echo 'Error: commercial signer checkout must be clean (including untracked files); merge signer changes in the private repo before hotfix signing.'
        exit 1
    fi
    if ! commercial_git fetch origin +refs/heads/main:refs/remotes/origin/main; then
        echo 'Error: cannot fetch commercial origin/main; hotfix signer integrity cannot be verified.'
        exit 1
    fi
    if ! commercial_git merge-base --is-ancestor HEAD origin/main; then
        echo 'Error: commercial signer HEAD has not been merged into origin/main; hotfix refused.'
        exit 1
    fi
    HOTFIX_OUTPUT="$PWD/commercial/artifacts/release-delivery/v$NEW_VERSION/assertion"
    HOTFIX_ARGS=(--hotfix --version "$NEW_VERSION" --previous-tag "v$CURRENT_VERSION"
        --platform-root "$PWD" --private-root "$PWD/commercial"
        --signing-key "$HOME/.duduclaw-keys/release-delivery/signing-20261011.key"
        --output "$HOTFIX_OUTPUT")
    if [[ -n "${DUDUCLAW_HOTFIX_ALLOW_DOC_DRIFT:-}" ]]; then
        HOTFIX_ARGS+=(--allow-doc-drift "$DUDUCLAW_HOTFIX_ALLOW_DOC_DRIFT")
    fi
    if ! python3 commercial/scripts/prepare-release-delivery.py "${HOTFIX_ARGS[@]}"; then
        echo 'Error: hotfix delivery refused. Use minor for delivery updates, or for reviewed doc drift rerun with DUDUCLAW_HOTFIX_ALLOW_DOC_DRIFT="<reason>".'
        exit 1
    fi
    DELIVERY_ASSERTION="$HOTFIX_OUTPUT/v$NEW_VERSION.json"
    DELIVERY_SIGNATURE="$HOTFIX_OUTPUT/v$NEW_VERSION.sig"
fi
python3 "$(dirname "$0")/release_delivery.py" --version "$NEW_VERSION" \
    --assertion "$DELIVERY_ASSERTION" --signature "$DELIVERY_SIGNATURE" --install
# Snapshot exact bytes that were verified; the pre-tag validator verifies them
# again after commit, so changes to external inputs cannot reach a tag.
DELIVERY_DIR="distribution/release-delivery"

# --- Bump every platform manifest (rewrites ANY semver, so drift is corrected) ---
echo "Bumping all platform manifests to $NEW_VERSION..."
while IFS='|' read -r kind file; do
    case "$kind" in
        cargo|pyproject)
            # Only the manifest's OWN version line: the first `version = "x.y.z"`
            # that follows `[package]` / `[workspace.package]` (Cargo) or `[project]`
            # (pyproject) and
            # precedes the next table header. The old whole-file sed
            # ("rewrites ANY semver") hit table-form dependency entries too:
            # v1.63.0 turned duduclaw-comp's `[dependencies.smithay]
            # version = "0.7.0"` into `"1.63.0"`, and the OS bake died with
            # `failed to select a version for the requirement smithay = "^1.63.0"`
            # (2026-09-08). Drift correction still happens — any semver in the
            # package's own line is rewritten — just never outside it.
            awk -v semver_re="^version = \"$SEMVER\"" -v newv="$NEW_VERSION" '
                /^\[/ { in_pkg = ($0 == "[package]" || $0 == "[workspace.package]" || $0 == "[project]"); done_pkg = 0 }
                in_pkg && !done_pkg && $0 ~ semver_re {
                    print "version = \"" newv "\""
                    done_pkg = 1
                    next
                }
                { print }
            ' "$file" > "${file}.tmp" && mv "${file}.tmp" "$file"
            ;;
        pyinit)
            sed -i '' -E "s/^([[:space:]]*)__version__ = \"$SEMVER\"/\1__version__ = \"$NEW_VERSION\"/" "$file"
            ;;
        npm)
            # "version": "x.y.z" plus any "@duduclaw/<plat>": "x.y.z" dep refs
            sed -i '' -E "s/(\"version\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            sed -i '' -E "s/(\"@duduclaw\/[a-z0-9-]+\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            ;;
        tauri|mcp_registry)
            # mcp_registry: rewrites both `version` fields (top level and the
            # npm package entry); extract_version reads both and reports a
            # mismatch, so the audit and the post-bump assert check both.
            sed -i '' -E "s/(\"version\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            ;;
        badge)
            sed -i '' -E "s|(badge/version-)$SEMVER(-blue)|\1$NEW_VERSION\2|" "$file"
            ;;
        installer_sh)
            sed -i '' -E "s/^(FALLBACK_VERSION=\")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            ;;
        installer_ps1)
            sed -i '' -E "s/^(\\\$FallbackVersion = \")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            ;;
    esac
    echo "  Updated: $file"
done < <(platform_manifests)

# --- POST-BUMP ASSERT: every manifest must now read NEW_VERSION (the real fix) ---
echo ""
echo "Asserting all platform manifests reached $NEW_VERSION..."
ASSERT_FAIL=0
while IFS='|' read -r kind file; do
    v="$(extract_version "$file" "$kind")"
    if [[ "$v" != "$NEW_VERSION" ]]; then
        echo "  ERROR: $file is '$v', expected '$NEW_VERSION'"
        ASSERT_FAIL=1
    fi
done < <(platform_manifests)
if [[ $ASSERT_FAIL -eq 1 ]]; then
    echo ""
    echo "Aborting: not every platform reached $NEW_VERSION (PyPI/npm would silently"
    echo "freeze). Reverting all changes."
    # `git reset --hard HEAD`, not `git checkout -- .`: HEAD is exactly where
    # the pre-flight check at the top of this script verified the working tree
    # was clean, so resetting to it is a full, correct undo of everything this
    # run has done so far (all sed edits are unstaged, no commit has happened
    # yet). reset --hard clears both the working tree and any staged changes in
    # one step; checkout -- . would only re-sync the tree to the index.
    git reset --hard HEAD
    exit 1
fi
echo "  All platforms synchronized at $NEW_VERSION."

# --- Update CHANGELOG.md ---
echo ""
echo "Updating CHANGELOG.md..."
DATE=$(date +%Y-%m-%d)

# The version header follows this repo's convention (Keep a Changelog + a
# one-line theme after the date):
#   "## [1.36.0] - 2026-07-15 — <title>"
# The title is passed via --title; without it we emit the bare date header and
# nag, because a themed header is the house style.
if [[ -n "$TITLE" ]]; then
    VERSION_HEADER="## [$NEW_VERSION] - $DATE — $TITLE"
else
    VERSION_HEADER="## [$NEW_VERSION] - $DATE"
    echo "  NOTE: no --title given — header has no theme line. House style is"
    echo "        '## [$NEW_VERSION] - $DATE — <one-line theme>'. Re-run with"
    echo "        --title \"<theme>\" or edit the header before pushing."
fi

if [[ -f "CHANGELOG.md" ]]; then
    if grep -qE '^## \[Unreleased\]' CHANGELOG.md; then
        # Release move (Keep a Changelog): the curated [Unreleased] section
        # BECOMES this version. We insert the version header right after the
        # [Unreleased] heading, so everything accumulated during the cycle now
        # sits under [X.Y.Z] and a fresh, empty [Unreleased] stays on top.
        # This is why a bump no longer strands hand-written notes under
        # [Unreleased] (the historical bug: a placeholder block was prepended
        # and the real notes were left behind).
        TEMP=$(mktemp)
        awk -v hdr="$VERSION_HEADER" '
            !done && /^## \[Unreleased\]/ {
                print                # keep the (now-empty) [Unreleased] heading
                print ""
                print hdr            # curated content below falls under this version
                done = 1
                next
            }
            { print }
        ' CHANGELOG.md > "$TEMP"
        mv "$TEMP" CHANGELOG.md
        echo "  Renamed [Unreleased] -> $NEW_VERSION (curated notes preserved)"
    else
        # No [Unreleased] section: prepend a fresh version block with placeholder
        # buckets for the author to fill in.
        TEMP=$(mktemp)
        head -2 CHANGELOG.md > "$TEMP"
        {
            echo ""
            echo "$VERSION_HEADER"
            echo ""
            echo "### Added"
            echo "- (describe new features here)"
            echo ""
            echo "### Changed"
            echo "- (describe changes here)"
            echo ""
            echo "### Fixed"
            echo "- (describe bug fixes here)"
            echo ""
        } >> "$TEMP"
        tail -n +3 CHANGELOG.md >> "$TEMP"
        mv "$TEMP" CHANGELOG.md
    fi
else
    cat > CHANGELOG.md << HEREDOC
# Changelog

## [Unreleased]

$VERSION_HEADER

- Initial tracked release

HEREDOC
    echo "  Created CHANGELOG.md"
fi

# --- Verify build ---
echo ""
echo "Running cargo check..."
if ! cargo check --workspace 2>/dev/null; then
    echo "Error: cargo check failed. Reverting version bump."
    # See the ASSERT_FAIL revert above for why this is `reset --hard HEAD`
    # and not `checkout -- .` (clears staged + unstaged in one step; tree was
    # verified clean at HEAD before this run).
    git reset --hard HEAD
    exit 1
fi

# The commercial tree (gitignored, own git repo) builds against this
# workspace via path deps but is NOT covered by `cargo check --workspace`.
# A public-struct change that misses it only surfaces inside the pro-image
# Docker build, long after the tag exists (v1.40.0 lesson: new
# GatewayConfig field broke duduclaw-pro at image time). Check it here so
# the release aborts before the bump commit instead.
if [ -d "commercial/duduclaw-pro-gateway" ]; then
    echo "Running cargo check (commercial/duduclaw-pro-gateway)..."
    if ! cargo check --manifest-path commercial/duduclaw-pro-gateway/Cargo.toml 2>/dev/null; then
        echo "Error: duduclaw-pro-gateway no longer compiles against this workspace."
        echo "       Fix the commercial tree first (it is not covered by --workspace)."
        # See the ASSERT_FAIL revert above for why this is `reset --hard
        # HEAD` and not `checkout -- .` (clears staged + unstaged in one step).
        git reset --hard HEAD
        exit 1
    fi
fi

# The desktop app (src-tauri) is its own Cargo workspace with its own
# Cargo.lock, and that lock pins the versions of the workspace crates it
# reaches by path (duduclaw-core, duduclaw-pets). `cargo check --workspace`
# never touches it, so after a bump those entries still carry the old version
# and every `--locked` build of the desktop refuses the lock: the CI Security
# Audit, the desktop release workflow. v1.72.3's bump PR failed exactly that
# way. `cargo metadata` re-resolves the lock and rewrites only those path
# entries (same transformation release_delivery.py accepts for a Cargo.lock).
if [[ -f src-tauri/Cargo.toml && -f src-tauri/Cargo.lock ]]; then
    echo "Re-resolving src-tauri/Cargo.lock for the new workspace crate versions..."
    if ! cargo metadata --format-version 1 --manifest-path src-tauri/Cargo.toml >/dev/null; then
        echo "Error: could not re-resolve src-tauri/Cargo.lock. Reverting version bump."
        git reset --hard HEAD
        exit 1
    fi
fi

# --- Distributor technical brief (private source, separate repository) ---
# Keep content review dates and historical feature versions intact. The renderer
# labels a newer release edition separately from the source's reviewed version.
# A missing private checkout is normal; present but broken sources are fatal.
# Never stage commercial artifacts in the public release commit.
echo ""
echo "Generating distributor technical brief..."
if [[ "${BUMP_TYPE:-minor}" == "hotfix" ]]; then
    echo "Hotfix: distributor technical brief unchanged."
elif ! python3 "$(dirname "$0")/build-distributor-brief.py" --version "$NEW_VERSION" --output-dir "commercial/marketing/distributor/releases" --skip-missing; then
    echo "Error: distributor HTML generation failed; no release commit or tag was created."
    echo "       Fix the document source and regenerate before retrying the release."
    echo "       The version bump remains uncommitted for inspection."
    exit 1
fi

# --- Git commit + tag ---
echo ""
echo "Creating git commit and tag..."
# Stage exactly what the bump touched. Derive the manifest list from the SAME
# source of truth used by bump/audit/assert (platform_manifests) so a new
# publish target can never be bumped-but-not-committed — the historical bug
# that left python/duduclaw/__init__.py + scripts/install.{sh,ps1} on disk at
# the new version yet absent from the bump commit. Plus Cargo.lock (rewritten
# by `cargo check` when workspace crate versions change) and CHANGELOG.md.
STAGE_FILES=()
while IFS='|' read -r _kind file; do
    STAGE_FILES+=("$file")
done < <(platform_manifests)
[[ -f Cargo.lock ]] && STAGE_FILES+=("Cargo.lock")
[[ -f src-tauri/Cargo.lock ]] && STAGE_FILES+=("src-tauri/Cargo.lock")
STAGE_FILES+=("CHANGELOG.md")
STAGE_FILES+=("$DELIVERY_DIR/v$NEW_VERSION.json" "$DELIVERY_DIR/v$NEW_VERSION.sig")
git add -- "${STAGE_FILES[@]}"
git commit -m "chore: bump v$NEW_VERSION"
# Validate the actual committed bytes and exact version-only parent before
# creating any tag. A temporary local ref is removed even on validation failure.
git tag "v$NEW_VERSION"
if ! python3 "$(dirname "$0")/release_delivery.py" --version "$NEW_VERSION" --release-ref "v$NEW_VERSION"; then
    git tag -d "v$NEW_VERSION" >/dev/null
    echo "Error: final delivery gate failed; release commit remains for inspection, no publishable tag remains."
    exit 1
fi
git tag -d "v$NEW_VERSION" >/dev/null
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"
# Desktop tag rides the same bump commit: pushing it triggers
# .github/workflows/desktop-release.yml (Tauri build, sign/notarize, publish —
# no draft). Kept as a separate tag so a desktop-only rebuild stays possible.
if [[ -f src-tauri/tauri.conf.json ]]; then
    git tag -a "desktop-v$NEW_VERSION" -m "Desktop release v$NEW_VERSION"
fi

# --- Enterprise pro-image (commercial checkout only) ---
# The duduclaw-pro image is versioned by THIS release train (same gateway,
# license-gated modules), so building it belongs to the normal release flow:
# the cloud console's version dropdown lists GitHub releases filtered by
# which duduclaw-pro:<tag> images actually exist in the private registry —
# skipping this step means the new version never appears in the dropdown.
# No-op on public checkouts (script absent). Opt out: DUDUCLAW_SKIP_PRO_IMAGE=1.
PRO_IMAGE_SCRIPT="commercial/duduclaw-pro-gateway/build-image.sh"
# Tracked so the pro-binary-assets step below knows whether it can extract
# linux-x64 from a freshly-built local image tag (docker build always tags
# locally even if the AR push itself fails, but a skipped/failed build leaves
# no local image to extract from).
PRO_IMAGE_PUSHED=0
# build-image.sh writes "<version> <tar sha256>" here after uploading the
# offline tar; the control-plane sync below turns it into the delivery
# manifest entry for this release.
PRO_TAR_SHA_FILE="$(mktemp)"
if [[ "$PRO_RELEASE" == "1" && "${DUDUCLAW_SKIP_PRO_IMAGE:-0}" == "1" ]]; then
    PRO_GAPS+=("pro-image + oem-tar skipped (DUDUCLAW_SKIP_PRO_IMAGE=1) — re-run: $PRO_IMAGE_SCRIPT v$NEW_VERSION")
fi
# Fetch the distributor notice once so the darwin binary below embeds the same
# notice as the image (build-image.sh fetches its own copy). The token goes in
# as a prefix assignment, never as an `env` argument, so it does not show up
# in a process listing. A previous release's file is removed first: a failed
# fetch must not leave an old notice for the darwin build to embed.
if [[ "$PRO_RELEASE" == "1" && "${DUDUCLAW_DISTRIBUTOR_NOTICE:-fetch}" == "fetch" ]]; then
    echo ""
    echo "Fetching the distributor notice..."
    rm -f commercial/duduclaw-pro-gateway/distributor-notice.html
    if ! ( cd commercial/duduclaw-pro-gateway &&
           DUDUCLAW_CONTROL_ADMIN_TOKEN="${PRO_NOTICE_TOKEN:-}" ./fetch-distributor-notice.sh distributor-notice.html &&
           python3 build-policy.py --check ); then
        rm -f commercial/duduclaw-pro-gateway/distributor-notice.html
        echo "  WARNING: distributor notice fetch failed — the darwin binary will ship without it."
        PRO_GAPS+=("distributor notice not fetched — re-run commercial/duduclaw-pro-gateway/fetch-distributor-notice.sh and rebuild the darwin binary")
    fi
fi
if [[ "$PRO_RELEASE" == "1" && -x "$PRO_IMAGE_SCRIPT" && "${DUDUCLAW_SKIP_PRO_IMAGE:-0}" != "1" ]]; then
    echo ""
    echo "Building + pushing enterprise duduclaw-pro:v$NEW_VERSION image..."
    if DUDUCLAW_CONTROL_ADMIN_TOKEN="${PRO_NOTICE_TOKEN:-}" DUDUCLAW_PRO_TAR_SHA_FILE="$PRO_TAR_SHA_FILE" \
        "$PRO_IMAGE_SCRIPT" "v$NEW_VERSION"; then
        echo "  Enterprise image v$NEW_VERSION pushed."
        PRO_IMAGE_PUSHED=1
    else
        echo ""
        echo "  WARNING: duduclaw-pro image build/push FAILED. The release commit +"
        echo "  tag stand, but the cloud console will not offer v$NEW_VERSION until"
        echo "  you re-run:  $PRO_IMAGE_SCRIPT v$NEW_VERSION"
        PRO_GAPS+=("pro-image build/push failed — re-run: $PRO_IMAGE_SCRIPT v$NEW_VERSION")
    fi
fi

# --- Enterprise pro binary assets (darwin + linux-x64, commercial checkout only) ---
# Bare-metal duduclaw-pro binaries for the P0 control-plane auto-update
# channel (commercial/docs/specs/DESIGN-pro-auto-update-2026-08.md §3 P0, §4 item
# 3). Signed with an INDEPENDENT minisign keypair
# (~/.minisign/duduclaw-pro-release.key) so a control-plane compromise can
# never poison CE users via the CE update channel, or vice versa (design
# principle 2 — key isolation from UPDATE_PUBKEY / duduclaw-release.key). The
# darwin binary builds natively on this machine, LABELED BY ITS ACTUAL BUILD
# HOST TRIPLE (darwin_platform_label — darwin-arm64 or darwin-x64, never
# assumed; see that function's doc comment for the 2026-08-17 mislabel
# finding); linux-x64 is extracted from the duduclaw-pro image built above (no
# cross-compile toolchain needed). Best-effort, same discipline as the
# pro-image step above: any missing prerequisite or failure WARNs and this
# step is skipped — it never fails the release. Opt out: DUDUCLAW_SKIP_PRO_BIN=1.
PRO_BIN_KEY="$HOME/.minisign/duduclaw-pro-release.key"
PRO_BIN_BUCKET="${DUDUCLAW_IMAGE_TAR_BUCKET:-duduclaw-oem-images}"
# Tracked so the control-plane allowlist sync below only fires when this
# release actually shipped at least one pro asset.
PRO_BIN_UPLOADED=0
PRO_BIN_DARWIN_OK=0
PRO_BIN_LINUX_OK=0

# Package <staged_dir>/<plat>/duduclaw-pro into a signed, checksummed tar.gz
# and upload the three-piece set (tar.gz + .sha256 + .minisig) to GCS.
# Isolated into a function so the tar-naming / GCS-path formula is
# independently callable (source this script and invoke directly to check
# the naming/path logic without building or uploading anything real).
package_and_upload_pro_bin() {
    local plat="$1" staged_dir="$2" version="$3"
    local tar_name="duduclaw-pro-${plat}.tar.gz"
    local tar_path="${staged_dir}/${tar_name}"
    local dest="gs://${PRO_BIN_BUCKET}/duduclaw-pro-bin/v${version}/"

    # Darwin was stripped by Cargo before its final code signature. Do not
    # mutate that Mach-O again here: stripping can invalidate the arm64 seal.
    # Linux comes from a private image with its own profile, so strip the staged
    # ELF explicitly before hashing/signing. The tool is checked at preflight.
    if [[ "$plat" == linux-* ]]; then
        local strip_tool="${DUDUCLAW_LLVM_STRIP:-llvm-strip}"
        if ! "$strip_tool" --strip-all "$staged_dir/$plat/duduclaw-pro"; then
            echo "  ERROR: stripping $plat failed."
            return 1
        fi
    fi
    local app_path="${staged_dir}/duduclaw-pro-${plat}.app.zip"
    if [[ "$plat" == darwin-* ]]; then
        # Sign before creating either asset. A signing failure skips this platform.
        if ! python3 scripts/macos/build-gateway-bundle.py \
                --exe "$staged_dir/$plat/duduclaw-pro" --name duduclaw-pro \
                --version "$version" --out "$staged_dir/$plat" ||
           ! bash scripts/macos/sign-gateway.sh \
                --app "$staged_dir/$plat/DuDuClaw Gateway.app" \
                --exe "$staged_dir/$plat/duduclaw-pro" --out "$app_path" \
                --keychain-profile "${DUDUCLAW_NOTARY_PROFILE:-duduclaw-notary}" ||
           ! bash scripts/macos/verify-gateway-signature.sh \
                --app "$staged_dir/$plat/DuDuClaw Gateway.app" \
                --exe "$staged_dir/$plat/duduclaw-pro" --tag "v$version"; then
            echo "  WARNING: gateway signing/notarization unavailable — skipping $plat."
            PRO_GAPS+=("pro-bin $plat v$version signing/notarization failed — no darwin assets uploaded")
            return 1
        fi
        if ! python3 scripts/check_update_archive_size.py "$app_path" ||
           ! python3 scripts/verify_release_marker.py --tag "v$version" --archive "$app_path" ||
           ! shasum -a 256 "$app_path" > "${app_path}.sha256" ||
           ! minisign -S -s "$PRO_BIN_KEY" -m "$app_path" \
                -t "duduclaw-pro v$version $(basename "$app_path")"; then
            echo "  WARNING: gateway bundle validation/signing failed — skipping $plat."
            return 1
        fi
    fi
    if ! ( cd "$staged_dir/$plat" && tar czf "$tar_path" duduclaw-pro ); then
        echo "  WARNING: failed to package $tar_name — skipping."
        return 1
    fi
    # Hard failure, not a best-effort Pro upload: never publish an update that
    # strands <=1.79.2 installations on their legacy 200 MiB extraction limit.
    python3 "$(dirname "$0")/check_update_archive_size.py" "$tar_path" || exit 1
    if ! shasum -a 256 "$tar_path" > "${tar_path}.sha256"; then
        echo "  WARNING: sha256 failed for $tar_name — skipping."
        return 1
    fi
    if ! python3 scripts/verify_release_marker.py --tag "v$version" --archive "$tar_path"; then
        echo "  WARNING: release identity check failed for $tar_name — refusing signing/upload."
        return 1
    fi
    if ! minisign -S -s "$PRO_BIN_KEY" -m "$tar_path" -t "duduclaw-pro v$version $tar_name"; then
        echo "  WARNING: minisign failed for $tar_name — skipping upload."
        return 1
    fi
    if ! command -v gcloud >/dev/null 2>&1; then
        echo "  WARNING: gcloud not found — $tar_name built + signed locally, not uploaded."
        return 1
    fi
    local extra_assets=()
    if [[ "$plat" == darwin-* ]]; then
        extra_assets=("$app_path" "${app_path}.sha256" "${app_path}.minisig")
    fi
    if gcloud storage cp "$tar_path" "${tar_path}.sha256" "${tar_path}.minisig" ${extra_assets[@]+"${extra_assets[@]}"} "$dest"; then
        echo "  Uploaded: ${dest}${tar_name} (+ .sha256 + .minisig)"
        return 0
    fi
    echo "  WARNING: GCS upload failed for $tar_name."
    return 1
}

if [[ "$PRO_RELEASE" == "1" && -d "commercial/duduclaw-pro-gateway" ]]; then
    if [[ "${DUDUCLAW_SKIP_PRO_BIN:-0}" == "1" ]]; then
        echo ""
        echo "Skipping duduclaw-pro binary assets (DUDUCLAW_SKIP_PRO_BIN=1)."
        PRO_GAPS+=("pro-bin darwin + linux-x64 skipped (DUDUCLAW_SKIP_PRO_BIN=1)")
    elif ! command -v minisign >/dev/null 2>&1; then
        echo ""
        echo "  WARNING: minisign not found in PATH — skipping duduclaw-pro binary assets."
    elif [[ ! -f "$PRO_BIN_KEY" ]]; then
        echo ""
        echo "  WARNING: $PRO_BIN_KEY not found — skipping duduclaw-pro binary assets."
    else
        echo ""
        echo "Building duduclaw-pro bare-metal binary assets (v$NEW_VERSION)..."
        PRO_BIN_TMP="$(mktemp -d)"

        # darwin: native build on this machine, LABELED BY THE ACTUAL BUILD
        # HOST TRIPLE (never assumed as darwin-arm64 — see darwin_platform_label
        # above). An unrecognized host triple skips the darwin asset entirely
        # rather than uploading it under a guessed label (fail-closed:
        # mislabeling a binary is worse than not shipping one this round).
        # Built with CWD inside the crate dir (not --manifest-path from repo
        # root) so cargo's config discovery actually walks through
        # commercial/duduclaw-pro-gateway/.cargo/config.toml and honors its
        # target-dir="../../target" redirect — verified via `cargo metadata`:
        # --manifest-path from the repo root resolves target_directory to
        # commercial/duduclaw-pro-gateway/target instead (config discovery
        # follows CWD, not the manifest path).
        DARWIN_PLATFORM_LABEL="$(darwin_platform_label)"
        if [[ -z "$DARWIN_PLATFORM_LABEL" ]]; then
            echo "  WARNING: could not determine a darwin-arm64/darwin-x64 host triple via"
            echo "           'rustc -vV' — skipping darwin binary asset (fail-closed)."
        else
            echo "  [$DARWIN_PLATFORM_LABEL] validating policy and building duduclaw-pro..."
            # Ask cargo where the binary lands: CARGO_TARGET_DIR (a shared
            # target across worktrees) overrides the crate's target-dir, and
            # v1.76.2's first run looked in ../../target after a good build.
            PRO_TARGET_DIR="$( (cd commercial/duduclaw-pro-gateway &&
                cargo metadata --no-deps --format-version 1 2>/dev/null) |
                python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null || true)"
            [[ -n "$PRO_TARGET_DIR" ]] || PRO_TARGET_DIR="$PWD/target"
            PRO_DARWIN_BIN="$PRO_TARGET_DIR/release/duduclaw-pro"
            # The distributor build also embeds the About-page distributor
            # notice when the private checkout has exported one
            # (distributor-notice.html, validated by build-policy.py --check).
            # vendored-openssl statically links OpenSSL: without it the binary
            # links Homebrew's openssl@N dylib, which customers do not have.
            # check_portable_binary.py then fails the build if any non-system
            # dylib still sneaks in.
            PORTABLE_CHECK="$PWD/scripts/check_portable_binary.py"
            if ( cd commercial/duduclaw-pro-gateway &&
                 python3 build-policy.py --check &&
                 notice_env=() &&
                 { [[ ! -f distributor-notice.html ]] ||
                   notice_env=("DUDUCLAW_DISTRIBUTOR_NOTICE_PATH=$PWD/distributor-notice.html"); } &&
                 env ${notice_env[@]+"${notice_env[@]}"} \
                     DUDUCLAW_INSTALLATION_POLICY_PATH="$PWD/installation-policy.json" \
                     CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked --features vendored-openssl --bin duduclaw-pro &&
                 python3 "$PORTABLE_CHECK" "$PRO_DARWIN_BIN" &&
                 "$PRO_DARWIN_BIN" --version >/dev/null ); then
                mkdir -p "$PRO_BIN_TMP/$DARWIN_PLATFORM_LABEL"
                if cp "$PRO_DARWIN_BIN" "$PRO_BIN_TMP/$DARWIN_PLATFORM_LABEL/duduclaw-pro"; then
                    if package_and_upload_pro_bin "$DARWIN_PLATFORM_LABEL" "$PRO_BIN_TMP" "$NEW_VERSION"; then
                        PRO_BIN_UPLOADED=1
                        PRO_BIN_DARWIN_OK=1
                    fi
                else
                    echo "  WARNING: built binary missing at $PRO_DARWIN_BIN — skipping $DARWIN_PLATFORM_LABEL."
                fi
            else
                echo "  WARNING: $DARWIN_PLATFORM_LABEL duduclaw-pro build failed — skipping this platform."
            fi
        fi

        # linux-x64: extract from the duduclaw-pro image built above (docker
        # build always tags the image locally even if the AR push failed, but
        # we only attempt this when the step above actually reported success).
        if [[ "$PRO_IMAGE_PUSHED" == "1" ]] && command -v docker >/dev/null 2>&1; then
            echo "  [linux-x64] extracting binary from duduclaw-pro:v$NEW_VERSION image..."
            PRO_BIN_REGION="${DUDUCLAW_GCP_REGION:-asia-east1}"
            PRO_BIN_PROJECT="${DUDUCLAW_GCP_PROJECT:-$(gcloud config get-value project 2>/dev/null)}"
            if [[ -n "$PRO_BIN_PROJECT" && "$PRO_BIN_PROJECT" != "(unset)" ]]; then
                PRO_BIN_IMAGE="${PRO_BIN_REGION}-docker.pkg.dev/${PRO_BIN_PROJECT}/duduclaw/duduclaw-pro:v${NEW_VERSION}"
                PRO_BIN_CID=""
                if docker pull --platform linux/amd64 "$PRO_BIN_IMAGE"; then
                    PRO_BIN_CID="$(docker create --platform linux/amd64 "$PRO_BIN_IMAGE" 2>/dev/null || true)"
                fi
                if [[ -n "$PRO_BIN_CID" ]]; then
                    mkdir -p "$PRO_BIN_TMP/linux-x64"
                    if docker cp "$PRO_BIN_CID:/usr/local/bin/duduclaw-pro" "$PRO_BIN_TMP/linux-x64/duduclaw-pro" 2>/dev/null; then
                        if python3 "$(dirname "$0")/check_glibc.py" "$PRO_BIN_TMP/linux-x64/duduclaw-pro"; then
                            if package_and_upload_pro_bin linux-x64 "$PRO_BIN_TMP" "$NEW_VERSION"; then
                                PRO_BIN_UPLOADED=1
                                PRO_BIN_LINUX_OK=1
                            fi
                        else
                            echo "  ERROR: linux-x64 duduclaw-pro failed the GLIBC_2.31 baseline check — skipping upload."
                            PRO_GAPS+=("pro-bin-linux v$NEW_VERSION failed the GLIBC_2.31 baseline check — not uploaded")
                        fi
                    else
                        echo "  WARNING: docker cp from duduclaw-pro image failed — skipping linux-x64."
                    fi
                    docker rm "$PRO_BIN_CID" >/dev/null 2>&1 || true
                else
                    echo "  WARNING: docker create duduclaw-pro:v$NEW_VERSION failed — skipping linux-x64."
                fi
            else
                echo "  WARNING: no GCP project configured — skipping linux-x64 (need image ref)."
            fi
        else
            echo "  NOTE: pro image was not built/pushed above (skipped or failed), or docker"
            echo "        is unavailable — linux-x64 binary asset skipped."
        fi

        rm -rf "$PRO_BIN_TMP"
    fi
    if [[ "${DUDUCLAW_SKIP_PRO_BIN:-0}" != "1" ]]; then
        [[ "$PRO_BIN_DARWIN_OK" == "1" ]] \
            || PRO_GAPS+=("pro-bin-mac v$NEW_VERSION not uploaded — see the WARNING above")
        [[ "$PRO_BIN_LINUX_OK" == "1" ]] \
            || PRO_GAPS+=("pro-bin-linux v$NEW_VERSION not uploaded (needs the pro image) — see the WARNING above")
    fi

    # --- Control-plane offered-version allowlist (DUDUCLAW_PRO_VERSIONS) ---
    # Auto-discovery (GitHub releases × AR manifest probe) proved unreliable
    # in production (2026-08-17: silently all-false manifest probes degraded
    # the offered list to ["latest"], blanking the Pro update channel and the
    # console version dropdown). The operator allowlist — path ① in
    # pro_versions::resolve, highest precedence — is therefore maintained
    # per-release HERE as the authoritative source; discovery + the registry
    # fallback stay behind it as the safety net. Newest-first, top 3 release
    # tags from git (desktop-v* excluded); first entry doubles as the console
    # packaging default pin. Any failed regional update/readback is recorded
    # in PRO_GAPS and fails the enterprise release gate below.
    # Opt out: DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1.
    # With neither the image nor a binary uploaded the sync below does not
    # run (it would offer a version with nothing behind it); v1.76.2's first
    # run hit this without the gap being listed.
    if [[ "${DUDUCLAW_SKIP_PRO_VERSIONS_ENV:-0}" != "1" && "$PRO_IMAGE_PUSHED" != "1" && \
        "$PRO_BIN_UPLOADED" != "1" ]]; then
        PRO_GAPS+=("control-plane allowlist and delivery manifest not synced (no enterprise asset was uploaded)")
    fi
    if [[ "${DUDUCLAW_SKIP_PRO_VERSIONS_ENV:-0}" == "1" ]]; then
        echo ""
        echo "Skipping control-plane DUDUCLAW_PRO_VERSIONS sync (DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1)."
        PRO_GAPS+=("control-plane allowlist not synced (DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1)")
    elif [[ "$PRO_IMAGE_PUSHED" == "1" || "$PRO_BIN_UPLOADED" == "1" ]]; then
        CP_SERVICE="${DUDUCLAW_CP_SERVICE:-duduclaw-control-plane}"
        CONTROL_PLANE_REGIONS=()
        resolve_cp_regions || exit 1
        CP_SOURCE_REGION="${CONTROL_PLANE_REGIONS[0]}"
        CP_PROJECT="${DUDUCLAW_GCP_PROJECT:-$(gcloud config get-value project 2>/dev/null || true)}"
        # Newest three vX.Y.Z tags — the tag for THIS release already exists
        # at this point in the flow, so it is the natural first entry.
        CP_VERSIONS="$(git tag --list 'v*' --sort=-v:refname \
            | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -3 | paste -sd, -)"
        if ! command -v gcloud >/dev/null 2>&1 || [[ -z "$CP_PROJECT" || "$CP_PROJECT" == "(unset)" ]]; then
            echo ""
            echo "  WARNING: gcloud/project unavailable — control-plane version allowlist NOT synced."
            PRO_GAPS+=("control-plane allowlist not synced (gcloud/project unavailable)")
            echo "           Run manually for each region (${CONTROL_PLANE_REGIONS[*]}): gcloud run services update $CP_SERVICE --region REGION \\"
            echo "                            --update-env-vars '^:^DUDUCLAW_PRO_VERSIONS=${CP_VERSIONS:-v$NEW_VERSION}'"
        elif [[ -z "$CP_VERSIONS" ]]; then
            echo ""
            echo "  WARNING: could not derive a version list from git tags — allowlist NOT synced."
            PRO_GAPS+=("control-plane allowlist not synced (no version list from git tags)")
        else
            # --- General-distributor delivery manifest (same update) ---
            # /v1/dist/versions offers only versions that are BOTH in the
            # allowlist and in DUDUCLAW_GENERAL_DELIVERY_MANIFEST with the
            # exact tar SHA. Until 2026-10-22 only the allowlist was synced
            # here, so every release after v1.72.2 offered no download. The
            # new entry is the tar build-image.sh just hashed; entries no
            # longer in the allowlist are dropped.
            CP_MANIFEST_SCRIPT="commercial/duduclaw-pro-gateway/delivery_manifest.py"
            CP_MANIFEST=""
            if [[ -f "$CP_MANIFEST_SCRIPT" ]]; then
                # A failed read must not look like "no manifest yet": that would
                # rewrite it with only the new release and drop every approved one.
                CP_MANIFEST_READ=1
                CP_CURRENT_MANIFEST="$(gcloud run services describe "$CP_SERVICE" --project "$CP_PROJECT" --region "$CP_SOURCE_REGION" \
                    --format=json 2>/dev/null | python3 -c 'import json, sys
env = json.load(sys.stdin)["spec"]["template"]["spec"]["containers"][0].get("env", [])
print(next((e.get("value", "") for e in env if e["name"] == "DUDUCLAW_GENERAL_DELIVERY_MANIFEST"), ""))')" || CP_MANIFEST_READ=0
                CP_ADD=""
                if [[ -s "$PRO_TAR_SHA_FILE" ]]; then
                    read -r CP_TAR_VERSION CP_TAR_SHA < "$PRO_TAR_SHA_FILE" || true
                    CP_ADD="${CP_TAR_VERSION}=${CP_TAR_SHA}"
                fi
                if [[ "$CP_MANIFEST_READ" != "1" ]]; then
                    CP_MANIFEST=""
                    echo "  WARNING: could not read the live delivery manifest — left unchanged."
                elif ! CP_MANIFEST="$(python3 -I "$CP_MANIFEST_SCRIPT" --offered "$CP_VERSIONS" \
                    --current "$CP_CURRENT_MANIFEST" ${CP_ADD:+--add "$CP_ADD"})"; then
                    CP_MANIFEST=""
                    echo "  WARNING: delivery manifest could not be computed — left unchanged."
                fi
                if [[ -z "$CP_ADD" ]]; then
                    PRO_GAPS+=("delivery manifest has no offline tar SHA for v$NEW_VERSION")
                    echo "  WARNING: no offline tar was uploaded for v$NEW_VERSION, so distributors"
                    echo "           cannot download it from the console until build-image.sh succeeds"
                    echo "           and the manifest gets its tar SHA."
                fi
            fi
            echo ""
            echo "Syncing control-plane version allowlist: DUDUCLAW_PRO_VERSIONS=$CP_VERSIONS"
            [[ -n "$CP_MANIFEST" ]] && echo "Syncing control-plane delivery manifest: $CP_MANIFEST"
            # ^|^ switches the pair delimiter to '|' so the comma-separated
            # version list and the JSON manifest each survive as ONE value.
            if [[ -z "$CP_MANIFEST" ]]; then
                PRO_GAPS+=("control-plane delivery manifest not computed — no regions updated")
            else
                for CP_REGION in "${CONTROL_PLANE_REGIONS[@]}"; do
                    if gcloud run services update "$CP_SERVICE" --project "$CP_PROJECT" --region "$CP_REGION" \
                        --update-env-vars "^|^DUDUCLAW_PRO_VERSIONS=${CP_VERSIONS}|DUDUCLAW_GENERAL_DELIVERY_MANIFEST=${CP_MANIFEST}" >/dev/null 2>&1; then
                        # Explicitly move pinned traffic to the updated revision.
                        if ! gcloud run services update-traffic "$CP_SERVICE" --project "$CP_PROJECT" --region "$CP_REGION" \
                            --to-latest --quiet >/dev/null 2>&1; then
                            PRO_GAPS+=("[$CP_REGION] control-plane traffic update failed")
                        fi
                    else
                        PRO_GAPS+=("[$CP_REGION] control-plane env update failed")
                        echo "  WARNING: [$CP_REGION] control-plane env update FAILED. Run manually:"
                        echo "    gcloud run services update $CP_SERVICE --project $CP_PROJECT --region $CP_REGION \\"
                        echo "      --update-env-vars '^|^DUDUCLAW_PRO_VERSIONS=$CP_VERSIONS|DUDUCLAW_GENERAL_DELIVERY_MANIFEST=$CP_MANIFEST'"
                    fi
                done
                # Read back every region after all updates, including failed ones.
                for CP_REGION in "${CONTROL_PLANE_REGIONS[@]}"; do
                    read_cp_serving_env "$CP_SERVICE" "$CP_PROJECT" "$CP_REGION"
                    if [[ "$CP_SERVING_MANIFEST" != "$CP_MANIFEST" ]]; then
                        PRO_GAPS+=("[$CP_REGION] delivery manifest written but not serving on '${CP_SERVING_REV:-unknown}'")
                        echo "  WARNING: [$CP_REGION] serving delivery manifest differs from intended value."
                    fi
                    if [[ "$CP_SERVING_VERSIONS" == "$CP_VERSIONS" ]]; then
                        echo "  Control-plane [$CP_REGION] allowlist updated; serving revision $CP_SERVING_REV carries it."
                    else
                        PRO_GAPS+=("[$CP_REGION] control-plane allowlist written but not serving on '${CP_SERVING_REV:-unknown}'")
                        echo "  WARNING: [$CP_REGION] serving allowlist differs from intended value (${CP_SERVING_VERSIONS:-unknown})."
                    fi
                done
            fi
        fi
    fi
    # --- End control-plane env sync ---
fi

# --- Console manuals publish ---
# Runs after the bump renamed [Unreleased] and after the local tag exists: the
# publisher reads CHANGELOG.md and builds cadence from the public git tags.
if [[ "$PRO_RELEASE" == "1" ]]; then
    MANUALS_CMD=(python3 commercial/scripts/publish-manuals.py --version "$NEW_VERSION" --public-repo "$PWD")
    MANUALS_RERUN="python3 commercial/scripts/publish-manuals.py --version $NEW_VERSION"
    if [[ -n "${DUDUCLAW_MANUALS_BUCKET:-}" ]]; then
        MANUALS_CMD+=(--bucket "$DUDUCLAW_MANUALS_BUCKET")
        MANUALS_RERUN="$MANUALS_RERUN --bucket $DUDUCLAW_MANUALS_BUCKET"
    fi
    MANUALS_RERUN="$MANUALS_RERUN --public-repo $PWD"
    if [[ "${DUDUCLAW_SKIP_MANUALS:-0}" == "1" ]]; then
        echo ""
        echo "Skipping console manuals (DUDUCLAW_SKIP_MANUALS=1)."
        PRO_GAPS+=("console manuals not published (DUDUCLAW_SKIP_MANUALS=1) — re-run: python3 commercial/scripts/publish-manuals.py --version $NEW_VERSION")
    else
        echo ""
        echo "Publishing console manuals for v$NEW_VERSION (dry run first)..."
        if ! "${MANUALS_CMD[@]}" --dry-run; then
            PRO_GAPS+=("console manuals dry-run failed — fix, then re-run: $MANUALS_RERUN")
        elif ! "${MANUALS_CMD[@]}"; then
            PRO_GAPS+=("console manuals publish failed — re-run: $MANUALS_RERUN")
        else
            echo "  Console manuals published for v$NEW_VERSION."
        fi
    fi
else
    echo ""
    echo "NOTE: CE-only release — console manuals were not published. Publish manually:"
    echo "      python3 commercial/scripts/publish-manuals.py --version $NEW_VERSION"
fi
# --- End console manuals publish ---

rm -f "${PRO_TAR_SHA_FILE:-}"

# --- Enterprise release gate ---
# `cargo check` above re-resolves commercial/duduclaw-pro-gateway/Cargo.lock
# for the new workspace versions; the image build and the next release use
# `--locked`, so that lock must be committed in the private repository
# (v1.76.1's backfill failed its first image build on a stale lock). It
# changes on every release, so the release commits it here, together with
# the distributor brief generated above, on the commercial branch the
# preflight required; only a failed commit is a gap.
PRO_FOLLOW_UPS=()
if [[ "$PRO_RELEASE" == "1" ]] && git -C commercial rev-parse --git-dir >/dev/null 2>&1; then
    PRO_COMMIT_PATHS=()
    [[ -z "$(git -C commercial status --porcelain -- duduclaw-pro-gateway/Cargo.lock)" ]] \
        || PRO_COMMIT_PATHS+=("duduclaw-pro-gateway/Cargo.lock")
    PRO_BRIEF="marketing/distributor/releases/duduclaw-technology-brief-v$NEW_VERSION.html"
    [[ -z "$(git -C commercial status --porcelain -- "$PRO_BRIEF")" ]] || PRO_COMMIT_PATHS+=("$PRO_BRIEF")
    if [[ ${#PRO_COMMIT_PATHS[@]} -gt 0 ]]; then
        PRO_COMMERCIAL_BRANCH="$(git -C commercial symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
        if git -C commercial add -- "${PRO_COMMIT_PATHS[@]}" &&
            git -C commercial commit -q -m "chore: v$NEW_VERSION pro-gateway Cargo.lock and distributor brief" \
                -- "${PRO_COMMIT_PATHS[@]}"; then
            echo "  Committed in commercial ($PRO_COMMERCIAL_BRANCH): ${PRO_COMMIT_PATHS[*]}"
            PRO_FOLLOW_UPS+=("push the commercial branch and open its PR: git -C commercial push -u origin $PRO_COMMERCIAL_BRANCH")
        else
            PRO_GAPS+=("could not commit ${PRO_COMMIT_PATHS[*]} in commercial — commit it in the private repository")
        fi
    fi
fi
if [[ "$PRO_RELEASE" == "1" && ${#PRO_GAPS[@]} -gt 0 ]]; then
    echo ""
    echo "================================================"
    echo " Release v$NEW_VERSION is INCOMPLETE: the enterprise channel is missing pieces."
    echo "================================================"
    printf '  - %s\n' "${PRO_GAPS[@]}"
    echo ""
    echo "The commit and tags stand locally. Fix every item above, then confirm with:"
    echo "  ./scripts/release.sh verify $NEW_VERSION"
    exit 1
fi
if [[ "$PRO_RELEASE" != "1" ]]; then
    echo ""
    echo "NOTE: CE-only release (DUDUCLAW_RELEASE_WITHOUT_PRO=1) — no enterprise assets"
    echo "      were built; enterprise installs keep their current version."
fi

echo ""
echo "================================================"
echo " Release v$NEW_VERSION prepared successfully!"
echo " All platforms synchronized: Cargo / pyproject (PyPI) / npm / READMEs / installers / desktop"
echo "================================================"
echo ""
echo "Lane: $BUMP_TYPE; Pro image CLOUD_BUILD=$CLOUD_BUILD"
if [[ -n "${DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI:-}" ]]; then
    printf 'Platform CI override reason: %s\n' "$DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI"
fi
if [[ -n "${DUDUCLAW_HOTFIX_ALLOW_DOC_DRIFT:-}" && "$BUMP_TYPE" == "hotfix" ]]; then
    printf 'Hotfix doc-drift reason (signed evidence): %s\n' "$DUDUCLAW_HOTFIX_ALLOW_DOC_DRIFT"
fi
echo "Next steps (owner):"
if [[ ${#PRO_FOLLOW_UPS[@]} -gt 0 ]]; then
    printf '  %s\n' "${PRO_FOLLOW_UPS[@]}"
fi
echo "  git push -u origin $RELEASE_BRANCH"
echo "  gh pr create --base main --head $RELEASE_BRANCH --title 'chore: bump v$NEW_VERSION' --body 'Prepare $BUMP_TYPE release v$NEW_VERSION'"
if [[ "$BUMP_TYPE" == "hotfix" ]]; then
    echo "  Wait for Rust ubuntu-latest success (hotfix fixes and bump PRs)."
    echo "  gh pr merge --admin <n> --merge"
else
    echo "  Wait for Rust ubuntu-latest, Dashboard, Python, Security Audit success."
    echo "  gh pr merge <n> --merge"
fi
echo "  git fetch origin main"
echo "  git merge-base --is-ancestor v$NEW_VERSION origin/main"
echo "  git push origin v$NEW_VERSION desktop-v$NEW_VERSION"
echo "  Watch release, desktop-release, docker-image and computer-use-image workflows."
echo "  ./scripts/release.sh verify $NEW_VERSION"
if [[ "$BUMP_TYPE" == "hotfix" ]]; then
    echo "  Websites retain the preceding artifacts; the console manual entry was published"
    echo "  by this script (it inherits the previous handbooks)."
else
    echo "  Deploy the reviewed website builds (console manuals were published by this script)."
fi
