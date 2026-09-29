//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Security ────────────────────────────────────────────

    pub(crate) async fn handle_security_audit_log(&self, params: Value) -> WsFrame {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let events = duduclaw_security::audit::read_recent_events(&self.home_dir, limit);
        let events_json: Vec<Value> = events
            .iter()
            .map(|e| {
                json!({
                    "timestamp": e.timestamp,
                    "event_type": e.event_type,
                    "agent_id": e.agent_id,
                    "severity": e.severity,
                    "details": e.details,
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "events": events_json }))
    }

    /// Unified audit log that merges events from four JSONL sources:
    /// - `security_audit.jsonl` (SOUL drift / injection / quarantine events)
    /// - `tool_calls.jsonl` (MCP tool invocations)
    /// - `channel_failures.jsonl` (channel reply failures)
    /// - `feedback.jsonl` (heterogeneous user / evolution feedback signals)
    ///
    /// Each event is normalized into a common envelope with `source`,
    /// `event_type`, `severity`, `summary`, and `details`. Missing files are
    /// treated as zero-event sources; malformed lines are skipped silently.
    pub(crate) async fn handle_audit_unified_log(&self, params: Value) -> WsFrame {
        const DEFAULT_LIMIT: usize = 200;
        const MAX_LIMIT: usize = 1000;
        const SUMMARY_MAX_BYTES: usize = 240;

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        let all_sources = ["security", "tool_call", "channel_failure", "feedback"];
        let requested_sources: Vec<String> = params
            .get("sources")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(|x| x.to_string()))
                    .filter(|s| all_sources.contains(&s.as_str()))
                    .collect()
            })
            .unwrap_or_else(|| all_sources.iter().map(|s| s.to_string()).collect());

        let severity_filter = params
            .get("severity_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let agent_id_filter = params
            .get("agent_id_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Initialize counts for every source so the frontend always sees the
        // key even when the caller whitelisted a subset.
        let mut source_counts: std::collections::HashMap<String, usize> = all_sources
            .iter()
            .map(|s| ((*s).to_string(), 0usize))
            .collect();

        let mut events: Vec<Value> = Vec::new();

        // Helper: read jsonl file tolerating missing files + malformed lines.
        async fn read_jsonl_lines(path: &std::path::Path) -> Vec<Value> {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => content
                    .split('\n')
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect(),
                Err(_) => Vec::new(),
            }
        }

        // ── Source: security_audit.jsonl ────────────────────────────
        if requested_sources.iter().any(|s| s == "security") {
            let path = self.home_dir.join("security_audit.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("security".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let event_type = row
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let severity = row
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_lowercase();

                if let Some(sf) = &severity_filter
                    && &severity != sf
                {
                    continue;
                }
                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                let raw_summary = row
                    .get("details")
                    .map(|d| d.to_string())
                    .unwrap_or_default();
                let summary = truncate_bytes(&raw_summary, SUMMARY_MAX_BYTES).to_string();

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "security",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "security_audit": row },
                }));
            }
        }

        // ── Source: tool_calls.jsonl ────────────────────────────────
        if requested_sources.iter().any(|s| s == "tool_call") {
            let path = self.home_dir.join("tool_calls.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("tool_call".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = row
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let success = row.get("success").and_then(|v| v.as_bool()).unwrap_or(true);
                let params_summary = row
                    .get("params_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let severity = if success { "info" } else { "warning" };
                let event_type = format!(
                    "tool.{tool_name}.{}",
                    if success { "success" } else { "failure" }
                );
                let summary = truncate_bytes(params_summary, SUMMARY_MAX_BYTES).to_string();

                // severity_filter only applies to security per spec.
                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "tool_call",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "tool_call": row },
                }));
            }
        }

        // ── Source: channel_failures.jsonl ──────────────────────────
        if requested_sources.iter().any(|s| s == "channel_failure") {
            let path = self.home_dir.join("channel_failures.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("channel_failure".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Producer uses lowercase "agent" field; fall back to
                // "agent_id" to be defensive.
                let agent_id = row
                    .get("agent")
                    .or_else(|| row.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = row
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown");
                let error_msg = row.get("error").and_then(|v| v.as_str()).unwrap_or("");

                let event_type = format!("channel.{reason}");
                // W2-8: `channel_alerts::record_recovery` appends a
                // `channel_recovered` row to this SAME file (see its module
                // docs) so a consumer can tell "is this outage still
                // current?" without re-deriving the failure window. That row
                // carries no `error` field — it isn't a failure — so without
                // this branch it rendered here with a blank summary next to
                // the same amber "warning" border as an actual outage: a
                // recovery silently looking like an unexplained warning.
                let is_recovery = row.get("event").and_then(|v| v.as_str())
                    == Some(crate::channel_alerts::RECOVERED_EVENT);
                let (severity, summary) = if is_recovery {
                    let channel_name = row.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                    (
                        "info",
                        format!(
                            "通道「{}」已恢復正常發送",
                            crate::channel_alerts::channel_label(channel_name)
                        ),
                    )
                } else {
                    (
                        "warning",
                        truncate_bytes(error_msg, SUMMARY_MAX_BYTES).to_string(),
                    )
                };

                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "channel_failure",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "channel_failure": row },
                }));
            }
        }

        // ── Source: feedback.jsonl (heterogeneous shape) ────────────
        if requested_sources.iter().any(|s| s == "feedback") {
            let path = self.home_dir.join("feedback.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("feedback".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .or_else(|| row.get("agent"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // `signal_type` is used by evolution feedback; fall back to
                // `kind` / `type`, else "generic".
                let kind = row
                    .get("signal_type")
                    .or_else(|| row.get("kind"))
                    .or_else(|| row.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("generic");
                let event_type = format!("feedback.{kind}");

                // Prefer `detail`, fall back to `message`, else stringified row.
                let raw_summary = row
                    .get("detail")
                    .or_else(|| row.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| row.to_string());
                let summary = truncate_bytes(&raw_summary, SUMMARY_MAX_BYTES).to_string();

                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "feedback",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": "info",
                    "summary": summary,
                    "details": { "feedback": row },
                }));
            }
        }

        // Sort descending by timestamp. Lexicographic compare works for
        // RFC3339/ISO8601 timestamps with consistent timezone suffix.
        events.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });

        let total = events.len();
        events.truncate(limit);

        let counts_json = json!({
            "security": source_counts.get("security").copied().unwrap_or(0),
            "tool_call": source_counts.get("tool_call").copied().unwrap_or(0),
            "channel_failure": source_counts.get("channel_failure").copied().unwrap_or(0),
            "feedback": source_counts.get("feedback").copied().unwrap_or(0),
        });

        WsFrame::ok_response(
            "",
            json!({
                "events": events,
                "source_counts": counts_json,
                "total": total,
            }),
        )
    }

    /// Audit Trail Evolution Query — W19-P1 M4.
    ///
    /// Queries the SQLite-backed index cache of EvolutionEvent JSONL audit logs.
    /// Runs `sync_from_files()` first to pick up any new events written since
    /// the last query, then executes a filtered, paginated query.
    ///
    /// ## Parameters
    /// | Field        | Type   | Description                                    |
    /// |--------------|--------|------------------------------------------------|
    /// | `agent_id`   | string | Filter by agent (optional)                     |
    /// | `event_type` | string | Filter by event type, e.g. `governance_violation` |
    /// | `outcome`    | string | Filter by outcome, e.g. `blocked`              |
    /// | `skill_id`   | string | Filter by skill                                |
    /// | `since`      | string | RFC3339 lower bound (inclusive)                |
    /// | `until`      | string | RFC3339 upper bound (exclusive)                |
    /// | `limit`      | int    | Page size (default 100, max 1000)              |
    /// | `offset`     | int    | Pagination offset (default 0)                  |
    ///
    /// ## Response
    /// ```json
    /// { "events": [...], "total": N, "limit": L, "offset": O }
    /// ```
    pub(crate) async fn handle_audit_evolution_query(&self, params: Value) -> WsFrame {
        use crate::evolution_events::query::AuditQueryFilter;

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

        // M60: reuse the shared, background-synced index (no per-request open +
        // full sync). The background task keeps it fresh.
        let idx = match self.audit_index().await {
            Ok(i) => i,
            Err(e) => {
                warn!("audit.evolution_query: cannot open index: {e}");
                return WsFrame::error_response("", &format!("index open failed: {e}"));
            }
        };

        match idx.query(filter).await {
            Ok(result) => {
                let events_json: Vec<Value> = result
                    .events
                    .iter()
                    .map(|ev| {
                        json!({
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

                WsFrame::ok_response(
                    "",
                    json!({
                        "events": events_json,
                        "total":  result.total,
                        "limit":  result.limit,
                        "offset": result.offset,
                    }),
                )
            }
            Err(e) => {
                warn!("audit.evolution_query: query error: {e}");
                WsFrame::error_response("", &format!("query failed: {e}"))
            }
        }
    }

    /// WebSocket RPC handler for `audit.reliability_summary` (W20-P0).
    ///
    /// Computes the four-metric Agent Reliability Summary from the evolution-event
    /// audit trail SQLite index.  Requires Admin scope.
    ///
    /// ## Request params
    /// - `agent_id` (required) — Agent identifier to query
    /// - `window_days` (optional, default 7, clamped to 1–365)
    ///
    /// ## Response
    /// ```json
    /// { "agent_id": "...", "window_days": 7, "consistency_score": 0.87, ... }
    /// ```
    pub(crate) async fn handle_audit_reliability_summary(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => return WsFrame::error_response("", "agent_id is required"),
        };

        let window_days = params
            .get("window_days")
            .and_then(|v| v.as_u64())
            .map(|d| d.clamp(1, 365) as u32)
            .unwrap_or(7);

        // M60: reuse the shared, background-synced index.
        let idx = match self.audit_index().await {
            Ok(i) => i,
            Err(e) => {
                warn!("audit.reliability_summary: cannot open index: {e}");
                return WsFrame::error_response("", &format!("index open failed: {e}"));
            }
        };

        match idx
            .compute_reliability_summary(&agent_id, window_days)
            .await
        {
            Ok(s) => WsFrame::ok_response(
                "",
                json!({
                    "agent_id":            s.agent_id,
                    "window_days":         s.window_days,
                    "consistency_score":   s.consistency_score,
                    "task_success_rate":   s.task_success_rate,
                    "skill_adoption_rate": s.skill_adoption_rate,
                    "fallback_trigger_rate": s.fallback_trigger_rate,
                    "total_events":        s.total_events,
                    "generated_at":        s.generated_at,
                }),
            ),
            Err(e) => {
                warn!("audit.reliability_summary: compute error: {e}");
                WsFrame::error_response("", &format!("compute failed: {e}"))
            }
        }
    }
}
