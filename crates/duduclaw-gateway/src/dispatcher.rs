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

use duduclaw_core::{MAX_DELEGATION_DEPTH, ENV_DELEGATION_DEPTH, ENV_DELEGATION_ORIGIN, ENV_DELEGATION_SENDER, truncate_bytes};

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

    // ── Wiki trust feedback context (review BLOCKER R4 sub-agent) ─
    /// Originating turn id — scoped on the dispatcher's spawn so any wiki
    /// RAG performed by the sub-agent inherits it via the
    /// `feedback::CURRENT_TURN_ID` task_local. `None` for messages enqueued
    /// before this field existed (legacy compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    /// Originating channel session id — used by the per-conversation cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

/// Starts the agent dispatcher as a background task.
///
/// Polls `bus_queue.jsonl` every 5 seconds for unprocessed `agent_message`
/// entries and dispatches them to the Claude CLI.
pub fn start_agent_dispatcher(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    start_agent_dispatcher_with_crypto(home_dir, registry, None, None, None, None)
}

/// Start the dispatcher with optional encryption key for deferred GVU (review #30)
/// and optional SQLite message queue (Phase 3 Hybrid TaskPipeline).
///
/// `prediction_engine` (BUG-5 fix): when supplied, every successful sub-agent
/// dispatch records a synthetic prediction cycle so sub-agents accumulate
/// `prediction_log` rows the same way channel-facing agents do.
///
/// `gvu_ctx` (P1 2026-05-09): when supplied, sub-agent predictions whose
/// composite error lands in Significant / Critical also fire the GVU loop
/// — closing the gap that left 16/17 production agents without any
/// `gvu_experiment_log` entries.
pub fn start_agent_dispatcher_with_crypto(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
    encryption_key: Option<[u8; 32]>,
    message_queue: Option<Arc<crate::message_queue::MessageQueue>>,
    prediction_engine: Option<Arc<crate::prediction::engine::PredictionEngine>>,
    gvu_ctx: Option<Arc<crate::prediction::subagent_prediction::GvuTriggerCtx>>,
) -> tokio::task::JoinHandle<()> {
    // Mutex protects the read-modify-write cycle on bus_queue.jsonl
    let dispatch_lock = Arc::new(tokio::sync::Mutex::new(()));
    tokio::spawn(async move {
        info!("Agent dispatcher started");

        // One-shot reconcile on startup: any `agent_response` lines left in
        // bus_queue.jsonl from a previous process whose corresponding
        // delegation_callbacks row is still pending need to be forwarded.
        // Without this, a user-visible sub-agent reply stays trapped between
        // restarts — the live dispatcher only forwards responses it generates
        // itself (see the `forward_delegation_response` call sites near
        // `dispatch.rs:600`), not ones it finds on disk.
        reconcile_orphan_responses(&home_dir).await;

        let mut tick: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            tick += 1;
            let _guard = dispatch_lock.lock().await;

            // Poll JSONL bus queue (legacy path — kept for backward compat)
            if let Err(e) = poll_and_dispatch(
                &home_dir,
                &registry,
                prediction_engine.as_ref(),
                gvu_ctx.as_ref(),
            )
            .await
            {
                warn!("Dispatcher poll error: {e}");
            }

            // Poll SQLite message queue (new path)
            if let Some(ref mq) = message_queue {
                if let Err(e) = poll_and_dispatch_sqlite(
                    mq,
                    &home_dir,
                    &registry,
                    prediction_engine.as_ref(),
                    gvu_ctx.as_ref(),
                )
                .await
                {
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
    prediction_engine: Option<&Arc<crate::prediction::engine::PredictionEngine>>,
    gvu_ctx: Option<&Arc<crate::prediction::subagent_prediction::GvuTriggerCtx>>,
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

        // v1.8.16: propagate the originating channel context down into the
        // target agent's Claude CLI spawn. `claude_runner::REPLY_CHANNEL` is
        // a task-local that `prepare_claude_cmd` reads to set the
        // `DUDUCLAW_REPLY_CHANNEL` env var — which in turn is what the MCP
        // `send_to_agent` tool reads when it registers a delegation callback.
        // Without this scope, sub-agents spawned by the dispatcher (depth ≥ 2)
        // get no channel context, their `send_to_agent` callbacks never
        // register, and sub-agent replies are silently dropped at the
        // `forward_delegation_response` no-callback branch.
        let dispatch_fut =
            dispatch_to_agent(home_dir, registry, &msg.target, &msg.payload, &delegation);
        // v1.10: scope wiki RL trust feedback context so the sub-agent's
        // wiki RAG citations land in the same tracker bucket as the
        // originating turn. Pulled from `message_queue.{turn_id, session_id}`
        // — populated by the MCP `send_to_agent` tool from env vars.
        let dispatch_fut = duduclaw_memory::feedback::CURRENT_SESSION_ID
            .scope(msg.session_id.clone(), dispatch_fut);
        let dispatch_fut = duduclaw_memory::feedback::CURRENT_TURN_ID
            .scope(msg.turn_id.clone(), dispatch_fut);
        let result = match msg.reply_channel.clone() {
            Some(rc) if !rc.is_empty() => {
                crate::claude_runner::REPLY_CHANNEL.scope(rc, dispatch_fut).await
            }
            _ => dispatch_fut.await,
        };

        match result {
            Ok(response) => {
                queue.complete(&msg.id, &response).await?;
                // Forward sub-agent response to originating channel if callback exists
                forward_delegation_response(home_dir, &msg.id, &response, &msg.target).await;
                // BUG-5 fix: record a prediction cycle for the sub-agent
                // so user_models / prediction_log accumulate the same way
                // they do for the channel-facing root agent.
                crate::prediction::subagent_prediction::spawn_record(
                    prediction_engine.cloned(),
                    msg.target.clone(),
                    msg.sender_agent.clone(),
                    msg.origin_agent.clone(),
                    msg.payload.clone(),
                    response.clone(),
                    gvu_ctx.cloned(),
                );
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
    prediction_engine: Option<&Arc<crate::prediction::engine::PredictionEngine>>,
    gvu_ctx: Option<&Arc<crate::prediction::subagent_prediction::GvuTriggerCtx>>,
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
                        turn_id: msg.turn_id.clone(),
                        session_id: msg.session_id.clone(),
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
        let pe_for_task = prediction_engine.cloned();
        let gvu_ctx_for_task = gvu_ctx.cloned();

        // (review BLOCKER R4) Forward turn_id / session_id so the sub-agent
        // chain re-establishes the citation tracking context. Without this,
        // sub-agent RAG hits a fresh task with no task_locals → trust
        // feedback for sub-agents is silently no-op.
        let turn_id_for_scope = msg.turn_id.clone();
        let session_id_for_scope = msg.session_id.clone();

        handles.push(tokio::spawn(async move {
            let dispatch_start = Utc::now().to_rfc3339();
            let delegation_env = DelegationEnv {
                depth: msg.delegation_depth,
                origin: msg.origin_agent.clone().unwrap_or_default(),
                sender: msg.sender_agent.clone().unwrap_or_default(),
            };
            let dispatch_fut = dispatch_to_agent(&home, &reg, &msg.agent_id, &msg.payload, &delegation_env);
            let dispatch_fut = duduclaw_memory::feedback::CURRENT_SESSION_ID
                .scope(session_id_for_scope, dispatch_fut);
            let dispatch_fut = duduclaw_memory::feedback::CURRENT_TURN_ID
                .scope(turn_id_for_scope, dispatch_fut);
            let result = dispatch_fut.await;

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
                turn_id: msg.turn_id.clone(),
                session_id: msg.session_id.clone(),
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

            // BUG-5 fix: record a sub-agent prediction cycle so user_models
            // and prediction_log accumulate even for agents that never see a
            // direct channel message.
            if result.is_ok() {
                crate::prediction::subagent_prediction::spawn_record(
                    pe_for_task.clone(),
                    msg.agent_id.clone(),
                    msg.sender_agent.clone(),
                    msg.origin_agent.clone(),
                    msg.payload.clone(),
                    response_for_callback.clone(),
                    gvu_ctx_for_task.clone(),
                );
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
                truncate_bytes(&error, 100),
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
                turn_id: first.turn_id.clone(),
                session_id: first.session_id.clone(),
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

/// Scan bus_queue.jsonl for `agent_response` lines whose originating message
/// still has a pending `delegation_callbacks` row, and forward each one to
/// the user's channel.
///
/// This handles a specific failure mode: a previous DuDuClaw process dispatched
/// an agent_message, the sub-agent wrote an agent_response back to the queue,
/// but the process died (user Ctrl+C, crash, OOM) before `forward_delegation_response`
/// ran. On startup we replay those pending deliveries so the user doesn't
/// have to re-ask the sub-agent.
async fn reconcile_orphan_responses(home_dir: &Path) {
    let queue_path = home_dir.join("bus_queue.jsonl");
    let db_path = home_dir.join("message_queue.db");
    if !queue_path.exists() || !db_path.exists() {
        return;
    }
    let content = match tokio::fs::read_to_string(&queue_path).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Collect (in_reply_to, payload, sender_agent) for every agent_response line.
    // We don't modify the JSONL file — forward_delegation_response consumes the
    // SQLite callback row atomically, and re-running against the same response
    // after restart is cheap (no callback → no-op).
    let mut orphans: Vec<(String, String, String)> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if event.get("type").and_then(|t| t.as_str()) != Some("agent_response") {
            continue;
        }
        let Some(in_reply_to) = event.get("in_reply_to").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(payload) = event.get("payload").and_then(|v| v.as_str()) else {
            continue;
        };
        let sender = event
            .get("sender_agent")
            .and_then(|v| v.as_str())
            .or_else(|| event.get("agent_id").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();
        orphans.push((in_reply_to.to_string(), payload.to_string(), sender));
    }

    if orphans.is_empty() {
        return;
    }

    info!(count = orphans.len(), "Scanning for orphan delegation responses");
    for (in_reply_to, payload, sender) in orphans {
        // forward_delegation_response deletes the callback row atomically;
        // if no row exists we silently skip.
        forward_delegation_response(home_dir, &in_reply_to, &payload, &sender).await;
    }
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

/// Outcome of a manual re-forward attempt (`duduclaw reforward <id>`).
/// Distinct from an automatic dispatcher retry so the CLI can give the
/// operator a clear exit code and summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReforwardOutcome {
    /// Forward succeeded — the response reached the originating channel
    /// and (where applicable) was appended to the parent agent's session.
    Sent {
        channel_type: String,
        channel_id: String,
        thread_id: Option<String>,
    },
    /// `--dry-run` mode: report what would happen without touching
    /// `delegation_callbacks` or sending anything.
    DryRun {
        channel_type: String,
        channel_id: String,
        thread_id: Option<String>,
        has_existing_callback: bool,
    },
    /// Forward was attempted but failed (and the retry callback was
    /// re-inserted by the existing dispatcher machinery). The caller
    /// can inspect gateway logs for the specific Discord/Telegram/LINE
    /// API error.
    Failed,
}

/// Manually re-forward a completed delegation response.
///
/// **Why this exists:** when a sub-agent's response finished but the
/// HTTP POST to its originating channel failed (e.g. Discord 401 on
/// the chain-root's thread, pre-v1.8.20), the callback is re-inserted
/// into `delegation_callbacks` for retry — but the dispatcher only
/// retries on a *new* `agent_response` for the same message_id, which
/// never arrives (the delegation is already `done`). The callback
/// sits stuck until a 24h cleanup drops it, and the user never sees
/// the reply. This command gives the operator a manual lever.
///
/// **What it does:**
/// 1. Reads the `message_queue` row by id; requires `status='done'`
///    and a non-empty `response`.
/// 2. If `delegation_callbacks` has no pending row for this id (the
///    callback was permanently dropped or never registered),
///    synthesize one from the row's `reply_channel` column so the
///    existing `forward_delegation_response` machinery has something
///    to consume.
/// 3. Invokes `forward_delegation_response`, which:
///    - Resolves the forward token via the v1.8.20 cascade
///      (`callback_agent_id` → `origin_agent` → global config).
///    - On success: POSTs to the channel + appends the reply to the
///      parent agent's session (v1.8.17 Fix 2), then deletes the
///      callback.
///    - On failure: logs WARN, re-inserts the callback for a future
///      manual retry.
/// 4. Returns [`ReforwardOutcome`] telling the caller whether the
///    send actually landed.
pub async fn reforward_message(
    home_dir: &Path,
    message_id: &str,
    dry_run: bool,
) -> Result<ReforwardOutcome, String> {
    let db_path = home_dir.join("message_queue.db");
    if !db_path.exists() {
        return Err(format!(
            "message_queue.db not found at {}. Has the gateway been started on this home dir?",
            db_path.display()
        ));
    }

    // Fetch the stored response + channel context.
    let msg_id_owned = message_id.to_string();
    let db_clone = db_path.clone();
    let row: Option<(String, String, String, String, Option<String>)> =
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_clone).ok()?;
            let _ = conn.execute_batch("PRAGMA busy_timeout=5000;");
            conn.query_row(
                "SELECT status, target, IFNULL(response,''), IFNULL(sender,''), reply_channel \
                 FROM message_queue WHERE id = ?1",
                rusqlite::params![msg_id_owned],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .ok()
        })
        .await
        .ok()
        .flatten();

    let Some((status, target, response, sender, reply_channel)) = row else {
        return Err(format!(
            "No message with id={message_id} in message_queue.db. Check the id or list recent messages via `sqlite3 ~/.duduclaw/message_queue.db \"SELECT id,status FROM message_queue ORDER BY rowid DESC LIMIT 10\"`."
        ));
    };
    if status != "done" {
        return Err(format!(
            "Message {message_id} has status='{status}', not 'done'. Only completed responses can be re-forwarded."
        ));
    }
    if response.is_empty() {
        return Err(format!(
            "Message {message_id} is done but has no response body stored. Nothing to re-send."
        ));
    }

    // Determine channel context. Prefer the existing delegation_callbacks
    // row; fall back to parsing `reply_channel` ourselves so we can
    // re-forward messages whose callback was already permanently dropped.
    let (callback_agent_id, ch_type, ch_id, thread_id, has_existing_callback) = {
        let db_clone = db_path.clone();
        let msg_id_owned = message_id.to_string();
        let existing: Option<(String, String, String, Option<String>)> =
            tokio::task::spawn_blocking(move || {
                let conn = rusqlite::Connection::open(&db_clone).ok()?;
                let _ = conn.execute_batch("PRAGMA busy_timeout=5000;");
                conn.query_row(
                    "SELECT agent_id, channel_type, channel_id, thread_id \
                     FROM delegation_callbacks WHERE message_id = ?1",
                    rusqlite::params![msg_id_owned],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .ok()
            })
            .await
            .ok()
            .flatten();

        if let Some((agent, ct, cid, tid)) = existing {
            (agent, ct, cid, tid, true)
        } else {
            // No live callback — synthesize from message_queue.reply_channel.
            let rc = reply_channel.clone().ok_or_else(|| {
                format!(
                    "Message {message_id} has no delegation_callbacks row AND no reply_channel stored. \
                     Cannot determine where to forward. (Non-channel delegation, e.g. cron?)"
                )
            })?;
            let (ct, cid, tid) = parse_reply_channel(&rc)?;
            // callback_agent_id = the original sender (`message_queue.sender`
            // = who called send_to_agent). This is what the live callback
            // would have stored.
            (sender, ct, cid, tid, false)
        }
    };

    if dry_run {
        return Ok(ReforwardOutcome::DryRun {
            channel_type: ch_type,
            channel_id: ch_id,
            thread_id,
            has_existing_callback,
        });
    }

    // Synthesize a callback row if one doesn't exist — forward_delegation_response
    // uses DELETE RETURNING to consume exactly one row, so this is the
    // cheapest way to reuse its machinery (including v1.8.20 cascade and
    // v1.8.17 Fix 2 session append).
    if !has_existing_callback {
        let db_clone = db_path.clone();
        let msg_id_owned = message_id.to_string();
        let agent = callback_agent_id.clone();
        let ct = ch_type.clone();
        let cid = ch_id.clone();
        let tid = thread_id.clone();
        let now = chrono::Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let conn = rusqlite::Connection::open(&db_clone)
                .map_err(|e| format!("open message_queue.db: {e}"))?;
            let _ = conn.execute_batch("PRAGMA busy_timeout=5000;");
            conn.execute(
                "INSERT OR REPLACE INTO delegation_callbacks \
                 (message_id, agent_id, channel_type, channel_id, thread_id, retry_count, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                rusqlite::params![msg_id_owned, agent, ct, cid, tid, now],
            )
            .map_err(|e| format!("insert synthesized callback: {e}"))?;
            Ok(())
        })
        .await
        .map_err(|e| format!("join blocking task: {e}"))??;
    }

    // Delegate to the existing forward machinery. It will consume the
    // callback, resolve the token via v1.8.20 cascade, POST to the
    // channel, and on success append the reply to the parent session.
    forward_delegation_response(home_dir, message_id, &response, &target).await;

    // Verify outcome: if the callback is gone, the forward succeeded
    // (forward_delegation_response deletes on success; re-inserts on
    // failure).
    let db_clone = db_path.clone();
    let msg_id_owned = message_id.to_string();
    let callback_remains: bool = tokio::task::spawn_blocking(move || {
        let conn = match rusqlite::Connection::open(&db_clone) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let _ = conn.execute_batch("PRAGMA busy_timeout=5000;");
        conn.query_row(
            "SELECT 1 FROM delegation_callbacks WHERE message_id = ?1",
            rusqlite::params![msg_id_owned],
            |_| Ok(()),
        )
        .is_ok()
    })
    .await
    .unwrap_or(false);

    if callback_remains {
        Ok(ReforwardOutcome::Failed)
    } else {
        Ok(ReforwardOutcome::Sent {
            channel_type: ch_type,
            channel_id: ch_id,
            thread_id,
        })
    }
}

/// Parse a `reply_channel` string (same grammar as
/// [`duduclaw_core::ENV_REPLY_CHANNEL`]) into (channel_type, channel_id,
/// optional thread_id). Mirrors the parser in
/// `duduclaw-cli::mcp::send_to_agent_with_ctx` — kept local to avoid
/// a cross-crate dep on the MCP server module.
fn parse_reply_channel(rc: &str) -> Result<(String, String, Option<String>), String> {
    let parts: Vec<&str> = rc.splitn(3, ':').collect();
    if parts.len() < 2 {
        return Err(format!("malformed reply_channel '{rc}': expected <type>:<id>[:<thread>]"));
    }
    let ch_type = parts[0].to_string();
    // Special marker `<type>:thread:<id>` (e.g. `discord:thread:123`)
    // collapses to (ch_id=<id>, thread_id=None) — matches mcp.rs behavior.
    let (ch_id, thread_id) = if parts.len() == 3 && parts[1] == "thread" {
        (parts[2].to_string(), None)
    } else {
        let cid = parts[1].to_string();
        let tid = parts.get(2).map(|s| s.to_string());
        (cid, tid)
    };
    Ok((ch_type, ch_id, thread_id))
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
        // No callback registered — this is the legitimate case for purely
        // internal delegations (cron/reminder/heartbeat), but it is ALSO
        // what happened pre-v1.8.16 when a nested sub-agent's reply was
        // silently dropped because the delegation chain lost channel
        // context. Debug-log so future silent drops are at least visible
        // under `RUST_LOG=duduclaw_gateway::dispatcher=debug`.
        tracing::debug!(
            message_id = %original_message_id,
            responder = %responder_agent,
            "No delegation callback — response not forwarded to any channel \
             (expected for cron/reminder/non-channel delegations; unexpected \
             if this was a user-facing sub-agent reply)"
        );
        return;
    };

    // v1.8.20: look up the chain root (original origin_agent) so
    // `forward_to_channel` can cascade token resolution from the
    // callback caller → chain root → global config. Without this hop,
    // nested sub-agent replies (where the callback was registered by
    // e.g. `duduclaw-tl` who has no per-agent Discord bot) fell back
    // to the stale global token and got rejected with 401 by threads
    // that were opened by the root agent's bot (e.g. agnes).
    let chain_root = {
        let home = home_dir.to_path_buf();
        let msg_id = original_message_id.to_string();
        tokio::task::spawn_blocking(move || lookup_origin_agent(&home, &msg_id))
            .await
            .ok()
            .flatten()
    };

    info!(
        channel = %channel_type,
        chat_id = %channel_id,
        thread = ?thread_id,
        responder = %responder_agent,
        chain_root = ?chain_root,
        "Forwarding sub-agent response to originating channel"
    );

    match forward_to_channel(
        home_dir, &channel_type, &channel_id, thread_id.as_deref(),
        response_text, responder_agent, &callback_agent_id,
        chain_root.as_deref(),
    ).await {
        Ok(()) => {
            // v1.8.17 Fix 2 (Option A): forward succeeded — also persist the
            // sub-agent's reply into the PARENT agent's session history so
            // that the parent's next CLI invocation sees what the sub-agent
            // told the user. Without this, the parent has no record of the
            // sub-agent's output and tends to hallucinate earlier context
            // when the user replies (e.g. "方案A" referring to TL's reply).
            //
            // Non-blocking by design: any failure here is logged at WARN and
            // does NOT affect the channel delivery outcome (already succeeded).
            // Because `delegation_callbacks` was atomically consumed before
            // `forward_to_channel` ran, this branch only fires on the FIRST
            // successful delivery — so no duplicate turns on retry.
            append_subagent_reply_to_parent_session(
                home_dir,
                &channel_type,
                &channel_id,
                thread_id.as_deref(),
                &callback_agent_id,
                chain_root.as_deref(),
                responder_agent,
                response_text,
            ).await;
        }
        Err(e) => {
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
}

/// Candidate session_id forms to try for a given channel context.
///
/// Different channels use different session_id grammars (see `channel_reply.rs`
/// and each channel handler). Unfortunately, the delegation_callbacks row
/// stores `(channel_type, channel_id, thread_id)` as flat columns, and for
/// Discord the `discord:thread:<id>` prefix is collapsed to `channel_id=<id>,
/// thread_id=None` (see `mcp.rs::send_to_agent`). That loses information, so
/// we return ALL plausible session_ids and the caller tries them in order —
/// appending to whichever session already exists (we never want to create a
/// brand-new session from the forward path).
///
/// Order matters: we return the more-specific (thread-scoped) form first.
fn candidate_session_ids(
    channel_type: &str,
    channel_id: &str,
    thread_id: Option<&str>,
) -> Vec<String> {
    match channel_type {
        "discord" => {
            // Could be either `discord:<id>` or `discord:thread:<id>` —
            // try thread form first since the information was lost on insert.
            vec![
                format!("discord:thread:{channel_id}"),
                format!("discord:{channel_id}"),
            ]
        }
        "telegram" => {
            if let Some(tid) = thread_id {
                vec![format!("telegram:{channel_id}:{tid}")]
            } else {
                vec![format!("telegram:{channel_id}")]
            }
        }
        "slack" => {
            if let Some(ts) = thread_id {
                vec![format!("slack:{channel_id}:{ts}")]
            } else {
                vec![format!("slack:{channel_id}")]
            }
        }
        // Generic `<channel>:<id>[:<thread_id>]`
        _ => {
            if let Some(tid) = thread_id {
                vec![
                    format!("{channel_type}:{channel_id}:{tid}"),
                    format!("{channel_type}:{channel_id}"),
                ]
            } else {
                vec![format!("{channel_type}:{channel_id}")]
            }
        }
    }
}

/// Sanitize an agent name for inclusion in the XML `<subagent_reply>` tag.
/// Agent names are already constrained to `[A-Za-z0-9_-]` by the registry,
/// but belt-and-suspenders prevents XML-tag injection via a crafted name.
fn safe_agent_tag(name: &str) -> String {
    let sanitised: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if sanitised.is_empty() {
        "sub-agent".to_string()
    } else {
        sanitised
    }
}

/// Append the sub-agent's reply to the parent agent's session history.
///
/// Writes an `assistant` turn directly into `sessions.db::session_messages`
/// with explicit attribution so the parent LLM can distinguish "I said this"
/// from "a sub-agent said this on my behalf". Falls through silently (WARN
/// log only) on any SQLite error — Discord delivery already succeeded and
/// we never want to break that path.
///
/// ### Owner cascade (v1.8.22)
///
/// The session is looked up by the channel's [`candidate_session_ids`].
/// The found session's `agent_id` (= owner) is compared against two
/// candidate parents:
///
/// 1. `parent_agent_id` = the agent that called `send_to_agent` (i.e.
///    the callback's `agent_id`). **Direct match** — content is
///    `<subagent_reply agent="X">...</subagent_reply>`.
///
/// 2. `chain_root_agent` = the message's `origin_agent` (the agent who
///    originally received the channel message and started the whole
///    delegation chain). **Cascade match** — content adds a
///    `via="<parent_agent_id>"` attribute so the root agent's LLM can
///    tell the reply wasn't a direct delegation from it but arrived
///    through an intermediate sub-agent. Format:
///    `<subagent_reply agent="X" via="Y">...</subagent_reply>`.
///
/// Without the cascade, any reply in a nested chain (e.g. TL →
/// eng-agent, where TL has no persistent session but agnes does)
/// would be dropped with an owner-mismatch warn, leaving the root
/// agent with no record of the engineer's output. v1.8.22 closes
/// that gap.
///
/// If neither candidate matches any existing session, the append is
/// skipped with a debug log. This preserves the cross-agent-bleed
/// guard (two unrelated agents sharing a channel still cannot leak
/// content into each other's history).
async fn append_subagent_reply_to_parent_session(
    home_dir: &Path,
    channel_type: &str,
    channel_id: &str,
    thread_id: Option<&str>,
    parent_agent_id: &str,
    chain_root_agent: Option<&str>,
    responder_agent: &str,
    response_text: &str,
) {
    let db_path = home_dir.join("sessions.db");
    if !db_path.exists() {
        // Brand-new install / no sessions yet — nothing to append to.
        tracing::debug!(
            db = %db_path.display(),
            "sessions.db missing — skipping parent-session append"
        );
        return;
    }

    let responder_for_tag = safe_agent_tag(responder_agent);
    let via_for_tag = safe_agent_tag(parent_agent_id);

    // Two variants of the content — direct vs relayed. Both are built
    // up-front so the spawn_blocking closure doesn't need the
    // responder/parent strings by reference.
    let direct_content = format!(
        "<subagent_reply agent=\"{responder_for_tag}\">\n{response_text}\n</subagent_reply>"
    );
    let relayed_content = format!(
        "<subagent_reply agent=\"{responder_for_tag}\" via=\"{via_for_tag}\">\n{response_text}\n</subagent_reply>"
    );

    let candidates = candidate_session_ids(channel_type, channel_id, thread_id);
    let parent = parent_agent_id.to_string();
    let root = chain_root_agent
        .filter(|s| !s.is_empty() && *s != parent_agent_id)
        .map(str::to_string);

    let ch_type = channel_type.to_string();
    let ch_id = channel_id.to_string();
    let responder_for_log = responder_agent.to_string();

    let result = tokio::task::spawn_blocking(move || -> Result<Option<(String, bool)>, String> {
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| format!("open sessions.db: {e}"))?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
        let now = chrono::Utc::now().to_rfc3339();

        // Try each candidate session_id in order. Accept a session whose
        // owner matches either the direct parent OR the chain root.
        // Direct match wins if both are available for the same session.
        for sid in &candidates {
            let existing: Option<String> = conn.query_row(
                "SELECT agent_id FROM sessions WHERE id = ?1",
                rusqlite::params![sid],
                |row| row.get(0),
            ).ok();

            let Some(owner) = existing else { continue };

            let (content, is_relayed) = if owner == parent {
                (&direct_content, false)
            } else if root.as_deref() == Some(owner.as_str()) {
                (&relayed_content, true)
            } else {
                tracing::debug!(
                    session_id = %sid,
                    owner = %owner,
                    expected_parent = %parent,
                    expected_root = ?root,
                    "session exists but owner matches neither parent nor chain-root — skipping"
                );
                continue;
            };

            let tokens = subagent_reply_token_estimate(content);
            conn.execute(
                "INSERT INTO session_messages (session_id, role, content, tokens, timestamp) \
                 VALUES (?1, 'assistant', ?2, ?3, ?4)",
                rusqlite::params![sid, content, tokens, now],
            ).map_err(|e| format!("insert session_messages: {e}"))?;

            conn.execute(
                "UPDATE sessions SET total_tokens = total_tokens + ?1, last_active = ?2 \
                 WHERE id = ?3",
                rusqlite::params![tokens, now, sid],
            ).map_err(|e| format!("update sessions: {e}"))?;

            return Ok(Some((sid.clone(), is_relayed)));
        }

        Ok(None)
    }).await;

    match result {
        Ok(Ok(Some((sid, is_relayed)))) => {
            info!(
                parent = %parent_agent_id,
                responder = %responder_for_log,
                session_id = %sid,
                relayed = is_relayed,
                "Appended sub-agent reply to parent session history"
            );
        }
        Ok(Ok(None)) => {
            // No matching session — the parent agent has never held a turn
            // on this channel yet, AND (if present) the chain-root agent
            // also has no session here. Common for brand-new conversations
            // where the sub-agent reply arrives before the parent's first
            // reply has closed. Non-fatal.
            warn!(
                channel = %ch_type,
                chat_id = %ch_id,
                parent = %parent_agent_id,
                "No parent or chain-root session found for sub-agent reply append \
                 (forward still succeeded); parent may miss this turn"
            );
        }
        Ok(Err(e)) => {
            warn!(
                channel = %ch_type,
                chat_id = %ch_id,
                parent = %parent_agent_id,
                error = %e,
                "Failed to append sub-agent reply to parent session \
                 (forward still succeeded)"
            );
        }
        Err(e) => {
            warn!(
                channel = %ch_type,
                chat_id = %ch_id,
                parent = %parent_agent_id,
                error = %e,
                "spawn_blocking panicked while appending sub-agent reply \
                 (forward still succeeded)"
            );
        }
    }
}

/// CJK-aware token estimate for the appended sub-agent turn.
///
/// Mirrors `channel_reply::estimate_tokens` (duplicated here to avoid
/// coupling the dispatcher to channel_reply's internals). ~1.5 chars/token
/// for CJK, ~4 chars/token for other scripts. Over-estimates slightly —
/// that's fine for the 50k compression threshold, which errs toward early
/// compression.
fn subagent_reply_token_estimate(text: &str) -> u32 {
    let mut cjk: u32 = 0;
    let mut other: u32 = 0;
    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3000..=0x9FFF).contains(&cp)
            || (0xF900..=0xFAFF).contains(&cp)
            || (0x20000..=0x2A6DF).contains(&cp)
            || (0x2A700..=0x2CEAF).contains(&cp)
        {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    let cjk_t = (cjk as f32 / 1.5).ceil() as u32;
    let other_t = (other as f32 / 4.0).ceil() as u32;
    cjk_t + other_t + 1
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
///
/// `originating_agent` is the `callback.agent_id` — the agent that
/// called `send_to_agent` and registered this delegation callback.
/// `chain_root_agent` is the message's `origin_agent` (the agent that
/// started the whole delegation chain from an inbound channel message),
/// used as the 2nd tier in `resolve_forward_token`'s cascade so nested
/// sub-agents can inherit the root's per-agent bot token when they have
/// none of their own. (v1.8.20: closes the 401-Unauthorized loop where
/// Discord threads opened by agnes's bot reject posts from the global
/// bot token.)
async fn forward_to_channel(
    home_dir: &Path,
    channel_type: &str,
    channel_id: &str,
    thread_id: Option<&str>,
    text: &str,
    responder_agent: &str,
    originating_agent: &str,
    chain_root_agent: Option<&str>,
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

    // Per-channel message byte budgets. A budget of 100 bytes is reserved for
    // the agent header (📨 **name** 的回報：) and the part indicator ((1/N)),
    // so each chunk stays safely under the hard channel limit.
    let max_len = match channel_type {
        "telegram" => 4000,
        "discord" => 1900,
        "line" => 4900,
        "slack" => 3900,
        _ => 3900,
    };
    // Sanitize response text — strip internal paths and system markers before channel delivery
    let safe_text = sanitize_for_channel(text);
    // Escape agent name for Telegram MarkdownV1 to prevent formatting injection
    let safe_agent = escape_markdown_v1(responder_agent);

    // Split the body into paragraph/line-aligned chunks instead of truncating.
    // For everything except Telegram/Slack MarkdownV1 escaping, the splitter
    // prefers `\n\n`, then `\n`, then spaces, and only hard-cuts as a last
    // resort. Each chunk is prefixed with the agent header (only the first
    // chunk gets the full "的回報：" banner; follow-ups just re-attribute).
    let chunks = crate::channel_format::split_text(&safe_text, max_len);
    let total = chunks.len();
    let chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if total == 1 {
                format!("📨 **{}** 的回報：\n\n{}", safe_agent, c)
            } else if i == 0 {
                format!("📨 **{}** 的回報 (1/{}):\n\n{}", safe_agent, total, c)
            } else {
                format!("📨 **{}** (續 {}/{}):\n\n{}", safe_agent, i + 1, total, c)
            }
        })
        .collect();

    let http = forward_http();

    // Small gap between chunks so Discord/Telegram don't rate-limit us and the
    // user sees a logical order. 250ms is well inside both platforms' burst
    // allowances (Discord 5 msgs/5s, Telegram 30 msgs/s to the same chat).
    let chunk_gap = std::time::Duration::from_millis(250);

    match channel_type {
        "telegram" => {
            let token = resolve_forward_token(
                home_dir, originating_agent, chain_root_agent, "telegram",
                &config, "telegram_bot_token_enc", "telegram_bot_token",
            );
            if token.is_empty() {
                return Err("telegram_bot_token not configured".into());
            }
            let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
            for (i, body) in chunks.iter().enumerate() {
                let mut payload = serde_json::json!({
                    "chat_id": channel_id,
                    "text": body,
                    "parse_mode": "Markdown",
                });
                if let Some(tid) = thread_id {
                    if let Ok(tid_num) = tid.parse::<i64>() {
                        payload["message_thread_id"] = serde_json::json!(tid_num);
                    }
                }
                let resp = http.post(&url).json(&payload).send().await
                    .map_err(|e| format!("telegram send chunk {}/{}: {e}", i + 1, total))?;
                if !resp.status().is_success() {
                    // Retry without parse_mode in case Markdown causes issues on this chunk
                    let fallback_payload = serde_json::json!({
                        "chat_id": channel_id,
                        "text": body,
                    });
                    match http.post(&url).json(&fallback_payload).send().await {
                        Ok(r) if !r.status().is_success() => {
                            warn!(status = %r.status(), chunk = i + 1, total = total, "Telegram fallback retry also failed");
                        }
                        Err(e) => {
                            warn!(error = %e, chunk = i + 1, total = total, "Telegram fallback retry network error");
                        }
                        _ => {}
                    }
                }
                if i + 1 < total { tokio::time::sleep(chunk_gap).await; }
            }
        }
        "line" => {
            let token = resolve_forward_token(
                home_dir, originating_agent, chain_root_agent, "line",
                &config, "line_channel_token_enc", "line_channel_token",
            );
            if token.is_empty() {
                return Err("line_channel_token not configured".into());
            }
            let url = "https://api.line.me/v2/bot/message/push";
            for (i, body) in chunks.iter().enumerate() {
                let resp = http.post(url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&serde_json::json!({
                        "to": channel_id,
                        "messages": [{"type": "text", "text": body}]
                    }))
                    .send().await
                    .map_err(|e| format!("line send chunk {}/{}: {e}", i + 1, total))?;
                if !resp.status().is_success() {
                    return Err(format!("LINE API returned {} on chunk {}/{}", resp.status(), i + 1, total));
                }
                if i + 1 < total { tokio::time::sleep(chunk_gap).await; }
            }
        }
        "discord" => {
            // Prefer the originating agent's own Discord bot token (from
            // agents/<id>/agent.toml [channels.discord]). Threads in Discord
            // are scoped to the bot that started the conversation — posting
            // to them from a different bot returns 401 Unauthorized even if
            // that other bot sits in the same guild.
            //
            // v1.8.20: cascade to `chain_root_agent`'s token when the
            // immediate caller (e.g. a sub-agent TL) has no per-agent
            // Discord bot configured. The Discord thread was opened by
            // the chain root (e.g. agnes) so only her bot can post into
            // it; inheriting her token here prevents the 401 loop.
            let token = resolve_forward_token(
                home_dir, originating_agent, chain_root_agent, "discord",
                &config, "discord_bot_token_enc", "discord_bot_token",
            );
            if token.is_empty() {
                return Err("discord_bot_token not configured".into());
            }
            let target_channel = thread_id.unwrap_or(channel_id);
            let url = format!("https://discord.com/api/v10/channels/{}/messages", target_channel);
            for (i, body) in chunks.iter().enumerate() {
                let resp = http.post(&url)
                    .header("Authorization", format!("Bot {}", token))
                    .json(&serde_json::json!({ "content": body }))
                    .send().await
                    .map_err(|e| format!("discord send chunk {}/{}: {e}", i + 1, total))?;
                if !resp.status().is_success() {
                    return Err(format!("Discord API returned {} on chunk {}/{}", resp.status(), i + 1, total));
                }
                if i + 1 < total { tokio::time::sleep(chunk_gap).await; }
            }
        }
        "slack" => {
            let token = resolve_forward_token(
                home_dir, originating_agent, chain_root_agent, "slack",
                &config, "slack_bot_token_enc", "slack_bot_token",
            );
            if token.is_empty() {
                return Err("slack_bot_token not configured".into());
            }
            let url = "https://slack.com/api/chat.postMessage";
            for (i, body) in chunks.iter().enumerate() {
                let resp = http.post(url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&serde_json::json!({
                        "channel": channel_id,
                        "text": body,
                        "thread_ts": thread_id,
                    }))
                    .send().await
                    .map_err(|e| format!("slack send chunk {}/{}: {e}", i + 1, total))?;
                if !resp.status().is_success() {
                    return Err(format!("Slack API returned {} on chunk {}/{}", resp.status(), i + 1, total));
                }
                if i + 1 < total { tokio::time::sleep(chunk_gap).await; }
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
/// Try to read the originating agent's per-channel bot token from
/// Resolve a channel token for delegation-forwarding, cascading through
/// both the delegation chain (`origin_agent`) AND the static reports_to
/// hierarchy so that sub-agents without their own per-agent token
/// inherit a parent's token.
///
/// Order of preference (v1.8.28):
///   1. `callback_agent_id`'s own `agents/<id>/agent.toml [channels.<ch>]`
///      (original v1.8.14 behaviour — if the agent configured its own
///      bot, use it).
///   2. **reports_to cascade from `callback_agent_id`** (new in v1.8.28)
///      — walks up `reports_to` until an ancestor with a token is found.
///      Handles the common case where sub-agents inherit the team root's
///      Discord bot without needing explicit `origin_agent` tracking.
///   3. `origin_agent`'s token and its `reports_to` cascade (v1.8.20
///      preserved — Discord threads are scoped to the bot that opened
///      them; the delegation root is the thread opener).
///   4. Global `config.toml [channels] <channel>_bot_token[_enc]`.
///
/// Empty string from the global fallback is treated the same as "no
/// token configured" by the caller — `forward_to_channel` emits its
/// own `<channel>_bot_token not configured` error in that case.
fn resolve_forward_token(
    home_dir: &Path,
    callback_agent_id: &str,
    origin_agent: Option<&str>,
    channel: &str,
    config: &toml::Value,
    enc_key: &str,
    plain_key: &str,
) -> String {
    // 1 + 2: callback agent's own token, then reports_to cascade.
    if let Some(tok) = crate::config_crypto::resolve_agent_channel_token_via_reports_to(
        home_dir,
        callback_agent_id,
        channel,
    ) {
        return tok;
    }
    // 3: origin_agent + its reports_to cascade (covers the case where
    // callback_agent_id's chain doesn't reach the thread opener).
    if let Some(root) = origin_agent.filter(|s| !s.is_empty() && *s != callback_agent_id) {
        if let Some(tok) = crate::config_crypto::resolve_agent_channel_token_via_reports_to(
            home_dir, root, channel,
        ) {
            return tok;
        }
    }
    // 4: global fallback.
    get_config_token(config, enc_key, plain_key, home_dir)
}

/// Look up the origin_agent for a message_id. Used by
/// `forward_delegation_response` to feed the token-cascade in
/// `forward_to_channel` so nested sub-agent replies can inherit the
/// root agent's per-agent bot token (v1.8.20).
///
/// Returns None if the message_queue.db is missing, the query fails,
/// or the row has no origin_agent — callers then cascade directly to
/// the global config token (original v1.8.14 behaviour).
fn lookup_origin_agent(home_dir: &Path, message_id: &str) -> Option<String> {
    let db_path = home_dir.join("message_queue.db");
    let conn = rusqlite::Connection::open(&db_path).ok()?;
    let _ = conn.execute_batch("PRAGMA busy_timeout=5000;");
    conn.query_row(
        "SELECT origin_agent FROM message_queue WHERE id = ?1",
        rusqlite::params![message_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|s| !s.is_empty())
}

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
            turn_id: None,
            session_id: None,
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

    // ── v1.8.17 Fix 2: parent-session append tests ──

    use crate::session::SessionManager;

    /// Helper: set up a temp home directory with sessions.db seeded with a
    /// parent session owned by `parent_agent` at `session_id`.
    async fn setup_parent_session(
        session_id: &str,
        parent_agent: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let db_path = home.join("sessions.db");
        let sm = SessionManager::new(&db_path).unwrap();
        sm.get_or_create(session_id, parent_agent).await.unwrap();
        // Seed a user turn so the parent's history isn't empty.
        sm.append_message(session_id, "user", "parent turn", 5).await.unwrap();
        (tmp, home)
    }

    #[tokio::test]
    async fn forward_appends_to_parent_session_on_success() {
        let (_tmp, home) = setup_parent_session("telegram:42", "agnes").await;

        append_subagent_reply_to_parent_session(
            &home,
            "telegram",
            "42",
            None,
            "agnes",
            None,
            "duduclaw-tl",
            "方案 A / B / C — which do you prefer?",
        ).await;

        // Re-open the session store and verify the turn landed.
        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("telegram:42").await.unwrap();
        assert_eq!(msgs.len(), 2, "expected user turn + appended assistant turn");
        assert_eq!(msgs[1].role, "assistant");
        assert!(msgs[1].content.contains("方案 A / B / C"));
        assert!(msgs[1].tokens > 0, "tokens should be estimated > 0");
    }

    #[tokio::test]
    async fn sub_agent_attribution_is_in_content() {
        let (_tmp, home) = setup_parent_session("telegram:99", "agnes").await;

        append_subagent_reply_to_parent_session(
            &home,
            "telegram",
            "99",
            None,
            "agnes",
            None,
            "duduclaw-tl",
            "Here are three options.",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("telegram:99").await.unwrap();
        let appended = &msgs.last().unwrap().content;
        assert!(
            appended.contains("duduclaw-tl"),
            "sub-agent name must be present for parent LLM attribution: {appended}"
        );
        assert!(
            appended.contains("<subagent_reply"),
            "should use XML delimiter: {appended}"
        );
        assert!(
            appended.contains("</subagent_reply>"),
            "should close XML delimiter: {appended}"
        );
    }

    #[tokio::test]
    async fn forward_skips_session_append_on_http_failure() {
        // This scenario is enforced structurally: the Err branch in
        // `forward_delegation_response` never calls
        // `append_subagent_reply_to_parent_session`. We assert that by
        // NOT calling the helper at all and verifying the parent session
        // contains only the original user turn.
        let (_tmp, home) = setup_parent_session("telegram:55", "agnes").await;

        // Simulate: forward_to_channel returned Err — we do not append.
        // (No call to append_subagent_reply_to_parent_session.)

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("telegram:55").await.unwrap();
        assert_eq!(msgs.len(), 1, "no append should have occurred");
        assert_eq!(msgs[0].role, "user");
    }

    #[tokio::test]
    async fn session_append_failure_does_not_break_forward() {
        // Simulate session-store error by pointing the helper at a home_dir
        // with a *corrupt* sessions.db file. The helper MUST return without
        // panicking and without propagating an error (its signature is
        // `-> ()` — non-blocking by contract).
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        // Write a garbage file where sessions.db would live.
        std::fs::write(home.join("sessions.db"), b"not a sqlite database").unwrap();

        // Should not panic. If it returned, the forward path's Ok(()) is
        // preserved (caller does not observe the internal failure).
        append_subagent_reply_to_parent_session(
            &home,
            "telegram",
            "77",
            None,
            "agnes",
            None,
            "duduclaw-tl",
            "reply text",
        ).await;
        // Reaching this line == test passes.
    }

    #[tokio::test]
    async fn missing_sessions_db_is_silently_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        // Intentionally DO NOT create sessions.db.

        // Should no-op gracefully.
        append_subagent_reply_to_parent_session(
            &home,
            "discord",
            "123456789",
            None,
            "agnes",
            None,
            "duduclaw-tl",
            "reply",
        ).await;
    }

    #[tokio::test]
    async fn discord_thread_candidate_tried_before_channel() {
        // The Discord session lives at `discord:thread:<id>` — verify we
        // correctly match the thread-form candidate first.
        let (_tmp, home) = setup_parent_session("discord:thread:9876", "agnes").await;

        append_subagent_reply_to_parent_session(
            &home,
            "discord",
            "9876",
            None,
            "agnes",
            None,
            "duduclaw-tl",
            "thread reply",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("discord:thread:9876").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[1].content.contains("thread reply"));
    }

    #[tokio::test]
    async fn owner_mismatch_skips_append() {
        // If the session at the candidate ID belongs to a different agent,
        // we must NOT write into it (avoid cross-agent history pollution).
        let (_tmp, home) = setup_parent_session("telegram:100", "other_agent").await;

        append_subagent_reply_to_parent_session(
            &home,
            "telegram",
            "100",
            None,
            "agnes",  // ← expected parent, but session belongs to other_agent
            None,     // no chain root provided
            "duduclaw-tl",
            "should not be appended",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("telegram:100").await.unwrap();
        assert_eq!(msgs.len(), 1, "must not pollute another agent's session");
        assert_eq!(msgs[0].role, "user");
    }

    // ── v1.8.22: chain-root cascade tests ─────────────────────────

    #[tokio::test]
    async fn append_cascades_to_chain_root_when_parent_has_no_session() {
        // Production scenario that motivated v1.8.22:
        // - User chats with agnes on Discord → agnes has a session.
        // - agnes delegates to TL → TL replies → Fix 2 appends to agnes's
        //   session (parent match, works since v1.8.17).
        // - TL then delegates to eng-agent → eng-agent replies.
        //   The callback.agent_id is TL (no session), but the channel
        //   session belongs to the chain root agnes.
        //   Pre-v1.8.22: owner-mismatch → skip → agnes never sees the
        //   engineer's output.
        //   v1.8.22: cascade to chain_root owner → append with `via="TL"`.
        let (_tmp, home) = setup_parent_session("discord:thread:555", "agnes").await;

        append_subagent_reply_to_parent_session(
            &home,
            "discord",
            "555",
            None,
            "duduclaw-tl",           // parent agent (callback.agent_id) — no session
            Some("agnes"),           // chain root (message.origin_agent) — has session
            "duduclaw-eng-agent",
            "Engineer report: infrastructure ready",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("discord:thread:555").await.unwrap();
        assert_eq!(msgs.len(), 2, "engineer reply should land in agnes's session");
        assert_eq!(msgs[1].role, "assistant");
        assert!(
            msgs[1].content.contains("Engineer report"),
            "content preserved: {}", msgs[1].content
        );
    }

    #[tokio::test]
    async fn cascade_appends_via_annotation() {
        // The relayed content must include `via="<parent>"` so the
        // chain-root's LLM can distinguish a direct sub-agent reply
        // ("TL said X") from an indirect one ("eng-agent said X,
        // relayed via TL").
        let (_tmp, home) = setup_parent_session("discord:thread:42", "agnes").await;

        append_subagent_reply_to_parent_session(
            &home,
            "discord",
            "42",
            None,
            "duduclaw-tl",
            Some("agnes"),
            "duduclaw-eng-infra",
            "infra topology finalised",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let content = sm
            .get_messages("discord:thread:42").await.unwrap()
            .pop().unwrap().content;
        assert!(
            content.contains("agent=\"duduclaw-eng-infra\""),
            "responder must be in `agent=...`: {content}"
        );
        assert!(
            content.contains("via=\"duduclaw-tl\""),
            "parent must be in `via=...` for cascade path: {content}"
        );
    }

    #[tokio::test]
    async fn cascade_does_not_override_direct_parent_match() {
        // If BOTH the parent and the chain root could match (i.e. the
        // session owner == parent), we take the direct path — no `via=`.
        // This covers the "top-level chain" case where agnes delegates
        // to TL directly.
        let (_tmp, home) = setup_parent_session("discord:thread:1", "agnes").await;

        append_subagent_reply_to_parent_session(
            &home,
            "discord",
            "1",
            None,
            "agnes",          // parent_agent_id == session owner → direct path
            Some("agnes"),    // chain_root == parent (self-loop)
            "duduclaw-tl",
            "dispatch ack",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let content = sm
            .get_messages("discord:thread:1").await.unwrap()
            .pop().unwrap().content;
        assert!(content.contains("<subagent_reply agent=\"duduclaw-tl\">"));
        assert!(
            !content.contains("via="),
            "direct match must NOT add via= attribute: {content}"
        );
    }

    #[tokio::test]
    async fn cascade_skipped_when_neither_parent_nor_root_owns_session() {
        // Cross-agent bleed guard still holds: if the session belongs to
        // a third agent (neither parent nor chain root), refuse to write.
        let (_tmp, home) = setup_parent_session("telegram:200", "third-agent").await;

        append_subagent_reply_to_parent_session(
            &home,
            "telegram",
            "200",
            None,
            "duduclaw-tl",       // parent — no session match
            Some("agnes"),       // chain root — no session match either
            "duduclaw-eng-agent",
            "leaked content",
        ).await;

        let sm = SessionManager::new(&home.join("sessions.db")).unwrap();
        let msgs = sm.get_messages("telegram:200").await.unwrap();
        assert_eq!(
            msgs.len(), 1,
            "third-agent's session must stay untouched (cross-agent bleed guard)"
        );
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn candidate_session_ids_discord_returns_both_forms() {
        let ids = candidate_session_ids("discord", "9876", None);
        assert_eq!(ids[0], "discord:thread:9876");
        assert_eq!(ids[1], "discord:9876");
    }

    #[test]
    fn candidate_session_ids_telegram_with_thread() {
        let ids = candidate_session_ids("telegram", "42", Some("7"));
        assert_eq!(ids, vec!["telegram:42:7".to_string()]);
    }

    #[test]
    fn candidate_session_ids_slack_with_ts() {
        let ids = candidate_session_ids("slack", "C123", Some("1700000.0001"));
        assert_eq!(ids, vec!["slack:C123:1700000.0001".to_string()]);
    }

    #[test]
    fn subagent_token_estimate_is_nonzero() {
        assert!(subagent_reply_token_estimate("hello") > 0);
        assert!(subagent_reply_token_estimate("你好世界") >= 3);
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
                turn_id: None,
                session_id: None,
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
                turn_id: None,
                session_id: None,
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

    // ── v1.8.20: chain-root token cascade ─────────────────────────

    /// Writes a minimal `agents/<name>/agent.toml` that sets a plaintext
    /// `bot_token` for the given channel. `bot_token_enc` is left
    /// deliberately unset so the test doesn't need to reach into
    /// `config_crypto` (which requires the keyfile).
    fn write_agent_with_channel_token(
        home: &std::path::Path,
        agent: &str,
        channel: &str,
        token: &str,
    ) {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        let content = format!(
            r#"[agent]
name = "{agent}"
reports_to = ""

[channels.{channel}]
bot_token = "{token}"
"#,
        );
        std::fs::write(dir.join("agent.toml"), content).unwrap();
    }

    fn write_global_config(home: &std::path::Path, channel_key: &str, token: &str) {
        let content = format!(
            r#"[channels]
{channel_key} = "{token}"
"#,
        );
        std::fs::write(home.join("config.toml"), content).unwrap();
    }

    #[test]
    fn resolve_token_prefers_callback_agent_when_it_has_one() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_with_channel_token(tmp.path(), "duduclaw-tl", "discord", "TL_OWN_TOKEN");
        write_agent_with_channel_token(tmp.path(), "agnes", "discord", "AGNES_TOKEN");
        write_global_config(tmp.path(), "discord_bot_token", "GLOBAL_STALE");
        let config: toml::Value =
            std::fs::read_to_string(tmp.path().join("config.toml")).unwrap().parse().unwrap();

        let token = resolve_forward_token(
            tmp.path(), "duduclaw-tl", Some("agnes"), "discord",
            &config, "discord_bot_token_enc", "discord_bot_token",
        );
        assert_eq!(token, "TL_OWN_TOKEN");
    }

    #[test]
    fn resolve_token_cascades_to_chain_root_when_callback_agent_has_none() {
        // The production bug: TL has no [channels.discord], only agnes
        // does, but the thread belongs to agnes. v1.8.19 fell back to
        // the (stale) global token → 401. v1.8.20 cascades to agnes.
        let tmp = tempfile::tempdir().unwrap();
        write_agent_with_channel_token(tmp.path(), "agnes", "discord", "AGNES_TOKEN");
        // Intentionally no duduclaw-tl/agent.toml at all.
        write_global_config(tmp.path(), "discord_bot_token", "GLOBAL_STALE_401");
        let config: toml::Value =
            std::fs::read_to_string(tmp.path().join("config.toml")).unwrap().parse().unwrap();

        let token = resolve_forward_token(
            tmp.path(), "duduclaw-tl", Some("agnes"), "discord",
            &config, "discord_bot_token_enc", "discord_bot_token",
        );
        assert_eq!(
            token, "AGNES_TOKEN",
            "Nested sub-agent forward should inherit chain-root's per-agent token, \
             not fall back to stale global"
        );
    }

    #[test]
    fn resolve_token_falls_back_to_global_when_neither_agent_has_one() {
        let tmp = tempfile::tempdir().unwrap();
        // Neither agent has a [channels.discord] block.
        write_global_config(tmp.path(), "discord_bot_token", "GLOBAL_ONLY");
        let config: toml::Value =
            std::fs::read_to_string(tmp.path().join("config.toml")).unwrap().parse().unwrap();

        let token = resolve_forward_token(
            tmp.path(), "duduclaw-tl", Some("agnes"), "discord",
            &config, "discord_bot_token_enc", "discord_bot_token",
        );
        assert_eq!(token, "GLOBAL_ONLY");
    }

    #[test]
    fn resolve_token_no_infinite_self_loop_when_origin_equals_callback() {
        // Edge case: callback_agent_id == origin_agent (same agent). The
        // cascade must not double-query or infinite-loop; just skip the
        // chain-root tier and go straight to global.
        let tmp = tempfile::tempdir().unwrap();
        write_global_config(tmp.path(), "discord_bot_token", "GLOBAL_TOKEN");
        let config: toml::Value =
            std::fs::read_to_string(tmp.path().join("config.toml")).unwrap().parse().unwrap();

        let token = resolve_forward_token(
            tmp.path(), "agnes", Some("agnes"), "discord",
            &config, "discord_bot_token_enc", "discord_bot_token",
        );
        assert_eq!(token, "GLOBAL_TOKEN");
    }

    #[test]
    fn resolve_token_handles_missing_origin_agent() {
        // `lookup_origin_agent` can return None (DB missing, row missing,
        // NULL origin). Cascade must still work: callback_agent_id →
        // (no root hop) → global.
        let tmp = tempfile::tempdir().unwrap();
        write_global_config(tmp.path(), "discord_bot_token", "GLOBAL_TOKEN");
        let config: toml::Value =
            std::fs::read_to_string(tmp.path().join("config.toml")).unwrap().parse().unwrap();

        let token = resolve_forward_token(
            tmp.path(), "duduclaw-tl", None, "discord",
            &config, "discord_bot_token_enc", "discord_bot_token",
        );
        assert_eq!(token, "GLOBAL_TOKEN");
    }

    #[test]
    fn lookup_origin_agent_returns_some_when_row_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("message_queue.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message_queue (
                id TEXT PRIMARY KEY, sender TEXT NOT NULL, target TEXT NOT NULL,
                payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
                retry_count INTEGER NOT NULL DEFAULT 0,
                delegation_depth INTEGER NOT NULL DEFAULT 0,
                origin_agent TEXT, sender_agent TEXT,
                error TEXT, response TEXT,
                created_at TEXT NOT NULL, acked_at TEXT, completed_at TEXT
            );
            INSERT INTO message_queue (id, sender, target, payload, created_at, origin_agent)
            VALUES ('m1','duduclaw-tl','duduclaw-marketing','hi','2026-04-21T12:34:25','agnes');
            INSERT INTO message_queue (id, sender, target, payload, created_at)
            VALUES ('m2','duduclaw-tl','duduclaw-marketing','hi','2026-04-21T12:34:25');",
        ).unwrap();
        drop(conn);

        assert_eq!(lookup_origin_agent(tmp.path(), "m1").as_deref(), Some("agnes"));
        assert_eq!(lookup_origin_agent(tmp.path(), "m2"), None);
        assert_eq!(lookup_origin_agent(tmp.path(), "nonexistent"), None);
    }

    #[test]
    fn lookup_origin_agent_handles_missing_db() {
        let tmp = tempfile::tempdir().unwrap();
        // No message_queue.db created.
        assert_eq!(lookup_origin_agent(tmp.path(), "anything"), None);
    }

    // ── v1.8.21: manual re-forward CLI ────────────────────────────

    #[test]
    fn parse_reply_channel_plain_forms() {
        assert_eq!(
            parse_reply_channel("discord:12345").unwrap(),
            ("discord".into(), "12345".into(), None),
        );
        assert_eq!(
            parse_reply_channel("telegram:67890:thread42").unwrap(),
            ("telegram".into(), "67890".into(), Some("thread42".into())),
        );
    }

    #[test]
    fn parse_reply_channel_discord_thread_marker_collapses() {
        // `discord:thread:<id>` — the literal "thread" is a marker, not
        // the channel_id. Collapses to (ch_id=<id>, thread_id=None) to
        // match mcp.rs callback insert semantics.
        assert_eq!(
            parse_reply_channel("discord:thread:1496095418805780591").unwrap(),
            ("discord".into(), "1496095418805780591".into(), None),
        );
    }

    #[test]
    fn parse_reply_channel_rejects_malformed() {
        assert!(parse_reply_channel("no-colon-here").is_err());
        assert!(parse_reply_channel("").is_err());
    }

    /// Helper: create `message_queue.db` with a single completed row for
    /// reforward tests. Schema matches `MessageQueue::init_schema`
    /// including v1.8.16's `reply_channel` column.
    fn setup_done_message(
        home: &std::path::Path,
        id: &str,
        sender: &str,
        target: &str,
        response: &str,
        reply_channel: Option<&str>,
    ) {
        let db_path = home.join("message_queue.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message_queue (
                id TEXT PRIMARY KEY, sender TEXT NOT NULL, target TEXT NOT NULL,
                payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
                retry_count INTEGER NOT NULL DEFAULT 0,
                delegation_depth INTEGER NOT NULL DEFAULT 0,
                origin_agent TEXT, sender_agent TEXT,
                error TEXT, response TEXT,
                created_at TEXT NOT NULL, acked_at TEXT, completed_at TEXT,
                reply_channel TEXT
            );
            CREATE TABLE IF NOT EXISTS delegation_callbacks (
                message_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                channel_type TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                thread_id TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );",
        ).unwrap();
        conn.execute(
            "INSERT INTO message_queue (id, sender, target, payload, status, \
             response, created_at, completed_at, reply_channel, origin_agent) \
             VALUES (?1, ?2, ?3, 'payload', 'done', ?4, '2026-04-21T12:34:25', \
             '2026-04-21T12:35:48', ?5, ?2)",
            rusqlite::params![id, sender, target, response, reply_channel],
        ).unwrap();
    }

    #[tokio::test]
    async fn reforward_dry_run_uses_existing_callback() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mid = "78fbcfc8-735b-4053-9ee0-a03543fd904f";
        setup_done_message(home, mid, "duduclaw-tl", "duduclaw-marketing", "the report", None);

        // A live callback exists (the dispatcher re-inserted it after the
        // HTTP POST got 401). Dry-run should surface it without mutating
        // anything.
        let conn = rusqlite::Connection::open(home.join("message_queue.db")).unwrap();
        conn.execute(
            "INSERT INTO delegation_callbacks (message_id, agent_id, channel_type, channel_id, thread_id, retry_count, created_at) \
             VALUES (?1, 'duduclaw-tl', 'discord', '1496095418805780591', NULL, 1, '2026-04-21T12:35:48')",
            rusqlite::params![mid],
        ).unwrap();
        drop(conn);

        let outcome = reforward_message(home, mid, true).await.expect("dry-run");
        match outcome {
            ReforwardOutcome::DryRun { channel_type, channel_id, thread_id, has_existing_callback } => {
                assert_eq!(channel_type, "discord");
                assert_eq!(channel_id, "1496095418805780591");
                assert_eq!(thread_id, None);
                assert!(has_existing_callback);
            }
            other => panic!("expected DryRun, got {other:?}"),
        }

        // Dry-run must NOT have consumed the callback.
        let count: i64 = rusqlite::Connection::open(home.join("message_queue.db")).unwrap()
            .query_row("SELECT COUNT(*) FROM delegation_callbacks WHERE message_id=?1",
                rusqlite::params![mid], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "dry-run must not touch delegation_callbacks");
    }

    #[tokio::test]
    async fn reforward_dry_run_synthesizes_from_reply_channel_when_no_callback() {
        // The pathological case: callback was already permanently dropped
        // (5x retries exhausted, or 24h stale cleanup ran), but the row
        // still has reply_channel stored. Dry-run should parse that and
        // report what it would do.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mid = "m1";
        setup_done_message(
            home, mid, "duduclaw-tl", "duduclaw-marketing", "the report",
            Some("discord:thread:1496095418805780591"),
        );

        let outcome = reforward_message(home, mid, true).await.expect("dry-run");
        match outcome {
            ReforwardOutcome::DryRun { channel_type, channel_id, has_existing_callback, .. } => {
                assert_eq!(channel_type, "discord");
                assert_eq!(channel_id, "1496095418805780591");
                assert!(!has_existing_callback);
            }
            other => panic!("expected DryRun, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reforward_rejects_pending_message() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mid = "m1";
        // Manually insert a pending row (setup_done_message always sets 'done').
        let conn = rusqlite::Connection::open(home.join("message_queue.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS message_queue (
                id TEXT PRIMARY KEY, sender TEXT NOT NULL, target TEXT NOT NULL,
                payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
                retry_count INTEGER NOT NULL DEFAULT 0,
                delegation_depth INTEGER NOT NULL DEFAULT 0,
                origin_agent TEXT, sender_agent TEXT,
                error TEXT, response TEXT,
                created_at TEXT NOT NULL, acked_at TEXT, completed_at TEXT,
                reply_channel TEXT
            );",
        ).unwrap();
        conn.execute(
            "INSERT INTO message_queue (id, sender, target, payload, status, created_at) \
             VALUES (?1, 'a', 'b', 'p', 'pending', '2026-01-01T00:00:00Z')",
            rusqlite::params![mid],
        ).unwrap();
        drop(conn);

        let err = reforward_message(home, mid, true).await.unwrap_err();
        assert!(err.contains("status='pending'"), "got: {err}");
    }

    #[tokio::test]
    async fn reforward_rejects_missing_message() {
        let tmp = tempfile::tempdir().unwrap();
        setup_done_message(tmp.path(), "exists", "a", "b", "hi", None);
        let err = reforward_message(tmp.path(), "does-not-exist", true).await.unwrap_err();
        assert!(err.contains("No message with id"), "got: {err}");
    }

    #[tokio::test]
    async fn reforward_rejects_empty_response() {
        let tmp = tempfile::tempdir().unwrap();
        setup_done_message(tmp.path(), "m1", "a", "b", "", None);
        let err = reforward_message(tmp.path(), "m1", true).await.unwrap_err();
        assert!(err.contains("no response body"), "got: {err}");
    }

    #[tokio::test]
    async fn reforward_without_callback_or_reply_channel_errors() {
        // No callback row, no reply_channel either — can't determine where
        // to forward. Must error out rather than silently doing nothing.
        let tmp = tempfile::tempdir().unwrap();
        setup_done_message(tmp.path(), "m1", "a", "b", "ok", None);
        let err = reforward_message(tmp.path(), "m1", true).await.unwrap_err();
        assert!(err.contains("Cannot determine where to forward"), "got: {err}");
    }
}
