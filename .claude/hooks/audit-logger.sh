#!/usr/bin/env bash
# Phase 2: Async audit logger (PostToolUse — all tools)
#
# CONFLICT NOTE [P2→P1]:
#   This hook MUST appear AFTER secret-scanner.sh in the PostToolUse array.
#
# Appends JSONL entries to ~/.duduclaw/security_audit.jsonl.
# Runs with async:true — does NOT block tool execution.
#
# [CR-5] Output format compatible with Rust audit.rs AuditEvent:
#   { timestamp, event_type, agent_id, severity, details: {...} }
#
# Exit behavior:
#   exit 0 always — audit failures must never block the workflow

# Intentionally no set -e: audit logging must never fail fatally
set -uo pipefail

INPUT="$(cat)" || true

AUDIT_FILE="${HOME}/.duduclaw/security_audit.jsonl"
AUDIT_DIR="$(dirname "$AUDIT_FILE")"

# Ensure directory exists
mkdir -p "$AUDIT_DIR" 2>/dev/null || true

# Extract fields from hook input
TOOL_NAME="$(printf '%s' "$INPUT" | jq -r '.tool_name // "unknown"' 2>/dev/null || echo "unknown")"
SESSION_ID="${CLAUDE_SESSION_ID:-$(printf '%s' "$INPUT" | jq -r '.session_id // "unknown"' 2>/dev/null || echo "unknown")}"
AGENT_ID="${DUDUCLAW_AGENT_ID:-claude-code-session}"

# Truncate tool input to 200 chars for summary
INPUT_SUMMARY="$(printf '%s' "$INPUT" | jq -r '.tool_input // {} | tostring' 2>/dev/null | head -c 200 || echo "{}")"

# Truncate tool result to 200 chars for summary
RESULT_SUMMARY="$(printf '%s' "$INPUT" | jq -r '.tool_result // "" | tostring' 2>/dev/null | head -c 200 || echo "")"

# Determine severity based on tool type
SEVERITY="info"
case "$TOOL_NAME" in
  Bash)       SEVERITY="info" ;;
  Write|Edit) SEVERITY="info" ;;
  mcp__*)     SEVERITY="warning" ;;
esac

# Build audit entry [CR-5] compatible with Rust AuditEvent schema
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

ENTRY="$(jq -cn \
  --arg ts "$TIMESTAMP" \
  --arg evt "tool_use" \
  --arg aid "$AGENT_ID" \
  --arg sev "$SEVERITY" \
  --arg tool "$TOOL_NAME" \
  --arg sid "$SESSION_ID" \
  --arg input_s "$INPUT_SUMMARY" \
  --arg result_s "$RESULT_SUMMARY" \
  '{
    timestamp: $ts,
    event_type: $evt,
    agent_id: $aid,
    severity: $sev,
    details: {
      tool: $tool,
      session_id: $sid,
      input_summary: $input_s,
      result_summary: $result_s
    }
  }' 2>/dev/null)" || exit 0

# Append — single-line printf is atomic for < PIPE_BUF on POSIX [L-1]
printf '%s\n' "$ENTRY" >> "$AUDIT_FILE" 2>/dev/null || true

exit 0
