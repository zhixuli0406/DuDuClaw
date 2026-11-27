//! Tools whose authority comes from a credential bound to one human.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonToolHost {
    Gateway,
    McpWithPersonalOdoo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonTool {
    pub name: &'static str,
    pub scope: &'static str,
    pub executes_in: PersonToolHost,
}

/// The separate person-tool contract. The Odoo read/write subset will be
/// added by 141O-P10; no existing employee Odoo tools are admitted here.
pub const PERSON_TOOLS: &[PersonTool] = &[
    PersonTool { name: "my_tasks_list", scope: "tasks:read", executes_in: PersonToolHost::Gateway },
    PersonTool { name: "my_agents_status", scope: "agents:status", executes_in: PersonToolHost::Gateway },
];

pub fn person_tool(name: &str) -> Option<&'static PersonTool> {
    PERSON_TOOLS.iter().find(|tool| tool.name == name)
}

pub fn is_person_tool(name: &str) -> bool {
    person_tool(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_scopes::{EMPLOYEE_KEY_SCOPE_STRINGS, EXTERNALLY_GRANTABLE_SCOPE_STRINGS,
        MCP_SCOPE_STRINGS, PERSON_KEY_MCP_SCOPE_STRINGS};

    #[test]
    fn person_tools_are_exact_gateway_reads() {
        assert_eq!(PERSON_TOOLS.len(), 2);
        for (name, scope) in [("my_tasks_list", "tasks:read"), ("my_agents_status", "agents:status")] {
            let tool = person_tool(name).unwrap();
            assert_eq!(tool.scope, scope);
            assert_eq!(tool.executes_in, PersonToolHost::Gateway);
            assert!(is_person_tool(name));
            assert!(PERSON_KEY_MCP_SCOPE_STRINGS.contains(&scope));
            assert!(MCP_SCOPE_STRINGS.contains(&scope));
            assert!(!EXTERNALLY_GRANTABLE_SCOPE_STRINGS.contains(&scope));
            assert!(!EMPLOYEE_KEY_SCOPE_STRINGS.contains(&scope));
            assert_eq!(crate::tool_effect::effect_of_builtin(name), Some(crate::ToolEffect::Read));
            assert_eq!(crate::tool_effect::reads_private(name), Some(true));
            assert_eq!(crate::tool_effect::egress_capable(name), Some(false));
            assert!(!crate::mcp_tool_defs::EXTERNAL_TOOLS_WHITELIST.contains(&name));
            assert!(!crate::tool_catalog::builtin_tool_catalog().iter().any(|entry| entry.name == name),
                "person tools must not enter the employee dashboard picker");
        }
        for name in ["", "tasks_list", "agent_status", "odoo_search", "my_tasks_list_extra",
            "MY_TASKS_LIST", " my_tasks_list", "my_agents_status "] {
            assert!(person_tool(name).is_none(), "{name}");
            assert!(!is_person_tool(name), "{name}");
        }
    }

    #[test]
    fn person_tool_schemas_never_accept_identity() {
        for person in PERSON_TOOLS {
            let tool = crate::mcp_tool_defs::tools().find(|tool| tool.name == person.name).unwrap();
            let schema = crate::mcp_tool_defs::build_tool_schema(tool);
            assert!(schema["inputSchema"]["required"].as_array().unwrap().is_empty());
            if person.name == "my_tasks_list" {
                assert_eq!(tool.params.iter().map(|p| p.name).collect::<Vec<_>>(), ["status", "limit"]);
                assert_eq!(schema["inputSchema"]["properties"]["status"]["type"], "string");
                assert_eq!(schema["inputSchema"]["properties"]["limit"]["type"], "integer");
            } else {
                assert!(tool.params.is_empty());
            }
        }
    }
}
