//! Operator-only employee status and evolution switches. These commands
//! remain directly available in an operator's terminal; no approval is filed.
//! Environment markers and the Bash lane are speed bumps, not isolation.
//! Bypasses include cleared markers, variable/command-substitution binaries, renamed copies, scripts, xargs, and aliases.

use std::path::Path;

use console::style;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::approval::operator_cli_gate::UNVERIFIED_ACTOR;
use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event};
use serde_json::json;

pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] = &[
    duduclaw_core::OperatorCommand {
        name: "duduclaw agent pause",
        path: &["agent", "pause"],
    },
    duduclaw_core::OperatorCommand {
        name: "duduclaw agent resume",
        path: &["agent", "resume"],
    },
    duduclaw_core::OperatorCommand {
        name: "duduclaw agent freeze",
        path: &["agent", "freeze"],
    },
    duduclaw_core::OperatorCommand {
        name: "duduclaw agent unfreeze",
        path: &["agent", "unfreeze"],
    },
];

pub fn bash_agent_control_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    Pause,
    Resume,
    Freeze,
    Unfreeze,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Freeze => "freeze",
            Self::Unfreeze => "unfreeze",
        }
    }

    fn event(self) -> &'static str {
        match self {
            Self::Pause => "agent_pause",
            Self::Resume => "agent_resume",
            Self::Freeze => "agent_freeze",
            Self::Unfreeze => "agent_unfreeze",
        }
    }
}

const AGENT_SESSION_REFUSAL: &str =
    "這個指令不能在 AI 員工的工作階段中執行；請由管理者在自己的終端機執行。";

pub fn run(home: &Path, agent: &str, action: Action) -> Result<()> {
    run_with_markers(home, agent, action, &crate::ai_session_guard::markers())
}

// Inject the shared marker lookup result rather than mutating process-wide
// environment variables in parallel tests. An empty-valued marker is present.
fn run_with_markers(home: &Path, agent: &str, action: Action, markers: &[&str]) -> Result<()> {
    if !markers.is_empty() {
        audit_refusal(home, agent, action, "agent_session");
        return Err(DuDuClawError::Agent(AGENT_SESSION_REFUSAL.into()));
    }
    if !crate::is_valid_agent_id(agent) {
        audit_refusal(home, agent, action, "invalid_agent_id");
        return Err(DuDuClawError::Agent(
            "Agent name must be lowercase alphanumeric with hyphens".into(),
        ));
    }

    #[cfg(test)]
    crate::profile_edit_test_hook::run();
    let written = duduclaw_gateway::agent_files::external::edit_agent_toml(
        home,
        agent,
        "cli",
        UNVERIFIED_ACTOR,
        |table| {
            match action {
                Action::Pause | Action::Resume => {
                    let section = table
                        .get_mut("agent")
                        .and_then(|v| v.as_table_mut())
                        .ok_or("agent.toml missing [agent] section")?;
                    let status = if matches!(action, Action::Pause) {
                        "paused"
                    } else {
                        "active"
                    };
                    section.insert("status".into(), toml::Value::String(status.into()));
                }
                Action::Freeze | Action::Unfreeze => {
                    let enabled = matches!(action, Action::Unfreeze);
                    for section in ["evolution", "heartbeat"] {
                        let sub = table
                            .entry(section.to_string())
                            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                        let sub = sub
                            .as_table_mut()
                            .ok_or_else(|| format!("agent.toml [{section}] must be a table"))?;
                        sub.insert("enabled".into(), toml::Value::Boolean(enabled));
                    }
                }
            }
            Ok(())
        },
    );
    if let Err(e) = written {
        audit_refusal(home, agent, action, "write_failed");
        return Err(DuDuClawError::Agent(e));
    }

    let mut detail = json!({"action": action.name(), "source": "cli", "actor": UNVERIFIED_ACTOR});
    match action {
        Action::Pause | Action::Resume => {
            detail["status"] = json!(if matches!(action, Action::Pause) {
                "paused"
            } else {
                "active"
            });
        }
        Action::Freeze | Action::Unfreeze => {
            let enabled = matches!(action, Action::Unfreeze);
            detail["evolution_enabled"] = json!(enabled);
            detail["heartbeat_enabled"] = json!(enabled);
        }
    }
    // Record the action before stdout can fail (for example a closed pipe).
    append_agent_audit_event(home, agent, action.event(), detail);
    match action {
        Action::Pause | Action::Resume => {
            let status = if matches!(action, Action::Pause) {
                "paused"
            } else {
                "active"
            };
            let icon = if matches!(action, Action::Pause) {
                style("⏸").yellow()
            } else {
                style("▶").green()
            };
            println!(
                "  {} Agent '{}' is now {}",
                icon,
                agent,
                style(status).bold()
            );
        }
        Action::Freeze | Action::Unfreeze => {
            let enabled = matches!(action, Action::Unfreeze);
            if enabled {
                println!(
                    "  {} Agent '{}' is {} — evolution and heartbeat re-enabled.",
                    style("▶").green(),
                    agent,
                    style("UNFROZEN").bold().green()
                );
            } else {
                println!(
                    "  {} Agent '{}' is now {} — evolution and heartbeat halted.",
                    style("🧊").blue(),
                    agent,
                    style("FROZEN").bold().blue()
                );
                println!("      {}", style("Autopilot rules (if any) are user-defined — disable them in the dashboard if needed.").dim());
            }
        }
    }
    Ok(())
}

fn audit_refusal(home: &Path, agent: &str, action: Action, reason: &str) {
    append_agent_audit_event(
        home,
        agent,
        "agent_cli_refused",
        json!({"action": action.name(), "source": "cli", "actor": UNVERIFIED_ACTOR, "reason": reason}),
    );
}

fn append_agent_audit_event(
    home: &Path,
    agent: &str,
    event_type: &str,
    mut detail: serde_json::Value,
) {
    let capped_agent = duduclaw_core::truncate_chars(agent, 128);
    if capped_agent.len() != agent.len() {
        detail["agent_truncated"] = json!(true);
    }
    append_audit_event(
        home,
        &AuditEvent::new(event_type, capped_agent, Severity::Warning, detail),
    );
}

#[cfg(test)]
mod agent_cli_guard_tests;
