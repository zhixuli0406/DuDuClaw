# RFC-26: Deep Agents Alignment — Live Run Forking & Concept Parity

**Status:** Implemented & complete — P1–P6 all landed (core forking, cross-process `ForkStore`, gateway `/metrics`, dashboard `ForkPage`, Activity Feed, native CoW, per-branch streaming budget kill, **cross-branch aggregate pre-emption**, external SIGKILL, and all five §4 parity items). See [`docs/todo/TODO-rfc26-live-forking.md`](../todo/TODO-rfc26-live-forking.md) for the per-item checklist. No residual feature work remains; the only by-design exclusion is killing a branch child across a *different* process (out of scope — `terminate_branch` runs in the process that owns the child).
**Author:** DuDuClaw team
**Date:** 2026-06-19
**Inspiration:** [vstorm-co/pydantic-deepagents](https://github.com/vstorm-co/pydantic-deepagents) — a Pydantic-AI agent harness whose signature capability is *live run forking* (split an in-flight run into N competing branches, let an AI judge merge the winner).

---

## 1. Motivation

`pydantic-deepagents` packages the "deep agent" pattern: planning → forking → memory → teams → persistence. A concept-by-concept audit against DuDuClaw shows we already cover **most** of it, but with one genuine gap and several places where a small enhancement reaches feature parity.

This RFC does two things:

1. **Maps every deep-agents concept onto DuDuClaw's existing subsystems** (so we don't rebuild what we have).
2. **Specifies the one genuinely missing capability — Live Run Forking + Judge Agent — built on DuDuClaw's existing `AccountRotator`, container sandbox, and GVU judge primitives.**

### 1.1 Concept audit

| deep-agents concept | DuDuClaw today | Action |
|---|---|---|
| Memory (`MEMORY.md` auto-inject) | `SqliteMemoryEngine` + Memory Intelligence F1–F3 | **Enhance**: add `/improve`-style session→memory distillation surface |
| Skills (`SKILL.md` on-demand) | Skill ecosystem + auto-synthesis | **Parity**: ship built-in `code-review / refactor / test-writer / git-workflow` skill set |
| MCP integration | Full MCP server + HTTP/SSE | ✅ covered |
| Hooks (PRE/POST_TOOL_USE + security preset) | `.claude/hooks/` 3-phase defense | ✅ covered |
| Agent Teams (shared TODO + message bus + deps) | `reports_to` hierarchy + Task Board + Shared Wiki + `bus_queue.jsonl` | **Enhance**: task-claim atomicity + dependency cycle detection on Task Board |
| Checkpoints (save / rewind / fork) | `duduclaw-durability/checkpoint.rs` (linear, in-memory) | **Enhance**: add `fork()` + `rewind()` + durable backend |
| Sub-agents / Swarms | `create_agent` / `spawn_agent` / `list_agents` | ✅ covered |
| Plan Mode (clarify-first planner subagent) | Task Board (no interactive clarify) | **New (small)**: clarify-first planner mode |
| **Live Run Forking + Judge Agent** | GVU is **sequential** self-play, not parallel competing branches | **New (core)** — §3 |

---

## 2. Design principles (carried from DuDuClaw conventions)

- **Default off, per-agent opt-in** via `agent.toml` — same posture as the PTY pool (`[runtime] pty_pool_enabled`).
- **Reuse, don't rebuild**: forking sits on `AccountRotator` (N accounts → N parallel runs without rate-limit collision), the container sandbox (`duduclaw-container` — Apple Container gives native copy-on-write), and GVU's `build_judge_prompt` / `parse_judge_response` (the LLM judge already exists).
- **Fail closed**: a missing judge, an unparseable verdict, or a sandbox spawn failure falls back to `manual` merge (surface branches to the operator), never silently auto-picks.
- **No raw byte slicing / unanchored contains / unlocked appends** — per the 2026-06 security conventions; branch logs go through `truncate_bytes` + `with_file_lock`.

---

## 3. Core feature: Live Run Forking

