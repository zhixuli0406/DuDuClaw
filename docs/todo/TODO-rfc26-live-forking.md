# TODO: RFC-26 Live Run Forking — Fine-grained Work Items

> Companion to [RFC-26](./RFC-26-deep-agents-alignment.md). Each item is sized to be a
> single focused commit. Checkboxes track progress. `[x]` = done, `[ ]` = pending.
> Convention: default-off, fail-closed, no silent caps, `truncate_bytes` / `with_file_lock`
> on all snippets & appends.

Legend: **(test)** = item must ship with unit test(s). **(wire)** = integration/plumbing.

---

## ✅ Round 2 (2026-06-19): follow-ups completed

The cross-process + parity follow-ups were implemented. Summary of what landed:

- **Shared SQLite fork store** (`duduclaw-fork/src/store.rs`, `ForkStore`, 8 tests) — WAL DB at
  `<home>/fork_store.db`, the cross-process source of truth. `mcp_fork` + `mcp_fork_exec` were
  refactored off the in-process registry onto it.
- **P5.1 gateway `/metrics`** — `metrics::render_fork_metrics_from` reads the store and emits
  `duduclaw_fork_*` Prometheus lines (2 tests).
- **P5.3 dashboard** — `fork.list/inspect/resolve` WebSocket RPC (`handlers.rs`) + `web/src/pages/ForkPage.tsx`
  (list, side-by-side branches, judge winner, manual resolve) + nav entry + `nav.forks` i18n (zh/en/ja).
- **P6.4 `memory_improve`** MCP tool — clusters memories by tag into a propose-not-apply scaffold (2 tests).
- **P4 distinct-account cap** — branches capped to available account count with logging; `DUDUCLAW_FORK_NO_EXEC`
  test/CI guard. **Cancellation registry** — `terminate_branch` cancels a not-yet-started branch (2 tests).
- **P6.1 Plan Mode** — `plan_start` clarify-first tool + `[planner]` config (7 tests).
- **P6.5 Task Board** — `introduces_parent_cycle` cycle detection + `claim_task` CAS + `would_create_parent_cycle`
  store method (5 tests).
- **P6.3 built-in skills** — `builtin_skills` bundles code-review/refactor/test-writer/git-workflow, seeded
  idempotently into every new agent's `SKILLS/` at creation (3 tests).
- **Checkpoint SQLite** — `CheckpointManager::with_persistence` durable backend; fork/rewind/lineage survive
  restart (3 tests). `new()` stays pure in-memory (unchanged).

## ✅ Round 3 (2026-06-19): the last 4 deferred items completed

- **Native CoW overlay** — `overlay.rs` now has a real `NativeCow` backend: `clonefile(2)` via `cp -c`
  on macOS/APFS, `cp --reflink` on Linux. `detect_backend()` probes the host once (cached) and falls back
  to `Snapshot` if a clone fails — isolation is never compromised, only the speed/space optimization.
  Verified the native clone+promote path on this APFS host (3 overlay tests + probe).
- **Streaming budget + external SIGKILL** — `ClaudeCliSpawner` now streams stream-json line-by-line,
  charges `total_cost_usd` as it grows, and `start_kill()`s the child the moment running cost crosses the
  per-branch budget (`SpawnOutcome::BudgetExceeded`). A per-branch kill-switch registry (`register_kill`
  / `request_cancel` fires `Notify::notify_waiters`) lets `terminate_branch` SIGKILL an *in-flight*
  subprocess mid-stream (`SpawnOutcome::Cancelled` → `Terminated`). `run_branch` maps the 4 outcomes.
- **Activity-Feed mirroring** — `append_fork_activity` inserts a `fork_resolved` row into the gateway's
  cross-process `activity` table (`<home>/tasks.db`, idempotent schema guard) so resolutions appear on the
  dashboard Activity Feed. Called from `execute_fork` on resolution.

All four ship with unit tests; only an *already-running subprocess across a different process* (the gateway
killing an MCP-server child) remains out of scope by design — `terminate_branch` runs in the same MCP-server
process that owns the child, which is where kill must originate.

---

## Phase 1 — `duduclaw-fork` crate MVP ✅ DONE

### 1.1 Crate scaffold
- [x] Create `crates/duduclaw-fork/Cargo.toml` (deps: tokio, async-trait, serde, serde_json, thiserror, tracing, uuid, tempfile)
- [x] Register `crates/duduclaw-fork` in workspace `Cargo.toml` members
- [x] `src/error.rs` — `ForkError` enum (`Overlay`/`Executor`/`Config`/`NotFound`) + `Result<T>` alias

