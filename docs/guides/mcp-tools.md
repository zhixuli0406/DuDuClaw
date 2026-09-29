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
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | Denied tools hidden; a non-empty allowlist hides everything else |
| `os_native` | `agent.toml [capabilities]` | 6 `os_*` automation tools hidden |
| `recording` | `agent.toml [capabilities]` | 5 browser/desktop recording tools hidden |
| `system_operator` | `agent.toml [capabilities]` | 19 appliance operation tools hidden |
| `codrive` | `agent.toml [capabilities]` | `codrive_run` / `codrive_status` hidden |
| `computer_use` | `agent.toml [capabilities]` | 7 `computer_*` tools hidden |
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

## Deprecated aliases are still listed

A deprecated tool name keeps appearing in `tools/list` with a
`[deprecated → …]` prefix on its description, because hiding it would make it
uncallable — the opposite of what a deprecation window is for. The full old →
new table is [deprecations.md](deprecations.md).

## Related

- [custom-mcp-tool.md](custom-mcp-tool.md) — adding a tool
- [mcp-bridge.md](mcp-bridge.md) — mounting external MCP servers
- [remote-mcp.md](remote-mcp.md) — the HTTP/OAuth transport
- [../spec/task-packet.md](../spec/task-packet.md) — the TaskPacket schema
- [../spec/reversible-context-ccr.md](../spec/reversible-context-ccr.md) — the
  `duduclaw_ccr_*` tools, which are injected by the direct-API tool loop rather
  than dispatched by the MCP server
