//! Operator-only online exploration. Policies see only the Question trait;
//! budgets, evaluator paths, prompt rendering and persistence remain in Rust.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::budget::{BudgetSnapshot, SharedBudget};
use super::config::DiscoveryConfig;
use super::contracts::*;
use super::policy::{GridContext, Question, RoundSummary};
use super::replay::{
    CellMeta, IllegalBatchReason, MetaError, Observation, ProbeError, TerminationReason,
};
use super::store::DiscoveryStore;
use super::tree::{Direction, FailClass, NODE_SCHEMA, Node, WORLD_SCHEMA, World, WorldTree};
use super::workspace::{IntegrityGuard, prepare_attempt_workspace};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    pub goal: String,
    pub agent_id: String,
    pub runtime: String,
    pub model: String,
    pub evaluator: String,
    pub starting_workspace: PathBuf,
    pub direction: Direction,
    pub budget: RunBudget,
    pub branch_count: u32,
    pub refine_count: u32,
    pub max_parallelism: u32,
    pub attempt_timeout_secs: u64,
    pub max_turns: u32,
    #[serde(default)]
    pub beta: f64,
    #[serde(default = "default_dream_versions")]
    pub dream_versions: u32,
    pub policy_model: Option<String>,
}
fn default_dream_versions() -> u32 {
    3
}

/// Host-only attribution; request bodies cannot deserialize caller authority.
#[derive(Debug, Clone)]
pub struct RunIdentity {
    pub run_id: String,
    pub task_id: Option<String>,
    pub creator_id: String,
    pub creator_origin: String,
    pub approved_root_id: Option<String>,
}
impl RunIdentity {
    pub fn operator() -> Self {
        Self { run_id: uuid::Uuid::new_v4().to_string(), task_id: None,
            creator_id: "operator".into(), creator_origin: "operator".into(), approved_root_id: None }
    }
    fn validate(&self) -> Result<(), String> {
        if self.run_id.is_empty() || self.run_id.len() > 128
            || !self.run_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || self.creator_id.trim().is_empty()
            || !matches!(self.creator_origin.as_str(), "operator" | "manager" | "mcp" | "channel" | "rpc") {
            return Err("invalid trusted discovery identity".into());
        }
        Ok(())
    }
}
impl RunSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.goal.trim().is_empty()
            || self.model.trim().is_empty()
            || self.evaluator.trim().is_empty()
            || self.branch_count == 0
            || self.max_parallelism == 0
            || self.max_parallelism > 64
            || self.branch_count > 1000
            || self.refine_count > 1000
            || self.max_turns == 0
            || self.attempt_timeout_secs == 0
            || !self.beta.is_finite()
            || !(0.0..=1.0).contains(&self.beta)
            || self.dream_versions > 10
            || self.budget.max_rounds > 100
            || self.agent_id.is_empty()
            || self
                .agent_id
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        {
            return Err("invalid discovery run specification".into());
        }
        let planned_cells = u64::from(self.branch_count)
            * (u64::from(self.refine_count) + 1)
            * u64::from(self.budget.max_rounds);
        if planned_cells > super::tree::MAX_PLANNED_CELLS {
            return Err("discovery planned cells exceed the cumulative query size limit".into());
        }
        SharedBudget::new(self.budget).map(|_| ())
    }
}

pub struct OnlineComponents {
    pub runner: Arc<dyn AttemptRunner>,
    pub evaluator: Arc<dyn Evaluator>,
    pub policy: Arc<dyn PolicySource>,
    /// When present, this is the same source as `policy`, held concretely so
    /// the dream loop can install a winning version.
    pub dreaming: Option<Arc<super::policy_runner::ManagedPolicySource>>,
}

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub run_id: String,
    pub status: String,
    pub stop_reason: Option<String>,
    /// Public token from [`super::stop_code::STOP_CODES`]; the raw
    /// `stop_reason` stays private. Older reports lack it (readers default).
    pub stop_code: Option<String>,
    pub best_cell_id: Option<String>,
    pub best_score: Option<f64>,
    pub artifact: Option<PathBuf>,
    pub budget: BudgetSnapshot,
    pub rounds: Vec<RoundSummary>,
    pub policy_degraded: Option<String>,
    pub isolation_warning: Option<String>,
}

fn audit(home: &Path, agent: &str, event: &str, details: serde_json::Value) {
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            event,
            agent,
            duduclaw_security::audit::Severity::Info,
            details,
        ),
    );
}
fn check_maintenance(home: &Path, budget: &SharedBudget) -> Result<(), String> {
    super::maintenance::check_clean(home).inspect_err(|_| budget.cancel())
}
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
/// I1: the function has no budget, beta or policy argument. All content from
/// earlier attempts is data inside escaped fences, never prompt instructions.
pub fn render_attempt_prompt(
    goal: &str,
    ancestors: &[Node],
    visible: &[Node],
    earlier: &[Node],
) -> String {
    let mut prompt = format!(
        "Solve the goal in your current workspace. Produce the artifact there. Other workspaces are read-only.\n<goal>{}</goal>\n",
        xml(goal)
    );
    for (name, nodes) in [
        ("ancestor_chain", ancestors),
        ("visible_siblings", visible),
        ("earlier_rounds", earlier),
    ] {
        prompt.push_str(&format!("<{name}>\n"));
        for node in nodes {
            prompt.push_str(&format!(
                "<attempt id=\"{}\">score={:?}; valid={}; workspace={}\n{}\n{}</attempt>\n",
                xml(&node.cell_id),
                node.score,
                node.valid,
                xml(node.workspace.as_deref().unwrap_or("")),
                xml(node.proposal_excerpt.as_deref().unwrap_or("")),
                xml(node.error.as_deref().unwrap_or(""))
            ));
        }
        prompt.push_str(&format!("</{name}>\n"));
    }
    prompt
}
/// Scoring shares the absolute run deadline and cancellation authority.
async fn score_bounded(evaluator: &dyn Evaluator, req: &ScoreRequest, budget: &SharedBudget)
    -> ScoreOutcome {
    let remaining = budget.remaining_wall();
    let failed = || ScoreOutcome {
        evaluated: false, valid: false, score: None, fail_class: FailClass::Timeout,
        diagnostics: Some("run deadline exhausted during evaluation".into()),
        isolation: IsolationBackend::None, wall_secs: remaining.as_secs_f64(),
    };
    if remaining.is_zero() { return failed(); }
    let mut request = req.clone();
    request.timeout = Some(remaining);
    tokio::select! {
        result = tokio::time::timeout(remaining, evaluator.score(&request)) => result.unwrap_or_else(|_| failed()),
        _ = budget.cancelled() => failed(),
    }
}

