#!/usr/bin/env bash
#
# dev-replace-binary.sh — local dev loop for shipping a fresh duduclaw
# binary into the npm-installed location.
#
# What it does (in order):
#
#   1. Rebuild the embedded web dashboard (vite) so React changes land
#      inside the Rust binary's rust-embed asset bundle.
#   2. cargo build --release the CLI binary (depends on duduclaw-gateway,
#      which depends on duduclaw-dashboard with the new dist/).
#   3. Stop any running `duduclaw run` so the file isn't busy.
#   4. Back up the existing npm-installed binary with a timestamped name.
#   5. Copy the fresh release binary into place.
#   6. Quick smoke: version, license fingerprint, license status.
#
# DOES NOT restart `duduclaw run` or `duduclaw mcp-server` — those have
# side effects (Discord bot online, Claude Desktop reconnect, cron tasks
# resume) that you should trigger explicitly.
#
# Usage:
#   scripts/dev-replace-binary.sh             # full pipeline
#   scripts/dev-replace-binary.sh --skip-web  # Rust-only change, faster
#   scripts/dev-replace-binary.sh --dry-run   # print actions, do nothing
#
# Pre-existing PyO3 linker errors in duduclaw-bridge are ignored — this
# script builds duduclaw-cli specifically.

set -euo pipefail

# Resolve repo root regardless of where the script is invoked from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Helpers (must precede first use) ──────────────────────────

step() { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m⚠\033[0m %s\n' "$*"; }
die()  { printf '\n\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

run() {
  if [ "${DRY_RUN:-0}" = "1" ]; then
    printf '  [dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

# ── Configuration ──────────────────────────────────────────────

# npm-installed binary path. Detected automatically; can be overridden by
# DUDUCLAW_INSTALLED_BIN env var.
DEFAULT_INSTALLED_BIN="$(command -v duduclaw 2>/dev/null || true)"
INSTALLED_BIN="${DUDUCLAW_INSTALLED_BIN:-$DEFAULT_INSTALLED_BIN}"

# `which duduclaw` returns a wrapper (npm's `bin/duduclaw`, often a JS
# shim) — but we need to replace the platform-native binary that lives
# inside `node_modules/@duduclaw/<platform>/bin/duduclaw`. The wrapper
# itself must stay intact so npm's `node bin/duduclaw …` invocation
# keeps working.
if [ -n "$INSTALLED_BIN" ] && [ -L "$INSTALLED_BIN" ]; then
  INSTALLED_BIN="$(readlink -f "$INSTALLED_BIN" 2>/dev/null || readlink "$INSTALLED_BIN")"
fi

# Detect if INSTALLED_BIN points at a Node wrapper (script) rather than a
# Mach-O / ELF binary. If so, walk the npm package layout to the native one.
if [ -n "$INSTALLED_BIN" ]; then
  FILE_KIND="$(file -b "$INSTALLED_BIN" 2>/dev/null || true)"
  case "$FILE_KIND" in
    *"script text"*|*"Node.js script"*|*"a /usr/bin/env"*|*"a "*"node"*)
      # Wrapper detected → find the real native binary alongside it.
      WRAPPER_PKG_DIR="$(dirname "$(dirname "$INSTALLED_BIN")")"  # .../duduclaw
      NATIVE_BIN="$(find "$WRAPPER_PKG_DIR/node_modules/@duduclaw" \
        -name duduclaw -type f 2>/dev/null | head -1)"
      if [ -n "$NATIVE_BIN" ]; then
        INSTALLED_BIN="$NATIVE_BIN"
      else
        die "Could not locate native binary under $WRAPPER_PKG_DIR/node_modules/@duduclaw/. Set DUDUCLAW_INSTALLED_BIN manually."
      fi
      ;;
  esac
fi

RELEASE_BIN="$REPO_ROOT/target/release/duduclaw"

# ── CLI flags ──────────────────────────────────────────────────

SKIP_WEB=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --skip-web)  SKIP_WEB=1 ;;
    --dry-run)   DRY_RUN=1 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown arg: $arg" >&2
      exit 2
      ;;
  esac
done

# ── Pre-flight ─────────────────────────────────────────────────

step "Pre-flight"

if [ -z "$INSTALLED_BIN" ]; then
  die "Cannot locate the installed duduclaw binary. Set DUDUCLAW_INSTALLED_BIN to the path of @duduclaw/<platform>/bin/duduclaw"
fi
ok "Installed binary: $INSTALLED_BIN"

if [ ! -w "$(dirname "$INSTALLED_BIN")" ]; then
  die "Installed binary's directory is not writable. Try with sudo, or pick a different DUDUCLAW_INSTALLED_BIN."
fi
ok "Target directory writable"

# Refuse if duduclaw run is alive — replacing a mmap'd binary may corrupt
# the running process or refuse with "Text file busy".
RUNNING_PIDS=$(pgrep -f "duduclaw run" 2>/dev/null || true)
if [ -n "$RUNNING_PIDS" ]; then
  warn "Found running 'duduclaw run' process(es): $RUNNING_PIDS"
  warn "On macOS replacing a running binary usually works, but please stop the gateway for a clean test."
  warn "  pkill -f 'duduclaw run'"
  if [ "$DRY_RUN" != "1" ]; then
    read -r -p "Continue anyway? [y/N] " yn
    case "$yn" in [yY]*) ;; *) exit 0 ;; esac
  fi
