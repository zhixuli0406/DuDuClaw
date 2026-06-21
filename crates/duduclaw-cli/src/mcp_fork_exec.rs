//! RFC-26 P4: real branch execution for Live Run Forking.
//!
//! [`RotatingBranchExecutor`] implements `duduclaw_fork::BranchExecutor` by selecting
//! a distinct account per branch (via an [`AccountProvider`], normally the
//! `AccountRotator`) and spawning one agent run through a [`CliSpawner`]. Aggregate
//! and per-branch budgets are enforced with `duduclaw_fork::budget::Pool`.
//!
//! Execution runs in the **background**: `fork_run` returns immediately and a tokio
//! task drives the branches → judge → merge, updating the shared `ForkStore`. This
//! avoids blocking the MCP stdio loop (the calling agent is itself a `claude`
//! process waiting on the tool response) for the minutes a fork can take.
//!
//! The orchestration core (account distribution, budgeting, state transitions,
//! judge + merge, registry updates) is unit-tested with fakes. The production
//! [`ClaudeCliSpawner`] is a thin `claude` subprocess wrapper layered on top.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use duduclaw_fork::branch::BranchState;
use duduclaw_fork::{
    BranchExecutor, BranchInvocation, BranchResult, Charge, ForkConfig, ForkController, JudgeAgent,
    Pool, Result as ForkResult,
};

use crate::mcp_fork::{parse_merge_mode, ForkSettings};

// ── Account selection abstraction ───────────────────────────────────────────

/// Minimal env handed to a spawned branch run.
#[derive(Debug, Clone, Default)]
pub struct SelectedAccount {
    pub id: String,
    pub env_vars: HashMap<String, String>,
}

/// Abstracts "pick an account to run a branch with". Implemented by the real
/// `AccountRotator`; faked in tests.
#[async_trait]
pub trait AccountProvider: Send + Sync {
    async fn select(&self) -> Option<SelectedAccount>;
    /// Report a branch's outcome so the rotator can update health / spend.
    async fn report(&self, account_id: &str, ok: bool, cost_cents: u64);
    /// Number of accounts available to distribute across branches. Used to cap the
    /// branch count so parallel branches get distinct accounts (RFC-26 §4.1).
    /// Defaults to "unbounded" for fakes/tests.
    async fn account_count(&self) -> usize {
        usize::MAX
    }
}

// ── CLI spawning abstraction ────────────────────────────────────────────────

/// How a branch's spawned run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    /// Ran to completion successfully.
    Completed,
    /// Killed mid-stream for budget — either this branch's own per-branch cap, or
    /// cross-branch aggregate pre-emption (it was the most-expensive in-flight
    /// branch when the combined live spend crossed the aggregate cap).
    BudgetExceeded,
    /// Killed by an external `terminate_branch` request.
    Cancelled,
    /// Spawn or execution error.
    Failed,
}

/// Result of spawning one agent run for a branch.
#[derive(Debug, Clone)]
pub struct CliRunOutput {
    pub output: String,
    pub spent_usd: f64,
    pub outcome: SpawnOutcome,
}

impl CliRunOutput {
    pub fn ok(&self) -> bool {
        self.outcome == SpawnOutcome::Completed
    }
}

/// Context for one branch spawn: identity + budget so the spawner can stream cost
/// and self-kill on per-branch overspend, cross-branch aggregate pre-emption, or
/// external cancellation.
#[derive(Debug, Clone)]
pub struct SpawnCtx {
    pub branch_id: String,
    pub budget_usd: f64,
    /// Shared cross-branch live-aggregate tracker (RFC-26 §4.2). `None` ⇒ only the
    /// per-branch streaming budget is enforced (single-branch / test paths).
    pub aggregate: Option<Arc<duduclaw_fork::LiveAggregate>>,
}

#[async_trait]
pub trait CliSpawner: Send + Sync {
    async fn spawn(
        &self,
        ctx: &SpawnCtx,
        prompt: &str,
        workspace: &Path,
        env: &HashMap<String, String>,
    ) -> CliRunOutput;
}

// ── Cancellation registry (RFC-26 §4.4) ─────────────────────────────────────

/// Process-global set of branch ids the operator asked to terminate. The executor
/// checks it before spawning each branch so a cancel issued before/at start skips
/// the run.
fn cancel_set() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Per-running-branch kill switches. A spawned `claude` subprocess registers a
/// `Notify`; `terminate_branch` fires it to SIGKILL the *in-flight* subprocess
/// mid-stream (not just the pre-spawn flag).
type KillMap = std::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Notify>>>;
fn kill_registry() -> &'static KillMap {
    static MAP: std::sync::OnceLock<KillMap> = std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Register (or fetch) a kill switch for a running branch.
pub fn register_kill(branch_id: &str) -> std::sync::Arc<tokio::sync::Notify> {
    let mut map = kill_registry().lock().expect("kill registry poisoned");
    map.entry(branch_id.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Notify::new()))
        .clone()
}

/// Drop a branch's kill switch once it has finished.
pub fn unregister_kill(branch_id: &str) {
    kill_registry().lock().expect("kill registry poisoned").remove(branch_id);
}

