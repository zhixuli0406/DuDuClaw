# Delegation Isolation

> Delegation control for AI employees: who can hand work to whom is decided by the `reports_to` tree, departments, and a white-list.

DuDuClaw 1.52 introduces delegation controls for multi-person teams. Your AI employees now follow the organizational structure instead of taking orders from just anyone.

---

## The problem

Earlier versions had only a single-level parent-child check — a grandparent could not assign work directly to a grandchild-level employee (skip-level requests were rejected), and same-department colleagues could not delegate to each other either. Backend paths (task queue, automation rules, external A2A calls) had no check at all: anyone could forge a delegation instruction.

---

## Three policies, your choice

Configure `[delegation] policy` in `config.toml` or under "Advanced settings" in the dashboard:

| Policy | Behavior | Use case |
|--------|----------|----------|
| `department` (default) | Up/down the hierarchy, same-department horizontal collaboration, and white-listed pairs are all allowed | Most teams |
| `hierarchy` | Only hierarchy and white-listed pairs; no same-department horizontal | Strict chain of command |
| `open` | Legacy behavior — no checks except "cannot delegate to yourself" | Emergency fallback |

White-list pairs work under both `department` and `hierarchy`; only `open` has no use for them (everything is already allowed).

The default `department` suits everyday collaboration — it fixes the skip-level assignment gap while keeping the flexibility for colleagues to back each other up.

---

## Where workflows are affected

Every delegation route is governed:

**1. Direct assignment** — calling `send_to_agent` / `spawn_agent` from the dashboard, Telegram, or the API

- A lead can assign to direct reports and also to grandchildren (grandparent→grandchild is now allowed)
- Same-department employees can assign to each other
- Everything else is rejected with a clear error naming who tried to assign to whom and which relationship is missing

**2. Task dispatch** — assigning someone while creating a task on the task board (`tasks_create`) or reassigning (`assigned_to` in `tasks_update`)

- Same checks as 1; on rejection the task creation/reassignment fails
- Claiming your own task (`tasks_claim`) is unrestricted
- Dashboard assignment operations are unrestricted (they are human operations)

**2b. Multi-step plans and routines** — every step of `create_task` can name an executor, and `schedule_task` can set up recurring work for someone else

- Both count as delegation and are checked at creation time; `create_task` steps are checked once more right before actual dispatch
- Scheduling for yourself or naming yourself as executor is unrestricted

**3. Automation rules** — the delegate action inside autopilot rules

- Checked under the `autopilot` system identity, automatically allowed
- The rules themselves are gated by the dashboard and require admin setup

**4. External A2A calls** — other systems delegating tasks via the ACP protocol's `message/send`

- Rejected by default (fail-closed)
- If a trusted external partner needs it, set `[acp] trusted = true` to open it up
- Once enabled, external calls use the `a2a-client` identity and are subject to the same delegation policy

---

## What counts as the same department

Each AI employee's `agent.toml [agent] department` field defines its department.

- Employees with the same department value are "same department" and may assign to each other under the `department` policy
- Empty or unset = no department; such an employee never counts as same-department with anyone
- Department values are compared as plain text, e.g. `sales` and `Sales` are different departments

---

## Cross-department collaboration: white-list pairs

The sales lead and the warehouse lead are in different departments but need to delegate to each other? Use the white-list:

```toml
[delegation]
policy = "department"
allow = [
  ["sales-lead", "warehouse-lead"],
  ["HR-manager", "finance-lead"]
]
```

- Each entry is a pair of two employee IDs, unordered (`["A", "B"]` equals `["B", "A"]`)
- Once paired, the two can assign to each other with no hierarchy relationship required
- Self-pairs (`["x", "x"]`) are ignored
- When editing the config file by hand, malformed entries (not exactly two strings, or containing an empty string) are ignored with a warning and do not affect other pairs; a mistyped ID is not detected — that pair simply never takes effect
- Saving from the dashboard is stricter: if any ID has no matching AI employee the whole save is rejected with the offending ID named, and at most 200 pairs may be saved at once

The dashboard card "Advanced settings → Delegation permissions" provides the editor; each pair shows both sides' department badges so the collaboration intent is visible at a glance.

---

## Dashboard settings (admin only)

**Advanced settings → Delegation permissions**

Three controls:

1. **Policy selection** (radio): department / hierarchy / open
   - Each option carries a one-line explanation
   - `open` carries a red risk warning: "abandons all checks, back to legacy behavior"

2. **Cross-department collaboration** (pair list)
   - Each row shows two AI employee names + department badges
   - Click "+" to add a pair (agent picker dropdown), "✕" to delete
   - Changes take effect immediately, no restart needed

3. **Status indicators**
   - Current policy, pair count
   - Configuration warnings (e.g. invalid IDs) are shown if present

---

