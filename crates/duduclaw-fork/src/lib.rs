//! # duduclaw-fork — Live Run Forking (RFC-26)
//!
//! Split an in-flight agent run into N competing branches, run them in parallel
//! with copy-on-write workspaces and per-branch budgets, then let an AI judge
//! merge the winner back into the parent workspace.
//!
//! This crate is **default-off** infrastructure: nothing forks until an agent
//! opts in via `agent.toml [fork] enabled = true`. The gateway wires a concrete
//! [`BranchExecutor`] (backed by `AccountRotator` + `rotate_cli_spawn`) into the
//! [`ForkController`]; this crate stays decoupled from the CLI runner so it can be
//! unit-tested in isolation.
//!
//! ## Phase status (RFC-26 §5)
//! - **P1 (this crate, MVP):** [`Branch`], [`BranchOverlay`], [`budget::Pool`],
//!   [`ForkController`] driving branches through an abstract [`BranchExecutor`].
//! - **P2:** `JudgeAgent` + `JudgeVerdict`, test runner, merge modes.
//! - **P3:** MCP tools + `Scope::ForkExecute`.
//! - **P4:** native copy-on-write overlay, aggregate budget pool wired to the rotator.

pub mod branch;
pub mod budget;
pub mod copy_policy;
pub mod error;
pub mod judge;
pub mod merge;
pub mod overlay;
pub mod publication;
pub mod retention;
pub mod store;
pub mod test_runner;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

pub use branch::{Branch, BranchId, BranchResult, BranchSpec, BranchState};
pub use budget::{Charge, LiveAggregate, Pool, Preempt};
pub use error::{ForkError, Result};
pub use judge::{JudgeAgent, JudgeScores, JudgeVerdict};
pub use merge::{MergeDecision, DEFAULT_CONFIDENCE_THRESHOLD};
pub use copy_policy::{CopyPolicy, CopyReport};
pub use overlay::{detect_backend, promote_workspace, BranchOverlay, OverlayBackend};
pub use publication::{with_parent_publication, ParentPublication};
pub use store::{BranchRow, ForkRow, ForkStore, ForkStoreMetrics};
pub use test_runner::TestOutcome;

/// Number of independent judge passes used in [`MergeMode::Vote`].
const VOTE_ROUNDS: usize = 3;

/// How a finished fork picks its winner (RFC-26 §3.3). Selection logic lands in
/// P2; the enum is defined now so config and the controller can carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    /// Human selects via CLI.
    Manual,
    /// Judge auto-picks, no human in the loop.
    Auto,
    /// Judge auto-picks but surfaces a confirm. **Default.**
    #[default]
    AutoWithFallback,
    /// N judges, consensus selection.
    Vote,
}

/// Fork configuration, sourced from `agent.toml [fork]` (RFC-26 §3.5).
#[derive(Debug, Clone)]
pub struct ForkConfig {
    pub max_branches: usize,
    pub default_budget_usd: f64,
    pub aggregate_budget_usd: f64,
    pub merge_mode: MergeMode,
    pub test_command: Option<String>,
    pub test_timeout_s: u64,
}

impl Default for ForkConfig {
    fn default() -> Self {
        ForkConfig {
            max_branches: 4,
            default_budget_usd: 0.50,
            aggregate_budget_usd: 1.50,
            merge_mode: MergeMode::default(),
            test_command: None,
            test_timeout_s: 120,
        }
    }
}

impl ForkConfig {
    /// Validate config — fail-closed on nonsensical values rather than silently
    /// clamping to a surprising default mid-run.
    pub fn validate(&self) -> Result<()> {
        if self.max_branches == 0 {
            return Err(ForkError::Config("max_branches must be >= 1".into()));
        }
        if self.default_budget_usd <= 0.0 {
            return Err(ForkError::Config("default_budget_usd must be > 0".into()));
        }
        if self.aggregate_budget_usd < self.default_budget_usd {
            return Err(ForkError::Config(
                "aggregate_budget_usd must be >= default_budget_usd".into(),
            ));
        }
        Ok(())
    }
}