fn append_node(path: &Path, node: &Node, isolation: IsolationBackend) -> Result<(), String> {
    use std::io::Write;
    let mut row = serde_json::to_value(node).map_err(|e| e.to_string())?;
    row["isolation_backend"] = serde_json::to_value(isolation).map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(&row).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    duduclaw_core::with_file_lock(path, || {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(&bytes)?;
        file.sync_data()
    })
    .map_err(|e| e.to_string())
}

#[derive(Clone)]
struct VerifiedCheckpoint {
    node: Node,
    artifact: PathBuf,
    guard: IntegrityGuard,
    isolation: IsolationBackend,
}
type SharedProgress = Arc<Mutex<Option<VerifiedCheckpoint>>>;

fn checkpoint(home: &Path, world_dir: &Path, config: &DiscoveryConfig, direction: Direction,
    progress: &SharedProgress, node: &Node, source_guard: &IntegrityGuard,
    isolation: IsolationBackend) -> Result<(), String> {
    let mut current = progress.lock().map_err(|_| "checkpoint lock unavailable")?;
    source_guard.verify().map_err(|e|format!("integrity_changed before checkpoint: {e}"))?;
    let improves = node.evaluated && node.valid && node.score.is_some_and(f64::is_finite)
        && current.as_ref().is_none_or(|prior| direction.orient(prior.node.score.unwrap()) < direction.orient(node.score.unwrap()));
    let candidate = if improves {
        let source = PathBuf::from(node.workspace.as_ref().ok_or("checkpoint workspace missing")?);
        let target = home.join("discovery/artifacts").join(&node.run_id).join(&node.cell_id);
        super::workspace::create_private_directory(&target).map_err(|e|e.to_string())?;
        let artifact = super::workspace::export_workspace(&source, &target.join("ws"), config)
            .map_err(|e|e.to_string())?;
        source_guard.verify_export(&artifact)
            .map_err(|e|format!("integrity_changed during checkpoint export: {e}"))?;
        let guard = IntegrityGuard::capture(&artifact).map_err(|e|e.to_string())?;
        Some(VerifiedCheckpoint { node: node.clone(), artifact, guard, isolation })
    } else { None };
    // Neither the durable valid node nor the shared best may become visible
    // before the exported content has been bound to the pre-score guard.
    source_guard.verify().map_err(|e|format!("integrity_changed before node commit: {e}"))?;
    let store = DiscoveryStore::open(home).map_err(|e|e.to_string())?;
    store.insert_nodes(std::slice::from_ref(node)).map_err(|e|e.to_string())?;
    store.set_node_isolation(&node.run_id, &node.cell_id, isolation).map_err(|e|e.to_string())?;
    if let Some(run) = store.load_run(&node.run_id).map_err(|e|e.to_string())? {
        if let Some(model) = run.configured_model {
            store.set_configured_model(&node.run_id, &node.cell_id, &model).map_err(|e|e.to_string())?;
        }
    }
    if let Some(candidate) = &candidate {
        candidate.guard.verify().map_err(|e|e.to_string())?;
        let hash = super::workspace::directory_sha256(&candidate.artifact).map_err(|e|e.to_string())?;
        candidate.guard.verify().map_err(|e|e.to_string())?;
        store.record_verified_artifact(&node.run_id, &node.cell_id, &hash).map_err(|e|e.to_string())?;
    }
    append_node(&world_dir.join("tree.jsonl"), node, isolation)?;
    if let Some(candidate) = candidate { *current = Some(candidate); }
    Ok(())
}

