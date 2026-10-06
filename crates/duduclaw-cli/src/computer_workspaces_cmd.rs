//! `duduclaw ops computer-workspaces` — operator management of durable
//! computer-use workspaces (P2-C design Appendix B D8): list / fence /
//! revoke / regrant / renew / delete.
//!
//! - Refused when any DuDuClaw turn or identity variable is in the
//!   environment ([`SESSION_ENV_VARS`]), and the Bash lane of the
//!   agent-file-guard hook refuses the command for employees
//!   ([`bash_invokes_workspace_ops`]).
//! - `list` runs directly. Every state-changing action (`fence`, `revoke`,
//!   `regrant`, `renew`, `delete`) needs an Admin's approval in the dashboard
//!   first ([`duduclaw_gateway::computer_workspaces::cli_approval`]): the
//!   terminal cannot tell the operator from an AI employee with Bash, so an
//!   immediate fence would let any such employee stop someone else's
//!   workspace. Emergencies go through the dashboard's
//!   `computer_workspaces.*` RPCs or the master switch. No setting turns
//!   this off.
//! - Every requested, applied and refused action writes a security audit
//!   row (`computer_workspace_cli_action`).
//! - Events record [`UNVERIFIED_ACTOR`], never a verified operator.
//! - This process holds no computer-use sessions, so there is no barrier
//!   ([`NO_BARRIER_NOTE`]) and no `barrier_at` in its answers.

use std::path::Path;

use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::computer_use_sessions::workspace_admin::admin_sessions;
use duduclaw_gateway::computer_workspaces::cli_approval::{
    self, CliPhase, GO_TO_DASHBOARD, Gate, GatedAction, UNVERIFIED_ACTOR, audit_cli_action,
};
use serde_json::Value;

#[derive(Subcommand, Debug)]
pub enum ComputerWorkspaceCommands {
    /// List workspaces (all, or one owner's)
    List {
        #[arg(long)]
        owner: Option<String>,
    },
    /// Freeze: the AI loses control of the workspace at once
    Fence {
        workspace_id: String,
        #[arg(long, default_value = "operator_fence")]
        reason: String,
    },
    /// Suspend the workspace (status only, no reads, no attach)
    Revoke { workspace_id: String },
    /// Lift a revoke (needs an Admin approval in the dashboard)
    Regrant { workspace_id: String },
    /// Extend retention (needs an Admin approval in the dashboard)
    Renew { workspace_id: String },
    /// Delete the workspace and its files (irreversible; needs --confirm
    /// and an Admin approval in the dashboard)
    Delete {
        workspace_id: String,
        #[arg(long)]
        confirm: bool,
    },
}

/// Printed after a fence / revoke / delete from the terminal. Never "stopped
/// immediately": there is no barrier here.
pub const NO_BARRIER_NOTE: &str = "已依核准更新工作區登錄：進行中的電腦操作 session 中，還沒通過最後一道檢查的動作會立即被拒；已經通過的那一個動作會跑完；session 最慢在下一次操作或約 15 秒後的續約時結束。緊急處置（不必等核准、並等進行中的動作結束）請改用儀表板的 computer_workspaces.* RPC 或關閉 [computer_use.workspaces] 總開關。";

/// Variables a DuDuClaw-spawned process (an employee's turn, a delegated
/// task, an MCP server) carries. Any of them present and non-empty refuses.
pub const SESSION_ENV_VARS: &[&str] = &[
    "DUDUCLAW_AGENT_ID",
    "DUDUCLAW_AGENT_TOKEN",
    "DUDUCLAW_TURN_ID",
    "DUDUCLAW_SESSION_ID",
    "DUDUCLAW_REPLY_CHANNEL",
    "DUDUCLAW_HOP_DEPTH",
    "DUDUCLAW_DELEGATION_SENDER",
    "DUDUCLAW_DELEGATION_ORIGIN",
    "DUDUCLAW_DELEGATION_DEPTH",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_DATA_FILE_GUARD",
];

/// The refusal for an environment that carries any [`SESSION_ENV_VARS`],
/// `None` for a plain terminal.
fn agent_session_refusal(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    let present = SESSION_ENV_VARS
        .iter()
        .any(|k| get(k).is_some_and(|v| !v.trim().is_empty()));
    present.then(|| {
        "這個指令不能在 AI 員工的工作階段中執行；電腦操作工作區只能由管理者在自己的終端機管理。"
            .to_string()
    })
}

