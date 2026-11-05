//! Finite task collections. The SQL query consumes this call's selected grants;
//! a narrowed capability never reaches the employee-wide legacy list handlers.
use duduclaw_gateway::task_store::TaskStore;
use duduclaw_isolation::capability::ResourceGrant;
use duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall;
use serde_json::{Value, json};

pub(crate) async fn execute(call: &AuthorizedToolCall) -> Option<Value> {
    if call.employee_scope() || !matches!(call.name(), "tasks_list" | "activity_list" | "list_cron_tasks" | "goals_list" | "list_reminders" | "responsibility_get") {
        return None;
    }
    let refused = || crate::mcp::tool_error("無法確認這次查詢的資源授權，已拒絕讀取。");
    let Some(current) = crate::mcp_isolation::current() else { return Some(refused()); };
    if current.context.record() != call.context().record()
        || !duduclaw_core::is_valid_agent_id(&current.agent)
        || duduclaw_core::is_reserved_agent_id(&current.agent)
        || current.agent == duduclaw_core::UNTRUSTED_AGENT_ID
        || verify_live(call).is_err() {
        return Some(refused());
    }
    let ids: Vec<String> = call.resources().iter().filter_map(|resource| match resource {
        ResourceGrant::Task { id } => Some(id.as_str().to_owned()), _ => None,
    }).collect();
    if call.name() == "list_cron_tasks" {
        return Some(list_cron(call, &current.home, &current.agent, &ids).await);
    }
    if call.name() == "list_reminders" {
        return Some(list_reminders(call, &current.home, &current.agent, &ids).await);
    }
    if call.name() == "responsibility_get" {
        return Some(list_responsibilities(call, &current.home, &current.agent, &ids).await);
    }
    let store = match TaskStore::open(&current.home) {
        Ok(store) => store,
        Err(error) => return Some(crate::mcp::tool_error(&format!("open task store: {error}"))),
    };
    let arguments = call.arguments();
    let result = match call.name() {
        "goals_list" => {
            let status = arguments.get("status").and_then(Value::as_str)
                .filter(|s| matches!(*s, "active" | "done" | "archived"));
            let rows = match store.list_goals_isolated(&current.agent, &ids, status).await {
                Ok(rows) => rows, Err(error) => return Some(crate::mcp::tool_error(&format!("list goals: {error}"))),
            };
            let limit = crate::mcp::clamp_limit(arguments, 50, 200) as usize;
            let goals: Vec<Value> = rows.iter().take(limit).map(crate::mcp::goal_row_to_json).collect();
            crate::mcp::tool_text(&json!({"goals": goals, "total": rows.len()}).to_string())
        }
        "tasks_list" => {
            let assigned_to = selection(arguments, "assigned_to", &current.agent);
            // Keep the original filter response shape while refusing to widen
            // the SQL scope to another employee, even for a permissive resolver.
            let mut rows = if assigned_to.is_some_and(|agent| agent != current.agent) {
                Vec::new()
            } else {
                match store.list_tasks_isolated(&current.agent, &ids,
                    arguments.get("status").and_then(Value::as_str),
                    arguments.get("priority").and_then(Value::as_str)).await {
                    Ok(rows) => rows,
                    Err(error) => return Some(crate::mcp::tool_error(&format!("list tasks: {error}"))),
                }
            };
            rows.retain(|row| duduclaw_gateway::review_evidence::audience::agent_may_read_task_row(
                &current.home, &current.agent, row));
            let limit = crate::mcp::clamp_limit(arguments, 20, 100) as usize;
            let tasks: Vec<Value> = rows.iter().take(limit).map(crate::mcp::task_row_to_json).collect();
            crate::mcp::tool_text(&json!({"tasks":tasks, "total":rows.len(),
                "filtered_by_agent":assigned_to}).to_string())
        }
        "activity_list" => {
            let agent = selection(arguments, "agent_id", &current.agent);
            let (rows, total) = if agent.is_some_and(|agent| agent != current.agent) {
                (Vec::new(), 0)
            } else {
                match store.list_activity_isolated(&current.agent, &ids,
                    arguments.get("event_type").and_then(Value::as_str),
                    arguments.get("task_id").and_then(Value::as_str),
                    crate::mcp::clamp_limit(arguments, 20, 100)).await {
                    Ok(result) => result,
                    Err(error) => return Some(crate::mcp::tool_error(&format!("list activity: {error}"))),
                }
            };
            // The bounded query returns only this employee's own activity.
            // This is exactly the original audience helper's own-author rule;
            // task assignment is independently rechecked inside the SQL query.
            if rows.iter().any(|row| row.agent_id != current.agent) { return Some(refused()); }
            let activities: Vec<Value> = rows.iter().map(crate::mcp::activity_row_to_json).collect();
            crate::mcp::tool_text(&json!({"activities":activities, "total":total}).to_string())
        }
        _ => unreachable!(),
    };
    if verify_live(call).is_err() { return Some(refused()); }
    Some(result)
}

