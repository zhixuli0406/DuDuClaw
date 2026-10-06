# Review evidence and workflow drafts

Open a task's **Review evidence and workflow drafts** panel. Capture a review snapshot before accepting deliverables or creating a draft. Its hash binds the task contract revision, artifact bytes and audience. An unchanged hash proves unchanged bytes; an employee's success summary remains a self-report. Legacy artifacts remain unverified. Changed or missing files and changed task contracts make the saved snapshot stale. Only the task's latest snapshot can be accepted: if another reviewer captured a newer one while yours was on screen, accepting the older one is refused. A snapshot that could not hold every deliverable cannot be accepted. Capturing and accepting a snapshot need an Operator binding on the AI employee, as do test runs, review requests, applying and revoking.

A completed task can provide source DATA for a disabled proposal. Supply an installed skill ID, typed input/output schemas and sequence steps, the tools it needs, five fixture inputs with explicit expectations, budgets, freshness and timezone. The server pins the skill, source snapshot and current creator policy. Saving does not authorize tools or enable a schedule. The tools a draft needs are compared only with the AI employee's current allowed and denied tool lists, so the comparison is an upper bound: permission switches, tools that need approval, task-scoped grants and the other gates are not part of it, and they still apply when the workflow runs. Stop conditions are no longer part of a draft: nothing executed them. Runs stop on the budgets, the count limits and the consecutive-failure limit below.

Run all five fixtures: normal, empty, expired, injection and missing permission. Runs use the production runner and recorded operations. Staging resources must be explicitly allowed by the operator's configuration; this does not provide operating-system isolation. The panel displays the actual run ID/status, result hash and assertion outcomes. An expected refusal can match an assertion while the run remains blocked. A model's statement that a fixture passed is not evidence. Each negative case must carry what its kind names, and must end with that gate's own error code: an expired case gives an `observed_at` older than the freshness limit and ends with `workflow_input_expired` (or `workflow_read_data_expired`); an injection case gives an input the input guard blocks and ends with `input_injection_blocked`; a missing-permission case ends with the permission refusal `workflow_read_denied:-32003` or `workflow_effect_refused:-32003`. A schema error, a refusal by a person, or a scripted decision in the fixture does not count for these three cases.

Request activation only after current matching evidence exists for all five fixtures. The human decision covers the exact draft, skill, fixture digest, effect templates, audience, budgets and optional schedule. Approval creates no synthetic run receipt. Applying the revision reloads the actual service state; an unapproved, expired or changed revision is refused. Re-run expired fixtures before another activation attempt. After activation the accepted revision stands on its own: later edits to the source task or its artifacts no longer stop runs, while a change to the installed skill stops them and a change to the employee's authority suspends the activation (see below).

Fixture effects require exact resource IDs in the operator-owned `config.toml`:

```toml
[workflow]
fixture_environment = "staging"
allowed_origins = ["https://staging.example.com"]
fixture_task_ids = ["test-task-id"]
fixture_cron_ids = ["test-cron-id"]
```

Register only disposable test resources. `tasks_update` requires a listed `task_id`; `update_cron_task` requires a listed `id` and refuses name-based selection. Missing, malformed or empty lists grant no access. The server checks resolved arguments after native rewrites during preparation and immediately before execution, so removing an ID also blocks a previously prepared operation. These lists do not replace employee ownership, capability or human-approval checks.

## Who can see a task's content

The audience is not a privacy setting an operator chooses. It comes from the hand-off packets that AI role members write during a team round (`team_handoff`): each packet may list who its content may reach. Role names in that list (such as `verifier`) only limit the flow between roles and never limit people. Only entries in the three human-facing namespaces `user:<account id>`, `role:<dashboard role>` and `channel:<channel>` do; a task whose packets carry none of them follows its ordinary access rules. When several packets carry such entries, only the keys they all share remain; if they share none, only admins can see the content.

What it limits: for a dashboard reader who is not an Admin, everything that shows what the task did follows it (the review panel and drafts, artifacts and downloads, file changes, rounds and judge feedback, the timeline, comments, role turns, run transcripts, approval cards built from the task, live dashboard updates); such a reader sees only the task's card (title, status, owner, times) in lists. A dashboard user is matched by `user:`, `role:` or `channel:dashboard`; a list with only `channel:telegram` hides the content from every non-admin dashboard user. Reads of task content and the task changes that act on it (status, decision, assignment, archive, pin, rename, removal) re-read the account's role, status and bindings and the task's list on every request, so withdrawing a binding, lowering a role or suspending an account also stops an open connection from doing them; live updates catch up within 2 seconds (see *Live dashboard updates* below). Other dashboard requests keep the checks they already had.

