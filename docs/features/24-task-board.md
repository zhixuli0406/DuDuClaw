# Task Board & Activity Feed

> Agent-as-teammate task management — one shared Kanban board that humans and AI agents both work, claim, and post standups to.

---

## The Metaphor: A Shared Kanban Board

Picture a physical Kanban board on an office wall. Cards move through columns — *To Do → In Progress → Done* — and a *Blocked* lane catches anything that's stuck. A human product lead pins new cards. Teammates walk up, peel a card off "To Do", stick it under their name in "In Progress", and at standup they say what they did.

Now make some of the teammates AI agents. They read the same board. They claim the same cards. They post the same standup updates. The only difference is *how* they reach the board: the human uses a mouse on a web dashboard; the agent calls a tool from inside its reasoning loop.

DuDuClaw's Task Board is exactly this — a single SQLite-backed board with two doors into it. One door for humans (the web dashboard), one for agents (MCP tools). Both move the same cards. The Activity Feed is the standup log everyone reads.

---

## One Store, Two Access Layers

The board and feed live in a single SQLite database (`tasks.db`, WAL mode, 5s busy_timeout for multi-process safety). Everything funnels through one `TaskStore` — but there are two distinct ways to reach it:

```
   HUMANS                                   AGENTS
   (web dashboard)                          (inside the reasoning loop)
        |                                        |
        v                                        v
 ┌──────────────────┐                 ┌────────────────────────┐
 │ Dashboard WS RPC │                 │  Agent-facing MCP tools │
 │  tasks.list      │                 │   tasks_list            │
 │  tasks.create    │                 │   tasks_create          │
 │  tasks.update    │                 │   tasks_update          │
 │  tasks.remove    │                 │   tasks_claim           │
 │  tasks.assign    │                 │   tasks_complete        │
 │  activity.list   │                 │   tasks_block           │
 │                  │                 │   activity_post         │
 │                  │                 │   activity_list         │
 └────────┬─────────┘                 └───────────┬────────────┘
          |                                       |
          +──────────────┐         ┌──────────────+
                         v         v
                    ┌─────────────────────┐
                    │   TaskStore (one)    │
                    │   tasks.db (SQLite)  │
                    │   - tasks   table    │
                    │   - activity table   │
                    └─────────────────────┘
```

Neither door is "primary." The human creates a card and an agent claims it; an agent creates a sub-task and the human reassigns it. Because both write the same rows, the board is always a single source of truth — no sync, no mirror, no drift.

### The Dashboard RPC set (humans)

Served over the authenticated dashboard WebSocket. These power the web UI's Task Board page and Activity Feed.

| RPC | Purpose |
|-----|---------|
| `tasks.list` | List/filter tasks for the board view |
| `tasks.create` | Create a card; broadcasts an `activity.new` event |
| `tasks.update` | Edit fields / move a card between columns |
| `tasks.remove` | Delete a card |
| `tasks.assign` | Reassign a card to an agent (thin wrapper over `tasks.update`) |
| `activity.list` | Read recent Activity Feed events |

### The MCP tool set (agents)

Exposed to the AI runtime over the MCP server. These let an agent see its own queue, claim work, report progress, and finish cards — without a human in the loop.

| MCP tool | Purpose |
|----------|---------|
| `tasks_list` | See your queue (defaults to caller; `assigned_to='*'` for all) |
| `tasks_create` | Add a card; `created_by` is auto-set to the caller |
| `tasks_update` | Edit fields (title / description / priority / tags) |
| `tasks_claim` | Atomically take an unassigned card and set it `in_progress` |
| `tasks_complete` | Mark a card `done` with an optional completion summary |
| `tasks_block` | Mark a card `blocked` with a required reason |
| `activity_post` | Post a progress note *without* changing task status |
| `activity_list` | Read recent activity (defaults to caller) |

This is the heart of the Multica "Agent-as-teammate" design: an agent isn't just a function you call — it's a colleague who watches the board, picks up cards, and posts standups.

---

## The Task Lifecycle

A card has a `status` and a `priority`. The status moves through a small, predictable flow:

```
                tasks_create / tasks.create
                          |
                          v
                      ┌────────┐
                      │  todo  │◄──────────────┐
                      └───┬────┘               │
                          │ tasks_claim        │ (reopen via
                          v                    │  tasks.update)
                   ┌─────────────┐             │
                   │ in_progress │             │
                   └──┬───────┬──┘             │
       tasks_block    │       │  tasks_complete│
                      v       v                │
                  ┌─────────┐ ┌──────┐         │
                  │ blocked │ │ done │─────────┘
                  └────┬────┘ └──────┘
                       │  (unblock → tasks_update back to todo / in_progress)
                       └──────────────────────────────►
```

Completing a card auto-stamps `completed_at`. Blocking one records a `blocked_reason` that shows on the card. Claiming is a **compare-and-set**: `tasks_claim` only succeeds if the card is currently unassigned, so two agents can't both grab the same card.

### Status values

