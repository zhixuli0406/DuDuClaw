# Goal loop

Drop a goal into a conversation and the AI employee plans, executes, and self-checks its own work, then comes back to you when it's done or stuck. This page covers the `/goal` command in channels, the AutonomyLevel tiers, the relevant config keys, and what the buttons mean when a task escalates to a human.

The dispatch engine has been on by default since v1.59 (with no goal tasks queued it only does periodic SQLite polling; an LLM call only happens once a task actually reaches review — see "Two-stage acceptance judging" below), so plain question-and-answer conversations are completely unaffected. To turn it off, set `[dispatch] enabled = false` in `config.toml`, or flip the "Dispatch engine (派工引擎)" switch under Settings → Automation (設定 → 自動化) on the dashboard (hot-reloads, no restart needed).

---

## The `/goal` command

Type this to an AI employee in any connected channel (Telegram / Discord / Slack / LINE / …):

| Command | Behavior |
|------|------|
| `/goal <goal description>` | Creates an autonomous goal task assigned to the AI employee in the current conversation. With no separate acceptance criteria, the goal description itself becomes the acceptance basis. |
| `/goal <goal> \|\| <acceptance criteria>` | Split with `\|\|`: the first half is the goal, the second half is the acceptance criteria (what the judge checks against). |
| `/goal <goal> \|\| <acceptance criteria> \|\| outcome:<spec>` | Adds a layer of structured outcome verification (see "Structured outcome verification" below). A zero-cost deterministic check runs before delivery; anything that fails goes straight back for revision without spending a judge call. |
| `/goal status` | Lists the AI employee's in-flight goal tasks (short code / status / round number). |
| `/goal` | Shows usage help. |

**Example**

```
/goal Compile this batch of customer data into a monthly report and send it || The report needs a monthly revenue chart, send to boss@example.com
/goal Produce the Q3 monthly report || Include a monthly revenue chart || outcome:files:report.docx
```

Creating a goal returns a confirmation message with the task's short code, its round cap, and a note that "I'll notify you here when it's done or stuck." Progress updates and human-needed notifications are **pushed back to the conversation where you created the goal** (the source channel), not just to the AI employee's `[proactive]` notification channel.

> If the dispatch engine is off (`[dispatch] enabled = false`), the task is still created, but the confirmation message tells you it won't start running on its own.

---

## Goal contract: frozen at creation, never quietly edited later

The moment a goal is created, its acceptance criteria are frozen into an immutable baseline (`acceptance_criteria_baseline`). Every subsequent verdict — the first-stage evaluator, the MAV acceptance judge — reads only this frozen baseline, never a field that could have been edited afterward. This blocks contract drift in both directions: the AI employee can't loosen its own acceptance bar mid-task, and an operator can't mistake editing a dashboard field for actually changing what the judges check against.

- **The AI employee can't change it**: if an agent-identity call to the MCP `tasks_update` tool tries to change `acceptance_criteria` on its own goal task, the whole call is rejected, and an audit entry is logged (reason: `goal_contract_frozen`). The goal's `title` and `description` are what the judge reads as the goal, so an AI employee's `tasks_update` that touches either of them on a goal task is refused the same way. The control tags on the task (`outcome:…`, `grant:…`, `auto-research`) cannot be added, removed or reordered by an AI employee either (reason: `reserved_tag_change`).
- **The operator can edit the display copy, but that never changes what gets judged**: the dashboard's `tasks.update` can still edit the `acceptance_criteria` shown on a task (for example, adding a note for a human reader), but the frozen baseline value does not move with it. The judge and the evaluator keep grading against the standard set when the task was created. Changing the acceptance criteria for real means creating a new goal.
- Older tasks with no frozen baseline (created before this mechanism shipped) fall back to reading the mutable field, unchanged from before.

### Guidance when no acceptance criteria are given

`/goal <goal description>` (with no `||`) still creates the task as usual, using the goal description itself as the acceptance basis, but the confirmation message adds a hint to help you think through what to specify next time:

```
💡 No separate acceptance criteria this time — next time these four things will make it easier to pin down:
 • Goal: what needs to be accomplished
 • Input: what data or material is needed
 • Output format: what the result should look like (e.g. Word / Excel / a block of text / an image)
 • Constraints: style, deadlines, and other limits, plus "what counts as done"
 Try 3-5 concrete, checkable acceptance criteria (e.g. "the report includes a monthly revenue chart" rather than "make it good"),
 then resubmit with /goal description || criteria.
```

This hint doesn't appear when `||` acceptance criteria are given explicitly. Subtask decomposition under `planner_enabled` follows the same discipline: each acceptance criterion states the *result*, not the *method* (freezing HOW would fail a correct output that took a different but equally valid path); keep it to 3-5 criteria (a heavier contract makes subtasks harder to converge); anything out of scope goes under Non-goals instead of being crammed into acceptance criteria.

---

## Acceptance ledger (per-criterion)

The frozen acceptance criteria are one block of text. The judge is told to check them item by item, but nothing a program could read showed that every item had actually been handled. The acceptance ledger fixes that.

**When it is created.** A goal created through the dashboard or the MCP `tasks_create kind="goal"` tool gets a ledger at creation, from the frozen baseline: one criterion per non-empty line, numbered `C1`, `C2`, … in order. At most 20 criteria are tracked; lines beyond the 20th are folded into `C20` with a note, never dropped. The system also derives a stable id per criterion (`canonical_id(task id, "criterion", index, fingerprint of the text)`), so the model only ever echoes a short handle and never invents an id. Goals created before this feature, or while the mode is `off`, have no ledger and run exactly as before. Goals started from the chat `/goal` command and from a confirmed goal suggestion (both "立為目標任務" and "想一想") get a ledger the same way. Goals created by autopilot rules and the sub-tasks of a planner-decomposed goal have no ledger.

**What the worker sees.** Every dispatch round carries a `## 驗收帳本` section right after the `<state>` block, one line per criterion with its current status, and the reporting instruction. The worker ends its `tasks_complete` result summary with a tag:

```
<criteria_status>[{"id": "C1", "status": "covered", "evidence": ["wrote reports/summary.md"], "unresolved": []},
 {"id": "C2", "status": "blocked", "evidence": [], "unresolved": ["no permission to send mail"]}]</criteria_status>
```

The tag body must be exactly one JSON array (no prose around it, no extra fields), every handle exactly once. `status` is `covered` (evidence required, unresolved empty), `blocked` (unresolved required) or `candidate` (thinks it is done, wants confirmation; evidence required). Each evidence or unresolved entry is cut at 500 characters, at most 8 entries per field. A report that breaks any rule is discarded as a whole: the ledger stays as it was, its `invalid_reports` counter goes up, and one `criteria_status_invalid` event goes to `security_audit.jsonl` with the violation and at most 200 masked characters of the tag body. A round without the tag changes nothing and is not counted. The tag is removed from the text the judge reads and from the ✅ message pushed back to the chat.

**Modes** (`config.toml [goal_loop] criteria_ledger`, read at every round, no restart):

| Mode | Ledger | Judge |
|---|---|---|
| `off` | Not created, not injected, not parsed | Unchanged |
| `report` (default) | Created, injected, parsed, shown to people | Gets the worker's self-reported ledger as a reference block marked "self-report, not evidence"; its reply format is unchanged |
| `enforce` | Same as `report` | The panel must also return `criteria: [{"id": "C1", "pass": true, "reason": "..."}]` with every handle exactly once. A missing or duplicated handle is a FAIL for that criterion, and `correctness` passes only if every criterion passes and the aspect itself passes |

An unknown value reads as `report`. Moving to `enforce` follows the same observation discipline as `strict_reply_parsing`: switch it yourself after watching real judge rounds. Under `enforce` an external judge (`judge = "external"`) is not asked for per-criterion verdicts; its own pass/fail stands.

