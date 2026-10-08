//! Service layer for responsibilities — what the RPC / MCP / CLI surfaces
//! (C9, next round) call. Every function validates its input server-side,
//! returns a closed error code, and never trusts a client-supplied state,
//! epoch or revision beyond using it as a compare-and-set expectation.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::wake::{next_slot, parse_schedule};
use super::{EVENT_WHITELIST, ResponsibilityConfig, sha256_hex};
use crate::task_store::{
    RespCas, ResponsibilityContract, ResponsibilityRow, TaskStore, WakeupRow, resp_ts,
};

/// A refusal with a stable machine code (`code`) and a human detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceError {
    pub code: &'static str,
    pub detail: String,
}

impl ServiceError {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

fn internal(e: String) -> ServiceError {
    ServiceError::new("internal", e)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSpec {
    pub cron: String,
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventSubscription {
    pub event_name: String,
    /// `autopilot_engine::evaluate` condition tree; `None` = every event of
    /// that name owned by the responsibility's employee.
    #[serde(default)]
    pub filter: Option<Value>,
    /// Optional "did not happen by" time (≤ `stop_at`): a timeout fact.
    #[serde(default)]
    pub timeout_at: Option<DateTime<Utc>>,
}

/// Operator input for a new responsibility (and for a contract update).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsibilityInput {
    pub owner_agent_id: String,
    pub objective: String,
    pub acceptance_template: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
    /// `{enabled, on:[result|needs_decision|paused]}`; notifications are C8.
    #[serde(default)]
    pub notification_policy: Option<Value>,
    #[serde(default)]
    pub schedule: Option<ScheduleSpec>,
    #[serde(default)]
    pub event_subscriptions: Vec<EventSubscription>,
    pub occurrence_hours: i64,
    pub occurrence_cost_cap_cents: i64,
    pub budget_period: String,
    pub budget_timezone: String,
    pub period_cost_limit_cents: i64,
    pub period_occurrence_limit: i64,
    pub min_wake_interval_secs: i64,
    #[serde(default = "default_max_failures")]
    pub max_consecutive_failures: i64,
    pub stop_at: DateTime<Utc>,
    /// P5: `"explore"` runs every occurrence round in the read-only explore
    /// lane ([`super::lane`]); absent = no lane restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
}

fn default_max_failures() -> i64 {
    3
}

/// Validated, canonical form of an input.
struct Validated {
    contract: ResponsibilityContract,
    schedule: Option<ScheduleSpec>,
    events: Vec<EventSubscription>,
}

/// E-L14: the shortest allowed gap between two scheduled wake-ups.
pub const MIN_SCHEDULE_INTERVAL_SECS: i64 = 60;
/// S-L6: contract size limits.
pub const MAX_EVENT_SUBSCRIPTIONS: usize = 8;
pub const MAX_POLICY_BYTES: usize = 4096;

fn validate(
    home: &Path,
    input: &ResponsibilityInput,
    now: DateTime<Utc>,
) -> Result<Validated, ServiceError> {
    let cfg = ResponsibilityConfig::from_home(home);
    if !duduclaw_core::is_valid_agent_id(&input.owner_agent_id) {
        return Err(ServiceError::new(
            "invalid_owner",
            "owner_agent_id is not a valid employee id",
        ));
    }
    let objective = input.objective.trim();
    let template = input.acceptance_template.trim();
    if objective.is_empty() || objective.chars().count() > 4000 {
        return Err(ServiceError::new(
            "invalid_objective",
            "objective must be 1–4000 characters",
        ));
    }
    if template.is_empty() || template.chars().count() > 4000 {
        return Err(ServiceError::new(
            "invalid_acceptance",
            "acceptance_template must be 1–4000 characters",
        ));
    }
    if input.source_refs.len() > 32 || input.source_refs.iter().any(|r| r.chars().count() > 512) {
        return Err(ServiceError::new(
            "invalid_source_refs",
            "at most 32 references of ≤512 characters",
        ));
    }
    let checks: [(&'static str, bool); 7] = [
        (
            "invalid_occurrence_hours",
            (1..=72).contains(&input.occurrence_hours),
        ),
        (
            "invalid_occurrence_cost_cap",
            input.occurrence_cost_cap_cents > 0,
        ),
        (
            "invalid_budget_period",
            matches!(input.budget_period.as_str(), "day" | "week" | "month"),
        ),
        (
            "invalid_period_cost_limit",
            input.period_cost_limit_cents >= input.occurrence_cost_cap_cents,
        ),
        (
            "invalid_period_occurrence_limit",
            (1..=96).contains(&input.period_occurrence_limit),
        ),
        (
            "invalid_min_wake_interval",
            input.min_wake_interval_secs >= 300,
        ),
        (
            "invalid_max_failures",
            (1..=10).contains(&input.max_consecutive_failures),
        ),
    ];
    if let Some((code, _)) = checks.iter().find(|(_, ok)| !ok) {
        return Err(ServiceError::new(code, "value out of range"));
    }
    if input.budget_timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(ServiceError::new(
            "invalid_budget_timezone",
            "budget_timezone is not an IANA zone",
        ));
    }
    let max_stop = now + Duration::days(cfg.max_stop_at_days);
    if input.stop_at <= now || input.stop_at > max_stop {
        return Err(ServiceError::new(
            "invalid_stop_at",
            format!(
                "stop_at must be in the future and within {} days",
                cfg.max_stop_at_days
            ),
        ));
    }
    let schedule_json = match &input.schedule {
        Some(s) => {
            let raw = serde_json::json!({ "cron": s.cron, "timezone": s.timezone }).to_string();
            let (schedule, tz) =
                parse_schedule(&raw).map_err(|e| ServiceError::new("invalid_schedule", e))?;
            // E-L14: a cron that fires more often than once a minute is refused
            // (it would only produce dropped facts).
            let next: Vec<_> = schedule.after(&now.with_timezone(&tz)).take(10).collect();
            if next
                .windows(2)
                .any(|w| (w[1] - w[0]).num_seconds() < MIN_SCHEDULE_INTERVAL_SECS)
            {
                return Err(ServiceError::new(
                    "invalid_schedule",
                    format!(
                        "the schedule fires more often than every {MIN_SCHEDULE_INTERVAL_SECS} seconds"
                    ),
                ));
            }
            Some(raw)
        }
        None => None,
    };
    // S-L6: bounded contract size.
    if input.event_subscriptions.len() > MAX_EVENT_SUBSCRIPTIONS {
        return Err(ServiceError::new(
            "too_many_subscriptions",
            format!("at most {MAX_EVENT_SUBSCRIPTIONS} event subscriptions"),
        ));
    }
    if input
        .notification_policy
        .as_ref()
        .is_some_and(|p| p.to_string().len() > MAX_POLICY_BYTES)
    {
        return Err(ServiceError::new(
            "invalid_notification_policy",
            format!("notification_policy is limited to {MAX_POLICY_BYTES} bytes"),
        ));
    }
    for e in &input.event_subscriptions {
        if e.filter
            .as_ref()
            .is_some_and(|f| f.to_string().len() > MAX_POLICY_BYTES)
        {
            return Err(ServiceError::new(
                "invalid_event_filter",
                format!("a filter is limited to {MAX_POLICY_BYTES} bytes"),
            ));
        }
        if !EVENT_WHITELIST.contains(&e.event_name.as_str()) {
            return Err(ServiceError::new(
                "invalid_event_name",
                format!("only {EVENT_WHITELIST:?} can wake a responsibility"),
            ));
        }
        if let Some(f) = &e.filter {
            crate::handlers::validate_autopilot_conditions(f)
                .map_err(|d| ServiceError::new("invalid_event_filter", d))?;
        }
        if e.timeout_at.is_some_and(|t| t <= now || t > input.stop_at) {
            return Err(ServiceError::new(
                "invalid_event_timeout",
                "timeout_at must lie between now and stop_at",
            ));
        }
    }
    if schedule_json.is_none() && input.event_subscriptions.is_empty() {
        return Err(ServiceError::new(
            "no_wake_source",
            "a schedule or at least one event subscription is required",
        ));
    }
    // E-M7: canonical (sorted) so reordering the same subscriptions is not
    // a change.
    let mut names: Vec<&str> = input
        .event_subscriptions
        .iter()
        .map(|e| e.event_name.as_str())
        .collect();
    names.sort_unstable();
    let lane = super::lane::parse_input_lane(input.lane.as_deref())
        .map_err(|d| ServiceError::new("invalid_lane", d))?;
    // P5: the lane key is written only when set, so a responsibility without
    // it keeps a byte-identical scope (and contract hash).
    let scope_json = match lane {
        super::lane::RespLane::Normal => serde_json::json!({ "event_names": names }),
        super::lane::RespLane::Explore => {
            serde_json::json!({ "event_names": names, "lane": super::lane::LANE_EXPLORE })
        }
    }
    .to_string();
    let source_refs_json =
        serde_json::to_string(&input.source_refs).map_err(|e| internal(e.to_string()))?;
    let notification_policy_json = input
        .notification_policy
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "enabled": false, "on": [] }))
        .to_string();
    let stop_at = resp_ts(input.stop_at);
    let hash_input = serde_json::json!([
        objective,
        template,
        scope_json,
        source_refs_json,
        schedule_json,
        input.occurrence_hours,
        input.occurrence_cost_cap_cents,
        input.budget_period,
        input.budget_timezone,
        input.period_cost_limit_cents,
        input.period_occurrence_limit,
        input.min_wake_interval_secs,
        input.max_consecutive_failures,
        stop_at,
        input.event_subscriptions,
    ]);
    Ok(Validated {
        contract: ResponsibilityContract {
            objective: objective.to_string(),
            acceptance_template: template.to_string(),
            scope_json,
            source_refs_json,
            notification_policy_json,
            schedule_json,
            occurrence_hours: input.occurrence_hours,
            occurrence_cost_cap_cents: input.occurrence_cost_cap_cents,
            budget_period: input.budget_period.clone(),
            budget_timezone: input.budget_timezone.clone(),
            period_cost_limit_cents: input.period_cost_limit_cents,
            period_occurrence_limit: input.period_occurrence_limit,
            min_wake_interval_secs: input.min_wake_interval_secs,
            max_consecutive_failures: input.max_consecutive_failures,
            stop_at,
            contract_hash: sha256_hex(&hash_input.to_string()),
        },
        schedule: input.schedule.clone(),
        events: input.event_subscriptions.clone(),
    })
}

