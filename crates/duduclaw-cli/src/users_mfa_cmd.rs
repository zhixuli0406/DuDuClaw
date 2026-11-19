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
    MfaEnrollmentCode { email: String, challenge_ref: String },
    /// 在閘道主機產生第一位管理員的一次性首次設定碼（30 分鐘有效）
    MfaSetupCode,
    /// TEST BUILD ONLY (never in a release): write the first Administrator's
    /// one-time setup code to a new root-only file for automated acceptance.
    /// Linux, root, a TEST-licence build and DUDUCLAW_ISO_DISPOSABLE=1 only.
    #[cfg(duduclaw_test_root)]
    MfaSetupCodeTest {
        /// New file for the code (must not exist; its directory must be
        /// root-owned and not writable by group or other).
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

/// E10 (owner decision 2026-11-20): exists only in a `--cfg duduclaw_test_root` build.
#[cfg(duduclaw_test_root)]
#[path = "users_mfa_test_build.rs"]
mod test_build;

const MFA_RESET_COMMAND: duduclaw_core::OperatorCommand = duduclaw_core::OperatorCommand {
    name: "duduclaw ops users mfa-reset",
    path: &["ops", "users", "mfa-reset"],
};
const MFA_ENROLLMENT_CODE_COMMAND: duduclaw_core::OperatorCommand = duduclaw_core::OperatorCommand {
    name: "duduclaw ops users mfa-enrollment-code",
    path: &["ops", "users", "mfa-enrollment-code"],
};
const MFA_SETUP_CODE_COMMAND: duduclaw_core::OperatorCommand = duduclaw_core::OperatorCommand {
    name: "duduclaw ops users mfa-setup-code",
    path: &["ops", "users", "mfa-setup-code"],
};
#[cfg(not(duduclaw_test_root))]
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[MFA_RESET_COMMAND, MFA_ENROLLMENT_CODE_COMMAND, MFA_SETUP_CODE_COMMAND];
#[cfg(duduclaw_test_root)]
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[MFA_RESET_COMMAND, MFA_ENROLLMENT_CODE_COMMAND, MFA_SETUP_CODE_COMMAND, test_build::OPERATOR_COMMAND];
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
        UsersCommands::MfaEnrollmentCode { email, challenge_ref } => run_action(home, &email, &markers, Some(&challenge_ref)),
        UsersCommands::MfaSetupCode => {
            use std::io::IsTerminal;
            run_setup_code(home, &markers, std::io::stdout().is_terminal(), &mut std::io::stdout())
        }
        // Own gates (no AI-marker or terminal check: unattended acceptance);
        // prints only fixed lines and exits 0 / 3 / 1.
        #[cfg(duduclaw_test_root)]
        UsersCommands::MfaSetupCodeTest { out } => std::process::exit(test_build::run(home, &out)),
    }
}
/// RM-135: the first Administrator's one-time setup code. Host access only;
/// the only place a code is ever generated. Printed once, and only to an
/// interactive terminal's stdout (never to a pipe, file, journald or
/// `docker logs`; for Docker run `docker exec -it <container> duduclaw ops
/// users mfa-setup-code`), never logged; replaces an outstanding one.
/// Refused in AI sessions, and for good once the first Administrator has
/// finished two-factor setup (closed latch).
fn run_setup_code(home: &Path, markers: &[&str], interactive: bool, out: &mut dyn std::io::Write) -> Result<()> {
    use std::io::Write as _;
    if !markers.is_empty() {
        return Err(DuDuClawError::Config(
            "這個指令不能在 AI 工作階段中執行；請由管理者在閘道主機的終端機執行。".into(),
        ));
    }
    // Speed bump, not a boundary (Bash can wrap this in a PTY): docs/features/05-security-defense.md "Known limitation".
    if !interactive {
        return Err(DuDuClawError::Config(
            "首次設定碼只會顯示在互動式終端機上；請直接在主機的終端機執行（Docker：docker exec -it <容器> duduclaw ops users mfa-setup-code），不要導向檔案或管線。".into(),
        ));
    }
    if !home.join("users.db").is_file() {
        return Err(DuDuClawError::Config("找不到帳號資料庫。".into()));
    }
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).map_err(DuDuClawError::Config)?;
    let issued = db.mfa_issue_bootstrap_token("本機指令列（身分未驗證）", chrono::Utc::now().timestamp())
        .map_err(DuDuClawError::Config)?;
    let text = match issued {
        Some(code) => format!("第一位管理員的一次性首次設定碼（30 分鐘有效，只能使用一次，僅顯示一次）：{code}\n請登入儀表板，在兩步驟驗證設定畫面輸入這組設定碼。"),
        None => "目前不需要首次設定碼：未要求兩步驟驗證，或找不到唯一的第一位管理員帳號。".to_owned(),
    };
    writeln!(out, "{text}").map_err(|e| DuDuClawError::Config(e.to_string()))
}
fn run_with_markers(home: &Path, email: &str, markers: &[&str]) -> Result<()> {
    run_action(home,email,markers,None)
}
fn run_action(home: &Path, email: &str, markers: &[&str], enrollment: Option<&str>) -> Result<()> {
    if !markers.is_empty() {
        return Err(DuDuClawError::Config(
            "這個指令不能在 AI 工作階段中執行；請由管理者在閘道主機的終端機執行。".into(),
        ));
    }
    if !home.join("users.db").is_file() {
        return Err(DuDuClawError::Config("找不到帳號資料庫。".into()));
    }
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).map_err(DuDuClawError::Config)?;
    if let Some(challenge_ref) = enrollment {
        let user=db.get_user_by_email(email).map_err(DuDuClawError::Config)?.ok_or_else(||DuDuClawError::Config("找不到帳號。".into()))?;
        let code=db.mfa_issue_enrollment_code(None,&user.id,challenge_ref,chrono::Utc::now().timestamp()).map_err(DuDuClawError::Config)?;
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
            let mut argv=vec!["duduclaw",path[0],path[1],path[2]];
            match path[2] {
                "mfa-setup-code" => {}
                // Only present in a `--cfg duduclaw_test_root` build.
                "mfa-setup-code-test" => argv.extend(["--out", "/root/setup-code"]),
                _ => argv.push("a@example.com"),
            }
            if path[2]=="mfa-enrollment-code" { argv.push("0123456789ab"); }
            assert!(crate::Cli::try_parse_from(&argv).is_ok());
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
    fn mfa_setup_code_refuses_ai_markers_before_opening_database_and_issues_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            assert!(run_setup_code(dir.path(), &[*marker], true, &mut out).unwrap_err().to_string().contains("AI"));
        }
        assert!(!dir.path().join("users.db").exists());
        let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
        db.ensure_default_admin().unwrap();
        // Not enforced: nothing to issue, still a success for the operator.
        run_setup_code(dir.path(), &[], true, &mut out).unwrap();
        let rows = db.query_audit_log(None, Some("mfa.bootstrap_code_issued"), 10).unwrap();
        assert!(rows.is_empty());
        for caller in [duduclaw_core::HookCaller::Agent("alice".into()), duduclaw_core::HookCaller::Untrusted("unknown".into())] {
            assert!(bash_decision("duduclaw ops users mfa-setup-code", &caller).is_some());
        }
    }

    /// An enforced, claimed installation nobody has enrolled on yet.
    fn pending_bootstrap() -> (tempfile::TempDir, duduclaw_auth::UserDb) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
        let db = duduclaw_auth::UserDb::new_with_mfa_default(&dir.path().join("users.db"), true).unwrap();
        db.ensure_default_admin().unwrap();
        assert!(db.claim_default_admin_with_email(Some("owner@example.test"), "password-test").unwrap());
        (dir, db)
    }

    #[test]
    fn mfa_setup_code_prints_only_to_an_interactive_terminal() {
        let (dir, db) = pending_bootstrap();
        assert!(db.mfa_bootstrap_pending().unwrap());
        // Piped / redirected / captured by a service manager: refused, no code minted.
        let mut out = Vec::new();
        let error = run_setup_code(dir.path(), &[], false, &mut out).unwrap_err().to_string();
        assert!(error.contains("互動式終端機") && error.contains("docker exec -it"), "{error}");
        assert!(out.is_empty());
        assert!(db.query_audit_log(None, Some("mfa.bootstrap_code_issued"), 10).unwrap().is_empty());
        // An interactive terminal gets the code, once.
        run_setup_code(dir.path(), &[], true, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.split(|c: char| !c.is_ascii_hexdigit()).any(|word| word.len() == 32), "{text}");
        let rows = db.query_audit_log(None, Some("mfa.bootstrap_code_issued"), 10).unwrap();
        assert_eq!(rows.len(), 1);
        // The host command's audit actor is unchanged (E10 gives the TEST-build issuer its own).
        assert_eq!(rows[0].detail.as_deref(), Some("本機指令列（身分未驗證）"));
    }

    /// E10 production proof: a normal build has no TEST-only setup-code
    /// sub-command anywhere in the clap tree, no Bash-lane entry for it, and
    /// `mfa-setup-code` keeps every gate (AI markers, interactive terminal).
    #[cfg(not(duduclaw_test_root))]
    #[test]
    fn production_build_has_no_test_only_setup_code_command() {
        use clap::{CommandFactory, Parser};
        fn names(command: &clap::Command, out: &mut Vec<String>) {
            out.push(command.get_name().to_owned());
            out.extend(command.get_all_aliases().map(str::to_owned));
            for sub in command.get_subcommands() {
                names(sub, out);
            }
        }
        let mut all = Vec::new();
        names(&crate::Cli::command(), &mut all);
        assert!(all.iter().any(|name| name == "mfa-setup-code"), "walked the real command tree");
        assert!(all.iter().all(|name| !name.contains("mfa-setup-code-test")), "TEST-only sub-command in a production build");
        assert!(crate::Cli::try_parse_from(["duduclaw", "ops", "users", "mfa-setup-code-test", "--out", "/root/code"]).is_err());
        assert!(crate::Cli::try_parse_from(["duduclaw", "ops", "users", "mfa-setup-code", "--out", "/root/code"]).is_err(),
            "no hidden flag on the production command");
        assert_eq!(OPERATOR_COMMANDS.len(), 3);
        assert!(OPERATOR_COMMANDS.iter().all(|entry| !entry.name.contains("mfa-setup-code-test")));
        // `mfa-setup-code` behaviour unchanged: AI markers and non-terminals refused, nothing minted.
        let (dir, db) = pending_bootstrap();
        let mut out = Vec::new();
        for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            assert!(run_setup_code(dir.path(), &[*marker], true, &mut out).unwrap_err().to_string().contains("AI"));
        }
        assert!(run_setup_code(dir.path(), &[], false, &mut out).is_err());
        assert!(out.is_empty());
        assert!(db.query_audit_log(None, Some("mfa.bootstrap_code_issued"), 10).unwrap().is_empty());
    }

    #[test]
    fn mfa_setup_code_refused_after_first_admin_setup_points_to_host_reset() {
        let (dir, db) = pending_bootstrap();
        let owner = db.get_bootstrap_owner().unwrap().unwrap();
        // The first Administrator factor closes the window for good …
        rusqlite::Connection::open(dir.path().join("users.db")).unwrap()
            .execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'enc',0)", [&owner.id]).unwrap();
        // … even after it is removed again.
        db.mfa_operator_reset("owner@example.test").unwrap();
        let mut out = Vec::new();
        let error = run_setup_code(dir.path(), &[], true, &mut out).unwrap_err().to_string();
        assert!(error.contains("duduclaw ops users mfa-reset"), "{error}");
        assert!(out.is_empty());
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
