# Autonomous Goal Loop

> Drop a goal in chat; the agent plans, works, and gets independently judged until it's done — or a hard bound trips and a human decides.

---

## What It Is

The goal loop (v1.37) turns one-shot Q&A into "give a goal → agent loops to completion → stuck escalates to a human". From any connected channel (Telegram / Discord / Slack / LINE / …) you type:

```
/goal <goal description> || <acceptance criteria>
/goal 產出 Q3 月報 || 含每月營收圖表 || outcome:files:report.docx
/goal status
```

This creates a `goal_mode` task on the Task Board, stamped with the source channel and chat so progress pushes back to the conversation that started it. Omit the `||` part and the goal description itself becomes the acceptance baseline; an optional third `outcome:<spec>` segment adds a machine-checkable output contract (JSON Schema subset or workdir file globs) that runs deterministically, at zero LLM cost, *before* the judge — structurally deficient output bounces straight back to revision without spending a judge call.

Acceptance criteria are frozen into an immutable baseline the moment the goal is created — every judge call (both the first-stage evaluator and the panel) reads that baseline, never a field an agent could edit later. An agent-identity `tasks_update` call is refused (with an audit entry) if it tries to change the acceptance criteria on a `goal_mode` task; only a dashboard operator can still edit the display copy, and doing so does not retroactively change what the frozen baseline judges against. Skip the `||` criteria and the confirmation reply also nudges you toward a sharper contract next time: a four-element prompt (goal / inputs / output format / constraints) plus a suggestion to write 3–5 outcome-style criteria.

The dispatch engine behind this is **on by default since v1.59** (it idles at a periodic SQLite poll until a goal task exists; the acceptance judge only spends an LLM call when a goal actually reaches review). To opt out, set `config.toml [dispatch] enabled = false` — or flip the「派工引擎」switch in Settings → Automation, which hot-applies without a restart.

## The Driver

`GoalLoopDriver` (`goal_loop.rs`) is the outer loop. Every 30 seconds (configurable `tick_secs`) it finds `goal_mode` tasks waiting to run and enqueues a work message into the existing message queue — the same wake-up rail a channel message uses — so the agent claims, works, and completes through unchanged plumbing. The closed loop:

```
driver enqueue ─▶ dispatcher ─▶ agent works ─▶ goal task → review
     ▲                                              │
     └── reject → pending (+judge feedback) ◀── acceptance judge ──▶ pass → done
```

On rejection the task returns to `pending` carrying the judge's feedback; the very next tick re-dispatches it with that feedback in the work message — a Generator-Verifier retry loop. Every state transition pushes a short (1–3 line) progress note back to the source conversation, deduped per state.

## The Acceptance Judge — Never Self-Report

"Done" is only ever declared by the verifier, not the worker. When the agent reports completion, the task enters `review`. A cheap **first-stage evaluator** runs first — one tool-less LLM call returning a three-way JSON decision (`continue` / `candidate_complete` / `blocked`). `continue` skips the panel and re-dispatches immediately with the evaluator's `next_step` as feedback (counts against the iteration cap); `blocked` goes straight to `needs_human`; only `candidate_complete` reaches the full panel below. Any evaluator failure (timeout, parse error, transport error) degrades straight to the panel — it never auto-accepts or auto-rejects on its own malfunction. Config: `[dispatch] two_stage_judge` (default `true`; `false` reverts to every review going straight to the panel).

Once a round reaches the panel, `DispatchEngine` runs a **three-aspect MAV panel** — correctness, completeness, safety — in one LLM call through the account rotator. All three must pass; a parse failure, a truncated/malformed panel reply, or a judge transport error parks the task as `needs_human`, never auto-accepts (fail-closed) — a truncated JSON fragment can no longer fall through to a legacy token scanner that might misread a stray `pass` substring as an accept. This is the defense against the classic loop trap where an agent narrates success and the system believes it.