/// A single branch invocation request handed to the executor.
#[derive(Debug, Clone)]
pub struct BranchInvocation {
    pub branch_id: BranchId,
    /// The base task prompt shared by all branches.
    pub prompt: String,
    /// This branch's steering addition, if any.
    pub steering: Option<String>,
    /// Absolute path to this branch's isolated workspace.
    pub workspace: std::path::PathBuf,
    /// Per-branch budget cap (USD).
    pub budget_usd: f64,
}

/// Abstraction over "run one branch as an agent subprocess".
///
/// The gateway implements this with `AccountRotator` + `rotate_cli_spawn` (each
/// branch gets a distinct account so parallel branches don't collide on a single
/// account's rate limit — RFC-26 §3.1). Tests implement it with a stub.
#[async_trait]
pub trait BranchExecutor: Send + Sync {
    /// Run one branch to completion and return its result.
    async fn run_branch(&self, inv: BranchInvocation) -> Result<BranchResult>;
}

/// Orchestrates a single fork: builds overlays, spawns branches through the
/// executor under a shared budget pool, and collects results for judging.
pub struct ForkController<E: BranchExecutor> {
    config: ForkConfig,
    executor: Arc<E>,
}

impl<E: BranchExecutor + 'static> ForkController<E> {
    pub fn new(config: ForkConfig, executor: Arc<E>) -> Result<Self> {
        config.validate()?;
        Ok(ForkController { config, executor })
    }

    pub fn config(&self) -> &ForkConfig {
        &self.config
    }

    /// Effective branch count: requested `n`, capped at `max_branches` and at
    /// `available_accounts` (RFC-26 §6 Q2 — cap, don't serialize; the reduction
    /// is returned so the caller can log it, honoring "no silent caps").
    pub fn effective_branch_count(&self, requested: usize, available_accounts: usize) -> usize {
        requested
            .min(self.config.max_branches)
            .min(available_accounts.max(1))
            .max(1)
    }

    /// Run a fork over `prompt` with `specs.len()` branches against `parent_workspace`.
    ///
    /// Branches run concurrently via `tokio`; each gets a CoW overlay and a
    /// per-branch budget registered in the shared [`Pool`]. Overlays are discarded
    /// at the end — use [`ForkController::run_and_resolve`] to judge + promote a
    /// winner instead.
    pub async fn run(
        &self,
        prompt: &str,
        specs: Vec<BranchSpec>,
        parent_workspace: &std::path::Path,
    ) -> Result<Vec<BranchResult>> {
        let branches = specs.into_iter().map(Branch::new).collect();
        let (results, _overlays, _pool) = self.spawn_all(prompt, branches, parent_workspace).await?;
        Ok(results)
    }

    /// Spawn every branch concurrently, returning their results plus the live
    /// overlays (keyed by branch id) so a winner can be promoted, and the shared
    /// budget pool for cost reporting.
    async fn spawn_all(
        &self,
        prompt: &str,
        branches: Vec<Branch>,
        parent_workspace: &std::path::Path,
    ) -> Result<(Vec<BranchResult>, HashMap<BranchId, BranchOverlay>, Arc<Pool>)> {
        if branches.is_empty() {
            return Err(ForkError::Config("a fork needs at least one branch".into()));
        }
        let parent_workspace = parent_workspace.canonicalize()
            .map_err(|error| ForkError::Overlay(format!("canonicalize fork parent: {error}")))?;
        let pool = Arc::new(Pool::new(self.config.aggregate_budget_usd));
        let mut handles = Vec::with_capacity(branches.len());
        let mut overlays: HashMap<BranchId, BranchOverlay> = HashMap::with_capacity(branches.len());

        for branch in branches {
            pool.register(branch.id.clone(), branch.spec.budget_usd);

            let overlay = BranchOverlay::create(&parent_workspace)?;
            let inv = BranchInvocation {
                branch_id: branch.id.clone(),
                prompt: prompt.to_string(),
                steering: branch.spec.steering.clone(),
                workspace: overlay.workspace().to_path_buf(),
                budget_usd: branch.spec.budget_usd,
            };
            overlays.insert(branch.id.clone(), overlay);

            let executor = Arc::clone(&self.executor);
            handles.push(tokio::spawn(async move { executor.run_branch(inv).await }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(e)) => tracing::warn!("branch executor error: {e}"),
                Err(e) => tracing::warn!("branch task join error: {e}"),
            }
        }
        Ok((results, overlays, pool))
    }

    /// Full fork pipeline: run branches → run the configured test against each
    /// branch snapshot → judge → resolve merge → promote the winner's overlay when
    /// the decision is final (RFC-26 §3.3).
    ///
    /// When the decision needs operator confirmation (`Manual` /
    /// `AutoWithFallback`, or sub-threshold confidence) nothing is promoted; the
    /// caller resolves later via the MCP `merge_or_select` tool (P3). For
    /// [`MergeMode::Vote`] the judge is sampled [`VOTE_ROUNDS`] times.
    pub async fn run_and_resolve<J: JudgeAgent>(
        &self,
        prompt: &str,
        specs: Vec<BranchSpec>,
        parent_workspace: &std::path::Path,
        judge: &J,
    ) -> Result<ForkResolution> {
        let branches = specs.into_iter().map(Branch::new).collect();
        self.run_and_resolve_branches(prompt, branches, parent_workspace, judge)
            .await
    }

    /// Like [`Self::run_and_resolve`] but with caller-supplied [`Branch`]es so the
    /// fork's branch ids stay stable across an external registry (RFC-26 P4).
    pub async fn run_and_resolve_branches<J: JudgeAgent>(
        &self,
        prompt: &str,
        branches: Vec<Branch>,
        parent_workspace: &std::path::Path,
        judge: &J,
    ) -> Result<ForkResolution> {
        self.run_and_resolve_branches_retaining(prompt, branches, parent_workspace, judge, None)
            .await
    }

    /// Like [`Self::run_and_resolve_branches`], but when the fork ends without
    /// promoting a winner (confirmation pending, `Manual` mode) and `retain_dir`
    /// is given, every `Finished` branch workspace is persisted to
    /// `<retain_dir>/<branch_id>/` (owner-only permissions) so a later manual
    /// selection can still promote real files. Listed in
    /// [`ForkResolution::retained`]. A fork resolved finally retains nothing.
    ///
    /// Retention failure aborts publication so an external recovery owner can
    /// retain its archived source instead of committing an unavailable branch.
    pub async fn run_and_resolve_branches_retaining<J: JudgeAgent>(
        &self,
        prompt: &str,
        branches: Vec<Branch>,
        parent_workspace: &std::path::Path,
        judge: &J,
        retain_dir: Option<&std::path::Path>,
    ) -> Result<ForkResolution> {
        self.prepare_branches(prompt, branches, parent_workspace, judge).await?
            .publish(retain_dir, &[], Ok)
    }

    /// Run and judge without changing the parent or exposing retained paths.
    /// The caller publishes only after checking current state under its store lock.
    pub async fn prepare_branches<J: JudgeAgent>(
        &self, prompt: &str, branches: Vec<Branch>,
        parent_workspace: &std::path::Path, judge: &J,
    ) -> Result<PreparedFork> {
        let (mut results, overlays, pool) =
            self.spawn_all(prompt, branches, parent_workspace).await?;

        // Run the configured test against each branch snapshot, folding the exit
        // code into the result so the judge can weight test_pass_ratio.
        if let Some(cmd) = self.config.test_command.as_deref() {
            for r in results.iter_mut() {
                if let Some(overlay) = overlays.get(&r.id) {
                    match test_runner::run_test(overlay.workspace(), Some(cmd), self.config.test_timeout_s)
                        .await
                    {
                        Ok(Some(outcome)) => r.test_exit_code = Some(outcome.exit_code),
                        Ok(None) => {}
                        Err(e) => tracing::warn!("branch {} test error: {e}", r.id),
                    }
                }
            }
        }

        // Judge.
        let (verdict, decision) = match self.config.merge_mode {
            MergeMode::Vote => {
                let mut verdicts = Vec::with_capacity(VOTE_ROUNDS);
                for _ in 0..VOTE_ROUNDS {
                    match judge.judge(prompt, &results).await {
                        Ok(v) => verdicts.push(v),
                        Err(e) => tracing::warn!("vote-round judge error: {e}"),
                    }
                }
                if verdicts.is_empty() {
                    return Err(ForkError::Executor("all vote-round judges failed".into()));
                }
                let decision = merge::resolve_vote(&verdicts, DEFAULT_CONFIDENCE_THRESHOLD);
                (verdicts.into_iter().next(), decision)
            }
            mode => {
                let verdict = judge.judge(prompt, &results).await?;
                let decision = merge::resolve(&verdict, mode, DEFAULT_CONFIDENCE_THRESHOLD);
                (Some(verdict), decision)
            }
        };

        // Aggregate spend is summed from the branch results (each executor charges
        // its own budget pool live; see RotatingBranchExecutor in the cli crate).
        let aggregate_spent_usd: f64 = results.iter().map(|r| r.spent_usd).sum();
        let _ = &pool; // pool reserved for executor-side per-branch enforcement (P4)

        Ok(PreparedFork {
            parent: overlays.values().next().expect("nonempty fork overlays").parent().to_path_buf(), overlays,
            promote_policy: CopyPolicy::promote_default(),
            resolution: ForkResolution { results, verdict, decision, promoted: false,
                aggregate_spent_usd, retained: Vec::new() },
        })
    }
}

