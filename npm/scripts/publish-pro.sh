#!/usr/bin/env bash
set -euo pipefail

# Publish DuDuClaw Pro to npm
# Usage: ./publish-pro.sh <version> [--dry-run]
#
# Prerequisites:
#   - Pro release artifacts downloaded to ./artifacts/
#   - npm logged in with publish access to @duduclaw scope
#
# Example:
#   VERSION=1.4.4
#   mkdir -p artifacts && cd artifacts
#   gh release download "v${VERSION}" --repo zhixuli0406/duduclaw-pro-releases
#   cd ..
#   ./npm/scripts/publish-pro.sh "$VERSION"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NPM_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
VERSION="${1:?Usage: publish-pro.sh <version> [--dry-run]}"
DRY_RUN="${2:-}"

ARTIFACTS_DIR="${ARTIFACTS_DIR:-./artifacts}"

# Mapping: artifact name → npm platform directory → binary name in tarball
declare -A PLATFORM_MAP=(
  ["duduclaw-pro-aarch64-apple-darwin"]="pro-darwin-arm64"
  ["duduclaw-pro-x86_64-apple-darwin"]="pro-darwin-x64"
)

# Binary names inside the Pro tarballs
declare -A BINARY_NAMES=(
  ["duduclaw-pro-aarch64-apple-darwin"]="duduclaw-pro-aarch64-apple-darwin"
  ["duduclaw-pro-x86_64-apple-darwin"]="duduclaw-pro-x86_64-apple-darwin"
)

echo "=== DuDuClaw Pro npm publish v${VERSION} ==="

# Step 1: Update version in all package.json files
echo "--- Updating versions ---"
for dir in duduclaw-pro pro-darwin-arm64 pro-darwin-x64; do
  pkg_json="${NPM_DIR}/${dir}/package.json"
  node -e "
    const fs = require('fs');
    const pkg = JSON.parse(fs.readFileSync('${pkg_json}', 'utf8'));
    pkg.version = '${VERSION}';
    if (pkg.optionalDependencies) {
      for (const key of Object.keys(pkg.optionalDependencies)) {
        pkg.optionalDependencies[key] = '${VERSION}';
      }
    }
    fs.writeFileSync('${pkg_json}', JSON.stringify(pkg, null, 2) + '\n');
  "
  echo "  Updated ${dir}/package.json → ${VERSION}"
done

# Step 2: Extract binaries into platform packages
echo "--- Extracting binaries ---"
for artifact in "${!PLATFORM_MAP[@]}"; do
  platform_dir="${PLATFORM_MAP[$artifact]}"
  binary_name="${BINARY_NAMES[$artifact]}"
  tarball="${ARTIFACTS_DIR}/${artifact}.tar.gz"

  if [ ! -f "$tarball" ]; then
    echo "  SKIP: ${tarball} not found"
    continue
  fi

  bin_dir="${NPM_DIR}/${platform_dir}/bin"
  mkdir -p "$bin_dir"

  # Extract and rename to duduclaw-pro
  tar xzf "$tarball" -C "/tmp/_duduclaw_extract_$$"  2>/dev/null || {
    mkdir -p "/tmp/_duduclaw_extract_$$"
    tar xzf "$tarball" -C "/tmp/_duduclaw_extract_$$"
  }

  # Find the binary (may be named duduclaw-pro-<arch> or duduclaw-pro)
  found_bin=$(find "/tmp/_duduclaw_extract_$$" -name "duduclaw-pro*" -type f ! -name "*.sha256" | head -1)
  if [ -n "$found_bin" ]; then
    cp "$found_bin" "${bin_dir}/duduclaw-pro"
    chmod +x "${bin_dir}/duduclaw-pro"
    echo "  Extracted ${artifact} → ${platform_dir}/bin/duduclaw-pro"
  else
    echo "  WARNING: no binary found in ${tarball}"
  fi

  rm -rf "/tmp/_duduclaw_extract_$$"
done

# Step 3: Publish platform packages first, then main package
echo "--- Publishing platform packages ---"
for dir in pro-darwin-arm64 pro-darwin-x64; do
  pkg_dir="${NPM_DIR}/${dir}"
  if [ ! -f "${pkg_dir}/bin/duduclaw-pro" ]; then
    echo "  SKIP: ${dir} (no binary)"
    continue
  fi
  echo "  Publishing @duduclaw/${dir}..."
  npm publish "$pkg_dir" --access public ${DRY_RUN}
done

echo "--- Publishing main package ---"
npm publish "${NPM_DIR}/duduclaw-pro" --access public ${DRY_RUN}

echo "=== Done ==="
echo "Install: npm install -g duduclaw-pro"
echo "Run:     npx duduclaw-pro version"