/// Subscriptions a validated contract arms for `epoch` (schedule slots start
/// at the first slot after `now`; nothing missed is replayed).
fn subscriptions(
    id: &str,
    epoch: i64,
    schedule: Option<&ScheduleSpec>,
    events: &[EventSubscription],
    armed_by: &str,
    event_floor: Option<i64>,
    now: DateTime<Utc>,
) -> Result<Vec<WakeupRow>, String> {
    let ts = resp_ts(now);
    let mut out = Vec::new();
    let base = |kind: &str| WakeupRow {
        wakeup_id: uuid::Uuid::new_v4().to_string(),
        responsibility_id: id.to_string(),
        control_epoch: epoch,
        kind: kind.to_string(),
        recurring: false,
        due_at: None,
        event_name: None,
        event_filter_json: None,
        approval_id: None,
        armed_by: armed_by.to_string(),
        state: "armed".into(),
        created_at: ts.clone(),
        updated_at: ts.clone(),
        armed_after_event_id: None,
    };
    if let Some(s) = schedule {
        let raw = serde_json::json!({ "cron": s.cron, "timezone": s.timezone }).to_string();
        let (sched, tz) = parse_schedule(&raw)?;
        let due = next_slot(&sched, tz, now).ok_or("schedule has no future slot")?;
        let mut w = base("time");
        w.recurring = true;
        w.due_at = Some(resp_ts(due));
        w.armed_by = "schedule".into();
        out.push(w);
    }
    for e in events {
        let mut w = base("event");
        w.event_name = Some(e.event_name.clone());
        w.event_filter_json = e.filter.as_ref().map(Value::to_string);
        w.due_at = e.timeout_at.map(resp_ts);
        w.armed_after_event_id = event_floor;
        out.push(w);
    }
    Ok(out)
}