struct OnlineQuestion {
    home: PathBuf,
    run_dir: PathBuf,
    world_dir: PathBuf,
    spec: RunSpec,
    config: DiscoveryConfig,
    world: World,
    baseline: PathBuf,
    runner: Arc<dyn AttemptRunner>,
    evaluator: Arc<dyn Evaluator>,
    budget: SharedBudget,
    handle: tokio::runtime::Handle,
    nodes: BTreeMap<String, Node>,
    earlier: Vec<Node>,
    guards: Vec<IntegrityGuard>,
    progress: SharedProgress,
    seq: u64,
    decision_rounds: u64,
    unconfined: bool,
    stop_reason: Option<String>,
}
impl OnlineQuestion {
    fn cell(&self, branch: u32, attempt: u32) -> String {
        format!("r{}-b{branch}-a{attempt}", self.world.round)
    }
    fn frontier(&self) -> BTreeMap<u32, u32> {
        let mut out = BTreeMap::new();
        for node in self.nodes.values() {
            out.entry(node.branch)
                .and_modify(|k: &mut u32| *k = (*k).max(node.attempt))
                .or_insert(node.attempt);
        }
        out
    }
    fn legal(&self) -> Vec<String> {
        if self.stop_reason.is_some() {
            return vec![];
        }
        let frontier = self.frontier();
        let mut cells = (0..self.world.branch_count)
            .filter(|b| !frontier.contains_key(b))
            .map(|b| self.cell(b, 0))
            .collect::<Vec<_>>();
        cells.extend(
            frontier
                .into_iter()
                .filter(|(_, a)| *a < self.world.refine_count)
                .map(|(b, a)| self.cell(b, a + 1)),
        );
        cells
    }
    fn observation(&self, n: &Node) -> Observation {
        let parent_score = n
            .parent_id
            .as_ref()
            .and_then(|p| self.nodes.get(p))
            .and_then(|n| n.score);
        Observation {
            cell_id: n.cell_id.clone(),
            branch: n.branch,
            attempt: n.attempt,
            score: n.score.map(super::score::round9),
            evaluated: n.evaluated,
            valid: n.valid,
            fail_class: n.fail_class,
            error: n
                .error
                .as_deref()
                .map(|s| duduclaw_core::truncate_bytes(s, 300).into()),
            delta_vs_baseline: n
                .score
                .map(|s| super::score::round9(s - self.world.baseline_score)),
            delta_vs_parent: n
                .score
                .zip(parent_score)
                .map(|(s, p)| super::score::round9(s - p)),
            n_valid: None,
            n_total: None,
        }
    }
    fn ancestors(&self, branch: u32, attempt: u32) -> Vec<Node> {
        (0..attempt)
            .filter_map(|a| self.nodes.get(&self.cell(branch, a)).cloned())
            .collect()
    }
}
impl Question for OnlineQuestion {
    fn observed(&mut self) -> BTreeMap<String, Observation> {
        self.nodes
            .iter()
            .map(|(id, n)| (id.clone(), self.observation(n)))
            .collect()
    }
    fn legal_actions(&mut self) -> Vec<String> {
        self.legal()
    }
    fn legal_roots(&mut self) -> Vec<String> {
        let frontier = self.frontier();
        if self.stop_reason.is_some() {
            return vec![];
        }
        (0..self.world.branch_count)
            .filter(|b| !frontier.contains_key(b))
            .map(|b| self.cell(b, 0))
            .collect()
    }
    fn opened_branches(&mut self) -> Vec<u32> {
        self.frontier().keys().copied().collect()
    }
    fn meta(&mut self, cell: &str) -> Result<CellMeta, MetaError> {
        if let Some(n) = self.nodes.get(cell) {
            return Ok(CellMeta {
                branch: n.branch,
                attempt: n.attempt,
                parent_id: n.parent_id.clone(),
                seq: n.seq,
                tags: vec![],
            });
        }
        if !self.legal().iter().any(|id| id == cell) {
            return Err(MetaError(cell.into()));
        }
        for b in 0..self.world.branch_count {
            for a in 0..=self.world.refine_count {
                if self.cell(b, a) == cell {
                    return Ok(CellMeta {
                        branch: b,
                        attempt: a,
                        parent_id: if a > 0 {
                            Some(self.cell(b, a - 1))
                        } else {
                            None
                        },
                        seq: self.seq,
                        tags: vec![],
                    });
                }
            }
        }
        Err(MetaError(cell.into()))
    }
    fn baseline_score(&self) -> f64 {
        self.world.baseline_score
    }
    fn max_parallelism(&self) -> u32 {
        self.world.max_parallelism
    }
    fn probe_batch(&mut self, cells: &[String]) -> Result<Vec<Observation>, ProbeError> {
        if self.stop_reason.is_some() {
            return Err(ProbeError::Terminated(TerminationReason::K2Reached));
        }
        if cells.is_empty() {
            return Err(IllegalBatchReason::Empty.into());
        }
        if cells.len() > self.world.max_parallelism as usize {
            return Err(IllegalBatchReason::TooLarge {
                len: cells.len(),
                max: self.world.max_parallelism,
            }
            .into());
        }
        let legal = self.legal();
        let mut unique = BTreeSet::new();
        for cell in cells {
            if !unique.insert(cell) {
                return Err(IllegalBatchReason::Duplicate(cell.clone()).into());
            }
            if !legal.iter().any(|id| id == cell) {
                return Err(IllegalBatchReason::NotLegal(cell.clone()).into());
            }
        }
        if let Err(e) = self.guards.iter().try_for_each(IntegrityGuard::verify) {
            self.stop_reason = Some(format!("integrity_changed: {e}"));
            audit(
                &self.home,
                &self.spec.agent_id,
                "discovery_tamper",
                serde_json::json!({"run_id":self.world.run_id}),
            );
            return Err(ProbeError::Terminated(TerminationReason::K2Reached));
        }
        let visible = self.nodes.values().cloned().collect::<Vec<_>>();
        let mut jobs = Vec::new();
        for cell in cells {
            let meta = self
                .meta(cell)
                .map_err(|_| IllegalBatchReason::NotLegal(cell.clone()))?;
            let ancestors = self.ancestors(meta.branch, meta.attempt);
            let visible = visible.iter().filter(|n| n.branch != meta.branch).cloned().collect::<Vec<_>>();
            let visible_ids = visible.iter().map(|n|n.cell_id.clone()).collect::<Vec<_>>();
            let parent = ancestors
                .last()
                .and_then(|n| n.workspace.as_ref())
                .map(PathBuf::from)
                .unwrap_or_else(|| self.baseline.clone());
            let container = self
                .world_dir
                .join(format!("b{}", meta.branch))
                .join(format!("a{}", meta.attempt));
            if let Err(e) = super::workspace::create_private_directory(&container) {
                self.stop_reason = Some(e.to_string());
                break;
            }
            let request = AttemptRequest {
                run_id: self.world.run_id.clone(),
                cell_id: cell.clone(),
                node_dir: container.join("ws"),
                run_dir: self.run_dir.clone(),
                read_workspaces: ancestors.iter().chain(visible.iter()).chain(self.earlier.iter())
                    .filter_map(|n| n.workspace.as_ref().map(PathBuf::from))
                    .collect::<BTreeSet<_>>().into_iter().collect(),
                prompt: render_attempt_prompt(&self.spec.goal, &ancestors, &visible, &self.earlier),
                agent_id: self.spec.agent_id.clone(),
                model: Some(self.spec.model.clone()),
                timeout: Duration::from_secs(self.spec.attempt_timeout_secs)
                    .min(self.budget.remaining_wall()),
                max_turns: self.spec.max_turns,
                account_pool: self.config.account_pool.clone(),
            };
            let seq = self.seq;
            self.seq += 1;
            jobs.push((meta, request, parent, seq, visible_ids.clone()));
        }
        self.decision_rounds += 1;
        let config = &self.config;
        let runner = &self.runner;
        let evaluator = &self.evaluator;
        let evaluator_name = &self.spec.evaluator;
        let budget = &self.budget;
        let guards = &self.guards;
        let home = &self.home;
        let world_dir = &self.world_dir;
        let progress = &self.progress;
        let direction = self.spec.direction;
        let agent_id = &self.spec.agent_id;
        let jobs = self.handle.block_on(async {
            futures_util::future::join_all(jobs.into_iter().map(
                |(meta, req, parent, seq, visible_set)| async move {
                    check_maintenance(home, budget)?;
                    let prepared =
                        prepare_attempt_workspace(&parent, &req.node_dir, &req.run_dir, config)
                            .map_err(|e| e.to_string())?;
                    if budget.remaining_wall().is_zero() {
                        return Err("budget_exhausted".into());
                    }
                    let started = chrono::Utc::now().to_rfc3339();
                    let outcome = runner.run_attempt(&req).await.map_err(|e| e.to_string())?;
                    // Verify all previously completed workspaces after every attempt,
                    // including unconfined runs. Evidence is kept outside the run.
                    prepared
                        .guard
                        .verify()
                        .map_err(|e| format!("integrity_changed: {e}"))?;
                    guards
                        .iter()
                        .try_for_each(IntegrityGuard::verify)
                        .map_err(|e| format!("integrity_changed: {e}"))?;
                    let guard = IntegrityGuard::capture(&req.node_dir).map_err(|e|e.to_string())?;
                    check_maintenance(home, budget)?;
                    let verdict = score_bounded(evaluator.as_ref(), &ScoreRequest {
                            run_id: req.run_id.clone(),
                            timeout: None,
                            cell_id: req.cell_id.clone(),
                            node_dir: req.node_dir.clone(),
                            evaluator: evaluator_name.clone(),
                        }, budget).await;
                    guard.verify().map_err(|e|format!("integrity_changed during scoring: {e}"))?;
                    let node = Node {
                        schema: NODE_SCHEMA.into(),
                        run_id: req.run_id,
                        round: req
                            .cell_id
                            .strip_prefix('r')
                            .and_then(|s| s.split('-').next())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0),
                        cell_id: req.cell_id,
                        parent_id: meta.parent_id,
                        branch: meta.branch,
                        attempt: meta.attempt,
                        seq,
                        dispatched_at: Some(started),
                        finished_at: Some(chrono::Utc::now().to_rfc3339()),
                        evaluated: verdict.evaluated,
                        valid: verdict.valid,
                        score: verdict.score,
                        fail_class: if outcome.timed_out && !verdict.valid {
                            FailClass::Timeout
                        } else {
                            verdict.fail_class
                        },
                        error: verdict.diagnostics,
                        visible_set,
                        cost: outcome.cost,
                        runtime: Some(outcome.runtime),
                        model: (!outcome.model.is_empty()).then_some(outcome.model),
                        workspace: Some(req.node_dir.to_string_lossy().into()),
                        proposal_excerpt: Some(
                            duduclaw_core::truncate_bytes(&outcome.final_text, 500).into(),
                        ),
                    };
                    if budget.is_cancelled() { return Err("budget_exhausted".into()); }
                    // Commit each finished job immediately. A slow sibling must
                    // not hide completed evidence behind join_all's barrier.
                    check_maintenance(home, budget)?;
                    checkpoint(home, world_dir, config, direction, progress, &node, &guard, outcome.isolation)?;
                    audit(home, agent_id, "discovery_node_scored",
                        serde_json::json!({"run_id":node.run_id,"cell_id":node.cell_id,"score":node.score,"valid":node.valid,"isolation":outcome.isolation}));
                    Ok::<_, String>((node, outcome.isolation, guard))
                },
            ))
            .await
        });
        let mut observations = Vec::new();
        for result in jobs {
            if self.budget.is_cancelled() { self.stop_reason = Some("budget_exhausted".into()); break; }
            match result {
                Ok((node, isolation, guard)) => {
                    self.unconfined |= isolation == IsolationBackend::None;
                    self.nodes.insert(node.cell_id.clone(), node.clone());
                    self.guards.push(guard);
                    observations.push(self.observation(&node));
                }
                Err(error) => self.stop_reason = Some(error),
            }
        }
        if observations.len() != cells.len() {
            return Err(ProbeError::Terminated(TerminationReason::K2Reached));
        }
        Ok(observations)
    }
}

