//! Last-Admin host recovery. Dashboard approval would deadlock this specific
//! recovery case; all other account resets use the fresh-Admin dashboard RPC.
use duduclaw_core::error::{DuDuClawError, Result};
use std::path::Path;

#[derive(clap::Subcommand)]
pub enum UsersCommands {
    /// 在閘道主機重設最後一位管理員的雙因素驗證
    MfaReset { email: String },
}
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw users mfa-reset",
        path: &["users", "mfa-reset"],
    }];
pub fn bash_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}
pub fn run(home: &Path, command: UsersCommands) -> Result<()> {
    let mut markers = crate::ai_session_guard::markers();
    for key in ["CODEX_THREAD_ID", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID"] {
        if std::env::var_os(key).is_some() {
            markers.push(key);
        }
    }
    match command {
        UsersCommands::MfaReset { email } => run_with_markers(home, &email, &markers),
    }
}
fn run_with_markers(home: &Path, email: &str, markers: &[&str]) -> Result<()> {
    if !markers.is_empty() {
        return Err(DuDuClawError::Config(
            "這個指令不能在 AI 工作階段中執行；請由管理者在閘道主機的終端機執行。".into(),
        ));
    }
    if !home.join("users.db").is_file() {
        return Err(DuDuClawError::Config("找不到帳號資料庫。".into()));
    }
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).map_err(DuDuClawError::Config)?;
    db.mfa_operator_reset_last_admin(email)
        .map_err(DuDuClawError::Config)?;
    println!("雙因素驗證已重設。若已啟用強制驗證，請重新登入並註冊。");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mfa_reset_refuses_every_ai_marker_before_opening_database() {
        let dir = tempfile::tempdir().unwrap();
        for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            assert!(
                run_with_markers(dir.path(), "admin@test.invalid", &[*marker])
                    .unwrap_err()
                    .to_string()
                    .contains("AI")
            );
        }
        assert!(!dir.path().join("users.db").exists());
    }
    #[test]
    fn mfa_reset_bash_lane_blocks_agent_and_untrusted() {
        for caller in [
            duduclaw_core::HookCaller::Agent("alice".into()),
            duduclaw_core::HookCaller::Untrusted("unknown".into()),
        ] {
            for cmd in [
                "duduclaw users mfa-reset a@example.com",
                "duduclaw-pro users mfa-reset a@example.com",
            ] {
                assert!(bash_decision(cmd, &caller).is_some());
            }
        }
        assert!(
            bash_decision(
                "duduclaw users mfa-reset a@example.com",
                &duduclaw_core::HookCaller::Absent
            )
            .is_none()
        );
    }
    #[test]
    fn mfa_reset_only_last_admin_and_audited() {
        let dir = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
        let a = db
            .create_user(
                "a@example.com",
                "A",
                "password-test",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap();
        run_with_markers(dir.path(), "a@example.com", &[]).unwrap();
        assert_eq!(
            db.query_audit_log(Some(&a.id), Some("mfa.operator_reset"), 10)
                .unwrap()
                .len(),
            1
        );
        db.create_user(
            "b@example.com",
            "B",
            "password-test",
            duduclaw_auth::UserRole::Admin,
        )
        .unwrap();
        assert!(run_with_markers(dir.path(), "a@example.com", &[]).is_err());
    }
}