/// Request cancellation of a branch by id: sets the pre-spawn flag AND fires the
/// kill switch if the branch is already running.
pub fn request_cancel(branch_id: &str) {
    cancel_set().lock().expect("cancel set poisoned").insert(branch_id.to_string());
    if let Some(notify) = kill_registry().lock().expect("kill registry poisoned").get(branch_id) {
        notify.notify_waiters();
    }
}

/// Whether a branch was asked to cancel.
pub fn is_cancelled(branch_id: &str) -> bool {
    cancel_set().lock().expect("cancel set poisoned").contains(branch_id)
}

// ── Aggregate budget pre-emption (RFC-26 §4.2) ──────────────────────────────

/// Branches the cross-branch aggregate pre-emptor asked to kill *for budget*
/// (as opposed to an operator `terminate_branch`). The two share one per-branch
/// `Notify` kill switch, so this set is how the spawner tells *why* it was woken:
/// a tagged branch maps to `BudgetExceeded` (→ `BudgetKilled`), an untagged one
/// to `Cancelled` (→ `Terminated`).
fn budget_kill_set() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Pre-empt a *running* branch for aggregate budget: tag the reason, then fire
/// its kill switch so the in-flight subprocess is SIGKILLed mid-stream.
pub fn request_budget_kill(branch_id: &str) {
    budget_kill_set().lock().expect("budget kill set poisoned").insert(branch_id.to_string());
    if let Some(notify) = kill_registry().lock().expect("kill registry poisoned").get(branch_id) {
        notify.notify_waiters();
    }
}

/// Consume a branch's budget-kill tag, returning whether it was set. Removing it
/// keeps the process-global set from growing and resets state for branch-id reuse.
fn took_budget_kill(branch_id: &str) -> bool {
    budget_kill_set().lock().expect("budget kill set poisoned").remove(branch_id)
}

/// Pure streaming-budget decision for one cost update (RFC-26 §4.2). Factored out
/// so the real `claude` spawner and the unit tests exercise identical logic:
/// per-branch cap first, then cross-branch aggregate pre-emption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamAction {
    /// Within budget — keep streaming.
    Continue,
    /// This branch must die now (its own per-branch cap, or it is itself the
    /// most-expensive branch over the aggregate cap).
    SelfKill,
    /// A *different* branch is the most-expensive over-aggregate one; ask the
    /// registry to kill it, then keep streaming this branch.
    PreemptOther(String),
}

/// Decide what a branch should do given its latest cumulative `running_cost`.
pub fn stream_budget_decision(ctx: &SpawnCtx, running_cost: f64) -> StreamAction {
    if running_cost > ctx.budget_usd {
        return StreamAction::SelfKill;
    }
    match &ctx.aggregate {
        None => StreamAction::Continue,
        Some(agg) => match agg.observe(&ctx.branch_id, running_cost) {
            duduclaw_fork::Preempt::Ok => StreamAction::Continue,
            duduclaw_fork::Preempt::Kill(v) if v == ctx.branch_id => StreamAction::SelfKill,
            duduclaw_fork::Preempt::Kill(v) => StreamAction::PreemptOther(v),
        },
    }
}

// ── Executor ────────────────────────────────────────────────────────────────

pub struct RotatingBranchExecutor<P: AccountProvider, S: CliSpawner> {
    provider: Arc<P>,
    spawner: Arc<S>,
    pool: Arc<Pool>,
    /// Streaming-time aggregate tracker shared across this fork's concurrent
    /// branches, enabling cross-branch pre-emption (RFC-26 §4.2). Complements
    /// `pool`'s post-completion fail-closed accounting.
    aggregate: Arc<duduclaw_fork::LiveAggregate>,
}

impl<P: AccountProvider, S: CliSpawner> RotatingBranchExecutor<P, S> {
    pub fn new(provider: Arc<P>, spawner: Arc<S>, aggregate_budget_usd: f64) -> Self {
        let cap = aggregate_budget_usd.max(0.0);
        RotatingBranchExecutor {
            provider,
            spawner,
            pool: Arc::new(Pool::new(cap)),
            aggregate: Arc::new(duduclaw_fork::LiveAggregate::new(cap)),
        }
    }
}

