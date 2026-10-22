//! `experts.install` (the dashboard's「上傳安裝」) and the CLI spawn it
//! shares with the one-click installs.
//!
//! The installer runs as `duduclaw pack install` (see `experts_rpc.rs` for
//! why). A failed run comes back **structured**: `success: false` with
//! `error_code` / `error_params` (the CLI's stable failure code, see
//! `duduclaw-cli` `expert::import_error`) and the import report, so the
//! dashboard explains it in the viewer's language with a suggested fix.
//! `error` stays as plain zh-TW text for older dashboards and logs; it never
//! carries the installer's console output or terminal commands.

#[allow(unused_imports)]
use super::*;

/// Prefix of the CLI's machine-readable failure line on stderr. Must match
/// `duduclaw-cli` `expert::import_error::MARKER`.
pub(crate) const IMPORT_ERROR_MARKER: &str = "DUDUCLAW_IMPORT_ERROR ";

/// How long one import (plan or real run) may take before it is stopped.
pub(crate) const IMPORT_TIMEOUT_SECS: u64 = 300;

/// A failed CLI run: plain text plus the failure code when the CLI (or the
/// spawn itself) gave one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CliFailure {
    /// Human text: the CLI's own message when coded, else its output tail.
    pub text: String,
    pub code: Option<String>,
    pub params: Value,
}

impl CliFailure {
    fn uncoded(text: String) -> Self {
        CliFailure { text, code: None, params: Value::Null }
    }
}

/// Split the CLI's stderr into the marker (if any) and the remaining text.
pub(crate) fn split_marker(stderr: &str) -> (Option<(String, Value, Option<String>)>, String) {
    let mut marker = None;
    let mut rest = Vec::new();
    for line in stderr.lines() {
        match line.trim().strip_prefix(IMPORT_ERROR_MARKER) {
            Some(json) => {
                if let Ok(v) = serde_json::from_str::<Value>(json)
                    && let Some(code) = v.get("code").and_then(|c| c.as_str())
                    && is_code_shape(code)
                {
                    let message = v.get("message").and_then(|m| m.as_str()).map(str::to_string);
                    marker = Some((code.to_string(), v.get("params").cloned().unwrap_or(Value::Null), message));
                }
            }
            None => rest.push(line),
        }
    }
    (marker, rest.join("\n"))
}

/// A failure code is `group.name` in lowercase ASCII — anything else from
/// the subprocess is ignored rather than passed to the dashboard.
fn is_code_shape(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.contains('.')
        && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
}

impl MethodHandler {
    pub(crate) async fn handle_experts_install(&self, params: Value) -> WsFrame {
        let bin = duduclaw_core::resolve_duduclaw_bin();
        self.experts_install_with(&params, &bin, IMPORT_TIMEOUT_SECS).await
    }

