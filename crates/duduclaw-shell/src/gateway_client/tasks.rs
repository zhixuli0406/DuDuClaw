// A1 result-loopback (2026-08-24) — typed wrappers over `ws_rpc::call_once`
// for the RPCs the Launcher's 交辦 card and `crate::task_result`'s poll loop
// need: `agents.list` (pick who to delegate to), `tasks.goal_create`
// (submit), `tasks.list` (poll one agent's own tasks for a terminal state)
// and `tasks.goal_decide` (act on a `needs_human` pause from a notification
// card). Field names and JSON shapes are copied from
// `duduclaw-gateway/src/handlers.rs`'s own response-building code
// (`handle_agents_list_filtered`, `handle_tasks_goal_create`,
// `handle_tasks_list`/`task_row_to_json`, `handle_tasks_goal_decide`) — not
// guessed. Same "blocking, run from a `std::thread::spawn`" contract as
// `approvals.rs`, this module tree's established convention (see
// `gateway_client`'s own header comment).
//
// A SEPARATE, same-day `gateway_client::task_progress` module (A4) also
// calls `tasks.list`, for a different reason: this file's `list_tasks`
// below is scoped to ONE agent's own delegated tasks (`agent_id` required —
// the Launcher poll loop only ever watches tasks it itself created via
// `create_goal`), where `task_progress::list_in_progress_tasks` pulls every
// `in_progress` task across every agent for the dock badge. See that
// module's own header comment for why the two intentionally do not share a
// function or a struct.

use serde::Deserialize;
use serde_json::json;

use super::ws_rpc::{self, RpcError};

/// One entry out of `agents.list`, trimmed to what `pick_default_agent`
/// needs. `id` is `cfg.agent.name` on the gateway side — the same string
/// `tasks.goal_create`'s `agent_id` param and `tasks.list`'s own `agent_id`
/// filter expect (`is_valid_agent_id` is checked against agent NAMES, not a
/// separate numeric id — verified against `handle_tasks_goal_create`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AgentRef {
    #[serde(rename = "name")]
    pub id: String,
    pub role: String,
}

/// Lists every agent this session can see. Blocking.
pub fn list_agents(jwt: &str) -> Result<Vec<AgentRef>, RpcError> {
    let payload = ws_rpc::call_once(jwt, "agents.list", json!({}))?;
    let items = payload.get("agents").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
    serde_json::from_value(items).map_err(|e| RpcError::Malformed(format!("agents.list payload did not match the expected shape: {e}")))
}

/// Which agent a Super+K delegation goes to when the operator has not named
/// one (this round's Launcher card has no agent picker — see
/// `overlay/launcher.rs`'s own header comment on the delegate card still
/// being demo content beyond the plain text field). Prefers the org's
/// `role: "main"` agent — the same anchor this codebase's wiki-ACL
/// convention uses elsewhere for "who answers when nobody named a specific
/// agent" — and falls back to the first agent this session can see so a
/// deployment with no `main`-role agent configured still has somewhere to
/// send a delegation rather than refusing outright. `None` only when the
/// list itself is empty (no agents at all reachable by this session).
pub fn pick_default_agent(agents: &[AgentRef]) -> Option<&AgentRef> {
    agents.iter().find(|a| a.role == "main").or_else(|| agents.first())
}

/// What `tasks.goal_create` hands back that this crate actually uses.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedGoal {
    pub task_id: String,
    pub title: String,
}

/// Submits one natural-language delegation as a `goal_mode` task, the same
/// RPC (and therefore the same acceptance/judge/needs_human machinery) the
/// dashboard's AssignSheet and the channel `/goal` command already use — see
/// `handle_tasks_goal_create`'s own doc comment. No `acceptance_criteria` is
/// sent: the gateway already defaults it to the description text itself,
/// which is the only acceptance bar a bare typed sentence can honestly imply.
pub fn create_goal(jwt: &str, agent_id: &str, description: &str) -> Result<CreatedGoal, RpcError> {
    let payload = ws_rpc::call_once(jwt, "tasks.goal_create", json!({ "agent_id": agent_id, "description": description }))?;
    let task = payload.get("task").ok_or_else(|| RpcError::Malformed("tasks.goal_create payload carried no task".to_string()))?;
    let task_id = task.get("id").and_then(|v| v.as_str()).ok_or_else(|| RpcError::Malformed("tasks.goal_create task had no id".to_string()))?.to_string();
    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok(CreatedGoal { task_id, title })
}

