//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Co-edited plan handlers (U4) ────────────────────────
    //
    // A plan is agent-scoped exactly like a task (`plans.agent_id` mirrors
    // `tasks.assigned_to`): Viewer binding to read, Operator to mutate.
    // Every mutation appends to the Activity Feed so the timeline shows the
    // co-editing, and broadcasts `plan.updated` for live dashboard refresh.

    /// Max plan / step text length (chars, CJK-safe truncation).
    pub(crate) const MAX_PLAN_TEXT_CHARS: usize = 2000;

    /// Resolve the agent owning `plan_id` and confirm the caller may access it
    /// at `level`. Fail-closed: unknown plan denies for non-admins (an admin
    /// gets a plain "not found"). Mirrors `authorize_task_access`.
    pub(crate) async fn authorize_plan_access(
        &self,
        store: &TaskStore,
        ctx: &UserContext,
        plan_id: &str,
        level: AccessLevel,
    ) -> Result<PlanRow, WsFrame> {
        match store.get_plan(plan_id).await {
            Ok(Some(row)) => {
                if let Err(e) = acl::require_agent_access(ctx, &row.agent_id, level) {
                    return Err(WsFrame::error_response("", &e));
                }
                Ok(row)
            }
            Ok(None) => {
                if ctx.is_admin() {
                    Err(WsFrame::error_response(
                        "",
                        &format!("Plan not found: {plan_id}"),
                    ))
                } else {
                    Err(WsFrame::error_response("", "permission denied"))
                }
            }
            Err(e) => Err(WsFrame::error_response("", &format!("get plan: {e}"))),
        }
    }

    /// Append a plan mutation to the Activity Feed + broadcast the co-editing
    /// signal (`plan.updated` for panel refresh, `activity.new` for the feed).
    pub(crate) async fn record_plan_activity(
        &self,
        store: &TaskStore,
        event_type: &str,
        plan: &PlanRow,
        summary: String,
    ) {
        let activity = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.into(),
            agent_id: plan.agent_id.clone(),
            task_id: None,
            summary,
            timestamp: Utc::now().to_rfc3339(),
            metadata: Some(json!({ "plan_id": plan.id }).to_string()),
        };
        let _ = store.append_activity(&activity).await;
        self.broadcast_event("activity.new", activity_row_to_json(&activity))
            .await;
        self.broadcast_event(
            "plan.updated",
            json!({ "plan_id": plan.id, "agent_id": plan.agent_id }),
        )
        .await;
    }

    /// Actor label for activity summaries: the logged-in user or "system".
    pub(crate) fn plan_actor(ctx: &UserContext) -> &str {
        if ctx.user_id.is_empty() {
            "system"
        } else {
            ctx.user_id.as_str()
        }
    }

    pub(crate) async fn handle_plans_list(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let status = params
            .get("status")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let plans = match store.list_plans(agent_id, status).await {
            Ok(rows) => rows,
            Err(e) => return WsFrame::error_response("", &format!("list plans: {e}")),
        };
        // Progress counters ride along so the list view renders without N
        // follow-up `plans.get` calls (plans are few; steps are tens).
        let mut out: Vec<Value> = Vec::with_capacity(plans.len());
        for p in &plans {
            let steps = store.list_plan_steps(&p.id).await.unwrap_or_default();
            let done = steps
                .iter()
                .filter(|s| s.status == "done" || s.status == "skipped")
                .count();
            let mut v = plan_row_to_json(p);
            v["steps_total"] = json!(steps.len());
            v["steps_done"] = json!(done);
            out.push(v);
        }
        WsFrame::ok_response("", json!({ "plans": out }))
    }

    pub(crate) async fn handle_plans_get(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let plan_id = params.get("plan_id").and_then(|v| v.as_str()).unwrap_or("");
        if plan_id.is_empty() {
            return WsFrame::error_response("", "plan_id is required");
        }
        let plan = match self
            .authorize_plan_access(&store, ctx, plan_id, AccessLevel::Viewer)
            .await
        {
            Ok(p) => p,
            Err(f) => return f,
        };
        match store.list_plan_steps(plan_id).await {
            Ok(steps) => WsFrame::ok_response(
                "",
                json!({
                    "plan": plan_row_to_json(&plan),
                    "steps": steps.iter().map(plan_step_row_to_json).collect::<Vec<_>>(),
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("list plan steps: {e}")),
        }
    }

    pub(crate) async fn handle_plans_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let title_raw = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
        if title_raw.trim().is_empty() {
            return WsFrame::error_response("", "title is required");
        }
        // agent_id presence + Operator binding already enforced at dispatch.
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut plan = PlanRow::new(
            uuid::Uuid::new_v4().to_string(),
            duduclaw_core::truncate_chars(title_raw.trim(), 200),
            agent_id,
            Self::plan_actor(ctx).to_string(),
        );
        if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
            plan.description = duduclaw_core::truncate_chars(desc, Self::MAX_PLAN_TEXT_CHARS);
        }
        // Optional G8 goal linkage — fail-closed on a dangling goal id.
        if let Some(goal_id) = params
            .get("goal_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            match store.get_goal(goal_id).await {
                Ok(Some(_)) => plan.goal_id = Some(goal_id.to_string()),
                Ok(None) => {
                    return WsFrame::error_response("", &format!("goal not found: {goal_id}"));
                }
                Err(e) => return WsFrame::error_response("", &format!("validate goal_id: {e}")),
            }
        }
        if let Err(e) = store.insert_plan(&plan).await {
            return WsFrame::error_response("", &format!("create plan: {e}"));
        }
        // Optional initial steps: [{text, assignee_kind?, assignee?}, …].
        let mut steps_out: Vec<Value> = Vec::new();
        if let Some(steps) = params.get("steps").and_then(|v| v.as_array()) {
            for s in steps {
                let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.trim().is_empty() {
                    continue;
                }
                let kind = s
                    .get("assignee_kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent");
                let assignee =
                    s.get("assignee")
                        .and_then(|v| v.as_str())
                        .unwrap_or(if kind == "agent" {
                            plan.agent_id.as_str()
                        } else {
                            ""
                        });
                match store
                    .add_plan_step(
                        &plan.id,
                        &uuid::Uuid::new_v4().to_string(),
                        &duduclaw_core::truncate_chars(text.trim(), Self::MAX_PLAN_TEXT_CHARS),
                        kind,
                        assignee,
                        None,
                    )
                    .await
                {
                    Ok(row) => steps_out.push(plan_step_row_to_json(&row)),
                    Err(e) => return WsFrame::error_response("", &format!("add plan step: {e}")),
                }
            }
        }
        self.record_plan_activity(
            &store,
            "plan_created",
            &plan,
            format!("{} created plan: {}", Self::plan_actor(ctx), plan.title),
        )
        .await;
        WsFrame::ok_response(
            "",
            json!({ "plan": plan_row_to_json(&plan), "steps": steps_out }),
        )
    }

    pub(crate) async fn handle_plans_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let plan_id = params.get("plan_id").and_then(|v| v.as_str()).unwrap_or("");
        if plan_id.is_empty() {
            return WsFrame::error_response("", "plan_id is required");
        }
        if let Err(f) = self
            .authorize_plan_access(&store, ctx, plan_id, AccessLevel::Operator)
            .await
        {
            return f;
        }
        match store.update_plan(plan_id, &params).await {
            Ok(Some(plan)) => {
                self.record_plan_activity(
                    &store,
                    "plan_updated",
                    &plan,
                    format!("{} updated plan: {}", Self::plan_actor(ctx), plan.title),
                )
                .await;
                WsFrame::ok_response("", json!({ "plan": plan_row_to_json(&plan) }))
            }
            Ok(None) => WsFrame::error_response("", &format!("Plan not found: {plan_id}")),
            Err(e) => WsFrame::error_response("", &format!("update plan: {e}")),
        }
    }

    pub(crate) async fn handle_plans_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let plan_id = params.get("plan_id").and_then(|v| v.as_str()).unwrap_or("");
        if plan_id.is_empty() {
            return WsFrame::error_response("", "plan_id is required");
        }
        let plan = match self
            .authorize_plan_access(&store, ctx, plan_id, AccessLevel::Operator)
            .await
        {
            Ok(p) => p,
            Err(f) => return f,
        };
        match store.remove_plan(plan_id).await {
            Ok(true) => {
                self.record_plan_activity(
                    &store,
                    "plan_removed",
                    &plan,
                    format!("{} removed plan: {}", Self::plan_actor(ctx), plan.title),
                )
                .await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", &format!("Plan not found: {plan_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove plan: {e}")),
        }
    }

    pub(crate) async fn handle_plans_add_step(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let plan_id = params.get("plan_id").and_then(|v| v.as_str()).unwrap_or("");
        if plan_id.is_empty() {
            return WsFrame::error_response("", "plan_id is required");
        }
        let plan = match self
            .authorize_plan_access(&store, ctx, plan_id, AccessLevel::Operator)
            .await
        {
            Ok(p) => p,
            Err(f) => return f,
        };
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            return WsFrame::error_response("", "text is required");
        }
        let kind = params
            .get("assignee_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");
        let assignee =
            params
                .get("assignee")
                .and_then(|v| v.as_str())
                .unwrap_or(if kind == "agent" {
                    plan.agent_id.as_str()
                } else {
                    ""
                });
        let position = params
            .get("position")
            .and_then(|v| v.as_u64())
            .map(|p| p as usize);
        match store
            .add_plan_step(
                plan_id,
                &uuid::Uuid::new_v4().to_string(),
                &duduclaw_core::truncate_chars(text.trim(), Self::MAX_PLAN_TEXT_CHARS),
                kind,
                assignee,
                position,
            )
            .await
        {
            Ok(row) => {
                self.record_plan_activity(
                    &store,
                    "plan_step_added",
                    &plan,
                    format!(
                        "{} added a step to {}: {}",
                        Self::plan_actor(ctx),
                        plan.title,
                        duduclaw_core::truncate_chars(&row.text, 80)
                    ),
                )
                .await;
                WsFrame::ok_response("", json!({ "step": plan_step_row_to_json(&row) }))
            }
            Err(e) => WsFrame::error_response("", &format!("add plan step: {e}")),
        }
    }

    pub(crate) async fn handle_plans_update_step(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let step_id = params.get("step_id").and_then(|v| v.as_str()).unwrap_or("");
        if step_id.is_empty() {
            return WsFrame::error_response("", "step_id is required");
        }
        // Resolve the step → its plan → the plan's agent, then gate (Operator).
        let step = match store.get_plan_step(step_id).await {
            Ok(Some(s)) => s,
            Ok(None) if ctx.is_admin() => {
                return WsFrame::error_response("", &format!("Step not found: {step_id}"));
            }
            Ok(None) => return WsFrame::error_response("", "permission denied"),
            Err(e) => return WsFrame::error_response("", &format!("get step: {e}")),
        };
        let plan = match self
            .authorize_plan_access(&store, ctx, &step.plan_id, AccessLevel::Operator)
            .await
        {
            Ok(p) => p,
            Err(f) => return f,
        };
        // Field edits (text / status / assignee_kind / assignee) — validated
        // fail-closed by the store; `position` alone is a pure reorder.
        let has_field_edit = ["text", "status", "assignee_kind", "assignee"]
            .iter()
            .any(|k| params.get(k).is_some());
        if has_field_edit {
            if let Err(e) = store.update_plan_step(step_id, &params).await {
                return WsFrame::error_response("", &format!("update plan step: {e}"));
            }
        }
        if let Some(pos) = params.get("position").and_then(|v| v.as_u64()) {
            match store
                .move_plan_step(&step.plan_id, step_id, pos as usize)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return WsFrame::error_response("", &format!("Step not found: {step_id}"));
                }
                Err(e) => return WsFrame::error_response("", &format!("move plan step: {e}")),
            }
        } else if !has_field_edit {
            return WsFrame::error_response("", "no step fields to update");
        }
        let updated = match store.get_plan_step(step_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return WsFrame::error_response("", &format!("Step not found: {step_id}")),
            Err(e) => return WsFrame::error_response("", &format!("get step: {e}")),
        };
        self.record_plan_activity(
            &store,
            "plan_step_updated",
            &plan,
            format!(
                "{} updated a step in {}: {}",
                Self::plan_actor(ctx),
                plan.title,
                duduclaw_core::truncate_chars(&updated.text, 80)
            ),
        )
        .await;
        WsFrame::ok_response("", json!({ "step": plan_step_row_to_json(&updated) }))
    }

    pub(crate) async fn handle_plans_remove_step(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let step_id = params.get("step_id").and_then(|v| v.as_str()).unwrap_or("");
        if step_id.is_empty() {
            return WsFrame::error_response("", "step_id is required");
        }
        let step = match store.get_plan_step(step_id).await {
            Ok(Some(s)) => s,
            Ok(None) if ctx.is_admin() => {
                return WsFrame::error_response("", &format!("Step not found: {step_id}"));
            }
            Ok(None) => return WsFrame::error_response("", "permission denied"),
            Err(e) => return WsFrame::error_response("", &format!("get step: {e}")),
        };
        let plan = match self
            .authorize_plan_access(&store, ctx, &step.plan_id, AccessLevel::Operator)
            .await
        {
            Ok(p) => p,
            Err(f) => return f,
        };
        match store.remove_plan_step(step_id).await {
            Ok(Some(removed)) => {
                self.record_plan_activity(
                    &store,
                    "plan_step_removed",
                    &plan,
                    format!(
                        "{} removed a step from {}: {}",
                        Self::plan_actor(ctx),
                        plan.title,
                        duduclaw_core::truncate_chars(&removed.text, 80)
                    ),
                )
                .await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(None) => WsFrame::error_response("", &format!("Step not found: {step_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove plan step: {e}")),
        }
    }
}