### 1.2 Branch domain types (`src/branch.rs`)
- [x] `BranchId` (uuid v4 newtype) + `Display` + `Default`
- [x] `BranchState` enum (`Pending`/`Running`/`Finished`/`BudgetKilled`/`Terminated`/`Failed`)
- [x] `BranchState::is_judgeable()` — only `Finished` (test)
- [x] `BranchState::is_terminal()` (test)
- [x] `BranchSpec { steering, budget_usd }`
- [x] `Branch { id, spec, state, spent_usd }` + `Branch::new`
- [x] `BranchResult { id, state, output, spent_usd, test_exit_code }`
- [x] `BranchResult::test_passed()` — `None` neutral when unconfigured (test)

### 1.3 Budget pool (`src/budget.rs`)
- [x] `Charge` enum (`Allowed`/`BranchExceeded`/`AggregateExceeded`)
- [x] `Pool::new(aggregate_cap)` + `register(id, branch_cap)`
- [x] `Pool::try_charge` — per-branch + aggregate enforcement, fail-closed (no commit on reject) (test)
- [x] `aggregate_spent()` / `branch_spent()` accessors (test)
- [x] Unregistered branch ⇒ zero cap ⇒ charge denied (test)

### 1.4 CoW overlay (`src/overlay.rs`)
- [x] `BranchOverlay::create(parent)` — reject non-dir parent (fail-closed) (test)
- [x] `workspace()` / `parent()` accessors
- [x] `copy_tree` recursive snapshot (MVP backend)
- [x] `promote()` — merge branch writes back to parent (additive) (test)
- [x] writes stay local until promote (test); nested dirs copied (test)

### 1.5 Controller (`src/lib.rs`)
- [x] `MergeMode` enum w/ `#[default] AutoWithFallback`
- [x] `ForkConfig` + `validate()` (fail-closed on bad values) (test)
- [x] `BranchInvocation` struct
- [x] `BranchExecutor` async trait (decouples CLI runner)
- [x] `ForkController::new` (validates config)
- [x] `effective_branch_count()` — cap to min(req, max_branches, accounts), never 0 (test)
- [x] `ForkController::run` — spawn branches concurrently, per-branch overlay + budget register, collect results (test)
- [x] clippy clean + 17 tests green

---

## Phase 2 — Judge + test runner + merge resolution ✅ DONE

### 2.1 Judge verdict types (`src/judge.rs`)
- [x] `JudgeVerdict { winner: BranchId, confidence: f64, per_branch_scores: Vec<(BranchId, f64)>, rationale: String }`
- [x] `JudgeScores { quality_spread: f64, test_pass_ratio: f64, internal_consistency: f64 }` (each 0.0–1.0)
- [x] `JudgeScores::confidence()` = `quality_spread*0.4 + test_pass_ratio*0.4 + internal_consistency*0.2` (test: boundary 0/1, weight sum)
- [x] Clamp/validate each sub-score to [0,1]; out-of-range ⇒ clamp + warn (test); NaN ⇒ 0.0

### 2.2 Judge agent abstraction
- [x] `JudgeAgent` trait: `async fn judge(&self, task: &str, results: &[BranchResult]) -> Result<JudgeVerdict>`
- [x] `LlmJudge<C: LlmCaller>` impl — multi-candidate XML-delimited prompt (`build_judge_prompt`), candidate-tag escaping (injection-resistant, test)
- [x] Parse judge response → `JudgeVerdict` (`parse_judge_verdict`, JSON-first + fence strip); unparseable / OOB index ⇒ `Err` (fail-closed, test)
- [x] `test_pass_ratio` computed from `BranchResult::test_passed()` across judgeable branches; neutral 0.5 when none tested (test)
- [x] `internal_consistency` heuristic: deterministic L1/L2 check (empty/error-marker/dangling) before LLM quality pass (test)
- [x] Cost control (RFC-26 §6 Q3): backend selection deferred to injected `LlmCaller` (gateway wires Confidence Router local-first) **(wire — P4)**
- [x] Non-judgeable branches (`Failed`/`Terminated`/`BudgetKilled`) excluded via `judgeable()` (test)
- [x] Zero judgeable branches ⇒ `Err` → caller surfaces to operator (test)
- [x] `HeuristicJudge` — deterministic zero-LLM fallback judge (test)

