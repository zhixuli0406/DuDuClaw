//! Administrator AI tools use only gateway-owned, live attendance proofs.
use std::{path::{Path, PathBuf}, sync::{Mutex, OnceLock}, time::{Duration, Instant}};

use duduclaw_core::admin_ai_settings::{enabled_from_home, Item, Proposal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const TOOLS: &[&str] = &["admin_settings_read", "admin_settings_propose"];
const PROOF_TTL: Duration = Duration::from_secs(5);
const REQUEST_LIMIT: usize = 64 * 1024;
const READ_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const REFUSED: &str = "目前無法使用管理員設定工具，請在有效的管理員分身對話中重試。";
const INVALID: &str = "設定參數無效，請檢查項目與數量。";
const UNAVAILABLE: &str = "管理員設定服務無法完成請求，請重新讀取設定後再試。";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Proof { pub(crate) read: bool, pub(crate) write: bool }
impl Proof {
    pub(crate) fn permits(self, tool: &str) -> bool {
        match tool {
            "admin_settings_read" => self.read,
            "admin_settings_propose" => self.write,
            _ => false,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Credentials { agent: String, context: String, key: String, token: String }
struct CachedProof { home: PathBuf, who: Credentials, started: Instant, proof: Proof }
impl CachedProof {
    fn get(&self, home: &Path, who: &Credentials, now: Instant) -> Option<Proof> {
        (self.home == home && &self.who == who && now.saturating_duration_since(self.started) < PROOF_TTL)
            .then_some(self.proof)
    }
}
// One entry bounds memory and avoids retaining credentials from past turns.
static PROOF_CACHE: OnceLock<Mutex<Option<CachedProof>>> = OnceLock::new();

fn credentials(agent: &str) -> Option<Credentials> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent) { return None; }
    #[cfg(test)]
    if let Ok(value) = TEST_TRANSPORT.try_with(|state| state.credentials.clone()) { return value; }
    let env = |key| crate::mcp_isolation_identity::var(key).ok().filter(|v| !v.is_empty());
    let context = env(duduclaw_gateway::on_behalf_of::context::ENV_CONTEXT)?;
    if context.len() != 64 || !context.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
    Some(Credentials { agent: agent.into(), context, key: env(duduclaw_core::ENV_MCP_API_KEY)?,
        token: env(duduclaw_core::ENV_AGENT_TOKEN)? })
}

/// Shared by listing and dispatch. Transport/caller exclusions precede even a proof request.
pub(crate) async fn available_for(home: &Path, principal: &crate::mcp_auth::Principal,
    agent: &str, remote_bearer: bool) -> Proof {
    if principal.is_external || remote_bearer { return Proof::default(); }
    proof(home, agent).await
}

pub(crate) async fn proof(home: &Path, agent: &str) -> Proof {
    if !enabled_from_home(home) || crate::mcp_isolation::current().is_some()
        || duduclaw_core::protected_mode::active()
        || duduclaw_gateway::runtime_readiness::isolation_active() {
        return Proof::default();
    }
    let Some(who) = credentials(agent) else { return Proof::default(); };
    let cache = PROOF_CACHE.get_or_init(|| Mutex::new(None));
    let started = Instant::now();
    match cache.lock() {
        Ok(entry) => {
            if let Some(proof) = entry.as_ref().and_then(|entry| entry.get(home, &who, started)) {
                return proof;
            }
        }
        Err(_) => return Proof::default(),
    }
    let result = match fetch(home, &who, json!({"op":"proof", "context":who.context}), 4096).await {
        Ok(bytes) => decode_proof(&bytes).unwrap_or_default(),
        Err(_) => Proof::default(),
    };
    // Start TTL before the request, never extend an old proof by network latency.
    if started.elapsed() >= PROOF_TTL { return Proof::default(); }
    match cache.lock() {
        Ok(mut entry) => *entry = Some(CachedProof { home: home.into(), who, started, proof: result }),
        Err(_) => return Proof::default(),
    }
    result
}

fn decode_proof(bytes: &[u8]) -> Result<Proof, &'static str> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Answer { ok: bool, read: bool, write: bool }
    let answer: Answer = serde_json::from_slice(bytes).map_err(|_| UNAVAILABLE)?;
    if !answer.ok || (answer.write && !answer.read) { return Err(UNAVAILABLE); }
    Ok(Proof { read: answer.read, write: answer.write })
}

async fn fetch(home: &Path, who: &Credentials, body: Value, limit: usize) -> Result<Vec<u8>, &'static str> {
    let bytes = serde_json::to_vec(&body).map_err(|_| INVALID)?;
    if bytes.len() > REQUEST_LIMIT { return Err(INVALID); }
    #[cfg(test)]
    if let Ok(result) = TEST_TRANSPORT.try_with(|state| state.fetch(body.clone())) { return result; }
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10)).build().map_err(|_| UNAVAILABLE)?;
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| UNAVAILABLE)?.as_secs();
    let nonce: [u8; 16] = rand::random();
    let mut request = client.post(format!("http://127.0.0.1:{port}{}", duduclaw_gateway::admin_ai::route::ROUTE))
        .header("Content-Type", "application/json");
    for (key, value) in super::computer_use_client::signed_headers(&who.key, &who.agent, &who.token, timestamp, &nonce, &bytes) {
        request = request.header(key, value);
    }
    let mut response = request.body(bytes).send().await.map_err(|_| UNAVAILABLE)?;
    if response.status() != reqwest::StatusCode::OK { return Err(UNAVAILABLE); }
    let mut output = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if chunk.len() <= limit.saturating_sub(output.len()) => output.extend_from_slice(&chunk),
            Ok(None) => break,
            _ => return Err(UNAVAILABLE),
        }
    }
    Ok(output)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Section { Profile, Model, Limits, Heartbeat, Schedules, Skills }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs { agent_ids: Option<Vec<String>>, sections: Option<Vec<Section>> }

