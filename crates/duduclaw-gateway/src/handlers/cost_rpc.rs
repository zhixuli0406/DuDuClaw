//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;
use crate::dept_grants::{department_grant_allows, DeptGrantView, GrantAction, GrantGroup};

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
    pub(crate) fn cost_telemetry(&self) -> Option<&crate::cost_telemetry::CostTelemetry> {
        #[cfg(test)]
        if let Some(telemetry) = self.cost_telemetry_fixture.get() {
            return Some(telemetry);
        }
        if crate::cost_telemetry::get_telemetry().is_none() {
            let _ = crate::cost_telemetry::init_telemetry(&self.home_dir);
        }
        crate::cost_telemetry::get_telemetry()
    }

    /// `cost.summary` — window totals: tokens / cost / cache-hit-rate + a
    /// derived 200K price-cliff status block. `hours` defaults to 24.
    pub(crate) async fn handle_cost_summary(&self, params: Value) -> WsFrame {
        let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);
        let since = match cost_since(&params, chrono::Utc::now()) {
            Ok(since) => since,
            Err(message) => return WsFrame::error_response("", message),
        };
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({ "available": false }));
        };

        let result = match since {
            Some(since) => tel.summary_global_since(since).await,
            None => tel.summary_global(hours).await,
        };
        let summary = match result {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("Cost summary failed: {e}")),
        };

        // Price-cliff status: reuse `near_price_cliff_for` (registry-derived
        // per-model threshold, 180K fallback) over recent per-request records —
        // no new pricing math. This is a status signal, not the aggregate cost.
        let (requests_near_cliff, max_input_tokens) = match tel.recent_records(500).await {
            Ok(records) => price_cliff_status(&records, since),
            Err(_) => (0, 0),
        };

        WsFrame::ok_response(
            "",
            json!({
                "available": true,
                "period": summary.period,
                "since": since.map(|value| value.to_rfc3339()),
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

    /// Build an exact department scope before reading any cost records.
    async fn department_cost_agents(
        &self,
        params: &Value,
        ctx: &UserContext,
    ) -> Result<(String, Vec<(String, String)>), WsFrame> {
        let department = params.get("department").and_then(Value::as_str)
            .filter(|name| duduclaw_core::is_valid_department(name))
            .ok_or_else(|| WsFrame::error_response("", "invalid department"))?
            .to_owned();
        // Load before acquiring registry guards: this revalidates the live
        // account, credential epoch, provenance and grants on every request.
        let view = if ctx.is_admin() { DeptGrantView::empty() }
            else { DeptGrantView::load(self, ctx).await };
        if !ctx.is_admin() && !view.departments_for(GrantGroup::Cost).contains(&department) {
            return Err(WsFrame::error_response("", "permission denied"));
        }
        let org = duduclaw_core::org_store::load_for_write(&self.home_dir)
            .map_err(|_| WsFrame::error_response("", "permission denied"))?;
        let authoritative: std::collections::BTreeMap<String, String> = org.iter()
            .filter_map(|(id, _)| {
                duduclaw_agent::skill_loader::authoritative_department(&self.home_dir, id)
                    .map(|department| (id.clone(), department))
            }).collect();
        if ctx.is_admin() && !authoritative.values().any(|name| name == &department) {
            return Err(WsFrame::error_response("", "permission denied"));
        }
        let mut agents = {
            let registry = self.registry().read().await;
            registry.list().into_iter().filter_map(|agent| {
                let id = agent.dir.file_name()?.to_str()?;
                if authoritative.get(id) != Some(&department) || ctx.owner_only_denied(id) {
                    return None;
                }
                if !ctx.is_admin()
                    && !department_grant_allows(&view, id, GrantGroup::Cost, GrantAction::Read)
                {
                    return None;
                }
                Some((id.to_owned(), agent.config.agent.display_name.clone()))
            }).collect::<Vec<_>>()
        };
        agents.sort_by(|a, b| a.0.cmp(&b.0));
        Ok((department, agents))
    }

    /// Per-employee department costs; totals contain only the returned rows.
    pub(crate) async fn handle_cost_department_summary(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let (department, agents) = match self.department_cost_agents(&params, ctx).await {
            Ok(scope) => scope,
            Err(frame) => return frame,
        };
        let days = params.get("days").and_then(Value::as_u64).unwrap_or(30).clamp(1, 90);
        let mut totals = crate::cost_telemetry::CostSummary::default();
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({"available": false, "department": department,
                "days": days, "agents": [], "totals": department_cost_numbers(&totals)}));
        };
        let mut rows = Vec::with_capacity(agents.len());
        for (id, display_name) in agents {
            // Reuse the per-agent query: no company-wide or ephemeral buckets
            // can enter the result, and idle department employees get zero-valued rows.
            let summary = match tel.summary_by_agent(&id, days * 24).await {
                Ok(row) => row.summary,
                Err(error) => return WsFrame::error_response("", &format!("Cost department summary failed: {error}")),
            };
            totals.total_requests += summary.total_requests;
            totals.total_input_tokens += summary.total_input_tokens;
            totals.total_output_tokens += summary.total_output_tokens;
            totals.total_cost_millicents += summary.total_cost_millicents;
            totals.total_cache_savings_millicents += summary.total_cache_savings_millicents;
            // Weight each agent's average by its request count, then normalize below.
            totals.avg_cache_efficiency += summary.avg_cache_efficiency * summary.total_requests as f64;
            let mut row = department_cost_numbers(&summary);
            row["agent_id"] = json!(id);
            row["display_name"] = json!(display_name);
            rows.push(row);
        }
        totals.avg_cache_efficiency = if totals.total_requests == 0 {
            0.0
        } else {
            totals.avg_cache_efficiency / totals.total_requests as f64
        };
        WsFrame::ok_response("", json!({"available": true, "department": department,
            "days": days, "agents": rows, "totals": department_cost_numbers(&totals)}))
    }

    /// Recent records for the admitted department ids only, newest first.
    pub(crate) async fn handle_cost_department_recent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let (department, agents) = match self.department_cost_agents(&params, ctx).await {
            Ok(scope) => scope,
            Err(frame) => return frame,
        };
        let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 200) as u32;
        let Some(tel) = self.cost_telemetry() else {
            return WsFrame::ok_response("", json!({"available": false, "department": department, "records": []}));
        };
        let ids: Vec<String> = agents.into_iter().map(|(id, _)| id).collect();
        match tel.recent_records_for_agents(&ids, limit).await {
            Ok(records) => WsFrame::ok_response("", json!({"available": true, "department": department,
                "records": records.iter().map(cost_record_json).collect::<Vec<_>>()})),
            Err(error) => WsFrame::error_response("", &format!("Cost department recent failed: {error}")),
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
    /// historical NULL-role rows. Admin only.
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
                    .map(cost_record_json)
                    .collect();
                WsFrame::ok_response("", json!({ "available": true, "records": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("Cost recent failed: {e}")),
        }
    }
}