What it cannot limit: an Admin account (its role re-read at that moment) and the gateway admin token always read and decide, including a private task waiting for a person. An AI-written list cannot remove oversight.

When a task is removed, what outlives it (round transcripts, activity entries, workflow drafts and runs started from it) can be read and acted on by admins only.

Channels: `channel:X` names a whole chat platform, not one chat: every chat on that platform connected to this gateway counts as inside the list. A chat channel X gets the full notices and can decide from its buttons when the task has no human-facing limit or the list contains `channel:X`. Otherwise it gets a notice that only says something happened, with a link to the dashboard, and a press on a button is refused. A task whose packets name only roles behaves on every channel exactly as before.

Traces: the first packet that limits people writes an Activity Feed entry and a security audit event naming the task, the round, the role and account that wrote it, and the keys. An admin's task page shows the limit and which packet set it. If the packets cannot be read or are damaged, everyone except admins is refused, the task page says so to admins, and the Activity Feed records it once.

A private file is bound to its name in the attachments folder. A second name for it (a symbolic or hard link) is refused rather than treated as a file without restrictions; an AI employee can still copy what it produced into a new file, which is a limit of this design. A file with no binding is checked against the sign-in token's access, so a withdrawn binding applies to it when that token expires. After reconnecting, retrieve drafts from the task panel using the server's paginated list.

### Live dashboard updates

Every live update the gateway sends a dashboard tab is checked against that tab's account before it leaves; the account is re-read at most every 2 seconds per tab.

| Updates | Who receives them |
|---|---|
| Gateway status, update availability and progress, maintenance mode | every tab, including the lock screen |
| A page the dashboard is asked to open, a removed task's id, a chat platform's queue being full | every signed-in account |
| Task created or changed, task comments, Activity Feed entries | accounts with access to the task's AI employee; outside the task's list, a changed task arrives as its card only, an Activity Feed entry without its text, and a comment not at all; an entry about a removed task reaches admins only |
| Conversations, plans, canvases, routines, memory, skills and channel settings of one AI employee | accounts with access to that AI employee |
| Sign-in and install output, chat delivery failures, and any kind of update not listed here | admins only |
| The gateway log stream (Logs page) | Managers and Admins, re-checked against the current role |

### Known limits

- AI employees read tasks through their own tools (`tasks_list`, the task board in their prompt) under their own access rules, without the list. The list limits what people see on the dashboard and in chats.
- The log stream shows everything the gateway logs, across all AI employees, to every Manager and Admin.
- A dashboard tab opened before the first account was created keeps the gateway administrator's access until it is closed.

## Running an activated workflow

An activated revision runs on its schedule or when the operator triggers it. Each trigger creates one run; the same scheduled occurrence or the same manual request ID always maps to the same run.