**What you see.** `tasks.timeline` returns `criteria_ledger` (`null` when the goal has none) with `mode` (the mode in effect now), `units[]` (`id`, `handle`, `text`, `status`, `evidence`, `unresolved`, `updated_round`), `last_report_round` and `invalid_reports`. A needs_human card for a goal with a ledger adds one line such as `驗收帳本：1/3 條已回報達成；C2 受阻（沒有寄信權限）；C3 尚未回報`. Each round's ledger is also kept on its iteration row for later review.

`needs_human`'s `pause_reason` and the ledger answer different questions (why the loop stopped versus which criterion is still open); they are not merged.

## Outer-loop progress board

Every state transition on a goal task pushes a short (one-to-three-line) progress message back to the source conversation:

- Started / retrying (round N of the cap)
- In review
- Rejected → revising and retrying (with a summary of the judge's feedback)
- Done ✅ (with a result summary)
- Stuck → needs your decision (also pushes approval buttons, tagged with a pause-reason category — see "needs_human button semantics" below)

The same task won't push the same state twice. If the source conversation no longer exists, it falls back to the AI employee's `[proactive]` channel; if neither exists, it's written only to the dashboard's Activity Feed, without interrupting you.

### Stall-timeout progress reports

Once a task is claimed (`in_progress`), if `[goal_loop] progress_report_minutes` (10 minutes by default) passes with no observable progress signal (measured against Activity Feed events — the `updated_at` field is refreshed periodically by the lease renewer and can't be used as evidence of "something is happening"), the driver pushes a single "still running after X minutes with no progress reported" notice (Activity Feed + source conversation), at most once per round.

This is **purely a report, not an intervention**: it doesn't redispatch, escalate, or cancel the task. The guardrails that actually act remain `stalled_secs` (redispatch), `iteration_cap` (escalate to a human), and `wall_clock_hours` (escalate to a human). Setting it to `0` (or any negative number) turns the whole feature off, with zero extra queries when disabled.

---

## Tool streak advisory

In the autonomous loop, an AI employee sometimes gets stuck calling the same tool with the same arguments over and over — it already has the result and just hasn't noticed it's repeating itself. This is different from "Stall detection" below: stall detection needs two full rounds (dispatch → review) to notice a stuck task, while the tool streak advisory watches the tool-call sequence **within a single round**, so it can warn before the judge even gets involved.

The trigger is a run of consecutive calls to the same tool with the same **masked** arguments (reusing the secret-masking the audit trail already does), and the warning escalates by threshold:

| Consecutive calls | Warning |
|------|------|
| 3 | Suggests re-reading the last result to check whether the needed information is already there, before wasting another round repeating the call |
| 5 | Notes the current approach may not be making progress, and suggests a different method or angle |
| 8 | Strongly suggests stopping the repetition: wrap up whatever result is already available and report it, or call `tasks_block` to explain what's blocking and ask for help |

The reminder is injected into the **next round's** `<state>` block, at zero LLM cost and **purely advisory**: it never blocks, retries, or vetoes a tool call or a dispatch round — whether to act on it is left to the AI employee. The reminder itself is deliberately excluded from `state_hash`, so it doesn't interfere with the existing (state, action) oscillation detection.

`config.toml [goal_loop] tool_streak_advisory` (`true` by default) turns this off entirely when disabled.

---

## AutonomyLevel: five levels of autonomy

Each AI employee's autonomy level is controlled by a single dial: `agent.toml [capabilities] autonomy_level`. Unset or unparseable defaults to **Approver** (conservative: only asks you when it's stuck or needs a human).

| Level | Behavior |
|------|------|
| `operator` | The loop never drives itself; a task sits idle after creation and a human advances it manually. |
| `collaborator` | Needs human approval before the first dispatch (kickoff approval); once approved, it retries on its own until done. |
| `consultant` | Same kickoff approval as collaborator. |
| `approver` | **Default**. No kickoff gate; escalates to a human only when stuck or genuinely needing one. |
| `observer` | Fully automatic; when it needs a human it only notifies, never waits (the task ends on its own). |

```toml
# agent.toml
[capabilities]
autonomy_level = "approver"
```

---

## Task-scoped tool grants (scoped_tools, v1.41)

High-risk tools can be declared "usable only with a grant": a tool listed under `scoped_tools` is refused until the AI employee holds a valid grant, and a grant lives only for the lifetime of a single task — it's revoked automatically the moment that task ends (accepted, rejected, escalated to a human, or cancelled), never carrying over to the next one.

```toml
# agent.toml
[capabilities]
scoped_tools = ["shared_wiki_delete", "odoo_execute"]  # these tools need a per-task grant
grant_ttl_secs = 3600                                   # hard cap on how long a grant can live (seconds), default 3600
```

Two ways to get a grant:

1. **The AI employee requests one itself**: calling the MCP tool `capability_request { tool, reason, task_id? }` turns into an approval request (through the same notification/dashboard flow as other approvals); once you approve it the grant takes effect, and an approval left undecided past its deadline counts as denied.
2. **Granted at goal kickoff**: adding a `grant:<tool name>` tag to a goal task grants it atomically when the kickoff approval (collaborator/consultant level) is approved, and it's reclaimed automatically when the task ends.

The check is always fail-closed: if the grant store can't be read, that's treated as no grant. Tools not listed under `scoped_tools` are completely unaffected.

---

## Configuration keys

### `config.toml` (global)

```toml
[dispatch]
enabled = true          # Enable the autonomous dispatch engine (includes the goal loop driver). Default false
policy = "fixed_hierarchy"  # Dispatch policy (which AI employee picks up a task). See "Dispatch policy" below. Default fixed_hierarchy
grounding_precheck_enabled = true  # Grounding precheck before acceptance (see "Grounding precheck"). Default true
two_stage_judge = true  # Run a cheap first-stage evaluation before acceptance (see "Two-stage acceptance judging"). Default true
strict_reply_parsing = "shadow"  # Judge replies under the strict JSON contract: off / shadow / enforce (see "Strict reply contract"). Default shadow
judge = "mav"           # Who makes the acceptance call (see "Swapping the acceptance judge"). mav / external (evaluator_only / human_only were removed in v1.69.0). Default mav
judge_provider = "antigravity"      # Optional: run the judge on another runtime (see "Running the judge on a different model"). Unset ⇒ the default utility runtime
judge_model = "gemini-3-pro-preview" # Optional: judge model id within that runtime. Unset ⇒ the default utility model
admission = "queue"     # What happens when ephemeral spawns hit the concurrency cap, "queue" or "fail". Default queue (see "Ephemeral spawn admission queueing" below)

[task_forward_model]    # Task-level forward model (see the section of the same name). On by default since v1.54
enabled = true

[goal_loop]
iteration_cap = 5        # Hard dispatch cap for hard goals; escalates to a human past this. Default 5
iteration_cap_simple = 3 # Dispatch cap for simple goals (dynamic judge depth). Default 3
wall_clock_hours = 24    # Wall-clock budget from creation (hours); escalates to a human past this. Default 24
max_concurrent = 3       # Cap on goal tasks in flight at once (prevents a spawn storm). Default 3
tick_secs = 30           # Driver polling interval (seconds). Default 30
stalled_secs = 600       # Seconds after dispatch with no claim before a task counts as stalled and can be redispatched. Default 600
planner_enabled = false  # When on, allows splitting a goal into a dependency DAG of subtasks (see "Parallel subtasks"). Default false
resume_on_restart = "pause"  # What happens to in-flight goal tasks on gateway restart, "auto" or "pause" (see "Restart behavior"). Default pause, switchable under Settings → Automation on the dashboard
progress_report_minutes = 10  # How long a claimed task can go without a progress signal before one report fires; `0` disables it (see "Stall-timeout progress reports"). Default 10
tool_streak_advisory = true   # Whether to inject a reminder at 3/5/8 consecutive calls to the same tool with the same arguments (see "Tool streak advisory"). Default true
criteria_ledger = "report"    # Per-criterion acceptance ledger: off / report / enforce (see "Acceptance ledger"). Default report
steering_enabled = false      # Directions for the next round from the task page (see "Directions and stopping on the task page"). Default false

[dispatch_guard]        # Feedback-path circuit breaker (guards against self-reinforcing loops)
window_secs = 60        # Sliding window length (seconds). Default 60
max_in_window = 20      # Dispatches allowed within one window before tripping. Default 20
cooldown_secs = 60      # Cooldown seconds after tripping, during which dispatch is refused. Default 60
max_hop_depth = 5       # Cap on cross-process re-spawn depth along a delegation chain. Default 5
```

All blocks are optional; anything missing or partially set falls back to the built-in defaults above. An unrecognized `policy` value always falls back to `fixed_hierarchy`, logging a warning.

---

## Parallel subtasks (dependency DAG)

With `[goal_loop] planner_enabled = true`, creating a goal first lets the AI employee "try" splitting it into a set of subtasks annotated with dependencies (for example: query two data sources independently, then merge). The resulting subtasks each land on the Task Board, and any subtask whose `depends_on` list is fully satisfied runs **in parallel**, each checked for acceptance independently. Parallelism is still bounded by `max_concurrent` and the `dispatch_guard` circuit breaker — it never bypasses them.

- **Not mandatory**: when the model decides a split isn't needed (or its reply can't be parsed), it falls back to a single task, behaving exactly as if the setting were off.
- **Cycle protection**: if the resulting plan has a dependency cycle (or an out-of-range index), the whole plan is discarded, falls back to a single task, and a warning is logged. A broken DAG is never allowed to land.
- **A stuck upstream never orphans its downstream**: if a subtask's upstream dependency lands in `failed` / `cancelled` / `needs_human` (or the dependency doesn't exist), the downstream **inherits the escalation** and also moves to needs-human, so you see the whole stuck branch at once. If the upstream is simply still running, the downstream is frozen for that round and reconsidered the next one.

