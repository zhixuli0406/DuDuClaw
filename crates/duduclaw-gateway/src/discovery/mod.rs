//! Discovery: exploration-tree ledger, replay simulator, scoring and the
//! policy protocol (Dream-RSI work package B0-1).
//!
//! An exploration *run* spends rounds; each round grows one tree (a
//! *world*) of `(branch, attempt)` cells, each an agent attempt plus a score.
//! A search *policy* decides which cells to expand. This module replays a
//! policy against a recorded world — revealing only what the policy asks for
//! — and scores how quickly and how in-parallel it reaches the world's best
//! result, so policies can be compared offline without paying for new agent
//! attempts.
//!
//! The online orchestrator owns confined attempts, pinned evaluators, policy
//! containers, shared budgets and lifecycle reconciliation. Public discovery
//! tasks enter through authenticated RPC or verified MCP identities via
//! [`service`], approved workspace IDs and manager approval when required.
//! A dedicated dispatcher runs them outside ordinary goal-worker claims.
//! The hidden CLI remains an operator-only configuration/experiment entry.
//! [`night`] compares frozen policies on recorded, task-scoped held-out worlds
//! without provider calls; reports redact private policy source.
//!
//! The authoritative semantics are in
//! `commercial/docs/SPEC-discovery-tree-replay-2026-09.md`; a Python
//! implementation of the same spec must produce identical numbers (after
//! 9-decimal rounding) on the shared fixtures under
//! `crates/duduclaw-gateway/tests/fixtures/discovery/`.
//!
//! Layout:
//! - [`tree`]: node / world types, loaders, validation (SPEC §1-§3).
//! - [`replay`]: the replay state machine (SPEC §4).
//! - [`policy`]: the `Question` / `ExplorationPolicy` traits and the
//!   built-in baseline policy (SPEC §6, §8).
//! - [`eval`]: five-beta sweep driver.
//! - [`score`]: attainment, Pareto AUC / reward, rounding (SPEC §5).
//! - [`protocol`]: line-delimited JSON host loop (SPEC §7).
//! - [`store`]: `discovery.db`.
//! - [`contracts`]: seams of the online orchestrator (work package B1).
//! - [`service`]: shared authority, approval, task lifecycle and public views.
//! - [`night`]: task-level recorded-world evidence and versioned defaults.

pub mod agent_spawn;
pub mod artifact;
pub mod attempt_adapter;
pub mod attempt_container;
pub mod attempt_guard;
pub mod budget;
pub mod config;
pub mod contracts;
pub mod dream;
pub mod eval;
pub mod evaluator;
mod timing_gate;
pub mod policy;
pub mod policy_runner;
pub mod online;
pub mod process;
pub mod isolation;
pub mod protocol;
pub mod replay;
pub mod score;
pub mod service;
pub mod stop_code;
pub mod store;
pub mod tree;
pub mod workspace;

pub use eval::{EvalError, ReplayConfig, evaluate_world, run_point};
pub use policy::{
    BASELINE_POLICY_ID, BaselineParallelRefine, ExplorationPolicy, GridContext, GridPlan,
    PolicyConfig, PolicyError, Question, RoundSummary,
};
pub use protocol::{ProtocolViolation, ServeError, ServeLimits, ServeOutcome, serve_question};
pub use replay::{CellMeta, Observation, ProbeError, Replay, ReplayTrace};
pub use score::{BETA_GRID, PointScore, WorldScore, round9};
pub use store::DiscoveryStore;
pub use tree::{Direction, FailClass, Node, TreeError, World, WorldTree};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fixtures;
#[cfg(test)]
mod tests_protocol;
#[cfg(test)]
mod tests_policy_runner;
#[cfg(test)]
mod tests_dream;
#[cfg(test)]
mod tests_metadata_pipeline;

pub mod maintenance;
pub mod night;
