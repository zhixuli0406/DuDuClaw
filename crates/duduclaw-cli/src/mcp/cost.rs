use super::*;

pub(crate) async fn handle_cost_summary(params: &Value, home_dir: &Path) -> Value {
    // Ensure telemetry is initialized (idempotent on second call)
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => {
            return serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
            });
        }
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);
    let agent_id = params.get("agent_id").and_then(|v| v.as_str());

    // WP-A2: per-model split for the same window/agent, folded in as an extra
    // `by_model` key. Additive — every pre-existing field keeps its name and
    // value. A rollup error degrades to an empty array rather than failing the
    // whole call: the summary the caller asked for is still correct.
    let by_model = telemetry
        .summary_by_model(agent_id, cost_since_unix(hours))
        .await
        .unwrap_or_default();

    if let Some(agent_id) = agent_id {
        match telemetry.summary_by_agent(agent_id, hours).await {
            Ok(summary) => cost_payload_with_by_model(&summary, None, by_model),
            Err(e) => serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("Error: {e}")}]
            }),
        }
    } else {
        match telemetry.summary_global(hours).await {
            Ok(summary) => cost_payload_with_by_model(&summary, None, by_model),
            Err(e) => serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("Error: {e}")}]
            }),
        }
    }
}

/// `hours` (as accepted by the `cost_*` tools) → Unix-seconds cutoff for
/// [`CostTelemetry::summary_by_model`]. Clamped to one year like
/// `cost_telemetry::cutoff_time`, so the two views cover the same window.
pub(crate) fn cost_since_unix(hours: u64) -> i64 {
    let clamped = hours.min(8760) as i64;
    chrono::Utc::now().timestamp() - clamped * 3600
}

/// Render a `cost_*` payload with a `by_model` array added.
///
/// `body` is serialized and, when it is a JSON object, gains a `by_model` key
/// in place; when it is an array (the `cost_agents` shape — a top-level array
/// cannot carry a named sibling) it is nested under `wrap_key` alongside
/// `by_model`. Existing field names and values are untouched either way.
pub(crate) fn cost_payload_with_by_model<T: serde::Serialize>(
    body: &T,
    wrap_key: Option<&str>,
    by_model: Vec<duduclaw_gateway::cost_telemetry::ModelCostRow>,
) -> Value {
    let rows = serde_json::to_value(&by_model).unwrap_or(Value::Array(Vec::new()));
    let mut payload = serde_json::to_value(body).unwrap_or(Value::Null);
    match (payload.as_object_mut(), wrap_key) {
        (Some(obj), _) => {
            obj.insert("by_model".to_string(), rows);
        }
        (None, Some(key)) => {
            payload = serde_json::json!({ key: payload, "by_model": rows });
        }
        (None, None) => {
            payload = serde_json::json!({ "summary": payload, "by_model": rows });
        }
    }
    serde_json::json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&payload).unwrap_or_default()}]
    })
}

pub(crate) async fn handle_cost_agents(params: &Value, home_dir: &Path) -> Value {
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => {
            return serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
            });
        }
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);

    match telemetry.all_agents_summary(hours).await {
        Ok(agents) => {
            if agents.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No cost data in the selected time window."}]
                });
            }
            // WP-A2: the same window split by model, so "which agent" and
            // "which model" are answerable from one call. The agent rows keep
            // their fields; they move under `agents` because a top-level JSON
            // array cannot gain a named sibling key.
            let by_model = telemetry
                .summary_by_model(None, cost_since_unix(hours))
                .await
                .unwrap_or_default();
            cost_payload_with_by_model(&agents, Some("agents"), by_model)
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

pub(crate) async fn handle_cost_users(params: &Value, home_dir: &Path) -> Value {
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => {
            return serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
            });
        }
    };

    let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(24);

    match telemetry.summary_by_user(hours).await {
        Ok(users) => {
            if users.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No per-user cost data in the selected time window."}]
                });
            }
            let text = serde_json::to_string_pretty(&users).unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": text}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

/// O4 — honest multi-agent vs single-agent cost report (arXiv:2604.02460).
/// Granularity is per-agent per-day dispatch-vs-chat; the report carries its
/// own `granularity_note` explaining why per-episode linkage is not claimed.
pub(crate) async fn handle_cost_multi_vs_single(params: &Value, home_dir: &Path) -> Value {
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);

    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => {
            return serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
            });
        }
    };

    let days = params.get("days").and_then(|v| v.as_u64()).unwrap_or(7);

    match telemetry.multi_vs_single(days).await {
        Ok(report) => serde_json::json!({
            "content": [{"type": "text", "text": serde_json::to_string_pretty(&report).unwrap_or_default()}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

/// O4 — best-effort one-line zh-TW delegation-cost advisory for spawn/fork
/// tool responses. `None` when telemetry is unavailable (the spawn result
/// must never fail because of the advisory).
pub(crate) async fn delegation_cost_advisory(home_dir: &Path) -> Option<String> {
    let _ = duduclaw_gateway::cost_telemetry::init_telemetry(home_dir);
    let telemetry = duduclaw_gateway::cost_telemetry::get_telemetry()?;
    let (dispatch, direct) = telemetry.dispatch_vs_direct_totals(24).await.ok()?;
    Some(duduclaw_gateway::cost_telemetry::render_delegation_advisory(dispatch, direct, 24))
}

pub(crate) async fn handle_cost_recent(params: &Value) -> Value {
    let telemetry = match duduclaw_gateway::cost_telemetry::get_telemetry() {
        Some(t) => t,
        None => {
            return serde_json::json!({
                "isError": true,
                "content": [{"type": "text", "text": "Cost telemetry not initialized"}]
            });
        }
    };

    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    match telemetry.recent_records(limit).await {
        Ok(records) => {
            if records.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "No cost records yet."}]
                });
            }
            let text = serde_json::to_string_pretty(&records).unwrap_or_default();
            serde_json::json!({
                "content": [{"type": "text", "text": text}]
            })
        }
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Error: {e}")}]
        }),
    }
}

// ── Odoo ERP handlers ───────────────────────────────────────
