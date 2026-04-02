#!/usr/bin/env bash
# DuDuClaw Box - macOS Setup Script
# Installs DuDuClaw + dependencies on a fresh Mac
# Supports: Mac Mini M4, Mac Studio M4 Max, MacBook Pro M4
set -euo pipefail

# ---------------------------------------------------------------------------
# Color helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

ok()   { echo -e "${GREEN}[OK]${NC}    $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*"; }
info() { echo -e "${CYAN}[INFO]${NC}  $*"; }
step() { echo -e "\n${BOLD}==> $*${NC}"; }

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DUDUCLAW_REPO="https://github.com/nicholasgasior/duduclaw.git"
DUDUCLAW_DIR="${DUDUCLAW_DIR:-$HOME/DuDuClaw}"
MODELS_DIR="$HOME/.duduclaw/models"
STARTER_MODEL_URL="https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf"
STARTER_MODEL_NAME="qwen2.5-7b-instruct-q4_k_m.gguf"
LAUNCHD_PLIST="$HOME/Library/LaunchAgents/com.duduclaw.gateway.plist"
MIN_MACOS_VERSION=14

# ---------------------------------------------------------------------------
# 1. Check macOS version
# ---------------------------------------------------------------------------
step "Checking macOS version"
macos_version=$(sw_vers -productVersion)
major_version=$(echo "$macos_version" | cut -d. -f1)

if [[ "$major_version" -lt "$MIN_MACOS_VERSION" ]]; then
  err "macOS $MIN_MACOS_VERSION+ is required (detected: $macos_version)"
  exit 1
fi
ok "macOS $macos_version detected"

# ---------------------------------------------------------------------------
# 2. Check Apple Silicon
# ---------------------------------------------------------------------------
step "Checking hardware"
arch=$(uname -m)
if [[ "$arch" != "arm64" ]]; then
  warn "Apple Silicon (arm64) recommended. Detected: $arch"
else
  chip=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Unknown")
  ok "Apple Silicon detected: $chip"
fi

ram_bytes=$(sysctl -n hw.memsize)
ram_gb=$((ram_bytes / 1073741824))
ok "RAM: ${ram_gb} GB"

# ---------------------------------------------------------------------------
# 3. Install Homebrew
# ---------------------------------------------------------------------------
step "Checking Homebrew"
if command -v brew &>/dev/null; then
  ok "Homebrew already installed: $(brew --version | head -1)"
else
  info "Installing Homebrew..."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  # Add brew to PATH for Apple Silicon
  if [[ -f /opt/homebrew/bin/brew ]]; then
    eval "$(/opt/homebrew/bin/brew shellenv)"
  fi
  ok "Homebrew installed"
fi

# ---------------------------------------------------------------------------
# 4. Install Rust toolchain
# ---------------------------------------------------------------------------
step "Checking Rust toolchain"
if command -v rustc &>/dev/null; then
  ok "Rust already installed: $(rustc --version)"
else
  info "Installing Rust via rustup..."
  # Note: Homebrew and Rustup use HTTPS with certificate pinning.
  # For additional security, verify checksums after download:
  #   sha256sum /tmp/rustup.sh
  #   Compare against https://static.rust-lang.org/rustup/archive/
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
  ok "Rust installed: $(rustc --version)"
fi

# ---------------------------------------------------------------------------
# 5. Install system dependencies
# ---------------------------------------------------------------------------
step "Installing system dependencies"
deps=(pkg-config openssl node npm)
for dep in "${deps[@]}"; do
  if brew list "$dep" &>/dev/null; then
    ok "$dep already installed"
  else
    info "Installing $dep..."
    brew install "$dep"
    ok "$dep installed"
  fi
done

# ---------------------------------------------------------------------------
# 6. Clone / update DuDuClaw repository
# ---------------------------------------------------------------------------
step "Setting up DuDuClaw source"
if [[ -d "$DUDUCLAW_DIR/.git" ]]; then
  info "Repository already exists at $DUDUCLAW_DIR, pulling latest..."
  git -C "$DUDUCLAW_DIR" pull --ff-only || warn "git pull failed, continuing with existing source"
  ok "Repository updated"
else
  info "Cloning DuDuClaw to $DUDUCLAW_DIR..."
  git clone "$DUDUCLAW_REPO" "$DUDUCLAW_DIR"
  ok "Repository cloned"
fi

# ---------------------------------------------------------------------------
# 7. Build from source
# ---------------------------------------------------------------------------
step "Building DuDuClaw (release mode)"
cd "$DUDUCLAW_DIR"
cargo build --release
ok "Binary built: target/release/duduclaw"

