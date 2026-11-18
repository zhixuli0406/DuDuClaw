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
#   scripts/isolation-sign --help                          # OFFLINE, owner present: sign the
#                                                          # native Linux isolation release
#
# isolation-sign (plan R2 §4.2) runs after CI has published the tag's Linux
# archives. It is started ONLY through the launcher scripts/isolation-sign (the
# kernel starts it with an empty environment), never as `bash scripts/release.sh
# isolation-sign` and never through sudo. Two stages: stage 1 (this working tree,
# no secret) snapshots the tag's ceremony files into a private read-only directory
# and runs stage 2 from that snapshot as a child process with an empty,
# allow-listed environment, removing the snapshot itself on every exit (see
# --help). It needs release/bootstrap-root.sha256 (added after the key
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

# isolation-sign (plan R2 §4.2): its trusted computing base must not be chosen by the
# caller's PATH. These are the first statements executed for it, before any external
# command; every tool is later resolved to an absolute path (isolation_resolve_tools).
# A clean shell is the launcher's job (scripts/isolation-sign: `env -i` from the kernel);
# stage 1 refuses to run unless that launcher started it (isolation_require_launcher).
if [[ "${1:-}" == "isolation-sign" ]]; then
    PATH="/usr/bin:/bin:/usr/sbin:/sbin"
    export PATH
fi

# --- Config ---
WORKSPACE_TOML="Cargo.toml"
PYPI_PKG="duduclaw"   # PyPI + npm package name (used by `verify`)
NPM_PKG="duduclaw"
DRY_RUN=false

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
ISOLATION_REPO="zhixuli0406/DuDuClaw"
# Paths RELATIVE TO THE CEREMONY SNAPSHOT: stage 2 reads them only from there.
ISOLATION_PIN_FILE="release/bootstrap-root.sha256"
ISOLATION_TEMPLATE="release/isolation/manifest-template.json"
# R2-2: the probe container image, by digest only (see the file's header).
ISOLATION_PROBE_DIGEST_FILE="release/isolation/probe-image.digest"
ISOLATION_WORK=""
ISOLATION_SNAPSHOT=""
# R2-1: every file stage 2 reads or executes. Stage 1 writes each of them from the
# TAG's blob into a private read-only snapshot; stage 2 runs only from that snapshot,
# checks it holds exactly these files, and re-checks every byte against the tag.
ISOLATION_CEREMONY_FILES=(
    scripts/isolation-sign
    scripts/release.sh
    scripts/package-linux-release.py
    scripts/refuse-test-license-build.py
    crates/duduclaw-gateway/src/license_runtime.rs
    scripts/sign-isolation-manifest.py
    scripts/check_glibc.py
    release/isolation/manifest-template.json
    release/isolation/probe-image.digest
    release/isolation/README.md
    release/bootstrap-root.sha256
    crates/duduclaw-isolation/src/update.rs
    Cargo.toml
)
# The fixed PATH of the ceremony and where tools may come from (never the caller's PATH).
ISOLATION_SYSTEM_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
ISOLATION_SYSTEM_DIRS="/usr/bin /bin /usr/sbin /sbin"
ISOLATION_HOMEBREW_DIRS="/opt/homebrew/bin /usr/local/bin"
# <variable suffix>:<command>. System tools only from ISOLATION_SYSTEM_DIRS; the others
# from those, then ISOLATION_HOMEBREW_DIRS (ssh-keygen: Homebrew first, for FIDO2).
ISOLATION_SYSTEM_TOOLS="ENV:env BASH:bash GIT:git PYTHON:python3 MKTEMP:mktemp MKDIR:mkdir RM:rm CHMOD:chmod CP:cp DATE:date UNAME:uname"
ISOLATION_HOMEBREW_TOOLS="SSH_KEYGEN:ssh-keygen GH:gh MINISIGN:minisign DOCKER:docker"
ISOLATION_TOOL_ENV="" ISOLATION_TOOL_BASH="" ISOLATION_TOOL_GIT="" ISOLATION_TOOL_PYTHON="" ISOLATION_TOOL_MKTEMP=""
ISOLATION_TOOL_MKDIR="" ISOLATION_TOOL_RM="" ISOLATION_TOOL_CHMOD="" ISOLATION_TOOL_CP="" ISOLATION_TOOL_DATE=""
ISOLATION_TOOL_UNAME="" ISOLATION_TOOL_SSH_KEYGEN="" ISOLATION_TOOL_GH="" ISOLATION_TOOL_MINISIGN="" ISOLATION_TOOL_DOCKER=""
# FIDO2: the ssh-sk-helper of the SELECTED ssh-keygen's own installation (isolation_resolve_tools),
# audited like every tool; empty when that installation has none. Only ssh-keygen -Y sign sees it.
ISOLATION_TOOL_SSH_SK_HELPER=""
# Stage 1 runs only when started by scripts/isolation-sign: the launcher exports exactly these
# names (TMPDIR only on macOS), bash itself adds PWD, OLDPWD, SHLVL and _ (macOS possibly
# __CF_USER_TEXT_ENCODING), and the marker holds the repository the launcher found
# (isolation_require_launcher).
ISOLATION_LAUNCHER_VARIABLES="PATH HOME TERM TMPDIR DUDUCLAW_ISOLATION_LAUNCHER PWD OLDPWD SHLVL _ __CF_USER_TEXT_ENCODING"
# Stage 1 created the snapshot, so stage 1 alone removes it (isolation_cleanup).
ISOLATION_OWNS_SNAPSHOT=0
# Stage-2 markers: set only by stage 1's `env -i` start of stage 2 (their names contain no "KEY").
ISOLATION_SNAPSHOT_MARKER="DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT"
ISOLATION_REPO_MARKER="DUDUCLAW_ISOLATION_CEREMONY_REPO"
# Attended key-location prompts use the controlling terminal only.
ISOLATION_TTY_IN="/dev/tty"
ISOLATION_TTY_OUT="/dev/tty"
# The complete environment of every child (isolation_child_environment) and of git.
ISOLATION_CHILD_ENV=("PATH=$ISOLATION_SYSTEM_PATH")
ISOLATION_GIT_ENV=("PATH=$ISOLATION_SYSTEM_PATH" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
    GIT_TERMINAL_PROMPT=0 GIT_OPTIONAL_LOCKS=0 GIT_NO_REPLACE_OBJECTS=1 GIT_ATTR_NOSYSTEM=1 GIT_NO_LAZY_FETCH=1
    LC_ALL=C)
# No hook, fsmonitor, pager, attribute/ignore file, credential/askpass helper, replace
# ref or transport. The ceremony only runs rev-parse, ls-tree, cat-file blob and
# hash-object --no-filters, none of which runs a filter, textconv or alias.
ISOLATION_GIT_OPTIONS=(--no-pager --no-replace-objects --literal-pathspecs
    -c core.fsmonitor= -c core.hooksPath=/dev/null -c core.pager=cat -c protocol.allow=never
    -c core.untrackedCache=false -c core.attributesFile=/dev/null -c core.excludesFile=/dev/null
    -c core.askPass= -c credential.helper= -c diff.external= -c gc.auto=0 -c maintenance.auto=false)

isolation_sign_usage() {
    # Builtins only: --help runs before any tool is resolved.
    local line
    while IFS= read -r line; do printf '%s\n' "$line"; done <<'EOF'
Usage: scripts/isolation-sign \
         --root-metadata <root.json> --root-signatures <root.json.signatures.json> \
         --output-dir <new directory> [--ssh-keygen <absolute path of ssh-keygen>] \
         [--manifest-valid-days <1-1830, default 365>] \
         [--migration-bundle [--authorization-valid-days <1-1830, default 730>]] \
         [--no-upload] [--preflight]

OFFLINE signing of the native Linux isolation release (plan R2 §4.2). Run it
attended, in an interactive terminal, on the release machine with the hardware
key attached, from a checkout whose HEAD is tag v<version>, after CI has
published that tag's Linux archives. Never in CI; never with stdin/stdout sent
to a log.

Start it ONLY as scripts/isolation-sign <options>, from the repository top level
in a fresh terminal: never as bash scripts/release.sh isolation-sign, never as
bash scripts/isolation-sign, never through sudo. The launcher's first line is
  #!/usr/bin/env -S -i /bin/bash --noprofile --norc
so the kernel starts it with an EMPTY environment (no exported shell function,
no BASH_ENV, no variable of yours). It sets PATH=/usr/bin:/bin:/usr/sbin:/sbin,
HOME from the password database, TERM=dumb, TMPDIR (macOS: your private
per-user directory) and a marker, and starts scripts/release.sh isolation-sign.
Without that marker and exactly that environment, stage 1 refuses before any
tool runs. Your own shell, which starts the launcher, remains trusted.

Two stages (what you will see):
  STAGE 1 runs from this working tree and handles NO secret. Its first statement
  sets PATH=/usr/bin:/bin:/usr/sbin:/sbin. It refuses a start that is not the
  launcher's (that includes CI variables, any DUDUCLAW_ISOLATION_*KEY*
  environment variable, BASH_ENV and exported shell functions), shell tracing,
  wrong options, a non-interactive terminal, a missing
  release/bootstrap-root.sha256 and a HEAD that is not tag v<version>. It
  resolves every tool ONCE to an absolute path:
  env, bash, git, python3, mktemp, mkdir, rm, chmod, cp, date and uname only
  from /usr/bin /bin /usr/sbin /sbin; gh, minisign and docker from those, then
  /opt/homebrew/bin and /usr/local/bin; ssh-keygen from --ssh-keygen (which must
  name ssh-keygen in one of those directories) or else Homebrew first (FIDO2 on
  macOS), then the system. A tool that is not a regular executable file (after
  resolving links) owned by root or you, or whose directory (as named, and after
  resolving links) is group/other-writable or owned by someone else, is refused.
  For security keys (FIDO2), ssh-keygen starts ssh-sk-helper: the ceremony uses
  only the one of the selected ssh-keygen's own installation (/usr/bin/ssh-keygen:
  /usr/libexec/ssh-sk-helper; Homebrew <prefix>/bin/ssh-keygen: libexec/ next to
  its resolved Cellar path, else <prefix>/libexec/ssh-sk-helper), audited by the
  same rule, and passes it as SSH_SK_HELPER to ssh-keygen -Y sign only. Without
  one, signing with a security key is refused.
  For every trusted tool directory owned by YOUR user (Homebrew on Apple Silicon)
  you see one line:
    NOTICE: trusting <directory>: this tool directory is owned by your user, ...
  git runs without any GIT_* variable of yours, with GIT_CONFIG_NOSYSTEM=1,
  GIT_CONFIG_GLOBAL=/dev/null and no hook, fsmonitor, pager, filter, alias or
  transport. Stage 1 then writes each ceremony file FROM THE TAG'S BLOB (git
  cat-file blob, never the working tree) into a new private directory (yours,
  0700), checks each against its blob id (git hash-object --no-filters), makes
  the files 0400 and the directories 0500, prints
    Stage 1: snapshot of v<version> (<commit>) in <directory>; continuing from it (stage 2).
  and starts that snapshot's scripts/release.sh as a CHILD process through env -i
  with only PATH, HOME, TERM, LANG/LC_ALL/LC_CTYPE and two marker variables
  (no SSH_AUTH_SOCK). Stage 1 waits for it and removes the snapshot itself on every
  exit (success, failure, Ctrl-C), also when stage 2 stops at its first check.
  STAGE 2 runs only from such a snapshot and refuses otherwise (the directory is
  yours, 0500/0700, holds exactly the ceremony files, read-only, and $0 is its
  scripts/release.sh). It repeats the tool resolution, the git scrubbing and
  every precondition, re-checks every snapshot byte against tag v<version> of the
  original repository (blob ids recomputed by python3), and prints the tools it
  uses. After that NOTHING of the working tree is read or executed: the scripts,
  template, pin and probe digests come from the snapshot (a file changed in the
  working tree after stage 1 is irrelevant), the released archives come from
  GitHub, --root-metadata must match the pin. --root-metadata and
  --root-signatures are copied ONCE into the private work directory; only those
  copies are checked (pin, threshold, ssh-keygen -Y verify) and only those
  copies are read by every later step. Python helpers run only from the
  snapshot as <absolute python3> -I -B; every child process gets the same
  allow-listed environment. Its work directory is removed on every exit
  (success, failure, Ctrl-C). --preflight stops after the preconditions:
  nothing is signed and no key location is asked for.

