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
}
#[derive(Clone, Copy, Debug, Deserialize)]
pub enum ScheduleSource {
    Heartbeat,
    Cron,
}
#[derive(Clone, Debug, Default)]
pub struct Activity {
    pub working: bool,
    pub error: bool,
    pub completed_id: Option<String>,
}
/// Only explicit, identified turn events can create a durable completion badge.
pub(crate) fn activity_event(map: &mut HashMap<String, Activity>, name: &str, payload: &Value) {
    let Some(agent) = payload.get("agent_id").and_then(Value::as_str) else {
        return;
    };
    match name {
        "agent.turn_started" => {
            map.entry(agent.into()).or_default().working = true;
        }
        "agent.turn_finished" => {
            let a = map.entry(agent.into()).or_default();
            a.working = false;
            a.error = payload
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| matches!(s, "failed" | "error"));
            if !a.error
                && let Some(id) = payload.get("run_id").and_then(Value::as_str)
            {
                a.completed_id = Some(format!("run:{agent}:{id}"));
            }
        }
        _ => {}
    }
}
pub(crate) async fn schedules(client: &Client) -> Result<Vec<ScheduledRun>> {
    let mut result = Vec::new();
    // These are existing Admin-only RPCs. No prediction from a cron expression:
    // timezones, disabled jobs and the scheduler's clock belong to the gateway.
    for (method, field, source) in [
        ("heartbeat.status", "heartbeats", ScheduleSource::Heartbeat),
        ("cron.list", "tasks", ScheduleSource::Cron),
    ] {
        let response = client
            .call(method, json!({}), Duration::from_secs(5))
            .await?;
        let rows = response
            .get(field)
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        for row in rows {
            if row.get("enabled").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let Some(next) = row.get("next_run").and_then(Value::as_str) else {
                continue;
            };
            let Some(agent) = row.get("agent_id").and_then(Value::as_str) else {
                continue;
            };
            let id = row.get("id").and_then(Value::as_str).unwrap_or(agent);
            result.push(ScheduledRun {
                id: format!("{method}:{id}"),
                agent: TerminalText::new(agent.into()),
                next_run: TerminalText::new(next.into()),
                source,
            });
        }
    }
    result.sort_by_key(|r| r.next_run.plain());
    Ok(result)
}

/// Completion receipts survive a client restart via the existing runs.list RPC.
/// Its channel "running" status is a recency heuristic, so only live turn events
/// (or an explicit employee status) drive working; never infer it from history.
pub(crate) async fn recent_activity(
    client: &Client,
    employees: &[crate::state::Employee],
) -> Result<HashMap<String, Activity>> {
    use futures_util::{StreamExt, TryStreamExt, stream};
    let ids: Vec<_> = employees.iter().map(|e| e.id().to_owned()).collect();
    stream::iter(ids)
        .map(|id| {
            let client = client.clone();
            async move {
                let response = client
                    .call(
                        "runs.list",
                        json!({"agent_id":id, "limit":50}),
                        Duration::from_secs(5),
                    )
                    .await?;
                let activity = activity_from_runs(&id, &response)?;
                Ok((id, activity))
            }
        })
        .buffer_unordered(4)
        .try_collect()
        .await
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
        assert_eq!(map["adu"].completed_id.as_deref(), Some("run:adu:r1"));
    }
}