struct RunTerminalGuard { home: PathBuf, run_id: String, run_dir: PathBuf,
    budget: SharedBudget, finished: bool }
impl Drop for RunTerminalGuard {
    fn drop(&mut self) {
        self.budget.cancel();
        if !self.finished {
            if let Ok(store) = DiscoveryStore::open(&self.home) {
                let _ = store.finish_run(&self.run_id, "interrupted", None);
            }
            let _ = std::fs::write(self.run_dir.join(".completed"), "interrupted");
        }
    }
}

/// Run within an already authorized operator session. Callers never receive
/// an authority flag from TOML or model-provided parameters.
pub async fn run(
    home: PathBuf,
    config: DiscoveryConfig,
    spec: RunSpec,
    components: OnlineComponents,
    budget: SharedBudget,
    scorer_hash: String,
) -> Result<RunReport, String> {
    let lease = super::maintenance::OperatorLeaseGuard::acquire(&home)
        .inspect_err(|_| budget.cancel())?;
    run_with_lease(home, config, spec, components, budget, scorer_hash, lease).await
}

pub async fn run_with_lease(
    home: PathBuf, config: DiscoveryConfig, spec: RunSpec,
    components: OnlineComponents, budget: SharedBudget, scorer_hash: String,
    lease: super::maintenance::OperatorLeaseGuard,
) -> Result<RunReport, String> {
    run_with_lease_and_identity(home, config, spec, components, budget, scorer_hash,
        lease, RunIdentity::operator()).await
}

