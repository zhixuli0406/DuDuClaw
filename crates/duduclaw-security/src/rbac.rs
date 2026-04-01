use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::types::{AgentConfig, AgentRole};

/// Actions an agent may attempt.
#[derive(Debug, Clone)]
pub enum AgentAction {
    /// Create a new child agent.
    CreateAgent,
    /// Pause a target agent by name.
    PauseAgent(String),
    /// Send a cross-agent message to the named agent.
    SendCrossAgent(String),
    /// Modify the agent's own skill set.
    ModifyOwnSkills,
    /// Modify the agent's own soul/persona.
    ModifyOwnSoul,
    /// Schedule a recurring task.
    ScheduleTask,
    /// Send a message to an external channel.
    SendToChannel(String),
}

/// Role-based access-control engine for agents.
pub struct RbacEngine;

impl RbacEngine {
    pub fn new() -> Self {
        Self
    }

    /// Check whether `agent` is allowed to perform `action`.
    pub fn check_permission(
        &self,
        agent: &AgentConfig,
        action: &AgentAction,
    ) -> Result<()> {
        let perms = &agent.permissions;

        match action {
            AgentAction::CreateAgent => {
                if !perms.can_create_agents {
                    return Err(Self::denied(&agent.agent.name, "create agents"));
                }
            }
            AgentAction::PauseAgent(target) => {
                // Only Main agents can pause others.
                if agent.agent.role != AgentRole::Main {
                    return Err(Self::denied(
                        &agent.agent.name,
                        &format!("pause agent '{target}' (requires Main role)"),
                    ));
                }
            }
            AgentAction::SendCrossAgent(_) => {
                if !perms.can_send_cross_agent {
                    return Err(Self::denied(&agent.agent.name, "send cross-agent messages"));
                }
            }
            AgentAction::ModifyOwnSkills => {
                if !perms.can_modify_own_skills {
                    return Err(Self::denied(&agent.agent.name, "modify own skills"));
                }
            }
            AgentAction::ModifyOwnSoul => {
                if !perms.can_modify_own_soul {
                    return Err(Self::denied(&agent.agent.name, "modify own soul"));
                }
            }
            AgentAction::ScheduleTask => {
                if !perms.can_schedule_tasks {
                    return Err(Self::denied(&agent.agent.name, "schedule tasks"));
                }
            }
            AgentAction::SendToChannel(channel) => {
                // Support wildcard "*" to allow all channels (MW-H1)
                let allowed = perms.allowed_channels.iter().any(|c| c == "*" || c == channel);
                if !allowed {
                    return Err(Self::denied(
                        &agent.agent.name,
                        &format!("send to channel '{channel}'"),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Validate that `creator` is allowed to spawn `new_agent` and that the
    /// new agent does not exceed the creator's own permissions.
    pub fn validate_agent_creation(
        &self,
        creator: &AgentConfig,
        new_agent: &AgentConfig,
    ) -> Result<()> {
        // Creator must be allowed to create agents.
        self.check_permission(creator, &AgentAction::CreateAgent)?;

        let cp = &creator.permissions;
        let np = &new_agent.permissions;

        // New agent must not exceed creator's permissions.
        if np.can_create_agents && !cp.can_create_agents {
            return Err(DuDuClawError::Security(
                "new agent cannot have can_create_agents when creator does not".to_string(),
            ));
        }
        if np.can_send_cross_agent && !cp.can_send_cross_agent {
            return Err(DuDuClawError::Security(
                "new agent cannot have can_send_cross_agent when creator does not".to_string(),
            ));
        }
        if np.can_modify_own_skills && !cp.can_modify_own_skills {
            return Err(DuDuClawError::Security(
                "new agent cannot have can_modify_own_skills when creator does not".to_string(),
            ));
        }
        if np.can_modify_own_soul && !cp.can_modify_own_soul {
            return Err(DuDuClawError::Security(
                "new agent cannot have can_modify_own_soul when creator does not".to_string(),
            ));
        }
        if np.can_schedule_tasks && !cp.can_schedule_tasks {
            return Err(DuDuClawError::Security(
                "new agent cannot have can_schedule_tasks when creator does not".to_string(),
            ));
        }

        // Every channel the new agent can access must also be accessible by
        // the creator.
        for channel in &np.allowed_channels {
            if !cp.allowed_channels.contains(channel) {
                return Err(DuDuClawError::Security(format!(
                    "new agent cannot access channel '{channel}' that creator lacks"
                )));
            }
        }

        Ok(())
    }

    fn denied(agent: &str, action: &str) -> DuDuClawError {
        DuDuClawError::Security(format!("agent '{agent}' is not permitted to {action}"))
    }
}

impl Default for RbacEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::*;

    fn make_agent(role: AgentRole, perms: PermissionsConfig) -> AgentConfig {
        AgentConfig {
            agent: AgentInfo {
                name: "test-agent".to_string(),
                display_name: "Test".to_string(),
                role,
                status: AgentStatus::Active,
                trigger: "manual".to_string(),
                reports_to: "none".to_string(),
                icon: "".to_string(),
            },
            model: ModelConfig {
                preferred: "gpt-4".to_string(),
                fallback: "gpt-3.5".to_string(),
                account_pool: vec![],
                local: None,
                api_mode: "cli".to_string(),
            },
            container: ContainerConfig {
                timeout_ms: 30000,
                max_concurrent: 1,
                readonly_project: true,
                additional_mounts: vec![],
                sandbox_enabled: false,
                network_access: false,
            },
            heartbeat: HeartbeatConfig {
                enabled: false,
                interval_seconds: 60,
                max_concurrent_runs: 1,
                cron: "".to_string(),
            },
            budget: BudgetConfig {
                monthly_limit_cents: 1000,
                warn_threshold_percent: 80,
                hard_stop: true,
            },
            permissions: perms,
            evolution: EvolutionConfig {
                skill_auto_activate: false,
                skill_security_scan: false,
                ..Default::default()
            },
            capabilities: CapabilitiesConfig::default(),
        }
    }

    fn full_perms() -> PermissionsConfig {
        PermissionsConfig {
            can_create_agents: true,
            can_send_cross_agent: true,
            can_modify_own_skills: true,
            can_modify_own_soul: true,
            can_schedule_tasks: true,
            allowed_channels: vec!["telegram".to_string(), "discord".to_string()],
        }
    }

    fn limited_perms() -> PermissionsConfig {
        PermissionsConfig {
            can_create_agents: false,
            can_send_cross_agent: false,
            can_modify_own_skills: false,
            can_modify_own_soul: false,
            can_schedule_tasks: false,
            allowed_channels: vec![],
        }
    }

    #[test]
    fn full_perms_agent_can_do_everything() {
        let engine = RbacEngine::new();
        let agent = make_agent(AgentRole::Main, full_perms());
        assert!(engine.check_permission(&agent, &AgentAction::CreateAgent).is_ok());
        assert!(engine.check_permission(&agent, &AgentAction::PauseAgent("x".into())).is_ok());
        assert!(engine
            .check_permission(&agent, &AgentAction::SendToChannel("telegram".into()))
            .is_ok());
    }

    #[test]
    fn limited_agent_is_denied() {
        let engine = RbacEngine::new();
        let agent = make_agent(AgentRole::Worker, limited_perms());
        assert!(engine.check_permission(&agent, &AgentAction::CreateAgent).is_err());
        assert!(engine.check_permission(&agent, &AgentAction::ScheduleTask).is_err());
    }

    #[test]
    fn child_cannot_exceed_parent_permissions() {
        let engine = RbacEngine::new();
        let creator = make_agent(AgentRole::Main, full_perms());
        let mut child_perms = full_perms();
        child_perms.allowed_channels.push("slack".to_string());
        let child = make_agent(AgentRole::Specialist, child_perms);
        assert!(engine.validate_agent_creation(&creator, &child).is_err());
    }

    #[test]
    fn valid_child_creation_succeeds() {
        let engine = RbacEngine::new();
        let creator = make_agent(AgentRole::Main, full_perms());
        let child = make_agent(AgentRole::Worker, limited_perms());
        assert!(engine.validate_agent_creation(&creator, &child).is_ok());
    }
}
