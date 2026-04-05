//! Agent Dispatcher — consumes messages from `bus_queue.jsonl` and spawns
//! Claude CLI sub-processes for target agents.
//!
//! The dispatcher polls the queue file every 5 seconds. When it finds an
//! `agent_message`, it calls the Claude CLI on behalf of the target agent
//! and appends the result back to the queue as an `agent_response`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::claude_runner::{call_claude_for_agent_with_type};
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_container::sandbox;

use duduclaw_core::{MAX_DELEGATION_DEPTH, ENV_DELEGATION_DEPTH, ENV_DELEGATION_ORIGIN, ENV_DELEGATION_SENDER};

/// Message envelope stored in `bus_queue.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BusMessage {
    /// "agent_message" | "agent_response"
    #[serde(rename = "type")]
    msg_type: String,
    message_id: String,
    agent_id: String,
    payload: String,
    timestamp: String,
    /// Present only on responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<String>,
    /// Present only on responses — the original message_id being answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to: Option<String>,

    // ── Delegation safety metadata ─────────────────────────────
    /// How many times this message has been forwarded between agents.
    /// Incremented on each delegation hop. Messages exceeding
    /// `MAX_DELEGATION_DEPTH` are dropped by the dispatcher.
    #[serde(default)]
    delegation_depth: u8,
    /// The agent that originally initiated the delegation chain.
    /// Remains constant across all hops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin_agent: Option<String>,
    /// The agent that directly sent this message (updated on each hop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender_agent: Option<String>,
}

/// Starts the agent dispatcher as a background task.
///
/// Polls `bus_queue.jsonl` every 5 seconds for unprocessed `agent_message`
/// entries and dispatches them to the Claude CLI.
pub fn start_agent_dispatcher(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    start_agent_dispatcher_with_crypto(home_dir, registry, None)
}

/// Start the dispatcher with optional encryption key for deferred GVU (review #30).
pub fn start_agent_dispatcher_with_crypto(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    encryption_key: Option<[u8; 32]>,
) -> tokio::task::JoinHandle<()> {
    // Mutex protects the read-modify-write cycle on bus_queue.jsonl
    let dispatch_lock = Arc::new(tokio::sync::Mutex::new(()));
    tokio::spawn(async move {
        info!("Agent dispatcher started");
        let mut tick: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            tick += 1;
            let _guard = dispatch_lock.lock().await;
            if let Err(e) = poll_and_dispatch(&home_dir, &registry).await {
                warn!("Dispatcher poll error: {e}");
            }
            // Deferred GVU polling every 60 ticks (~5 min)
            if tick % 60 == 0 {
                poll_deferred_gvu(&home_dir, encryption_key.as_ref()).await;
            }
        }
    })
}