--output-dir is NOT where the ceremony works. Every output is produced, signed,
verified, hashed and uploaded from a private staging directory inside stage 2's
work directory (yours, 0700, removed on every exit; a failure deletes only
there). Only at the very end, after the upload and its read-back (with
--no-upload: after the final verification), the results and
SHA256SUMS-isolation.txt are COPIED into --output-dir as a convenience. It must
not exist yet, and its parent must be an existing directory owned by you that
is neither a symbolic link nor group/other-writable (checked before any prompt
and again right before the copy). It is created 0700 (exclusively, never
through a link), each file 0600, each copy checked against the staged SHA-256.
A failed copy never affects what was uploaded; it is reported as such.

Key locations are NEVER command-line arguments (any local process can read
argv) and NEVER environment variables (every child would inherit them): stage 2
asks for each at an attended prompt on the terminal (echoed; a location is not a
secret) and keeps it only in an unexported shell variable:
  target key handle                         every release
  root key R1, root key R2                  --migration-bundle
  legacy release minisign key               --migration-bundle
                                            (empty answer: ~/.minisign/duduclaw-release.key)
Each reaches its signer helper ONLY on the helper's stdin, one location per
line, in this order: the target key (package-linux-release.py
--signing-key-stdin, which passes it on to sign-isolation-manifest.py's stdin);
R1 then R2 (sign-isolation-manifest.py --kind bundle --key-count 2); the legacy
key (sign-isolation-manifest.py --minisign-sign). The helper links it as
<private 0700 temp dir>/signing-key and starts ssh-keygen / minisign with that
neutral name, an explicit minimal environment (no key variable, no
SSH_AUTH_SOCK) and the terminal for your PIN / passphrase; the link is removed
right after. Residual exposure: while ssh-keygen or minisign runs its argv shows
the neutral temporary name, and only your own user (and root) can read the link.

Steps, in order (any failure stops everything; no output is ever overwritten):
  0. Preconditions (stage 2, again): not CI, interactive terminal,
     release/bootstrap-root.sha256 in the snapshot (absent = no production pin =
     nothing to sign), HEAD at tag v<version>, every snapshot file
     byte-identical to its blob at the tag; SHA-256 of --root-metadata equal to
     the pin and its signatures meeting its own root threshold, each verified
     with ssh-keygen -Y verify; --output-dir absent and its parent a directory
     of yours, not a link, not group/other-writable; gh, minisign and docker
     resolved.
  For x86_64 and aarch64:
  1. Download duduclaw-linux-<arch>.tar.gz (+ .sha256, .minisig) of the tag; verify
     the CI minisign signature with the official release key and its exact
     trusted comment; extract duduclaw-bootstrap and execute --print-root-pin
     (natively on Linux of that arch; otherwise in docker: the image ONLY by
     digest from release/isolation/probe-image.digest, pre-pulled by you as a
     separate step (--pull=never), read-only, non-root, no capabilities,
     no-new-privileges, pid and memory limits, --network none, only the
     verifier file mounted read-only, no environment): it must print exactly
     PRODUCTION <pin>.
  2. The target key signs the isolation manifest: release/isolation/manifest-template.json
     rendered for this version/arch (serial = major*10^6+minor*10^3+patch) over the
     binaries of that same released archive; ssh-keygen asks for touch + PIN. The
     signature must be made by the target_key of the pinned root metadata (same
     shape check and ssh-keygen -Y verify as the root threshold, against that key
     only): a wrong key is refused before anything is written, and the written
     manifest is verified again before anything joins the upload list (otherwise
     this architecture's outputs are deleted).
  Only for ISOLATION_MIGRATION_BUNDLE_VERSIONS (owner decision Q6: ONLY v1.83.0),
  which also REQUIRE --migration-bundle (every other version refuses it):
  3. Assemble the bootstrap inbox and the plain-ustar migration bundle
     duduclaw-isolated-linux-<arch>-<version>.tar.
  4. The root threshold (R1 touch + PIN, R2 passphrase) signs its
     BundleAuthorization; the legacy release minisign key signs the tar
     (minisign asks for its password); both are verified before going on. The
     signers are identified by the keys embedded in the signatures (two paths to
     one key are one signer): distinct, registered in --root-metadata, not
     revoked, meeting its threshold, each signature in the shape the Rust
     verifier accepts and verified with ssh-keygen -Y verify over the exact
     bytes; otherwise the authorization is deleted and nothing is added to the
     upload list.
  Then (all from the private staging copies):
  5. After you type UPLOAD (with --no-upload: at once), immediately before the
     upload, everything is verified again: each manifest's target-key signature
     and every artifact it binds (exactly the signed SHA-256 and size), each
     BundleAuthorization's root threshold and its tar (digest, size, version,
     architecture), each bundle's legacy minisign signature. Then
     SHA256SUMS-isolation.txt is written from those same bytes, plus
     "<pin>  root.json" (the root metadata fingerprint).
  6. Upload exactly those files: the update-service assets isolated-linux-<arch>-*
     (and, for the bundle version, duduclaw-bootstrap-linux-<arch> and the four
     bundle files) to the GitHub release without replacing any asset, then
     download every uploaded asset again and compare its SHA-256.
  7. Copy the results and SHA256SUMS-isolation.txt into --output-dir (above).
Publishing SHA256SUMS-isolation.txt on the website and the licensed console is
the separate [external] gate; this script never publishes it there.

Secrets: key files and key handles are opened only by ssh-keygen / minisign,
which prompt on the terminal. This script never copies, prints or logs a key,
passphrase or PIN; its temporary directory holds public release data and, only
while a signer runs, the neutral `signing-key` symlink; it is removed on exit.
gh and docker see only HOME (their own configuration): log in with
`gh auth login` and select the docker context beforehand (GH_TOKEN, DOCKER_HOST
and similar variables are not passed). Before the docker probe can run, pre-pull
the digests of release/isolation/probe-image.digest yourself (docker pull
--platform ... ubuntu@sha256:...).
EOF
}

isolation_fail() {
    echo "Error: $*" >&2
    return 1
}

isolation_sha256() {
    isolation_run "$ISOLATION_TOOL_PYTHON" -I -B -c 'import hashlib, sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as source:
    for chunk in iter(lambda: source.read(1 << 20), b""):
        digest.update(chunk)
print(digest.hexdigest())' "$1"
}

