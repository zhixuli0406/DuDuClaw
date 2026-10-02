use super::*;

pub(crate) fn task_row_to_json(row: &duduclaw_gateway::task_store::TaskRow) -> Value {
    let tags: Vec<&str> = row
        .tags
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    serde_json::json!({
        "id": row.id,
        "kind": row.kind,
        "discovery_run_id": row.discovery_run_id,
        "title": row.title,
        "description": row.description,
        "status": row.status,
        "priority": row.priority,
        "assigned_to": row.assigned_to,
        "created_by": row.created_by,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "completed_at": row.completed_at,
        "blocked_reason": row.blocked_reason,
        "parent_task_id": row.parent_task_id,
        "tags": tags,
        "message_id": row.message_id,
        "claimed_by": row.claimed_by,
        "lease_expires_at": row.lease_expires_at,
        "lease_renewed_at": row.lease_renewed_at,
        "goal_id": row.goal_id,
        "depends_on": duduclaw_gateway::task_store::parse_depends_on(&row.depends_on),
        "retry_count": row.retry_count,
        "max_retries": row.max_retries,
        "goal_mode": row.goal_mode,
        "acceptance_criteria": row.acceptance_criteria,
        // H9-G goal contract freeze: the immutable snapshot taken at goal
        // creation, when one exists (see `TaskRow::acceptance_criteria_baseline`).
        "acceptance_criteria_baseline": row.acceptance_criteria_baseline,
        "result_summary": row.result_summary,
        "judge_feedback": row.judge_feedback,
        // Iterative Kanban (v1.45): revision-round cache + agent clock.
        "revision_round": row.revision_round,
        "diminishing": row.diminishing,
        "agent_seconds": row.agent_seconds,
    })
}

pub(crate) fn activity_row_to_json(row: &duduclaw_gateway::task_store::ActivityRow) -> Value {
    serde_json::json!({
        "id": row.id,
        "type": row.event_type,
        "agent_id": row.agent_id,
        "task_id": row.task_id,
        "summary": row.summary,
        "timestamp": row.timestamp,
        "metadata": row.metadata,
    })
}

pub(crate) async fn append_activity(
    store: &duduclaw_gateway::task_store::TaskStore,
    event_type: &str,
    agent_id: &str,
    task_id: Option<&str>,
    summary: &str,
    metadata: Option<String>,
) {
    let row = duduclaw_gateway::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    };
    let _ = store.append_activity(&row).await;
}

pub(crate) fn clamp_limit(args: &Value, default: i64, max: i64) -> i64 {
    let n = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(default);
    n.max(1).min(max)
}

pub(crate) async fn handle_tasks_list(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let status = args.get("status").and_then(|v| v.as_str());
    let priority = args.get("priority").and_then(|v| v.as_str());
    let assigned_to_raw = args.get("assigned_to").and_then(|v| v.as_str());
    // Default to caller; "*" means all agents
    let assigned_to: Option<&str> = match assigned_to_raw {
        Some("*") => None,
        Some(s) if !s.is_empty() => Some(s),
        _ => Some(default_agent),
    };

    let rows = match store.list_tasks(status, assigned_to, priority).await {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list tasks: {e}")),
    };
    let limit = clamp_limit(args, 20, 100) as usize;
    let tasks: Vec<Value> = rows.iter().take(limit).map(task_row_to_json).collect();
    tool_text(
        &serde_json::json!({
            "tasks": tasks,
            "total": rows.len(),
            "filtered_by_agent": assigned_to,
        })
        .to_string(),
    )
}

