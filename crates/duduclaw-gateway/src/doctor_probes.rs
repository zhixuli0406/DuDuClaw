//! Shared doctor probes — single source of truth for BOTH surfaces:
//! the CLI `duduclaw doctor` (zh-TW verbose print) and the dashboard
//! `system.doctor` RPC (structured check cards). Each probe returns data,
//! never prints, so the two surfaces can't drift apart.
//!
//! Probe 1 (`mcp_cold_start_probe`): spawns `duduclaw mcp-server` exactly the
//! way a CLI runtime would (declared env block only: agent id +
//! `mcp_forward_env_vars`) and sends one JSON-RPC `initialize`. Detects the
//! "agent has no tools" class — the M6 fail-closed auth gate killing the MCP
//! server at boot when no `DUDUCLAW_MCP_API_KEY` reaches its env.
//!
//! Probe 2 (`grok_probe`): binary + version + live `grok -p "ping"` with the
//! runtime's own HOME/env stamping and auth-signature verdict. The CLI keeps
//! its richer evidence bundle (PTY one-shot retry) on top of this.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

// ── MCP server cold-start ───────────────────────────────────────

/// Outcome of spawning `duduclaw mcp-server` with a runtime-shaped env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpColdStartOutcome {
    /// Server answered the `initialize` request — tool surface available.
    Pass,
    /// Server died at the M6 fail-closed auth gate (missing/unknown key).
    AuthFailed,
    /// duduclaw binary did not resolve to an absolute path.
    BinaryUnresolved,
    /// Could not spawn the child at all.
    SpawnFailed(String),
    /// Child ran but exited without an `initialize` response.
    Abnormal {
        exit: Option<i32>,
        stderr_tail: String,
    },
    /// Still running after the cap with stdin closed — inconclusive.
    Timeout,
}

/// Structured result of [`mcp_cold_start_probe`].
#[derive(Debug, Clone)]
pub struct McpColdStartReport {
    /// Resolved duduclaw binary (absolute), when resolution succeeded.
    pub binary: Option<PathBuf>,
    /// Whether provisioning left a key in the forward env set.
    pub key_ready: bool,
    /// Error text when internal-key provisioning itself failed.
    pub provision_error: Option<String>,
    pub outcome: McpColdStartOutcome,
}

/// Run the same internal-key provisioning the gateway does at startup, then
/// spawn one `mcp-server` child and classify its cold-start behavior.
/// Idempotent and side-effect-light: provisioning reuses the existing
/// `gateway-internal` key (or mints it on a fresh home, exactly like a first
/// gateway boot would).
pub async fn mcp_cold_start_probe(home: &Path) -> McpColdStartReport {
    let bin = duduclaw_core::resolve_duduclaw_bin();
    if !bin.is_absolute() {
        return McpColdStartReport {
            binary: None,
            key_ready: false,
            provision_error: None,
            outcome: McpColdStartOutcome::BinaryUnresolved,
        };
    }

    let provision_error = match crate::mcp_internal_key::ensure_internal_mcp_key(home) {
        Ok(key) => {
            duduclaw_core::set_internal_mcp_api_key(key);
            None
        }
        Err(e) => Some(e),
    };

    let forward = duduclaw_core::mcp_forward_env_vars();
    let key_ready = forward
        .iter()
        .any(|(k, _)| k == duduclaw_core::ENV_MCP_API_KEY);

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("mcp-server")
        .env(duduclaw_core::ENV_AGENT_ID, "doctor-probe")
        .envs(forward.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return McpColdStartReport {
                binary: Some(bin),
                key_ready,
                provision_error,
                outcome: McpColdStartOutcome::SpawnFailed(e.to_string()),
            };
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let init = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","#,
            r#""capabilities":{},"clientInfo":{"name":"duduclaw-doctor","version":"0"}}}"#,
            "\n"
        );
        let _ = stdin.write_all(init.as_bytes()).await;
        // Drop stdin → EOF, so a healthy server answers then exits cleanly.
    }

    let outcome = match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await
    {
        Err(_) => McpColdStartOutcome::Timeout,
        Ok(Err(e)) => McpColdStartOutcome::SpawnFailed(format!("wait failed: {e}")),
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            classify_mcp_cold_start(out.status.code(), &stdout, &stderr)
        }
    };

    McpColdStartReport {
        binary: Some(bin),
        key_ready,
        provision_error,
        outcome,
    }
}

