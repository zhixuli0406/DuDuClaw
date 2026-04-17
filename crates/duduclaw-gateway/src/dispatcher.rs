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
    /// Additional message_ids absorbed during coalescing.
    /// Used to consume delegation callbacks for all merged messages, not just the first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    coalesced_ids: Vec<String>,
}

/// Starts the agent dispatcher as a background task.
///
/// Polls `bus_queue.jsonl` every 5 seconds for unprocessed `agent_message`
/// entries and dispatches them to the Claude CLI.
pub fn start_agent_dispatcher(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    start_agent_dispatcher_with_crypto(home_dir, registry, None, None)
}

/// Start the dispatcher with optional encryption key for deferred GVU (review #30)
/// and optional SQLite message queue (Phase 3 Hybrid TaskPipeline).
pub fn start_agent_dispatcher_with_crypto(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    encryption_key: Option<[u8; 32]>,
    message_queue: Option<Arc<crate::message_queue::MessageQueue>>,
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

            // Poll JSONL bus queue (legacy path — kept for backward compat)
            if let Err(e) = poll_and_dispatch(&home_dir, &registry).await {
                warn!("Dispatcher poll error: {e}");
            }

            // Poll SQLite message queue (new path)
            if let Some(ref mq) = message_queue {
                if let Err(e) = poll_and_dispatch_sqlite(mq, &home_dir, &registry).await {
                    warn!("SQLite dispatcher poll error: {e}");
                }
                // Sweep stale messages every 12 ticks (~60 seconds)
                if tick % 12 == 0 {
                    if let Err(e) = sweep_stale_messages(mq).await {
                        warn!("Stale message sweep error: {e}");
                    }
                }
            }

            // Deferred GVU polling every 60 ticks (~5 min)
            if tick % 60 == 0 {
                poll_deferred_gvu(&home_dir, encryption_key.as_ref()).await;
            }
            // Clean up orphaned delegation callbacks every 720 ticks (~1 hour)
            if tick % 720 == 0 {
                cleanup_stale_delegation_callbacks(&home_dir).await;
            }
            // TaskSpec polling every 2 ticks (~10 seconds)
            if tick % 2 == 0 {
                poll_pending_taskspecs(&home_dir, &registry).await;
            }
        }
    })
}

/// Poll the SQLite message queue for pending messages and dispatch them.
async fn poll_and_dispatch_sqlite(
    queue: &Arc<crate::message_queue::MessageQueue>,
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
) -> Result<(), String> {
    let messages = queue.pending_messages(10).await?;
    if messages.is_empty() {
        return Ok(());
    }

    info!(count = messages.len(), "SQLite queue: dispatching pending messages");

    for msg in messages {
        // ACK immediately to prevent double-pickup
        queue.ack(&msg.id).await?;

        let delegation = DelegationEnv {
            depth: msg.delegation_depth as u8,
            origin: msg.origin_agent.clone().unwrap_or_default(),
            sender: msg.sender_agent.clone().unwrap_or_default(),
        };

        match dispatch_to_agent(home_dir, registry, &msg.target, &msg.payload, &delegation).await
        {
            Ok(response) => {
                queue.complete(&msg.id, &response).await?;
                // Forward sub-agent response to originating channel if callback exists
                forward_delegation_response(home_dir, &msg.id, &response, &msg.target).await;
                info!(
                    msg_id = %msg.id,
                    target = %msg.target,
                    "SQLite queue: message dispatched successfully"
                );
            }
            Err(e) => {
                queue.fail(&msg.id, &e).await?;
                warn!(
                    msg_id = %msg.id,
                    target = %msg.target,
                    error = %e,
                    "SQLite queue: message dispatch failed"
                );
            }
        }
    }

    Ok(())
}