### 3.1 Concept mapping

deep-agents forks an in-process `agent.run()`. DuDuClaw runs agents as **`claude` CLI subprocesses** through `rotate_cli_spawn` / `call_with_rotation`. So a "branch" in DuDuClaw = **one isolated subprocess run** with:

- its own **workspace overlay** (copy-on-write dir, isolated writes),
- its own **account** (distinct `AccountEnv` from the rotator → no shared rate limit),
- its own **budget cap** + optional **steering prompt**,
- a shared **read-through** view of the parent workspace.

### 3.2 New crate: `duduclaw-fork`

```
crates/duduclaw-fork/
├── src/
│   ├── lib.rs              # ForkController — orchestrates N branches
│   ├── branch.rs           # Branch, BranchId, BranchState, BranchResult
│   ├── overlay.rs          # BranchOverlay — copy-on-write workspace (read-through parent, local writes)
│   ├── budget.rs           # per-branch + aggregate budget_usd enforcement (wraps AccountRotator)
│   ├── judge.rs            # JudgeAgent + JudgeVerdict (reuses gvu::verifier judge primitives)
│   ├── merge.rs            # MergeMode { Manual, Auto, AutoWithFallback, Vote }
│   └── test_runner.rs      # run test_command against a branch snapshot → exit code → score
```

### 3.3 Branch lifecycle

1. **`fork_run`** — clone the parent run context into N `Branch`es. Each gets a `BranchOverlay` (CoW workspace), a rotator-assigned account, a `budget_usd`, and an optional steering message.
2. **Parallel execution** — `tokio::spawn` one `rotate_cli_spawn` per branch; aggregate budget enforced via a shared `budget::Pool` (a branch that would exceed the aggregate cap is paused/terminated).
3. **Scoring** — when a branch finishes, `test_runner` runs `test_command` (e.g. `pytest -q`) against the branch snapshot; exit code feeds the judge.
4. **Judging** — `JudgeAgent` produces a `JudgeVerdict` per the deep-agents formula:
   `confidence = quality_spread·0.4 + test_pass_ratio·0.4 + internal_consistency·0.2`.
5. **Merge** — per `MergeMode`: `Auto` promotes the winner's overlay to the parent workspace; `AutoWithFallback` (default) auto-picks but surfaces a confirm; `Vote` runs N judges and takes consensus; `Manual` always defers to operator.
6. **Cleanup** — losing overlays discarded; winner's writes merged through.

### 3.4 MCP tools (added to `crates/duduclaw-cli/src/mcp.rs` `TOOLS` table + dispatch)

| Tool | Purpose | Key params |
|---|---|---|
| `fork_run` | Split current task into N branches | `n`, `strategies[]` (steering per branch), `budget_usd`, `test_command`, `merge_mode` |
| `inspect_branches` | List live branches + state + spend | `fork_id` |
| `diff_branches` | Show file/output diff between branches | `fork_id`, `branch_a`, `branch_b` |
| `merge_or_select` | Resolve a fork (judge or explicit pick) | `fork_id`, `branch_id?` |
| `terminate_branch` | Kill a runaway branch | `fork_id`, `branch_id` |
| `fork_cost` | Aggregate + per-branch spend | `fork_id` |

All six are gated behind a new `Scope::ForkExecute` (defence-in-depth, enumerated explicitly — unknown ⇒ Admin per existing fail-closed rule) and only registered when the agent has `[fork] enabled = true`.

### 3.5 `agent.toml` config

```toml
[fork]
enabled = false              # default off
max_branches = 4             # hard cap (avoid account/quota blowout)
default_budget_usd = 0.50    # per-branch cap
aggregate_budget_usd = 1.50  # across all branches
merge_mode = "auto_with_fallback"
test_command = ""            # optional; empty ⇒ test_pass_ratio neutralized in judge
test_timeout_s = 120
```

### 3.6 Observability

