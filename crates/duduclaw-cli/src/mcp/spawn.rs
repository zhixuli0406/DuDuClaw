use super::*;

/// Spawn a persistent sub-agent task in the background.
pub(crate) async fn handle_spawn_agent(params: &Value, home_dir: &Path, caller: &str) -> Value {
    spawn_agent_with_ctx(params, home_dir, caller, DelegationContext::from_env()).await
}

/// Core spawn_agent with injectable delegation context.
pub(crate) async fn spawn_agent_with_ctx(
    params: &Value,
    home_dir: &Path,
    caller: &str,
    ctx: DelegationContext,
) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let task = params.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let session_key = params
        .get("session_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_id.is_empty() || task.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and task are required"}],
            "isError": true
        });
    }
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    // Verify agent exists
    let agent_dir = home_dir.join("agents").join(agent_id);
    if !agent_dir.join("agent.toml").exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        });
    }

    // ── F2: off-boarded target guard ───────────────────────────
    // An archived / soft-deleted agent must never be spawned. Fail-closed on a
    // resolved non-operational status; an indeterminate status (pre-WP4 config
    // without the field) keeps the pre-existing allow behaviour.
    if let Some(status) = agent_status_of(home_dir, agent_id)
        && !status.is_operational()
    {
        let status_str = format!("{status:?}").to_lowercase();
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: agent '{agent_id}' is not operational (status: {status_str}); \
                 it has been off-boarded and cannot be spawned."
            )}],
            "isError": true
        });
    }

    // ── WP21 C2: department × hierarchy delegation gate ────────
    if let Err(reason) = check_delegation_allowed(home_dir, caller, agent_id, "spawn_agent").await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // ── Delegation depth tracking ────────────────────────────────
    let incoming_depth = ctx.depth;
    let outgoing_depth = incoming_depth.saturating_add(1);

    if outgoing_depth >= duduclaw_core::MAX_DELEGATION_DEPTH {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: delegation depth limit ({}) would be exceeded. \
                 Current depth: {incoming_depth}. Cannot spawn further agents.",
                duduclaw_core::MAX_DELEGATION_DEPTH,
            )}],
            "isError": true
        });
    }

    let origin = ctx.origin.as_deref().unwrap_or(caller);

    // ── P3 runaway guard (cascade hop-depth + dispatch circuit breaker) ──
    let outgoing_hop = match check_dispatch_runaway(home_dir, "spawn", caller) {
        Ok(h) => h,
        Err(resp) => return resp,
    };

    let task_id = uuid::Uuid::new_v4().to_string();

    // Write a structured task entry to bus_queue.jsonl with spawn metadata
    let queue_path = home_dir.join("bus_queue.jsonl");
    let entry = serde_json::json!({
        "type": "agent_message",
        "message_id": &task_id,
        "agent_id": agent_id,
        "payload": task,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_key": if session_key.is_empty() { &task_id } else { session_key },
        "persistent": true,
        "delegation_depth": outgoing_depth,
        "hop_depth": outgoing_hop,
        "origin_agent": origin,
        "sender_agent": caller,
    });

    // RFC-22 Decision 2-C (Phase 3 W1): on bus_queue write failure, surface
    // the underlying I/O error to the caller. Previously we returned an
    // opaque "Failed to queue agent task" which left the LLM (e.g. agnes
    // 5/5 trace) unable to distinguish "bus full" from "permission denied"
    // from "disk full" — and prone to hallucinating sub-agent replies as a
    // fallback.  Concrete error → caller can inform the user / stop early.
    // Use std::result::Result explicitly — the crate-level `Result<T>` alias
    // is single-arg (DuDuClawError default), incompatible with our String error.
    let queued: std::result::Result<(), String> = tokio::task::spawn_blocking({
        let path = queue_path.clone();
        let entry_str = entry.to_string();
        move || -> std::result::Result<(), String> {
            use std::io::Write;
            // Project convention #3 (2026-07 MED): hold the cross-process
            // advisory lock — the dispatcher REWRITES bus_queue.jsonl and a
            // bare append racing that rewrite is silently dropped.
            duduclaw_core::with_file_lock(&path, || {
                // Enforce bus_queue.jsonl size limit (CLI-H4)
                const MAX_QUEUE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
                if let Ok(meta) = std::fs::metadata(&path)
                    && meta.len() > MAX_QUEUE_SIZE
                {
                    return Err(std::io::Error::other(format!(
                        "bus_queue.jsonl exceeds {}MB size limit (current: {} bytes). \
                         Run `duduclaw bus rotate` or wait for dispatcher to drain.",
                        MAX_QUEUE_SIZE / (1024 * 1024),
                        meta.len()
                    )));
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                writeln!(f, "{entry_str}")?;
                Ok(())
            })
            .map_err(|e| format!("queue {}: {e}", path.display()))
        }
    })
    .await
    .unwrap_or_else(|join_err| Err(format!("spawn_blocking panicked: {join_err}")));

    match queued {
        Ok(()) => {
            // O4: one-line honest delegation-cost advisory (arXiv:2604.02460).
            let advisory = delegation_cost_advisory(home_dir)
                .await
                .map(|line| format!("\n\n{line}"))
                .unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Sub-agent '{agent_id}' task spawned successfully.\n\
                     Task ID: {task_id}\n\
                     Session key: {}\n\
                     \n\
                     The task is queued and will be picked up by the dispatcher.\n\
                     Use agent_status to check progress, or check bus_queue.jsonl for the response.{advisory}",
                    if session_key.is_empty() { &task_id } else { session_key }
                )}]
            })
        }
        Err(reason) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: Failed to queue agent task for '{agent_id}'. Reason: {reason}\n\
                 \n\
                 Per RFC-22, do NOT fabricate a reply on behalf of '{agent_id}'. \
                 Inform the user that '{agent_id}' is unreachable and surface \
                 the reason verbatim."
            )}],
            "isError": true
        }),
    }
}

