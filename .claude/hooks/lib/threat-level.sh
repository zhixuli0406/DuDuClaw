#!/usr/bin/env bash
# Phase 3: Threat level state machine (shared library)
#
# Sourced by bash-gate.sh, threat-eval.sh, and security-file-ai-review.sh.
# NOT a hook itself — no direct conflict with other phases.
#
# State machine:
#   GREEN → YELLOW:  ≥ 2 blocked events in 1 hour
#   YELLOW → RED:    prompt injection detected OR SOUL.md drift
#   RED → YELLOW:    24h with no events
#   YELLOW → GREEN:  24h with no events
#
# State files:
#   ~/.duduclaw/threat_level        — current level (GREEN|YELLOW|RED)
#   ~/.duduclaw/threat_events.jsonl — recent block/injection events

DUDUCLAW_HOME="${HOME}/.duduclaw"
THREAT_LEVEL_FILE="${DUDUCLAW_HOME}/threat_level"
THREAT_EVENTS_FILE="${DUDUCLAW_HOME}/threat_events.jsonl"
AUDIT_FILE="${DUDUCLAW_HOME}/security_audit.jsonl"

# Thresholds
ESCALATE_TO_YELLOW_COUNT=2    # blocked events in 1 hour
DEGRADE_AFTER_SECONDS=86400   # 24 hours with no events

# ---------------------------------------------------------------------------
# _timeout — portable timeout (macOS has no `timeout` by default)
#   Usage: _timeout <seconds> <command> [args...]
# ---------------------------------------------------------------------------
_timeout() {
  local secs="$1"; shift
  if command -v timeout &>/dev/null; then
    timeout "$secs" "$@"
  elif command -v gtimeout &>/dev/null; then
    gtimeout "$secs" "$@"
  else
    # macOS fallback: perl-based timeout with proper zombie cleanup [L-5]
    perl -e '
      use POSIX ":sys_wait_h";
      my $timeout = shift @ARGV;
      my $pid = fork();
      if ($pid == 0) { exec @ARGV; exit 127; }
      eval {
        local $SIG{ALRM} = sub {
          kill("TERM", $pid);
          for (1..3) { last unless kill(0, $pid); select(undef,undef,undef,0.5); }
          kill("KILL", $pid) if kill(0, $pid);
          die "timeout\n";
        };
        alarm($timeout);
        waitpid($pid, 0);
        alarm(0);
      };
      if ($@ =~ /timeout/) { waitpid($pid, 0); exit 124; }
      exit($? >> 8);
    ' "$secs" "$@"
  fi
}

# ---------------------------------------------------------------------------
# _parse_utc_timestamp — convert "2026-03-26T14:39:38Z" to epoch seconds
#   Timestamps are UTC (suffix Z). macOS date -j interprets as local time
#   unless we force TZ=UTC.
# ---------------------------------------------------------------------------
_parse_utc_timestamp() {
  local ts="$1"
  # macOS: TZ=UTC date -j
  TZ=UTC date -j -f "%Y-%m-%dT%H:%M:%SZ" "$ts" "+%s" 2>/dev/null && return 0
  # GNU/Linux: date -d
  date -d "$ts" "+%s" 2>/dev/null && return 0
  # [L-6] Warn on parse failure instead of silent "0"
  echo "0"
  echo "[threat-level] WARNING: cannot parse timestamp: $ts" >&2
}

# ---------------------------------------------------------------------------
# _utc_cutoff — compute ISO 8601 cutoff timestamp for awk string comparison
#   Usage: _utc_cutoff <seconds_ago>
# ---------------------------------------------------------------------------
_utc_cutoff() {
  local secs_ago="$1"
  # macOS
  TZ=UTC date -j -v-${secs_ago}S +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null && return 0
  # GNU/Linux
  date -u -d "@$(($(date +%s) - secs_ago))" +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null && return 0
  echo "1970-01-01T00:00:00Z"
}

# ---------------------------------------------------------------------------
# _find_contract — locate nearest CONTRACT.toml walking up from CWD
#   Shared by inject-contract.sh and session-init.sh [M-6]
# ---------------------------------------------------------------------------
_find_contract() {
  local dir="$1"
  while [[ "$dir" != "/" ]]; do
    if [[ -f "$dir/CONTRACT.toml" ]]; then
      printf '%s' "$dir/CONTRACT.toml"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  return 1
}

