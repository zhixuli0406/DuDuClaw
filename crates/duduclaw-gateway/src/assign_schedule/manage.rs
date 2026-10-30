//! Creating, listing and controlling scheduled assignments, shared by the
//! dashboard RPCs and the chat `/交辦` command. Callers authenticate; this
//! module decides what the authenticated person may do.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::spec::{ScheduleSpec, SpecError, Zone};
use super::{AssignKind, AssignPayload, audit, caps};
use crate::task_store::{AssignScheduleRow, TaskStore};

/// The person acting: a dashboard account (never the gateway admin token,
/// which names nobody and so cannot be re-checked at fire time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creator {
    pub user_id: String,
    pub is_admin: bool,
}

impl Creator {
    /// `None` for anything that is not an account (`system`, a system
    /// sender name, a malformed id).
    pub fn new(user_id: &str, is_admin: bool) -> Option<Self> {
        crate::trigger_route::assign::principal_for_user(user_id).map(|_| Self {
            user_id: user_id.to_string(),
            is_admin,
        })
    }

    pub fn principal(&self) -> String {
        format!("user:{}", self.user_id)
    }
}

/// A schedule to create. The caller has already checked that `creator` may
/// assign to `agent_id` right now.
#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub agent_id: String,
    pub kind: AssignKind,
    pub payload: AssignPayload,
    pub spec: ScheduleSpec,
    pub timezone: Option<String>,
    /// `dashboard`, or the chat channel.
    pub created_via: String,
    /// The conversation progress goes back to (chat).
    pub source: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    Spec(SpecError),
    /// The creator already holds `max` active or paused schedules.
    TooMany {
        max: i64,
    },
    /// A per-task risk boundary from someone who is not an Admin.
    RiskBoundaryAdminOnly,
    /// Nothing to do (empty goal / title).
    Empty,
    Store(String),
}

impl CreateError {
    /// Machine code (RPC errors, audit).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Spec(e) => e.code(),
            Self::TooMany { .. } => "too_many_schedules",
            Self::RiskBoundaryAdminOnly => "risk_boundary_admin_only",
            Self::Empty => "empty_task",
            Self::Store(_) => "store_error",
        }
    }
}

/// Create a schedule (status `active`, first fire computed now).
pub async fn create(
    home: &Path,
    store: &TaskStore,
    creator: &Creator,
    req: NewSchedule,
    now: DateTime<Utc>,
) -> Result<AssignScheduleRow, CreateError> {
    if let Some(cap) = req.payload.cost_cap_cents {
        if req.kind != AssignKind::Goal || cap == 0 || cap > i64::MAX as u64 {
            return Err(CreateError::Store("cost_cap_cents must be a positive integer for a goal".into()));
        }
    }
    if req.payload.timeout_secs.is_some() && req.kind != AssignKind::Goal {
        return Err(CreateError::Store("timeout_secs is only supported for goals".into()));
    }
    crate::goal_create_core::check_timeout_secs(req.payload.timeout_secs).map_err(CreateError::Store)?;
    crate::goal_create_core::check_timeout_widening(
        home,
        &req.agent_id,
        req.payload.timeout_secs,
        creator.is_admin,
    )
    .map_err(CreateError::Store)?;
    let zone = Zone::parse(req.timezone.as_deref()).map_err(CreateError::Spec)?;
    let (max_active, min_interval) = caps(home).map_err(CreateError::Store)?;
    let first = req
        .spec
        .validate(creator.is_admin, now, zone, min_interval)
        .map_err(CreateError::Spec)?;
    if req.payload.admin_only() && !creator.is_admin {
        return Err(CreateError::RiskBoundaryAdminOnly);
    }
    let title = display_title(req.kind, &req.payload).ok_or(CreateError::Empty)?;
    let now_s = now.to_rfc3339();
    let row = AssignScheduleRow {
        id: uuid::Uuid::new_v4().to_string(),
        creator_user_id: creator.user_id.clone(),
        agent_id: req.agent_id,
        kind: req.kind.as_str().to_string(),
        title,
        payload_json: serde_json::to_string(&req.payload)
            .map_err(|e| CreateError::Store(e.to_string()))?,
        schedule_json: serde_json::to_string(&req.spec)
            .map_err(|e| CreateError::Store(e.to_string()))?,
        timezone: req.timezone.filter(|t| !t.trim().is_empty()),
        status: "active".into(),
        pause_reason: None,
        created_via: req.created_via,
        source_channel: req.source.as_ref().map(|s| s.0.clone()),
        source_chat_id: req.source.map(|s| s.1),
        next_fire_at: Some(first.to_rfc3339()),
        last_fire_at: None,
        last_task_id: None,
        last_outcome: None,
        fire_count: 0,
        skip_count: 0,
        consecutive_failures: 0,
        created_at: now_s.clone(),
        updated_at: now_s,
    };
    match store.insert_assign_schedule(&row, max_active).await {
        Ok(true) => {
            audit::created(home, &row);
            Ok(row)
        }
        Ok(false) => Err(CreateError::TooMany { max: max_active }),
        Err(e) => Err(CreateError::Store(e)),
    }
}

