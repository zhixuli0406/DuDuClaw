//! Replay simulator (SPEC §4).
//!
//! [`Replay`] owns the revealed set of one (policy, world, beta) replay and
//! implements [`Question`], the only surface a policy is handed. The world is
//! a private field, so a policy holding `&mut dyn Question` can read an
//! unrevealed node's data through no path: `observed` covers revealed cells,
//! `legal_*`/`opened_branches` return ids only, and `meta` refuses cells that
//! are neither legal nor revealed.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::policy::Question;
use super::score::round9;
use super::tree::{FailClass, Node, WorldTree};

/// Default cap on decision rounds (SPEC §4 `K2`).
pub const DEFAULT_K2: u32 = 1000;
/// `Observation.error` is truncated to this many bytes (SPEC §4).
pub const OBSERVATION_ERROR_MAX_BYTES: usize = 300;

/// What a policy sees of one revealed cell (SPEC §4; field names follow the
/// paper's Listing 2). Every float is rounded to 9 decimals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub cell_id: String,
    pub branch: u32,
    pub attempt: u32,
    pub score: Option<f64>,
    pub evaluated: bool,
    pub valid: bool,
    pub fail_class: FailClass,
    pub error: Option<String>,
    pub delta_vs_baseline: Option<f64>,
    pub delta_vs_parent: Option<f64>,
    /// Always `null` in this problem.
    pub n_valid: Option<u64>,
    /// Always `null` in this problem.
    pub n_total: Option<u64>,
}

/// `meta(cell_id)` result (SPEC §4). `tags` is always empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellMeta {
    pub branch: u32,
    pub attempt: u32,
    pub parent_id: Option<String>,
    pub seq: u64,
    pub tags: Vec<String>,
}

/// Why a `probe_batch` was rejected as `illegal_batch`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IllegalBatchReason {
    #[error("batch is empty")]
    Empty,
    #[error("batch contains {0} more than once")]
    Duplicate(String),
    #[error("batch size {len} exceeds max_parallelism {max}")]
    TooLarge { len: usize, max: u32 },
    #[error("{0} is not a legal action")]
    NotLegal(String),
}

/// Why a replay stopped accepting probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    /// The decision-round cap `K2` was reached.
    K2Reached,
}

/// `probe_batch` failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("illegal_batch: {0}")]
    IllegalBatch(IllegalBatchReason),
    #[error("replay terminated: {0:?}")]
    Terminated(TerminationReason),
}

impl From<IllegalBatchReason> for ProbeError {
    fn from(reason: IllegalBatchReason) -> Self { Self::IllegalBatch(reason) }
}

/// `meta` failure: the cell is neither legal nor revealed (or unknown).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("meta is only available for legal or revealed cells: {0}")]
pub struct MetaError(pub String);

/// Counters of one replay (SPEC §4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCounters {
    pub decision_rounds: u64,
    pub probes: u64,
    pub effective_sequential_rounds: u64,
}

/// Everything a finished replay produced, for scoring and reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayTrace {
    pub counters: ReplayCounters,
    /// Accepted batches, in order.
    pub batches: Vec<Vec<String>>,
    /// Revealed cells whose `visible_set` held a not-yet-revealed cell.
    pub context_mismatches: u64,
    /// Longest run of consecutive rejected `probe_batch` calls.
    pub max_consecutive_illegal: u32,
    /// Set when the replay stopped on `K2`.
    pub terminated: Option<TerminationReason>,
}

/// One replay over a validated world.
pub struct Replay<'w> {
    tree: &'w WorldTree,
    k2: u32,
    observed: BTreeMap<String, Observation>,
    revealed: HashSet<String>,
    /// Highest revealed attempt per opened branch.
    frontier: BTreeMap<u32, u32>,
    counters: ReplayCounters,
    batches: Vec<Vec<String>>,
    context_mismatches: u64,
    consecutive_illegal: u32,
    max_consecutive_illegal: u32,
    terminated: Option<TerminationReason>,
}

impl<'w> Replay<'w> {
    pub fn new(tree: &'w WorldTree, k2: u32) -> Self {
        Self {
            tree,
            k2,
            observed: BTreeMap::new(),
            revealed: HashSet::new(),
            frontier: BTreeMap::new(),
            counters: ReplayCounters::default(),
            batches: Vec::new(),
            context_mismatches: 0,
            consecutive_illegal: 0,
            max_consecutive_illegal: 0,
            terminated: None,
        }
    }

    pub fn counters(&self) -> ReplayCounters {
        self.counters
    }

    pub fn batches(&self) -> &[Vec<String>] {
        &self.batches
    }

    /// Revealed cell ids (unordered set view).
    pub fn is_revealed(&self, cell_id: &str) -> bool {
        self.revealed.contains(cell_id)
    }

    pub fn consecutive_illegal(&self) -> u32 {
        self.consecutive_illegal
    }

    pub fn terminated(&self) -> Option<TerminationReason> {
        self.terminated
    }

    /// Consume the replay into its trace.
    pub fn finish(self) -> ReplayTrace {
        ReplayTrace {
            counters: self.counters,
            batches: self.batches,
            context_mismatches: self.context_mismatches,
            max_consecutive_illegal: self.max_consecutive_illegal,
            terminated: self.terminated,
        }
    }

    fn roots(&self) -> Vec<String> {
        self.tree
            .branches()
            .iter()
            .filter(|b| !self.frontier.contains_key(b))
            .filter_map(|&b| self.tree.node_at(b, 0))
            .map(|n| n.cell_id.clone())
            .collect()
    }