/// One task row, trimmed to what the result-loopback poll
/// (`crate::task_result`) needs to decide "has this reached a state the
/// operator must be told about". A SUBSET of `task_row_to_json`'s full
/// shape — this module has no use for e.g. `agent_seconds`/
/// `lease_expires_at`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TaskSnapshot {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub result_summary: Option<String>,
    #[serde(default)]
    pub judge_feedback: Option<String>,
    /// Only ever `Some` while `status == "needs_human"` — `task_row_to_json`
    /// itself scopes it the same way (its own comment: "a legacy /
    /// unrecognised row must reach the dashboard as `unknown`... Scoped to
    /// `needs_human` so a class can never linger on a task that is no
    /// longer paused").
    #[serde(default)]
    pub pause_reason: Option<String>,
}

/// Lists one agent's tasks (every status — the poll loop needs to see a
/// transition INTO a terminal status, not just tasks already there).
/// Blocking. Scoped to one `agent_id` (never "every agent this session can
/// see" — that is `task_progress::list_in_progress_tasks`'s job, see this
/// file's header comment) since the poll loop only ever watches tasks it
/// itself created via `create_goal` above, all under the same resolved
/// default agent.
pub fn list_tasks(jwt: &str, agent_id: &str) -> Result<Vec<TaskSnapshot>, RpcError> {
    let payload = ws_rpc::call_once(jwt, "tasks.list", json!({ "agent_id": agent_id }))?;
    let items = payload.get("tasks").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
    serde_json::from_value(items).map_err(|e| RpcError::Malformed(format!("tasks.list payload did not match the expected shape: {e}")))
}

/// Acts on a `needs_human` pause — the same `tasks.goal_decide` RPC (and
/// therefore the same `goal_notify`/audit-trail machinery) the dashboard's
/// needs_human board already drives, per this file's header comment. Only
/// `retry`/`abort` are exposed from a notification card (see
/// `overlay/notifications_apps.rs`'s own doc comment on why `done`/
/// `takeover` are deliberately left to the dashboard for now); `action` is
/// still a plain `&str` here rather than a closed enum so this module does
/// not have to widen every time the dashboard grows another verb.
pub fn decide_goal_task(jwt: &str, task_id: &str, action: &str, note: &str) -> Result<(), RpcError> {
    ws_rpc::call_once(jwt, "tasks.goal_decide", json!({ "task_id": task_id, "action": action, "note": note }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, role: &str) -> AgentRef {
        AgentRef { id: id.to_string(), role: role.to_string() }
    }

    #[test]
    fn picks_the_main_role_agent_when_one_exists() {
        let agents = vec![agent("ops", "specialist"), agent("root", "main"), agent("qa", "worker")];
        assert_eq!(pick_default_agent(&agents).map(|a| a.id.as_str()), Some("root"));
    }

    #[test]
    fn falls_back_to_the_first_agent_when_no_main_role_exists() {
        let agents = vec![agent("ops", "specialist"), agent("qa", "worker")];
        assert_eq!(pick_default_agent(&agents).map(|a| a.id.as_str()), Some("ops"));
    }

    #[test]
    fn an_empty_list_has_no_default_agent() {
        assert_eq!(pick_default_agent(&[]), None);
    }

    #[test]
    fn a_main_role_agent_wins_even_when_listed_after_others() {
        // Order in the response is whatever the registry scan happened to
        // produce, not something this fn may rely on — `main` must win
        // regardless of position.
        let agents = vec![agent("a", "worker"), agent("b", "worker"), agent("c", "main")];
        assert_eq!(pick_default_agent(&agents).map(|a| a.id.as_str()), Some("c"));
    }
}