# ---------------------------------------------------------------------------
# _extract_toml_array — extract TOML array values [M-6]
#   Usage: _extract_toml_array <key> <file>
#   Outputs one value per line, stripped of quotes and whitespace
# ---------------------------------------------------------------------------
_extract_toml_array() {
  local key="$1" file="$2"
  awk -v key="$key" '
    $0 ~ "^"key"[[:space:]]*=" { found=1 }
    found { print }
    found && /\]/ { found=0 }
  ' "$file" | sed 's/^[^=]*=//; s/\[//g; s/\]//g; s/"//g; s/,/\
/g' \
    | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' \
    | grep -v '^[[:space:]]*$'
}

# ---------------------------------------------------------------------------
# _write_audit_entry — append compact JSONL to audit file [M-7]
#   Usage: _write_audit_entry <json_string>
# ---------------------------------------------------------------------------
_write_audit_entry() {
  local entry="$1"
  [[ -z "${entry:-}" ]] && return 0
  mkdir -p "$DUDUCLAW_HOME" 2>/dev/null || true
  # Single-line printf is atomic for < PIPE_BUF on POSIX
  printf '%s\n' "$entry" >> "$AUDIT_FILE" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# get_threat_level — read current level, auto-degrade if stale
# ---------------------------------------------------------------------------
get_threat_level() {
  mkdir -p "$DUDUCLAW_HOME" 2>/dev/null || true

  # Default to GREEN if file doesn't exist
  if [[ ! -f "$THREAT_LEVEL_FILE" ]]; then
    echo "GREEN"
    return 0
  fi

  local level
  level="$(cat "$THREAT_LEVEL_FILE" 2>/dev/null || echo "GREEN")"

  # Auto-degrade if no events in 24h
  if [[ "$level" != "GREEN" ]]; then
    local last_event_ts
    last_event_ts="$(tail -1 "$THREAT_EVENTS_FILE" 2>/dev/null | jq -r '.timestamp // empty' 2>/dev/null || true)"

    if [[ -n "$last_event_ts" ]]; then
      local last_epoch now_epoch
      last_epoch="$(_parse_utc_timestamp "$last_event_ts")"
      now_epoch="$(date "+%s")"

      local elapsed=$(( now_epoch - last_epoch ))
      if (( elapsed > DEGRADE_AFTER_SECONDS )); then
        case "$level" in
          RED)    level="YELLOW"; set_threat_level "YELLOW" "auto_degrade" "24h timeout from RED" ;;
          YELLOW) level="GREEN";  set_threat_level "GREEN" "auto_degrade" "24h timeout from YELLOW" ;;
        esac
      fi
    fi
  fi

  echo "$level"
}

# ---------------------------------------------------------------------------
# set_threat_level — write new level atomically and log the transition [M-5]
# ---------------------------------------------------------------------------
set_threat_level() {
  local new_level="$1"
  local reason="${2:-manual}"
  local detail="${3:-}"

  mkdir -p "$DUDUCLAW_HOME" 2>/dev/null || true

  local old_level
  old_level="$(cat "$THREAT_LEVEL_FILE" 2>/dev/null || echo "GREEN")"

  # Atomic write via tmp + mv [M-5]
  printf '%s\n' "$new_level" > "${THREAT_LEVEL_FILE}.tmp" && \
    mv "${THREAT_LEVEL_FILE}.tmp" "$THREAT_LEVEL_FILE"

  # Log transition to audit file
  if [[ "$old_level" != "$new_level" ]]; then
    local severity="warning"
    [[ "$new_level" == "RED" ]] && severity="critical"
    [[ "$new_level" == "GREEN" ]] && severity="info"

    local entry
    entry="$(jq -cn \
      --arg ts "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
      --arg evt "threat_level_change" \
      --arg aid "${DUDUCLAW_AGENT_ID:-claude-code-session}" \
      --arg old "$old_level" \
      --arg new "$new_level" \
      --arg reason "$reason" \
      --arg detail "$detail" \
      --arg sev "$severity" \
      '{
        timestamp: $ts,
        event_type: $evt,
        agent_id: $aid,
        severity: $sev,
        details: { old_level: $old, new_level: $new, reason: $reason, detail: $detail }
      }' 2>/dev/null)" || true

    _write_audit_entry "$entry"
  fi
}

