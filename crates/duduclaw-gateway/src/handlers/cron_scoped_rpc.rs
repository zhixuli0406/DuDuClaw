//! Dashboard automation access uses stored ownership, live bindings and department grants.
use super::*;
use duduclaw_core::company_timezone;
use crate::dept_grants::{DeptGrantView, GrantAction, GrantGroup, department_grant_allows, record_grant_use};

// v1.81 P1g: a routine of another person's Personal employee is never
// readable or writable through a creator right or the Admin role.
fn may_read(ctx: &UserContext, view: &DeptGrantView, row: &CronTaskRow) -> bool {
    !ctx.owner_only_denied(&row.agent_id)
        && (ctx.is_admin() || row.created_by_user_id.as_deref() == Some(ctx.user_id.as_str())
            || ctx.has_agent_access(&row.agent_id, AccessLevel::Viewer)
            || department_grant_allows(view, &row.agent_id, GrantGroup::Schedules, GrantAction::Read))
}

fn may_write(ctx: &UserContext, view: &DeptGrantView, row: &CronTaskRow, action: GrantAction) -> bool {
    !ctx.owner_only_denied(&row.agent_id)
        && (ctx.is_admin() || row.created_by_user_id.as_deref() == Some(ctx.user_id.as_str())
            || ctx.has_agent_access(&row.agent_id, AccessLevel::Operator)
            || department_grant_allows(view, &row.agent_id, GrantGroup::Schedules, action))
}

fn prompt_only(row: &CronTaskRow) -> bool {
    row.trigger_kind == "time" && row.condition_script.is_none() && row.watch_command.is_none()
        && row.notify_channel.is_none() && row.notify_chat_id.is_none() && row.notify_thread_id.is_none()
}

fn member_cron_row(row: &CronTaskRow, writable: bool, can_edit: bool, can_resume: bool, can_run: bool) -> Value {
    json!({ "id": row.id, "name": row.name, "agent_id": row.agent_id,
        "cron": row.cron, "schedule": row.cron, "cron_timezone": row.cron_timezone, "task": row.task, "enabled": row.enabled,
        "last_run_at": row.last_run_at, "last_status": row.last_status,
        "skip_reason": row.skip_reason(),
        "last_delivery_status": row.last_delivery_status,
        "last_delivery_reason": row.last_delivery_reason,
        "can_manage": writable, "can_edit": can_edit, "can_resume": can_resume,
        "can_run": can_run })
}

impl MethodHandler {
    fn cron_task_input_blocked(&self, agent: &str, name: &str, task: &str) -> bool {
        // Name is also interpolated into the model prompt, so scan the exact
        // combined instruction instead of allowing a second unscanned input.
        let prompt = format!("[Scheduled Task: {name}] {task}");
        duduclaw_security::input_guard::scan_input_with_audit(
            &prompt, duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD, &self.home_dir, agent,
        ).blocked
    }

    pub(crate) async fn handle_dashboard_reminders(&self, params: Value, ctx: &UserContext, cancel: bool) -> WsFrame {
        let id = params.get("id").and_then(Value::as_str);
        if cancel && id.is_none() { return WsFrame::error_response("", "id is required"); }
        let view = if ctx.is_admin() { DeptGrantView::empty() } else { DeptGrantView::load(self, ctx).await };
        match crate::reminder_scheduler::dashboard_reminders(&self.home_dir, ctx, view, if cancel { id } else { None }).await {
            Ok((rows, grant_cancelled_agent)) => {
                if let Some(agent) = grant_cancelled_agent {
                    record_grant_use(self, ctx, &agent, GrantGroup::Schedules, GrantAction::Remove, "reminders.cancel").await;
                }
                let reminders: Vec<Value> = rows.into_iter().map(|(r, can_manage)| json!({
                    "id": r.id, "agent_id": r.agent_id, "trigger_at": r.trigger_at,
                    "message": if r.mode == crate::reminder_scheduler::ReminderMode::Direct { r.message.as_deref().unwrap_or("") } else { r.prompt.as_deref().unwrap_or("") },
                    "status": r.status,
                    "can_manage": can_manage,
                })).collect();
                WsFrame::ok_response("", json!({ "reminders": reminders }))
            }
            Err(_) => WsFrame::error_response("", "Reminder unavailable or permission denied"),
        }
    }