The panel prompt also carries four standing discipline clauses, always on (no config gate): **no ratcheting** the bar higher round over round when the criteria haven't changed; **audit, don't author** — the judge may only check the worker's submitted evidence and the tool-activity digest, never invent its own; **no scope creep** — a requirement absent from the acceptance criteria can't be the reason for a rejection; and **a self-reported "done" is not evidence** on its own.

Judge depth scales with goal difficulty (a local, zero-LLM heuristic): simple single-step goals get a two-aspect check (correctness + safety) and a lower iteration cap; hard goals get the full panel. The safety aspect is never dropped at any depth.

## Hard Guards — the Driver Owns the Bounds

Termination is guaranteed by the driver, not by trusting the model:

| Guard | Default | On breach |
|---|---|---|
| Iteration cap (dispatches per task) | 8 (hard goals), 3 (simple) | `needs_human` |
| Wall clock from creation | 24 h | `needs_human` |
| Concurrent goal tasks | 3 | queued, not dispatched |
| Stagnation detection | two consecutive rejections with the same **gap fingerprint** (`path:line` citations + backtick key tokens extracted and normalized from the feedback, not raw text equality — a reworded restatement of the same gap still matches; falls back to literal-text comparison only when no citation/token is extractable) | early `needs_human` |
| Bail-pattern detection | nine zh+en anchored regexes over the agent's last non-empty paragraph (e.g. self-signed `VERDICT:`, "check back later", "ready for review") | telemetry + activity event + a hint folded into the next round's judge/evaluator input — never rejects or blocks on its own |
| In-flight dedup | dispatched-but-unclaimed tasks are not re-enqueued until a stall timeout (600 s) | re-dispatch |
| Cross-process circuit breaker (`dispatch_guard`) | 20 dispatches / 60 s sliding window | cooldown refusal |
| Delegation hop depth | 5 | dispatch refused |
| Resume on gateway restart (`resume_on_restart`) | `pause` (**default**) escalates every in-flight `goal_mode` task to `needs_human` (reason `gateway_restart`) at boot | `auto` resumes in-flight tasks exactly as before a restart instead; toggle either in `config.toml` or the dashboard's Settings → Automation tab (`system.update_config`, whitelisted to `"auto"`/`"pause"` only) |
| No-progress report (`progress_report_minutes`) | 10 min of silence on an already-claimed task (Activity Feed signal, not the lease-renewer-refreshed `updated_at`) | one "still running, no progress signal" notice to Activity Feed + source conversation, at most once per round — never re-dispatches, escalates, or cancels; `0` disables it |
| Tool-call streak advisory (`tool_streak_advisory`) | 3 / 5 / 8 consecutive calls to the same tool with the same (masked) arguments, within one round | an escalating zh-TW hint injected into the next round's `<state>` block — zero LLM cost, advisory only, never blocks or vetoes a call; excluded from the oscillation-detection state hash |
| Ephemeral spawn admission (`[dispatch] admission`) | Over the concurrency cap (`ephemeral_max_active`, default 32) | `"queue"` (**default**) durably FIFO-queues the request instead of rejecting it (bounded depth, per-ticket TTL, audited); `"fail"` reverts to the pre-H19 immediate rejection |

All under `[goal_loop]` / `[dispatch_guard]` / `[dispatch]` in `config.toml`, with built-in defaults when the sections are absent.

## needs_human Escalation

