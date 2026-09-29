# Security Defense

> Four live guards, where each one runs, and what none of them covers.

---

## A note on history

Until 2026-09 this page described a three-phase shell-script defense: a deterministic blacklist, an obfuscation/exfiltration scanner, and a Haiku AI judgment layer, all living in `.claude/hooks/` and orchestrated by a GREEN/YELLOW/RED threat-level state machine.

Those scripts were deleted in commit `ba015a48` when `.claude/` was taken out of the public repository, and `.claude/` is gitignored today. Nothing in the shipped binary reads them. There is no threat-level state machine.

What is actually in the product is smaller and easier to reason about: **two PreToolUse hooks** — both Rust subcommands — that the gateway installs into every agent directory, **one input scanner** on the message path, and **one field-level freeze** over the files that decide who may command whom.

---

## Guard 1 — `agent-file-guard` (PreToolUse, Rust)

`duduclaw hook agent-file-guard` is a real subcommand rather than a shell script, so it behaves identically on macOS, Linux and Windows. The gateway registers it in `<agent_dir>/.claude/settings.json` with the matcher `Write|Edit|MultiEdit|Bash` and re-registers it on every boot (`agent_hook_installer`), merging into whatever else the operator has configured instead of clobbering it.

It exits 2 — which Claude Code reads as "block this tool call" — when:

- an agent writes an **agent-structure file** (`agent.toml`, `SOUL.md`, `CLAUDE.md`, `.mcp.json`, …) outside the canonical `<home>/agents/<name>/` tree. Scaffolding a new agent has to go through the `create_agent` MCP tool, which carries the delegation-authorization gate;
- an agent writes **its own `SOUL.md`**, even in the right place. Personality is operator-managed. The one exception is an agent that has explicitly opted in with `agent.toml [permissions] can_modify_own_soul = true`, and even then only for itself;
- an agent touches **another agent's** files at all.

## Guard 2 — `data-file-guard` (PreToolUse, Rust, RFC-23 §14.4)

Guard 1 protects DuDuClaw's own structure files. This one protects the customer's data.

`Read` and `Bash` are built-in Claude Code tools, so `cat customers.csv` never passes the MCP redaction choke point that `file_read` / `csv_read` / `xlsx_read` go through. The installer registers `duduclaw hook data-file-guard` for the matcher `Read|Bash`; the decision logic lives in `duduclaw_core::data_file_guard`, shared by the CLI subcommand and the gateway's installer tests. Same contract as Guard 1: exit 0 allows, exit 2 plus stderr blocks, and the stderr is shown to the model.

It stays inert unless the gateway sets `DUDUCLAW_DATA_FILE_GUARD` at spawn time, which it does only when redaction is actually active for that agent. A deployment with redaction off behaves exactly as it did before the guard existed.

Until H10 (2026-09) this was a POSIX shell script at `<agent_dir>/.claude/hooks/data-file-guard.sh`, and it was **inert on a Windows host with no bash on `PATH`** — the hook command failed, and Claude Code reads a non-2 exit (including "command not found") as *allow*, so the guard went missing exactly where nobody would notice. The installer now deletes any leftover script on upgrade, so a stale copy cannot be mistaken for the live guard.

**Stated limitation.** The `Bash` check matches filenames. A command that builds its path dynamically (`python -c "open(chr(99)+…)"`) walks straight past it. The real protection is the MCP tool surface; this guard lowers the odds of the model taking the ungated route. It is a heuristic, not a sandbox.

## Guard 3 — `input_guard` (prompt-injection scanner, Rust library)

`duduclaw_security::input_guard::scan_input` scores text 0–100 across **seven rule categories** and blocks at or above `DEFAULT_BLOCK_THRESHOLD` (60):

| Rule | Weight | Instant block |
|---|---|---|
| `instruction_override` | 40 | yes |
| `role_hijack` | 35 | yes |
| `tool_abuse` | 30 | yes |
| `data_exfiltration` | 25 | yes |
| `system_prompt_extraction` | 30 | no |
| `encoding_bypass` | 25 | no |
| `termination_manipulation` | 30 | no |

Patterns cover English and zh-TW, since the platform's primary language is Traditional Chinese. Text is NFKC-normalized first (`unicode_normalizer`), so homograph and invisible-character tricks cannot slip past a pattern.