# ---------------------------------------------------------------------------
# record_threat_event — append event
# ---------------------------------------------------------------------------
record_threat_event() {
  local event_type="$1"
  local detail="${2:-}"

  mkdir -p "$DUDUCLAW_HOME" 2>/dev/null || true

  local entry
  entry="$(jq -cn \
    --arg ts "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
    --arg type "$event_type" \
    --arg detail "$detail" \
    '{ timestamp: $ts, type: $type, detail: $detail }' 2>/dev/null)" || true

  if [[ -n "${entry:-}" ]]; then
    printf '%s\n' "$entry" >> "$THREAT_EVENTS_FILE" 2>/dev/null || true
  fi

  # Prune events older than 24h to prevent file growth
  _prune_old_events
}

# ---------------------------------------------------------------------------
# count_recent_events — count events in the last N seconds [H-7]
#   Uses single awk pass with ISO 8601 string comparison (no per-line fork)
# ---------------------------------------------------------------------------
count_recent_events() {
  local window_seconds="${1:-3600}"

  if [[ ! -f "$THREAT_EVENTS_FILE" ]]; then
    echo "0"
    return 0
  fi

  local cutoff
  cutoff="$(_utc_cutoff "$window_seconds")"

  awk -F'"' -v cutoff="$cutoff" '
    /\"timestamp\"/ {
      for (i=1; i<=NF; i++) {
        if ($(i) == "timestamp" && $(i+1) ~ /:/) { ts=$(i+2); break }
      }
      if (ts >= cutoff) count++
    }
    END { print count+0 }
  ' "$THREAT_EVENTS_FILE"
}

# ---------------------------------------------------------------------------
# evaluate_escalation — check thresholds and escalate/degrade [CR-4]
#   Fixed: also search threat_events.jsonl for injection-like patterns
#   written by bash-gate.sh when Layer 1 detects tool_abuse patterns
# ---------------------------------------------------------------------------
evaluate_escalation() {
  local current_level
  current_level="$(get_threat_level)"

  local recent_count
  recent_count="$(count_recent_events 3600)"

  case "$current_level" in
    GREEN)
      if (( recent_count >= ESCALATE_TO_YELLOW_COUNT )); then
        set_threat_level "YELLOW" "auto_escalate" "${recent_count} events in last hour"
        echo "YELLOW"
        return 0
      fi
      ;;
    YELLOW)
      # [CR-4] Check for critical events using precise jq type field matching [H-2]
      local critical_count=0
      if [[ -f "$THREAT_EVENTS_FILE" ]]; then
        critical_count="$(jq -r 'select(.type == "prompt_injection" or .type == "soul_drift" or .type == "critical_blocked") | .type' "$THREAT_EVENTS_FILE" 2>/dev/null | wc -l | tr -d ' ')"
      fi
      if (( critical_count > 0 )); then
        set_threat_level "RED" "auto_escalate" "Critical event detected"
        echo "RED"
        return 0
      fi
      ;;
    RED)
      # RED stays until auto-degrade in get_threat_level()
      ;;
  esac

  echo "$current_level"
}

# ---------------------------------------------------------------------------
# _prune_old_events — remove events older than 24h [M-1]
#   Uses mktemp + trap for cleanup on interruption
# ---------------------------------------------------------------------------
_prune_old_events() {
  [[ ! -f "$THREAT_EVENTS_FILE" ]] && return 0

  local cutoff
  cutoff="$(_utc_cutoff 86400)"

  local tmp_file
  tmp_file="$(mktemp "${THREAT_EVENTS_FILE}.tmp.XXXXXX" 2>/dev/null)" || return 0
  trap "rm -f '$tmp_file'" RETURN

  awk -F'"' -v cutoff="$cutoff" '
    /\"timestamp\"/ {
      for (i=1; i<=NF; i++) {
        if ($(i) == "timestamp" && $(i+1) ~ /:/) { ts=$(i+2); break }
      }
      if (ts >= cutoff) print
    }
  ' "$THREAT_EVENTS_FILE" > "$tmp_file" 2>/dev/null

  mv "$tmp_file" "$THREAT_EVENTS_FILE" 2>/dev/null || rm -f "$tmp_file"
  trap - RETURN
}
