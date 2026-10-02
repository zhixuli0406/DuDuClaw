//! Tree ledger types (SPEC §2 node record, §3 world header), loaders and
//! structural validation.
//!
//! A *world* is one online exploration round: a `world.json` header plus a
//! `tree.jsonl` file with one node record per line. [`WorldTree`] is the
//! validated, indexed form every other part of this module works from.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// `schema` value of a node record.
pub const NODE_SCHEMA: &str = "duduclaw.discovery.node.v1";
/// `schema` value of a world header.
pub const WORLD_SCHEMA: &str = "duduclaw.discovery.world.v1";
/// World header file name inside a world directory.
pub const WORLD_FILE: &str = "world.json";
/// Node ledger file name inside a world directory.
pub const TREE_FILE: &str = "tree.jsonl";
/// Maximum cumulative planned cells in a queryable online run.
pub const MAX_PLANNED_CELLS: u64 = 20_000;

/// Score direction of a world (SPEC §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Max,
    Min,
}

impl Direction {
    /// Map a raw score into "higher is better" space. Under `min` every
    /// comparison negates the score first and then proceeds as `max`.
    pub fn orient(self, score: f64) -> f64 {
        match self {
            Direction::Max => score,
            Direction::Min => -score,
        }
    }

    /// Stable string form (`"max"` / `"min"`), used by the store.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Max => "max",
            Direction::Min => "min",
        }
    }

    /// Inverse of [`Direction::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "max" => Some(Direction::Max),
            "min" => Some(Direction::Min),
            _ => None,
        }
    }
}

/// Closed failure classification of a node (SPEC §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailClass {
    Ok,
    InvalidSolution,
    RuntimeError,
    Timeout,
    NoOutput,
    Tamper,
}

impl FailClass {
    /// Stable string form, identical to the serde spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            FailClass::Ok => "ok",
            FailClass::InvalidSolution => "invalid_solution",
            FailClass::RuntimeError => "runtime_error",
            FailClass::Timeout => "timeout",
            FailClass::NoOutput => "no_output",
            FailClass::Tamper => "tamper",
        }
    }

    /// Inverse of [`FailClass::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(FailClass::Ok),
            "invalid_solution" => Some(FailClass::InvalidSolution),
            "runtime_error" => Some(FailClass::RuntimeError),
            "timeout" => Some(FailClass::Timeout),
            "no_output" => Some(FailClass::NoOutput),
            "tamper" => Some(FailClass::Tamper),
            _ => None,
        }
    }
}

/// Provenance of the dollar amount. Legacy zeros are unknown, not free calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    #[default]
    Unknown,
    Reported,
    Estimated,
    Pending,
}

impl CostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Reported => "reported",
            Self::Estimated => "estimated",
            Self::Pending => "pending",
        }
    }
}

/// Numeric fields retain the legacy zero defaults. The source separately
/// records whether those numbers are a bill, estimate, or incomplete subtotal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeCost {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub usd: f64,
    #[serde(default)]
    pub usd_source: CostSource,
    #[serde(default)]
    pub unknown_calls: u32,
    #[serde(default)]
    pub wall_secs: f64,
}

#[cfg(test)]
mod cost_provenance_tests {
    use super::NodeCost;

    #[test]
    fn legacy_zero_is_unknown_and_explicit_reported_zero_survives() {
        let legacy: NodeCost = serde_json::from_str("{\"usd\":0.0}").unwrap();
        assert_eq!(serde_json::to_value(legacy).unwrap()["usd_source"], "unknown");
        let reported: NodeCost = serde_json::from_str("{\"usd\":0.0,\"usd_source\":\"reported\"}").unwrap();
        assert_eq!(serde_json::to_value(reported).unwrap()["usd_source"], "reported");
    }
}

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One node record (one line of `tree.jsonl`, SPEC §2). Unknown fields are
/// ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub schema: String,
    pub run_id: String,
    pub round: u32,
    pub cell_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub branch: u32,
    pub attempt: u32,
    pub seq: u64,
    #[serde(default)]
    pub dispatched_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub evaluated: bool,
    pub valid: bool,
    #[serde(default)]
    pub score: Option<f64>,
    pub fail_class: FailClass,
    #[serde(default)]
    pub error: Option<String>,
    pub visible_set: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub cost: NodeCost,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub proposal_excerpt: Option<String>,
}

impl Node {
    /// The score if this node counts as a valid scored node
    /// (`evaluated && valid` with a number).
    pub fn valid_score(&self) -> Option<f64> {
        if self.evaluated && self.valid {
            self.score
        } else {
            None
        }
    }
}

/// World header (`world.json`, SPEC §3). Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct World {
    #[serde(default)]
    pub schema: String,
    pub run_id: String,
    pub round: u32,
    pub direction: Direction,
    pub baseline_score: f64,
    pub branch_count: u32,
    pub refine_count: u32,
    pub max_parallelism: u32,
    #[serde(default)]
    pub policy_id: String,
    #[serde(default)]
    pub beta: f64,
}

