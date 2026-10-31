//! Running due scheduled assignments, from the cron scheduler's tick.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Duration, Utc};
use duduclaw_agent::registry::AgentRegistry;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use super::manage::{CreatorRight, creator_right};
use super::spec::{ScheduleSpec, Zone, parse_utc};
use super::{AssignKind, AssignPayload, CATCH_UP_HOURS, MAX_CONSECUTIVE_FAILURES, audit, notify};
use crate::goal_create_core::{GoalCreateRequest, create_scheduled_goal_task};
use crate::task_store::{
    ACTIVE_STATUSES, AssignScheduleRow, AssignScheduleSlot, ScheduleFireOutcome, TaskRow, TaskStore,
};

/// Most rows handled per tick (the rest wait 30 seconds).
const PER_TICK: i64 = 50;

/// What one tick did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub created: Vec<String>,
    pub skipped: usize,
    pub paused: usize,
}

/// Homes with a tick in progress: a tick that outlasts the 30-second
/// interval (a plan-first goal waits for its planner) is not overlapped.
fn running() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    static RUNNING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    RUNNING
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct TickGuard(PathBuf);

impl Drop for TickGuard {
    fn drop(&mut self) {
        running().remove(&self.0);
    }
}

/// Fire every due schedule once. A tick already running for `home` makes
/// this one return an empty report.
pub async fn tick(
    home: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    now: DateTime<Utc>,
) -> Result<TickReport, String> {
    if !running().insert(home.to_path_buf()) {
        return Ok(TickReport::default());
    }
    let _guard = TickGuard(home.to_path_buf());
    let store = TaskStore::open(home)?;
    // A corrupt/unreadable config cannot restore the looser defaults.
    super::caps(home)?;
    let due = store.due_assign_schedules(now, PER_TICK).await?;
    let mut report = TickReport::default();
    for row in due {
        match fire_one(home, &store, registry, &row, now).await {
            Fired::Created(id) => report.created.push(id),
            Fired::Skipped => report.skipped += 1,
            Fired::Paused => report.paused += 1,
            Fired::Deferred => {}
        }
    }
    Ok(report)
}

enum Fired {
    Created(String),
    Skipped,
    Paused,
    /// Nothing written; tried again next tick.
    Deferred,
}

