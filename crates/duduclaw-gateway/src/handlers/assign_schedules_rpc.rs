//! Scheduled assignments over the dashboard RPC (`crate::assign_schedule`):
//! `tasks.create` / `tasks.goal_create` with a `schedule` field create one;
//! `assign_schedules.list` / `.pause` / `.resume` / `.remove` show and
//! control the caller's own (an Admin: everyone's). `ctx` is the live
//! identity (`live_context.rs`); the caller of the create paths already
//! checked `can_assign_to` on it.

#[allow(unused_imports)]
use super::*;

use crate::assign_schedule::{
    AssignKind, AssignPayload, CreateError, Creator, ManageAction, NewSchedule, ScheduleSpec, Zone,
    manage, natural, spec,
};

/// Error text for a refused schedule: a stable code the dashboard maps to
/// its own words (`assignSchedule.error.<code>`), plus detail.
fn create_error(e: &CreateError) -> String {
    match e {
        CreateError::TooMany { max } => {
            format!("{}: at most {max} active or paused schedules", e.code())
        }
        CreateError::Spec(crate::assign_schedule::SpecError::TooFrequent { min_minutes }) => {
            format!(
                "{}: the shortest interval is {min_minutes} minutes",
                e.code()
            )
        }
        CreateError::Store(detail) => format!("{}: {detail}", e.code()),
        other => other.code().to_string(),
    }
}

/// The schedule in a create request: the dashboard's structured object, or
/// a phrase (`"每週一 9:00"`, `"daily 09:00"`) read like a chat `時間:`.
fn schedule_from(value: &Value, zone: Zone) -> Result<ScheduleSpec, String> {
    let parsed = match value {
        Value::Object(_) => spec::from_json(value, zone),
        Value::String(s) => natural::parse_natural(s, Utc::now(), zone),
        _ => Err(crate::assign_schedule::SpecError::Invalid),
    };
    parsed.map_err(|e| e.code().to_string())
}

/// The goal fields, checked the way `goal_create_core` will check them at
/// every fire, so a bad value is refused now and not at 09:00 next Monday.
fn goal_payload(params: &Value) -> Result<AssignPayload, String> {
    let s = |k: &str| params.get(k).and_then(Value::as_str).map(str::to_string);
    let description = s("description").unwrap_or_default();
    if description.trim().is_empty() {
        return Err("description is required".into());
    }
    let priority = s("priority").unwrap_or_default();
    if !matches!(priority.trim(), "" | "low" | "medium" | "high" | "urgent") {
        return Err(format!("invalid priority: {priority}"));
    }
    let duration_hours = match params.get("duration_hours") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_f64() {
            Some(h) if (1.0..=720.0).contains(&h) => Some(h),
            _ => return Err("duration_hours must be between 1 and 720".into()),
        },
    };
    let outcome = s("outcome").filter(|o| !o.trim().is_empty());
    if let Some(o) = outcome.as_deref() {
        crate::outcome_spec::OutcomeSpec::parse(o)
            .map_err(|e| format!("outcome 產出驗收格式錯誤：{e}"))?;
    }
    Ok(AssignPayload {
        description,
        acceptance_criteria: s("acceptance_criteria"),
        priority,
        outcome,
        duration_hours,
        cost_cap_cents: crate::goal_create_core::parse_cost_cap_cents(params)?,
        timeout_secs: crate::goal_create_core::parse_timeout_secs(params)?,
        risk_boundary: s("risk_boundary").filter(|b| !b.trim().is_empty()),
        require_beliefs: params
            .get("require_beliefs")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        plan_first: params
            .get("plan_first")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    })
}

fn task_payload(params: &Value) -> Result<AssignPayload, String> {
    let s = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let title = s("title");
    if title.trim().is_empty() {
        return Err("title is required".into());
    }
    let priority = s("priority");
    if !matches!(priority.trim(), "" | "low" | "medium" | "high" | "urgent") {
        return Err(format!("invalid priority: {priority}"));
    }
    let tags = params
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    Ok(AssignPayload {
        title,
        description: s("description"),
        priority,
        tags,
        ..Default::default()
    })
}