#[async_trait]
impl<P: AccountProvider, S: CliSpawner> BranchExecutor for RotatingBranchExecutor<P, S> {
    async fn run_branch(&self, inv: BranchInvocation) -> ForkResult<BranchResult> {
        self.pool.register(inv.branch_id.clone(), inv.budget_usd);

        // Honor a cancellation requested before the branch started.
        if is_cancelled(&inv.branch_id.0) {
            return Ok(BranchResult {
                id: inv.branch_id,
                state: BranchState::Terminated,
                output: "branch terminated before start".into(),
                spent_usd: 0.0,
                test_exit_code: None,
            });
        }

        let account = match self.provider.select().await {
            Some(a) => a,
            None => {
                // No account available ⇒ branch fails (excluded from judging).
                return Ok(BranchResult {
                    id: inv.branch_id,
                    state: BranchState::Failed,
                    output: "no account available for this branch".into(),
                    spent_usd: 0.0,
                    test_exit_code: None,
                });
            }
        };

        let full_prompt = match &inv.steering {
            Some(s) if !s.trim().is_empty() => format!("{}\n\n## Strategy for this branch\n{}", inv.prompt, s),
            _ => inv.prompt.clone(),
        };

        // Register a kill switch so an external terminate_branch — or a
        // cross-branch aggregate pre-emption — can SIGKILL the in-flight
        // subprocess mid-stream.
        let _kill = register_kill(&inv.branch_id.0);
        let ctx = SpawnCtx {
            branch_id: inv.branch_id.0.clone(),
            budget_usd: inv.budget_usd,
            aggregate: Some(self.aggregate.clone()),
        };
        let out = self
            .spawner
            .spawn(&ctx, &full_prompt, &inv.workspace, &account.env_vars)
            .await;
        // Stop counting this branch toward the live aggregate so its siblings'
        // combined spend drops once it has finished or been killed.
        self.aggregate.finish(&inv.branch_id.0);
        unregister_kill(&inv.branch_id.0);

        // Charge the aggregate pool for whatever was spent.
        let charge = self.pool.try_charge(&inv.branch_id, out.spent_usd);
        let state = match out.outcome {
            SpawnOutcome::Failed => BranchState::Failed,
            SpawnOutcome::Cancelled => BranchState::Terminated,
            SpawnOutcome::BudgetExceeded => BranchState::BudgetKilled,
            SpawnOutcome::Completed => match charge {
                // Completed but pushed the aggregate over its cap ⇒ exclude (fail-closed).
                Charge::BranchExceeded | Charge::AggregateExceeded => BranchState::BudgetKilled,
                Charge::Allowed => BranchState::Finished,
            },
        };

        let cost_cents = (out.spent_usd * 100.0).round().max(0.0) as u64;
        self.provider.report(&account.id, out.ok(), cost_cents).await;

        Ok(BranchResult {
            id: inv.branch_id,
            state,
            output: out.output,
            spent_usd: out.spent_usd,
            test_exit_code: None,
        })
    }
}

// ── Background driver ───────────────────────────────────────────────────────

/// Build a [`ForkConfig`] from an agent's `[fork]` settings.
fn fork_config(settings: &ForkSettings) -> ForkConfig {
    ForkConfig {
        max_branches: settings.max_branches,
        default_budget_usd: settings.default_budget_usd,
        aggregate_budget_usd: settings.aggregate_budget_usd.max(settings.default_budget_usd),
        merge_mode: parse_merge_mode(&settings.merge_mode),
        test_command: settings.test_command.clone(),
        test_timeout_s: settings.test_timeout_s,
    }
}

/// Run a fork to completion in the background and write results into the registry.
///
/// Transitions every branch `Pending → Running` up front, then runs the full
/// `ForkController` pipeline and folds the per-branch results + winner back into
/// the `ForkRecord`.
/// Inputs describing a fork to execute (separated from the generic backends so
/// `execute_fork` stays within a sane argument count).
pub struct ForkExecRequest {
    pub fork_id: String,
    pub prompt: String,
    pub branches: Vec<duduclaw_fork::Branch>,
    pub parent_workspace: std::path::PathBuf,
    pub settings: ForkSettings,
    /// DuDuClaw home (`~/.duduclaw`) — where `fork_history.jsonl` is appended.
    pub home_dir: std::path::PathBuf,
}

