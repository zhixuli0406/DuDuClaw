#!/usr/bin/env bash
# Phase 2: Session initialization (SessionStart)
#
# Runs once when a Claude Code session begins. Responsibilities:
# 1. Verify secret.key file permissions (0600)
# 2. Write environment variables to $CLAUDE_ENV_FILE if available
# 3. Parse CONTRACT.toml → cache to ~/.duduclaw/.contract_cache.json
#
# Exit behavior:
#   exit 0  → session continues normally
#   exit 2  → block session start (only for critical failures)

set -euo pipefail

DUDUCLAW_HOME="${HOME}/.duduclaw"
SECRET_KEY="${DUDUCLAW_HOME}/secret.key"
CONTRACT_CACHE="${DUDUCLAW_HOME}/.contract_cache.json"

# Source shared lib for _find_contract and _extract_toml_array [M-6]
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/threat-level.sh
source "${SCRIPT_DIR}/lib/threat-level.sh"

# ---------------------------------------------------------------------------
# 1. Verify secret.key permissions
# ---------------------------------------------------------------------------
if [[ -f "$SECRET_KEY" ]]; then
  # stat -f '%Lp' is macOS/BSD (outputs octal like "600"), stat -c '%a' is GNU
  PERMS="$(stat -f '%Lp' "$SECRET_KEY" 2>/dev/null || stat -c '%a' "$SECRET_KEY" 2>/dev/null || echo "unknown")"
  if [[ "$PERMS" != "600" ]] && [[ "$PERMS" != "unknown" ]]; then
    echo "[SECURITY WARNING] secret.key has permissions $PERMS (expected 600). Fixing..." >&2
    chmod 600 "$SECRET_KEY" 2>/dev/null || echo "[SECURITY WARNING] Failed to fix secret.key permissions. Please run: chmod 600 $SECRET_KEY" >&2
  fi
fi

# ---------------------------------------------------------------------------
# 2. Write environment variables to CLAUDE_ENV_FILE [H-5] with validation
# ---------------------------------------------------------------------------
if [[ -n "${CLAUDE_ENV_FILE:-}" ]]; then
  if [[ -f ".env.claude" ]]; then
    # [H-5] Validate: only allow VARNAME=value, reject dangerous variables
    DANGEROUS_VARS='(PATH|LD_PRELOAD|LD_LIBRARY_PATH|DYLD_LIBRARY_PATH|HOME|SHELL|USER|PYTHONPATH|NODE_PATH|RUBYLIB|PERL5LIB)'
    while IFS= read -r line; do
      # Skip comments and blank lines
      [[ "$line" =~ ^[[:space:]]*# ]] && continue
      [[ -z "${line// /}" ]] && continue
      # Validate format: VARNAME=value
      if [[ "$line" =~ ^[A-Z_][A-Z0-9_]*= ]]; then
        # Reject dangerous variable names
        local_varname="${line%%=*}"
        if printf '%s' "$local_varname" | grep -qE "^${DANGEROUS_VARS}$"; then
          echo "[SECURITY WARNING] Rejected dangerous variable in .env.claude: $local_varname" >&2
          continue
        fi
        printf '%s\n' "$line" >> "$CLAUDE_ENV_FILE"
      fi
    done < ".env.claude"
  fi

  # Set DuDuClaw-specific environment
  {
    echo "DUDUCLAW_HOME=${DUDUCLAW_HOME}"
    echo "DUDUCLAW_HOOKS_PHASE=3"
  } >> "$CLAUDE_ENV_FILE"
fi

# ---------------------------------------------------------------------------
# 3. Parse CONTRACT.toml → cache to .contract_cache.json
#    Uses shared _find_contract and _extract_toml_array from lib [M-6]
# ---------------------------------------------------------------------------

mkdir -p "$DUDUCLAW_HOME" 2>/dev/null || true

CONTRACT_PATH="$(_find_contract "$(pwd)")" || {
  rm -f "$CONTRACT_CACHE" 2>/dev/null || true
  exit 0
}

# Build JSON cache using shared _extract_toml_array [M-6]
MUST_NOT_JSON="$(_extract_toml_array "must_not" "$CONTRACT_PATH" | jq -Rn '[inputs | select(length > 0)]' 2>/dev/null || echo '[]')"
MUST_ALWAYS_JSON="$(_extract_toml_array "must_always" "$CONTRACT_PATH" | jq -Rn '[inputs | select(length > 0)]' 2>/dev/null || echo '[]')"
MAX_CALLS="$(sed -n 's/^max_tool_calls_per_turn[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONTRACT_PATH" 2>/dev/null || echo "0")"

jq -n \
  --argjson must_not "$MUST_NOT_JSON" \
  --argjson must_always "$MUST_ALWAYS_JSON" \
  --arg max_calls "${MAX_CALLS:-0}" \
  --arg source "$CONTRACT_PATH" \
  --arg cached_at "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
  '{
    must_not: $must_not,
    must_always: $must_always,
    max_tool_calls_per_turn: ($max_calls | tonumber),
    source: $source,
    cached_at: $cached_at
  }' > "$CONTRACT_CACHE"

exit 0
