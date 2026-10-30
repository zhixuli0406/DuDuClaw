//! Offline lease recovery is operator-only, including the list command.
use std::path::Path;
use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};

pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] = duduclaw_core::EGRESS_LEASE_COMMANDS;

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum EgressLeasesCommands {
    /// List leases and confirmation tokens. Requires a stopped gateway.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Back up the database, then clear exactly the confirmed lease set.
    Clear {
        #[arg(long, required_unless_present = "all", conflicts_with = "all")]
        lease: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        confirm: String,
    },
}

pub(crate) fn run(home: &Path, command: EgressLeasesCommands) -> Result<()> {
    run_with_markers(home, command, &crate::ai_session_guard::markers())
}

fn run_with_markers(home: &Path, command: EgressLeasesCommands, markers: &[&str]) -> Result<()> {
    if !markers.is_empty() {
        return Err(DuDuClawError::Config("這個指令不能在 AI 員工的工作階段中執行；請先停止 gateway，再由管理者在自己的終端機執行。".into()));
    }
    match command {
        EgressLeasesCommands::List { json } => {
            let list = duduclaw_gateway::egress_guard::recovery::list(home).map_err(DuDuClawError::Config)?;
            if json { println!("{}", serde_json::to_string_pretty(&list)?); }
            else {
                for row in &list.leases {
                    println!("{}\t{}\t{}\t{}\t{}\t--confirm {}", row.lease_id, row.agent_id, row.turn_id, row.phase, row.acquired_at, list.lease_tokens[&row.lease_id]);
                }
                println!("{} 筆；清除全部：--all --confirm {}", list.leases.len(), list.confirm);
            }
        }
        EgressLeasesCommands::Clear { lease, all, confirm } => {
            if all == lease.is_some() { return Err(DuDuClawError::Config("請指定 --lease 或 --all 其中一項。".into())); }
            let result = duduclaw_gateway::egress_guard::recovery::clear(home, lease.as_deref(), &confirm).map_err(DuDuClawError::Config)?;
            println!("已清除 {} 筆；備份：{}", result.deleted, result.backup.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_sessions_are_refused_before_files_are_touched() {
        let home = tempfile::tempdir().unwrap();
        for command in [EgressLeasesCommands::List { json: true }, EgressLeasesCommands::Clear { lease: None, all: true, confirm: "x".into() }] {
            assert!(run_with_markers(home.path(), command, &["DUDUCLAW_AGENT_ID"]).is_err());
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn bash_lane_refuses_employee_and_untrusted_callers() {
        for command in ["duduclaw egress-leases list", "duduclaw-pro egress-leases clear --all --confirm abc", "bash -c 'duduclaw egress-leases list'"] {
            for caller in [duduclaw_core::HookCaller::Agent("bot".into()), duduclaw_core::HookCaller::Untrusted("x".into())] {
                assert!(duduclaw_core::bash_operator_command_decision(command, &caller, OPERATOR_COMMANDS).is_some());
            }
        }
    }
}
