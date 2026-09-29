use super::*;

/// Check responses from a specific agent in the bus queue.
///
/// Reads bus_queue.jsonl and SQLite message_queue for `agent_response` entries
/// from the specified agent, returning the most recent ones.
pub(crate) async fn handle_check_responses(params: &Value, home_dir: &Path) -> Value {
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return mcp_error("'agent_id' is required"),
    };
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let mut responses: Vec<(String, String, usize)> = Vec::new(); // (timestamp, payload_preview, full_len)

    // 1. Check JSONL bus queue
    let queue_path = home_dir.join("bus_queue.jsonl");
    if let Ok(content) = std::fs::read_to_string(&queue_path) {
        for line in content.lines().rev() {
            if responses.len() >= limit {
                break;
            }
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                let is_response =
                    msg.get("type").and_then(|v| v.as_str()) == Some("agent_response");
                let matches_agent = msg.get("agent_id").and_then(|v| v.as_str()) == Some(agent_id);
                if is_response && matches_agent {
                    let ts = msg
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let payload = msg.get("payload").and_then(|v| v.as_str()).unwrap_or("");
                    let full_len = payload.len();
                    let preview: String = payload.chars().take(500).collect();
                    responses.push((ts, preview, full_len));
                }
            }
        }
    }

    // 2. Check SQLite message queue
    let db_path = home_dir.join("message_queue.db");
    if db_path.exists()
        && let Ok(conn) = rusqlite::Connection::open(&db_path)
    {
        let _ = conn.execute_batch("PRAGMA busy_timeout=3000;");
        if let Ok(mut stmt) = conn.prepare(
            "SELECT created_at, substr(response, 1, 500), length(response), status \
             FROM message_queue WHERE target = ?1 AND status = 'done' AND response IS NOT NULL \
             ORDER BY created_at DESC LIMIT ?2",
        ) && let Ok(rows) = stmt.query_map(rusqlite::params![agent_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as usize,
            ))
        }) {
            for row in rows.flatten() {
                if responses.len() < limit {
                    responses.push(row);
                }
            }
        }
    }

    if responses.is_empty() {
        return mcp_text(&format!(
            "No responses found from agent '{agent_id}'. The agent may not have \
             responded yet, or its responses may have expired from the queue."
        ));
    }

    let mut report = format!(
        "Found {} response(s) from agent '{agent_id}':\n",
        responses.len()
    );
    for (i, (ts, preview, full_len)) in responses.iter().enumerate() {
        let truncated = if *full_len > 500 {
            format!(" [truncated, full={full_len} chars]")
        } else {
            String::new()
        };
        report.push_str(&format!(
            "\n--- Response {} ({ts}){truncated} ---\n{preview}\n",
            i + 1
        ));
    }

    mcp_text(&report)
}

