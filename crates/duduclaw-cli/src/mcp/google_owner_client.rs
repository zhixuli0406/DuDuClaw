//! Private dispatcher handoff for owner execution. No bearer protocol exists.
use std::path::Path;
use std::time::Duration;
use serde_json::{Value, json};
use duduclaw_core::google_identity as gi;

const DRIVE_WRITE_TIMEOUT: Duration = Duration::from_secs(665);
fn request_timeout(tool: &str, prepare: bool) -> Duration {
    if gi::is_drive_write_tool(tool) { DRIVE_WRITE_TIMEOUT }
    else if prepare { Duration::from_secs(30) }
    else { Duration::from_secs(if tool == "google_chat_send" { 425 } else { 125 }) }
}

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
        || (!gi::is_drive_write_tool(tool) && tool != "google_chat_send" && gi::effective_for(home, Some(agent)).effective != gi::Effective::DelegateOwner) {
        return Ok(None);
    }
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let mut call = OwnerCall { agent:agent.to_owned(), tool:tool.to_owned(),
        args_digest:gi::google_arguments_digest(&args), call_id:uuid::Uuid::new_v4().to_string(),
        turn_id:env(duduclaw_core::ENV_TRUST_TURN_ID), run_id:env(duduclaw_core::ENV_DISPATCH_RUN_ID),
        binding:String::new(), owner_email:String::new() };
    let response = post(home, agent, &call.request("employee_prepare", &args),
        ROUTE, request_timeout(tool, true), 512 * 1024).await?;
    let result = response.get("result").ok_or(REFUSED)?;
    call.binding = result.get("approval_binding_digest").and_then(Value::as_str)
        .filter(|s| gi::google_digest_well_formed(s)).ok_or(REFUSED)?.to_owned();
    // Drive prepare returns only the opaque binding, not an owner account.
    call.owner_email = if gi::is_drive_write_tool(tool) { String::new() }
        else { result.get("owner_email").and_then(Value::as_str).ok_or(REFUSED)?.to_owned() };
    params.as_object_mut().ok_or(REFUSED)?.insert("google_owner_binding".into(), json!(call.binding));
    if !gi::is_drive_write_tool(tool) {
        params.as_object_mut().ok_or(REFUSED)?.insert("google_owner_account".into(), json!(call.card()));
    }
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
    match post(home, &call.agent, &call.request("employee", args),
        ROUTE, request_timeout(tool, false), 512 * 1024).await {
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
        assert_eq!(gi::NATIVE_GOOGLE_TOOLS.len(), 38);
        for tool in gi::NATIVE_GOOGLE_TOOLS { assert!(gi::native_google_scope(tool).is_some()); }
    }
    #[tokio::test]
    async fn no_approved_dispatch_context_never_sends() {
        let result = execute(Path::new("/unavailable"), "gmail_read", &json!({})).await;
        assert_eq!(result["isError"], true);
    }
}

pub(super) fn account() -> Option<String> {
    APPROVED.try_with(|c| c.as_ref().filter(|call| !call.owner_email.is_empty())
        .map(|call| call.owner_email.clone())).ok().flatten()
}

async fn post(home: &Path, agent: &str, body: &Value, route: &str, timeout: Duration, max_response: usize) -> Result<Value, String> {
    #[cfg(test)]
    if super::google_execute_client::test_transport::installed(home) {
        return super::google_execute_client::test_transport::invoke(home,
            &json!({"route":route, "timeout_secs":timeout.as_secs(), "body":body}));
    }
    super::google_identity_client::post_at(home, agent, body, route, timeout, max_response).await
}

#[cfg(test)]
mod drive_write_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn only_drive_writes_have_665_second_timeout() {
        for tool in gi::NATIVE_GOOGLE_TOOLS {
            for prepare in [true, false] {
                let expected = if gi::is_drive_write_tool(tool) { 665 }
                    else if prepare { 30 } else if *tool == "google_chat_send" { 425 } else { 125 };
                assert_eq!(request_timeout(tool, prepare).as_secs(), expected);
            }
        }
    }

    #[tokio::test]
    async fn all_nine_use_prepare_then_bound_employee_without_token_route() {
        let home = tempfile::tempdir().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let digest = "a".repeat(64);
        let binding = digest.clone();
        let _transport = super::super::google_execute_client::test_transport::install(home.path(), Arc::new(move |wire| {
            assert_eq!(wire["route"], "/api/internal/google-execute");
            assert_eq!(wire["timeout_secs"], 665);
            captured.lock().unwrap().push(wire["body"].clone());
            Ok(if wire["body"]["kind"] == "employee_prepare" {
                json!({"ok":true,"result":{"approval_binding_digest":binding}})
            } else { json!({"ok":true,"result":{"ok":true}}) })
        }));
        for tool in gi::DRIVE_WRITE_TOOLS {
            let args = json!({"text":"model text", "name":"model name", "description":"model description", "note":"model note"});
            let mut params = json!({"name":tool,"arguments":args});
            let call = prepare(home.path(), "employee-a", tool, &mut params).await.unwrap().unwrap();
            assert!(params.get("google_owner_account").is_none());
            call.validate(&params).unwrap();
            let result = APPROVED.scope(Some(call), execute(home.path(), tool, &args)).await;
            assert_eq!(result["ok"], true);
        }
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 18);
        for pair in requests.chunks_exact(2) {
            assert_eq!(pair[0]["kind"], "employee_prepare");
            assert_eq!(pair[1]["kind"], "employee");
            assert_eq!(pair[1]["approval_binding_digest"], digest);
            assert_eq!(pair[0]["call_id"], pair[1]["call_id"]);
            assert_eq!(pair[0]["arguments"], pair[1]["arguments"]);
        }
    }

    #[tokio::test]
    async fn drive_gateway_failure_never_falls_back_and_changed_arguments_refuse() {
        let home = tempfile::tempdir().unwrap();
        let _transport = super::super::google_execute_client::test_transport::install(home.path(), Arc::new(|_| Err("gateway down".into())));
        for tool in gi::DRIVE_WRITE_TOOLS {
            let mut params = json!({"name":tool,"arguments":{}});
            assert!(prepare(home.path(), "employee-a", tool, &mut params).await.is_err());
            let result = execute(home.path(), tool, &json!({})).await;
            assert_eq!(result["isError"], true);
        }
        let call = OwnerCall { agent:"employee-a".into(), tool:"drive_create_file".into(),
            args_digest:gi::google_arguments_digest(&json!({"text":"original"})),
            call_id:"call".into(), turn_id:None, run_id:None, binding:"a".repeat(64), owner_email:String::new() };
        let params = json!({"arguments":{"text":"changed"},"google_owner_binding":"a".repeat(64)});
        assert!(call.validate(&params).is_err());
        let result = APPROVED.scope(Some(call), execute(home.path(), "drive_create_file", &params["arguments"])).await;
        assert_eq!(result["isError"], true);
    }
}