fi

# ── Step 1: Web bundle ─────────────────────────────────────────

if [ "$SKIP_WEB" = "0" ]; then
  step "Building web bundle (vite)"
  if [ ! -d "$REPO_ROOT/web/node_modules" ]; then
    warn "web/node_modules missing — running npm ci first"
    run sh -c "cd '$REPO_ROOT/web' && npm ci"
  fi
  run sh -c "cd '$REPO_ROOT/web' && npm run build"
  ok "Web bundle → crates/duduclaw-dashboard/dist/"
else
  step "Skipping web bundle (--skip-web)"
fi

# ── Step 2: Cargo release build ────────────────────────────────

step "cargo build --release -p duduclaw-cli"
run cargo build --release -p duduclaw-cli
ok "Release binary built: $RELEASE_BIN"

if [ "$DRY_RUN" = "0" ] && [ ! -x "$RELEASE_BIN" ]; then
  die "Expected $RELEASE_BIN to exist after cargo build"
fi

# ── Step 3: Stop running duduclaw run ──────────────────────────

step "Stopping any running 'duduclaw run'"
if [ -n "$RUNNING_PIDS" ]; then
  run pkill -f "duduclaw run" || true
  if [ "$DRY_RUN" = "0" ]; then sleep 1; fi
  ok "Sent SIGTERM"
else
  ok "No 'duduclaw run' process found"
fi

# ── Step 4: Backup ─────────────────────────────────────────────

step "Backing up current installed binary"
BACKUP="${INSTALLED_BIN}.bak-$(date +%Y%m%d-%H%M%S)"
if [ "$DRY_RUN" = "1" ]; then
  printf '  [dry-run] cp -p %s %s\n' "$INSTALLED_BIN" "$BACKUP"
else
  cp -p "$INSTALLED_BIN" "$BACKUP"
  ok "Backup: $BACKUP ($(wc -c < "$BACKUP" | awk '{print $1}') bytes)"
fi

# ── Step 5: Replace ────────────────────────────────────────────

step "Replacing installed binary"
run cp "$RELEASE_BIN" "$INSTALLED_BIN"
run chmod +x "$INSTALLED_BIN"
ok "Replaced ($(wc -c < "$INSTALLED_BIN" 2>/dev/null | awk '{print $1}') bytes)"

# ── Step 6: Smoke ──────────────────────────────────────────────

step "Smoke test"
if [ "$DRY_RUN" = "1" ]; then
  printf '  [dry-run] duduclaw version\n'
  printf '  [dry-run] duduclaw license fingerprint\n'
  printf '  [dry-run] duduclaw license status\n'
else
  VERSION=$("$INSTALLED_BIN" version 2>&1 | grep -v "log level" | tail -1)
  ok "version: $VERSION"
  FP=$("$INSTALLED_BIN" license fingerprint 2>&1 | grep -v "log level" | tail -1)
  ok "fingerprint: $FP"
  MODE=$("$INSTALLED_BIN" license status 2>&1 | grep -v "log level" | grep -E "^(Mode|Tier):" | head -2 | tr '\n' ' ')
  ok "license status: $MODE"
fi

# ── Done ───────────────────────────────────────────────────────

cat <<EOF

\033[1;32m──────────────────────────────────────────────\033[0m
\033[1;32m  Done. Next steps:\033[0m
\033[1;32m──────────────────────────────────────────────\033[0m

  1. Restart the gateway when you're ready:
       duduclaw run

  2. If you have Claude Desktop using mcp-server, restart it so it
     respawns against the new binary.

  3. Hard-refresh any open dashboard browser tabs (Cmd+Shift+R).

  Rollback if needed:
       cp '$BACKUP' '$INSTALLED_BIN'

EOF
