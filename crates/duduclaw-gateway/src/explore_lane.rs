//! The read-only explore lane for dispatched work (2026-10-08): the one
//! task-local flag every spawn reads.
//!
//! The heartbeat proactive check has run in the lane since 2026-10-07
//! (`duduclaw-agent/src/heartbeat.rs`, its own spawn). Two sources put a
//! *dispatched* run in the lane, and the dispatcher folds both into the one
//! flag [`EXPLORE`] (`dispatcher::resolve_dispatch_lane`):
//!
//! - **External event wake-ups**: work woken by an MCP Events delivery
//!   ([`crate::mcp_events`]) carries `lane = "explore"` on its bus message
//!   (`message_queue.lane`; any other non-empty value is read as explore too,
//!   see [`is_explore`]) unless the operator opted the subscription into
//!   normal mode.
//! - **Read-only responsibilities**: a goal-loop round of a responsibility
//!   whose contract has `lane = "explore"` ([`crate::responsibility::lane`]).
//!
//! Before anything starts, the dispatcher refuses a run in the lane whose
//! employee cannot carry it ([`dispatch_refusal`]): runtimes other than
//! Claude, Codex and OpenAI-compatible, and task-sandboxed employees (the
//! sandbox has a shell). The message is failed; a refused goal round is returned
//! unrun. Inside the run, whichever source set the flag:
//!
//! - **Claude CLI** (`claude_runner::prepare_claude_cmd`, and
//!   `channel_reply/cli_env.rs`, used by the `ClaudeRuntime` choke-point a
//!   failover can reach): `DUDUCLAW_LANE=explore` (inherited by the DuDuClaw
//!   MCP server, `duduclaw mcp-proxy` and `duduclaw mcp-remote-bridge`:
//!   read-only tools only, see `duduclaw_core::ProcessLane`), `--tools` =
//!   `EXPLORE_LANE_BUILTIN_TOOLS` minus denied and `--allowedTools` =
//!   `mcp__duduclaw__*` + those built-ins, an explicit allowlist only
//!   narrowing ([`claude_tools`]); third-party stdio `.mcp.json` servers go
//!   through the gated proxy.
//! - **OpenAI-compatible runtime**: has no built-in tools and never starts
//!   `.mcp.json` servers; its tool loop (`claude_runner::build_mcp_tool_registry`)
//!   starts the DuDuClaw MCP server with the lane variable ([`lane_env`]) and,
//!   in the lane, mounts no `agent.toml [mcp.external]` server (those would
//!   run ungated).
//! - **Codex** (`runtime::codex`): `-s read-only` + `approval_policy=never`
//!   ([`codex_sandbox_args`]) whatever the employee's capability level (the
//!   lane only narrows; the OS sandbox ignores config allow lists and blocks
//!   writes and network), the DuDuClaw server registered with
//!   `DUDUCLAW_LANE=explore` in its `-c` env overrides, and no third-party
//!   server added by the gateway. Codex 0.156.1 rejects every MCP call under
//!   `-s read-only` (live-verified 2026-09-24), so no MCP tool, DuDuClaw or
//!   third-party, is callable in the lane; servers in the operator's own
//!   `~/.codex/config.toml` may still be started by Codex but cannot be used.
//! - **Refused inside the run** (fail closed, `explore_lane_unsupported`):
//!   Gemini CLI, Antigravity, Grok and generic CLI `execute`
//!   ([`refuse_unsupported_runtime`], reachable through failover); a MoA
//!   model and `inference_mode = "local"` in `claude_runner`. The hybrid
//!   local offload is skipped.

use duduclaw_core::types::{CapabilitiesConfig, RuntimeType};

tokio::task_local! {
    /// `true` while a dispatched run must stay in the explore lane. Absent
    /// scope ⇒ not in the lane (byte-identical spawns).
    pub static EXPLORE: bool;
}

/// Is the current task in the explore lane?
pub fn in_explore() -> bool {
    EXPLORE.try_with(|e| *e).unwrap_or(false)
}

