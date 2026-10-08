# Continuous responsibilities, mid-run directions and stopping a task

A continuous responsibility lets one AI employee wake up repeatedly over a set period, on a schedule or when a matching event happens, to do the same bounded job. Every wake-up creates a new goal task, called a run here (an *occurrence* in the code). A run has its own deadline, round cap and spending cap, and is accepted through the normal goal-loop judging. Between two runs no task exists at all, so waiting holds no execution slot and calls no model.

The same change adds two controls that work on any goal task: giving the AI employee extra directions while the task is running (steering), and stopping a task together with its sub-tasks.

What this release ships:

| To do this | Use |
|---|---|
| Create, change, pause, resume, disable or re-enable a responsibility | The command line `duduclaw responsibility …`; every change needs an Admin's approval in the dashboard |
| Look at responsibilities, their runs and wake records | The `/responsibilities` dashboard page, or the command line (read commands need no approval) |
| Create, pause, resume, turn off or on, clear failures from the dashboard | The `/responsibilities` page (your binding to the employee decides which buttons work) |
| Give a running goal task a direction | The task detail page in the dashboard |
| Stop a task and its sub-tasks | The task detail page (takes effect at once), or the command line (needs approval) |

The dashboard's `/responsibilities` page is described below under "The responsibilities page".

## Switches and settings

Both switches are off by default. They live in `~/.duduclaw/config.toml` and are re-read on every use, so no gateway restart is needed.

```toml
[responsibilities]
enabled = false                    # master switch
max_stop_at_days = 30              # how far ahead stop_at may be (1–90; values above 90 count as 90)
event_poll_batch = 500             # events read per pass (1–2000)
max_notifications_per_period = 10  # pushes per budget window; 0 = never push
max_event_wakes_per_period = 12    # runs per budget window that events may start
operator_approval_minutes = 30     # how long a command-line change stays usable after approval (1–1440)

[goal_loop]
steering_enabled = false           # mid-run directions
```

- Responsibilities run on the dispatch engine (`[dispatch] enabled`, on by default since v1.59). With the engine off, creating a responsibility is refused and existing ones do not wake. The event read position does not move while the engine is off, so when it is switched back on, events from that period (up to the 7 days `events.db` keeps) are read as new and can wake the responsibility.
- A malformed `[responsibilities]` section switches the feature off. An out-of-range or unreadable `operator_approval_minutes` falls back to 30.
- Setting `enabled` back to `false`: nothing new is created, nothing wakes, the three employee tools disappear from the tool list, and command-line actions that widen permissions or spending are refused. Stored records are kept. A run already in progress finishes as an ordinary goal task; after the feature is switched back on, the next pass settles it.
- With `steering_enabled` off, new directions cannot be sent. Directions already sent and waiting for the next round are still delivered.

## Where a run comes from

There are three wake sources. Each one only records a "time to wake" fact; the one component that turns a fact into a task is the dispatch engine's pass every 30 seconds.

- **Schedule**: a cron expression plus a time zone, for example 9 a.m. on weekdays. Slots missed while the gateway was down are caught up at most once after it starts (only the latest slot within the last 24 hours), never as a burst.
- **Events**: only `task.created` and `task.updated`, and only events that belong to this employee (`assigned_to` in the event equals it). These events are written by the MCP task tools (an AI employee's, or an operator's MCP key); changes made in the dashboard write none. A contract that names any other event, `activity.new` included, is refused. An optional condition uses the same syntax as automation rule conditions.
- **Decisions**: during a run the employee asks the operator a question with `responsibility_ask`; the answer wakes the next run.

A responsibility has at most one run open at a time. While the previous run has not finished (including a run parked for a human), new wake facts wait and are merged into one run afterwards. Two runs are at least `min_wake_interval_secs` apart, and a run is not created while other goal tasks fill every execution slot, so its deadline does not burn down in a queue.

### Event wake rules

