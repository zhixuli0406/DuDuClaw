//! Internal contracts of the online discovery orchestrator (work package B1).
//!
//! These types are the seams between the four B1 components, which are
//! implemented independently:
//!
//! - [`AttemptRunner`] — runs ONE agent attempt in a prepared workspace and
//!   reports what it cost. Implemented per runtime family.
//! - [`Evaluator`] — scores a finished attempt. Runs an operator-registered
//!   command in isolation; never trusts anything the agent wrote as code.
//! - [`PolicySource`] — yields the exploration policy for a round: the
//!   built-in baseline, or an LLM-written Python policy behind a sandboxed
//!   subprocess.
//! - the orchestrator (`online.rs`) — owns budgets, concurrency, the ledger
//!   and the round loop, and is the only component that talks to all three.
//!
//! Design: `commercial/docs/DESIGN-dream-rsi-2026-09.md` §7.5–§7.8.
//! Invariants every implementation must keep (design §7.5):
//!
//! - I1: the attempt prompt is a function of the goal, the ancestor chain,
//!   the currently visible siblings and earlier rounds. It never carries
//!   budget, policy state or `beta`. That is why [`AttemptRequest`] has no
//!   such field — do not add one.
//! - I3: the evaluator and its data live outside the run directory; an
//!   attempt must not be able to read or write them.
//! - I4: cost is captured by the runner for the call it made and written
//!   with the node. No after-the-fact time-window attribution.
//! - I5: infrastructure failures are retried by the runner and never become
//!   nodes; a retry re-sends the byte-identical prompt.
//! - Every isolation gate fails closed: no usable backend ⇒ refuse.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::policy::ExplorationPolicy;
use super::tree::{FailClass, NodeCost};

/// How a child process was confined. Recorded on every node and audit row so
/// an operator can see after the fact what actually applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationBackend {
    /// One-shot container (no host filesystem beyond explicit binds).
    Container,
    /// OS-native confinement (Seatbelt / Landlock).
    Native,
    /// No confinement. Only reachable through the explicit operator opt-in
    /// (`[discovery] allow_unconfined = true` AND an operator-identity
    /// caller); never a fallback.
    None,
}

/// One agent attempt to run.
#[derive(Debug, Clone)]
pub struct AttemptRequest {
    pub run_id: String,
    pub cell_id: String,
    /// The attempt's own directory; the ONLY location it may write. Already
    /// populated with the parent's workspace copy by the orchestrator.
    pub node_dir: PathBuf,
    /// Host-owned run root used to validate workspace provenance. The child
    /// receives no read permission for this directory or its ledgers.
    pub run_dir: PathBuf,
    /// Explicit completed ancestor, visible-sibling and earlier-round workspaces.
    /// Live siblings and world/tree ledgers must never be included.
    pub read_workspaces: Vec<PathBuf>,
    /// Fully rendered prompt (see I1).
    pub prompt: String,
    pub agent_id: String,
    /// Model id for this attempt; `None` ⇒ the agent's configured model.
    pub model: Option<String>,
    /// Hard wall-clock cap for the attempt; the whole process group is
    /// killed on expiry.
    pub timeout: Duration,
    pub max_turns: u32,
    /// Rotator account pool to draw from (empty ⇒ all accounts).
    pub account_pool: Vec<String>,
}

/// A finished attempt (it ran; whether it produced a good solution is the
/// evaluator's question, not the runner's).
#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    /// Tokens, dollars and wall time of THIS attempt (I4).
    pub cost: NodeCost,
    /// Canonical runtime id that actually answered.
    pub runtime: String,
    /// Model that actually answered.
    pub model: String,
    pub isolation: IsolationBackend,
    /// Final assistant text, bounded by the runner (≤ 64 KiB).
    pub final_text: String,
    /// True when the wall-clock cap killed the attempt after it had started
    /// working. The node is still recorded (`fail_class = timeout` unless
    /// the evaluator finds a valid solution).
    pub timed_out: bool,
    /// Infra retries that preceded this outcome (0 when first try worked).
    pub infra_retries: u32,
}