**Approval and question steps.** When a run reaches an approval step, a question step, or an effect your policy marks `ask`, it stops and the card appears in the inbox. In the dashboard, deciding a step card needs a current Manager or Admin who has access to the employee and is in the run's audience; anyone else may see the card without its binding details and cannot decide it. Step cards are decided in the dashboard only; the people who may decide get a plain notice in their verified chats (see *Who is told* below), and a reply there does not decide anything. Deciding the card queues the same run again in the same transaction as the decision (if the gateway stops between the two, the background check adds the missing queue entry within about a minute); nothing re-runs completed steps. An approved card lets the step continue, but the run still re-checks policy, audience, the grant and the input age before doing anything. A refused card ends the run as `failed` with `workflow_approval_denied`. A card nobody decides expires with the run's waiting window (24 hours, or the grant's expiry if sooner); a background check notices the expiry within about a minute and ends the run as `failed` with `workflow_approval_expired`, also when the run's own deadline passed at the same moment. If the employee's authority changed while a card was waiting, the card can no longer be decided and the run stays waiting until that window ends. Each step has at most one decidable card.

Freshness is judged by when data was actually read. The run input's age (`input_max_age_seconds`) is checked when the run starts and again right before every effect whose input comes from the run input; data read by an earlier step must be equally fresh when an effect uses it. An effect whose input is fixed in the definition (a literal, possibly passed through an approval step) is not held to the input's age, so a person taking hours to approve it does not make it fail. A run stopped because its data went stale is blocked with `workflow_input_expired` or `workflow_read_data_expired` (failure class `stale`), which does not count toward the consecutive-failure limit.

**One gateway per data directory.** At start the gateway takes a lock on `<home>/locks/gateway.lock` for as long as it runs. Only the holder releases interrupted runs, delivers queued workflow work and runs the background check; a second gateway started on the same data directory logs an error and leaves workflows alone. `duduclaw doctor` shows the holder in its 單一 gateway row and warns when a gateway was refused. The holder's `pid=… since=…` line is kept in the sidecar `<home>/locks/gateway.lock.holder` (the lock file itself stays empty, because on Windows no other process can read a locked file).

**Restarts and crashes.** A gateway restart releases the runs the old process was executing and continues them. A completed step is never repeated. If the process stopped while an effect was running, the run ends as `uncertain` with `effect_outcome_unconfirmed` and the operation is shown on the run; the effect is not sent again. An administrator resolves it from the operations list. Infrastructure errors (a busy database, the tool process not starting, another process still holding the operation, settings that cannot be read for a moment) are retried a few times and are never treated as a refusal of the effect; if they keep failing the run is blocked with `workflow_transient_retry_exhausted` and that block does not count toward the consecutive-failure limit. Upgrading the binary changes the environment fingerprint, so runs that were waiting across an upgrade are blocked instead of continuing.

**Stopping.** `workflow_runs.cancel` (a Manager with an Operator binding on the AI employee) stops one run: the next step does not start, its pending cards are withdrawn, and an effect that has not begun can no longer begin. An effect that already began is not undone and stays visible on its step. Revoking the activation stops all future runs.

**Consecutive failures.** After `max_consecutive_failures` failed, blocked or uncertain runs in a row, new runs are refused with `workflow_consecutive_failure_limit`. An administrator can clear the count with `workflow_runs.reset_failures` (a reason is required) without re-activating; earlier runs stop counting. Cancelled runs, exhausted infrastructure retries, limit blocks and stale-data blocks never count.

**Schedules.** Enabling or re-enabling a routine starts it from that moment; occurrences while it was disabled are not run late. After downtime the routine runs once for the latest missed occurrence within the last 24 hours; older occurrences are given up and recorded on the routine (`last_status = skipped`, with the count). Times follow the routine's timezone: in a timezone with daylight saving, a local time that occurs twice runs twice and a local time that is skipped does not run. Taiwan has no daylight saving.

`workflow_runs.get` and `workflow_runs.list` return each run's status, error code, failure class and every step's state, including its decision card and operation state.

## Who approves an activation

Accepting a workflow version is decided by an Admin in the dashboard only. The card never offers buttons in a chat channel, a channel reply cannot decide it, and the plain notice sent to Admins says only that an activation is waiting and when it lapses. The card also shows when the activation would end (see *Activation validity*). The decision re-reads the deciding account's role at that moment. An Admin may approve a request they submitted; the card and the audit record (`workflow_activation_decided`) say that the submitter and the approver are the same person.

## Fixed effect targets

Every effect names the one record it may change, and that record is fixed when the workflow is activated. `tasks_update` is pinned by `task_id` and `update_cron_task` by `id` (selection by `name` is refused). The effect template's `resource_scope` must carry that id, and the step's input must produce the same id from the definition itself: a literal, possibly passed through identity, artifact or approval steps. An id taken from a read result, from the run input or from a transform cannot be activated. A tool without a fixed target cannot be an effect at all. The activation card lists each effect with its record.

## Limits: counts always, money only with prices

Count limits always apply, per `config.toml`:

```toml
[workflow.limits]
max_runs_per_month = 500    # formal runs per workflow, per UTC month
max_steps_per_run = 128     # step dispatches per run, retries included
max_reads_per_run = 20      # read attempts per run
max_effects_per_run = 10    # effect steps per run
```

The per-run and monthly budgets of a draft are in micro-units and are charged at operator-set unit prices:

```toml
[workflow.unit_cost_micros]
read = 0
effect = 0
process = 0
artifact = 0
approval = 0
```

The prices are your estimate per step, not an API bill or measured CPU time. All prices default to 0, and while every price is 0 the money budgets cannot stop anything: `workflow_runs.get` reports `pricing.money_limits_effective = false`, and only the count limits bind. The server charges each dispatch in the same transaction as the step's running checkpoint, before the step does anything: a read attempt each time, an approval or effect step once. A price that cannot be read is treated as unknown and charged at the cap (it uses up what is left of the run's budget). An effect cannot be claimed without its charge in the ledger. Reaching a budget or a count ends the run as `blocked` (failure class `limit`, e.g. `workflow_budget_exhausted`, `workflow_limit_reads_per_run`); such runs do not count toward the consecutive-failure limit. An invalid `[workflow.limits]` value refuses every run until it is fixed.