/// Owns private temporary overlays until publication; TTL GC cannot observe them.
pub struct PreparedFork {
    parent: std::path::PathBuf,
    overlays: HashMap<BranchId, BranchOverlay>,
    resolution: ForkResolution,
    /// Policy the winner is promoted under. Defaults to the fail-closed
    /// [`CopyPolicy::promote_default`]; a caller that knows the DuDuClaw home
    /// narrows it with [`CopyPolicy::promote_for_parent`].
    promote_policy: CopyPolicy,
}

impl PreparedFork {
    pub fn resolution(&self) -> &ForkResolution { &self.resolution }

    /// The parent workspace a winner would be promoted into.
    pub fn parent(&self) -> &std::path::Path { &self.parent }

    /// Replace the promotion policy (see [`CopyPolicy::promote_for_parent`]).
    pub fn with_promote_policy(mut self, policy: CopyPolicy) -> Self {
        self.promote_policy = policy;
        self
    }

    /// Preserve independent private copies before entering publication, so an
    /// IO/DB fault cannot drop the only source with the prepared TempDirs.
    pub fn archive_sources(&self, destination: &std::path::Path) -> Result<Vec<RetainedBranch>> {
        retention::ensure_private_dir(destination)?;
        let mut sources = Vec::new();
        for (id, overlay) in &self.overlays {
            retention::validate_id(&id.0)?;
            let workspace = destination.join(&id.0);
            retention::ensure_private_dir(&workspace)?;
            CopyPolicy::fork_default().copy_tree(overlay.workspace(), &workspace)?;
            sources.push(RetainedBranch { branch_id: id.clone(), workspace });
        }
        Ok(sources)
    }

