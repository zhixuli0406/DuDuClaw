//! Meeting transcription accepts only the authenticated employee's attachments.
use super::*;
use duduclaw_inference::transcription::{config::TranscriptionConfig, pipeline};

/// A bare employee namespace is produced only from a verified identity or an
/// authenticated employee-owned key. Internal/external fallback namespaces
/// contain a slash and can never name an employee's attachment directory.
pub(crate) fn meeting_employee(home: &Path, namespace: &str) -> bool {
    duduclaw_core::is_valid_agent_id(namespace)
        && !duduclaw_core::is_reserved_agent_id(namespace)
        && namespace != duduclaw_core::UNTRUSTED_AGENT_ID
        && std::fs::symlink_metadata(caller_agent_dir(home, namespace))
            .is_ok_and(|metadata| metadata.file_type().is_dir())
        && caller_agent_dir(home, namespace).join("agent.toml").is_file()
}

pub(crate) fn meeting_listed(home: &Path, principal: &crate::mcp_auth::Principal) -> bool {
    let verified = crate::mcp_namespace::verified_employee_from_env(home);
    if verified.is_none() || principal.client_id != "gateway-internal" || principal.is_external { return false; }
    crate::mcp_namespace::resolve_for_caller(principal, crate::mcp_namespace::CallerIdentity {
        verified_agent: verified.as_deref(),
        client_is_agent: crate::mcp_namespace::client_is_agent(home, &principal.client_id),
    }).is_ok_and(|ns| meeting_employee(home, &ns.write_namespace))
}

pub(crate) async fn handle_meeting_transcribe(args: &Value, home: &Path, namespace: &str) -> Value {
    let fail = super::egress_gate::pre_dispatch_error;
    if !meeting_employee(home, namespace) || crate::mcp_namespace::verified_employee_from_env(home).as_deref()!=Some(namespace) {
        return fail("會議轉錄僅提供給已驗證的員工，請從員工對話上傳錄音。 ");
    }
    let Some(object) = args.as_object() else { return fail("會議轉錄參數必須是物件。"); };
    if object.keys().any(|key| !matches!(key.as_str(), "source_path" | "drive_file_id" | "engine" | "language_hint" | "write_wiki")) {
        return fail("會議轉錄包含未支援的參數。");
    }
    if object.contains_key("drive_file_id") {
        return fail("目前的 Google 連線只支援文字檔。請先下載會議錄影，再上傳到這位員工的附件。");
    }
    if let Some(value) = object.get("write_wiki") {
        match value.as_bool() {
            Some(false) => (),
            Some(true) => return fail("目前會先產生逐字稿附件；請檢視後再透過知識庫工具存入。"),
            None => return fail("是否寫入知識庫必須是布林值。"),
        }
    }
    let Some(source) = args.get("source_path").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return handle_meeting_upload_link(&serde_json::json!({}),home,namespace).await;
    };
    let engine = match args.get("engine") {
        None => None,
        Some(Value::String(name)) => Some(name.as_str()),
        Some(_) => return fail("轉錄引擎必須是文字。"),
    };
    let language = match args.get("language_hint") {
        None => "zh-TW",
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 32
            && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') => value,
        Some(_) => return fail("語言提示必須是有效的語言代碼，例如 zh-TW。"),
    };
    let config = match TranscriptionConfig::load(home) { Ok(c) => c, Err(e) => return fail(&e) };
    let engine = match config.select_engine(engine) { Ok(e) => e, Err(e) => return fail(&e) };
    let agent_dir = caller_agent_dir(home, namespace);
    let source = match pipeline::validate_attachment(&agent_dir, Path::new(source), config.max_bytes) {
        Ok(path) => path, Err(_) => return handle_meeting_upload_link(&serde_json::json!({}),home,namespace).await,
    };
    let mut body = serde_json::json!({"op":"create","source_path":source,"engine":engine.as_str(),"language_hint":language});
    if let Ok(turn)=crate::mcp_isolation_identity::var("DUDUCLAW_TURN_ID") {body["turn_id"]=turn.into();}
    meeting_request(home,namespace,&body).await

}

