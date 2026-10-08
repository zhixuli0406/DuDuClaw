//! P5 (2026-10-08) — opt-in read-only lane for a responsibility's runs.
//!
//! A responsibility created with `lane = "explore"` runs every occurrence
//! round in the read-only explore lane that #69 introduced for the heartbeat
//! proactive check: `DUDUCLAW_LANE=explore` reaches the DuDuClaw MCP server
//! (only `read` / `draft` tools listed and callable) and the Claude CLI's
//! built-in tools are narrowed to `EXPLORE_LANE_BUILTIN_TOOLS`.
//!
//! The lane is stored inside the contract's `scope_json`
//! (`{"event_names":[…],"lane":"explore"}`), so it is part of the contract
//! hash, needs no schema change, and a responsibility without it keeps a
//! byte-identical `scope_json`.
//!
//! Which runtimes carry it ([`lane_runtime_supported`]):
//! * Claude CLI (dispatch path and the `ClaudeRuntime` choke-point): env +
//!   `--tools` + an auto-approve list limited to DuDuClaw MCP tools and the
//!   read-only built-ins.
//! * OpenAI-compatible runtime and the local-inference tool loop: no built-in
//!   tools; the MCP child gets the lane variable (`mcp_client_envs`).
//! * Codex, Gemini CLI, Antigravity, Grok and generic CLI runtimes cannot be
//!   restricted the same way, so an explore-lane round is refused before
//!   dispatch (and again inside each of those runtimes' `execute`, which a
//!   failover could reach). A task-sandboxed employee is refused too (the
//!   sandbox has a shell). Fail closed: never run unrestricted.

use serde_json::Value;

use crate::task_store::ResponsibilityRow;

/// The only lane value a responsibility may carry.
pub const LANE_EXPLORE: &str = duduclaw_core::LANE_EXPLORE;

/// Lane of a responsibility, read from its stored `scope_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespLane {
    /// No lane restriction (the default).
    Normal,
    /// Read-only explore lane.
    Explore,
}

/// Validate a lane given at create / update time. `None` and `"normal"` are
/// the default; `"explore"` is the read-only lane; anything else is refused.
pub fn parse_input_lane(lane: Option<&str>) -> Result<RespLane, String> {
    match lane.map(str::trim) {
        None | Some("") | Some("normal") => Ok(RespLane::Normal),
        Some(LANE_EXPLORE) => Ok(RespLane::Explore),
        Some(other) => Err(format!(
            "lane must be \"explore\" or absent (got {:?})",
            duduclaw_core::truncate_chars(other, 40)
        )),
    }
}

/// Lane recorded in a stored `scope_json`. An unparseable value, or a lane
/// key with any value other than `"explore"`, is an error: the caller must
/// not run the round (fail closed).
pub fn lane_of_scope(scope_json: &str) -> Result<RespLane, String> {
    let v: Value = serde_json::from_str(scope_json)
        .map_err(|e| format!("responsibility scope is unreadable: {e}"))?;
    match v.get("lane") {
        None | Some(Value::Null) => Ok(RespLane::Normal),
        Some(Value::String(s)) if s == LANE_EXPLORE => Ok(RespLane::Explore),
        Some(other) => Err(format!("responsibility lane {other} is not recognised")),
    }
}

pub fn lane_of(row: &ResponsibilityRow) -> Result<RespLane, String> {
    lane_of_scope(&row.scope_json)
}

/// The `event_names` part of a stored scope (what decides whether the
/// subscriptions changed; the lane is not a subscription).
pub fn scope_event_names(scope_json: &str) -> Value {
    serde_json::from_str::<Value>(scope_json)
        .ok()
        .and_then(|v| v.get("event_names").cloned())
        .unwrap_or(Value::Null)
}

/// Whether an employee whose `[runtime] provider` is `provider` can run a
/// round in the explore lane.
pub fn lane_runtime_supported(provider: duduclaw_core::types::RuntimeType) -> bool {
    use duduclaw_core::types::RuntimeType;
    matches!(provider, RuntimeType::Claude | RuntimeType::OpenAiCompat)
}