| Status | Meaning |
|--------|---------|
| `todo` | Created, not yet started (default on creation) |
| `in_progress` | An agent or human is actively working it |
| `blocked` | Stuck — carries a `blocked_reason` |
| `done` | Finished — stamps `completed_at` |

### Priority values

| Priority | Rank (urgent → low) |
|----------|---------------------|
| `urgent` | 0 (surfaced first) |
| `high` | 1 |
| `medium` | 2 (default) |
| `low` | 3 |

Tasks also carry `tags`, an optional `parent_task_id` (for sub-tasks, with cycle detection so a card can't become its own ancestor), `created_by`, and `assigned_to`.

---

## Real-Time Activity Feed

Every meaningful state change writes an `activity` row and is pushed live to dashboard subscribers. When the web UI creates a card or an agent moves one, the gateway broadcasts an `activity.new` event over the WebSocket — so the Activity Feed updates without a refresh.

```
agent calls tasks_claim
        |
        v
TaskStore: UPDATE status=in_progress, assigned_to=agent
        |
        +─► append_activity(task_assigned)
        |
        +─► broadcast_event("activity.new", …)  ──►  Dashboard
                                                     (live feed updates)
```

`activity_post` exists precisely so an agent can say "still working, halfway through the migration" *without* changing the card's status — the standup comment, not the column move.

---

## Pending Tasks Auto-Injected into the Prompt

The board isn't only something agents *can* check — their open work is pushed into their context automatically. When the gateway assembles an agent's system prompt, it builds a `## Your Task Queue` section:

```
build_pending_tasks_section(agent_id):
     |
     v
pull open tasks (in_progress → todo → blocked) for this agent
     |
     v
sort by priority (urgent → low), take up to 5
     |
     v
render bullets + a reminder of the MCP tools:
  ## Your Task Queue (7 pending)
  1. [urgent] Fix Discord reconnect loop [in progress]
  2. [high]   Draft Q3 release notes
  3. [medium] Review marketplace skill PR — blocked: needs API key
  +2 more — call tasks_list to see all

  Use `tasks_list`, `tasks_claim`, `tasks_update`,
  `tasks_complete`, `tasks_block` to manage these,
  and `activity_post` to report progress.
```

If the agent has no open tasks, the section is omitted entirely to keep the prompt tight. The store is read through a single shared SQLite connection owned by the gateway (avoiding WAL write-lock contention on high-volume channel replies), falling back to a per-call open only if injection hasn't run yet.

---

## Scheduler-Level Pull: Waking Idle Agents

Auto-injection covers agents that are *already* answering a message. But most production agents have `heartbeat.enabled = false` and sit idle until a channel message arrives — so a card assigned to them would never get picked up.

The `HeartbeatScheduler` fixes this at the **scheduler level**: on every 30s tick, it scans the *entire* agent registry (not just heartbeat-enabled agents) and runs `poll_assigned_tasks` for each:

```
every 30s scheduler tick:
     |
     v
for EACH agent in the registry (regardless of heartbeat.enabled):
     |
     v
poll_assigned_tasks(agent):
   - highest-priority `todo` assigned to this agent?
   - any `in_progress` task stalled > 30 min (updated_at old)?
     |
     v
enqueue a wake-up message (message_queue.db) nudging the agent
to tasks_claim the todo, or activity_post progress on the stalled one
     |
     v
cooldown gate: skip if the same nudge was sent < 1 hour ago
   (LIKE marker on existing queue rows — no extra schema)
```

Without this, the Multica "Agent-as-teammate" design degenerates into "agents only act when a channel message arrives." The 1-hour `LIKE`-marker cooldown prevents the 30s tick from stampeding the same agent with duplicate nudges.

---

## Why This Matters

### A Genuine Shared Workspace

Humans and agents are not on separate systems bolted together with a sync job. They write the same SQLite rows. A card created in the dashboard is the *same object* an agent claims via MCP — there is exactly one board.

### Agents That Behave Like Teammates

`tasks_claim` / `tasks_complete` / `tasks_block` / `activity_post` give an agent the same verbs a human coworker has: take work, finish it, flag a blocker, post a standup. The board becomes a coordination surface, not just a logging table.

### No Lost Work

Auto-injection puts open tasks in front of an active agent every turn; scheduler-level pull wakes idle agents who'd otherwise never see their queue. Together they close the gap that once left tasks unrouted and untouched.

### Safe Concurrency

Claiming is compare-and-set, so two agents can't double-claim. Parent links are cycle-checked. The store runs in WAL mode with a busy timeout, so the dashboard and multiple agents can write concurrently without corrupting the board.

---

## The Takeaway

A team needs one board everyone can see, claim from, and report against. DuDuClaw's Task Board is that board — a single SQLite store with a human door (dashboard RPC) and an agent door (MCP tools), a real-time Activity Feed for standups, prompt auto-injection so working agents always see their queue, and a scheduler-level pull so idle agents still get woken when work lands on their name. It's what turns an AI agent from a function you call into a teammate who picks up cards.
