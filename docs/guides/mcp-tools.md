# MCP tools: what an agent is shown, and why

DuDuClaw's MCP server advertises its tools through the standard `tools/list`
call. This page explains two things that are easy to get wrong:

1. why an agent sees fewer tools than the server actually implements, and
2. where the long-form details went, now that every tool description is capped.

If you are adding a tool, read
[custom-mcp-tool.md](custom-mcp-tool.md) first; this page is about the
declaration surface, not the implementation.

## The rule: discoverable ⇔ callable

`tools/list` only lists the tools the calling agent can actually call right now.
Every filter below mirrors a gate the dispatcher already enforces, so the list
an agent reads and the calls the server accepts describe the same set.

This matters for cost as much as correctness. The tool schemas are a *fixed*
prompt cost paid on every spawn: the CLI reads `tools/list` once per session and
the whole payload lands in the model's context before the first user token. A
schema for a tool the gate would reject is paid for twice — the tokens, and the
model planning around something it cannot use.

Hiding a tool is **not** an authorization decision. Calling an unlisted tool
still reaches the real gate and is still refused, with the gate's own message.

### What is filtered

| Filter | Source | Effect when off / empty |
|---|---|---|
| External client whitelist | `principal.is_external` | Exactly 7 tools are listed |
| Google Workspace | `config.toml [integrations] google_workspace` | 19 `gmail_*` / `calendar_*` / `sheets_*` / `forms_*` / `gtasks_*` / `drive_*` / `docs_*` / `slides_*` tools hidden |
| GitHub | `config.toml [integrations] github` | 5 `github_*` tools hidden |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | Denied tools hidden; a non-empty allowlist hides everything else. An entry matches a tool exactly, or by a trailing `*`: `*`, `mcp__duduclaw__*` (every DuDuClaw tool), `mcp__duduclaw__odoo_*` / `memory_*` (anchored prefix). `mcp__<other server>__…` never names a DuDuClaw tool; a `*` elsewhere is literal. The approval lists and `scoped_tools` use the same rule |
| `os_native` | `agent.toml [capabilities]` | 6 `os_*` automation tools hidden |
| `recording` | `agent.toml [capabilities]` | 5 browser/desktop recording tools hidden |
| `system_operator` | `agent.toml [capabilities]` | 19 appliance operation tools hidden |
| `codrive` | `agent.toml [capabilities]` | `codrive_run` / `codrive_status` hidden |
| `computer_use` | `agent.toml [capabilities]` | 8 `computer_*` tools hidden (see [`computer_*`](#computer_--sessions-run-by-the-gateway) below) |
| `db_sources` | `agent.toml [capabilities]` | 4 `db_*` tools hidden |
| `[fork] enabled` | `agent.toml` | 6 forking tools hidden |
| `scoped_tools` | `agent.toml [capabilities]` + live grant | Hidden until a task-scoped grant is active |

A freshly created agent has none of these opted in, which is the point: the
default deployment pays for the tools it can use and nothing else.

### How a mid-session change reaches the agent

Hiding a tool used to make it permanently unreachable, because an MCP client
reads `tools/list` once when the session opens. The server now declares
`tools.listChanged` in its `initialize` response and emits
`notifications/tools/list_changed` whenever the caller's visible set actually
changes — compared as a set, so touching a config file without changing it
sends nothing.

So this sequence works without a restart:

1. An agent hits a `scoped_tools` denial, or an operator grants a capability.
2. `capability_request` is approved (or the dashboard saves `agent_update`, or
   someone edits `agent.toml`).
3. Within a few seconds the server notices the change and notifies the client.
4. The client re-reads `tools/list` and the tool is there.

A task-scoped grant is revoked at every task terminal state, and the same
mechanism takes the tool back out of view.

### Caveat: an agent cannot self-discover a `scoped_tools` name

A tool listed in `scoped_tools` is hidden until a grant is active, so the agent
cannot read the name off `tools/list` in order to ask for it. Tell it the name
another way — in its `SOUL.md`, in a playbook rule, or by minting the grant at
goal kickoff with a `grant:<tool>` tag. This is the one place the pruning costs
something, and it is deliberate: advertising a tool that is denied right now is
the failure mode the rule above exists to remove.

### Exceptions since v1.68.0: listed but refused

Two new gates refuse a call without removing the tool from `tools/list`:

- `agent.toml [permissions]`: a flag written as `false` refuses `create_agent` (`can_create_agents`); `send_to_agent`, `spawn_agent` (`can_send_cross_agent`); `create_reminder` and `tasks_create` with a `schedule` (`can_schedule_tasks`); `skill_hub_install`, `shared_skill_adopt`, `skill_graduate`, `skill_pin`, `skill_from_recording` (`can_modify_own_skills`). The refusal is JSON-RPC error -32003 and a `permission_denied` audit event. An `agent.toml` that exists but cannot be read or parsed refuses these tools; a missing one allows them. `tasks_create` without a `schedule` stays allowed, which is why these flags are checked per call. Ephemeral role members are scaffolded with `can_create_agents`, `can_modify_own_skills` and `can_schedule_tasks` set to `false`.
- `config.toml [odoo] features_*`: Odoo tools stay listed and are refused per call for models of a switched-off module (project and hr are off by default).

### Record relationship checks: listed but refused

Some tools change or trigger a record that belongs to an AI employee. They stay
listed for every caller and check the relationship per call:

- **Tasks**: `tasks_update` and `activity_post` with a `task_id` pass for the
  task's assignee, claimer or creator; `tasks_complete` and `tasks_block` pass
  for its assignee or claimer; `tasks_claim` passes for an unassigned task or
  one already assigned to the caller. Anything else needs a delegation
  relationship with the assignee (same department, `reports_to` above or below,
  or a whitelist pair, per `[delegation] policy`). An unassigned, unclaimed task
  must be claimed first. Taking another employee's task for yourself through
  `tasks_update` always needs the relationship.
- **Task fields**: an AI employee cannot change `title` or `description` of a
  goal-mode task (`acceptance_criteria` is refused for every MCP caller), and
  cannot add, remove or reorder tags starting with `outcome:` or `grant:` or the
  `auto-research` tag, in `tasks_update` or in `tasks_create`'s `tags`.
- **Routines**: `update_cron_task`, `delete_cron_task`, `pause_cron_task` and
  `run_cron_task` need the caller to be the employee the routine runs as, or to
  have the relationship with it. Addressed by `name`, they act on exactly one
  routine; a shared name is refused with the candidate ids.
- **Reminders**: `create_reminder` with an `agent_id` other than the caller
  needs the relationship with that employee.
- **`agent_update` on yourself**: an AI employee cannot send `reports_to`,
  `db_sources`, `db_sources_add`, `db_sources_remove`, `budget_cents` or `role`
  about itself (audit `agent_authority_refused`). Editing a subordinate is
  unchanged.

An operator (an MCP key that maps to no AI employee, in a process that is not
running for one) is not restricted. The shared internal key in a process with
no employee identity owns no record, so it is refused on everyone's. A process
whose identity is a system sender name (`dashboard`, `cron`, …) is treated as
untrusted. Every refusal is audited in `tool_calls.jsonl`. Details:
[task board](../features/24-task-board.md),
[delegation isolation](../features/37-delegation-isolation.md).

## The description budget

Each tool's `description` is capped at **200 bytes** and each parameter's
description at **200 bytes** (one documented exception, below). The cap is
enforced by a test, not by convention.

Two consequences for anyone adding or editing a tool:

- **Say what the tool does and what will refuse it.** Safety-critical clauses —
  "this does not send", "this is publicly visible", "rejected whole, never
  truncated" — stay in the description. Rationale, worked examples and design
  labels do not.
- **Put the long version here.** Link a section of this page, or the tool's own
  spec page, from the description.

### The one exception

`team_handoff`'s `packet` parameter carries the full TaskPacket shape (capped at
1,024 bytes). `build_tool_schema` types every parameter as a bare JSON-Schema
string, so a parameter description is the only place its real shape can be
stated — and a TaskPacket is rejected whole rather than truncated, so an agent
that cannot see the shape cannot produce a valid one. The full reference is
[../spec/task-packet.md](../spec/task-packet.md).

Exceptions live in one list (`PARAM_CAP_EXEMPTIONS`) with a test that fails when
an exemption stops being necessary.

## Long-form details moved out of descriptions

### `codrive_run` — the three-rung execution ladder

Each step is dispatched through three rungs, tried in this order; prefer the
highest one that applies.

- **C-L2 — `api_action`.** Calls a registered third-party app's native
  API/CLI/D-Bus action for `target_app` before touching the GUI at all. Prefer
  this whenever the app and action you need are registered. `action` is a short
  registered identifier (chromium's `open_url`, networkmanager's `state`, …);
  `params` is that action's payload, validated against the action's own schema
  at dispatch time. A registry miss or an exec failure falls through to the
  step's `action` field — which is why `action` is required even here.
- **C-L3 — `locate`.** Resolves a `move`/`click` step's coordinates by
  `(role, name)` lookup in `target_app`'s AT-SPI2 accessibility tree instead of
  hand-guessed pixels. Far more robust to layout, resolution and theme changes.
  Ignored for `text` / `key_name` / `wait` / `take_over`. A miss falls back to
  the literal `x`/`y`.
- **C-L1 — literal `x`/`y`.** The fallback when the other two are absent or fail.

Other script-level rules:

- `target_app` is a **single script-level field**, not per-step. One script
  drives one app; issue a second `codrive_run` call to drive another.
- Every consequential step (`send` / `submit` / `delete` / `purchase` / `other`)
  pauses for human approval before any rung dispatches it. A refuse-list hit
  (banking pages, CAPTCHA bypass, …) is rejected outright, before a connection
  is attempted.
- A login/password/payment step (`take_over`, or a `credential` class) hands the
  shared desktop to a human. You never send the credential text yourself through
  any rung; the script resumes when they hand control back.
- Any human input on the shared desktop immediately freezes the agent's seat.
  The dropped step is retried once a person hands control back.
- `watch_mode: true` arms idle-based supervision for the rest of the run.
- Maximum 50 steps.

### `working_state_handoff` — structured mode

Two modes:

- **Plain note.** Omit `status`; legacy behaviour, silently truncated at about
  1,200 characters.
- **Structured (Ralph-loop style).** Pass `status` and it is validated together
  with `next_steps` / `evidence` / `blocker`:
  - `continue` — requires non-empty `next_steps`, forbids `blocker`.
  - `complete` — requires non-empty `evidence`, forbids both `blocker` and
    `next_steps`. A self-declared "done" with no evidence, or with a leftover
    next step, is rejected: "I finished" is not evidence.
  - `blocked` — requires a non-empty, specific `blocker`.

  The combined payload is capped by `config.toml [memory]
  working_state_handoff_max_bytes` (default 16,384, CJK-safe byte count). Going
  over **rejects the whole call**; it is never silently truncated, because
  truncating could delete exactly the evidence that made the handoff
  authoritative.

### `skill_search` — picking a source

Leave `source` alone unless you already know where the skill lives.

- `all` (default) — configured hubs plus this agent's learned skill bank,
  de-duplicated by name and labelled with its source. Hub results are ranked by
  relevance × trust × install count × freshness, with official first-party
  skills floored into the top results.
- `github` — a skill you expect in a public GitHub repo.
- `hub` — the curated registries (`anthropic-skills`, `github`, `clawhub`,
  `lobehub`, `skills-sh`); narrow to one with `hub`.
- `bank` — only what this deployment learned on its own. `hub` is not accepted
  with this source.

### `evolution_toggle` — the stagnation sub-fields

Beyond the standard flags, `field` accepts `stagnation_enabled` (bool),
`stagnation_window_seconds` (60–604800), `stagnation_trigger_threshold`
(1–1000) and `stagnation_action` (`log_only` | `suppress`). See
[evolution-switches.md](evolution-switches.md).

### `execute_program` — where the script runs

The script runs in the script sandbox: a container from the image in
`config.toml [container.sandbox] image` (the same image as the
[task sandbox](task-sandbox.md), never pulled automatically). The container
runs as the host user (`1000:1000` when the host process is root, and always on
WSL2), with all capabilities dropped, `no-new-privileges`, a read-only root
filesystem, no network, 2 GiB of memory with no swap, 256 processes, one CPU
and a small `/tmp` tmpfs. Only a private directory holding the script is
mounted, read-only, at `/workspace`. `timeout_seconds` (default 30, at most
300) applies under a hard cap of 600 seconds, stdout and stderr come back as
one output (the read is capped at 2 MiB, the reply at 1 MiB), and the container
is force-removed if the call is cancelled. Docker is used on macOS and Linux;
on Windows WSL2 is tried first, then Docker.

When the sandbox cannot run (no Docker, image missing, invalid
`[container.sandbox]`, …) the script is **not run**: the tool returns
`Script sandbox unavailable (<code>): …` with the `docker pull <image>`
command and writes the audit event `script_sandbox_unavailable`. Older versions
silently ran the script on the host instead. To get that back, set
`[container.sandbox] script_when_unavailable = "run_unsandboxed"` (a separate
key from the task sandbox's `when_unavailable`); every host run is then
audited as `script_sandbox_bypassed`.

A script cannot call platform tools back: there is no RPC socket inside the
container.

### `computer_*` — sessions run by the gateway

The eight tools drive one computer-use session per employee: an isolated
container with a virtual display and a kiosk browser. The MCP server only
forwards each call to the gateway over loopback
(`POST /api/internal/computer-use`, signed per request); the gateway owns the
container and runs every check, so the gateway must be running. Hidden unless
`agent.toml [capabilities] computer_use = true`.

| Tool | Parameters | Notes |
|---|---|---|
| `computer_session_start` | `task` string, optional; `width` integer 320–1920; `height` integer 240–1200 | One session per employee. The result lists the limits, whether high-risk actions can be confirmed in a chat, and the sites `computer_navigate` can open |
| `computer_screenshot` | none | MCP image block (PNG, masked) followed by a text block with actions used and time left. A fully masked picture is reported as such in the text, with the reason (several windows, sensitive or unreadable front window, detection failure) and the next step |
| `computer_click` | `x`, `y` integers (required); `button` string `left`/`right`; `double` boolean | `double` is left button only |
| `computer_type` | `text` string (required), 1–2,000 characters | Audited as a character count only |
| `computer_key` | `key` string (required): letters, digits, `+`, `-`, `_` | e.g. `Return`, `ctrl+s` |
| `computer_scroll` | `x`, `y` integers (required); `direction` string `up`/`down` (default `down`); `amount` integer 1–20 (default 3) | |
| `computer_navigate` | `url` string (required) | `https://` only, host exactly on the employee's `[capabilities.computer_use_config] allowed_domains` and resolved at session start, port absent or 443, no user name or password, at most 2,000 bytes. With no allowlist the session has no network and the call is refused |
| `computer_session_stop` | `session_id` string, optional | Removes the container |

Integer and boolean parameters also accept numeric and `"true"`/`"false"`
strings. Click, type, key, scroll and navigate each count as one action
against `max_actions` (default 50). Limits, the approval and confirmation
rules, the network allowlist and its residual risks are in
[Browser automation](../features/08-browser-automation.md).

### `belief_stats` / `belief_settle` — verified and self-reported settlements

Only a settlement cross-checked against a platform price counts toward
calibration. Nothing in production supplies that cross-check today:
`belief_settle` records every settlement as the employee's own report
(`settle_source = "agent_unverified"`), so calibration reads "no verified
settlements" on every current deployment.

`belief_settle` returns the settled row plus `counts_toward_calibration`
(boolean) and, when it is `false`, a `note` saying the settlement was recorded
but does not count.

`belief_stats` returns (the same object as the dashboard's `belief.summary`
`stats`, plus a `note`):

| Field | Meaning |
|-------|---------|
| `n_submitted` | every belief submitted, settled or not |
| `n_settled_all` | every settled belief (`verified.n + self_reported.n`) |
| `calibration_status` | `no_verified_settlements`, `insufficient_samples` (1–29 verified) or `calibrated` (30 or more) |
| `verified` | `n`, `hits`, and `hit_rate`, `hit_rate_wilson_low`, `mean_brier`, `overconfidence` (all `null` unless `calibrated`) |
| `self_reported` | `n` and a descriptive `hit_rate` (`null` when `n` is 0); not calibration |
| `per_subject[]` | `subject`, `verified` (`n`, `hits`, `mean_brier`), `self_reported` (`n`) |

The flat fields of earlier versions (`n_total`, `n_settled`,
`insufficient_samples`, top-level `hit_rate` …) are gone. See
[Belief loop](../features/46-belief-loop.md).

### `create_agent` / `agent_remove` — removed names are reserved

`agent_remove` moves the employee to `~/.duduclaw/agents/_trash/` and answers
that the employee was removed, that the administrator can restore it, and that
the name is reserved. It returns no path. `create_agent` then refuses that
name for every MCP caller while a trash entry exists, while `org.toml` still
records the id without a directory, or while the trash cannot be listed; a
different name works. Operators can reuse the name from the dashboard or a
terminal. Over HTTP with a non-internal key, both tools act as that key's own
client id. Details:
[Delegation isolation](../features/37-delegation-isolation.md#a-removed-employees-name-stays-reserved).

## Deprecated aliases are still listed

A deprecated tool name keeps appearing in `tools/list` with a
`[deprecated → …]` prefix on its description, because hiding it would make it
uncallable — the opposite of what a deprecation window is for. The full old →
new table is [deprecations.md](deprecations.md). No MCP tool is deprecated at the moment: the aliases of the v1.66.0 window were removed in v1.69.0.

## Related

- [custom-mcp-tool.md](custom-mcp-tool.md) — adding a tool
- [mcp-bridge.md](mcp-bridge.md) — mounting external MCP servers
- [remote-mcp.md](remote-mcp.md) — the HTTP/OAuth transport
- [../spec/task-packet.md](../spec/task-packet.md) — the TaskPacket schema
- [../spec/reversible-context-ccr.md](../spec/reversible-context-ccr.md) — the
  `duduclaw_ccr_*` tools, which are injected by the direct-API tool loop rather
  than dispatched by the MCP server