/// Does a bus message's `lane` put the run in the explore lane? `None` ⇒
/// normal; `explore` ⇒ explore; any other value also ⇒ explore, the most
/// restrictive lane there is (an unknown value is never read as normal).
pub fn is_explore(raw: Option<&str>) -> bool {
    raw.is_some()
}

/// Whether an employee whose `[runtime] provider` is `provider` can run
/// work in the explore lane.
///
/// Claude (tool allowlist + gated MCP), Codex (`-s read-only` OS sandbox) and
/// OpenAI-compatible (no built-in tools). Antigravity, Gemini CLI, Grok and
/// generic CLIs have no mechanism this gateway can show to be read-only and
/// are refused.
pub fn runtime_supported(provider: RuntimeType) -> bool {
    matches!(
        provider,
        RuntimeType::Claude | RuntimeType::Codex | RuntimeType::OpenAiCompat
    )
}

/// Codex argument values inside the lane: always the OS-level read-only
/// sandbox with `approval_policy=never`, whatever the employee's own
/// capability level (the lane only narrows). Under `-s read-only` Codex
/// rejects every MCP tool call (live-verified on 0.156.1, see
/// `runtime::codex::sandbox_args`), so neither the DuDuClaw server nor any
/// server in the operator's Codex configuration is callable in the lane.
pub fn codex_sandbox_args() -> Vec<String> {
    vec![
        "-s".to_string(),
        "read-only".to_string(),
        "-c".to_string(),
        "approval_policy=never".to_string(),
    ]
}

