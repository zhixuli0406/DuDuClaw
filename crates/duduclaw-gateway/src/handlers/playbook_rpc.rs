//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Playbook (WP1.2/1.3 gene-shaped experience entries) ──────

    /// `playbook.list` — every current playbook entry for one agent (all
    /// lifecycle states, including `retired`, so the dashboard can show full
    /// audit history; `select.rs`'s injection filtering is a separate, more
    /// restrictive concern this RPC does not replicate).
    pub(crate) async fn handle_playbook_list(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "agent_id": agent_id, "entries": [] }));
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        let active = playbook::list_active(&engine, agent_id).await;
        let entries: Vec<Value> = active
            .into_iter()
            .map(|(mem, meta, stats)| {
                // W3-2: the stored `content` is written for the model. Every
                // read surface also gets the zero-LLM zh-TW rewrite so the UI
                // can lead with plain language and keep the raw text folded
                // away. `tags` carry the shadow/probation signal the metadata
                // blob alone cannot express (see `humanize::status_of`).
                let h = playbook::humanize(&mem.content, &meta, &stats, &mem.tags);
                json!({
                    "id": mem.id,
                    "content": mem.content,
                    "category": meta.category.as_str(),
                    "state": meta.state.as_str(),
                    "signals_match": meta.signals_match,
                    "eval_cases": meta.eval_cases.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                    "success_streak": meta.success_streak,
                    "revision": meta.revision,
                    "helpful": stats.helpful,
                    "harmful": stats.harmful,
                    "net_score": stats.net(),
                    "origin": meta.origin,
                    "created_at": mem.timestamp.to_rfc3339(),
                    "humanized": {
                        "sentence": h.sentence,
                        "condition": h.condition,
                        "action": h.action,
                        "purpose": h.purpose,
                        "purpose_key": h.purpose_key,
                        "status": h.status,
                        "status_key": h.status_key,
                        "why": h.why,
                        "fallback": h.fallback,
                        "evidence": {
                            "eval_cases": h.evidence.eval_cases,
                            "failure_notes": h.evidence.failure_notes,
                            "applications": h.evidence.applications,
                            "helpful": h.evidence.helpful,
                            "harmful": h.evidence.harmful,
                            "success_streak": h.evidence.success_streak,
                        },
                    },
                })
            })
            .collect();

        WsFrame::ok_response("", json!({ "agent_id": agent_id, "entries": entries }))
    }

    /// `playbook.retire` — human-initiated terminal retirement of one entry
    /// (mirrors `memory.forget`'s Owner-level, single-row, non-batch shape).
    /// Goes through the same `PlaybookDelta::Retire` + `apply_deltas` path the
    /// autonomous AEE loop uses — there is no separate "operator delete" code
    /// path, so a manual retire is auditable exactly like an automatic one.
    pub(crate) async fn handle_playbook_retire(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let entry_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let reason_raw = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let reason = if reason_raw.is_empty() {
            "operator manual retire"
        } else {
            reason_raw
        };

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        if entry_id.is_empty() {
            return WsFrame::error_response("", "Missing 'id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::error_response("", "Playbook entry not found");
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        // `Retire` deltas never touch `must_not`/eval-case validation (see
        // `delta::validate_delta`), so an empty contract slice + the agent's
        // eval root are safe placeholders here — no CONTRACT.toml read needed
        // for this op.
        let eval_cases_root = self.home_dir.join("agents").join(agent_id).join("evals");
        let delta = playbook::PlaybookDelta::Retire {
            id: entry_id.to_string(),
            reason: reason.to_string(),
        };
        let outcome = playbook::apply_deltas(
            &engine,
            agent_id,
            vec![delta],
            &[],
            &eval_cases_root,
            Utc::now(),
        )
        .await;

        if let Some((_, reason)) = outcome.rejected.first() {
            return WsFrame::error_response("", &format!("Retire rejected: {reason}"));
        }
        let retired = outcome
            .applied
            .iter()
            .any(|op| matches!(op, playbook::AppliedOp::Retired { .. }));
        if !retired {
            return WsFrame::ok_response(
                "",
                json!({
                    "success": false,
                    "retired": false,
                    "reason": "entry not found (already retired or unknown id)",
                }),
            );
        }
        // W3-2: a human switching a rule off is a learning event too — record
        // it so the daily digest and the Activity Feed show it next to the
        // automatic adoptions/rollbacks. Best-effort: the retire already
        // succeeded, so a feed failure must not turn into an RPC error.
        if let Some(store) = self.task_store.read().await.clone() {
            let row = ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "playbook_rule_retired".to_string(),
                agent_id: agent_id.to_string(),
                task_id: None,
                summary: format!("管理者停用了 AI 員工「{agent_id}」的一條經驗法則"),
                timestamp: Utc::now().to_rfc3339(),
                metadata: None,
            };
            if let Err(e) = store.append_activity(&row).await {
                tracing::debug!(error = %e, "playbook.retire: activity append failed (non-fatal)");
            }
        }
        WsFrame::ok_response(
            "",
            json!({ "success": true, "retired": true, "id": entry_id }),
        )
    }

    /// `playbook.export` — lossless GEP-gene-shaped JSON export (D5=B: local
    /// schema alignment only, no hub I/O). Patches `x-duduclaw.entry_id` /
    /// `.agent_id` onto each gene per `gene::to_gene`'s documented caller
    /// contract (the pure function itself has no id to fill those with).
    pub(crate) async fn handle_playbook_export(&self, params: Value) -> WsFrame {
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
                json!({ "agent_id": agent_id, "gene_schema": playbook::gene::GENE_SCHEMA, "genes": [] }),
            );
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        let active = playbook::list_active(&engine, agent_id).await;
        let genes: Vec<Value> = active
            .into_iter()
            .map(|(mem, meta, stats)| {
                let mut gene = playbook::to_gene(&mem.content, &meta, &stats);
                if let Some(obj) = gene.get_mut("x-duduclaw").and_then(|v| v.as_object_mut()) {
                    obj.insert("entry_id".to_string(), json!(mem.id));
                    obj.insert("agent_id".to_string(), json!(agent_id));
                }
                gene
            })
            .collect();

        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "gene_schema": playbook::gene::GENE_SCHEMA,
                "genes": genes,
            }),
        )
    }
}
