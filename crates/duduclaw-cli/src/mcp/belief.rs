use super::*;

pub(crate) async fn handle_belief_submit(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let horizon = args
        .get("horizon")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let Some(prob) = args.get("prob").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    }) else {
        return tool_error("prob is required and must be a number in [0,1]");
    };
    let rationale = args
        .get("rationale")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| duduclaw_core::truncate_chars(s, 400));
    let ref_value = args.get("ref_value").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    });

    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let belief = duduclaw_gateway::prediction::belief::NewBelief {
        agent_id: agent,
        subject,
        horizon,
        direction,
        prob,
        rationale,
        ref_value,
        source_goal_id: None,
    };
    let result = tokio::task::spawn_blocking(move || {
        let db_path = home.join("prediction.db");
        duduclaw_gateway::prediction::belief::submit(&db_path, belief)
    })
    .await
    .unwrap_or_else(|e| Err(format!("belief_submit join error: {e}")));
    match result {
        Ok(belief_id) => {
            tool_text(&serde_json::json!({ "ok": true, "belief_id": belief_id }).to_string())
        }
        Err(e) => tool_error(&e),
    }
}

pub(crate) async fn handle_belief_settle(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let belief_id = args
        .get("belief_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if belief_id.is_empty() {
        return tool_error("belief_id is required");
    }
    let Some(realized_value) = args.get("realized_value").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    }) else {
        return tool_error("realized_value is required and must be a number");
    };
    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let db_path = home.join("prediction.db");
        // Cross-check against live tick data happens gateway-side (WP3's
        // tick-wake hook reads the same table); the MCP server process has
        // no TickHub access, so tick_price is always None here and
        // settle_source is honestly recorded as "agent_unverified" rather
        // than pretending to verify it.
        duduclaw_gateway::prediction::belief::settle(
            &db_path,
            &agent,
            &belief_id,
            realized_value,
            None,
        )
    })
    .await
    .unwrap_or_else(|e| Err(format!("belief_settle join error: {e}")));
    match result {
        Ok(row) => tool_text(&serde_json::to_string(&row).unwrap_or_else(|_| "{}".to_string())),
        Err(e) => tool_error(&e),
    }
}

pub(crate) async fn handle_belief_stats(home_dir: &Path, default_agent: &str) -> Value {
    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let db_path = home.join("prediction.db");
        duduclaw_gateway::prediction::belief::stats(&db_path, &agent)
    })
    .await;
    match result {
        Ok(stats) => tool_text(&serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string())),
        Err(e) => tool_error(&format!("belief_stats join error: {e}")),
    }
}
