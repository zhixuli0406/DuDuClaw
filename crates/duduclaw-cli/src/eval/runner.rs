//! Case execution — live agent runs and recorded-transcript replay.
//!
//! **Live** mirrors the gateway's harness invocation
//! (`channel_reply::build_claude_cli_args`, `pub(crate)` there): spawn the
//! `claude` CLI inside the agent directory with `--output-format stream-json`,
//! honouring the agent's `[capabilities]` allow/deny tool lists and per-agent
//! `.mcp.json`. The run uses the ambient `claude` credentials of whoever runs
//! `duduclaw eval` (no account rotation — evals are an operator/CI tool).
//!
//! **Replay** re-parses a previously recorded `*.transcript.jsonl` so the
//! deterministic assertion layer works offline in CI with zero credentials —
//! that is the regression half of the suite (`--record` refreshes baselines).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::case::EvalCaseFile;
use super::transcript::{parse_stream_json, EvalTranscript};

/// How to obtain the transcript for a case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunMode {
    /// Spawn the agent live; `record: true` writes the raw stream-json next
    /// to the case file for future `--replay` runs.
    Live { record: bool },
    /// Parse the recorded transcript instead of running the agent.
    Replay,
}

/// Resolve the replay/record transcript path for a case:
/// `[case] transcript` (validated relative at load time) or
/// `<case-file-stem>.transcript.jsonl` beside the case file.
pub fn transcript_path(case_path: &Path, case: &EvalCaseFile) -> PathBuf {
    let dir = case_path.parent().unwrap_or_else(|| Path::new("."));
    match &case.case.transcript {
        Some(rel) => dir.join(rel),
        None => {
            let stem = case_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("case");
            dir.join(format!("{stem}.transcript.jsonl"))
        }
    }
}

/// Obtain a parsed transcript per `mode`. Any failure (missing agent,
/// missing binary, timeout, in-band stream error) is an `Err` — the caller
/// records it as a failed case with the message as diagnostics.
pub async fn obtain_transcript(
    case_path: &Path,
    case: &EvalCaseFile,
    home: &Path,
    mode: RunMode,
) -> Result<EvalTranscript, String> {
    match mode {
        RunMode::Replay => {
            let path = transcript_path(case_path, case);
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "replay transcript missing: {} ({e}) — run once with --record to create it",
                    path.display()
                )
            })?;
            parse_stream_json(&raw)
        }
        RunMode::Live { record } => {
            let raw = run_live(case, home).await?;
            if record {
                let path = transcript_path(case_path, case);
                std::fs::write(&path, &raw)
                    .map_err(|e| format!("cannot record transcript {}: {e}", path.display()))?;
            }
            parse_stream_json(&raw)
        }
    }
}

/// Temp file that best-effort deletes itself (system-prompt hand-off; the
/// prompt is not a secret but shouldn't accumulate in tmp).
struct TempPromptFile(PathBuf);

impl TempPromptFile {
    fn create(content: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "duduclaw-eval-sys-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write system prompt temp file: {e}"))?;
        Ok(TempPromptFile(path))
    }
}

impl Drop for TempPromptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// WP2.1 side-effect fix: an agent directory copied into a sandbox home (or
/// provisioned by a template) may carry a `.mcp.json` whose duduclaw server
/// entry still points `DUDUCLAW_HOME` at the ORIGINAL (often production)
/// home — a live eval run would then write tasks/memory/audit into
/// production data even though the operator asked for an isolated run.
///
/// This rewrites the duduclaw entries (identified by an `mcp-server` arg) to
/// `DUDUCLAW_HOME = <eval home>` and returns the rewritten JSON. Non-duduclaw
/// servers (playwright, …) are untouched. `None` ⇒ nothing needed rewriting
/// (already correct, or no duduclaw entry) — caller passes the original file.
/// A parse failure also returns `None`: degrading to today's behavior beats
/// refusing to run over a hand-edited config.
pub fn rewrite_mcp_home(raw: &str, home: &Path) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let servers = v.get_mut("mcpServers")?.as_object_mut()?;
    let home_str = home.to_string_lossy().into_owned();
    let mut changed = false;
    for (_name, server) in servers.iter_mut() {
        let is_duduclaw = server
            .get("args")
            .and_then(|a| a.as_array())
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("mcp-server")));
        if !is_duduclaw {
            continue;
        }
        let obj = server.as_object_mut()?;
        let env = obj
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(Default::default()))
            .as_object_mut()?;
        if env.get("DUDUCLAW_HOME").and_then(|h| h.as_str()) != Some(home_str.as_str()) {
            env.insert(
                "DUDUCLAW_HOME".to_string(),
                serde_json::Value::String(home_str.clone()),
            );
            changed = true;
        }
        // The CLI's init event echoes the MCP server env into the recorded
        // stream — a production `DUDUCLAW_MCP_API_KEY` would land verbatim in
        // every shipped transcript (found the hard way: 24 recorded baselines
        // carried the real key before this existed). The stdio transport never
        // authenticates with it, so a placeholder is functionally identical.
        if env
            .get("DUDUCLAW_MCP_API_KEY")
            .and_then(|k| k.as_str())
            .is_some_and(|k| k != "eval-local")
        {
            env.insert(
                "DUDUCLAW_MCP_API_KEY".to_string(),
                serde_json::Value::String("eval-local".to_string()),
            );
            changed = true;
        }
    }
    if changed {
        serde_json::to_string_pretty(&v).ok()
    } else {
        None
    }
}