fn department_cost_numbers(summary: &crate::cost_telemetry::CostSummary) -> Value {
    json!({
        "total_requests": summary.total_requests,
        "total_input_tokens": summary.total_input_tokens,
        "total_output_tokens": summary.total_output_tokens,
        "total_cost_millicents": summary.total_cost_millicents,
        "total_cache_savings_millicents": summary.total_cache_savings_millicents,
        "avg_cache_efficiency": summary.avg_cache_efficiency,
    })
}

fn cost_record_json(r: &crate::cost_telemetry::CostRecord) -> Value {
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
}

fn price_cliff_status(
    records: &[crate::cost_telemetry::CostRecord],
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> (u64, u64) {
    let mut requests_near_cliff = 0;
    let mut max_input_tokens = 0;
    for record in records {
        if let Some(since) = since {
            // Compare instants in UTC, including the boundary itself. Malformed
            // timestamps cannot establish membership in the requested window.
            let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&record.created_at) else { continue; };
            if created_at.with_timezone(&chrono::Utc) < since { continue; }
        }
        let usage = crate::cost_telemetry::TokenUsage {
            input_tokens: record.input_tokens,
            cache_read_tokens: record.cache_read_tokens,
            cache_creation_tokens: record.cache_creation_tokens,
            output_tokens: record.output_tokens,
        };
        max_input_tokens = max_input_tokens.max(usage.total_input());
        if crate::cost_telemetry::near_price_cliff_for(&record.model, &usage) {
            requests_near_cliff += 1;
        }
    }
    (requests_near_cliff, max_input_tokens)
}

/// A supplied boundary takes precedence over hours and must fit retained data.
fn cost_since(params: &Value, now: chrono::DateTime<chrono::Utc>) -> Result<Option<chrono::DateTime<chrono::Utc>>, &'static str> {
    let Some(value) = params.get("since") else { return Ok(None); };
    let text = value.as_str().filter(|s| s.len() <= 64).ok_or("since must be RFC3339")?;
    let since = chrono::DateTime::parse_from_rfc3339(text).map_err(|_| "since must be RFC3339")?.with_timezone(&chrono::Utc);
    let days = crate::cost_telemetry::RETENTION_DAYS.min(31) as i64;
    if since > now || since < now - chrono::Duration::days(days) { return Err("since is outside telemetry retention"); }
    Ok(Some(since))
}
#[cfg(test)]
mod cost_since_tests {
    use super::*;
    #[test]
    fn cost_since_validates_type_format_future_and_retention() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-02T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        for bad in [json!(null), json!(12), json!("bad"), json!("2026-11-03T00:00:00Z"), json!("2026-10-01T00:00:00Z")] {
            assert!(cost_since(&json!({"since":bad}), now).is_err());
        }
        assert_eq!(cost_since(&json!({}), now), Ok(None));
        let since = cost_since(&json!({"since":"2026-11-02T00:00:00+08:00"}), now).unwrap().unwrap();
        assert_eq!(since.to_rfc3339(), "2026-11-01T16:00:00+00:00");
    }
    #[test]
    fn price_cliff_uses_the_same_inclusive_utc_since_boundary() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-02T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let since = cost_since(&json!({"since":"2026-11-02T00:00:00+08:00"}), now).unwrap();
        let record = |created_at: &str, input_tokens| crate::cost_telemetry::CostRecord {
            agent_id: "adu".into(), request_type: "chat".into(), model: "unknown-model".into(),
            input_tokens, cache_read_tokens: 0, cache_creation_tokens: 0, output_tokens: 0,
            cache_efficiency: 0.0, cost_millicents: 0, cache_hit_rate: 0.0,
            cache_savings_millicents: 0, created_at: created_at.into(),
            compressed: false, compression_stages: String::new(),
        };
        let records = [
            record("2026-11-01T15:59:59.999Z", 300_000),
            record("2026-11-01T16:00:00Z", 190_000),
            record("2026-11-02T00:00:00.001+08:00", 200_000),
            record("invalid", 400_000),
        ];
        assert_eq!(price_cliff_status(&records, since), (2, 200_000));
        assert_eq!(price_cliff_status(&records, since.map(|time| time + chrono::Duration::milliseconds(1))), (1, 200_000));
        assert_eq!(price_cliff_status(&records[..1], since), (0, 0));
        assert_eq!(price_cliff_status(&records[..3], None), (3, 300_000));
    }
}
