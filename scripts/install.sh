#!/usr/bin/env bash
# DuDuClaw Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
# Or:    curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh -s -- --yes
set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
# Version is auto-detected from the latest GitHub Release at runtime so this
# script (served from `main`) never drifts behind published binaries. Override
# with `DUDUCLAW_VERSION=1.2.3 ...` to pin. FALLBACK is used only when the
# GitHub API is unreachable. NOTE: do NOT hardcode a version that lacks a
# published Release — a 404 here silently forces an hour-long source build.
DUDUCLAW_VERSION="${DUDUCLAW_VERSION:-}"
FALLBACK_VERSION="1.81.0"
GITHUB_REPO="zhixuli0406/DuDuClaw"
INSTALL_DIR="${DUDUCLAW_HOME:-$HOME/.duduclaw}/bin"
BINARY_NAME="duduclaw"
MIN_PYTHON_MAJOR=3
MIN_PYTHON_MINOR=10
AUTO_YES=false
# Release signing key — must equal duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY
# (the key the self-updater pins). Never read from the environment.
RELEASE_PUBKEY="RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh"
# Designated requirement of DuDuClaw Gateway.app — must equal
# duduclaw_core::macos_bundle::PINNED_DR (identifier + Developer ID + team).
GATEWAY_DR='identifier "ai.duduclaw.gateway" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = "7469HYQ6HH"'

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------
if [ -t 1 ] && command -v tput >/dev/null 2>&1; then
  GREEN=$(tput setaf 2)
  RED=$(tput setaf 1)
  YELLOW=$(tput setaf 3)
  CYAN=$(tput setaf 6)
  BOLD=$(tput bold)
  RESET=$(tput sgr0)
else
  GREEN=""
  RED=""
  YELLOW=""
  CYAN=""
  BOLD=""
  RESET=""
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
info()    { echo "${GREEN}✓${RESET} $*"; }
warn()    { echo "${YELLOW}⚠${RESET} $*"; }
error()   { echo "${RED}✗${RESET} $*" >&2; }
fatal()   { error "$@"; exit 1; }
heading() { echo; echo "${BOLD}${CYAN}$*${RESET}"; }

confirm() {
  if [ "$AUTO_YES" = true ]; then
    return 0
  fi
  printf "%s [y/N] " "$1"
  read -r answer
  case "$answer" in
    [Yy]*) return 0 ;;
    *) return 1 ;;
  esac
}

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
for arg in "$@"; do
  case "$arg" in
    --yes|-y) AUTO_YES=true ;;
    --help|-h)
      echo "Usage: install.sh [--yes]"
      echo "  --yes, -y    Skip confirmation prompts"
      echo ""
      echo "Requires minisign (https://jedisct1.github.io/minisign/) to verify the"
      echo "release signature; e.g. 'brew install minisign' or 'apt install minisign'."
      echo ""
      echo "Environment variables:"
      echo "  DUDUCLAW_HOME    Override install directory (default: ~/.duduclaw)"
      echo "  DUDUCLAW_APP_DIR Override macOS app directory (default: ~/Applications)"
      exit 0
      ;;
    *)
      warn "Unknown option: $arg"
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Detect OS and Architecture
# ---------------------------------------------------------------------------
detect_platform() {
  local os arch target

  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)  os="linux"  ;;
    Darwin) os="darwin" ;;
    MINGW*|MSYS*|CYGWIN*)
      fatal "Windows detected. Please use the PowerShell installer instead:\n  irm https://raw.githubusercontent.com/${GITHUB_REPO}/main/scripts/install.ps1 | iex"
      ;;
    *)
      fatal "Unsupported operating system: $os"
      ;;
  esac

  case "$arch" in
    x86_64|amd64)   arch="x64"   ;;
    arm64|aarch64)   arch="arm64" ;;
    *)
      fatal "Unsupported architecture: $arch"
      ;;
  esac

  TARGET="${BINARY_NAME}-${os}-${arch}"
  echo "$TARGET"
}

