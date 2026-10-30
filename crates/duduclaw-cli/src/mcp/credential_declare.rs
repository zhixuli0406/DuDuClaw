//! Credential metadata declarations use the authenticated gateway, never local writes.
use super::{record_authz::{check_actor_identity, RecordActor}, tool_error, tool_text};
use serde_json::Value;
use std::path::Path;

pub(crate) async fn handle_credential_declare(args: &Value, home: &Path, actor: RecordActor<'_>) -> Value {
    let Some(object) = args.as_object() else { return tool_error("憑證宣告格式不正確。"); };
    if object.keys().any(|key| !matches!(key.as_str(), "credential_id" | "service" | "note")) {
        return tool_error("只接受憑證編號、服務及備註，不接受憑證值。");
    }
    let Some(id) = args.get("credential_id").and_then(Value::as_str) else { return tool_error("需要憑證編號。"); };
    let Some(service) = args.get("service").and_then(Value::as_str) else { return tool_error("需要服務名稱。"); };
    let note = match args.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        _ => return tool_error("備註格式不正確。"),
    };
    if let Err(error) = check_actor_identity(home, actor, id, "credential_declare") { return tool_error(&error); }
    let Some(agent) = actor.agent() else { return tool_error("只有持有憑證的 AI 員工能宣告服務；管理員請使用儀表板。"); };
    match duduclaw_gateway::credential_declaration::declare(home, agent, id, service, note).await {
        Ok(()) => tool_text("已更新憑證服務宣告。"),
        Err(error) => tool_error(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn credential_declare_rejects_values_before_transport() {
        let home = tempfile::tempdir().unwrap();
        let result = handle_credential_declare(&serde_json::json!({"credential_id":"cred_test", "service":"CRM", "value":"secret"}), home.path(), RecordActor::Agent("bot")).await;
        assert_eq!(result["isError"], true);
    }
    #[tokio::test]
    async fn credential_declare_refuses_operator() {
        let home = tempfile::tempdir().unwrap();
        let result = handle_credential_declare(&serde_json::json!({"credential_id":"cred_test", "service":"CRM"}), home.path(), RecordActor::Operator("admin")).await;
        assert_eq!(result["isError"], true);
    }
}