/// The title shown in lists: the task title, or the goal text (both cut to
/// 120 characters).
fn display_title(kind: AssignKind, p: &AssignPayload) -> Option<String> {
    let text = match kind {
        AssignKind::Task => p.title.trim(),
        AssignKind::Goal => p.description.trim(),
    };
    (!text.is_empty()).then(|| duduclaw_core::truncate_chars(text, 120))
}

/// May the creator still assign to `agent`, on their live account?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorRight {
    Allowed {
        is_admin: bool,
    },
    /// Missing, suspended or offboarded account, or no Operator right left.
    Lost,
}

/// Read the creator's right from `users.db` now (team grants only in the
/// Enterprise edition, as everywhere else). `Err` = the identity store could
/// not be read: the caller must not fire (fail closed).
pub fn creator_right(home: &Path, user_id: &str, agent: &str) -> Result<CreatorRight, String> {
    let Some(db) = crate::decision_notify::open_user_db(home)? else {
        return Ok(CreatorRight::Lost);
    };
    let Some(access) = crate::trigger_route::access::load_account(home, &db, user_id)? else {
        return Ok(CreatorRight::Lost);
    };
    if access.role == duduclaw_auth::UserRole::Admin {
        return Ok(CreatorRight::Allowed { is_admin: true });
    }
    Ok(if access.may_assign(agent) {
        CreatorRight::Allowed { is_admin: false }
    } else {
        CreatorRight::Lost
    })
}

/// Pause / resume / remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageAction {
    Pause,
    Resume,
    Remove,
}

impl ManageAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageError {
    /// No such schedule, or not the actor's (the two read the same, so ids
    /// of other people's schedules cannot be probed).
    NotFound,
    /// Already in the asked state (or finished).
    NotApplicable,
    /// Resume refused: the creator may no longer assign to the employee.
    CreatorLostAccess,
    /// Resume refused: a one-shot whose time has passed.
    TimePassed,
    /// The deployment raised its minimum interval after creation.
    ScheduleRestricted,
    Store(String),
}

impl ManageError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::NotApplicable => "not_applicable",
            Self::CreatorLostAccess => "creator_lost_access",
            Self::TimePassed => "time_passed",
            Self::ScheduleRestricted => "schedule_restricted",
            Self::Store(_) => "store_error",
        }
    }
}

/// Whether `actor` may see and control `row`: its creator, or an Admin.
pub fn may_manage(actor: &Creator, row: &AssignScheduleRow) -> bool {
    actor.is_admin || row.creator_user_id == actor.user_id
}