# ---------------------------------------------------------------------------
# Download helpers
# ---------------------------------------------------------------------------
has_cmd() { command -v "$1" >/dev/null 2>&1; }

# Resolve DUDUCLAW_VERSION from the latest GitHub Release (unless pinned).
resolve_version() {
  if [ -n "$DUDUCLAW_VERSION" ]; then
    info "Using pinned version v${DUDUCLAW_VERSION}"
    return
  fi

  local api="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
  local tag=""
  if has_cmd curl; then
    tag="$(curl -fsSL "$api" 2>/dev/null | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v?([^"]+)".*/\1/')"
  elif has_cmd wget; then
    tag="$(wget -qO- "$api" 2>/dev/null | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v?([^"]+)".*/\1/')"
  fi

  # "latest" can be claimed by a desktop installer release (desktop-v1.46.2)
  # — map it to the paired CLI version, and reject any tag that still doesn't
  # look like MAJOR.MINOR.PATCH so we never build a bogus download URL.
  tag="${tag#desktop-}"
  tag="${tag#v}"
  case "$tag" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) tag="" ;;
  esac

  if [ -n "$tag" ]; then
    DUDUCLAW_VERSION="$tag"
  else
    DUDUCLAW_VERSION="$FALLBACK_VERSION"
    warn "Could not query latest release; falling back to v${DUDUCLAW_VERSION}"
  fi
}

download() {
  local url="$1" dest="$2"
  if has_cmd curl; then
    curl -fsSL --retry 3 --retry-delay 2 -o "$dest" "$url"
  elif has_cmd wget; then
    wget -q -O "$dest" "$url"
  else
    fatal "Neither curl nor wget found. Please install one and retry."
  fi
}

# Verify a downloaded release archive BEFORE anything reads its contents:
# the minisign signature against the pinned RELEASE_PUBKEY (the trust anchor —
# the .sha256 sidecar is published by the same release-write authority, so it
# alone proves nothing), the signed trusted comment naming exactly this
# version and file, and our own SHA-256 against the single digest the sidecar
# lists for exactly this file name. Pure POSIX shell + minisign.
#   verify_release_archive ARCHIVE FILE_NAME VERSION SHA256_SIDECAR MINISIG
verify_release_archive() (
  archive="$1"
  name="$2"
  version="$3"
  sums="$4"
  sig="$5"
  case "$version" in
    ''|*[!0-9.]*|.*|*.|*..*) fatal "Invalid release version: $version" ;;
  esac
  case "$version" in
    *.*.*.*) fatal "Invalid release version: $version" ;;
    *.*.*) ;;
    *) fatal "Invalid release version: $version" ;;
  esac
  case "$name" in
    ''|*[!A-Za-z0-9._-]*) fatal "Invalid release file name: $name" ;;
  esac
  has_cmd minisign || fatal "minisign is required to verify the DuDuClaw release signature and was not found. Install it (macOS: brew install minisign; Debian/Ubuntu: sudo apt install minisign; other: https://jedisct1.github.io/minisign/) and re-run, or install with: npm install -g duduclaw"

  # Exactly one line "<64 lowercase hex>  <name>" (optionally "*<name>").
  expected="$(LC_ALL=C awk -v want="$name" '
    NR == 1 && NF == 2 && length($1) == 64 && $1 !~ /[^0-9a-f]/ && ($2 == want || $2 == "*" want) { digest = $1; next }
    { bad = 1 }
    END { if (bad || NR != 1 || digest == "") exit 1; print digest }
  ' "$sums")" || fatal "Checksum verification failed: ${name}.sha256 must list exactly one digest for ${name}"
  if has_cmd shasum; then
    actual="$(shasum -a 256 "$archive" | awk '{ print $1 }')" || fatal "Cannot hash $name"
  elif has_cmd sha256sum; then
    actual="$(sha256sum "$archive" | awk '{ print $1 }')" || fatal "Cannot hash $name"
  else
    fatal "No SHA-256 tool found (shasum or sha256sum)"
  fi
  [ -n "$actual" ] && [ "$actual" = "$expected" ] || fatal "Checksum verification failed for $name"

  # Prehashed (ED) signature only, and the trusted comment must be exactly
  # the one CI signs: "duduclaw v<version> <file>". minisign -V checks the
  # global signature over that comment, so it cannot be edited.
  LC_ALL=C awk -v want="trusted comment: duduclaw v${version} ${name}" '
    NR == 1 && index($0, "untrusted comment: ") == 1 { next }
    NR == 2 && $0 ~ /^RU[QRST][A-Za-z0-9+\/]*=*$/ { next }
    NR == 3 && $0 == want { next }
    NR == 4 && $0 ~ /^[A-Za-z0-9+\/]+=*$/ { next }
    { bad = 1 }
    END { if (bad || NR != 4) exit 1 }
  ' "$sig" || fatal "Release signature for $name is malformed or names another version/file"
  minisign -V -q -P "$RELEASE_PUBKEY" -m "$archive" -x "$sig" >/dev/null 2>&1 \
    || fatal "Release signature verification FAILED for $name — refusing to install"
)