/// Reset stale acked messages back to pending, or fail them if retries exhausted.
async fn sweep_stale_messages(
    queue: &Arc<crate::message_queue::MessageQueue>,
) -> Result<(), String> {
    let stale = queue.stale_messages(60).await?;
    for msg in stale {
        if msg.retry_count < 3 {
            queue.reset_to_pending(&msg.id).await?;
            warn!(
                msg_id = %msg.id,
                target = %msg.target,
                retry = msg.retry_count + 1,
                "SQLite queue: stale message reset for retry"
            );
        } else {
            queue
                .fail(&msg.id, "Timeout: unacked after 60s, retries exhausted")
                .await?;
            warn!(
                msg_id = %msg.id,
                target = %msg.target,
                "SQLite queue: stale message abandoned after 3 retries"
            );
        }
    }
    Ok(())
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
                        coalesced_ids: vec![],
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

    // Pre-initialize MistakeNotebook once for all dispatch tasks (review R2-5).
    // Shared via Arc to avoid repeated init_table() + SQLite open per task.
    let notebook = Arc::new(crate::gvu::mistake_notebook::MistakeNotebook::new(
        &home.join("evolution.db"),
    ));

    let mut handles = Vec::new();
    for msg in to_dispatch {
        let permit = semaphore.clone().acquire_owned().await.map_err(|e| e.to_string())?;
        let home = home.clone();
        let reg = reg.clone();
        let queue = queue.clone();
        let notebook = notebook.clone();

        handles.push(tokio::spawn(async move {
            let dispatch_start = Utc::now().to_rfc3339();
            let delegation_env = DelegationEnv {
                depth: msg.delegation_depth,
                origin: msg.origin_agent.clone().unwrap_or_default(),
                sender: msg.sender_agent.clone().unwrap_or_default(),
            };
            let result = dispatch_to_agent(&home, &reg, &msg.agent_id, &msg.payload, &delegation_env).await;

            let mut response_text = match &result {
                Ok(text) => text.clone(),
                Err(e) => format!("Error: {e}"),
            };

            // ── L2+L4: Post-Action Hallucination Audit ──────────
            // Cross-reference agent output claims against MCP tool call log.
            // Zero LLM cost — pure regex + log lookup.
            // Wrapped in spawn_blocking to avoid blocking the Tokio runtime
            // (file I/O + SQLite operations are synchronous).
            if result.is_ok() {
                let home_bl = home.clone();
                let agent_bl = msg.agent_id.clone();
                let msg_id_bl = msg.message_id.clone();
                let resp_bl = response_text.clone();
                let start_bl = dispatch_start.clone();
                let notebook_bl = notebook.clone();

                let hallucination_warning = tokio::task::spawn_blocking(move || {
                    let hallucinations = duduclaw_security::action_claim_verifier::detect_hallucinations(
                        &home_bl,
                        &agent_bl,
                        &resp_bl,
                        &start_bl,
                    );
                    if hallucinations.is_empty() {
                        return None;
                    }

                    tracing::warn!(
                        agent = %agent_bl,
                        count = hallucinations.len(),
                        "Tool-use hallucination detected in agent output"
                    );

                    // Log each hallucination to security audit
                    for h in &hallucinations {
                        if let duduclaw_security::action_claim_verifier::VerifyResult::Hallucination { claim, reason } = h {
                            duduclaw_security::audit::log_tool_hallucination(
                                &home_bl,
                                &agent_bl,
                                &claim.matched_text,
                                claim.claim_type.expected_tool(),
                            );
                            tracing::info!(
                                agent = %agent_bl,
                                claim = %claim.matched_text,
                                reason = %reason,
                                "Hallucination detail"
                            );
                        }
                    }

                    // Record hallucination in MistakeNotebook for GVU evolution (L5)
                    // Pre-truncate response to avoid allocating large Vec<char> in truncate_str
                    let resp_summary: String = resp_bl.chars().take(200).collect();
                    for h in &hallucinations {
                        if let duduclaw_security::action_claim_verifier::VerifyResult::Hallucination { claim, .. } = h {
                            if let Err(e) = notebook_bl.record_hallucination(
                                &agent_bl,
                                &msg_id_bl,
                                &claim.matched_text,
                                claim.claim_type.expected_tool(),
                                &resp_summary,
                            ) {
                                tracing::warn!(agent = %agent_bl, error = %e, "Failed to record hallucination in MistakeNotebook");
                            }
                        }
                    }

                    Some(hallucinations.len())
                }).await.ok().flatten();

                if let Some(count) = hallucination_warning {
                    // Inject a system correction so downstream agents / users see the truth.
                    // The original hallucinated text remains in the response but is clearly
                    // flagged — full redaction would break response coherence.
                    response_text.push_str(&format!(
                        "\n\n[DUDUCLAW_SYSTEM:HALLUCINATION_DETECTED] ⚠️ This response contains \
                         {count} action claim(s) that have NO corresponding MCP tool call in the \
                         audit log. The claimed actions were NOT actually performed. \
                         Do NOT trust these claims. If re-delegating, use the create_task tool \
                         for reliable deterministic execution.",
                    ));
                }
            }

            // ── L5: MCP Permission Failure Detection ──────────
            // When an agent's response indicates it couldn't use MCP tools
            // due to permission issues, inject a system-level escalation
            // so the sender (and user) can see the real problem.
            let permission_blocked = result.is_ok()
                && (response_text.contains("權限")
                    || response_text.contains("permission")
                    || response_text.contains("無法存取")
                    || response_text.contains("需要授權")
                    || response_text.contains("尚未授權")
                    || response_text.contains("工具權限受阻"))
                && (response_text.contains("list_agents")
                    || response_text.contains("wiki_")
                    || response_text.contains("send_to_agent")
                    || response_text.contains("mcp__duduclaw"));

            if permission_blocked {
                warn!(
                    agent = %msg.agent_id,
                    sender = ?msg.sender_agent,
                    "Agent response indicates MCP tool permission failure — \
                     likely missing --allowedTools in CLI spawn"
                );
                response_text.push_str(
                    "\n\n[DUDUCLAW_SYSTEM:MCP_PERMISSION_BLOCKED] ⚠️ This agent reported \
                     MCP tool permission failures. This is a system configuration issue, \
                     not an agent error. The gateway needs to pass --allowedTools \
                     'mcp__duduclaw__*' when spawning sub-agents. Please update the \
                     gateway binary to fix this.",
                );
            }

            info!(
                message_id = %msg.message_id,
                agent = %msg.agent_id,
                ok = result.is_ok(),
                permission_blocked,
                "Agent dispatch completed"
            );

            // Clone response text before moving into BusMessage (needed for callback forwarding)
            let response_for_callback = response_text.clone();

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
                coalesced_ids: vec![],
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

            // ── Delegation callback: forward response to originating channel ──
            // If the original `send_to_agent` was called from a channel context,
            // a callback was registered linking message_id → channel info.
            // Forward the sub-agent's response directly to that channel.
            // Also consume callbacks for coalesced (merged) messages.
            forward_delegation_response(&home, &msg.message_id, &response_for_callback, &msg.agent_id).await;
            for extra_id in &msg.coalesced_ids {
                forward_delegation_response(&home, extra_id, &response_for_callback, &msg.agent_id).await;
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

/// Dispatch a task to an agent — L0 worktree → L1 sandbox → direct call.
async fn dispatch_to_agent(
    home_dir: &std::path::Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    delegation: &DelegationEnv,
) -> Result<String, String> {
    // Read isolation flags from agent config.
    let (use_sandbox, use_worktree, worktree_cfg) = {
        let reg = registry.read().await;
        let agent = if agent_id == "default" {
            reg.main_agent()
        } else {
            reg.get(agent_id)
        };
        match agent {
            Some(a) => (
                a.config.container.sandbox_enabled,
                a.config.container.worktree_enabled,
                WorktreeCfg {
                    auto_merge: a.config.container.worktree_auto_merge,
                    cleanup: a.config.container.worktree_cleanup_on_exit,
                    copy_files: a.config.container.worktree_copy_files.clone(),
                    agent_dir: a.dir.clone(),
                },
            ),
            None => (false, false, WorktreeCfg::default()),
        }
    };

    // Pass delegation context via task-local so prepare_claude_cmd can
    // inject it as per-subprocess env vars (thread-safe, no global state).
    let env_map = delegation.to_env_map();

    let env_map_clone = env_map.clone();
    crate::claude_runner::DELEGATION_ENV.scope(env_map, async {
        if use_worktree {
            info!(agent = agent_id, "Dispatching via worktree (L0)");
            dispatch_in_worktree(home_dir, registry, agent_id, prompt, &worktree_cfg).await
        } else if use_sandbox && sandbox::is_sandbox_available().await {
            info!(agent = agent_id, "Dispatching via sandbox (L1)");
            dispatch_sandboxed(home_dir, registry, agent_id, prompt, &env_map_clone).await
        } else {
            call_claude_for_agent_with_type(
                home_dir, registry, agent_id, prompt,
                crate::cost_telemetry::RequestType::Dispatch,
            ).await
        }
    }).await
}

/// Per-agent worktree configuration snapshot (avoids holding registry lock).
#[derive(Debug, Clone, Default)]
struct WorktreeCfg {
    auto_merge: bool,
    cleanup: bool,
    copy_files: Vec<String>,
    agent_dir: PathBuf,
}

/// Execute a task in an isolated git worktree (L0 isolation).
///
/// Flow: create worktree → copy env files → call Claude CLI → inspect result
///       → snap decision (merge / cleanup / keep) → return response.
async fn dispatch_in_worktree(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    cfg: &WorktreeCfg,
) -> Result<String, String> {
    let manager = crate::worktree::WorktreeManager::new(home_dir);

    // Use the agent directory as repo root (it should be a git repo or
    // inside one). Fall back to the agent dir itself.
    let repo_root = find_git_root(&cfg.agent_dir).await.unwrap_or_else(|| cfg.agent_dir.clone());

    // Create worktree.
    let wt = manager.create(&repo_root, agent_id).await?;
    info!(
        agent = agent_id,
        branch = %wt.branch,
        path = %wt.path.display(),
        "Worktree created for task"
    );

    // Copy environment files.
    if let Err(e) = manager.copy_env_files(&cfg.agent_dir, &wt.path, &cfg.copy_files).await {
        warn!(agent = agent_id, err = %e, "Failed to copy env files to worktree");
    }

    // Call Claude CLI with worktree as working directory.
    // We use WORKTREE_PATH task-local to communicate the override to claude_runner.
    let result = crate::claude_runner::WORKTREE_PATH.scope(
        Some(wt.path.clone()),
        call_claude_for_agent_with_type(
            home_dir, registry, agent_id, prompt,
            crate::cost_telemetry::RequestType::Dispatch,
        ),
    ).await;

    // Snap: inspect and decide what to do with the worktree.
    let response_text = result.unwrap_or_else(|e| format!("Error: {e}"));

    if cfg.auto_merge || cfg.cleanup {
        let status = manager.inspect_worktree(&wt.path, &repo_root).await;
        let action = crate::worktree::determine_snap_action(&status);

        let target_branch = get_main_branch(&repo_root).await;
        match manager.execute_snap(&action, &repo_root, &wt.path, &wt.branch, &target_branch).await {
            Ok(outcome) => {
                info!(agent = agent_id, ?outcome, "Worktree snap completed");
            }
            Err(e) => {
                warn!(agent = agent_id, err = %e, "Worktree snap failed — keeping worktree");
            }
        }
    } else {
        info!(
            agent = agent_id,
            path = %wt.path.display(),
            branch = %wt.branch,
            "Worktree kept (auto_merge and cleanup both disabled) — run cleanup_stale to reclaim"
        );
    }

    Ok(response_text)
}

/// Find the git repository root using `git rev-parse --show-toplevel`.
///
/// Uses git itself instead of manually walking directories, which avoids
/// TOCTOU races with `.git` existence checks and handles gitdir files.
async fn find_git_root(start: &Path) -> Option<PathBuf> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .await
        .ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Detect the main/master/default branch of the repo.
///
/// Validates output to only contain safe branch name characters.
async fn get_main_branch(repo_root: &Path) -> String {
    let is_safe_branch = |s: &str| -> bool {
        !s.is_empty()
            && !s.starts_with('-')
            && !s.contains("..")
            && s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.')
    };

    // Try symbolic-ref for the default branch.
    let output = tokio::process::Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_root)
        .output()
        .await;
    if let Ok(o) = output {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            if let Some(branch) = s.trim().strip_prefix("refs/remotes/origin/") {
                if is_safe_branch(branch) {
                    return branch.to_string();
                }
            }
        }
    }
    // Fallback: check if "main" or "master" exists.
    for candidate in &["main", "master"] {
        let check = tokio::process::Command::new("git")
            .args(["rev-parse", "--verify", candidate])
            .current_dir(repo_root)
            .output()
            .await;
        if check.map(|o| o.status.success()).unwrap_or(false) {
            return candidate.to_string();
        }
    }
    "main".to_string()
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
// TaskSpec polling — picks up tasks created by create_task MCP tool
// ---------------------------------------------------------------------------

/// Scan all agents for pending TaskSpecs and dispatch them.
///
/// A TaskSpec is picked up if its `status` is `Planned`. It is immediately
/// marked `Running` to prevent double-pickup by concurrent poll cycles.
async fn poll_pending_taskspecs(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
) {
    let agents_dir = home_dir.join("agents");
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }
        let agent_name = match agent_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.starts_with('_') && !n.starts_with('.') => n.to_string(),
            _ => continue,
        };

        let task_ids = crate::task_spec::TaskSpec::list(&agent_dir);
        for task_id in task_ids {
            let mut spec = match crate::task_spec::TaskSpec::load(&agent_dir, &task_id) {
                Ok(s) => s,
                Err(_) => continue,
            };

            if spec.status != crate::task_spec::TaskStatus::Planned {
                continue;
            }

            info!(
                task = %spec.task_id,
                agent = %agent_name,
                goal = %spec.goal,
                steps = spec.steps.len(),
                "Picking up planned TaskSpec for execution"
            );

            // Clone for the spawned task.
            let home = home_dir.to_path_buf();
            let reg = registry.clone();
            let dir = agent_dir.clone();

            tokio::spawn(async move {
                if let Err(e) = dispatch_taskspec(&home, &reg, &mut spec, &dir).await {
                    warn!(
                        task = %spec.task_id,
                        error = %e,
                        "TaskSpec execution failed"
                    );
                }
            });
        }
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

            // Collect message_ids from non-first messages so their delegation
            // callbacks can also be consumed when the coalesced response arrives.
            let extra_ids: Vec<String> = chunk.iter().skip(1).map(|m| m.message_id.clone()).collect();

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
                coalesced_ids: extra_ids,
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

        duduclaw_core::platform::flock_exclusive(&file)
            .map_err(|e| format!("flock failed on {}: {e}", path.display()))?;

        writeln!(file, "{line}")
            .map_err(|e| format!("Failed to write to {}: {e}", path.display()))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}

// ── Delegation callback forwarding ────────────────────────────

/// Shared HTTP client for channel forwarding (avoids creating a new client per call).
static FORWARD_HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn forward_http() -> &'static reqwest::Client {
    FORWARD_HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    })
}