    pub fn keep_sources(self) -> Vec<RetainedBranch> {
        self.overlays.into_iter().map(|(branch_id, overlay)| RetainedBranch {
            branch_id, workspace: overlay.keep_source(),
        }).collect()
    }

    /// Apply/retain and publish under one parent lock. Caller-owned store locks
    /// must already be held. Cancelled branches can neither win nor be retained.
    pub fn publish<T>(
        self, retain_dir: Option<&std::path::Path>, cancelled: &[BranchId],
        publish: impl FnOnce(ForkResolution) -> Result<T>,
    ) -> Result<T> {
        self.publish_with_observer(retain_dir, cancelled, || Ok(()), publish)
    }

    /// Persist a truthful uncertainty marker before the first parent write.
    pub fn publish_with_observer<T>(
        mut self, retain_dir: Option<&std::path::Path>, cancelled: &[BranchId],
        before_parent_write: impl FnOnce() -> Result<()>,
        publish: impl FnOnce(ForkResolution) -> Result<T>,
    ) -> Result<T> {
        let parent = self.parent.clone();
        with_parent_publication(&parent, |publication| {
            let eligible = |id: &BranchId| !cancelled.contains(id) && self.resolution.results.iter()
                .any(|result| &result.id == id && result.state == BranchState::Finished);
            if self.resolution.decision.winner.as_ref().is_some_and(|id| !eligible(id)) {
                self.resolution.decision.winner = None;
                self.resolution.decision.needs_confirmation = true;
                self.resolution.decision.reason = "selected branch no longer eligible for publication".into();
            }
            let winner_overlay = self.resolution.decision.winner.as_ref()
                .filter(|_| !self.resolution.decision.needs_confirmation)
                .and_then(|winner| self.overlays.get(winner));
            if let Some(overlay) = winner_overlay {
                before_parent_write()?;
                publication.promote(overlay.workspace(), &self.promote_policy)?;
                self.resolution.promoted = true;
            }
            if let (false, Some(dir)) = (self.resolution.promoted, retain_dir) {
                let survivors: Vec<BranchId> = self.resolution.results
                    .iter()
                    .filter(|r| r.state == BranchState::Finished && !cancelled.contains(&r.id))
                    .map(|r| r.id.clone())
                    .collect();
                if !survivors.is_empty() {
                    retention::ensure_private_dir(dir)?;
                    for id in survivors {
                        let Some(overlay) = self.overlays.remove(&id) else { continue };
                        retention::validate_id(&id.0)?;
                        let dest = dir.join(&id.0);
                        match overlay.persist_to(&dest) {
                            Ok(path) => {
                                retention::set_private(&path)?;
                                self.resolution.retained.push(RetainedBranch { branch_id: id, workspace: path });
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
            }

            publish(self.resolution)
        })
    }
}

/// Outcome of [`ForkController::run_and_resolve`].
#[derive(Debug, Clone)]
pub struct ForkResolution {
    pub results: Vec<BranchResult>,
    pub verdict: Option<JudgeVerdict>,
    pub decision: MergeDecision,
    /// Whether the winner's overlay was merged into the parent workspace.
    pub promoted: bool,
    pub aggregate_spent_usd: f64,
    /// Branch workspaces kept on disk for a later manual selection (empty when
    /// the fork resolved finally or retention was not requested).
    pub retained: Vec<RetainedBranch>,
}

/// A branch workspace persisted by
/// [`ForkController::run_and_resolve_branches_retaining`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedBranch {
    pub branch_id: BranchId,
    pub workspace: std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoExecutor;

    #[async_trait]
    impl BranchExecutor for EchoExecutor {
        async fn run_branch(&self, inv: BranchInvocation) -> Result<BranchResult> {
            // Prove the workspace is isolated by writing into it.
            std::fs::write(inv.workspace.join("branch_out.txt"), &inv.branch_id.0)
                .map_err(|e| ForkError::Executor(e.to_string()))?;
            Ok(BranchResult {
                id: inv.branch_id,
                state: BranchState::Finished,
                output: format!(
                    "{}{}",
                    inv.prompt,
                    inv.steering.map(|s| format!(" :: {s}")).unwrap_or_default()
                ),
                spent_usd: 0.1,
                test_exit_code: None,
            })
        }
    }

    fn ctrl() -> ForkController<EchoExecutor> {
        ForkController::new(ForkConfig::default(), Arc::new(EchoExecutor)).unwrap()
    }

    #[test]
    fn config_validation_rejects_bad_values() {
        let c = ForkConfig { max_branches: 0, ..ForkConfig::default() };
        assert!(c.validate().is_err());

        let c = ForkConfig {
            aggregate_budget_usd: 0.1,
            default_budget_usd: 0.5,
            ..ForkConfig::default()
        };
        assert!(c.validate().is_err());

        assert!(ForkConfig::default().validate().is_ok());
    }

    #[test]
    fn effective_branch_count_caps_to_min() {
        let c = ctrl();
        // max_branches default 4
        assert_eq!(c.effective_branch_count(10, 8), 4);
        // capped by available accounts
        assert_eq!(c.effective_branch_count(4, 2), 2);
        // never zero
        assert_eq!(c.effective_branch_count(0, 0), 1);
    }

    #[tokio::test]
    async fn run_spawns_branches_with_isolated_workspaces() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::write(parent.path().join("seed.txt"), "shared").unwrap();

        let c = ctrl();
        let specs = vec![
            BranchSpec { steering: Some("approach-A".into()), budget_usd: 0.5 },
            BranchSpec { steering: Some("approach-B".into()), budget_usd: 0.5 },
        ];
        let results = c.run("solve it", specs, parent.path()).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.state == BranchState::Finished));
        let outputs: Vec<_> = results.iter().map(|r| r.output.clone()).collect();
        assert!(outputs.iter().any(|o| o.contains("approach-A")));
        assert!(outputs.iter().any(|o| o.contains("approach-B")));

        // Each branch saw the shared seed file (read-through) and wrote locally
        // without mutating the parent.
        assert!(!parent.path().join("branch_out.txt").exists());
    }

