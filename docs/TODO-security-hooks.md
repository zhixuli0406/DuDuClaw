# TODO: Security Hooks — Hybrid Plan (Plan 6)

> DuDuClaw Claude Code Hook Security Enhancement
> Created: 2026-03-26
> Status: Phase 1 COMPLETE (2026-03-26) — Phase 2 COMPLETE (2026-03-26) — Phase 3 COMPLETE (2026-03-26) — Code Review Fixes COMPLETE (2026-03-27)

---

## Overview

Three-phase progressive deployment. Each phase is independently functional.
Later phases build on top of earlier ones — see **Conflict Notes** for integration points.

---

## Phase 1: Immediate Defense (Est. 0.5 day)

> Goal: Block known dangerous patterns + protect critical files + inject contract context

### 1.1 PreToolUse — Bash Command Blacklist

- [x] Create `.claude/hooks/bash-blacklist.sh`
- [x] Implement dangerous command pattern matching:
  - `rm -rf /`
  - `DROP TABLE` / `DELETE FROM.*WHERE 1`
  - `:(){:|:&};:` (fork bomb)
  - `curl.*| sh` / `wget.*| bash` (remote execution)
  - `chmod 777`
  - `> /dev/sda`
  - `mkfs.`
  - `dd if=.*of=/dev/`
- [x] Output: `permissionDecision: "deny"` with reason on match
- [x] Output: `exit 0` (no JSON) on safe commands
- [x] Add hook to `.claude/settings.local.json`:
  ```json
  {
    "hooks": {
      "PreToolUse": [
        {
          "matcher": "Bash",
          "hooks": [{
            "type": "command",
            "command": ".claude/hooks/bash-blacklist.sh",
            "timeout": 5
          }]
        }
      ]
    }
  }
  ```

> **CONFLICT NOTE [P1→P2]**: Phase 2 adds async audit logging to PostToolUse for ALL tools.
> This hook does NOT conflict — they are different events (Pre vs Post).
>
> **CONFLICT NOTE [P1→P3]**: Phase 3 upgrades this same `PreToolUse` Bash matcher.
> Must merge into a single dispatcher script (see §3.1) instead of adding a second matcher entry.
> Two matchers on `"Bash"` both execute — but the deny/allow logic can race.
> **Resolution**: Phase 3 replaces this script with `bash-gate.sh` that includes blacklist as Layer 1.

---

### 1.2 PreToolUse — Sensitive File Protection

- [x] Create `.claude/hooks/file-protect.sh`
- [x] Block writes to protected paths:
  - `~/.duduclaw/secret.key`
  - `**/SOUL.md`
  - `**/CONTRACT.toml`
  - `~/.claude/settings.json` (global settings)
  - `**/.credentials.json`
  - `**/.env` / `**/.env.*`
- [x] Read `tool_input.file_path` from stdin JSON
- [x] Resolve symlinks before comparison (prevent traversal bypass)
- [x] Add hook:
  ```json
  {
    "matcher": "Write|Edit",
    "hooks": [{
      "type": "command",
      "command": ".claude/hooks/file-protect.sh",
      "timeout": 5
    }]
  }
  ```

> **CONFLICT NOTE [P1→P2]**: Phase 2 does NOT add another PreToolUse for Write|Edit.
> No conflict.
>
> **CONFLICT NOTE [P1→P3]**: Phase 3 may add agent-type hook for Write|Edit on security files.
> The command hook (this) runs first; if it denies, agent hook never fires.
> **This is the desired behavior** — deterministic deny takes precedence over AI review.
> No change needed, but document the execution order in the script header.

---

### 1.3 PostToolUse — Secret Leak Scanner

- [x] Create `.claude/hooks/secret-scanner.sh`
- [x] Scan written file content for patterns:
  - `sk-[a-zA-Z0-9]{20,}` (OpenAI/Anthropic keys)
  - `ghp_[a-zA-Z0-9]{36}` (GitHub PAT)
  - `AKIA[0-9A-Z]{16}` (AWS access key)
  - `-----BEGIN (RSA |EC )?PRIVATE KEY-----`
  - `xoxb-[0-9]+-[0-9a-zA-Z]+` (Slack bot token)
  - Generic high-entropy base64 strings > 40 chars adjacent to `key`, `token`, `secret`, `password`
