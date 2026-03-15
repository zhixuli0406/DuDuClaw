#!/usr/bin/env bash
# DuDuClaw Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
# Or:    curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh -s -- --yes
set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DUDUCLAW_VERSION="0.1.0"
GITHUB_REPO="zhixuli0406/DuDuClaw"
INSTALL_DIR="${DUDUCLAW_HOME:-$HOME/.duduclaw}/bin"
BINARY_NAME="duduclaw"
MIN_PYTHON_MAJOR=3
MIN_PYTHON_MINOR=10
AUTO_YES=false

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
      echo "Environment variables:"
      echo "  DUDUCLAW_HOME    Override install directory (default: ~/.duduclaw)"
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

# ---------------------------------------------------------------------------
# Install binary from GitHub release
# ---------------------------------------------------------------------------
install_from_release() {
  local target="$1"
  local archive_ext="tar.gz"
  local release_url="https://github.com/${GITHUB_REPO}/releases/download/v${DUDUCLAW_VERSION}/${target}.${archive_ext}"
  local sha_url="${release_url}.sha256"
  local tmp_dir

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  heading "Downloading DuDuClaw v${DUDUCLAW_VERSION} (${target})..."
  echo "  URL: ${release_url}"

  if ! download "$release_url" "${tmp_dir}/${target}.${archive_ext}" 2>/dev/null; then
    return 1
  fi

  # Verify checksum if available
  if download "$sha_url" "${tmp_dir}/${target}.${archive_ext}.sha256" 2>/dev/null; then
    heading "Verifying checksum..."
    cd "$tmp_dir"
    if has_cmd sha256sum; then
      sha256sum -c "${target}.${archive_ext}.sha256" >/dev/null 2>&1 && info "Checksum verified" || warn "Checksum verification failed"
    elif has_cmd shasum; then
      shasum -a 256 -c "${target}.${archive_ext}.sha256" >/dev/null 2>&1 && info "Checksum verified" || warn "Checksum verification failed"
    fi
    cd - >/dev/null
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
    warn "Pre-built binary not available for ${target}."
    echo ""
    if confirm "Build from source using cargo instead?"; then
      install_from_source
    else
      fatal "Installation cancelled. No binary available."
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
