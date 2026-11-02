//! Test-only count of model-calling entry points (P2-A ET2: "zero model
//! calls while waiting"). Thread-local, so parallel tests on their own
//! current-thread runtimes never see each other's calls.

use std::cell::{Cell, RefCell};

thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static DRY_RUN: Cell<bool> = const { Cell::new(false) };
    static LAST_LANE: Cell<Option<bool>> = const { Cell::new(None) };
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

/// Whether the last dry-run call ran in the explore lane.
pub(crate) fn last_lane() -> Option<bool> {
    LAST_LANE.with(Cell::get)
}

pub(crate) fn set_last_lane(v: bool) {
    LAST_LANE.with(|l| l.set(Some(v)));
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

thread_local! {
    static BUS_DELEGATION: RefCell<Option<(String, String, String)>> = const { RefCell::new(None) };
    static REGISTRATION_FAILURE: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
}

pub(crate) fn fail_downstream_registration(home: Option<&std::path::Path>) {
    REGISTRATION_FAILURE.with(|h| *h.borrow_mut() = home.map(std::path::Path::to_path_buf));
}
pub(crate) fn downstream_registration_fails(home: &std::path::Path) -> bool {
    REGISTRATION_FAILURE.with(|h| h.borrow().as_deref() == Some(home))
}

/// One dry-run runtime emits a delegation using its actual host scopes.
pub(crate) fn delegate_on_run(parent_prompt: &str, child_id: &str, target: &str) {
    BUS_DELEGATION.with(|d| *d.borrow_mut() = Some((parent_prompt.into(), child_id.into(), target.into())));
}
pub(crate) async fn capture_bus_delegation(home: &std::path::Path, prompt: &str, agent: &str) {
    let child = BUS_DELEGATION.with(|d| {
        let mut d = d.borrow_mut();
        if d.as_ref().is_some_and(|(p, _, _)| p == prompt) { d.take().map(|(_, id, target)| (id, target)) } else { None }
    });
    let Some((id, target)) = child else { return; };
    let (turn, session, missing) = crate::memory_provenance::complete_upstream_pair(
        duduclaw_memory::feedback::CURRENT_TURN_ID.with(Clone::clone),
        duduclaw_memory::feedback::CURRENT_SESSION_ID.with(Clone::clone),
    );
    assert!(missing.is_none());
    let row = serde_json::json!({"type":"agent_message", "message_id":id, "agent_id":target,
        "payload":id, "timestamp":chrono::Utc::now().to_rfc3339(), "sender_agent":agent,
        "origin_agent":agent, "session_id":session, "turn_id":turn,
        "on_behalf_of":crate::on_behalf_of::current()});
    crate::dispatcher::append_line(&home.join("bus_queue.jsonl"), &row.to_string()).await.unwrap();
}