    pub(crate) async fn handle_scoped_cron(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        self.handle_scoped_cron_inner(method, params, ctx, false).await
    }

    /// Internal Admin-AI path. The creator cannot be supplied by RPC callers.
    pub(crate) async fn handle_admin_ai_cron(
        &self, method: &str, params: Value, ctx: &UserContext, expected_hash: Option<&str>,
    ) -> WsFrame {
        use crate::admin_ai::proposal::STALE;
        if !ctx.is_admin() { return WsFrame::error_response("", PERMISSION_DENIED); }
        if method == "cron.add" {
            return self.handle_scoped_cron_inner(method, params, ctx, true).await;
        }
        let Some(expected_hash) = expected_hash else { return WsFrame::error_response("", STALE); };
        if let Err(frame) = self.p1g_admin_cron_guard(method, &params, ctx).await { return frame; }
        let store = match self.cron_store().await { Ok(s) => s, Err(f) => return f };
        let id = params["id"].as_str().unwrap_or("");
        let row = match store.get(id).await {
            Ok(Some(row)) if prompt_only(&row) => row,
            _ => return WsFrame::error_response("", STALE),
        };
        let mut changed = row.clone();
        match method {
            "cron.update" => {
                if !params.as_object().is_some_and(|p| p.keys().all(|k|
                    matches!(k.as_str(), "id" | "name" | "cron" | "cron_timezone" | "task"))) {
                    return WsFrame::error_response("", PERMISSION_DENIED);
                }
                for key in ["name", "cron", "task", "cron_timezone"] {
                    if let Some(value) = params.get(key) {
                        let Some(value) = value.as_str() else { return WsFrame::error_response("", "Invalid automation field"); };
                        match key {
                            "name" => changed.name = value.into(),
                            "cron" => changed.cron = value.into(),
                            "task" => changed.task = value.into(),
                            _ => changed.cron_timezone = Some(value.into()),
                        }
                    }
                }
                if changed.name != row.name {
                    if let Err(error) = crate::scheduled_webchat::check_name(&changed.name) {
                        return WsFrame::error_response("", &error);
                    }
                }
                if changed.name.trim().is_empty() || changed.task.len() > 16_384
                    || changed.cron.len() > crate::cron_store::MAX_CRON_BYTES
                    || crate::cron_scheduler::normalise_cron(&changed.cron).parse::<cron::Schedule>().is_err()
                    || changed.cron_timezone.as_deref().is_some_and(|tz| duduclaw_core::parse_timezone(tz).is_none()) {
                    return WsFrame::error_response("", "Invalid automation name, schedule or message");
                }
                if self.cron_task_input_blocked(&changed.agent_id, &changed.name, &changed.task) {
                    return WsFrame::error_response("", "Automation message rejected by input protection");
                }
            }
            "cron.pause" => changed.enabled = false,
            "cron.resume" => changed.enabled = true,
            "cron.remove" => (),
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        }
        match store.edit_admin_ai(&row, (method != "cron.remove").then_some(&changed), expected_hash).await {
            Ok(true) => {
                if method == "cron.remove" {
                    crate::recipient_guard::origin::forget_async(&self.home_dir,
                        row.notify_origin_proof.into_iter().collect()).await;
                }
                self.notify_cron_reload().await;
                self.emit_cron_changed(json!({"action":"updated", "id":id})).await;
                WsFrame::ok_response("", json!({"success":true}))
            }
            Ok(false) => WsFrame::error_response("", STALE),
            Err(error) => WsFrame::error_response("", &error),
        }
    }

