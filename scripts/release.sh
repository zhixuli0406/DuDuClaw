#!/usr/bin/env bash
# DuDuClaw Release Automation
# Usage:
#   ./scripts/release.sh <patch|minor|major> [--title "<theme>"] [--dry-run]
#                                                          # bump + sync all platforms
#   ./scripts/release.sh audit                              # show every platform's version + drift
#   ./scripts/release.sh verify [version]                  # confirm registries published <version>
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
# Steps (bump mode):
#   1. Validate working tree is clean
#   2. Pre-flight audit: print every platform's current version + flag drift
#   3. Bump version in all manifests (Cargo / crates / npm / pyproject / READMEs)
#   4. POST-BUMP ASSERT: every platform manifest now reads the new version, else abort
#   5. Update CHANGELOG.md
#   6. cargo check
#   7. git commit + tag
#   8. Print next steps + the registry-verify command
set -euo pipefail

# --- Config ---
WORKSPACE_TOML="Cargo.toml"
PYPI_PKG="duduclaw"   # PyPI + npm package name (used by `verify`)
NPM_PKG="duduclaw"
# Homebrew tap: unmaintained (frozen at 1.50.0). published_tap_version() below
# still reads it for the `audit` drift row — no more `homebrew` sync subcommand.
TAP_REPO="zhixuli0406/homebrew-tap"
TAP_FORMULA="Formula/duduclaw.rb"
DRY_RUN=false

# Extended-regex semver matcher (no leading anchor — reused in several patterns).
SEMVER='[0-9]+\.[0-9]+\.[0-9]+'

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
    for r in README.md README.en.md README.ja.md; do
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

# --- Homebrew tap: read the formula version actually published on the tap. ---
# The tap lives in a SEPARATE repo, so it is not covered by platform_manifests
# (which enumerates files in this checkout). Prefer the GitHub API (no cache) —
# the raw.githubusercontent fallback has a ~5 min CDN cache, which would show a
# stale MISMATCH right after a successful push. Empty string on network failure.
published_tap_version() {
    local body=""
    if command -v gh >/dev/null 2>&1; then
        body="$(gh api "repos/$TAP_REPO/contents/$TAP_FORMULA" --jq '.content' 2>/dev/null \
            | base64 -d 2>/dev/null)" || true
    fi
    if [[ -z "$body" ]]; then
        body="$(curl -fsSL "https://raw.githubusercontent.com/$TAP_REPO/main/$TAP_FORMULA" 2>/dev/null)" || true
    fi
    { printf '%s\n' "$body" \
        | grep -m1 -E "^[[:space:]]*version \"$SEMVER\"" \
        | sed -E "s/.*version \"($SEMVER)\".*/\1/"; } || true
}

# --- Audit: print each platform's current version, flagging drift vs Cargo. ---
# Returns non-zero if any manifest disagrees with the Cargo workspace version.
run_audit() {
    local truth="$1" drift=0 kind file v flag brew_v
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
    # Homebrew tap (remote repo — audited from the published formula, not a local
    # file). This is how the tap froze at 1.8.8 for ~35 releases: it was updated
    # by hand, never enumerated anywhere, so nothing ever flagged it.
    brew_v="$(published_tap_version)"
    flag=""
    if [[ -z "$brew_v" ]]; then
        brew_v="?"
        flag="   (tap unreachable — not checked)"
    elif [[ "$brew_v" != "$truth" ]]; then
        flag="   <-- DRIFT (brew installs $brew_v; tap unmaintained, not fixed here)"
        drift=1
    fi
    printf "  %-34s %-10s [%s]%s\n" "tap:$TAP_REPO" "$brew_v" "brew" "$flag"
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
        else
            echo "  pro-image / oem-tar: skipped (no GCP project configured)"
        fi
    fi

    echo "------------------------------------------------------------------"
    if [[ $rc -ne 0 ]]; then
        echo "One or more registries are behind. The CI release.yml jobs for those"
        echo "platforms either skipped (stale manifest + skip-existing) or lack"
        echo "credentials (PYPI_TRUSTED_PUBLISHER / PYPI_TOKEN / NPM_TOKEN)."
    fi
    return $rc
}