pub async fn run_with_identity(
    home: PathBuf, config: DiscoveryConfig, spec: RunSpec,
    components: OnlineComponents, budget: SharedBudget, scorer_hash: String,
    identity: RunIdentity,
) -> Result<RunReport, String> {
    let lease = super::maintenance::OperatorLeaseGuard::acquire(&home)?;
    run_with_lease_and_identity(home, config, spec, components, budget, scorer_hash,
        lease, identity).await
}

pub async fn run_with_lease_and_identity(
    home: PathBuf, config: DiscoveryConfig, spec: RunSpec,
    components: OnlineComponents, budget: SharedBudget, scorer_hash: String,
    lease: super::maintenance::OperatorLeaseGuard, identity: RunIdentity,
) -> Result<RunReport, String> {
    identity.validate()?;
    let home = super::workspace::canonical_real_directory(&home).map_err(|e| e.to_string())?;
    lease.check_home(&home)?;
    lease.bind_budget(budget.clone())?;
    // Public requests can create discovery.db before the first episode. The
    // shared retention sweep still needs its private, initially empty root.
    super::workspace::create_private_directory(&home.join("discovery/runs")).map_err(|e|e.to_string())?;
    super::maintenance::reconcile_with_lease(&home, &lease)
        .inspect_err(|_| budget.cancel())?;
    check_maintenance(&home, &budget)?;
    spec.validate()?;
    let run_id = identity.run_id.clone();
    super::workspace::create_private_directory(&home.join("discovery")).map_err(|e| e.to_string())?;
    let run_dir = home.join("discovery/runs").join(&run_id);
    super::workspace::create_private_directory(&run_dir).map_err(|e| e.to_string())?;
    let store = DiscoveryStore::open(&home).map_err(|e| e.to_string())?;
    store
        .create_run(
            &run_id,
            &spec.goal,
            &spec.agent_id,
            &spec.evaluator,
            &scorer_hash,
            spec.direction,
            &spec.budget,
        )
        .map_err(|e| e.to_string())?;
    let mut terminal = RunTerminalGuard { home: home.clone(), run_id: run_id.clone(),
        run_dir: run_dir.clone(), budget: budget.clone(), finished: false };
    store.attach_run_identity(&identity).map_err(|e| e.to_string())?;
    budget.bind_run(&home, &run_id)?;
    // Aliases (`agy`) are recorded under the canonical family name.
    let runtime = super::attempt_adapter::RuntimeFamily::parse(&spec.runtime).map(|family| family.name()).unwrap_or(spec.runtime.as_str());
    store.set_run_runtime(&run_id, runtime, &spec.model).map_err(|e| e.to_string())?;
    audit(
        &home,
        &spec.agent_id,
        "discovery_created",
        serde_json::json!({"run_id":run_id,"evaluator":spec.evaluator,"scorer_hash":scorer_hash,"workspace":spec.starting_workspace,"budget":spec.budget,"branch_count":spec.branch_count,"refine_count":spec.refine_count,"creator":identity.creator_id,"creator_origin":identity.creator_origin,"task_id":identity.task_id,"approved_root_id":identity.approved_root_id}),
    );
    let initial = run_dir.join("baseline");
    super::workspace::create_private_directory(&initial).map_err(|e| e.to_string())?;
    let prepared = prepare_attempt_workspace(
        &spec.starting_workspace,
        &initial.join("ws"),
        &run_dir,
        &config,
    )
    .map_err(|e| e.to_string())?;
    lease.check_home(&home)?;
    check_maintenance(&home, &budget)?;
    let baseline = score_bounded(components.evaluator.as_ref(), &ScoreRequest {
            run_id: run_id.clone(),
            timeout: None,
            cell_id: "baseline".into(),
            node_dir: prepared.workspace.clone(),
            evaluator: spec.evaluator.clone(),
        }, &budget).await;
    lease.check_home(&home)?;
    check_maintenance(&home, &budget)?;
    if baseline.isolation == IsolationBackend::None {
        store.mark_unconfined(&run_id).map_err(|e| e.to_string())?;
    }
    if !baseline.evaluated || !baseline.valid || baseline.score.is_none() {
        store
            .finish_run(&run_id, "failed", None)
            .map_err(|e| e.to_string())?;
        std::fs::write(run_dir.join(".completed"), "failed").map_err(|e| e.to_string())?;
        terminal.finished = true;
        return Err("starting workspace has no valid baseline score".into());
    }
    let baseline_guard = IntegrityGuard::capture(&prepared.workspace).map_err(|e| e.to_string())?;
    let mut history = Vec::new();
    let mut worlds = Vec::new();
    let mut all_nodes = Vec::new();
    let progress: SharedProgress = Arc::new(Mutex::new(None));
    let mut seq = 0;
    let mut planned_cells = 0u64;
    let mut stop = None;
    let mut guards = vec![prepared.guard, baseline_guard];
    let mut isolation_warning = None;
    let mut degraded = components.policy.degraded().map(|e| e.to_string());
    for round in 1..=spec.budget.max_rounds {
        let beta = components.policy.live_beta(spec.beta);
        lease.check_home(&home)?;
        check_maintenance(&home, &budget)?;
        if budget.remaining_wall().is_zero()
            || budget.snapshot().agent_calls >= spec.budget.max_agent_calls
            || budget.snapshot().spent_usd >= spec.budget.max_usd
        {
            stop = Some("budget_exhausted".into());
            break;
        }
        if let Some(source) = components.dreaming.as_ref() {
            source.set_online_timeout(budget.remaining_wall());
        }
        let mut policy = match components.policy.instantiate(beta) {
            Ok(policy) => policy,
            Err(error) => {
                degraded = Some(error.to_string());
                Box::new(super::policy::BaselineParallelRefine) as Box<dyn super::policy::ExplorationPolicy + Send>
            }
        };
        let (affordable_width, affordable_refine) = super::workspace::affordable_grid(
            &prepared.workspace, &run_dir, &config, spec.branch_count, spec.refine_count)
            .map_err(|e|e.to_string())?;
        let ctx = GridContext {
            history: history.clone(),
            hard_max_branch_count: affordable_width,
            hard_max_refine_count: affordable_refine,
            worker_cap: spec.max_parallelism,
            trace_branch_count: None,
            trace_refine_count: None,
        };
        let context = ctx.clone();
        let planning = tokio::task::spawn_blocking(move || {
            let plan = policy.plan_grid(&context);
            (policy, plan)
        });
        let planned = tokio::select! {
            result = tokio::time::timeout(budget.remaining_wall(), planning) => Some(result),
            _ = budget.cancelled() => None,
        };
        lease.check_home(&home)?;
        check_maintenance(&home, &budget)?;
        let Some(planned) = planned else { stop = Some("budget_exhausted".into()); break; };
        let (mut policy, plan) = match planned {
            Ok(Ok((policy, Ok(plan)))) => (policy, plan),
            Ok(Ok((_, Err(error)))) => {
                degraded = Some(format!("policy_plan_failed: {error}"));
                let mut baseline: Box<dyn super::policy::ExplorationPolicy + Send> =
                    Box::new(super::policy::BaselineParallelRefine);
                let plan = baseline.plan_grid(&ctx).map_err(|e| e.to_string())?;
                (baseline, plan)
            }
            Ok(Err(error)) => { stop = Some(format!("policy_plan_failed: {error}")); break; }
            Err(_) => { stop = Some("budget_exhausted".into()); break; }
        };
        degraded = components.policy.degraded().map(|e|e.to_string()).or(degraded);
        if plan.branch_count == 0
            || plan.branch_count > affordable_width
            || plan.refine_count > affordable_refine
        {
            stop = Some("policy proposed a grid outside its hard limits".into());
            break;
        }
        let round_cells = u64::from(plan.branch_count) * (u64::from(plan.refine_count) + 1);
        let Some(cumulative_cells) = planned_cells.checked_add(round_cells)
            .filter(|count| *count <= super::tree::MAX_PLANNED_CELLS) else {
            stop = Some("policy proposed a grid beyond the cumulative query size limit".into());
            break;
        };
        planned_cells = cumulative_cells;
        let world = World {
            schema: WORLD_SCHEMA.into(),
            run_id: run_id.clone(),
            round,
            direction: spec.direction,
            baseline_score: baseline.score.unwrap(),
            branch_count: plan.branch_count,
            refine_count: plan.refine_count,
            max_parallelism: spec.max_parallelism,
            policy_id: policy.id().into(),
            beta: beta,
        };
        let world_dir = run_dir.join(format!("r{round}"));
        super::workspace::create_private_directory(&world_dir).map_err(|e| e.to_string())?;
        std::fs::write(
            world_dir.join("world.json"),
            serde_json::to_vec_pretty(&world).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        store.insert_world(&world).map_err(|e| e.to_string())?;
        let current = components.policy.current();
        let frozen_version = PolicyVersion {
            policy_id: world.policy_id.clone(),
            source_sha256: (current.policy_id == world.policy_id).then_some(current.source_sha256).flatten(),
        };
        let frozen_source = frozen_version.source_sha256.as_ref()
            .and_then(|_| components.dreaming.as_ref()?.current_source());
        store.freeze_round_plan_with_origin(&world, &frozen_version, frozen_source.as_deref(), &plan,
                &components.dreaming.as_ref().map_or_else(Vec::new, |source| source.origin_task_ids()))
            .map_err(|e| e.to_string())?;
        let handle = tokio::runtime::Handle::current();
        let q_home = home.clone();
        let q_run = run_dir.clone();
        let q_spec = spec.clone();
        let q_config = config.clone();
        let q_budget = budget.clone();
        let runner = components.runner.clone();
        let evaluator = components.evaluator.clone();
        let earlier = all_nodes.clone();
        let baseline_dir = prepared.workspace.clone();
        let round_guards = guards.clone();
        let round_progress = progress.clone();
        let solving = tokio::task::spawn_blocking(move || {
            let mut question = OnlineQuestion {
                home: q_home.clone(),
                run_dir: q_run,
                world_dir,
                spec: q_spec,
                config: q_config,
                world,
                baseline: baseline_dir,
                runner,
                evaluator,
                budget: q_budget,
                handle,
                nodes: BTreeMap::new(),
                earlier,
                guards: round_guards,
                progress: round_progress,
                seq,
                decision_rounds: 0,
                unconfined: false,
                stop_reason: None,
            };
            let result = policy.solve(&mut question);
            Ok::<_, String>((question, result))
        });
        let solved = tokio::select! {
            result = tokio::time::timeout(budget.remaining_wall(), solving) => Some(result),
            _ = budget.cancelled() => None,
        };
        lease.check_home(&home)?;
        check_maintenance(&home, &budget)?;
        let Some(solved) = solved else { stop = Some("budget_exhausted".into()); break; };
        let (question, result) = match solved {
            Ok(result) => result.map_err(|e| e.to_string())??,
            Err(_) => {
                budget.cancel();
                stop = Some("budget_exhausted".into());
                break;
            }
        };
        if question.unconfined {
            isolation_warning = Some("experimental unconfined attempts: no OS isolation or resource ceiling".into());
        }
        seq = question.seq;
        guards = question.guards;
        let nodes = question.nodes.into_values().collect::<Vec<_>>();
        store.finish_round(&question.world).map_err(|e| e.to_string())?;
        if question.unconfined {
            store.mark_unconfined(&run_id).map_err(|e| e.to_string())?;
        }
        history.push(RoundSummary {
            round,
            planned_branch_count: plan.branch_count,
            planned_refine_count: plan.refine_count,
            actual_branch_count: nodes
                .iter()
                .map(|n| n.branch)
                .collect::<BTreeSet<_>>()
                .len() as u32,
            actual_refine_count: nodes.iter().map(|n| n.attempt).max().unwrap_or(0),
            probes: nodes.len() as u64,
            decision_rounds: question.decision_rounds,
            best_score: nodes.iter().filter_map(|n| n.score).max_by(|a, b| {
                spec.direction
                    .orient(*a)
                    .total_cmp(&spec.direction.orient(*b))
            }),
            beta: beta,
        });
        all_nodes.extend(nodes.clone());
        if !nodes.is_empty() {
            worlds.push(WorldTree::new(question.world, nodes).map_err(|e| e.to_string())?);
        }
        if question.stop_reason.is_some() {
            stop = question.stop_reason;
            break;
        }
        if let Err(e) = result {
            degraded = Some(format!("policy_failed: {e}"));
            if let Some(source) = components.dreaming.as_ref() {
                source.degrade(PolicyDegraded::Rejected(e.to_string()));
            }
        }
        degraded = components.policy.degraded().map(|e|e.to_string()).or(degraded);
        if round < spec.budget.max_rounds && spec.dream_versions > 0 && degraded.is_none() {
            if let Some(source) = components.dreaming.as_ref() {
                let dream_root = home
                    .join("discovery/policy-development")
                    .join(&run_id)
                    .join(format!("r{round}"));
                super::workspace::create_private_directory(&dream_root).map_err(|e| e.to_string())?;
                let request = super::dream::DreamRequest {
                    home_dir: home.clone(),
                    run_dir: dream_root,
                    run_id: run_id.clone(),
                    round,
                    agent_id: spec.agent_id.clone(),
                    model: Some(spec
                        .policy_model
                        .clone()
                        .unwrap_or_else(|| spec.model.clone())),
                    account_pool: config.account_pool.clone(),
                    versions: spec.dream_versions,
                    max_turns: spec.max_turns,
                    timeout: Duration::from_secs(spec.attempt_timeout_secs),
                    round_probe_cap: spec.branch_count.saturating_mul(spec.refine_count + 1),
                };
                if let Err(e) = super::dream::dream(
                    source,
                    components.runner.as_ref(),
                    &budget,
                    &worlds,
                    &request,
                )
                .await
                {
                    degraded = Some(format!("dream_failed: {e}"));
                }
            }
        }
    }
    // Every improvement has already been evaluated and copied to a private
    // checkpoint before its sibling batch completes. Deadline exhaustion can
    // therefore deliver the last verified checkpoint without another call.
    lease.check_home(&home)?;
    check_maintenance(&home, &budget)?;
    let saved = progress.lock().map_err(|_| "checkpoint lock unavailable")?.clone();
    let mut artifact = None;
    let mut best_score = None;
    let mut best_id = None;
    if let Some(saved) = saved {
        if saved.isolation == IsolationBackend::None {
            isolation_warning = Some("experimental unconfined attempts: no OS isolation or resource ceiling".into());
        }
        saved.guard.verify().map_err(|e|e.to_string())?;
        let mut deliver = true;
        let mut delivered_score = saved.node.score;
        if !budget.remaining_wall().is_zero() {
            let verdict = score_bounded(components.evaluator.as_ref(), &ScoreRequest {
                run_id: run_id.clone(), timeout: None, cell_id: saved.node.cell_id.clone(),
                node_dir: saved.artifact.clone(), evaluator: spec.evaluator.clone(),
            }, &budget).await;
            if verdict.evaluated {
                deliver = verdict.valid && verdict.score.is_some_and(f64::is_finite);
                if deliver { delivered_score = verdict.score; }
                if !deliver { stop = Some("winner failed its final evaluation".into()); }
            } else if budget.remaining_wall().is_zero() {
                stop = Some("budget_exhausted".into());
            } else {
                deliver = false;
                stop = Some("winner evaluator unavailable".into());
            }
        }
        saved.guard.verify().map_err(|e|e.to_string())?;
        lease.check_home(&home)?;
        check_maintenance(&home, &budget)?;
        if deliver {
            best_id = Some(saved.node.cell_id.clone());
            best_score = delivered_score;
            artifact = Some(saved.artifact);
            audit(&home, &spec.agent_id, "discovery_artifact_exported",
                serde_json::json!({"run_id":run_id,"cell_id":best_id,"score":best_score,"artifact":artifact,"verified_checkpoint":true}));
        }
    }
    let status = if budget.rate_limit_stopped() {
        stop = Some("rate_limit".into());
        "rate_limited"
    } else if stop
        .as_ref()
        .is_some_and(|s| s == "budget_exhausted" || s == "agent budget exhausted")
    {
        audit(
            &home,
            &spec.agent_id,
            "discovery_budget_exhausted",
            serde_json::json!({"run_id":run_id,"budget":budget.snapshot()}),
        );
        "budget_exhausted"
    } else if stop.is_some() || degraded.is_some() || isolation_warning.is_some() {
        "degraded"
    } else if artifact.is_some() {
        "complete"
    } else {
        "failed"
    };
    lease.check_home(&home)?;
    check_maintenance(&home, &budget)?;
    budget.persist_snapshot()?;
    store
        .finish_run(&run_id, status, best_id.as_deref())
        .map_err(|e| e.to_string())?;
    store.verify_run_provenance(&run_id).map_err(|e| e.to_string())?;
    terminal.finished = true;
    std::fs::write(run_dir.join(".completed"), status).map_err(|e|e.to_string())?;
    let stop_code = super::stop_code::classify(status, stop.as_deref()).map(str::to_owned);
    let report = RunReport {
        run_id: run_id.clone(),
        status: status.into(),
        stop_reason: stop,
        stop_code,
        best_cell_id: best_id,
        best_score,
        artifact,
        budget: budget.snapshot(),
        rounds: history,
        policy_degraded: degraded,
        isolation_warning,
    };
    let reports = home.join("discovery/reports");
    super::workspace::create_private_directory(&reports).map_err(|e| e.to_string())?;
    std::fs::write(
        reports.join(format!("{run_id}.json")),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    lease.check_home(&home)?;
    check_maintenance(&home, &budget)?;
    terminal.finished = true;
    Ok(report)
}

#[cfg(test)]
#[path = "tests_online.rs"]
mod tests;

#[cfg(test)]
mod lease_tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct LeasePolicy;
    impl PolicySource for LeasePolicy {
        fn current(&self) -> PolicyVersion {
            PolicyVersion { policy_id: super::super::policy::BASELINE_POLICY_ID.into(), source_sha256: None }
        }
        fn instantiate(&self, _: f64) -> Result<Box<dyn super::super::policy::ExplorationPolicy + Send>, PolicyDegraded> {
            Ok(Box::new(super::super::policy::BaselineParallelRefine))
        }
        fn degraded(&self) -> Option<PolicyDegraded> { None }
    }
    struct LeaseRunner(SharedBudget);
    #[async_trait]
    impl AttemptRunner for LeaseRunner {
        async fn run_attempt(&self, req: &AttemptRequest) -> Result<AttemptOutcome, AttemptInfraError> {
            let call = self.0.reserve_call()?;
            std::fs::write(req.node_dir.join("score.txt"), "2").unwrap();
            self.0.finish_call(call, 0.05);
            Ok(AttemptOutcome {
                cost: super::super::tree::NodeCost { usd: 0.05, ..Default::default() },
                runtime: "fake".into(), model: "fake".into(), isolation: IsolationBackend::None,
                final_text: "ready".into(), timed_out: false, infra_retries: 0,
            })
        }
    }
    struct RevokingScorer { home: PathBuf, calls: AtomicUsize }
    #[async_trait]
    impl Evaluator for RevokingScorer {
        async fn score(&self, req: &ScoreRequest) -> ScoreOutcome {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            if index == 2 {
                duduclaw_core::concurrency_gate::release_class(&self.home, "discovery-operator");
            }
            ScoreOutcome {
                evaluated: true, valid: true,
                score: Some(std::fs::read_to_string(req.node_dir.join("score.txt")).unwrap().parse().unwrap()),
                fail_class: FailClass::Ok, diagnostics: None,
                isolation: IsolationBackend::None, wall_secs: 0.,
            }
        }
    }
    #[tokio::test]
    async fn revoked_operator_cannot_deliver_a_valid_checkpoint_after_final_scoring() {
        let home = tempfile::tempdir().unwrap();
        let seed = home.path().join("seed");
        std::fs::create_dir(&seed).unwrap();
        std::fs::write(seed.join("score.txt"), "1").unwrap();
        let limits = RunBudget { max_agent_calls: 1, max_usd: 1., max_wall_secs: 30, max_rounds: 1 };
        let budget = SharedBudget::new(limits).unwrap();
        let lease = super::super::maintenance::OperatorLeaseGuard::acquire(home.path()).unwrap();
        let spec = RunSpec {
            goal: "test lease fence".into(), agent_id: "test".into(), runtime: "fake".into(),
            model: "fake".into(), evaluator: "score".into(), starting_workspace: seed.clone(),
            direction: Direction::Max, budget: limits, branch_count: 1, refine_count: 0,
            max_parallelism: 1, attempt_timeout_secs: 2, max_turns: 2, beta: 0.6,
            dream_versions: 0, policy_model: None,
        };
        let result = run_with_lease(home.path().into(), DiscoveryConfig {
                approved_workspace_roots: vec![seed], ..Default::default()
            }, spec, OnlineComponents {
                runner: Arc::new(LeaseRunner(budget.clone())),
                evaluator: Arc::new(RevokingScorer { home: home.path().into(), calls: AtomicUsize::new(0) }),
                policy: Arc::new(LeasePolicy), dreaming: None,
            }, budget.clone(), "hash".into(), lease).await;
        assert!(result.is_err(), "a lost operator lease must refuse checkpoint delivery");
        assert!(budget.is_cancelled());
    }
}