/// T5/O4 — the one agent-facing entry point for "create work".
///
/// `kind` chooses the object (`task` = Kanban board row, the default and
/// byte-identical to every pre-merge call; `goal` = an autonomous goal on the
/// shared `goal_create_core` path, contract freeze and `plan_first` included)
/// and `schedule` chooses the rail (a cron expression registers a recurring
/// job exactly as `schedule_task` always did; an RFC3339 instant registers a
/// one-shot reminder in `agent_callback` mode).
///
/// **The WP21 C3 delegation gate is enforced once, here**, before any branch
/// runs — that is the point of merging the entry points: a caller can no
/// longer launder a cross-department assignment through whichever of the four
/// old tools happened to check least.
pub(crate) async fn handle_tasks_create(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let kind = match crate::mcp_alias::resolve_task_kind(args) {
        Ok(k) => k,
        Err(e) => return tool_error(&e),
    };
    if kind == crate::mcp_alias::TaskKind::Discovery {
        if args.get("schedule").is_some_and(|value| !value.is_null()) {
            return tool_error("discovery cannot be scheduled through ordinary task workers");
        }
        return handle_discovery_create(args, home_dir, default_agent).await;
    }
    let schedule = match args.get("schedule") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(raw)) => match crate::mcp_alias::classify_schedule(raw) {
            Ok(spec) => Some(spec),
            Err(e) => return tool_error(&e),
        },
        Some(_) => return tool_error("schedule must be a string (cron expression or RFC3339 instant)"),
    };
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
    // A goal is driven to completion by the goal loop; a schedule registers a
    // recurring/deferred wake-up. Combining them would silently create N
    // independent goals, so it is refused rather than guessed at.
    if kind == crate::mcp_alias::TaskKind::Goal && schedule.is_some() {
        return tool_error(
            "kind=\"goal\" cannot be combined with schedule — a goal runs to completion once; \
             schedule a plain task (kind=\"task\") that creates goals instead",
        );
    }
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let priority = args
        .get("priority")
        .and_then(|v| v.as_str())
        .filter(|p| matches!(*p, "low" | "medium" | "high" | "urgent"))
        .unwrap_or("medium")
        .to_string();
    let assigned_to = args
        .get("assigned_to")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent)
        .to_string();
    // Reject malformed / wildcard agent ids — `assigned_to` is stored verbatim
    // and later used as an equality filter, so an invalid value produces
    // tasks nobody can query.
    if !is_valid_agent_id(&assigned_to) {
        return tool_error(&format!(
            "assigned_to must be a valid agent id (lowercase alphanumeric + hyphens), got: {assigned_to}"
        ));
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    // ── WP21 C3: department × hierarchy delegation gate ────────
    // Assigning to yourself never invokes the predicate (self-delegation is a
    // hard DENY under every policy in core) — this mirrors tasks_claim, which
    // is unconditionally allowed. System senders (dashboard/heartbeat/...) pass
    // through `check_delegation_allowed` unaffected: the core predicate ALLOWs
    // them before it ever consults the org tree.
    if assigned_to != default_agent {
        if let Err(reason) =
            check_delegation_allowed(home_dir, default_agent, &assigned_to, "tasks_create").await
        {
            return tool_error(&reason);
        }
    }

    // ── O4 branch 1: kind="goal" ─────────────────────────────────────────
    // The SAME function the dashboard `tasks.goal_create` RPC calls — the
    // H9-G acceptance-contract freeze, the structured-outcome parse, the
    // per-goal wall clock / risk boundary and the I-1c plan-first parking all
    // come from there, never from a second copy that could drift.
    if kind == crate::mcp_alias::TaskKind::Goal {
        let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
            Ok(s) => s,
            Err(e) => return tool_error(&format!("open task store: {e}")),
        };
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            // A goal's description IS the contract; when the caller gave only
            // a title, that title is the goal text (same rule as `/goal`).
            .unwrap_or_else(|| title.to_string());
        let req = duduclaw_gateway::goal_create_core::GoalCreateRequest {
            agent_id: assigned_to.clone(),
            created_by: default_agent.to_string(),
            description,
            acceptance_criteria: args
                .get("acceptance_criteria")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            priority: args
                .get("priority")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            outcome: args.get("outcome").and_then(|v| v.as_str()).map(str::to_string),
            duration_hours: args.get("duration_hours").and_then(|v| v.as_f64()),
            risk_boundary: args
                .get("risk_boundary")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            require_beliefs: args
                .get("require_beliefs")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            plan_first: args
                .get("plan_first")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            source_label: String::new(),
        };
        return match duduclaw_gateway::goal_create_core::create_goal_task(home_dir, &store, req)
            .await
        {
            Ok(created) => {
                append_bus_event(home_dir, "task.created", &task_row_to_json(&created.task)).await;
                tool_text(
                    &serde_json::json!({
                        "task": task_row_to_json(&created.task),
                        "kind": "goal",
                        "plan_first": created.plan_first,
                    })
                    .to_string(),
                )
            }
            Err(e) => tool_error(&e),
        };
    }

    // ── O4 branch 2: schedule ────────────────────────────────────────────
    // Delegated verbatim to the rails that already own these objects, so the
    // deprecated `schedule_task` alias and this merged entry produce the same
    // rows. A cron expression is recurring agent work; an RFC3339 instant is a
    // one-shot wake-up, which the platform models as a reminder (cron rows
    // cannot express "once", and pinning a date into a cron field would
    // silently fire again next year).
    if let Some(spec) = schedule {
        let prompt = args
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(title);
        return match spec {
            crate::mcp_alias::ScheduleSpec::Cron(cron) => {
                let synth = serde_json::json!({
                    "cron": cron,
                    "task": prompt,
                    "name": title,
                    "agent_id": assigned_to,
                    "notify_channel": args.get("notify_channel"),
                    "notify_chat_id": args.get("notify_chat_id"),
                    "notify_thread_id": args.get("notify_thread_id"),
                    "cron_timezone": args.get("cron_timezone"),
                });
                handle_schedule_task(&synth, home_dir, default_agent).await
            }
            crate::mcp_alias::ScheduleSpec::Once(at) => {
                // The one-shot rail needs somewhere to deliver the result —
                // it runs detached, long after this call returned, so there is
                // no ambient conversation to answer into. Say so instead of
                // creating a reminder that can never be delivered.
                if args.get("notify_channel").and_then(|v| v.as_str()).is_none()
                    || args.get("notify_chat_id").and_then(|v| v.as_str()).is_none()
                {
                    return tool_error(
                        "a one-shot schedule (RFC3339 instant) also needs notify_channel and \
                         notify_chat_id — the wake-up runs detached and has nowhere to reply",
                    );
                }
                let synth = serde_json::json!({
                    "time": at,
                    "mode": "agent_callback",
                    "prompt": prompt,
                    "agent_id": assigned_to,
                    "channel": args.get("notify_channel"),
                    "chat_id": args.get("notify_chat_id"),
                });
                handle_create_reminder(&synth, home_dir, default_agent).await
            }
        };
    }

    let tags = args
        .get("tags")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parent_task_id = args
        .get("parent_task_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };

    let mut row = duduclaw_gateway::task_store::TaskRow::new(
        uuid::Uuid::new_v4().to_string(),
        title.to_string(),
        description,
        priority,
        assigned_to.clone(),
        default_agent.to_string(),
    );
    row.tags = tags;
    row.parent_task_id = parent_task_id;

    // G1 durable dispatch options. `depends_on` accepts a JSON array or a
    // comma-separated list of task ids; when any dispatch option is present the
    // task enters the durable `pending` lifecycle instead of the board `todo`.
    let depends_on: Vec<String> = match args.get("depends_on") {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(String::from)
            .collect(),
        Some(serde_json::Value::String(s)) => s
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    };
    let goal_mode = args
        .get("goal_mode")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let acceptance_criteria = args
        .get("acceptance_criteria")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let durable = args
        .get("durable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !depends_on.is_empty() {
        // Fail-closed dependency validation: every dep must exist (an unknown
        // id would gate the task forever) and must not close a cycle.
        for dep in &depends_on {
            match store.get_task(dep).await {
                Ok(Some(_)) => {}
                Ok(None) => return tool_error(&format!("depends_on task not found: {dep}")),
                Err(e) => return tool_error(&format!("validate depends_on: {e}")),
            }
        }
        let edges = match store.depends_edges().await {
            Ok(e) => e,
            Err(e) => return tool_error(&format!("validate depends_on: {e}")),
        };
        if duduclaw_gateway::task_store::introduces_dependency_cycle(&edges, &row.id, &depends_on) {
            return tool_error(
                "dependency cycle rejected: the task would (transitively) depend on itself",
            );
        }
        row.depends_on = serde_json::to_string(&depends_on).unwrap_or_else(|_| "[]".into());
    }
    // G8: link the task to a goal. Fail-closed — the goal must exist so the
    // why-chain injection never dangles.
    if let Some(goal_id) = args
        .get("goal_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        match store.get_goal(goal_id).await {
            Ok(Some(_)) => row.goal_id = Some(goal_id.to_string()),
            Ok(None) => return tool_error(&format!("goal not found: {goal_id}")),
            Err(e) => return tool_error(&format!("validate goal_id: {e}")),
        }
    }
    if goal_mode {
        row.goal_mode = true;
        row.acceptance_criteria = acceptance_criteria;
    }
    if let Some(mr) = args.get("max_retries").and_then(|v| v.as_i64()) {
        row.max_retries = mr.clamp(0, 100);
    }
    // Durable lifecycle when any dispatch feature is requested.
    if durable || goal_mode || !depends_on.is_empty() {
        row.status = "pending".into();
    }

    if let Err(e) = store.insert_task(&row).await {
        return tool_error(&format!("insert task: {e}"));
    }

    // Record activity
    append_activity(
        &store,
        "task_created",
        default_agent,
        Some(&row.id),
        &format!("Created task: {}", row.title),
        None,
    )
    .await;
    // If the task was assigned to someone else, also record task_assigned
    if assigned_to != default_agent {
        append_activity(
            &store,
            "task_assigned",
            default_agent,
            Some(&row.id),
            &format!("Assigned to {}: {}", assigned_to, row.title),
            None,
        )
        .await;
    }

    append_bus_event(home_dir, "task.created", &task_row_to_json(&row)).await;

    tool_text(&serde_json::json!({ "task": task_row_to_json(&row) }).to_string())
}