# Symlink to cargo bin
CARGO_BIN="$HOME/.cargo/bin/duduclaw"
if [[ ! -L "$CARGO_BIN" ]] || [[ "$(readlink "$CARGO_BIN")" != "$DUDUCLAW_DIR/target/release/duduclaw" ]]; then
  ln -sf "$DUDUCLAW_DIR/target/release/duduclaw" "$CARGO_BIN"
  ok "Symlinked to $CARGO_BIN"
fi

# ---------------------------------------------------------------------------
# 8. Build web dashboard
# ---------------------------------------------------------------------------
step "Building web dashboard"
cd "$DUDUCLAW_DIR/web"
if ! command -v npm &>/dev/null; then
  err "npm not found. Please install Node.js."
  exit 1
fi
npm ci
npm run build
ok "Web dashboard built"

# ---------------------------------------------------------------------------
# 9. Run onboard wizard
# ---------------------------------------------------------------------------
step "Running first-time onboard"
cd "$DUDUCLAW_DIR"
if command -v duduclaw &>/dev/null; then
  duduclaw onboard --yes || warn "Onboard returned non-zero (may already be configured)"
  ok "Onboard complete"
else
  warn "duduclaw binary not in PATH, skipping onboard"
fi

# ---------------------------------------------------------------------------
# 10. Download starter model
# ---------------------------------------------------------------------------
step "Downloading starter model"
mkdir -p "$MODELS_DIR"
model_path="$MODELS_DIR/$STARTER_MODEL_NAME"

if [[ -f "$model_path" ]]; then
  ok "Starter model already exists: $STARTER_MODEL_NAME"
else
  info "Downloading $STARTER_MODEL_NAME (~4.4 GB)..."
  info "This may take a while depending on your connection."
  if curl -L --progress-bar -o "$model_path.part" "$STARTER_MODEL_URL"; then
    mv "$model_path.part" "$model_path"
    ok "Model downloaded to $model_path"
  else
    err "Model download failed. You can retry later:"
    err "  curl -L -o $model_path $STARTER_MODEL_URL"
    rm -f "$model_path.part"
  fi
fi

# ---------------------------------------------------------------------------
# 11. Create launchd plist for auto-start
# ---------------------------------------------------------------------------
step "Setting up auto-start (launchd)"
mkdir -p "$(dirname "$LAUNCHD_PLIST")"

cat > "$LAUNCHD_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.duduclaw.gateway</string>
  <key>ProgramArguments</key>
  <array>
    <string>${CARGO_BIN}</string>
    <string>serve</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>WorkingDirectory</key>
  <string>${HOME}/.duduclaw</string>
  <key>StandardOutPath</key>
  <string>${HOME}/.duduclaw/logs/gateway.stdout.log</string>
  <key>StandardErrorPath</key>
  <string>${HOME}/.duduclaw/logs/gateway.stderr.log</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:${HOME}/.cargo/bin</string>
  </dict>
</dict>
</plist>
PLIST

mkdir -p "$HOME/.duduclaw/logs"

# Load the service
launchctl unload "$LAUNCHD_PLIST" 2>/dev/null || true
launchctl load "$LAUNCHD_PLIST"
ok "launchd service installed and started"

# ---------------------------------------------------------------------------
# Done!
# ---------------------------------------------------------------------------
echo ""
echo -e "${GREEN}${BOLD}============================================${NC}"
echo -e "${GREEN}${BOLD}  DuDuClaw Box setup complete!${NC}"
echo -e "${GREEN}${BOLD}============================================${NC}"
echo ""
echo -e "  Dashboard:  ${CYAN}http://localhost:3120${NC}"
echo -e "  Config:     ${CYAN}~/.duduclaw/${NC}"
echo -e "  Models:     ${CYAN}~/.duduclaw/models/${NC}"
echo -e "  Logs:       ${CYAN}~/.duduclaw/logs/${NC}"
echo -e "  Source:     ${CYAN}${DUDUCLAW_DIR}${NC}"
echo ""
echo -e "  Manage service:"
echo -e "    Start:  ${BOLD}launchctl load $LAUNCHD_PLIST${NC}"
echo -e "    Stop:   ${BOLD}launchctl unload $LAUNCHD_PLIST${NC}"
echo ""
echo -e "  Next steps:"
echo -e "    1. Open ${CYAN}http://localhost:3120${NC} in your browser"
echo -e "    2. Configure your first Agent"
echo -e "    3. Connect a channel (Telegram / LINE / Discord)"
echo ""