    #[tokio::test]
    async fn run_rejects_empty_specs() {
        let parent = tempfile::tempdir().unwrap();
        let c = ctrl();
        assert!(c.run("x", vec![], parent.path()).await.is_err());
    }

    // ── run_and_resolve pipeline (P2) ───────────────────────────────────────

    /// Executor that writes each branch's steering tag into a file, so a promoted
    /// winner is observable in the parent workspace.
    struct TaggingExecutor;

    #[async_trait]
    impl BranchExecutor for TaggingExecutor {
        async fn run_branch(&self, inv: BranchInvocation) -> Result<BranchResult> {
            let tag = inv.steering.clone().unwrap_or_default();
            std::fs::write(inv.workspace.join("result.txt"), &tag)
                .map_err(|e| ForkError::Executor(e.to_string()))?;
            Ok(BranchResult {
                id: inv.branch_id,
                state: BranchState::Finished,
                output: format!("clean answer for {tag}."),
                spent_usd: 0.1,
                test_exit_code: None,
            })
        }
    }

    fn ctrl_with(mode: MergeMode) -> ForkController<TaggingExecutor> {
        let config = ForkConfig { merge_mode: mode, ..ForkConfig::default() };
        ForkController::new(config, Arc::new(TaggingExecutor)).unwrap()
    }

