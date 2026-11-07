# MCP tools: what an agent is shown, and why

DuDuClaw's MCP server advertises its tools through the standard `tools/list`
call. This page explains two things that are easy to get wrong:

1. why an agent sees fewer tools than the server actually implements, and
2. where the long-form details went, now that every tool description is capped.

If you are adding a tool, read
[custom-mcp-tool.md](custom-mcp-tool.md) first; this page is about the
declaration surface, not the implementation. To run the server on its own,
without the gateway, see [mcp-standalone.md](mcp-standalone.md).


## MCP client protocol versions

The API/openai-compat client and remote MCP bridge prefer **2026-07-28**. They
probe with `server/discover` and put the revision, client capabilities and client
identity in each request. HTTP requests include `MCP-Protocol-Version`, `Mcp-Method`
and, where required, `Mcp-Name`. Valid `x-mcp-header` tool arguments are mirrored
into headers; tools with invalid annotations are hidden. Unicode header values
use the protocol's Base64 encoding.

Older servers fall back to **2025-11-25** or **2025-06-18** with `initialize`,
`notifications/initialized` and the negotiated session header. During initial
discovery, `-32022` selects the newest mutually supported revision. Once connected,
the bridge refuses `-32022`, invalidates the connection and requires a conversation
restart to negotiate, list tools and apply the gates again; it never replays the
call under a different revision. The API client also terminates a `tools/call`
that receives `-32022` and refuses all subsequent requests on that client. Its host
must reconnect, list tools and apply the gates again; initial handshake and
standalone list recovery remain available. Explicit modern capability/header errors do not
trigger a downgrade. Each new API HTTP connection performs its own discovery;
there is no global URL-only version cache. The bridge retains negotiation state
only for its own connection. Credentials, sessions and tool lists are not shared
between principals. The API client also tries deprecated HTTP+SSE if legacy initialization returns 400/404/405, accepting
an `endpoint` only on the configured origin.

The API client's modern MRTR uses a fresh request ID, the original tool arguments
and the server's unchanged `requestState` for each retry, with a finite round limit. It
supports a host-provided input handler; without one it declares no interactive
capabilities and refuses unsolicited input requests. There is no new consent UI
or automatic acceptance of elicitation. Tasks, sampling and roots are not enabled
by default. The remote bridge declares no modern interactive capabilities and
refuses every `input_required` result, including state-only results. It also
refuses downstream requests containing `requestState` or `inputResponses` before
sending them upstream. Bridge MRTR remains unavailable until the complete
continuation payload can pass egress and approval checks bound to the original
approved call generation. A broken tool response is an error; an uncertain side
effect is not silently replayed. Tool filters and third-party classification still apply.

Remote OAuth uses pre-registered credentials first. To use CIMD, an administrator
may configure an existing public HTTPS metadata document:

```toml
[remote_mcp]
client_metadata_url = "https://your-domain.example/mcp-client.json"
```

When the authorization server supports CIMD, the document must identify that exact
URL, allow the configured callback URI and use public-client authentication
(`token_endpoint_auth_method = "none"`). Invalid selected metadata fails closed.
Otherwise dynamic registration remains the fallback, with `application_type`.
PKCE and resource indicators remain required; callback `iss` is checked before
code exchange, and saved credentials remain bound to their issuer. Both discovered
and stored OAuth resources must exactly match the MCP endpoint's canonical
resource (scheme, host, port and path, with trailing slashes removed). Resource
query strings, fragments and other paths on the same origin are refused. Older
stored credentials that do not match require reconnection. Automatic
403 scope step-up and CIMD `private_key_jwt` are not implemented.

## Meeting transcription

`meeting_transcribe` takes `source_path` from the calling employee's own attachments,
optional `engine`, `language_hint` (default `zh-TW`) and `write_wiki` (default `false`).
It starts a background job and immediately returns its ID and state. The audio is
sent to the configured transcription provider. Use `meeting_transcribe_status` and
`meeting_transcribe_cancel` with `job_id` for the employee's own jobs. `meeting_upload_link {}` (also used when no usable source path exists) returns a single-upload link for the original conversation. It requires the same
`messaging:send` scope as `transcribe_audio`, plus an authenticated employee identity
and the existing capability, approval and outgoing-data gates. External fallback
namespaces cannot use it. Its effect is a stored-data change with network egress.
The optional engine must match the administrator's selected provider.

Audio is sent to Gemini or OpenAI, and generated text uses Taiwan Traditional Chinese
conversion. RFC-23 masks saved text when enabled; it does not mask uploaded audio.
`drive_file_id` and `write_wiki: true` return explicit unsupported errors in this
version. Transcription runs in the gateway background. See [Meeting transcripts](../features/73-meeting-transcripts.md)
for ffmpeg requirements, provider limits, configuration and privacy.

