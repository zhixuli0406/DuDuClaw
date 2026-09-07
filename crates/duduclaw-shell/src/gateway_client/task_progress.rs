// Typed wrapper over `ws_rpc::call_once` for `tasks.list(status="in_progress")`
// — A4 (2026-08-24): the dock badge and the Notifications panel's "進行中
//任務" section this round need. Same shape as `approvals.rs` (that module's
// own header comment gives the reasoning this one inherits verbatim —
// read-only reference to `duduclaw-gateway/src/handlers.rs::
// handle_tasks_list`, not modified from here, field names copied from
// `task_row_to_json`'s own response-building code, not guessed).
//
// ── Deliberately its own file, not `tasks.rs` ───────────────────────────
// `gateway_client/tasks.rs` already exists (A1 result-loopback, same day):
// `list_tasks(jwt, agent_id)` scoped to ONE agent's own delegated tasks
// (the Launcher card's poll loop), a different query shape from what this
// module needs — every in-progress task ACROSS every agent, for a
// process-wide badge count, with no `agent_id` to scope by. Reusing that
// name/shape would mean either widening it with an optional-agent branch
// two unrelated features would both have to reason about, or silently
// shadowing it — this file picks a distinct name (`list_in_progress_tasks`)
// and type (`TaskProgressItem`, not `TaskSnapshot`) instead, so both
// features keep their own honest, narrow contract.

use serde::Deserialize;
use serde_json::json;

use super::ws_rpc::{self, RpcError};

/// One task-board row, as the dock badge / Notifications panel's
/// "進行中任務" section render it. A SUBSET of `task_row_to_json`'s full
/// response shape — only the fields this round's card actually uses.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TaskProgressItem {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub assigned_to: String,
}

/// The statuses Home and the dock badge treat as "open": what the AI team is
/// doing right now, what is queued behind it, and what is parked waiting on
/// the operator. Order matters — it is the display order when several
/// compete for the three Home card slots, and the fetch order below.
///
/// 2026-09-05 (QEMU feature walkthrough): before this the feed asked only
/// for `in_progress`, so a freshly delegated goal task (`status: "todo"`,
/// picked up by the goal loop on its next tick or never, if no API key is
/// configured) was invisible on Home — the operator pressed Enter on 交辦
/// and saw「還沒有交辦的任務」. `needs_human` (the dispatch engine's
/// parking status for a structured blocker such as a missing API key) was
/// equally invisible unless it happened to be an approval-broker row.
pub const OPEN_STATUSES: [&str; 3] = ["needs_human", "in_progress", "todo"];

/// Display rank of an open status — lower sorts first (`needs_human`
/// outranks everything: it is the one the operator can unblock).
pub fn open_status_rank(status: &str) -> usize {
    OPEN_STATUSES.iter().position(|s| *s == status).unwrap_or(OPEN_STATUSES.len())
}

/// Every open task, across all agents (admin session), ordered by
/// `OPEN_STATUSES` rank and stable within a status. One `tasks.list` per
/// status — the gateway's filter takes a single status string.
pub fn list_open_tasks(jwt: &str) -> Result<Vec<TaskProgressItem>, RpcError> {
    let mut all = Vec::new();
    for status in OPEN_STATUSES {
        let payload = ws_rpc::call_once(jwt, "tasks.list", json!({ "status": status }))?;
        let items = payload.get("tasks").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
        let mut batch: Vec<TaskProgressItem> = serde_json::from_value(items)
            .map_err(|e| RpcError::Malformed(format!("tasks.list payload did not match the expected shape: {e}")))?;
        all.append(&mut batch);
    }
    all.sort_by_key(|t| open_status_rank(&t.status));
    Ok(all)
}