/// Temp `.mcp.json` guard (same lifetime discipline as [`TempPromptFile`]).
struct TempMcpFile(PathBuf);

impl TempMcpFile {
    fn create(content: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "duduclaw-eval-mcp-{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write rewritten mcp config: {e}"))?;
        Ok(TempMcpFile(path))
    }
}

impl Drop for TempMcpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Spawn `claude` for the case's agent and return raw stream-json stdout.
async fn run_live(case: &EvalCaseFile, home: &Path) -> Result<String, String> {
    let agent_dir = home.join("agents").join(&case.case.agent);
    if !agent_dir.join("agent.toml").exists() {
        return Err(format!(
            "agent '{}' not found under {} (live mode needs a provisioned agent; \
             use --replay for offline regression)",
            case.case.agent,
            agent_dir.display()
        ));
    }

    let claude = duduclaw_core::which_claude()
        .or_else(|| duduclaw_core::which_claude_in_home(home))
        .ok_or_else(|| "claude CLI not found (PATH + known install locations)".to_string())?;

    // Keep the guards alive for the whole child lifetime.
    let sys_file = match case.case.system_prompt.as_deref() {
        Some(sp) if !sp.trim().is_empty() => Some(TempPromptFile::create(sp)?),
        _ => None,
    };
    let mcp_original = agent_dir.join(".mcp.json");
    let mut mcp_temp: Option<TempMcpFile> = None;
    let mcp_path: Option<PathBuf> = if mcp_original.exists() {
        match std::fs::read_to_string(&mcp_original) {
            Ok(raw) => match rewrite_mcp_home(&raw, home) {
                Some(rewritten) => {
                    let guard = TempMcpFile::create(&rewritten)?;
                    let p = guard.0.clone();
                    mcp_temp = Some(guard);
                    Some(p)
                }
                None => Some(mcp_original.clone()),
            },
            Err(_) => Some(mcp_original.clone()),
        }
    } else {
        None
    };

    let capabilities = duduclaw_gateway::runtime::load_agent_capabilities(&agent_dir);
    let args = build_eval_cli_args(
        case,
        capabilities.as_ref(),
        mcp_path.as_deref(),
        sys_file.as_ref().map(|f| f.0.as_path()),
        duduclaw_core::agent_toml::resolve_minimal_context(Some(&agent_dir)),
    );
    let _keep_alive = &mcp_temp;

    let mut cmd = tokio::process::Command::new(&claude);
    cmd.args(&args)
        .current_dir(&agent_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {claude}: {e}"))?;

    let timeout = std::time::Duration::from_secs(case.case.timeout_secs);
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "live run timed out after {}s (case timeout_secs)",
                case.case.timeout_secs
            )
        })?
        .map_err(|e| format!("claude CLI wait failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() && stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "claude CLI exited with {} and no stream output; stderr_tail={:?}",
            output.status,
            duduclaw_core::truncate_bytes(stderr.trim(), 400)
        ));
    }
    Ok(stdout)
}