## MCP credential boundaries

`DUDUCLAW_` environment variables remain unchanged when MCP credentials are
moved into the encrypted store, regardless of the server name. This does not
make a `duduclaw-*` server trusted: only exact host-owned names reserve built-in
slots. Effective rewritten configs replace those slots wholesale with the current
host-generated command, arguments and env; stale keys or a different executable
in raw JSON are never used. Raw `mcp-proxy` invocations are always refused, as are
proxy-private env fields on third-party entries. Forked copies use the original
employee's host-known identity and policy directory, even when the copied file
lives elsewhere.

## Local text to speech

`synthesize_speech` uses providers available in the MCP process: installed local
Kokoro or Piper models. Paid TTS keys are held only by the gateway and are not
available to this process. Audio is returned as base64, in MP3 or WAV format
according to the provider.

## Per-employee tool list pruning

In an employee's overview, Admins can open **Tool list pruning** to select a mode, inspect unused tools and estimated token savings, or restore the list while pruning is on. The selector uses the existing employee update path. The status and restore RPCs are Admin-only: `agents.tool_pruning_status {agent_id}` and `agents.tool_pruning_restore {agent_id}`. Estimates use each candidate tool's serialized definition length divided by four; they are not tokenizer measurements or measured provider savings.

```toml
# agent.toml
[capabilities]
tool_pruning = "shadow" # "off" | "shadow" | "on"
```

The default is `shadow`: observe and report, with the same `tools/list` response as `off`. Unknown or malformed values behave as `off` and warn once. Only `on` hides eligible tools, after all existing discovery filters. This never grants or revokes permission: a hidden tool remains callable through the existing dispatch gates. A successful hidden-tool call makes it visible again through `notifications/tools/list_changed`.

The calculation reads at most the last 5 MB of `tool_calls.jsonl`. Missing or unreadable logs, and fewer than ten recorded turns, leave the list intact. A candidate must be unused across at least ten turns spanning two sessions, or five sessions. Memory, tasks, working state, messaging, approvals, wiki reads, tool discovery, Admin/unknown-effect tools and tools with active scoped task grants stay visible. A turn is a distinct host-stamped `(session_id, turn_id)` pair, not a log line. Channel calls use the conversation session and turn; dispatched work uses its own dispatch session and run (one run is one turn). Older rows without these identities do not establish turn samples, but a successful use still resets that tool’s unused window. Thresholds count the turns and sessions since each tool’s last successful use. Results are cached per process for five minutes. Restore writes an employee-specific reset marker; usage before that marker no longer contributes, so the full eligible list returns. Existing permission filters still apply.

Claude CLI `minimal_context` uses `--tools` for built-in CLI tools; it does not prune MCP definitions. Its behavior is unchanged.

## The rule: discovery follows authorization

`tools/list` only lists the tools the calling agent can actually call right now.
Authorization filters mirror the dispatcher gates. Optional per-employee pruning
then narrows discovery only: an authorized tool can be callable while hidden.

This matters for cost as much as correctness. The tool schemas are a *fixed*
prompt cost paid on every spawn: the CLI reads `tools/list` once per session and
the whole payload lands in the model's context before the first user token. A
schema for a tool the gate would reject is paid for twice — the tokens, and the
model planning around something it cannot use.

Hiding a tool is **not** an authorization decision. Calling an unlisted tool
still reaches the real gates, which accept or refuse it under the existing rules.

### What is filtered

