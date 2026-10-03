//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Cron ────────────────────────────────────────────────

    /// Return a reference to the injected cron store, or an error frame if
    /// the gateway has not finished initializing the store yet.
    pub(crate) async fn cron_store(&self) -> Result<Arc<CronStore>, WsFrame> {
        match self.cron_store.read().await.as_ref() {
            Some(store) => Ok(store.clone()),
            None => Err(WsFrame::error_response(
                "",
                "Cron store not initialized yet — retry in a moment",
            )),
        }
    }

    /// Serialize a `CronTaskRow` into the JSON shape the dashboard expects.
    pub(crate) fn cron_row_to_json(row: &CronTaskRow) -> Value {
        json!({
            "id": row.id,
            "name": row.name,
            "agent_id": row.agent_id,
            "cron": row.cron,
            // Alias kept for legacy dashboard clients that read `schedule`.
            "schedule": row.cron,
            "task": row.task,
            "enabled": row.enabled,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
            "last_run_at": row.last_run_at,
            "last_status": row.last_status,
            "last_error": row.last_error,
            "run_count": row.run_count,
            "failure_count": row.failure_count,
            "notify_channel": row.notify_channel,
            "notify_chat_id": row.notify_chat_id,
            "notify_thread_id": row.notify_thread_id,
            "cron_timezone": row.cron_timezone,
            // G3 event-trigger fields.
            "trigger_kind": row.trigger_kind,
            "condition_script": row.condition_script,
            "condition_state": row.condition_state,
            "watch_command": row.watch_command,
        })
    }

    pub(crate) async fn handle_cron_list(&self) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_all().await {
            Ok(rows) => {
                let tasks: Vec<Value> = rows.iter().map(Self::cron_row_to_json).collect();
                WsFrame::ok_response("", json!({ "tasks": tasks }))
            }
            Err(e) => WsFrame::error_response("", &format!("list cron tasks: {e}")),
        }
    }

    /// WP6 — announce a cron mutation made through the dashboard.
    ///
    /// Deliberately routed through [`crate::dashboard_feedback::emit`] (the
    /// `events.db` path the MCP `tasks_create`-with-`schedule` / `*_cron_task` tools use)
    /// rather than [`Self::broadcast_event`]: one emitter, one whitelist, and
    /// one place to change if the transport ever moves. The acting browser tab
    /// already refetches locally — this is what makes a *second* tab, or the
    /// operator's phone, agree with it. Called only on success paths; a failed
    /// mutation announces nothing, so no client refetches to discover that
    /// nothing changed.
    pub(crate) async fn emit_cron_changed(&self, payload: Value) {
        crate::dashboard_feedback::emit(
            &self.home_dir,
            crate::dashboard_feedback::EV_CRON_CHANGED,
            payload,
        )
        .await;
    }

    pub(crate) async fn handle_cron_add(&self, params: Value) -> WsFrame {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return WsFrame::error_response("", "Missing 'name' parameter"),
        };
        // Accept both `cron` (new) and `schedule` (legacy) from the dashboard.
        let cron_expr = params
            .get("cron")
            .or_else(|| params.get("schedule"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cron_expr.is_empty() {
            return WsFrame::error_response("", "Missing 'cron' parameter");
        }
        // Validate (accept 5- or 6-field). `normalise_cron` turns 5 fields into 6.
        let normalised = crate::cron_scheduler::normalise_cron(&cron_expr);
        if normalised.parse::<cron::Schedule>().is_err() {
            return WsFrame::error_response("", &format!("Invalid cron expression: {cron_expr}"));
        }
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        // `task` is the actual prompt body; `action` is kept as a legacy alias.
        let task_body = params
            .get("task")
            .or_else(|| params.get("prompt"))
            .or_else(|| params.get("action"))
            .and_then(|v| v.as_str())
            .unwrap_or("heartbeat")
            .to_string();

        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Enforce unique name for friendly dashboard UX.
        match store.get_by_name(&name).await {
            Ok(Some(_)) => {
                return WsFrame::error_response("", &format!("Cron task '{name}' already exists"));
            }
            Ok(None) => {}
            Err(e) => return WsFrame::error_response("", &format!("lookup: {e}")),
        }

        let notify_channel = params
            .get("notify_channel")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let notify_chat_id = params
            .get("notify_chat_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let notify_thread_id = params
            .get("notify_thread_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        if notify_channel.is_some() != notify_chat_id.is_some() {
            return WsFrame::error_response(
                "",
                "notify_channel and notify_chat_id must be set together",
            );
        }

        // Optional cron_timezone — validated against the IANA database so
        // typos surface at the dashboard instead of at firing time.
        let cron_timezone = params
            .get("cron_timezone")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        if let Some(ref tz_name) = cron_timezone {
            if duduclaw_core::parse_timezone(tz_name).is_none() {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "Unknown cron_timezone '{tz_name}'. Use an IANA name like 'Asia/Taipei'."
                    ),
                );
            }
        }

        // G3 event-trigger fields. `trigger_kind` defaults to "time" (legacy).
        // Unknown kinds are rejected (strict) so a typo surfaces here rather
        // than silently degrading to a schedule-only task.
        let trigger_kind_str = params
            .get("trigger_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("time")
            .to_string();
        let trigger_kind = match crate::condition_eval::TriggerKind::parse_strict(&trigger_kind_str)
        {
            Some(k) => k,
            None => {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "Unknown trigger_kind '{trigger_kind_str}'. Use 'time', 'condition', or 'on_exit'."
                    ),
                );
            }
        };
        let condition_script = params
            .get("condition_script")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from);
        let watch_command = params
            .get("watch_command")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from);
        // A condition/on_exit task without its script/command would fail closed
        // forever — reject at creation time.
        if matches!(trigger_kind, crate::condition_eval::TriggerKind::Condition)
            && condition_script.is_none()
        {
            return WsFrame::error_response(
                "",
                "trigger_kind 'condition' requires a non-empty 'condition_script'",
            );
        }
        if matches!(trigger_kind, crate::condition_eval::TriggerKind::OnExit)
            && watch_command.is_none()
        {
            return WsFrame::error_response(
                "",
                "trigger_kind 'on_exit' requires a non-empty 'watch_command'",
            );
        }

        let mut row = CronTaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            name.clone(),
            agent_id.clone(),
            cron_expr.clone(),
            task_body,
        );
        row.notify_channel = notify_channel;
        row.notify_chat_id = notify_chat_id;
        row.notify_thread_id = notify_thread_id;
        row.cron_timezone = cron_timezone;
        row.trigger_kind = trigger_kind.as_db().to_string();
        row.condition_script = condition_script;
        row.watch_command = watch_command;
        if let Err(e) = store.insert(&row).await {
            return WsFrame::error_response("", &format!("insert: {e}"));
        }
        self.notify_cron_reload().await;
        self.emit_cron_changed(json!({
            "action": "created",
            "id": row.id,
            "name": name,
            "cron": cron_expr,
            "agent_id": agent_id,
        }))
        .await;
        info!(name = %name, cron = %cron_expr, agent_id = %agent_id, "Cron task added");
        WsFrame::ok_response(
            "",
            json!({ "success": true, "task": Self::cron_row_to_json(&row) }),
        )
    }

    pub(crate) async fn handle_cron_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };

        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        let existing = match store.get(&id).await {
            Ok(Some(row)) => row,
            Ok(None) => return WsFrame::error_response("", &format!("Cron task '{id}' not found")),
            Err(e) => return WsFrame::error_response("", &format!("lookup: {e}")),
        };

        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.name);
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.agent_id);
        let cron_expr = params
            .get("cron")
            .or_else(|| params.get("schedule"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.cron);
        let task_body = params
            .get("task")
            .or_else(|| params.get("prompt"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.task);
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(existing.enabled);

        // Validate cron expression before persisting.
        let normalised = crate::cron_scheduler::normalise_cron(&cron_expr);
        if normalised.parse::<cron::Schedule>().is_err() {
            return WsFrame::error_response("", &format!("Invalid cron expression: {cron_expr}"));
        }

        match store
            .update_fields(&id, &name, &agent_id, &cron_expr, &task_body, enabled)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return WsFrame::error_response("", &format!("Cron task '{id}' not found"));
            }
            Err(e) => return WsFrame::error_response("", &format!("update: {e}")),
        }

        // Optional: only touch notify_* when any of those keys are present
        // in the payload. Absence means "leave existing values alone".
        let has_notify_update = params.get("notify_channel").is_some()
            || params.get("notify_chat_id").is_some()
            || params.get("notify_thread_id").is_some();
        if has_notify_update {
            let notify_channel = params
                .get("notify_channel")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let notify_chat_id = params
                .get("notify_chat_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let notify_thread_id = params
                .get("notify_thread_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if notify_channel.is_some() != notify_chat_id.is_some() {
                return WsFrame::error_response(
                    "",
                    "notify_channel and notify_chat_id must be set together",
                );
            }
            if let Err(e) = store
                .update_notify(&id, notify_channel, notify_chat_id, notify_thread_id)
                .await
            {
                return WsFrame::error_response("", &format!("update_notify: {e}"));
            }
        }

        // Optional cron_timezone update — empty string clears it.
        if params.get("cron_timezone").is_some() {
            let tz_input = params
                .get("cron_timezone")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let tz_to_store: Option<&str> = if tz_input.is_empty() {
                None
            } else {
                if duduclaw_core::parse_timezone(tz_input).is_none() {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "Unknown cron_timezone '{tz_input}'. Use an IANA name like 'Asia/Taipei'."
                        ),
                    );
                }
                Some(tz_input)
            };
            if let Err(e) = store.update_cron_timezone(&id, tz_to_store).await {
                return WsFrame::error_response("", &format!("update_cron_timezone: {e}"));
            }
        }

        // G3 event-trigger update — only touch these columns when any of the
        // three keys is present. Absent keys mean "leave existing values". An
        // empty-string script/command clears it.
        let has_trigger_update = params.get("trigger_kind").is_some()
            || params.get("condition_script").is_some()
            || params.get("watch_command").is_some();
        if has_trigger_update {
            let trigger_kind_str = match params.get("trigger_kind") {
                Some(v) => v.as_str().map(str::trim).unwrap_or("").to_string(),
                None => existing.trigger_kind.clone(),
            };
            let trigger_kind = match crate::condition_eval::TriggerKind::parse_strict(
                &trigger_kind_str,
            ) {
                Some(k) => k,
                None => {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "Unknown trigger_kind '{trigger_kind_str}'. Use 'time', 'condition', or 'on_exit'."
                        ),
                    );
                }
            };
            let condition_script: Option<String> = match params.get("condition_script") {
                Some(v) => v
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                None => existing.condition_script.clone(),
            };
            let watch_command: Option<String> = match params.get("watch_command") {
                Some(v) => v
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                None => existing.watch_command.clone(),
            };
            if matches!(trigger_kind, crate::condition_eval::TriggerKind::Condition)
                && condition_script.is_none()
            {
                return WsFrame::error_response(
                    "",
                    "trigger_kind 'condition' requires a non-empty 'condition_script'",
                );
            }
            if matches!(trigger_kind, crate::condition_eval::TriggerKind::OnExit)
                && watch_command.is_none()
            {
                return WsFrame::error_response(
                    "",
                    "trigger_kind 'on_exit' requires a non-empty 'watch_command'",
                );
            }
            if let Err(e) = store
                .update_trigger(
                    &id,
                    trigger_kind.as_db(),
                    condition_script.as_deref(),
                    watch_command.as_deref(),
                )
                .await
            {
                return WsFrame::error_response("", &format!("update_trigger: {e}"));
            }
        }

        self.notify_cron_reload().await;
        self.emit_cron_changed(json!({ "action": "updated", "id": id }))
            .await;
        info!(id = %id, "Cron task updated");
        WsFrame::ok_response("", json!({ "success": true, "id": id }))
    }

    pub(crate) async fn handle_cron_set_enabled(&self, params: Value, enabled: bool) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Accept either `id` (preferred) or `name` (legacy).
        let result = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
            store.set_enabled(id, enabled).await
        } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            store.set_enabled_by_name(name, enabled).await
        } else {
            return WsFrame::error_response("", "Missing 'id' or 'name' parameter");
        };

        match result {
            Ok(true) => {
                self.notify_cron_reload().await;
                self.emit_cron_changed(json!({
                    "action": "toggled",
                    "id": params.get("id").and_then(|v| v.as_str()),
                    "name": params.get("name").and_then(|v| v.as_str()),
                    "enabled": enabled,
                }))
                .await;
                info!(enabled, "Cron task enable state changed");
                WsFrame::ok_response("", json!({ "success": true, "enabled": enabled }))
            }
            // `Ok(false)` = no row matched. Nothing changed, so nothing is
            // announced — same fail-quiet rule as the MCP side.
            Ok(false) => WsFrame::error_response("", "Cron task not found"),
            Err(e) => WsFrame::error_response("", &format!("set_enabled: {e}")),
        }
    }

    pub(crate) async fn handle_cron_remove(&self, params: Value) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        let result = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
            store.delete(id).await
        } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            store.delete_by_name(name).await
        } else {
            return WsFrame::error_response("", "Missing 'id' or 'name' parameter");
        };

        match result {
            Ok(true) => {
                self.notify_cron_reload().await;
                self.emit_cron_changed(json!({
                    "action": "removed",
                    "id": params.get("id").and_then(|v| v.as_str()),
                    "name": params.get("name").and_then(|v| v.as_str()),
                }))
                .await;
                info!("Cron task removed");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", "Cron task not found"),
            Err(e) => WsFrame::error_response("", &format!("delete: {e}")),
        }
    }

    /// Trigger a single immediate ("test") execution of a cron task. Routes
    /// through the live [`CronScheduler::run_now`] so the run takes the exact
    /// same path as a scheduled fire (trigger gate + execute + run history).
    /// Returns immediately; the outcome lands in the task's run history, which
    /// the dashboard picks up on its next `cron.list` refresh.
    pub(crate) async fn handle_cron_run_now(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let scheduler = match self.cron_scheduler.read().await.as_ref() {
            Some(s) => s.clone(),
            None => {
                return WsFrame::error_response(
                    "",
                    "Cron scheduler not initialized yet — retry in a moment",
                );
            }
        };
        match scheduler.run_now(&id).await {
            Ok(name) => {
                // The run itself is async; what changed synchronously is the
                // task's run history, which `cron.list` carries — so a refetch
                // is exactly the right reaction here too.
                self.emit_cron_changed(json!({ "action": "ran", "id": id, "name": name }))
                    .await;
                info!(id = %id, name = %name, "Cron task manual run-now triggered");
                WsFrame::ok_response("", json!({ "success": true, "id": id, "name": name }))
            }
            Err(e) => WsFrame::error_response("", &format!("run cron task: {e}")),
        }
    }

    /// Return the built-in "office scheduling" templates so the dashboard can
    /// prefill the routine create dialog. Pure constants — no store access.
    pub(crate) async fn handle_cron_templates(&self) -> WsFrame {
        WsFrame::ok_response(
            "",
            json!({ "templates": crate::cron_templates::templates_json() }),
        )
    }
}