- [x] Read `tool_input.file_path` from stdin, then scan file on disk
- [x] On match: output `{ "decision": "block", "reason": "..." }`
- [x] Add hook:
  ```json
  {
    "matcher": "Write|Edit",
    "hooks": [{
      "type": "command",
      "command": ".claude/hooks/secret-scanner.sh",
      "timeout": 10
    }]
  }
  ```

> **CONFLICT NOTE [P1→P2]**: Phase 2 adds a SECOND PostToolUse entry for ALL tools (audit logger).
> Both fire independently — no conflict. But be aware:
> - secret-scanner matches `Write|Edit` only
> - audit-logger matches all (empty matcher or `.*`)
> - If secret-scanner blocks, audit-logger still fires (PostToolUse hooks all execute)
> - **Desired**: audit-logger should log the block event. Ensure audit-logger reads
>   the tool result (which will contain the block message).

---

### 1.4 UserPromptSubmit — Contract Context Injection

- [x] Create `.claude/hooks/inject-contract.sh`
- [x] On session start, read current agent's `CONTRACT.toml` (if exists)
- [x] Extract `must_not[]` and `must_always[]` rules
- [ ] Output as `additionalContext` string:
  ```json
  {
    "additionalContext": "ACTIVE CONTRACT RULES:\nMUST NOT: ...\nMUST ALWAYS: ..."
  }
  ```
- [x] If no CONTRACT.toml found, `exit 0` silently
- [x] Add hook:
  ```json
  {
    "hooks": {
      "UserPromptSubmit": [
        {
          "hooks": [{
            "type": "command",
            "command": ".claude/hooks/inject-contract.sh",
            "timeout": 5
          }]
        }
      ]
    }
  }
  ```

> **CONFLICT NOTE**: No other phase touches `UserPromptSubmit`. No conflict.

---

### Phase 1 Verification Checklist

- [x] All 4 scripts are executable (`chmod +x`)
- [x] Test each script with mock JSON input via pipe
- [x] Test blacklist: `echo '{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}' | .claude/hooks/bash-blacklist.sh`
- [x] Test file protect: verify secret.key / .env / .credentials.json write is denied
- [x] Test secret scanner: write a file with API key / private key and verify block
- [x] Test contract injection: verify additionalContext appears with CONTRACT.toml
- [ ] Verify normal development workflow is not disrupted

---

## Phase 2: Audit Integration (Est. +1 day)

> Goal: Full traceability + environment hardening + config tamper detection
> Prerequisite: Phase 1 complete

### 2.1 SessionStart — Environment Initialization

- [x] Create `.claude/hooks/session-init.sh`
- [x] Verify `~/.duduclaw/secret.key` exists and has permissions `0600`
  - If wrong permissions: warn via stderr, attempt `chmod 600`
- [x] Write decrypted env vars to `$CLAUDE_ENV_FILE` (if set)
- [x] Parse current agent's `CONTRACT.toml` → cache to `~/.duduclaw/.contract_cache.json`
  (reused by §1.4 inject-contract.sh to avoid re-parsing per prompt)
- [x] Add hook:
  ```json
  {
    "hooks": {
      "SessionStart": [
        {
          "hooks": [{
            "type": "command",
            "command": ".claude/hooks/session-init.sh",
            "timeout": 15
          }]
        }
      ]
    }
  }
  ```

> **CONFLICT NOTE [P2→P1]**: RESOLVED
> Phase 1's `inject-contract.sh` already reads from cache first, falling back to direct TOML parse.
> Phase 2's `session-init.sh` generates the cache at `~/.duduclaw/.contract_cache.json`.
> No further changes needed.

---

### 2.2 PostToolUse — Async Audit Logger