| Filter | Source | Effect when off / empty |
|---|---|---|
| External client whitelist | `principal.is_external` | Only the 7 legacy whitelist tools plus tools in explicitly granted scopes; then the scope row below applies |
| Scopes of a non-employee caller | `principal.scopes`, for a key that holds no `admin` and is not an AI employee (not the gateway-internal key, not a per-agent key, not a process the gateway spawned for an employee) | Only tools whose minimum scope the key holds; tools that act for the process's own agent (`working_state_*`, `canvas_*`, `shared_wiki_delete`, `memory_search_by_layer` and three consolidation reads, …) are hidden from it, and from every external key, and the dispatch gate refuses them by the same rule (`process_agent_tool`). See [mcp-standalone.md](mcp-standalone.md) |
| Google Workspace | `config.toml [integrations] google_workspace` | All 21 Google tools, including `google_status`, are hidden while off. Dashboard setup and tests remain available: [Connect Google](google-setup.md). |
| Google identity (v1.76.0) | `agent.toml [google] identity`, else `config.toml [integrations.google_identity] default_identity` | `none` hides the same 21 tools and the dispatch gate refuses them. Any other value keeps them listed; a call that cannot use the identity returns the reason. `google_status` now starts with who the employee acts as |
| GitHub | `config.toml [integrations] github` | 5 `github_*` tools hidden |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | Denied tools hidden; a non-empty allowlist hides everything else. An entry matches a tool exactly, or by a trailing `*`: `*`, `mcp__duduclaw__*` (every DuDuClaw tool), `mcp__duduclaw__odoo_*` / `memory_*` (anchored prefix). `mcp__<other server>__…` never names a DuDuClaw tool; a `*` elsewhere is literal. The approval lists and `scoped_tools` use the same rule |
| `os_native` | `agent.toml [capabilities]` | 6 `os_*` automation tools hidden |
| `recording` | `agent.toml [capabilities]` | 5 browser/desktop recording tools hidden |
| `system_operator` | `agent.toml [capabilities]` | 19 appliance operation tools hidden |
| `codrive` | `agent.toml [capabilities]` | `codrive_run` / `codrive_status` hidden |
| `computer_use` | `agent.toml [capabilities]` | 8 `computer_*` tools hidden (see [`computer_*`](#computer_--sessions-run-by-the-gateway) below) |
| `computer_use_config.workspace` | `agent.toml [capabilities.computer_use_config]` (with `computer_use`) | 3 `computer_workspace_*` tools hidden (see [`computer_workspace_*`](#computer_workspace_--durable-workspaces) below) |
| `db_sources` | `agent.toml [capabilities]` | 4 `db_*` tools hidden |
| `[fork] enabled` | `agent.toml` | 6 forking tools hidden |
| `[knowledge] enabled` + collection grants | `config.toml` / `knowledge.db` / `agent.toml [knowledge] collections` | 4 `knowledge_*` tools hidden unless the knowledge base is on and at least one collection is granted to the employee or its department (after `[knowledge] collections` narrowing); a grant change sends `list_changed` (see [`knowledge_*`](#knowledge_--document-knowledge-base) below) |
| `[responsibilities] enabled` | `config.toml` | 3 `responsibility_*` tools hidden (see [responsibility tools](#responsibility_get--responsibility_followup--responsibility_ask--responsibility-tools) below) |
| `scoped_tools` | `agent.toml [capabilities]` + live grant | Hidden until a task-scoped grant is active |

The machine-operation console (`/console`) is available only to administrators. Machine-operation tools used by a `system_operator` employee also verify on the server that the person requesting them is an active administrator; ordinary chat with other AI employees remains available according to existing access rules.

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
- **Parent of a new task**: the gateway tells the MCP server which task the
  current round works on (`DUDUCLAW_TASK_ID`, the same value approval cards
  carry). When that task is the caller's own and belongs to a
  [continuous responsibility](continuous-responsibilities.md) run (the run's
  task or any task under it, including a sub-task woken by the task board),
  `tasks_create` (including `kind="goal"`) places the new task under it when no
  `parent_task_id` is given, and a `parent_task_id` the employee gives must be
  that task or one of its sub-tasks, or the call is refused. A value that is
  present but empty or malformed is refused, never read as "no round". In every
  other case (an ordinary goal round, a task-board wake-up of a task outside
  any run, no round information) there is no default parent. New in this
  release for every caller: a given `parent_task_id` needs a relationship to
  the parent (assignee, claimer, creator, or the delegation policy), where v1.69
  wrote it unchecked; `kind="goal"` accepts `parent_task_id`, which v1.69
  ignored; and a task can hold at most 200 unfinished sub-tasks created by AI
  employees (a `schedule`, routine or reminder, is not a sub-task). The round is
  not passed to a server started from Bash, or under the Grok runtime; a task created there is not placed under a run unless the employee
  names the run, and is not counted toward its spending. Correct placement
  also relies on the employee not being able to rewrite the `duduclaw` entry of
  its own `.mcp.json`, a separate platform fix that must ship first.
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
1,024 bytes). `build_tool_schema` types a parameter as a bare JSON-Schema
string unless the short `PARAM_TYPES` list gives it a scalar or string-array
type, so for an object such as a TaskPacket the parameter description is the
only place its real shape can be stated — and a TaskPacket is rejected whole rather than truncated, so an agent
that cannot see the shape cannot produce a valid one. The full reference is
[../spec/task-packet.md](../spec/task-packet.md).

Exceptions live in one list (`PARAM_CAP_EXEMPTIONS`) with a test that fails when
an exemption stops being necessary.

## Tool annotations

Every tool in `tools/list` carries MCP `annotations`, derived from the same
effect table the dispatch gate and `action_rules` use
(`duduclaw_core::effect_of`):

| Effect class | `readOnlyHint` | `destructiveHint` |
|---|---|---|
| `read` | `true` | (omitted) |
| `draft`, `send` | `false` | `false` |
| `publish` | `false` | `true` (a publish can replace what others already read) |
| `modify` | `false` | `true`, except `memory_store`, `user_profile_record`, `wiki_rebuild_fts` (they only add or regenerate). `wiki_export` stays `true`: it overwrites the previous export, including an Obsidian vault someone may have edited |
| `delete`, `purchase`, `admin`, and any unknown name | `false` | `true` |

Memory, wiki, user-profile and `code_map` tools also say `openWorldHint: false`;
`memory_alias_add`, `wiki_export` and `wiki_rebuild_fts` say
`idempotentHint: true`. The overlays live in
`crates/duduclaw-cli/src/mcp/tools_def/annotations.rs`.

The hints are for the MCP client (for example, whether it asks before a call).
They never authorize anything: every DuDuClaw gate still runs on the call. They
are also left out of the description budget above, because Claude Code and Codex
build the model's tool list from name, description and input schema only.

## Long-form details moved out of descriptions

### `tasks_create` / `tasks_update` — goal cost cap

`tasks_create` accepts `cost_cap_cents` with `kind="goal"`: a positive integer in US cents (up to `9223372036854775807`). Omit it to use `[goal_loop] default_cost_cap_cents` (`0` means no default cap). Non-goal and scheduled requests reject the argument. Example: `{"kind":"goal","title":"Review the report","assigned_to":"agnes","cost_cap_cents":500}`. Keep the usual description and acceptance criteria for your goal.

`tasks_update` accepts the same field for a goal. Employees may add or narrow a ceiling, but cannot raise or remove an effective ceiling; raising is an operator action. Saving a new cap does not itself resume a paused goal. The dashboard's **Raise cap and retry** saves it, then uses the existing retry decision. At or above the measured task-tree cap, the goal pauses as `budget_exhausted`; unreadable cost skips dispatch. See [goal loop](../features/34-goal-loop.md).

### `tasks_create` — per-goal run limit

`tasks_create` with `kind="goal"` (not scheduled) accepts `timeout_secs`, an integer from 30 to 3600: the limit for each CLI run / API request of that goal's rounds, replacing the assignee's own run limit. Over MCP it can only be shorter than or equal to that limit; giving a goal more time is an Admin decision on the dashboard. Claude employees ignore it (fixed 30-minute limit). `tasks_update` refuses the field. Example: `{"kind":"goal","title":"Reconcile the ledger","assigned_to":"anna","timeout_secs":180}`.

### `office_script` — bundled office scripts

`office_script` runs a manifest-verified script bundled in the binary for
`docx`, `xlsx`, `pptx`, or `pdf`. Select `create`, `extract`, or `to_pdf`;
`.py` is optional and script names cannot contain paths. Skills installed in an
agent's skill folder may change the prompt/instructions, but cannot replace the
executable script. This tool runs on Linux and macOS.

Pass `args` as a JSON string array, for example `["outline.md", "--out",
"attachments/deck.pptx"]`. Input may come from the agent directory or shared
attachments. Output must be a new file under the agent directory, with an
existing parent directory and the expected extension. Pass each flag and value
as separate array elements; `--key=value`, symlinks and path traversal are
refused. To hand a produced file to the user, end the reply with
`DELIVER:<absolute path>`.

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

### `evolution_toggle` — fields and permissions

`field` accepts `gvu_enabled` (bool), `max_silence_hours`,
`skill_token_budget` and `max_active_skills` (numbers). The old
`stagnation_*` fields are no longer writable. See
[evolution-switches.md](evolution-switches.md).

An AI employee may change its own settings or those of its subordinates,
using the same identity check and organization gate as `agent_update`. The
`open` delegation policy permits cross-team changes; human operators remain
unrestricted. Callers rejected by either check are refused before reading
the target configuration. Self-edits remain allowed because these fields
do not grant tool, data or organization permissions and do not bypass the
evolution master switch.

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

The nine tools drive one computer-use session per employee: an isolated
container with a virtual display and a kiosk browser. The MCP server only
forwards each call to the gateway over loopback
(`POST /api/internal/computer-use`, signed per request); the gateway owns the
container and runs every check, so the gateway must be running. Hidden unless
`agent.toml [capabilities] computer_use = true`.

| Tool | Parameters | Notes |
|---|---|---|
| `computer_session_start` | `task` string, optional; `width` integer 320–1920; `height` integer 240–1200; `workspace` string, optional: `"new"` or a `ws-…` id from `computer_workspace_list` | One session per employee. The result lists the limits, whether high-risk actions can be confirmed in a chat, and the sites `computer_navigate` can open. With `workspace` it also returns `workspace_id`, `mount_path` (`/workspace/files`, read-only, root only), the revision, usage and quota |
| `computer_screenshot` | none | MCP image block (PNG, masked) followed by a text block with actions used and time left. A fully masked picture is reported as such in the text, with the reason (several windows, sensitive or unreadable front window, detection failure) and the next step |
| `computer_click` | `x`, `y` integers (required); `button` string `left`/`right`; `double` boolean | `double` is left button only |
| `computer_type` | `text` string (required), 1–2,000 characters | Audited as a character count only |
| `computer_login` | `credential` (required label string) | Gateway fills and submits a saved login for an allowed employee; only status and a sentence are returned |
| `computer_key` | `key` string (required): letters, digits, `+`, `-`, `_` | e.g. `Return`, `ctrl+s` |
| `computer_scroll` | `x`, `y` integers (required); `direction` string `up`/`down` (default `down`); `amount` integer 1–20 (default 3) | |
| `computer_navigate` | `url` string (required) | `https://` only, host exactly on the employee's `[capabilities.computer_use_config] allowed_domains` and resolved at session start, port absent or 443, no user name or password, at most 2,000 bytes. With no allowlist the session has no network and the call is refused |
| `computer_session_stop` | `session_id` string, optional | Removes the container |

`computer_login` is the ninth session tool. An Admin creates the labeled login in **System → Security → Website login credentials**, with an HTTPS `login_url` and a non-empty allowed-employee list; passwords go in that form, never chat. Optional `allowed_paths` defaults to none and only permits exact same-origin second-step paths with a visible password input after the helper submits step one. The gateway first navigates to the saved URL through the existing navigation checks; this navigation and the login each consume one session action. The helper checks the exact origin/path and effective POST, including the selected submitter's `formmethod` in native `requestSubmit` and fallback paths and rechecks after submit handlers. It intercepts every request and redirect hop to refuse other origins, rejects non-POST queries containing a complete credential value and bypasses service workers. The MCP request carries only the label. Session start and status responses expose the caller's available labels, never usernames. Use the existing takeover flow when the result is `needs_takeover`; no OTP tool is provided. `logged_in` only means the password field disappeared. Failed cleanup (`failed_uncleared`) stops and removes the session and container and returns an error requiring a new session; navigation cannot restore it. Before removing interception, the helper tracks `Network.requestWillBeSent`, `Network.loadingFinished` and `Network.loadingFailed` to confirm requests have ended. It waits for at most three seconds and requires two quiet command barriers with no pending requests. If completion cannot be confirmed, it returns `failed_uncleared` and calls `Browser.close` while the Fetch fence is still active; the gateway then removes the session and container. Screenshots are suppressed during the call.

Login is restricted to the saved origin (scheme, host, port), and form submission must use the same origin. HTTPS is required, including for localhost. Same-origin page JavaScript can read filled fields. 2FA requires a person. Rebuild the computer-use image for the new helper before use.

Integer and boolean parameters also accept numeric and `"true"`/`"false"`
strings. Click, type, login, key, scroll and navigate each count as one action
against `max_actions` (default 50). Limits, the approval and confirmation
rules, the network allowlist and its residual risks are in
[Browser automation](../features/08-browser-automation.md).

### `computer_workspace_*` — durable workspaces

A folder the gateway keeps for one employee across computer-use sessions.
Listed only when `[capabilities] computer_use` and
`[capabilities.computer_use_config] workspace` are both true; every call also
needs `config.toml [computer_use.workspaces] enabled = true` (except that the
owner may still list and read when the feature is switched off). A workspace
of another employee, a malformed id and a missing one all get the same
"not found" answer.

| Tool | Parameters | Notes |
|---|---|---|
| `computer_workspace_list` | none | The caller's workspaces: `state`, `data_revision`, `bytes_used`, `files_used`, `quota`, `expires_at`, `leased`, and for readable ones `files` (`path`, `size`, `sha256`, `hash_unknown`), `files_truncated` (more than 200 files) and `unprocessable_items` (a count; names are never shown) |
| `computer_workspace_read` | `workspace_id` string (required); `path` string (required) | One UTF-8 text file of at most 48 KiB. The content comes back inside a `<computer_workspace_file>` data fence with injection-scan flags |
| `computer_workspace_write` | `workspace_id` string (required); `path` string (required); `content` string (required); `expected_revision` integer, optional | Only into the workspace the caller's live session attached. At most 48 KiB of UTF-8, and the whole request must fit 64 KiB after JSON encoding, so content full of quotes, backslashes, newlines or control characters fits less. Atomic; refused over quota, on a revision mismatch, and while the session is stopped or paused or the threat level is not GREEN. Content is never restored from redaction tokens |

Paths are relative, at most 4 levels, letters, digits, inner spaces and
`-_.()（）` only. The audit rows carry a path hash and a character count, never
the path or the content. Settings, the lease, operator commands and known
limitations: [Computer-use workspaces](computer-workspaces.md).

### `os_*` — device operations on DuDuClaw OS

The `os_*` tools that need root (power, update status, the update install that sysd re-verifies, boot
assessment, rollback, factory reset) keep every check they had in the MCP
server (Admin scope, `[permissions]`, the confirmation, the factory-reset
approval) and then send the request to the gateway over loopback
(`POST /api/internal/sysd`, signed per request like the `computer_*` route).
The gateway forwards it to `duduclaw-sysd` with its own connection, so the
tools work when sysd answers only the gateway process. The route accepts only
those verbs; SSH, network, hostname, timezone and local-model operations are
refused there. This needs platform v1.73.4 or later on the device, so it applies
from DuDuClaw OS v0.4.0, which embeds v1.73.4 (v0.3.0 embeds v1.73.1, where the
tools still call sysd directly). When the gateway is not running, or the MCP server was not
started by the gateway (no employee identity), the tool fails with a reason;
it never talks to sysd directly. Off DuDuClaw OS nothing changes.

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

### `responsibility_get` / `responsibility_followup` / `responsibility_ask` — responsibility tools

The three tools for [continuous responsibilities](continuous-responsibilities.md).
They are listed only while `config.toml [responsibilities] enabled` is true and
the caller is an employee identity (external clients never see them), and every
call is refused while the feature is off. Like the `tasks_*` tools they sit
behind the Admin scope, which the gateway's own key carries.

| Tool | Who may call | Rules |
|---|---|---|
| `responsibility_get` | Any employee; an operator key may read any responsibility | No `responsibility_id`: lists the caller's own. With an id: the summary (schedule, subscriptions, window spend and runs, the open run, `cost_not_counted`). Another employee's responsibility reads as not found unless the delegation policy relates the caller to its owner |
| `responsibility_followup` | The owning employee, only while its own run of that responsibility is open and the responsibility is active | One one-shot wake-up at `due_at` (RFC 3339), at least `min_wake_interval_secs` from now and no later than `stop_at`. It counts toward the window's run limit and is refused when that limit is used up; it is never silently moved to another time |
| `responsibility_ask` | Same as `responsibility_followup` | One question (at most 1,000 characters) with up to 5 options (each at most 1,000 characters); `ttl_secs` defaults to one day and is held between 60 seconds and `stop_at`. The question is scanned for prompt injection and shown to the operator as quoted text cut to 200 characters, with a warning when the scan matched. Any push goes through the responsibility's notification gates and per-window cap. The answer wakes the next run as data and grants nothing. Stopping the run withdraws the question |

A responsibility holds at most one live wake-up armed by its employee, so a
pending `responsibility_followup` and a pending `responsibility_ask` exclude
each other. Operator keys cannot use the two wake-up tools, since that would be
acting as the employee. Closed error codes: `not_found`, `not_active`,
`no_open_occurrence`, `invalid_due_at`, `period_occurrence_limit`,
`agent_followup_limit`, `epoch_changed`, `invalid_question`.

There is no tool for creating, changing, resuming or re-enabling a
responsibility, and none for steering a task.

### `memory_store` / `user_profile_record` / `decision_resolve` / `wiki_write` — sources and refusals

Every memory write records where it came from, and `wiki_write` stamps the page it writes. The gateway supplies the source through the environment of the employee process (turn id, conversation id, and the number and time of the user message of that turn, or the run key of a scheduled or dispatched run); the model cannot name or change it. A call from an external MCP client is recorded as one call of that client.

A memory write is refused with a tool error in two cases:

- The source was forgotten by the operator (`duduclaw memory forget-source`): "Not stored: the conversation this write comes from was forgotten by the operator (forget by source), so nothing derived from it may be stored again". `decision_resolve` answers "Not resolved: the conversation this choice comes from was forgotten by the operator". The refusal is audited as `memory_write_fenced`.
- The employee process carries a conversation or run identity from the gateway that is malformed: "Not stored: this employee process carries a malformed conversation or run identity from the gateway, so the write could not be tied to its source. Restart the gateway; if it persists, report it." The write is not recorded as an anonymous external call.

A write that is derived from other memories and names a parent memory that does not exist is also refused. The memory, key fact and its source rows are written in one transaction, so a write either has its source recorded or does not happen.

`wiki_write` (both scopes) adds a `host_sources` key to the page's frontmatter with the sources of that write, keeping the newest 20 across rewrites. A `host_sources` value the employee writes itself is dropped. The key is only read by forget by source, which lists such pages for human review and never deletes them. No MCP tool performs a forget: it is an operator command, refused in an employee's session.

### `knowledge_*` — document knowledge base

`knowledge_search`, `knowledge_get_passage` and `knowledge_list_collections` read the
[document knowledge base](../features/63-knowledge-base.md). Scope `knowledge:read`, which
cannot be granted to external MCP keys. Each call goes to the gateway over the signed
loopback route `POST /api/internal/knowledge`; the gateway works out which collections the
call may read (the employee's own and department grants, narrowed by `agent.toml
[knowledge] collections`, intersected with the person the turn is for and the sensitivity
rules) and writes an access-log row. When the gateway is not running the tools say so; they
never read the database themselves.

Results are labelled as document content: every passage (and every collection name and description) sits in a marked block whose lines start with `│ `, and titles and locations are flattened to one line with `[` `]` replaced, so a document cannot forge a result header or close the block. `knowledge_search` numbers its passages
`[n] title · location (collection) chunk_id=…` so a reply can cite them; a passage flagged
at index time as instruction-like text carries a warning line. `collections` only narrows.
`knowledge_get_passage` re-checks permissions on every call and answers a missing chunk and
a forbidden one the same way (`permission denied`). Inside a channel or WebChat reply the
numbers continue the turn's citation numbering (passages already put into the prompt keep
theirs), so `[n]` in the answer points at the same passage the reader can open.

`knowledge_add_source` (scope `knowledge:write`, also not grantable to external keys)
proposes a document for a collection on the employee's own side: `collection` plus exactly
one of `path` (a file in the employee's own `attachments/`), `url` or `text`, and an
optional `title`. It refuses in a public visitor's, an unbound sender's or an unregistered
turn, and when too many of the employee's cards are waiting. It never adds anything itself: it files a
`knowledge_add_source` card that only an Admin or the collection's editor can decide, in the
dashboard, and answers with the card id. There is no exemption, for any caller.

`knowledge_search` and `knowledge_list_collections` start their answer with the search mode
(hybrid = keywords plus meaning-based vectors, or keywords only) and, when vectors were not
used, the reason (no model installed, cloud provider not acknowledged, redaction off, a
cloud error, …); `knowledge_list_collections` also reports each collection's vector
coverage. Vectors only ever rank passages inside the collections the call may read.

### `odoo_schema_overview` — saved Odoo structure

- `odoo_schema_overview` requires **`odoo:read`**. With no parameters it lists scanned models, labels and custom-field markers. Optional `model` returns fields for one model; optional `query` searches model / field names and labels. The response is narrowed to models the employee's Odoo model / action settings and feature switches allow. Snapshot metadata does not grant access to blocked business records.
- Odoo schema scans are available only to dashboard administrators through **Re-scan Odoo structure** (`odoo.schema_scan`), with a persisted 60-second limit per saved profile, including failures. The overview marks metadata as untrusted, screens and caps labels, and drops invalid technical names.
- `odoo_schema_fields {model}` still reads live metadata for one permitted model. Built-in and generic reads with explicit field lists drop missing fields using live `fields_get`; a metadata failure or an empty surviving projection refuses the read.
- `odoo_status` includes snapshot age, scan-time server version and the latest scan failure. A version mismatch recommends a re-scan. Missing snapshots and changed server versions trigger a background scan after successful connection, at most one automatic attempt per hour per effective profile. Same-version customizations need a manual scan.

A scan is bounded to 400 models, 500 fields per model and 90 seconds; module and model discovery catalogs each cap at 5,000 entries. Truncation is explicit, and partial diffs do not prove removals. See [Odoo field compatibility and schema scans](../features/12-industry-templates.md#schema-scans-and-field-compatibility) for storage and permission details.

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

## Declare a credential's service

`credential_declare` accepts `credential_id` and `service` only. Employee notes are rejected; the service must match `^[a-z0-9][a-z0-9._-]{0,63}$` and pass credential detection. Only an authenticated employee holding that credential may declare it, through the signed gateway loopback route. An employee cannot override an Admin-set service. This tool annotates existing audit metadata; it does not retrieve a secret, grant another holder access or substitute a value into a tool call. Use **Security → Credential audit** as an Admin to review and correct records. See [Credential audit](../features/70-credential-audit.md).


## Third-party MCP environment credentials

Third-party MCP keys leave the employee's config files. `.mcp.json` stores `secret://mcp/<server>/<ENV_NAME>` references; values are encrypted in `<home>/secrets/mcp/<agent_id>.enc` with the existing `.keyfile`. All third-party `env` entries use the store except `PATH`, `HOME`, `NODE_ENV`, `LOG_LEVEL` and existing `secret://` references. DuDuClaw's own server entry and its `DUDUCLAW_*` variables are unchanged.

A built-in slot must use exactly `duduclaw`, `duduclaw-pro` or the current instance key; `duduclaw-evil` is a third-party server. Rewriting replaces the entire built-in definition with the host's current canonical entry, including after MCP key rotation or an executable change. A forged built-in command cannot survive into that effective config. Raw proxy invocations remain forbidden under every name, and third-party entries cannot carry proxy-private env. Claude's existing no-flags behavior remains when no proxy purpose is active or the file contains only built-in slots; the ordinary built-in preparation path still owns their repair. Copied fork configs always regenerate built-in slots, including when gates are off, because a source project may not have passed employee preparation. Secret reads require a valid `DUDUCLAW_AGENT_TOKEN` HMAC for the store owner, even with lenient delegation settings; a default agent cannot authorize a read.

The proxy resolves references for its own employee only, immediately before starting the upstream server. Missing, invalid or undecryptable references prevent that server from starting; the error names the server and variable, never the value. A config requiring secret resolution cannot fall back to direct Claude/Grok loading if proxy rewriting fails. Exports retain environment names with empty placeholders, so the importer must enter values again.

At gateway boot, legacy literal env values are migrated after an encrypted backup is saved to `<home>/backups/mcp-migrate/<agent>-<timestamp>.enc`. The encrypted store is written and read back before the config is replaced atomically. Every stage waits for a parent-directory fsync barrier in this order: key, encrypted backup, store, config. A failed stage aborts that employee; failure after config replacement restores the original bytes. If restoration also fails, the error is reported and the durable encrypted backup is retained for recovery. Migration continues with other employees; at most three migration backups are retained per employee after cleanup; an I/O failure is logged and cleanup is retried on the next sweep. Deleting a server or employee removes its store entries. Restoring a deleted employee requires re-entering its keys.

The Read guard denies `<home>/secrets`. This is not isolation from Bash: an employee running under the same OS user can still read gateway files, including `.keyfile`; real isolation requires no Bash or the task sandbox. Only `env` is covered: keys in `args` or `url`, and tool results that echo a key, are not covered. Existing plaintext exports and backups are not retroactively cleaned.

## Google Chat read tools

`google_chat_spaces` lists spaces the connected user belongs to: resource name, display name, type and last active time when Google supplies it. `limit` defaults to 20 (1–50); `filter` accepts exactly `SPACE`, `GROUP_CHAT` or `DIRECT_MESSAGE`.

`google_chat_messages` reads recent messages from a strict `spaces/...` resource name, newest first. `limit` defaults to 30 (1–100); optional `since` must be RFC3339 and `thread` must be a thread resource within that space. Each message includes its sender, creation time, text (at most 2,000 characters), thread and attachment names. Attachments are never downloaded. Sender names use Google's returned display name or the `users/...` resource; no People API or extra membership permission is used. The complete returned content is capped at 30,000 characters.

1. An administrator enables **Google Chat read** in the dashboard under Manage → Integrations → Google (`[google] chat_read = true`, default false).
2. The user reconnects their own Google account in **My connections** and selects both Chat read permissions. The optional scopes are `https://www.googleapis.com/auth/chat.spaces.readonly` and `https://www.googleapis.com/auth/chat.messages.readonly`; existing users are not asked for them while the switch is off.
3. In the OAuth client's Google Cloud project, enable **Google Chat API** and configure a **Chat app**, even for user authentication. A Workspace administrator may also need to allow the OAuth app.

These tools use the same employee/account ownership, scope, approval and private-data egress gates as Gmail reads. They read the account selected by that existing identity policy; they do not accept a user/account argument. Apps Script does not support these Chat reads. This is separate from the Chat bot channel's DMs and mentions. Disabling the switch blocks new Chat reads, including existing tokens; it does not revoke Google's previously granted permissions.

Returned content is untrusted reference data, wrapped and scanned with the group-history policy. Any injection hit refuses the whole response with `not available`. Audit entries retain space identifiers and counts, never message text. Fixture and local tests do not establish real-account access; verify consent and reading with the customer's account before acceptance.

## `mail_send` — Requester confirmation for mail

`mail_send` uses the existing recipient and outgoing-data guards. An eligible verified requester with current Operator access may confirm the full recipients, subject and body summary in the original one-to-one conversation or authenticated dashboard WebChat; otherwise a dashboard Admin decides. The single-use approval binds final arguments and expires as deny. `[security] mail_requester_approval = false` restores Admin-only decisions; the default is `true`, while unreadable or malformed configuration fails closed. See [requester rules and limits](../features/05-security-defense.md#requester-confirmation-for-mail). Only an adapter-attested one-to-one conversation or authenticated dashboard WebChat is eligible. Group chats and unknown chat types still need a dashboard Admin; public WebChat widget visitors are never eligible. Channel notices for ineligible requests contain no recipients, subject or body; full content stays in the dashboard.

This confirmation clears the outgoing-data gate only. The native mail tool still creates its existing outbox draft confirmation; clearing this gate is not a receipt that the email was sent.
