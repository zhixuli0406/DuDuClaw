//! Operator-only root policy commands; all storage authority belongs to core.
use super::{operator_gate, refused, Result};
use duduclaw_core::root_key::policy::{self, EngineSocketAction};

#[derive(clap::Subcommand)]
pub(crate) enum ProtectedLinuxCommand {
    /// Inspect or change the root-managed protection policy
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
}

#[derive(clap::Subcommand)]
pub(crate) enum PolicyCommand {
    /// Print the effective engine-socket policy (invalid or missing means revoke)
    Show,
    /// Set the response to a reachable container-engine control interface
    EngineSocket {
        #[arg(value_enum)]
        action: Action,
        /// Acknowledge the risk of keeping personal accounts enabled
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action { Revoke, Warn }

impl From<Action> for EngineSocketAction {
    fn from(action: Action) -> Self {
        match action { Action::Revoke => Self::Revoke, Action::Warn => Self::Warn }
    }
}

// Use the same gates in tests without changing process credentials or files.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn authorize(uid: u32, command: &PolicyCommand) -> Result<()> {
    operator_gate()?;
    if uid != 0 {
        return Err(refused("需要 root 權限，請使用 sudo / Root privileges are required; use sudo."));
    }
    if let PolicyCommand::EngineSocket { action: Action::Warn, yes } = command {
        eprintln!("{}\n\n{}", policy::RISK_ZH, policy::RISK_EN);
        if !*yes {
            return Err(refused("切換為只警告前，請閱讀上述風險並加上 --yes / Read the risks above and pass --yes to select warn."));
        }
    }
    Ok(())
}

pub(crate) fn handle(command: ProtectedLinuxCommand) -> Result<()> {
    // Refuse employee sessions before platform detection or any filesystem work.
    operator_gate()?;
    let ProtectedLinuxCommand::Policy { command } = command;
    #[cfg(target_os = "linux")]
    {
        authorize(unsafe { libc::geteuid() }, &command)?;
        match command {
            PolicyCommand::Show => {
                let action = match policy::read().action {
                    EngineSocketAction::Revoke => "revoke",
                    EngineSocketAction::Warn => "warn",
                };
                println!("engine_socket_action = \"{action}\"");
            }
            PolicyCommand::EngineSocket { action, .. } => {
                policy::write(action.into())?;
                let action = match action { Action::Revoke => "revoke", Action::Warn => "warn" };
                println!("engine_socket_action = \"{action}\"\n政策已保存；下次檢查時生效 / Policy saved; effective at the next check.");
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = command;
        Err(refused("protected-linux policy requires native Linux"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_protected_policy_requires_root_and_explicit_warn_acknowledgement() {
        for command in [PolicyCommand::Show,
            PolicyCommand::EngineSocket { action: Action::Revoke, yes: false },
            PolicyCommand::EngineSocket { action: Action::Warn, yes: true }] {
            assert!(authorize(1000, &command).unwrap_err().to_string().contains("root"));
            assert!(authorize(0, &command).is_ok());
        }
        assert!(authorize(0, &PolicyCommand::EngineSocket { action: Action::Warn, yes: false })
            .unwrap_err().to_string().contains("--yes"));
    }

    #[test]
    fn service_protected_policy_refuses_every_ai_session_marker() {
        for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            let _session = crate::ai_session_guard::test_session(&[(marker, "")]);
            for command in [PolicyCommand::Show,
                PolicyCommand::EngineSocket { action: Action::Revoke, yes: true },
                PolicyCommand::EngineSocket { action: Action::Warn, yes: true }] {
                assert!(authorize(0, &command).unwrap_err().to_string().contains("AI 員工不能"));
                assert!(handle(ProtectedLinuxCommand::Policy { command })
                    .unwrap_err().to_string().contains("AI 員工不能"));
            }
        }
    }

    #[test]
    fn service_protected_policy_cli_routes_exact_commands() {
        use clap::Parser;
        crate::test_support::run_on_big_stack(|| {
            {
                let prefix = vec!["duduclaw", "service"];
                let mut show = prefix.clone();
                show.extend(["protected-linux", "policy", "show"]);
                let cli = crate::Cli::try_parse_from(show).unwrap();
                assert!(matches!(cli.command, crate::Commands::Ops(crate::OpsCommands::Service {
                    command: crate::ServiceCommands::ProtectedLinux { command: ProtectedLinuxCommand::Policy {
                        command: PolicyCommand::Show,
                    } },
                })));
                for action in ["revoke", "warn"] {
                    let mut args = prefix.clone();
                    args.extend(["protected-linux", "policy", "engine-socket", action, "--yes"]);
                    let cli = crate::Cli::try_parse_from(args).unwrap();
                    assert!(matches!(cli.command, crate::Commands::Ops(crate::OpsCommands::Service {
                        command: crate::ServiceCommands::ProtectedLinux { command: ProtectedLinuxCommand::Policy {
                            command: PolicyCommand::EngineSocket { yes: true, .. },
                        } },
                    })));
                }
                let mut invalid = prefix;
                invalid.extend(["protected-linux", "policy", "engine-socket", "ignore"]);
                assert!(crate::Cli::try_parse_from(invalid).is_err());
            }
        });
    }
}
