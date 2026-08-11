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

The whole feature is opt-in: nothing runs unless `config.toml` has `[dispatch] enabled = true`.

## The Driver

`GoalLoopDriver` (`goal_loop.rs`) is the outer loop. Every 30 seconds (configurable `tick_secs`) it finds `goal_mode` tasks waiting to run and enqueues a work message into the existing message queue — the same wake-up rail a channel message uses — so the agent claims, works, and completes through unchanged plumbing. The closed loop:

```
driver enqueue ─▶ dispatcher ─▶ agent works ─▶ goal task → review
     ▲                                              │
     └── reject → pending (+judge feedback) ◀── acceptance judge ──▶ pass → done
```

On rejection the task returns to `pending` carrying the judge's feedback; the very next tick re-dispatches it with that feedback in the work message — a Generator-Verifier retry loop. Every state transition pushes a short (1–3 line) progress note back to the source conversation, deduped per state.

## The Acceptance Judge — Never Self-Report

"Done" is only ever declared by the verifier, not the worker. When the agent reports completion, the task enters `review` and `DispatchEngine` runs a **three-aspect MAV panel** — correctness, completeness, safety — in one LLM call through the account rotator. All three must pass; a parse failure or judge error parks the task as `needs_human`, never auto-accepts (fail-closed). This is the defense against the classic loop trap where an agent narrates success and the system believes it.

Judge depth scales with goal difficulty (a local, zero-LLM heuristic): simple single-step goals get a two-aspect check (correctness + safety) and a lower iteration cap; hard goals get the full panel. The safety aspect is never dropped at any depth.

## Hard Guards — the Driver Owns the Bounds

Termination is guaranteed by the driver, not by trusting the model:

| Guard | Default | On breach |
|---|---|---|
| Iteration cap (dispatches per task) | 8 (hard goals), 3 (simple) | `needs_human` |
| Wall clock from creation | 24 h | `needs_human` |
| Concurrent goal tasks | 3 | queued, not dispatched |
| Oscillation detection | two consecutive near-identical rejection feedbacks | early `needs_human` |
| In-flight dedup | dispatched-but-unclaimed tasks are not re-enqueued until a stall timeout (600 s) | re-dispatch |
| Cross-process circuit breaker (`dispatch_guard`) | 20 dispatches / 60 s sliding window | cooldown refusal |
| Delegation hop depth | 5 | dispatch refused |

All under `[goal_loop]` / `[dispatch_guard]` in `config.toml`, with built-in defaults when the sections are absent.

## needs_human Escalation

When a task parks as `needs_human`, `goal_notify.rs` pushes an approval message with four inline actions — **retry / mark done / abort / take over** — to the source conversation (falling back to the agent's `[proactive]` control channel). A message caps at 3 primary actions, so retry/mark-done stay primary and abort/take-over collapse into each platform's secondary affordance: a second row on Telegram and Discord, a native `overflow` menu on Slack; LINE has no secondary-menu affordance at all, so those two are dropped from its quick reply and listed as plain text with a dashboard link instead. Other non-button channels get a text fallback, and the dashboard shows a needs_human board column.

**Take over** claims the task (`claimed_by`) without resolving it — the task stays `needs_human`, which is already outside the driver's dispatch-candidate query, so automatic retries are already stopped without a status change. This is the scoped first layer of takeover (stop the loop + mark + collapse the card); transferring the conversation itself to the human is a later-phase feature, not yet implemented.

Decisions are idempotent and fail-closed: retry/mark-done/abort only transition *from* `needs_human`, so a stale or double press is a no-op; take-over has no terminal state to compare against, so a repeat press (even by a different authorized decider) simply re-claims it.

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