pub async fn execute_fork<P, S, J>(
    req: ForkExecRequest,
    provider: Arc<P>,
    spawner: Arc<S>,
    judge: Arc<J>,
) where
    P: AccountProvider + 'static,
    S: CliSpawner + 'static,
    J: JudgeAgent + 'static,
{
    let ForkExecRequest { fork_id, prompt, branches, parent_workspace, settings, home_dir } = req;
    let branch_count = branches.len();
    let store = match duduclaw_fork::ForkStore::open(crate::mcp_fork::fork_store_path(&home_dir)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("fork {fork_id}: cannot open store: {e}");
            return;
        }
    };
    let _ = store.set_all_branch_states(&fork_id, "running");

    let aggregate = settings.aggregate_budget_usd.max(settings.default_budget_usd);
    let executor = Arc::new(RotatingBranchExecutor::new(provider, spawner, aggregate));
    let controller = match ForkController::new(fork_config(&settings), executor) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("fork {fork_id}: invalid config: {e}");
            let _ = store.set_all_branch_states(&fork_id, "failed");
            return;
        }
    };

    match controller
        .run_and_resolve_branches(&prompt, branches, &parent_workspace, judge.as_ref())
        .await
    {
        Ok(resolution) => {
            // Persist each branch result.
            for res in &resolution.results {
                let _ = store.update_branch(
                    &res.id.0,
                    branch_state_str(res.state),
                    res.spent_usd,
                    &res.output,
                    res.test_exit_code.map(|c| c as i64),
                );
            }
            let winner = resolution.decision.winner.as_ref().map(|w| w.0.clone());
            let resolved = winner.is_some() && !resolution.decision.needs_confirmation;
            let _ = store.set_resolution(
                &fork_id,
                winner.as_deref(),
                resolution.promoted,
                resolved,
                resolution.aggregate_spent_usd,
            );

            tracing::info!(
                "fork {fork_id} resolved: promoted={} spend=${:.4}",
                resolution.promoted,
                resolution.aggregate_spent_usd
            );
            FORK_METRICS.record_resolution(&resolution);
            // Mirror onto the dashboard Activity Feed (cross-process).
            let agent_id = store.get_fork(&fork_id).ok().flatten().map(|f| f.agent_id).unwrap_or_default();
            append_fork_activity(
                &home_dir,
                &agent_id,
                &fork_id,
                &format!(
                    "Fork resolved over {branch_count} branches: winner={}, promoted={}, spend=${:.4}",
                    resolution.decision.winner.as_ref().map(|w| &w.0[..w.0.len().min(8)]).unwrap_or("none"),
                    resolution.promoted,
                    resolution.aggregate_spent_usd
                ),
            );
            append_fork_history(
                &home_dir,
                &ForkHistoryEntry {
                    ts: chrono::Utc::now().to_rfc3339(),
                    fork_id: fork_id.clone(),
                    branches: branch_count,
                    merge_mode: settings.merge_mode.clone(),
                    winner,
                    promoted: resolution.promoted,
                    aggregate_spent_usd: resolution.aggregate_spent_usd,
                    outcomes: resolution
                        .results
                        .iter()
                        .map(|r| branch_outcome_label(r.state).to_string())
                        .collect(),
                },
            );
        }
        Err(e) => {
            tracing::error!("fork {fork_id} execution failed: {e}");
            // Any branch still 'running' in the store is marked failed.
            if let Ok(rows) = store.list_branches(&fork_id) {
                for b in rows.iter().filter(|b| b.state == "running") {
                    let _ = store.update_branch(&b.branch_id, "failed", b.spent_usd, &b.output, b.test_exit_code);
                }
            }
        }
    }
}

/// Map a `BranchState` to its store string (lowercase, matches `ForkStore`).
fn branch_state_str(state: BranchState) -> &'static str {
    match state {
        BranchState::Pending => "pending",
        BranchState::Running => "running",
        BranchState::Finished => "finished",
        BranchState::BudgetKilled => "budget_killed",
        BranchState::Terminated => "terminated",
        BranchState::Failed => "failed",
    }
}

// ── Real account provider (AccountRotator) ──────────────────────────────────

/// Wraps the production `AccountRotator` as an [`AccountProvider`].
pub struct RotatorProvider(pub Arc<duduclaw_agent::account_rotator::AccountRotator>);

#[async_trait]
impl AccountProvider for RotatorProvider {
    async fn select(&self) -> Option<SelectedAccount> {
        self.0
            .select()
            .await
            .map(|e| SelectedAccount { id: e.id, env_vars: e.env_vars })
    }
    async fn report(&self, account_id: &str, ok: bool, cost_cents: u64) {
        if ok {
            self.0.on_success(account_id, cost_cents).await;
        } else {
            self.0.on_error(account_id).await;
        }
    }
    async fn account_count(&self) -> usize {
        self.0.count().await
    }
}

/// Best-effort: build an account provider for an agent's home, loading accounts
/// from config. Returns `None` when no usable account is configured (caller then
/// keeps the fork in `pending_execution_backend`).
pub async fn build_rotator_provider(home_dir: &Path) -> Option<Arc<RotatorProvider>> {
    use duduclaw_agent::account_rotator::{AccountRotator, RotationStrategy};
    // Escape hatch for tests/CI: never load real accounts or spawn claude.
    if std::env::var_os("DUDUCLAW_FORK_NO_EXEC").is_some() {
        return None;
    }
    let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
    match rotator.load_from_config(home_dir).await {
        Ok(n) if n > 0 => Some(Arc::new(RotatorProvider(Arc::new(rotator)))),
        Ok(_) => {
            tracing::info!("fork: no accounts loaded; execution backend stays pending");
            None
        }
        Err(e) => {
            tracing::warn!("fork: failed to load accounts: {e}");
            None
        }
    }
}

// ── Production claude spawner ────────────────────────────────────────────────

/// Spawns the real `claude` CLI for a branch. Best-effort: discovers the binary
/// via `duduclaw_core::which_claude`, runs `claude -p` with stream-json output in
/// the branch workspace, and extracts the final text + usage cost.
pub struct ClaudeCliSpawner;

