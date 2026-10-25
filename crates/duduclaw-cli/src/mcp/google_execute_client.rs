//! Host-attested dedicated Google execution. Raw MCP and Google credentials
//! never travel in the body or the result. This is a trusted-host assertion;
//! possession of an external MCP key alone cannot sign it.
use std::path::Path;
#[cfg(not(test))]
use std::time::Duration;

use duduclaw_core::google_identity::{CredentialRef, NativeGoogleScope, native_google_scope};
use serde_json::{Value, json};

#[cfg(not(test))]
const ROUTE: &str = "/api/internal/google-execute";
const MAX_BODY: usize = 256 * 1024;
#[cfg(not(test))]
const MAX_RESPONSE: usize = 512 * 1024;

pub(crate) async fn execute(
    home: &Path,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    arguments: &Value,
    selection: Option<&duduclaw_core::google_identity::GoogleSelectionStamp>,
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
    let now = duduclaw_gateway::google_personal::authority::now();
    let body = encode_execute_body(tool, arguments, reference, selection, now)?;
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
    let timestamp = now;
    let nonce: [u8; 16] = rand::random();
    let headers =
        super::computer_use_client::signed_headers(&key, agent, &token, timestamp, &nonce, &body);
    #[cfg(test)]
    {
        let _ = (headers, uncertain);
        test_transport::invoke(
            home,
            &serde_json::from_slice(&body).map_err(|_| "Google 請求無法編碼。")?,
        )
    }
    #[cfg(not(test))]
    {
        let (port, _) = duduclaw_core::gateway_port_for_home(home);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(125));
        let client = if selection.is_some() {
            client.retry(reqwest::retry::never())
        } else {
            client
        };
        let client = client
            .build()
            .map_err(|_| "Google 連線無法建立，請求尚未送出。")?;
        let mut request = client
            .post(format!("http://127.0.0.1:{port}{ROUTE}"))
            .header("Content-Type", "application/json");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        // Selected claimed calls disable transport retries explicitly. Legacy
        // transport policy is unchanged. A timeout can follow an effect.
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
}

/// Only the caller's private AcceptedGoogleCall supplies selection. Tool
/// args/_meta are never inspected for a stamp or approval capability.
fn encode_execute_body(
    tool: &str,
    arguments: &Value,
    reference: &CredentialRef,
    selection: Option<&duduclaw_core::google_identity::GoogleSelectionStamp>,
    now: u64,
) -> Result<Vec<u8>, String> {
    let mut body = json!({"tool":tool,"arguments":arguments,"credential_ref":reference});
    if let Some(stamp) = selection {
        if !stamp.is_well_formed()
            || stamp.expires_at <= now
            || stamp.final_arguments_digest.as_deref()
                != Some(duduclaw_core::google_identity::google_arguments_digest(arguments).as_str())
        {
            return Err("Google 帳號或最終參數綁定已失效，這次沒有送出操作。".into());
        }
        body["selection_stamp"] = json!(stamp.opaque_id);
    }
    serde_json::to_vec(&body).map_err(|_| "Google 請求無法編碼。".into())
}

#[cfg(test)]
mod selected_wire_tests {
    use super::*;
    #[test]
    fn host_selected_body_carries_only_opaque_id_and_exact_final_arguments() {
        let reference = CredentialRef {
            kind: duduclaw_core::google_identity::CredentialKind::Legacy,
            sha256: "a".repeat(64),
        };
        let args = json!({"message_id":"owned"});
        let now = duduclaw_gateway::google_personal::authority::now();
        let approval: duduclaw_core::google_identity::GoogleAccountApproval=serde_json::from_value(json!({"selected_identity":"personal","selected_google_account":"unit@unit.test","selected_credential_owner":"owner-1","selected_person_login":"login@unit.test","requester_kind":"unknown","configured_home_fallback":true,"home_fallback_authorized":false,"account_binding_digest":"b".repeat(64)})).unwrap();
        let mut stamp = duduclaw_core::google_identity::GoogleSelectionStamp {
            opaque_id: uuid::Uuid::new_v4().to_string(),
            expires_at: now + 100,
            original_arguments_digest: duduclaw_core::google_identity::google_arguments_digest(
                &args,
            ),
            final_arguments_digest: Some(duduclaw_core::google_identity::google_arguments_digest(
                &args,
            )),
            approval,
        };
        let bytes =
            encode_execute_body("gmail_read", &args, &reference, Some(&stamp), now).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["selection_stamp"], stamp.opaque_id);
        assert!(body.get("home_connection").is_none());
        assert!(body.get("host_google_account").is_none());
        stamp.final_arguments_digest = Some("c".repeat(64));
        assert!(encode_execute_body("gmail_read", &args, &reference, Some(&stamp), now).is_err());
    }
}

/// An owned home must explicitly register a no-socket test transport. Missing
/// fixtures never probe a live local gateway as an accidental fallback.
#[cfg(test)]
pub(crate) mod test_transport {
    use super::*;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex, OnceLock},
    };
    type Handler = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;
    fn handlers() -> &'static Mutex<HashMap<PathBuf, Handler>> {
        static MAP: OnceLock<Mutex<HashMap<PathBuf, Handler>>> = OnceLock::new();
        MAP.get_or_init(Default::default)
    }
    pub(crate) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            handlers().lock().unwrap().remove(&self.0);
        }
    }
    pub(crate) fn install(home: &Path, handler: Handler) -> Guard {
        let home = home.canonicalize().unwrap();
        assert!(
            handlers()
                .lock()
                .unwrap()
                .insert(home.clone(), handler)
                .is_none()
        );
        Guard(home)
    }
    pub(super) fn invoke(home: &Path, body: &Value) -> Result<Value, String> {
        let home = home
            .canonicalize()
            .map_err(|_| "isolated execute home unavailable")?;
        let handler = handlers()
            .lock()
            .unwrap()
            .get(&home)
            .cloned()
            .ok_or("isolated execute transport is required; no live fallback")?;
        handler(body)
    }
}
