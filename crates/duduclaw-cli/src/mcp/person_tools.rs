//! Person tools never enter employee dispatch, memory or wiki namespaces.
use std::path::Path;
use serde_json::{Value, json};
use crate::mcp_auth::{Principal, PrincipalKind};

pub(crate) async fn call(home: &Path, odoo: &super::OdooState, principal: &Principal, tool: &str, args: &Value) -> Value {
    if crate::mcp_auth::person_tool_refused(tool, principal) {
        return super::tool_error("This personal key does not authorize this tool");
    }
    let Some(call) = crate::mcp_auth::current_call_matches(principal) else {
        return super::tool_error("Personal-key authentication is required");
    };
    let PrincipalKind::PersonKey { user_id, .. } = call.kind() else {
        return super::tool_error("Personal-key authentication is required");
    };
    let Some(raw) = call.raw_person_key() else {
        return super::tool_error("Personal-key authentication is required");
    };
    use duduclaw_core::mcp_person_tools::{person_tool, PersonToolHost};
    if person_tool(tool).is_some_and(|t| t.executes_in == PersonToolHost::McpWithPersonalOdoo) {
        let actor = super::odoo_actor::OdooActor::Person { user_id, raw_key: raw };
        return super::odoo_identity_client::with_new_call(async {
            let result = super::odoo_identity_client::with_person_call(
                super::odoo::handle_odoo_actor(tool, args, home, odoo, actor)).await;
            let text = result["content"][0]["text"].as_str().unwrap_or("");
            super::odoo_identity_client::audit_result(home, &actor.audit_name(), tool,
                args, result["isError"] != true, text, true);
            result
        }).await;
    }
    let result = super::person_key_client::post(home, raw,
        &json!({"op":"call", "tool":tool, "args":args})).await;
    match result {
        Ok(value) => match value.get("result") {
            Some(result) => json!({"content":[{"type":"text", "text":result.to_string()}], "structuredContent":result}),
            None => super::tool_error("本人金鑰服務的回應缺少結果。"),
        },
        Err(message) => super::tool_error(&message),
    }
}
