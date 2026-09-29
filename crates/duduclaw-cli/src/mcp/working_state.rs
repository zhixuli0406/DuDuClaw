use super::*;

pub(crate) async fn handle_working_state_set(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let value = args
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // Number-or-numeric-string tolerance: some CLI runtimes stringify args.
    let ttl_hours = args.get("ttl_hours").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    });
    let expected_value = args
        .get("expected_value")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::working_state::set_entry(
            &home,
            &agent,
            &key,
            &value,
            &reason,
            ttl_hours,
            expected_value.as_deref(),
        )
    })
    .await
    .unwrap_or_else(|e| Err(format!("working_state_set join error: {e}")));
    match result {
        Ok(out) => tool_text(
            &serde_json::json!({ "ok": true, "version": out.version, "superseded": out.superseded, "truncated": out.truncated })
                .to_string(),
        ),
        Err(e) => tool_error(&e),
    }
}

pub(crate) async fn handle_working_state_clear(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::working_state::clear_entry(&home, &agent, &key, &reason)
    })
    .await
    .unwrap_or_else(|e| Err(format!("working_state_clear join error: {e}")));
    match result {
        Ok(out) => tool_text(
            &serde_json::json!({ "ok": true, "version": out.version, "retired_value": out.superseded })
                .to_string(),
        ),
        Err(e) => tool_error(&e),
    }
}

pub(crate) async fn handle_working_state_handoff(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let note = args
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // H8: `status` opts into the structured Ralph-style report; omitted or
    // blank ⇒ legacy plain-note mode (see `working_state::set_handoff`).
    let status = match args.get("status").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => {
            match duduclaw_gateway::working_state::HandoffStatus::parse(s) {
                Ok(st) => Some(st),
                Err(e) => return tool_error(&e),
            }
        }
        _ => None,
    };
    let next_steps = args
        .get("next_steps")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let evidence = args
        .get("evidence")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let blocker = args
        .get("blocker")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::working_state::set_handoff(
            &home,
            &agent,
            &note,
            status,
            next_steps.as_deref(),
            evidence.as_deref(),
            blocker.as_deref(),
        )
    })
    .await
    .unwrap_or_else(|e| Err(format!("working_state_handoff join error: {e}")));
    match result {
        Ok(out) => tool_text(
            &serde_json::json!({ "ok": true, "version": out.version, "truncated": out.truncated })
                .to_string(),
        ),
        Err(e) => tool_error(&e),
    }
}

pub(crate) async fn handle_working_state_get(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let limit = args
        .get("history_limit")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .map(|n| (n as usize).clamp(1, 100))
        .unwrap_or(20);
    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::working_state::read_full(&home, &agent, limit)
    })
    .await
    .unwrap_or_else(|e| Err(format!("working_state_get join error: {e}")));
    match result {
        Ok(v) => tool_text(&v.to_string()),
        Err(e) => tool_error(&e),
    }
}

// ── Team-as-Agent handoff (P1/WP-5, design §3.4) ───────────────────────────
//
// `team_handoff` is the ONLY way a `TaskPacket` crosses a role boundary. It
// follows the `working_state_set` discipline exactly: an explicit tool call
// (never parsed out of completion text), identity taken from the process and
// never from a parameter, every write audit-logged, every refusal a stable
// code. Two extra properties are specific to packets:
//
//   * **Reject, never truncate.** An over-cap packet is refused whole
//     (`TaskPacket::validate`). Trimming `constraints` to fit is the measured
//     failure (arXiv:2608.29028) the field exists to prevent.
//   * **Self-echo.** `team_handoff` is on
//     `duduclaw_core::grounding::SELF_ECHO_TOOL_NAMES`: a packet is the
//     sending role's own summary, so it can never ground that role's claims.