/// The attempt could not be run at all. Never recorded as a node (I5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttemptInfraError {
    #[error("runtime {runtime} cannot enforce required capability: {capability}")]
    CapabilityUnsupported { runtime: String, capability: String },
    #[error("runtime {0} cannot enforce a strict server-side dollar ceiling")]
    StrictUsdUnsupported(String),
    #[error("attempt container cleanup could not be confirmed: {0}")]
    CleanupFailed(String),
    #[error("no isolation backend is available and unconfined runs are not enabled")]
    IsolationUnavailable,
    #[error("runtime {0} is not supported for discovery attempts")]
    RuntimeUnsupported(String),
    #[error("no usable account (all cooling down, exhausted or auth-dead)")]
    NoAccount,
    #[error("agent budget exhausted")]
    BudgetExhausted,
    #[error("provider rate or usage limit reached; discovery stopped without retry")]
    RateLimited,
    #[error("agent CLI could not be started: {0}")]
    Spawn(String),
    #[error("attempt failed after {retries} infra retries: {last}")]
    RetriesExhausted { retries: u32, last: String },
    /// The host stream guard saw a tool outside the attempt surface. Never
    /// retried; `tool` is sanitised (ASCII alphanumerics and `_-.`, ≤ 64 bytes).
    #[error("runtime {runtime} used a tool outside the attempt tool surface: {tool}")]
    ToolSurfaceViolation { runtime: String, tool: String },
}

/// Runs one attempt. Implementations own account selection, environment
/// scrubbing, confinement, cost capture and infra retries.
#[async_trait]
pub trait AttemptRunner: Send + Sync {
    async fn run_attempt(&self, req: &AttemptRequest) -> Result<AttemptOutcome, AttemptInfraError>;
}

/// A scoring request for one finished attempt.
#[derive(Debug, Clone)]
pub struct ScoreRequest {
    pub run_id: String,
    pub cell_id: String,
    /// The attempt's directory. The evaluator takes its own post-exit
    /// snapshot of it; it never scores the live directory.
    pub node_dir: PathBuf,
    /// Registered evaluator name (exact match against the registry).
    pub evaluator: String,
    /// Host scoring deadline, bounded again by the operator evaluator timeout.
    /// This metadata is never exposed to an exploration policy.
    pub timeout: Option<Duration>,
}

/// Evaluator verdict for one attempt. Maps 1:1 onto the node ledger
/// (`evaluated`, `valid`, `score`, `fail_class`, `error`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreOutcome {
    /// False when the evaluator itself could not run (timeout, crash,
    /// isolation refused, registry hash mismatch).
    pub evaluated: bool,
    pub valid: bool,
    /// Present iff `evaluated && valid`; always finite.
    pub score: Option<f64>,
    pub fail_class: FailClass,
    /// Sanitized, bounded diagnostics (≤ 500 bytes, injection-scanned).
    /// This text becomes history that later attempts read.
    pub diagnostics: Option<String>,
    pub isolation: IsolationBackend,
    pub wall_secs: f64,
}

/// Scores attempts. Never returns an error: every failure mode is a
/// `ScoreOutcome` with `evaluated = false` or `valid = false`, so one bad
/// evaluation can never abort a round.
#[async_trait]
pub trait Evaluator: Send + Sync {
    async fn score(&self, req: &ScoreRequest) -> ScoreOutcome;
}

/// Identity of a policy version, for the ledger and audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyVersion {
    /// `baseline-parallel-refine` or `llm-<sha256 prefix>`.
    pub policy_id: String,
    /// sha256 of the policy source; `None` for the built-in baseline.
    pub source_sha256: Option<String>,
}

/// Why the policy source is not serving LLM-written policies.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyDegraded {
    #[error("python3 is not available")]
    NoPython,
    #[error("no isolation backend for the policy subprocess")]
    NoIsolation,
    #[error("policy rejected: {0}")]
    Rejected(String),
}

/// Yields the exploration policy instance for a round and beta.
///
/// An LLM-written policy runs in a confined subprocess behind
/// `protocol::serve_question`; this trait hides that. When LLM policies
/// cannot be served the source returns the built-in baseline and reports
/// why through [`PolicySource::degraded`] — dreaming is then skipped and the
/// run is reported DEGRADED, never silently.
pub trait PolicySource: Send + Sync {
    /// The currently deployed version.
    fn current(&self) -> PolicyVersion;
    /// The deployed default is frozen once per live episode; replay still
    /// supplies each sweep beta explicitly through instantiate().
    fn live_beta(&self, configured: f64) -> f64 { configured }
    /// A fresh policy instance of the current version.
    fn instantiate(&self, beta: f64) -> Result<Box<dyn ExplorationPolicy + Send>, PolicyDegraded>;
    /// `Some` when only the built-in baseline can be served.
    fn degraded(&self) -> Option<PolicyDegraded>;
}

/// Hard limits of a run. Enforced by the orchestrator, outside any policy:
/// a policy can only choose which legal cells to probe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RunBudget {
    /// Agent CLI spawns of every kind (attempts, policy-development calls,
    /// infra retries). Checked BEFORE each spawn.
    pub max_agent_calls: u32,
    pub max_usd: f64,
    pub max_wall_secs: u64,
    pub max_rounds: u32,
}