## Audit and debugging

Rejected delegation attempts always leave a trail, split into two files by interception point:

- Blocked when dispatch is actually about to execute (bus queue, multi-step plans) → `~/.duduclaw/security_audit.jsonl`, event type `delegation_denied`
- Blocked at the MCP tool itself (`send_to_agent` / `spawn_agent` / `tasks_create` / `tasks_update` / `create_task` / `schedule_task`) → `~/.duduclaw/tool_calls.jsonl`, also `delegation_denied`; blocked org adjustments via `create_agent` / `agent_update` are recorded as `org_placement_denied`

A `security_audit.jsonl` record looks like this:

```json
{
  "timestamp": "2026-08-06T10:30:00Z",
  "event_type": "delegation_denied",
  "agent_id": "bob",
  "severity": "warning",
  "details": {
    "sender": "alice",
    "target": "bob",
    "reason": "different_department",
    "policy": "department",
    "path_kind": "bus_dispatch",
    "task_id": "…",
    "message": "委派遭拒：…"
  }
}
```

Troubleshooting checklist:

- Check whether the agent's `department` field is spelled correctly
- Verify the white-list pairs (case-sensitive)
- If the whole team cannot hand work around, switch to `hierarchy` or `open` to confirm department isolation is the cause

---

## Upgrade notes

When upgrading from earlier versions to 1.52:

- **Zero regression on front-door paths**: all parent-child delegation still passes (the new policy is more permissive)
- **Back-door paths tighten**: paths that previously had no checks — direct appends to bus_queue.jsonl (when a sender field is present), task-board assignment, multi-step plans, routines, external A2A calls — now enforce isolation
  - Automation scripts relying on the old "anyone can assign to anyone" behavior will see rejection errors
  - Fix: set `policy = "open"` as a temporary rollback, or fill in the org structure (add departments, add white-list pairs)
- **`[acp] trusted` defaults to false**: external A2A calls are now rejected until explicitly enabled

Keep the default `department` policy. Even though earlier versions had no checks at all, the new policy has zero impact on existing legitimate assignments and only raises warnings on unreasonable cross-cutting attempts.

---

## Technical details

### Who counts as "system" (exempt from isolation)

Assignments from these identities pass automatically:

- `dashboard` (dashboard operations, a human identity)
- `webhook` (scheduled tasks arriving via webhook)
- `goal-loop-driver` (autonomous goal loop)
- `cron` (scheduled tasks)
- `heartbeat` (heartbeat responses)
- `autopilot` (automation rules, provided the rule itself has been validated)

External A2A calls default to the `a2a-client` identity, which is **not** on the list. It is included only when `[acp] trusted = true`.

The names above (plus `a2a-client`, `default`, and any name starting with `__`) are **reserved words** and cannot be used to create AI employees — doing so would amount to issuing yourself a free pass.

### What this line of defense covers, and what it does not

Covered: every route by which AI employees delegate to each other through platform features (MCP tools, task board, multi-step plans, routines, task queue, external A2A). This is an **organizational permission boundary**: "who can tell whom what to do" follows the org chart.

Not covered (known boundaries by design, not bugs):

- **Legacy-format tasks**: 1.52 still lets queued tasks with no sender field at all pass, logging only a warning (to avoid wiping out work still queued during an upgrade). The next version switches to rejection.
- **Config-file-level changes**: since v1.52 a PreToolUse hook freezes the sensitive org data fields — an agent cannot rewrite `agent.toml`'s `name` / `reports_to` / `department`, the `[delegation]` / `[acp]` sections of `config.toml`, the `.mcp.json` identity block, `.claude/settings.json`, or `identity.key` through the Write/Edit/Bash tools. Changing these settings must go through the dashboard or the `agent_update` MCP tool — the vetted official channels. Cross-employee file edits (e.g. modifying someone else's SOUL.md) are also rejected. Non-Claude runtimes (codex/gemini etc.) cannot write to the `~/.duduclaw/` directory under the workspace-write sandbox, adding a sandbox-level line of defense. Only the FullAccess sandbox is exempt — an extreme permission the operator chooses explicitly.
- **System- and human-initiated operations**: dashboard, webhooks, schedules, and automation rules are the operator's will to begin with, and always pass.

### Visibility filtering

The `list_agents` and `agent_status` commands return only the employees the caller is allowed to see.

Depending on the policy, the visible scope includes:

- Yourself
- Your entire subtree
- All of your ancestors
- (`department` policy) every employee in the same department
- (all policies) white-list partners

Probing an invisible employee (e.g. `agent_status bob` when alice has no right to see bob) returns "not found or not visible", without distinguishing the two cases.

### No self-service privilege escalation

When `create_agent` creates a new employee, the caller can only attach it to itself or to a node inside its own subtree.