The expected payoff is greatest for "multi-source lookup" style goals; an independent re-test measured roughly a 1.25x speedup (not the 3.7x a paper self-reported) — treat eval measurements as the source of truth before generalizing.

---

## Dispatch policy (DispatchPolicy)

`[dispatch] policy` decides which AI employee picks up a goal task. The default `fixed_hierarchy` behaves exactly as before (dispatches to whoever the task was already assigned to).

| Policy | Behavior |
|------|------|
| `fixed_hierarchy` | **Default**. Dispatches to the task's existing `assigned_to`, unchanged. Zero LLM cost, fully deterministic. |
| `round_robin` | Rotates through the roster by "task category" (the first tag if any, otherwise priority). State lives in memory only, resetting on restart. |
| `llm_select` | An LLM picks the best-fitting AI employee from the roster via a tool call. **Fails closed**: if the output isn't on the roster, or parsing/the LLM call fails, it always falls back to the `fixed_hierarchy` result, never dispatching to a made-up AI employee. No model name is hardcoded; it uses whatever utility runtime is configured. |
| `role_team` | Selection still yields the **AI employee**, exactly as `fixed_hierarchy` does. The roles live *inside* that employee — see "Team rounds" below. The setting exists so "this deployment composes teams" is visible in logs and telemetry; it does not change who a task is assigned to. |

Roster = the AI employee directories under `<home>/agents/`. When the roster is empty, both `round_robin` and `llm_select` fall back to the original assignment (never orphaning a task). A reassignment writes back to the task's `assigned_to`, so heartbeat pulls and the activity log stay consistent.

---

## Team rounds (Team-as-Agent)

An AI employee can work a goal round as a small team inside itself: **規劃 → 執行 → 審核**, each role on its own vendor's model. Since v1.66 `[team] enabled` defaults to **`true`** — but what actually forms a team is naming a second vendor under `[team.roles]`, not the flag. Write no roles and the executor and verifier both cascade onto the employee's own model, share a model family, and the decorrelation rule refuses the spec: the task runs Solo, quietly, with no audit row. See [56-team-as-agent.md](../features/56-team-as-agent.md) for the full picture; this section is what it means for the goal loop specifically.

> The whole path — planner, executors, verifier and the `team_handoff` tool that carries packets between them — has landed and has been driven through a live round the judge accepted; that is one integration result, not a measured win over Solo. Configure `[team.roles]` only on a deployment you are watching. `enabled = false` (fleet-wide or per employee) and `gate = "always_solo"` are both one-line reverts.

### What you see

Nothing new in the conversation. The employee answers with one voice, progress arrives on the same board, and a task that needs you still carries one of the six pause classes. Under the hood the round runs as three stages instead of one wake-up message, and only the executor's final product reaches the acceptance judge — which is the **same** two-stage evaluator and three-aspect panel as always. A team changes who does the work, not who decides it is done.

Two new reasons a team task can land in your lap:

| Pause | What happened |
|---|---|
| 需要決策 (`blocked_needs_decision`) | The planning stage handed back no sub-tasks. Rather than guessing a breakdown from its prose, the loop asks you whether the goal is actually splittable — or whether it should just run as a single employee. |
| 預算用盡 (`budget_exhausted`) | The task ran out of role-member spawns **after spending some**. It hands over the best round it managed, same as any other budget escalation. A task whose budget could never have paid for even one degraded round runs Solo instead — being parked over work that never started would be a worse answer than doing it the ordinary way. |

### The gate

Before each round, a zero-LLM rule set (`crates/duduclaw-core/src/team_gate.rs`) decides Solo or Team. It leans Solo on purpose.

1. **Hard Solo**: the employee has `[container] sandbox_enabled = true` (checked before every mode, including `always_team`, so a sandboxed employee never forms a team), a live channel turn, a plan-first goal still awaiting your approval, an irreversible action in the plan, or fewer than three rounds of budget left.
2. **Count four signals**: *bulk* (at least four independent work items and no dependency hubs), *context overflow* (the estimated input is larger than the model's context window), *capability gap* (the role-matrix gap is at or above the matrix's declared MDE) and *long horizon* (at least three acceptance criteria and the task produces artifacts). A signal it cannot measure never fires.
3. **Decide**: three or more signals form the team; zero or one runs Solo; exactly two is the grey band.

In the grey band the loop runs the planning stage once (a call the task needed anyway) and applies the same rule again, this time with the bulk signal measured from the sub-task packets the planner wrote. Before a plan exists the bulk signal has nothing to measure, so the grey band always means two of the other three signals fired, and the second pass forms the team only when the plan holds four or more sub-tasks with no dependency hubs between them. Otherwise the round falls back to the ordinary single-employee round. A team spec with no planner role cannot take the second pass and runs Solo.

Every verdict is written to the audit log as `team_gate_decision` with the signals that fired, so a deployment's Solo/Team split is measurable rather than anecdotal.

### The spec is frozen per task

The team a task runs with is decided once, at creation, and stored on the task. Later changes to `[team]` affect the *next* task. A spec that fails validation — most often a verifier sharing the executor's model family — forms no team at all: the task runs Solo and there is no partial team.

Whether that refusal is loud depends on who asked for the team. An operator who wrote `enabled = true`, or who configured roles that then fail validation, gets the refusal audited as `team_refused` with the role and the reason. A deployment that never touched `[team]` at all gets a `debug!` line and nothing in the audit log — that refusal is the default state of every install since the v1.66 flip, and stamping a row on every goal task everywhere is how a real refusal stops being findable.

A role whose `model` is unset can also be filled from a measured capability matrix (`role_model_matrix.toml` in `<DUDUCLAW_HOME>`, written by `duduclaw eval --matrix`) before it falls through to the employee's `[model] preferred`. Only `resolved` cells on the role's own runtime count, ties select nothing, and an explicitly configured model always wins.

### Budget and degrade chain

```toml
# config.toml
[dispatch.team_budget]
max_spawns_per_task = 12                 # 4 roles x 3 rounds; clamped to a floor of 3
max_turns_per_role  = 3
degrade_order = ["utility", "verifier_second_pass", "executor_replica"]
```

