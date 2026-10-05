//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ═══════════════════════════════════════════════════════════════
// Task Board, Activity Feed, Autopilot, Shared Skills handlers
// ═══════════════════════════════════════════════════════════════
impl MethodHandler {
    // ── Store accessors ─────────────────────────────────────

    pub(crate) async fn task_store(&self) -> Result<Arc<TaskStore>, WsFrame> {
        self.task_store
            .read()
            .await
            .clone()
            .ok_or_else(|| WsFrame::error_response("", "Task store not initialized"))
    }

    pub(crate) async fn ap_store(&self) -> Result<Arc<AutopilotStore>, WsFrame> {
        self.autopilot_store
            .read()
            .await
            .clone()
            .ok_or_else(|| WsFrame::error_response("", "Autopilot store not initialized"))
    }

    /// Broadcast an event via the injected event_tx (best-effort, no error on failure).
    pub(crate) async fn broadcast_event(&self, event: &str, payload: Value) {
        if let Some(tx) = self.event_tx.read().await.as_ref() {
            let frame = WsFrame::Event {
                event: event.to_string(),
                payload,
                seq: None,
                state_version: None,
            };
            let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
        }
    }

    // ── Task handlers ───────────────────────────────────────

    /// Live reader for a task listing. `check_agent_filter!` ran on the
    /// connection's cached identity; the filtered agent is re-checked on the
    /// live one so an unbound or downgraded session stops listing at once.
    pub(crate) fn task_list_reader(
        &self,
        ctx: &UserContext,
        agent_id: Option<&str>,
    ) -> Result<TaskReader<'_>, WsFrame> {
        let reader = TaskReader::new(&self.home_dir, ctx)?;
        if !reader.live().is_admin()
            && !agent_id.is_some_and(|a| reader.live().has_agent_access(a, AccessLevel::Viewer))
        {
            return Err(WsFrame::error_response("", PERMISSION_DENIED));
        }
        Ok(reader)
    }

    pub(crate) async fn handle_tasks_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let status = params.get("status").and_then(|v| v.as_str());
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let reader = match self.task_list_reader(ctx, agent_id) {
            Ok(r) => r,
            Err(f) => return f,
        };
        let priority = params.get("priority").and_then(|v| v.as_str());
        // Goal-scoped callers (the `/goals` page) filter server-side so the
        // whole board never crosses the wire just to keep the goal rows.
        let goal_mode = params.get("goal_mode").and_then(|v| v.as_bool());
        match store
            .list_tasks_filtered(status, agent_id, priority, goal_mode)
            .await
        {
            Ok(rows) => {
                // E8 reverse handoff: "在 <通道> 中開啟" — resolve from the
                // `/goal` entry point's stamped source conversation
                // (`source_channel`/`source_chat_id`, P5 write-back). No
                // message id is stamped at goal-creation time today (see
                // `channel_link.rs` module docs for the platforms this
                // still reaches without one), so this is always a
                // conversation-level link, never message-level. Only the
                // resolved URL (or nothing) crosses to the frontend — the
                // raw `chat_id` never does (project convention: internal
                // identifiers don't leak to the UI).
                let mut tasks: Vec<Value> = Vec::with_capacity(rows.len());
                for r in &rows {
                    let mut v = reader.task_json(r);
                    let channel_link =
                        match (r.source_channel.as_deref(), r.source_chat_id.as_deref()) {
                            (Some(channel), Some(chat_id))
                                if !channel.is_empty() && !chat_id.is_empty() =>
                            {
                                // W2-7: Discord's guild id was snapshotted onto
                                // this row at `/goal` creation time (see
                                // `channel_link.rs` module docs) — pass it
                                // through rather than re-resolving it live.
                                crate::channel_link::resolve_conversation_link(
                                    &self.home_dir,
                                    channel,
                                    chat_id,
                                    None,
                                    r.source_discord_guild_id.as_deref(),
                                )
                                .await
                            }
                            _ => None,
                        };
                    v["channel"] = json!(r.source_channel);
                    v["channel_link"] = json!(channel_link);
                    tasks.push(v);
                }
                WsFrame::ok_response("", json!({ "tasks": tasks }))
            }
            Err(e) => WsFrame::error_response("", &format!("list tasks: {e}")),
        }
    }

    pub(crate) async fn handle_tasks_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match params.get("kind").and_then(Value::as_str).unwrap_or("task").trim().to_ascii_lowercase().as_str() {
            "discovery" => return self.handle_discovery_create(params, ctx).await,
            "goal" => return self.handle_tasks_goal_create(params, ctx).await,
            "" | "task" => {},
            _ => return WsFrame::error_response("", "unknown task kind"),
        }
        if params.get("kind").is_some_and(|value| !value.is_null() && !value.is_string()) {
            return WsFrame::error_response("", "kind must be a string");
        }
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            return WsFrame::error_response("", "title is required");
        }
        // `assigned_to` is REQUIRED on this RPC: the dispatcher
        // (`dispatch_org.rs`, "tasks.create") refuses an empty value before
        // this handler runs, because the per-agent Operator check needs a
        // named target. (The empty-means-unassigned handling below only
        // matters for direct in-process callers.) An unassigned task is never
        // auto-dispatched: the heartbeat task-board pull and the goal-loop
        // driver both filter on a concrete assignee (Bug#4).
        let assigned_to = params
            .get("assigned_to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let priority = params
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("medium")
            .to_string();
        let tags = params
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();

        let mut row = TaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            title.clone(),
            description,
            priority,
            assigned_to.clone(),
            if ctx.user_id.is_empty() {
                "system"
            } else {
                &ctx.user_id
            }
            .to_string(),
        );
        row.tags = tags;
        row.parent_task_id = params
            .get("parent_task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Err(e) = store.insert_task(&row).await {
            return WsFrame::error_response("", &format!("create task: {e}"));
        }

        // Record activity event
        let activity = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "task_created".into(),
            agent_id: assigned_to,
            task_id: Some(row.id.clone()),
            summary: title,
            timestamp: Utc::now().to_rfc3339(),
            metadata: None,
        };
        let _ = store.append_activity(&activity).await;

        let task_json = task_row_to_json(&row);
        self.broadcast_event("task.created", task_json.clone())
            .await;
        self.broadcast_event("activity.new", activity_row_to_json(&activity))
            .await;
        self.emit_autopilot_event(crate::autopilot_engine::AutopilotEvent::TaskCreated {
            task: task_json.clone(),
        })
        .await;

        WsFrame::ok_response("", json!({ "task": task_json }))
    }

    pub(crate) async fn handle_tasks_update(&self, mut params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let task_id = task_id.as_str();
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        // HS4: enforce the caller is bound (Operator) to the task's agent before
        // any mutation. Resolving the agent from the task prevents an Employee
        // bound only to agent A from mutating agent B's tasks.
        // F5-D: the live identity and the task's audience apply as for
        // `tasks.goal_decide` — an update can decide a needs_human task.
        let live = match live_reader_context(&self.home_dir, ctx) {
            Ok(l) => l,
            Err(()) => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let ctx = &live;
        let existing = store.get_task(task_id).await.ok().flatten();
        let owner_agent = existing.as_ref().map(|r| r.assigned_to.clone());
        if let Some(agent) = owner_agent.as_deref() {
            if !task_content_visible(&self.home_dir, ctx, task_id, agent)
                || !ctx.has_agent_access(agent, AccessLevel::Operator)
            {
                return WsFrame::error_response("", PERMISSION_DENIED);
            }
        } else if !ctx.is_admin() {
            // Unknown task → only admins may probe; others get a generic denial.
            return WsFrame::error_response("", "permission denied");
        }
        // If re-assigning to a different agent, the caller must also be bound
        // (Operator) to the destination agent.
        if let Some(dest) = params.get("assigned_to").and_then(|v| v.as_str()) {
            if !dest.is_empty() {
                if let Err(e) = acl::require_agent_access(ctx, dest, AccessLevel::Operator) {
                    return WsFrame::error_response("", &e);
                }
            }
        }
        // HIGH-1 parity with the MCP layer: a depends_on rewire must reference
        // existing tasks only (fail-closed — the store validates shape+cycle,
        // existence is validated here at the RPC boundary). Accepts the stored
        // JSON-array-string form or a JSON array; normalized before update.
        if let Some(deps_val) = params.get("depends_on") {
            let deps: Vec<String> = match deps_val {
                Value::Array(a) => a
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect(),
                Value::String(s) => match serde_json::from_str::<Vec<String>>(s) {
                    Ok(d) => d,
                    Err(_) => {
                        return WsFrame::error_response(
                            "",
                            "depends_on must be a JSON array of task ids",
                        );
                    }
                },
                _ => {
                    return WsFrame::error_response(
                        "",
                        "depends_on must be a JSON array of task ids",
                    );
                }
            };
            for dep in &deps {
                match store.get_task(dep).await {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return WsFrame::error_response(
                            "",
                            &format!("depends_on task not found: {dep}"),
                        );
                    }
                    Err(e) => {
                        return WsFrame::error_response("", &format!("validate depends_on: {e}"));
                    }
                }
            }
            let deps_json = serde_json::to_string(&deps).unwrap_or_else(|_| "[]".into());
            params["depends_on"] = Value::String(deps_json);
        }
        // Capture previous status for TaskStatusChanged event emission.
        let prev_status = existing.map(|r| r.status);
        match store.update_task(task_id, &params).await {
            Ok(Some(row)) => {
                let task_json = task_row_to_json(&row);
                self.broadcast_event("task.updated", task_json.clone())
                    .await;
                self.emit_autopilot_event(crate::autopilot_engine::AutopilotEvent::TaskUpdated {
                    task: task_json.clone(),
                })
                .await;

                // Emit TaskStatusChanged when status actually changed.
                if let (Some(prev), Some(new_status)) = (
                    prev_status.as_deref(),
                    params.get("status").and_then(|v| v.as_str()),
                ) {
                    if prev != new_status {
                        self.emit_autopilot_event(
                            crate::autopilot_engine::AutopilotEvent::TaskStatusChanged {
                                task_id: task_id.to_string(),
                                from: prev.to_string(),
                                to: new_status.to_string(),
                                task: task_json.clone(),
                            },
                        )
                        .await;
                    }
                }

                // If status changed to done/blocked, record activity
                if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
                    let event_type = match status {
                        "done" => "task_completed",
                        "blocked" => "task_blocked",
                        _ => "",
                    };
                    if !event_type.is_empty() {
                        let activity = ActivityRow {
                            id: uuid::Uuid::new_v4().to_string(),
                            event_type: event_type.into(),
                            agent_id: row.assigned_to.clone(),
                            task_id: Some(task_id.to_string()),
                            summary: row.title.clone(),
                            timestamp: Utc::now().to_rfc3339(),
                            metadata: None,
                        };
                        let _ = store.append_activity(&activity).await;
                        self.broadcast_event("activity.new", activity_row_to_json(&activity))
                            .await;
                    }

                    // ── S1 (PORTICO): subgoal closed ⇒ auto-revoke caps ──
                    // A completed task IS a closed subgoal. Revoke every
                    // capability granted under this task_id so no handle
                    // survives its purpose (post-closure reuse denied).
                    if status == "done" {
                        match crate::capability::CapabilityBroker::open(&self.home_dir) {
                            Ok(caps) => match caps.close_scope(task_id).await {
                                Ok(n) if n > 0 => {
                                    tracing::info!(
                                        task_id,
                                        revoked = n,
                                        "task done — capabilities auto-revoked"
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!(task_id, error = %e, "close_scope on task done failed");
                                }
                            },
                            Err(e) => {
                                tracing::warn!(task_id, error = %e, "open capability store for close_scope failed");
                            }
                        }
                    }
                }

                // H1 (unified decision hand-off, 07-unified-decision-design.md
                // §6): a dashboard decision on a needs_human goal task (the
                // Inbox / task board "等你決定" resolution, wired through this
                // generic RPC) must retire the channel card the same way a
                // channel button press does. Only the three legal goal-loop
                // outcomes count as a settled decision (`goal_task_settle_verb`,
                // fail-closed); any other status change out of needs_human is
                // left uncollapsed rather than guessed at. Fire-and-forget:
                // cosmetic, must never delay or fail a decision already
                // durable in the task store.
                if prev_status.as_deref() == Some("needs_human") {
                    if let Some(new_status) = params.get("status").and_then(|v| v.as_str()) {
                        if let Some(verb) = goal_task_settle_verb(new_status) {
                            let decider_name = self.user_display_name(&ctx.user_id).await;
                            crate::goal_notify::spawn_dashboard_collapse(
                                self.home_dir.clone(),
                                task_id.to_string(),
                                row.title.clone(),
                                row.assigned_to.clone(),
                                decider_name,
                                verb,
                            );
                        }
                    }
                }

                // The caller's copy goes through the same reader gate as a
                // list row: an operator outside the task's audience gets
                // the board card only.
                let reply = match TaskReader::new(&self.home_dir, ctx) {
                    Ok(reader) => reader.task_json(&row),
                    Err(_) => restricted_task_json(&task_json),
                };
                WsFrame::ok_response("", json!({ "task": reply }))
            }
            Ok(None) => WsFrame::error_response("", &format!("Task not found: {task_id}")),
            Err(e) => WsFrame::error_response("", &format!("update task: {e}")),
        }
    }

    pub(crate) async fn handle_tasks_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        // HS4: resolve the task's agent and require Operator binding before removal.
        // F5-D: live identity and the task's audience, as for goal_decide.
        let live = match live_reader_context(&self.home_dir, ctx) {
            Ok(l) => l,
            Err(()) => return WsFrame::error_response("", PERMISSION_DENIED),
        };
        let ctx = &live;
        match store.get_task(task_id).await.ok().flatten() {
            Some(row) => {
                if !ctx.has_agent_access(&row.assigned_to, AccessLevel::Operator)
                    || !task_content_visible(&self.home_dir, ctx, task_id, &row.assigned_to)
                {
                    return WsFrame::error_response("", PERMISSION_DENIED);
                }
            }
            None if !ctx.is_admin() => {
                return WsFrame::error_response("", "permission denied");
            }
            None => {}
        }
        match store.remove_task(task_id).await {
            Ok(true) => {
                self.broadcast_event("task.removed", json!({ "task_id": task_id }))
                    .await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", &format!("Task not found: {task_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove task: {e}")),
        }
    }

    pub(crate) async fn handle_tasks_assign(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if task_id.is_empty() || agent_id.is_empty() {
            return WsFrame::error_response("", "task_id and agent_id are required");
        }
        // L1: dead `update` binding removed. Authorization (source + destination
        // agent binding) is enforced by `handle_tasks_update`.
        self.handle_tasks_update(json!({ "task_id": task_id, "assigned_to": agent_id }), ctx)
            .await
    }

    // ── I-3b: task list operations (archive / pin / rename) ─────
    // Thin wrappers over `handle_tasks_update`, same delegation pattern as
    // `handle_tasks_assign` above — HS4 authorization (Operator bound to the
    // task's owning agent) is enforced once, inside `handle_tasks_update`,
    // so these convenience RPCs don't duplicate the ACL walk. `TaskStore::
    // update_task` accepts `archived`/`pinned` as JSON booleans (see its
    // `fields.get("archived").and_then(|v| v.as_bool())` handling).

    pub(crate) async fn handle_tasks_archive(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        self.handle_tasks_update(json!({ "task_id": task_id, "archived": true }), ctx)
            .await
    }

    pub(crate) async fn handle_tasks_unarchive(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        self.handle_tasks_update(json!({ "task_id": task_id, "archived": false }), ctx)
            .await
    }

    pub(crate) async fn handle_tasks_pin(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        self.handle_tasks_update(json!({ "task_id": task_id, "pinned": true }), ctx)
            .await
    }

    pub(crate) async fn handle_tasks_unpin(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        self.handle_tasks_update(json!({ "task_id": task_id, "pinned": false }), ctx)
            .await
    }

    pub(crate) async fn handle_tasks_rename(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if title.is_empty() {
            return WsFrame::error_response("", "title is required");
        }
        self.handle_tasks_update(json!({ "task_id": task_id, "title": title }), ctx)
            .await
    }

    /// I-3b: paginated task listing with a total count — same per-row
    /// enrichment (`channel`/`channel_link`) as `handle_tasks_list`, plus
    /// the `archived`/`pinned` fields that `task_row_to_json` doesn't carry
    /// (left untouched deliberately; this endpoint merges them in locally
    /// rather than editing that shared helper).
    pub(crate) async fn handle_tasks_list_page(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let status = params.get("status").and_then(|v| v.as_str());
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let reader = match self.task_list_reader(ctx, agent_id) {
            Ok(r) => r,
            Err(f) => return f,
        };
        let priority = params.get("priority").and_then(|v| v.as_str());
        let goal_mode = params.get("goal_mode").and_then(|v| v.as_bool());
        let archived = params.get("archived").and_then(|v| v.as_bool());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
        let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);
        match store
            .list_tasks_paginated(
                status, agent_id, priority, goal_mode, archived, limit, offset,
            )
            .await
        {
            Ok((rows, total)) => {
                let mut tasks: Vec<Value> = Vec::with_capacity(rows.len());
                for r in &rows {
                    let mut v = reader.task_json(r);
                    v["archived"] = json!(r.archived);
                    v["pinned"] = json!(r.pinned);
                    let channel_link =
                        match (r.source_channel.as_deref(), r.source_chat_id.as_deref()) {
                            (Some(channel), Some(chat_id))
                                if !channel.is_empty() && !chat_id.is_empty() =>
                            {
                                crate::channel_link::resolve_conversation_link(
                                    &self.home_dir,
                                    channel,
                                    chat_id,
                                    None,
                                    r.source_discord_guild_id.as_deref(),
                                )
                                .await
                            }
                            _ => None,
                        };
                    v["channel"] = json!(r.source_channel);
                    v["channel_link"] = json!(channel_link);
                    tasks.push(v);
                }
                WsFrame::ok_response(
                    "",
                    json!({ "tasks": tasks, "total": total, "limit": limit, "offset": offset }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("list tasks page: {e}")),
        }
    }
}
