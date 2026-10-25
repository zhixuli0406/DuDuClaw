//! Signed metadata-only account selection. No bearer is returned or read.
use duduclaw_core::google_identity::{
    CredentialRef, GOOGLE_SELECTION_ROUTE, GOOGLE_SELECTION_TTL_SECS, GoogleSelectionRequest,
    GoogleSelectionStamp,
};
use serde::Deserialize;
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    ok: bool,
    selection: Option<GoogleSelectionStamp>,
}
pub(crate) async fn select(
    home: &Path,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    args: &Value,
) -> Result<Option<GoogleSelectionStamp>, String> {
    post(
        home,
        agent,
        GoogleSelectionRequest::SelectGoogleAccount {
            tool: tool.into(),
            arguments: args.clone(),
            credential_ref: reference.clone(),
        },
    )
    .await
}
pub(crate) async fn bind(
    home: &Path,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    stamp: &GoogleSelectionStamp,
    args: &Value,
) -> Result<GoogleSelectionStamp, String> {
    let returned = post(
        home,
        agent,
        GoogleSelectionRequest::BindGoogleArguments {
            opaque_id: stamp.opaque_id.clone(),
            tool: tool.into(),
            credential_ref: reference.clone(),
            original_arguments_digest: stamp.original_arguments_digest.clone(),
            account_binding_digest: stamp.approval.account_binding_digest.clone(),
            arguments: args.clone(),
        },
    )
    .await?
    .ok_or("Google 帳號選擇已失效。")?;
    if returned.opaque_id != stamp.opaque_id
        || returned.expires_at != stamp.expires_at
        || returned.original_arguments_digest != stamp.original_arguments_digest
        || returned.approval != stamp.approval
        || returned.final_arguments_digest.as_deref()
            != Some(duduclaw_core::google_identity::google_arguments_digest(args).as_str())
    {
        return Err("Google 帳號或參數綁定已變更，請重新選擇並核准。".into());
    }
    Ok(returned)
}
async fn post(
    home: &Path,
    agent: &str,
    request: GoogleSelectionRequest,
) -> Result<Option<GoogleSelectionStamp>, String> {
    const ERROR: &str = "Google 帳號選擇沒有完成，這次沒有執行操作。請重新操作。";
    if !duduclaw_core::is_valid_agent_id(agent)
        || !request.credential_ref().is_well_formed()
        || duduclaw_core::google_identity::native_google_scope(request.tool()).is_none()
        || !request.arguments().is_object()
    {
        return Err(ERROR.into());
    }
    let body = serde_json::to_vec(&request).map_err(|_| ERROR)?;
    if body.len() > 256 * 1024 {
        return Err("Google 帳號選擇的請求過大。".into());
    }
    let key = duduclaw_gateway::mcp_internal_key::valid_internal_keys(home)
        .into_iter()
        .next()
        .ok_or(ERROR)?;
    let identity = duduclaw_core::load_identity_key(home).ok_or(ERROR)?;
    let token = duduclaw_core::mint_identity_token(&identity, agent);
    let now = duduclaw_gateway::google_personal::authority::now();
    let nonce: [u8; 16] = rand::random();
    let headers =
        super::computer_use_client::signed_headers(&key, agent, &token, now, &nonce, &body);
    #[cfg(test)]
    let value = test_transport::invoke(home, &serde_json::from_slice(&body).map_err(|_| ERROR)?)?;
    #[cfg(not(test))]
    let value = {
        let (port, _) = duduclaw_core::gateway_port_for_home(home);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| ERROR)?;
        let mut builder = client
            .post(format!("http://127.0.0.1:{port}{GOOGLE_SELECTION_ROUTE}"))
            .header("Content-Type", "application/json");
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        // One metadata attempt. A lost reply does not authorize an effect.
        let mut response = builder.body(body).send().await.map_err(|_| ERROR)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ERROR)? {
            if bytes.len().saturating_add(chunk.len()) > 64 * 1024 {
                return Err(ERROR.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| ERROR)?;
        if !status.is_success() || value["ok"] != true {
            return Err(value["message"]
                .as_str()
                .map(|s| duduclaw_core::truncate_chars(s, 500))
                .unwrap_or(ERROR.into()));
        }
        value
    };
    #[cfg(test)]
    let _ = headers; // Signing is exercised even in the no-socket seam.
    if value["ok"] != true {
        return Err(value["message"]
            .as_str()
            .map(|s| duduclaw_core::truncate_chars(s, 500))
            .unwrap_or(ERROR.into()));
    }
    let reply: Reply = serde_json::from_value(value).map_err(|_| ERROR)?;
    if !reply.ok {
        return Err(ERROR.into());
    }
    if let Some(stamp) = &reply.selection {
        if !stamp.is_well_formed()
            || stamp.expires_at <= now
            || stamp.expires_at > now.saturating_add(GOOGLE_SELECTION_TTL_SECS + 60)
        {
            return Err(ERROR.into());
        }
    }
    Ok(reply.selection)
}

/// Unit tests use only an owned in-memory metadata transport. A missing seam
/// is an error in cfg(test), never a fallback to a live local gateway socket.
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
        static H: OnceLock<Mutex<HashMap<PathBuf, Handler>>> = OnceLock::new();
        H.get_or_init(Default::default)
    }
    pub(crate) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            handlers().lock().unwrap().remove(&self.0);
        }
    }
    pub(crate) fn install(home: &Path, handler: Handler) -> Guard {
        let key = home.canonicalize().unwrap();
        assert!(
            handlers()
                .lock()
                .unwrap()
                .insert(key.clone(), handler)
                .is_none()
        );
        Guard(key)
    }
    pub(super) fn invoke(home: &Path, body: &Value) -> Result<Value, String> {
        let key = home
            .canonicalize()
            .map_err(|_| "isolated metadata home unavailable")?;
        let handler = handlers()
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or("isolated metadata transport is required")?;
        handler(body)
    }
}