When a task parks as `needs_human`, it is stamped with a closed six-way `pause_reason` classification alongside the free-text `judge_feedback` — `no_progress` / `budget_exhausted` / `blocked_needs_decision` / `infra` / `restart` / `unknown` — assigned statically at the trigger site (never guessed from judge/evaluator prose). It renders as a chip on the `/goals` board and task detail, and as a "type" line in the channel approval message; unclassified or legacy rows read as `unknown`, the safe default. `goal_notify.rs` pushes an approval message with four inline actions — **retry / mark done / abort / take over** — to the source conversation (falling back to the agent's `[proactive]` control channel). A message caps at 3 primary actions, so retry/mark-done stay primary and abort/take-over collapse into each platform's secondary affordance: a second row on Telegram and Discord, a native `overflow` menu on Slack; LINE has no secondary-menu affordance at all, so those two are dropped from its quick reply and listed as plain text with a dashboard link instead. Other non-button channels get a text fallback, and the dashboard shows a needs_human board column.

**Take over** claims the task (`claimed_by`) without resolving it — the task stays `needs_human`, which is already outside the driver's dispatch-candidate query, so automatic retries are already stopped without a status change. This is the button-driven layer of takeover (stop the loop + mark + collapse the card). Transferring the **conversation** — pausing inbound AI replies and every scheduled dispatch aimed at it — is the separate typing-triggered feature in [42-human-takeover.md](42-human-takeover.md); a conversation takeover also freezes this loop for any goal task that came from it.

Decisions are idempotent and fail-closed: retry/mark-done/abort only transition *from* `needs_human`, so a stale or double press is a no-op; take-over has no terminal state to compare against, so a repeat press (even by a different authorized decider) simply re-claims it.

## Plan-First Mode ("想一想", I-1c)

The dashboard assign panel's third mode, alongside 問一問 (ask) and 交辦 (assign). Selecting it sends `tasks.goal_create` with `plan_first: true`: instead of dispatching, the gateway synchronously calls the utility LLM to draft a short (3–8 bullet, plain-text) execution plan from the goal + acceptance criteria, and the task is born directly in `needs_human` — under the existing `blocked_needs_decision` pause reason, no new class added — so it never reaches the driver's dispatch-candidate query until a human approves.

The plan lives in a dedicated `plan_pending` column, kept separate from `judge_feedback` on purpose: the "重試" (retry) action any `needs_human` task uses to resume overwrites `judge_feedback` with the human's own approval note, which would otherwise erase the plan before it was ever read. Approving is that same existing retry action — no new button kind was added. Once approved, the very first dispatch round injects the plan as an `<execution_plan>` block into the work prompt, then clears `plan_pending` so later rounds don't repeat it. The plan is guidance, not an exemption — the executed work still goes through the same two-stage/MAV acceptance judging as any other goal task.

If the planner call itself fails (timeout, transport error, empty reply), the task still parks `needs_human` fail-closed — but under the `infra` pause reason instead of `blocked_needs_decision`, with no `plan_pending` set, so approval can never silently start a task with no plan behind it.

## Autonomy Levels

Each agent's leash length is one dial: `agent.toml [capabilities] autonomy_level`. Missing or unparseable defaults to the conservative `approver`.

| Level | Behavior |
|---|---|
| `operator` | Loop never auto-drives; tasks sit until a human pushes them. |
| `collaborator` / `consultant` | First dispatch requires a human kickoff approval (ApprovalBroker, 1 h TTL, expiry = denial). Then autonomous to completion. |
| `approver` | **Default.** No kickoff gate; humans are only consulted at `needs_human`. |
| `observer` | Fully autonomous; `needs_human` notifies but does not wait. |

## ActionGuard: Three-Valued Irreversibility

Layered on top, per tool call (`approval.rs`, after Magentic-UI's ActionGuard):

- `irreversible_tools` — **always** requires human approval.
- `maybe_irreversible_tools` — an LLM judge rules on *this specific call*; risky (or judge failure/timeout) escalates to a human, safe auto-proceeds. Fail-closed.
- Unlisted — the existing allowed/denied/policy flow, no new friction.

The merge with the legacy `approval_required_tools` is take-the-stricter, so existing configs keep their exact semantics.

Full operator reference — config keys, dispatch policies, parallel sub-task DAGs, outcome schemas — lives in [`docs/guides/goal-loop.md`](../guides/goal-loop.md).
