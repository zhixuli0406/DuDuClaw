//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// F1 Temporal Memory (v1.19.0): return the full supersession chain for a
    /// fact, oldest → newest. Accepts either an explicit `(subject, predicate)`
    /// pair or a `memory_id` (resolved to its triple first). Reuses the engine's
    /// `get_history` — the ranking / validity logic is not re-implemented here.
    pub(crate) async fn handle_memory_history(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response(
                "",
                json!({ "subject": null, "predicate": null, "chain": [], "current_id": null }),
            );
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        // Resolve (subject, predicate): explicit params win; otherwise derive
        // from a supplied memory_id.
        let (subject, predicate) = {
            let s = params
                .get("subject")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let p = params
                .get("predicate")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match (s, p) {
                (Some(s), Some(p)) => (s.to_string(), p.to_string()),
                _ => {
                    let memory_id = params
                        .get("memory_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if memory_id.is_empty() {
                        return WsFrame::error_response(
                            "",
                            "Provide either (subject, predicate) or memory_id",
                        );
                    }
                    match engine.triple_for_id(agent_id, memory_id).await {
                        Ok(Some(t)) => t,
                        Ok(None) => {
                            // Fail-safe: not a temporal triple (or not owned) →
                            // empty chain, not an error.
                            return WsFrame::ok_response(
                                "",
                                json!({ "subject": null, "predicate": null, "chain": [], "current_id": null }),
                            );
                        }
                        Err(e) => {
                            return WsFrame::error_response(
                                "",
                                &format!("Triple lookup failed: {e}"),
                            );
                        }
                    }
                }
            }
        };

        match engine.get_history(agent_id, &subject, &predicate).await {
            Ok(records) => {
                let mut current_id: Option<String> = None;
                let chain: Vec<Value> = records
                    .iter()
                    .map(|r| {
                        let is_current = r.valid_until.is_none();
                        if is_current {
                            current_id = Some(r.id.clone());
                        }
                        json!({
                            "id": r.id,
                            "content": r.content,
                            "valid_from": r.valid_from,
                            "valid_until": r.valid_until,
                            "superseded_by": r.superseded_by,
                            "supersedes": r.supersedes,
                            "confidence": r.confidence,
                            "is_current": is_current,
                        })
                    })
                    .collect();
                WsFrame::ok_response(
                    "",
                    json!({
                        "subject": subject,
                        "predicate": predicate,
                        "chain": chain,
                        "current_id": current_id,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("Memory history failed: {e}")),
        }
    }

    /// F1 Temporal Memory (v1.19.0): point-in-time lookup — the fact that was
    /// valid for `(subject, predicate)` at instant `at` (RFC-3339). Reuses the
    /// engine's `get_at`. Returns `found: false` (not an error) when no version
    /// was valid at that instant.
    pub(crate) async fn handle_memory_at(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject = params
            .get("subject")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        let predicate = params
            .get("predicate")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        let at_raw = params.get("at").and_then(|v| v.as_str()).unwrap_or("");

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        if subject.is_empty() || predicate.is_empty() {
            return WsFrame::error_response("", "Missing 'subject' or 'predicate' parameter");
        }
        let at = match chrono::DateTime::parse_from_rfc3339(at_raw) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                return WsFrame::error_response(
                    "",
                    "Missing or invalid 'at' parameter (expected RFC-3339 timestamp)",
                );
            }
        };

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "found": false, "record": null }));
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        match engine.get_at(agent_id, subject, predicate, at).await {
            Ok(Some(r)) => WsFrame::ok_response(
                "",
                json!({
                    "found": true,
                    "record": {
                        "id": r.id,
                        "content": r.content,
                        "valid_from": r.valid_from,
                        "valid_until": r.valid_until,
                        "superseded_by": r.superseded_by,
                        "supersedes": r.supersedes,
                        "confidence": r.confidence,
                    },
                }),
            ),
            Ok(None) => WsFrame::ok_response("", json!({ "found": false, "record": null })),
            Err(e) => {
                WsFrame::error_response("", &format!("Memory point-in-time lookup failed: {e}"))
            }
        }
    }

    /// D6 (2026-07) — export the agent's SPO knowledge graph for the curation
    /// UI's force-directed viewer. `limit` (default 500, capped 2000) bounds the
    /// edge set; `truncated` flags when the newest-first cut kicked in. Nodes
    /// carry degree; edges carry `origin_trust` (source-confidence tier colour)
    /// and `quarantined` (held-for-review). Empty db ⇒ empty graph, not an error.
    pub(crate) async fn handle_memory_graph(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .clamp(1, 2000) as usize;

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response(
                "",
                json!({ "nodes": [], "edges": [], "truncated": false }),
            );
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        match engine.export_graph(agent_id, limit).await {
            Ok(g) => {
                let nodes: Vec<Value> = g
                    .nodes
                    .iter()
                    .map(|n| json!({ "entity": n.entity, "degree": n.degree }))
                    .collect();
                let edges: Vec<Value> = g
                    .edges
                    .iter()
                    .map(|e| {
                        json!({
                            "subject": e.subject,
                            "predicate": e.predicate,
                            "object": e.object,
                            "memory_id": e.memory_id,
                            "origin_trust": e.origin_trust,
                            "quarantined": e.quarantined,
                        })
                    })
                    .collect();
                WsFrame::ok_response(
                    "",
                    json!({
                        "nodes": nodes,
                        "edges": edges,
                        "truncated": g.truncated,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("Memory graph export failed: {e}")),
        }
    }

    /// D6 (2026-07) — DESTRUCTIVE by-origin rollback. Expires (never deletes)
    /// every currently-valid fact for `agent_id` that originated from exactly
    /// `origin`, optionally limited to facts learned at/after `since` (RFC-3339
    /// transaction time). Manager role + Owner agent access gated at the call
    /// site. Returns the number of rows expired. Exposed only on the dashboard
    /// RPC surface (not MCP) — the "清除此來源" queue action lives here.
    pub(crate) async fn handle_memory_invalidate_origin(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let origin = params
            .get("origin")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        if origin.is_empty() {
            return WsFrame::error_response("", "Missing 'origin' parameter");
        }
        // Optional `since` transaction-time bound (RFC-3339). Absent ⇒ all time.
        let since = match params.get("since").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => match chrono::DateTime::parse_from_rfc3339(s.trim())
            {
                Ok(dt) => Some(dt.with_timezone(&Utc)),
                Err(_) => {
                    return WsFrame::error_response(
                        "",
                        "Invalid 'since' parameter (expected RFC-3339 timestamp)",
                    );
                }
            },
            _ => None,
        };

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "expired": 0 }));
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        match engine.invalidate_by_origin(agent_id, origin, since).await {
            Ok(n) => WsFrame::ok_response("", json!({ "expired": n })),
            Err(e) => WsFrame::error_response("", &format!("Origin rollback failed: {e}")),
        }
    }
}