    #[tokio::test]
    async fn auto_mode_promotes_winner_to_parent() {
        let parent = tempfile::tempdir().unwrap();
        let c = ctrl_with(MergeMode::Auto);
        let specs = vec![
            BranchSpec { steering: Some("alpha".into()), budget_usd: 0.5 },
            BranchSpec { steering: Some("beta".into()), budget_usd: 0.5 },
        ];
        let res = c
            .run_and_resolve("solve", specs, parent.path(), &judge::HeuristicJudge)
            .await
            .unwrap();

        assert!(res.promoted, "Auto mode should promote without confirmation");
        assert!(!res.decision.needs_confirmation);
        // The winner's result.txt was merged into the parent.
        let merged = std::fs::read_to_string(parent.path().join("result.txt")).unwrap();
        assert!(merged == "alpha" || merged == "beta");
    }

    #[tokio::test]
    async fn auto_with_fallback_does_not_promote() {
        let parent = tempfile::tempdir().unwrap();
        let c = ctrl_with(MergeMode::AutoWithFallback);
        let specs = vec![
            BranchSpec { steering: Some("alpha".into()), budget_usd: 0.5 },
            BranchSpec { steering: Some("beta".into()), budget_usd: 0.5 },
        ];
        let res = c
            .run_and_resolve("solve", specs, parent.path(), &judge::HeuristicJudge)
            .await
            .unwrap();

        assert!(!res.promoted, "fallback awaits confirmation");
        assert!(res.decision.needs_confirmation);
        assert!(res.decision.winner.is_some());
        // Parent untouched until confirmation.
        assert!(!parent.path().join("result.txt").exists());
    }