# Validate the bundle's zip listing BEFORE extraction (nothing is written
# until this passes). Reads `zipinfo -1` (exact names) and `zipinfo -s`
# (unix modes) — the same Info-ZIP parser `unzip` extracts with, so the
# listing cannot disagree with what is extracted. Refuses: links, special
# files, setuid/setgid/sticky, group/world-writable modes, non-unix entries,
# absolute / `..` / empty components, names outside the bundle or with
# unusual characters, duplicates and case-insensitive collisions, a file used
# as a directory, any executable other than Contents/MacOS/duduclaw.
#   check_bundle_listing NAMES_FILE LONG_LISTING_FILE
check_bundle_listing() {
  LC_ALL=C awk -v exe="DuDuClaw Gateway.app/Contents/MacOS/duduclaw" '
    FNR == NR { names[++n] = $0; next }
    length($1) == 10 && $2 ~ /^[0-9]+[.][0-9]+$/ {
      i = ++m
      name = names[i]
      mode = $1
      if (i > n || $3 != "unx" || substr($0, length($0) - length(name)) != " " name) bad = 1
      dir = (name ~ /\/$/)
      if (dir) { if (mode !~ /^d/) bad = 1 } else if (mode !~ /^-/) bad = 1
      if (mode !~ /^[-d][-r][-w][-x][-r]-[-x][-r]-[-x]$/) bad = 1
      if ($4 !~ /^[0-9]+$/) bad = 1
      total += $4
      if (name !~ /^DuDuClaw Gateway[.]app\// || name ~ /[^A-Za-z0-9 ._+\/-]/) bad = 1
      parts = split(name, part, "/")
      for (j = 1; j <= parts; j++) {
        if (part[j] == ".." || part[j] == "." || (part[j] == "" && !(dir && j == parts))) bad = 1
      }
      key = tolower(name)
      sub(/\/$/, "", key)
      if (seen[key]++) bad = 1
      keys[i] = key
      if (!dir) file[key] = 1
      if (name ~ /^DuDuClaw Gateway[.]app\/Contents\/MacOS\/./ && name != exe) bad = 1
      if (name == exe) { exes++; if (mode !~ /^-..x/) bad = 1 }
      else if (mode ~ /x/ && !dir) bad = 1
      if (name == "DuDuClaw Gateway.app/Contents/Info.plist") plist++
      next
    }
    END {
      if (bad || n == 0 || m != n || n > 4096 || total > 1073741824 || exes != 1 || plist != 1) exit 1
      for (i = 1; i <= m; i++) {
        parts = split(keys[i], part, "/")
        prefix = part[1]
        for (j = 2; j <= parts; j++) {
          if (prefix in file) exit 1
          prefix = prefix "/" part[j]
        }
      }
    }
  ' "$1" "$2"
}

# Install a verified bundle using only shell and built-in macOS tools.
#   install_macos_bundle ARCHIVE APP_DIR BIN_DIR VERSION
# ARCHIVE must already have passed verify_release_archive.
install_macos_bundle() (
  archive="$1"
  app_dir="$2"
  bin_dir="$3"
  version="$4"
  app_name='DuDuClaw Gateway.app'
  [ -n "$version" ] || fatal "Gateway bundle version is required"
  umask 022
  mkdir -p "$app_dir" "$bin_dir" || fatal "Cannot create install directories"
  app_dir="$(CDPATH= cd "$app_dir" && pwd -P)" || fatal "Cannot resolve app directory"
  bin_dir="$(CDPATH= cd "$bin_dir" && pwd -P)" || fatal "Cannot resolve CLI directory"
  # Fresh private (0700) staging directory on the destination volume.
  stage="$(mktemp -d "$app_dir/.duduclaw-install.XXXXXX")" || fatal "Cannot stage gateway bundle"
  link_stage=""
  trap 'rm -rf "$stage"; if [ -n "$link_stage" ]; then rm -rf "$link_stage"; fi' EXIT

  /usr/bin/zipinfo -1 "$archive" > "$stage/names" || fatal "Cannot list gateway bundle"
  /usr/bin/zipinfo -s "$archive" > "$stage/listing" || fatal "Cannot list gateway bundle"
  check_bundle_listing "$stage/names" "$stage/listing" || fatal "Unsafe gateway bundle listing"
  rm "$stage/names" "$stage/listing"
  mkdir "$stage/x" || fatal "Cannot stage gateway bundle"
  /usr/bin/unzip -qq -n "$archive" -d "$stage/x" || fatal "Cannot extract gateway bundle"
  # Defence in depth only — the listing check above is the boundary.
  find "$stage/x" ! -type f ! -type d > "$stage/special" || fatal "Cannot inspect gateway bundle"
  [ ! -s "$stage/special" ] || fatal "Gateway bundle contains links or special files"
  rm "$stage/special"
  source="$stage/x/$app_name"
  destination="$app_dir/$app_name"
  [ -d "$source" ] && [ ! -L "$source" ] || fatal "Gateway bundle missing from archive"
  [ -f "$source/Contents/MacOS/duduclaw" ] && [ -x "$source/Contents/MacOS/duduclaw" ] || fatal "Gateway executable missing or not executable"

  # Same identity the self-updater pins: Developer ID, team 7469HYQ6HH,
  # identifier ai.duduclaw.gateway, the signed release version, CE edition.
  [ -x /usr/bin/codesign ] && [ -x /usr/bin/plutil ] || fatal "codesign and plutil are required to verify the gateway bundle"
  /usr/bin/codesign --verify --strict --deep -R "=$GATEWAY_DR" "$source" || fatal "Gateway bundle signature verification failed"
  plist="$source/Contents/Info.plist"
  [ "$(/usr/bin/plutil -extract CFBundleIdentifier raw -o - -- "$plist" 2>/dev/null)" = "ai.duduclaw.gateway" ] \
    || fatal "Gateway bundle identifier mismatch"
  [ "$(/usr/bin/plutil -extract CFBundleShortVersionString raw -o - -- "$plist" 2>/dev/null)" = "$version" ] \
    || fatal "Gateway bundle version mismatch (expected $version)"
  [ "$(/usr/bin/plutil -extract CFBundleExecutable raw -o - -- "$plist" 2>/dev/null)" = "duduclaw" ] \
    || fatal "Gateway bundle edition mismatch (expected duduclaw)"
  "$source/Contents/MacOS/duduclaw" version < /dev/null > "$stage/version" 2>/dev/null || fatal "Gateway executable failed to run"
  LC_ALL=C awk -v want="$version" '
    index($0, "duduclaw ") == 1 { count++; if (substr($0, 10) != want) bad = 1 }
    END { if (bad || count != 1) exit 1 }
  ' "$stage/version" || fatal "Gateway executable version mismatch (expected duduclaw $version)"
  rm "$stage/version"

  # Reserve temporary names on each destination volume before changing the app.
  link_stage="$(mktemp -d "$bin_dir/.duduclaw-link.XXXXXX")" || fatal "Cannot stage CLI symlink"
  ln -s "$destination/Contents/MacOS/duduclaw" "$link_stage/duduclaw" || fatal "Cannot create CLI symlink"
  [ ! -d "$bin_dir/duduclaw" ] || fatal "CLI destination is a directory"
  backup="$(mktemp -d "$app_dir/.duduclaw-backup.XXXXXX")" || fatal "Cannot reserve gateway backup"
  existing=false
  if [ -e "$destination" ] || [ -L "$destination" ]; then
    if ! mv "$destination" "$backup/$app_name"; then
      rmdir "$backup"
      fatal "Cannot back up existing gateway"
    fi
    existing=true
  fi
  if ! mv "$source" "$destination"; then
    if [ "$existing" = true ]; then
      mv "$backup/$app_name" "$destination" || fatal "Cannot restore gateway; backup retained at $backup"
    fi
    rmdir "$backup"
    fatal "Cannot install gateway bundle"
  fi
  if ! mv -f "$link_stage/duduclaw" "$bin_dir/duduclaw"; then
    mv "$destination" "$source" || fatal "Cannot roll back gateway; backup retained at $backup"
    if [ "$existing" = true ]; then
      mv "$backup/$app_name" "$destination" || fatal "Cannot restore gateway; backup retained at $backup"
    fi
    rmdir "$backup"
    fatal "Cannot install CLI symlink"
  fi
  rm -rf "$backup"
)

# ---------------------------------------------------------------------------
# Install binary from GitHub release
# ---------------------------------------------------------------------------
install_from_release() {
  local target="$1"
  local archive_ext="tar.gz"
  local release_url="https://github.com/${GITHUB_REPO}/releases/download/v${DUDUCLAW_VERSION}/${target}.${archive_ext}"
  local tmp_dir archive_name

  # Fail before downloading anything when the signature cannot be checked.
  has_cmd minisign || fatal "minisign is required to verify the DuDuClaw release signature and was not found. Install it (macOS: brew install minisign; Debian/Ubuntu: sudo apt install minisign; other: https://jedisct1.github.io/minisign/) and re-run, or install with: npm install -g duduclaw"

  tmp_dir="$(mktemp -d)"
  INSTALL_TMP="$tmp_dir"
  trap 'rm -rf "$INSTALL_TMP"' EXIT

  heading "Downloading DuDuClaw v${DUDUCLAW_VERSION} (${target})..."
  echo "  URL: ${release_url}"

  if [[ "$target" == duduclaw-darwin-* ]]; then
    local bundle_url="${release_url%.tar.gz}.app.zip" status
    has_cmd curl || fatal "curl is required to download the macOS gateway bundle"
    status="$(curl -sSL --retry 3 --retry-delay 2 -w '%{http_code}' -o "${tmp_dir}/${target}.app.zip" "$bundle_url")" || fatal "Gateway bundle download failed"
    case "$status" in
      200) archive_ext="app.zip"; release_url="$bundle_url" ;;
      404) warn "Gateway bundle unavailable for this release; using legacy tarball" ;;
      *) fatal "Gateway bundle download failed (HTTP $status)" ;;
    esac
  fi
  archive_name="${target}.${archive_ext}"
  if [ "$archive_ext" != "app.zip" ]; then
    if ! download "$release_url" "${tmp_dir}/${archive_name}" 2>/dev/null; then
      return 1
    fi
  fi

  # Signature + checksum are required for every archive; any failure is fatal
  # and happens before the archive is listed or extracted.
  heading "Verifying release signature..."
  download "${release_url}.sha256" "${tmp_dir}/${archive_name}.sha256" 2>/dev/null \
    || fatal "Release checksum ${archive_name}.sha256 is unavailable"
  download "${release_url}.minisig" "${tmp_dir}/${archive_name}.minisig" 2>/dev/null \
    || fatal "Release signature ${archive_name}.minisig is unavailable"
  verify_release_archive "${tmp_dir}/${archive_name}" "$archive_name" "$DUDUCLAW_VERSION" \
    "${tmp_dir}/${archive_name}.sha256" "${tmp_dir}/${archive_name}.minisig" \
    || fatal "Release verification failed for ${archive_name}"
  info "Signature and checksum verified"

  if [ "$archive_ext" = "app.zip" ]; then
    local app_dir="${DUDUCLAW_APP_DIR:-$HOME/Applications}"
    install_macos_bundle "${tmp_dir}/${target}.app.zip" "$app_dir" "$INSTALL_DIR" "$DUDUCLAW_VERSION" || fatal "Gateway bundle installation failed"
    info "Installed to $app_dir/DuDuClaw Gateway.app (CLI: $INSTALL_DIR/duduclaw)"
    return 0
  fi

  # Extract
  heading "Installing..."
  mkdir -p "$INSTALL_DIR"
  tar xzf "${tmp_dir}/${target}.${archive_ext}" -C "$tmp_dir"
  mv "${tmp_dir}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
  chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

  info "Installed to ${INSTALL_DIR}/${BINARY_NAME}"
  return 0
}