/// Pre-dispatch check for a run of `agent_id` in the explore lane: the
/// reason it cannot run, or `None` when it can.
pub fn dispatch_refusal(home: &std::path::Path, agent_id: &str) -> Option<String> {
    let agent_dir = home.join("agents").join(agent_id);
    let settings = crate::runtime_config::load_runtime_settings(&agent_dir);
    if !runtime_supported(settings.provider) {
        return Some(format!(
            "explore_lane_unsupported: runtime {} cannot run read-only (explore lane) work; \
             switch the employee to Claude, Codex or an OpenAI-compatible runtime, or remove the lane",
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
            "explore_lane_unsupported: the task sandbox gives the employee a shell, so \
             read-only (explore lane) work cannot run there"
                .to_string(),
        );
    }
    None
}

/// `(DUDUCLAW_LANE, "explore")` inside the lane, for spawn and MCP-client
/// environments. `None` everywhere else (byte-identical env).
pub fn lane_env() -> Option<(String, String)> {
    in_explore().then(|| {
        (
            duduclaw_core::ENV_LANE.to_string(),
            duduclaw_core::LANE_EXPLORE.to_string(),
        )
    })
}

/// Fail-closed guard for runtimes that cannot carry the explore lane (they
/// would run with their full tool surface). `Err` inside the lane, `Ok`
/// otherwise.
pub fn refuse_unsupported_runtime(runtime: &str) -> Result<(), String> {
    if in_explore() {
        return Err(format!(
            "explore_lane_unsupported: the {runtime} runtime cannot run read-only \
             (explore lane) work"
        ));
    }
    Ok(())
}

/// Claude CLI argument values inside the lane: the built-in tool list
/// (`--tools`) and the auto-approve list (`--allowedTools`), both only ever
/// narrower than the employee's own. `None` outside the lane.
pub fn claude_tools(caps: &CapabilitiesConfig) -> Option<(Vec<String>, Vec<String>)> {
    if !in_explore() {
        return None;
    }
    let explore = duduclaw_core::tool_effect::EXPLORE_LANE_BUILTIN_TOOLS;
    let builtins: Vec<String> = caps
        .minimal_builtin_tools(explore)
        .into_iter()
        .filter(|t| explore.contains(&t.as_str()))
        .collect();
    let allowlist = caps.allowed_tools();
    let allowed: Vec<String> = if allowlist.is_empty() {
        std::iter::once("mcp__duduclaw__*".to_string())
            .chain(builtins.iter().cloned())
            .collect()
    } else {
        // An explicit allowlist can only narrow further: keep the entries
        // that are DuDuClaw MCP tools or read-only built-ins.
        // Never empty: an empty `--allowedTools` value would not mean
        // "nothing"; DuDuClaw MCP calls stay gated by the server itself
        // (lane + `allowed_tools`), so naming them here adds nothing.
        let kept: Vec<String> = allowlist
            .into_iter()
            .filter(|t| t.starts_with("mcp__duduclaw__") || explore.contains(&t.as_str()))
            .collect();
        if kept.is_empty() {
            vec!["mcp__duduclaw__*".to_string()]
        } else {
            kept
        }
    };
    Some((builtins, allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scope_and_lane_parsing() {
        assert!(!in_explore());
        assert!(EXPLORE.scope(true, async { in_explore() }).await);
        assert!(!is_explore(None));
        assert!(is_explore(Some("explore")));
        assert!(is_explore(Some("")));
        assert!(is_explore(Some("normal")));
    }

    #[test]
    fn only_claude_codex_and_openai_compat_carry_the_lane() {
        for r in [RuntimeType::Claude, RuntimeType::Codex, RuntimeType::OpenAiCompat] {
            assert!(runtime_supported(r), "{r:?}");
        }
        for r in [RuntimeType::Gemini, RuntimeType::Antigravity, RuntimeType::Grok] {
            assert!(!runtime_supported(r), "{r:?}");
        }
        let args = codex_sandbox_args();
        assert_eq!(args[..2], ["-s", "read-only"]);
        assert!(!args.iter().any(|a| a.contains("bypass") || a.contains("approve")));
    }

    #[test]
    fn sandboxed_or_gemini_employee_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("agents").join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.toml"), "[agent]\nname = \"a\"\n").unwrap();
        assert!(dispatch_refusal(dir.path(), "a").is_none());
        std::fs::write(
            a.join("agent.toml"),
            "[agent]\nname = \"a\"\n[container]\nsandbox_enabled = true\n",
        )
        .unwrap();
        assert!(dispatch_refusal(dir.path(), "a").is_some());
        std::fs::write(
            a.join("agent.toml"),
            "[agent]\nname = \"a\"\n[runtime]\nprovider = \"gemini\"\n",
        )
        .unwrap();
        assert!(dispatch_refusal(dir.path(), "a")
            .unwrap()
            .starts_with("explore_lane_unsupported"));
        std::fs::write(
            a.join("agent.toml"),
            "[agent]\nname = \"a\"\n[runtime]\nprovider = \"codex\"\n",
        )
        .unwrap();
        assert!(dispatch_refusal(dir.path(), "a").is_none(), "Codex carries the lane");
    }

    #[tokio::test]
    async fn lane_scope_narrows_claude_tools_and_refuses_other_runtimes() {
        let caps = CapabilitiesConfig::default();
        assert!(claude_tools(&caps).is_none());
        assert!(lane_env().is_none());
        assert!(refuse_unsupported_runtime("gemini").is_ok());
        EXPLORE
            .scope(true, async {
                let (tools, allowed) = claude_tools(&caps).unwrap();
                assert!(tools.iter().all(|t| {
                    duduclaw_core::tool_effect::EXPLORE_LANE_BUILTIN_TOOLS.contains(&t.as_str())
                }));
                assert!(!tools.iter().any(|t| t == "Bash" || t == "Write"));
                assert_eq!(allowed[0], "mcp__duduclaw__*");
                assert!(!allowed.iter().any(|t| t == "Bash"));
                assert_eq!(
                    lane_env(),
                    Some(("DUDUCLAW_LANE".to_string(), "explore".to_string()))
                );
                assert!(refuse_unsupported_runtime("gemini").is_err());
                // An explicit allowlist only narrows: Bash and other servers drop out.
                let mut narrow = CapabilitiesConfig::default();
                narrow.allowed_tools = vec!["Bash".into(), "Read".into(), "mcp__notion__*".into()];
                let (tools, allowed) = claude_tools(&narrow).unwrap();
                assert_eq!(tools, vec!["Read".to_string()]);
                assert_eq!(allowed, vec!["Read".to_string()]);
            })
            .await;
    }
}
