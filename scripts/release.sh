#!/usr/bin/env bash
# DuDuClaw Release Automation
# Usage:
#   ./scripts/release.sh <patch|minor|major> [--dry-run]   # bump + sync all platforms
#   ./scripts/release.sh audit                              # show every platform's version + drift
#   ./scripts/release.sh verify [version]                  # confirm registries published <version>
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
    echo "  $0 <patch|minor|major> [--dry-run]   bump + sync every platform manifest"
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
if [[ "${2:-}" == "--dry-run" ]]; then
    DRY_RUN=true
    echo "[DRY RUN] No files will be modified"
fi

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
CHANGELOG_ENTRY="## [$NEW_VERSION] - $DATE

### Added
- (describe new features here)

### Changed
- (describe changes here)

### Fixed
- (describe bug fixes here)

"

if [[ -f "CHANGELOG.md" ]]; then
    TEMP=$(mktemp)
    head -2 CHANGELOG.md > "$TEMP"
    echo "" >> "$TEMP"
    echo "$CHANGELOG_ENTRY" >> "$TEMP"
    tail -n +3 CHANGELOG.md >> "$TEMP"
    mv "$TEMP" CHANGELOG.md
else
    cat > CHANGELOG.md << HEREDOC
# Changelog

All notable changes to DuDuClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

$CHANGELOG_ENTRY## [$CURRENT_VERSION] - $DATE

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

# --- Git commit + tag ---
echo ""
echo "Creating git commit and tag..."
git add -A Cargo.toml crates/*/Cargo.toml npm/*/package.json pyproject.toml README.md README.en.md README.ja.md CHANGELOG.md
git commit -m "chore: bump v$NEW_VERSION"
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"

echo ""
echo "================================================"
echo " Release v$NEW_VERSION prepared successfully!"
echo " All platforms synchronized: Cargo / pyproject (PyPI) / npm / READMEs"
echo "================================================"
echo ""
echo "Next steps:"
echo "  1. Edit CHANGELOG.md to fill in release notes"
echo "  2. Update the release highlight in ALL THREE localized READMEs"
echo "     (README.md / README.en.md / README.ja.md) — keep them aligned:"
echo "       - Replace the top release highlight block with v$NEW_VERSION"
echo "       - Demote the previous highlight into the history <details>"
echo "       - Translate the new highlight into en + ja (zh-TW is the source)"
echo "       (version badges were bumped automatically; the prose is manual)"
echo "  3. Amend the commit if needed:  git commit --amend"
echo "  4. Push to remote:              git push && git push --tags"
echo "     -> the tag push triggers .github/workflows/release.yml, which builds"
echo "        binaries and AUTO-PUBLISHES GitHub Release + npm + PyPI (all 3)."
echo "  5. After CI finishes, CONFIRM every registry actually got it:"
echo "       ./scripts/release.sh verify $NEW_VERSION"
echo "     (this catches a PyPI/npm 'skip-existing' silent miss)"
echo ""
