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

/// Lists every task-board row currently `status == "in_progress"`, across
/// every agent this session can see (the task board's own `todo |
/// in_progress | done | blocked` vocabulary — see
/// `duduclaw-gateway/src/task_store.rs::TaskRow`'s own field comment). No
/// `agent_id` filter is sent — `admin@local`'s local session is admin, which
/// `check_agent_filter!` on the gateway side lets through unscoped (verified
/// against `handle_dispatch`'s own `"tasks.list"` arm).
pub fn list_in_progress_tasks(jwt: &str) -> Result<Vec<TaskProgressItem>, RpcError> {
    let payload = ws_rpc::call_once(jwt, "tasks.list", json!({ "status": "in_progress" }))?;
    let items = payload.get("tasks").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
    serde_json::from_value(items).map_err(|e| RpcError::Malformed(format!("tasks.list payload did not match the expected shape: {e}")))
}