`termination_manipulation` (LoopTrap, arXiv:2605.05846) is deliberately not an instant block: weight 30 sits below the threshold, so a single match warns and audits rather than blocking, which keeps ordinary "please continue" requests working.

Call sites: the MCP dispatch front door (`scan_input_with_audit`), `duduclaw migrate-from` imports, expert-pack installation, and skill vetting — anywhere untrusted text crosses into an agent's context.

## Guard 4 — `org_field_guard` (organizational authority freeze)

The A2A delegation predicate (`delegation_policy::can_delegate`) decides who may command whom by reading `[agent] reports_to` / `department` / `name` from `agent.toml`, plus `[delegation]` and `[acp]` from `config.toml`. Both are plain files, so an agent holding `Edit` could rewrite its own `reports_to` to point at a victim and then claim the "subordinate → ancestor" rule. The judged party owned the evidence.

`org_field_guard` runs inside the same `agent-file-guard` hook and compares the reconstructed *post-write* content field by field against what is on disk. A change to a protected field or section is denied. The `[capabilities]` table is frozen as a whole table rather than as a key list, so a capability key added in a later release is protected the day it lands instead of the day someone remembers to extend a list.

Fail-closed by construction: unparseable new content, unparseable existing content, and an unreconstructable write intent all deny. A file that does not exist yet is allowed, because creation goes through `create_agent` and its own gate.

Legitimate changes keep every route they had: the MCP `agent_update` tool and the dashboard `agents.update` RPC, neither of which passes through the hook.

---

## Supporting layers

**MCP authorization gate** — every MCP tool is enumerated in a scope table; a tool that is not listed defaults to requiring Admin scope. Scope, per-agent capability grants and `denied_tools` are each enforced at the dispatcher front door, and every refusal is audited with an `error_class`.

**SOUL.md drift detection** — `soul_guard` fingerprints each `SOUL.md` with SHA-256 at startup and on every heartbeat tick, keeps up to 10 versioned backups in `.soul_history/`, and reports drift alongside an Agent Stability Index.

**Audit trail** — `tool_calls.jsonl` records every tool call with masked `result_text` / `input_text` (three-pass secret masking, mask before truncate), `0600` permissions, hash-chained lines, and rotation at 16 MB. `security_audit.jsonl` carries security events separately. The same log is the evidence source the grounding precheck and the acceptance judge read, so weakening it weakens verification too.

**Per-agent key isolation** — MCP API keys, channel tokens and connector credentials are per-agent and resolved through `secret_ref`, so one agent's leak is not a platform leak.

---

## What these guards do not cover

Saying this plainly is part of the defense.

- **The hooks see Claude Code's own tool calls, not MCP tool calls.** MCP has its own gate (scopes, grants, `denied_tools`); the hooks are the second lock, on the built-in `Write` / `Edit` / `Read` / `Bash` surface.
- **`data-file-guard` is a heuristic.** It matches filenames in a `Bash` command line; a dynamically-built path defeats it. (It is no longer inert on Windows — H10 made it a Rust subcommand.)
- **There is no threat-level state machine.** `~/.duduclaw/threat_level` survives as an operator-controlled kill switch that the computer-use orchestrator polls (`RED` stops the run, `YELLOW` pauses it), but nothing inside the workspace writes it. Absent or unreadable means `GREEN`.
- *(Removed 2026-09.)* This section used to note that the PTY session pool sat outside the redaction rewrite. That pool no longer exists — every Claude spawn is a per-call spawn, which is exactly what the rewrite hooks into.

---

## Interaction with other systems

- **CONTRACT.toml** defines what an agent must never do, and `duduclaw test` red-teams it. The guards enforce at the tool-call level.
- **Evolution engine** — because `SOUL.md` is read-only to agents, the evolving artifact is the playbook. See [38-aee-playbook-evolution.md](38-aee-playbook-evolution.md).
- **Redaction and data sources** — see [55-data-sources.md](55-data-sources.md) for the pipeline `data-file-guard` complements.
- **Delegation isolation** — see [37-delegation-isolation.md](37-delegation-isolation.md) for the predicate `org_field_guard` protects.

---

## The takeaway

Four guards with stated failure modes beat a three-layer story with no code behind it. When a defense is removed the documentation has to go with it: a page describing a shell script that does not exist is worse than no page, because it makes an operator stop looking.