/// Poll deferred GVU entries from evolution.db and write bus messages for retry.
///
/// Runs every ~5 min from the dispatcher loop. Checks all agents for pending
/// deferred GVU tasks whose retry_after has elapsed.
async fn poll_deferred_gvu(home_dir: &Path, encryption_key: Option<&[u8; 32]>) {
    let db_path = home_dir.join("evolution.db");
    if !db_path.exists() {
        return;
    }

    let vs = crate::gvu::version_store::VersionStore::with_crypto(&db_path, encryption_key);

    // Get all agent IDs that have pending deferrals by scanning the table
    // (VersionStore::get_pending_deferred requires an agent_id, so we do a broader check)
    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let agent_ids: Vec<String> = conn
        .prepare("SELECT DISTINCT agent_id FROM deferred_gvu WHERE status = 'pending' AND retry_after <= ?1")
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map(
                rusqlite::params![chrono::Utc::now().to_rfc3339()],
                |row| row.get(0),
            )
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    drop(conn);

    for agent_id in &agent_ids {
        let pending = vs.get_pending_deferred(agent_id);
        for deferred in &pending {
            info!(
                agent = %agent_id,
                deferred_id = %deferred.id,
                retry_count = deferred.retry_count,
                gradients = deferred.gradients.len(),
                "Deferred GVU ready for retry — injecting bus message"
            );

            let trigger = format!(
                "## Deferred GVU Retry (attempt {})\n\
                 Accumulated {} gradients from previous failed attempts.\n\
                 Previous feedback:\n{}",
                deferred.retry_count,
                deferred.gradients.len(),
                deferred.gradients.iter()
                    .map(|g| format!("- [{}] {}", g.source_layer, g.critique))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );

            // Write bus message FIRST, then mark completed (review issue #33).
            // If bus write fails, the deferral stays pending and will be retried.
            let queue_path = home_dir.join("bus_queue.jsonl");
            let msg = serde_json::json!({
                "type": "agent_message",
                "message_id": uuid::Uuid::new_v4().to_string(),
                "agent_id": agent_id,
                "payload": trigger,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "delegation_depth": 0,
                "origin_agent": "__deferred_gvu__",
                "sender_agent": "__deferred_gvu__",
            });
            let bus_written = if let Ok(line) = serde_json::to_string(&msg) {
                match append_line(&queue_path, &line).await {
                    Ok(()) => true,
                    Err(e) => {
                        warn!(agent = %agent_id, "Failed to write deferred GVU bus message: {e} — will retry next poll");
                        false
                    }
                }
            } else {
                false
            };

            // Only mark completed if bus message was written successfully
            if bus_written {
                if let Err(e) = vs.mark_deferred_completed(&deferred.id) {
                    warn!(
                        agent = %agent_id,
                        deferred_id = %deferred.id,
                        error = %e,
                        "Failed to mark deferred GVU as completed — may re-process"
                    );
                }
            }
        }
    }
}