One round is charged for `planner? + executors + verifier + repair?` — the verifier is a utility call rather than a scaffold, but it writes its own ledger row and the budget counts exactly the rows that carry a member id, so it costs a slot like every other stage. The floor of 3 is planner + executor + verifier.

As the spawn budget shrinks, capabilities are surrendered in that order — 合成 first, then the verifier's one repair pass, then fan-out collapses to a single executor — before the task escalates as `budget_exhausted`. An unrecognised entry is dropped with a warning; a completely unusable list keeps the default chain.

Before its first team round the loop also checks, once, that `[dispatch] ephemeral_max_active` can hold the concurrent role members this configuration can need (`max_concurrent × iteration_cap × roles` — 45 against a default of 32) and warns with the number to raise. Nothing is enforced: past the ceiling, role spawns queue and can expire, which would otherwise surface much later as a round mysteriously missing its verifier.

Role-member spawns run on their own circuit-breaker bucket and budget (`[dispatch_guard] role_team_max_in_window`, default 60), so a composing team can never trip the breaker that guards an employee's own sub-agent spawns.

### Evidence on a team round: the employee **and** its members

Everything downstream of a round — the team's own verifier, the grounding precheck, and the MAV panel's `<tool_activity>` digest — reads the tool-call audit trail for the task's claim→review window. On a Solo round that trail belongs to one agent id. On a team round it does not: the work is done by ephemeral role members under **their own** ids, and the employee's own window may hold nothing but the bookkeeping call that opened the round.

Read that way, a team round looks exactly like a task that claimed to do work and did nothing — which is what happened in live round 3, where the verifier and the settle evaluator both rejected honest work with "no tool activity supports file creation". So the evidence set for a team round is the **employee ∪ that round's role members**, taken from `role_turns.jsonl`, on both paths:

- the team verifier's `<tool_activity>` block, and
- the settle path's grounding precheck plus the judge digest.

Duplicate ids are collapsed, and a task with no role members adds no ids — a Solo task sees byte-identical evidence to before. The time window is unchanged, so members from other rounds contribute nothing even when the round-number lookup falls back to the whole task (the loop's in-flight iteration counter and the settle path's revision counter are separate and can diverge across a gateway restart).

Role members also work in the **employee's** workspace rather than in their own throwaway scaffold, so files a member writes are still there when the verifier asks about them. Roles that write files are limited to claude and codex today; see [56-team-as-agent.md](../features/56-team-as-agent.md#a-member-runs-in-the-employees-workspace-not-in-its-own-scaffold).

Union-ing the ids was necessary but not sufficient. Two further evidence sources landed after live round 8, both shared by the team verifier and the settle path so neither ever judges on a different account of the round:

- **Native tool work is persisted.** A member that works through native tools (a codex `shell`, a Claude `Write`) makes no MCP call, so before this its work left nothing in the audit trail at all — round 8 rejected three files that demonstrably existed. Each native tool event is now written as a `tool_calls.jsonl` row under the member's id, carrying the masked call input and result text plus `source = "native"` and the runtime/model that produced it. Self-echo tools keep their output suppressed, exactly as the MCP writer does, so a role can never ground a claim on its own echoed packet.
- **`<artifact_receipts>`** — every `artifacts[].path` a packet declares is stat'd and hashed against the employee's workspace, and the result (`<path> <bytes>B sha256=<hex> exists|missing|mismatch`) becomes both a prompt block and an `artifact_receipt` audit row. A declared-but-absent hash is filled in from the bytes; a declared hash that **disagrees** is kept as declared and recorded as `mismatch` with a `team_packet_artifact_mismatch` event, so a swap stays visible rather than being quietly corrected. Only `exists` counts as confirmation.

A task that declares no artefacts adds no block, and a Solo round with no native-tool members sees exactly what it saw before.

### Audit and per-role records

| Event | Meaning |
|---|---|
| `team_gate_decision` | Solo / Team / grey band, with the signals that fired |
| `team_refused` | An enabled spec failed validation; the task runs Solo |
| `team_round_started` | A three-stage round began, with the roles and any degrade steps |
| `team_member_spawned` | One role member was created (role, runtime, model) |
| `team_stage_failed` | A stage did not produce what the next one needs, or could not run at all — carries `role`, `runtime`, `model` and the error |
| `team_handoff` | A role filed a packet (leg, round, size, audience, file) |
| `team_packet_skipped` | A packet file was unreadable, mislabelled or invalid and was ignored |
| `team_packet_artifact_refused` | A packet declared an artifact path resolving outside the employee's workspace |
| `team_packet_fidelity_corrected` | A packet's self-declared evidence grade disagreed with what was observed, and was overwritten |

### Where the packets live

Each stage hands over by calling `team_handoff`, which derives the file path rather than taking one:

```
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.json
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.01.json   … up to .99
```

A planner that breaks a goal into four sub-tasks writes four packets on the same leg, so a **new** packet takes the next numbered slot while re-filing the **same** `packet_id` overwrites its own file (a retry after a timeout does not duplicate the sub-task). The next stage reads those slots back in numeric order — canonical file first, then `.01`, `.02`, … A packet whose own `from_role` / `to_role` / task / round disagree with the file it sits in, or that fails validation, is skipped and audited as `team_packet_skipped`; one bad file does not cost the stage the rest of its packets.

Each stage also appends a row to `<home>/role_turns.jsonl` — role, runtime, model, effort, the packet it produced, the evidence grade of its observations, how it ended. Same permissions, locking and rotation as `tool_calls.jsonl`.

---

## Ephemeral spawn admission queueing

Goal decomposition, delegation, and similar paths sometimes need to spin up a short-lived sub-agent (an ephemeral spawn). When that hits the concurrency cap (`ephemeral_max_active`, 32 by default), `config.toml [dispatch] admission` decides how to handle the request that pushed it over the limit:

| Value | Behavior |
|------|------|
| `queue` | **Default**. Bounded FIFO queueing: a request never simply vanishes, it runs once a slot frees up, in order. Each queued ticket carries a TTL (`queue_item_ttl_secs`, 600 seconds by default); past that it's dropped and an audit entry is logged. The queue itself has a depth cap (`queue_max_depth`, 64 by default), and a full queue rejects outright (so an unbounded queue can't itself become a new runaway risk). When the turn/session that made the request ends, its queued tickets are voided along with it, so a process that has already finished can't suddenly spawn a late sub-agent. |
| `fail` | The old behavior: refuse outright past the cap, no queueing. |

`ephemeral_max_active` itself follows "adjustable, but never zero": setting it to `0` clamps it to `1` and logs a warning. The concurrency cap can never be turned fully off.

```toml
# config.toml
[dispatch]
admission = "queue"           # "queue" (default) or "fail"
queue_max_depth = 64          # queue depth cap, rejects past this
queue_item_ttl_secs = 600     # seconds a queued ticket survives before being dropped and audited
ephemeral_max_active = 32     # concurrency cap, 0 gets clamped to 1
```

---

## Structured outcome verification (outcome schema, WP2.4)

`/goal … || outcome:<spec>` lets you add a **machine-checkable** deliverable contract on top of free-text acceptance criteria. When the AI employee reports it's done and the task enters `review`, this contract runs a **deterministic, zero-LLM-cost** check **before the acceptance judge**:

- **Check fails** → the task goes straight back to `revising` with feedback naming the specific gap (which field is missing, which file is missing), **without calling the judge at all**. This is a defense against judge false positives: an output with an obvious structural defect never gets waved through by an over-lenient judge, and no judge call is wasted on it.
- **Check passes** → only then does it reach the judge, and the judge's prompt gets a note that "structured outcome verification already passed its deterministic check," so the judge can focus on quality.

Three spec types:

