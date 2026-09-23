#!/usr/bin/env bash
# RFC-23 §14.4 — data-file guard (Claude Code PreToolUse hook).
#
# Installed into <agent_dir>/.claude/hooks/ by
# `duduclaw-gateway::agent_hook_installer` and registered as a PreToolUse hook
# for `Read` and `Bash`. Its job is to keep the built-in file-reading route
# from bypassing DuDuClaw's de-identification: `Read` and `Bash` are NOT MCP
# tools, so whatever they return goes straight into the model's context
# without passing the redaction choke point. `csv_read` / `xlsx_read` /
# `file_read` are the de-identified route; this hook is what makes the model
# take it.
#
# Contract (same as `duduclaw hook agent-file-guard`):
#   exit 0 → allow, exit 2 + stderr → block, stderr is shown to the model.
#
# Mode comes from DUDUCLAW_DATA_FILE_GUARD, set by the gateway at spawn time
# ONLY when redaction is actually active for that agent:
#   on         block Read of a data file AND Bash naming one
#   read_only  block Read only
#   off/unset  allow everything (byte-identical to not having this hook)
#
# HONEST LIMITATION 1 — heuristic, not a sandbox: the Bash check matches
# filenames. A command that builds its path dynamically
# (`python -c "open(chr(99)+...)"`) walks past it. The real protection is the
# MCP tool surface; this hook lowers the odds of the model taking the wrong
# road by accident.
#
# HONEST LIMITATION 2 — POSIX only: this is a shell script, unlike its sibling
# `agent-file-guard`, which is a Rust subcommand specifically so it runs on
# Windows. On a Windows host without a bash on PATH the hook command fails and
# the tool call proceeds (Claude Code treats a non-2 exit as allow), i.e. the
# guard is absent there. Data read that way still reaches the model unredacted,
# so a Windows deployment should not rely on the guard — only on the MCP tools.
# Porting this to `duduclaw hook data-file-guard` would close it.

set -u

MODE="${DUDUCLAW_DATA_FILE_GUARD:-off}"
case "$MODE" in
  on | read_only) ;;
  *) exit 0 ;;
esac

# Data-file extensions, as one alternation reused by both checks.
EXTS='csv|tsv|xlsx|xlsm|xls|ods'

DENY_MESSAGE='此檔案受去識別化保護，請改用 csv_read／xlsx_read／file_read'

payload="$(cat)"
[ -n "$payload" ] || exit 0

# ── Envelope parsing ────────────────────────────────────────────────────────
# python3 is the accurate path (JSON escapes in a Bash command are real). The
# sed fallback keeps the guard working on a minimal image with no python; both
# fail OPEN on a payload they cannot read, matching agent-file-guard's
# behaviour on a malformed envelope — a guard must not brick an agent over a
# parse error it did not cause.
tool_name=""
field_value=""

if command -v python3 >/dev/null 2>&1; then
  parsed="$(
    printf '%s' "$payload" | python3 -c '
import json, sys
try:
    env = json.load(sys.stdin)
except Exception:
    sys.exit(0)
if not isinstance(env, dict):
    sys.exit(0)
name = env.get("tool_name") or ""
ti = env.get("tool_input") or {}
if not isinstance(ti, dict):
    ti = {}
value = ti.get("file_path") if name == "Read" else ti.get("command")
if not isinstance(value, str):
    value = ""
# NUL-free single line out: the two fields cannot contain a newline that
# matters to us, and the shell reads them positionally.
print(str(name).replace("\n", " "))
print(value.replace("\n", " "))
' 2>/dev/null
  )"
  tool_name="$(printf '%s\n' "$parsed" | sed -n '1p')"
  field_value="$(printf '%s\n' "$parsed" | sed -n '2p')"
else
  flat="$(printf '%s' "$payload" | tr '\n' ' ')"
  tool_name="$(printf '%s' "$flat" |
    sed -n 's/.*"tool_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
  if [ "$tool_name" = "Read" ]; then
    field_value="$(printf '%s' "$flat" |
      sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
  else
    # Without a JSON parser an escaped quote inside the command truncates the
    # capture. Truncating EARLY is the safe direction: we may miss a filename
    # and allow, never invent one and block.
    field_value="$(printf '%s' "$flat" |
      sed -n 's/.*"command"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
  fi
fi

[ -n "$tool_name" ] || exit 0
[ -n "$field_value" ] || exit 0

deny() {
  printf '%s\n' "$DENY_MESSAGE" >&2
  printf '%s\n' "（被 DuDuClaw 資料檔守門攔截：$1）" >&2
  exit 2
}

case "$tool_name" in
  Read)
    # Extension test on the path's own tail, case-insensitive.
    if printf '%s' "$field_value" | grep -Eiq "\.($EXTS)[[:space:]]*$"; then
      deny "$tool_name $field_value"
    fi
    ;;
  Bash)
    [ "$MODE" = "on" ] || exit 0
    # A token ending in a data-file extension: the character after the
    # extension must not continue the word, so `.xlsxnote` does not match but
    # `report.xlsx`, `"a.csv"`, and `x.csv;` all do.
    if printf '%s' "$field_value" | grep -Eiq "\.($EXTS)([^A-Za-z0-9_]|$)"; then
      deny "$tool_name"
    fi
    ;;
  *) exit 0 ;;
esac

exit 0