- [x] Create `.claude/hooks/audit-logger.sh`
- [x] Append JSONL entry to `~/.duduclaw/security_audit.jsonl`:
  ```json
  {
    "timestamp": "ISO8601",
    "session_id": "$CLAUDE_SESSION_ID",
    "event": "tool_use",
    "tool": "<tool_name>",
    "input_summary": "<truncated first 200 chars>",
    "result_summary": "<truncated first 200 chars>",
    "severity": "info"
  }
  ```
- [x] Use `flock` for file-level locking (consistent with existing audit.rs)
- [x] Set `async: true` to avoid blocking tool execution
- [x] Add hook:
  ```json
  {
    "matcher": "",
    "hooks": [{
      "type": "command",
      "command": ".claude/hooks/audit-logger.sh",
      "timeout": 10,
      "async": true
    }]
  }
  ```

> **CONFLICT NOTE [P2→P1]**: Phase 1 has a PostToolUse for `Write|Edit` (secret-scanner).
> Phase 2 adds a PostToolUse with empty matcher (matches ALL tools).
> Both hooks fire independently — audit-logger runs for ALL tools, secret-scanner only for Write|Edit.
> **Execution order**: Hooks in the same event execute in array order.
> **Ensure in settings.json**: secret-scanner entry comes BEFORE audit-logger entry.
> ```json
> "PostToolUse": [
>   { "matcher": "Write|Edit", "hooks": [{ "command": "secret-scanner.sh" }] },
>   { "matcher": "",            "hooks": [{ "command": "audit-logger.sh", "async": true }] }
> ]
> ```
> This way secret-scanner can block synchronously, then audit-logger records the outcome async.

---

### 2.3 ConfigChange — Tamper Detection

- [x] Create `.claude/hooks/config-guard.sh`
- [x] Log config change event to `security_audit.jsonl` with severity `critical`
- [x] Include: source, file_path, session_id, timestamp
- [x] If source is `project_settings` or `user_settings`, output warning as `additionalContext`
- [x] Add hook:
  ```json
  {
    "hooks": {
      "ConfigChange": [
        {
          "hooks": [{
            "type": "command",
            "command": ".claude/hooks/config-guard.sh",
            "timeout": 5
          }]
        }
      ]
    }
  }
  ```

> **CONFLICT NOTE**: No other phase touches `ConfigChange`. No conflict.

---

### Phase 2 Verification Checklist

- [x] Verify SessionStart hook fires on new session
- [x] Verify `secret.key` permissions are checked
- [x] Verify `contract_cache.json` is created
- [x] Update `inject-contract.sh` to use cache (see §2.1 conflict note — RESOLVED, already supported)
- [x] Verify audit-logger writes compact JSONL entries for every tool use
- [x] Verify flock/fallback locking for macOS compatibility
- [x] Verify ConfigChange fires with critical severity for settings changes
- [x] Run full Phase 1 tests again to ensure no regression

---

## Phase 3: AI-Powered Escalation (Est. +2 days)

> Goal: Adaptive threat response + AI judgment for unknown attack patterns
> Prerequisite: Phase 1 + Phase 2 complete

### 3.1 Threat Level State Machine

- [x] Create `.claude/hooks/lib/threat-level.sh` (shared library)
- [x] State file: `~/.duduclaw/threat_level` (contains `GREEN`, `YELLOW`, or `RED`)
- [x] State file: `~/.duduclaw/threat_events.jsonl` (recent block/injection events)
- [x] Functions:
  - `get_threat_level()` — read current level, auto-degrade if >24h since last event
  - `record_threat_event(type, detail)` — append to events file
  - `evaluate_escalation()` — count events in last 1h, escalate if threshold met
- [x] Escalation rules:
  - GREEN → YELLOW: ≥ 2 blocked events in 1 hour
  - YELLOW → RED: prompt injection detected OR SOUL.md drift
  - RED → YELLOW: 24h with no events
  - YELLOW → GREEN: 24h with no events
- [x] UTC timestamp parsing (`TZ=UTC date -j` for macOS compatibility)
- [x] Portable `_timeout` via perl fallback (macOS lacks `timeout`)

