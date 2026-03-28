#!/usr/bin/env bash
# Phase 1: Contract context injection (UserPromptSubmit)
#
# Reads the nearest CONTRACT.toml and injects must_not / must_always
# rules as additionalContext so Claude is aware of behavioral boundaries.
#
# CONFLICT NOTE [P1->P2]: RESOLVED
#   Phase 2's SessionStart (session-init.sh) pre-parses CONTRACT.toml
#   into ~/.duduclaw/.contract_cache.json. This script reads cache first,
#   falling back to direct TOML parse via shared _extract_toml_array [M-6].
#
# Exit behavior:
#   exit 0 + JSON with additionalContext  → inject contract rules
#   exit 0 + no JSON                      → no contract found, skip

set -euo pipefail

# Source shared lib for _find_contract and _extract_toml_array [M-6]
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "${SCRIPT_DIR}/lib/threat-level.sh" ]]; then
  # shellcheck source=lib/threat-level.sh
  source "${SCRIPT_DIR}/lib/threat-level.sh"
fi

# Phase 2 cache path (read if available, otherwise parse TOML directly)
CACHE_FILE="${HOME}/.duduclaw/.contract_cache.json"

if [[ -f "$CACHE_FILE" ]]; then
  # Phase 2+: use pre-parsed cache
  MUST_NOT="$(jq -r '.must_not // [] | join(", ")' "$CACHE_FILE" 2>/dev/null || true)"
  MUST_ALWAYS="$(jq -r '.must_always // [] | join(", ")' "$CACHE_FILE" 2>/dev/null || true)"
  MAX_CALLS="$(jq -r '.max_tool_calls_per_turn // 0' "$CACHE_FILE" 2>/dev/null || true)"
else
  # Phase 1 fallback: parse CONTRACT.toml directly
  # Use shared _find_contract if available, otherwise inline
  if type _find_contract &>/dev/null; then
    CONTRACT_PATH="$(_find_contract "$(pwd)")" || exit 0
  else
    # Inline fallback for standalone Phase 1
    _dir="$(pwd)"
    CONTRACT_PATH=""
    while [[ "$_dir" != "/" ]]; do
      if [[ -f "$_dir/CONTRACT.toml" ]]; then
        CONTRACT_PATH="$_dir/CONTRACT.toml"
        break
      fi
      _dir="$(dirname "$_dir")"
    done
    [[ -z "$CONTRACT_PATH" ]] && exit 0
  fi

  # Use shared _extract_toml_array if available [M-6]
  if type _extract_toml_array &>/dev/null; then
    MUST_NOT="$(_extract_toml_array "must_not" "$CONTRACT_PATH" | tr '\n' ', ' | sed 's/,[[:space:]]*$//' || true)"
    MUST_ALWAYS="$(_extract_toml_array "must_always" "$CONTRACT_PATH" | tr '\n' ', ' | sed 's/,[[:space:]]*$//' || true)"
  else
    # Inline fallback
    MUST_NOT="$(awk -v key="must_not" '$0 ~ "^"key"[[:space:]]*=" { found=1 } found { print } found && /\]/ { found=0 }' "$CONTRACT_PATH" | sed 's/^[^=]*=//; s/\[//g; s/\]//g; s/"//g; s/,/ /g' | tr '\n' ' ' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' || true)"
    MUST_ALWAYS="$(awk -v key="must_always" '$0 ~ "^"key"[[:space:]]*=" { found=1 } found { print } found && /\]/ { found=0 }' "$CONTRACT_PATH" | sed 's/^[^=]*=//; s/\[//g; s/\]//g; s/"//g; s/,/ /g' | tr '\n' ' ' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' || true)"
  fi

  MAX_CALLS="$(sed -n 's/^max_tool_calls_per_turn[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONTRACT_PATH" 2>/dev/null || echo "0")"
fi

# Nothing to inject
if [[ -z "$MUST_NOT" ]] && [[ -z "$MUST_ALWAYS" ]] && [[ "$MAX_CALLS" == "0" ]]; then
  exit 0
fi

# [M-8] Build context string using $'\n' for real newlines
CONTEXT="[CONTRACT RULES - Active behavioral boundaries for this agent]"

if [[ -n "$MUST_NOT" ]]; then
  CONTEXT="${CONTEXT}"$'\n'"MUST NOT: $MUST_NOT"
fi

if [[ -n "$MUST_ALWAYS" ]]; then
  CONTEXT="${CONTEXT}"$'\n'"MUST ALWAYS: $MUST_ALWAYS"
fi

if [[ "$MAX_CALLS" != "0" ]]; then
  CONTEXT="${CONTEXT}"$'\n'"MAX TOOL CALLS PER TURN: $MAX_CALLS"
fi

jq -n --arg ctx "$CONTEXT" '{ "additionalContext": $ctx }'
exit 0