/// The operator-only subcommands this module owns on the Bash lane, judged by
/// the shared matcher (`duduclaw_core::bash_operator_command_decision`), the
/// same one `duduclaw ops channel-ingress` uses.
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw ops computer-workspaces",
        path: &["ops", "computer-workspaces"],
    }];

/// The Bash-lane decision for the agent-file-guard hook: employees and
/// unverified callers may not run `duduclaw ops computer-workspaces`. A speed
/// bump like the rest of the Bash lane: the command is read the way bash
/// reads it, but a variable, an alias or a renamed binary evades it; the
/// command's own environment check is the second line.
pub fn bash_workspace_ops_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

/// The terminal never reports a barrier it did not run.
fn without_barrier(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("barrier_at");
    }
    v
}

fn gated(cmd: &ComputerWorkspaceCommands) -> Option<(GatedAction, &str)> {
    match cmd {
        ComputerWorkspaceCommands::Fence { workspace_id, .. } => {
            Some((GatedAction::Fence, workspace_id))
        }
        ComputerWorkspaceCommands::Revoke { workspace_id } => {
            Some((GatedAction::Revoke, workspace_id))
        }
        ComputerWorkspaceCommands::Regrant { workspace_id } => {
            Some((GatedAction::Regrant, workspace_id))
        }
        ComputerWorkspaceCommands::Renew { workspace_id } => {
            Some((GatedAction::Renew, workspace_id))
        }
        ComputerWorkspaceCommands::Delete { workspace_id, .. } => {
            Some((GatedAction::Delete, workspace_id))
        }
        _ => None,
    }
}

/// The dashboard approval of a gated action: the consumed approval id, or
/// the error the terminal prints (non-zero exit). Requests and refusals are
/// audited here.
async fn require_approval(home: &Path, action: GatedAction, workspace_id: &str) -> Result<String> {
    let act = action.as_str();
    let refuse = |reason: &str, msg: String| {
        audit_cli_action(home, CliPhase::Refused, act, workspace_id, None, reason);
        DuDuClawError::Agent(msg)
    };
    if !duduclaw_gateway::computer_workspaces::paths::valid_workspace_id(workspace_id) {
        return Err(refuse("invalid_id", "workspace_id 格式不正確。".into()));
    }
    let store = duduclaw_gateway::computer_workspaces::shared::shared_async(home)
        .await
        .map_err(|e| refuse("registry_unavailable", format!("工作區登錄無法開啟：{e:?}")))?;
    let row = store
        .get(workspace_id)
        .map_err(|e| refuse("registry_unavailable", format!("工作區登錄無法讀取：{e:?}")))?
        .ok_or_else(|| refuse("not_found", "找不到這個電腦操作工作區。".into()))?;
    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home)
        .map_err(|e| refuse("approvals_unavailable", format!("核准紀錄無法開啟：{e}")))?;
    let valid_minutes = duduclaw_gateway::computer_workspaces::config::load(home)
        .map(|c| c.admin_approval_minutes)
        .unwrap_or(30);
    let decided = cli_approval::gate(&broker, action, &row, i64::from(valid_minutes))
        .await
        .map_err(|e| refuse("approvals_unavailable", e))?;
    match decided {
        Gate::Proceed(id) => {
            // Consumed. Check the state once more right before acting
            // (review M-6): a change in between voids the approval.
            let now = store
                .get(workspace_id)
                .map_err(|e| refuse("registry_unavailable", format!("工作區登錄無法讀取：{e:?}")))?
                .ok_or_else(|| refuse("not_found", "找不到這個電腦操作工作區。".into()))?;
            if cli_approval::state_version(&now) != cli_approval::state_version(&row) {
                audit_cli_action(
                    home,
                    CliPhase::Refused,
                    act,
                    workspace_id,
                    Some(id.as_str()),
                    "state_changed_after_approval",
                );
                return Err(DuDuClawError::Agent(format!(
                    "{APPROVAL_SPENT}工作區的狀態在核准之後又改變了，這次沒有執行任何動作。"
                )));
            }
            Ok(id.as_str().to_string())
        }
        Gate::Requested(id) | Gate::Pending(id) => {
            audit_cli_action(
                home,
                CliPhase::Requested,
                act,
                workspace_id,
                Some(id.as_str()),
                "awaiting_dashboard_approval",
            );
            Err(DuDuClawError::Agent(format!(
                "{GO_TO_DASHBOARD}（核准編號 {id}）。管理員核准後再執行一次同一個指令；工作區狀態若有變動，需要重新核准。"
            )))
        }
    }
}