> **CONFLICT NOTE**: This is a shared library, not a hook itself. No direct conflict.
> All Phase 3 hooks source this file: `. .claude/hooks/lib/threat-level.sh`

---

### 3.2 PreToolUse — Unified Bash Gate (replaces §1.1)

- [x] Create `.claude/hooks/bash-gate.sh` (replaces `bash-blacklist.sh`)
- [x] Layer 1: Deterministic blacklist (same as §1.1, <50ms)
- [x] Layer 2 (YELLOW+): Extended parameter inspection
  - Check for obfuscated commands (`base64 -d`, `eval`, `$(...)` nesting >2 levels)
  - Check for network exfiltration (`curl`, `wget`, `nc` to non-localhost, with safe-list for github/crates.io/etc)
- [x] Layer 3 (RED only): Haiku AI judgment
  - Call `claude -p --model haiku` with `CLAUDECODE=` unset (bypass nested session check)
  - Prompt: security review of bash command in context of Rust project
  - Timeout: 60s via perl-based `_timeout` (Haiku startup takes ~35s)
  - On timeout: default to deny (fail-closed)
- [x] `bash-blacklist.sh` renamed to `.bak`, settings.local.json updated (timeout: 90s)

> #### **CRITICAL CONFLICT NOTE [P3→P1]**
>
> This script **replaces** Phase 1's `bash-blacklist.sh`.
>
> **Migration steps**:
> 1. Copy all blacklist patterns from `bash-blacklist.sh` into `bash-gate.sh` Layer 1
> 2. Update `.claude/settings.local.json`: change the PreToolUse Bash matcher's command
>    from `bash-blacklist.sh` → `bash-gate.sh`
> 3. **Do NOT have both scripts in the same matcher** — two PreToolUse Bash entries
>    would both execute, and conflicting deny/allow decisions cause undefined behavior
> 4. Keep `bash-blacklist.sh` as backup, rename to `bash-blacklist.sh.bak`
> 5. Test: all Phase 1 blacklist tests must still pass under `bash-gate.sh`
>
> **settings.json diff**:
> ```diff
>  "PreToolUse": [
>    {
>      "matcher": "Bash",
>      "hooks": [{
>        "type": "command",
> -      "command": ".claude/hooks/bash-blacklist.sh",
> +      "command": ".claude/hooks/bash-gate.sh",
> -      "timeout": 5
> +      "timeout": 15
>      }]
>    },
> ```
> Timeout increased from 5→15 to accommodate potential Haiku call in RED mode.

---

### 3.3 Stop — Threat Level Re-evaluation

- [x] Create `.claude/hooks/threat-eval.sh`
- [x] Source `lib/threat-level.sh`
- [x] Call `evaluate_escalation()` to recalculate threat level
- [x] If level changed, log to `security_audit.jsonl` (reuse Phase 2 audit format)
- [x] In RED mode: invoke Haiku prompt to review session summary
  - Prompt: "Review this session for security concerns: {last 20 tool calls from audit log}"
  - If Haiku flags issues: output `{ "decision": "block", "reason": "..." }`
- [x] GREEN/YELLOW fast path: <200ms (verified)
- [x] Add hook:
  ```json
  {
    "hooks": {
      "Stop": [
        {
          "hooks": [{
            "type": "command",
            "command": ".claude/hooks/threat-eval.sh",
            "timeout": 30
          }]
        }
      ]
    }
  }
  ```

> **CONFLICT NOTE**: No other phase uses `Stop` hooks. No conflict.
> But beware: Stop fires on EVERY assistant response, not just task completion.
> The script must be lightweight in GREEN/YELLOW mode (just read + compare counts).
> Haiku call only in RED mode.

---

### 3.4 PreToolUse — AI Review for Security File Edits (optional)

- [x] Only activate when threat_level is RED
- [x] Scope: Write|Edit to files matching security paths (`duduclaw-security/`, `contract`, `hooks/`, `security`)
- [x] Uses `claude -p --model haiku` to review proposed changes vs existing file
  - Reads existing content + proposed change (first 2000 chars each)
  - Evaluates if the change weakens security