# ---------------------------------------------------------------------------
# Build from source via cargo
# ---------------------------------------------------------------------------
install_from_source() {
  heading "Building from source with cargo..."

  if ! has_cmd cargo; then
    error "cargo is not installed."
    echo ""
    echo "  Install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo ""
    fatal "Cannot build from source without cargo."
  fi

  info "Found cargo: $(cargo --version)"

  mkdir -p "$INSTALL_DIR"

  cargo install \
    --git "https://github.com/${GITHUB_REPO}.git" \
    --tag "v${DUDUCLAW_VERSION}" \
    --root "${DUDUCLAW_HOME:-$HOME/.duduclaw}" \
    --locked \
    duduclaw-cli 2>&1 || {
      # If tagged version doesn't exist yet, try main branch
      warn "Tagged release v${DUDUCLAW_VERSION} not found, building from main branch..."
      cargo install \
        --git "https://github.com/${GITHUB_REPO}.git" \
        --branch main \
        --root "${DUDUCLAW_HOME:-$HOME/.duduclaw}" \
        duduclaw-cli 2>&1 || fatal "Failed to build from source."
    }

  info "Built and installed to ${INSTALL_DIR}/${BINARY_NAME}"
}

# ---------------------------------------------------------------------------
# Add to PATH
# ---------------------------------------------------------------------------
add_to_path() {
  local path_line="export PATH=\"${INSTALL_DIR}:\$PATH\""
  local duduclaw_home_line=""

  if [ -n "${DUDUCLAW_HOME:-}" ]; then
    duduclaw_home_line="export DUDUCLAW_HOME=\"${DUDUCLAW_HOME}\""
  fi

  # Check if already in PATH
  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*)
      info "Already in PATH"
      return
      ;;
  esac

  heading "Adding to PATH..."

  local shell_configs=()
  [ -f "$HOME/.bashrc" ]  && shell_configs+=("$HOME/.bashrc")
  [ -f "$HOME/.zshrc" ]   && shell_configs+=("$HOME/.zshrc")
  [ -f "$HOME/.profile" ] && shell_configs+=("$HOME/.profile")

  # If no config files exist, create .profile
  if [ ${#shell_configs[@]} -eq 0 ]; then
    shell_configs+=("$HOME/.profile")
    touch "$HOME/.profile"
  fi

  for rc in "${shell_configs[@]}"; do
    if ! grep -q "${INSTALL_DIR}" "$rc" 2>/dev/null; then
      {
        echo ""
        echo "# DuDuClaw"
        [ -n "$duduclaw_home_line" ] && echo "$duduclaw_home_line"
        echo "$path_line"
      } >> "$rc"
      info "Updated ${rc}"
    fi
  done

  export PATH="${INSTALL_DIR}:${PATH}"
}

# ---------------------------------------------------------------------------
# Check optional dependencies
# ---------------------------------------------------------------------------
check_python() {
  heading "Checking Python..."

  local py_cmd=""
  for cmd in python3 python; do
    if has_cmd "$cmd"; then
      py_cmd="$cmd"
      break
    fi
  done

  if [ -z "$py_cmd" ]; then
    warn "Python not found. Python ${MIN_PYTHON_MAJOR}.${MIN_PYTHON_MINOR}+ is recommended for the Python SDK."
    echo "  Install Python: https://www.python.org/downloads/"
    return
  fi

  local py_version
  py_version="$($py_cmd -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
  local py_major py_minor
  py_major="$(echo "$py_version" | cut -d. -f1)"
  py_minor="$(echo "$py_version" | cut -d. -f2)"

  if [ "$py_major" -ge "$MIN_PYTHON_MAJOR" ] && [ "$py_minor" -ge "$MIN_PYTHON_MINOR" ]; then
    info "Python ${py_version} found"
    echo ""
    echo "  Install the Python SDK with:"
    echo "    pip install duduclaw"
  else
    warn "Python ${py_version} found, but ${MIN_PYTHON_MAJOR}.${MIN_PYTHON_MINOR}+ is recommended."
    echo "  Upgrade Python: https://www.python.org/downloads/"
  fi
}

check_docker() {
  heading "Checking Docker..."

  if has_cmd docker; then
    local docker_version
    docker_version="$(docker --version 2>/dev/null || echo "unknown")"
    info "Docker found: ${docker_version}"
  else
    warn "Docker not found. Docker is optional but recommended for containerized agents."
    echo "  Install Docker: https://docs.docker.com/get-docker/"
  fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
  echo ""
  resolve_version
  echo "${BOLD}DuDuClaw Installer v${DUDUCLAW_VERSION}${RESET}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

  # Detect platform
  heading "Detecting platform..."
  local target
  target="$(detect_platform)"
  info "Platform: ${target}"

  # Confirm installation
  echo ""
  echo "This will install DuDuClaw to: ${BOLD}${INSTALL_DIR}/${BINARY_NAME}${RESET}"
  if ! confirm "Proceed with installation?"; then
    echo "Installation cancelled."
    exit 0
  fi

  # Try release binary first, fall back to source build
  if ! install_from_release "$target"; then
    warn "Pre-built binary download failed for ${target} (v${DUDUCLAW_VERSION})."
    echo ""
    echo "  ${BOLD}Recommended:${RESET} install the prebuilt binary via npm instead (no compiler needed):"
    echo "       ${CYAN}npm install -g duduclaw${RESET}"
    echo ""
    echo "  Building from source requires Rust + a C toolchain and can take ~1.5h."
    echo ""
    if confirm "Build from source using cargo instead?"; then
      install_from_source
    else
      fatal "Installation cancelled. Use 'npm install -g duduclaw' for the prebuilt binary."
    fi
  fi

  # Add to PATH
  add_to_path

  # Check optional dependencies
  check_python
  check_docker

  # Success
  heading "Installation complete!"
  echo ""
  echo "  ${BOLD}Next steps:${RESET}"
  echo ""
  echo "    1. Restart your shell or run:"
  echo "       ${CYAN}source ~/.bashrc${RESET}  (or ~/.zshrc)"
  echo ""
  echo "    2. Run the onboarding wizard:"
  echo "       ${CYAN}duduclaw onboard${RESET}"
  echo ""
  echo "    3. Start the gateway:"
  echo "       ${CYAN}duduclaw gateway start${RESET}"
  echo ""
  echo "  ${BOLD}Documentation:${RESET} https://github.com/${GITHUB_REPO}"
  echo ""
}

main "$@"