/// Prefix of every message after an approval was consumed but the action
/// did not run.
const APPROVAL_SPENT: &str = "核准已使用，需要重新申請：";

pub async fn run(home: &Path, cmd: ComputerWorkspaceCommands) -> Result<()> {
    run_with_env(home, cmd, |k| std::env::var(k).ok()).await
}

/// [`run`] with the environment lookup injected (tests run in one process
/// whose environment other tests change).
async fn run_with_env(
    home: &Path,
    cmd: ComputerWorkspaceCommands,
    env: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    if let Some(msg) = agent_session_refusal(env) {
        if let Some((action, id)) = gated(&cmd) {
            audit_cli_action(
                home,
                CliPhase::Refused,
                action.as_str(),
                id,
                None,
                "agent_session_env",
            );
        }
        return Err(DuDuClawError::Agent(msg));
    }
    if let ComputerWorkspaceCommands::Delete {
        confirm: false,
        ref workspace_id,
    } = cmd
    {
        audit_cli_action(
            home,
            CliPhase::Refused,
            "delete",
            workspace_id,
            None,
            "missing_confirm",
        );
        return Err(DuDuClawError::Agent(
            "刪除會移除工作區裡的所有檔案且無法復原；確定要刪除請加上 --confirm。".to_string(),
        ));
    }
    let approved = match gated(&cmd) {
        Some((action, id)) => {
            let approval = require_approval(home, action, id).await?;
            Some((action.as_str(), id.to_string(), approval))
        }
        None => None,
    };
    let sessions = admin_sessions(home);
    let op = UNVERIFIED_ACTOR;
    let no_barrier = matches!(
        cmd,
        ComputerWorkspaceCommands::Fence { .. }
            | ComputerWorkspaceCommands::Revoke { .. }
            | ComputerWorkspaceCommands::Delete { .. }
    );
    let result = match cmd {
        ComputerWorkspaceCommands::List { owner } => {
            sessions.admin_workspace_list(owner.as_deref()).await
        }
        ComputerWorkspaceCommands::Fence {
            workspace_id,
            reason,
        } => {
            sessions
                .admin_workspace_fence(&workspace_id, op, &reason)
                .await
        }
        ComputerWorkspaceCommands::Revoke { workspace_id } => {
            sessions.admin_workspace_revoke(&workspace_id, op).await
        }
        ComputerWorkspaceCommands::Regrant { workspace_id } => {
            sessions.admin_workspace_regrant(&workspace_id, op).await
        }
        ComputerWorkspaceCommands::Renew { workspace_id } => {
            sessions.admin_workspace_renew(&workspace_id, op).await
        }
        ComputerWorkspaceCommands::Delete { workspace_id, .. } => {
            sessions.admin_workspace_delete(&workspace_id, op).await
        }
    };
    if let Some((action, id, approval)) = &approved {
        let (phase, reason) = match &result {
            Ok(_) => (CliPhase::Applied, "applied"),
            Err(_) => (CliPhase::Refused, "action_failed"),
        };
        audit_cli_action(home, phase, action, id, Some(approval), reason);
    }
    match result {
        Ok(v) => {
            let v = without_barrier(v);
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            if no_barrier {
                println!("{NO_BARRIER_NOTE}");
            }
            Ok(())
        }
        Err(e) if approved.is_some() => Err(DuDuClawError::Agent(format!(
            "{APPROVAL_SPENT}{}",
            e.message
        ))),
        Err(e) => Err(DuDuClawError::Agent(e.message)),
    }
}

#[cfg(test)]
#[path = "computer_workspaces_cmd_tests.rs"]
mod tests;
