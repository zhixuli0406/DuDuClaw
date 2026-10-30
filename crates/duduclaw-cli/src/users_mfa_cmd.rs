//! Host recovery, including multiple administrators after machine key loss.
use duduclaw_core::error::{DuDuClawError, Result};
use std::path::Path;

#[derive(clap::Subcommand)]
pub enum MfaOpsCommands {
    /// 帳號救援（僅限閘道主機操作員）
    Users {
        #[command(subcommand)]
        command: UsersCommands,
    },
}

#[derive(clap::Subcommand)]
pub enum UsersCommands {
    /// 在閘道主機重設指定帳號的雙因素驗證
    MfaReset { email: String },
    /// 在閘道主機核發一次性雙因素註冊碼（5 分鐘有效）
    MfaEnrollmentCode { email: String },
}
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw ops users mfa-reset",
        path: &["ops", "users", "mfa-reset"],
    }, duduclaw_core::OperatorCommand {
        name: "duduclaw ops users mfa-enrollment-code",
        path: &["ops", "users", "mfa-enrollment-code"],
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
        UsersCommands::MfaEnrollmentCode { email } => run_action(home, &email, &markers, true),
    }
}
fn run_with_markers(home: &Path, email: &str, markers: &[&str]) -> Result<()> {
    run_action(home,email,markers,false)
}
fn run_action(home: &Path, email: &str, markers: &[&str], enrollment: bool) -> Result<()> {
    if !markers.is_empty() {
        return Err(DuDuClawError::Config(
            "這個指令不能在 AI 工作階段中執行；請由管理者在閘道主機的終端機執行。".into(),
        ));
    }
    if !home.join("users.db").is_file() {
        return Err(DuDuClawError::Config("找不到帳號資料庫。".into()));
    }
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).map_err(DuDuClawError::Config)?;
    if enrollment {
        let user=db.get_user_by_email(email).map_err(DuDuClawError::Config)?.ok_or_else(||DuDuClawError::Config("找不到帳號。".into()))?;
        let code=db.mfa_issue_enrollment_code(None,&user.id,chrono::Utc::now().timestamp()).map_err(DuDuClawError::Config)?;
        println!("一次性註冊碼（5 分鐘有效，僅顯示一次）：{code}");
        return Ok(());
    }
    db.mfa_operator_reset(email)
        .map_err(DuDuClawError::Config)?;
    println!("雙因素驗證已重設。若已啟用強制驗證，請重新登入並註冊。");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mfa_operator_paths_match_real_clap_commands() {
        use clap::{CommandFactory, Parser};
        assert!(crate::Cli::try_parse_from(["duduclaw","users","mfa-reset","a@example.com"]).is_err());
        let cli=crate::Cli::command();
        let ops=cli.find_subcommand("ops").unwrap();
        let users=ops.find_subcommand("users").unwrap();
        for command in users.get_subcommands() {
            let path=[ops.get_name(),users.get_name(),command.get_name()];
            assert!(OPERATOR_COMMANDS.iter().any(|entry|entry.path==path));
            let argv=["duduclaw",path[0],path[1],path[2],"a@example.com"];
            assert!(crate::Cli::try_parse_from(argv).is_ok());
            assert!(bash_decision(&argv.join(" "), &duduclaw_core::HookCaller::Agent("a".into())).is_some());
        }
    }
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
                "duduclaw ops users mfa-reset a@example.com",
                "duduclaw-pro ops users mfa-reset a@example.com",
            ] {
                assert!(bash_decision(cmd, &caller).is_some());
            }
        }
        assert!(
            bash_decision(
                "duduclaw ops users mfa-reset a@example.com",
                &duduclaw_core::HookCaller::Absent
            )
            .is_none()
        );
    }
    #[test]
    fn mfa_reset_multiple_admins_and_audited() {
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
        assert!(run_with_markers(dir.path(), "a@example.com", &[]).is_ok());
    }
}