# --- Arg parsing / sub-commands ---
if [[ $# -lt 1 ]]; then
    echo "Usage:"
    echo "  $0 <patch|minor|major> [--title \"<theme>\"] [--dry-run]"
    echo "                                       bump + sync every platform manifest"
    echo "  $0 audit                             show each platform's version + drift"
    echo "  $0 verify [version]                  confirm PyPI/npm published <version>"
    exit 1
fi

# Read current version up-front (needed by all sub-commands).
CURRENT_VERSION="$(extract_version "$WORKSPACE_TOML" cargo)"
if [[ -z "$CURRENT_VERSION" ]]; then
    echo "Error: could not read version from $WORKSPACE_TOML"
    exit 1
fi

case "$1" in
    audit)
        run_audit "$CURRENT_VERSION" || {
            echo ""
            echo "DRIFT DETECTED: a manifest is behind the Cargo version. The next"
            echo "'$0 <patch|minor|major>' run re-syncs every manifest to the new version."
            echo "(Homebrew tap drift is expected and no longer fixed by this script —"
            echo " the tap is unmaintained and frozen at 1.50.0; direct users to npm"
            echo " or the desktop app instead of a tap sync.)"
        }
        exit 0
        ;;
    verify)
        run_verify "${2:-$CURRENT_VERSION}"
        exit $?
        ;;
esac

BUMP_TYPE="$1"
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

if [[ "$BUMP_TYPE" != "patch" && "$BUMP_TYPE" != "minor" && "$BUMP_TYPE" != "major" ]]; then
    echo "Error: bump type must be 'patch', 'minor', 'major', or sub-command 'audit'/'verify'"
    exit 1
fi

# Check working tree is clean
if ! git diff --quiet HEAD 2>/dev/null; then
    echo "Error: working tree has uncommitted changes"
    echo "Please commit or stash your changes first."
    exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"
case "$BUMP_TYPE" in
    patch) NEW_VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    minor) NEW_VERSION="$MAJOR.$((MINOR + 1)).0" ;;
    major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
esac

echo "Version: $CURRENT_VERSION -> $NEW_VERSION"
echo ""

# --- Pre-flight platform audit (always shown; surfaces pre-existing drift) ---
run_audit "$CURRENT_VERSION" || echo "(drift above will be re-synced to $NEW_VERSION below)"
echo ""

if $DRY_RUN; then
    echo "[DRY RUN] Would bump every manifest above to $NEW_VERSION, update CHANGELOG,"
    echo "          cargo check, then commit + tag v$NEW_VERSION."
    echo "[DRY RUN] After tag push, CI release.yml publishes GitHub + npm + PyPI."
    echo "[DRY RUN] Confirm with: $0 verify $NEW_VERSION"
    exit 0
fi

# --- Bump every platform manifest (rewrites ANY semver, so drift is corrected) ---
echo "Bumping all platform manifests to $NEW_VERSION..."
while IFS='|' read -r kind file; do
    case "$kind" in
        cargo|pyproject)
            sed -i '' -E "s/^version = \"$SEMVER\"/version = \"$NEW_VERSION\"/" "$file"
            ;;
        pyinit)
            sed -i '' -E "s/^([[:space:]]*)__version__ = \"$SEMVER\"/\1__version__ = \"$NEW_VERSION\"/" "$file"
            ;;
        npm)
            # "version": "x.y.z" plus any "@duduclaw/<plat>": "x.y.z" dep refs
            sed -i '' -E "s/(\"version\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            sed -i '' -E "s/(\"@duduclaw\/[a-z0-9-]+\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$file"
            ;;
        tauri)
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
    git checkout -- .
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
    git checkout -- .
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
        git checkout -- .
        exit 1
    fi
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
STAGE_FILES+=("CHANGELOG.md")
git add -- "${STAGE_FILES[@]}"
git commit -m "chore: bump v$NEW_VERSION"
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
if [[ -x "$PRO_IMAGE_SCRIPT" && "${DUDUCLAW_SKIP_PRO_IMAGE:-0}" != "1" ]]; then
    echo ""
    echo "Building + pushing enterprise duduclaw-pro:v$NEW_VERSION image..."
    if "$PRO_IMAGE_SCRIPT" "v$NEW_VERSION"; then
        echo "  Enterprise image v$NEW_VERSION pushed."
        PRO_IMAGE_PUSHED=1
    else
        echo ""
        echo "  WARNING: duduclaw-pro image build/push FAILED. The release commit +"
        echo "  tag stand, but the cloud console will not offer v$NEW_VERSION until"
        echo "  you re-run:  $PRO_IMAGE_SCRIPT v$NEW_VERSION"
    fi