    async fn handle_scoped_cron_inner(&self, method: &str, params: Value, ctx: &UserContext, record_creator: bool) -> WsFrame {
        if ctx.is_admin() {
            // v1.81 P1g: a cron routine makes its employee run work, so on
            // another person's Personal employee it is owner-only — the
            // Admin path resolves the row's employee (by id or name) and the
            // named target before any handler. `cron.list` drops those rows
            // in the dispatch response filter (`personal_p1g_gate`).
            if let Err(frame) = self.p1g_admin_cron_guard(method, &params, ctx).await {
                return frame;
            }
            return match method {
                "cron.list" => self.handle_cron_list().await,
                "cron.add" => self.handle_cron_add_with_creator(params, record_creator.then_some(ctx)).await,
                "cron.update" => self.handle_cron_update(params).await,
                "cron.pause" => self.handle_cron_set_enabled(params, false).await,
                "cron.resume" => self.handle_cron_set_enabled(params, true).await,
                "cron.remove" => self.handle_cron_remove(params).await,
                "cron.run_now" => self.handle_cron_run_now(params).await,
                "cron.templates" => self.handle_cron_templates().await,
                _ => WsFrame::error_response("", PERMISSION_DENIED),
            };
        }
        if method == "cron.templates" { return self.handle_cron_templates().await; }
        let view = DeptGrantView::load(self, ctx).await;
        let store = match self.cron_store().await { Ok(s) => s, Err(f) => return f };
        if method == "cron.list" {
            let rows = match store.list_all().await {
                Ok(rows) => rows,
                Err(_) => return WsFrame::error_response("", "Automation data unavailable"),
            };
            let mut tasks = Vec::new();
            let mut display = crate::dept_grants::DisplayCreatorAccessCache::load(self).await;
            for row in rows.into_iter().filter(|r| may_read(ctx, &view, r)) {
                let bound = match store.workflow_binding(&row.id).await {
                    Ok(binding) => binding.is_some(),
                    Err(_) => return WsFrame::error_response("", "Automation data unavailable"),
                };
                // can_manage covers narrowing controls (pause/remove). Editing
                // needs Edit; activation keeps its structural checks and uses
                // Toggle for resume, RunNow for an immediate run.
                let writable = !bound
                    && may_write(ctx, &view, &row, GrantAction::Toggle)
                    && may_write(ctx, &view, &row, GrantAction::Remove);
                let operator = ctx.has_agent_access(&row.agent_id, AccessLevel::Operator);
                let structural = !bound && prompt_only(&row)
                    && crate::cron_store::scoped_cron_timezone_allowed(&row)
                    && crate::cron_scheduler::scoped_cron_target_allowed(&*self.registry.read().await, &row.agent_id);
                let can_edit = structural && (operator
                    || department_grant_allows(&view, &row.agent_id, GrantGroup::Schedules, GrantAction::Edit));
                let can_activate = structural && crate::cron_store::validate_scoped_schedule(&row).is_ok();
                let can_resume = can_activate && (operator
                    || department_grant_allows(&view, &row.agent_id, GrantGroup::Schedules, GrantAction::Toggle));
                let can_run = can_activate && (operator
                    || department_grant_allows(&view, &row.agent_id, GrantGroup::Schedules, GrantAction::RunNow));
                // Script bodies and notification destinations remain operator-only configuration.
                let mut value = member_cron_row(&row, writable, can_edit, can_resume, can_run);
                value["creator_access_lost"] = json!(display.display_creator_access_lost(
                    row.created_by_user_id.as_deref(), &row.agent_id));
                tasks.push(value);
            }
            return WsFrame::ok_response("", json!({ "tasks": tasks }));
        }
        let principal = format!("user:{}", ctx.user_id);
        if method == "cron.add" {
            if !params.as_object().is_some_and(|p| p.keys().all(|k|
                matches!(k.as_str(), "name" | "agent_id" | "cron" | "task"))) {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
            let agent = params["agent_id"].as_str().unwrap_or("");
            let operator = ctx.has_agent_access(agent, AccessLevel::Operator);
            if agent == "default" || !is_valid_agent_id(agent)
                || !(operator || department_grant_allows(&view, agent, GrantGroup::Schedules, GrantAction::Create)) {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
            if !crate::cron_scheduler::scoped_cron_target_allowed(&*self.registry.read().await, agent) {
                return WsFrame::error_response("", "Employee automations require an available agent with task sandbox disabled");
            }
            if let Err(f) = self.deny_main_for_non_admin(ctx, agent).await { return f; }
            if crate::machine_employee::check_target(&self.home_dir, agent, Some(&principal)).is_err() {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
            let name = params["name"].as_str().unwrap_or("").trim();
            let cron = params["cron"].as_str().unwrap_or("");
            let task = params["task"].as_str().unwrap_or("");
            if name.is_empty() || name.len() > 200 || task.len() > 16_384
                || cron.len() > crate::cron_store::MAX_CRON_BYTES
                || crate::cron_scheduler::normalise_cron(cron).parse::<cron::Schedule>().is_err() {
                return WsFrame::error_response("", "Invalid automation name, schedule or message");
            }
            if self.cron_task_input_blocked(agent, name, task) { return WsFrame::error_response("", "Automation message rejected by input protection"); }
            let mut row = CronTaskRow::new(uuid::Uuid::new_v4().to_string(), name.into(), agent.into(), cron.into(), task.into());
            row.on_behalf_of = Some(principal);
            row.created_by_user_id = Some(ctx.user_id.clone());
            row.scoped_by_user_id = Some(ctx.user_id.clone());
            let choice = company_timezone::default_for_new_schedule(
                &company_timezone::read(&self.home_dir), true, Utc::now(),
            );
            let zone_note = company_timezone::zone_note_zh(&choice.source);
            row.cron_timezone = Some(choice.zone);
            if let Err(error) = store.insert_scoped(&row).await { return WsFrame::error_response("", &error); }
            if !operator {
                record_grant_use(self, ctx, agent, GrantGroup::Schedules, GrantAction::Create, method).await;
            }
            self.notify_cron_reload().await;
            self.emit_cron_changed(json!({ "action": "created", "id": row.id })).await;
            return WsFrame::ok_response("", json!({ "success": true, "task": Self::cron_row_to_json(&row), "zone_note": zone_note }));
        }
        // Resolve once by id; legacy name-only changes are deliberately not employee APIs.
        let action = match method {
            "cron.update" => GrantAction::Edit,
            "cron.pause" | "cron.resume" => GrantAction::Toggle,
            "cron.remove" => GrantAction::Remove,
            "cron.run_now" => GrantAction::RunNow,
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let id = params["id"].as_str().unwrap_or("");
        let row = match store.get(id).await {
            Ok(Some(row)) if may_write(ctx, &view, &row, action) => row,
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let bound = match store.workflow_binding(id).await {
            Ok(b) => b.is_some(), Err(_) => true,
        };
        if bound { return WsFrame::error_response("", PERMISSION_DENIED); }
        let activating = matches!(method, "cron.add" | "cron.update" | "cron.resume" | "cron.run_now");
        let operator = ctx.has_agent_access(&row.agent_id, AccessLevel::Operator);
        // Creator rights suffice only for narrowing operations. Activating
        // another time still requires an Operator binding or the action grant.
        let grant_backed = !operator
            && (activating || row.created_by_user_id.as_deref() != Some(ctx.user_id.as_str()));
        if activating {
            if !prompt_only(&row) || !(operator
                || department_grant_allows(&view, &row.agent_id, GrantGroup::Schedules, action))
                || !crate::cron_scheduler::scoped_cron_target_allowed(&*self.registry.read().await, &row.agent_id) {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
            if let Err(f) = self.deny_main_for_non_admin(ctx, &row.agent_id).await { return f; }
            if crate::machine_employee::check_target(&self.home_dir, &row.agent_id, Some(&principal)).is_err() {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
        }
        if activating && method != "cron.update" {
            if let Err(error) = crate::cron_store::validate_scoped_schedule(&row) { return WsFrame::error_response("", &error); }
            if self.cron_task_input_blocked(&row.agent_id, &row.name, &row.task) { return WsFrame::error_response("", "Automation message rejected by input protection"); }
        }
        let mut changed = row.clone();
        if activating { changed.scoped_by_user_id = Some(ctx.user_id.clone()); }
        match method {
            "cron.update" => {
                if !params.as_object().is_some_and(|p| p.keys().all(|k|
                    matches!(k.as_str(), "id" | "name" | "agent_id" | "cron" | "task" | "enabled"))) {
                    return WsFrame::error_response("", PERMISSION_DENIED);
                }
                if params.get("agent_id").is_some_and(|v| v.as_str() != Some(row.agent_id.as_str())) {
                    return WsFrame::error_response("", PERMISSION_DENIED);
                }
                for key in ["name", "cron", "task"] {
                    if let Some(value) = params.get(key) {
                        let Some(value) = value.as_str() else { return WsFrame::error_response("", "Invalid automation field"); };
                        match key { "name" => changed.name = value.into(), "cron" => changed.cron = value.into(), _ => changed.task = value.into() }
                    }
                }
                if let Some(value) = params.get("enabled") {
                    let Some(value) = value.as_bool() else { return WsFrame::error_response("", "Invalid automation state"); };
                    changed.enabled = value;
                }
                if changed.name.trim().is_empty() || changed.name.len() > 200 || changed.task.len() > 16_384
                    || changed.cron.len() > crate::cron_store::MAX_CRON_BYTES
                    || crate::cron_scheduler::normalise_cron(&changed.cron).parse::<cron::Schedule>().is_err() {
                    return WsFrame::error_response("", "Invalid automation name, schedule or message");
                }
                if let Err(error) = crate::cron_store::validate_scoped_schedule(&changed) { return WsFrame::error_response("", &error); }
                if self.cron_task_input_blocked(&changed.agent_id, &changed.name, &changed.task) { return WsFrame::error_response("", "Automation message rejected by input protection"); }
                changed.on_behalf_of = Some(principal.clone());
            }
            "cron.pause" => changed.enabled = false,
            "cron.resume" => { changed.enabled = true; changed.on_behalf_of = Some(principal.clone()); }
            "cron.remove" => (),
            "cron.run_now" => {
                let scheduler = match self.cron_scheduler.read().await.as_ref() {
                    Some(s) => s.clone(), None => return WsFrame::error_response("", "Automation scheduler unavailable"),
                };
                let agent = row.agent_id.clone();
                return match scheduler.run_now_scoped(row, &principal).await {
                    Ok(name) => {
                        if grant_backed {
                            record_grant_use(self, ctx, &agent, GrantGroup::Schedules, action, method).await;
                        }
                        WsFrame::ok_response("", json!({ "success": true, "name": name }))
                    }
                    Err(error) => WsFrame::error_response("", &error),
                };
            }
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        }
        match store.edit_scoped(&row, if method == "cron.remove" { None } else { Some(&changed) }).await {
            Ok(true) => {
                if grant_backed {
                    record_grant_use(self, ctx, &row.agent_id, GrantGroup::Schedules, action, method).await;
                }
                self.notify_cron_reload().await;
                self.emit_cron_changed(json!({ "action": "updated", "id": id })).await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", "Automation changed; refresh and retry"),
            Err(error) => WsFrame::error_response("", &error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn assert_member_default(company: &str, expected: &str, source: company_timezone::ZoneSource) {
        use chrono::TimeZone;
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            format!("[general]\ncompany_timezone = '{company}'\n")).unwrap();
        let store = CronStore::open(home.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 27, 0, 0, 0).unwrap();
        let choice = company_timezone::default_for_new_schedule(&company_timezone::read(home.path()), true, now);
        assert_eq!(choice.source, source);
        assert_eq!(choice.zone, expected);
        let mut row = CronTaskRow::new("member-zone".into(), "Morning".into(), "worker".into(),
            "0 9 * * *".into(), "report".into());
        row.on_behalf_of = Some("user:member".into());
        row.created_by_user_id = Some("member".into());
        row.scoped_by_user_id = Some("member".into());
        row.cron_timezone = Some(choice.zone);
        store.insert_scoped(&row).await.unwrap();
        let stored = store.get(&row.id).await.unwrap().unwrap();
        assert_eq!(stored.cron_timezone.as_deref(), Some(expected));
        assert_eq!(member_cron_row(&stored, true, true, true, true)["cron_timezone"], expected);
        match source {
            company_timezone::ZoneSource::Company => assert_eq!(company_timezone::zone_note_zh(&source), None),
            company_timezone::ZoneSource::UtcMemberDst => assert_eq!(company_timezone::zone_note_zh(&source),
                Some("公司時區有夏令時間，成員自建的例行工作以 UTC 計時；要依當地時間請管理員建立。")),
            _ => panic!("unexpected fixture source"),
        }
    }

    #[tokio::test]
    async fn member_default_with_dst_company_zone_is_utc_with_note() {
        assert_member_default("America/New_York", "UTC", company_timezone::ZoneSource::UtcMemberDst).await;
    }

    #[tokio::test]
    async fn member_default_with_taipei_is_taipei() {
        assert_member_default("Asia/Taipei", "Asia/Taipei", company_timezone::ZoneSource::Company).await;
    }

    #[test]
    fn member_row_only_exposes_the_fixed_skip_reason() {
        let mut row = CronTaskRow::new("id".into(), "Morning".into(), "agent".into(),
            "0 9 * * *".into(), "report".into());
        row.last_status = Some("skipped".into());
        row.last_error = Some(crate::cron_scheduler::SCOPED_SCHEDULE_SKIPPED.into());
        let member = member_cron_row(&row, false, false, false, false);
        assert_eq!(member["skip_reason"], "timezone_or_interval");
        assert!(member.get("last_error").is_none());

        row.last_error = Some("internal detail".into());
        let member = member_cron_row(&row, false, false, false, false);
        assert!(member["skip_reason"].is_null());
        assert!(member.get("last_error").is_none());
    }

    #[tokio::test]
    async fn member_row_skip_reason_clears_on_allowed_timezone_edit() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::cron_store::CronStore::open(dir.path()).unwrap();
        let mut row = CronTaskRow::new("id".into(), "Morning".into(), "agent".into(),
            "0 9 * * *".into(), "report".into());
        row.on_behalf_of = Some("user:member".into());
        store.insert(&row).await.unwrap();
        store.record_scoped_schedule_skip(&row.id, crate::cron_scheduler::SCOPED_SCHEDULE_SKIPPED).await.unwrap();
        let skipped = store.get(&row.id).await.unwrap().unwrap();
        assert_eq!(member_cron_row(&skipped, false, false, false, false)["skip_reason"], "timezone_or_interval");
        store.update_cron_timezone(&row.id, Some("Asia/Taipei")).await.unwrap();
        let cleared = store.get(&row.id).await.unwrap().unwrap();
        let member = member_cron_row(&cleared, false, false, false, false);
        assert!(member["last_status"].is_null());
        assert!(member["skip_reason"].is_null());
        assert!(cleared.last_error.is_none());
        assert_eq!(cleared.last_run_at, skipped.last_run_at);
        assert_eq!((cleared.run_count, cleared.failure_count), (skipped.run_count, skipped.failure_count));

        store.record_scoped_schedule_skip(&row.id, "another skip reason").await.unwrap();
        store.update_cron_timezone(&row.id, Some("UTC")).await.unwrap();
        let other = store.get(&row.id).await.unwrap().unwrap();
        assert_eq!(other.last_status.as_deref(), Some("skipped"));
        assert_eq!(other.last_error.as_deref(), Some("another skip reason"));
    }
}
