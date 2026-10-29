//! Host-owned per-call egress enforcement.
use duduclaw_core::egress_guard::{effective_mode_checked, Mode};
use serde_json::{json, Value};
use std::path::Path;

/// `Some` is a required single-call approval, never a standing grant.
pub async fn check(
    home: &Path,
    agent: &str,
    tool: &str,
    sink: bool,
) -> Result<Option<Value>, String> {
    if !sink {
        return Ok(None);
    }
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    if effective_mode_checked(home, Some(&dir))? == Mode::Off {
        return Ok(None);
    }
    let taint = crate::egress_client::query(home, agent).await;
    evaluate(home, agent, tool, sink, taint)
}

pub fn evaluate(
    home: &Path,
    agent: &str,
    tool: &str,
    sink: bool,
    taint: crate::egress_guard::Taint,
) -> Result<Option<Value>, String> {
    if !sink {
        return Ok(None);
    }
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let mode = effective_mode_checked(home, Some(&dir))?;
    evaluate_mode(home, agent, tool, mode, taint)
}

pub(crate) fn mode(home: &Path, agent: &str) -> Result<Mode, String> {
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    effective_mode_checked(home, Some(&dir))
}

/// Re-check while the dispatch lease is held, after every approval wait.
/// A grant for older sources cannot authorize newly observed private data.
pub fn confirm_dispatch(
    home: &Path, agent: &str, tool: &str, sink: bool,
    taint: crate::egress_guard::Taint, approved: Option<&Value>,
) -> Result<(), String> {
    if let Some(required) = evaluate(home, agent, tool, sink, taint)? {
        let covered = required["sources"].as_array().is_some_and(|sources| {
            approved.and_then(|a| a["sources"].as_array())
                .is_some_and(|old| sources.iter().all(|source| old.contains(source)))
        });
        if !covered {
            return Err("等待核准期間本回合的私人資料來源已改變，已暫停對外動作；請重試並重新核准。".into());
        }
    }
    Ok(())
}

/// External API clients have no gateway-owned turn or employee policy.
/// Never inherit the MCP server process's turn identity for these callers.
pub fn check_external(home: &Path, client: &str, tool: &str, sink: bool) -> Result<Option<Value>, String> {
    if !sink {
        return Ok(None);
    }
    evaluate_mode(home, client, tool, effective_mode_checked(home, None)?, crate::egress_guard::Taint::unknown())
}

/// An authenticated employee API key still has no host-owned runtime turn.
/// Keep its narrowing policy, but never borrow the server process's turn.
pub fn check_unattributed_employee(home: &Path, agent: &str, tool: &str, sink: bool) -> Result<Option<Value>, String> {
    evaluate(home, agent, tool, sink, crate::egress_guard::Taint::unknown())
}

fn evaluate_mode(
    home: &Path,
    agent: &str,
    tool: &str,
    mode: Mode,
    taint: crate::egress_guard::Taint,
) -> Result<Option<Value>, String> {
    if mode == Mode::Off {
        return Ok(None);
    }
    if !taint.tainted {
        return Ok(None);
    }
    match mode {
        Mode::Off => Ok(None),
        Mode::Shadow => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home,
                agent,
                tool,
                "egress_guard: would ask before sending private data",
                true,
                &[
                    ("egress_guard", json!("would_ask")),
                    ("sources", json!(taint.sources)),
                ],
            );
            Ok(None)
        }
        Mode::Ask => Ok(Some(
            json!({"reason": "egress_guard", "sources": taint.sources}),
        )),
        Mode::Block => Err(format!(
            "外送防護已阻擋工具「{tool}」：本回合已讀取私人資料（來源：{}）。",
            taint.sources.join("、")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_turn_lost_during_approval_requires_unknown_source_approval() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "").unwrap();
        for mode in ["ask", "block", "shadow"] {
            std::fs::write(home.path().join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
            assert_eq!(confirm_dispatch(home.path(), "bot", "send_message", true, crate::egress_guard::Taint::unknown(), None).is_err(), mode != "shadow");
            let approved = json!({"sources":["turn_state_unavailable"]});
            assert_eq!(confirm_dispatch(home.path(), "bot", "send_message", true, crate::egress_guard::Taint::unknown(), Some(&approved)).is_err(), mode == "block");
        }
    }

    #[test]
    fn malformed_policy_refuses_even_a_clean_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard='typo'").unwrap();
        assert!(evaluate(tmp.path(), "bot", "send_message", true, crate::egress_guard::Taint { tainted: false, sources: vec![] }).is_err());
        assert!(check_external(tmp.path(), "external", "send_message", true).is_err());
    }

    #[test]
    fn employee_api_keys_keep_narrowing_without_a_runtime_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard='off'").unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\negress_guard='block'").unwrap();
        assert!(check_external(tmp.path(), "bot", "docs_append", true).unwrap().is_none());
        assert!(check_unattributed_employee(tmp.path(), "bot", "docs_append", true).is_err());
    }

    #[tokio::test]
    async fn modes_narrow_and_unknown_turn_is_not_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        for (mode, needs_approval, refused) in [
            ("off", false, false),
            ("shadow", false, false),
            ("ask", true, false),
            ("block", false, true),
        ] {
            std::fs::write(
                tmp.path().join("config.toml"),
                format!("[security]\negress_guard='{mode}'\n"),
            )
            .unwrap();
            let result = check(tmp.path(), "bot", "send_message", true).await;
            assert_eq!(result.is_err(), refused, "{mode}");
            if let Ok(value) = result {
                assert_eq!(value.is_some(), needs_approval, "{mode}");
            }
            assert!(check(tmp.path(), "bot", "memory_search", false)
                .await
                .unwrap()
                .is_none());
        }
        std::fs::write(
            tmp.path().join("config.toml"),
            "[security]\negress_guard='off'\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[capabilities]\negress_guard='block'\n",
        )
        .unwrap();
        assert!(check(tmp.path(), "bot", "send_message", true)
            .await
            .is_err());
        let rows = std::fs::read_to_string(tmp.path().join("tool_calls.jsonl")).unwrap();
        let row: Value = serde_json::from_str(rows.lines().next().unwrap()).unwrap();
        assert_eq!(row["egress_guard"], "would_ask");
        assert!(!row["sources"].as_array().unwrap().is_empty());
    }
}