The counts are also checked against the definition itself. A definition with more steps, reads or effects than one pass may use, or whose cheapest pass already costs more than the per-run budget at the current prices, cannot be activated. If the limits are lowered later so that an activated definition no longer fits, the next trigger is refused and the activation is suspended with reason `limit`. The same happens after `max_consecutive_failures` runs in a row ended on a limit. In both cases Admins are told.

## Suspended activations

The activation remembers the employee's authority as it was when an Admin accepted it: the effective `[capabilities]`, `[permissions]` and `[agent]` reports-to / department / role (a preset's resolution included), `CONTRACT.toml`, the preset binding, the `org.toml` records of the employee and its managers, `config.toml` `[delegation]`, `[acp]`, `[provenance]` and `[integrations]`, and `KILLSWITCH.toml`. Other settings (channel tokens, schedules of other routines, tick sources, log level) do not matter. When a run finds that authority changed, the activation becomes `suspended`: the current run is blocked with `workflow_policy_changed`, no new run starts, its routine is switched off, an Activity Feed entry and an Admin notice name the categories that changed, and `workflow_runs.get` / `workflow_drafts.get` show the reason. If the settings cannot be read, the run is retried a few times, and only settings that are still unreadable after that suspend the activation; a run that sees a changed setting reads it again before deciding, so a file caught halfway through a save does not suspend anything. Changes to `[redaction]` and to the custom redaction rules count too.

A suspended activation never runs again, by any path: it cannot be committed again, the stored record cannot move back to active, the grant behind it is revoked in the approval ledger, and undoing the edit does not resume it. To continue, create a new draft revision from the task (it records the current authority), run its fixtures and request a new activation.

## Activation validity

Fixture runs prove the draft at the time it is accepted: every fixture must still be fresh when the Admin approves. After that the activation is valid for `config.toml [workflow] activation_days` days (default 30, allowed 1 to 365; any other value refuses new activation requests until it is fixed). The approval card and the draft page show the end date. Freshness of the fixtures does not shorten it, because every run re-checks the skill, the employee's authority, the environment, the audience and the grant anyway.

Three days before the end, Admins are told once. At the end the activation becomes `expired`: its routine is switched off, no new run starts or continues, an Activity Feed entry is written and Admins are told. There is no extension. To continue, create a new draft revision, run its fixtures and request a new activation.

## Who is told

Notices never contain workflow content: they say that something waits or changed, for which AI employee, and until when. They go to verified chats only.

| Event | Recipients |
|---|---|
| Activation request waiting | Active Admin accounts |
| Activation suspended or expired, or 3 days before expiry | Active Admin accounts |
| Step card (approval or question) waiting | Accounts that may decide it: Manager or Admin with Operator access to the employee, inside the run's audience |

A deployment where nobody has linked a chat to their account sees all of these only in the dashboard inbox and the Activity Feed.

## Known costs

An effect is charged before it waits for a person; a refused or expired card does not refund it. A processing or artifact step that is re-entered after a crash is charged again. With all prices at 0 this only uses up counts.

## Fixture decisions

A fixture can script the human decisions of its run, by step id:

```json
"decisions": {
  "confirm": {"decision": "approve"},
  "update":  {"decision": "deny"},
  "pick":    {"decision": "answer", "text": "B"}
}
```

`approve` and `deny` apply to approval steps and to effects your policy marks `ask`; `answer` applies to question steps. A fixture run records these decisions directly (decided by `fixture:<run id>`); it creates no inbox card and sends nothing to any channel. A step without a scripted decision fails with `fixture_decision_missing`. A denial lets you assert the refusal path (`workflow_approval_denied`). Scripted decisions exist only for fixture runs: the server refuses them for an activated run, so a formal run always waits for a real person.

## Running a routine by hand

A manual run of an activated routine needs a request id, so that repeating the same request reconnects to the same run instead of starting another: the dashboard `cron.run_now` call must carry `request_id` and returns the `run_id` and its current status. The MCP tool `run_cron_task` and other paths that cannot supply a request id are refused for these routines.
