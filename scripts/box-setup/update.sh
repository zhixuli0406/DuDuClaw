#!/usr/bin/env bash
# DuDuClaw Box - Update Script
# Pulls latest version and rebuilds
set -euo pipefail

# ---------------------------------------------------------------------------
# Color helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

ok()   { echo -e "${GREEN}[OK]${NC}    $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*"; }
info() { echo -e "${CYAN}[INFO]${NC}  $*"; }
step() { echo -e "\n${BOLD}==> $*${NC}"; }

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DUDUCLAW_DIR="${DUDUCLAW_DIR:-$HOME/DuDuClaw}"
BACKUP_DIR="$HOME/.duduclaw/backups"
LAUNCHD_PLIST="$HOME/Library/LaunchAgents/com.duduclaw.gateway.plist"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

echo -e "${BOLD}DuDuClaw Box — Update${NC}"
echo -e "${BOLD}$(printf '=%.0s' {1..40})${NC}"

# ---------------------------------------------------------------------------
# 1. Verify source directory
# ---------------------------------------------------------------------------
step "Verifying source directory"
if [[ ! -d "$DUDUCLAW_DIR/.git" ]]; then
  err "DuDuClaw source not found at $DUDUCLAW_DIR"
  err "Set DUDUCLAW_DIR env var to the correct path."
  exit 1
fi
ok "Source directory: $DUDUCLAW_DIR"

cd "$DUDUCLAW_DIR"
current_version=$(git describe --tags --always 2>/dev/null || echo "unknown")
info "Current version: $current_version"

# ---------------------------------------------------------------------------
# 2. Backup config
# ---------------------------------------------------------------------------
step "Backing up configuration"
mkdir -p "$BACKUP_DIR"
backup_file="$BACKUP_DIR/config_${TIMESTAMP}.tar.gz"

config_dir="$HOME/.duduclaw"
if [[ -d "$config_dir" ]]; then
  # Backup config files only (exclude models and logs to save space)
  tar czf "$backup_file" \
    --exclude='models' \
    --exclude='logs' \
    --exclude='backups' \
    -C "$HOME" ".duduclaw" 2>/dev/null || true
  ok "Config backed up to $backup_file"

  # Keep only last 5 backups
  ls -tp "$BACKUP_DIR"/config_*.tar.gz 2>/dev/null | tail -n +6 | xargs -I {} rm -- {} 2>/dev/null || true
else
  warn "No config directory found, skipping backup"
fi

# ---------------------------------------------------------------------------
# 3. Check for uncommitted changes
# ---------------------------------------------------------------------------
step "Checking working directory"
if [[ -n "$(git status --porcelain 2>/dev/null)" ]]; then
  warn "Uncommitted changes detected. Stashing..."
  git stash push -m "duduclaw-update-$TIMESTAMP"
  ok "Changes stashed"
fi

# ---------------------------------------------------------------------------
# 4. Pull latest
# ---------------------------------------------------------------------------
step "Pulling latest changes"
current_branch=$(git branch --show-current)
info "Branch: $current_branch"

if ! git pull --ff-only origin "$current_branch"; then
  warn "Fast-forward pull failed. Attempting rebase..."
  if ! git pull --rebase origin "$current_branch"; then
    err "Pull failed. Please resolve conflicts manually."
    exit 1
  fi
fi

new_version=$(git describe --tags --always 2>/dev/null || echo "unknown")
if [[ "$current_version" == "$new_version" ]]; then
  info "Already up to date ($new_version)"
else
  ok "Updated: $current_version -> $new_version"
fi

# ---------------------------------------------------------------------------
# 5. Rebuild Rust binary
# ---------------------------------------------------------------------------
step "Rebuilding DuDuClaw binary"
cargo build --release
ok "Binary rebuilt: target/release/duduclaw"

# Update symlink
CARGO_BIN="$HOME/.cargo/bin/duduclaw"
ln -sf "$DUDUCLAW_DIR/target/release/duduclaw" "$CARGO_BIN"
ok "Symlink updated"

# ---------------------------------------------------------------------------
# 6. Rebuild web dashboard
# ---------------------------------------------------------------------------
step "Rebuilding web dashboard"
cd "$DUDUCLAW_DIR/web"
npm ci
npm run build
ok "Web dashboard rebuilt"
cd "$DUDUCLAW_DIR"

# ---------------------------------------------------------------------------
# 7. Restart service if running
# ---------------------------------------------------------------------------
step "Restarting service"
if [[ -f "$LAUNCHD_PLIST" ]]; then
  if launchctl list | grep -q "com.duduclaw.gateway"; then
    info "Stopping service..."
    launchctl unload "$LAUNCHD_PLIST" 2>/dev/null || true
    sleep 1
    info "Starting service..."
    launchctl load "$LAUNCHD_PLIST"
    ok "Service restarted"
  else
    info "Service not currently running"
    launchctl load "$LAUNCHD_PLIST"
    ok "Service started"
  fi
else
  warn "No launchd plist found. Service not managed by launchd."
  info "If you run DuDuClaw manually, please restart it."
fi

# ---------------------------------------------------------------------------
# 8. Restore stashed changes if any
# ---------------------------------------------------------------------------
stash_list=$(git stash list 2>/dev/null | grep "duduclaw-update-$TIMESTAMP" || true)
if [[ -n "$stash_list" ]]; then
  step "Restoring stashed changes"
  git stash pop || warn "Could not restore stashed changes. Use 'git stash pop' manually."
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo ""
echo -e "${GREEN}${BOLD}============================================${NC}"
echo -e "${GREEN}${BOLD}  Update complete!${NC}"
echo -e "${GREEN}${BOLD}============================================${NC}"
echo ""
echo -e "  Version:  ${CYAN}$new_version${NC}"
echo -e "  Backup:   ${CYAN}$backup_file${NC}"
echo -e "  Dashboard: ${CYAN}http://localhost:3120${NC}"
echo ""