- A lead can only create direct reports, or attach new employees under an existing report
- You cannot create an employee attached under your own manager (unless the operator is that manager or someone above)
- Creating employees from the dashboard is unrestricted (a human operation)

---

## Identity verification (advanced)

### The MCP identity token mechanism

Since v1.52, every agent's MCP calls carry a signed identity token to prevent impersonation.

**How it works**:

1. On startup, the gateway generates a 256-bit random key at `~/.duduclaw/identity.key` (file permission 0600)
2. When spawning a sub-agent, the gateway produces a signed token (HMAC-SHA256, bound to the agent ID)
3. The token is injected as the environment variable `DUDUCLAW_AGENT_TOKEN`, and the sub-agent starts up carrying it
4. The MCP server verifies the token's validity and the grantor's identity

**Soft and strict modes**:

- **Soft mode** (default): `config.toml [delegation] require_identity_token = false`
  - A missing or invalid token only produces a warning; MCP startup is not rejected
  - Meant for tolerance: mid-upgrade transitions or moments when a token is briefly out of sync

- **Strict mode**: `config.toml [delegation] require_identity_token = true`
  - An invalid identity rejects MCP startup outright, with the failure written to the log
  - Suited to environments with high security requirements

**Upgrade order matters**: restart the gateway first (so agents' MCP configs get re-signed), then switch to strict mode. In the reverse order, agents start up with invalid tokens and are rejected the moment strict mode takes effect.

### Org data protection

#### Freeze targets

Agent changes made through file tools (Write/Edit/Bash) are intercepted by the PreToolUse hook:

| File | Fields / sections | Reason |
|------|-------------------|--------|
| `agent.toml` | `name`, `reports_to`, `department` in `[agent]` | Changing these rewrites the org chart — a self-service escalation hole |
| `config.toml` | Entire `[delegation]`, `[acp]` sections | Policy settings affect the whole team's safety and cannot be changed casually |
| `.mcp.json` | `DUDUCLAW_AGENT_ID`, `DUDUCLAW_AGENT_TOKEN` | Identity tokens; changing them means impersonating someone else |
| `.claude/settings.json` | Whole file | Permission lists and other sensitive settings are managed centrally by the dashboard |
| `identity.key` | (whole file) | Signing key; any change breaks identity verification |

#### The right channels for changes

When these settings need to change:

- **`name`, `reports_to`, `department`** → dashboard "AI employees → details → edit", or the MCP `agent_update` tool
- **Adjusting permissions or adding tools** → dashboard "AI employees → advanced settings", or edit `agent.toml [capabilities]` and specify by hand (not through file tools)
- **Delegation policy or white-list** → dashboard "Advanced settings → Delegation permissions", or edit `config.toml [delegation]` directly and restart the gateway
- **Adding an MCP server** → edit the `tools` array in `.mcp.json` (leave the identity block alone), or add manually via dashboard "Advanced settings → MCP servers"

Interceptions are recorded in `~/.duduclaw/tool_calls.jsonl` with the `org_placement_denied` marker for easier debugging.

#### System identities are unrestricted

System senders (dashboard, webhook, cron, autopilot) operate without restriction and may change any setting. This is guaranteed by design: those sources embody the operator's will.

### White-list input flexibility

When editing pairs on the dashboard delegation-permissions card, both fields accept:

- **AI employee display name** (`agent.toml [agent] display_name`): e.g. "Alice"
- **Directory name** (`agent.toml [agent] id`): e.g. "alice-engineer"

The input search scans both fields, but storage always normalizes to the directory name, so pairs stay stable even when display names change.

### Automatic departments on team deployment

With the dashboard's "Deploy new team" feature, there is no need to edit each member's department field one by one:

1. Upload or pick an industry template (`team.toml` defines `[team] industry`, e.g. `retail`, `healthcare`)
2. On deployment, every front-office agent (support, sales, ...) and background worker (data prep, reports, ...) gets its `department` set to that industry code automatically
3. Industry packs installed standalone carry the department too; when new employees join, set the same department by hand or change several at once from the dashboard

The whole team ends up in the same department automatically, matching how most industry teams are organized. Add white-list pairs when cross-department collaboration is needed.

---

## Common Q&A

**Q: After the upgrade, my employees can't hand out work anymore**

A: Read the error message. If it says "different department", fill in the `department` fields or add a white-list pair. If the message says the hierarchy level is insufficient, check the `reports_to` tree structure.

**Q: Should I turn on `policy = "open"`?**

A: Not recommended. The default `department` amounts to "everyone may assign to their reports, colleagues may help out" — the common collaboration pattern. Falling back to `open` should only ever be temporary debugging.

**Q: Is there a limit on white-list pairs?**

A: Saving from the dashboard caps at 200 pairs at a time. Needing more usually means the org structure or department split should be adjusted, not that exceptions should keep piling up.

**Q: What goes into a new employee's `department` field?**

A: A department code is enough (e.g. `sales`, `engineering`, `hr`). Literal comparison, case-sensitive. Empty or unset = no department; such an employee never counts as same-department with anyone and can only collaborate through the hierarchy or the white-list.

---

## The authoritative source of org data

Since v1.52, the organizational structure has one central authoritative store, preventing inconsistencies caused by concurrent edits.

### `org.toml` — the single source of truth for the org chart

On first startup, the gateway automatically scans the `agent.toml` under every agent directory and imports the org data (`reports_to`, `department`) into the central file `~/.duduclaw/org.toml`. From then on, all changes to the org structure go through this file:

```toml
# ~/.duduclaw/org.toml — new in v1.52+
[agents."alice-engineer"]
display_name = "Alice"
reports_to = "sales-lead"
department = "sales"

[agents."bob-qa"]
display_name = "Bob"
reports_to = "alice-engineer"
department = "sales"
```

This file is managed by the gateway; a broken format from hand editing can fail startup, so prefer the commands.

### Three commands to manage the org structure

#### 1. `duduclaw org sync` — sync local agent.toml into the authoritative file

Run by the operator in a terminal (**not inside an AI employee session**):

```bash
duduclaw org sync
```

Scans the `reports_to` / `department` fields of `agent.toml [agent]` under every agent directory and updates `~/.duduclaw/org.toml`. On conflict (e.g. alice-engineer is sales in the authoritative file but the local `agent.toml` says engineering), the command lists every difference and asks for confirmation.

**When to use**:

- You hand-edited an agent.toml's org fields and want the change synced into the authoritative file
- You upgraded from an older version: agent.toml still holds org data but the authoritative file is empty

#### 2. `duduclaw org show` — inspect the current org structure

```bash
duduclaw org show
```

Shows every employee, who reports to whom, and each employee's department as a tree or table. Handy for confirming the org chart matches expectations.

#### 3. `duduclaw doctor` — diagnose org data inconsistencies

```bash
duduclaw doctor
```

Scans for and reports the following problems:

- Missing or malformed authoritative file
- `reports_to` or `department` in an agent.toml disagreeing with the authoritative file (called "drift")
- Dangling reports_to (pointing at a nonexistent employee)
- Circular dependencies (A→B→C→A)

The output lists each problem clearly with suggested repair steps.

### Why the move — org structure is a decision, not data

Older versions let an agent hand-edit the `agent.toml [agent]` fields in its own directory, directly affecting delegation decisions (changing `reports_to` changed your boss). That opened a hole: an employee could rewrite the org chart on its own and escalate its own authority.

The new version makes org data a **central authority**, which means:

- **Who decides who reports to whom**: the operator (a human), not the AI employee
- **Hand edits don't take effect**: editing the org fields of an agent.toml only gets flagged as drift by doctor; delegation behavior never changes automatically
- **Org changes follow a process**: the dashboard or the `duduclaw org sync` command, so every change carries a human's sign-off intent

---

## Cross-employee file isolation

AI employees should stay cleanly separated: one employee must not be able to modify another employee's files.

### File tools stop at the border

Since v1.52, the Write / Edit / Bash tools enforce boundary checks on file paths:

- **Inside your own employee directory ✅**: alice-engineer can read and write everything under `~/.duduclaw/agents/alice-engineer/`
- **Inside another employee's directory ❌**: alice-engineer trying to modify bob-qa's SOUL.md or memory files → **tool execution rejected**, written to `tool_calls.jsonl` with the `access_denied` marker
- **Global sensitive files ❌**: alice-engineer cannot change `~/.duduclaw/config.toml`, `org.toml`, `identity.key`, or other global files

### The PreToolUse hook interception layer

Every file Write / Edit / Bash operation goes through the hook check, and a rejected call returns an error immediately — no waiting for file I/O, which prevents races and log leakage.

### The sandbox layer — non-Claude runtimes

Non-Claude runtimes such as Codex, Gemini, and Antigravity cannot write to the `~/.duduclaw/` directory under the `workspace-write` sandbox. Even if the PreToolUse hook had a hole, the sandbox blocks at the system level. Only the FullAccess sandbox (an extreme permission the operator chooses explicitly) opens it up — at that point, treat it as granting the employee temporary global access.

### Legitimate channels for cross-employee changes

If another employee's settings must change (e.g. a lead adjusting a report's department):

1. **Employee basics** — dashboard "AI employees → details → edit" (performed by a human/system operator)
2. **Org structure** — `duduclaw org sync` or dashboard "Advanced settings → org structure"
3. **Capabilities and permissions** — dashboard "AI employees → advanced settings" or the MCP `agent_update` tool (admin verification required)

These channels all embody human decisions and cannot be bypassed by employee self-service.