/// Read the queue, extract pending `agent_message` entries, process them,
/// then rewrite the queue without those entries (and append responses).
async fn poll_and_dispatch(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
) -> Result<(), String> {
    let queue_path = home_dir.join("bus_queue.jsonl");

    // Read all lines
    let content = match tokio::fs::read_to_string(&queue_path).await {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return Ok(()), // no file or empty — nothing to do
    };

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Ok(());
    }

    let mut to_dispatch: Vec<BusMessage> = Vec::new();
    let mut remaining_lines: Vec<String> = Vec::new();

    for line in &lines {
        match serde_json::from_str::<BusMessage>(line) {
            Ok(msg) if msg.msg_type == "agent_message" => {
                if !duduclaw_core::is_valid_agent_id(&msg.agent_id) {
                    warn!("Invalid agent_id in bus queue, skipping");
                    continue;
                }
                // Enforce payload size limit (BE-H6) — drop, do NOT keep in queue
                if msg.payload.len() > 100_000 {
                    warn!(id = %msg.message_id, len = msg.payload.len(), "Dropping oversized message (removed from queue)");
                } else if msg.delegation_depth >= MAX_DELEGATION_DEPTH {
                    // Delegation depth exceeded — drop to prevent infinite loops
                    warn!(
                        id = %msg.message_id,
                        agent = %msg.agent_id,
                        depth = msg.delegation_depth,
                        origin = ?msg.origin_agent,
                        sender = ?msg.sender_agent,
                        "Dropping message: delegation depth {}/{MAX_DELEGATION_DEPTH} exceeded",
                        msg.delegation_depth,
                    );
                    // Write an error response so the caller knows the chain was terminated
                    let err_response = BusMessage {
                        msg_type: "agent_response".to_string(),
                        message_id: uuid::Uuid::new_v4().to_string(),
                        agent_id: msg.agent_id.clone(),
                        payload: format!(
                            "Error: delegation depth limit ({MAX_DELEGATION_DEPTH}) exceeded. \
                             Chain: origin={}, sender={}, depth={}. \
                             Message dropped to prevent infinite loop.",
                            msg.origin_agent.as_deref().unwrap_or("unknown"),
                            msg.sender_agent.as_deref().unwrap_or("unknown"),
                            msg.delegation_depth,
                        ),
                        timestamp: Utc::now().to_rfc3339(),
                        response: None,
                        in_reply_to: Some(msg.message_id.clone()),
                        delegation_depth: msg.delegation_depth,
                        origin_agent: msg.origin_agent.clone(),
                        sender_agent: Some(msg.agent_id.clone()),
                    };
                    if let Ok(json) = serde_json::to_string(&err_response) {
                        if let Err(e) = append_line(&queue_path, &json).await {
                            warn!(
                                id = %msg.message_id,
                                error = %e,
                                "Failed to write delegation depth-exceeded error response"
                            );
                        }
                    }
                } else {
                    to_dispatch.push(msg);
                }
            }
            _ => {
                // Keep non-message lines (responses, unknown types)
                remaining_lines.push(line.to_string());
            }
        }
    }

    if to_dispatch.is_empty() {
        return Ok(());
    }

    // ── Request Coalescing ──────────────────────────────────────
    // Merge consecutive messages for the same agent into a single prompt.
    // This reduces API calls and amortizes the system prompt overhead.
    // Max: 5 messages per coalesced group, 2-second window is implicit
    // (we batch within a single poll cycle which is 5 seconds).
    let to_dispatch = coalesce_messages(to_dispatch);

    info!(count = to_dispatch.len(), "Dispatching agent messages (after coalescing)");

    // Rewrite the queue without the messages we're about to process.
    // Uses write→rename for atomicity — prevents data loss if process crashes mid-write.
    let new_content = if remaining_lines.is_empty() {
        String::new()
    } else {
        let mut s = remaining_lines.join("\n");
        s.push('\n');
        s
    };
    let tmp_path = queue_path.with_extension("jsonl.tmp");
    tokio::fs::write(&tmp_path, &new_content)
        .await
        .map_err(|e| format!("Failed to write temp bus_queue: {e}"))?;
    tokio::fs::rename(&tmp_path, &queue_path)
        .await
        .map_err(|e| format!("Failed to rename temp bus_queue: {e}"))?;

    // Process each message concurrently (up to 4 at a time)
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let home = home_dir.to_path_buf();
    let reg = registry.clone();
    let queue = queue_path.clone();

    let mut handles = Vec::new();
    for msg in to_dispatch {
        let permit = semaphore.clone().acquire_owned().await.map_err(|e| e.to_string())?;
        let home = home.clone();
        let reg = reg.clone();
        let queue = queue.clone();

        handles.push(tokio::spawn(async move {
            let delegation_env = DelegationEnv {
                depth: msg.delegation_depth,
                origin: msg.origin_agent.clone().unwrap_or_default(),
                sender: msg.sender_agent.clone().unwrap_or_default(),
            };
            let result = dispatch_to_agent(&home, &reg, &msg.agent_id, &msg.payload, &delegation_env).await;

            let response_text = match &result {
                Ok(text) => text.clone(),
                Err(e) => format!("Error: {e}"),
            };

            info!(
                message_id = %msg.message_id,
                agent = %msg.agent_id,
                ok = result.is_ok(),
                "Agent dispatch completed"
            );

            // Append response to the queue
            let response_entry = BusMessage {
                msg_type: "agent_response".to_string(),
                message_id: uuid::Uuid::new_v4().to_string(),
                agent_id: msg.agent_id.clone(),
                payload: response_text,
                timestamp: Utc::now().to_rfc3339(),
                response: None,
                in_reply_to: Some(msg.message_id.clone()),
                delegation_depth: msg.delegation_depth,
                origin_agent: msg.origin_agent.clone(),
                sender_agent: Some(msg.agent_id.clone()),
            };

            if let Ok(json) = serde_json::to_string(&response_entry) {
                if let Err(e) = append_line(&queue, &json).await {
                    warn!(
                        message_id = %msg.message_id,
                        agent = %msg.agent_id,
                        error = %e,
                        "Failed to write agent response to bus_queue"
                    );
                }
            }

            drop(permit);
        }));
    }

    // Wait for all dispatches to complete
    for handle in handles {
        if let Err(e) = handle.await {
            warn!("Dispatch task panicked: {e}");
        }
    }

    Ok(())
}

/// Delegation context injected as environment variables into Claude CLI subprocesses.
/// The MCP server reads these to track depth without relying on spoofable tool params.
#[derive(Debug, Clone)]
struct DelegationEnv {
    depth: u8,
    origin: String,
    sender: String,
}

