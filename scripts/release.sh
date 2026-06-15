#!/usr/bin/env bash
# DuDuClaw Release Automation
# Usage: ./scripts/release.sh <patch|minor|major> [--dry-run]
#
# Steps:
#   1. Validate working tree is clean
#   2. Bump version in all Cargo.toml files
#   3. Bump the version badge in all localized READMEs (zh-TW / en / ja)
#      + remind to refresh & translate the release highlight in each
#   4. Update CHANGELOG.md with new entry
#   5. Build and run tests
#   6. Create git commit + tag
#   7. Print next steps (push, GitHub release)
set -euo pipefail

# --- Config ---
WORKSPACE_TOML="Cargo.toml"
HOMEBREW_FORMULA="Formula/duduclaw.rb"  # if exists
DRY_RUN=false

# --- Parse args ---
if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <patch|minor|major> [--dry-run]"
    echo ""
    echo "Examples:"
    echo "  $0 patch          # 0.12.0 -> 0.9.8"
    echo "  $0 minor          # 0.12.0 -> 0.10.0"
    echo "  $0 major          # 0.12.0 -> 1.0.0"
    echo "  $0 patch --dry-run # preview changes without writing"
    exit 1
fi

BUMP_TYPE="$1"
if [[ "${2:-}" == "--dry-run" ]]; then
    DRY_RUN=true
    echo "[DRY RUN] No files will be modified"
fi

# --- Validate ---
if [[ "$BUMP_TYPE" != "patch" && "$BUMP_TYPE" != "minor" && "$BUMP_TYPE" != "major" ]]; then
    echo "Error: bump type must be 'patch', 'minor', or 'major'"
    exit 1
fi

# Check working tree is clean
if ! git diff --quiet HEAD 2>/dev/null; then
    echo "Error: working tree has uncommitted changes"
    echo "Please commit or stash your changes first."
    exit 1
fi

# --- Read current version ---
CURRENT_VERSION=$(grep '^version = ' "$WORKSPACE_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/')
if [[ -z "$CURRENT_VERSION" ]]; then
    echo "Error: could not read version from $WORKSPACE_TOML"
    exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"

# --- Calculate new version ---
case "$BUMP_TYPE" in
    patch) NEW_VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    minor) NEW_VERSION="$MAJOR.$((MINOR + 1)).0" ;;
    major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
esac

echo "Version: $CURRENT_VERSION -> $NEW_VERSION"
echo ""

if $DRY_RUN; then
    echo "[DRY RUN] Would update the following files:"
    echo "  - Cargo.toml (workspace.package.version)"
    echo "  - All crate Cargo.toml files (via cargo)"
    echo "  - CHANGELOG.md (new entry)"
    echo "  - Git commit: 'chore: bump v$NEW_VERSION'"
    echo "  - Git tag: 'v$NEW_VERSION'"
    exit 0
fi

# --- Bump version in workspace Cargo.toml ---
echo "Bumping version in Cargo.toml files..."
sed -i '' "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" "$WORKSPACE_TOML"

# Bump in individual crate Cargo.toml files that reference workspace version
for crate_toml in crates/*/Cargo.toml; do
    # Update direct version fields (not workspace inherited ones)
    if grep -q "^version = \"$CURRENT_VERSION\"" "$crate_toml" 2>/dev/null; then
        sed -i '' "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" "$crate_toml"
        echo "  Updated: $crate_toml"
    fi
done

# --- Sync sibling package manifests (npm + Python) ---
# These ship to npm / PyPI and MUST track the Cargo version, otherwise the
# published packages drift (e.g. the v1.17.0 vs npm 1.17.1 mismatch). They may
# legitimately sit at a different patch level than Cargo, so rewrite every
# version-shaped field rather than matching $CURRENT_VERSION exactly.
echo "Syncing npm + pyproject versions to $NEW_VERSION..."
# Extended-regex (sed -E) semver matcher.
SEMVER='[0-9]+\.[0-9]+\.[0-9]+'
for pkg in npm/*/package.json; do
    [[ -f "$pkg" ]] || continue
    # "version": "x.y.z" plus any "@duduclaw/<plat>": "x.y.z" optionalDependency refs
    sed -i '' -E "s/(\"version\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$pkg"
    sed -i '' -E "s/(\"@duduclaw\/[a-z0-9-]+\"[[:space:]]*:[[:space:]]*\")$SEMVER(\")/\1$NEW_VERSION\2/" "$pkg"
    echo "  Updated: $pkg"
done
if [[ -f "pyproject.toml" ]]; then
    sed -i '' -E "s/^version = \"$SEMVER\"/version = \"$NEW_VERSION\"/" pyproject.toml
    echo "  Updated: pyproject.toml"
fi

# --- Bump README version badges (all languages) ---
# The shields.io badge is mechanical and lives in every localized README
# (README.md / README.en.md / README.ja.md). The human-readable release
# highlight at the top of each still needs a manual edit per language (see the
# reminder at the end).
echo "Bumping README version badges (zh-TW / en / ja)..."
for readme in README.md README.en.md README.ja.md; do
    [[ -f "$readme" ]] || continue
    sed -i '' -E "s|(badge/version-)$SEMVER(-blue)|\1$NEW_VERSION\2|" "$readme"
    echo "  Updated: $readme"
done

# --- Update CHANGELOG.md ---
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
    # Insert after the first line (header)
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
echo "  5. Create GitHub Release:       gh release create v$NEW_VERSION --generate-notes"
echo "  6. Build release binaries:      ./scripts/build-release.sh $NEW_VERSION"
echo ""
