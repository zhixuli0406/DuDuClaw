# Autopilot Rule Engine

> Event-driven automation — when *this* happens, do *that*, with a breaker panel that trips when a rule runs away.

---

## The Metaphor: A Smart Home with a Breaker Panel

A smart home runs on simple rules: *when the front door opens after sunset, turn on the hallway light.* *When the temperature drops below 18°C, start the heater.* You don't supervise these — they fire on their own, reacting to events as they occur.

But a naive rules engine has a failure mode: a rule that triggers an event that triggers the same rule. The light turns on, which trips a motion sensor, which turns the light on, forever. A well-designed home has a **breaker panel** — if a circuit draws too much current too fast, the breaker trips and isolates it, leaving the rest of the house running.

DuDuClaw's Autopilot Rule Engine is that smart home for your agent fleet. Rules listen for events (a task is created, a channel message arrives, an agent goes idle), check conditions, and dispatch actions (delegate work, send a notification, run a skill). And every rule has its own three-state circuit breaker so a self-reinforcing loop trips *that rule* without taking down the engine.

---

## How It Works

The engine is built on a `tokio::broadcast` event bus. Producers (WebSocket handlers, the cron scheduler, channel listeners) publish `AutopilotEvent`s onto the bus; the `AutopilotEngine` is the single consumer that evaluates every rule against each event:

```
Event producers                  Event bus              Rule evaluation         Action dispatch
─────────────────                ──────────             ───────────────         ───────────────
WebSocket handler ──┐
Cron scheduler     ─┼──> broadcast::channel(8192) ──>  AutopilotEngine ──> ┌─ delegate
Channel listener   ─┤         (AutopilotEvent)          for each rule:     ├─ notify
MCP bridge         ─┘                                   1. event matches?  └─ run_skill
                                                        2. conditions pass?
                                                        3. breaker closed?
                                                        4. → dispatch action
                                                        5. → append history
```

The bus has a capacity of 8,192 — enough to absorb a burst of events without a slow database write back-pressuring producers. If the consumer ever lags far enough to drop events, the engine logs the lag count so operators can investigate a slow DB or raise the capacity.

---

## The Event Types

A rule can subscribe to thirteen kinds of `AutopilotEvent`. Each carries a payload that is flattened into a field map the conditions can match against:

| Event | `event_name` | Fired when | Key fields |
|-------|--------------|------------|------------|
| **TaskCreated** | `task_created` | A new task lands on the Task Board | task object (id, title, priority, ...) |
| **TaskUpdated** | `task_updated` | A task's fields are updated | task object |
| **TaskStatusChanged** | `task_status_changed` | A task moves between statuses | `task_id`, `from`, `to`, task object |
| **ActivityNew** | `activity_new` | A new Activity Feed entry is posted | activity object |
| **ChannelMessage** | `channel_message` | A message arrives on a channel | `channel`, `agent_id`, `text` |
| **AgentIdle** | `agent_idle` | An agent has been idle | `agent_id`, `idle_minutes` |
| **CronTick** | `cron_tick` | Never: the variant exists but no producer sends it. Since v1.67.1 a new rule cannot use it (see below) | `now` |
| **RunAtRisk** | `run_at_risk` | Foresight predicts that a running task is heading for failure | `agent_id`, `session_id`, `score`, `level`, `reasons` |
| **OsFileEvent** | `os_file` | The agent's `[os_watch]` watcher sees a file change | `agent_id`, `path`, `kind` (created / modified / removed / renamed), `file_name`, `extension` |
| **OsFrontmostEvent** | `os_frontmost` | The frontmost app or window title changes | `agent_id`, `app`, `window_title`, `prev_app` |
| **Tick** | `tick` | A resident-sensing source (`[[tick.sources]]`) produces an observation | `source`, `ts`, the extracted fields and their `prev_` / `delta_` / `pct_` companions |
| **SecurityEvent** | `security_event` | A warning- or critical-level audit event, or a security posture change | `severity`, `event_type`, `agent_id`, `source` |
| **OdooEvent** | `odoo_event` | The Odoo poller or `POST /webhook/odoo` reports an ERP change | `event_type`, `model`, `record_id`, `record` (its top-level scalar keys are also flattened) |