impl DelegationEnv {
    /// Convert to a map of env vars to pass to the Claude CLI subprocess.
    fn to_env_map(&self) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        map.insert(ENV_DELEGATION_DEPTH.to_string(), self.depth.to_string());
        map.insert(ENV_DELEGATION_ORIGIN.to_string(), self.origin.clone());
        map.insert(ENV_DELEGATION_SENDER.to_string(), self.sender.clone());
        map
    }
}

/// Dispatch a task to an agent — using sandbox if enabled, otherwise direct call.
async fn dispatch_to_agent(
    home_dir: &std::path::Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    delegation: &DelegationEnv,
) -> Result<String, String> {
    // Check if agent has sandbox enabled
    let use_sandbox = {
        let reg = registry.read().await;
        let agent = if agent_id == "default" {
            reg.main_agent()
        } else {
            reg.get(agent_id)
        };
        agent.is_some_and(|a| a.config.container.sandbox_enabled)
    };

    // Pass delegation context via task-local so prepare_claude_cmd can
    // inject it as per-subprocess env vars (thread-safe, no global state).
    let env_map = delegation.to_env_map();

    let env_map_clone = env_map.clone();
    crate::claude_runner::DELEGATION_ENV.scope(env_map, async {
        if use_sandbox && sandbox::is_sandbox_available().await {
            info!(agent = agent_id, "Dispatching via sandbox");
            dispatch_sandboxed(home_dir, registry, agent_id, prompt, &env_map_clone).await
        } else {
            call_claude_for_agent_with_type(
                home_dir, registry, agent_id, prompt,
                crate::cost_telemetry::RequestType::Dispatch,
            ).await
        }
    }).await
}