- [x] Timeout: 45s (fail-closed in RED mode)
- [x] GREEN/YELLOW: instant skip (<100ms verified)

> **CONFLICT NOTE [P3→P1]**: Phase 1's `file-protect.sh` runs on the same PreToolUse Write|Edit matcher.
> **Execution order matters**:
> 1. `file-protect.sh` (command, fast) — denies writes to SOUL.md/CONTRACT.toml/secret.key
> 2. AI review (agent, slow) — only fires if file-protect.sh allowed the write
>
> **Implementation**: Add as a SECOND hook in the same matcher entry's hooks array:
> ```json
> {
>   "matcher": "Write|Edit",
>   "hooks": [
>     { "type": "command", "command": ".claude/hooks/file-protect.sh", "timeout": 5 },
>     { "type": "command", "command": ".claude/hooks/security-file-ai-review.sh", "timeout": 60 }
>   ]
> }
> ```
> Hooks in the same array execute sequentially. If the first denies, the second is skipped.

---

### Phase 3 Verification Checklist

- [x] Verify threat_level state machine transitions correctly (GREEN→YELLOW→RED)
- [x] Verify UTC timestamp parsing with `TZ=UTC` (fixed timezone offset bug)
- [x] Verify `bash-gate.sh` passes ALL Phase 1 blacklist tests (6/6)
- [x] Verify Layer 2 catches obfuscated commands in YELLOW mode (base64, nested subshell, /dev/tcp, external curl)
- [x] Verify Layer 2 allows safe patterns (localhost, github.com, crates.io)
- [x] Verify Haiku call fires only in RED mode — `cargo build` allowed, obfuscated python denied
- [x] Verify Haiku timeout defaults to deny (fail-closed)
- [x] Verify Stop hook is lightweight in GREEN (191ms) and YELLOW (152ms) modes
- [x] Run full Phase 1 + Phase 2 + Phase 3 regression — all passed
- [ ] Cost check: estimate Haiku API spend under simulated RED scenario (~$0.001/call, ~35s/call)

---

## Phase 4: Code Review Fixes (2026-03-27)

> All 6 CRITICAL, 10 HIGH, 12 MEDIUM, 7 LOW issues from `code-review-security-hooks.md` resolved.

### Files Modified

| File | Fixes Applied |
|------|--------------|
| `lib/threat-level.sh` | CR-4 (YELLOW→RED via `critical_blocked`), H-7 (awk-based `count_recent_events`), M-1 (mktemp+trap in prune), M-5 (atomic write), M-6 (shared `_find_contract`/`_extract_toml_array`), M-7 (shared `_write_audit_entry`), L-5 (zombie cleanup in `_timeout`), L-6 (parse warning) |
| `bash-gate.sh` | CR-1 (threat state file patterns), CR-2 (strict whitelist + compound reject), CR-3 (XML delimiters), H-1 (`[[:space:]]` globally), H-2 (hostname-position URL check), H-4 (expanded eval/bash-c), H-9 (bare command `$` anchor), M-10 (`unset CLAUDECODE`) |
| `file-protect.sh` | CR-1 (threat_level/threat_events/security_audit paths), H-10 (Read tool support), M-4 (symlink resolve for new files), Architecture (SOUL.md/CONTRACT.toml with DUDUCLAW_EVOLUTION bypass) |
| `secret-scanner.sh` | H-1 (`[[:space:]]`), H-3 (MIME allowlist), H-6 (5MB limit), M-3 (Stripe/npm/Vault/DB URL/GCP patterns), L-7 (`additionalContext` instead of `decision:block`) |
| `audit-logger.sh` | CR-5 (`agent_id` + `details` object for Rust AuditEvent compat), CR-6 (async in settings), L-1 (simplified to atomic printf) |
| `session-init.sh` | H-5 (`.env.claude` validation, reject PATH/LD_PRELOAD), M-6 (shared lib functions) |
| `threat-eval.sh` | CR-3 (XML `<audit_log>` delimiters), H-8 (internal 25s, settings 45s), M-7 (shared `_write_audit_entry`), M-10 (`unset CLAUDECODE`) |
| `security-file-ai-review.sh` | CR-3 (XML `<existing>`/`<proposed>` delimiters), M-9 (precise crate/basename matching), M-10 (`unset CLAUDECODE`) |
| `inject-contract.sh` | M-6 (shared lib functions), M-8 (`$'\n'` real newlines) |
| `settings.local.json` | CR-6 (`async: true`), H-8 (Stop: 45s), H-10 (Read matcher), timeout alignment (Bash: 60s) |

