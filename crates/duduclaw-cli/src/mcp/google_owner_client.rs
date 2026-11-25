//! Private dispatcher handoff for owner execution. No bearer protocol exists.
use std::path::Path;
use std::time::Duration;
use serde_json::{Value, json};
use duduclaw_core::google_identity as gi;

const ROUTE: &str = "/api/internal/google-execute";
const REFUSED: &str = "Google 負責人或核准綁定已變更，這次操作未送出。請重新核准。";
#[derive(Clone)]
pub(crate) struct OwnerCall {
    agent: String, tool: String, args_digest: String, call_id: String,
    turn_id: Option<String>, run_id: Option<String>, binding: String,
    owner_email: String,
}
tokio::task_local! { pub(crate) static APPROVED: Option<OwnerCall>; }
fn env(name: &str) -> Option<String> {
    crate::mcp_isolation_identity::var(name).ok().filter(|s|
        !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
}
pub(crate) async fn prepare(home: &Path, agent: &str, tool: &str, params: &mut Value) -> Result<Option<OwnerCall>, String> {
    if gi::native_google_scope(tool).is_none()
        || (tool != "google_chat_send" && gi::effective_for(home, Some(agent)).effective != gi::Effective::DelegateOwner) {
        return Ok(None);
    }
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let mut call = OwnerCall { agent:agent.to_owned(), tool:tool.to_owned(),
        args_digest:gi::google_arguments_digest(&args), call_id:uuid::Uuid::new_v4().to_string(),
        turn_id:env(duduclaw_core::ENV_TRUST_TURN_ID), run_id:env(duduclaw_core::ENV_DISPATCH_RUN_ID),
        binding:String::new(), owner_email:String::new() };
    let response = super::google_identity_client::post_at(home, agent, &call.request("employee_prepare", &args),
        ROUTE, Duration::from_secs(30), 512 * 1024).await?;
    let result = response.get("result").ok_or(REFUSED)?;
    call.binding = result.get("approval_binding_digest").and_then(Value::as_str)
        .filter(|s| gi::google_digest_well_formed(s)).ok_or(REFUSED)?.to_owned();
    call.owner_email = result.get("owner_email").and_then(Value::as_str).ok_or(REFUSED)?.to_owned();
    params.as_object_mut().ok_or(REFUSED)?.insert("google_owner_binding".into(), json!(call.binding));
    params.as_object_mut().ok_or(REFUSED)?.insert("google_owner_account".into(), json!(call.card()));
    Ok(Some(call))
}
impl OwnerCall {
    fn request(&self, kind: &str, args: &Value) -> Value {
        let mut body = json!({"kind":kind, "tool":self.tool, "arguments":args,
            "turn_id":self.turn_id, "run_id":self.run_id, "call_id":self.call_id});
        if kind == "employee" { body["approval_binding_digest"] = json!(self.binding); }
        body
    }
    pub(crate) fn validate(&self, params: &Value) -> Result<(), String> {
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if self.args_digest != gi::google_arguments_digest(&args)
            || params.get("google_owner_binding").and_then(Value::as_str) != Some(self.binding.as_str()) {
            return Err(REFUSED.into());
        }
        Ok(())
    }
    pub(crate) fn card(&self) -> String { format!("Google 負責人帳號：{}；更換負責人或重新認證後，此核准失效。", self.owner_email) }
}
pub(crate) fn required(home: &Path, agent: Option<&str>) -> bool {
    APPROVED.try_with(|c| c.is_some()).unwrap_or(false)
        || agent.is_some_and(|a| gi::effective_for(home, Some(a)).effective == gi::Effective::DelegateOwner)
}
pub(crate) async fn execute(home: &Path, tool: &str, args: &Value) -> Value {
    let Some(call) = APPROVED.try_with(Clone::clone).ok().flatten() else { return super::tool_error(REFUSED); };
    if call.tool != tool || call.args_digest != gi::google_arguments_digest(args) { return super::tool_error(REFUSED); }
    match super::google_identity_client::post_at(home, &call.agent, &call.request("employee", args),
        ROUTE, Duration::from_secs(if tool == "google_chat_send" { 425 } else { 125 }), 512 * 1024).await {
        Ok(value) => value.get("result").cloned().unwrap_or_else(|| super::tool_error("Google 結果無法確認。")),
        // Transport failure after dispatch never asserts non-execution.
        Err(error) => super::tool_error(&format!("Google 結果無法確認（result_unconfirmed）；請先檢查 Google，勿直接重試。{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_26_native_tools_are_in_owner_lane() {
        assert_eq!(gi::NATIVE_GOOGLE_TOOLS.len(), 29);
        for tool in gi::NATIVE_GOOGLE_TOOLS { assert!(gi::native_google_scope(tool).is_some()); }
    }
    #[tokio::test]
    async fn no_approved_dispatch_context_never_sends() {
        let result = execute(Path::new("/unavailable"), "gmail_read", &json!({})).await;
        assert_eq!(result["isError"], true);
    }
}

pub(super) fn account() -> Option<String> {
    APPROVED.try_with(|c| c.as_ref().map(|call| call.owner_email.clone())).ok().flatten()
}