pub(crate) async fn handle_tasks_update(args: &Value, home_dir: &Path, caller: &str) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    // ── H9-G goal contract freeze (harness-borrowings 2026-08 WP-D) ──────
    // An agent-identity caller may never modify the acceptance criteria of a
    // goal_mode task through this MCP tool — the contract is frozen into
    // `acceptance_criteria_baseline` at goal-creation time and the judge
    // reads that column, never this one. Only the dashboard/operator RPC
    // path (`handlers.rs::handle_tasks_update`, Operator-ACL-gated) may still
    // edit the mutable `acceptance_criteria` copy. Checked BEFORE building
    // `fields` so the whole call fails closed (never silently drops just
    // this one key and proceeds with the rest) — same fail-closed posture as
    // every other security gate in this file (coding convention 4).
    if args.get("acceptance_criteria").is_some() {
        let target_is_goal_mode = store
            .get_task(task_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.goal_mode)
            .unwrap_or(false);
        if target_is_goal_mode {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                caller,
                "tasks_update",
                &format!(
                    "denied: agent attempted to modify frozen acceptance_criteria on goal_mode task {task_id}"
                ),
                false,
                &[
                    ("task_id", serde_json::json!(task_id)),
                    ("field", serde_json::json!("acceptance_criteria")),
                    ("reason", serde_json::json!("goal_contract_frozen")),
                ],
            );
            return tool_error(
                "acceptance_criteria on a goal_mode task is frozen at creation time; only an \
                 operator can change it, from the dashboard — not via this tool",
            );
        }
    }
    // Build fields map — only pass through allowed fields
    let mut fields = serde_json::Map::new();
    for k in ["title", "description", "priority", "tags"] {
        if let Some(v) = args.get(k) {
            fields.insert(k.into(), v.clone());
        }
    }
    // ── WP21 C3: reassignment goes through the same delegation gate as
    // tasks_create. `tasks_claim` remains the unconditional self-take path —
    // this branch only fires when the caller is reassigning to *someone else*.
    if let Some(new_owner) = args.get("assigned_to").and_then(|v| v.as_str()) {
        let new_owner = new_owner.trim();
        if !is_valid_agent_id(new_owner) {
            return tool_error(&format!(
                "assigned_to must be a valid agent id (lowercase alphanumeric + hyphens), got: {new_owner}"
            ));
        }
        if new_owner != caller {
            if let Err(reason) =
                check_delegation_allowed(home_dir, caller, new_owner, "tasks_update").await
            {
                return tool_error(&reason);
            }
        }
        fields.insert(
            "assigned_to".into(),
            serde_json::Value::String(new_owner.to_string()),
        );
    }
    // depends_on rewiring: accept JSON array or comma-separated ids, verify
    // every dep exists (fail-closed), and normalize to the stored JSON form.
    // The store itself rejects dependency cycles (`introduces_dependency_cycle`).
    if let Some(deps_val) = args.get("depends_on") {
        let deps: Vec<String> = match deps_val {
            serde_json::Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect(),
            serde_json::Value::String(s) => s
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            _ => return tool_error("depends_on must be a JSON array or comma-separated ids"),
        };
        for dep in &deps {
            match store.get_task(dep).await {
                Ok(Some(_)) => {}
                Ok(None) => return tool_error(&format!("depends_on task not found: {dep}")),
                Err(e) => return tool_error(&format!("validate depends_on: {e}")),
            }
        }
        let deps_json = serde_json::to_string(&deps).unwrap_or_else(|_| "[]".into());
        fields.insert("depends_on".into(), serde_json::Value::String(deps_json));
    }
    if fields.is_empty() {
        return tool_error("no fields to update");
    }
    let updated = match store.update_task(task_id, &Value::Object(fields)).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("update task: {e}")),
    };
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    tool_text(&serde_json::json!({ "task": task_row_to_json(&updated) }).to_string())
}