/// Argument layout mirrors the gateway's `build_claude_cli_args` (harness
/// parity: same permission mode, tool allow/deny wiring, strict per-agent
/// MCP config, system prompt via file). No `--resume`: eval cases are
/// intentionally single-shot and session-free for reproducibility.
fn build_eval_cli_args(
    case: &EvalCaseFile,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    mcp_config: Option<&Path>,
    system_prompt_file: Option<&Path>,
    // WP-7A: mirror the harness's minimal-context flags so eval stays
    // representative of production spawns. Resolved from the eval agent dir.
    minimal_context: bool,
) -> Vec<String> {
    // WP-7A (bug1): `--exclude-dynamic-system-prompt-sections` removed — a
    // documented no-op when combined with `--system-prompt-file` (which eval
    // always sets for a case with a system prompt).
    let mut args: Vec<String> = vec![
        "-p".into(),
        case.case.prompt.clone(),
        "--model".into(),
        case.model().to_string(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--dangerously-skip-permissions".into(),
        "--max-turns".into(),
        case.case.max_turns.to_string(),
    ];

    let caps = capabilities.cloned().unwrap_or_default();
    let allowed = caps.allowed_tools();
    if !allowed.is_empty() {
        args.push("--allowedTools".into());
        args.push(allowed.join(","));
    }
    let denied = caps.disallowed_tools();
    if !denied.is_empty() {
        args.push("--disallowedTools".into());
        args.push(denied.join(","));
    }

    // WP-7A minimal-context (harness parity): keep `project,local` so the
    // agent's own `.claude/settings.json` hook survives.
    if minimal_context {
        args.push("--setting-sources".into());
        args.push("project,local".into());
        args.push("--tools".into());
        args.push(
            caps.minimal_builtin_tools(&duduclaw_core::types::CURATED_BUILTIN_TOOLS)
                .join(","),
        );
    }

    if let Some(mcp_json) = mcp_config {
        args.push("--mcp-config".into());
        args.push(mcp_json.to_string_lossy().into_owned());
        args.push("--strict-mcp-config".into());
    }

    if let Some(f) = system_prompt_file {
        args.push("--system-prompt-file".into());
        args.push(f.to_string_lossy().into_owned());
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(extra: &str) -> EvalCaseFile {
        let toml = format!(
            "[case]\nname = \"t\"\nagent = \"support-bot\"\nprompt = \"hi there\"\n{extra}[judge]\nrubric = \"r\"\n"
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn transcript_path_defaults_to_case_stem() {
        let c = case("");
        let p = transcript_path(Path::new("/suite/refund-flow.toml"), &c);
        assert_eq!(p, PathBuf::from("/suite/refund-flow.transcript.jsonl"));
    }

    #[test]
    fn transcript_path_honours_explicit_relative() {
        let c = case("transcript = \"recorded/run1.jsonl\"\n");
        let p = transcript_path(Path::new("/suite/refund-flow.toml"), &c);
        assert_eq!(p, PathBuf::from("/suite/recorded/run1.jsonl"));
    }

    #[test]
    fn cli_args_mirror_harness_invocation() {
        let c = case("model = \"claude-haiku-4-5\"\nmax_turns = 7\n");
        // minimal_context = false → no minimal flags, no dead exclude-dynamic.
        let args = build_eval_cli_args(&c, None, None, None, false);
        let joined = args.join(" ");
        assert!(joined.contains("-p hi there"));
        assert!(joined.contains("--model claude-haiku-4-5"));
        assert!(joined.contains("--output-format stream-json"));
        assert!(joined.contains("--max-turns 7"));
        assert!(joined.contains("--dangerously-skip-permissions"));
        assert!(!joined.contains("--mcp-config"));
        assert!(!joined.contains("--system-prompt-file"));
        // WP-7A: the dead flag is gone; minimal flags only appear when enabled.
        assert!(!joined.contains("--exclude-dynamic-system-prompt-sections"));
        assert!(!joined.contains("--setting-sources"));
        assert!(!joined.contains("--tools"));
    }

    #[test]
    fn cli_args_minimal_context_adds_setting_sources_and_tools() {
        let c = case("");
        let args = build_eval_cli_args(&c, None, None, None, true);
        let joined = args.join(" ");
        assert!(joined.contains("--setting-sources project,local"));
        // Curated built-in set, no operator globals dropped via "".
        assert!(joined.contains("--tools "));
        assert!(joined.contains("TodoWrite"));
        assert!(!joined.contains("--exclude-dynamic-system-prompt-sections"));
    }

    #[test]
    fn cli_args_wire_capabilities_and_mcp_and_sysprompt() {
        let c = case("");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "{}").unwrap();
        let caps = duduclaw_core::types::CapabilitiesConfig {
            allowed_tools: vec!["Read".into(), "mcp__duduclaw__tasks_create".into()],
            denied_tools: vec!["Bash".into()],
            ..Default::default()
        };
        let sys = dir.path().join("sys.txt");
        let mcp = dir.path().join(".mcp.json");
        let args = build_eval_cli_args(&c, Some(&caps), Some(&mcp), Some(&sys), false);
        let joined = args.join(" ");
        assert!(joined.contains("--allowedTools"));
        assert!(joined.contains("--disallowedTools Bash"));
        assert!(joined.contains("--mcp-config"));
        assert!(joined.contains("--strict-mcp-config"));
        assert!(joined.contains("--system-prompt-file"));
    }

    // ── WP2.1 side-effect fix: DUDUCLAW_HOME rewrite ─────────────────

    #[test]
    fn rewrite_points_duduclaw_home_at_the_eval_home() {
        let raw = r#"{"mcpServers":{"duduclaw":{"command":"/usr/local/bin/duduclaw","args":["mcp-server"],"env":{"DUDUCLAW_HOME":"/Users/prod/.duduclaw","DUDUCLAW_AGENT_ID":"law-intake","DUDUCLAW_MCP_API_KEY":"ddc_prod_deadbeef"}},"playwright":{"command":"npx","args":["@anthropic-ai/mcp-server-playwright"]}}}"#;
        let out = rewrite_mcp_home(raw, Path::new("/tmp/sandbox-home")).expect("must rewrite");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_HOME"],
            "/tmp/sandbox-home",
            "duduclaw server must point at the eval home"
        );
        // The production MCP key never reaches the recorded stream.
        assert_eq!(v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_MCP_API_KEY"], "eval-local");
        // Sibling env vars and non-duduclaw servers are untouched.
        assert_eq!(v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"], "law-intake");
        assert!(v["mcpServers"]["playwright"].get("env").is_none());
    }

    #[test]
    fn rewrite_inserts_home_when_env_was_absent() {
        let raw = r#"{"mcpServers":{"duduclaw":{"command":"duduclaw","args":["mcp-server"]}}}"#;
        let out = rewrite_mcp_home(raw, Path::new("/tmp/h")).expect("must rewrite");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_HOME"], "/tmp/h");
    }

    #[test]
    fn rewrite_is_a_noop_when_already_correct_or_unparseable() {
        let correct = r#"{"mcpServers":{"duduclaw":{"command":"duduclaw","args":["mcp-server"],"env":{"DUDUCLAW_HOME":"/tmp/h"}}}}"#;
        assert!(rewrite_mcp_home(correct, Path::new("/tmp/h")).is_none());
        assert!(rewrite_mcp_home("not json", Path::new("/tmp/h")).is_none());
        // No duduclaw entry ⇒ nothing to do.
        let other = r#"{"mcpServers":{"playwright":{"command":"npx","args":["x"]}}}"#;
        assert!(rewrite_mcp_home(other, Path::new("/tmp/h")).is_none());
    }

    #[tokio::test]
    async fn replay_reads_and_parses_recorded_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let case_path = dir.path().join("t.toml");
        let c = case("");
        std::fs::write(
            dir.path().join("t.transcript.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
        )
        .unwrap();
        let t = obtain_transcript(&case_path, &c, dir.path(), RunMode::Replay)
            .await
            .unwrap();
        assert_eq!(t.final_text, "hi");
    }

    #[tokio::test]
    async fn replay_missing_transcript_is_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let case_path = dir.path().join("t.toml");
        let c = case("");
        let err = obtain_transcript(&case_path, &c, dir.path(), RunMode::Replay)
            .await
            .unwrap_err();
        assert!(err.contains("--record"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn live_without_agent_dir_is_actionable_error() {
        let dir = tempfile::tempdir().unwrap(); // empty home: no agents/
        let case_path = dir.path().join("t.toml");
        let c = case("");
        let err = obtain_transcript(&case_path, &c, dir.path(), RunMode::Live { record: false })
            .await
            .unwrap_err();
        assert!(err.contains("not found"), "unexpected: {err}");
        assert!(err.contains("--replay"), "unexpected: {err}");
    }
}