impl MethodHandler {
    /// `tasks.create` / `tasks.goal_create` carrying `schedule`.
    pub(crate) async fn handle_assign_schedule_create(
        &self,
        kind: AssignKind,
        agent_id: &str,
        params: &Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(creator) = Creator::new(&ctx.user_id, ctx.is_admin()) else {
            return WsFrame::error_response(
                "",
                "account_required: a scheduled assignment needs a signed-in account",
            );
        };
        if !is_valid_agent_id(agent_id) || !ctx.can_assign_to(agent_id) {
            return WsFrame::error_response("", PERMISSION_DENIED);
        }
        if !ctx.is_admin() {
            if let Err(refusal) = crate::machine_employee::check_target(
                &self.home_dir, agent_id, Some(&format!("user:{}", ctx.user_id)),
            ) {
                return WsFrame::error_response("", &refusal);
            }
        }
        let timezone = params
            .get("schedule_timezone")
            .and_then(Value::as_str)
            .or_else(|| {
                params
                    .get("schedule")
                    .and_then(|s| s.get("timezone"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        let zone = match Zone::parse(timezone.as_deref()) {
            Ok(z) => z,
            Err(e) => return WsFrame::error_response("", e.code()),
        };
        let schedule = match schedule_from(params.get("schedule").unwrap_or(&Value::Null), zone) {
            Ok(s) => s,
            Err(code) => return WsFrame::error_response("", &code),
        };
        let payload = match kind {
            AssignKind::Goal => goal_payload(params),
            AssignKind::Task => task_payload(params),
        };
        let payload = match payload {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let req = NewSchedule {
            agent_id: agent_id.to_string(),
            kind,
            payload,
            spec: schedule,
            timezone,
            created_via: "dashboard".to_string(),
            source: None,
        };
        match manage::create(&self.home_dir, &store, &creator, req, Utc::now()).await {
            Ok(row) => {
                let view = manage::row_json(&row, None);
                self.broadcast_event("assign_schedule.changed", json!({}))
                    .await;
                WsFrame::ok_response("", json!({ "schedule": view }))
            }
            Err(e) => WsFrame::error_response("", &create_error(&e)),
        }
    }

    /// `assign_schedules.list` — `{scope: "mine" | "all"}` (`all` for
    /// Admins; anyone else gets their own).
    pub(crate) async fn handle_assign_schedules_list(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(actor) = Creator::new(&ctx.user_id, ctx.is_admin()) else {
            return WsFrame::ok_response(
                "",
                json!({ "schedules": [], "max_active": 0, "min_interval_minutes": 0 }),
            );
        };
        let all = params.get("scope").and_then(Value::as_str) == Some("all");
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rows = match manage::list(&store, &actor, all).await {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("list schedules: {e}")),
        };
        let home = self.home_dir.clone();
        let schedules: Vec<Value> = rows
            .iter()
            .map(|r| {
                let name = (r.creator_user_id != actor.user_id)
                    .then(|| {
                        crate::trigger_route::assign_create::account_name(&home, &r.creator_user_id)
                    })
                    .flatten();
                manage::row_json(r, name.as_deref())
            })
            .collect();
        // Listing and narrowing controls remain available when config is
        // broken. Unknown limits are reported as null, never as defaults.
        let limits = crate::assign_schedule::caps(&self.home_dir).ok();
        WsFrame::ok_response(
            "",
            json!({
                "schedules": schedules,
                "max_active": limits.map(|(max, _)| max),
                "min_interval_minutes": limits.map(|(_, min)| if actor.is_admin { spec::ADMIN_MIN_INTERVAL_MINUTES } else { min }),
            }),
        )
    }

    /// `assign_schedules.pause` / `.resume` / `.remove` — `{id}`.
    pub(crate) async fn handle_assign_schedules_manage(
        &self,
        params: Value,
        ctx: &UserContext,
        action: ManageAction,
    ) -> WsFrame {
        let Some(actor) = Creator::new(&ctx.user_id, ctx.is_admin()) else {
            return WsFrame::error_response("", "not_found");
        };
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if id.is_empty() || id.len() > 64 {
            return WsFrame::error_response("", "id is required");
        }
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match manage::manage(&self.home_dir, &store, &actor, id, action, Utc::now()).await {
            Ok(row) => {
                self.broadcast_event("assign_schedule.changed", json!({}))
                    .await;
                WsFrame::ok_response("", json!({ "schedule": manage::row_json(&row, None) }))
            }
            Err(e) => WsFrame::error_response("", e.code()),
        }
    }
}
