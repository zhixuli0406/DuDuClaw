use super::*;

/// The holder rule shared by `tasks_complete` and `tasks_block`: an
/// AI-employee caller that is neither the task's `claimed_by` nor its
/// `assigned_to` must pass [`check_record_change_allowed`] against the
/// assignee (an unassigned, unclaimed task has no owner to authorize against
/// and is refused — claim it first). `Err` is the ready tool error.
pub(crate) async fn check_task_holder(
    store: &duduclaw_gateway::task_store::TaskStore,
    home_dir: &Path,
    task_id: &str,
    actor: RecordActor<'_>,
    tool: &str,
) -> std::result::Result<(), Value> {
    let Some(me) = actor.agent() else {
        return Ok(());
    };
    let task = match store.get_task(task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return Err(tool_error(&format!("task not found: {task_id}"))),
        Err(e) => return Err(tool_error(&format!("{tool}: {e}"))),
    };
    check_actor_identity(home_dir, actor, &task.assigned_to, tool).map_err(|r| tool_error(&r))?;
    if task.claimed_by.as_deref() == Some(me) || task.assigned_to == me {
        return Ok(());
    }
    check_record_change_allowed(home_dir, actor, &task.assigned_to, tool, RecordKind::Task)
        .await
        .map_err(|reason| tool_error(&reason))
}

/// Submit a task as done (goal-mode tasks go to judge review), subject to
/// [`check_task_holder`].
pub(crate) async fn handle_tasks_complete(
    args: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    let default_agent = actor.id();
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    if let Err(refusal) = check_task_holder(&store, home_dir, task_id, actor, "tasks_complete").await {
        return refusal;
    }
    // G1: `complete_task` routes goal-mode tasks to `review` (judge acceptance
    // pending) carrying the result summary; plain tasks go straight to `done`.
    // Also clears the lease so the completed task isn't reclaimed as a zombie.
    // HIGH-2: caller identity (default_agent — same identity tasks_renew uses)
    // is enforced against `claimed_by` in the store, so a reclaimed zombie
    // worker cannot clobber the new holder's in_progress task.
    let updated = match store.complete_task(task_id, summary, default_agent).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("complete task: {e}")),
    };
    let activity_summary = if updated.status == "review" {
        format!("Submitted for goal-mode review: {}", updated.title)
    } else if summary.is_empty() {
        format!("Completed: {}", updated.title)
    } else {
        format!("Completed: {} — {}", updated.title, summary)
    };
    append_activity(
        &store,
        "task_completed",
        default_agent,
        Some(task_id),
        &activity_summary,
        None,
    )
    .await;
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

/// Flag a task as blocked, subject to [`check_task_holder`].
pub(crate) async fn handle_tasks_block(
    args: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    let default_agent = actor.id();
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if reason.is_empty() {
        return tool_error("reason is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    if let Err(refusal) = check_task_holder(&store, home_dir, task_id, actor, "tasks_block").await {
        return refusal;
    }
    // HIGH-2 sweep: same holder guard as tasks_complete — a task claimed by X
    // may only be blocked by X (a reclaimed zombie must not flip the new
    // holder's in_progress task to blocked). Unclaimed tasks keep the current
    // behavior (any agent may flag a blocker on an unclaimed board task).
    match store.get_task(task_id).await {
        Ok(Some(t)) => {
            if let Some(holder) = t.claimed_by.as_deref() {
                if holder != default_agent {
                    return tool_error(&format!(
                        "task {task_id} is claimed by '{holder}'; only the claim holder may block it"
                    ));
                }
            }
        }
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("block task: {e}")),
    }
    let fields = serde_json::json!({
        "status": "blocked",
        "blocked_reason": reason,
    });
    let updated = match store.update_task(task_id, &fields).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("block task: {e}")),
    };
    append_activity(
        &store,
        "task_blocked",
        default_agent,
        Some(task_id),
        &format!("Blocked: {} — {}", updated.title, reason),
        None,
    )
    .await;
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

