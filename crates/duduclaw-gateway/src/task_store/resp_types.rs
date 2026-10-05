//! Row types and small pure helpers for the P2-A tables (see `resp_schema`).

use super::*;
use chrono::{Datelike, SecondsFormat};

/// Canonical timestamp for every P2-A column: whole seconds, `Z` suffix.
/// One shape everywhere keeps the `due_at <= ?now` string comparisons exact.
pub fn resp_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse an RFC3339 column; `None` on anything malformed.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Calendar window a responsibility's budget is counted in. The key embeds the
/// timezone so a later timezone change can never merge two windows.
pub fn period_key(period: &str, timezone: &str, now: DateTime<Utc>) -> Result<String, String> {
    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| format!("invalid budget timezone: {timezone}"))?;
    let local = now.with_timezone(&tz);
    match period {
        "day" => Ok(format!("day:{}@{timezone}", local.format("%Y-%m-%d"))),
        "week" => {
            let w = local.iso_week();
            Ok(format!("week:{}-W{:02}@{timezone}", w.year(), w.week()))
        }
        "month" => Ok(format!("month:{}@{timezone}", local.format("%Y-%m"))),
        other => Err(format!("invalid budget period: {other}")),
    }
}

/// Steering states visible to readers. `Applied` means "placed in the payload
/// of round N that was handed to the employee" — never "the employee adopted it".
pub const STEERING_OPEN_STATES: [&str; 2] = ["pending", "delivering"];
/// Maximum open (pending + delivering) steering entries per task.
pub const STEERING_OPEN_LIMIT: i64 = 10;
/// Maximum tasks one stop request may cancel.
pub const STOP_TREE_LIMIT: usize = 500;
/// How many members of one stopped tree reconciliation looks at; beyond this
/// the stop is reported as unverifiable.
pub const STOP_TREE_SCAN_LIMIT: usize = 5000;
/// M-5 / M3-4: open (unfinished) direct sub-tasks an employee may hang under
/// one task through MCP. Schedules and reminders are not sub-tasks.
pub const MAX_CHILDREN_PER_TASK: i64 = 200;
/// S-L6: pending wake facts kept per responsibility; beyond it a new fact is
/// recorded only as `dropped(pending_cap)` without its data.
pub const MAX_PENDING_FIRES: i64 = 100;
/// S-L6: settled / dropped facts are pruned after this many days.
pub const FIRE_RETENTION_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsibilityRow {
    pub responsibility_id: String,
    pub owner_agent_id: String,
    pub created_by: String,
    pub objective: String,
    pub acceptance_template: String,
    pub scope_json: String,
    pub source_refs_json: String,
    pub notification_policy_json: String,
    pub schedule_json: Option<String>,
    pub occurrence_hours: i64,
    pub occurrence_cost_cap_cents: i64,
    pub budget_period: String,
    pub budget_timezone: String,
    pub period_cost_limit_cents: i64,
    pub period_occurrence_limit: i64,
    pub min_wake_interval_secs: i64,
    pub max_consecutive_failures: i64,
    pub stop_at: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub state_changed_by: Option<String>,
    pub state_changed_at: Option<String>,
    pub contract_revision: i64,
    pub contract_hash: String,
    pub control_epoch: i64,
    pub consecutive_failures: i64,
    pub last_occurrence_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub(crate) const RESP_COLUMNS: &str = "responsibility_id, owner_agent_id, created_by, objective, \
     acceptance_template, scope_json, source_refs_json, notification_policy_json, schedule_json, \
     occurrence_hours, occurrence_cost_cap_cents, budget_period, budget_timezone, period_cost_limit_cents, \
     period_occurrence_limit, min_wake_interval_secs, max_consecutive_failures, stop_at, state, \
     state_reason, state_changed_by, state_changed_at, contract_revision, contract_hash, \
     control_epoch, consecutive_failures, last_occurrence_at, created_at, updated_at";

pub(crate) fn row_to_responsibility(r: &rusqlite::Row<'_>) -> rusqlite::Result<ResponsibilityRow> {
    Ok(ResponsibilityRow {
        responsibility_id: r.get(0)?,
        owner_agent_id: r.get(1)?,
        created_by: r.get(2)?,
        objective: r.get(3)?,
        acceptance_template: r.get(4)?,
        scope_json: r.get(5)?,
        source_refs_json: r.get(6)?,
        notification_policy_json: r.get(7)?,
        schedule_json: r.get(8)?,
        occurrence_hours: r.get(9)?,
        occurrence_cost_cap_cents: r.get(10)?,
        budget_period: r.get(11)?,
        budget_timezone: r.get(12)?,
        period_cost_limit_cents: r.get(13)?,
        period_occurrence_limit: r.get(14)?,
        min_wake_interval_secs: r.get(15)?,
        max_consecutive_failures: r.get(16)?,
        stop_at: r.get(17)?,
        state: r.get(18)?,
        state_reason: r.get(19)?,
        state_changed_by: r.get(20)?,
        state_changed_at: r.get(21)?,
        contract_revision: r.get(22)?,
        contract_hash: r.get(23)?,
        control_epoch: r.get(24)?,
        consecutive_failures: r.get(25)?,
        last_occurrence_at: r.get(26)?,
        created_at: r.get(27)?,
        updated_at: r.get(28)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeupRow {
    pub wakeup_id: String,
    pub responsibility_id: String,
    pub control_epoch: i64,
    pub kind: String,
    pub recurring: bool,
    pub due_at: Option<String>,
    pub event_name: Option<String>,
    pub event_filter_json: Option<String>,
    pub approval_id: Option<String>,
    pub armed_by: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    /// Event subscriptions only (E-H1): the newest `events.db` id that
    /// existed when this row was armed. Only later events count, so a
    /// re-enable, a new subscription or a re-subscribe never turns earlier
    /// events into new facts. `None` ⇒ no floor (rows from before this
    /// column; every production arming path sets it).
    #[serde(default)]
    pub armed_after_event_id: Option<i64>,
}

pub(crate) const WAKEUP_COLUMNS: &str = "wakeup_id, responsibility_id, control_epoch, kind, \
     recurring, due_at, event_name, event_filter_json, approval_id, armed_by, state, created_at, \
     updated_at, armed_after_event_id";

pub(crate) fn row_to_wakeup(r: &rusqlite::Row<'_>) -> rusqlite::Result<WakeupRow> {
    Ok(WakeupRow {
        wakeup_id: r.get(0)?,
        responsibility_id: r.get(1)?,
        control_epoch: r.get(2)?,
        kind: r.get(3)?,
        recurring: r.get::<_, i64>(4)? != 0,
        due_at: r.get(5)?,
        event_name: r.get(6)?,
        event_filter_json: r.get(7)?,
        approval_id: r.get(8)?,
        armed_by: r.get(9)?,
        state: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        armed_after_event_id: r.get(13)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireRow {
    pub fire_id: String,
    pub wakeup_id: String,
    pub responsibility_id: String,
    pub control_epoch: i64,
    pub fire_key: String,
    pub reason: String,
    pub data_json: Option<String>,
    pub guard_flags_json: Option<String>,
    pub state: String,
    pub drop_reason: Option<String>,
    pub occurrence_task_id: Option<String>,
    pub observed_at: String,
    pub settled_at: Option<String>,
}

pub(crate) const FIRE_COLUMNS: &str = "fire_id, wakeup_id, responsibility_id, control_epoch, \
     fire_key, reason, data_json, guard_flags_json, state, drop_reason, occurrence_task_id, \
     observed_at, settled_at";

pub(crate) fn row_to_fire(r: &rusqlite::Row<'_>) -> rusqlite::Result<FireRow> {
    Ok(FireRow {
        fire_id: r.get(0)?,
        wakeup_id: r.get(1)?,
        responsibility_id: r.get(2)?,
        control_epoch: r.get(3)?,
        fire_key: r.get(4)?,
        reason: r.get(5)?,
        data_json: r.get(6)?,
        guard_flags_json: r.get(7)?,
        state: r.get(8)?,
        drop_reason: r.get(9)?,
        occurrence_task_id: r.get(10)?,
        observed_at: r.get(11)?,
        settled_at: r.get(12)?,
    })
}

/// One fact a wake source observed. Written once per `(responsibility, key)`.
#[derive(Debug, Clone)]
pub struct NewFire {
    pub wakeup_id: String,
    pub fire_key: String,
    pub reason: String,
    pub data_json: Option<String>,
    pub guard_flags_json: Option<String>,
    /// Write the fire already `dropped` with this reason (self-triggered
    /// events): kept for debugging, never consumed.
    pub dropped: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceRow {
    pub responsibility_id: String,
    pub occurrence_key: String,
    pub task_id: String,
    pub contract_revision: i64,
    pub control_epoch: i64,
    pub period_key: String,
    pub reserved_cents: i64,
    pub charged_cents: Option<i64>,
    pub cost_basis: Option<String>,
    pub outcome: Option<String>,
    pub predecessor_task_id: Option<String>,
    pub created_at: String,
    pub settled_at: Option<String>,
}

pub(crate) const OCC_COLUMNS: &str = "responsibility_id, occurrence_key, task_id, \
     contract_revision, control_epoch, period_key, reserved_cents, charged_cents, cost_basis, outcome, \
     predecessor_task_id, created_at, settled_at";

pub(crate) fn row_to_occurrence(r: &rusqlite::Row<'_>) -> rusqlite::Result<OccurrenceRow> {
    Ok(OccurrenceRow {
        responsibility_id: r.get(0)?,
        occurrence_key: r.get(1)?,
        task_id: r.get(2)?,
        contract_revision: r.get(3)?,
        control_epoch: r.get(4)?,
        period_key: r.get(5)?,
        reserved_cents: r.get(6)?,
        charged_cents: r.get(7)?,
        cost_basis: r.get(8)?,
        outcome: r.get(9)?,
        predecessor_task_id: r.get(10)?,
        created_at: r.get(11)?,
        settled_at: r.get(12)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringRow {
    pub steering_id: String,
    pub task_id: String,
    pub seq: i64,
    pub body: String,
    pub body_hash: String,
    pub guard_flags_json: String,
    pub submitted_by: String,
    pub submitted_via: String,
    pub submitted_authority_revision: i64,
    pub client_request_id: String,
    pub state: String,
    pub intent_id: Option<String>,
    pub applied_round: Option<i64>,
    pub applied_message_id: Option<String>,
    pub applied_authority_revision: Option<i64>,
    pub discard_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub(crate) const STEERING_COLUMNS: &str = "steering_id, task_id, seq, body, body_hash, \
     guard_flags_json, submitted_by, submitted_via, submitted_authority_revision, \
     client_request_id, state, intent_id, applied_round, applied_message_id, \
     applied_authority_revision, discard_reason, created_at, updated_at";

pub(crate) fn row_to_steering(r: &rusqlite::Row<'_>) -> rusqlite::Result<SteeringRow> {
    Ok(SteeringRow {
        steering_id: r.get(0)?,
        task_id: r.get(1)?,
        seq: r.get(2)?,
        body: r.get(3)?,
        body_hash: r.get(4)?,
        guard_flags_json: r.get(5)?,
        submitted_by: r.get(6)?,
        submitted_via: r.get(7)?,
        submitted_authority_revision: r.get(8)?,
        client_request_id: r.get(9)?,
        state: r.get(10)?,
        intent_id: r.get(11)?,
        applied_round: r.get(12)?,
        applied_message_id: r.get(13)?,
        applied_authority_revision: r.get(14)?,
        discard_reason: r.get(15)?,
        created_at: r.get(16)?,
        updated_at: r.get(17)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIntentRow {
    pub intent_id: String,
    pub task_id: String,
    pub iter: i64,
    pub authority_revision: i64,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

pub(crate) fn row_to_intent(r: &rusqlite::Row<'_>) -> rusqlite::Result<DispatchIntentRow> {
    Ok(DispatchIntentRow {
        intent_id: r.get(0)?,
        task_id: r.get(1)?,
        iter: r.get(2)?,
        authority_revision: r.get(3)?,
        state: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

pub(crate) const INTENT_COLUMNS: &str =
    "intent_id, task_id, iter, authority_revision, state, created_at, updated_at";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRequestRow {
    pub root_task_id: String,
    pub requested_by: String,
    pub requested_at: String,
    pub expected_authority_revision: i64,
    pub affected_task_ids: Vec<String>,
    pub state: String,
    pub detail_json: Option<String>,
    pub updated_at: String,
    /// The stop was not a manager's decision: an occurrence it ends counts
    /// as an unsuccessful run (an employee cannot dodge the failure pause by
    /// having its own failing run stopped).
    pub counts_as_failure: bool,
}

pub(crate) fn row_to_stop(r: &rusqlite::Row<'_>) -> rusqlite::Result<StopRequestRow> {
    let ids: String = r.get(4)?;
    Ok(StopRequestRow {
        root_task_id: r.get(0)?,
        requested_by: r.get(1)?,
        requested_at: r.get(2)?,
        expected_authority_revision: r.get(3)?,
        affected_task_ids: serde_json::from_str(&ids).unwrap_or_default(),
        state: r.get(5)?,
        detail_json: r.get(6)?,
        updated_at: r.get(7)?,
        counts_as_failure: r.get::<_, i64>(8)? != 0,
    })
}

pub(crate) const STOP_COLUMNS: &str = "root_task_id, requested_by, requested_at, \
     expected_authority_revision, affected_task_ids_json, state, detail_json, updated_at, \
     counts_as_failure";

/// Result of a compare-and-set on a responsibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespCas {
    /// The write took effect; the fresh row.
    Applied(ResponsibilityRow),
    /// The expected epoch/state/revision no longer matched; the current row.
    Conflict(Option<ResponsibilityRow>),
}

/// SQL predicate: the task id bound to `param` (e.g. `"?1"`) is inside any
/// stop request's tree.
/// "The task or one of its ancestors has a stop request." Ancestry, not the
/// recorded member list, so a tree too large for one pass and a child added
/// later are both covered (S-M2). `param` must be a bound parameter.
pub(crate) fn in_stop_tree_sql(param: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM task_stop_requests sr WHERE sr.root_task_id IN ( \
           WITH RECURSIVE anc(id, depth) AS ( \
             SELECT {param}, 0 \
             UNION ALL SELECT t.parent_task_id, anc.depth + 1 FROM tasks t JOIN anc ON t.id = anc.id \
              WHERE t.parent_task_id IS NOT NULL AND anc.depth < {STOP_ANCESTRY_DEPTH}) \
           SELECT id FROM anc))"
    )
}

/// How far up the parent chain the stop check looks.
pub const STOP_ANCESTRY_DEPTH: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_keys_follow_the_local_calendar() {
        let t = parse_ts("2026-10-04T17:30:00Z").unwrap(); // 01:30 on the 5th in Taipei
        assert_eq!(
            period_key("day", "Asia/Taipei", t).unwrap(),
            "day:2026-10-05@Asia/Taipei"
        );
        assert_eq!(period_key("day", "UTC", t).unwrap(), "day:2026-10-04@UTC");
        assert_eq!(
            period_key("week", "Asia/Taipei", t).unwrap(),
            "week:2026-W41@Asia/Taipei"
        );
        assert_eq!(
            period_key("month", "Asia/Taipei", t).unwrap(),
            "month:2026-10@Asia/Taipei"
        );
        assert!(period_key("year", "UTC", t).is_err());
        assert!(period_key("day", "Mars/Base", t).is_err());
    }

    #[test]
    fn canonical_timestamps_compare_as_strings() {
        let a = resp_ts(parse_ts("2026-10-05T09:00:00+08:00").unwrap());
        let b = resp_ts(parse_ts("2026-10-05T01:00:01Z").unwrap());
        assert_eq!(a, "2026-10-05T01:00:00Z");
        assert!(a < b);
    }
}