### 2.3 Test runner (`src/test_runner.rs`)
- [x] `run_test(workspace, command, timeout_s) -> Result<Option<TestOutcome>>` (`{ exit_code, stdout_tail, stderr_tail, timed_out }`)
- [x] Run `test_command` against branch snapshot dir (cwd = overlay workspace) (test)
- [x] Timeout kill at `test_timeout_s` → `timed_out=true`, exit_code = `TIMEOUT_EXIT_CODE` (124) (test); `kill_on_drop`
- [x] Empty/whitespace `test_command` ⇒ `Ok(None)` skip, `test_exit_code = None` (neutral in judge) (test)
- [x] `truncate_bytes` on stdout/stderr tails (CJK-safe)
- [x] Wire `TestOutcome.exit_code` into `BranchResult.test_exit_code` in `run_and_resolve` **(wire)**

### 2.4 Merge resolution (`src/merge.rs`)
- [x] `resolve(verdict, mode, threshold) -> MergeDecision { winner: Option<BranchId>, needs_confirmation: bool, reason }`
- [x] `MergeMode::Auto` — pick verdict winner, `needs_confirmation=false` (test)
- [x] `MergeMode::AutoWithFallback` — pick winner, `needs_confirmation=true` (test)
- [x] `MergeMode::Manual` — `winner=None`, always defer (test)
- [x] `MergeMode::Vote` — `resolve_vote` majority across N verdicts; tie ⇒ defer; low mean-confidence ⇒ defer (test)
- [x] Below-confidence-threshold winner ⇒ defer regardless of mode (`DEFAULT_CONFIDENCE_THRESHOLD`) (test)
- [x] `ForkController::run_and_resolve` — run → test → judge (Vote samples `VOTE_ROUNDS=3`) → merge → `promote()` winner overlay when final; `ForkResolution` returned (test with `HeuristicJudge`)

---

## Phase 3 — MCP tools + scope ✅ DONE

### 3.1 Scope (`crates/duduclaw-cli/src/mcp_auth.rs`)
- [x] Add `Scope::ForkExecute` to the scope enum (+ Display `"fork:execute"`)
- [x] `parse_scopes` accepts `"fork:execute"`
- [x] Enumerate all 6 fork tools in `tool_requires_scope` → `Scope::ForkExecute` (unknown ⇒ Admin fail-closed preserved)
- [x] Gate at **handler entry** via `require_enabled()` (fail-closed deny when `[fork] enabled = false`) — chosen over registration-filtering so the deny is uniform + testable (test `disabled_agent_is_gated`)

### 3.2 Tool definitions (`crates/duduclaw-cli/src/mcp.rs` `TOOLS` table)
- [x] `fork_run` — params: `prompt`, `n?`, `strategies[]?`, `budget_usd?`, `merge_mode?`
- [x] `inspect_branches` — param: `fork_id`
- [x] `diff_branches` — params: `fork_id`, `branch_a`, `branch_b`
- [x] `merge_or_select` — params: `fork_id`, `branch_id?`
- [x] `terminate_branch` — params: `fork_id`, `branch_id`
- [x] `fork_cost` — param: `fork_id`
- [x] `fork_run`/`merge_or_select`/`terminate_branch` added to `is_state_changing` (audit trail)

### 3.3 Dispatch handlers (`crates/duduclaw-cli/src/mcp_fork.rs` + dispatch arms in `mcp.rs`)
- [x] In-process `ForkRegistry` (`OnceLock<Mutex<HashMap<String, ForkRecord>>>`, global per-process)
- [x] `handle_fork_run` — validate params, load `[fork]` settings, cap to `max_branches`, register `ForkRecord`, return `fork_id` + branch ids (exec backend status `pending_execution_backend` → P4)
- [x] `handle_inspect_branches` — serialize branch states + spend + test result
- [x] `handle_diff_branches` — outputs side-by-side (`truncate_bytes` 8 KB each)
- [x] `handle_merge_or_select` — explicit `branch_id` selects + marks resolved; absent ⇒ deferred to judge (P4)
- [x] `handle_terminate_branch` — mark `Terminated` (subprocess kill wired in P4)
- [x] `handle_fork_cost` — aggregate + per-branch spend
- [x] Param validation at boundary: bad `n` (<2)/missing `prompt`/unknown `fork_id`/unknown branch ⇒ structured error (tests)
- [x] `truncate_bytes` on branch output echoed through `diff_branches` (test)
- [x] 6 dispatch arms wired in `mcp.rs` before the `odoo_` catch-all

