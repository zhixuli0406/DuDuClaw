//! Policy abstraction (SPEC §6, §8).
//!
//! The policy drives the loop: `solve` receives the host side as
//! `&mut dyn Question` and calls `probe_batch` until it wants to stop. The
//! same trait is served in-process by [`super::replay::Replay`] and, across
//! a process boundary, by [`super::protocol::serve_question`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::replay::{CellMeta, MetaError, Observation, ProbeError};

/// Host side of one question, as seen by a policy (SPEC §6). This is the
/// policy's entire view of the world.
pub trait Question {
    /// Revealed cells and their observations.
    fn observed(&mut self) -> BTreeMap<String, Observation>;
    /// Legal cells: unopened roots first, then frontiers; each by branch.
    fn legal_actions(&mut self) -> Vec<String>;
    /// Attempt-0 cells of branches not yet opened, by branch.
    fn legal_roots(&mut self) -> Vec<String>;
    /// Branches with at least one revealed cell, ascending.
    fn opened_branches(&mut self) -> Vec<u32>;
    /// Structural metadata of a legal or revealed cell.
    fn meta(&mut self, cell_id: &str) -> Result<CellMeta, MetaError>;
    /// Reveal a batch; all-or-nothing (SPEC §4).
    fn probe_batch(&mut self, cells: &[String]) -> Result<Vec<Observation>, ProbeError>;
    fn baseline_score(&self) -> f64;
    fn max_parallelism(&self) -> u32;
}

/// Policy construction config (`__init__(config)`); only `beta` is read.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PolicyConfig {
    pub beta: f64,
}

/// Summary of one completed round, the only history `plan_grid` sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
    pub round: u32,
    pub planned_branch_count: u32,
    pub planned_refine_count: u32,
    pub actual_branch_count: u32,
    pub actual_refine_count: u32,
    pub probes: u64,
    pub decision_rounds: u64,
    pub best_score: Option<f64>,
    pub beta: f64,
}

/// `plan_grid` context (SPEC §6). Contains no node scores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridContext {
    pub history: Vec<RoundSummary>,
    pub hard_max_branch_count: u32,
    pub hard_max_refine_count: u32,
    pub worker_cap: u32,
    /// Present only during replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_branch_count: Option<u32>,
    /// Present only during replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_refine_count: Option<u32>,
}

/// `plan_grid` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridPlan {
    pub branch_count: u32,
    pub refine_count: u32,
    pub reason: String,
}

/// Errors a policy can surface.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error(transparent)]
    Probe(#[from] ProbeError),
    #[error(transparent)]
    Meta(#[from] MetaError),
    #[error("policy failed: {0}")]
    Failed(String),
}

/// A search policy over the branch × attempt grid.
pub trait ExplorationPolicy {
    /// Stable policy id (e.g. `baseline-parallel-refine`).
    fn id(&self) -> &str;
    /// Called once before a round; sees only completed-round summaries.
    fn plan_grid(&mut self, ctx: &GridContext) -> Result<GridPlan, PolicyError>;
    /// Drive the question until the policy wants to stop.
    fn solve(&mut self, question: &mut dyn Question) -> Result<(), PolicyError>;
}

/// Id of the built-in baseline policy.
pub const BASELINE_POLICY_ID: &str = "baseline-parallel-refine";

/// SPEC §8 built-in "simple parallel refining" policy. Ignores `beta`.
#[derive(Debug, Clone, Default)]
pub struct BaselineParallelRefine;

impl BaselineParallelRefine {
    pub fn new(_config: &PolicyConfig) -> Self {
        Self
    }
}

/// Probe `cells` in consecutive chunks of `chunk` (in order). Returns
/// `Ok(false)` when the replay terminated on `K2`.
fn probe_in_chunks(
    question: &mut dyn Question,
    cells: &[String],
    chunk: usize,
) -> Result<bool, PolicyError> {
    for batch in cells.chunks(chunk) {
        match question.probe_batch(batch) {
            Ok(_) => {}
            Err(ProbeError::Terminated(_)) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

impl ExplorationPolicy for BaselineParallelRefine {
    fn id(&self) -> &str {
        BASELINE_POLICY_ID
    }

    fn plan_grid(&mut self, ctx: &GridContext) -> Result<GridPlan, PolicyError> {
        let cap = |hard: u32, trace: Option<u32>| trace.map_or(hard, |t| hard.min(t));
        Ok(GridPlan {
            branch_count: cap(ctx.hard_max_branch_count, ctx.trace_branch_count),
            refine_count: cap(ctx.hard_max_refine_count, ctx.trace_refine_count),
            reason: "fixed baseline".to_string(),
        })
    }

    fn solve(&mut self, question: &mut dyn Question) -> Result<(), PolicyError> {
        let chunk = question.max_parallelism().max(1) as usize;
        let roots = question.legal_roots();
        if !probe_in_chunks(question, &roots, chunk)? {
            return Ok(());
        }
        loop {
            let actions = question.legal_actions();
            if actions.is_empty() {
                return Ok(());
            }
            if !probe_in_chunks(question, &actions, chunk)? {
                return Ok(());
            }
        }
    }
}
