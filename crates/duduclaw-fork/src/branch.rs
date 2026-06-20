//! Branch types — one `Branch` is one isolated agent subprocess run within a fork.
//!
//! RFC-26 §3.1: a "branch" in DuDuClaw maps to a single `claude` CLI run with its
//! own workspace overlay, account, budget cap, and optional steering prompt, while
//! sharing a read-through view of the parent workspace.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a single branch within a fork.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BranchId(pub String);

impl BranchId {
    /// Generate a fresh random branch id.
    pub fn new() -> Self {
        BranchId(Uuid::new_v4().to_string())
    }
}

impl Default for BranchId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for BranchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle state of a branch.
///
/// Fail-closed ordering (RFC-26 §2): a branch only reaches `Finished` after its
/// subprocess exits cleanly; any spawn/budget/timeout fault lands it in a terminal
/// non-winning state so the judge never scores a half-run as a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchState {
    /// Created but not yet spawned.
    Pending,
    /// Subprocess running.
    Running,
    /// Subprocess exited and produced output.
    Finished,
    /// Terminated because it would exceed the aggregate or per-branch budget.
    BudgetKilled,
    /// Terminated by the operator (`terminate_branch`) or by timeout.
    Terminated,
    /// Spawn or execution error — excluded from judging.
    Failed,
}

impl BranchState {
    /// A branch is judgeable only if it finished cleanly with output.
    pub fn is_judgeable(self) -> bool {
        matches!(self, BranchState::Finished)
    }

    /// Terminal states no longer transition.
    pub fn is_terminal(self) -> bool {
        !matches!(self, BranchState::Pending | BranchState::Running)
    }
}

/// Per-branch configuration captured at fork time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSpec {
    /// Optional steering message appended to this branch's prompt (RFC-26 §3.3).
    #[serde(default)]
    pub steering: Option<String>,
    /// Per-branch spend cap in USD.
    pub budget_usd: f64,
}

/// A single competing branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub id: BranchId,
    pub spec: BranchSpec,
    pub state: BranchState,
    /// Cumulative spend for this branch so far.
    pub spent_usd: f64,
}

impl Branch {
    pub fn new(spec: BranchSpec) -> Self {
        Branch::with_id(BranchId::new(), spec)
    }

    /// Build a branch with a caller-supplied id, so a fork's branch ids stay
    /// stable across the MCP registry and the controller (RFC-26 P4).
    pub fn with_id(id: BranchId, spec: BranchSpec) -> Self {
        Branch {
            id,
            spec,
            state: BranchState::Pending,
            spent_usd: 0.0,
        }
    }
}

/// Output of a finished branch, handed to the judge and test runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchResult {
    pub id: BranchId,
    pub state: BranchState,
    /// Final assistant text / answer produced by the branch.
    pub output: String,
    pub spent_usd: f64,
    /// Exit code of `test_command` against the branch snapshot, if a test was configured.
    #[serde(default)]
    pub test_exit_code: Option<i32>,
}

impl BranchResult {
    /// Whether the configured test command passed for this branch.
    /// `None` (no test configured) is treated as neutral, not a pass.
    pub fn test_passed(&self) -> Option<bool> {
        self.test_exit_code.map(|code| code == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_ids_are_unique() {
        assert_ne!(BranchId::new(), BranchId::new());
    }

    #[test]
    fn only_finished_is_judgeable() {
        assert!(BranchState::Finished.is_judgeable());
        for s in [
            BranchState::Pending,
            BranchState::Running,
            BranchState::BudgetKilled,
            BranchState::Terminated,
            BranchState::Failed,
        ] {
            assert!(!s.is_judgeable(), "{s:?} must not be judgeable");
        }
    }

    #[test]
    fn terminal_states_classified() {
        assert!(!BranchState::Pending.is_terminal());
        assert!(!BranchState::Running.is_terminal());
        assert!(BranchState::Finished.is_terminal());
        assert!(BranchState::Failed.is_terminal());
    }

    #[test]
    fn test_passed_neutral_when_unconfigured() {
        let r = BranchResult {
            id: BranchId::new(),
            state: BranchState::Finished,
            output: "ok".into(),
            spent_usd: 0.1,
            test_exit_code: None,
        };
        assert_eq!(r.test_passed(), None);
    }

    #[test]
    fn test_passed_reflects_exit_code() {
        let mk = |code| BranchResult {
            id: BranchId::new(),
            state: BranchState::Finished,
            output: String::new(),
            spent_usd: 0.0,
            test_exit_code: Some(code),
        };
        assert_eq!(mk(0).test_passed(), Some(true));
        assert_eq!(mk(1).test_passed(), Some(false));
    }
}
