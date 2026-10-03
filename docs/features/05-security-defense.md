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
- an agent writes **its own `SOUL.md`**, even in the right place. Personality is operator-managed. The hook has no opt-in: an agent explicitly opted in with `agent.toml [permissions] can_modify_own_soul = true` changes its own `SOUL.md` through the `agent_update_soul` MCP tool, never by writing the file;
- an agent writes **its own `CONTRACT.toml`**, even in the right place (decision `BlockedOwnContractWrite`). The contract is the operator's boundary on the agent, so there is no opt-in flag at all; the block message tells the agent to ask the operator, who changes it from the dashboard (`contract.update`, admin only, which does not pass through this hook);
- an agent touches **another agent's** files at all;
- an agent writes, moves or deletes anything under **`agents/_trash/`**, where removed employees are kept (Bash by heuristic);
- an agent runs **`duduclaw agent create <name>`** from Bash for a name that is reserved because a removed employee had it (see [Delegation isolation](37-delegation-isolation.md#a-removed-employees-name-stays-reserved)); the refusal is audited as `agent_name_reserved` with `path_kind` `cli_bash_agent_create`.

For Bash, the own-`SOUL.md` and own-`CONTRACT.toml` rules are a heuristic: a write-shaped command that names the file, as `agents/<self>/…` or as a relative spelling such as `CONTRACT.toml` or `./CONTRACT.toml`, is blocked. That is a speed bump. A command that hides the file name (a variable, an encoded string, a script) can get past it; real isolation is not giving the agent Bash.

Live forking (`fork_run`) respects the same files from the other side: branches can read the agent's structure files, but promoting a branch back into the agent directory never copies `SOUL.md`, `CONTRACT.toml`, `agent.toml`, `.mcp.json`, `.claude/` or the other agent-structure files over the parent's copies.

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

Patterns cover English and Chinese (Traditional and Simplified). Text is NFKC-normalized first (`unicode_normalizer`), so homograph and invisible-character tricks cannot slip past a pattern.

**Chinese coverage (v1.67.1).** Released versions matched Chinese instruction override only as four exact strings, so inserting a word such as 所有, 之前 or 的 got past it; in a live test four such sentences were stored through `user_profile_record`. Since v1.67.1:

- `instruction_override`: an override verb (忽略／無視／忘記／忘掉／不要理會／不用理會／別管, and Simplified forms) followed, inside the same clause and within 12 characters, by an instruction noun (指示／指令／規則／提示詞／系統提示) with a scope word between them (先前／之前／以上／上面／上述／前面／所有／全部／一切／你的／原本／原來). Whitespace is ignored; `。！？；` and line breaks end a clause. Same weight and immediate block as the English phrases.
- `system_prompt_extraction`: an extraction noun (系統提示詞／系統提示語／你的系統提示／你的指示／你的設定) and an output verb (輸出／顯示／告訴我／給我看／洩漏／列出／重複) within 12 characters of each other, either order. Scored like the English rule: weight 30, not a block on its own. Bare 系統提示 is not an extraction noun, because it also means "system notice".
- `role_hijack`: fixed phrases such as 你現在是管理員模式 / 開發者模式 / 越獄模式, 你現在不受限制, 進入越獄模式 and the bare word 越獄模式. Same as English.
- Thresholds and the English lists are unchanged.

**Known false positives.** The rule matches by shape, so ordinary sentences with an override verb, a scope word and an instruction noun in one short clause are blocked too, for example 「請忽略之前寄的指示，以新版為準」, 「請忽略以上規則中的第三條，已經取消」 and 「忘記之前的規則了，可以再說一次嗎」 (pinned by the test `known_benign_shapes_that_do_match`). Any mention of 越獄模式 also blocks. The workaround is to say the same thing without the override verb, for example 「之前的指示作廢，以新版為準」; the close sentence 「之前的報價單請作廢，以新的指示為準」 is in the test list of text that must not match. The scanner is a phrase heuristic, not a classifier, and has not been measured against real conversation data.

`termination_manipulation` (LoopTrap, arXiv:2605.05846) is deliberately not an instant block: weight 30 sits below the threshold, so a single match warns and audits rather than blocking, which keeps ordinary "please continue" requests working.

Where a match shows up (verified call sites):

- Inbound chat messages (`channel_reply`, `scan_input_with_audit`): a blocked message gets a warning reply and is not passed to the AI.
- MCP tool calls (`mcp_dispatch`, `scan_input_with_audit` over the serialized arguments): a call whose arguments quote a blocked sentence is refused and audited.
- Conversation-fact, profile and knowledge-routing distillation (`wiki_ingest`, `profile_distill`, `knowledge_route`): content is dropped on **any** rule match, including the non-blocking extraction rule.
- `user_profile_record`: the predicate and the value are scanned; a block-level hit is refused.
- `duduclaw migrate-from` imports skip blocked items; expert-pack installation refuses a blocked pack.
- Agent Mail: an inbound mail that matches is stored but flagged, and a flagged mail never triggers the agent.
- Reminders: a reminder whose prompt is blocked does not run.

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

**Per-agent key isolation** — MCP API keys and connector credentials are per-agent and resolved through `secret_ref`, so one agent's leak is not a platform leak. Channel credentials are split: LINE, WhatsApp, Feishu, Google Chat, Teams, WeCom and DingTalk use the deployment-wide credentials in `config.toml [channels]`; per-employee bot tokens exist for Telegram, Discord and Slack only (an employee without its own token falls back up the `reports_to` chain, then to the global token).

**Chat commands on channels (v1.68.0)** — `!STOP`, `!STOP ALL`, `!RESUME` and `/model <name>` need an admin. On WhatsApp, Feishu, Teams, WeCom, Google Chat and DingTalk the gateway used to pass `is_admin = true` for every sender, so anyone who could message the bot could stop or resume it. Those channels now compare the sender id or conversation id, exactly, with the channel's `admin_users` setting (global scope; Google Chat and Teams can now have one); with no list, nobody is an admin. On WebChat only an active dashboard account with the Admin role counts, and website widget visitors never do.

**Kill-switch triggers (v1.68.0)** — the four `KILLSWITCH.toml [triggers]` thresholds had no reader before. Now a key is enforced only when it is written in the file and in range, and the Security page has a checkbox per trigger (unticking sends `null`, which removes it). The file is re-read when it changes. `cost_limit_usd` compares 24-hour spend across all employees and, when reached, sets the global failsafe level to restricted until the failsafe recovers or someone sends `!RESUME`; `max_replies_per_minute` counts per conversation and silently drops the excess; `max_consecutive_errors` and `error_rate_threshold` (last 20 replies, at least 10) raise that conversation's failsafe level by one step. Each trip is audited as `killswitch_trigger`. The `[audit]` section of `KILLSWITCH.toml` is no longer read.

**Redaction source protections (v1.68.0)** — the 資料來源保護 switches on the Privacy / Redaction tab now act: `user_input` redacts a channel message before it reaches the AI, `system_prompt` redacts the assembled prompt (by default only rules marked `apply_to_system_prompt`), `cron_context` redacts the trigger message of a condition script. An error stops that turn instead of sending unredacted text. The `sub_agent` switch was removed. `purge_after_expire_days` now drives the vault cleanup.

**Permission flags (v1.68.0)** — `agent.toml [permissions]` `can_create_agents`, `can_send_cross_agent`, `can_modify_own_skills` and `can_schedule_tasks` are enforced at the MCP dispatch gate when written as `false` (refusal audited as `permission_denied`). A one-time boot migration turns old template `false` values into `true`; see [dashboard-settings.md](../guides/dashboard-settings.md#employee-edit-page).

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