- Only events that happen after a subscription was armed count. A new subscription, disable followed by enable, or switching the feature off and on never treats earlier events as new.
- Events the employee produced itself never wake its own responsibility: a task it created or updated with its own task tools, and updates of its own runs. They are kept as skipped wake records for debugging. Tasks created by a one-off helper (`spawn_agent`) or by a colleague and assigned to the employee are not its own events and can wake it; the per-window event cap below bounds that.
- At most `max_event_wakes_per_period` runs per budget window start from events. Event facts past that cap are dropped for the window, not carried into the next one.
- Event content is data only: it becomes a quoted block in the run's description and is scanned for prompt injection. The assignee, deadline, tags, acceptance criteria, budget and tool access all come from the responsibility, never from the event.
- Events live in `events.db` and are kept for 7 days. If the gateway is down longer than that, the events in between are lost, and a gap record is written to the Activity Feed.
- Known limit: if the feature is switched off and back on while the gateway is down, the gateway does not see that switch, and the subscriptions' starting point is not reset.

## Spending and run limits

Every responsibility sets these limits. Money is in US cents, the same unit as `monthly_budget_cents`.

| Field | Meaning |
|---|---|
| `occurrence_cost_cap_cents` | Most one run may spend. Checked before every round; past it the task goes to a human. Spend is recorded after a round ends, so this is not a hard ceiling (see below) |
| `period_cost_limit_cents` | Most one budget window may spend (`budget_period`: `day` / `week` / `month`, calendar windows in `budget_timezone`) |
| `period_occurrence_limit` | Most runs per window (1–96) |
| `occurrence_hours` | Longest a run may take (1–72 hours); past it the task goes to a human |
| `max_consecutive_failures` | Failures in a row before the responsibility pauses itself (1–10, default 3) |
| `stop_at` | When the whole responsibility ends; required |

How it is counted:

