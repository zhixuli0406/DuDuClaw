//! Host-attested dedicated Google execution. Raw MCP and Google credentials
//! never travel in the body or the result. This is a trusted-host assertion;
//! possession of an external MCP key alone cannot sign it.
use std::path::Path;
use std::time::Duration;

use duduclaw_core::google_identity::{CredentialRef, NativeGoogleScope, native_google_scope};
use serde_json::{Value, json};

const ROUTE: &str = "/api/internal/google-execute";
const MAX_BODY: usize = 256 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;

pub(crate) async fn execute(
    home: &Path,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let scope = native_google_scope(tool).ok_or("Google 工具不存在。")?;
    let uncertain = match scope {
        NativeGoogleScope::Write => {
            "Google 操作結果無法確認。請先到 Google 檢查是否已完成，勿直接重試，以免重複寫入。"
        }
        NativeGoogleScope::Read => "Google 暫時無法使用：DuDuClaw 主程式沒有傳回完整結果。",
    };
    if !duduclaw_core::is_valid_agent_id(agent)
        || !reference.is_well_formed()
        || !arguments.is_object()
    {
        return Err("Google 專用連線的請求格式不正確。".into());
    }
    let body = serde_json::to_vec(&json!({
        "tool": tool, "arguments": arguments, "credential_ref": reference,
    }))
    .map_err(|_| "Google 請求無法編碼。")?;
    if body.len() > MAX_BODY {
        return Err("Google 請求過大，尚未送出。".into());
    }
    let key = duduclaw_gateway::mcp_internal_key::valid_internal_keys(home)
        .into_iter()
        .next()
        .ok_or("無法確認 DuDuClaw 主程式，Google 請求尚未送出。")?;
    let identity = duduclaw_core::load_identity_key(home)
        .ok_or("無法確認 DuDuClaw 的員工身分，Google 請求尚未送出。")?;
    let token = duduclaw_core::mint_identity_token(&identity, agent);
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(125))
        .build()
        .map_err(|_| "Google 連線無法建立，請求尚未送出。")?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nonce: [u8; 16] = rand::random();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}{ROUTE}"))
        .header("Content-Type", "application/json");
    for (name, value) in
        super::computer_use_client::signed_headers(&key, agent, &token, timestamp, &nonce, &body)
    {
        request = request.header(name, value);
    }
    // One attempt only, including for writes. A timeout can follow an effect.
    let mut response = request.body(body).send().await.map_err(|_| uncertain)?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| uncertain)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(uncertain.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| uncertain)?;
    if status.is_success() && value["ok"] == true {
        let result = value
            .get("result")
            .filter(|v| v.is_object())
            .ok_or(uncertain)?;
        if result.get("content").and_then(Value::as_array).is_none() {
            return Err(uncertain.into());
        }
        return Ok(result.clone());
    }
    Err(value
        .get("message")
        .and_then(Value::as_str)
        .map(|m| duduclaw_core::truncate_chars(m, 500))
        .unwrap_or_else(|| uncertain.into()))
}
