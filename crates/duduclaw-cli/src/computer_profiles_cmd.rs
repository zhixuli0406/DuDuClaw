//! Read-only operator metadata. Login archives cannot be exported here.
use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use std::path::Path;

#[derive(Subcommand)]
pub enum ComputerProfileCommands {
    /// List saved browser login-state metadata without decrypting it.
    List,
}

/// Read-only metadata is operator-only too; share the Bash-lane matcher.
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw computer-profiles",
        path: &["computer-profiles"],
    }];

/// A speed bump for employee and unverified callers; operators pass through.
pub fn bash_computer_profiles_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

pub fn run(home: &Path, command: ComputerProfileCommands) -> Result<()> {
    if !crate::ai_session_guard::markers().is_empty() {
        return Err(DuDuClawError::Security(
            "請由管理者在自己的終端機查詢瀏覽器登入狀態。".into(),
        ));
    }
    match command {
        ComputerProfileCommands::List => {
            let records =
                duduclaw_gateway::computer_profiles::list(home).map_err(DuDuClawError::Security)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&records)
                    .map_err(|e| DuDuClawError::Security(e.to_string()))?
            );
        }
    }
    Ok(())
}
