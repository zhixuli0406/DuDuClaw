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

/// Person-key contract. Odoo names share schemas with employees, but execute
/// with a personal actor and never inherit employee authority.
pub const PERSON_TOOLS: &[PersonTool] = &[
    PersonTool { name: "my_tasks_list", scope: "tasks:read", executes_in: PersonToolHost::Gateway },
    PersonTool { name: "my_agents_status", scope: "agents:status", executes_in: PersonToolHost::Gateway },
    PersonTool { name: "odoo_search", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_crm_leads", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_sale_orders", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_inventory_products", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_inventory_check", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_invoice_list", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_payment_status", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_partner_search", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_schema_overview", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_schema_fields", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_status", scope: "odoo:read", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_create", scope: "odoo:write", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_write", scope: "odoo:write", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_activity_create", scope: "odoo:write", executes_in: PersonToolHost::McpWithPersonalOdoo },
    PersonTool { name: "odoo_note_post", scope: "odoo:write", executes_in: PersonToolHost::McpWithPersonalOdoo },
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
        assert_eq!(PERSON_TOOLS.len(), 17);
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
        for name in ["", "tasks_list", "agent_status", "odoo_execute", "my_tasks_list_extra",
            "MY_TASKS_LIST", " my_tasks_list", "my_agents_status "] {
            assert!(person_tool(name).is_none(), "{name}");
            assert!(!is_person_tool(name), "{name}");
        }
    }

    #[test]
    fn mcp_person_odoo_contract_is_closed() {
        let reads = &["odoo_search", "odoo_crm_leads", "odoo_sale_orders", "odoo_inventory_products", "odoo_inventory_check", "odoo_invoice_list", "odoo_payment_status", "odoo_partner_search", "odoo_schema_overview", "odoo_schema_fields", "odoo_status"];
        let writes = &["odoo_create", "odoo_write", "odoo_activity_create", "odoo_note_post"];
        for (names, scope) in [(reads.as_slice(), "odoo:read"), (writes.as_slice(), "odoo:write")] {
            for name in names {
                let person = person_tool(name).unwrap();
                assert_eq!(person.scope, scope);
                assert_eq!(person.executes_in, PersonToolHost::McpWithPersonalOdoo);
                assert!(crate::mcp_tool_defs::tools().any(|tool| tool.name == *name));
            }
        }
        for name in ["odoo_execute", "odoo_report", "odoo_message_send", "odoo_attachment_add",
            "odoo_connect", "odoo_read", "odoo_crm_create_lead", "odoo_crm_update_stage",
            "odoo_sale_create_quotation", "odoo_sale_confirm"] {
            assert!(person_tool(name).is_none(), "{name}");
        }
    }

    #[test]
    fn person_tool_schemas_never_accept_identity() {
        for person in PERSON_TOOLS.iter().filter(|p| p.executes_in == PersonToolHost::Gateway) {
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
