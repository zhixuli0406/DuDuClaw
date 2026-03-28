#!/usr/bin/env bash
# Phase 1: Secret leak scanner (PostToolUse — Write|Edit)
#
# CONFLICT NOTE [P1->P2]:
#   This scanner MUST appear BEFORE audit-logger in PostToolUse array.
#
# Exit behavior:
#   exit 0 + JSON with additionalContext → warn Claude about leak
#   exit 0 + no JSON                    → clean, no secrets found

set -euo pipefail

INPUT="$(cat)"

FILE_PATH="$(printf '%s' "$INPUT" | jq -r '.tool_input.file_path // empty')"

# No file path or file doesn't exist — skip
if [[ -z "$FILE_PATH" ]] || [[ ! -f "$FILE_PATH" ]]; then
  exit 0
fi

# [H-6] Skip files larger than 5MB
FILE_SIZE="$(stat -f%z "$FILE_PATH" 2>/dev/null || stat -c%s "$FILE_PATH" 2>/dev/null || echo "0")"
if (( FILE_SIZE > 5242880 )); then
  exit 0
fi

# [H-3] Fixed: explicitly list text-based MIME types to scan, skip everything else
MIME="$(file --brief --mime-type "$FILE_PATH" 2>/dev/null || echo "unknown")"
case "$MIME" in
  text/*|application/json|application/x-yaml|application/toml|application/xml|application/javascript|unknown) ;;
  *) exit 0 ;;  # Binary file, skip
esac

# ---------------------------------------------------------------------------
# Secret patterns (extended regex)
#   All \s replaced with [[:space:]] for macOS compatibility [H-1]
# ---------------------------------------------------------------------------
PATTERNS=(
  # Anthropic / OpenAI API keys
  'sk-ant-[a-zA-Z0-9_-]{20,}'
  'sk-[a-zA-Z0-9]{20,}'

  # GitHub Personal Access Token
  'ghp_[a-zA-Z0-9]{36}'
  'github_pat_[a-zA-Z0-9_]{22,}'

  # AWS Access Key
  'AKIA[0-9A-Z]{16}'

  # AWS Secret Key (common assignment patterns)
  'aws_secret_access_key[[:space:]]*=[[:space:]]*[A-Za-z0-9/+=]{40}'

  # Private keys (PEM)
  '-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----'

  # Slack tokens
  'xoxb-[0-9]+-[0-9a-zA-Z]+'
  'xoxp-[0-9]+-[0-9a-zA-Z]+'
  'xapp-[0-9]+-[A-Za-z0-9]+'

  # Generic tokens in assignments (key = "long_secret")
  '(api_key|api_secret|access_token|auth_token|secret_key)[[:space:]]*[:=][[:space:]]*["'"'"'][A-Za-z0-9/+=_-]{20,}["'"'"']'

  # Discord bot token
  '[MN][A-Za-z0-9]{23,}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27}'

  # Telegram bot token
  '[0-9]+:AA[A-Za-z0-9_-]{33}'

  # LINE channel access token (long JWT-like)
  'LINE_CHANNEL_ACCESS_TOKEN[[:space:]]*=[[:space:]]*[A-Za-z0-9/+=]{100,}'

  # Base64-encoded long secrets next to suspicious keywords
  '(password|passwd|token|secret)[[:space:]]*[:=][[:space:]]*["'"'"'][A-Za-z0-9+/]{40,}={0,2}["'"'"']'

  # [M-3] Additional secret patterns
  # Stripe Secret Key
  'sk_live_[A-Za-z0-9]{24,}'
  # npm access token
  'npm_[A-Za-z0-9]{36}'
  # HashiCorp Vault token
  'hvs\.[A-Za-z0-9]{24}'
  # Database URLs with credentials
  '(postgres|mysql|mongodb)://[^[:space:]:]+:[^@[:space:]]+@'
  # HTTPS URLs with embedded credentials
  'https?://[^[:space:]:]+:[^@[:space:]]+@'
  # GCP service account JSON
  '"type":[[:space:]]*"service_account"'
)

FOUND_PATTERNS=()

for pattern in "${PATTERNS[@]}"; do
  if grep -qE -- "$pattern" "$FILE_PATH" 2>/dev/null; then
    # Get line number for context (don't show actual secret value)
    local_line="$(grep -nE -- "$pattern" "$FILE_PATH" 2>/dev/null | head -1 | cut -d: -f1)"
    FOUND_PATTERNS+=("$pattern (line ${local_line:-?})")
  fi
done

if [[ ${#FOUND_PATTERNS[@]} -gt 0 ]]; then
  REASON="Potential secret(s) detected in $FILE_PATH:"
  for p in "${FOUND_PATTERNS[@]}"; do
    REASON="${REASON}"$'\n'"  - Pattern: $p"
  done

  # [L-7] Use additionalContext instead of decision:block for PostToolUse
  jq -n --arg reason "$REASON" '{
    "additionalContext": ("SECRET LEAK WARNING: " + $reason + "\nPlease remove secrets and use environment variables or a secret manager instead.")
  }'
  exit 0
fi

# Clean — no secrets found
exit 0
