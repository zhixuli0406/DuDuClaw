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

fn print_status(status: &ProtectionStatus, json: bool) {
    if json {
        let value = serde_json::json!({
            "level": status.level,
            "platform_level": status.platform_level,
            "backend": status.backend,
            "state": status.state,
            "key_id": status.key_id,
            "enabled_at": status.enabled_at,
            "person_identity_allowed": status.person_identity_allowed,
            "summary": status.summary_zh,
            "detail": status.detail_zh,
        });
        println!("{value}");
        return;
    }
    println!("{}", status.summary_zh);
    println!("  {}", status.detail_zh);
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
}
