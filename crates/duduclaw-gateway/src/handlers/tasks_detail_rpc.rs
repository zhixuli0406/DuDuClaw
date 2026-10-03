//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Task comment handlers (L2) ──────────────────────────

    /// Maximum comment length (characters). Long enough for a substantive note,
    /// bounded so a single comment can't bloat the store / timeline.
    pub(crate) const MAX_COMMENT_CHARS: usize = 4000;

    /// Resolve the agent that owns `task_id` and confirm the caller may access
    /// it at `level`. Fail-closed: an unknown task denies for non-admins (an
    /// admin gets a plain "not found"). Returns the loaded `TaskRow` on success.
    pub(crate) async fn authorize_task_access(
        &self,
        store: &TaskStore,
        ctx: &UserContext,
        task_id: &str,
        level: AccessLevel,
    ) -> Result<TaskRow, WsFrame> {
        match store.get_task(task_id).await {
            Ok(Some(row)) => {
                if let Err(e) = acl::require_agent_access(ctx, &row.assigned_to, level) {
                    return Err(WsFrame::error_response("", &e));
                }
                Ok(row)
            }
            Ok(None) => {
                if ctx.is_admin() {
                    Err(WsFrame::error_response(
                        "",
                        &format!("Task not found: {task_id}"),
                    ))
                } else {
                    Err(WsFrame::error_response("", "permission denied"))
                }
            }
            Err(e) => Err(WsFrame::error_response("", &format!("get task: {e}"))),
        }
    }

    pub(crate) async fn handle_tasks_comment(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        // Empty / whitespace-only comments are rejected before the ACL round-trip.
        let body_raw = params.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if body_raw.trim().is_empty() {
            return WsFrame::error_response("", "body is required");
        }
        // Viewer suffices to comment — anyone who can see the task may discuss it.
        if let Err(f) = self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            return f;
        }
        // CJK-safe length cap on the char count (never slices mid-codepoint).
        let body = duduclaw_core::truncate_chars(body_raw.trim(), Self::MAX_COMMENT_CHARS);
        let author = if ctx.user_id.is_empty() {
            "system"
        } else {
            ctx.user_id.as_str()
        };
        let row = CommentRow {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            author_user: author.to_string(),
            body,
            created_at: Utc::now().to_rfc3339(),
        };
        if let Err(e) = store.insert_comment(&row).await {
            return WsFrame::error_response("", &format!("comment: {e}"));
        }
        let comment_json = comment_row_to_json(&row);
        // Surface the new comment to every open dashboard tab in real time.
        self.broadcast_event("task.comment", comment_json.clone())
            .await;
        WsFrame::ok_response("", json!({ "comment": comment_json }))
    }

    pub(crate) async fn handle_tasks_comments(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        if let Err(f) = self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            return f;
        }
        match store.list_comments(task_id).await {
            Ok(rows) => {
                let comments: Vec<Value> = rows.iter().map(comment_row_to_json).collect();
                WsFrame::ok_response("", json!({ "comments": comments }))
            }
            Err(e) => WsFrame::error_response("", &format!("list comments: {e}")),
        }
    }

    /// Iterative Kanban: the revision timeline (dispatched → submitted → verdict
    /// per round) for one task. Viewer-gated on the task's owning agent.
    pub(crate) async fn handle_tasks_iterations(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        if let Err(f) = self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            return f;
        }
        match store.list_iterations(task_id).await {
            Ok(rows) => {
                let iterations: Vec<Value> = rows.iter().map(task_iteration_to_json).collect();
                WsFrame::ok_response("", json!({ "iterations": iterations }))
            }
            Err(e) => WsFrame::error_response("", &format!("list iterations: {e}")),
        }
    }

    /// WP-F (P2-c) `tasks.changes` — the file-change evidence behind the
    /// dashboard's 「變更」tab: what this task's rounds actually wrote / edited /
    /// deleted, so a human deciding a `needs_human` escalation reviews the
    /// recorded effects instead of the agent's narrative. Read-only board
    /// evidence (Viewer), gated on the task's owning agent inside the handler
    /// exactly like `tasks.comments` / `tasks.iterations`.
    ///
    /// Attribution: the native half is keyed by task id (persisted per
    /// dispatch round by `task_changes::record_round_changes`); the MCP-audit
    /// half reuses the claim→review window convention `dispatch_engine` uses
    /// for the judge's `<tool_activity>` block. No evidence ⇒ an empty list —
    /// the tab says so rather than inventing a summary.
    pub(crate) async fn handle_tasks_changes(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        let task = match self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            Ok(t) => t,
            Err(f) => return f,
        };
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n > 0)
            .unwrap_or(crate::task_changes::DEFAULT_QUERY_LIMIT);

        let agent_id = task
            .claimed_by
            .clone()
            .unwrap_or_else(|| task.assigned_to.clone());
        let since = task
            .claimed_at
            .clone()
            .unwrap_or_else(|| task.created_at.clone());
        let until = task
            .completed_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        let evidence = crate::task_changes::collect_task_changes(
            &self.home_dir,
            task_id,
            &agent_id,
            &since,
            &until,
            limit,
        );
        let changes: Vec<Value> = evidence.changes.iter().map(|c| c.to_wire_json()).collect();
        WsFrame::ok_response(
            "",
            json!({
                "changes": changes,
                "distinct_paths": evidence.distinct_paths,
                "truncated": evidence.truncated,
            }),
        )
    }

    /// I-2b `tasks.artifacts` — the deliverables a task produced, for the
    /// detail page's 「產物」tab: 「東西在哪」, which the page previously could
    /// not answer at all (走查 2 卡點 1).
    ///
    /// Read-only board evidence (Viewer), gated on the task's owning agent
    /// exactly like `tasks.changes`. Two trails, one shape: rows the artifacts
    /// ledger ties to this task id, plus deliverable-shaped file writes the
    /// task's own change ledger recorded. Rows that only the (agent,
    /// claim→review window) convention places here come back labelled
    /// `attribution: "inferred"` so the UI can say so — an inference is never
    /// dressed up as a fact, and no evidence yields an empty list.
    pub(crate) async fn handle_tasks_artifacts(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        let task = match self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            Ok(t) => t,
            Err(f) => return f,
        };
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n > 0)
            .unwrap_or(crate::artifacts::DEFAULT_QUERY_LIMIT);

        let agent_id = task
            .claimed_by
            .clone()
            .unwrap_or_else(|| task.assigned_to.clone());
        let since = task
            .claimed_at
            .clone()
            .unwrap_or_else(|| task.created_at.clone());
        let until = task
            .completed_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        let evidence = crate::artifacts::collect_task_artifacts(
            &self.home_dir,
            task_id,
            &agent_id,
            &since,
            &until,
            limit,
        );
        let artifacts: Vec<Value> = evidence
            .artifacts
            .iter()
            .map(|a| a.to_wire_json())
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "artifacts": artifacts,
                "truncated": evidence.truncated,
                "inferred_count": evidence.inferred_count,
            }),
        )
    }

    /// Read-only role drill-down for a team goal. Reuse the task's Viewer ACL
    /// before reading its task-scoped ledger rows; the ledger is an internal
    /// file, so a caller must never choose an arbitrary path or agent id.
    pub(crate) async fn handle_tasks_role_turns(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        if let Err(f) = self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            return f;
        }
        let turns: Vec<Value> = crate::role_turns::read_rows_for_task(&self.home_dir, task_id)
            .into_iter()
            .map(|row| {
                json!({
                    "timestamp": row.timestamp,
                    "round": row.round,
                    "role": row.role,
                    "runtime": row.runtime,
                    "provider": row.provider,
                    "request_model": row.request_model,
                    "response_model": row.response_model,
                    "runtime_used": row.runtime_used,
                    "failover": row.failover,
                    "outcome": row.outcome,
                    "observation_fidelity": row.observation_fidelity,
                    "usage_input_tokens": row.usage.usage_input_tokens,
                    "usage_output_tokens": row.usage.usage_output_tokens,
                    "usage_cache_read_tokens": row.usage.usage_cache_read_tokens,
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "turns": turns }))
    }

    /// `tasks.goal_create` — assign an autonomous goal from the dashboard,
    /// with the SAME semantics as the channel `/goal` command
    /// (`chat_commands::handle_goal_create`): `goal_mode` task in `todo`,
    /// acceptance criteria defaulting to the goal text, structured outcome
    /// spec parsed fail-closed. Differences, both deliberate: `created_by`
    /// is `goal:dashboard` and there is no source conversation (progress
    /// and needs_human cards fall back to the agent's `[proactive]` notify
    /// target, exactly like any channel-less goal). The optional LLMCompiler
    /// sub-task decomposition (`goal_plan::planner_enabled`) is chat-path-only
    /// for now. I-1c `plan_first` (dashboard-path only, see below) is a
    /// different feature — a single-task narrative plan for human approval,
    /// not a machine-parsed DAG — and the two are independent. Operator
    /// access on the target agent.
    pub(crate) async fn handle_tasks_goal_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "valid agent_id is required");
        }
        if let Err(e) = acl::require_agent_access(ctx, agent_id, AccessLevel::Operator) {
            return WsFrame::error_response("", &e);
        }
        // T5/O4 (feature audit 2026-09-29): every field below is read
        // verbatim from the request and handed to `goal_create_core`, which
        // is now the ONE implementation of "create an autonomous goal" —
        // shared with the MCP `tasks_create kind="goal"` entry. Validation
        // (priority / outcome / duration), the H9-G contract freeze, I-1c
        // plan-first parking, the team freeze and the activity row all live
        // there; authorisation (the Operator ACL above) deliberately stays
        // here, because the two rails answer to different authorities.
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let duration_hours = match params.get("duration_hours") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_f64() {
                Some(h) => Some(h),
                None => return WsFrame::error_response("", "duration_hours must be a number"),
            },
        };
        let req = crate::goal_create_core::GoalCreateRequest {
            agent_id: agent_id.to_string(),
            created_by: "goal:dashboard".to_string(),
            description: params
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            acceptance_criteria: params
                .get("acceptance_criteria")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            priority: params
                .get("priority")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            outcome: params
                .get("outcome")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            duration_hours,
            risk_boundary: params
                .get("risk_boundary")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            require_beliefs: params
                .get("require_beliefs")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            // I-1c "想一想": dashboard AssignSheet's third execution mode
            // (alongside 問一問 / 直接做, WorkBuddy Plan mode). Default `false`
            // keeps every existing caller (including the chat `/goal`
            // command, which never sends this field) byte-identical.
            plan_first: params
                .get("plan_first")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            source_label: "儀表板".to_string(),
        };
        let created = match crate::goal_create_core::create_goal_task(
            &self.home_dir,
            &store,
            req,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let task = created.task;
        let plan_first = created.plan_first;
        let cap = crate::goal_loop::GoalLoopConfig::from_home(&self.home_dir).iteration_cap;
        let dispatch_enabled = crate::dispatch_engine::dispatch_engine_enabled(&self.home_dir);
        WsFrame::ok_response(
            "",
            json!({
                "task": task_row_to_json(&task),
                "iteration_cap": cap,
                "dispatch_enabled": dispatch_enabled,
                // I-1c: lets the caller (AssignSheet) show "計畫已生成，等待
                // 你核准" instead of the normal "已交辦" toast without having
                // to re-derive it from `task.status`/`pause_reason`.
                "plan_first": plan_first,
            }),
        )
    }

    /// `tasks.timeline` — one task's whole goal-loop story in a single call:
    /// the task row (goal fields included), every judge round, the
    /// task-scoped Activity Feed (kickoff / oscillation / needs_human /
    /// human decisions — previously only reachable by client-side filtering
    /// a bounded global window), and any still-pending kickoff approval.
    /// Read-only aggregation for the `/goals` page; Viewer access.
    pub(crate) async fn handle_tasks_timeline(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        let row = match self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Viewer)
            .await
        {
            Ok(r) => r,
            Err(f) => return f,
        };
        let iterations = store.list_iterations(task_id).await.unwrap_or_default();
        let activity = store
            .list_activity_for_task(task_id, 500)
            .await
            .unwrap_or_default();
        // A pending kickoff approval is an intervention node the timeline
        // must show *now* — decided ones already live in the activity feed
        // (`goal_loop.kickoff_approved` / `kickoff_denied`).
        let pending_kickoff = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(broker) => broker
                .list_pending(Some(&row.assigned_to))
                .await
                .unwrap_or_default()
                .into_iter()
                .find(|r| {
                    r.action_kind == "goal_kickoff"
                        && r.payload.get("task_id").and_then(|t| t.as_str()) == Some(task_id)
                })
                .map(|r| {
                    json!({
                        "id": r.id,
                        "summary": r.summary,
                        "created_at": r.created_at,
                        "ttl_seconds": r.ttl_seconds,
                    })
                }),
            Err(_) => None,
        };
        // Durable run ↔ round linkage (dispatch_runs.task_id/round) — lets
        // the timeline deep-link each round to its execution transcript.
        let runs: Vec<Value> = crate::run_steps::shared_store(&self.home_dir)
            .and_then(|s| s.list_dispatch_runs_for_task(task_id, 100).ok())
            .unwrap_or_default()
            .iter()
            .map(|r| {
                json!({
                    "id": format!("dispatch:{}", r.id),
                    "round": r.round,
                    "status": r.status,
                    "started_at": r.started_at,
                    "ended_at": r.ended_at,
                    "step_count": r.step_count,
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "task": task_row_to_json(&row),
                "iterations": iterations.iter().map(task_iteration_to_json).collect::<Vec<_>>(),
                "activity": activity.iter().map(activity_row_to_json).collect::<Vec<_>>(),
                "pending_kickoff": pending_kickoff,
                "runs": runs,
                // WP-G2: the per-criterion acceptance ledger, `null` when the
                // goal has none (older goals, or created with
                // `[goal_loop] criteria_ledger = "off"`).
                "criteria_ledger": criteria_ledger_json(&self.home_dir, &row),
            }),
        )
    }

    /// `tasks.goal_decide` — the dashboard's needs_human intervention
    /// (`retry` / `done` / `abort` / `takeover`), routed through the SAME
    /// path as the channel buttons (`goal_notify`), not a bare status
    /// update: fail-closed `WHERE status='needs_human'`, claim/lease/result
    /// cleanup on retry, Activity Feed event, channel-card collapse.
    /// Operator access on the task's agent — the same bar as `tasks.update`.
    ///
    /// I-3a: `action: "continue"` is a fifth, separate action — WorkBuddy's
    /// "已完成／失敗任務可續推" (design doc §3.3): reopen a `done` / `failed`
    /// / `cancelled` goal task with a required follow-up `message` instead
    /// of resolving a pending `needs_human` intervention. Same authorization
    /// gate as every other branch here; routed to
    /// [`crate::goal_notify::apply_continue_from_dashboard`] rather than the
    /// `DecisionAct` match below since it operates on a different set of
    /// source statuses and always carries a message.
    pub(crate) async fn handle_tasks_goal_decide(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        let action_str = params.get("action").and_then(|v| v.as_str()).unwrap_or("");

        if action_str == "continue" {
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| duduclaw_core::truncate_chars(s.trim(), 2000))
                .unwrap_or_default();
            if message.is_empty() {
                return WsFrame::error_response("", "接著做需要附上訊息");
            }
            if let Err(f) = self
                .authorize_task_access(&store, ctx, task_id, AccessLevel::Operator)
                .await
            {
                return f;
            }
            let decider = format!("dashboard:{}", ctx.user_id);
            return match crate::goal_notify::apply_continue_from_dashboard(
                &self.home_dir,
                &decider,
                task_id,
                &message,
            )
            .await
            {
                Ok(message) => {
                    let task = store.get_task(task_id).await.ok().flatten();
                    WsFrame::ok_response(
                        "",
                        json!({
                            "ok": true,
                            "message": message,
                            "task": task.as_ref().map(task_row_to_json),
                        }),
                    )
                }
                Err(e) => WsFrame::error_response("", &e),
            };
        }

        let act = match action_str {
            "retry" => crate::decision_action::DecisionAct::Retry,
            "done" => crate::decision_action::DecisionAct::Done,
            "abort" => crate::decision_action::DecisionAct::Abort,
            "takeover" => crate::decision_action::DecisionAct::Takeover,
            _ => {
                return WsFrame::error_response(
                    "",
                    "action must be one of retry|done|abort|takeover|continue",
                );
            }
        };
        let note = params
            .get("note")
            .and_then(|v| v.as_str())
            .map(|s| duduclaw_core::truncate_chars(s.trim(), 2000))
            .unwrap_or_default();
        if let Err(f) = self
            .authorize_task_access(&store, ctx, task_id, AccessLevel::Operator)
            .await
        {
            return f;
        }
        let decider = format!("dashboard:{}", ctx.user_id);
        let decider_name = self.user_display_name(&ctx.user_id).await;
        match crate::goal_notify::apply_needs_human_from_dashboard(
            &self.home_dir,
            &decider,
            decider_name,
            task_id,
            act,
            &note,
        )
        .await
        {
            Ok(message) => {
                let task = store.get_task(task_id).await.ok().flatten();
                WsFrame::ok_response(
                    "",
                    json!({
                        "ok": true,
                        "message": message,
                        "task": task.as_ref().map(task_row_to_json),
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Iterative Kanban: per-agent + board flow metrics (first-pass yield, avg
    /// rounds, dual-clock means, review queue depth, WIP limit + 7-day accept
    /// throughput for the Little's-Law wait estimate). Non-admins pass an
    /// `agent_id` (check_agent_filter) and see only that agent's slice; admins
    /// see all agents.
    pub(crate) async fn handle_tasks_flow_metrics(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let now = Utc::now().to_rfc3339();
        let metrics = match store.flow_metrics(&now).await {
            Ok(m) => m,
            Err(e) => return WsFrame::error_response("", &format!("flow metrics: {e}")),
        };
        let review_wip_limit = crate::goal_loop::review_wip_limit(&self.home_dir);
        // Scope the per-agent slice when the caller filtered to one agent.
        let filter_agent = params.get("agent_id").and_then(|v| v.as_str());
        let agents: Vec<Value> = metrics
            .agents
            .iter()
            .filter(|a| filter_agent.map(|f| f == a.agent_id).unwrap_or(true))
            .map(|a| {
                json!({
                    "agent_id": a.agent_id,
                    "goal_tasks": a.goal_tasks,
                    "finished": a.finished,
                    "first_pass_yield": a.first_pass_yield,
                    "avg_rounds": a.avg_rounds,
                    "avg_agent_seconds": a.avg_agent_seconds,
                    "avg_cycle_seconds": a.avg_cycle_seconds,
                    "review_queue_depth": a.review_queue_depth,
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "agents": agents,
                "review_queue_depth": metrics.review_queue_depth,
                "review_wip_limit": review_wip_limit,
                "accepts_last_7d": metrics.accepts_last_7d,
                "avg_daily_accepts_7d": metrics.avg_daily_accepts_7d,
            }),
        )
    }
}

/// WP-G2 `criteria_ledger` field of `tasks.timeline`: the design's RPC shape
/// (`{mode, units[], last_report_round, invalid_reports}`) or `null`. `mode`
/// is the mode in effect now (`[goal_loop] criteria_ledger`).
pub(crate) fn criteria_ledger_json(home_dir: &std::path::Path, row: &TaskRow) -> Value {
    use crate::goal_loop::criteria_ledger::{CriteriaLedger, CriteriaLedgerMode};
    CriteriaLedger::from_json(row.criteria_ledger.as_deref())
        .map(|l| l.to_rpc_json(CriteriaLedgerMode::from_home(Some(home_dir))))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod criteria_ledger_rpc_tests {
    use super::*;
    use crate::goal_loop::criteria_ledger::{CriteriaLedger, CriteriaLedgerMode};

    #[test]
    fn tasks_timeline_criteria_ledger_shape_and_null() {
        let home = tempfile::tempdir().unwrap();
        let mut row = TaskRow::new(
            "t1".into(),
            "goal".into(),
            String::new(),
            "medium".into(),
            "a".into(),
            "system".into(),
        );
        assert!(criteria_ledger_json(home.path(), &row).is_null());
        row.criteria_ledger = Some("not json".into());
        assert!(criteria_ledger_json(home.path(), &row).is_null());

        row.criteria_ledger = CriteriaLedger::new("t1", "產出報表\n寄出", CriteriaLedgerMode::Report)
            .map(|l| l.to_json());
        std::fs::write(
            home.path().join("config.toml"),
            "[goal_loop]\ncriteria_ledger = \"enforce\"\n",
        )
        .unwrap();
        let v = criteria_ledger_json(home.path(), &row);
        assert_eq!(v["mode"], "enforce", "mode is the one in effect now");
        assert_eq!(v["units"].as_array().unwrap().len(), 2);
        let unit = &v["units"][1];
        for key in ["id", "handle", "text", "status", "evidence", "unresolved", "updated_round"] {
            assert!(unit.get(key).is_some(), "missing {key}");
        }
        assert_eq!(unit["handle"], "C2");
        assert_eq!(unit["text"], "寄出");
        assert_eq!(unit["status"], "planned");
        assert!(unit["updated_round"].is_null());
        assert!(v["last_report_round"].is_null());
        assert_eq!(v["invalid_reports"], 0);
    }
}