/// Execute a task inside a sandboxed Docker container.
async fn dispatch_sandboxed(
    home_dir: &std::path::Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    delegation_env: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let reg = registry.read().await;
    let agent = if agent_id == "default" {
        reg.main_agent()
    } else {
        reg.get(agent_id)
    };
    let agent = agent.ok_or_else(|| format!("Agent '{agent_id}' not found"))?;

    let agent_dir = agent.dir.clone();
    let model = agent.config.model.preferred.clone();
    let network = agent.config.container.network_access;
    let timeout_ms = agent.config.container.timeout_ms;
    let capabilities = agent.config.capabilities.clone();

    // Build system prompt
    let mut parts = Vec::new();
    if let Some(soul) = &agent.soul {
        parts.push(format!("# Soul\n{soul}"));
    }
    if let Some(identity) = &agent.identity {
        parts.push(format!("# Identity\n{identity}"));
    }
    let system_prompt = parts.join("\n\n---\n\n");
    drop(reg);

    // Get API key
    let api_key = crate::claude_runner::get_api_key_from_home(home_dir).await;
    if api_key.is_empty() {
        return Err("No API key configured for sandbox".to_string());
    }

    let timeout = std::time::Duration::from_millis(timeout_ms);
    let extra_env: Vec<(String, String)> = delegation_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let denied_tools = capabilities.disallowed_tools();
    let result = sandbox::run_sandboxed_with_env(
        &agent_dir,
        prompt,
        &model,
        &system_prompt,
        &api_key,
        timeout,
        network,
        &extra_env,
        &denied_tools,
    )
    .await?;

    if result.timed_out {
        return Err("Sandbox execution timed out".to_string());
    }

    if result.exit_code != 0 {
        return Err(format!(
            "Sandbox exit code {}: {}",
            result.exit_code,
            result.stderr.chars().take(200).collect::<String>()
        ));
    }

    let text = result.stdout.trim().to_string();
    if text.is_empty() {
        Ok("(empty response from sandbox)".to_string())
    } else {
        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// TaskSpec step-by-step execution (Phase 3 GVU²)
// ---------------------------------------------------------------------------

/// Execute a TaskSpec step-by-step through the dispatcher.
///
/// For each ready step:
/// 1. Build a prompt with prior step context (via `build_step_prompt`)
/// 2. Dispatch to the target agent (or default agent)
/// 3. Verify output against acceptance criteria (Auto only; LLM/Sandbox deferred)
/// 4. Mark step as passed or failed
/// 5. On failure: retry (max 3), replan (max 2), or abandon
///
/// Returns the final TaskSpec state after execution.
pub async fn dispatch_taskspec(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    spec: &mut crate::task_spec::TaskSpec,
    agent_dir: &Path,
) -> Result<(), String> {
    use crate::task_spec::*;

    info!(
        task = %spec.task_id,
        goal = %spec.goal,
        steps = spec.steps.len(),
        "Starting TaskSpec execution"
    );

    spec.status = TaskStatus::Running;
    spec.save(agent_dir).map_err(|e| format!("Save: {e}"))?;

    loop {
        // Find next ready step
        let step_index = match spec.next_ready_step_index() {
            Some(i) => i,
            None => {
                // No more ready steps — check if we're done or blocked
                if spec.is_terminal() {
                    break;
                }
                // Check for blocked state (all remaining are pending but deps not met)
                let has_pending = spec.steps.iter().any(|s| s.status == StepStatus::Pending);
                let has_running = spec.steps.iter().any(|s| s.status == StepStatus::Running);
                if has_pending && !has_running {
                    warn!(task = %spec.task_id, "Task blocked: pending steps have unmet dependencies");
                    spec.status = TaskStatus::Failed;
                }
                break;
            }
        };

        let step = &spec.steps[step_index];
        let step_desc = step.description.clone();
        let target_agent = if step.agent.is_empty() {
            spec.agent_id.clone()
        } else {
            step.agent.clone()
        };

        info!(
            task = %spec.task_id,
            step = step_index,
            agent = %target_agent,
            "Executing step: {}",
            &step_desc,
        );

        // Build prompt with prior step context
        let prompt = match build_step_prompt(spec, step_index) {
            Some(p) => p,
            None => {
                warn!(task = %spec.task_id, step = step_index, "Failed to build step prompt");
                break;
            }
        };

        spec.mark_running(step_index);
        spec.save(agent_dir).ok();

        // Dispatch to agent
        let delegation = DelegationEnv {
            depth: 0,
            origin: spec.agent_id.clone(),
            sender: spec.agent_id.clone(),
        };
        let result = dispatch_to_agent(home_dir, registry, &target_agent, &prompt, &delegation).await;

        match result {
            Ok(output) => {
                // Verify with Auto criteria
                let criteria_results = verify_step_auto(&spec.steps[step_index], &output);
                let all_passed = criteria_results.is_empty() || criteria_results.iter().all(|r| r.passed);

                if all_passed {
                    spec.mark_passed(step_index, StepResult {
                        output: output.clone(),
                        artifacts: Vec::new(),
                        criteria_results,
                        self_confidence: None,
                        completed_at: Utc::now(),
                    });
                    info!(task = %spec.task_id, step = step_index, "Step passed");
                } else {
                    let failed_criteria: Vec<String> = criteria_results.iter()
                        .filter(|r| !r.passed)
                        .map(|r| r.description.clone())
                        .collect();
                    let error_msg = format!("Criteria not met: {}", failed_criteria.join(", "));
                    handle_step_failure(spec, step_index, &error_msg, home_dir, registry, agent_dir).await?;
                }
            }
            Err(e) => {
                handle_step_failure(spec, step_index, &e, home_dir, registry, agent_dir).await?;
            }
        }

        spec.save(agent_dir).ok();
    }

    // Final save
    spec.save(agent_dir).map_err(|e| format!("Final save: {e}"))?;

    info!(
        task = %spec.task_id,
        status = ?spec.status,
        completed = spec.steps.iter().filter(|s| s.status == StepStatus::Passed).count(),
        total = spec.steps.len(),
        "TaskSpec execution finished"
    );

    Ok(())
}

/// Handle a step failure: retry, replan, or abandon.
async fn handle_step_failure(
    spec: &mut crate::task_spec::TaskSpec,
    step_index: usize,
    error: &str,
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_dir: &Path,
) -> Result<(), String> {
    use crate::task_spec::*;

    let action = spec.mark_failed(step_index, error);
    spec.save(agent_dir).ok();

    match action {
        FailureAction::Retry { step_index, attempt, error } => {
            // Exponential backoff: 5s, 10s, 20s (review issue #31)
            let delay_secs = 5u64 * (1 << attempt.min(3));
            info!(
                task = %spec.task_id,
                step = step_index,
                attempt,
                delay_secs,
                "Retrying step after {delay_secs}s backoff (error: {})",
                &error[..error.len().min(100)],
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            // Step is already reset to Pending — next loop iteration will pick it up
            Ok(())
        }
        FailureAction::Replan { failed_step, error } => {
            warn!(
                task = %spec.task_id,
                step = failed_step,
                replan = spec.replan_count + 1,
                "Step failed after retries — requesting replan"
            );

            // Call planner to get new remaining steps
            let replan_prompt = format!(
                "The original plan for '{}' failed at step {}. Error: {}\n\n\
                 Completed steps so far:\n{}\n\n\
                 Please provide a new plan for the remaining work.",
                spec.goal, failed_step, error, spec.completed_steps_briefing(),
            );

            let delegation = DelegationEnv {
                depth: 0,
                origin: spec.agent_id.clone(),
                sender: "__planner__".to_string(),
            };
            let planner_response = dispatch_to_agent(
                home_dir, registry, &spec.agent_id, &replan_prompt, &delegation,
            ).await?;

            match parse_planner_response(&planner_response) {
                Ok(new_steps) => {
                    spec.replan(new_steps);
                    spec.save(agent_dir).ok();
                    info!(task = %spec.task_id, "Replan succeeded — continuing execution");
                    Ok(())
                }
                Err(e) => {
                    warn!(task = %spec.task_id, "Replan failed to parse: {e}");
                    spec.status = TaskStatus::Failed;
                    spec.save(agent_dir).ok();
                    Err(format!("Replan failed: {e}"))
                }
            }
        }
        FailureAction::Abandon { reason } => {
            warn!(task = %spec.task_id, "Task abandoned: {reason}");
            // status already set to Failed by mark_failed
            Ok(())
        }
    }
}

/// Maximum number of messages to coalesce into a single prompt per agent.
const MAX_COALESCE: usize = 5;

/// Merge consecutive bus messages for the same agent into a single message.
///
/// This amortizes the system prompt overhead across multiple user messages,
/// reducing total API calls. Messages are grouped by `agent_id`; within each
/// group, payloads are joined with a separator. The first message's
/// `message_id` is reused as the coalesced message's ID.
fn coalesce_messages(messages: Vec<BusMessage>) -> Vec<BusMessage> {
    use std::collections::HashMap;

    // Group by agent_id, preserving insertion order via Vec
    let mut groups: HashMap<String, Vec<BusMessage>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for msg in messages {
        let key = msg.agent_id.clone();
        let entry = groups.entry(key.clone()).or_default();
        if entry.is_empty() {
            order.push(key);
        }
        entry.push(msg);
    }

    let mut result = Vec::new();
    for agent_id in order {
        let group = groups.remove(&agent_id).unwrap_or_default();
        if group.len() <= 1 {
            result.extend(group);
            continue;
        }

        // Split into chunks of MAX_COALESCE
        for chunk in group.chunks(MAX_COALESCE) {
            if chunk.len() == 1 {
                result.push(chunk[0].clone());
                continue;
            }

            let coalesced_payload = chunk
                .iter()
                .enumerate()
                .map(|(i, m)| format!("[Message {}] {}", i + 1, m.payload))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            let mut coalesced_payload = coalesced_payload;
            if coalesced_payload.len() > 200_000 {
                warn!(size = coalesced_payload.len(), "Coalesced payload too large, truncating");
                coalesced_payload.truncate(200_000);
            }

            let first = &chunk[0];
            info!(
                agent = %first.agent_id,
                merged = chunk.len(),
                "Coalesced messages into single prompt"
            );

            // Use the maximum delegation depth from the chunk to prevent
            // a low-depth message from masking a deeper one.
            let max_depth = chunk.iter().map(|m| m.delegation_depth).max().unwrap_or(0);

            result.push(BusMessage {
                msg_type: "agent_message".to_string(),
                message_id: first.message_id.clone(),
                agent_id: first.agent_id.clone(),
                payload: coalesced_payload,
                timestamp: first.timestamp.clone(),
                response: None,
                in_reply_to: None,
                delegation_depth: max_depth,
                origin_agent: first.origin_agent.clone(),
                sender_agent: first.sender_agent.clone(),
            });
        }
    }

    result
}

/// Append a single line to a file with advisory file lock (MCP-M4).
///
/// Uses `spawn_blocking` + `flock(LOCK_EX)` on Unix for safe concurrent writes.
pub async fn append_line(path: &Path, line: &str) -> Result<(), String> {
    let path = path.to_path_buf();
    let line = line.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: fd comes from a valid, open File handle obtained above.
            // flock is async-signal-safe and the fd remains valid for the duration of this call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "flock failed on {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }

        writeln!(file, "{line}")
            .map_err(|e| format!("Failed to write to {}: {e}", path.display()))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_message_deserializes_without_delegation_fields() {
        // Old-format messages (before delegation safety) should still parse
        let json = r#"{"type":"agent_message","message_id":"abc","agent_id":"test","payload":"hello","timestamp":"2026-01-01T00:00:00Z"}"#;
        let msg: BusMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.delegation_depth, 0);
        assert!(msg.origin_agent.is_none());
        assert!(msg.sender_agent.is_none());
    }

    #[test]
    fn bus_message_deserializes_with_delegation_fields() {
        let json = r#"{"type":"agent_message","message_id":"abc","agent_id":"worker","payload":"task","timestamp":"2026-01-01T00:00:00Z","delegation_depth":3,"origin_agent":"main","sender_agent":"researcher"}"#;
        let msg: BusMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.delegation_depth, 3);
        assert_eq!(msg.origin_agent.as_deref(), Some("main"));
        assert_eq!(msg.sender_agent.as_deref(), Some("researcher"));
    }

    #[test]
    fn bus_message_serializes_omits_none_fields() {
        let msg = BusMessage {
            msg_type: "agent_message".to_string(),
            message_id: "abc".to_string(),
            agent_id: "test".to_string(),
            payload: "hello".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            response: None,
            in_reply_to: None,
            delegation_depth: 0,
            origin_agent: None,
            sender_agent: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // None fields with skip_serializing_if should be absent
        assert!(!json.contains("origin_agent"));
        assert!(!json.contains("sender_agent"));
        assert!(!json.contains("response"));
        // delegation_depth is always serialized (no skip)
        assert!(json.contains("delegation_depth"));
    }

    #[test]
    fn depth_limit_constant_is_reasonable() {
        // Ensure MAX_DELEGATION_DEPTH is between 2 and 10
        assert!(MAX_DELEGATION_DEPTH >= 2);
        assert!(MAX_DELEGATION_DEPTH <= 10);
    }

    #[test]
    fn coalesce_uses_max_depth_from_chunk() {
        let msgs = vec![
            BusMessage {
                msg_type: "agent_message".to_string(),
                message_id: "m1".to_string(),
                agent_id: "worker".to_string(),
                payload: "task 1".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                response: None,
                in_reply_to: None,
                delegation_depth: 2,
                origin_agent: Some("main".to_string()),
                sender_agent: Some("researcher".to_string()),
            },
            BusMessage {
                msg_type: "agent_message".to_string(),
                message_id: "m2".to_string(),
                agent_id: "worker".to_string(),
                payload: "task 2".to_string(),
                timestamp: "2026-01-01T00:00:01Z".to_string(),
                response: None,
                in_reply_to: None,
                delegation_depth: 4,
                origin_agent: Some("main".to_string()),
                sender_agent: Some("analyst".to_string()),
            },
        ];
        let coalesced = coalesce_messages(msgs);
        assert_eq!(coalesced.len(), 1);
        // Should use the maximum depth from the chunk (4, not 2)
        assert_eq!(coalesced[0].delegation_depth, 4);
        // origin_agent and sender_agent come from first message
        assert_eq!(coalesced[0].origin_agent.as_deref(), Some("main"));
        assert_eq!(coalesced[0].sender_agent.as_deref(), Some("researcher"));
    }
}