/// Clean up orphaned delegation callbacks older than 24 hours.
async fn cleanup_stale_delegation_callbacks(home_dir: &Path) {
    let db_path = home_dir.join("message_queue.db");
    if !db_path.exists() {
        return;
    }
    let cutoff = (Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
    let result = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path).ok()?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
        conn.execute(
            "DELETE FROM delegation_callbacks WHERE created_at < ?1",
            rusqlite::params![cutoff],
        ).ok()
    }).await;
    if let Ok(Some(count)) = result {
        if count > 0 {
            info!(removed = count, "Cleaned up stale delegation callbacks");
        }
    }
}

/// Check if a delegation callback exists for this message and forward
/// the response to the originating channel.
async fn forward_delegation_response(
    home_dir: &Path,
    original_message_id: &str,
    response_text: &str,
    responder_agent: &str,
) {
    let db_path = home_dir.join("message_queue.db");
    if !db_path.exists() {
        return;
    }

    // Atomically consume the callback (DELETE RETURNING — SQLite 3.35+)
    let msg_id = original_message_id.to_string();
    let callback = tokio::task::spawn_blocking(move || {
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(_) => return None,
        };
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");

        conn.query_row(
            "DELETE FROM delegation_callbacks WHERE message_id = ?1 \
             RETURNING message_id, agent_id, channel_type, channel_id, thread_id, retry_count",
            rusqlite::params![msg_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i32>(5)?,
                ))
            },
        ).ok()
    }).await.ok().flatten();

    let Some((_, callback_agent_id, channel_type, channel_id, thread_id, retry_count)) = callback else {
        return; // No callback registered — normal non-channel delegation
    };

    info!(
        channel = %channel_type,
        chat_id = %channel_id,
        thread = ?thread_id,
        responder = %responder_agent,
        "Forwarding sub-agent response to originating channel"
    );

    if let Err(e) = forward_to_channel(
        home_dir, &channel_type, &channel_id, thread_id.as_deref(),
        response_text, responder_agent,
    ).await {
        let next_retry = retry_count + 1;
        if next_retry >= 5 {
            warn!(
                channel = %channel_type,
                chat_id = %channel_id,
                error = %e,
                retries = retry_count,
                "Delegation callback forwarding permanently failed after 5 retries — dropping"
            );
        } else {
            warn!(
                channel = %channel_type,
                chat_id = %channel_id,
                error = %e,
                retry = next_retry,
                "Failed to forward delegation response — re-inserting callback for retry"
            );
            let db_path = home_dir.join("message_queue.db");
            let msg_id = original_message_id.to_string();
            let agent = callback_agent_id.clone();
            let ch_type = channel_type.clone();
            let ch_id = channel_id.clone();
            let tid = thread_id.clone();
            let now = chrono::Utc::now().to_rfc3339();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                    let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO delegation_callbacks \
                         (message_id, agent_id, channel_type, channel_id, thread_id, retry_count, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![msg_id, agent, ch_type, ch_id, tid, next_retry, now],
                    );
                }
            }).await;
        }
    }
}