/// Create a structured multi-step task for deterministic execution by the dispatcher.
///
/// The task is persisted as a TaskSpec JSON file; the gateway dispatcher picks it
/// up on its next poll cycle and executes steps sequentially with retry/replan.
pub(crate) async fn handle_create_task(params: &Value, home_dir: &Path, caller: &str) -> Value {
    // The TaskSpec dispatcher scans registered agents only. An ephemeral
    // member's task file would never be picked up, so refuse it explicitly;
    // team role work crosses stages through team_handoff instead.
    if duduclaw_gateway::ephemeral::is_ephemeral_id(caller) {
        return mcp_error("ephemeral members cannot create TaskSpec tasks; use team_handoff");
    }
    let goal = match params.get("goal").and_then(|v| v.as_str()) {
        Some(g) if !g.is_empty() => g,
        _ => return mcp_error("'goal' is required"),
    };

    let steps_raw = match params.get("steps") {
        Some(v) => v,
        None => return mcp_error("'steps' is required (JSON array)"),
    };

    // Parse steps — accept either a JSON array directly or a JSON string.
    let steps_array = if let Some(arr) = steps_raw.as_array() {
        arr.clone()
    } else if let Some(s) = steps_raw.as_str() {
        match serde_json::from_str::<Vec<serde_json::Value>>(s) {
            Ok(arr) => arr,
            Err(e) => return mcp_error(&format!("failed to parse steps JSON: {e}")),
        }
    } else {
        return mcp_error("'steps' must be a JSON array or JSON string");
    };

    if steps_array.is_empty() {
        return mcp_error("'steps' must contain at least one step");
    }

    // Convert raw JSON to Step structs.
    use duduclaw_gateway::task_spec::{Criterion, Step, StepStatus, VerificationMethod};
    let mut steps = Vec::with_capacity(steps_array.len());
    for (i, raw) in steps_array.iter().enumerate() {
        let description = raw
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if description.is_empty() {
            return mcp_error(&format!("step {i} missing 'description'"));
        }

        let agent = raw
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // ── WP21 C3 (create_task front door) ─────────────────────────────
        // A step naming another agent is a delegation: the gateway's TaskSpec
        // executor spawns that agent directly. It is gated there too (the real
        // choke point, `execute_task_spec`); this check just refuses the plan
        // up front with a message the caller can act on, instead of letting it
        // fail one step at a time.
        if !agent.is_empty() && agent != caller {
            if !is_valid_agent_id(&agent) {
                return mcp_error(&format!("step {i} has an invalid 'agent' id"));
            }
            if let Err(reason) =
                check_delegation_allowed(home_dir, caller, &agent, "create_task").await
            {
                return mcp_error(&format!("step {i}: {reason}"));
            }
        }

        let depends_on: Vec<u8> = raw
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u8))
                    .collect()
            })
            .unwrap_or_default();

        let acceptance_criteria: Vec<Criterion> = raw
            .get("acceptance_criteria")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.get("description")
                            .and_then(|d| d.as_str())
                            .map(|desc| Criterion {
                                description: desc.to_string(),
                                method: VerificationMethod::Auto,
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();

        steps.push(Step {
            id: i as u8,
            description,
            agent,
            depends_on,
            acceptance_criteria,
            status: StepStatus::Pending,
            result: None,
            retry_count: 0,
        });
    }

    // Create TaskSpec and persist it.
    use duduclaw_gateway::task_spec::TaskSpec;
    let spec = TaskSpec::new(caller, goal, steps);
    let task_id = spec.task_id.clone();
    let step_count = spec.steps.len();

    let agent_dir = home_dir.join("agents").join(caller);
    if let Err(e) = spec.save(&agent_dir) {
        return mcp_error(&format!("failed to save task: {e}"));
    }

    // Write a signal to bus_queue.jsonl so the dispatcher picks up the new task.
    let signal = serde_json::json!({
        "type": "task_created",
        "task_id": task_id,
        "agent_id": caller,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    let queue_path = home_dir.join("bus_queue.jsonl");
    if let Ok(line) = serde_json::to_string(&signal) {
        let _ = tokio::task::spawn_blocking(move || {
            append_to_jsonl_sync(&queue_path, &line);
        })
        .await;
    }

    mcp_text(&format!(
        "Task created: id={task_id}, steps={step_count}, status=planned. \
         The gateway dispatcher will execute steps automatically."
    ))
}

/// Check the status of a previously created task.
pub(crate) async fn handle_task_status(params: &Value, home_dir: &Path, caller: &str) -> Value {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return mcp_error("'task_id' is required"),
    };

    use duduclaw_gateway::task_spec::TaskSpec;
    let agent_dir = caller_agent_dir(home_dir, caller);

    match TaskSpec::load(&agent_dir, task_id) {
        Ok(spec) => {
            let passed = spec
                .steps
                .iter()
                .filter(|s| s.status == duduclaw_gateway::task_spec::StepStatus::Passed)
                .count();
            let failed = spec
                .steps
                .iter()
                .filter(|s| s.status == duduclaw_gateway::task_spec::StepStatus::Failed)
                .count();
            let pending = spec
                .steps
                .iter()
                .filter(|s| s.status == duduclaw_gateway::task_spec::StepStatus::Pending)
                .count();

            let mut report = format!(
                "Task: {}\nGoal: {}\nStatus: {:?}\nSteps: {} total, {} passed, {} failed, {} pending\n",
                spec.task_id,
                spec.goal,
                spec.status,
                spec.steps.len(),
                passed,
                failed,
                pending
            );

            for step in &spec.steps {
                report.push_str(&format!(
                    "\n  [{}] {:?} — {}{}",
                    step.id,
                    step.status,
                    step.description,
                    if let Some(ref result) = step.result {
                        // HC3: byte-slice would panic on a multi-byte UTF-8
                        // boundary (CJK). truncate_bytes walks back to a char
                        // boundary ≤ 200 bytes.
                        format!("\n      Output: {}...", truncate_bytes(&result.output, 200))
                    } else {
                        String::new()
                    }
                ));
            }

            mcp_text(&report)
        }
        Err(e) => mcp_error(&format!("failed to load task '{task_id}': {e}")),
    }
}

/// Count pending agent_message entries in bus_queue.jsonl for a given agent.
///
/// Reads line-by-line with a size cap to avoid OOM on large queues (CLI-M2).
pub(crate) async fn count_pending_tasks(home_dir: &Path, agent_id: &str) -> usize {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let queue_path = home_dir.join("bus_queue.jsonl");
    let file = match tokio::fs::File::open(&queue_path).await {
        Ok(f) => f,
        Err(_) => return 0,
    };

    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut count = 0usize;

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
            && v.get("type").and_then(|t| t.as_str()) == Some("agent_message")
            && v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
        {
            count += 1;
        }
    }

    count
}

// ── Feedback handler ────────────────────────────────────────
