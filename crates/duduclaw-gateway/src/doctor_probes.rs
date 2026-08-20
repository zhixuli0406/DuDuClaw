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

/// Render one [`McpColdStartOutcome`] into a `(status, message)` pair —
/// `status` is one of `"pass"`/`"fail"`/`"warn"`, matching the doctor check
/// card vocabulary. `provision_error` is [`McpColdStartReport::provision_error`],
/// only consulted on `AuthFailed`. Single source of truth for BOTH
/// `run_doctor_checks` (`duduclaw-gateway::handlers`, the dashboard
/// `system.doctor`/`system.doctor_repair` RPCs) and the O-0 `os_doctor_repair`
/// MCP tool (`duduclaw-cli::mcp`) so their zh-TW copy can never drift apart.
pub fn mcp_cold_start_status_and_message(
    outcome: &McpColdStartOutcome,
    provision_error: Option<&str>,
) -> (&'static str, String) {
    match outcome {
        McpColdStartOutcome::Pass => {
            ("pass", "MCP server 啟動並回應 initialize，工具面可用。".to_string())
        }
        McpColdStartOutcome::AuthFailed => (
            "fail",
            format!(
                "MCP server 因認證被拒而終止 — agent 會完全叫不到 duduclaw 工具。{}重啟 gateway 讓它自動配發 internal key，或設定 env DUDUCLAW_MCP_API_KEY。",
                provision_error
                    .map(|e| format!("（internal key 配發失敗：{e}）"))
                    .unwrap_or_default()
            ),
        ),
        McpColdStartOutcome::BinaryUnresolved => (
            "fail",
            "duduclaw binary 無法解析為絕對路徑 — CLI runtime 無法註冊 MCP server。".to_string(),
        ),
        McpColdStartOutcome::SpawnFailed(e) => ("fail", format!("mcp-server 無法啟動：{e}")),
        McpColdStartOutcome::Timeout => (
            "warn",
            "mcp-server 10 秒內未結束（stdin 已關閉），無法確認 initialize 是否成功。".to_string(),
        ),
        McpColdStartOutcome::Abnormal { exit, stderr_tail } => (
            "fail",
            format!(
                "mcp-server 異常結束（exit={}，無 initialize 回應）。stderr：{}",
                exit.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                if stderr_tail.is_empty() { "(空)" } else { stderr_tail }
            ),
        ),
    }
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

    // The probe claims `doctor-probe` and signs it with the install's
    // `identity.key` (WP21 debt ⑧) when one exists, so that flipping
    // `[delegation] require_identity_token = true` does not turn a healthy
    // MCP server into a red doctor result. Signed ≠ authorized: `doctor-probe`
    // is no agent's ancestor and no system sender, so it still cannot delegate.
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("mcp-server")
        .envs(duduclaw_core::agent_identity_env_vars(home, "doctor-probe"))
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

// ── Local auto-login exposure (G8 residual-risk finding) ──────────
//
// Personal edition's passwordless local-login switch (`[dashboard]`/
// `[gateway] local_auto_login`, WP-F1, see `local_session.rs`) is only safe
// because its own per-request gate independently re-checks the TCP peer
// address (`ConnectInfo`, never a header) and refuses outright whenever a
// proxy-class header is present. That defence has one blind spot by design:
// a *bare* reverse proxy in front of the gateway (nginx/Caddy configured
// with zero `X-Forwarded-*` forwarding) makes every remote peer look like
// `127.0.0.1` to `ConnectInfo` without ever sending a proxy header — so
// condition 3 (loopback peer) is defeated without ever tripping condition 4
// (proxy header present). This probe is a second, static line of defence:
// a config combination doctor can flag before any request ever arrives —
// the switch left on while `[gateway] bind` is not itself loopback. Pure
// function (no I/O beyond the two existing config reads it delegates to),
// unit-testable, shared by both the CLI `duduclaw doctor` printout and the
// dashboard `system.doctor` RPC per this module's stated single-source
// convention.

/// Returns `Some(zh-TW warning message)` when local auto-login is enabled
/// AND the gateway bind is not loopback; `None` when either half of that is
/// false (nothing to warn about).
pub fn local_auto_login_exposure(home: &Path) -> Option<String> {
    if !crate::local_session::auto_login_enabled(home) {
        return None;
    }
    let (bind, _) = duduclaw_core::gateway_bind_for_home(home);
    if bind_is_loopback(&bind) {
        return None;
    }
    Some(format!(
        "你開了本機自動登入（local_auto_login），但服務綁在非本機位址「{bind}」—— \
         任何能連到這台機器的人都可能免密碼以 admin 身分進入。請確認前面沒有會覆寫來源位址的\
         反向代理（例如裸 nginx／Caddy 未轉發 X-Forwarded-For 等標頭），否則就關閉自動登入\
         （`config.toml [dashboard] local_auto_login = false`）改用密碼登入。"
    ))
}

/// Loopback-ish bind strings doctor treats as safe. Mirrors the ad-hoc check
/// `duduclaw run` already prints (`bind == "127.0.0.1" || "::1" ||
/// "localhost"`), generalized via `IpAddr::is_loopback()` so any literal
/// loopback address (e.g. `127.0.0.5`) also counts, not just the canonical
/// one.
fn bind_is_loopback(bind: &str) -> bool {
    if bind.eq_ignore_ascii_case("localhost") {
        return true;
    }
    bind.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

// ── Credentials (WP-H1 P1) ──────────────────────────────────────
//
// The design doc's §3 P1 line: "`duduclaw doctor` 加憑證體檢（列出每個 ref 的
// `describe()`,標出 unset / 不可解 / 明文殘留）". Same
// `security_posture::credential_inventory` the dashboard RPC reads, so the CLI
// and the dashboard can never disagree about what is configured — this
// module's stated single-source convention.

/// What `doctor` needs to say about this deployment's credentials.
///
/// Counts and paths only. `describe()` does not resolve, so building this
/// costs no backend round-trip and cannot leak a value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialAudit {
    /// Total credential fields found.
    pub total: usize,
    /// Fields with a source configured.
    pub configured: usize,
    /// Fields sourced from an external `secret://` reference.
    pub referenced: usize,
    /// Fields still holding bare plaintext (no `_enc`, no reference).
    pub plaintext_paths: Vec<String>,
    /// Fields with a plaintext twin sitting next to a `_enc` twin (§1.5).
    pub residue_paths: Vec<String>,
    /// Fields whose reference cannot be parsed at all — the config names a
    /// source that does not exist, so the credential is silently unusable.
    pub broken_reference_paths: Vec<String>,
}

impl CredentialAudit {
    /// Nothing an operator needs to act on.
    pub fn is_clean(&self) -> bool {
        self.plaintext_paths.is_empty()
            && self.residue_paths.is_empty()
            && self.broken_reference_paths.is_empty()
    }

    /// One-line zh-TW summary for the `doctor` table.
    pub fn summary(&self) -> String {
        if self.total == 0 {
            return "設定檔中沒有任何憑證欄位。".to_string();
        }
        if self.is_clean() {
            return format!(
                "{} 個憑證欄位（{} 個已設定,{} 個走 secret:// 外部來源）,沒有明文或殘留。",
                self.total, self.configured, self.referenced
            );
        }
        let mut parts = Vec::new();
        if !self.residue_paths.is_empty() {
            parts.push(format!(
                "{} 個明文殘留（已有加密孿生,可用 `duduclaw doctor --fix-residue` 清掉）:{}",
                self.residue_paths.len(),
                self.residue_paths.join(", ")
            ));
        }
        if !self.plaintext_paths.is_empty() {
            parts.push(format!(
                "{} 個仍是明文（請在儀表板重存以加密,或改成 secret:// 參照）:{}",
                self.plaintext_paths.len(),
                self.plaintext_paths.join(", ")
            ));
        }
        if !self.broken_reference_paths.is_empty() {
            parts.push(format!(
                "{} 個 secret:// 參照無法解析,這些憑證等於沒設定:{}",
                self.broken_reference_paths.len(),
                self.broken_reference_paths.join(", ")
            ));
        }
        parts.join("；")
    }
}

/// Audit a parsed `config.toml`. Split from [`credential_audit`] so the
/// classification rules are testable without touching a filesystem.
pub fn credential_audit_of(table: &toml::Table) -> CredentialAudit {
    use duduclaw_security::secret_ref::SourceKind;

    let entries = crate::security_posture::credential_inventory(table);
    let mut audit = CredentialAudit {
        total: entries.len(),
        ..Default::default()
    };
    for e in entries {
        if e.configured {
            audit.configured += 1;
        }
        match e.source {
            SourceKind::Legacy => audit.plaintext_paths.push(e.path.clone()),
            SourceKind::Env
            | SourceKind::Keychain
            | SourceKind::File
            | SourceKind::Vault
            | SourceKind::OnePassword
            | SourceKind::Infisical
            | SourceKind::Local => audit.referenced += 1,
            // `describe()` reports an unparseable reference as `Unset` with the
            // label `unset(invalid-reference)` — the one case where "unset" has
            // a cause worth naming, because the operator believes it is set.
            SourceKind::Unset if e.source_label == "unset(invalid-reference)" => {
                audit.broken_reference_paths.push(e.path.clone())
            }
            SourceKind::Unset | SourceKind::Inline | SourceKind::Ambiguous => {}
        }
        if e.residue {
            audit.residue_paths.push(e.path);
        }
    }
    audit
}

/// Audit the live home's `config.toml`.
///
/// An unreadable or unparsable file yields an empty audit rather than an error:
/// `doctor` already has a dedicated check for "config.toml is missing/broken",
/// and this probe must not double-report it.
pub fn credential_audit(home: &Path) -> CredentialAudit {
    std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|raw| raw.parse::<toml::Table>().ok())
        .map(|t| credential_audit_of(&t))
        .unwrap_or_default()
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

    // ── local_auto_login_exposure ──────────────────────────────

    #[test]
    fn bind_is_loopback_recognizes_all_accepted_forms() {
        assert!(bind_is_loopback("127.0.0.1"));
        assert!(bind_is_loopback("127.0.0.5"));
        assert!(bind_is_loopback("::1"));
        assert!(bind_is_loopback("localhost"));
        assert!(bind_is_loopback("LOCALHOST"));
        assert!(!bind_is_loopback("0.0.0.0"));
        assert!(!bind_is_loopback("192.168.1.10"));
        assert!(!bind_is_loopback("::"));
        // Unparseable strings fail closed to "not loopback" — never silently
        // treated as safe.
        assert!(!bind_is_loopback("not-an-ip"));
    }

    #[test]
    fn warns_when_auto_login_on_and_bind_is_remote() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = true\n\n[gateway]\nbind = \"0.0.0.0\"\n",
        )
        .unwrap();
        let msg = local_auto_login_exposure(dir.path()).expect("should warn");
        assert!(msg.contains("0.0.0.0"), "{msg}");
        assert!(msg.contains("local_auto_login"), "{msg}");
    }

    #[test]
    fn silent_when_bind_is_loopback_even_with_auto_login_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = true\n\n[gateway]\nbind = \"127.0.0.1\"\n",
        )
        .unwrap();
        assert_eq!(local_auto_login_exposure(dir.path()), None);
    }

    #[test]
    fn silent_when_auto_login_is_off_even_on_a_remote_bind() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = false\n\n[gateway]\nbind = \"0.0.0.0\"\n",
        )
        .unwrap();
        assert_eq!(local_auto_login_exposure(dir.path()), None);
    }

    #[test]
    fn default_bind_is_loopback_so_a_fresh_install_with_no_config_is_silent() {
        // No config.toml at all: auto_login_enabled defaults to true, but
        // gateway_bind_for_home also defaults to loopback — nothing to warn
        // about on a brand-new install.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(local_auto_login_exposure(dir.path()), None);
    }

    // ── WP-H1 P1: credential audit ────────────────────────────────────

    fn audit(toml_str: &str) -> CredentialAudit {
        credential_audit_of(&toml_str.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn a_fully_encrypted_config_is_clean() {
        let a = audit(
            r#"
            [channels]
            telegram_bot_token_enc = "Y2lwaGVy"
            discord_bot_token = "secret://env/DISCORD_TOKEN"
            "#,
        );
        assert!(a.is_clean(), "{a:?}");
        assert_eq!(a.total, 2);
        assert_eq!(a.configured, 2);
        assert_eq!(a.referenced, 1);
        assert!(a.summary().contains("沒有明文或殘留"));
    }

    #[test]
    fn plaintext_residue_and_broken_references_are_reported_separately() {
        let a = audit(
            r#"
            [channels]
            slack_bot_token = "xoxb-bare-plaintext"
            [[accounts]]
            oauth_token = "leftover"
            oauth_token_enc = "Y2lwaGVy"
            [voice]
            stt_api_key = "secret://nosuchbackend/whatever"
            "#,
        );
        assert_eq!(a.plaintext_paths, vec!["channels.slack_bot_token"]);
        assert_eq!(a.residue_paths, vec!["accounts[0].oauth_token"]);
        assert_eq!(a.broken_reference_paths, vec!["voice.stt_api_key"]);
        assert!(!a.is_clean());
        // The summary names paths, never values.
        let s = a.summary();
        assert!(!s.contains("xoxb-bare-plaintext"), "{s}");
        assert!(!s.contains("leftover"), "{s}");
        assert!(!s.contains("Y2lwaGVy"), "{s}");
    }

    #[test]
    fn an_unreadable_home_yields_an_empty_audit_not_a_false_alarm() {
        let a = credential_audit(std::path::Path::new("/nonexistent-duduclaw-home"));
        assert_eq!(a, CredentialAudit::default());
        assert!(a.is_clean());
        assert!(a.summary().contains("沒有任何憑證欄位"));
    }
}
