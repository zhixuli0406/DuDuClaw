#!/usr/bin/env bash
set -euo pipefail

# Publish DuDuClaw CE to npm
# Usage: ./publish-ce.sh <version> [--dry-run]
#
# Prerequisites:
#   - GitHub release artifacts downloaded to ./artifacts/
#   - npm logged in with publish access to @duduclaw scope
#
# Example:
#   VERSION=1.3.24
#   mkdir -p artifacts && cd artifacts
#   gh release download "v${VERSION}" --repo zhixuli0406/DuDuClaw
#   cd ..
#   ./npm/scripts/publish-ce.sh "$VERSION"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NPM_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
VERSION="${1:?Usage: publish-ce.sh <version> [--dry-run]}"
DRY_RUN="${2:-}"

ARTIFACTS_DIR="${ARTIFACTS_DIR:-./artifacts}"

# Mapping: artifact tarball name → npm platform directory
declare -A PLATFORM_MAP=(
  ["duduclaw-darwin-arm64"]="darwin-arm64"
  ["duduclaw-darwin-x64"]="darwin-x64"
  ["duduclaw-linux-x64"]="linux-x64"
  ["duduclaw-linux-arm64"]="linux-arm64"
)

echo "=== DuDuClaw CE npm publish v${VERSION} ==="

# Step 1: Update version in all package.json files
echo "--- Updating versions ---"
for dir in duduclaw darwin-arm64 darwin-x64 linux-x64 linux-arm64 win32-x64; do
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

# Step 2: Extract binaries into platform packages (Unix tarballs)
echo "--- Extracting binaries ---"
for artifact in "${!PLATFORM_MAP[@]}"; do
  platform_dir="${PLATFORM_MAP[$artifact]}"
  tarball="${ARTIFACTS_DIR}/${artifact}.tar.gz"

  if [ ! -f "$tarball" ]; then
    echo "  SKIP: ${tarball} not found"
    continue
  fi

  bin_dir="${NPM_DIR}/${platform_dir}/bin"
  mkdir -p "$bin_dir"

  tar xzf "$tarball" -C "$bin_dir" --strip-components=0 duduclaw
  chmod +x "${bin_dir}/duduclaw"
  echo "  Extracted ${artifact} → ${platform_dir}/bin/duduclaw"
done

# Step 2b: Extract Windows binary from zip
WIN_ZIP="${ARTIFACTS_DIR}/duduclaw-windows-x64.zip"
if [ -f "$WIN_ZIP" ]; then
  win_bin_dir="${NPM_DIR}/win32-x64/bin"
  mkdir -p "$win_bin_dir"
  unzip -o -j "$WIN_ZIP" "duduclaw.exe" -d "$win_bin_dir" 2>/dev/null || \
    python3 -c "import zipfile,sys; zipfile.ZipFile('${WIN_ZIP}').extract('duduclaw.exe','${win_bin_dir}')" 2>/dev/null || \
    echo "  WARNING: could not extract Windows zip"
  if [ -f "${win_bin_dir}/duduclaw.exe" ]; then
    echo "  Extracted duduclaw-windows-x64 → win32-x64/bin/duduclaw.exe"
  fi
else
  echo "  SKIP: ${WIN_ZIP} not found"
fi

# Step 3: Publish platform packages first, then main package
echo "--- Publishing platform packages ---"
for dir in darwin-arm64 darwin-x64 linux-x64 linux-arm64; do
  pkg_dir="${NPM_DIR}/${dir}"
  if [ ! -f "${pkg_dir}/bin/duduclaw" ]; then
    echo "  SKIP: ${dir} (no binary)"
    continue
  fi
  echo "  Publishing @duduclaw/${dir}..."
  npm publish "$pkg_dir" --access public ${DRY_RUN}
done

# Publish Windows package
if [ -f "${NPM_DIR}/win32-x64/bin/duduclaw.exe" ]; then
  echo "  Publishing @duduclaw/win32-x64..."
  npm publish "${NPM_DIR}/win32-x64" --access public ${DRY_RUN}
else
  echo "  SKIP: win32-x64 (no binary)"
fi

echo "--- Publishing main package ---"
npm publish "${NPM_DIR}/duduclaw" --access public ${DRY_RUN}

echo "=== Done ==="
echo "Install: npm install -g duduclaw"
echo "Run:     npx duduclaw version"
