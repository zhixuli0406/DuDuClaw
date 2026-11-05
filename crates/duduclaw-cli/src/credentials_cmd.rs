//! `duduclaw credentials status | authorize` — the B2c protected key in the
//! OS credential store (design `DESIGN-credential-keychain-2026-10` §8, §12).
//!
//! `status` only reads: when nothing is enabled it does not touch the
//! credential store at all. `authorize` is for the person at the Mac after an
//! upgrade changed the executable's code hash: it lets macOS show its dialog
//! once, then re-creates the item from this executable. It refuses inside an
//! AI employee's session (first line only; the dialog itself is the real
//! gate, and the key never leaves this process).

use std::path::Path;

use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_security::credential_protection::{CredentialProtection, ProtectionStatus};

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum CredentialsCommands {
    /// Show where person-identity credentials are protected (OS keychain or
    /// file). Never prints a key.
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// macOS: allow this (upgraded) duduclaw to read the protected key again.
    /// Shows the system keychain dialog; run it at the Mac, not over SSH.
    Authorize,
}

pub(crate) fn run(home: &Path, command: CredentialsCommands) -> Result<()> {
    match command {
        CredentialsCommands::Status { json } => {
            let status = CredentialProtection::platform(home).status();
            print_status(&status, json);
            Ok(())
        }
        CredentialsCommands::Authorize => authorize(home, &crate::ai_session_guard::markers()),
    }
}

fn authorize(home: &Path, markers: &[&str]) -> Result<()> {
    if !markers.is_empty() {
        return Err(DuDuClawError::Config(format!(
            "AI 員工不能重新授權系統金鑰圈，請由管理者在這台機器的終端機執行（偵測到 {}）",
            markers.join("、")
        )));
    }
    let status = CredentialProtection::platform(home)
        .authorize()
        .map_err(|e| DuDuClawError::Config(e.message_zh))?;
    print_status(&status, false);
    Ok(())
}

fn status_text(status: &ProtectionStatus) -> (String, String) {
    use duduclaw_core::root_key::recovery::escape_diagnostic_text;
    (escape_diagnostic_text(&status.summary_zh), escape_diagnostic_text(&status.detail_zh))
}

fn print_status(status: &ProtectionStatus, json: bool) {
    let (summary, detail) = status_text(status);
    if json {
        let value = serde_json::json!({
            "level": status.level,
            "platform_level": status.platform_level,
            "backend": status.backend,
            "state": status.state,
            "key_id": status.key_id,
            "enabled_at": status.enabled_at,
            "person_identity_allowed": status.person_identity_allowed,
            "summary": summary,
            "detail": detail,
            "root_key_recovery": status.root_key_recovery,
        });
        println!("{value}");
        return;
    }
    println!("{summary}");
    println!("  {detail}");
    let allowed = if status.person_identity_allowed { "可以" } else { "不可以" };
    println!("  AI 員工使用個人帳號：{allowed}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_refuses_inside_an_ai_session_before_touching_the_store() {
        let home = std::env::temp_dir().join(format!("duduclaw-cred-cmd-{}", std::process::id()));
        let err = authorize(&home, &["DUDUCLAW_AGENT_ID"]).unwrap_err().to_string();
        assert!(err.contains("AI 員工不能"), "{err}");
        assert!(!home.exists(), "nothing may be created on refusal");
    }
    #[test]
    fn credentials_status_text_escapes_terminal_controls() {
        let home = tempfile::tempdir().unwrap();
        let mut status = CredentialProtection::platform(home.path()).status();
        status.summary_zh = "狀態\u{1b}[2J".into();
        status.detail_zh = "詳情\r\n\u{9b}\u{202e}\u{7f}".into();
        let (summary, detail) = status_text(&status);
        assert_eq!(summary, "狀態\\u{1b}[2J");
        assert_eq!(detail, "詳情\\u{d}\\u{a}\\u{9b}\\u{202e}\\u{7f}");
        assert!(!summary.chars().chain(detail.chars()).any(char::is_control));
    }

}
