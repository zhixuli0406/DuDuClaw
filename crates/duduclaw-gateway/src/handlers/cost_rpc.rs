//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Odoo ERP ─────────────────────────────────────────────────

    /// Return the current Odoo connection status.
    ///
    /// Reads `[odoo]` from config.toml, attempts to connect if configured,
    /// and returns connected/edition/version info.
    // ── Cost / cache-efficiency telemetry (dashboard surface for the
    //    `cost_summary` / `cost_agents` / `cost_recent` MCP tools) ──────────
    //
    // All three reuse the exact same `CostTelemetry` query methods the MCP
    // tools call (`summary_global` / `all_agents_summary` / `recent_records`) —
    // no second cost/cache-efficiency formula is defined here. `init_telemetry`
    // is idempotent; when telemetry is unavailable these return a well-formed
    // empty/zero payload rather than erroring, so the dashboard renders a clean
    // "no data yet" state instead of a red banner.

    /// Resolve the process-wide telemetry singleton, initialising it on first
    /// use. Returns `None` only if init itself fails (disk/permission).
    pub(crate) fn cost_telemetry(&self) -> Option<&'static crate::cost_telemetry::CostTelemetry> {
        if crate::cost_telemetry::get_telemetry().is_none() {
            let _ = crate::cost_telemetry::init_telemetry(&self.home_dir);
        }
        crate::cost_telemetry::get_telemetry()
    }

    /// `cost.summary` — window totals: tokens / cost / cache-hit-rate + a
    /// derived 200K price-cliff status block. `hours` defaults to 24.
    pub(crate) async fn handle_cost_summary(&self, params: Value) -> WsFrame {
        let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({ "available": false }));
        };

        let summary = match tel.summary_global(hours).await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("Cost summary failed: {e}")),
        };

        // Price-cliff status: reuse `near_price_cliff_for` (registry-derived
        // per-model threshold, 180K fallback) over recent per-request records —
        // no new pricing math. This is a status signal, not the aggregate cost.
        let mut requests_near_cliff = 0u64;
        let mut max_input_tokens = 0u64;
        if let Ok(records) = tel.recent_records(500).await {
            for r in &records {
                let usage = crate::cost_telemetry::TokenUsage {
                    input_tokens: r.input_tokens,
                    cache_read_tokens: r.cache_read_tokens,
                    cache_creation_tokens: r.cache_creation_tokens,
                    output_tokens: r.output_tokens,
                };
                let total_in = usage.total_input();
                if total_in > max_input_tokens {
                    max_input_tokens = total_in;
                }
                if crate::cost_telemetry::near_price_cliff_for(&r.model, &usage) {
                    requests_near_cliff += 1;
                }
            }
        }

        WsFrame::ok_response(
            "",
            json!({
                "available": true,
                "period": summary.period,
                "total_requests": summary.total_requests,
                "total_input_tokens": summary.total_input_tokens,
                "total_cache_read_tokens": summary.total_cache_read_tokens,
                "total_cache_creation_tokens": summary.total_cache_creation_tokens,
                "total_output_tokens": summary.total_output_tokens,
                "avg_cache_efficiency": summary.avg_cache_efficiency,
                // Alias: cache_efficiency == cache_hit_rate (both from the same formula).
                "cache_hit_rate": summary.avg_cache_hit_rate,
                "total_cost_millicents": summary.total_cost_millicents,
                "total_cache_savings_millicents": summary.total_cache_savings_millicents,
                "price_cliff": {
                    // Legacy model-agnostic warn threshold (registry may use a
                    // per-model one internally); surfaced for the UI copy.
                    "threshold_input_tokens": 180_000,
                    "requests_near_cliff": requests_near_cliff,
                    "max_input_tokens": max_input_tokens,
                    "warning": requests_near_cliff > 0,
                },
            }),
        )
    }

    /// `cost.agents` — per-agent cost + cache-health rollup. `hours` defaults
    /// to 24. Empty window → `{ agents: [] }` (not an error).
    pub(crate) async fn handle_cost_agents(&self, params: Value) -> WsFrame {
        let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({ "available": false, "agents": [] }));
        };
        match tel.all_agents_summary(hours).await {
            Ok(agents) => {
                let rows: Vec<Value> = agents.iter().map(|a| json!({
                    "agent_id": a.agent_id,
                    "cache_health": a.cache_health,
                    "total_requests": a.summary.total_requests,
                    "total_input_tokens": a.summary.total_input_tokens,
                    "total_cache_read_tokens": a.summary.total_cache_read_tokens,
                    "total_cache_creation_tokens": a.summary.total_cache_creation_tokens,
                    "total_output_tokens": a.summary.total_output_tokens,
                    "avg_cache_efficiency": a.summary.avg_cache_efficiency,
                    "total_cost_millicents": a.summary.total_cost_millicents,
                    "total_cache_savings_millicents": a.summary.total_cache_savings_millicents,
                })).collect();
                WsFrame::ok_response("", json!({ "available": true, "agents": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("Cost agents failed: {e}")),
        }
    }

    /// `cost.by_model` — per-model cost rollup (WP-A2), costliest model first.
    ///
    /// Params: `agent_id` (optional — omit for every agent), `days` (default 7,
    /// clamped to 365). Empty window → `{ by_model: [] }`, never an error.
    /// Rows with no recorded model id bucket under `"(unknown)"`.
    pub(crate) async fn handle_cost_by_model(&self, params: Value) -> WsFrame {
        let days = params
            .get("days")
            .and_then(|v| v.as_u64())
            .unwrap_or(7)
            .clamp(1, 365);
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({ "available": false, "by_model": [] }));
        };
        let since = chrono::Utc::now().timestamp() - (days as i64) * 86_400;
        match tel.summary_by_model(agent_id, since).await {
            Ok(rows) => {
                let out: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "model": r.model,
                            "requests": r.requests,
                            "input_tokens": r.input_tokens,
                            "output_tokens": r.output_tokens,
                            "cache_read_tokens": r.cache_read_tokens,
                            "cache_creation_tokens": r.cache_creation_tokens,
                            // Both units: `cost_millicents` matches every other
                            // `cost.*` RPC (the legacy cents scale), `cost_usd`
                            // spares the UI from re-deriving it.
                            "cost_millicents": r.cost_millicents,
                            "cost_usd": r.cost_usd,
                            "cache_efficiency": r.cache_efficiency,
                        })
                    })
                    .collect();
                WsFrame::ok_response(
                    "",
                    json!({
                        "available": true,
                        "days": days,
                        "agent_id": agent_id,
                        "by_model": out,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("Cost by_model failed: {e}")),
        }
    }

    /// `cost.by_role` — measured team-stage spend, with no attribution of
    /// historical NULL-role rows. Admin only, like all `cost.*` methods.
    pub(crate) async fn handle_cost_by_role(&self, params: Value) -> WsFrame {
        let days = params
            .get("days")
            .and_then(|v| v.as_u64())
            .unwrap_or(7)
            .clamp(1, 365);
        let episode_id = params
            .get("episode_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({ "available": false, "by_role": [] }));
        };
        let since = chrono::Utc::now().timestamp() - (days as i64) * 86_400;
        match tel.summary_by_role(episode_id, since).await {
            Ok(rows) => WsFrame::ok_response(
                "",
                json!({
                    "available": true,
                    "days": days,
                    "episode_id": episode_id,
                    "by_role": rows,
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("Cost by_role failed: {e}")),
        }
    }

    /// `cost.recent` — the last `limit` (≤500, default 20) per-request cost
    /// records, newest first.
    pub(crate) async fn handle_cost_recent(&self, params: Value) -> WsFrame {
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(500) as u32;
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({ "available": false, "records": [] }));
        };
        match tel.recent_records(limit).await {
            Ok(records) => {
                let rows: Vec<Value> = records
                    .iter()
                    .map(|r| {
                        json!({
                            "agent_id": r.agent_id,
                            "request_type": r.request_type,
                            "model": r.model,
                            "input_tokens": r.input_tokens,
                            "cache_read_tokens": r.cache_read_tokens,
                            "cache_creation_tokens": r.cache_creation_tokens,
                            "output_tokens": r.output_tokens,
                            "cache_efficiency": r.cache_efficiency,
                            "cost_millicents": r.cost_millicents,
                            "cache_savings_millicents": r.cache_savings_millicents,
                            "created_at": r.created_at,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "available": true, "records": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("Cost recent failed: {e}")),
        }
    }
}