/// Escape Telegram MarkdownV1 special characters to prevent formatting injection.
fn escape_markdown_v1(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
        .replace('[', "\\[")
}

/// Sanitize agent response text before forwarding to a public channel.
/// Strips internal paths, DUDUCLAW_SYSTEM markers, and other implementation details.
fn sanitize_for_channel(text: &str) -> String {
    text.lines()
        .filter(|line| !line.contains("[DUDUCLAW_SYSTEM:"))
        .map(|line| {
            // Redact absolute paths like /Users/xxx/... or /home/xxx/...
            if line.contains("/Users/") || line.contains("/home/") {
                let mut result = line.to_string();
                for prefix in &["/Users/", "/home/"] {
                    while let Some(start) = result.find(prefix) {
                        let end = result[start..].find(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ')')
                            .map(|e| start + e)
                            .unwrap_or(result.len());
                        result.replace_range(start..end, "[path]");
                    }
                }
                result
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Validate that a channel_id contains only safe characters (alphanumeric, hyphen, underscore).
/// Prevents path traversal and URL injection attacks when channel_id is used in API URLs.
fn validate_channel_id(channel_type: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("channel_id is empty".into());
    }
    // All channel IDs should be alphanumeric (with optional hyphens/underscores/dots for Slack timestamps)
    let valid = id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !valid {
        return Err(format!("channel_id contains invalid characters for {channel_type}: {id}"));
    }
    // Discord snowflakes: 17-20 digit numbers
    if channel_type == "discord" && !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("Discord channel_id must be numeric snowflake, got: {id}"));
    }
    // Telegram chat_id: signed integer (can be negative for groups)
    if channel_type == "telegram" && id.parse::<i64>().is_err() {
        return Err(format!("Telegram chat_id must be numeric, got: {id}"));
    }
    Ok(())
}

/// Send a message to a specific channel (Telegram/LINE/Discord/Slack).
/// Reads channel tokens from config.toml.
async fn forward_to_channel(
    home_dir: &Path,
    channel_type: &str,
    channel_id: &str,
    thread_id: Option<&str>,
    text: &str,
    responder_agent: &str,
) -> Result<(), String> {
    // Validate channel_id format to prevent URL injection / SSRF
    validate_channel_id(channel_type, channel_id)?;
    if let Some(tid) = thread_id {
        validate_channel_id(channel_type, tid)?;
    }

    // Read config for channel token
    let config_path = home_dir.join("config.toml");
    let config_str = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|e| format!("read config.toml: {e}"))?;
    let config: toml::Value = config_str
        .parse()
        .map_err(|e| format!("parse config.toml: {e}"))?;

    // Truncate long responses for channel delivery
    let max_len = match channel_type {
        "telegram" => 4096,
        "discord" => 2000,
        "line" => 5000,
        _ => 4096,
    };
    // Sanitize response text — strip internal paths and system markers before channel delivery
    let safe_text = sanitize_for_channel(text);

    // Escape agent name for Telegram MarkdownV1 to prevent formatting injection
    let safe_agent = escape_markdown_v1(responder_agent);
    let char_count = safe_text.chars().count();
    let display_text = if char_count > max_len {
        let truncated: String = safe_text.chars().take(max_len - 100).collect();
        format!(
            "📨 **{}** 的回報：\n\n{}…\n\n_(回應過長，已截斷)_",
            safe_agent, truncated
        )
    } else {
        format!("📨 **{}** 的回報：\n\n{}", safe_agent, safe_text)
    };

    let http = forward_http();

    match channel_type {
        "telegram" => {
            let token = get_config_token(&config, "telegram_bot_token_enc", "telegram_bot_token", home_dir);
            if token.is_empty() {
                return Err("telegram_bot_token not configured".into());
            }
            let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
            let mut payload = serde_json::json!({
                "chat_id": channel_id,
                "text": display_text,
                "parse_mode": "Markdown",
            });
            if let Some(tid) = thread_id {
                if let Ok(tid_num) = tid.parse::<i64>() {
                    payload["message_thread_id"] = serde_json::json!(tid_num);
                }
            }
            let resp = http.post(&url).json(&payload).send().await
                .map_err(|e| format!("telegram send: {e}"))?;
            if !resp.status().is_success() {
                // Retry without parse_mode in case Markdown causes issues
                let fallback_payload = serde_json::json!({
                    "chat_id": channel_id,
                    "text": &display_text,
                });
                match http.post(&url).json(&fallback_payload).send().await {
                    Ok(r) if !r.status().is_success() => {
                        warn!(status = %r.status(), "Telegram fallback retry also failed");
                    }
                    Err(e) => {
                        warn!(error = %e, "Telegram fallback retry network error");
                    }
                    _ => {}
                }
            }
        }
        "line" => {
            let token = get_config_token(&config, "line_channel_token_enc", "line_channel_token", home_dir);
            if token.is_empty() {
                return Err("line_channel_token not configured".into());
            }
            let url = "https://api.line.me/v2/bot/message/push";
            let resp = http.post(url)
                .header("Authorization", format!("Bearer {}", token))
                .json(&serde_json::json!({
                    "to": channel_id,
                    "messages": [{"type": "text", "text": display_text}]
                }))
                .send().await
                .map_err(|e| format!("line send: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("LINE API returned {}", resp.status()));
            }
        }
        "discord" => {
            let token = get_config_token(&config, "discord_bot_token_enc", "discord_bot_token", home_dir);
            if token.is_empty() {
                return Err("discord_bot_token not configured".into());
            }
            let target_channel = thread_id.unwrap_or(channel_id);
            let url = format!("https://discord.com/api/v10/channels/{}/messages", target_channel);
            let resp = http.post(&url)
                .header("Authorization", format!("Bot {}", token))
                .json(&serde_json::json!({ "content": display_text }))
                .send().await
                .map_err(|e| format!("discord send: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("Discord API returned {}", resp.status()));
            }
        }
        "slack" => {
            let token = get_config_token(&config, "slack_bot_token_enc", "slack_bot_token", home_dir);
            if token.is_empty() {
                return Err("slack_bot_token not configured".into());
            }
            let url = "https://slack.com/api/chat.postMessage";
            let resp = http.post(url)
                .header("Authorization", format!("Bearer {}", token))
                .json(&serde_json::json!({
                    "channel": channel_id,
                    "text": display_text,
                    "thread_ts": thread_id,
                }))
                .send().await
                .map_err(|e| format!("slack send: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("Slack API returned {}", resp.status()));
            }
        }
        "whatsapp" | "feishu" => {
            // These channels use webhook-based APIs that require more complex setup.
            // Log as unsupported for now — the callback is consumed to prevent orphans.
            warn!(channel = %channel_type, "Delegation callback forwarding not yet implemented for this channel");
        }
        other => {
            return Err(format!("unsupported channel type for forwarding: {other}"));
        }
    }

    info!(
        channel = %channel_type,
        chat_id = %channel_id,
        "Delegation response forwarded to channel successfully"
    );
    Ok(())
}

/// Read a channel token from config.toml `[channels]` table,
/// trying encrypted value first then plaintext fallback.
fn get_config_token(config: &toml::Value, enc_key: &str, plain_key: &str, home_dir: &Path) -> String {
    let channels = config.get("channels");
    // Try encrypted token first
    if let Some(enc) = channels.and_then(|c| c.get(enc_key)).and_then(|v| v.as_str()) {
        match crate::config_crypto::decrypt_value(enc, home_dir) {
            Some(decrypted) => return decrypted,
            None => warn!(key = enc_key, "Failed to decrypt channel token — falling back to plaintext"),
        }
    }
    // Fallback to plaintext
    channels.and_then(|c| c.get(plain_key)).and_then(|v| v.as_str()).unwrap_or("").to_string()
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
            coalesced_ids: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        // None fields with skip_serializing_if should be absent
        assert!(!json.contains("origin_agent"));
        assert!(!json.contains("sender_agent"));
        assert!(!json.contains("response"));
        assert!(!json.contains("coalesced_ids"));
        // delegation_depth is always serialized (no skip)
        assert!(json.contains("delegation_depth"));
    }

    #[test]
    fn depth_limit_constant_is_reasonable() {
        // Ensure MAX_DELEGATION_DEPTH is between 2 and 10
        let depth = MAX_DELEGATION_DEPTH;
        assert!(depth >= 2, "MAX_DELEGATION_DEPTH too small: {depth}");
        assert!(depth <= 10, "MAX_DELEGATION_DEPTH too large: {depth}");
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
                coalesced_ids: vec![],
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
                coalesced_ids: vec![],
            },
        ];
        let coalesced = coalesce_messages(msgs);
        assert_eq!(coalesced.len(), 1);
        // Should use the maximum depth from the chunk (4, not 2)
        assert_eq!(coalesced[0].delegation_depth, 4);
        // origin_agent and sender_agent come from first message
        assert_eq!(coalesced[0].origin_agent.as_deref(), Some("main"));
        assert_eq!(coalesced[0].sender_agent.as_deref(), Some("researcher"));
        // Coalesced should contain the second message's ID
        assert_eq!(coalesced[0].coalesced_ids, vec!["m2"]);
    }
}