/// Errors from loading or validating a world.
#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("world header is not valid JSON: {0}")]
    WorldJson(#[source] serde_json::Error),
    #[error("tree.jsonl line {line}: {source}")]
    NodeJson {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("tree.jsonl line {line}: read failed: {source}")]
    NodeRead {
        line: usize,
        #[source]
        source: std::io::Error,
    },
    #[error("unexpected schema for {what}: {found:?}")]
    Schema { what: String, found: String },
    #[error("invalid world header: {0}")]
    InvalidWorld(String),
    #[error("world has no nodes")]
    EmptyTree,
    #[error("duplicate cell id {0}")]
    DuplicateCell(String),
    #[error("duplicate (branch, attempt) = ({branch}, {attempt})")]
    DuplicatePosition { branch: u32, attempt: u32 },
    #[error("cell id {cell_id:?} does not match r{round}-b{branch}-a{attempt}")]
    CellIdMismatch {
        cell_id: String,
        round: u32,
        branch: u32,
        attempt: u32,
    },
    #[error("node {cell_id}: run/round {run_id}/{round} does not match the world header")]
    WorldMismatch {
        cell_id: String,
        run_id: String,
        round: u32,
    },
    #[error("node {cell_id}: outside the world grid (branch_count/refine_count)")]
    OutOfGrid { cell_id: String },
    #[error("node {cell_id}: expected parent {expected:?}, found {found:?}")]
    ParentMismatch {
        cell_id: String,
        expected: Option<String>,
        found: Option<String>,
    },
    #[error("node {cell_id}: parent {parent} is not in the tree")]
    MissingParent { cell_id: String, parent: String },
    #[error("node {cell_id}: score must be a finite number iff evaluated && valid")]
    ScoreInconsistent { cell_id: String },
}

/// Format a cell id: `r{round}-b{branch}-a{attempt}` (SPEC §1).
pub fn cell_id(round: u32, branch: u32, attempt: u32) -> String {
    format!("r{round}-b{branch}-a{attempt}")
}

/// Parse a canonical cell id back into `(round, branch, attempt)`.
/// Non-canonical spellings (leading zeros, signs, extra text) are rejected.
pub fn parse_cell_id(id: &str) -> Option<(u32, u32, u32)> {
    let rest = id.strip_prefix('r')?;
    let (round, rest) = rest.split_once("-b")?;
    let (branch, attempt) = rest.split_once("-a")?;
    let parse = |s: &str| -> Option<u32> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse::<u32>().ok()
    };
    let parsed = (parse(round)?, parse(branch)?, parse(attempt)?);
    if cell_id(parsed.0, parsed.1, parsed.2) == id {
        Some(parsed)
    } else {
        None
    }
}