#[async_trait]
impl CliSpawner for ClaudeCliSpawner {
    async fn spawn(
        &self,
        ctx: &SpawnCtx,
        prompt: &str,
        workspace: &Path,
        env: &HashMap<String, String>,
    ) -> CliRunOutput {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let bin = match duduclaw_core::which_claude() {
            Some(b) => b,
            None => {
                return CliRunOutput {
                    output: "claude binary not found".into(),
                    spent_usd: 0.0,
                    outcome: SpawnOutcome::Failed,
                };
            }
        };
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("-p")
            .arg(prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .current_dir(workspace)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return CliRunOutput {
                    output: format!("spawn error: {e}"),
                    spent_usd: 0.0,
                    outcome: SpawnOutcome::Failed,
                };
            }
        };
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                return CliRunOutput { output: "no stdout".into(), spent_usd: 0.0, outcome: SpawnOutcome::Failed };
            }
        };

        // The kill switch was registered by run_branch under this branch id.
        let kill = register_kill(&ctx.branch_id);
        let mut reader = BufReader::new(stdout).lines();
        let mut text = String::new();
        let mut running_cost = 0.0_f64;

        let outcome = loop {
            tokio::select! {
                // Woken by an external terminate_branch OR a cross-branch budget
                // pre-emption — the budget-kill tag tells which.
                _ = kill.notified() => {
                    let _ = child.start_kill();
                    break if took_budget_kill(&ctx.branch_id) {
                        SpawnOutcome::BudgetExceeded
                    } else {
                        SpawnOutcome::Cancelled
                    };
                }
                line = reader.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            if let Some((t, c)) = parse_stream_json_line(&l) {
                                if let Some(t) = t { text = t; }
                                if let Some(c) = c {
                                    running_cost = c;
                                    // Stream budget enforcement: per-branch cap +
                                    // cross-branch aggregate pre-emption.
                                    match stream_budget_decision(ctx, running_cost) {
                                        StreamAction::Continue => {}
                                        StreamAction::SelfKill => {
                                            let _ = child.start_kill();
                                            break SpawnOutcome::BudgetExceeded;
                                        }
                                        StreamAction::PreemptOther(victim) => {
                                            request_budget_kill(&victim);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => break SpawnOutcome::Completed, // EOF
                        Err(_) => break SpawnOutcome::Failed,
                    }
                }
            }
        };
        // Hygiene: clear any budget-kill tag not consumed by the kill arm (e.g.
        // this branch was pre-empted but reached EOF before the switch fired).
        let _ = took_budget_kill(&ctx.branch_id);

        let final_outcome = match outcome {
            SpawnOutcome::Completed => match child.wait().await {
                Ok(s) if s.success() => SpawnOutcome::Completed,
                _ => SpawnOutcome::Failed,
            },
            other => {
                let _ = child.wait().await; // reap the killed child
                other
            }
        };

        CliRunOutput {
            output: duduclaw_core::truncate_bytes(&text, 16_000).to_string(),
            spent_usd: running_cost,
            outcome: final_outcome,
        }
    }
}

/// Parse one stream-json line → `(maybe final result text, maybe cumulative cost)`.
fn parse_stream_json_line(line: &str) -> Option<(Option<String>, Option<f64>)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let text = v.get("result").and_then(|r| r.as_str()).map(|s| s.to_string());
    let cost = v.get("total_cost_usd").and_then(|c| c.as_f64());
    if text.is_none() && cost.is_none() {
        return None;
    }
    Some((text, cost))
}

// ── Observability: metrics + history (RFC-26 P5) ────────────────────────────

/// Outcome label for a finished branch, shared by metrics + history.
pub fn branch_outcome_label(state: BranchState) -> &'static str {
    match state {
        BranchState::Finished => "win_or_finish",
        BranchState::BudgetKilled => "budget_killed",
        BranchState::Terminated => "timeout_or_terminated",
        BranchState::Failed => "failed",
        BranchState::Pending | BranchState::Running => "incomplete",
    }
}

/// Process-global fork counters. The MCP server runs in its own process (separate
/// from the gateway's `/metrics`), so these are exposed via `fork_history.jsonl`
/// and `fork_metrics_snapshot()`; wiring them into the gateway `/metrics` endpoint
/// is a follow-up (RFC-26 P5 cross-process note).
#[derive(Default)]
pub struct ForkMetrics {
    pub runs_total: std::sync::atomic::AtomicU64,
    pub branches_total: std::sync::atomic::AtomicU64,
    pub branches_finished: std::sync::atomic::AtomicU64,
    pub branches_budget_killed: std::sync::atomic::AtomicU64,
    pub branches_failed: std::sync::atomic::AtomicU64,
    pub promoted_total: std::sync::atomic::AtomicU64,
}

impl ForkMetrics {
    pub fn record_resolution(&self, resolution: &duduclaw_fork::ForkResolution) {
        use std::sync::atomic::Ordering::Relaxed;
        self.runs_total.fetch_add(1, Relaxed);
        self.branches_total.fetch_add(resolution.results.len() as u64, Relaxed);
        for r in &resolution.results {
            match r.state {
                BranchState::Finished => self.branches_finished.fetch_add(1, Relaxed),
                BranchState::BudgetKilled => self.branches_budget_killed.fetch_add(1, Relaxed),
                BranchState::Failed => self.branches_failed.fetch_add(1, Relaxed),
                _ => 0,
            };
        }
        if resolution.promoted {
            self.promoted_total.fetch_add(1, Relaxed);
        }
    }