pub(crate) fn goal_row_to_json(row: &duduclaw_gateway::task_store::GoalRow) -> Value {
    serde_json::json!({
        "id": row.id,
        "title": row.title,
        "description": row.description,
        "parent_goal_id": row.parent_goal_id,
        "status": row.status,
        "created_at": row.created_at,
    })
}

/// G8: create a goal node (Initiative → Project → Issue hierarchy). Parent
/// existence + cycle rejection are enforced fail-closed in the store.
pub(crate) async fn handle_goals_create(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if title.is_empty() {
        return tool_error("title is required");
    }
    if title.len() > 200 {
        return tool_error("title must be <= 200 chars");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parent_goal_id = args
        .get("parent_goal_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let mut row = duduclaw_gateway::task_store::GoalRow::new(
        uuid::Uuid::new_v4().to_string(),
        title.to_string(),
        description,
    );
    row.parent_goal_id = parent_goal_id;
    if let Err(e) = store.insert_goal(&row).await {
        return tool_error(&format!("create goal: {e}"));
    }
    append_activity(
        &store,
        "goal_created",
        default_agent,
        None,
        &format!("Created goal: {}", row.title),
        None,
    )
    .await;
    tool_text(&serde_json::json!({ "goal": goal_row_to_json(&row) }).to_string())
}

pub(crate) async fn handle_goals_list(args: &Value, home_dir: &Path) -> Value {
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .filter(|s| matches!(*s, "active" | "done" | "archived"));
    let rows = match store.list_goals(status).await {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list goals: {e}")),
    };
    let limit = clamp_limit(args, 50, 200) as usize;
    let goals: Vec<Value> = rows.iter().take(limit).map(goal_row_to_json).collect();
    tool_text(
        &serde_json::json!({
            "goals": goals,
            "total": rows.len(),
        })
        .to_string(),
    )
}

// ── Cross-wake authoritative working state (D3 ghost-memory fix) ──────────
//
// Thin MCP fronts over `duduclaw_gateway::working_state` — validation, caps,
// CAS and the supersession chain all live in the gateway module (single
// source of truth shared with the prompt-injection builder). Store I/O is
// small bounded files; `spawn_blocking` keeps the advisory file lock off the
// async runtime.

/// Post an Activity Feed row. Naming a `task_id` adds activity to that task,
/// and the feed is what the goal loop's silent-progress reminder reads, so
/// an AI-employee caller that is not the task's assignee, claimer or creator
/// needs [`check_record_change_allowed`] against its assignee; an unknown
/// task id is refused.
pub(crate) async fn handle_activity_post(
    args: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    let default_agent = actor.id();
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if summary.is_empty() {
        return tool_error("summary is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let event_type = args
        .get("event_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("agent_comment")
        .to_string();
    // P2-A S-M6: these event types are written by the gateway only (stop
    // records, responsibility notices and their push counts).
    if duduclaw_gateway::responsibility::activity::RESERVED_PREFIXES
        .iter()
        .any(|p| event_type.to_ascii_lowercase().starts_with(p))
    {
        return tool_error(&format!(
            "activity_post 遭拒：「{}」是系統保留的事件類型。",
            duduclaw_core::truncate_chars(&event_type, 64)
        ));
    }
    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let metadata = args.get("metadata").map(|v| v.to_string());

    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    if let (Some(me), Some(tid)) = (actor.agent(), task_id.as_deref()) {
        let task = match store.get_task(tid).await {
            Ok(Some(t)) => t,
            Ok(None) => return tool_error(&format!("task not found: {tid}")),
            Err(e) => return tool_error(&format!("activity_post: {e}")),
        };
        if let Err(reason) = check_actor_identity(home_dir, actor, &task.assigned_to, "activity_post") {
            return tool_error(&reason);
        }
        let is_party = task.assigned_to == me
            || task.claimed_by.as_deref() == Some(me)
            || task.created_by == me;
        if !is_party {
            if let Err(reason) = check_record_change_allowed(
                home_dir,
                actor,
                &task.assigned_to,
                "activity_post",
                RecordKind::Task,
            )
            .await
            {
                return tool_error(&reason);
            }
        }
    }
    let row = duduclaw_gateway::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type,
        agent_id: default_agent.to_string(),
        task_id,
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    };
    if let Err(e) = store.append_activity(&row).await {
        return tool_error(&format!("append activity: {e}"));
    }
    append_bus_event(home_dir, "activity.new", &activity_row_to_json(&row)).await;
    tool_text(&serde_json::json!({ "activity": activity_row_to_json(&row) }).to_string())
}

/// List Activity Feed rows. An AI employee caller (`actor` = `Agent`) does not
/// get rows tied to a task whose audience it may not read (see
/// `agent_may_read_task`); operators are unrestricted.
pub(crate) async fn handle_activity_list(
    args: &Value,
    home_dir: &Path,
    default_agent: &str,
    actor: RecordActor<'_>,
) -> Value {
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let agent_id_raw = args.get("agent_id").and_then(|v| v.as_str());
    let agent_id: Option<&str> = match agent_id_raw {
        Some("*") => None,
        Some(s) if !s.is_empty() => Some(s),
        _ => Some(default_agent),
    };
    let event_type = args.get("event_type").and_then(|v| v.as_str());
    let task_id_filter = args.get("task_id").and_then(|v| v.as_str());
    let limit = clamp_limit(args, 20, 100);

    let (rows, total) = match store.list_activity(agent_id, event_type, limit, 0).await {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list activity: {e}")),
    };
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| match task_id_filter {
            Some(t) => r.task_id.as_deref() == Some(t),
            None => true,
        })
        .collect();
    let rows = match actor.agent() {
        Some(viewer) => activity_rows_visible_to(&store, home_dir, viewer, rows).await,
        None => rows,
    };
    let items: Vec<Value> = rows.iter().map(activity_row_to_json).collect();
    tool_text(
        &serde_json::json!({
            "activities": items,
            "total": total,
        })
        .to_string(),
    )
}

/// Drop activity rows tied to a task the employee `viewer` may not read. One
/// audience read per distinct task; the task row is looked up only when the
/// audience limits it. A row the viewer wrote itself stays. A limited task
/// that no longer exists has no owner, so only a list naming the viewer
/// lets its rows through (fail closed).
async fn activity_rows_visible_to(
    store: &duduclaw_gateway::task_store::TaskStore,
    home_dir: &Path,
    viewer: &str,
    rows: Vec<duduclaw_gateway::task_store::ActivityRow>,
) -> Vec<duduclaw_gateway::task_store::ActivityRow> {
    use duduclaw_gateway::review_evidence::audience::{
        TaskAudience, agent_may_read_task, task_audience, task_owners,
    };
    let mut verdicts: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(task_id) = row.task_id.clone() else {
            out.push(row);
            continue;
        };
        if row.agent_id == viewer {
            out.push(row);
            continue;
        }
        let allowed = match verdicts.get(&task_id) {
            Some(v) => *v,
            None => {
                let audience = task_audience(home_dir, &task_id);
                let v = match &audience {
                    TaskAudience::Open => true,
                    _ => match store.get_task(&task_id).await {
                        Ok(Some(task)) => agent_may_read_task(viewer, &task_owners(&task), &audience),
                        _ => agent_may_read_task(viewer, &[], &audience),
                    },
                };
                verdicts.insert(task_id, v);
                v
            }
        };
        if allowed {
            out.push(row);
        }
    }
    out
}

// ── Co-edited plan tools (U4) ───────────────────────────────────
//
// The shared plan is co-edited: the user edits from the dashboard
// (`plans.*` RPCs), the agent reads it with `plan_get` and ticks its own
// steps with `plan_update_step`. Holder rule (fail-closed): an agent may
// only update steps with `assignee_kind == "agent"` AND `assignee == caller`.

pub(crate) fn plan_row_to_json(row: &duduclaw_gateway::task_store::PlanRow) -> Value {
    serde_json::json!({
        "id": row.id,
        "title": row.title,
        "description": row.description,
        "agent_id": row.agent_id,
        "goal_id": row.goal_id,
        "status": row.status,
        "created_by": row.created_by,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

pub(crate) fn plan_step_row_to_json(row: &duduclaw_gateway::task_store::PlanStepRow) -> Value {
    serde_json::json!({
        "id": row.id,
        "plan_id": row.plan_id,
        "text": row.text,
        "assignee_kind": row.assignee_kind,
        "assignee": row.assignee,
        "status": row.status,
        "step_order": row.step_order,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

pub(crate) async fn handle_plan_get(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let plan = match args
        .get("plan_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(plan_id) => match store.get_plan(plan_id).await {
            Ok(Some(p)) => p,
            Ok(None) => return tool_error(&format!("plan not found: {plan_id}")),
            Err(e) => return tool_error(&format!("get plan: {e}")),
        },
        None => {
            // Default: the caller's most recently updated active plan
            // (list_plans orders newest-activity-first).
            match store.list_plans(Some(default_agent), Some("active")).await {
                Ok(plans) => match plans.into_iter().next() {
                    Some(p) => p,
                    None => {
                        return tool_text(
                            &serde_json::json!({
                                "plan": Value::Null,
                                "steps": [],
                                "note": "no active shared plan for this agent",
                            })
                            .to_string(),
                        );
                    }
                },
                Err(e) => return tool_error(&format!("list plans: {e}")),
            }
        }
    };
    let steps = match store.list_plan_steps(&plan.id).await {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("list plan steps: {e}")),
    };
    tool_text(
        &serde_json::json!({
            "plan": plan_row_to_json(&plan),
            "steps": steps.iter().map(plan_step_row_to_json).collect::<Vec<_>>(),
        })
        .to_string(),
    )
}

pub(crate) async fn handle_plan_update_step(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let step_id = args.get("step_id").and_then(|v| v.as_str()).unwrap_or("");
    if step_id.is_empty() {
        return tool_error("step_id is required");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let step = match store.get_plan_step(step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return tool_error(&format!("plan step not found: {step_id}")),
        Err(e) => return tool_error(&format!("get plan step: {e}")),
    };
    // Holder rule (fail-closed): agents may only update agent-kind steps
    // explicitly assigned to THEM. A user's step, another agent's step, or an
    // unassigned step is off-limits — the user owns the shared plan's shape.
    if step.assignee_kind != "agent" || step.assignee != default_agent {
        return tool_error(&format!(
            "permission denied: step {step_id} is assigned to {} '{}' — you may only update your own agent steps",
            step.assignee_kind,
            if step.assignee.is_empty() {
                "(unassigned)"
            } else {
                &step.assignee
            },
        ));
    }
    // Whitelisted fields only: status (validated in the store) and text.
    let mut fields = serde_json::Map::new();
    for k in ["status", "text"] {
        if let Some(v) = args.get(k) {
            fields.insert(k.into(), v.clone());
        }
    }
    if fields.is_empty() {
        return tool_error("no fields to update (pass status and/or text)");
    }
    let updated = match store
        .update_plan_step(step_id, &Value::Object(fields))
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) => return tool_error(&format!("plan step not found: {step_id}")),
        Err(e) => return tool_error(&format!("update plan step: {e}")),
    };
    // Co-editing timeline: surface the agent's tick in the Activity Feed.
    let plan_title = store
        .get_plan(&updated.plan_id)
        .await
        .ok()
        .flatten()
        .map(|p| p.title)
        .unwrap_or_else(|| updated.plan_id.clone());
    append_activity(
        &store,
        "plan_step_updated",
        default_agent,
        None,
        &format!(
            "{} updated a plan step in {}: {} [{}]",
            default_agent,
            plan_title,
            duduclaw_core::truncate_chars(&updated.text, 80),
            updated.status,
        ),
        Some(serde_json::json!({ "plan_id": updated.plan_id }).to_string()),
    )
    .await;
    append_bus_event(
        home_dir,
        "plan.updated",
        &serde_json::json!({ "plan_id": updated.plan_id, "agent_id": default_agent }),
    )
    .await;
    tool_text(&serde_json::json!({ "step": plan_step_row_to_json(&updated) }).to_string())
}
