#!/usr/bin/env bash
# Phase 2: Config tamper detection (ConfigChange)
#
# Fires when settings.json or other config files are externally modified.
# Logs a critical-severity audit event and warns Claude via additionalContext.
#
# CONFLICT NOTE: No other phase touches ConfigChange. No conflict.
#
# Exit behavior:
#   exit 0 + JSON with additionalContext  → warn Claude about the change
#   exit 0 + no JSON                      → silent logging only

set -uo pipefail

INPUT="$(cat)" || true

AUDIT_FILE="${HOME}/.duduclaw/security_audit.jsonl"
AUDIT_DIR="$(dirname "$AUDIT_FILE")"

# Ensure directory exists
mkdir -p "$AUDIT_DIR" 2>/dev/null || true

# Extract fields from hook input
SOURCE="$(printf '%s' "$INPUT" | jq -r '.source // "unknown"' 2>/dev/null || echo "unknown")"
FILE_PATH="$(printf '%s' "$INPUT" | jq -r '.file_path // "unknown"' 2>/dev/null || echo "unknown")"
SESSION_ID="${CLAUDE_SESSION_ID:-$(printf '%s' "$INPUT" | jq -r '.session_id // "unknown"' 2>/dev/null || echo "unknown")}"

# Determine severity based on source
SEVERITY="warning"
case "$SOURCE" in
  user_settings)    SEVERITY="critical" ;;
  project_settings) SEVERITY="critical" ;;
  *)                SEVERITY="warning" ;;
esac

# Build and append audit entry
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

ENTRY="$(jq -cn \
  --arg ts "$TIMESTAMP" \
  --arg sid "$SESSION_ID" \
  --arg evt "config_change" \
  --arg src "$SOURCE" \
  --arg fp "$FILE_PATH" \
  --arg sev "$SEVERITY" \
  '{
    timestamp: $ts,
    session_id: $sid,
    event_type: $evt,
    source: $src,
    file_path: $fp,
    severity: $sev
  }' 2>/dev/null)" || true

# Append to audit log (atomic single-line write)
if [[ -n "${ENTRY:-}" ]]; then
  printf '%s\n' "$ENTRY" >> "$AUDIT_FILE" 2>/dev/null || true
fi

# Warn Claude about the config change for critical sources
if [[ "$SEVERITY" == "critical" ]]; then
  jq -n \
    --arg src "$SOURCE" \
    --arg fp "$FILE_PATH" \
    '{ "additionalContext": ("[SECURITY ALERT] Configuration file modified externally.\nSource: " + $src + "\nFile: " + $fp + "\nThis change was NOT initiated by the current session. Verify if this is expected.") }'
fi

exit 0