    fn frontier_cells(&self) -> Vec<String> {
        self.frontier
            .iter()
            .filter_map(|(&b, &k)| k.checked_add(1).and_then(|next| self.tree.node_at(b, next)))
            .map(|n| n.cell_id.clone())
            .collect()
    }

    fn legal_set(&self) -> Vec<String> {
        let mut all = self.roots();
        all.extend(self.frontier_cells());
        all
    }

    fn check_batch(&self, cells: &[String]) -> Result<(), IllegalBatchReason> {
        if cells.is_empty() {
            return Err(IllegalBatchReason::Empty);
        }
        let mut seen = HashSet::with_capacity(cells.len());
        for c in cells {
            if !seen.insert(c.as_str()) {
                return Err(IllegalBatchReason::Duplicate(c.clone()));
            }
        }
        let max = self.tree.world().max_parallelism;
        if cells.len() > max as usize {
            return Err(IllegalBatchReason::TooLarge {
                len: cells.len(),
                max,
            });
        }
        let legal: HashSet<String> = self.legal_set().into_iter().collect();
        for c in cells {
            if !legal.contains(c) {
                return Err(IllegalBatchReason::NotLegal(c.clone()));
            }
        }
        Ok(())
    }

    fn observation(&self, node: &Node) -> Observation {
        let baseline = self.tree.world().baseline_score;
        let score = node.valid_score();
        let delta_vs_baseline = score.map(|s| s - baseline);
        let delta_vs_parent = if node.attempt == 0 {
            delta_vs_baseline
        } else {
            let parent_score = node
                .parent_id
                .as_deref()
                .and_then(|p| self.tree.node(p))
                .and_then(Node::valid_score);
            match (score, parent_score) {
                (Some(s), Some(p)) => Some(s - p),
                _ => None,
            }
        };
        Observation {
            cell_id: node.cell_id.clone(),
            branch: node.branch,
            attempt: node.attempt,
            score: score.map(round9),
            evaluated: node.evaluated,
            valid: node.valid,
            fail_class: node.fail_class,
            error: node
                .error
                .as_deref()
                .map(|e| duduclaw_core::truncate_bytes(e, OBSERVATION_ERROR_MAX_BYTES).to_string()),
            delta_vs_baseline: delta_vs_baseline.map(round9),
            delta_vs_parent: delta_vs_parent.map(round9),
            n_valid: None,
            n_total: None,
        }
    }

    fn record_illegal(&mut self) {
        self.consecutive_illegal = self.consecutive_illegal.saturating_add(1);
        self.max_consecutive_illegal = self.max_consecutive_illegal.max(self.consecutive_illegal);
    }
}

impl Question for Replay<'_> {
    fn observed(&mut self) -> BTreeMap<String, Observation> {
        self.observed.clone()
    }

    fn legal_actions(&mut self) -> Vec<String> {
        self.legal_set()
    }

    fn legal_roots(&mut self) -> Vec<String> {
        self.roots()
    }

    fn opened_branches(&mut self) -> Vec<u32> {
        self.frontier.keys().copied().collect()
    }

    fn meta(&mut self, cell_id: &str) -> Result<CellMeta, MetaError> {
        let queryable =
            self.revealed.contains(cell_id) || self.legal_set().iter().any(|c| c == cell_id);
        let node = if queryable {
            self.tree.node(cell_id)
        } else {
            None
        };
        match node {
            Some(n) => Ok(CellMeta {
                branch: n.branch,
                attempt: n.attempt,
                parent_id: n.parent_id.clone(),
                seq: n.seq,
                tags: Vec::new(),
            }),
            None => Err(MetaError(cell_id.to_string())),
        }
    }

    fn probe_batch(&mut self, cells: &[String]) -> Result<Vec<Observation>, ProbeError> {
        if self.counters.decision_rounds >= u64::from(self.k2) {
            self.terminated = Some(TerminationReason::K2Reached);
            return Err(ProbeError::Terminated(TerminationReason::K2Reached));
        }
        if let Err(reason) = self.check_batch(cells) {
            self.record_illegal();
            return Err(ProbeError::IllegalBatch(reason));
        }
        self.consecutive_illegal = 0;

        // Cells of the same batch are revealed simultaneously: mismatch is
        // judged against the set revealed before this batch.
        let tree = self.tree;
        let mut observations = Vec::with_capacity(cells.len());
        for c in cells {
            let Some(node) = tree.node(c) else {
                continue; // unreachable: check_batch accepted only known cells
            };
            if node.visible_set.iter().any(|v| !self.revealed.contains(v)) {
                self.context_mismatches += 1;
            }
            observations.push(self.observation(node));
        }
        for (c, obs) in cells.iter().zip(observations.iter()) {
            if let Some(node) = tree.node(c) {
                let entry = self.frontier.entry(node.branch).or_insert(node.attempt);
                *entry = (*entry).max(node.attempt);
            }
            self.revealed.insert(c.clone());
            self.observed.insert(c.clone(), obs.clone());
        }

        let k = cells.len() as u64;
        let mp = u64::from(tree.world().max_parallelism.max(1));
        self.counters.decision_rounds += 1;
        self.counters.probes += k;
        self.counters.effective_sequential_rounds += k.div_ceil(mp);
        self.batches.push(cells.to_vec());
        if self.counters.decision_rounds >= u64::from(self.k2) {
            self.terminated = Some(TerminationReason::K2Reached);
        }
        Ok(observations)
    }

    fn baseline_score(&self) -> f64 {
        round9(self.tree.world().baseline_score)
    }

    fn max_parallelism(&self) -> u32 {
        self.tree.world().max_parallelism
    }
}