/// The task id of the run for `slot`: the same schedule and slot always
/// give the same id, so a retry never creates a second run. UUID-shaped
/// (version 5 nibble, RFC 4122 variant).
pub(crate) fn task_id_for_slot(schedule_id: &str, slot: &str) -> String {
    let digest = Sha256::digest(format!("assign-schedule\0{schedule_id}\0{slot}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

async fn fire_one(
    home: &Path,
    store: &TaskStore,
    registry: &Arc<RwLock<AgentRegistry>>,
    row: &AssignScheduleRow,
    now: DateTime<Utc>,
) -> Fired {
    let Some(slot_s) = row.next_fire_at.clone() else {
        return Fired::Deferred;
    };
    let parsed = (
        parse_utc(&slot_s),
        serde_json::from_str::<ScheduleSpec>(&row.schedule_json).ok(),
        serde_json::from_str::<AssignPayload>(&row.payload_json).ok(),
        Zone::parse(row.timezone.as_deref()).ok(),
        AssignKind::parse(&row.kind),
    );
    let (Some(slot), Some(spec), Some(payload), Some(zone), Some(kind)) = parsed else {
        return auto_pause(home, store, row, &slot_s, "create_failed").await;
    };
    let next = spec.next_after(slot.max(now), zone).map(|t| t.to_rfc3339());
    let skip = |reason: &str| ScheduleFireOutcome::Skipped {
        reason: reason.to_string(),
    };

    if now - slot > Duration::hours(CATCH_UP_HOURS) {
        return advance_skip(
            home,
            store,
            row,
            &slot_s,
            next.as_deref(),
            skip("missed"),
            now,
        )
        .await;
    }
    // The creator's right, on their live account, every time.
    let via = match creator_right(home, &row.creator_user_id, &row.agent_id) {
        Err(e) => {
            tracing::warn!(schedule = %row.id, error = %e, "scheduled assignment: identity store unreadable; not firing");
            return Fired::Deferred;
        }
        Ok(CreatorRight::Lost) => {
            return auto_pause(home, store, row, &slot_s, "access_lost").await;
        }
        Ok(CreatorRight::Allowed { is_admin }) => {
            if !is_admin && (spec.admin_only() || payload.admin_only()) {
                return auto_pause(home, store, row, &slot_s, "access_lost").await;
            }
            let min_interval = match super::caps(home) {
                Ok((_, min_interval)) => min_interval,
                Err(e) => {
                    tracing::warn!(schedule = %row.id, error = %e, "scheduled assignment: configuration unavailable; not firing");
                    return Fired::Deferred;
                }
            };
            if spec
                .validate_policy(is_admin, now, zone, min_interval)
                .is_err()
            {
                return auto_pause(home, store, row, &slot_s, "schedule_restricted").await;
            }
            if is_admin { "admin" } else { "schedule" }
        }
    };
    if !agent_operational(registry, &row.agent_id).await {
        return auto_pause(home, store, row, &slot_s, "agent_unavailable").await;
    }
    if duduclaw_core::autonomy_gate::check(
        home, &row.agent_id, duduclaw_core::autonomy_gate::AutonomyPath::AssignSchedule,
    ).is_err() {
        return advance_skip(home, store, row, &slot_s, next.as_deref(), skip("autonomy_paused"), now).await;
    }
    if previous_run_open(store, row).await {
        return advance_skip(
            home,
            store,
            row,
            &slot_s,
            next.as_deref(),
            skip("previous_open"),
            now,
        )
        .await;
    }
    let task_id = task_id_for_slot(&row.id, &slot_s);
    let created = match store.get_task(&task_id).await {
        // Recovery for a run written by an older gateway before its slot
        // advance. New inserts consume both in a single transaction.
        Ok(Some(_)) => store
            .advance_assign_schedule(
                &row.id,
                &slot_s,
                next.as_deref(),
                &ScheduleFireOutcome::Created {
                    task_id: task_id.clone(),
                },
                now,
            )
            .await
            .map(|_| ()),
        _ => match create_run(
            home,
            store,
            row,
            kind,
            payload,
            via,
            &AssignScheduleSlot {
                id: &row.id,
                slot: &slot_s,
                next: next.as_deref(),
                now,
            },
        )
        .await
        {
            // Lost a race with another writer of the same run: it exists.
            Err(_) if matches!(store.get_task(&task_id).await, Ok(Some(_))) => Ok(()),
            other => other,
        },
    };
    match created {
        Ok(()) => {
            audit::fired(home, row, &slot_s, &task_id);
            Fired::Created(task_id)
        }
        Err(e) => {
            // A pause/remove/other tick won the insertion fence. Do not
            // turn a deliberate control change into a create failure.
            match store.get_assign_schedule(&row.id).await {
                Ok(Some(current))
                    if current.status == "active" && current.next_fire_at == row.next_fire_at => {}
                Ok(_) => return Fired::Deferred,
                Err(read_error) => {
                    tracing::warn!(schedule = %row.id, create_error = %e, error = %read_error,
                        "scheduled assignment: failed insert could not be reconciled");
                    return Fired::Deferred;
                }
            }
            tracing::warn!(schedule = %row.id, error = %e, "scheduled assignment: create failed");
            if row.consecutive_failures + 1 >= MAX_CONSECUTIVE_FAILURES {
                return auto_pause(home, store, row, &slot_s, "create_failed").await;
            }
            let failed = ScheduleFireOutcome::Failed;
            if let Err(e) = store
                .advance_assign_schedule(&row.id, &slot_s, next.as_deref(), &failed, now)
                .await
            {
                tracing::warn!(schedule = %row.id, error = %e, "scheduled assignment: failure not recorded");
            }
            audit::skipped(home, row, &slot_s, "create_failed");
            Fired::Skipped
        }
    }
}

async fn agent_operational(registry: &Arc<RwLock<AgentRegistry>>, agent: &str) -> bool {
    registry
        .read()
        .await
        .get(agent)
        .is_some_and(|a| a.config.agent.status.is_operational())
}

/// The run this schedule created last is still unfinished.
async fn previous_run_open(store: &TaskStore, row: &AssignScheduleRow) -> bool {
    let Some(last) = row.last_task_id.as_deref() else {
        return false;
    };
    match store.get_task(last).await {
        Ok(Some(t)) => !t.archived && ACTIVE_STATUSES.contains(&t.status.as_str()),
        Ok(None) => false,
        // Unreadable: assume it is open (do not pile up runs).
        Err(_) => true,
    }
}

async fn advance_skip(
    home: &Path,
    store: &TaskStore,
    row: &AssignScheduleRow,
    slot: &str,
    next: Option<&str>,
    outcome: ScheduleFireOutcome,
    now: DateTime<Utc>,
) -> Fired {
    let reason = match &outcome {
        ScheduleFireOutcome::Skipped { reason } => reason.clone(),
        _ => "skipped".into(),
    };
    match store
        .advance_assign_schedule(&row.id, slot, next, &outcome, now)
        .await
    {
        Ok(true) => {
            audit::skipped(home, row, slot, &reason);
            Fired::Skipped
        }
        Ok(false) => Fired::Deferred,
        Err(e) => {
            tracing::warn!(schedule = %row.id, error = %e, "scheduled assignment: skip not recorded");
            Fired::Deferred
        }
    }
}

/// Pause the schedule at this slot (nothing is created), record why and
/// tell the creator.
async fn auto_pause(
    home: &Path,
    store: &TaskStore,
    row: &AssignScheduleRow,
    slot: &str,
    reason: &str,
) -> Fired {
    match store
        .pause_assign_schedule(&row.id, reason, Utc::now())
        .await
    {
        Ok(true) => {
            audit::skipped(home, row, slot, reason);
            audit::auto_paused(home, row, slot, reason);
            notify::creator_paused(home, row, reason).await;
            Fired::Paused
        }
        Ok(false) => Fired::Deferred,
        Err(e) => {
            tracing::warn!(schedule = %row.id, error = %e, "scheduled assignment: pause not recorded");
            Fired::Deferred
        }
    }
}

/// Create the run: a goal through `goal_create_core`, or a board task.
async fn create_run(
    home: &Path,
    store: &TaskStore,
    row: &AssignScheduleRow,
    kind: AssignKind,
    p: AssignPayload,
    via: &str,
    slot: &AssignScheduleSlot<'_>,
) -> Result<(), String> {
    let task_id = task_id_for_slot(&row.id, slot.slot);
    let principal = format!("user:{}", row.creator_user_id);
    let source = match (&row.source_channel, &row.source_chat_id) {
        (Some(c), Some(id)) if !c.is_empty() && !id.is_empty() => Some((c.clone(), id.clone())),
        _ => None,
    };
    match kind {
        AssignKind::Goal => {
            let req = GoalCreateRequest {
                agent_id: row.agent_id.clone(),
                created_by: "goal:schedule".to_string(),
                description: p.description,
                acceptance_criteria: p.acceptance_criteria,
                priority: p.priority,
                outcome: p.outcome,
                duration_hours: p.duration_hours,
                cost_cap_cents: p.cost_cap_cents,
                // The creator's right is re-read at every fire: a creator who
                // is no longer an Admin gets at most the employee's own limit.
                timeout_secs: p.timeout_secs.map(|secs| {
                    if via == "admin" {
                        secs
                    } else {
                        secs.min(crate::runtime_config::employee_timeout_ceiling(home, &row.agent_id))
                    }
                }),
                risk_boundary: p.risk_boundary,
                require_beliefs: p.require_beliefs,
                plan_first: p.plan_first,
                parent_task_id: None,
                source_label: "排程交辦".to_string(),
                on_behalf_of: Some(principal),
                created_via: Some(row.created_via.clone()),
                source,
                task_id: Some(task_id.to_string()),
                grant_via: Some(via.to_string()),
            };
            create_scheduled_goal_task(home, store, req, slot)
                .await
                .map(|_| ())
        }
        AssignKind::Task => {
            let mut task = TaskRow::new(
                task_id.to_string(),
                p.title.clone(),
                p.description,
                if p.priority.is_empty() {
                    "medium".into()
                } else {
                    p.priority
                },
                row.agent_id.clone(),
                row.creator_user_id.clone(),
            );
            task.tags = p.tags;
            task.on_behalf_of = Some(principal);
            task.created_via = Some(row.created_via.clone());
            store.insert_assign_schedule_task(&task, slot).await?;
            let activity = crate::task_store::ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "task_created".into(),
                agent_id: row.agent_id.clone(),
                task_id: Some(task.id.clone()),
                summary: format!("排程交辦：{}", duduclaw_core::truncate_chars(&p.title, 200)),
                timestamp: Utc::now().to_rfc3339(),
                metadata: Some(
                    serde_json::json!({ "schedule_id": row.id, "grant_via": via }).to_string(),
                ),
            };
            if let Err(e) = store.append_activity(&activity).await {
                tracing::warn!(error = %e, "scheduled assignment: activity row not written");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "fire_tests.rs"]
mod tests;