- A run's spend includes the tasks recorded under it (sub-tasks and their sub-tasks). A task the employee creates during one of the run's rounds is placed under the task that round is working on by the system, from the round it is running, not from what the model asks for; a parent the employee names must be that task or one of its sub-tasks, or the creation is refused. If the round information the system passes along is present but empty or malformed, the creation is refused rather than treated as "no round". A task can hold at most 200 unfinished sub-tasks (finished ones do not count; schedules and reminders are not sub-tasks), and a chain may be at most 64 levels deep.
- The default parent applies only to rounds of a continuous responsibility (the run's task or a task under it, including a sub-task woken by the task board). In an ordinary goal task's rounds, and in task-board wake-ups of tasks outside any run, there is no default parent. Two rules are new in this release for every caller: a parent the employee names needs a relationship to it (v1.69 wrote it unchecked), and `kind="goal"` accepts a parent (v1.69 ignored it).
- The placement depends on the employee not being able to change how its own MCP server is started. Freezing the `duduclaw` entry of an employee's `.mcp.json` (command, arguments and environment) and normalising its environment at every start is a separate platform fix that must be merged before this feature; until it ships, an employee that rewrites that entry can create tasks outside the run.
- A sub-task's own wake-ups from the task board (the heartbeat) are charged to that sub-task, so they count toward the run it hangs under.
- Not everything lands in the tree. A task created through an MCP server that does not know the round (one started from Bash, or under the Grok and Gemini CLI runtimes, whose MCP configuration is a stored file) hangs under the run only if the employee names the run as its parent. Work handed to other employees and one-off helpers is not in the tree either (see "Not counted").
- The cap is checked before each round, after the previous round's spend was recorded. The round in progress, and sub-tasks working at the same time, can therefore push a run past the cap before the next check; how far is not bounded by this setting. The employee's own monthly budget (`agent.toml [budget]`) is also checked before every round and sends the run to a human when it is used up.
- Just before the system hands a round of a run to the AI runtime, it records that the round started. A round without that record costs nothing: one cancelled by the dispatch gate, one refused before it started (for example by the delegation check), and one still waiting in the queue (also across a gateway restart). Pausing and resuming a run therefore does not use up its cap. A round with that record counts as run even if the runtime failed afterwards, before the employee claimed the task, or the gateway crashed mid-round and the message went back to the queue.
- A round that ran but whose spend was not measured counts as the full per-run cap, never as zero. A usage record with no token counts at all (some runtimes report none, see below) counts as not measured. If cost data cannot be read at all, the responsibility does not wake, and an Activity Feed entry says why.
- Spend is recorded per run for every runtime: Claude, Codex, Gemini, Antigravity, Grok and OpenAI-compatible. Grok reports no token usage, so its figures are estimates from the length of the text sent and received. Antigravity and Gemini CLI runs that report no usage, a Codex run without a usage event, and a Claude round reached by failing over from another runtime (which reports zeros), count as not measured. With such a runtime, as the employee's or as the acceptance judge's (`[dispatch] judge_provider`), one unmeasured round uses up the whole per-run cap, so a run usually gets one round and goes to a human when that round is rejected. Creating a responsibility (command line and RPC) prints this as a warning, and `duduclaw doctor` lists such responsibilities.
- When a window's budget is used up the responsibility goes to budget-paused and resumes by itself in the next window, with the wake facts it collected merged into one run.
- Deadlines, round counts and spend are never reset by a retry or a restart; sleeping a day does not buy a day's extra budget.

Not counted (listed under `cost_not_counted` by `duduclaw responsibility get`):

- the trajectory simulation attached when a task goes to a human
- the narrative of a kickoff approval push
- the dispatch policy's choice of employee
- the action-guard judge inside the MCP server process
- work delegated to other AI employees
- one-off helpers started with `spawn_agent`
- tasks created without round information and not placed under the run (see above)
- cron routines and reminders created with `tasks_create` and a `schedule`

The notification score (see "Notifications") costs one utility-model call, and that call is charged to the run.

## How a run ends

| Task result | Recorded as | Counts as a failure |
|---|---|---|
| Accepted | done | No; the failure streak resets |
| Failed or cancelled | failed / cancelled | Yes |
| The employee parked it with `tasks_block` | blocked | Yes, and a notification is sent |
| Stopped in the dashboard by a Manager or higher, or stopped from the command line after an Admin's approval | stopped | No |
| Stopped in the dashboard by an account with only Operator access | stopped | Yes |

The last row exists so that the employee's operator cannot dodge the failure-streak pause by stopping a run that is failing.

A run parked as needs_human is not settled. It blocks the next wake until someone handles it in the dashboard.

A run past its deadline goes to a human even when its responsibility is paused or has expired, and its slot is freed at that point (until the deadline, a paused or expired run keeps its slot); pausing never leaves a run hanging. A run whose employee has run out of its monthly budget also goes to a human, with the budget as the reason.

Run tasks are managed by the system: neither an AI employee nor the dashboard's ordinary task edit can reassign one or change its title, description, acceptance criteria, tags, deadline or other control fields. Status and progress updates still work. Handing an employee's open tasks to someone else (the departure hand-off) leaves its runs with it; stop them or disable the responsibility instead.

## Command line

```bash
duduclaw responsibility list [--agent <employee>]
duduclaw responsibility get <id>
duduclaw responsibility occurrences <id>
duduclaw responsibility fires <id>

duduclaw responsibility create --file contract.json [--confirm]
duduclaw responsibility update-contract <id> --file contract.json [--confirm]
duduclaw responsibility pause <id> [--reason <text>] [--confirm]
duduclaw responsibility resume <id> [--confirm]
duduclaw responsibility disable <id> [--reason <text>] [--confirm]
duduclaw responsibility enable <id> [--confirm]
duduclaw responsibility clear-failures <id> [--confirm]
duduclaw responsibility stop <task_id> [--confirm]
```

Every subcommand, the read ones included, is refused inside an AI employee's session.

### Contract file

```json
{
  "owner_agent_id": "support-lead",
  "objective": "Go through the support tickets still unanswered since yesterday and list the three I need to handle.",
  "acceptance_template": "At most three tickets, each with its ticket number and one sentence on why.",
  "schedule": { "cron": "0 9 * * 1-5", "timezone": "Asia/Taipei" },
  "event_subscriptions": [],
  "occurrence_hours": 2,
  "occurrence_cost_cap_cents": 50,
  "budget_period": "week",
  "budget_timezone": "Asia/Taipei",
  "period_cost_limit_cents": 300,
  "period_occurrence_limit": 7,
  "min_wake_interval_secs": 3600,
  "max_consecutive_failures": 3,
  "stop_at": "2026-11-01T00:00:00Z",
  "notification_policy": { "enabled": true, "on": ["result", "needs_decision", "paused"] }
}
```

Rules: a schedule or at least one event subscription is required; `min_wake_interval_secs` is at least 300; `period_cost_limit_cents` may not be below `occurrence_cost_cap_cents`; `stop_at` must be in the future and within `max_stop_at_days`; an event subscription's optional `timeout_at` must lie between now and `stop_at`, and if no matching event arrives by then the employee wakes once to handle "it did not happen"; the employee must exist.

An event subscription:

```json
"event_subscriptions": [
  {
    "event_name": "task.updated",
    "filter": { "all": [ { "field": "status", "op": "eq", "value": "blocked" } ] },
    "timeout_at": "2026-10-20T00:00:00Z"
  }
]
```

Without `filter`, every event of that name belonging to the employee counts.

### The approval step

The command line cannot prove who typed a command: an AI employee with Bash runs as the same OS user as you. So every state-changing action, `stop`, `pause` and `disable` included, takes effect only after an Admin approves it in the dashboard.

1. Run it without `--confirm`: it prints what would change and changes nothing. For `create` and `update-contract` the contract is checked here too (limits, schedule, the employee exists); an invalid one is refused before any request is filed.
2. Run it with `--confirm`: it files an approval request in the dashboard inbox. Only an Admin can decide this kind of request, and only in the dashboard; channel buttons and replies have no effect. A request left undecided for 24 hours is denied.
3. After the Admin approves it, run exactly the same command again (with `--confirm`) within `operator_approval_minutes` (30 minutes by default). The change is applied once.

An approval is bound to the action, the target, the exact change and the target's current state. Once the state changes, an old approval no longer applies: if a pause was approved and someone then resumed and paused again, that approval is dead, and it is withdrawn with an audit row. Identical requests (same action, target and content) are merged into one. One action on one target can have at most 3 different undecided requests; a fourth is refused until one is decided or expires. A target gets at most 2 pushes per hour, requests beyond that stay in the dashboard inbox with an audit row, and no reminder pushes are sent for these requests.

Since the release after v1.70.0 a waiting request made against a state that has since changed is withdrawn (`state_changed`) when the same command is run again, and a new one is filed, so from then on it no longer counts toward the 3; until that re-run it keeps counting until it expires after 24 hours. An approved request whose state changed is withdrawn the same way on the re-run. If two terminals re-run the same approved command at once, one applies and the other prints that the approval was already used or invalidated by another run and does nothing. The command also refuses inside an AI employee's session when any variable the gateway sets on an employee process is present, even empty. The rules are shared with the other operator commands; see [Operator command-line approvals share one gate](../features/05-security-defense.md#operator-command-line-approvals-share-one-gate).

`stop` is bound to the task's current state as well, and a running goal task changes state often, so an approved command-line stop frequently no longer matches when you re-run it and has to be requested again. Use the dashboard button for stopping.

The approval card is built by the server. It shows the AI employee, the action, the spending and run limits, the schedule and the event wake conditions. The responsibility's job text is shown as quoted data cut to 80 characters, since someone else may have written it. The card also says that the request came from the local command line and that the system cannot tell who ran it; if you are not sure who filed it, deny it.

Every request, apply and refusal writes one security-audit row: `responsibility_cli_requested`, `responsibility_cli_applied`, `responsibility_cli_refused`.

With the feature off, actions that widen permissions or spending (`create`, `update-contract`, `enable`, `resume`, `clear-failures`) are refused outright; `pause`, `disable` and `stop` can still be requested.

### The three controls

| Control | Effect | Not affected |
|---|---|---|
| `pause` | No more wake-ups; the open run gets no further round | A round already running finishes, and submitted work is still judged; wake facts keep collecting and merge into one run after `resume` |
| `disable` | Cancels every subscription and drops unprocessed wake facts | The open run (stop that task separately). `enable` re-arms the original conditions; nothing missed while disabled is replayed. `enable` keeps the failure streak: a responsibility that was paused for failures comes back paused for failures, and only clearing the failures resets the streak (the RPC needs Manager or higher; the command line needs an Admin's approval). An approved `update-contract` can still raise `max_consecutive_failures` for later runs |
| `stop` | Stops one task and all its sub-tasks | The responsibility itself, which still wakes on schedule |

To stop everything: `disable` first, then stop the open run.

## The responsibilities page

`/responsibilities` (navigation: Work → Responsibilities) lists the responsibilities of every employee you are bound to, grouped by employee. Each row shows the state, whether runs are read-only, the next wake-up, this budget window's spend against its cap, this window's run count against its limit, the failure streak against its limit, and the end date. Everything comes from the existing RPCs (`responsibilities.list`, `.get`, `.occurrences`); the page draws no sample data.

- **Feature off.** `responsibilities.status` reports both switches. When `[responsibilities] enabled` or `[dispatch] enabled` is off, the page says so and names the two keys. Existing rows can still be paused or turned off (the server accepts only those two narrowing actions while the feature is off).
- **Actions.** Pause, resume, turn off and turn on need Operator on the employee; clearing the failure count also needs Manager or higher; every call sends the `control_epoch` it showed, and a "changed in the meantime" answer refreshes the list instead of retrying. The detail dialog shows the acceptance text, the open run (with the existing stop button) and the last 20 runs with their outcome and charge.
- **Create.** Three templates: **daily briefing (read-only)**, **weekly review (read-only)** and **custom**. The read-only templates always set `lane = "explore"`, run at most once per budget window with a 1-hour run limit and a small cap you can change; the custom template is read-only unless you untick it. The server validates every value as it does for the command line.

### Read-only runs (`lane = "explore"`)

A responsibility's contract may carry `"lane": "explore"` (RPC `responsibilities.create` / `update_contract`, and the command line's contract file). The lane is stored in the contract's scope, so it is part of the contract hash; a responsibility without it is byte-identical to before. Any other value is refused (`invalid_lane`).

Every round of every run of such a responsibility runs in the read-only explore lane introduced for the heartbeat proactive check. Work woken by an MCP Events delivery uses the same lane ([MCP ecosystem](mcp-ecosystem.md)): the dispatcher sets one flag for both, so the rules below are the same for either source.

| Runtime | What happens |
|---|---|
| Claude CLI | `DUDUCLAW_LANE=explore` on the spawn (inherited by the DuDuClaw MCP server, which then lists and runs only `read` / `draft` tools), `--tools` limited to `Read`, `Glob`, `Grep`, `WebFetch`, `WebSearch` minus `denied_tools`, and `--allowedTools` limited to DuDuClaw MCP tools and those built-ins (never wider than the employee's own allowlist; tools of other `.mcp.json` servers are not auto-approved); other `.mcp.json` stdio servers go through the gated proxy and list only `read` tools |
| OpenAI-compatible runtime | No built-in tools; the DuDuClaw MCP child gets `DUDUCLAW_LANE=explore`; no `agent.toml [mcp.external]` server is mounted in the lane, and this runtime never starts `.mcp.json` servers |
| MoA model, `inference_mode = "local"` | Refused (`explore_lane_unsupported`); the hybrid local offload is skipped in the lane |
| Codex | `-s read-only` and `approval_policy=never` whatever the employee's own capability level (an operating-system sandbox that blocks writes and network and ignores config allow lists), the DuDuClaw MCP server registered with `DUDUCLAW_LANE=explore`, no third-party server added; Codex 0.156.1 rejects every MCP call under `-s read-only`, so no tool is callable (verified on 0.156.1 on 2026-09-24, not re-run for this change) |
| Gemini CLI, Antigravity, Grok, generic CLI | Refused: the round fails before dispatch (`explore_lane_unsupported`), and the same refusal sits in each of those runtimes' `execute` in case a failover reaches them. Antigravity has no read-only mode (`agy --sandbox` restricts the terminal only, and its user-level `permissions.allow` is shared by every employee of the OS user); Gemini CLI, Grok and generic CLIs have no mechanism shown to be read-only |
| Task sandbox (`[container] sandbox_enabled`) | Refused before dispatch (the sandbox gives the employee a shell) |

A refused round counts as an unsuccessful run, so the failure streak eventually pauses the responsibility. An unreadable lane fails the round too (`explore_lane_unreadable`). Not covered: a run's sub-tasks woken later by the heartbeat run outside the lane (in the lane, `tasks_create` is a `modify` tool and is refused, so a read-only run cannot create them itself); not exercised against a live gateway.

## The task detail page

### Directions (steering)

A goal task's detail page has a "Direction for the next round" panel. What you write is handed to the AI employee when the next round starts; the current round is not interrupted.

- Up to 4,000 characters per direction, and at most 10 undelivered directions per task.
- A direction counts as delivered only when its round was actually dispatched; the page then says "Queued for round N". That means handed over in that round's message, not adopted. If the round was refused or cancelled before it started, the direction goes back to waiting for the next round. Once the round started, the direction stays delivered even if the round failed afterwards.
- Directions never change the acceptance criteria. They are handed over with a note that the acceptance criteria win on conflict, and the judge reads only the criteria frozen at creation and never sees the directions.
- Each direction is scanned for prompt injection. A hit does not block it (it is the operator's own text, though it may be pasted from outside); the task page marks it with "This note contains text that looks like instructions", and the employee receives it with a note that it is reference only and grants no new permission or acceptance criterion.
- If the task finishes or is stopped first, undelivered directions are marked as not sent, with the reason.
- A round that carries directions always runs with the single employee, never as a team round.
- Permission: Operator or higher on the AI employee. AI employees have no steering tool.

To change the scope (different acceptance criteria), stop the task and create a new one, or use `update-contract` on a responsibility, which changes the next run while the open run keeps its contract.

### Stopping

The "Stop task" button on the task detail page works on any task that is not locked, for anyone with Operator or higher on the AI employee. It runs with a real identity, so it takes effect at once with no further approval. It asks for confirmation twice.

Opening the confirmation dialog records the task version you are looking at. If the task changes before you press Stop (for example, another round finished), the dialog says "The task changed after you opened this dialog"; check where it stands and press Stop again. The system never re-sends with the new version on your behalf. If the task has already ended, it tells you there is nothing to stop.

After a stop:

- The task and all its sub-tasks are cancelled for good; to redo the work, create a new task.
- A large tree is handled in batches: the root and the first batch are cancelled at once, the rest on the following passes. Meanwhile nobody can create a sub-task under the tree, claim or complete a member of it, and nothing dispatches a member: neither the dispatch engine nor the heartbeat's task-board wake-ups.
- A round waiting in the queue is cancelled, a task-board wake-up waiting in the queue for a member is cancelled, pending approvals bound to these tasks are withdrawn, and an external action that was prepared but not started will not run.
- Delegations the run already sent are separate tasks and are not stopped.

What the page shows:

| State | Page text | Meaning |
|---|---|---|
| `cancel_pending` | Stopping: work in progress finishes first, then it stops. | Something is still running: a round already started, a claim whose lease has not expired, a team round, or an external action being executed. A round cannot be interrupted midway, so the stop waits for it. "Check again" refreshes |
| `cancel_pending` (other text) | Stopping: some team work may still be running and cannot be checked from here yet. It will be confirmed later. | This gateway cannot tell whether a team round is still running and holds until the lease limit before deciding |
| `stopped` | Stopped | Nothing is running any more |
| `stopped_uncertain` | Stopped, but the result of some work or outside actions cannot be confirmed. Please check them yourself. | Something could not be confirmed: a claim whose lease expired with no finished round on record (a round cancelled before it ran does not count as finished), an external action with an unknown result, or a tree larger than the scan limit |

Neither stopping nor stopped means an external action already sent was undone. When an action's result is unknown, someone has to check in the other system whether it went through.

## The three employee tools

With the feature on, three tools appear in the AI employee's tool list. Full rules are in [MCP tools](mcp-tools.md#responsibility_get--responsibility_followup--responsibility_ask--responsibility-tools).

- `responsibility_get`: read your own responsibilities.
- `responsibility_followup`: during your own run, schedule one follow-up wake-up.
- `responsibility_ask`: during your own run, ask the operator one question; the answer reaches your next run as data and grants nothing.
  The notice about the question carries only the responsibility's name and a dashboard link, with no buttons, and no reminder is sent, so in practice it is answered in the dashboard inbox. An answer that does arrive another way (a channel decision on that request from a trusted sender) is handled the same. An answer only wakes one run; it changes no limit, state or permission and decides no other pending approval.

An AI employee cannot create, change, resume or re-enable a responsibility, and cannot raise its limits.

## Notifications

When a run has a result, when a decision is needed, when a responsibility pauses itself or expires, and when a stop ends with an unknown result, an Activity Feed entry is always written first. A push also needs every one of these, in order:

1. The responsibility's `notification_policy` is enabled and lists that kind of event in `on` (all off by default).
2. The employee's `agent.toml [proactive] enabled` is true (off by default).
3. The window's push count is below `max_notifications_per_period`. The gateway keeps this count itself; Activity rows an employee posts do not count.
4. The proactive gate's score passes. This step costs one utility-model call, charged to the run.

A push carries only the responsibility's name, its state and a dashboard link, never a result summary or event content. Quiet hours delay a push; they do not drop it.

## Security model and known limits

- The command-line gate is a speed bump. The Bash-lane check stops AI employees and unverified callers from running the `responsibility` subcommand of `duduclaw` / `duduclaw-pro` (reads included), but it reads command text: a renamed binary or a command assembled from variables gets through. An employee with unrestricted Bash can bypass the command-line gate, and can even edit the databases directly. Real isolation is not granting Bash, or the [task sandbox](task-sandbox.md).
- Emergencies. The dashboard's "Stop task" button stops the current run at once, but the responsibility keeps waking on schedule. The dashboard has no page to pause or disable a responsibility. To keep it from waking, either run `duduclaw responsibility disable` and wait for an Admin's approval, or set `[responsibilities] enabled = false` in `config.toml` (the dashboard only offers the raw settings editor for it); with the switch off, a run already in progress still finishes as an ordinary goal task, so stop it with the button as well. A `stop` from the command line waits for approval.
- A stop requested from the command line is checked in the command-line process, which cannot see a team round the gateway is just starting. For an ordinary goal task (not a run, which is always solo) there is a narrow window where the command line reports `stopped` while that team round still runs to its end.
- Responsibility runs, and rounds that carry directions, always run with the single employee, never as a team round.
- The model calls listed under "Not counted" are not counted toward spending, and the per-run cap is checked between rounds, so a run can exceed it by what its last round and its parallel sub-tasks spent.
- A responsibility narrows event sources and notification targets only. It cannot narrow tool access: a run has exactly the tools the employee already has.
- Event wake-ups see only `task.created` and `task.updated` written to `events.db` by the MCP task tools, and events older than 7 days are lost after a long outage.
- The 200-sub-task limit is checked only for an AI employee's `tasks_create`, and it is counted before the new task is written, so calls made at the same moment can go slightly past it. The gateway and operators are not limited.
- Spending warnings for runtimes that may not report usage appear on the command line and in `duduclaw doctor` only; the dashboard does not show the `usage_warnings` that `responsibilities.create` returns, and an external judge (`[dispatch] judge = "external"`) is not checked.
- When several gateways are started on the same data directory, only the one holding `<home>/locks/gateway.lock` runs the responsibility wake pass, the stop reconciliation, the steering sweep and the repair of half-done durable rounds; the others still dispatch goal rounds as before (a durable round's fixed message id keeps it from being sent twice).
- If the task board cannot be read for an employee three times in a row, an Activity Feed entry says that employee's task-board wake-ups have stopped (once per gateway process); every failure is also logged as a warning.
- The `/responsibilities` page covers listing, creating and the pause / resume / turn off / turn on / clear-failures controls; contract updates and responsibility-level stop still go through the command line.

## For developers

- Code: `crates/duduclaw-gateway/src/responsibility/` (`wake.rs`, `events.rs`, `cost.rs`, `stop.rs`, `steering.rs`, `operator_gate.rs`, `notify.rs`); tables live in `tasks.db`.
- Dashboard RPC (`handlers/responsibilities_rpc.rs`): `responsibilities.status` / `create` / `list` / `get` / `occurrences` / `fires` / `update_contract` / `pause` / `resume` / `disable` / `enable` / `clear_failures`, plus `tasks.steer` / `tasks.steering` / `tasks.stop` / `tasks.stop_status`. Reads need Viewer and changes need Operator on the owning employee, except `clear_failures`, which needs Manager; every write appends an audit row. `responsibilities.create` also returns `usage_warnings` (runtimes that may not report usage). `system.update_config` accepts `responsibilities.enabled` and `goal_loop.steering_enabled`.
- Command line: `crates/duduclaw-cli/src/responsibility_cmd.rs`; approval kind `responsibility_operator_change`, decided only by an Admin in the dashboard (`decided_by` starting with `dashboard:`).
- Bash-lane block: the shared operator-command matcher (`duduclaw_core::bash_operator_command_decision`, `GuardDecision::BlockedOperatorCommand`) with the list in `responsibility_cmd::OPERATOR_COMMANDS`, chained after the LINE inbox commands in the agent-file-guard hook.
- Every responsibility, steering and stop RPC re-reads the caller from the account store (`handlers/task_privacy.rs::live_reader_context`); `tasks.steer` / `tasks.steering` / `tasks.stop` / `tasks.stop_status` also go through the task-content gate (`authorize_private_task_read`: agent access plus the task's audience).
- The task this round runs for reaches the MCP server as `DUDUCLAW_TASK_ID` for every runtime (`runtime::round_task_env`); approval cards use the same value.