| Spec | Meaning |
|------|------|
| `outcome:text` | Default. No structured contract; behaves exactly as if no outcome were attached (not persisted, no check runs before the judge). |
| `outcome:json:<JSON Schema>` | A subset of JSON Schema (`object` / `array` / `string` / `number` / `integer` / `boolean`, supporting `properties` / `required` / `items`). Checks the ` ```json ` block in the AI employee's final reply (falling back to parsing the whole reply if no fenced block is found). Missing fields or type mismatches are all listed as specific defects. |
| `outcome:files:<glob,glob>` | Asserts that a deliverable file matching each glob (`*`/`?` supported) exists under the AI employee's working directory. Example: `outcome:files:report.docx, out/*.pdf`. |

**Example**

```
/goal Export this quarter's revenue numbers || outcome:json:{"type":"object","required":["revenue","month"],"properties":{"revenue":{"type":"number"},"month":{"type":"string"}}}
/goal Produce and save the quarterly report || Must include a revenue chart || outcome:files:report.docx,charts/*.png
```

**Boundaries and fail-closed behavior:**

- **Path traversal is refused**: a `files:` glob that's an absolute path, a home directory (`~`), or contains a parent-directory `..` is refused right when `/goal` is created (fail-closed, the task is never created), and checked again at verification time. The working-directory baseline is `<home>/agents/<agent>/`.
- **Malformed specs are refused**: `json:` that isn't a valid JSON object, an empty `files:` list, or an unrecognized type prefix all make `/goal` return an error immediately, with no task created (never silently degrading to plain text).
- **Persistence**: the spec is stored as a single `outcome:<base64url>` tag on the task's existing `tags` field (base64url contains no commas, so it can't collide with tag separation), with **no database schema change**. `text` isn't persisted at all.
- **Relationship to the planner**: setting an outcome spec skips `planner_enabled` subtask decomposition. A structured contract targets a single final deliverable, not each subtask that would otherwise be split out.
- **The judge is still the backstop**: if the tag is corrupted and can't be decoded, the deterministic check is skipped and it goes straight to the judge (which still gates it as usual). A task is never left stuck because of an observability gap.

## Grounding precheck (v1.53)

When the AI employee reports completion and a task enters review, before the acceptance judge is called it runs a zero-LLM grounding precheck: it compares the final reply against the actual tool-execution record for this task (tool results in the audit log). If the reply claims to have found or done something with no overlapping content in any real tool result, it goes straight back for revision without spending a judge call. Two anti-spoofing details:

- **Self-echo doesn't count as evidence**: tools like `tasks_complete`, which echo back the AI employee's own summary verbatim, are on an exclusion list — an AI employee can't use its own words to prove itself.
- **What it fed in doesn't count either**: text the AI employee itself put into a tool call's arguments is subtracted from the evidence; only what a tool actually returned counts.

`config.toml [dispatch] grounding_precheck_enabled = false` turns this off. When there's no tool record to compare against at all (a purely conversational task, for example), the precheck is skipped (Skip) rather than penalizing the task.

---

## Task forward model (v1.53; default on since v1.54)

When enabled, before each dispatch the goal loop "predicts" how the run will likely go, based on statistics from past tasks of the same kind (whether it'll fail, roughly which tool categories it'll use). After execution, it compares the prediction against the actual observation and records it as a transition, so the system builds up a task-level world model of "what tends to happen when doing this kind of thing." It runs for every runtime, but how much it sees depends on whether the runtime feeds the native tool-event collector. Claude (the dispatch stream-json path), Codex, Gemini CLI (deprecated in v1.67.0, removed in v1.72.0), Antigravity and OpenAI-compatible agents record their native tool events, so their observations can reach `Full`. Grok and the seven generic print-mode CLIs (Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode) attach no collector, so their observations are `McpOnly` (built from `tool_calls.jsonl` alone), or `None` when that file holds nothing for the round. The pipeline:

- **Layered prediction fallback**: uses matching statistics when available, falls back to overall marginal statistics, then a prior default. Cold start never spends an LLM call.
- **Honest fidelity grading**: every observation is tagged with its evidence fidelity (native tool events / audit-log-only / no evidence), so "we didn't see it" is never conflated with "it didn't happen."
- **The `<state>` block**: while a task is running, the prompt gets an injected structured current-state block, which the AI employee can revise via a state-update tag in its reply; paired with a (state, action) visit graph, repeating the same action from the same state more than once escalates early to a human (oscillation detection).
- **Foresight warnings**: when the prediction says this dispatch is likely to fail, the dispatch prompt gets a warning note attached, but nothing is blocked outright (the prediction assists, it isn't a gate).
- **Task rule induction**: when the same kind of transition repeats, it's distilled into a deterministic-template task rule injected into future prompts (capped at 2), sharing the same helpful/harmful lifecycle as other learned rules — an underperforming rule retires on its own.

```toml
# config.toml
[task_forward_model]
enabled = true    # on by default since v1.54; set false to switch the whole predict-act-verify pipeline off
```

---

## Two-stage acceptance judging

After the AI employee reports completion and a task enters `review`, it doesn't automatically burn a full MAV judge-panel call every time. A much cheaper **first-stage evaluation** runs first: a single tool-free LLM call that outputs one of three JSON verdicts:

| Verdict | Meaning | What happens next |
|------|------|----------|
| `continue` | Not done this round, but on the right track | Skips MAV — the evaluator's next-step suggestion becomes the feedback for an immediate redispatch (counted against the iteration cap) |
| `blocked` | Stuck on an external obstacle (missing permission, missing data, waiting on a third party…) | Goes straight to `needs_human`, with no pretense of finishing a judge round |
| `candidate_complete` | Looks like a completion candidate | Only now does it reach the full three-aspect MAV judge panel for a careful check |

**Any failure of the evaluator (timeout, parse failure, the call itself erroring out) always degrades to running the MAV judge directly**: a first-stage failure never auto-passes or auto-fails a task, and safety is identical to not having this layer at all. Set `config.toml [dispatch] two_stage_judge` (`true` by default); setting it to `false` reverts to the old single-stage behavior, running the full panel every round.

## Swapping the acceptance judge

"Is this work actually done" is the platform's single point of release authority. By default, the built-in three-aspect MAV judge panel decides it, but you can swap in a different implementation via `config.toml [dispatch] judge`:

| Value | Who decides | When to use |
|---|---|---|
| `mav` (default) | First-stage evaluator → three-aspect MAV judge panel | The general case |
| `external` | Your own program (`judge_command`) | Wiring in your own CI, a rules engine, or a second model as judge |
| `evaluator_only` | Only the first-stage evaluator runs; `candidate_complete` passes directly | **Removed in v1.69.0.** A leftover value is acted on as `mav` (stricter, more judge cost). Set `mav`: `two_stage_judge` already runs the cheap evaluator first and only pays for the panel on a completion candidate |
| `human_only` | No machine verdict; every `review` task escalates to `needs_human` | **Removed in v1.69.0.** A leftover value does not fall back to machine acceptance: every item sent for acceptance stops at `needs_human` until you mark it done, or fix the setting and retry (a retry clears the earlier result). Use `mav` plus per-agent `[capabilities] autonomy_level` / `approval_required_tools` |

All four values still parse, so a deployment already on a deprecated mode keeps behaving exactly as configured — it logs one warning per process, and a write through the dashboard records a `judge_mode_deprecated` audit event. The dashboard offers only `mav` and `external`, but shows a saved deprecated value (labelled 已棄用) rather than silently switching it. See [deprecations.md](deprecations.md).

A bad value doesn't quietly take effect: the gateway warns and falls back to `mav` (the strictest option). This setting is re-read on every verdict, and just like `two_stage_judge`, changes take effect immediately without a restart.

### Running the judge on a different model

A judge drawn from the same model family as the worker tends to forgive exactly the mistakes that family makes — it shares the worker's blind spots, so a second opinion from the same vendor is worth less than it looks (arXiv:2607.13918). Two optional keys move the acceptance judge, and the cheap first-stage evaluator with it, onto a different runtime and model:

```toml
[dispatch]
judge_provider = "antigravity"        # A runtime id: claude, codex, antigravity, grok, openai_compat, … (gemini is deprecated)
judge_model = "gemini-3-pro-preview"   # Model id within that runtime
```

Both are optional and independent. Unset, the judge keeps using the ordinary utility model (`[runtime] utility_provider` / `utility_model`) — which is exactly the behavior every existing deployment already has, so leaving these out changes nothing.

**Scope: global only.** There is no per-agent version of these keys. Every AI employee's work is judged by the same configured judge.

What happens when the setting can't be honoured: in all four cases below (the first three are caught before anything is spawned, the fourth after the judge failed while running), the judge falls back to the default utility model and keeps working, because a routing preference must never stall a verdict:

| Situation | Behavior |
|---|---|
| `judge_model` names another vendor's model but `judge_provider` is unset | **Refused, not guessed.** Setting only `judge_model = "gemini-…"` while the judge runtime resolves to Claude is a configuration error, not an instruction to infer the runtime. The override is dropped before anything is spawned (so it costs nothing) and the drop is written to `security_audit.jsonl` as `judge_seam_degraded`. Name the runtime too and it works. |
| `judge_provider` is not a runtime id this build knows | The **whole** override is dropped with a warning naming the valid ids. A typo never silently sends the judge to a different backend. |
| `judge_provider`'s CLI or backend isn't installed on this host | Detected before spawning; the override is dropped and audited the same way. |
| The judge runtime was available, was used, and then **failed while running** (bad arguments, auth failure, crash) | The judge is re-run once on the default utility model, and the failure is audited as `judge_seam_degraded` with `reason: "hinted_runtime_failed"` plus the provider, model, underlying error, and the fallback model that ended up judging. Crucially, the failed judge is **not** rescued by ordinary cross-runtime failover: a call routed to another family runs with cross-family failover disabled, so it can never be silently answered by the worker's own family. Same-family failover (one Claude tier to another) still works normally. |

Two audit events make the setting's actual effect visible instead of assumed, both written to `security_audit.jsonl`:

- `model_routed` — the override took effect and moved the judge off the worker's own model. Carries `{slot: "judge", from: <worker model>, to: <judge model>, provider, reason: "dispatch.judge_model"}`.
- `judge_same_family` — worker and judge turned out to share a model family, so you are not actually getting a decorrelated second opinion. This is a **warning, never a rejection**: the verdict still happens normally. Logged once per (AI employee, judge model) pair per gateway run, so a long goal loop produces one line, not one per round.

Like `judge` itself, both keys are re-read on every verdict — no gateway restart.

#### Structured output for a judge that needs it

Claude follows "reply with ONLY a JSON object". Codex does not reliably, and live round 8 of the team work is what proved it costs a verdict: with `judge_provider = "codex"` both the first-stage evaluator and the MAV panel answered in prose, and both parsers refused it fail-closed ("evaluator reply has no string `decision` field"). The parsers were right — auto-accepting garbage is the one thing a judge parser must never do — but a judge that structurally cannot be parsed is a seam that does not work.

So each adjudication stage now publishes the JSON schema **its own parser** requires, and a runtime that can enforce a reply shape does:

| Stage | Schema |
|---|---|
| First-stage evaluator | `{decision: "continue"｜"candidate_complete"｜"blocked", evidence, next_step}` (`blocker_key` declarable, not required — the parser refuses it on a non-blocked decision) |
| MAV panel | one `{pass, reason}` object per active aspect, all required; the aspect set follows the goal's difficulty exactly as the prompt and parser do |

Today `codex` is the only runtime wired to it: the schema is written to a temp file and passed as `codex exec --output-schema <FILE>`. Every other runtime logs the request at `debug` and ignores it. **A schema is a preference, never a precondition** — nothing fails because a backend cannot constrain its output, and if the schema file cannot be staged the call runs without the flag rather than not running at all. There is no configuration key: the schemas are derived from the parsers' contracts, so a parser change and a schema change move together.

#### Judge effort

The judge hint can also carry a reasoning **effort**, alongside the provider and model. Internally `UtilityModelHint` has three independent halves — `provider`, `model`, `effort` — so a caller may move the judge to another vendor, change how hard it thinks, or both.

Effort is deliberately **not** part of the hint's "is this empty" check: emptiness there means "resolves to the same `(provider, model)` spec", which is what the model-family validation is about. Effort changes how hard the model thinks, not which model answers.

There is **no `[dispatch] judge_effort` key yet** — the config-file half of this is a deliberate follow-up. Today an unset judge effort falls back to the answering agent's own `agent.toml [model] effort` (see [Effort](../features/13-multi-runtime.md#effort) for the per-runtime flag mapping and clamp table), which for most setups is the behavior you want: the judge thinks as hard as the agent it is judging.

### External judge (`external`)

```toml
[dispatch]
judge = "external"
judge_command = ["/usr/local/bin/my-judge", "--strict"]
judge_timeout_secs = 120   # default 120
```

The platform runs this command, feeding a JSON document into stdin:

```json
{
  "schema": "duduclaw.judge.v1",
  "task": "task description (already includes blocks like <tool_activity> / <risk_boundary>)",
  "acceptance_criteria": "the frozen acceptance-criteria baseline",
  "result": "the AI employee's submission this round",
  "tool_activity": "audit summary of tool activity"
}
```

The command prints a JSON object to stdout as its verdict:

```json
{"pass": true, "feedback": "Every acceptance item checked out"}
```

`pass` accepts `true`/`false`, as well as the strings `"pass"`/`"fail"`; `feedback` is optional.

Three things worth knowing up front:

1. **Any failure of the external judge falls back to the MAV judge panel** — including a timeout, a nonzero exit code, stdout that isn't valid JSON, or `judge_command` not being configured. Degradation always moves toward being stricter, never toward waving something through, and every degradation is written to `security_audit.jsonl` (`judge_seam_degraded`).
2. **Its output is treated as untrusted data.** `feedback` flows into the next dispatch round's prompt, so it's scanned for injection and truncated first; if the scan blocks it, the whole verdict is discarded and the MAV judge panel decides instead. The feedback text is prefixed with its source, so the task timeline shows which line came from the external judge.
3. **`judge_command` can only be changed by editing a file, never from the dashboard.** It names an executable, so the `system.update_config` RPC only accepts `judge` (the four enum values), never `judge_command` or `judge_timeout_secs`; the AI employee itself has no write access to `~/.duduclaw/config.toml` either.

To use `duduclaw eval` as the judge, just point `judge_command` at a script that wraps `duduclaw eval`. No separate mode is needed.

**The subprocess inherits the gateway's full environment.** `judge_command` runs directly through the platform's process-spawn path (`tokio::process::Command`), with no `env_clear()` and no allowlist filtering. Your judge program can see the gateway process's entire environment at the time it's called, including the secrets the gateway uses to call LLM providers and channel APIs. This doesn't mean data is actively handed to the judge (its only input is the stdin JSON shown above); the judge program simply has the *ability* to read those environment variables (for example, a malicious or buggy program reading `std::env::vars()`). This isn't a vulnerability — it's a design tradeoff of this seam today: **only point it at a program you trust and whose source you know**, never a third-party or unreviewed executable. If you need tighter isolation (a judge process that genuinely can't see the gateway's secrets), wrap `judge_command` in a script that clears its own environment first, then re-injects only the handful of variables the judge actually needs.

### Strict reply contract (`strict_reply_parsing`)

All three verdict parsers (the MAV panel, the first-stage evaluator and the external judge) find their JSON by cutting from the first `{` to the last `}`. That quietly accepts a reply with prose around the object, and repairs some broken ones. The strict contract accepts a reply only when the whole reply, after trimming and at most one outer ```` ```json ```` fence, is exactly one JSON value of the expected shape with no extra fields: the panel's `{pass, reason}` object for each active aspect, the evaluator's `decision` / `evidence` / `next_step` / optional `blocker_key`, or the external judge's `pass` plus optional `feedback`.

```toml
[dispatch]
strict_reply_parsing = "shadow"   # off | shadow (default) | enforce
```

| Mode | Who decides | Strict parse |
|---|---|---|
| `off` | Today's lenient parse, byte for byte | Never runs |
| `shadow` (default) | Today's lenient parse | Runs on the same reply and is only compared |
| `enforce` | The strict parse | A violation takes the parser's existing failure path: the panel returns its fail-closed FAIL; the evaluator and the external judge count as failed and the MAV panel decides |

An unknown value falls back to `shadow`. The key is read at every verdict, so a change takes effect without a restart.

Each compared verdict increments the Prometheus counter `judge_parse_shadow_total{parser, outcome}` (`parser` is `panel`, `pre_evaluator` or `external`; `outcome` is `agree`, `strict_rejects`, `lenient_rejects`, `both_reject` or `disagree`). Every outcome other than `agree` also writes one `judge_parse_shadow_mismatch` event to `security_audit.jsonl` with the parser, the outcome, the strict violation and the first 200 characters of the reply after secret masking.

The default moves from `shadow` to `enforce` only by an operator decision based on these numbers: at least 50 judge rounds or 7 days of observation, with `strict_rejects` under 2% of compared verdicts. Nothing flips it automatically. Before switching a deployment yourself, check `strict_rejects` for your own judge runtime; a model that habitually adds a sentence after the JSON will have its verdicts turned into FAILs (panel) or MAV degrades (evaluator, external judge) under `enforce`.

## Acceptance judge discipline

Both the MAV judge and the first-stage evaluator have several rules baked into their prompts, targeting a failure mode caught in live testing: a judge inventing its own false rejections and permanently blocking correct work.

- **Anti-ratchet**: when acceptance criteria haven't changed, the judge can't pick a fresh nitpick every single round. This is the classic pattern that makes a goal impossible to ever finish.
- **Audit only, never invent evidence**: the judge can only compare the AI employee's submitted evidence against the tool-audit summary; it can't imagine or fabricate evidence of its own, and it can't use "I think there's a better way to do this" as a standard.
- **No scope creep in rejections**: something the acceptance criteria never mentioned can't be used as grounds for rejection. This is the most common false rejection, and the number one reason correct, in-scope work gets stuck.
- **The agent's own claim of "done" isn't evidence**: self-reports like "already done" or "already handled" don't by themselves justify a pass; the judge must check the acceptance criteria against the actual output item by item.

These rules have no config switch — they apply immediately to every goal task.

The panel prompt also carries one line the system provides, not the worker: the worker's working directory, `<home>/agents/<agent_id>` as an absolute path (`<worker_working_directory>`). Relative paths in the result refer to it. It is there in every mode, including `criteria_ledger = "off"`, and is left out only when the dispatch engine has no home directory. Before this line existed, a correct `hello.txt` was rejected twice because the judge read the agent directory as "a subdirectory, not the working directory".

## Dynamic judge depth (MaAS)

The number of aspects the acceptance judge checks scales with how hard the goal is, saving unnecessary judge LLM cost:

- **Simple goals** (short, single-step, no keywords like multi-step / research / comparison / deployment / migration): the judge checks only two aspects, **correctness + safety**, and the dispatch cap uses `iteration_cap_simple` (default 3).
- **Hard goals**: the full three-aspect MAV panel — **correctness + completeness + safety** — with the dispatch cap using `iteration_cap` (default 5).

**Safety is checked at every depth** (fail-closed by design): dropping depth only trims completeness granularity, and the safety check is never cut. Difficulty is judged by a local, zero-LLM heuristic (length + CJK-aware token estimate + keywords); the same judgment drives both judge depth and the dispatch cap, so the two always agree.

### `agent.toml` (per AI employee)

```toml
[capabilities]
autonomy_level = "approver"
irreversible_tools = ["send_email"]          # irreversible tools that always need human approval
maybe_irreversible_tools = ["Bash", "http_post"]  # the judge decides whether these need escalation
```

---

## Stall detection: gap fingerprint matching

Deciding "stuck in the same place two rounds in a row" no longer just checks whether the rejection feedback is word-for-word identical. The judge doesn't necessarily phrase things the same way each time — "missing error handling at `goal_loop.rs:120`" and "you forgot to add validation at goal_loop.rs line 120" describe the same gap, but a string comparison would treat them as two different things, letting real stuck-signals get lost in phrasing differences.

Now it extracts `path:line` references and backtick-quoted keywords (function names, variable names, error codes) from the rejection feedback, normalizes them (scratch/temp paths collapse to the same placeholder, case-insensitive, deduplicated and sorted) into a fingerprint. The same gap, said differently, produces the same fingerprint. When no reference or keyword can be extracted at all (a purely descriptive piece of feedback, for example), it falls back to the original word-for-word comparison, staying behavior-compatible. Two rounds in a row with the same fingerprint trigger the `needs_human` escalation below; the threshold itself hasn't changed.

## Bail detection

An AI employee in the autonomous loop sometimes ends a round with something that *sounds* like a wrap-up but was never actually verified by the judge: "I'll stop here for now," "please check back on the result later," "submitted for review," "VERDICT: PASS" (self-signed, not the judge's), and the like. None of these phrases by themselves mean the work is wrong; they're just a process-level signal worth recording and worth extra attention next round.

Nine zh+en regex patterns check only the **last non-empty block of text** in the agent's reply for this round. A match:

- Logs an Activity Feed event (`goal_loop.premature_stop_suspected`)
- Increments the Prometheus counter `goal_loop_bail_pattern_total{pattern="<matched pattern name>"}`
- Carries a hint into the next round's `<state>` block, the first-stage evaluator's input, and the MAV judge's input: a neutral note ("possible premature stop, please confirm whether the task is actually complete") that doesn't pre-judge for the evaluator or the judge

This detection layer itself **never** rejects, blocks, or escalates a task. It's purely a signal and a reminder; whether it actually passes still comes down to the evaluator's/judge's read of the evidence.

## Restart behavior (resume_on_restart)

After a gateway restart or crash recovery, in-flight goal tasks default to escalating to `needs_human` (`resume_on_restart = "pause"`, **the default**): on every boot, the gateway moves every non-terminal `goal_mode` task (`todo`/`pending`/`revising`/`in_progress`/`review`/`blocked`) to `needs_human` (reason: `gateway_restart`), through the existing channel notification, and waits for you to press "Retry" before it continues. An unplanned process restart or deployment never quietly keeps running a goal that no one has re-confirmed as safe.

To keep the looser, original behavior, set `[goal_loop] resume_on_restart` to `"auto"`: in-flight goal tasks pick up right where they left off, as if the process had never been interrupted (this was the only behavior before this setting existed).

Either way, this check only runs once, at gateway boot; a config hot-reload (`system.update_config`) never triggers it. The change takes effect only on the next real gateway restart.

**Dashboard toggle**: the "In-flight goal tasks on gateway restart (gateway 重啟後的進行中目標任務)" dropdown under Settings → Automation (設定 → 自動化) switches this directly, no need to hand-edit `config.toml`; `system.update_config` accepts only the two values `"auto"`/`"pause"`, rejecting anything else.

---

## needs_human button semantics

When a task escalates to "needs a human" (hitting the dispatch cap / wall-clock timeout / two rejected rounds in a row with the same gap fingerprint / the acceptance judge still failing it once the retry budget runs out / an upstream dependency subtask stuck and escalating its inheritance / gateway restart while `resume_on_restart = "pause"`), four buttons are pushed to the AI employee's control channel.

### Pause reason categories (pause_reason)

"Needs a human" is no longer one single bucket. Alongside the existing free-text `judge_feedback` (the judge's or evaluator's full feedback, which can run several sentences), every escalation to a human is now also stamped with a **closed six-way category**. This is meant to give you an at-a-glance read on "what kind of stuck is this," while `judge_feedback` remains the sentence-by-sentence detail:

| Category token | UI text |
|------|------|
| `no_progress` | Stuck, no progress (卡住沒進展) |
| `budget_exhausted` | Out of rounds or time (次數或時限用盡) |
| `blocked_needs_decision` | Waiting on your decision (等你決策) |
| `infra` | System problem (系統問題) |
| `restart` | Paused after a restart (系統重啟後暫停) |
| `unknown` | Needs a human to check (需要人工確認) |

The category is stamped statically **at the point of the trigger** (each escalation path tags its own category), never reverse-engineered from `judge_feedback`'s LLM prose. A model's own wording isn't reliable and shouldn't be used to drive routing. Anything uncategorized, an unrecognized legacy value, or a task from before this field existed all read as `unknown` ("Needs a human to check"): a stuck task whose type can't be pinned down is better shown to you as ambiguous than misclassified as something specific that's really just a guess.

Where it shows up: the category chip on `/goals` board cards and the task-detail page, and a "Type" line in the channel's needs_human approval message (also present in Observer mode's automatic-only notification). Once a human decides the task (retry / mark complete / abandon), the category field clears and doesn't carry over into the next escalation.

### Buttons

| Button | Action |
|------|------|
| Retry (重試) | The task goes back to pending retry (`pending`); the driver dispatches it again on the next round. |
| Mark complete (標記完成) | Marks the task done (`done`) directly. |
| Give up (放棄) | Cancels the task (`cancelled`). |
| I'll take over (交給我) | You take over; the task is marked as claimed by you (`claimed_by`). Its status **stays** `needs_human`, so the driver already never auto-dispatches it again (the candidate query only looks at `todo`/`pending`/`revising`). That's the current scope of this feature: stop auto-retry, mark it, and collapse the card. Fully handing conversation control to you (so later messages stop reaching AI judgment) is a later-stage feature, not yet implemented. |

A single message allows at most 3 primary actions; four buttons exceed that, so "Give up" and "I'll take over" fold into a secondary tier on channels that support one: a second row of buttons on Telegram, a second row on Discord, a native `overflow` menu on Slack. LINE has no equivalent secondary-menu mechanism, so these two actions don't appear among LINE's quick-reply buttons — they're described in the message text instead, with a dashboard link.

Button decisions are **idempotent and fail-closed**: Retry/Mark complete/Give up only transition a task out of `needs_human`; pressing one twice, or after the state has already changed, is a no-op. "I'll take over" has no terminal state to compare against, so pressing it again (even by a different authorized person) just re-stamps the claim; it never errors. Collaborator/consultant kickoff approvals work the same way: left undecided past its deadline counts as denied (fail-closed).

Since v1.53, an escalation to a human comes with a **simulated preview** (simulate-before-act): if you choose to let the task continue, roughly what will happen over the next three steps. The simulation has a 15-second cap; on timeout it's skipped and the approval request goes out as normal (never blocking on it). Knowledge referenced by the simulation is limited to read-only namespaces, and the simulation's narrative can never decide on its own whether an action is reversible (no self-certification). The dashboard's approval card renders this preview.

### Reviewing changes before approval

The simulated preview covers "what might happen if this is allowed to continue"; the "Changes (變更)" tab covers **what has already happened**. Both the dashboard's inbox decision card and the task-detail page have a "Changes" tab listing every file this task has actually touched across its rounds:

| Field | Content |
|------|------|
| Path | The path of the file created/modified/deleted; for a `command` type it shows the shell command itself, without pretending to know which files it touched. One-click copy. |
| Operation | One of create/overwrite, edit, delete, or command. |
| Status | Failed or blocked calls are also listed and flagged "Failed" — this is exactly the half a live tool-status query can't see. |
| Summary excerpt | An excerpt of the written content or command description, reusing the audit trail's masked result as-is (it never re-reads the original file just to render this). |
| Source | Either a native runtime tool event (Write / Edit / NotebookEdit / Bash…) or an MCP audit entry (`wiki_write`, etc). |

The evidence comes from two existing trails: native tool events from execution land as a file-change record after every dispatch round (attributed by task id), and MCP audit entries reuse the same "claim-to-review window plus executor" attribution the judge's `<tool_activity>` already relies on. **No record means no record**: when there's nothing, the tab shows "This task left no file-change record" rather than papering over it with a made-up narrative.

What's shown today is which files were touched and what the operation was, not a line-by-line before/after diff yet. A real diff needs a snapshot taken before the write, which is a later item.

---

## "Think it through" plan-first mode (I-1c)

Besides "Just ask (問一問)" and "Assign (交辦)," the dashboard's dispatch panel has a third mode, "Plan first (想一想)": the AI employee drafts an execution plan for you to review first, and only starts working once you approve it.

### Creation

Submitting with "Plan first" selected still calls `tasks.goal_create` on the frontend, just with an extra `plan_first: true`. The backend calls the utility LLM at task-creation time (synchronously, not a round of the dispatch loop) to produce a 3-8 item, plain-text execution plan from the goal description and acceptance criteria (not JSON — prose meant for a person to read). The task is born directly in `needs_human`, reusing the existing `blocked_needs_decision` ("Waiting on your decision") category with no new category added. Before approval it never enters the dispatch loop's candidate query, so not even one round runs.

### Approval and injection

The plan text is written to two places:

- `judge_feedback` (an existing field): so the "waiting on your decision" card, the task-detail page, and the channel approval message can all show the plan content with no display-logic changes needed.
- `plan_pending` (a new field, I-1c-specific): stored separately from `judge_feedback` because approving (the "Retry" button, generic to any `needs_human` task) overwrites `judge_feedback` with your approval note; if the plan lived in that same field, the moment you approved it, your own approval action would overwrite it, and it would never reach the first dispatch round.

Approval is the existing "Retry" button — no new button type is added. Once approved, the task moves back to `pending`, and on the driver's next tick, `plan_pending`'s content is wrapped into an `<execution_plan>` block and injected into that round's work prompt, so the AI employee starts executing per the plan; `plan_pending` is cleared immediately after injection, so the plan is pasted in exactly once, never repeating in the prompt on later rounds.

A plan is guidance, not a pass that skips acceptance. Once execution finishes it still goes through the exact same two-stage acceptance judging / MAV judge panel as any other goal task — if the judge decides the plan's steps weren't enough, or were wrong, it gets sent back for revision all the same.

### When the planner fails

The utility-LLM call that drafts the plan can itself fail (timeout, a transport error, or an entirely blank reply). In that case the task still fails closed and sits at `needs_human`, but with its category changed to `infra` (system problem) instead of `blocked_needs_decision`, and with no `plan_pending` attached. Approving a task like this only ever releases it into a first round with "no plan to inject," never a plan vanishing or being silently skipped.

---

## Directions and stopping on the task page

The task detail page has two controls for a goal task in progress. Full rules are in [Continuous responsibilities, mid-run directions and stopping a task](continuous-responsibilities.md#the-task-detail-page).

- **Direction for the next round** (needs `[goal_loop] steering_enabled = true`, off by default): a note of up to 4,000 characters handed to the AI employee when the next round is dispatched. The current round is not interrupted. A note counts as delivered only when its round was actually dispatched ("Queued for round N"); a round that was refused or failed to dispatch gives its notes back to the queue. Notes never touch the frozen acceptance baseline and the judge never sees them. A round carrying notes runs with the single employee, not as a team round.
- **Stop task**: cancels the task and every sub-task at once, with a real dashboard identity. A round already running finishes first (`cancel_pending`), so the page reports `stopped` only when nothing is running any more, and `stopped_uncertain` when something cannot be confirmed (a lapsed claim with no finished round, an external action with an unknown result, a tree past the scan limit). The dialog remembers the version you saw when it opened; if the task changed before you confirmed, it says so and waits for you to confirm again. A stopped task cannot be retried or continued; create a new task to redo the work.

---

## Termination guarantee

The driver, not the model, owns the hard boundaries, so a stuck goal can never loop forever: only an acceptance judge's sign-off counts as a completion signal (the AI's own "I'm done" self-report is never trusted); the dispatch cap, wall-clock cap, concurrency cap, progress-oscillation detection, and the feedback-path circuit breaker each apply independently, and hitting any one of them triggers an escalation to a human or a trip of the breaker.