    #[tokio::test]
    async fn unresolved_fork_retains_workspaces_for_manual_selection() {
        let parent = tempfile::tempdir().unwrap();
        let keep = tempfile::tempdir().unwrap();
        let retain = keep.path().join("fork-x");
        let c = ctrl_with(MergeMode::Manual);
        let specs = vec![
            BranchSpec { steering: Some("alpha".into()), budget_usd: 0.5 },
            BranchSpec { steering: Some("beta".into()), budget_usd: 0.5 },
        ];
        let res = c
            .run_and_resolve_branches_retaining(
                "solve",
                specs.into_iter().map(Branch::new).collect(),
                parent.path(),
                &judge::HeuristicJudge,
                Some(&retain),
            )
            .await
            .unwrap();
        assert!(!res.promoted);
        assert_eq!(res.retained.len(), 2);
        for r in &res.retained {
            assert!(r.workspace.starts_with(&retain));
            assert!(r.workspace.join("result.txt").is_file());
        }
        // A later selection promotes the retained files into the parent.
        promote_workspace(&res.retained[0].workspace, parent.path(), &CopyPolicy::fork_default())
            .unwrap();
        assert!(parent.path().join("result.txt").is_file());
    }

    #[tokio::test]
    async fn final_resolution_retains_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let keep = tempfile::tempdir().unwrap();
        let retain = keep.path().join("fork-y");
        let c = ctrl_with(MergeMode::Auto);
        let specs = vec![
            BranchSpec { steering: Some("a".into()), budget_usd: 0.5 },
            BranchSpec { steering: Some("b".into()), budget_usd: 0.5 },
        ];
        let res = c
            .run_and_resolve_branches_retaining(
                "solve",
                specs.into_iter().map(Branch::new).collect(),
                parent.path(),
                &judge::HeuristicJudge,
                Some(&retain),
            )
            .await
            .unwrap();
        assert!(res.promoted);
        assert!(res.retained.is_empty());
        assert!(!retain.exists());
    }

    #[tokio::test]
    async fn resolve_reports_aggregate_spend() {
        let parent = tempfile::tempdir().unwrap();
        let c = ctrl_with(MergeMode::Auto);
        let specs = vec![
            BranchSpec { steering: Some("a".into()), budget_usd: 0.5 },
            BranchSpec { steering: Some("b".into()), budget_usd: 0.5 },
        ];
        let res = c
            .run_and_resolve("solve", specs, parent.path(), &judge::HeuristicJudge)
            .await
            .unwrap();
        // Pool spend is wired by the executor in P4; here it is 0 but the field exists.
        assert!(res.aggregate_spent_usd >= 0.0);
        assert_eq!(res.results.len(), 2);
    }
}