/// E-H1: the newest event id right now — new event subscriptions only count
/// what comes after it. `None` when no event subscription is involved (no
/// read at all); an unreadable event store refuses the change.
pub(crate) async fn event_floor(home: &Path, needed: bool) -> Result<Option<i64>, ServiceError> {
    if !needed {
        return Ok(None);
    }
    let bus = crate::events_store::EventBusStore::open(home)
        .map_err(|e| ServiceError::new("event_store_unavailable", e))?;
    bus.max_id()
        .await
        .map(Some)
        .map_err(|e| ServiceError::new("event_store_unavailable", e))
}

/// S-L12: a responsibility needs a live owner (`agents/<id>/agent.toml`).
/// L-3: validate an input (and its owner) without writing anything — the
/// command line runs this before a card is built, so a card never shows
/// values `create` / `update_contract` would refuse.
pub fn check_input(
    home: &Path,
    input: &ResponsibilityInput,
    now: DateTime<Utc>,
) -> Result<(), ServiceError> {
    validate(home, input, now)?;
    if !owner_exists(home, &input.owner_agent_id) {
        return Err(ServiceError::new(
            "owner_missing",
            "the owner employee does not exist",
        ));
    }
    Ok(())
}

pub(crate) fn owner_exists(home: &Path, owner: &str) -> bool {
    duduclaw_core::is_valid_agent_id(owner)
        && home.join("agents").join(owner).join("agent.toml").is_file()
}