### 3.4 Config loading (`mcp_fork::parse_fork_settings` / `load_fork_settings`)
- [x] Parse `agent.toml [fork]` → `ForkSettings` (enabled, max_branches, default/aggregate budget, merge_mode, test_command, test_timeout_s)
- [x] Missing/malformed `[fork]` ⇒ disabled fail-safe, no panic (tests)
- [x] Invalid sub-values (max_branches=0, negative budget) fall back to defaults (test)
- [x] `parse_merge_mode` string → `MergeMode`; unknown ⇒ `AutoWithFallback` + warn (test)

> **P3 → P4 handoff:** the tool surface, scope, gating, registry, and config are complete and tested
> (17 `mcp_fork` tests + scope wiring). The remaining behaviors — real branch execution, judge
> auto-selection, winner `promote()`, and subprocess kill on `terminate_branch` — are P4, which swaps
> the `pending_execution_backend` stub for the `RotatingBranchExecutor`.

---

## Phase 4 — Real executor, parallelism, native CoW ✅ DONE (core + all deep-integration follow-ups landed in Round 2/3)

### 4.1 `BranchExecutor` impl (`crates/duduclaw-cli/src/mcp_fork_exec.rs`)
- [x] `RotatingBranchExecutor<P, S>` implements `duduclaw_fork::BranchExecutor`
- [x] `AccountProvider` trait + `RotatorProvider` (wraps `AccountRotator`) + `build_rotator_provider` (loads accounts, `None` when zero)
- [x] `CliSpawner` trait + `ClaudeCliSpawner` (real `claude -p --output-format stream-json` in branch workspace, env from account)
- [x] Inject branch workspace as cwd + steering folded into the prompt (test `happy_path_finishes_and_charges`)
- [x] Map stream-json result → `BranchResult` via `parse_stream_json` (final `result` text + `total_cost_usd`) (test)
- [x] Per-branch + aggregate budget via `Pool::try_charge`; over-cap ⇒ `BudgetKilled` (test `per_branch_budget_exceeded_is_budget_killed`)
- [x] Fallback: no account / spawn failure ⇒ `BranchState::Failed`, excluded from judging (tests `no_account_fails_branch`, `spawner_failure_marks_failed`)
- [x] Background execution: `fork_run` spawns `execute_fork` via `tokio::spawn`, returns `status:"running"` without blocking the MCP stdio loop
- [x] `execute_fork` transitions `Pending→Running→…`, folds results + winner into `ForkRegistry`, runs full `ForkController` pipeline
- [x] **Round 2**: cap N to available-account count via `AccountProvider::account_count()` so parallel branches get distinct accounts (RFC-26 §4.1); reduction is `log()`ed, never silent

