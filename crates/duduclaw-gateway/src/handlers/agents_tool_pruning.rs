//! Administrative observation and reset of the employee's discovery filter.
use super::*;
use duduclaw_core::mcp_tool_defs::*;
use duduclaw_core::tool_pruning::{candidates_from_files, read_mode, Mode};

impl MethodHandler {
    pub(crate) async fn handle_agents_tool_pruning(&self, params: Value, restore: bool) -> WsFrame {
        let Some(agent_id) = params
            .get("agent_id")
            .and_then(Value::as_str)
            .filter(|id| is_valid_agent_id(id))
        else {
            return WsFrame::error_response("", "Invalid agent_id format");
        };
        if self.registry.read().await.get(agent_id).is_none() {
            return WsFrame::error_response("", "Agent not found");
        }
        let agent_dir = self.home_dir.join("agents").join(agent_id);
        if restore {
            let now = chrono::Utc::now().to_rfc3339();
            let write = crate::fs_safe::SafeDir::open_root(&self.home_dir)
                .and_then(|root| root.open_path(&["agents", agent_id], false))
                .and_then(|dir| {
                    dir.ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::NotFound, "Agent directory missing")
                    })
                })
                .and_then(|dir| dir.write_file_atomic("tool_pruning_reset_at", &now));
            if let Err(error) = write {
                return WsFrame::error_response("", &format!("Failed to restore tools: {error}"));
            }
            duduclaw_security::audit::append_audit_event(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "tool_pruning_restored",
                    agent_id,
                    duduclaw_security::audit::Severity::Info,
                    json!({"reset_at": now}),
                ),
            );
        }
        let mode = read_mode(&agent_dir);
        let caps = duduclaw_core::agent_toml::load(&agent_dir).capabilities;
        let rules = duduclaw_core::agent_toml::load_action_rules(&agent_dir);
        let sections = duduclaw_core::agent_toml::load(&agent_dir);
        let google_enabled = crate::google_workspace::integration_enabled(&self.home_dir)
            && duduclaw_core::google_identity::effective_for(&self.home_dir, Some(agent_id))
                .effective
                .tools_listed();
        let github_enabled = crate::github_workspace::integration_enabled(&self.home_dir);
        let responsibilities =
            crate::responsibility::ResponsibilityConfig::from_home(&self.home_dir).enabled;
        let knowledge_enabled = knowledge_available(&self.home_dir, &agent_dir, agent_id);
        let scoped = crate::capability_grants::scoped_tools(&agent_dir);
        let store = if scoped.is_empty() {
            None
        } else {
            crate::capability_grants::CapabilityGrantStore::open(&self.home_dir).ok()
        };
        let mut granted = std::collections::BTreeSet::new();
        let mut listed = Vec::new();
        for tool in tools() {
            let name = tool.name;
            if duduclaw_core::tool_catalog::tool_list_verdict(
                name,
                &caps.denied_tools,
                &caps.allowed_tools,
            ) != duduclaw_core::tool_catalog::ToolListVerdict::Allowed
            {
                continue;
            }
            if rules.resolve(name, duduclaw_core::effect_of(name))
                == Some(duduclaw_core::ActionVerdict::Block)
            {
                continue;
            }
            let switches: &[(&[&str], bool)] = &[
                (
                    &[
                        "knowledge_search",
                        "knowledge_get_passage",
                        "knowledge_list_collections",
                        "knowledge_add_source",
                    ],
                    knowledge_enabled,
                ),
                (OS_NATIVE_TOOLS, caps.os_native),
                (RECORDING_TOOLS, caps.recording),
                (SYSTEM_OPERATOR_TOOLS, caps.system_operator),
                (CODRIVE_TOOLS, caps.codrive),
                (COMPUTER_USE_TOOLS, caps.computer_use),
                (
                    COMPUTER_WORKSPACE_TOOLS,
                    caps.computer_use && caps.computer_use_config.workspace,
                ),
                (DB_SOURCE_TOOLS, !caps.db_sources.is_empty()),
                (FORK_TOOLS, sections.fork.enabled),
                (
                    duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS,
                    google_enabled,
                ),
                (
                    &[
                        "github_status",
                        "github_search_issues",
                        "github_issue_read",
                        "github_pr_read",
                        "github_issue_comment",
                    ],
                    github_enabled,
                ),
                (
                    &[
                        "responsibility_get",
                        "responsibility_followup",
                        "responsibility_ask",
                    ],
                    responsibilities,
                ),
            ];
            if switches
                .iter()
                .any(|(names, enabled)| !enabled && names.contains(&name))
            {
                continue;
            }
            if crate::capability_grants::set_contains_tool(&scoped, name) {
                if let Some(store) = store.as_ref() {
                    if store.has_active_grant(agent_id, name).await {
                        granted.insert(name.to_string());
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            listed.push(tool);
        }
        let names: Vec<&str> = listed.iter().map(|tool| tool.name).collect();
        let mut prunable = candidates_from_files(&self.home_dir, &agent_dir, agent_id, &names);
        prunable.retain(|name| !granted.contains(name));
        let estimated_tokens: usize = listed
            .iter()
            .filter(|tool| prunable.contains(tool.name))
            .map(|tool| build_tool_schema(tool).to_string().len())
            .sum::<usize>()
            / 4;
        let listed_count = listed.len() - if mode == Mode::On { prunable.len() } else { 0 };
        WsFrame::ok_response(
            "",
            json!({
                "mode": mode.as_str(), "listed_count": listed_count,
                "prunable_names": prunable, "estimated_tokens": estimated_tokens, "estimate": true,
                "listed_count_scope": "employee capabilities and integration gates; runtime/client-specific discovery gates may narrow further"
            }),
        )
    }
}

// Read-only counterpart to the MCP client's knowledge discovery check.
fn knowledge_available(home: &Path, agent_dir: &Path, agent: &str) -> bool {
    if !duduclaw_knowledge::limits::KnowledgeConfig::from_home(home).enabled {
        return false;
    }
    let (scope, department) = duduclaw_core::knowledge_agent::load_agent_knowledge(agent_dir);
    if scope.is_off() {
        return false;
    }
    let context = duduclaw_knowledge::requester::agent_ctx(agent, department.as_deref(), &scope);
    match duduclaw_knowledge::schema::open_read_only(home) {
        Ok(Some(conn)) => duduclaw_knowledge::acl::agent_side_collections(&conn, &context)
            .is_ok_and(|collections| !collections.is_empty()),
        _ => false,
    }
}
