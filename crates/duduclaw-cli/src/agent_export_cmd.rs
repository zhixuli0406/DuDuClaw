//! Operator-only profile export. All content is scanned before output creation.
use std::io::Write;
use std::path::Path;

use duduclaw_core::agent_profile::{self, ExportFormat};
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event};
use serde_json::json;

pub fn run(home: &Path, id: &str, format: &str, out: &Path, include_user: bool) -> Result<()> {
    run_with_markers(home, id, format, out, include_user, &crate::ai_session_guard::markers())
}

fn run_with_markers(home: &Path, id: &str, format: &str, out: &Path, include_user: bool, markers: &[&str]) -> Result<()> {
    if !markers.is_empty() {
        return Err(DuDuClawError::Config("AI 員工不能匯出設定檔，請由管理者在儀表板或自己的終端機操作".into()));
    }
    if !crate::is_valid_agent_id(id) || duduclaw_core::is_reserved_agent_id(id) {
        return Err(DuDuClawError::Config("無效的員工代號".into()));
    }
    let parsed: ExportFormat = format.parse().map_err(|e| DuDuClawError::Config(format!("{e}")))?;
    let profile = agent_profile::load(&home.join("agents").join(id), id)
        .map_err(|e| DuDuClawError::Config(e.to_string()))?;
    let artifacts = duduclaw_security::agent_profile::export(&profile, parsed, include_user)
        .map_err(|e| DuDuClawError::Config(e.to_string()))?;
    let directory = matches!(format, "openclaw" | "hermes");
    if directory {
        std::fs::create_dir(out).map_err(|e| DuDuClawError::Config(format!("輸出目錄必須尚未存在：{e}")))?;
    }
    for artifact in artifacts {
        let path = if directory { out.join(&artifact.name) } else { out.to_path_buf() };
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|e| DuDuClawError::Config(format!("無法建立匯出檔案：{e}")))?;
        file.write_all(artifact.content.as_bytes()).and_then(|_| file.sync_all())
            .map_err(|e| DuDuClawError::Config(format!("寫入匯出檔案失敗：{e}")))?;
    }
    append_audit_event(home, &AuditEvent::new("agent_profile_exported", id, Severity::Info,
        json!({"source": "cli", "actor": "本機指令列（身分未驗證）", "format": format, "include_user": include_user})));
    println!("已匯出員工設定檔：{}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ai_session_refused_before_read_or_output() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("profile.md");
        assert!(run_with_markers(root.path(), "worker", "duduclaw", &out, true, &["DUDUCLAW_AGENT_ID"]).is_err());
        assert!(!out.exists());
    }
    #[test]
    fn traversal_refused_before_path_join() {
        let root = tempfile::tempdir().unwrap();
        for id in ["../worker", "/tmp/worker", "worker/../../other", "worker\\other"] {
            assert!(run_with_markers(root.path(), id, "duduclaw", &root.path().join("out"), false, &[]).is_err());
        }
        assert!(!root.path().join("out").exists());
    }
    #[test]
    fn export_is_an_operator_bash_command() {
        assert!(crate::agent_control_cmd::bash_agent_control_decision("duduclaw agent export worker --out /tmp/profile.md", &duduclaw_core::HookCaller::Agent("worker".into())).is_some());
    }

    #[test]
    fn context_password_refuses_every_format_before_output_creation() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname = 'worker'\n").unwrap();
        std::fs::write(agent.join("INSTRUCTIONS.md"), "db password: hunter2xyz").unwrap();
        for format in ["duduclaw", "openclaw", "hermes", "agents-md", "claude-subagent"] {
            let out = root.path().join(format);
            let error = run_with_markers(root.path(), "worker", format, &out, false, &[]).unwrap_err().to_string();
            assert!(error.contains("Rules"), "{format}: {error}");
            assert!(!error.contains("hunter2xyz"));
            assert!(!out.exists(), "{format} must refuse before creating output");
        }
    }
}