A rule declares which `trigger_event` it cares about, so a `channel_message` rule never sees a `task_created`. `autopilot.create` accepts the twelve names other than `cron_tick`; `autopilot.update` accepts `cron_tick` only when the stored rule already has it, so such a rule can still be saved; changing another rule to `cron_tick` is refused with the same message as on create. Work that should run on a schedule belongs to the cron scheduler (the 例行工作 / Routines page, or `tasks_create` with `schedule`). All thirteen names are still legal `first` / `then` events of a `sequence` rule. The engine also carries an internal `cep_trigger` event that the sequence matcher emits to fire a matched rule; it is not a valid `trigger_event`.

---

## Conditions

A rule's conditions are a small JSON tree. The top level can be an `all` (every child must pass) or `any` (at least one child must pass) group, and each leaf compares one field against an expected value with an operator:

```
{ "all": [ <condition>, <condition>, ... ] }   ← every child must be true
{ "any": [ <condition>, <condition>, ... ] }   ← at least one child true
```

A leaf condition looks up a field by path and applies an operator:

| Operator | Meaning |
|----------|---------|
| `eq` | field equals expected |
| `neq` | field does not equal expected |
| `in` | field is one of an array of values |
| `not_in` | field is none of an array of values |
| `gt` / `gte` | field is numerically greater (or equal) |
| `lt` / `lte` | field is numerically less (or equal) |
| `contains` | string contains substring, or array contains value |

A field that is **absent** never satisfies any comparison — including `eq null`. This is deliberate: allowing an absent field to match `eq null` once caused a rule to mass-fire against every event. Missing means no match, full stop.

### No conditions

A rule with no conditions fires on every event of its trigger. "No conditions" is any of: `conditions` absent from the create call, `null`, `{}` or `{ "all": [] }`. `{ "any": [] }` is accepted but never matches.

**Behaviour change in v1.67.1.** `autopilot.create` stores an absent `conditions` as `{}`, and before v1.67.1 the engine treated `{}` as a leaf with an empty field, so such a rule never fired. From v1.67.1 those rules fire on every event of their trigger. `null` and `{ "all": [] }` already matched everything before. To find enabled rules that will start firing, run this on the gateway host and disable or edit any you did not intend:

```bash
sqlite3 ~/.duduclaw/autopilot.db \
  "SELECT id, name, trigger_event FROM autopilot_rules
   WHERE enabled = 1 AND sequence IS NULL AND trim(conditions) = '{}';"
```

In the dashboard, opening such a rule with the edit button shows "No conditions: the rule runs every time the trigger happens."

### Write-time validation

`autopilot.create` and `autopilot.update` (and enabling an induced rule) validate the condition tree before saving. Each node must be an object: an `all` / `any` group whose value is an array of conditions, or a leaf with a non-empty string `field` and an optional `op` from the table above (`eq` when omitted). Nesting is capped at 16 levels. A refusal names the path of the bad node, for example `conditions.all[1]: a condition needs a non-empty "field" (or use an "all"/"any" group)`. Before v1.67.1 such a leaf was stored and never matched.

---

## The Three Action Types

When a rule's event matches, its conditions pass, and its breaker is closed, the engine dispatches one of three actions:

| Action | What it does | Required fields |
|--------|--------------|-----------------|
| **delegate** | Enqueue a bus task for a target agent | `target_agent`, `prompt` |
| **notify** | Send a message to a channel | `channel`, `chat_id`, `text` |
| **run_skill** | Invoke a skill as a target agent | `target_agent`, `skill_name` |

`run_skill` is the most sensitive, because both the agent and the skill name come from rule config. The engine validates both:

```
run_skill action:
     |
     v
target_agent must be alphanumeric (allowlist) ─── reject if not
     |
     v
skill_name must be a safe file stem (allowlist) ── reject "../passwd", "skill/subdir"
     |
     v
canonicalize(skills_dir/skill_name)
     |
     v
canonical path MUST start_with canonicalize(skills_dir) ── reject if it escapes
     |
     v
invoke
```