/// Pure classification of an mcp-server cold-start run (unit-testable).
fn classify_mcp_cold_start(
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> McpColdStartOutcome {
    if stdout.contains("\"result\"") && stdout.contains("\"id\":1") {
        return McpColdStartOutcome::Pass;
    }
    if stderr.contains("MCP authentication failed")
        || stderr.contains(duduclaw_core::ENV_MCP_API_KEY)
    {
        return McpColdStartOutcome::AuthFailed;
    }
    McpColdStartOutcome::Abnormal {
        exit,
        stderr_tail: duduclaw_core::truncate_bytes(stderr.trim(), 300).to_string(),
    }
}

// ── Grok CLI ────────────────────────────────────────────────────

/// Outcome of the live `grok -p "ping"` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokProbeOutcome {
    /// Non-empty stdout on exit 0 — headless path healthy.
    Ok { stdout_chars: usize },
    /// stderr matched a not-logged-in / expired-credential signature.
    AuthFailed { stderr_tail: String },
    /// Exit 0 but empty stdout — the headless-under-pipe class (the runtime
    /// applies a PTY one-shot retry for this; the CLI doctor demonstrates it).
    EmptyExit0,
    /// Non-zero exit without an auth signature.
    Failed {
        exit: Option<i32>,
        stderr_tail: String,
    },
    SpawnFailed(String),
    Timeout,
}

/// Structured result of [`grok_probe`].
#[derive(Debug, Clone)]
pub struct GrokProbeReport {
    pub path: String,
    pub version: Option<String>,
    pub outcome: GrokProbeOutcome,
}

/// Probe the grok CLI the way `GrokRuntime` drives it (same HOME/env
/// stamping, same auth-signature helper). Returns `None` when grok is not
/// installed — callers omit the check instead of reporting a failure.
pub async fn grok_probe(home: &Path) -> Option<GrokProbeReport> {
    let path = duduclaw_core::which_grok().or_else(|| duduclaw_core::which_grok_in_home(home))?;

    let version = match tokio::time::timeout(
        Duration::from_secs(5),
        duduclaw_core::platform::async_command_for(&path)
            .arg("--version")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    {
        Ok(Ok(out)) => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!v.is_empty()).then_some(v)
        }
        _ => None,
    };

    // Same HOME/env stamping as the runtime (launchd/Docker HOME fix).
    let user_home =
        crate::runtime::grok::resolve_user_home(home, std::env::var("HOME").ok().as_deref());
    let grok_home_override = std::env::var("GROK_HOME").ok();
    let home_env =
        crate::runtime::grok::build_home_env(&user_home, grok_home_override.as_deref());

    let mut cmd = duduclaw_core::platform::async_command_for(&path);
    cmd.args(["-p", "ping"]).stdin(Stdio::null());
    for (k, v) in &home_env {
        cmd.env(k, v);
    }

    let outcome = match tokio::time::timeout(Duration::from_secs(15), cmd.output()).await {
        Err(_) => GrokProbeOutcome::Timeout,
        Ok(Err(e)) => GrokProbeOutcome::SpawnFailed(e.to_string()),
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr_tail = duduclaw_core::truncate_bytes(stderr.trim(), 300).to_string();
            if crate::runtime::grok::looks_like_grok_auth_failure(&stderr) {
                GrokProbeOutcome::AuthFailed { stderr_tail }
            } else if out.status.success() {
                let chars = stdout.trim().chars().count();
                if chars == 0 {
                    GrokProbeOutcome::EmptyExit0
                } else {
                    GrokProbeOutcome::Ok {
                        stdout_chars: chars,
                    }
                }
            } else {
                GrokProbeOutcome::Failed {
                    exit: out.status.code(),
                    stderr_tail,
                }
            }
        }
    };

    Some(GrokProbeReport {
        path,
        version,
        outcome,
    })
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_pass_on_initialize_result() {
        let out = classify_mcp_cold_start(
            Some(0),
            r#"{"id":1,"jsonrpc":"2.0","result":{"capabilities":{}}}"#,
            "",
        );
        assert_eq!(out, McpColdStartOutcome::Pass);
    }

    #[test]
    fn classify_auth_failure_on_m6_message() {
        let out = classify_mcp_cold_start(
            Some(1),
            "",
            "Error: gateway error: MCP authentication failed: DUDUCLAW_MCP_API_KEY environment variable not set",
        );
        assert_eq!(out, McpColdStartOutcome::AuthFailed);
    }

    #[test]
    fn classify_abnormal_keeps_stderr_tail() {
        let out = classify_mcp_cold_start(Some(101), "", "thread 'main' panicked at ...");
        match out {
            McpColdStartOutcome::Abnormal { exit, stderr_tail } => {
                assert_eq!(exit, Some(101));
                assert!(stderr_tail.contains("panicked"));
            }
            other => panic!("expected Abnormal, got {other:?}"),
        }
    }
}