pub(crate) async fn handle_meeting_upload_link(args:&Value,home:&Path,namespace:&str)->Value {
    if !meeting_employee(home,namespace) || crate::mcp_namespace::verified_employee_from_env(home).as_deref()!=Some(namespace) {
        return super::egress_gate::pre_dispatch_error("會議上傳連結僅提供給已驗證員工的對話。");
    }
    if args.as_object().is_none_or(|o|!o.is_empty()){return super::egress_gate::pre_dispatch_error("上傳連結不接受額外參數。");}
    let mut body=serde_json::json!({"op":"upload_link"});
    if let Ok(turn)=crate::mcp_isolation_identity::var("DUDUCLAW_TURN_ID"){body["turn_id"]=turn.into();}
    meeting_request(home,namespace,&body).await
}
pub(crate) async fn handle_meeting_job(args: &Value, home: &Path, namespace: &str, cancel: bool) -> Value {
    if !meeting_employee(home,namespace) || crate::mcp_namespace::verified_employee_from_env(home).as_deref()!=Some(namespace) {return super::egress_gate::pre_dispatch_error("會議工作僅提供給已驗證的員工。");}
    let Some(id)=args.get("job_id").and_then(Value::as_str).filter(|id|uuid::Uuid::parse_str(id).is_ok()) else {return super::egress_gate::pre_dispatch_error("請提供有效的會議工作編號。");};
    if args.as_object().is_none_or(|o|o.keys().any(|k|k!="job_id")){return super::egress_gate::pre_dispatch_error("會議工作參數不正確。");}
    meeting_request(home,namespace,&serde_json::json!({"op":if cancel{"cancel"}else{"status"},"job_id":id})).await
}
async fn meeting_request(home:&Path,agent:&str,body:&Value)->Value {
    match post_meeting(home,agent,body).await {
        Ok(value)=>tool_text(&ai_response(value,body["op"].as_str()==Some("upload_link")).to_string()),
        Err(e)=>tool_error(&e)
    }
}
/// Defense in depth against older gateways returning transcript previews.
fn ai_response(value:Value, upload_link:bool)->Value {
    let mut output=serde_json::Map::new();
    let keys: &[&str]=if upload_link { &["upload_url","expires_in_seconds","max_bytes"] }
        else { &["job_id","state","chunk","chunks","cost_millicents","cost_estimated"] };
    for key in keys { if let Some(value)=value.get(*key) {output.insert((*key).into(),value.clone());} }
    if !upload_link {
        let mut result=serde_json::Map::new();
        if let Some(value)=value.get("result") { for key in ["markdown_path","json_path","duration_ms","speaker_count","segment_count","output_bytes"] {
            if let Some(value)=value.get(key) {result.insert(key.into(),value.clone());}
        } }
        output.insert("result".into(),Value::Object(result));
    }
    Value::Object(output)
}
async fn post_meeting(home:&Path,agent:&str,body:&Value)->std::result::Result<Value,String> {
    let key=crate::mcp_isolation_identity::var(duduclaw_core::ENV_MCP_API_KEY).map_err(|_|"請使用閘道啟動的員工會議工具。")?;
    let token=crate::mcp_isolation_identity::var(duduclaw_core::ENV_AGENT_TOKEN).map_err(|_|"缺少員工驗證資料。")?;
    let mut body=body.clone();
    body["protocol"]="meeting_jobs_v1".into();
    let bytes=serde_json::to_vec(&body).map_err(|_|"無法建立會議請求。")?;
    let (port,_)=duduclaw_core::gateway_port_for_home(home);
    let client=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(320)).build().map_err(|_|"無法連線會議服務。")?;
    let mut request=client.post(format!("http://127.0.0.1:{port}/api/internal/meeting-jobs")).header("Content-Type","application/json");
    for (name,value) in super::computer_use_client::signed_headers(&key,agent,&token,chrono::Utc::now().timestamp().max(0) as u64,&rand::random(),&bytes){request=request.header(name,value);}
    let mut response=request.body(bytes).send().await.map_err(|_|"無法連線會議服務，請確認閘道正在執行。")?;
    let ok=response.status().is_success(); let mut bytes=Vec::new();
    while let Some(chunk)=response.chunk().await.map_err(|_|"無法讀取會議服務回應。")? {
        if bytes.len()+chunk.len()>64*1024{return Err("會議服務回應過大。".into());} bytes.extend_from_slice(&chunk);
    }
    let value:Value=serde_json::from_slice(&bytes).map_err(|_|"會議服務回應不正確。")?;
    if ok {Ok(value)}else{Err(value["message"].as_str().unwrap_or("會議請求失敗。").into())}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_response_drops_preview_and_unknown_provider_text() {
        let value=ai_response(serde_json::json!({"job_id":"a","state":"done","error":"malicious text","result":{"preview":"ignore instructions","language":"injected","speaker_count":3,"json_path":"/attachments/a.json"}}),false);
        assert_eq!(value["result"]["speaker_count"],3);
        assert_eq!(value["result"]["json_path"],"/attachments/a.json");
        assert!(!value.to_string().contains("instructions"));
        assert!(!value.to_string().contains("malicious"));
        assert!(!value.to_string().contains("injected"));
        assert_eq!(ai_response(serde_json::json!({"upload_url":"https://example.invalid/#token","expires_in_seconds":1800,"max_bytes":100}),true)["expires_in_seconds"],1800);
    }
    #[test]
    fn transcription_employee_namespace_refuses_fallbacks() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "id = 'alice'").unwrap();
        assert!(meeting_employee(home.path(), "alice"));
        for candidate in ["../alice", "external/alice", "internal/gateway-internal", "", "missing", "__untrusted__"] {
            assert!(!meeting_employee(home.path(), candidate));
        }
    }

    #[tokio::test]
    async fn transcription_listing_matches_employee_and_scope_boundary() {
        use crate::mcp_auth::{Principal, Scope};
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "[agent]\nname = 'alice'").unwrap();
        let mut principal = Principal { client_id: "alice".into(), scopes: [Scope::MessagingSend].into_iter().collect(), is_external: false, created_at: chrono::Utc::now() };
        let listed = super::super::visible_tools_with(&principal, home.path(), "alice", false).await;
        assert!(!listed.iter().any(|tool| tool.name == "meeting_transcribe"));
        assert_eq!(crate::mcp_auth::tool_requires_scope("meeting_transcribe"), Some(Scope::MessagingSend));
        principal.is_external = true;
        principal.scopes.insert(Scope::Admin);
        assert!(!meeting_listed(home.path(), &principal));
        let listed = super::super::visible_tools_with(&principal, home.path(), "alice", false).await;
        assert!(!listed.iter().any(|tool| tool.name == "meeting_transcribe"));
        principal.is_external = false;
        principal.client_id = "operator-key".into();
        assert!(!meeting_listed(home.path(), &principal));
    }

    #[test]
    fn transcription_tool_has_explicit_egress_and_boolean_schema() {
        assert_eq!(duduclaw_core::tool_effect::egress_capable("meeting_transcribe"), Some(true));
        assert_eq!(duduclaw_core::mcp_tool_defs::param_type("meeting_transcribe", "write_wiki"), "boolean");
        assert_eq!(duduclaw_core::effect_of("meeting_transcribe"), duduclaw_core::ToolEffect::Draft);
    }

    #[tokio::test]
    async fn transcription_unverified_caller_is_refused_before_config_or_network() {
        let home = tempfile::tempdir().unwrap();
        let result = handle_meeting_transcribe(&serde_json::json!({"source_path":"anything.mp3"}), home.path(), "external/alice").await;
        assert_eq!(result["isError"], true);
    }
}