/// E-M7: two subscription sets are the same when they contain the same
/// (event name, filter, timeout) entries, in any order.
fn subscription_key_set(
    entries: impl Iterator<Item = (String, Option<String>, Option<String>)>,
) -> Vec<String> {
    let mut keys: Vec<String> = entries
        .map(|(name, filter, timeout)| {
            let filter = filter
                .and_then(|f| serde_json::from_str::<Value>(&f).ok())
                .map(|v| v.to_string())
                .unwrap_or_default();
            serde_json::json!([name, filter, timeout.unwrap_or_default()]).to_string()
        })
        .collect();
    keys.sort();
    keys
}

/// Create a responsibility. Refused while `[responsibilities] enabled` or
/// `[dispatch] enabled` is off (nothing would ever run it).
pub async fn create(
    store: &TaskStore,
    home: &Path,
    input: &ResponsibilityInput,
    created_by: &str,
    now: DateTime<Utc>,
) -> Result<ResponsibilityRow, ServiceError> {
    if !ResponsibilityConfig::from_home(home).enabled {
        return Err(ServiceError::new(
            "responsibilities_disabled",
            "[responsibilities] enabled is off",
        ));
    }
    if !crate::dispatch_engine::dispatch_engine_enabled(home) {
        return Err(ServiceError::new(
            "dispatch_disabled",
            "the dispatch engine is off; responsibilities would never run",
        ));
    }
    if created_by.trim().is_empty() {
        return Err(ServiceError::new("invalid_actor", "created_by is required"));
    }
    let v = validate(home, input, now)?;
    if !owner_exists(home, &input.owner_agent_id) {
        return Err(ServiceError::new(
            "owner_missing",
            "the owner employee does not exist",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let cursor = event_floor(home, !v.events.is_empty()).await?;
    let wakeups = subscriptions(
        &id,
        1,
        v.schedule.as_ref(),
        &v.events,
        &format!("operator:{created_by}"),
        cursor,
        now,
    )
    .map_err(|e| ServiceError::new("invalid_schedule", e))?;
    let ts = resp_ts(now);
    let c = v.contract;
    let row = ResponsibilityRow {
        responsibility_id: id,
        owner_agent_id: input.owner_agent_id.clone(),
        created_by: created_by.to_string(),
        objective: c.objective,
        acceptance_template: c.acceptance_template,
        scope_json: c.scope_json,
        source_refs_json: c.source_refs_json,
        notification_policy_json: c.notification_policy_json,
        schedule_json: c.schedule_json,
        occurrence_hours: c.occurrence_hours,
        occurrence_cost_cap_cents: c.occurrence_cost_cap_cents,
        budget_period: c.budget_period,
        budget_timezone: c.budget_timezone,
        period_cost_limit_cents: c.period_cost_limit_cents,
        period_occurrence_limit: c.period_occurrence_limit,
        min_wake_interval_secs: c.min_wake_interval_secs,
        max_consecutive_failures: c.max_consecutive_failures,
        stop_at: c.stop_at,
        state: "active".into(),
        state_reason: None,
        state_changed_by: Some(created_by.to_string()),
        state_changed_at: Some(ts.clone()),
        contract_revision: 1,
        contract_hash: c.contract_hash,
        control_epoch: 1,
        consecutive_failures: 0,
        last_occurrence_at: None,
        created_at: ts.clone(),
        updated_at: ts,
    };
    store
        .insert_responsibility(&row, &wakeups, cursor)
        .await
        .map_err(internal)?;
    Ok(row)
}

/// Operator contract update (CAS on `expected_contract_revision`). Changes to
/// the schedule or event subscriptions re-arm the subscriptions under a new
/// epoch in the same transaction; the running occurrence keeps its contract.
pub async fn update_contract(
    store: &TaskStore,
    home: &Path,
    id: &str,
    expected_contract_revision: i64,
    input: &ResponsibilityInput,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<RespCas, ServiceError> {
    let current = store
        .get_responsibility(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ServiceError::new("not_found", "responsibility not found"))?;
    if current.owner_agent_id != input.owner_agent_id {
        return Err(ServiceError::new(
            "owner_immutable",
            "the owner of a responsibility cannot change",
        ));
    }
    let v = validate(home, input, now)?;
    // E-M7: compare the whole subscription (name, filter, timeout) as a set
    // on the current epoch, not the filters in storage order.
    let current_subs = {
        let rows = store.list_wakeups(id).await.map_err(internal)?;
        subscription_key_set(
            rows.into_iter()
                .filter(|w| w.kind == "event" && w.control_epoch == current.control_epoch)
                // L-10: a row left disarmed (restore failed) is not a live
                // subscription; resending the same contract re-arms it.
                .filter(|w| w.state == "armed")
                .filter(|w| !w.armed_by.starts_with("agent:"))
                .map(|w| {
                    (
                        w.event_name.unwrap_or_default(),
                        w.event_filter_json,
                        w.due_at,
                    )
                }),
        )
    };
    let new_subs = subscription_key_set(v.events.iter().map(|e| {
        (
            e.event_name.clone(),
            e.filter.as_ref().map(Value::to_string),
            e.timeout_at.map(resp_ts),
        )
    }));
    let sub_changed = current.schedule_json != v.contract.schedule_json
        || super::lane::scope_event_names(&current.scope_json)
            != super::lane::scope_event_names(&v.contract.scope_json)
        || current_subs != new_subs;
    let floor = event_floor(home, sub_changed && !v.events.is_empty()).await?;
    let schedule = v.schedule.clone();
    let events = v.events.clone();
    let armed_by = format!("operator:{actor}");
    let make = move |epoch: i64| {
        subscriptions(id, epoch, schedule.as_ref(), &events, &armed_by, floor, now)
    };
    let resub: Option<&(dyn Fn(i64) -> Result<Vec<WakeupRow>, String> + Sync)> =
        if sub_changed { Some(&make) } else { None };
    store
        .update_responsibility_contract(id, expected_contract_revision, &v.contract, resub, now)
        .await
        .map_err(internal)
}

/// Operator controls (CAS on `expected_epoch`).
pub async fn pause(
    store: &TaskStore,
    id: &str,
    epoch: i64,
    actor: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<RespCas, ServiceError> {
    store
        .pause_responsibility(id, epoch, actor, reason, now)
        .await
        .map_err(internal)
}

pub async fn resume(
    store: &TaskStore,
    id: &str,
    epoch: i64,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<RespCas, ServiceError> {
    store
        .resume_responsibility(id, epoch, actor, now)
        .await
        .map_err(internal)
}

pub async fn disable(
    store: &TaskStore,
    id: &str,
    epoch: i64,
    actor: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<RespCas, ServiceError> {
    store
        .disable_responsibility(id, epoch, actor, reason, now)
        .await
        .map_err(internal)
}

pub async fn clear_failures(
    store: &TaskStore,
    id: &str,
    epoch: i64,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<RespCas, ServiceError> {
    store
        .clear_responsibility_failures(id, epoch, actor, now)
        .await
        .map_err(internal)
}

/// Re-enable a disabled responsibility: epoch +1, and every subscription the
/// disable disarmed is re-armed with its original filter and timeout. A
/// schedule slot restarts at the next slot after `now` (nothing missed is
/// replayed). A subscription that cannot be restored exactly — its timeout or
/// one-shot time already passed or lies beyond `stop_at`, its filter no
/// longer validates, or its schedule is unreadable — is recorded on the new
/// epoch still disarmed (`state = 'cancelled'`) and reported by the summary
/// as unrestored. Nothing is ever armed with a wider condition than before.
pub async fn enable(
    store: &TaskStore,
    id: &str,
    epoch: i64,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<RespCas, ServiceError> {
    // E-H1: re-armed event subscriptions only count events after now.
    let home = store
        .db_path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ServiceError::new("internal", "task store has no home"))?;
    let has_events = store
        .list_wakeups(id)
        .await
        .map_err(internal)?
        .iter()
        .any(|w| w.kind == "event");
    let floor = event_floor(&home, has_events).await?;
    store
        .enable_responsibility(id, epoch, actor, now, |r, prior| {
            Ok(prior
                .iter()
                .map(|w| restore_subscription(r, w, floor, now))
                .collect())
        })
        .await
        .map_err(internal)
}

/// One disarmed subscription → its copy on the responsibility's new epoch,
/// armed only when it can be restored exactly.
pub(crate) fn restore_subscription(
    r: &ResponsibilityRow,
    w: &WakeupRow,
    event_floor: Option<i64>,
    now: DateTime<Utc>,
) -> WakeupRow {
    let ts = resp_ts(now);
    let stop_at = crate::task_store::parse_ts(&r.stop_at);
    let mut copy = WakeupRow {
        wakeup_id: uuid::Uuid::new_v4().to_string(),
        control_epoch: r.control_epoch,
        state: "cancelled".into(),
        created_at: ts.clone(),
        updated_at: ts,
        armed_after_event_id: if w.kind == "event" {
            event_floor.or(w.armed_after_event_id)
        } else {
            None
        },
        ..w.clone()
    };
    let in_window = |due: &Option<String>| match due {
        None => true,
        Some(d) => crate::task_store::parse_ts(d)
            .is_some_and(|t| t > now && stop_at.is_some_and(|s| t <= s)),
    };
    let restorable = match (w.kind.as_str(), w.recurring) {
        ("time", true) => match r.schedule_json.as_deref().map(parse_schedule) {
            Some(Ok((sched, tz))) => match next_slot(&sched, tz, now) {
                Some(next) if stop_at.is_some_and(|s| next <= s) => {
                    copy.due_at = Some(resp_ts(next));
                    true
                }
                _ => false,
            },
            _ => false,
        },
        ("time", false) => w.due_at.is_some() && in_window(&w.due_at),
        ("event", _) => {
            let filter_ok = match w.event_filter_json.as_deref() {
                None => true,
                Some(raw) => serde_json::from_str::<Value>(raw)
                    .ok()
                    .is_some_and(|f| crate::handlers::validate_autopilot_conditions(&f).is_ok()),
            };
            filter_ok
                && w.event_name
                    .as_deref()
                    .is_some_and(|n| EVENT_WHITELIST.contains(&n))
                && in_window(&w.due_at)
        }
        // Decisions belong to a question asked during a past occurrence.
        _ => false,
    };
    if restorable {
        copy.state = "armed".into();
    }
    copy
}

pub use super::agent_wake::{ask, followup};