pub(crate) async fn handle_tasks_claim(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };

    // G1 durable claim: try the atomic compare-and-set first (only one worker
    // can win a `pending` task, and the claim stamps a lease so a crashed worker
    // is reclaimable). Fall back to the legacy unconditional claim for
    // pre-G1 board tasks (`todo` status, no lease) so existing flows keep working.
    let now = chrono::Utc::now();
    let now_s = now.to_rfc3339();
    let lease = (now
        + chrono::Duration::seconds(duduclaw_gateway::dispatch_engine::DEFAULT_LEASE_SECS))
    .to_rfc3339();
    match store
        .atomic_claim(task_id, default_agent, &now_s, &lease)
        .await
    {
        Ok(duduclaw_gateway::task_store::ClaimOutcome::Claimed) => {}
        Ok(duduclaw_gateway::task_store::ClaimOutcome::BlockedByDeps(unmet)) => {
            // Dependency gate is enforced inside the claim transaction itself
            // (fail-closed); surface the unmet ids so the agent knows what to
            // wait for instead of retrying blindly.
            return tool_error(&format!(
                "task not claimable: {task_id} is blocked by unfinished dependencies [{}] — claim them first or wait until they are done",
                unmet.join(", ")
            ));
        }
        Ok(duduclaw_gateway::task_store::ClaimOutcome::NotClaimable) => {
            // Not a claimable durable (`pending`, unclaimed) task. Distinguish
            // "already taken" from "legacy board task" so we don't silently steal.
            // Legacy fallback is restricted to UNASSIGNED todo tasks (or ones
            // already assigned to the caller) — a todo task assigned to another
            // agent must not be silently re-assigned to the claimer.
            match store.get_task(task_id).await {
                Ok(Some(t))
                    if t.status == "todo"
                        && (t.assigned_to.is_empty() || t.assigned_to == default_agent) =>
                {
                    let fields = serde_json::json!({
                        "assigned_to": default_agent,
                        "status": "in_progress",
                    });
                    if let Err(e) = store.update_task(task_id, &fields).await {
                        return tool_error(&format!("claim task: {e}"));
                    }
                }
                Ok(Some(t)) => {
                    return tool_error(&format!(
                        "task not claimable: {task_id} is '{}' (assigned_to={:?}, claimed_by={:?})",
                        t.status, t.assigned_to, t.claimed_by
                    ));
                }
                Ok(None) => return tool_error(&format!("task not found: {task_id}")),
                Err(e) => return tool_error(&format!("claim task: {e}")),
            }
        }
        Err(e) => return tool_error(&format!("claim task: {e}")),
    }
    let updated = match store.get_task(task_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return tool_error(&format!("task not found: {task_id}")),
        Err(e) => return tool_error(&format!("claim task: {e}")),
    };
    append_activity(
        &store,
        "task_assigned",
        default_agent,
        Some(task_id),
        &format!("{} claimed task: {}", default_agent, updated.title),
        None,
    )
    .await;
    append_bus_event(home_dir, "task.updated", &task_row_to_json(&updated)).await;
    // G1: leased claims must heartbeat — tell the agent explicitly, in the
    // claim response itself, or a long task gets reclaimed as a zombie.
    let mut resp = serde_json::json!({ "task": task_row_to_json(&updated) });
    if updated.lease_expires_at.is_some() {
        resp["lease_note"] = serde_json::Value::String(
            "This claim is leased. For long-running work, call tasks_renew (same task_id) every few minutes; an unrenewed lease expires and the task is reclaimed and re-dispatched.".to_string(),
        );
    }
    tool_text(&resp.to_string())
}

/// G1: explicit lease heartbeat for external agent processes that claimed a
/// task via `tasks_claim`. Extends the lease by one full window; only the
/// claiming agent can renew (`claimed_by` guard in the store — fail-closed).
pub(crate) async fn handle_tasks_renew(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return tool_error("task_id is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open task store: {e}")),
    };
    let now = chrono::Utc::now();
    let new_expiry = (now
        + chrono::Duration::seconds(duduclaw_gateway::dispatch_engine::DEFAULT_LEASE_SECS))
    .to_rfc3339();
    match store
        .renew_lease(task_id, default_agent, &new_expiry, &now.to_rfc3339())
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return tool_error(&format!(
                "lease not renewable: {task_id} is not an in_progress task claimed by {default_agent}"
            ));
        }
        Err(e) => return tool_error(&format!("renew lease: {e}")),
    }
    match store.get_task(task_id).await {
        Ok(Some(t)) => tool_text(&serde_json::json!({ "task": task_row_to_json(&t) }).to_string()),
        Ok(None) => tool_error(&format!("task not found: {task_id}")),
        Err(e) => tool_error(&format!("renew lease: {e}")),
    }
}
