#!/usr/bin/env bash
# Phase 1: Sensitive file protection (PreToolUse — Write|Edit|Read)
#
# CONFLICT NOTE [P1->P3]:
#   Phase 3 adds security-file-ai-review.sh as SECOND entry in Write|Edit array.
#   This script (fast, deterministic) runs first. If it denies, AI-review is skipped.
#
# Exit behavior:
#   exit 0 + JSON with permissionDecision:"deny"  → block the write/read
#   exit 0 + no JSON                               → allow the write/read

set -euo pipefail

INPUT="$(cat)"

FILE_PATH="$(printf '%s' "$INPUT" | jq -r '.tool_input.file_path // empty')"

# No file path — allow (shouldn't happen for Write|Edit|Read)
if [[ -z "$FILE_PATH" ]]; then
  exit 0
fi

# [M-4] Resolve symlinks even for non-existent files using dirname resolution
if command -v realpath &>/dev/null; then
  if [[ -e "$FILE_PATH" ]]; then
    FILE_PATH="$(realpath "$FILE_PATH")"
  else
    # Resolve parent directory to catch symlink-based traversal on new files
    local_dir="$(dirname "$FILE_PATH")"
    local_base="$(basename "$FILE_PATH")"
    if [[ -d "$local_dir" ]]; then
      FILE_PATH="$(realpath "$local_dir")/$local_base"
    fi
  fi
fi

# Normalize to lowercase for case-insensitive comparison
FILE_LOWER="$(printf '%s' "$FILE_PATH" | tr '[:upper:]' '[:lower:]')"

# ---------------------------------------------------------------------------
# Protected path patterns (exact basename or suffix match)
# ---------------------------------------------------------------------------

# Exact basenames that are always protected
PROTECTED_BASENAMES=(
  "secret.key"
  ".credentials.json"
)

# Basename patterns (glob-style, matched with case-folding)
PROTECTED_BASENAME_PATTERNS=(
  ".env"
  ".env.*"
)

# Full path substrings that trigger protection
PROTECTED_PATH_SUBSTRINGS=(
  "/.duduclaw/secret.key"
  "/.claude/settings.json"
  # [CR-1] Protect threat state files from tampering
  "/.duduclaw/threat_level"
  "/.duduclaw/threat_events"
  "/.duduclaw/security_audit"
)

BASENAME="$(basename "$FILE_PATH")"
BASENAME_LOWER="$(printf '%s' "$BASENAME" | tr '[:upper:]' '[:lower:]')"

deny_write() {
  local reason="$1"
  jq -n --arg reason "$reason" --arg path "$FILE_PATH" '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "deny",
      "permissionDecisionReason": ("PROTECTED FILE: " + $reason + " — " + $path)
    }
  }'
  exit 0
}

# Check exact basenames
for name in "${PROTECTED_BASENAMES[@]}"; do
  name_lower="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')"
  if [[ "$BASENAME_LOWER" == "$name_lower" ]]; then
    deny_write "File '$BASENAME' is a protected credential file"
  fi
done

# Check basename patterns (.env, .env.*)
for pattern in "${PROTECTED_BASENAME_PATTERNS[@]}"; do
  pattern_lower="$(printf '%s' "$pattern" | tr '[:upper:]' '[:lower:]')"
  # shellcheck disable=SC2254
  case "$BASENAME_LOWER" in
    $pattern_lower) deny_write "File '$BASENAME' matches protected pattern '$pattern'" ;;
  esac
done

# Check full path substrings
for substr in "${PROTECTED_PATH_SUBSTRINGS[@]}"; do
  substr_lower="$(printf '%s' "$substr" | tr '[:upper:]' '[:lower:]')"
  if [[ "$FILE_LOWER" == *"$substr_lower"* ]]; then
    deny_write "Path contains protected segment '$substr'"
  fi
done

# SOUL.md and CONTRACT.toml — protected with evolution bypass [Architecture CR]
# Set DUDUCLAW_EVOLUTION=1 to allow legitimate evolution writes
if [[ "${DUDUCLAW_EVOLUTION:-}" != "1" ]]; then
  case "$BASENAME_LOWER" in
    soul.md)       deny_write "SOUL.md is integrity-protected (set DUDUCLAW_EVOLUTION=1 to bypass)" ;;
    contract.toml) deny_write "CONTRACT.toml defines security boundaries (set DUDUCLAW_EVOLUTION=1 to bypass)" ;;
  esac
fi

# All checks passed — allow
exit 0
