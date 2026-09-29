use super::*;

/// MCP handler for `audit_trail_query`.
///
/// Forwards to the gateway `audit.evolution_query` WebSocket endpoint
/// by delegating to [`AuditEventIndex`] directly (no gateway round-trip
/// needed from the CLI side since we share the same `home_dir`).
///
/// # Authorization
/// `caller_is_admin` must be `true`; if `false` the call is rejected immediately
/// (defence-in-depth: the MCP dispatch layer enforces `Scope::Admin` before
/// routing here, but this guard prevents privilege escalation from any future
/// call-path that skips the dispatch-level check — OWASP A01).
pub(crate) async fn handle_audit_trail_query(
    params: &Value,
    home_dir: &Path,
    caller_client_id: &str,
    caller_is_admin: bool,
) -> Value {
    // ── Defence-in-depth authorization guard (H1 / OWASP A01) ────────────────
    if !caller_is_admin {
        tracing::warn!(
            caller_client_id = %caller_client_id,
            "audit_trail_query: access denied — caller lacks Admin scope"
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: audit_trail_query requires Admin scope"}],
            "isError": true
        });
    }
    tracing::info!(caller_client_id = %caller_client_id, "audit_trail_query invoked");

    use duduclaw_gateway::evolution_events::query::{AuditEventIndex, AuditQueryFilter};

    let filter = AuditQueryFilter {
        agent_id: params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        event_type: params
            .get("event_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        outcome: params
            .get("outcome")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        skill_id: params
            .get("skill_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        since: params
            .get("since")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        until: params
            .get("until")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        limit: params.get("limit").and_then(|v| v.as_i64()),
        offset: params.get("offset").and_then(|v| v.as_i64()),
    };

    let idx = match AuditEventIndex::open(home_dir) {
        Ok(i) => i,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: cannot open audit index: {e}")}],
                "isError": true
            });
        }
    };

    if let Err(e) = idx.sync_from_files().await {
        // Non-fatal: log and continue with potentially stale index.
        tracing::warn!("audit_trail_query: sync warning (stale index): {e}");
    }

    match idx.query(filter).await {
        Ok(result) => {
            let events_json: Vec<serde_json::Value> = result
                .events
                .iter()
                .map(|ev| {
                    serde_json::json!({
                        "timestamp":      ev.timestamp,
                        "event_type":     ev.event_type.to_string(),
                        "agent_id":       ev.agent_id,
                        "skill_id":       ev.skill_id,
                        "generation":     ev.generation,
                        "outcome":        ev.outcome.to_string(),
                        "trigger_signal": ev.trigger_signal,
                        "metadata":       ev.metadata,
                    })
                })
                .collect();

            let summary = format!(
                "Audit Trail Query Results\n\
                 ─────────────────────────\n\
                 Total matching events: {}\n\
                 Showing: {} events (offset {}, limit {})\n\
                 \n\
                 {}",
                result.total,
                result.events.len(),
                result.offset,
                result.limit,
                serde_json::to_string_pretty(&events_json).unwrap_or_default(),
            );

            serde_json::json!({
                "content": [{"type": "text", "text": summary}],
                "audit_result": {
                    "events": events_json,
                    "total":  result.total,
                    "limit":  result.limit,
                    "offset": result.offset,
                }
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: audit query failed: {e}")}],
            "isError": true
        }),
    }
}

// ── Reliability Dashboard handler (W20-P0) ──────────────────

/// MCP handler for `reliability_summary`.
///
/// Computes the four-metric Agent Reliability Summary from the evolution-event
/// audit trail SQLite index.  Requires Admin scope (same as `audit_trail_query`).
///
/// # Authorization
/// Defence-in-depth: `caller_is_admin` must be `true`.  The dispatch-layer
/// scope check handles the primary guard; this check prevents privilege
/// escalation from any future call-path that bypasses dispatch (OWASP A01).
pub(crate) async fn handle_reliability_summary(
    params: &Value,
    home_dir: &Path,
    caller_client_id: &str,
    caller_is_admin: bool,
) -> Value {
    // ── Authorization guard ───────────────────────────────────────────────────
    if !caller_is_admin {
        tracing::warn!(
            caller_client_id = %caller_client_id,
            "reliability_summary: access denied — caller lacks Admin scope"
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: reliability_summary requires Admin scope"}],
            "isError": true
        });
    }

    // ── Parse parameters ──────────────────────────────────────────────────────
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => {
            if id.len() > MAX_AGENT_ID_LEN {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: agent_id must not exceed {MAX_AGENT_ID_LEN} characters")}],
                    "isError": true
                });
            }
            id.to_owned()
        }
        _ => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Error: reliability_summary requires agent_id"}],
                "isError": true
            });
        }
    };

    let window_days: u32 = params
        .get("window_days")
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, 365) as u32)
        .unwrap_or(7);

    tracing::info!(
        caller_client_id = %caller_client_id,
        agent_id = %agent_id,
        window_days = window_days,
        "reliability_summary invoked"
    );

    use duduclaw_gateway::evolution_events::query::AuditEventIndex;

    // ── Open and sync the audit index ────────────────────────────────────────
    let idx = match AuditEventIndex::open(home_dir) {
        Ok(i) => i,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: cannot open audit index: {e}")}],
                "isError": true
            });
        }
    };

    if let Err(e) = idx.sync_from_files().await {
        tracing::warn!("reliability_summary: sync warning (stale index): {e}");
    }

    // ── Compute summary ───────────────────────────────────────────────────────
    match idx
        .compute_reliability_summary(&agent_id, window_days)
        .await
    {
        Ok(s) => {
            let report = format!(
                "Agent Reliability Summary\n\
                 ─────────────────────────\n\
                 Agent:                  {agent_id}\n\
                 Window:                 {window_days} days\n\
                 Total events:           {total}\n\
                 \n\
                 Consistency Score:      {consistency:.4}  (per-task-type avg success rate)\n\
                 Task Success Rate:      {success:.4}  (outcome=success / total)\n\
                 Skill Adoption Rate:    {adoption:.4}  (skill_activate / total)\n\
                 Fallback Trigger Rate:  {fallback:.4}  (llm_fallback_triggered / total)\n\
                 \n\
                 Generated:              {generated_at}",
                agent_id = s.agent_id,
                window_days = s.window_days,
                total = s.total_events,
                consistency = s.consistency_score,
                success = s.task_success_rate,
                adoption = s.skill_adoption_rate,
                fallback = s.fallback_trigger_rate,
                generated_at = s.generated_at,
            );

            serde_json::json!({
                "content": [{"type": "text", "text": report}],
                "reliability_summary": {
                    "agent_id":             s.agent_id,
                    "window_days":          s.window_days,
                    "consistency_score":    s.consistency_score,
                    "task_success_rate":    s.task_success_rate,
                    "skill_adoption_rate":  s.skill_adoption_rate,
                    "fallback_trigger_rate": s.fallback_trigger_rate,
                    "total_events":         s.total_events,
                    "generated_at":         s.generated_at,
                }
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: reliability computation failed: {e}")}],
            "isError": true
        }),
    }
}

// ── Local inference handlers ────────────────────────────────