/// Apply `action` to schedule `id` for `actor`.
pub async fn manage(
    home: &Path,
    store: &TaskStore,
    actor: &Creator,
    id: &str,
    action: ManageAction,
    now: DateTime<Utc>,
) -> Result<AssignScheduleRow, ManageError> {
    let row = store
        .get_assign_schedule(id)
        .await
        .map_err(ManageError::Store)?
        .filter(|r| r.status != "removed" && may_manage(actor, r))
        .ok_or(ManageError::NotFound)?;
    let changed = match action {
        ManageAction::Pause => store.pause_assign_schedule(id, "by_user", now).await,
        ManageAction::Remove => store.remove_assign_schedule(id, now).await,
        ManageAction::Resume => {
            if row.status != "paused" {
                return Err(ManageError::NotApplicable);
            }
            let next = resume_slot(home, &row, now)?;
            store
                .resume_assign_schedule(id, &next.to_rfc3339(), now)
                .await
        }
    }
    .map_err(ManageError::Store)?;
    if !changed {
        return Err(ManageError::NotApplicable);
    }
    audit::managed(home, &row, actor, action);
    store
        .get_assign_schedule(id)
        .await
        .map_err(ManageError::Store)?
        .ok_or(ManageError::NotFound)
}

/// The slot a resumed schedule continues from, after re-checking the
/// creator's right (the creator's, not the actor's: an Admin resuming a
/// schedule does not lend it their own rights).
fn resume_slot(
    home: &Path,
    row: &AssignScheduleRow,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, ManageError> {
    let spec: ScheduleSpec =
        serde_json::from_str(&row.schedule_json).map_err(|e| ManageError::Store(e.to_string()))?;
    let payload: AssignPayload =
        serde_json::from_str(&row.payload_json).map_err(|e| ManageError::Store(e.to_string()))?;
    let is_admin = match creator_right(home, &row.creator_user_id, &row.agent_id)
        .map_err(ManageError::Store)?
    {
        CreatorRight::Allowed { is_admin }
            if is_admin || !(spec.admin_only() || payload.admin_only()) =>
        {
            is_admin
        }
        _ => return Err(ManageError::CreatorLostAccess),
    };
    let zone =
        Zone::parse(row.timezone.as_deref()).map_err(|e| ManageError::Store(e.code().into()))?;
    spec.validate_policy(
        is_admin,
        now,
        zone,
        caps(home).map_err(ManageError::Store)?.1,
    )
    .map_err(|_| ManageError::ScheduleRestricted)?;
    spec.next_after(now, zone).ok_or(ManageError::TimePassed)
}

/// The dashboard / chat view of one schedule. `creator_name` is filled for
/// Admin listings of other people's schedules.
pub fn row_json(row: &AssignScheduleRow, creator_name: Option<&str>) -> Value {
    let schedule: Value = serde_json::from_str(&row.schedule_json).unwrap_or(Value::Null);
    let payload: AssignPayload = serde_json::from_str(&row.payload_json).unwrap_or_default();
    json!({
        "id": row.id,
        "agent_id": row.agent_id,
        "kind": row.kind,
        "title": row.title,
        "description": payload.description,
        "schedule": schedule,
        "timezone": row.timezone,
        "status": row.status,
        "pause_reason": row.pause_reason,
        "created_via": row.created_via,
        "creator_user_id": row.creator_user_id,
        "creator_name": creator_name,
        "next_fire_at": row.next_fire_at,
        "last_fire_at": row.last_fire_at,
        "last_task_id": row.last_task_id,
        "last_outcome": row.last_outcome,
        "fire_count": row.fire_count,
        "skip_count": row.skip_count,
        "created_at": row.created_at,
    })
}

/// Rows `actor` may see: their own, or everyone's for an Admin asking for
/// `all`.
pub async fn list(
    store: &TaskStore,
    actor: &Creator,
    all: bool,
) -> Result<Vec<AssignScheduleRow>, String> {
    let scope = if all && actor.is_admin {
        None
    } else {
        Some(actor.user_id.as_str())
    };
    store.list_assign_schedules(scope, 200).await
}

#[cfg(test)]
#[path = "manage_tests.rs"]
pub(crate) mod tests;