# Exact minisign text: four lines, prehashed (ED) signature, exact trusted comment.
isolation_check_minisig() {
    isolation_run "$ISOLATION_TOOL_PYTHON" -I -B - "$1" "$2" <<'PY'
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

# Every child process of the ceremony: `env -i` (the resolved env) plus exactly
# ISOLATION_CHILD_ENV, and an absolute program path (never a bare name).
isolation_run() {
    "$ISOLATION_TOOL_ENV" -i "${ISOLATION_CHILD_ENV[@]}" "$@"
}

# Every git call goes through here: scrubbed environment (ISOLATION_GIT_ENV, no GIT_*
# of the caller, no system/global config) and ISOLATION_GIT_OPTIONS.
isolation_git() {
    "$ISOLATION_TOOL_ENV" -i "${ISOLATION_GIT_ENV[@]}" "$ISOLATION_TOOL_GIT" "${ISOLATION_GIT_OPTIONS[@]}" "$@"
}

# R2-3 hand-off: `isolation_handoff <n> <location 1> .. <location n> <program> <args>`
# runs the helper with the n key locations on its STDIN, one per line, in the given
# order. printf is a builtin: no process ever has a location in its argv or environment.
isolation_handoff() {
    local count="$1" index
    local -a locations=()
    shift
    for (( index = 0; index < count; index++ )); do
        locations+=("$1")
        shift
    done
    printf '%s\n' "${locations[@]}" | isolation_run "$@"
}

# Allow-listed environment for children (and for stage 2): fixed PATH, HOME, TERM, locale.
isolation_child_environment() {
    local name value
    ISOLATION_CHILD_ENV=("PATH=$ISOLATION_SYSTEM_PATH")
    if [[ "${HOME:-}" == /* && "$HOME" != *$'\n'* ]]; then ISOLATION_CHILD_ENV+=("HOME=$HOME"); fi
    for name in TERM LANG LC_ALL LC_CTYPE; do
        value="${!name:-}"
        if [[ "$value" =~ ^[A-Za-z0-9_.@-]+$ ]]; then ISOLATION_CHILD_ENV+=("$name=$value"); fi
    done
}

# Refusals that need no tool and no secret, first in both stages.
isolation_refuse_hostile_start() {
    local name
    if [[ -n "${CI:-}" || -n "${GITHUB_ACTIONS:-}" ]]; then
        isolation_fail "isolation-sign uses offline keys and never runs in CI"; return 1
    fi
    for name in ${!DUDUCLAW_ISOLATION_@}; do
        case "$name" in
            *KEY*)
                isolation_fail "$name is set: key locations are entered only at the attended prompt, never" \
                    "through the environment (every child process would inherit it); unset it and start again"
                return 1 ;;
        esac
    done
    # Secondary assertion only (the launcher's empty environment is the real protection):
    # an exported function carries the export attribute in `declare -F` ("declare -fx name").
    local line attributes exported=""
    while IFS= read -r line; do
        attributes="${line#declare -}"
        attributes="${attributes%% *}"
        case "$attributes" in *x*) exported=1 ;; esac
    done <<< "$(declare -F)"
    if [[ -n "$exported" || -n "${BASH_ENV:-}" ]]; then
        isolation_fail "the environment exports shell functions or BASH_ENV, which can replace commands of this" \
            "script; start the ceremony as scripts/isolation-sign from a fresh terminal"
        return 1
    fi
    case "$-" in
        *x*|*v*) isolation_fail "shell tracing (set -x / -v) is on; it is refused for the ceremony"; return 1 ;;
    esac
}

# Stage 1's FIRST check (builtins only): it runs only when started by scripts/isolation-sign.
# The marker must name the current directory (the repository the launcher found) and every
# exported variable must be one the launcher (or bash itself) sets. NOTE: this is defence
# against ACCIDENTAL direct invocation (`bash scripts/release.sh isolation-sign`, sudo, a
# polluted login shell), not against a hostile shell: whoever controls the bash running
# this script can fake every check here. The protection against exported functions,
# BASH_ENV and inherited variables is the launcher itself, which the kernel starts through
# `env -i`; only the operator's own shell that starts the launcher remains trusted.
isolation_require_launcher() {
    local name unexpected="" here
    here="$(pwd -P)"
    for name in $(compgen -e); do
        case " $ISOLATION_LAUNCHER_VARIABLES " in
            *" $name "*) ;;
            *) unexpected="$unexpected $name" ;;
        esac
    done
    if [[ "${DUDUCLAW_ISOLATION_LAUNCHER:-}" != /* || "$DUDUCLAW_ISOLATION_LAUNCHER" != "$here" \
            || -n "$unexpected" ]]; then
        isolation_fail "isolation-sign runs only when started as scripts/isolation-sign from the repository" \
            "top level in a fresh terminal (never bash scripts/release.sh isolation-sign, never through sudo)" \
            "${unexpected:+; unexpected environment variable(s):$unexpected}"
        return 1
    fi
}

# Unset every GIT_* variable before the first git call (isolation_git also starts
# git through env -i with ISOLATION_GIT_ENV only).
isolation_scrub_git_environment() {
    local name
    for name in ${!GIT_@}; do unset "$name"; done
    export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 GIT_OPTIONAL_LOCKS=0
}

# Resolves ISOLATION_TOOL_<NAME> once to absolute paths (rule: ISOLATION_SYSTEM_TOOLS
# only from ISOLATION_SYSTEM_DIRS; ISOLATION_HOMEBREW_TOOLS from those, then
# ISOLATION_HOMEBREW_DIRS, ssh-keygen Homebrew first unless --ssh-keygen names
# ssh-keygen in one of these directories). python3 (from a system directory) then
# audits every candidate: a regular executable after resolving links, owned by root
# or you, not group/other-writable, in directories (as named and resolved) owned by
# root or you and not group/other-writable; one NOTICE line names each trusted
# directory owned by you. gh, minisign and docker may be absent here; stage 2 refuses
# before any prompt if they are (isolation_require_tools). Everything else must exist.
ISOLATION_TOOL_AUDIT_PY='import os, stat, sys
uid = os.geteuid()
trusted = []
def directory_ok(path):
    try:
        info = os.stat(path)
    except OSError:
        return False
    if not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, uid) or info.st_mode & 0o022:
        return False
    if info.st_uid != 0 and path not in trusted:
        trusted.append(path)
    return True
status = 0
for argument in sys.argv[1:]:
    name, _, candidate = argument.partition("=")
    real = os.path.realpath(candidate)
    try:
        info = os.stat(real)
        ok = (os.path.isabs(candidate) and stat.S_ISREG(info.st_mode) and os.access(real, os.X_OK)
              and info.st_uid in (0, uid) and not info.st_mode & 0o022
              and not any(character in real for character in "\t\n=")
              and directory_ok(os.path.dirname(candidate)) and directory_ok(os.path.dirname(real)))
    except OSError:
        ok = False
    if not ok:
        sys.stderr.write("Error: refusing " + candidate + ": not a regular executable owned by root or you, or its"
                         " directory is group/other-writable or owned by someone else\n")
        status = 1
        continue
    sys.stdout.write(name + "\t" + real + "\n")
for path in trusted:
    sys.stderr.write("NOTICE: trusting " + path + ": this tool directory is owned by your user, not root;"
                     " anything running as you could replace its tools.\n")
sys.exit(status)'
isolation_resolve_tools() {
    local entry variable name directory candidate dirs output env_path="" python_path="" keygen_named=""
    local -a audit=()
    for entry in $ISOLATION_SYSTEM_TOOLS $ISOLATION_HOMEBREW_TOOLS; do
        variable="${entry%%:*}"
        name="${entry#*:}"
        printf -v "ISOLATION_TOOL_$variable" '%s' ""
        case " $ISOLATION_SYSTEM_TOOLS " in
            *" $entry "*) dirs="$ISOLATION_SYSTEM_DIRS" ;;
            *) dirs="$ISOLATION_SYSTEM_DIRS $ISOLATION_HOMEBREW_DIRS" ;;
        esac
        if [[ "$variable" == "SSH_KEYGEN" ]]; then dirs="$ISOLATION_HOMEBREW_DIRS $ISOLATION_SYSTEM_DIRS"; fi
        candidate=""
        if [[ "$variable" == "SSH_KEYGEN" && -n "$ISOLATION_SSH_KEYGEN_OPTION" ]]; then
            case " $dirs " in
                *" ""${ISOLATION_SSH_KEYGEN_OPTION%/*}"" "*) ;;
                *) isolation_fail "--ssh-keygen must be the absolute path of ssh-keygen in one of: $dirs"; return 1 ;;
            esac
            if [[ "${ISOLATION_SSH_KEYGEN_OPTION##*/}" != "ssh-keygen" ]]; then
                isolation_fail "--ssh-keygen must be the absolute path of ssh-keygen in one of: $dirs"; return 1
            fi
            candidate="$ISOLATION_SSH_KEYGEN_OPTION"
        else
            for directory in $dirs; do
                if [[ -e "$directory/$name" ]]; then candidate="$directory/$name"; break; fi
            done
        fi
        if [[ -z "$candidate" ]]; then
            case "$variable" in
                GH|MINISIGN|DOCKER) continue ;;
            esac
            isolation_fail "$name not found in $dirs (the ceremony never searches PATH)"; return 1
        fi
        if [[ "$variable" == "ENV" ]]; then env_path="$candidate"; fi
        if [[ "$variable" == "PYTHON" ]]; then python_path="$candidate"; fi
        if [[ "$variable" == "SSH_KEYGEN" ]]; then keygen_named="$candidate"; fi
        audit+=("$variable=$candidate")
    done
    output="$("$env_path" -i "PATH=$ISOLATION_SYSTEM_PATH" "$python_path" -I -B -c "$ISOLATION_TOOL_AUDIT_PY" \
        "${audit[@]}")" || { isolation_fail "a ceremony tool failed the location audit (above)"; return 1; }
    while IFS=$'\t' read -r variable candidate; do
        case " $ISOLATION_SYSTEM_TOOLS $ISOLATION_HOMEBREW_TOOLS " in
            *" ""$variable"":"*) printf -v "ISOLATION_TOOL_$variable" '%s' "$candidate" ;;
            *) isolation_fail "unexpected tool audit output"; return 1 ;;
        esac
    done <<< "$output"
    for entry in $ISOLATION_SYSTEM_TOOLS SSH_KEYGEN:ssh-keygen; do
        variable="ISOLATION_TOOL_${entry%%:*}"
        if [[ "${!variable}" != /* ]]; then isolation_fail "${entry#*:} could not be resolved"; return 1; fi
    done
    isolation_resolve_sk_helper "$keygen_named"
}

# FIDO2: ssh-keygen starts ssh-sk-helper for a security key. Only the helper of the SELECTED
# ssh-keygen's own installation is used ($1: ssh-keygen as named; ISOLATION_TOOL_SSH_KEYGEN:
# its resolved path): libexec/ next to the resolved path (/usr/bin -> /usr/libexec; Homebrew:
# the Cellar keg's libexec, that ssh-keygen's compiled-in default), else <prefix>/libexec of
# the path as named. It passes the same audit as every tool; a helper that is present but
# fails it refuses. None: ISOLATION_TOOL_SSH_SK_HELPER stays empty and the signer refuses a
# security key.
isolation_resolve_sk_helper() {
    local real_prefix="${ISOLATION_TOOL_SSH_KEYGEN%/*}" named_prefix="${1%/*}" tab=$'\t' directory candidate=""
    local output
    real_prefix="${real_prefix%/*}"
    named_prefix="${named_prefix%/*}"
    ISOLATION_TOOL_SSH_SK_HELPER=""
    for directory in "$real_prefix/libexec" "$named_prefix/libexec"; do
        if [[ -e "$directory/ssh-sk-helper" || -L "$directory/ssh-sk-helper" ]]; then
            candidate="$directory/ssh-sk-helper"
            break
        fi
    done
    if [[ -z "$candidate" ]]; then return 0; fi
    output="$("$ISOLATION_TOOL_ENV" -i "PATH=$ISOLATION_SYSTEM_PATH" "$ISOLATION_TOOL_PYTHON" -I -B \
        -c "$ISOLATION_TOOL_AUDIT_PY" "SSH_SK_HELPER=$candidate")" \
        || { isolation_fail "the ssh-sk-helper next to the selected ssh-keygen failed the location audit (above)"
             return 1; }
    if [[ "$output" != "SSH_SK_HELPER$tab"/* || "$output" == *$'\n'* ]]; then
        isolation_fail "unexpected tool audit output"; return 1
    fi
    ISOLATION_TOOL_SSH_SK_HELPER="${output#SSH_SK_HELPER$tab}"
}

# Stage 2, before any prompt: the tools only some steps use must exist as well.
isolation_require_tools() {
    local entry variable
    for entry in GH:gh MINISIGN:minisign DOCKER:docker; do
        variable="ISOLATION_TOOL_${entry%%:*}"
        if [[ "${!variable}" != /* ]]; then
            isolation_fail "${entry#*:} not found in $ISOLATION_SYSTEM_DIRS $ISOLATION_HOMEBREW_DIRS" \
                "(the ceremony never searches PATH)"
            return 1
        fi
    done
}

# Option parsing shared by both stages (sets ISOLATION_* option variables).
isolation_parse_arguments() {
    ISOLATION_ROOT_METADATA="" ISOLATION_ROOT_SIGNATURES="" ISOLATION_OUTPUT_DIR="" ISOLATION_SSH_KEYGEN_OPTION=""
    ISOLATION_MANIFEST_DAYS=365 ISOLATION_AUTHORIZATION_DAYS=730 ISOLATION_BUNDLE_OPTION=0 ISOLATION_UPLOAD=1
    ISOLATION_PREFLIGHT=0 ISOLATION_HELP=0
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -h|--help) ISOLATION_HELP=1; return 0 ;;
            --target-key|--root-key|--legacy-minisign-key)
                isolation_fail "$1 is refused: key locations are never command-line arguments (other local" \
                    "processes can read argv) nor environment variables; answer the attended prompt (see --help)"
                return 1 ;;
            --root-metadata) ISOLATION_ROOT_METADATA="${2:?--root-metadata needs a value}"; shift 2 ;;
            --root-signatures) ISOLATION_ROOT_SIGNATURES="${2:?--root-signatures needs a value}"; shift 2 ;;
            --output-dir) ISOLATION_OUTPUT_DIR="${2:?--output-dir needs a value}"; shift 2 ;;
            --ssh-keygen) ISOLATION_SSH_KEYGEN_OPTION="${2:?--ssh-keygen needs a value}"; shift 2 ;;
            --manifest-valid-days) ISOLATION_MANIFEST_DAYS="${2:?--manifest-valid-days needs a value}"; shift 2 ;;
            --authorization-valid-days)
                ISOLATION_AUTHORIZATION_DAYS="${2:?--authorization-valid-days needs a value}"; shift 2 ;;
            --migration-bundle) ISOLATION_BUNDLE_OPTION=1; shift ;;
            --no-upload) ISOLATION_UPLOAD=0; shift ;;
            --preflight) ISOLATION_PREFLIGHT=1; shift ;;
            *) isolation_fail "unknown isolation-sign option '$1' (see --help)"; return 1 ;;
        esac
    done
}

# Owner decision Q6, required options and validity windows for version $1.
isolation_check_arguments() {
    local version="$1" bundle_release=0 listed days
    for listed in $ISOLATION_MIGRATION_BUNDLE_VERSIONS; do
        if [[ "$listed" == "$version" ]]; then bundle_release=1; fi
    done
    if [[ "$ISOLATION_BUNDLE_OPTION" == "1" && "$bundle_release" == "0" ]]; then
        isolation_fail "owner decision Q6: only v$ISOLATION_MIGRATION_BUNDLE_VERSIONS ships a migration bundle;" \
            "v$version reaches migrated hosts through the update service. Drop --migration-bundle."
        return 1
    fi
    if [[ "$ISOLATION_BUNDLE_OPTION" == "0" && "$bundle_release" == "1" ]]; then
        isolation_fail "v$version is the migration-bundle release (owner decision Q6);" \
            "pass --migration-bundle (the R1 and R2 key locations are asked for at the prompt)."
        return 1
    fi
    if [[ -z "$ISOLATION_ROOT_METADATA" || -z "$ISOLATION_ROOT_SIGNATURES" || -z "$ISOLATION_OUTPUT_DIR" ]]; then
        isolation_fail "--root-metadata, --root-signatures and --output-dir are required (see --help)"
        return 1
    fi
    for days in "$ISOLATION_MANIFEST_DAYS" "$ISOLATION_AUTHORIZATION_DAYS"; do
        if [[ ! "$days" =~ ^[1-9][0-9]{0,3}$ ]] || (( days > 1830 )); then
            isolation_fail "validity must be 1 to 1830 days"; return 1
        fi
    done
}

# The first `version = "x.y.z"` line of Cargo.toml $1 (as extract_version), builtins only.
isolation_read_version() {
    local line pattern='^version = "([0-9]+\.[0-9]+\.[0-9]+)"'
    if [[ ! -f "$1" || -L "$1" ]]; then return 1; fi
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" =~ $pattern ]]; then
            printf '%s\n' "${BASH_REMATCH[1]}"
            return 0
        fi
    done < "$1"
    return 1
}

# The blob id of $2 at commit $1: a regular file (mode 100644/100755), never a link
# or submodule, exactly that path.
isolation_tag_blob() {
    local listing pattern="^(100644|100755) blob ([0-9a-f]{40}|[0-9a-f]{64})"$'\t'"(.+)$"
    listing="$(isolation_git ls-tree "$1" -- "$2")" || { isolation_fail "git ls-tree failed for $2"; return 1; }
    if [[ ! "$listing" =~ $pattern || "${BASH_REMATCH[3]}" != "$2" ]]; then
        isolation_fail "$2 is not a regular file tracked at the tag"; return 1
    fi
    printf '%s\n' "${BASH_REMATCH[2]}"
}

# EXIT trap of both stages. Stage 2 removes its work directory; stage 1, which created the
# snapshot and waits for stage 2 as a child process, removes the snapshot (ISOLATION_OWNS_SNAPSHOT)
# on every path: success, failure, a stage 2 that stopped before its own trap, signals.
isolation_cleanup() {
    if [[ -n "$ISOLATION_WORK" && -d "$ISOLATION_WORK" ]]; then
        isolation_run "$ISOLATION_TOOL_CHMOD" -R u+w -- "$ISOLATION_WORK" || true
        isolation_run "$ISOLATION_TOOL_RM" -rf -- "$ISOLATION_WORK" || true
    fi
    if [[ "$ISOLATION_OWNS_SNAPSHOT" == "1" && -n "$ISOLATION_SNAPSHOT" && -d "$ISOLATION_SNAPSHOT" ]]; then
        isolation_run "$ISOLATION_TOOL_CHMOD" -R u+w -- "$ISOLATION_SNAPSHOT" || true
        isolation_run "$ISOLATION_TOOL_RM" -rf -- "$ISOLATION_SNAPSHOT" || true
    fi
}

isolation_set_traps() {
    trap isolation_cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
}

# A private base for the snapshot: TMPDIR if it is yours with mode 0700 (macOS), else /tmp
# (root-owned, sticky).
ISOLATION_TMP_AUDIT_PY='import os, stat, sys
uid = os.geteuid()
for candidate in (sys.argv[1], "/tmp"):
    if not candidate or not os.path.isabs(candidate):
        continue
    real = os.path.realpath(candidate)
    try:
        info = os.stat(real)
    except OSError:
        continue
    private = info.st_uid == uid and not info.st_mode & 0o077
    shared = info.st_uid == 0 and info.st_mode & stat.S_ISVTX
    if stat.S_ISDIR(info.st_mode) and (private or shared) and "\n" not in real:
        print(real)
        sys.exit(0)
sys.exit("Error: no private (yours, 0700) or root-owned sticky directory for the ceremony snapshot")'

# STAGE 1 (runs from the mutable working tree; handles NO secret; started only by
# scripts/isolation-sign). Pre-checks, then a private snapshot of the TAG's ceremony files,
# then the snapshot's release.sh as a CHILD process with an empty, allow-listed environment;
# stage 1's EXIT trap removes the snapshot afterwards on every path. Nothing here is
# trusted by stage 2.
isolation_stage1() {
    isolation_require_launcher || return 1
    isolation_refuse_hostile_start || return 1
    isolation_parse_arguments "$@" || return 1
    if [[ "$ISOLATION_HELP" == "1" ]]; then isolation_sign_usage; return 0; fi
    local version
    version="$(isolation_read_version Cargo.toml)" \
        || { isolation_fail "run isolation-sign from the repository top level (no Cargo.toml version here)"; return 1; }
    isolation_check_arguments "$version" || return 1
    if [[ ! -t 0 ]]; then
        isolation_fail "isolation-sign must run attended in an interactive terminal (keys prompt on the tty)"
        return 1
    fi
    if [[ ! -f "$ISOLATION_PIN_FILE" || -L "$ISOLATION_PIN_FILE" ]]; then
        isolation_fail "$ISOLATION_PIN_FILE is absent: there is no production root pin, so no isolation" \
            "release can be signed (the key ceremony adds it through a reviewed PR)."
        return 1
    fi
    isolation_scrub_git_environment
    isolation_resolve_tools || return 1
    isolation_child_environment
    local tag="v$version" top head tag_commit base path id file recomputed
    top="$(isolation_git rev-parse --show-toplevel)" || { isolation_fail "not a git checkout"; return 1; }
    if [[ "$top" != "$(pwd -P)" ]]; then
        isolation_fail "run isolation-sign from the repository top level"; return 1
    fi
    head="$(isolation_git rev-parse --verify HEAD)" || { isolation_fail "cannot read HEAD"; return 1; }
    tag_commit="$(isolation_git rev-parse --verify --quiet "refs/tags/$tag^{commit}")" || tag_commit=""
    if [[ -z "$tag_commit" || "$head" != "$tag_commit" ]]; then
        isolation_fail "check out tag $tag first: the template, pin file and released archives must belong to it"
        return 1
    fi
    base="$(isolation_run "$ISOLATION_TOOL_PYTHON" -I -B -c "$ISOLATION_TMP_AUDIT_PY" "${TMPDIR:-}")" || return 1
    umask 077
    ISOLATION_OWNS_SNAPSHOT=1
    isolation_set_traps
    ISOLATION_SNAPSHOT="$(isolation_run "$ISOLATION_TOOL_MKTEMP" -d "$base/duduclaw-isolation-ceremony.XXXXXX")" \
        || { isolation_fail "cannot create the ceremony snapshot directory"; return 1; }
    for path in "${ISOLATION_CEREMONY_FILES[@]}"; do
        id="$(isolation_tag_blob "$tag_commit" "$path")" || return 1
        file="$ISOLATION_SNAPSHOT/$path"
        isolation_run "$ISOLATION_TOOL_MKDIR" -p -m 0700 -- "${file%/*}" || return 1
        isolation_git cat-file blob "$id" > "$file" || { isolation_fail "git cat-file failed for $path"; return 1; }
        recomputed="$(isolation_git hash-object --no-filters -t blob -- "$file")" || return 1
        if [[ "$recomputed" != "$id" ]]; then
            isolation_fail "$path written to the snapshot is not its blob $id at $tag"; return 1
        fi
    done
    isolation_run "$ISOLATION_TOOL_CHMOD" -R u=rX,go= -- "$ISOLATION_SNAPSHOT" || return 1
    echo "Stage 1: snapshot of $tag ($tag_commit) in $ISOLATION_SNAPSHOT; continuing from it (stage 2)."
    # A child, never exec: this shell's EXIT trap must outlive stage 2 to remove the snapshot
    # whatever stage 2 does (a terminal signal reaches both; this trap runs after stage 2 ends).
    local status=0
    "$ISOLATION_TOOL_ENV" -i "${ISOLATION_CHILD_ENV[@]}" "$ISOLATION_SNAPSHOT_MARKER=$ISOLATION_SNAPSHOT" \
        "$ISOLATION_REPO_MARKER=$top" "$ISOLATION_TOOL_BASH" --noprofile --norc \
        "$ISOLATION_SNAPSHOT/scripts/release.sh" isolation-sign "$@" || status=$?
    return "$status"
}

# Stage 2's own check of the snapshot it runs from: an absolute physical directory owned
# by you, mode 0500/0700, in a directory that is yours or root-owned (and sticky when
# shared); every entry a directory (0500/0700) on the way to a listed file or a listed
# regular file (yours, read-only, one link); exactly the listed files; and $0 is its
# scripts/release.sh.
ISOLATION_SNAPSHOT_AUDIT_PY='import os, stat, sys
snapshot, script, files = sys.argv[1], sys.argv[2], set(sys.argv[3:])
uid = os.geteuid()
def fail(message):
    sys.exit("Error: not a valid ceremony snapshot: " + message)
if not os.path.isabs(snapshot) or os.path.realpath(snapshot) != snapshot:
    fail("the snapshot path is not absolute and physical")
info = os.lstat(snapshot)
if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or stat.S_IMODE(info.st_mode) not in (0o500, 0o700):
    fail("the snapshot directory is not yours with mode 0500 or 0700")
parent = os.stat(os.path.dirname(snapshot))
if parent.st_uid not in (0, uid) or (parent.st_mode & 0o022 and not parent.st_mode & stat.S_ISVTX):
    fail("the snapshot sits in a directory others can rename it in")
if os.path.realpath(script) != os.path.join(snapshot, "scripts", "release.sh"):
    fail("this script is not the snapshot scripts/release.sh")
seen = set()
for directory, subdirectories, names in os.walk(snapshot):
    for name in subdirectories:
        path = os.path.join(directory, name)
        info = os.lstat(path)
        prefix = os.path.relpath(path, snapshot) + "/"
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or stat.S_IMODE(info.st_mode) not in (0o500, 0o700)
                or not any(item.startswith(prefix) for item in files)):
            fail("unexpected directory " + prefix)
    for name in names:
        path = os.path.join(directory, name)
        info = os.lstat(path)
        relative = os.path.relpath(path, snapshot)
        if (relative not in files or not stat.S_ISREG(info.st_mode) or info.st_uid != uid or info.st_nlink != 1
                or stat.S_IMODE(info.st_mode) != 0o400):
            fail("unexpected or writable entry " + relative)
        seen.add(relative)
if seen != files:
    fail("missing " + ", ".join(sorted(files - seen)))'

# Every snapshot file against the blob id listed at the tag (path=id ...), recomputed
# here from the snapshot bytes ("blob <size>\0" + bytes; SHA-1 or SHA-256 repositories).
ISOLATION_BLOB_CHECK_PY='import hashlib, os, sys
snapshot = sys.argv[1]
for argument in sys.argv[2:]:
    path, _, expected = argument.rpartition("=")
    with open(os.path.join(snapshot, path), "rb") as source:
        data = source.read()
    digest = hashlib.sha1 if len(expected) == 40 else hashlib.sha256
    if digest(b"blob " + str(len(data)).encode("ascii") + b"\0" + data).hexdigest() != expected:
        sys.exit("Error: snapshot file " + path + " differs from its blob at the tag")'

# STAGE 2 (runs from the snapshot; trusts nothing stage 1 did). Only started by stage 1, as
# its child; the snapshot belongs to stage 1 (which removes it), the work directory to stage 2.
isolation_stage2() {
    isolation_refuse_hostile_start || return 1
    local snapshot="${DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT:-}" repo="${DUDUCLAW_ISOLATION_CEREMONY_REPO:-}"
    if [[ "$snapshot" != /* || "$repo" != /* || "$0" != "$snapshot/scripts/release.sh" || ! -d "$snapshot" \
            || -L "$snapshot" || ! -O "$snapshot" ]]; then
        isolation_fail "not launched from a valid ceremony snapshot: stage 2 runs only as started by stage 1." \
            "Start the ceremony as scripts/isolation-sign from the checkout (nothing was removed)."
        return 1
    fi
    isolation_parse_arguments "$@" || return 1
    isolation_scrub_git_environment
    isolation_resolve_tools || return 1
    isolation_child_environment
    if ! isolation_run "$ISOLATION_TOOL_PYTHON" -I -B -c "$ISOLATION_SNAPSHOT_AUDIT_PY" "$snapshot" "$0" \
            "${ISOLATION_CEREMONY_FILES[@]}"; then
        isolation_fail "stage 2 refuses to run from it (nothing was removed)"; return 1
    fi
    if [[ "$(pwd -P)" != "$repo" ]]; then
        isolation_fail "stage 2 must start in the repository stage 1 checked"; return 1
    fi
    ISOLATION_SNAPSHOT="$snapshot"
    isolation_set_traps
    ISOLATION_WORK="$(isolation_run "$ISOLATION_TOOL_MKTEMP" -d "${snapshot%/*}/duduclaw-isolation-sign.XXXXXX")" \
        || { isolation_fail "cannot create the work directory"; return 1; }
    isolation_run "$ISOLATION_TOOL_MKDIR" -m 0700 -- "$ISOLATION_WORK/tmp" || return 1
    ISOLATION_CHILD_ENV+=("TMPDIR=$ISOLATION_WORK/tmp")
    echo "Stage 2: running from $snapshot. Tools: env=$ISOLATION_TOOL_ENV git=$ISOLATION_TOOL_GIT" \
        "python3=$ISOLATION_TOOL_PYTHON ssh-keygen=$ISOLATION_TOOL_SSH_KEYGEN" \
        "ssh-sk-helper=${ISOLATION_TOOL_SSH_SK_HELPER:-missing} gh=${ISOLATION_TOOL_GH:-missing}" \
        "minisign=${ISOLATION_TOOL_MINISIGN:-missing} docker=${ISOLATION_TOOL_DOCKER:-missing}"
    run_isolation_sign
}

# R2-1 stage-2 gate on the ORIGINAL repository. Stage 2 reads nothing from its working
# tree (every file it reads or executes is in the snapshot, the released archives come
# from GitHub, --root-metadata is checked against the pin), so the gate covers exactly
# what it uses from there: HEAD is the tag's commit, and every snapshot file is the
# blob of that commit (blob ids recomputed by python3 from the snapshot bytes).
isolation_check_repository() {
    local tag="$1" top head tag_commit path id
    local -a expected=()
    top="$(isolation_git rev-parse --show-toplevel)" || { isolation_fail "not a git checkout"; return 1; }
    if [[ "$top" != "$(pwd -P)" ]]; then
        isolation_fail "run isolation-sign from the repository top level"; return 1
    fi
    head="$(isolation_git rev-parse --verify HEAD)" || { isolation_fail "cannot read HEAD"; return 1; }
    tag_commit="$(isolation_git rev-parse --verify --quiet "refs/tags/$tag^{commit}")" || tag_commit=""
    if [[ -z "$tag_commit" || "$head" != "$tag_commit" ]]; then
        isolation_fail "check out tag $tag first: the template, pin file and released archives must belong to it"
        return 1
    fi
    for path in "${ISOLATION_CEREMONY_FILES[@]}"; do
        id="$(isolation_tag_blob "$tag_commit" "$path")" || return 1
        expected+=("$path=$id")
    done
    if ! isolation_run "$ISOLATION_TOOL_PYTHON" -I -B -c "$ISOLATION_BLOB_CHECK_PY" "$ISOLATION_SNAPSHOT" \
            "${expected[@]}"; then
        isolation_fail "the ceremony snapshot differs from tag $tag"; return 1
    fi
}

# R2-3: one key location, ONLY from an attended prompt on the controlling terminal
# (echo on; never argv, never the environment). Prints the absolute location for an
# unexported shell variable of the caller. $2 (optional) is used when the answer is
# empty. Messages name the key, never the location.
isolation_key_location() {
    local label="$1" fallback="${2:-}" value=""
    if ! { exec 3<"$ISOLATION_TTY_IN" 4>>"$ISOLATION_TTY_OUT"; } 2>/dev/null; then
        isolation_fail "no terminal for the $label prompt (key locations are entered only there)"; return 1
    fi
    printf 'Location of the %s (a path, not a secret): ' "$label" >&4
    IFS= read -r value <&3 || value=""
    exec 3<&- 4>&-
    if [[ -z "$value" ]]; then value="$fallback"; fi
    if [[ -z "$value" || "$value" == *$'\n'* || "$value" == *$'\r'* ]]; then
        isolation_fail "the $label location is empty or malformed"; return 1
    fi
    if [[ "$value" != /* ]]; then value="$PWD/$value"; fi
    if [[ ! -f "$value" ]]; then
        isolation_fail "the $label location is not a file"; return 1
    fi
    printf '%s\n' "$value"
}

# R2-2: the digest reference for docker platform $1 from ISOLATION_PROBE_DIGEST_FILE:
# exactly one "<platform> ubuntu@sha256:<64 lowercase hex>" line. A tag, a
# placeholder, a duplicate or a missing line refuses.
isolation_probe_image() {
    local platform="$1" line name reference found="" count=0
    if [[ ! -f "$ISOLATION_PROBE_DIGEST_FILE" || -L "$ISOLATION_PROBE_DIGEST_FILE" ]]; then
        isolation_fail "$ISOLATION_PROBE_DIGEST_FILE is missing"; return 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in ''|'#'*) continue ;; esac
        name="${line%% *}"
        reference="${line#* }"
        if [[ "$name" == "$platform" ]]; then
            found="$reference"
            count=$((count + 1))
        fi
    done < "$ISOLATION_PROBE_DIGEST_FILE"
    if [[ "$count" != "1" || ! "$found" =~ ^ubuntu@sha256:[0-9a-f]{64}$ ]]; then
        isolation_fail "$ISOLATION_PROBE_DIGEST_FILE has no verified digest for $platform (one line" \
            "'$platform ubuntu@sha256:<64 hex>'; tags and placeholders are refused). Fill in a digest you" \
            "verified through a reviewed commit, or run isolation-sign on Linux of that architecture."
        return 1
    fi
    printf '%s\n' "$found"
}

# R2-2: the complete docker argv for the probe, in ISOLATION_DOCKER_ARGV.
# $1 verifier file (mounted alone, read-only), $2 docker platform, $3 digest reference.
isolation_probe_docker_argv() {
    local binary="$1" platform="$2" image="$3"
    if [[ ! "$image" =~ ^ubuntu@sha256:[0-9a-f]{64}$ ]]; then
        isolation_fail "the probe image must be an ubuntu@sha256 digest reference"; return 1
    fi
    if [[ "$binary" != /* || "$binary" == *,* || "$binary" == *$'\n'* ]]; then
        isolation_fail "the verifier path cannot be bind-mounted safely"; return 1
    fi
    ISOLATION_DOCKER_ARGV=("$ISOLATION_TOOL_DOCKER" run --rm --pull=never --platform "$platform"
        --network none --read-only --user 65534:65534 --cap-drop=ALL
        --security-opt no-new-privileges --pids-limit 32 --memory 256m --memory-swap 256m
        --entrypoint /probe/duduclaw-bootstrap
        --mount "type=bind,source=$binary,target=/probe/duduclaw-bootstrap,readonly"
        "$image" --print-root-pin)
}

# Execute the verifier's read-only --print-root-pin; its stdout is the result.
# $1 is a private copy named duduclaw-bootstrap in its own directory.
isolation_probe_pin() {
    local binary="$1" arch="$2" host_arch host_os platform image
    host_arch="$(isolation_run "$ISOLATION_TOOL_UNAME" -m)" || return 1
    host_os="$(isolation_run "$ISOLATION_TOOL_UNAME" -s)" || return 1
    if [[ "$host_arch" == "arm64" ]]; then host_arch="aarch64"; fi
    if [[ "$host_os" == "Linux" && "$host_arch" == "$arch" ]]; then
        "$ISOLATION_TOOL_ENV" -i "$binary" --print-root-pin
        return
    fi
    case "$arch" in
        x86_64) platform="linux/amd64" ;;
        aarch64) platform="linux/arm64" ;;
        *) return 1 ;;
    esac
    if [[ "$ISOLATION_TOOL_DOCKER" != /* ]]; then
        isolation_fail "executing the $arch verifier here needs docker (or run on Linux $arch)"; return 1
    fi
    image="$(isolation_probe_image "$platform")" || return 1
    local -a ISOLATION_DOCKER_ARGV=()
    isolation_probe_docker_argv "$binary" "$platform" "$image" || return 1
    isolation_run "${ISOLATION_DOCKER_ARGV[@]}"
}

# --output-dir only RECEIVES copies at the very end; the ceremony works in its private staging
# directory. `check <dir>`: <dir> does not exist and its parent is an existing directory owned by
# you, not a symbolic link, not group/other-writable (else another user could rename the results).
# `copy <dir> <staging> <name>=<sha256> ...`: the same check on the opened parent, then <dir>
# created exclusively (never through a link) with mode 0700, and each <staging>/<name> copied
# into it with mode 0600; the staged bytes and the copy must both have <sha256>.
ISOLATION_OUTPUT_PY='import hashlib, os, stat, sys
mode, output = sys.argv[1], sys.argv[2]
uid = os.geteuid()
directory_flag = getattr(os, "O_DIRECTORY", 0)
def fail(message):
    sys.exit("Error: --output-dir " + output + ": " + message)
path = output.rstrip("/")
if not path or "\n" in path or any(part in (".", "..") for part in path.split("/")):
    fail("must name a new directory (no . or .. component)")
if not os.path.isabs(path):
    path = os.path.join(os.getcwd(), path)
parent, name = os.path.split(path)
def digest_of(descriptor):
    digest = hashlib.sha256()
    with os.fdopen(descriptor, "rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()
try:
    try:
        if stat.S_ISLNK(os.lstat(parent).st_mode):
            fail("its parent directory is a symbolic link")
        parent_fd = os.open(parent, os.O_RDONLY | directory_flag | os.O_NOFOLLOW)
    except FileNotFoundError:
        fail("its parent directory does not exist")
    info = os.fstat(parent_fd)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or info.st_mode & 0o022:
        fail("its parent must be an existing directory owned by you that is not group/other-writable")
    try:
        os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        pass
    else:
        fail("must not exist yet (outputs are never overwritten)")
    if mode == "check":
        sys.exit(0)
    if mode != "copy":
        fail("unknown mode")
    staging = sys.argv[3]
    os.mkdir(name, 0o700, dir_fd=parent_fd)
    out_fd = os.open(name, os.O_RDONLY | directory_flag | os.O_NOFOLLOW, dir_fd=parent_fd)
    info = os.fstat(out_fd)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid:
        fail("the created directory is not a directory of yours")
    os.fchmod(out_fd, 0o700)
    for argument in sys.argv[4:]:
        file, _, expected = argument.rpartition("=")
        if not file or "/" in file or file in (".", ".."):
            fail("unexpected file name " + file)
        source_fd = os.open(os.path.join(staging, file), os.O_RDONLY | os.O_NOFOLLOW)
        target_fd = os.open(file, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=out_fd)
        os.fchmod(target_fd, 0o600)
        staged = hashlib.sha256()
        with os.fdopen(source_fd, "rb") as source, os.fdopen(target_fd, "wb") as target:
            for chunk in iter(lambda: source.read(1 << 20), b""):
                staged.update(chunk)
                target.write(chunk)
            target.flush()
            os.fsync(target.fileno())
        if staged.hexdigest() != expected:
            fail("the staged " + file + " is not the uploaded/verified bytes")
        copy_fd = os.open(file, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=out_fd)
        info = os.fstat(copy_fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != uid or stat.S_IMODE(info.st_mode) != 0o600
                or digest_of(copy_fd) != expected):
            fail("the copy of " + file + " differs from the staged SHA-256")
except OSError as error:
    fail(str(error))'

# Before any prompt (and before --preflight ends): --output-dir is new and its parent qualifies.
isolation_check_output_dir() {
    if [[ -e "$1" || -L "$1" ]]; then
        isolation_fail "--output-dir must not exist yet (outputs are never overwritten)"; return 1
    fi
    isolation_run "$ISOLATION_TOOL_PYTHON" -I -B -c "$ISOLATION_OUTPUT_PY" check "$1"
}

# The convenience copy, LAST: $1 staging, $2 --output-dir, then <name>=<sha256> of each file
# (the uploaded/verified digests). The parent is checked again on the opened directory.
isolation_copy_results() {
    local staging="$1" output_dir="$2"
    shift 2
    if ! isolation_run "$ISOLATION_TOOL_PYTHON" -I -B -c "$ISOLATION_OUTPUT_PY" copy "$output_dir" "$staging" "$@"
    then
        isolation_fail "the results were NOT copied into --output-dir (above); nothing of the upload or its" \
            "read-back depends on that copy. Anything partially created there is not verified: remove it."
        return 1
    fi
}

# Immediately before the upload (with --no-upload: before the copy), every check again on the
# private staging copies that are hashed, uploaded and copied next, through the signer's own
# verification paths: --check-target --artifacts-dir (target-key signature; every artifact the
# manifest binds re-bound by the signing-time code: exactly the signed digest and size),
# --check-threshold --bundle (root threshold; the tar is the authorized one), the legacy minisign
# signature. $1 staging, $2 pin, $3 legacy minisign public key, $4 migration bundle (0/1).
isolation_verify_staged() {
    local staging="$1" pin="$2" legacy_public="$3" migration_bundle="$4" snapshot="$ISOLATION_SNAPSHOT"
    local root_metadata="$ISOLATION_WORK/root/root.json" arch bundle_name
    for arch in x86_64 aarch64; do
        if ! isolation_run "$ISOLATION_TOOL_PYTHON" -I -B "$snapshot/scripts/sign-isolation-manifest.py" \
                --kind release --check-target "$staging/isolated-linux-$arch-manifest.json" \
                --artifacts-dir "$staging" --root-metadata "$root_metadata" --root-sha256 "$pin" \
                --ssh-keygen "$ISOLATION_TOOL_SSH_KEYGEN"; then
            isolation_fail "re-verification before upload: the staged $arch manifest or an artifact it binds failed"
            return 1
        fi
        if [[ "$migration_bundle" == "1" ]]; then
            bundle_name="duduclaw-isolated-linux-$arch-$CURRENT_VERSION.tar"
            if ! isolation_run "$ISOLATION_TOOL_PYTHON" -I -B "$snapshot/scripts/sign-isolation-manifest.py" \
                    --kind bundle --check-threshold "$staging/$bundle_name.authorization.json" \
                    --bundle "$staging/$bundle_name" --root-metadata "$root_metadata" --root-sha256 "$pin" \
                    --ssh-keygen "$ISOLATION_TOOL_SSH_KEYGEN"; then
                isolation_fail "re-verification before upload: the staged $bundle_name authorization failed"
                return 1
            fi
            if ! isolation_run "$ISOLATION_TOOL_MINISIGN" -V -q -P "$legacy_public" -m "$staging/$bundle_name" \
                    -x "$staging/$bundle_name.minisig" \
                    || ! isolation_check_minisig "$staging/$bundle_name.minisig" \
                        "duduclaw v$CURRENT_VERSION $bundle_name"; then
                isolation_fail "re-verification before upload: the staged $bundle_name.minisig failed"
                return 1
            fi
        fi
    done
}

# STAGE 2 ceremony body (from isolation_stage2: snapshot audited, traps set, work
# directory created). Every file it reads or executes is "$snapshot/...".
run_isolation_sign() {
    local snapshot="$ISOLATION_SNAPSHOT" python="$ISOLATION_TOOL_PYTHON" ssh_keygen="$ISOLATION_TOOL_SSH_KEYGEN"
    local root_metadata="$ISOLATION_ROOT_METADATA" root_signatures="$ISOLATION_ROOT_SIGNATURES"
    local output_dir="$ISOLATION_OUTPUT_DIR" manifest_days="$ISOLATION_MANIFEST_DAYS"
    local authorization_days="$ISOLATION_AUTHORIZATION_DAYS" migration_bundle="$ISOLATION_BUNDLE_OPTION"
    local upload="$ISOLATION_UPLOAD" pin_file="$snapshot/$ISOLATION_PIN_FILE"
    ISOLATION_PROBE_DIGEST_FILE="$snapshot/$ISOLATION_PROBE_DIGEST_FILE"

    # 0. Preconditions again, from the snapshot (stage 1 is trusted with none of them).
    CURRENT_VERSION="$(isolation_read_version "$snapshot/Cargo.toml")" \
        || { isolation_fail "the snapshot Cargo.toml has no version"; return 1; }
    isolation_check_arguments "$CURRENT_VERSION" || return 1
    if [[ ! -t 0 ]]; then
        isolation_fail "isolation-sign must run attended in an interactive terminal (keys prompt on the tty)"
        return 1
    fi
    if [[ ! -f "$pin_file" || -L "$pin_file" ]]; then
        isolation_fail "$ISOLATION_PIN_FILE is absent: there is no production root pin, so no isolation" \
            "release can be signed (the key ceremony adds it through a reviewed PR)."
        return 1
    fi
    # R2-1: the snapshot is exactly tag v<version> of the original repository, and that
    # repository's HEAD is the tag; nothing else of the repository is used from here on.
    local tag="v$CURRENT_VERSION"
    isolation_check_repository "$tag" || return 1
    local pin
    pin="$(isolation_run "$python" -I -B -c 'import importlib.util, sys
spec = importlib.util.spec_from_file_location("packager", sys.argv[2])
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)
pin = packager.read_pin_file(sys.argv[1])
if pin is None:
    sys.exit("production pin file is absent")
print(pin)' "$pin_file" "$snapshot/scripts/package-linux-release.py")"
    if [[ ! -f "$root_metadata" || -L "$root_metadata" || ! -f "$root_signatures" || -L "$root_signatures" ]]; then
        isolation_fail "--root-metadata and --root-signatures must be regular files"; return 1
    fi
    # One private, read-only copy of each, made ONCE: only these copies are checked below and
    # only these copies are read by every later step (a file replaced at the path you gave,
    # after the check, is irrelevant).
    isolation_run "$ISOLATION_TOOL_MKDIR" -m 0700 -- "$ISOLATION_WORK/root" || return 1
    isolation_run "$ISOLATION_TOOL_CP" -- "$root_metadata" "$ISOLATION_WORK/root/root.json" || return 1
    isolation_run "$ISOLATION_TOOL_CP" -- "$root_signatures" "$ISOLATION_WORK/root/root.json.signatures.json" \
        || return 1
    isolation_run "$ISOLATION_TOOL_CHMOD" -R u=rX,go= -- "$ISOLATION_WORK/root" || return 1
    root_metadata="$ISOLATION_WORK/root/root.json"
    root_signatures="$ISOLATION_WORK/root/root.json.signatures.json"
    if [[ ! -f "$root_metadata" || -L "$root_metadata" || ! -f "$root_signatures" || -L "$root_signatures" ]]; then
        isolation_fail "cannot make the private copies of --root-metadata and --root-signatures"; return 1
    fi
    if [[ "$(isolation_sha256 "$root_metadata")" != "$pin" ]]; then
        isolation_fail "SHA-256 of --root-metadata differs from $ISOLATION_PIN_FILE"; return 1
    fi
    # The pinned root's own signatures: distinct registered root keys meeting its threshold,
    # each verified with ssh-keygen -Y verify over the exact bytes.
    if ! isolation_run "$python" -I -B "$snapshot/scripts/sign-isolation-manifest.py" --kind root \
            --check-threshold "$root_metadata" --signatures "$root_signatures" --root-sha256 "$pin" \
            --ssh-keygen "$ssh_keygen"; then
        isolation_fail "--root-signatures do not meet the root threshold of --root-metadata"; return 1
    fi
    local legacy_public="" line legacy_count=0
    local legacy_pattern='^pub const LEGACY_RELEASE_MINISIGN_KEY: &str = "([A-Za-z0-9+/=]*)";$'
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" =~ $legacy_pattern ]]; then
            legacy_public="${BASH_REMATCH[1]}"
            legacy_count=$((legacy_count + 1))
        fi
    done < "$snapshot/crates/duduclaw-isolation/src/update.rs"
    if [[ "$legacy_count" != "1" || ! "$legacy_public" =~ ^RW[A-Za-z0-9+/]{54}$ ]]; then
        isolation_fail "cannot read LEGACY_RELEASE_MINISIGN_KEY from duduclaw-isolation"; return 1
    fi
    # --output-dir only receives copies at the very end (step 7); new, and its parent an existing
    # directory of yours, not a link, not group/other-writable: here, before any prompt, and again
    # right before the copy.
    isolation_check_output_dir "$output_dir" || return 1
    isolation_require_tools || return 1
    if [[ "$ISOLATION_PREFLIGHT" == "1" ]]; then
        echo "Preflight passed for $tag: the snapshot is the tag, every tool resolved, --root-metadata matches" \
            "the pin and its signatures verify. Nothing was signed and no key location was asked for."
        return 0
    fi
    # R2-3: key locations only from the attended prompt, into unexported shell variables.
    local target_key="" legacy_minisign_key="" location index
    local -a root_keys=()
    target_key="$(isolation_key_location "target key handle")" || return 1
    if [[ "$migration_bundle" == "1" ]]; then
        for index in 1 2; do
            location="$(isolation_key_location "root key R$index")" || return 1
            root_keys+=("$location")
        done
        if [[ "${root_keys[0]}" == "${root_keys[1]}" ]]; then
            isolation_fail "--migration-bundle needs two distinct root keys (the 2-of-3 threshold)"; return 1
        fi
        legacy_minisign_key="$(isolation_key_location "legacy release minisign key" \
            "${HOME:+$HOME/.minisign/duduclaw-release.key}")" || return 1
    fi
    # Everything is produced, signed, verified, hashed and uploaded from this private staging
    # directory (inside the 0700 work directory the EXIT trap removes); deletions on failure
    # touch only it. Nothing before or during the upload reads --output-dir.
    local staging="$ISOLATION_WORK/staging"
    isolation_run "$ISOLATION_TOOL_MKDIR" -m 0700 -- "$staging"

    local arch triple archive bundle_name verifier probe_output expires asset now
    local -a uploads=()
    # Signing tools: the resolved ssh-keygen and, when its installation has one, its audited
    # ssh-sk-helper (the signer passes SSH_SK_HELPER to ssh-keygen -Y sign only, and refuses a
    # security key without one).
    local -a signing_tools=(--ssh-keygen "$ssh_keygen")
    if [[ -n "$ISOLATION_TOOL_SSH_SK_HELPER" ]]; then
        signing_tools+=(--ssh-sk-helper "$ISOLATION_TOOL_SSH_SK_HELPER")
    fi
    now="$(isolation_run "$ISOLATION_TOOL_DATE" +%s)"
    if [[ ! "$now" =~ ^[0-9]+$ ]]; then isolation_fail "cannot read the clock"; return 1; fi
    expires=$(( now + authorization_days * 86400 ))
    for arch in x86_64 aarch64; do
        if [[ "$arch" == "x86_64" ]]; then
            triple="x86_64-unknown-linux-gnu"; archive="duduclaw-linux-x64.tar.gz"
        else
            triple="aarch64-unknown-linux-gnu"; archive="duduclaw-linux-arm64.tar.gz"
        fi
        echo "== $arch: $archive of $tag"
        isolation_run "$ISOLATION_TOOL_MKDIR" -m 0700 -- "$ISOLATION_WORK/$arch" "$ISOLATION_WORK/$arch/probe"
        isolation_run "$ISOLATION_TOOL_GH" release download "$tag" --repo "$ISOLATION_REPO" \
            --dir "$ISOLATION_WORK/$arch" --pattern "$archive" --pattern "$archive.sha256" --pattern "$archive.minisig"
        isolation_run "$ISOLATION_TOOL_MINISIGN" -V -q -P "$legacy_public" -m "$ISOLATION_WORK/$arch/$archive" \
            -x "$ISOLATION_WORK/$arch/$archive.minisig"
        if ! isolation_check_minisig "$ISOLATION_WORK/$arch/$archive.minisig" "duduclaw $tag $archive"; then
            isolation_fail "$archive.minisig is not the release job's prehashed signature for $tag"; return 1
        fi
        # 1. The released verifier must be a PRODUCTION build of exactly this pin.
        isolation_run "$python" -I -B "$snapshot/scripts/package-linux-release.py" --target "$triple" \
            --extract-bootstrap "$ISOLATION_WORK/$arch/$archive" --output-dir "$ISOLATION_WORK/$arch/verifier"
        verifier="$ISOLATION_WORK/$arch/verifier/duduclaw-bootstrap-linux-$arch"
        isolation_run "$ISOLATION_TOOL_CP" -- "$verifier" "$ISOLATION_WORK/$arch/probe/duduclaw-bootstrap"
        isolation_run "$ISOLATION_TOOL_CHMOD" 0755 "$ISOLATION_WORK/$arch/probe/duduclaw-bootstrap"
        probe_output="$(isolation_probe_pin "$ISOLATION_WORK/$arch/probe/duduclaw-bootstrap" "$arch")"
        if [[ "$probe_output" != "PRODUCTION $pin" ]]; then
            # The probe output itself is not echoed; only the expectation is named.
            isolation_fail "the released $arch duduclaw-bootstrap does not print PRODUCTION <$ISOLATION_PIN_FILE>"
            return 1
        fi
        # 2. Target key: isolation manifest over the binaries of this released archive.
        # The signer refuses (and writes nothing) unless the signature verifies against the
        # target_key of the pinned root (the private copy): a wrong but valid key stops here.
        local -a package_args=(--target "$triple" --from-release-archive "$ISOLATION_WORK/$arch/$archive"
            --manifest-template "$snapshot/$ISOLATION_TEMPLATE" --manifest-valid-days "$manifest_days"
            --signing-key-stdin "${signing_tools[@]}" --target-root "$root_metadata" --target-root-sha256 "$pin"
            --output-dir "$staging")
        if [[ "$migration_bundle" == "1" ]]; then
            package_args+=(--bootstrap-root "$root_metadata" --bootstrap-root-signatures "$root_signatures")
        fi
        echo "Target key ($arch manifest): touch the key and enter its PIN when ssh-keygen asks."
        # The target key location travels only on stdin (package-linux-release.py passes it on
        # to sign-isolation-manifest.py's stdin, which links it for ssh-keygen).
        isolation_handoff 1 "$target_key" "$python" -I -B "$snapshot/scripts/package-linux-release.py" \
            "${package_args[@]}"
        # Again on the written bytes, before anything of this architecture joins the upload list:
        # shape check, then ssh-keygen -Y verify against the pinned target_key only. Otherwise
        # every output of this architecture is deleted (in the private staging directory only).
        if ! isolation_run "$python" -I -B "$snapshot/scripts/sign-isolation-manifest.py" --kind release \
                --check-target "$staging/isolated-linux-$arch-manifest.json" --root-metadata "$root_metadata" \
                --root-sha256 "$pin" --ssh-keygen "$ssh_keygen"; then
            isolation_run "$ISOLATION_TOOL_RM" -rf -- "$staging/isolated-linux-$arch-"*
            isolation_fail "the $arch manifest is not signed by the target key of the pinned root; deleted"
            return 1
        fi
        for asset in "$staging/isolated-linux-$arch-"*; do
            if [[ -f "$asset" && ! -L "$asset" ]]; then uploads+=("$asset"); fi
        done
        if [[ "$migration_bundle" == "1" ]]; then
            # 3. Plain-ustar migration bundle (plan §3.2).
            isolation_run "$python" -I -B "$snapshot/scripts/package-linux-release.py" --target "$triple" \
                --migration-bundle-from "$staging/isolated-linux-$arch-bootstrap-inbox" --output-dir "$staging"
            bundle_name="duduclaw-isolated-linux-$arch-$CURRENT_VERSION.tar"
            local authorization="$staging/$bundle_name.authorization.json"
            # 4. Transition dual signature: root threshold + legacy release key. R1 then R2
            # on the signer's stdin (never argv or the environment).
            echo "Root threshold ($arch bundle): R1 touch + PIN, then the R2 passphrase."
            isolation_handoff 2 "${root_keys[0]}" "${root_keys[1]}" "$python" -I -B \
                "$snapshot/scripts/sign-isolation-manifest.py" --kind bundle --bundle "$staging/$bundle_name" \
                --expires-at-unix "$expires" --output "$authorization" --key-count 2 \
                --root-metadata "$root_metadata" --root-sha256 "$pin" "${signing_tools[@]}"
            # R2-5, before anything of this bundle reaches the upload list: the signers are
            # the keys embedded in the signatures, distinct, registered in the pinned root,
            # not revoked, meeting its threshold, every signature verified by ssh-keygen
            # -Y verify over the written bytes. Otherwise the authorization is deleted.
            if ! isolation_run "$python" -I -B "$snapshot/scripts/sign-isolation-manifest.py" --kind bundle \
                    --check-threshold "$authorization" --root-metadata "$root_metadata" --root-sha256 "$pin" \
                    --ssh-keygen "$ssh_keygen"; then
                isolation_run "$ISOLATION_TOOL_RM" -f -- "$authorization" "$authorization.signatures.json"
                isolation_fail "the $arch BundleAuthorization signatures do not meet the root threshold; deleted"
                return 1
            fi
            echo "Legacy release key ($arch bundle): minisign asks for its password."
            isolation_handoff 1 "$legacy_minisign_key" "$python" -I -B "$snapshot/scripts/sign-isolation-manifest.py" \
                --minisign-sign "$staging/$bundle_name" --trusted-comment "duduclaw v$CURRENT_VERSION $bundle_name" \
                --output "$staging/$bundle_name.minisig" --minisign "$ISOLATION_TOOL_MINISIGN"
            isolation_run "$ISOLATION_TOOL_MINISIGN" -V -q -P "$legacy_public" -m "$staging/$bundle_name" \
                -x "$staging/$bundle_name.minisig"
            if ! isolation_check_minisig "$staging/$bundle_name.minisig" "duduclaw v$CURRENT_VERSION $bundle_name"; then
                isolation_fail "$bundle_name.minisig is not a prehashed signature with the verifier's trusted comment"
                return 1
            fi
            isolation_run "$ISOLATION_TOOL_CP" -- "$verifier" "$staging/duduclaw-bootstrap-linux-$arch"
            isolation_run "$ISOLATION_TOOL_CHMOD" 0755 "$staging/duduclaw-bootstrap-linux-$arch"
            uploads+=("$staging/duduclaw-bootstrap-linux-$arch" "$staging/$bundle_name"
                "$staging/$bundle_name.minisig" "$staging/$bundle_name.authorization.json"
                "$staging/$bundle_name.authorization.json.signatures.json")
        fi
    done

    local sums_file="$staging/SHA256SUMS-isolation.txt" index name sums_hash
    local -a hashes=()
    local -a copies=()
    echo ""
    echo "Assets for GitHub release $tag:"
    for index in "${!uploads[@]}"; do
        printf '  %s\n' "${uploads[$index]##*/}"
    done
    if [[ "$upload" == "1" ]]; then
        local answer=""
        read -r -p "Type UPLOAD to publish these ${#uploads[@]} assets to $tag: " answer
        if [[ "$answer" != "UPLOAD" ]]; then
            echo "Not uploaded; nothing was copied to --output-dir (the staging copies are removed on exit)."
            return 1
        fi
    fi
    # 5. Immediately before the upload: every check again on the staging copies, then the
    # publication list from those same bytes (+ root fingerprint).
    isolation_verify_staged "$staging" "$pin" "$legacy_public" "$migration_bundle" || return 1
    for index in "${!uploads[@]}"; do
        hashes+=("$(isolation_sha256 "${uploads[$index]}")")
    done
    {
        for index in "${!uploads[@]}"; do
            printf '%s  %s\n' "${hashes[$index]}" "${uploads[$index]##*/}"
        done
        printf '%s  %s\n' "$pin" "root.json"
    } > "$sums_file"
    if [[ "$upload" == "0" ]]; then
        echo "--no-upload: nothing uploaded."
    else
        # 6. Upload exactly the verified, recorded bytes, never replacing an asset, then read back.
        for index in "${!uploads[@]}"; do
            if [[ "$(isolation_sha256 "${uploads[$index]}")" != "${hashes[$index]}" ]]; then
                isolation_fail "${uploads[$index]##*/} changed after it was recorded"; return 1
            fi
        done
        isolation_run "$ISOLATION_TOOL_GH" release upload "$tag" --repo "$ISOLATION_REPO" "${uploads[@]}"
        isolation_run "$ISOLATION_TOOL_MKDIR" -m 0700 -- "$ISOLATION_WORK/readback"
        for index in "${!uploads[@]}"; do
            name="${uploads[$index]##*/}"
            isolation_run "$ISOLATION_TOOL_GH" release download "$tag" --repo "$ISOLATION_REPO" \
                --dir "$ISOLATION_WORK/readback" --pattern "$name"
            if [[ "$(isolation_sha256 "$ISOLATION_WORK/readback/$name")" != "${hashes[$index]}" ]]; then
                isolation_fail "uploaded $name differs from the signed bytes; investigate before publishing"; return 1
            fi
        done
        echo "Uploaded and read back ${#uploads[@]} assets."
    fi
    sums_hash="$(isolation_sha256 "$sums_file")"
    echo ""
    echo "SHA256SUMS-isolation.txt (sha256 $sums_hash):"
    while IFS= read -r line || [[ -n "$line" ]]; do printf '%s\n' "$line"; done < "$sums_file"
    echo "[external gate] Publish this list on the website AND the licensed console only after"
    echo "owner confirmation; both must show identical values (design §8 step 1)."
    # 7. Only now (after upload + read-back, or the final verification with --no-upload): a
    # convenience copy into --output-dir, each file checked against the staged/uploaded digest.
    for index in "${!uploads[@]}"; do
        copies+=("${uploads[$index]##*/}=${hashes[$index]}")
    done
    copies+=("SHA256SUMS-isolation.txt=$sums_hash")
    isolation_copy_results "$staging" "$output_dir" "${copies[@]}" || return 1
    echo "Copied ${#copies[@]} files into $output_dir (directory 0700, files 0600, SHA-256 checked)."
}

