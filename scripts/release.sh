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
        npm)
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

# --- Enterprise pro-image (commercial checkout only) ---
# The duduclaw-pro image is versioned by THIS release train (same gateway,
# license-gated modules), so building it belongs to the normal release flow:
# the cloud console's version dropdown lists GitHub releases filtered by
# which duduclaw-pro:<tag> images actually exist in the private registry —
# skipping this step means the new version never appears in the dropdown.
# No-op on public checkouts (script absent). Opt out: DUDUCLAW_SKIP_PRO_IMAGE=1.
PRO_IMAGE_SCRIPT="commercial/duduclaw-pro-gateway/build-image.sh"
if [[ -x "$PRO_IMAGE_SCRIPT" && "${DUDUCLAW_SKIP_PRO_IMAGE:-0}" != "1" ]]; then
    echo ""
    echo "Building + pushing enterprise duduclaw-pro:v$NEW_VERSION image..."
    if "$PRO_IMAGE_SCRIPT" "v$NEW_VERSION"; then
        echo "  Enterprise image v$NEW_VERSION pushed."
    else
        echo ""
        echo "  WARNING: duduclaw-pro image build/push FAILED. The release commit +"
        echo "  tag stand, but the cloud console will not offer v$NEW_VERSION until"
        echo "  you re-run:  $PRO_IMAGE_SCRIPT v$NEW_VERSION"
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
echo "     -> the tag push triggers .github/workflows/release.yml, which builds"
echo "        binaries and AUTO-PUBLISHES GitHub Release + npm + PyPI (all 3)."
echo "  4. After CI finishes, CONFIRM every registry actually got it:"
echo "       ./scripts/release.sh verify $NEW_VERSION"
echo "     (this catches a PyPI/npm 'skip-existing' silent miss)"
echo "     The cloud console's enterprise version dropdown picks the new"
echo "     version up automatically once the GitHub Release exists AND the"
echo "     duduclaw-pro:v$NEW_VERSION image is in the registry (built above)."
echo ""