### 4.2 Aggregate budget wired to live spend
- [x] Executor charges its `Pool` from the branch's actual `spent_usd` (single-shot cost), aggregate enforced across concurrent branches (test)
- [x] `ForkResolution.aggregate_spent_usd` summed from real branch spends
- [x] **Round 3**: `ClaudeCliSpawner` streams stream-json line-by-line, accumulates `total_cost_usd` as it grows, and `start_kill()`s the child the moment running cost crosses the per-branch budget (`SpawnOutcome::BudgetExceeded` → `BranchState::BudgetKilled`).
- [x] **Round 4**: cross-branch **aggregate pre-emption** — `duduclaw_fork::LiveAggregate` (a streaming-time companion to `Pool`, shared across the fork's concurrent branches via the executor) tracks every in-flight branch's live `total_cost_usd`. On each cost update the spawner calls the pure `stream_budget_decision`, which folds in `LiveAggregate::observe`: when the combined live spend crosses the aggregate cap it names the **most-expensive in-flight branch** (deterministic tie-break by id). If that victim is the observer it self-kills; otherwise it `request_budget_kill`s the sibling, firing the same per-branch kill switch but tagged so the woken branch maps to `BudgetExceeded` (→ `BudgetKilled`), not `Cancelled`. `LiveAggregate::finish` frees a branch's budget for survivors once it ends. Tests: `LiveAggregate` (5, in `duduclaw-fork`) + `stream_budget_decision`/`budget_kill` disambiguation (5, in `mcp_fork_exec`).

### 4.3 Native copy-on-write overlay backend (`duduclaw-fork/src/overlay.rs`)
- [x] `OverlayBackend` enum (`Snapshot` / `NativeCow`) + `detect_backend()` (fail-safe → `Snapshot`) (test `detect_backend_is_failsafe_snapshot`)
- [x] `SnapshotOverlay` = MVP `copy_tree` backend (current `BranchOverlay`)
- [x] **Round 3**: `NativeCow` backend — `clonefile(2)` via `cp -c` on macOS/APFS, `cp --reflink=always` on Linux btrfs/XFS; `detect_backend()` probes the host once (cached) and falls back to `Snapshot` on failure (isolation never compromised, only speed/space). Verified the native clone+promote path on this APFS host (overlay tests + probe)

### 4.4 Cancellation & cleanup
- [x] `terminate_branch` marks the branch `Terminated` in the registry (excluded from judging)
- [x] `ClaudeCliSpawner` uses `kill_on_drop(true)` — branch subprocess dies when its task is dropped/cancelled
- [x] **Round 3**: external SIGKILL of an *in-flight* branch subprocess — a per-branch kill-switch registry (`register_kill` / `request_cancel` fires `Notify::notify_waiters`) lets `terminate_branch` (`mcp_fork.rs` → `request_cancel`) interrupt the streaming `select!`, which `start_kill()`s the child mid-stream (`SpawnOutcome::Cancelled` → `Terminated`). `kill_on_drop(true)` covers task-drop/shutdown. (Orphan overlay temp-dir startup sweeper remains a minor housekeeping nicety; `tempfile` RAII already removes overlays on normal drop.)

> **P4 status (final):** the executor, budgeting, account provider, real claude spawner, stream-json
> parsing, and background-execution lifecycle are implemented and unit-tested. Every deep integration
> originally tracked as a follow-up has landed — distinct-account cap, streaming per-branch budget kill,
> **cross-branch aggregate pre-emption** (Round 4), native CoW (`clonefile`/reflink), and external
> SIGKILL of an in-flight branch. No residual items remain; the only by-design exclusion is killing a
> child across a *different* process (the gateway can't reach an MCP-server child — `terminate_branch`
> runs in the process that owns the child).

---

## Phase 5 — Observability & dashboard ✅ DONE (data layer + gateway `/metrics`, Activity Feed, and dashboard `ForkPage` all landed in Round 2/3)

### 5.1 Metrics (`mcp_fork_exec::ForkMetrics` / `FORK_METRICS`)
- [x] `fork_runs_total` counter
- [x] `fork_branches_total` counter
- [x] per-outcome counters: `fork_branches_finished/budget_killed/failed_total` + `branch_outcome_label()`
- [x] `fork_promoted_total` counter
- [x] `FORK_METRICS.snapshot()` JSON + `record_resolution()` from `execute_fork` (test `metrics_record_resolution_counts`)
- [x] **Round 2**: surfaced on the gateway `/metrics` endpoint via `metrics::render_fork_metrics_from(&home.join("fork_store.db"))` — the gateway reads the cross-process shared `ForkStore` (SQLite WAL) at scrape time and emits `duduclaw_fork_runs_total` / `_resolved_total` / `_promoted_total` / `_branches_total` + `duduclaw_fork_branch_outcome{outcome=...}` (2 tests). The shared store replaced the `fork_history.jsonl`-scrape approach.

### 5.2 History log (`mcp_fork_exec::append_fork_history`)
- [x] Append fork resolution to `<home>/fork_history.jsonl` via `with_file_lock` (cross-process safe; test `append_fork_history_writes_jsonl_line`)
- [x] Record: ts, fork_id, branches, merge_mode, winner, promoted, aggregate_spent_usd, per-branch outcomes (`ForkHistoryEntry`)
- [x] **Round 3**: `append_fork_activity` inserts a `fork_resolved` row into the gateway's cross-process `activity` table (`<home>/tasks.db`, idempotent schema guard), called from `execute_fork` on resolution, so resolutions appear on the dashboard Activity Feed (test `append_fork_activity_inserts_row`)

### 5.3 Dashboard fork visualization (`web/`) — follow-up
- [x] **Round 2**: `web/src/pages/ForkPage.tsx` (list recent forks, side-by-side branch view, judge winner, manual resolve) wired to dashboard RPC `fork.list` / `fork.inspect` / `fork.resolve` (`handlers.rs` → `handle_fork_list/inspect/resolve`, `fork.resolve` behind `require_manager!`), `/forks` route + nav entry + `nav.forks` i18n (zh/en/ja). Unblocked by the shared SQLite `ForkStore`. `npx tsc --noEmit` clean.

---

## Phase 6 — Secondary parity items (RFC-26 §4, independent) ✅ DONE

> These are **independent** of the core forking feature (P1–P5) and each touches a
> different existing subsystem. All five (6.1 Plan Mode, 6.2 Checkpoint fork/rewind +
> durable SQLite, 6.3 Built-in skills, 6.4 Memory `/improve`, 6.5 Task Board claim +
> cycle detection) landed across Round 2/3, each with unit tests.

### 6.2 Checkpoint fork/rewind (`crates/duduclaw-durability/src/checkpoint.rs`) ✅ DONE
- [x] `fork(checkpoint_id, new_task_id) -> Checkpoint` (copy state under new lineage) (test)
- [x] `rewind(task_id, checkpoint_id)` (restore earlier snapshot as current) (test)
- [x] Lineage tracking — `Checkpoint.parent_checkpoint_id` (test)
- [x] `get_by_id` + id-addressable `archive` (bounded at 2× `max_checkpoints`)
- [x] **Round 2**: durable SQLite backend — `CheckpointManager::with_persistence(config, &db_path)` opens a `rusqlite` connection, creates the `checkpoints` table (with `parent_checkpoint_id` lineage column), and reloads on construction so fork/rewind/lineage survive restart (test persists then reopens). `new()` stays pure in-memory (unchanged).

### 6.1 Plan Mode (clarify-first planner) ✅ DONE
- [x] **Round 2**: `plan_start` MCP tool (`mcp_planner.rs`) + `[planner]` config — clarify-first flow emits ≤3 clarifying questions then decomposes into `tasks_create` steps wiring `depends_on` for ordered steps (7 tests).

### 6.3 Built-in skill set parity ✅ DONE
- [x] **Round 2**: `builtin_skills` bundles `code-review` / `refactor` / `test-writer` / `git-workflow` SKILL.md, seeded idempotently into every new agent's `SKILLS/` at creation (3 tests).

### 6.4 Memory `/improve` ✅ DONE
- [x] **Round 2**: `memory_improve` MCP tool (`mcp_memory_handlers::handle_memory_improve`, tool def in `mcp.rs`, gated in `mcp_auth.rs`) — clusters memories by tag into a propose-not-apply reflection scaffold (does not auto-write) (2 tests).

### 6.5 Task Board team coordination ✅ DONE
- [x] **Round 2**: atomic `claim_task` (compare-and-set on `assignee`) + write-time dependency cycle detection — `introduces_parent_cycle` (pure graph walk, treats pre-existing cycles as unsafe) + `would_create_parent_cycle` store method, both in `gateway/src/task_store.rs` (5 tests: self/direct/deep back-edge + safe cases).

---

## Cross-cutting / Definition of Done
- [x] Every new public fn has unit test(s) — 90 tests across `duduclaw-fork` (49), cli `mcp_fork`+`mcp_fork_exec` (26), durability checkpoint (15)
- [x] `duduclaw-fork`, cli `mcp_fork`/`mcp_fork_exec`, durability `checkpoint` all clippy-clean
- [x] No raw byte slicing; `truncate_bytes` on judge rationale, diff output, test tails, branch output
- [x] No unanchored `contains`/`starts_with` for routing/security (merge-mode/scope use exact match)
- [x] Shared-file append (`fork_history.jsonl`) via `with_file_lock`
- [x] Security gates fail closed: disabled agent denied, no judgeable branch ⇒ Err, sub-threshold/Manual ⇒ defer (never auto-promote), unknown tool ⇒ Admin scope
- [x] `agent.toml [fork] enabled = false` default verified — disabled-agent gate test; zero behavior change when off
- [x] Smoke script `scripts/smoke-fork.{sh,ps1}` (build + fork crate tests + cli fork surface + checkpoint + fork clippy)
- [x] CHANGELOG entry (`[Unreleased]` — RFC-26)
- [x] **Done**: README feature blurb shipped in all three locales (`README.md` zh-TW, `README.en.md`, `README.ja.md`) now that the user-facing path (dashboard `ForkPage` + cross-process `ForkStore` + gateway `/metrics`) has landed.