fi

# --- Enterprise pro binary assets (darwin + linux-x64, commercial checkout only) ---
# Bare-metal duduclaw-pro binaries for the P0 control-plane auto-update
# channel (commercial/docs/DESIGN-pro-auto-update-2026-08.md §3 P0, §4 item
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

    if ! ( cd "$staged_dir/$plat" && tar czf "$tar_path" duduclaw-pro ); then
        echo "  WARNING: failed to package $tar_name — skipping."
        return 1
    fi
    if ! shasum -a 256 "$tar_path" > "${tar_path}.sha256"; then
        echo "  WARNING: sha256 failed for $tar_name — skipping."
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
    if gcloud storage cp "$tar_path" "${tar_path}.sha256" "${tar_path}.minisig" "$dest"; then
        echo "  Uploaded: ${dest}${tar_name} (+ .sha256 + .minisig)"
        return 0
    fi
    echo "  WARNING: GCS upload failed for $tar_name."
    return 1
}

if [[ -d "commercial/duduclaw-pro-gateway" ]]; then
    if [[ "${DUDUCLAW_SKIP_PRO_BIN:-0}" == "1" ]]; then
        echo ""
        echo "Skipping duduclaw-pro binary assets (DUDUCLAW_SKIP_PRO_BIN=1)."
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
            echo "  [$DARWIN_PLATFORM_LABEL] cargo build --release --bin duduclaw-pro..."
            if ( cd commercial/duduclaw-pro-gateway && cargo build --release --bin duduclaw-pro ); then
                mkdir -p "$PRO_BIN_TMP/$DARWIN_PLATFORM_LABEL"
                if cp target/release/duduclaw-pro "$PRO_BIN_TMP/$DARWIN_PLATFORM_LABEL/duduclaw-pro"; then
                    if package_and_upload_pro_bin "$DARWIN_PLATFORM_LABEL" "$PRO_BIN_TMP" "$NEW_VERSION"; then
                        PRO_BIN_UPLOADED=1
                    fi
                else
                    echo "  WARNING: built binary missing at target/release/duduclaw-pro — skipping $DARWIN_PLATFORM_LABEL."
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
                PRO_BIN_CID="$(docker create "$PRO_BIN_IMAGE" 2>/dev/null || true)"
                if [[ -n "$PRO_BIN_CID" ]]; then
                    mkdir -p "$PRO_BIN_TMP/linux-x64"
                    if docker cp "$PRO_BIN_CID:/usr/local/bin/duduclaw-pro" "$PRO_BIN_TMP/linux-x64/duduclaw-pro" 2>/dev/null; then
                        if package_and_upload_pro_bin linux-x64 "$PRO_BIN_TMP" "$NEW_VERSION"; then
                            PRO_BIN_UPLOADED=1
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

    # --- Control-plane offered-version allowlist (DUDUCLAW_PRO_VERSIONS) ---
    # Auto-discovery (GitHub releases × AR manifest probe) proved unreliable
    # in production (2026-08-17: silently all-false manifest probes degraded
    # the offered list to ["latest"], blanking the Pro update channel and the
    # console version dropdown). The operator allowlist — path ① in
    # pro_versions::resolve, highest precedence — is therefore maintained
    # per-release HERE as the authoritative source; discovery + the registry
    # fallback stay behind it as the safety net. Newest-first, top 3 release
    # tags from git (desktop-v* excluded); first entry doubles as the console
    # packaging default pin. Best-effort like every step above — a failure
    # WARNs with the manual command, never fails the release.
    # Opt out: DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1.
    if [[ "${DUDUCLAW_SKIP_PRO_VERSIONS_ENV:-0}" == "1" ]]; then
        echo ""
        echo "Skipping control-plane DUDUCLAW_PRO_VERSIONS sync (DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1)."
    elif [[ "$PRO_IMAGE_PUSHED" == "1" || "$PRO_BIN_UPLOADED" == "1" ]]; then
        CP_SERVICE="${DUDUCLAW_CP_SERVICE:-duduclaw-control-plane}"
        CP_REGION="${DUDUCLAW_GCP_REGION:-asia-east1}"
        CP_PROJECT="${DUDUCLAW_GCP_PROJECT:-$(gcloud config get-value project 2>/dev/null || true)}"
        # Newest three vX.Y.Z tags — the tag for THIS release already exists
        # at this point in the flow, so it is the natural first entry.
        CP_VERSIONS="$(git tag --list 'v*' --sort=-v:refname \
            | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -3 | paste -sd, -)"
        if ! command -v gcloud >/dev/null 2>&1 || [[ -z "$CP_PROJECT" || "$CP_PROJECT" == "(unset)" ]]; then
            echo ""
            echo "  WARNING: gcloud/project unavailable — control-plane version allowlist NOT synced."
            echo "           Run manually:  gcloud run services update $CP_SERVICE --region $CP_REGION \\"
            echo "                            --update-env-vars '^:^DUDUCLAW_PRO_VERSIONS=${CP_VERSIONS:-v$NEW_VERSION}'"
        elif [[ -z "$CP_VERSIONS" ]]; then
            echo ""
            echo "  WARNING: could not derive a version list from git tags — allowlist NOT synced."
        else
            echo ""
            echo "Syncing control-plane version allowlist: DUDUCLAW_PRO_VERSIONS=$CP_VERSIONS"
            # ^:^ switches the pair delimiter to ':' so the comma-separated
            # version list survives as ONE value (a bare comma would split it
            # into bogus KEY=VALUE pairs and fail the update).
            if gcloud run services update "$CP_SERVICE" --project "$CP_PROJECT" --region "$CP_REGION" \
                --update-env-vars "^:^DUDUCLAW_PRO_VERSIONS=${CP_VERSIONS}" >/dev/null 2>&1; then
                echo "  Control-plane allowlist updated (new revision rolled out)."
            else
                echo "  WARNING: control-plane env update FAILED. Run manually:"
                echo "    gcloud run services update $CP_SERVICE --region $CP_REGION \\"
                echo "      --update-env-vars '^:^DUDUCLAW_PRO_VERSIONS=$CP_VERSIONS'"
            fi
        fi
    fi
fi

echo ""
echo "================================================"
echo " Release v$NEW_VERSION prepared successfully!"
echo " All platforms synchronized: Cargo / pyproject (PyPI) / npm / READMEs"
echo "================================================"
echo ""
echo "Next steps:"
echo "  1. Review CHANGELOG.md release notes (curated [Unreleased] was renamed)"
echo "  2. Amend the commit if needed:  git commit --amend"
echo "  3. Push to remote:              git push && git push --tags"
echo "     -> v$NEW_VERSION triggers release.yml (binaries + GitHub Release + npm + PyPI)"
echo "     -> desktop-v$NEW_VERSION triggers desktop-release.yml (Tauri installers,"
echo "        signed + notarized, published directly — no draft)"
echo "  4. CONFIRM every registry actually got it:"
echo "       ./scripts/release.sh verify $NEW_VERSION"
echo "     (this catches a PyPI/npm 'skip-existing' silent miss)"
echo "     The cloud console's enterprise version dropdown picks the new"
echo "     version up automatically once the GitHub Release exists AND the"
echo "     duduclaw-pro:v$NEW_VERSION image is in the registry (built above)."
echo ""