# --- Arg parsing / sub-commands ---
# isolation-sign is dispatched before anything else runs (CURRENT_VERSION below would
# already execute grep/sed): stage 2 when started by stage 1 (marker variables present; it
# validates them and refuses otherwise), stage 1 everywhere else (which refuses unless
# scripts/isolation-sign started it).
if [[ "${1:-}" == "isolation-sign" ]]; then
    shift
    if [[ -n "${DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT+x}" || -n "${DUDUCLAW_ISOLATION_CEREMONY_REPO+x}" ]]; then
        isolation_stage2 "$@"
    else
        isolation_stage1 "$@"
    fi
    exit $?
fi

# Enterprise builds use this default; the offline ceremony must not export it
# before checking the launcher's environment allow-list.
export CLOUD_BUILD="${CLOUD_BUILD:-1}"

if [[ $# -lt 1 ]]; then
    echo "Usage:"
    echo "  $0 <hotfix|minor|major> [--title \"<theme>\"] [--dry-run]"
    echo "                                       bump + sync every platform manifest"
    echo "  $0 audit                             show each platform's version + drift"
    echo "  $0 verify [version]                  confirm PyPI/npm published <version>"
    echo "  $0 linux-assets --target <triple>     build/package all native Linux binaries"
    echo "  scripts/isolation-sign --help         OFFLINE: sign/upload the Linux isolation release"
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
    echo "          scripts/isolation-sign --help (owner present; migration bundle only for: $ISOLATION_MIGRATION_BUNDLE_VERSIONS)."
    echo "[DRY RUN] Pro image CLOUD_BUILD=$CLOUD_BUILD; no signing/build/install/commit/tag/publish performed."
    echo "[DRY RUN] Confirm after publishing: $0 verify $NEW_VERSION"
fi
python3 "$(dirname "$0")/release_platform_ci.py" --lane "$BUMP_TYPE"
if $DRY_RUN; then
    exit 0
fi

# --- TEST-licence build settings (before any source mutation) ---
# This host builds the darwin duduclaw-pro (cargo, below) and starts the Pro image
# build/push: refuse test-root rustc flags, the TEST issuer/key variables, rustc
# wrappers/overrides and Cargo configuration naming the cfg here, for the repository
# and the private Pro crate (scripts/refuse-test-license-build.py --check-environment).
REFUSER_CARGO_DIRS=(--cargo-dir .)
if [[ -d commercial/duduclaw-pro-gateway ]]; then
    REFUSER_CARGO_DIRS+=(--cargo-dir commercial/duduclaw-pro-gateway)
fi
if ! python3 -I -B "$(dirname "$0")/refuse-test-license-build.py" --check-environment "${REFUSER_CARGO_DIRS[@]}"; then
    echo "Error: TEST-licence build settings in this environment (above); unset them before releasing."
    exit 1
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
    # Hard failure, before any signature, notarisation or upload: never a
    # TEST-licence gateway (static marker check of the exact staged bytes).
    python3 -I -B scripts/refuse-test-license-build.py "$staged_dir/$plat/duduclaw-pro" || exit 1
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