/// O2 — synthesize an ephemeral sub-agent and dispatch one task to it.
/// See `duduclaw_gateway::ephemeral` for the scaffold/GC design (AOrchestra,
/// arXiv:2602.03786).
pub(crate) async fn handle_spawn_ephemeral(params: &Value, home_dir: &Path, caller: &str) -> Value {
    // The capability envelope is the ACTUAL caller's — in delegated contexts
    // that is the delegation sender (same trusted env source the audit trail
    // uses), never a spoofable tool param.
    let actual_caller = std::env::var(duduclaw_core::ENV_DELEGATION_SENDER)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| caller.to_string());
    spawn_ephemeral_with_ctx(
        params,
        home_dir,
        &actual_caller,
        DelegationContext::from_env(),
    )
    .await
}

/// Core spawn_ephemeral with injectable delegation context (testable).
pub(crate) async fn spawn_ephemeral_with_ctx(
    params: &Value,
    home_dir: &Path,
    caller: &str,
    ctx: DelegationContext,
) -> Value {
    let instruction = params
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let context = params.get("context").and_then(|v| v.as_str()).unwrap_or("");
    let tier = params
        .get("tier")
        .and_then(|v| v.as_str())
        .unwrap_or("standard");

    // `tools`: JSON array of strings, or a comma-separated string.
    let tools: Vec<String> = match params.get("tools") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => Vec::new(),
    };

    if instruction.is_empty() || context.is_empty() || tools.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: instruction, context, and a non-empty tools list are required (deny-by-default: the ephemeral agent only gets the tools you explicitly request)"}],
            "isError": true
        });
    }
    // The dispatcher drops payloads over 100 KB — reject early instead of
    // scaffolding an agent whose task can never be delivered.
    if context.len() > 100_000 {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: context exceeds the 100KB bus payload limit"}],
            "isError": true
        });
    }

    // WP21 C5 invariant: the ephemeral agent's parent is *always* the caller —
    // never a tool parameter. That is already the C4 rule ("you may only attach
    // an agent under yourself or your own subtree") satisfied by construction,
    // so no extra placement check is needed here. Do not turn `parent` into a
    // caller-supplied field without adding `check_org_placement_allowed`:
    // that would re-open the self-service escalation C4 closes.
    let parent = caller.to_string();
    if !is_valid_agent_id(&parent) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid caller agent id"}],
            "isError": true
        });
    }

    // ── Delegation depth tracking (same rule as spawn_agent) ───────────
    let incoming_depth = ctx.depth;
    let outgoing_depth = incoming_depth.saturating_add(1);
    if outgoing_depth >= duduclaw_core::MAX_DELEGATION_DEPTH {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: delegation depth limit ({}) would be exceeded. \
                 Current depth: {incoming_depth}. Cannot synthesize further agents.",
                duduclaw_core::MAX_DELEGATION_DEPTH,
            )}],
            "isError": true
        });
    }
    let origin = ctx.origin.as_deref().unwrap_or(caller).to_string();

    // ── Scaffold (fail-closed capability subsetting inside) ─────────────
    let spec = duduclaw_gateway::ephemeral::EphemeralSpawnSpec {
        parent: parent.clone(),
        instruction: instruction.to_string(),
        tools,
        tier: tier.to_string(),
    };
    // H19: kept for the admission-queue payload if the cap below is hit —
    // `spec` itself is moved into the spawn_blocking closure next.
    let spec_for_admission_queue = spec.clone();
    let context_for_admission_queue = context.to_string();
    // Captured NOW (request time) so a later replay re-derives the exact same
    // outgoing hop depth `check_dispatch_runaway` would have computed had
    // capacity been available immediately — hop_depth is a property of the
    // REQUEST, not of when it happens to be admitted.
    let incoming_hop_for_admission_queue = incoming_hop_depth();
    let home = home_dir.to_path_buf();
    let scaffolded =
        tokio::task::spawn_blocking(move || duduclaw_gateway::ephemeral::scaffold(&home, &spec))
            .await
            .unwrap_or_else(|join_err| Err(format!("spawn_blocking panicked: {join_err}")));

    let scaffolded = match scaffolded {
        Ok(s) => s,
        Err(reason)
            if reason.starts_with(duduclaw_gateway::ephemeral::EPHEMERAL_CAPACITY_ERROR_PREFIX) =>
        {
            // H19: over the (temporary) capacity cap — not a fundamentally
            // invalid request, so `[dispatch] admission` decides whether to
            // durably queue it (default) or fall back to the pre-H19 hard
            // reject (`admission = "fail"`).
            let admission_cfg =
                duduclaw_core::spawn_admission::AdmissionConfig::from_home(home_dir);
            if admission_cfg.admission == duduclaw_core::spawn_admission::AdmissionMode::Fail {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: ephemeral synthesis rejected: {reason}")}],
                    "isError": true
                });
            }
            // Best-effort owner scope for later invalidation: the current
            // turn id (same trusted env source `mcp_redaction`/trust-feedback
            // already read), absent for dispatch kinds that don't carry one
            // (cron/heartbeat/goal-loop) — those queued tickets are simply
            // never touched by turn-based invalidation, only by TTL.
            let owner_key = std::env::var(duduclaw_core::ENV_TRUST_TURN_ID)
                .ok()
                .filter(|s| !s.is_empty());
            let payload = serde_json::json!({
                "parent": spec_for_admission_queue.parent,
                "instruction": spec_for_admission_queue.instruction,
                "tools": spec_for_admission_queue.tools,
                "tier": spec_for_admission_queue.tier,
                "context": context_for_admission_queue,
                "origin": origin,
                "outgoing_depth": outgoing_depth,
                "incoming_hop": incoming_hop_for_admission_queue,
            });
            return match duduclaw_core::spawn_admission::enqueue(
                home_dir,
                duduclaw_gateway::ephemeral::EPHEMERAL_ADMISSION_CLASS,
                &admission_cfg,
                owner_key.as_deref(),
                payload,
            ) {
                Ok(duduclaw_core::spawn_admission::EnqueueOutcome::Queued {
                    ticket_id,
                    position,
                }) => {
                    // H19: audit trail for the "排隊" (queued) event — the
                    // release/expiry counterparts are logged by
                    // `ephemeral::drain_admission_queue`; this is the third
                    // leg (enqueue itself must also leave a trail).
                    duduclaw_security::audit::append_tool_call_with_extras(
                        home_dir,
                        &parent,
                        "ephemeral_admission_queued",
                        &format!("ticket_id={ticket_id} position={position}"),
                        true,
                        &[("ticket_id", serde_json::Value::String(ticket_id.clone()))],
                    );
                    serde_json::json!({
                        "content": [{"type": "text", "text": format!(
                            "Ephemeral agent synthesis queued: {reason}.\n\
                             Ticket: {ticket_id}\n\
                             FIFO position: {position}\n\
                             It will be scaffolded and dispatched automatically once capacity \
                             frees (checked hourly alongside GC); the request expires after {}s \
                             if capacity never frees. The response returns through the normal \
                             delegation path once it runs — per RFC-22, do NOT fabricate a reply \
                             on its behalf.",
                            admission_cfg.queue_item_ttl_secs,
                        )}]
                    })
                }
                Ok(duduclaw_core::spawn_admission::EnqueueOutcome::Rejected {
                    reason: queue_reason,
                }) => {
                    serde_json::json!({
                        "content": [{"type": "text", "text": format!(
                            "Error: ephemeral synthesis rejected: {reason}; admission queue also \
                             refused it: {queue_reason}"
                        )}],
                        "isError": true
                    })
                }
                Err(e) => {
                    // Queue backend unavailable — degrade to the pre-existing
                    // hard reject rather than silently bypass the cap.
                    serde_json::json!({
                        "content": [{"type": "text", "text": format!(
                            "Error: ephemeral synthesis rejected: {reason} (admission queue \
                             unavailable, fell back to immediate reject: {e})"
                        )}],
                        "isError": true
                    })
                }
            };
        }
        Err(reason) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: ephemeral synthesis rejected: {reason}")}],
                "isError": true
            });
        }
    };
    let eph_id = scaffolded.agent_id.clone();

    // ── P3 runaway guard (cascade hop-depth + dispatch circuit breaker) ──
    let outgoing_hop = match check_dispatch_runaway(home_dir, "ephemeral", &parent) {
        Ok(h) => h,
        Err(resp) => return resp,
    };

    // ── Enqueue on the bus — identical shape to spawn_agent ─────────────
    let task_id = uuid::Uuid::new_v4().to_string();
    let queue_path = home_dir.join("bus_queue.jsonl");
    let entry = serde_json::json!({
        "type": "agent_message",
        "message_id": &task_id,
        "agent_id": &eph_id,
        "payload": context,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_key": &task_id,
        "persistent": true,
        "delegation_depth": outgoing_depth,
        "hop_depth": outgoing_hop,
        "origin_agent": origin,
        "sender_agent": parent,
    });
    let queued: std::result::Result<(), String> = tokio::task::spawn_blocking({
        let path = queue_path.clone();
        let entry_str = entry.to_string();
        move || -> std::result::Result<(), String> {
            use std::io::Write;
            // Project convention #3 (2026-07 MED): same advisory lock as the
            // spawn_agent enqueue — the dispatcher rewrites this file.
            duduclaw_core::with_file_lock(&path, || {
                const MAX_QUEUE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB (CLI-H4)
                if let Ok(meta) = std::fs::metadata(&path)
                    && meta.len() > MAX_QUEUE_SIZE
                {
                    return Err(std::io::Error::other(format!(
                        "bus_queue.jsonl exceeds {}MB size limit (current: {} bytes)",
                        MAX_QUEUE_SIZE / (1024 * 1024),
                        meta.len()
                    )));
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                writeln!(f, "{entry_str}")?;
                Ok(())
            })
            .map_err(|e| format!("queue {}: {e}", path.display()))
        }
    })
    .await
    .unwrap_or_else(|join_err| Err(format!("spawn_blocking panicked: {join_err}")));

    match queued {
        Ok(()) => {
            // O4: one-line honest delegation-cost advisory (arXiv:2604.02460).
            let advisory = delegation_cost_advisory(home_dir)
                .await
                .map(|line| format!("\n\n{line}"))
                .unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Ephemeral agent '{eph_id}' synthesized and task queued.\n\
                     Task ID: {task_id}\n\
                     Tier: {tier}\n\
                     \n\
                     The scaffold lives under agents/.ephemeral/ and is \
                     garbage-collected ~1h after completion (24h hard TTL). \
                     The response returns through the normal delegation path.{advisory}"
                )}]
            })
        }
        Err(reason) => {
            // Queueing failed — tear the scaffold down (best-effort; the
            // path was produced by `scaffold` and is containment-checked).
            let _ = std::fs::remove_dir_all(&scaffolded.dir);
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: Failed to queue ephemeral task. Reason: {reason}\n\
                     The scaffold was rolled back. Per RFC-22, do NOT fabricate \
                     a reply on its behalf."
                )}],
                "isError": true
            })
        }
    }
}