    /// [`Self::handle_experts_install`] with the binary and timeout given,
    /// so tests can drive the response shapes with a fake installer.
    pub(crate) async fn experts_install_with(
        &self,
        params: &Value,
        bin: &std::path::Path,
        timeout_secs: u64,
    ) -> WsFrame {
        let Some(path) = params.get("path").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "path parameter is required");
        };
        let src = std::path::Path::new(path);
        if !src.exists() {
            return WsFrame::error_response("", "安裝來源不存在（請重新上傳）");
        }
        // Only what `POST /api/experts/upload` staged: since agent-definition
        // formats are recognised by common file names (`AGENTS.md`,
        // `SOUL.md`), an arbitrary server path (a home directory, a repo)
        // would otherwise qualify. Directory installs stay a CLI action.
        let staged = match (
            src.canonicalize(),
            crate::expert_admin::upload_dir(&self.home_dir).canonicalize(),
        ) {
            (Ok(s), Ok(dir)) => s.starts_with(&dir) && s.is_file(),
            _ => false,
        };
        if !staged {
            return WsFrame::error_response("", "安裝來源必須是透過儀表板上傳的檔案（請重新上傳）");
        }
        let accepted_ext = src
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(crate::expert_admin::upload_extension)
            .is_some();
        if !accepted_ext {
            return WsFrame::error_response("", "安裝來源必須是 .zip、.md 或 .yml 檔");
        }
        let dry_run = params.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);

        // Hooks are NEVER auto-trusted from the dashboard — they land
        // disabled with an approval request (fail-closed), decided in the
        // approval center. The machine-readable report comes back through a
        // private temp file.
        let report_path = crate::expert_admin::upload_dir(&self.home_dir)
            .join(format!("report-{}.json", uuid::Uuid::new_v4()));
        let mut args = vec!["install".into(), src.as_os_str().to_os_string()];
        if dry_run {
            args.push("--dry-run".into());
        }
        args.push("--report-json".into());
        args.push(report_path.as_os_str().to_os_string());
        for extra in [Self::attach_under_args(params), Self::reimport_args(params)] {
            match extra {
                Ok(a) => args.extend(a),
                Err(frame) => return frame,
            }
        }
        args.extend(self.agent_cap_args().await);

        let result = self
            .spawn_cli_group_detailed(bin, "pack", &args, timeout_secs)
            .await;
        let report = std::fs::read_to_string(&report_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let _ = std::fs::remove_file(&report_path);
        match result {
            Ok(output) => WsFrame::ok_response(
                "",
                json!({ "success": true, "dry_run": dry_run, "output": output, "report": report }),
            ),
            Err(fail) => WsFrame::ok_response("", failure_payload(dry_run, fail, report)),
        }
    }

    /// `reimport` ("replace" | "copy") + `replace_slug` + `accept_edits` →
    /// `--replace <slug> [--accept-edits]` / `--as-copy`. Unknown values
    /// and malformed slugs are refused (never dropped silently).
    pub(crate) fn reimport_args(params: &Value) -> Result<Vec<std::ffi::OsString>, WsFrame> {
        match params.get("reimport").and_then(|v| v.as_str()) {
            None | Some("") => Ok(Vec::new()),
            Some("copy") => Ok(vec!["--as-copy".into()]),
            Some("replace") => {
                let slug = params.get("replace_slug").and_then(|v| v.as_str()).unwrap_or("").trim();
                if !duduclaw_core::is_valid_agent_id(slug) {
                    return Err(WsFrame::error_response("", "replace_slug 不合法"));
                }
                let mut a: Vec<std::ffi::OsString> = vec!["--replace".into(), slug.into()];
                if params.get("accept_edits").and_then(|v| v.as_bool()) == Some(true) {
                    a.push("--accept-edits".into());
                }
                Ok(a)
            }
            Some(other) => Err(WsFrame::error_response(
                "",
                &format!("reimport '{}' 不是 replace 或 copy", other.escape_debug()),
            )),
        }
    }

    /// `--agent-cap <n>` when this gateway caps the employee count, so an
    /// import is held to the same cap as `agents.create`.
    pub(crate) async fn agent_cap_args(&self) -> Vec<std::ffi::OsString> {
        match self.effective_agent_cap().await {
            0 => Vec::new(),
            n => vec!["--agent-cap".into(), n.to_string().into()],
        }
    }

    /// Spawn `duduclaw <group> <args…>` against the gateway home and keep
    /// the failure structured (code from the CLI's marker line; timeouts and
    /// spawn errors coded here).
    pub(crate) async fn spawn_cli_group_detailed(
        &self,
        bin: &std::path::Path,
        group: &str,
        args: &[std::ffi::OsString],
        timeout_secs: u64,
    ) -> Result<String, CliFailure> {
        let fut = tokio::process::Command::new(bin)
            .arg(group)
            .args(args)
            .env("DUDUCLAW_HOME", &self.home_dir)
            .kill_on_drop(true)
            .output();
        let output = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(CliFailure {
                    text: format!("子程序啟動失敗: {e}"),
                    code: Some("import.spawn_failed".into()),
                    params: Value::Null,
                });
            }
            Err(_) => {
                return Err(CliFailure {
                    text: format!("匯入超過 {timeout_secs} 秒仍未完成，已中止"),
                    code: Some("import.timeout".into()),
                    params: json!({ "seconds": timeout_secs }),
                });
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let (marker, stderr_rest) = split_marker(&stderr);
        // CJK-safe truncation of the console tail.
        let tail = |s: &str| duduclaw_core::truncate_chars(s.trim(), 1200);
        if output.status.success() {
            return Ok(tail(&stdout));
        }
        Err(match marker {
            Some((code, params, message)) => CliFailure {
                text: message.unwrap_or_else(|| tail(&stderr_rest)),
                code: Some(code),
                params,
            },
            None => CliFailure::uncoded(tail(if stderr_rest.trim().is_empty() { &stdout } else { &stderr_rest })),
        })
    }
}

/// The `success: false` payload. The report's own `error` object (written
/// by the CLI) wins over the marker; both name the same failure.
fn failure_payload(dry_run: bool, fail: CliFailure, report: Option<Value>) -> Value {
    let from_report = report.as_ref().and_then(|r| r.get("error")).filter(|e| e.is_object());
    let code = from_report
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .filter(|c| is_code_shape(c))
        .map(str::to_string)
        .or(fail.code)
        .unwrap_or_else(|| "import.unknown".to_string());
    let params = from_report
        .and_then(|e| e.get("params"))
        .cloned()
        .unwrap_or(fail.params);
    let text = from_report
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or(fail.text);
    // Unclassified failures keep the console tail as their detail.
    let params = if code == "import.unknown" && params.get("detail").is_none() {
        json!({ "detail": text })
    } else {
        params
    };
    json!({
        "success": false,
        "dry_run": dry_run,
        "error": text,
        "error_code": code,
        "error_params": params,
        "report": report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_split_from_the_console_text_and_validated() {
        let stderr = "Error: x\nDUDUCLAW_IMPORT_ERROR {\"code\":\"import.not_zip\",\"params\":{},\"message\":\"非合法 zip\"}\n";
        let (m, rest) = split_marker(stderr);
        let (code, _, msg) = m.unwrap();
        assert_eq!(code, "import.not_zip");
        assert_eq!(msg.as_deref(), Some("非合法 zip"));
        assert_eq!(rest, "Error: x");
        // A code of the wrong shape is ignored, and the line still removed.
        let (m, rest) = split_marker("DUDUCLAW_IMPORT_ERROR {\"code\":\"<script>\"}\n");
        assert!(m.is_none() && rest.is_empty());
    }

    #[test]
    fn report_error_wins_and_unknown_keeps_the_detail() {
        let report = json!({ "error": { "code": "import.quota_exceeded", "params": { "max": 3 }, "message": "超過上限" } });
        let fail = CliFailure::uncoded("tail".into());
        let p = failure_payload(true, fail, Some(report));
        assert_eq!(p["error_code"], "import.quota_exceeded");
        assert_eq!(p["error_params"]["max"], 3);
        assert_eq!(p["error"], "超過上限");

        let p = failure_payload(false, CliFailure::uncoded("boom".into()), None);
        assert_eq!(p["error_code"], "import.unknown");
        assert_eq!(p["error_params"]["detail"], "boom");
        assert!(!p["error"].as_str().unwrap().starts_with("安裝失敗"));
    }
}

#[cfg(all(test, unix))]
#[path = "experts_import_rpc_tests.rs"]
mod rpc_tests;