/// Load and parse `world.json`.
pub fn load_world(path: &Path) -> Result<World, TreeError> {
    let text = std::fs::read_to_string(path).map_err(|source| TreeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(TreeError::WorldJson)
}

/// Parse a `tree.jsonl` stream. Whitespace-only lines are skipped; any other
/// malformed line is an error carrying its 1-based line number.
pub fn parse_tree_jsonl<R: BufRead>(reader: R) -> Result<Vec<Node>, TreeError> {
    let mut nodes = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line.map_err(|source| TreeError::NodeRead {
            line: line_no,
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let node: Node = serde_json::from_str(&line).map_err(|source| TreeError::NodeJson {
            line: line_no,
            source,
        })?;
        nodes.push(node);
    }
    Ok(nodes)
}

/// Load and parse a `tree.jsonl` file.
pub fn load_tree_jsonl(path: &Path) -> Result<Vec<Node>, TreeError> {
    let file = File::open(path).map_err(|source| TreeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_tree_jsonl(BufReader::new(file))
}

/// A validated, indexed world: header plus nodes.
#[derive(Debug, Clone)]
pub struct WorldTree {
    world: World,
    nodes: Vec<Node>,
    by_id: HashMap<String, usize>,
    by_pos: BTreeMap<(u32, u32), usize>,
    branches: Vec<u32>,
}

impl WorldTree {
    /// Validate and index a world.
    ///
    /// Checks: header sanity (`max_parallelism >= 1`, finite baseline,
    /// schema if present), non-empty tree, unique cell ids and positions,
    /// canonical cell ids matching `(round, branch, attempt)`, run/round
    /// matching the header, positions inside the header grid, attempt 0 has
    /// no parent, attempt k>0 has parent `(branch, k-1)` present in the tree,
    /// and score present exactly when `evaluated && valid`.
    pub fn new(world: World, nodes: Vec<Node>) -> Result<Self, TreeError> {
        validate_world(&world)?;
        if nodes.is_empty() {
            return Err(TreeError::EmptyTree);
        }
        let mut by_id = HashMap::with_capacity(nodes.len());
        let mut by_pos = BTreeMap::new();
        for (i, n) in nodes.iter().enumerate() {
            validate_node_local(&world, n)?;
            if by_id.insert(n.cell_id.clone(), i).is_some() {
                return Err(TreeError::DuplicateCell(n.cell_id.clone()));
            }
            if by_pos.insert((n.branch, n.attempt), i).is_some() {
                return Err(TreeError::DuplicatePosition {
                    branch: n.branch,
                    attempt: n.attempt,
                });
            }
        }
        for n in &nodes {
            if n.attempt == 0 {
                if n.parent_id.is_some() {
                    return Err(TreeError::ParentMismatch {
                        cell_id: n.cell_id.clone(),
                        expected: None,
                        found: n.parent_id.clone(),
                    });
                }
                continue;
            }
            let expected = cell_id(n.round, n.branch, n.attempt - 1);
            if n.parent_id.as_deref() != Some(expected.as_str()) {
                return Err(TreeError::ParentMismatch {
                    cell_id: n.cell_id.clone(),
                    expected: Some(expected),
                    found: n.parent_id.clone(),
                });
            }
            if !by_id.contains_key(&expected) {
                return Err(TreeError::MissingParent {
                    cell_id: n.cell_id.clone(),
                    parent: expected,
                });
            }
        }
        let branches: BTreeSet<u32> = by_pos
            .keys()
            .filter(|(_, a)| *a == 0)
            .map(|(b, _)| *b)
            .collect();
        Ok(Self {
            world,
            nodes,
            by_id,
            by_pos,
            branches: branches.into_iter().collect(),
        })
    }

    /// Load `world.json` + `tree.jsonl` from a world directory and validate.
    pub fn load_dir(dir: &Path) -> Result<Self, TreeError> {
        let world = load_world(&dir.join(WORLD_FILE))?;
        let nodes = load_tree_jsonl(&dir.join(TREE_FILE))?;
        Self::new(world, nodes)
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// Nodes in file order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn total_cells(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&self, cell_id: &str) -> Option<&Node> {
        self.by_id.get(cell_id).map(|&i| &self.nodes[i])
    }

    pub fn node_at(&self, branch: u32, attempt: u32) -> Option<&Node> {
        self.by_pos.get(&(branch, attempt)).map(|&i| &self.nodes[i])
    }

    /// Branches that exist in the record (have an attempt-0 cell), ascending.
    pub fn branches(&self) -> &[u32] {
        &self.branches
    }

    /// Best valid score over the whole world in oriented ("higher is
    /// better") space; the oriented baseline when no node is valid.
    pub fn oriented_ceiling(&self) -> f64 {
        let dir = self.world.direction;
        let mut best: Option<f64> = None;
        for n in &self.nodes {
            if let Some(s) = n.valid_score() {
                let o = dir.orient(s);
                best = Some(best.map_or(o, |b: f64| b.max(o)));
            }
        }
        best.unwrap_or_else(|| dir.orient(self.world.baseline_score))
    }
}

fn validate_world(world: &World) -> Result<(), TreeError> {
    if !world.schema.is_empty() && world.schema != WORLD_SCHEMA {
        return Err(TreeError::Schema {
            what: "world".to_string(),
            found: world.schema.clone(),
        });
    }
    if world.max_parallelism == 0 {
        return Err(TreeError::InvalidWorld(
            "max_parallelism must be at least 1".to_string(),
        ));
    }
    if !world.baseline_score.is_finite() {
        return Err(TreeError::InvalidWorld(
            "baseline_score must be finite".to_string(),
        ));
    }
    Ok(())
}

fn validate_node_local(world: &World, n: &Node) -> Result<(), TreeError> {
    if !n.schema.is_empty() && n.schema != NODE_SCHEMA {
        return Err(TreeError::Schema {
            what: format!("node {}", n.cell_id),
            found: n.schema.clone(),
        });
    }
    if n.cell_id != cell_id(n.round, n.branch, n.attempt) {
        return Err(TreeError::CellIdMismatch {
            cell_id: n.cell_id.clone(),
            round: n.round,
            branch: n.branch,
            attempt: n.attempt,
        });
    }
    if n.run_id != world.run_id || n.round != world.round {
        return Err(TreeError::WorldMismatch {
            cell_id: n.cell_id.clone(),
            run_id: n.run_id.clone(),
            round: n.round,
        });
    }
    if n.branch >= world.branch_count || n.attempt > world.refine_count {
        return Err(TreeError::OutOfGrid {
            cell_id: n.cell_id.clone(),
        });
    }
    let should_score = n.evaluated && n.valid;
    let score_ok = match n.score {
        Some(s) => should_score && s.is_finite(),
        None => !should_score,
    };
    if !score_ok {
        return Err(TreeError::ScoreInconsistent {
            cell_id: n.cell_id.clone(),
        });
    }
    Ok(())
}
