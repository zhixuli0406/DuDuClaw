# Live Run Forking

> Split one in-flight run into N competing branches, let an AI judge keep the best, and merge only the winner back.

---

## The Metaphor: Cloning Yourself at Every Fork in a Maze

Imagine standing at a junction in a maze. You don't know which way leads to the exit. Instead of picking one path and hoping, you **clone yourself** — one copy down the left tunnel, one down the right, one straight ahead. Each clone explores independently, on its own legs, in its own corridor, without bumping into the others.

When the clones come back, a **judge at the entrance** asks each one what it found: did you reach the exit? how clean was the route? are you sure? The judge keeps the clone that actually found the way out and discards the rest — as if the dead-ends never happened.

DuDuClaw's Live Run Forking is the same idea applied to an agent run. A single task is split into parallel **branches**, each running in its own isolated workspace with its own account and its own budget. When they finish, an **AI judge** scores them and the winner's work is merged back into the parent — the losing branches are thrown away.

This pattern is inspired by [`pydantic-deepagents`](https://github.com/vstorm-co/pydantic-deepagents), whose signature capability is exactly this: fork an in-flight run into competing branches and let a judge merge the winner. RFC-26 maps that concept onto DuDuClaw's existing `AccountRotator`, container sandbox, and GVU judge primitives.

---

## Why Forking, Not Just Retrying

DuDuClaw's GVU self-play loop already improves an answer — but it is **sequential**: generate, verify, update, repeat. That's a single thread of reasoning refining itself one step at a time.

Forking is different. It runs **genuinely independent attempts in parallel**, each potentially steered toward a different strategy:

```
Sequential (GVU):   attempt → critique → attempt' → critique → attempt''
                    (one line of reasoning, refined over time)

Parallel (Fork):    ┌─ branch A: "refactor with a state machine"
        fork_run ───┼─ branch B: "refactor with early returns"
                    └─ branch C: "refactor with a lookup table"
                    (three independent attempts, judged at the end)
```

When a task has multiple plausible approaches and you can afford to try several at once, forking explores the solution space in breadth rather than depth.

---

## The Flow

```
                      fork_run(prompt, n, strategies[], budget, merge_mode)
                                   |
                                   v
        ┌──────────────────────────────────────────────────────────┐
        │  Each branch gets:                                         │
        │   • a copy-on-write workspace overlay (read-through parent)│
        │   • a distinct account from the AccountRotator             │
        │   • its own per-branch budget_usd cap                      │
        │   • an optional steering message                           │
        └──────────────────────────────────────────────────────────┘
                                   |
              ┌────────────────────┼────────────────────┐
              v                    v                    v
        ┌──────────┐         ┌──────────┐         ┌──────────┐
        │ branch A │         │ branch B │         │ branch C │   ← tokio::spawn,
        │ (overlay)│         │ (overlay)│         │ (overlay)│     run in parallel
        └────┬─────┘         └────┬─────┘         └────┬─────┘
             │  (optional test_command run per branch) │
             └────────────────────┼────────────────────┘
                                   v
                            ┌─────────────┐
                            │  AI  JUDGE  │   confidence =
                            │ JudgeAgent  │     quality_spread·0.4
                            └──────┬──────┘   + test_pass_ratio·0.4
                                   │          + internal_consistency·0.2
                                   v
                            ┌─────────────┐
                            │  MergeMode  │   Auto / AutoWithFallback /
                            │   resolve   │   Vote / Manual
                            └──────┬──────┘
                                   v
                  winner.overlay.promote() → parent workspace
                  (losers discarded; nothing else changes)
```

Everything is **default off**. Nothing forks until an agent sets `[fork] enabled = true` in `agent.toml`.

---

## The Cross-Process Source of Truth: ForkStore

Forks **execute** inside the MCP-server process (where the `claude` subprocesses are spawned), but the **gateway** is what serves `/metrics` and the dashboard's `fork.list / inspect / resolve` RPC. An in-process registry can't span both processes.

So fork and branch state lives in a shared WAL SQLite database — `ForkStore` at `<home>/fork_store.db` — that both processes open:

```
        ┌────────────────────────┐        ┌────────────────────────┐
        │   MCP-server process    │        │     gateway process     │
        │  (spawns branch runs)   │        │  (/metrics + dashboard) │
        │                         │        │                         │
        │  mcp_fork /             │        │  render_fork_metrics_from│
        │  mcp_fork_exec          │        │  handle_fork_list/...    │
        └───────────┬────────────┘        └───────────┬─────────────┘
                    │  writes                          │  reads
                    │            ┌──────────────┐      │
                    └───────────▶│  ForkStore   │◀─────┘
                                 │  (WAL SQLite │
                                 │  fork_store. │
                                 │     db)      │
                                 └──────────────┘
                       WAL + busy_timeout ⇒ concurrent reader/writer safe
```

The `mcp_fork` + `mcp_fork_exec` handlers were refactored **off** the legacy in-process registry **onto** `ForkStore`. It has two tables — `forks` and `fork_branches` — and exposes `insert_fork`, `update_branch`, `set_resolution`, `set_all_branch_states`, `get_fork`, `list_branches`, `list_forks`, and `metrics`. The same pattern (WAL + busy timeout) used by `SqliteMemoryEngine` makes concurrent reader/writer access safe.

---

## Branch Lifecycle States

A branch moves through a typed `BranchState` enum (no numeric codes for callers to guess at):

| State | Meaning | Judgeable? |
|-------|---------|-----------|
| `Pending` | Created, not yet started | No |
| `Running` | Subprocess executing | No |
| `Finished` | Completed cleanly — eligible for the judge | **Yes** |
| `BudgetKilled` | Killed for crossing a per-branch or aggregate budget cap | No |
| `Terminated` | Externally killed via `terminate_branch` | No |
| `Failed` | No account, spawn failure, or executor error | No |

Only `Finished` branches are `is_judgeable()`. If **zero** branches are judgeable, the judge returns an error and the caller surfaces the fork to the operator — it never auto-picks from nothing (fail-closed).

---

## Budget Enforcement

Two layers of budget protection live in `budget.rs`, both fail-closed:

**`Pool` — post-charge accounting.** Each branch is `register`ed with a per-branch cap; `try_charge` enforces *both* the per-branch cap and the aggregate cap. On any rejection (`BranchExceeded` / `AggregateExceeded`) nothing is committed — the caller must stop the branch. An unregistered branch has a zero cap, so any positive charge is denied.

```
Pool::try_charge(branch, amount):
     branch_spent + amount > branch_cap   ⇒ BranchExceeded   (nothing committed)
     aggregate_spent + amount > agg_cap    ⇒ AggregateExceeded (nothing committed)
     otherwise                             ⇒ Allowed          (commit both counters)
```

**`LiveAggregate` — streaming-time pre-emption.** While branches stream stream-json, their *running* `total_cost_usd` is watched live. The moment the combined in-flight spend crosses the aggregate cap, `observe` names the **single most-expensive in-flight branch** (deterministic tie-break by id) for the caller to kill mid-stream — sacrificing the fewest branches rather than waiting for each to hit its own cap. NaN/negative costs are sanitized to 0.0; `finish` frees a branch's budget for survivors once it ends.

No silent caps: when the branch count is reduced to the number of available accounts, the reduction is logged.

---

## The Judge

`JudgeAgent` produces a `JudgeVerdict { winner, confidence, per_branch_scores, rationale }`. Confidence follows the deep-agents formula in `JudgeScores`:

```
confidence = quality_spread       · 0.4
           + test_pass_ratio      · 0.4
           + internal_consistency · 0.2
```

Each sub-score is clamped to `[0,1]` (out-of-range ⇒ clamp + warn; NaN ⇒ 0.0). Two implementations ship:

- **`LlmJudge<C: LlmCaller>`** — builds a multi-candidate, XML-delimited prompt (`build_judge_prompt`) with candidate-tag escaping for injection resistance; parses the response JSON-first (with fence stripping). The backend is injected so the gateway can wire the Confidence Router (local-first, escalate to Claude on low confidence). An unparseable verdict or out-of-bounds index returns an error (fail-closed).
- **`HeuristicJudge`** — a deterministic, zero-LLM fallback judge, used when no LLM is available.

`test_pass_ratio` is computed from each branch's `test_exit_code` across judgeable branches, neutral (0.5) when no branch was tested.

---

## Merge Modes

`merge::resolve` turns a verdict into a `MergeDecision { winner, needs_confirmation, reason }`:

| Mode | Behavior |
|------|----------|
| `Auto` | Pick the verdict winner, promote immediately, no human in the loop |
| `AutoWithFallback` | **Default** — pick the winner but surface a confirm before promoting |
| `Vote` | Sample the judge `VOTE_ROUNDS` (3) times, take the majority; tie or low mean-confidence ⇒ defer |
| `Manual` | Always defer to the operator (`winner = None`) |

A winner below `DEFAULT_CONFIDENCE_THRESHOLD` is deferred **regardless** of mode. The winner's overlay is `promote()`d into the parent workspace **only** when the decision is final and needs no confirmation — otherwise the parent is left untouched until an operator resolves it via `merge_or_select`.

---

## Copy-on-Write Overlays

Each branch works in a `BranchOverlay` that reads through the parent workspace but keeps its writes local until promotion:

- **`Snapshot`** — the portable MVP backend: a recursive `copy_tree` of the parent.
- **`NativeCow`** — `clonefile(2)` via `cp -c` on macOS/APFS, `cp --reflink=always` on Linux btrfs/XFS.

`detect_backend()` probes the host once (cached) and falls back to `Snapshot` if a native clone fails — **isolation is never compromised, only the speed/space optimization**. Losing overlays are discarded; only the winner's writes are merged through.

---

## MCP Tools

All six tools are gated behind `Scope::ForkExecute` (enumerated explicitly; any unknown tool defaults to requiring Admin scope — the existing fail-closed rule) and are only usable when the agent has `[fork] enabled = true`:

| Tool | Purpose |
|------|---------|
| `fork_run` | Split the current task into N branches |
| `inspect_branches` | List live branches + state + spend |
| `diff_branches` | Show file/output diff between two branches (`truncate_bytes` on each side) |
| `merge_or_select` | Resolve a fork — judge verdict or explicit pick |
| `terminate_branch` | Kill a runaway branch (cancels not-yet-started; SIGKILLs an in-flight subprocess mid-stream) |
| `fork_cost` | Aggregate + per-branch spend |

`fork_run`, `merge_or_select`, and `terminate_branch` are flagged `is_state_changing` for the audit trail.

---

## Configuration

Per-agent in `agent.toml`:

```toml
[fork]
enabled              = false              # default off
max_branches         = 4                  # hard cap (avoid account/quota blowout)
default_budget_usd   = 0.50               # per-branch cap
aggregate_budget_usd = 1.50               # across all branches
merge_mode           = "auto_with_fallback"
test_command         = ""                 # optional; empty ⇒ test_pass_ratio neutralized
test_timeout_s       = 120
```

A missing or malformed `[fork]` block is a disabled fail-safe — no panic. Invalid sub-values (e.g. `max_branches = 0`, negative budget) fall back to defaults; an unknown `merge_mode` string falls back to `AutoWithFallback` with a warning.

---

## Observability

The gateway `/metrics` endpoint reads the cross-process `ForkStore` at scrape time and emits Prometheus lines:

| Metric | Type | Meaning |
|--------|------|---------|
| `duduclaw_fork_runs_total` | counter | Total forks created |
| `duduclaw_fork_resolved_total` | counter | Forks resolved to a winner |
| `duduclaw_fork_promoted_total` | counter | Forks whose winner was promoted |
| `duduclaw_fork_branches_total` | counter | Total branches across all forks |
| `duduclaw_fork_branch_outcome{outcome="finished\|budget_killed\|failed"}` | counter | Branches by terminal outcome |
| `duduclaw_fork_spend_usd_total` | counter | Aggregate USD spent across all forks |

Every fork resolution also appends to `<home>/fork_history.jsonl` (via `with_file_lock`, cross-process safe) and inserts a `fork_resolved` row into the gateway's `activity` table so it appears on the dashboard Activity Feed. The dashboard `ForkPage` lists recent forks, shows branches side-by-side, highlights the judge's winner, and offers manual resolution (`fork.resolve` behind a manager check).

---

## Implemented vs Planned

Per RFC-26 §5, all six phases (P1–P6) have landed:

| Phase | Scope | Status |
|-------|-------|--------|
| P1 | `duduclaw-fork` crate: `Branch`, `BranchOverlay`, `budget::Pool`, controller | Done |
| P2 | `JudgeAgent` + `JudgeVerdict`, test runner, merge modes | Done |
| P3 | 6 MCP tools + `Scope::ForkExecute` + dispatch wiring | Done |
| P4 | Parallel execution, aggregate budget pool, native CoW, streaming budget kill, external SIGKILL | Done |
| P5 | Prometheus `/metrics` via shared `ForkStore` + `fork_history.jsonl` + Activity Feed + dashboard `ForkPage` | Done |
| P6 | Secondary parity items (Plan Mode, checkpoint fork/rewind + SQLite, built-in skills, memory `/improve`, Task Board claim + cycle detection) | Done |

The **one by-design exclusion**: `terminate_branch` can only kill a child in the process that owns it. Killing a branch subprocess across a *different* process (e.g. the gateway reaching into an MCP-server child) is out of scope — kill must originate where the child was spawned.

---

## Why This Matters

### Breadth-First Problem Solving

Some tasks have several plausible strategies and no obvious best one up front. Forking tries them at once and lets evidence (tests + judge) decide — instead of committing to one approach and discovering it was wrong an hour later.

### No Rate-Limit Collision

Because each branch draws a **distinct account** from the `AccountRotator`, N parallel runs don't fight over one account's rate limit. The branch count is capped to the number of available accounts so distinctness always holds.

### Cost Stays Bounded

Two budget layers — per-branch and live aggregate — mean a runaway branch is killed mid-stream, not after it has burned the whole budget. Nothing is silently capped; reductions are logged.

### Fail-Closed Throughout

A missing judge, an unparseable verdict, a sandbox spawn failure, or zero judgeable branches all defer to the operator — never a silent auto-pick. Sub-threshold confidence defers too. The default-off posture means none of this runs until explicitly enabled.

---

## The Takeaway

When you're lost in a maze and can afford to clone yourself, you don't pick one tunnel and pray — you send a copy down each, and let a judge at the entrance keep only the clone that found the exit. Live Run Forking gives DuDuClaw agents that ability: parallel competing branches, isolated workspaces, distinct accounts, bounded budgets, an AI judge, and a cross-process `ForkStore` so the gateway and dashboard can watch it happen. Default off, fail-closed, no silent caps — explore in breadth, merge only the winner.
