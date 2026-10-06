//! Test-only count of model-calling entry points (P2-A ET2: "zero model
//! calls while waiting"). Thread-local, so parallel tests on their own
//! current-thread runtimes never see each other's calls.

use std::cell::{Cell, RefCell};

thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static DRY_RUN: Cell<bool> = const { Cell::new(false) };
    static LAST_ROUND: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The round information (`ENV_TASK_ID` value) the last dry-run call
/// would have handed to the MCP server, `None` when it had none (M3-3).
pub(crate) fn last_round() -> Option<String> {
    LAST_ROUND.with(|r| r.borrow().clone())
}

pub(crate) fn set_last_round(v: Option<String>) {
    LAST_ROUND.with(|r| *r.borrow_mut() = v);
}

/// While set, `dispatch_to_agent_outcome` records the call and returns a
/// canned reply instead of spawning anything (dispatcher fence tests).
pub(crate) fn set_dry_run(on: bool) {
    DRY_RUN.with(|d| d.set(on));
}

pub(crate) fn dry_run() -> bool {
    DRY_RUN.with(Cell::get)
}

/// Called at the top of every entry point that can reach a model.
pub(crate) fn record(_site: &'static str) {
    CALLS.with(|c| c.set(c.get() + 1));
}

/// Model calls observed on this thread so far.
pub(crate) fn calls() -> usize {
    CALLS.with(Cell::get)
}