/// Pre-dispatch check for an explore-lane round of `agent_id`: the reason it
/// cannot run, or `None` when it can.
pub fn explore_round_refusal(home: &std::path::Path, agent_id: &str) -> Option<String> {
    let agent_dir = home.join("agents").join(agent_id);
    let settings = crate::runtime_config::load_runtime_settings(&agent_dir);
    if !lane_runtime_supported(settings.provider) {
        return Some(format!(
            "explore_lane_unsupported: runtime {} cannot run a read-only (explore lane) \
             responsibility round; switch the employee to Claude or an OpenAI-compatible \
             runtime, or remove the lane",
            settings.provider.as_str()
        ));
    }
    let sandboxed = std::fs::read_to_string(agent_dir.join("agent.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("container")
                .and_then(|c| c.get("sandbox_enabled"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false);
    if sandboxed {
        return Some(
            "explore_lane_unsupported: the task sandbox gives the employee a shell, so a \
             read-only (explore lane) round cannot run there"
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    #[test]
    fn input_lane_values() {
        assert_eq!(parse_input_lane(None), Ok(RespLane::Normal));
        assert_eq!(parse_input_lane(Some("normal")), Ok(RespLane::Normal));
        assert_eq!(parse_input_lane(Some("explore")), Ok(RespLane::Explore));
        assert!(parse_input_lane(Some("Explore")).is_err());
        assert!(parse_input_lane(Some("write")).is_err());
    }

    #[test]
    fn stored_lane_fails_closed() {
        assert_eq!(lane_of_scope(r#"{"event_names":[]}"#), Ok(RespLane::Normal));
        assert_eq!(
            lane_of_scope(r#"{"event_names":[],"lane":"explore"}"#),
            Ok(RespLane::Explore)
        );
        assert!(lane_of_scope(r#"{"event_names":[],"lane":"other"}"#).is_err());
        assert!(lane_of_scope(r#"{"event_names":[],"lane":1}"#).is_err());
        assert!(lane_of_scope("not json").is_err());
    }

    #[test]
    fn only_claude_and_openai_compat_carry_the_lane() {
        use duduclaw_core::types::RuntimeType;
        assert!(lane_runtime_supported(RuntimeType::Claude));
        assert!(lane_runtime_supported(RuntimeType::OpenAiCompat));
        for r in [RuntimeType::Codex, RuntimeType::Gemini] {
            assert!(!lane_runtime_supported(r));
        }
    }

    #[test]
    fn sandboxed_or_codex_employee_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("agents").join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.toml"), "[agent]\nname = \"a\"\n").unwrap();
        assert!(explore_round_refusal(dir.path(), "a").is_none());
        std::fs::write(
            a.join("agent.toml"),
            "[agent]\nname = \"a\"\n[container]\nsandbox_enabled = true\n",
        )
        .unwrap();
        assert!(explore_round_refusal(dir.path(), "a").is_some());
        std::fs::write(
            a.join("agent.toml"),
            "[agent]\nname = \"a\"\n[runtime]\nprovider = \"codex\"\n",
        )
        .unwrap();
        assert!(explore_round_refusal(dir.path(), "a").is_some());
    }

    #[tokio::test]
    async fn lane_scope_narrows_claude_tools_and_refuses_other_runtimes() {
        use duduclaw_core::types::CapabilitiesConfig;
        let caps = CapabilitiesConfig::default();
        assert!(crate::runtime::explore_lane_claude_tools(&caps).is_none());
        assert!(crate::runtime::round_lane_env().is_none());
        assert!(crate::runtime::refuse_unsupported_lane("codex").is_ok());
        crate::runtime::EXPLORE_ROUND_LANE
            .scope(true, async {
                let (tools, allowed) = crate::runtime::explore_lane_claude_tools(&caps).unwrap();
                assert!(tools.iter().all(|t| {
                    duduclaw_core::tool_effect::EXPLORE_LANE_BUILTIN_TOOLS.contains(&t.as_str())
                }));
                assert!(!tools.iter().any(|t| t == "Bash" || t == "Write"));
                assert_eq!(allowed[0], "mcp__duduclaw__*");
                assert!(!allowed.iter().any(|t| t == "Bash"));
                assert_eq!(
                    crate::runtime::round_lane_env(),
                    Some(("DUDUCLAW_LANE".to_string(), "explore".to_string()))
                );
                assert!(crate::runtime::refuse_unsupported_lane("codex").is_err());
                // An explicit allowlist only narrows: Bash and other servers drop out.
                let mut narrow = CapabilitiesConfig::default();
                narrow.allowed_tools = vec!["Bash".into(), "Read".into(), "mcp__notion__*".into()];
                let (tools, allowed) = crate::runtime::explore_lane_claude_tools(&narrow).unwrap();
                assert_eq!(tools, vec!["Read".to_string()]);
                assert_eq!(allowed, vec!["Read".to_string()]);
            })
            .await;
    }
}
