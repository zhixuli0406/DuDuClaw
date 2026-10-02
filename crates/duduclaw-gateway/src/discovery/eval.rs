//! In-process replay evaluation of a policy over one world (SPEC §4-§5):
//! for every beta of the fixed grid, build a fresh policy, ask `plan_grid`,
//! let `solve` drive a [`Replay`], then score the sweep.

use super::policy::{ExplorationPolicy, GridContext, PolicyConfig, PolicyError, RoundSummary};
use super::replay::{DEFAULT_K2, ProbeError, Replay, ReplayTrace};
use super::score::{
    BETA_GRID, PointScore, RawPoint, WorldScore, attainment, context_mismatch_rate,
    parallel_penalty, round9, work,
};
use super::tree::WorldTree;

/// Host-side limits and planning context of a replay evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayConfig {
    pub hard_max_branch_count: u32,
    pub hard_max_refine_count: u32,
    pub worker_cap: u32,
    pub k2: u32,
    /// Two consecutive `illegal_batch` results invalidate the policy.
    pub max_consecutive_illegal: u32,
    pub history: Vec<RoundSummary>,
}

impl ReplayConfig {
    /// Hard maxima equal to the world's own grid, worker cap equal to its
    /// `max_parallelism`, `K2 = 1000`, empty history.
    pub fn for_world(tree: &WorldTree) -> Self {
        let w = tree.world();
        Self {
            hard_max_branch_count: w.branch_count,
            hard_max_refine_count: w.refine_count,
            worker_cap: w.max_parallelism,
            k2: DEFAULT_K2,
            max_consecutive_illegal: 2,
            history: Vec::new(),
        }
    }

    /// The `plan_grid` context for replaying `tree`.
    pub fn grid_context(&self, tree: &WorldTree) -> GridContext {
        GridContext {
            history: self.history.clone(),
            hard_max_branch_count: self.hard_max_branch_count,
            hard_max_refine_count: self.hard_max_refine_count,
            worker_cap: self.worker_cap,
            trace_branch_count: Some(tree.world().branch_count),
            trace_refine_count: Some(tree.world().refine_count),
        }
    }
}

/// Evaluation failure: the policy version is invalid for this world.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("beta {beta}: {source}")]
    Policy {
        beta: f64,
        #[source]
        source: PolicyError,
    },
    #[error("beta {beta}: {count} consecutive illegal batches")]
    ConsecutiveIllegal { beta: f64, count: u32 },
}

/// Result of one beta point: the trace plus its plan.
#[derive(Debug, Clone)]
pub struct PointRun {
    pub beta: f64,
    pub plan: super::policy::GridPlan,
    pub trace: ReplayTrace,
}

/// Run one (policy, world, beta) replay.
pub fn run_point(
    policy: &mut dyn ExplorationPolicy,
    tree: &WorldTree,
    cfg: &ReplayConfig,
    beta: f64,
) -> Result<PointRun, EvalError> {
    let plan = policy
        .plan_grid(&cfg.grid_context(tree))
        .map_err(|source| EvalError::Policy { beta, source })?;
    let mut replay = Replay::new(tree, cfg.k2);
    match policy.solve(&mut replay) {
        Ok(()) | Err(PolicyError::Probe(ProbeError::Terminated(_))) => {}
        Err(source) => return Err(EvalError::Policy { beta, source }),
    }
    let trace = replay.finish();
    if trace.max_consecutive_illegal >= cfg.max_consecutive_illegal {
        return Err(EvalError::ConsecutiveIllegal {
            beta,
            count: trace.max_consecutive_illegal,
        });
    }
    Ok(PointRun { beta, plan, trace })
}

/// Score one finished point. Returns the emitted (rounded) point and the
/// unrounded values the aggregate is computed from.
pub fn score_point(tree: &WorldTree, run: &PointRun) -> (PointScore, RawPoint) {
    let t = &run.trace;
    let revealed = t.batches.iter().flatten().map(String::as_str);
    let raw = RawPoint {
        attainment: attainment(tree, revealed),
        work: work(t.counters.probes, tree.total_cells()),
        parallel_penalty: parallel_penalty(
            t.counters.effective_sequential_rounds,
            t.counters.probes,
        ),
    };
    let point = PointScore {
        beta: round9(run.beta),
        probes: t.counters.probes,
        decision_rounds: t.counters.decision_rounds,
        effective_sequential_rounds: t.counters.effective_sequential_rounds,
        attainment: round9(raw.attainment),
        work: round9(raw.work),
        parallel_penalty: round9(raw.parallel_penalty),
        context_mismatch_rate: round9(context_mismatch_rate(
            t.context_mismatches,
            t.counters.probes,
        )),
        batches: t.batches.clone(),
    };
    (point, raw)
}

/// Whether a plan asks for more than the world holds (SPEC §5).
pub fn plan_out_of_support(plan: &super::policy::GridPlan, tree: &WorldTree) -> bool {
    plan.branch_count > tree.world().branch_count || plan.refine_count > tree.world().refine_count
}

/// Evaluate a policy over one world across the five-beta sweep. `make`
/// builds a fresh policy instance for each beta (the `__init__(config)`
/// step). The world is out of support when any beta's plan exceeds it.
pub fn evaluate_world(
    make: &dyn Fn(&PolicyConfig) -> Box<dyn ExplorationPolicy>,
    tree: &WorldTree,
    cfg: &ReplayConfig,
) -> Result<WorldScore, EvalError> {
    let mut points = Vec::with_capacity(BETA_GRID.len());
    let mut raws = Vec::with_capacity(BETA_GRID.len());
    let mut out_of_support = false;
    for beta in BETA_GRID {
        let mut policy = make(&PolicyConfig { beta });
        let run = run_point(policy.as_mut(), tree, cfg, beta)?;
        out_of_support |= plan_out_of_support(&run.plan, tree);
        let (point, raw) = score_point(tree, &run);
        points.push(point);
        raws.push(raw);
    }
    let w = tree.world();
    Ok(WorldScore::aggregate(
        w.run_id.clone(),
        w.round,
        points,
        &raws,
        out_of_support,
    ))
}