fn operation_credentials(home: &Path, agent: &str) -> Result<Credentials, &'static str> {
    if !enabled_from_home(home) || crate::mcp_isolation::current().is_some()
        || duduclaw_core::protected_mode::active()
        || duduclaw_gateway::runtime_readiness::isolation_active() { return Err(REFUSED); }
    credentials(agent).ok_or(REFUSED)
}

pub(crate) async fn read(home: &Path, agent: &str, agent_ids: Option<Vec<String>>,
    sections: Option<Vec<Section>>) -> Result<Value, &'static str> {
    if agent_ids.as_ref().is_some_and(|ids| ids.len() > 200) { return Err(INVALID); }
    let who = operation_credentials(home, agent)?;
    let bytes = fetch(home, &who, json!({"op":"read", "context":who.context,
        "agent_ids":agent_ids, "sections":sections}), READ_RESPONSE_LIMIT).await?;
    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Answer { ok: bool, agents: Vec<Value>, not_available: Vec<String> }
    let answer: Answer = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
    if !answer.ok { return Err(UNAVAILABLE); }
    serde_json::to_value(answer).map_err(|_| UNAVAILABLE)
}

pub(crate) async fn propose(home: &Path, agent: &str, items: Vec<Item>, reason: String) -> Result<Value, &'static str> {
    let proposal = Proposal { items, reason };
    proposal.validate_limits().map_err(|_| INVALID)?;
    let who = operation_credentials(home, agent)?;
    let bytes = fetch(home, &who, json!({"op":"propose", "context":who.context,
        "items":proposal.items, "reason":proposal.reason}), 4096).await?;
    decode_proposal(&bytes)
}

fn decode_proposal(bytes: &[u8]) -> Result<Value, &'static str> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Answer { message: String, item_count: usize }
    let answer: Answer = serde_json::from_slice(bytes).map_err(|_| UNAVAILABLE)?;
    // Only the route's fixed notices may reach the model; no card IDs or codes.
    if !matches!(answer.message.as_str(), "已送出確認卡，請到儀表板收件匣確認；十分鐘內有效" | "沒有需要變更的內容")
    {
        return Err(UNAVAILABLE);
    }
    Ok(json!({"message":answer.message, "item_count":answer.item_count}))
}

pub(crate) async fn handle(tool: &str, args: &Value, home: &Path, agent: &str) -> Value {
    let result = match tool {
        "admin_settings_read" => match serde_json::from_value::<ReadArgs>(args.clone()) {
            Ok(args) => read(home, agent, args.agent_ids, args.sections).await,
            Err(_) => Err(INVALID),
        },
        "admin_settings_propose" => match serde_json::from_value::<Proposal>(args.clone()) {
            Ok(args) => propose(home, agent, args.items, args.reason).await,
            Err(_) => Err(INVALID),
        },
        _ => Err(REFUSED),
    };
    match result {
        Ok(value) => super::tool_text(&value.to_string()),
        Err(reason) => super::tool_error(reason),
    }
}

#[cfg(test)]
#[path = "admin_ai_client_tests.rs"]
mod tests;
#[cfg(test)]
tokio::task_local! { static TEST_TRANSPORT: tests::TestTransport; }
