//! Dashboard automation access is tied to the stored record and live direct bindings.
use super::*;

fn may_read(ctx: &UserContext, row: &CronTaskRow) -> bool {
    ctx.is_admin() || row.created_by_user_id.as_deref() == Some(ctx.user_id.as_str())
        || ctx.has_agent_access(&row.agent_id, AccessLevel::Viewer)
}

fn may_write(ctx: &UserContext, row: &CronTaskRow) -> bool {
    ctx.is_admin() || row.created_by_user_id.as_deref() == Some(ctx.user_id.as_str())
        || ctx.has_agent_access(&row.agent_id, AccessLevel::Operator)
}

fn prompt_only(row: &CronTaskRow) -> bool {
    row.trigger_kind == "time" && row.condition_script.is_none() && row.watch_command.is_none()
        && row.notify_channel.is_none() && row.notify_chat_id.is_none() && row.notify_thread_id.is_none()
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
        match crate::reminder_scheduler::dashboard_reminders(&self.home_dir, ctx, if cancel { id } else { None }).await {
            Ok(rows) => {
                let principal = format!("user:{}", ctx.user_id);
                let reminders: Vec<Value> = rows.into_iter().map(|r| json!({
                    "id": r.id, "agent_id": r.agent_id, "trigger_at": r.trigger_at,
                    "message": if r.mode == crate::reminder_scheduler::ReminderMode::Direct { r.message.as_deref().unwrap_or("") } else { r.prompt.as_deref().unwrap_or("") },
                    "status": r.status,
                    "can_manage": ctx.is_admin() || r.on_behalf_of.as_deref() == Some(principal.as_str())
                        || ctx.has_agent_access(&r.agent_id, AccessLevel::Operator),
                })).collect();
                WsFrame::ok_response("", json!({ "reminders": reminders }))
            }
            Err(_) => WsFrame::error_response("", "Reminder unavailable or permission denied"),
        }
    }

    pub(crate) async fn handle_scoped_cron(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if ctx.is_admin() {
            return match method {
                "cron.list" => self.handle_cron_list().await,
                "cron.add" => self.handle_cron_add(params).await,
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
        let store = match self.cron_store().await { Ok(s) => s, Err(f) => return f };
        if method == "cron.list" {
            let rows = match store.list_all().await {
                Ok(rows) => rows,
                Err(_) => return WsFrame::error_response("", "Automation data unavailable"),
            };
            let mut tasks = Vec::new();
            for row in rows.into_iter().filter(|r| may_read(ctx, r)) {
                let bound = match store.workflow_binding(&row.id).await {
                    Ok(binding) => binding.is_some(),
                    Err(_) => return WsFrame::error_response("", "Automation data unavailable"),
                };
                let writable = may_write(ctx, &row) && !bound;
                let can_edit = writable && prompt_only(&row)
                    && crate::cron_store::scoped_cron_timezone_allowed(&row)
                    && crate::cron_scheduler::scoped_cron_target_allowed(&*self.registry.read().await, &row.agent_id)
                    && ctx.has_agent_access(&row.agent_id, AccessLevel::Operator);
                let can_activate = can_edit && crate::cron_store::validate_scoped_schedule(&row).is_ok();
                // Script bodies and notification destinations remain operator-only configuration.
                tasks.push(json!({ "id": row.id, "name": row.name, "agent_id": row.agent_id,
                    "cron": row.cron, "schedule": row.cron, "task": row.task, "enabled": row.enabled,
                    "last_run_at": row.last_run_at, "last_status": row.last_status,
                    "can_manage": writable, "can_edit": can_edit, "can_resume": can_activate,
                    "can_run": can_activate }));
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
            if agent == "default" || !is_valid_agent_id(agent)
                || !ctx.has_agent_access(agent, AccessLevel::Operator) {
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
            if let Err(error) = store.insert_scoped(&row).await { return WsFrame::error_response("", &error); }
            self.notify_cron_reload().await;
            self.emit_cron_changed(json!({ "action": "created", "id": row.id })).await;
            return WsFrame::ok_response("", json!({ "success": true, "task": Self::cron_row_to_json(&row) }));
        }
        // Resolve once by id; legacy name-only changes are deliberately not employee APIs.
        let id = params["id"].as_str().unwrap_or("");
        let row = match store.get(id).await {
            Ok(Some(row)) if may_write(ctx, &row) => row,
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let bound = match store.workflow_binding(id).await {
            Ok(b) => b.is_some(), Err(_) => true,
        };
        if bound { return WsFrame::error_response("", PERMISSION_DENIED); }
        let activating = matches!(method, "cron.add" | "cron.update" | "cron.resume" | "cron.run_now");
        if activating {
            if !prompt_only(&row) || !ctx.has_agent_access(&row.agent_id, AccessLevel::Operator)
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
                return match scheduler.run_now_scoped(row, &principal).await {
                    Ok(name) => WsFrame::ok_response("", json!({ "success": true, "name": name })),
                    Err(error) => WsFrame::error_response("", &error),
                };
            }
            _ => return WsFrame::error_response("", PERMISSION_DENIED),
        }
        match store.edit_scoped(&row, if method == "cron.remove" { None } else { Some(&changed) }).await {
            Ok(true) => {
                self.notify_cron_reload().await;
                self.emit_cron_changed(json!({ "action": "updated", "id": id })).await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", "Automation changed; refresh and retry"),
            Err(error) => WsFrame::error_response("", &error),
        }
    }
}
