//! P2-A C9 — employee-facing responsibility tools.
//!
//! An employee can read only the responsibilities it owns (user ids
//! stripped), and while working on its own open occurrence it can
//! schedule one follow-up or ask the operator one question. It can never
//! create, enable, resume, re-contract or raise the limits of a
//! responsibility, and it has no steering tool. Callers are resolved with
//! [`RecordActor`]; an operator key is refused by all three (operators use
//! the dashboard or the CLI).

use std::path::Path;

use serde_json::Value;

use super::record_authz::{RecordActor, check_actor_identity};
use super::{tool_error, tool_text};

pub(crate) const RESPONSIBILITY_TOOLS: [&str; 3] = [
    "responsibility_get",
    "responsibility_followup",
    "responsibility_ask",
];

fn enabled(home: &Path) -> bool {
    duduclaw_gateway::responsibility::ResponsibilityConfig::from_home(home).enabled
}

fn open_store(home: &Path) -> Result<duduclaw_gateway::task_store::TaskStore, Value> {
    duduclaw_gateway::task_store::TaskStore::open(home)
        .map_err(|e| tool_error(&format!("open task store: {e}")))
}

/// The employee identity for wake-up tools; operators are refused.
fn employee(
    home: &Path,
    actor: RecordActor<'_>,
    tool: &str,
    target: &str,
) -> Result<String, Value> {
    check_actor_identity(home, actor, target, tool).map_err(|e| tool_error(&e))?;
    match actor.agent() {
        Some(a) if !a.trim().is_empty() => Ok(a.trim().to_string()),
        _ => Err(tool_error(&format!(
            "{tool} 只能由負責這份持續任務的 AI 員工在執行中呼叫；操作者請使用儀表板。"
        ))),
    }
}

/// Fields that carry a dashboard user id or an operator label: never shown
/// to an employee (S-L1).
const PERSON_FIELDS: [&str; 6] = [
    "created_by",
    "armed_by",
    "state_changed_by",
    "requested_by",
    "decided_by",
    "updated_by",
];

fn strip_person_fields(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for k in PERSON_FIELDS {
                map.remove(k);
            }
            for child in map.values_mut() {
                strip_person_fields(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_person_fields),
        _ => {}
    }
}

/// Only the owning employee reads its responsibilities (design §8.2, S-L1 /
/// E-L10); an operator key is refused like the other two tools, so what
/// `tools/list` shows (employees only) is what can be called.
pub(crate) async fn handle_responsibility_get(
    args: &Value,
    home: &Path,
    actor: RecordActor<'_>,
) -> Value {
    if !enabled(home) {
        return tool_error("持續任務功能目前關閉。");
    }
    let id = args
        .get("responsibility_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let caller = match employee(home, actor, "responsibility_get", id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let store = match open_store(home) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if id.is_empty() {
        return match store.list_responsibilities(Some(&caller)).await {
            Ok(rows) => {
                let mut v = serde_json::json!({ "responsibilities": rows });
                strip_person_fields(&mut v);
                tool_text(&v.to_string())
            }
            Err(e) => tool_error(&e),
        };
    }
    let not_found = || tool_error("找不到這份持續任務。");
    match store.get_responsibility(id).await {
        Ok(Some(r)) if r.owner_agent_id == caller => {}
        Ok(_) => return not_found(),
        Err(e) => return tool_error(&e),
    }
    let cost = duduclaw_gateway::responsibility::TelemetryCostSource::new(home);
    match duduclaw_gateway::responsibility::summary::summary(&store, &cost, id, chrono::Utc::now())
        .await
    {
        Ok(Some(s)) => {
            let mut v = serde_json::json!({ "summary": s });
            strip_person_fields(&mut v);
            tool_text(&v.to_string())
        }
        Ok(None) => not_found(),
        Err(e) => tool_error(&format!("{}: {}", e.code, e.detail)),
    }
}

pub(crate) async fn handle_responsibility_followup(
    args: &Value,
    home: &Path,
    actor: RecordActor<'_>,
) -> Value {
    if !enabled(home) {
        return tool_error("持續任務功能目前關閉。");
    }
    let id = args
        .get("responsibility_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let caller = match employee(home, actor, "responsibility_followup", id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let Some(due) = args
        .get("due_at")
        .and_then(|v| v.as_str())
        .and_then(duduclaw_gateway::task_store::parse_ts)
    else {
        return tool_error("due_at 必須是 RFC3339 時間。");
    };
    let store = match open_store(home) {
        Ok(s) => s,
        Err(e) => return e,
    };
    match duduclaw_gateway::responsibility::service::followup(
        &store,
        id,
        &caller,
        due,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(w) => tool_text(
            &serde_json::json!({ "wakeup_id": w.wakeup_id, "due_at": w.due_at }).to_string(),
        ),
        Err(e) => tool_error(&format!("{}: {}", e.code, e.detail)),
    }
}

pub(crate) async fn handle_responsibility_ask(
    args: &Value,
    home: &Path,
    actor: RecordActor<'_>,
) -> Value {
    if !enabled(home) {
        return tool_error("持續任務功能目前關閉。");
    }
    let id = args
        .get("responsibility_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let caller = match employee(home, actor, "responsibility_ask", id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let question = args.get("question").and_then(|v| v.as_str()).unwrap_or("");
    let options: Vec<String> = args
        .get("options")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|o| o.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let ttl = args
        .get("ttl_secs")
        .and_then(|v| v.as_i64())
        .unwrap_or(24 * 3600);
    let store = match open_store(home) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home) {
        Ok(b) => b,
        Err(e) => return tool_error(&format!("approval store unavailable: {e}")),
    };
    match duduclaw_gateway::responsibility::service::ask(
        &store,
        &broker,
        id,
        &caller,
        question,
        &options,
        ttl,
        None,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(w) => tool_text(
            &serde_json::json!({ "wakeup_id": w.wakeup_id, "approval_id": w.approval_id })
                .to_string(),
        ),
        Err(e) => tool_error(&format!("{}: {}", e.code, e.detail)),
    }
}