### Deferred Items (future work)

| ID | Issue | Reason |
|----|-------|--------|
| M-11 | Per-agent threat_level isolation | Requires `DUDUCLAW_AGENT_ID` env propagation from Rust layer |
| M-12 | Per-agent contract cache isolation | Same dependency as M-11 |
| L-2 | TOML parser multi-line edge cases | Low risk; full TOML parser would require external dependency |
| L-3 | SHA-256 integrity check on settings.json | Requires `shasum` availability; low priority |
| L-4 | Shebang for threat-level.sh | Library file sourced, not executed; risk is theoretical |

---

## Conflict Summary Matrix

Cross-phase hook interactions at a glance:

| Hook Event | Phase 1 | Phase 2 | Phase 3 | Phase 4 (Review Fix) | Conflict? |
|------------|---------|---------|---------|---------------------|-----------|
| **SessionStart** | — | `session-init.sh` | — | H-5 env validation | None |
| **UserPromptSubmit** | `inject-contract.sh` | — | — | M-6/M-8 shared lib | None |
| **PreToolUse** (Bash) | ~~`bash-blacklist.sh`~~ | — | `bash-gate.sh` | CR-1/2/3, H-1/2/4/9 | **RESOLVED** |
| **PreToolUse** (Write\|Edit) | `file-protect.sh` | — | `security-file-ai-review.sh` | CR-1, SOUL.md protect | **RESOLVED** |
| **PreToolUse** (Read) | — | — | — | H-10 `file-protect.sh` | **NEW** |
| **PostToolUse** (Write\|Edit) | `secret-scanner.sh` | — | — | H-3/6, M-3, L-7 | None |
| **PostToolUse** (all) | — | `audit-logger.sh` | — | CR-5/6, async:true | **RESOLVED** |
| **ConfigChange** | — | `config-guard.sh` | — | — | None |
| **Stop** | — | — | `threat-eval.sh` | CR-3, H-8 | None |

### Key Integration Rules

1. **Same event, same matcher** → merge into single matcher entry with sequential hooks array
2. **Same event, different matcher** → separate entries, both fire independently
3. **P3 replaces P1 script** → rename old script to `.bak`, update settings.json in single commit
4. **async hooks** → always place AFTER synchronous hooks in array order
5. **Phase 2 cache** → update Phase 1 scripts to read cache (backward-compatible enhancement)

---

## File Structure

```
.claude/
├── settings.local.json          # Hook configuration (all phases)
└── hooks/
    ├── lib/
    │   └── threat-level.sh      # [P3] Shared threat state library
    ├── bash-blacklist.sh.bak    # [P1] Archived — replaced by bash-gate.sh
    ├── bash-gate.sh             # [P3] Unified bash gate (3-layer defense)
    ├── file-protect.sh          # [P1] Sensitive file write protection
    ├── secret-scanner.sh        # [P1] Post-write secret leak scanner
    ├── inject-contract.sh       # [P1] CONTRACT.toml context injection
    ├── session-init.sh          # [P2] Environment init + key verification
    ├── audit-logger.sh          # [P2] Async audit trail
    ├── config-guard.sh          # [P2] Config tamper detection
    ├── threat-eval.sh           # [P3] Stop-time threat evaluation
    └── security-file-ai-review.sh  # [P3] Optional AI review for security edits
```
