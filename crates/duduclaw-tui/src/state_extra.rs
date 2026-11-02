//! Optional gateway data. Absence never implies idle, completed, or zero schedules.
use crate::{Error, Result, client::Client, sanitize::TerminalText};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};

#[derive(Clone, Debug, Deserialize)]
pub struct ScheduledRun {
    pub id: String,
    pub agent: TerminalText,
    pub next_run: TerminalText,
    pub source: ScheduleSource,
    #[serde(default)]
    pub detail: Value,
}
#[derive(Clone, Copy, Debug, Deserialize)]
pub enum ScheduleSource {
    Heartbeat,
    Cron,
}
#[derive(Clone, Debug, Default)]
pub struct Activity {
    pub working: bool,
    pub task_id: Option<String>,
    pub turns: Vec<Turn>,
    pub error: bool,
    pub completed_id: Option<String>,
}
/// Turn events have no run ID or outcome. Preserve only currently observed turns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    source: String,
    runtime: String,
    task_id: Option<String>,
}
pub(crate) fn activity_event(map: &mut HashMap<String, Activity>, name: &str, payload: &Value) {
    let Some(agent) = payload["agent_id"].as_str().filter(|id| id.len() <= 1024) else { return; };
    let source = payload["source"].as_str().unwrap_or("");
    let runtime = payload["runtime"].as_str().unwrap_or("");
    if source.len() > 128 || runtime.len() > 128 { return; }
    let task_id = payload["task_id"].as_str().filter(|id| !id.is_empty() && id.len() <= 1024).map(str::to_owned);
    let turn = Turn { source: source.into(), runtime: runtime.into(), task_id };
    match name {
        "agent.turn_started" => {
            let a = map.entry(agent.into()).or_default();
            if a.turns.len() < 256 { a.turns.push(turn); }
            a.working = true;
            a.task_id = a.turns.last().and_then(|t| t.task_id.clone());
        }
        "agent.turn_finished" => {
            let a = map.entry(agent.into()).or_default();
            if let Some(i) = a.turns.iter().position(|t| t == &turn) { a.turns.remove(i); }
            a.working = !a.turns.is_empty();
            a.task_id = a.turns.last().and_then(|t| t.task_id.clone());
            // Completion/error comes only from the authoritative runs history.
        }
        _ => {}
    }
}
pub(crate) async fn cost_summary(gateway: &impl crate::blocks::fetch::Gateway) -> Option<u64> {
    crate::blocks::autonomy::read(gateway, "cost.summary", json!({"hours":24})).await.ok()
        .as_ref().and_then(crate::state::cost_amount)
}

pub(crate) async fn schedules(client: &Client, role: crate::auth::Role, budget: &mut crate::budget::SnapshotBudget) -> Result<Vec<ScheduledRun>> {
    let mut result = Vec::new();
    // Cron is Admin-only; heartbeat is Manager+. No prediction from a cron expression:
    // timezones, disabled jobs and the scheduler's clock belong to the gateway.
    for (method, field, source) in [
        ("heartbeat.status", "heartbeats", ScheduleSource::Heartbeat),
        ("cron.list", "tasks", ScheduleSource::Cron),
    ] {
        if method == "cron.list" && role != crate::auth::Role::Admin { continue; }
        let response = client
            .call(method, json!({}), Duration::from_secs(5))
            .await?;
        budget.charge(&response, field)?;
        result.extend(schedule_rows(&response, method, field, source)?);
    }
    result.sort_by_key(|r| r.next_run.plain());
    Ok(result)
}

fn schedule_rows(response: &Value, method: &str, field: &str, source: ScheduleSource) -> Result<Vec<ScheduledRun>> {
    let rows = response.get(field).and_then(Value::as_array).ok_or(Error::Protocol)?;
    rows.iter().map(|row| {
        let agent = row["agent_id"].as_str().ok_or(Error::Protocol)?;
        let id = match source { ScheduleSource::Cron => row["id"].as_str().ok_or(Error::Protocol)?, ScheduleSource::Heartbeat => agent };
        let next = row["next_run"].as_str().unwrap_or(crate::strings::NOT_SCHEDULED);
        Ok(ScheduledRun { id: format!("{method}:{id}"), agent: TerminalText::new(agent.into()),
            next_run: TerminalText::new(next.into()), source, detail: row.clone() })
    }).collect()
}