    pub fn snapshot(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering::Relaxed;
        serde_json::json!({
            "fork_runs_total": self.runs_total.load(Relaxed),
            "fork_branches_total": self.branches_total.load(Relaxed),
            "fork_branches_finished_total": self.branches_finished.load(Relaxed),
            "fork_branches_budget_killed_total": self.branches_budget_killed.load(Relaxed),
            "fork_branches_failed_total": self.branches_failed.load(Relaxed),
            "fork_promoted_total": self.promoted_total.load(Relaxed),
        })
    }
}

/// Global metrics handle.
pub static FORK_METRICS: std::sync::LazyLock<ForkMetrics> =
    std::sync::LazyLock::new(ForkMetrics::default);

/// One appended line in `fork_history.jsonl`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForkHistoryEntry {
    pub ts: String,
    pub fork_id: String,
    pub branches: usize,
    pub merge_mode: String,
    pub winner: Option<String>,
    pub promoted: bool,
    pub aggregate_spent_usd: f64,
    pub outcomes: Vec<String>,
}

/// Mirror a fork resolution into the dashboard **Activity Feed** by inserting a
/// row into the gateway's cross-process `activity` table (`<home>/tasks.db`). The
/// schema guard (`CREATE TABLE IF NOT EXISTS`) matches `TaskStore` so this is safe
/// even if the MCP server touches the DB before the gateway has initialized it.
/// Best-effort: any error is logged, never fatal.
pub fn append_fork_activity(home_dir: &Path, agent_id: &str, fork_id: &str, summary: &str) {
    let path = home_dir.join("tasks.db");
    let run = || -> rusqlite::Result<()> {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS activity (
                 id TEXT PRIMARY KEY, event_type TEXT NOT NULL, agent_id TEXT NOT NULL,
                 task_id TEXT, summary TEXT NOT NULL, timestamp TEXT NOT NULL, metadata TEXT
             );",
        )?;
        conn.execute(
            "INSERT INTO activity (id, event_type, agent_id, task_id, summary, timestamp, metadata)
             VALUES (?1, 'fork_resolved', ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                format!("act-{}", duduclaw_fork::BranchId::new().0),
                agent_id,
                fork_id,
                duduclaw_core::truncate_bytes(summary, 500),
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    };
    if let Err(e) = run() {
        tracing::warn!("fork activity feed mirror failed: {e}");
    }
}

/// Append one fork resolution to `<home>/fork_history.jsonl` under an advisory
/// file lock (cross-process append safety — coding convention §3).
pub fn append_fork_history(home_dir: &Path, entry: &ForkHistoryEntry) {
    let path = home_dir.join("fork_history.jsonl");
    let line = match serde_json::to_string(entry) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("fork history serialize failed: {e}");
            return;
        }
    };
    let res = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{line}")
    });
    if let Err(e) = res {
        tracing::warn!("fork history append failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_fork::BranchId;

    struct FakeProvider {
        accounts: usize,
    }
    #[async_trait]
    impl AccountProvider for FakeProvider {
        async fn select(&self) -> Option<SelectedAccount> {
            if self.accounts == 0 {
                None
            } else {
                Some(SelectedAccount { id: "acct-1".into(), env_vars: HashMap::new() })
            }
        }
        async fn report(&self, _id: &str, _ok: bool, _cents: u64) {}
    }

    struct FakeSpawner {
        spent: f64,
        ok: bool,
    }
    #[async_trait]
    impl CliSpawner for FakeSpawner {
        async fn spawn(
            &self,
            _ctx: &SpawnCtx,
            prompt: &str,
            ws: &Path,
            _e: &HashMap<String, String>,
        ) -> CliRunOutput {
            // Prove the branch workspace is writable + isolated.
            let _ = std::fs::write(ws.join("ran.txt"), prompt);
            CliRunOutput {
                output: format!("answer: {prompt}"),
                spent_usd: self.spent,
                outcome: if self.ok { SpawnOutcome::Completed } else { SpawnOutcome::Failed },
            }
        }
    }

    fn inv(budget: f64) -> BranchInvocation {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        // Leak the tempdir for the duration of the test process so the path stays valid.
        std::mem::forget(dir);
        BranchInvocation {
            branch_id: BranchId::new(),
            prompt: "solve it".into(),
            steering: Some("be bold".into()),
            workspace: ws,
            budget_usd: budget,
        }
    }

    #[tokio::test]
    async fn happy_path_finishes_and_charges() {
        let exec = RotatingBranchExecutor::new(
            Arc::new(FakeProvider { accounts: 2 }),
            Arc::new(FakeSpawner { spent: 0.1, ok: true }),
            1.0,
        );
        let r = exec.run_branch(inv(0.5)).await.unwrap();
        assert_eq!(r.state, BranchState::Finished);
        assert_eq!(r.spent_usd, 0.1);
        assert!(r.output.contains("be bold")); // steering folded into prompt
    }

    #[tokio::test]
    async fn cancelled_branch_skips_execution() {
        let exec = RotatingBranchExecutor::new(
            Arc::new(FakeProvider { accounts: 1 }),
            Arc::new(FakeSpawner { spent: 0.1, ok: true }),
            1.0,
        );
        let invocation = inv(0.5);
        request_cancel(&invocation.branch_id.0);
        let r = exec.run_branch(invocation).await.unwrap();
        assert_eq!(r.state, BranchState::Terminated);
        assert_eq!(r.spent_usd, 0.0); // never spawned
    }

    #[tokio::test]
    async fn account_count_default_is_unbounded() {
        let p = FakeProvider { accounts: 3 };
        assert_eq!(p.account_count().await, usize::MAX); // fake uses trait default
    }

    #[tokio::test]
    async fn no_account_fails_branch() {
        let exec = RotatingBranchExecutor::new(
            Arc::new(FakeProvider { accounts: 0 }),
            Arc::new(FakeSpawner { spent: 0.1, ok: true }),
            1.0,
        );
        let r = exec.run_branch(inv(0.5)).await.unwrap();
        assert_eq!(r.state, BranchState::Failed);
    }

    #[tokio::test]
    async fn spawner_failure_marks_failed() {
        let exec = RotatingBranchExecutor::new(
            Arc::new(FakeProvider { accounts: 1 }),
            Arc::new(FakeSpawner { spent: 0.0, ok: false }),
            1.0,
        );
        let r = exec.run_branch(inv(0.5)).await.unwrap();
        assert_eq!(r.state, BranchState::Failed);
    }

    #[tokio::test]
    async fn per_branch_budget_exceeded_is_budget_killed() {
        let exec = RotatingBranchExecutor::new(
            Arc::new(FakeProvider { accounts: 1 }),
            Arc::new(FakeSpawner { spent: 0.9, ok: true }),
            10.0,
        );
        // per-branch cap 0.5 < spend 0.9 ⇒ BudgetKilled
        let r = exec.run_branch(inv(0.5)).await.unwrap();
        assert_eq!(r.state, BranchState::BudgetKilled);
    }

    #[test]
    fn parse_stream_json_line_extracts_text_and_cost() {
        // A result event carries both fields.
        let (t, c) =
            parse_stream_json_line("{\"type\":\"result\",\"result\":\"final answer\",\"total_cost_usd\":0.0234}")
                .unwrap();
        assert_eq!(t.as_deref(), Some("final answer"));
        assert!((c.unwrap() - 0.0234).abs() < 1e-9);
        // A cost-only progress event.
        let (t2, c2) = parse_stream_json_line("{\"total_cost_usd\":0.01}").unwrap();
        assert!(t2.is_none());
        assert_eq!(c2, Some(0.01));
    }

    #[test]
    fn parse_stream_json_line_ignores_irrelevant() {
        assert!(parse_stream_json_line("").is_none());
        assert!(parse_stream_json_line("garbage").is_none());
        assert!(parse_stream_json_line("{\"type\":\"system\"}").is_none());
    }

    #[tokio::test]
    async fn external_cancel_fires_kill_switch_for_running_branch() {
        // Registering a kill switch then request_cancel must notify its waiter.
        let bid = "running-branch-1";
        let kill = register_kill(bid);
        let waiter = kill.clone();
        let handle = tokio::spawn(async move { waiter.notified().await });
        // Give the waiter a tick to park, then fire.
        tokio::task::yield_now().await;
        request_cancel(bid);
        // The kill notification wakes the waiter (would hang if not fired).
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("kill switch did not fire")
            .unwrap();
        unregister_kill(bid);
    }

    #[test]
    fn append_fork_history_writes_jsonl_line() {
        let home = tempfile::tempdir().unwrap();
        let entry = ForkHistoryEntry {
            ts: "2026-06-19T00:00:00Z".into(),
            fork_id: "fork-x".into(),
            branches: 2,
            merge_mode: "auto".into(),
            winner: Some("b1".into()),
            promoted: true,
            aggregate_spent_usd: 0.2,
            outcomes: vec!["win_or_finish".into(), "failed".into()],
        };
        append_fork_history(home.path(), &entry);
        append_fork_history(home.path(), &entry);

        let content = std::fs::read_to_string(home.path().join("fork_history.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: ForkHistoryEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.fork_id, "fork-x");
        assert_eq!(parsed.branches, 2);
        assert!(parsed.promoted);
    }

    #[test]
    fn metrics_record_resolution_counts() {
        use duduclaw_fork::{merge::MergeDecision, ForkResolution};
        let metrics = ForkMetrics::default();
        let resolution = ForkResolution {
            results: vec![
                BranchResult {
                    id: BranchId::new(),
                    state: BranchState::Finished,
                    output: String::new(),
                    spent_usd: 0.1,
                    test_exit_code: None,
                },
                BranchResult {
                    id: BranchId::new(),
                    state: BranchState::Failed,
                    output: String::new(),
                    spent_usd: 0.0,
                    test_exit_code: None,
                },
            ],
            verdict: None,
            decision: MergeDecision {
                winner: None,
                needs_confirmation: false,
                reason: String::new(),
            },
            promoted: true,
            aggregate_spent_usd: 0.1,
        };
        metrics.record_resolution(&resolution);
        let snap = metrics.snapshot();
        assert_eq!(snap["fork_runs_total"], 1);
        assert_eq!(snap["fork_branches_total"], 2);
        assert_eq!(snap["fork_branches_finished_total"], 1);
        assert_eq!(snap["fork_branches_failed_total"], 1);
        assert_eq!(snap["fork_promoted_total"], 1);
    }

    #[test]
    fn append_fork_activity_inserts_row() {
        let home = tempfile::tempdir().unwrap();
        append_fork_activity(home.path(), "a1", "fork-1", "Fork resolved: winner=b1");
        // Read it back via a fresh connection (cross-process).
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        let (etype, agent, summary): (String, String, String) = conn
            .query_row(
                "SELECT event_type, agent_id, summary FROM activity WHERE task_id='fork-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(etype, "fork_resolved");
        assert_eq!(agent, "a1");
        assert!(summary.contains("winner=b1"));
    }

    #[test]
    fn outcome_labels() {
        assert_eq!(branch_outcome_label(BranchState::Finished), "win_or_finish");
        assert_eq!(branch_outcome_label(BranchState::BudgetKilled), "budget_killed");
        assert_eq!(branch_outcome_label(BranchState::Failed), "failed");
    }

    // ── Cross-branch aggregate pre-emption (RFC-26 §4.2) ────────────────────

    fn ctx_with(id: &str, budget: f64, agg: Option<Arc<duduclaw_fork::LiveAggregate>>) -> SpawnCtx {
        SpawnCtx { branch_id: id.into(), budget_usd: budget, aggregate: agg }
    }

    #[test]
    fn stream_decision_per_branch_cap_self_kills() {
        // No aggregate board: only the per-branch cap applies.
        let ctx = ctx_with("b", 0.5, None);
        assert_eq!(stream_budget_decision(&ctx, 0.4), StreamAction::Continue);
        assert_eq!(stream_budget_decision(&ctx, 0.6), StreamAction::SelfKill);
    }

    #[test]
    fn stream_decision_aggregate_self_kill_when_observer_is_priciest() {
        let agg = Arc::new(duduclaw_fork::LiveAggregate::new(1.0));
        let cheap = ctx_with("cheap", 10.0, Some(agg.clone()));
        let pricey = ctx_with("pricey", 10.0, Some(agg.clone()));
        // cheap(0.3) under aggregate; pricey(0.9) pushes total 1.2 > 1.0 and is
        // itself the most expensive ⇒ it self-kills (no sibling sacrificed).
        assert_eq!(stream_budget_decision(&cheap, 0.3), StreamAction::Continue);
        assert_eq!(stream_budget_decision(&pricey, 0.9), StreamAction::SelfKill);
    }

    #[test]
    fn stream_decision_aggregate_preempts_priciest_sibling() {
        let agg = Arc::new(duduclaw_fork::LiveAggregate::new(1.0));
        let pricey = ctx_with("pricey", 10.0, Some(agg.clone()));
        let cheap = ctx_with("cheap", 10.0, Some(agg.clone()));
        // pricey(0.9) under aggregate alone; when cheap(0.3) tips the total over
        // the cap, the cheap observer pre-empts the priciest in-flight sibling.
        assert_eq!(stream_budget_decision(&pricey, 0.9), StreamAction::Continue);
        assert_eq!(
            stream_budget_decision(&cheap, 0.3),
            StreamAction::PreemptOther("pricey".into())
        );
    }

    #[test]
    fn stream_decision_finish_frees_aggregate_for_survivors() {
        let agg = Arc::new(duduclaw_fork::LiveAggregate::new(1.0));
        let a = ctx_with("a", 10.0, Some(agg.clone()));
        let b = ctx_with("b", 10.0, Some(agg.clone()));
        // a is the pricier branch (0.7), still under the aggregate alone.
        assert_eq!(stream_budget_decision(&a, 0.7), StreamAction::Continue);
        // b(0.4) tips the total to 1.1 > 1.0; a is the priciest in-flight branch,
        // so the cheaper observer b pre-empts a (not itself).
        assert_eq!(stream_budget_decision(&b, 0.4), StreamAction::PreemptOther("a".into()));
        // Once the pre-empted a finishes, b's continued spend fits under the cap.
        agg.finish("a");
        assert_eq!(stream_budget_decision(&b, 0.9), StreamAction::Continue);
    }

    #[test]
    fn budget_kill_tag_maps_woken_branch_to_budget_outcome() {
        // A budget pre-emption tags the branch; the spawner's kill arm consumes
        // the tag exactly once to choose BudgetExceeded over Cancelled.
        let bid = "agg-victim-unique-xyz";
        let _switch = register_kill(bid);
        request_budget_kill(bid);
        assert!(took_budget_kill(bid), "first take sees the budget tag");
        assert!(!took_budget_kill(bid), "tag is consumed exactly once");
        unregister_kill(bid);
    }
}
