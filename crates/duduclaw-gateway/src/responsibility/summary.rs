//! Dashboard/CLI read view of one responsibility (`responsibilities.get`).

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::cost::CostSource;
use super::service::ServiceError;
use crate::task_store::{OccurrenceRow, ResponsibilityRow, TaskStore, WakeupRow, period_key};

/// Model calls that are **not** charged to a responsibility's budget: they
/// run outside the occurrence's cost attribution scope. Fixed list — the
/// documentation is written from this field.
pub const COST_NOT_COUNTED: [&str; 8] = [
    "needs_human_trajectory_simulation",
    "kickoff_notification_narrative",
    "dispatch_policy_selection",
    "mcp_subprocess_action_guard_judge",
    // Work handed to another employee (`send_to_agent`, delegation) that
    // has no task under the occurrence: its spend carries no episode here.
    "work_delegated_to_other_employees",
    // One-off helpers started with `spawn_agent`: their turns run outside
    // the round's attribution scope.
    "ephemeral_sub_agents",
    // A task created where the MCP server does not know the run (no round
    // value: a server started from Bash, the Grok and Gemini CLI runtimes)
    // hangs under no run unless the employee names the run as its parent;
    // its spend is not counted. Correct placement also depends on the
    // platform keeping the `.mcp.json` duduclaw entry frozen.
    "tasks_created_without_round_information",
    // `tasks_create` with a `schedule` (a cron routine or a reminder): those
    // later turns run outside any run's tree (third review L3-7).
    "scheduled_routines_and_reminders",
];

/// One subscription on the current epoch, with its wake semantics spelled
/// out for the operator.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionView {
    pub wakeup_id: String,
    pub kind: String,
    pub event_name: Option<String>,
    pub event_filter_json: Option<String>,
    pub due_at: Option<String>,
    pub armed_by: String,
    pub state: String,
    /// `true` ⇒ stays armed after it wakes the responsibility (schedule slots,
    /// operator event subscriptions); `false` ⇒ spent by one wake.
    pub repeats: bool,
    /// Disarmed on this epoch because re-enabling could not restore it with
    /// its original condition (it is never armed with a wider one).
    pub unrestored: bool,
}

fn view(w: &WakeupRow, disabled: bool) -> SubscriptionView {
    let repeats = match w.kind.as_str() {
        "time" => w.recurring,
        "event" => !w.armed_by.starts_with("agent:"),
        _ => false,
    };
    SubscriptionView {
        wakeup_id: w.wakeup_id.clone(),
        kind: w.kind.clone(),
        event_name: w.event_name.clone(),
        event_filter_json: w.event_filter_json.clone(),
        due_at: w.due_at.clone(),
        armed_by: w.armed_by.clone(),
        state: w.state.clone(),
        repeats,
        unrestored: !disabled && w.state == "cancelled",
    }
}

fn internal(e: String) -> ServiceError {
    ServiceError::new("internal", e)
}

/// Dashboard/CLI `responsibilities.get` view.
#[derive(Debug, Clone, Serialize)]
pub struct ResponsibilitySummary {
    pub responsibility: ResponsibilityRow,
    pub next_due_at: Option<String>,
    pub pending_fires: usize,
    pub period_key: Option<String>,
    pub period_occurrences: i64,
    /// `None` when cost telemetry could not be read.
    pub period_spent: Option<i64>,
    pub open_occurrence: Option<OccurrenceRow>,
    /// Subscriptions of the current epoch (armed, consumed, or unrestored).
    pub subscriptions: Vec<SubscriptionView>,
    /// See [`COST_NOT_COUNTED`].
    pub cost_not_counted: Vec<&'static str>,
}

pub async fn summary(
    store: &TaskStore,
    cost: &dyn CostSource,
    id: &str,
    now: DateTime<Utc>,
) -> Result<Option<ResponsibilitySummary>, ServiceError> {
    let Some(resp) = store.get_responsibility(id).await.map_err(internal)? else {
        return Ok(None);
    };
    let wakeups = store.list_wakeups(id).await.map_err(internal)?;
    let next_due_at = wakeups
        .iter()
        .filter(|w| w.state == "armed" && w.control_epoch == resp.control_epoch)
        .filter_map(|w| w.due_at.clone())
        .min();
    let subscriptions = wakeups
        .iter()
        .filter(|w| w.control_epoch == resp.control_epoch)
        .map(|w| view(w, resp.state == "disabled"))
        .collect();
    let pending_fires = store.pending_fires(id).await.map_err(internal)?.len();
    let window = period_key(&resp.budget_period, &resp.budget_timezone, now).ok();
    let (period_occurrences, period_spent) = match &window {
        Some(w) => {
            let usage = store.period_usage(id, w).await.map_err(internal)?;
            let ids: Vec<String> = usage.entries.iter().map(|(t, _)| t.clone()).collect();
            let spent = super::cost::tree_spent(store, cost, &ids)
                .await
                .ok()
                .map(|m| {
                    usage
                        .entries
                        .iter()
                        .map(|(t, floor)| m.get(t).map_or(*floor, |c| c.cost.max(*floor)))
                        .sum()
                });
            (usage.occurrences, spent)
        }
        None => (0, None),
    };
    let open_occurrence = store
        .list_occurrences(id)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|o| o.outcome.is_none());
    Ok(Some(ResponsibilitySummary {
        responsibility: resp,
        next_due_at,
        pending_fires,
        period_key: window,
        period_occurrences,
        period_spent,
        open_occurrence,
        subscriptions,
        cost_not_counted: COST_NOT_COUNTED.to_vec(),
    }))
}
