//! Operator entry point and shared meeting-transcription orchestration.
use std::path::{Path, PathBuf};

use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_inference::transcription::{
    config::{EngineKind, TranscriptionConfig},
};
use serde_json::Value;

#[derive(Subcommand)]
pub(crate) enum MeetingCommands {
    /// 將會議錄音或影片轉成繁體中文逐字稿（音訊會傳送至設定的服務商）
    Transcribe {
        file: PathBuf,
        #[arg(long)]
        engine: Option<String>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// Protect every meeting subcommand, including future additions, on the Bash lane.
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] = &[
    duduclaw_core::OperatorCommand { name: "duduclaw meeting", path: &["meeting"] },
];

pub(crate) async fn run(home: &Path, command: MeetingCommands) -> Result<()> {
    if !crate::ai_session_guard::command_markers().is_empty() {
        return Err(DuDuClawError::Config("AI 員工請使用會議逐字稿工具；此指令僅供本機操作人員使用。".into()));
    }
    let MeetingCommands::Transcribe { file, engine, out } = command;
    let cfg = TranscriptionConfig::load(home).map_err(DuDuClawError::Config)?;
    let kind = match engine.as_deref() {
        Some(name) => EngineKind::parse(name).map_err(DuDuClawError::Config)?,
        None => cfg.engine,
    };
    let output = out.unwrap_or(std::env::current_dir()?);
    std::fs::create_dir_all(&output)?;
    let result = execute(home, "meeting-operator", &file, &output, &cfg, kind, "zh-TW")
        .await.map_err(DuDuClawError::Gateway)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

// Preserve the real operator CLI's synchronous contract.
pub(crate) async fn execute(
    home: &Path, agent: &str, source: &Path, output: &Path,
    cfg: &TranscriptionConfig, kind: EngineKind, language: &str,
) -> std::result::Result<Value, String> {
    let session = std::env::var(duduclaw_core::ENV_TRUST_SESSION_ID).ok();
    duduclaw_gateway::meeting_transcription::execute(home, agent, source, output,
        cfg, kind, language, None, None, session.as_deref(), None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn meeting_cli_refuses_every_ai_marker_before_touching_files() {
        let home = tempfile::tempdir().unwrap();
        for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            let _guard = crate::ai_session_guard::test_session(&[(marker, "")]);
            let result = run(home.path(), MeetingCommands::Transcribe {
                file: home.path().join("secret.wav"), engine: None, out: Some(home.path().join("out")),
            }).await;
            assert!(result.is_err(), "accepted {marker}");
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn meeting_bash_registry_refuses_all_binary_spellings_and_keeps_operator() {
        use duduclaw_core::{bash_operator_command_decision, GuardDecision, HookCaller};
        for binary in ["duduclaw", "duduclaw-pro", "duduclaw.exe", "duduclaw-pro.exe", "/opt/bin/duduclaw"] {
            for suffix in ["meeting transcribe secret.wav", "--redact on meeting transcribe secret.wav", "meet''ing transcribe secret.wav", "meeting --help"] {
                let command = format!("{binary} {suffix}");
                for caller in [HookCaller::Agent("alice".into()), HookCaller::Untrusted("unknown".into())] {
                    assert!(matches!(bash_operator_command_decision(&command, &caller, OPERATOR_COMMANDS),
                        Some(GuardDecision::BlockedOperatorCommand { .. })), "{command}");
                }
                assert!(bash_operator_command_decision(&command, &HookCaller::Absent, OPERATOR_COMMANDS).is_none());
            }
        }
        assert!(bash_operator_command_decision("bash -c 'env duduclaw meeting transcribe a.wav'",
            &HookCaller::Agent("alice".into()), OPERATOR_COMMANDS).is_some());
        assert!(bash_operator_command_decision("duduclaw doctor", &HookCaller::Agent("alice".into()), OPERATOR_COMMANDS).is_none());
    }

    #[test]
    fn meeting_bash_hook_uses_operator_registry() {
        use duduclaw_core::{GuardDecision, HookCaller};
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        for id in ["alice", "unknown"] { std::fs::create_dir_all(root.join("agents").join(id)).unwrap(); }
        // Use an external working directory so the earlier home-write guard
        // does not preempt the operator-command registry for untrusted callers.
        let working = tempfile::tempdir().unwrap();
        let cwd = working.path().canonicalize().unwrap();
        let envelope = serde_json::json!({"cwd":cwd,"tool_input":{"command":"duduclaw meeting transcribe /tmp/a.wav"}});
        for caller in [HookCaller::Agent("alice".into()), HookCaller::Untrusted("unknown".into())] {
            let decision = crate::agent_file_guard_decision("Bash", &envelope, &root, &caller);
            assert!(matches!(decision, Some(GuardDecision::BlockedOperatorCommand { .. })), "{caller:?}: {decision:?}");
        }
        assert!(crate::agent_file_guard_decision("Bash", &envelope, &root, &HookCaller::Absent)
            .is_none_or(|decision| decision.is_allowed()));
    }

    #[test]
    fn transcription_cli_accepts_engine_and_output() {
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(["duduclaw", "meeting", "transcribe", "meeting.mp4", "--engine", "whisper", "--out", "output"]).unwrap();
        assert!(matches!(cli.command, crate::Commands::Meeting { command: MeetingCommands::Transcribe { engine: Some(_), out: Some(_), .. } }));
    }
}