/// Completion receipts survive a client restart via the existing runs.list RPC.
/// Its channel "running" status is a recency heuristic, so only live turn events
/// (or an explicit employee status) drive working; never infer it from history.
pub(crate) async fn recent_activity(
    client: &Client,
    employees: &[crate::state::Employee],
    budget: &mut crate::budget::SnapshotBudget,
) -> Result<HashMap<String, Activity>> {
    let mut result = HashMap::new();
    // Sequential requests let an exceeded global budget stop immediately.
    for employee in employees {
        let id = employee.id();
        let response = client.call("runs.list", json!({"agent_id":id, "limit":50}), Duration::from_secs(5)).await?;
        budget.charge(&response, "runs")?;
        result.insert(id.to_owned(), activity_from_runs(id, &response)?);
    }
    Ok(result)
}
fn activity_from_runs(agent: &str, response: &Value) -> Result<Activity> {
    let rows = response
        .get("runs")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol)?;
    let mut activity = Activity::default();
    for (index, row) in rows.iter().enumerate() {
        if row.get("agent_id").and_then(Value::as_str) != Some(agent) {
            return Err(Error::Protocol);
        }
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .ok_or(Error::Protocol)?;
        if index == 0 {
            activity.error = status == "error";
        }
        if status == "completed" && activity.completed_id.is_none() {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or(Error::Protocol)?;
            activity.completed_id = Some(format!("run:{agent}:{id}"));
        }
    }
    Ok(activity)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_schedule_shapes_keep_paused_jobs_and_missing_fields_explicit() {
        let cron = json!({"tasks":[{"id":"c1","name":"weekly","agent_id":"adu","cron":"0 9 * * 1","cron_timezone":"Asia/Taipei","enabled":false,"last_run_at":"2026-10-30","last_status":"success"}]});
        let rows = schedule_rows(&cron,"cron.list","tasks",ScheduleSource::Cron).unwrap();
        assert_eq!(rows.len(),1); assert!(rows[0].next_run.is(crate::strings::NOT_SCHEDULED));
        assert_eq!(rows[0].detail["enabled"],false); assert_eq!(rows[0].detail["last_status"],"success");
        let heartbeat=json!({"heartbeats":[{"agent_id":"adu","enabled":true,"interval_seconds":3600,"cron":"0 * * * *","cron_timezone":"UTC","last_run":"09:00","next_run":"10:00","total_runs":4,"active_runs":0,"max_concurrent":1}]});
        let rows=schedule_rows(&heartbeat,"heartbeat.status","heartbeats",ScheduleSource::Heartbeat).unwrap();
        assert!(rows[0].next_run.is("10:00")); assert!(rows[0].detail.get("last_status").is_none());
        assert!(schedule_rows(&json!({"tasks":[{"agent_id":"adu"}]}),"cron.list","tasks",ScheduleSource::Cron).is_err());
    }
    #[test]
    fn recent_completed_id_is_stable_and_running_history_is_not_live_work() {
        let response = json!({"runs":[{"id":"r2","agent_id":"adu","status":"running"},{"id":"r1","agent_id":"adu","status":"completed"}]});
        let a = activity_from_runs("adu", &response).unwrap();
        assert!(!a.working);
        assert_eq!(a.completed_id.as_deref(), Some("run:adu:r1"));
        assert!(activity_from_runs("other", &response).is_err());
    }
    #[test]
    fn unidentified_turn_does_not_fabricate_a_read_receipt() {
        let mut map = HashMap::new();
        activity_event(&mut map, "agent.turn_finished", &json!({"agent_id":"adu"}));
        assert!(map["adu"].completed_id.is_none());
        activity_event(&mut map, "agent.turn_started", &json!({"agent_id":"adu"}));
        assert!(map["adu"].working);
        activity_event(
            &mut map,
            "agent.turn_finished",
            &json!({"agent_id":"adu","run_id":"r1","status":"completed"}),
        );
        assert!(!map["adu"].working);
        assert!(map["adu"].completed_id.is_none());
    }
}