The path-containment check (`canonicalize()` + `starts_with`) means even a skill name that slips past the charset allowlist can't reference a file outside the agent's skills directory.

---

## The Circuit Breaker

Every rule has its own three-state circuit breaker. This is the engine's defense against self-reinforcing loops — a rule whose action produces an event that re-triggers the same rule (`task_created → delegate → agent creates task → task_created → ...`).

```
            ┌──────────────────────────────────────────────┐
            │                                              │
            v                                              │
      ┌──────────┐   >= 10 fires within 60s    ┌────────┐  │
      │  CLOSED  │ ─────────────────────────>  │  OPEN  │  │
      │ (normal) │                             │(blocked)│  │
      └──────────┘ <───────────────────────┐  └────────┘  │
            ^      quiet probe window       │       │      │
            │      (no re-trip)             │       │ after 60s cooldown
            │                               │       v      │
            │                          ┌──────────────┐    │
            └──────────────────────────│   HALF-OPEN  │    │
                                       │ (1 probe ok) │────┘
                                       └──────────────┘
                                          re-fire within
                                          probe window → OPEN
```

- **Closed** — normal operation. Fires are counted in a 60-second sliding window. Ten fires within that window trip the breaker to **Open**.
- **Open** — all of this rule's fires are blocked for a 60-second cooldown. The rest of the engine keeps running.
- **HalfOpen** — after the cooldown, one probe fire is allowed. If another fire arrives within the probe window (a sign the loop is still live), the breaker re-trips to Open. A quiet probe window returns it to Closed.

State transitions are logged to `autopilot_history` and surfaced on the Activity Feed, so an operator can see exactly when and why a rule was throttled.

---

## A Rule Definition

A rule is persisted in SQLite with a name, an enabled flag, a `trigger_event`, a `conditions` tree, and an `action`. Here is a rule that delegates urgent new tasks to an on-call agent:

```json
{
  "name": "urgent task → on-call",
  "enabled": true,
  "trigger_event": "task_created",
  "conditions": {
    "all": [
      { "field": "task.priority", "op": "eq", "value": "urgent" },
      { "field": "task.title", "op": "contains", "value": "incident" }
    ]
  },
  "action": {
    "type": "delegate",
    "target_agent": "oncall",
    "prompt": "An urgent incident task was just created. Triage it."
  }
}
```

A `notify` rule that pings a channel when an agent goes idle too long:

```json
{
  "name": "idle agent alert",
  "enabled": true,
  "trigger_event": "agent_idle",
  "conditions": {
    "all": [ { "field": "idle_minutes", "op": "gt", "value": 30 } ]
  },
  "action": {
    "type": "notify",
    "channel": "telegram",
    "chat_id": "12345",
    "text": "An agent has been idle for over 30 minutes."
  }
}
```

---

## Rule CRUD: Dashboard RPC + MCP

Rules are managed from two surfaces, both fail-closed (Admin scope required):

```
Dashboard (web UI)                         Agent (MCP)
──────────────────                         ───────────
autopilot.list    ── list all rules        autopilot_list ── read-only view
autopilot.create  ── add a rule                              of the rule set
autopilot.update  ── edit a rule
autopilot.remove  ── delete a rule
autopilot.history ── execution log
```

Every `create` / `update` validates the `trigger_event`, the `conditions` tree and the `action` structure **at write time** — a malformed rule is rejected immediately rather than failing silently later in `autopilot_history`. Every execution (success, error, or breaker transition) appends a row with status and error context.

### The dashboard rule form

Settings → Autopilot creates and edits rules. Before v1.67.1 its form sent an action with `agent_id` and `prompt_template` where the server requires `target_agent` and `prompt`, offered a `schedule` trigger that does not exist, and wrote conditions such as `from_status` / `idle_minutes` / `cron` that are not field/op/value leaves. The server refused every save, so no rule could be created from the dashboard. Since v1.67.1:

- The trigger list is the twelve events `autopilot.create` accepts, including `tick` ("A monitoring source reports new data") and `odoo_event` ("An Odoo record changes"). A stored rule whose trigger the form does not offer (for example `cron_tick`) keeps it and shows it.
- Conditions are a list of field / operator / value rows joined by "all" or "any" matching. Field names are suggested per trigger; any name can be typed. For `tick` and `odoo_event` the field names come from your source or Odoo records, and the form says so. Values for `gt` / `gte` / `lt` / `lte` must be numbers; `in` / `not_in` take a comma-separated list. With no rows the form saves `{ "all": [] }`, which fires on every event.
- Actions are `delegate`, `notify` and `run_skill`, with the required fields from the table above. `notify` offers telegram, line, discord, slack, whatsapp, feishu, googlechat, teams, wecom and dingtalk.
- Each rule has an edit button. Editing sends only the fields that changed. Conditions the form cannot show as a flat list, event-sequence rules and actions other than the three above are shown as locked and kept exactly as stored. Action keys the form does not own (`screen`, `context_ticks`, …) are kept as stored; the form cannot add them to a new rule, so set those through `autopilot.create` / `autopilot.update`.
- A refused save shows the server's message inside the dialog.

---

## The MCP → events.db Bridge

The engine's in-process producers can `send()` directly onto the broadcast bus. But events that originate from a *separate process* — the MCP server, Python adapters — can't reach an in-memory channel. The bridge is a SQLite event bus at `<home>/events.db`:

```
MCP server (separate process)          Gateway process
─────────────────────────────         ────────────────
emit "task.created" ──> events.db ──> background reader
                        ┌──────────┐   fetch_since(last_id)
                        │ id (PK,  │        |
                        │  AUTOINC)│        v
                        │ ts       │   re-emit onto the
                        │ type     │   broadcast bus as
                        │ payload  │   AutopilotEvent
                        └──────────┘
```

This SQLite bus replaced a legacy `events.jsonl` file bus. The columns and guarantees that make it safe:

- **WAL mode + busy_timeout** — multiple processes can write concurrently without corrupting lines (no `events.jsonl` rotation race, no partial-line hazard).
- **Monotonic `id INTEGER PRIMARY KEY AUTOINCREMENT`** — the reader just polls `fetch_since(last_id)`; no cursor file, no re-reading.
- **Built-in retention** — a background prune deletes rows older than the 7-day cutoff, so the table never grows unbounded.

---

## Why This Matters

### Agents That React, Not Just Respond

Without autopilot, an agent only acts when a human messages it. With rules, the fleet reacts to its own operational events — a new task auto-routes to the right specialist, an idle agent gets nudged, an incident triggers a notification — all without a human in the loop.

### Loops Can't Take Down the Engine

The single most dangerous failure in any event-driven system is the self-reinforcing loop. The per-rule circuit breaker isolates the runaway rule to a 60-second timeout while every other rule keeps working. A bad rule degrades to "throttled," never "engine down."

### Safe by Construction

`run_skill` validates the agent and skill name against an allowlist *and* confirms the resolved path stays inside the skills directory. `eq null` can't match absent fields. CRUD validates structure at write time. Each gate fails closed.

### Cross-Process Without the File-Bus Hazards

The `events.db` bridge gives a separate MCP process a way to feed the engine without the rotation races, partial-line corruption, or permission quirks of a shared JSONL file. WAL handles concurrency; the monotonic id handles ordering; the prune handles growth.

---

## The Takeaway

A smart home reacts to events automatically, and a breaker panel keeps a runaway circuit from burning the place down. DuDuClaw's Autopilot Rule Engine brings both to your agent fleet: a broadcast event bus, declarative rules with `all`/`any` conditions, three validated action types, and a three-state circuit breaker per rule. Producers in-process publish directly; producers out-of-process feed through a WAL-backed `events.db` bridge. Automation you can trust to run unsupervised — because the breaker trips before a loop ever does damage.