fn selection<'a>(arguments: &'a Value, key: &str, default: &'a str) -> Option<&'a str> {
    match arguments.get(key).and_then(Value::as_str) {
        Some("*") => None,
        Some(value) if !value.is_empty() => Some(value),
        _ => Some(default),
    }
}

fn verify_live(call: &AuthorizedToolCall) -> Result<(), duduclaw_mcp_boundary::VerificationError> {
    call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)
}

#[cfg(test)]
#[path = "mcp_isolation_collections_tests.rs"]
mod tests;

async fn list_cron(call: &AuthorizedToolCall, home: &std::path::Path, agent: &str, grants: &[String]) -> Value {
    let refused = || crate::mcp::tool_error("無法確認排程的資源授權，已拒絕讀取。");
    let ids: Option<Vec<String>> = grants.iter().map(|id| id.strip_prefix("cron:").map(str::to_owned)).collect();
    let Some(ids) = ids else { return refused(); };
    let arguments = call.arguments();
    let filter = arguments.get("agent_id").and_then(Value::as_str).unwrap_or("");
    let enabled = arguments.get("enabled_only").and_then(Value::as_bool)
        .or_else(|| arguments.get("enabled_only").and_then(Value::as_str).map(|s| s == "true")).unwrap_or(false);
    if verify_live(call).is_err() { return refused(); }
    let store = match duduclaw_gateway::cron_store::CronStore::open(home) {
        Ok(store) => store, Err(error) => return crate::mcp::tool_error(&format!("open cron store: {error}")),
    };
    let rows = if !filter.is_empty() && filter != agent { Vec::new() } else {
        match store.list_isolated(agent, &ids, enabled).await {
            Ok(rows) => rows, Err(error) => return crate::mcp::tool_error(&format!("list cron tasks: {error}")),
        }
    };
    if verify_live(call).is_err() { return refused(); }
    crate::mcp::render_cron_tasks(&rows, filter)
}

async fn list_responsibilities(call: &AuthorizedToolCall, home: &std::path::Path, agent: &str, grants: &[String]) -> Value {
    let refused = || crate::mcp::tool_error("無法確認持續任務的資源授權，已拒絕讀取。");
    let Some(ids) = grants.iter().map(|id| id.strip_prefix("responsibility:").map(str::to_owned)).collect::<Option<Vec<_>>>() else { return refused(); };
    if verify_live(call).is_err() { return refused(); }
    if !duduclaw_gateway::responsibility::ResponsibilityConfig::from_home(home).enabled {
        return crate::mcp::tool_error("持續任務功能目前關閉。");
    }
    let id = call.arguments().get("responsibility_id").and_then(Value::as_str).unwrap_or("").trim();
    if !id.is_empty() {
        if !ids.iter().any(|allowed| allowed == id) { return refused(); }
        // Existing single-record handler retains the own-employee, summary and
        // person-field gates; it never falls back to listing all when ID is set.
        let out = crate::mcp::handle_responsibility_get(call.arguments(), home,
            crate::mcp::RecordActor::Agent(agent)).await;
        return if verify_live(call).is_ok() { out } else { refused() };
    }
    let store = match TaskStore::open(home) { Ok(s) => s, Err(e) => return crate::mcp::tool_error(&e) };
    match store.list_responsibilities_isolated(agent, &ids).await {
        Ok(rows) => {
            if verify_live(call).is_err() { return refused(); }
            let mut value = json!({"responsibilities": rows});
            crate::mcp::strip_person_fields(&mut value);
            crate::mcp::tool_text(&value.to_string())
        }
        Err(e) => crate::mcp::tool_error(&e),
    }
}

async fn list_reminders(call: &AuthorizedToolCall, home: &std::path::Path, agent: &str, grants: &[String]) -> Value {
    let refused = || crate::mcp::tool_error("無法確認提醒的資源授權，已拒絕讀取。");
    let Some(ids) = grants.iter().map(|id| id.strip_prefix("reminder:").map(str::to_owned)).collect::<Option<Vec<_>>>() else { return refused(); };
    if verify_live(call).is_err() { return refused(); }
    let args = call.arguments();
    let requested = args.get("agent_id").and_then(Value::as_str).unwrap_or(agent);
    let rows = if requested != agent { Vec::new() } else {
        match duduclaw_gateway::reminder_scheduler::list_reminders_isolated(home, agent, &ids,
            args.get("status").and_then(Value::as_str)).await {
            Ok(rows) => rows, Err(e) => return crate::mcp::tool_error(&e),
        }
    };
    if verify_live(call).is_err() { return refused(); }
    crate::mcp::render_reminders(&rows)
}
