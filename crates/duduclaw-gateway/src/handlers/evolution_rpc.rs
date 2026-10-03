//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Evolution ────────────────────────────────────────────

    pub(crate) async fn handle_evolution_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let mut gvu_enabled_count = 0usize;
        let agents: Vec<Value> = reg
            .list()
            .iter()
            .map(|a| {
                let cfg = &a.config;
                if cfg.evolution.gvu_enabled {
                    gvu_enabled_count += 1;
                }
                json!({
                    "agent_id": cfg.agent.name,
                    "gvu_enabled": cfg.evolution.gvu_enabled,
                    "cognitive_memory": cfg.evolution.cognitive_memory_enabled(),
                    // v1.68: `skill_auto_activate` / `skill_security_scan` removed (no reader).
                    "max_silence_hours": cfg.evolution.max_silence_hours,
                })
            })
            .collect();
        let total_agents = agents.len();
        let agent_ids: Vec<String> = reg
            .list()
            .iter()
            .map(|a| a.config.agent.name.clone())
            .collect();
        drop(reg);

        // S11 (2026-09-29): `total_versions` / `last_applied_at` counted
        // SOUL.md versions. Nothing writes those any more — the aggregate
        // that replaced them is the AEE experiment log, which is what a round
        // actually produces.
        let db_path = self.home_dir.join("evolution.db");
        let (total_rounds, applied_rounds) = if db_path.exists() {
            let vs = VersionStore::new(&db_path);
            let mut total: u64 = 0;
            let mut applied: u64 = 0;
            for aid in &agent_ids {
                let summary = vs.get_experiment_summary(aid);
                total += summary.total_experiments;
                applied += summary.applied_count;
            }
            (total, applied)
        } else {
            (0u64, 0u64)
        };

        let enabled = gvu_enabled_count > 0;
        WsFrame::ok_response(
            "",
            json!({
                "enabled": enabled,
                "mode": if enabled { "prediction_driven" } else { "disabled" },
                "total_agents": total_agents,
                "gvu_enabled_count": gvu_enabled_count,
                "total_rounds": total_rounds,
                "applied_rounds": applied_rounds,
                "agents": agents,
            }),
        )
    }

    /// Resolve the `agent_id` param into a list of agent ids to scope a
    /// dashboard query over: the single named agent, or every registered
    /// agent when the param is absent/empty (mirrors the old
    /// `handle_evolution_history`'s
    /// existing "empty → all agents" convention).
    pub(crate) async fn resolve_evolution_agent_ids(&self, agent_id: &str) -> Vec<String> {
        if !agent_id.is_empty() {
            return vec![agent_id.to_string()];
        }
        let reg = self.registry.read().await;
        reg.list()
            .iter()
            .map(|a| a.config.agent.name.clone())
            .collect()
    }

    /// `evolution.stagnation` — AVO §2.4 stagnation detector snapshot, exposed
    /// read-only to the dashboard (WP0.5 wiring: the detector itself already
    /// runs in `StagnationMonitor::tick`; this RPC lets an operator query the
    /// same signals on demand instead of waiting for the next alert). Optional
    /// `agent_id`; empty scopes to every registered agent.
    pub(crate) async fn handle_evolution_stagnation(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let agent_ids = self.resolve_evolution_agent_ids(agent_id).await;
        let agents_dir = self.home_dir.join("agents");
        let db_path = self.home_dir.join("evolution.db");

        // No GVU history yet anywhere on this install — every agent is
        // trivially "not stagnant" (there is nothing to be stuck on).
        if !db_path.exists() {
            let snapshots: Vec<Value> = agent_ids
                .iter()
                .map(|aid| {
                    json!({
                        "agent_id": aid,
                        "is_stagnant": false,
                        "signals": [],
                        "summary": null,
                        "checked_at": Utc::now().to_rfc3339(),
                    })
                })
                .collect();
            return WsFrame::ok_response("", json!({ "snapshots": snapshots }));
        }

        let vs = VersionStore::new(&db_path);
        let snapshots: Vec<Value> = agent_ids
            .iter()
            .map(|aid| {
                let cfg = GvuStagnationConfig::from_agent_dir(&agents_dir.join(aid));
                let snap = stagnation_snapshot(&vs, aid, &cfg);
                let stagnant = snap.is_stagnant();
                json!({
                    "agent_id": snap.agent_id,
                    "is_stagnant": stagnant,
                    "signals": snap.signals,
                    "summary": if stagnant { Some(snap.summary_zh()) } else { None },
                    "checked_at": snap.checked_at.to_rfc3339(),
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "snapshots": snapshots }))
    }

    /// `evolution.telemetry` — WP0.6 Verifier/Updater rejection distribution
    /// (ABC §3.3 P2 diagnostic half). Optional `agent_id` (empty aggregates
    /// every registered agent) + `days` (default 7, capped 1..=90).
    /// `forward.summary` — per-agent aggregates over the task forward-model
    /// audit trail (`prediction.db` / `task_prediction_log`). Read-only,
    /// fail-open (missing db ⇒ empty), window-bounded and honest about it
    /// (`window_scanned`). Optional `agent_id` scopes server-side.
    pub(crate) async fn handle_forward_summary(&self, params: Value) -> WsFrame {
        let agent_filter = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(a) = agent_filter.as_deref() {
            if !is_valid_agent_id(a) {
                return WsFrame::error_response("", "Invalid agent_id format");
            }
        }
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::forward_view::forward_summaries(&db_path, agent_filter.as_deref())
        })
        .await;
        match result {
            Ok((summaries, scanned)) => WsFrame::ok_response(
                "",
                json!({
                    "agents": summaries,
                    "window_scanned": scanned,
                    "window_cap": crate::prediction::forward_view::SCAN_CAP,
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("forward summary: {e}")),
        }
    }

    /// `forward.recent` — newest predictions (settled + pending), newest
    /// first. Same access bar and fail-open shape as `forward.summary`.
    pub(crate) async fn handle_forward_recent(&self, params: Value) -> WsFrame {
        let agent_filter = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(a) = agent_filter.as_deref() {
            if !is_valid_agent_id(a) {
                return WsFrame::error_response("", "Invalid agent_id format");
            }
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(crate::prediction::forward_view::RECENT_MAX_LIMIT as u64)
            as usize;
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::forward_view::forward_recent(
                &db_path,
                agent_filter.as_deref(),
                limit,
            )
        })
        .await;
        match result {
            Ok(mut rows) => {
                // Resolve task-board titles so the list reads as "which goal,
                // which round" instead of a bare uuid prefix (forward_view
                // reads only prediction.db; titles live in tasks.db). Missing
                // store / vanished tasks simply leave the title `None`.
                if let Ok(store) = self.task_store().await {
                    let mut titles: std::collections::HashMap<String, Option<String>> =
                        std::collections::HashMap::new();
                    for row in &mut rows {
                        let entry = match titles.entry(row.task_id.clone()) {
                            std::collections::hash_map::Entry::Occupied(o) => o.get().clone(),
                            std::collections::hash_map::Entry::Vacant(v) => {
                                let t = store
                                    .get_task(&row.task_id)
                                    .await
                                    .ok()
                                    .flatten()
                                    .map(|t| t.title);
                                v.insert(t.clone());
                                t
                            }
                        };
                        row.task_title = entry;
                    }
                }
                WsFrame::ok_response("", json!({ "predictions": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("forward recent: {e}")),
        }
    }

    /// `forward.chain` — every round of one task's predict→act→observe→score
    /// loop, oldest round first, with the stored prediction/observation JSON
    /// parsed into typed expected/observed sides. The drill-down the list
    /// views can't provide (their SELECT deliberately skips the JSON blobs).
    pub(crate) async fn handle_forward_chain(&self, params: Value) -> WsFrame {
        let Some(task_id) = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .map(str::to_string)
        else {
            return WsFrame::error_response("", "Missing 'task_id' parameter");
        };
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::forward_view::forward_chain(&db_path, &task_id)
        })
        .await;
        match result {
            Ok(rounds) => WsFrame::ok_response("", json!({ "rounds": rounds })),
            Err(e) => WsFrame::error_response("", &format!("forward chain: {e}")),
        }
    }

    /// `forward.calibration` — query-time skill verdict for one agent
    /// (Brier / Murphy decomposition / reliability bins / three-state
    /// honest label). Nothing precomputed; empty store ⇒ `candidate`.
    pub(crate) async fn handle_forward_calibration(&self, params: Value) -> WsFrame {
        let Some(agent_id) = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::forward_view::forward_calibration(&db_path, &agent_id)
        })
        .await;
        match result {
            Ok(view) => WsFrame::ok_response("", json!({ "calibration": view })),
            Err(e) => WsFrame::error_response("", &format!("forward calibration: {e}")),
        }
    }

    /// `forward.states` — learned state buckets (`task_state_models`),
    /// most-sampled first. The "what has the world model actually learned"
    /// surface; previously this table had zero read paths outside the
    /// prediction engine itself.
    pub(crate) async fn handle_forward_states(&self, params: Value) -> WsFrame {
        let agent_filter = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(a) = agent_filter.as_deref() {
            if !is_valid_agent_id(a) {
                return WsFrame::error_response("", "Invalid agent_id format");
            }
        }
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::forward_view::forward_states(
                &db_path,
                agent_filter.as_deref(),
                limit,
            )
        })
        .await;
        match result {
            Ok(states) => WsFrame::ok_response("", json!({ "states": states })),
            Err(e) => WsFrame::error_response("", &format!("forward states: {e}")),
        }
    }

    /// `belief.recent` — newest belief entries (settled + pending),
    /// newest first. Optional `agent_id` scopes server-side; `limit` default
    /// 50, capped 200. Same fail-open shape as `forward.recent`: a missing
    /// store yields an empty list, never an error.
    pub(crate) async fn handle_belief_recent(&self, params: Value) -> WsFrame {
        let agent_filter = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(a) = agent_filter.as_deref() {
            if !is_valid_agent_id(a) {
                return WsFrame::error_response("", "Invalid agent_id format");
            }
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(200) as usize;
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::belief::recent(&db_path, agent_filter.as_deref(), limit)
        })
        .await;
        match result {
            Ok(beliefs) => WsFrame::ok_response("", json!({ "beliefs": beliefs })),
            Err(e) => WsFrame::error_response("", &format!("belief recent: {e}")),
        }
    }

    /// `belief.summary` — per-agent belief stats (`agent_id` required).
    /// Calibration figures live only under `stats.verified` (cross-checked
    /// settlements); `stats.self_reported` is a count plus a descriptive
    /// rate. Fewer than 30 verified settlements ⇒ `calibration_status` is not
    /// `calibrated` and every derived figure is `null` (§0-3).
    pub(crate) async fn handle_belief_summary(&self, params: Value) -> WsFrame {
        let Some(agent_id) = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "agent_id is required");
        };
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        let agent_id = agent_id.to_string();
        let db_path = self.home_dir.join("prediction.db");
        let result = tokio::task::spawn_blocking(move || {
            crate::prediction::belief::stats(&db_path, &agent_id)
        })
        .await;
        match result {
            Ok(stats) => WsFrame::ok_response("", json!({ "stats": stats })),
            Err(e) => WsFrame::error_response("", &format!("belief summary: {e}")),
        }
    }

    pub(crate) async fn handle_evolution_telemetry(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let days = params
            .get("days")
            .and_then(|v| v.as_i64())
            .unwrap_or(7)
            .clamp(1, 90);
        let agent_ids = self.resolve_evolution_agent_ids(agent_id).await;

        let mut total: u64 = 0;
        let mut by_stage_layer: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, u64>,
        > = Default::default();
        for aid in &agent_ids {
            let summary = telemetry_summary(&self.home_dir, aid, days);
            total += summary.total;
            for (stage, layers) in summary.by_stage_layer {
                let stage_entry = by_stage_layer.entry(stage).or_default();
                for (layer, count) in layers {
                    *stage_entry.entry(layer).or_insert(0) += count;
                }
            }
        }

        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "days": days,
                "total": total,
                "by_stage_layer": by_stage_layer,
            }),
        )
    }
}