Prometheus counters mirroring the PTY-pool style: `fork_runs_total`, `fork_branches_total`, `fork_branch_outcome{outcome=win|lose|timeout|budget_killed}`, `fork_judge_confidence` (histogram), `fork_spend_usd` (histogram). Every fork resolution appends to `~/.duduclaw/fork_history.jsonl` (via `with_file_lock`) and the Activity Feed.

---

## 4. Secondary enhancements (parity items)

### 4.1 Plan Mode (clarify-first planner)
A `plan_start` MCP tool + `[planner] clarify_first = true`: before executing an ambiguous task, the planner subagent emits up to 3 clarifying questions, waits for answers (or times out to best-effort), then decomposes into Task Board subtasks with dependencies. Reuses existing `tasks_create` + sub-agent spawn.

### 4.2 Checkpoint fork/rewind
Extend `duduclaw-durability/checkpoint.rs`: add `fork(checkpoint_id) -> new_id` (copy state under a new lineage) and `rewind(task_id, checkpoint_id)` (restore an earlier snapshot), plus a durable SQLite backend so checkpoints survive restart. Enables "explore alternative approach from checkpoint X".

### 4.3 Built-in skill set parity
Ship `code-review`, `refactor`, `test-writer`, `git-workflow` as first-class bundled `SKILL.md` files in the skill registry so a fresh agent has the deep-agents default toolbox.

### 4.4 Memory `/improve`
A `memory_improve` MCP tool that runs GVU's reflection over recent sessions and proposes (not auto-applies) MEMORY/SOUL updates — bringing the existing GVU machinery to a user-invokable surface.

### 4.5 Task Board team coordination
Add atomic task **claim** (compare-and-set on `assignee`) and **dependency cycle detection** at `tasks_create`/`tasks_update` write time, matching deep-agents' shared-TODO semantics.

---

## 5. Phased rollout

| Phase | Scope | Risk | Status |
|---|---|---|---|
| **P1** | `duduclaw-fork` crate: `Branch`, `BranchOverlay`, `budget::Pool`, sequential 2-branch MVP behind `[fork] enabled` | Low — additive, default off | ✅ Done |
| **P2** | `JudgeAgent` + `JudgeVerdict` (reuse GVU judge), `test_runner`, merge modes | Medium | ✅ Done |
| **P3** | 6 MCP tools + `Scope::ForkExecute` + dispatch wiring | Medium | ✅ Done |
| **P4** | Parallel execution (N branches), aggregate budget pool, native CoW overlay (`clonefile`/reflink), streaming budget kill, external SIGKILL | Medium-High | ✅ Done |
| **P5** | Prometheus metrics on gateway `/metrics` (via shared `ForkStore`) + `fork_history.jsonl` + Activity Feed + dashboard `ForkPage` | Low | ✅ Done |
| **P6** | Secondary parity items (§4) — Plan Mode, checkpoint fork/rewind + SQLite, built-in skills, memory `/improve`, Task Board claim + cycle detection | Low | ✅ Done |

Each phase is independently shippable and default-off; nothing changes runtime behavior until an agent sets `[fork] enabled = true`.

---

## 6. Open questions — resolved in implementation

1. **Workspace overlay backend** — *Resolved:* both shipped. `Snapshot` (`copy_tree`) is the portable MVP backend; `NativeCow` uses `clonefile(2)` (`cp -c`) on macOS/APFS and `cp --reflink=always` on Linux btrfs/XFS. `detect_backend()` probes the host once (cached) and falls back to `Snapshot` on failure, so isolation is never compromised — only the speed/space optimization.
2. **Account exhaustion** — *Resolved:* cap N. `RotatingBranchExecutor` caps the branch count to `AccountProvider::account_count()` so parallel branches get distinct accounts, and the reduction is `log()`ed ("no silent caps").
3. **Judge cost** — *Resolved:* the judge is decoupled behind the injected `LlmCaller`, so the gateway wires the Confidence Router (local-first, escalate to Claude on low confidence). A deterministic `HeuristicJudge` (zero-LLM) is the fail-safe fallback.
```
