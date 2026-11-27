//! Person tools never enter employee dispatch, memory or wiki namespaces.
use std::path::Path;
use serde_json::{Value, json};
use crate::mcp_auth::{Principal, PrincipalKind};

pub(crate) async fn call(home: &Path, principal: &Principal, tool: &str, args: &Value) -> Value {
    if crate::mcp_auth::person_tool_refused(tool, principal) {
        return super::tool_error("This personal key does not authorize this tool");
    }
    let Some(call) = crate::mcp_auth::current_call_matches(principal) else {
        return super::tool_error("Personal-key authentication is required");
    };
    let PrincipalKind::PersonKey { .. } = call.kind() else {
        return super::tool_error("Personal-key authentication is required");
    };
    let Some(raw) = call.raw_person_key() else {
        return super::tool_error("Personal-key authentication is required");
    };
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
