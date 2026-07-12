//! WeCom (企業微信 / WeChat Work) channel — self-built app (自建應用).
//!
//! Inbound: WeCom POSTs XML callbacks to `POST /webhook/wecom` and verifies
//! the URL with `GET /webhook/wecom` (echostr). Every request carries
//! `msg_signature` / `timestamp` / `nonce` query params; the signature is
//! `SHA1(sort(token, timestamp, nonce, encrypt))` and the payload is
//! AES-256-CBC encrypted with the 43-char `EncodingAESKey`
//! (key = Base64_Decode(EncodingAESKey + "="), IV = key[0..16], PKCS#7
//! padded to 32-byte multiples, plaintext = random(16B) + msg_len(4B BE) +
//! msg + receiveid where receiveid = corpid for self-built apps).
//! Source: developer.work.weixin.qq.com/document/path/90968 (加解密方案).
//!
//! Outbound: `GET /cgi-bin/gettoken?corpid=&corpsecret=` (token valid
//! ~7200s, cached; refreshed on errcode 40014/42001) then
//! `POST /cgi-bin/message/send?access_token=` with msgtype `markdown`
//! (WeCom renders a markdown subset in the WeCom client) falling back to
//! `text`. Content is capped at 2048 bytes per message → chunked.
//! Sources: document/path/91039 (gettoken), document/path/90236 (发送应用消息).
//!
//! The synchronous callback window is 5 s (WeCom retries 3×) — far too
//! short for an LLM reply, so the handler ACKs with an empty 200 at once
//! and delivers the answer asynchronously via `message/send`.
//!
//! Config (`config.toml [channels]`, secrets encrypted at rest):
//! - `wecom_corp_id` — enterprise ID (receiveid check target)
//! - `wecom_corp_secret` (`_enc`) — app secret for gettoken
//! - `wecom_agent_id` — self-built app AgentId
//! - `wecom_callback_token` (`_enc`) — callback Token (signature key)
//! - `wecom_encoding_aes_key` (`_enc`) — 43-char EncodingAESKey

use std::path::Path;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Router,
};
use base64::Engine;
use duduclaw_core::truncate_bytes;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::channel_reply::{build_reply_with_session, set_channel_connected, ReplyContext};

const WECOM_API: &str = "https://qyapi.weixin.qq.com/cgi-bin";

/// WeCom message content is capped at 2048 bytes (UTF-8); chunk below the
/// cap so multi-byte CJK never straddles the limit.
const WECOM_TEXT_CHUNK: usize = 1600;

/// Reject callbacks whose `timestamp` (Unix seconds) deviates from local
/// time by more than 1 hour — the same replay window DingTalk enforces.
/// Sound because `timestamp` is part of the signed tuple: an attacker
/// cannot forge a fresh timestamp without breaking the signature.
const WECOM_MAX_CLOCK_SKEW_SECS: i64 = 3600;

/// Format a reqwest error WITHOUT its request URL.
///
/// reqwest's `Display` appends the full URL (`… for url (https://…)`); for
/// WeCom the gettoken URL carries `corpid` + `corpsecret` and every send URL
/// carries `access_token` as query params. These messages flow into
/// `warn!`/`error!` (streamed to the dashboard via BroadcastLayer), the
/// channels.status error field, and delegation errors — so every reqwest
/// error MUST pass through this scrubber before formatting. Shared with
/// `dingtalk.rs`, whose sessionWebhook URLs embed session tokens the same way.
pub(crate) fn scrub_reqwest_err(e: reqwest::Error) -> String {
    e.without_url().to_string()
}

/// Whether a callback `timestamp` (Unix **seconds**) is within the replay
/// window around `now_secs`. Fail-closed: any parse failure rejects.
pub(crate) fn wecom_timestamp_fresh(timestamp: &str, now_secs: i64) -> bool {
    let ts: i64 = match timestamp.trim().parse() {
        Ok(t) => t,
        Err(_) => return false,
    };
    (now_secs - ts).abs() <= WECOM_MAX_CLOCK_SKEW_SECS
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── Callback crypto (document/path/90968) ───────────────────────

/// Derive the 32-byte AES key from the 43-char EncodingAESKey.
/// Returns `None` when the key is malformed (fail-closed).
pub(crate) fn derive_aes_key(encoding_aes_key: &str) -> Option<[u8; 32]> {
    if encoding_aes_key.len() != 43 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(format!("{encoding_aes_key}="))
        .ok()?;
    decoded.try_into().ok()
}

/// Compute the WeCom callback signature:
/// `SHA1(concat(sort(token, timestamp, nonce, encrypt)))`, lowercase hex.
pub(crate) fn wecom_msg_signature(
    token: &str,
    timestamp: &str,
    nonce: &str,
    encrypt: &str,
) -> String {
    use sha1::{Digest, Sha1};
    let mut parts = [token, timestamp, nonce, encrypt];
    parts.sort_unstable();
    let mut hasher = Sha1::new();
    for p in parts {
        hasher.update(p.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Constant-time byte comparison (length leak only).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Verify a WeCom callback signature. Fail-closed on any mismatch.
pub(crate) fn verify_wecom_signature(
    token: &str,
    timestamp: &str,
    nonce: &str,
    encrypt: &str,
    signature: &str,
) -> bool {
    let expected = wecom_msg_signature(token, timestamp, nonce, encrypt);
    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

/// Raw AES-256-CBC decrypt (IV = key[0..16]). Input must be a non-empty
/// multiple of the 16-byte block size. PKCS#7 unpadding (1..=32) applied.
fn aes256_cbc_decrypt(key: &[u8; 32], data: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
    use aes::Aes256;

    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut out = Vec::with_capacity(data.len());
    let mut prev: [u8; 16] = key[..16].try_into().ok()?;
    for block in data.chunks_exact(16) {
        let mut b = *GenericArray::from_slice(block);
        cipher.decrypt_block(&mut b);
        for (i, p) in prev.iter().enumerate() {
            b[i] ^= p;
        }
        out.extend_from_slice(&b);
        prev = block.try_into().ok()?;
    }
    // PKCS#7: WeCom pads to 32-byte multiples, so pad byte is 1..=32.
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 32 || pad > out.len() {
        return None;
    }
    // Verify the pad bytes are uniform (reject corrupt/forged padding).
    if out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

/// Raw AES-256-CBC encrypt (IV = key[0..16]) with PKCS#7 padding to
/// 32-byte multiples (WeCom convention). Exercised by the crypto
/// round-trip tests; also the building block for future passive replies.
#[cfg(test)]
fn aes256_cbc_encrypt(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
    use aes::Aes256;

    let pad = 32 - (plain.len() % 32);
    let mut buf = Vec::with_capacity(plain.len() + pad);
    buf.extend_from_slice(plain);
    buf.extend(std::iter::repeat_n(pad as u8, pad));

    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut prev: [u8; 16] = key[..16].try_into().expect("key >= 16 bytes");
    for block in buf.chunks_exact_mut(16) {
        for (i, p) in prev.iter().enumerate() {
            block[i] ^= p;
        }
        let ga = GenericArray::from_mut_slice(block);
        cipher.encrypt_block(ga);
        prev = (*block).try_into().expect("16-byte block");
    }
    buf
}

/// Decrypt a WeCom `Encrypt` payload and verify the trailing receiveid
/// (= corpid for self-built apps). Returns the inner message on success.
/// Every failure path is an `Err` — never fall through to a partial parse.
pub(crate) fn decrypt_wecom_message(
    aes_key: &[u8; 32],
    encrypted_b64: &str,
    expect_receiveid: &str,
) -> Result<String, String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64.trim())
        .map_err(|_| "invalid base64".to_string())?;
    let plain = aes256_cbc_decrypt(aes_key, &data).ok_or("AES decrypt failed")?;
    // random(16B) + msg_len(4B BE) + msg + receiveid
    if plain.len() < 20 {
        return Err("plaintext too short".into());
    }
    let msg_len = u32::from_be_bytes([plain[16], plain[17], plain[18], plain[19]]) as usize;
    let msg_end = 20usize.checked_add(msg_len).ok_or("msg_len overflow")?;
    if msg_end > plain.len() {
        return Err("msg_len out of bounds".into());
    }
    let receiveid = &plain[msg_end..];
    if !constant_time_eq(receiveid, expect_receiveid.as_bytes()) {
        return Err("receiveid mismatch".into());
    }
    String::from_utf8(plain[20..msg_end].to_vec()).map_err(|_| "message not UTF-8".into())
}

/// Encrypt a message into a WeCom `Encrypt` payload (base64). Test-only
/// today (crypto round-trip); becomes the passive-reply envelope if needed.
#[cfg(test)]
pub(crate) fn encrypt_wecom_message(aes_key: &[u8; 32], msg: &str, receiveid: &str) -> String {
    let mut plain = Vec::with_capacity(20 + msg.len() + receiveid.len());
    // 16 random bytes (rand is already a workspace dep).
    let random: [u8; 16] = rand::random();
    plain.extend_from_slice(&random);
    plain.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    plain.extend_from_slice(msg.as_bytes());
    plain.extend_from_slice(receiveid.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(aes256_cbc_encrypt(aes_key, &plain))
}

// ── Minimal XML field extraction ────────────────────────────────
//
// WeCom callbacks are small flat XML documents. Extract fields with a
// tag scanner (CDATA-aware) rather than pulling in an XML crate.

pub(crate) fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let inner = xml[start..end].trim();
    let value = inner
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
        .unwrap_or(inner);
    Some(value.to_string())
}

// ── Shared state ────────────────────────────────────────────────

struct WeComState {
    ctx: Arc<ReplyContext>,
    corp_id: String,
    corp_secret: String,
    agent_id: String,
    callback_token: String,
    aes_key: [u8; 32],
    /// Cached access token (valid ~7200 s; refreshed 5 min early).
    token: RwLock<(String, std::time::Instant)>,
    http: reqwest::Client,
}

impl WeComState {
    async fn get_token(&self, force_refresh: bool) -> Result<String, String> {
        if !force_refresh {
            let cached = self.token.read().await;
            if !cached.0.is_empty() && cached.1.elapsed().as_secs() < 6900 {
                return Ok(cached.0.clone());
            }
        }
        let (token, _expires) =
            fetch_access_token(&self.http, &self.corp_id, &self.corp_secret).await?;
        *self.token.write().await = (token.clone(), std::time::Instant::now());
        info!("WeCom access_token refreshed");
        Ok(token)
    }
}

/// `GET /cgi-bin/gettoken?corpid=&corpsecret=` → (access_token, expires_in).
async fn fetch_access_token(
    http: &reqwest::Client,
    corp_id: &str,
    corp_secret: &str,
) -> Result<(String, u64), String> {
    let resp: serde_json::Value = http
        .get(format!("{WECOM_API}/gettoken"))
        .query(&[("corpid", corp_id), ("corpsecret", corp_secret)])
        .send()
        .await
        .map_err(|e| format!("WeCom gettoken failed: {}", scrub_reqwest_err(e)))?
        .json()
        .await
        .map_err(|e| format!("WeCom gettoken parse failed: {}", scrub_reqwest_err(e)))?;
    let errcode = resp.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
    if errcode != 0 {
        let errmsg = resp
            .get("errmsg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("WeCom gettoken errcode {errcode}: {errmsg}"));
    }
    let token = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("WeCom gettoken: no access_token")?
        .to_string();
    let expires = resp
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(7200);
    Ok((token, expires))
}

// ── Public API ──────────────────────────────────────────────────

/// Create the WeCom webhook router. Returns `None` if not configured.
pub async fn start_wecom_webhook(home_dir: &Path, ctx: Arc<ReplyContext>) -> Option<Router> {
    let corp_id = read_wecom_config(home_dir, "wecom_corp_id")
        .await
        .unwrap_or_default();
    let corp_secret = read_wecom_config(home_dir, "wecom_corp_secret")
        .await
        .unwrap_or_default();
    let agent_id = read_wecom_config(home_dir, "wecom_agent_id")
        .await
        .unwrap_or_default();
    let callback_token = read_wecom_config(home_dir, "wecom_callback_token")
        .await
        .unwrap_or_default();
    let encoding_aes_key = read_wecom_config(home_dir, "wecom_encoding_aes_key")
        .await
        .unwrap_or_default();

    if corp_id.is_empty() || corp_secret.is_empty() {
        return None;
    }

    // Fail-closed: without the callback token + EncodingAESKey the webhook
    // could neither authenticate nor decrypt inbound events. Refuse to start
    // rather than run an open (or broken) endpoint.
    if callback_token.is_empty() || encoding_aes_key.is_empty() {
        error!(
            "WeCom webhook NOT started: wecom_callback_token / wecom_encoding_aes_key unset. \
             Set both in the channel config to authenticate incoming callbacks."
        );
        return None;
    }
    let aes_key = match derive_aes_key(&encoding_aes_key) {
        Some(k) => k,
        None => {
            error!(
                "WeCom webhook NOT started: wecom_encoding_aes_key is not a valid \
                 43-char EncodingAESKey."
            );
            return None;
        }
    };
    if agent_id.is_empty() {
        warn!("WeCom: wecom_agent_id unset — outbound message/send will fail until configured");
    }

    info!("📮 WeCom webhook starting (corp: {corp_id})");

    let state = Arc::new(WeComState {
        ctx,
        corp_id,
        corp_secret,
        agent_id,
        callback_token,
        aes_key,
        token: RwLock::new((String::new(), std::time::Instant::now())),
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default(),
    });

    // Pre-fetch token → dashboard connectivity status.
    match state.get_token(false).await {
        Ok(_) => {
            set_channel_connected(
                &state.ctx.channel_status,
                "wecom",
                true,
                None,
                Some(&state.ctx.event_tx),
            )
            .await;
        }
        Err(e) => {
            warn!("WeCom token error: {e}");
            set_channel_connected(
                &state.ctx.channel_status,
                "wecom",
                false,
                Some(e),
                Some(&state.ctx.event_tx),
            )
            .await;
        }
    }

    Some(
        Router::new()
            .route("/webhook/wecom", get(handle_verify).post(handle_webhook))
            .with_state(state),
    )
}

// ── Webhook handlers ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct VerifyParams {
    #[serde(default)]
    msg_signature: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    echostr: String,
}

/// Callback URL verification: check the signature over `echostr`, decrypt
/// it, and return the inner plaintext verbatim (no quotes, no wrapping).
async fn handle_verify(
    State(state): State<Arc<WeComState>>,
    Query(params): Query<VerifyParams>,
) -> (StatusCode, String) {
    if params.msg_signature.is_empty() || params.echostr.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing params".into());
    }
    // Replay protection: reject stale timestamps (fail-closed; the timestamp
    // is inside the signed tuple, so freshness + signature = sound).
    if !wecom_timestamp_fresh(&params.timestamp, now_secs()) {
        warn!("WeCom URL verification timestamp stale/invalid — rejecting");
        return (StatusCode::UNAUTHORIZED, "stale timestamp".into());
    }
    if !verify_wecom_signature(
        &state.callback_token,
        &params.timestamp,
        &params.nonce,
        &params.echostr,
        &params.msg_signature,
    ) {
        warn!("WeCom URL verification signature mismatch — rejecting");
        return (StatusCode::UNAUTHORIZED, "invalid signature".into());
    }
    match decrypt_wecom_message(&state.aes_key, &params.echostr, &state.corp_id) {
        Ok(plain) => {
            info!("WeCom callback URL verified");
            (StatusCode::OK, plain)
        }
        Err(e) => {
            warn!("WeCom echostr decrypt failed: {e}");
            (StatusCode::BAD_REQUEST, "decrypt failed".into())
        }
    }
}

#[derive(Debug, Deserialize)]
struct CallbackParams {
    #[serde(default)]
    msg_signature: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    nonce: String,
}

/// Message receive: verify signature over the XML `<Encrypt>` payload,
/// decrypt, then ACK immediately (5 s sync window) and reply async.
async fn handle_webhook(
    State(state): State<Arc<WeComState>>,
    Query(params): Query<CallbackParams>,
    body: Bytes,
) -> (StatusCode, String) {
    // Fail-closed: unsigned requests are rejected outright.
    if params.msg_signature.is_empty() {
        return (StatusCode::UNAUTHORIZED, "missing signature".into());
    }
    // Replay protection: a captured (timestamp, nonce, sign, body) tuple can
    // otherwise be replayed forever. Same ±1h window DingTalk enforces.
    if !wecom_timestamp_fresh(&params.timestamp, now_secs()) {
        warn!("WeCom webhook timestamp stale/invalid — rejecting request");
        return (StatusCode::UNAUTHORIZED, "stale timestamp".into());
    }
    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "body not UTF-8".into()),
    };
    let encrypt = match xml_field(body_str, "Encrypt") {
        Some(e) if !e.is_empty() => e,
        _ => return (StatusCode::BAD_REQUEST, "missing Encrypt".into()),
    };
    if !verify_wecom_signature(
        &state.callback_token,
        &params.timestamp,
        &params.nonce,
        &encrypt,
        &params.msg_signature,
    ) {
        warn!("WeCom webhook signature mismatch — rejecting request");
        return (StatusCode::UNAUTHORIZED, "invalid signature".into());
    }
    let msg_xml = match decrypt_wecom_message(&state.aes_key, &encrypt, &state.corp_id) {
        Ok(m) => m,
        Err(e) => {
            warn!("WeCom message decrypt failed: {e}");
            return (StatusCode::BAD_REQUEST, "decrypt failed".into());
        }
    };

    // ACK now (empty body = no passive reply); answer asynchronously.
    let st = state.clone();
    tokio::spawn(async move {
        handle_message(&msg_xml, &st).await;
    });
    (StatusCode::OK, String::new())
}

async fn handle_message(msg_xml: &str, state: &Arc<WeComState>) {
    let msg_type = xml_field(msg_xml, "MsgType").unwrap_or_default();
    if msg_type != "text" {
        return;
    }
    let text = xml_field(msg_xml, "Content").unwrap_or_default();
    let from_user = xml_field(msg_xml, "FromUserName").unwrap_or_default();
    if text.trim().is_empty() || from_user.is_empty() {
        return;
    }

    info!("📩 WeCom [{from_user}]: {}", truncate_bytes(&text, 80));

    let session_id = format!("wecom:{from_user}");

    // Chat commands
    if crate::chat_commands::is_command(&text) {
        if let Some(cmd) = crate::chat_commands::parse_command(&text, None) {
            let agent_id = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent()
                    .map(|a| a.config.agent.name.clone())
                    .unwrap_or_default()
            };
            let reply = crate::chat_commands::handle_command(
                &cmd,
                &state.ctx,
                &session_id,
                &agent_id,
                true,
            )
            .await;
            if !reply.trim().is_empty() {
                send_text(state, &from_user, &reply).await;
            }
            return;
        }
    }

    // Progress callback — WeCom has no typing API; forward tool progress and
    // the TodoUpdate task board as text messages (throttled 45 s; TodoUpdate
    // bypasses the throttle). Same pattern as Feishu.
    let progress_user = from_user.clone();
    let progress_state = state.clone();
    let last_progress = std::sync::Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        if matches!(event, crate::channel_reply::ProgressEvent::Step { .. }) {
            return;
        }
        let is_todo = matches!(
            event,
            crate::channel_reply::ProgressEvent::TodoUpdate { .. }
        );
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            if !is_todo && last.elapsed().as_secs() < 45 {
                return;
            }
            *last = std::time::Instant::now();
        }
        let msg_text = event.to_display();
        let st = progress_state.clone();
        let user = progress_user.clone();
        tokio::spawn(async move {
            send_text(&st, &user, &msg_text).await;
        });
    });

    let reply = build_reply_with_session(
        &text,
        &state.ctx,
        &session_id,
        &from_user,
        Some(on_progress),
    )
    .await;
    if reply.trim().is_empty() {
        warn!(from_user, "WeCom: reply is empty — skipping send");
        return;
    }

    // Rich reply: msgtype markdown (WeCom client renders a markdown subset);
    // fall back to plain text per chunk on rejection.
    for chunk in crate::channel_format::split_text(&reply, WECOM_TEXT_CHUNK) {
        let sent_md = send_wecom_msg(state, &from_user, "markdown", &chunk).await;
        if !sent_md {
            send_text(state, &from_user, &chunk).await;
        }
    }
}

/// Send a text message (chunk-safe helper for progress / fallback paths).
async fn send_text(state: &Arc<WeComState>, touser: &str, text: &str) {
    for chunk in crate::channel_format::split_text(text, WECOM_TEXT_CHUNK) {
        if !send_wecom_msg(state, touser, "text", &chunk).await {
            error!("WeCom send failed (touser: {touser})");
            return;
        }
    }
}

/// `POST /cgi-bin/message/send`. Returns `true` on errcode 0. Retries once
/// with a force-refreshed token on 40014 (invalid) / 42001 (expired).
async fn send_wecom_msg(
    state: &Arc<WeComState>,
    touser: &str,
    msgtype: &str,
    content: &str,
) -> bool {
    for attempt in 0..2 {
        let token = match state.get_token(attempt > 0).await {
            Ok(t) => t,
            Err(e) => {
                error!("WeCom token error: {e}");
                return false;
            }
        };
        let mut body = serde_json::json!({
            "touser": touser,
            "msgtype": msgtype,
            "agentid": state.agent_id.parse::<i64>().unwrap_or(0),
        });
        body[msgtype] = serde_json::json!({ "content": content });
        let resp = state
            .http
            .post(format!("{WECOM_API}/message/send"))
            .query(&[("access_token", token.as_str())])
            .json(&body)
            .send()
            .await;
        match resp {
            Ok(r) => {
                let v: serde_json::Value = match r.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("WeCom send parse error: {}", scrub_reqwest_err(e));
                        return false;
                    }
                };
                match v.get("errcode").and_then(|c| c.as_i64()).unwrap_or(-1) {
                    0 => return true,
                    // 40014 invalid access_token / 42001 access_token expired
                    40014 | 42001 if attempt == 0 => continue,
                    code => {
                        let errmsg = v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("");
                        warn!("WeCom send errcode {code}: {}", truncate_bytes(errmsg, 200));
                        return false;
                    }
                }
            }
            Err(e) => {
                warn!("WeCom send error: {}", scrub_reqwest_err(e));
                return false;
            }
        }
    }
    false
}

/// Delegation-forwarding / proactive send path: reads global config, fetches
/// a fresh access token, and sends `text` chunks to `touser`. Standalone so
/// the dispatcher can forward without a `ReplyContext`.
pub async fn send_text_via_config(home_dir: &Path, touser: &str, text: &str) -> Result<(), String> {
    let corp_id = read_wecom_config(home_dir, "wecom_corp_id")
        .await
        .unwrap_or_default();
    let corp_secret = read_wecom_config(home_dir, "wecom_corp_secret")
        .await
        .unwrap_or_default();
    let agent_id = read_wecom_config(home_dir, "wecom_agent_id")
        .await
        .unwrap_or_default();
    if corp_id.is_empty() || corp_secret.is_empty() || agent_id.is_empty() {
        return Err("wecom_corp_id / wecom_corp_secret / wecom_agent_id not configured".into());
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let (token, _) = fetch_access_token(&http, &corp_id, &corp_secret).await?;
    for (i, chunk) in crate::channel_format::split_text(text, WECOM_TEXT_CHUNK)
        .iter()
        .enumerate()
    {
        let body = serde_json::json!({
            "touser": touser,
            "msgtype": "text",
            "agentid": agent_id.parse::<i64>().unwrap_or(0),
            "text": { "content": chunk },
        });
        let resp: serde_json::Value = http
            .post(format!("{WECOM_API}/message/send"))
            .query(&[("access_token", token.as_str())])
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("wecom send chunk {}: {}", i + 1, scrub_reqwest_err(e)))?
            .json()
            .await
            .map_err(|e| format!("wecom send parse chunk {}: {}", i + 1, scrub_reqwest_err(e)))?;
        let errcode = resp.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
        if errcode != 0 {
            let errmsg = resp
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(format!(
                "WeCom errcode {errcode} on chunk {}: {errmsg}",
                i + 1
            ));
        }
    }
    Ok(())
}

/// Computer Use photo path: upload the PNG as temporary media
/// (`POST /cgi-bin/media/upload?type=image`) and send an image message.
pub async fn send_photo_via_config(
    home_dir: &Path,
    touser: &str,
    png_data: &[u8],
) -> Result<(), String> {
    let corp_id = read_wecom_config(home_dir, "wecom_corp_id")
        .await
        .unwrap_or_default();
    let corp_secret = read_wecom_config(home_dir, "wecom_corp_secret")
        .await
        .unwrap_or_default();
    let agent_id = read_wecom_config(home_dir, "wecom_agent_id")
        .await
        .unwrap_or_default();
    if corp_id.is_empty() || corp_secret.is_empty() || agent_id.is_empty() {
        return Err("wecom_corp_id / wecom_corp_secret / wecom_agent_id not configured".into());
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default();
    let (token, _) = fetch_access_token(&http, &corp_id, &corp_secret).await?;

    // Step 1: upload temporary media → media_id.
    let part = reqwest::multipart::Part::bytes(png_data.to_vec())
        .file_name("screenshot.png")
        .mime_str("image/png")
        .map_err(scrub_reqwest_err)?;
    let form = reqwest::multipart::Form::new().part("media", part);
    let upload: serde_json::Value = http
        .post(format!("{WECOM_API}/media/upload"))
        .query(&[("access_token", token.as_str()), ("type", "image")])
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("wecom media upload: {}", scrub_reqwest_err(e)))?
        .json()
        .await
        .map_err(|e| format!("wecom media upload parse: {}", scrub_reqwest_err(e)))?;
    let errcode = upload.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
    if errcode != 0 {
        let errmsg = upload
            .get("errmsg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("WeCom media upload errcode {errcode}: {errmsg}"));
    }
    let media_id = upload
        .get("media_id")
        .and_then(|v| v.as_str())
        .ok_or("WeCom media upload: no media_id")?;

    // Step 2: send the image message.
    let body = serde_json::json!({
        "touser": touser,
        "msgtype": "image",
        "agentid": agent_id.parse::<i64>().unwrap_or(0),
        "image": { "media_id": media_id },
    });
    let resp: serde_json::Value = http
        .post(format!("{WECOM_API}/message/send"))
        .query(&[("access_token", token.as_str())])
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("wecom sendImage: {}", scrub_reqwest_err(e)))?
        .json()
        .await
        .map_err(|e| format!("wecom sendImage parse: {}", scrub_reqwest_err(e)))?;
    let errcode = resp.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
    if errcode != 0 {
        let errmsg = resp
            .get("errmsg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("WeCom sendImage errcode {errcode}: {errmsg}"));
    }
    Ok(())
}

async fn read_wecom_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A structurally valid EncodingAESKey (43 chars, [a-zA-Z0-9]).
    const TEST_AES_KEY: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP0";
    const TEST_CORP_ID: &str = "wx5823bf96d3bd56c7";

    #[test]
    fn test_derive_aes_key() {
        let key = derive_aes_key(TEST_AES_KEY).expect("43-char key derives");
        assert_eq!(key.len(), 32);
        // Wrong length → fail-closed.
        assert!(derive_aes_key("short").is_none());
        assert!(derive_aes_key(&"a".repeat(44)).is_none());
        // Invalid base64 chars → fail-closed.
        assert!(derive_aes_key(&"!".repeat(43)).is_none());
    }

    /// Known-answer test: signature computed independently with Python
    /// hashlib over the same sorted-concat input.
    /// sorted(["QDG6eK","1409659589","263014780","dGVzdF9lbmNyeXB0ZWRfYmxvYg=="])
    /// → "1409659589" + "263014780" + "QDG6eK" + "dGVzdF9lbmNyeXB0ZWRfYmxvYg=="
    #[test]
    fn test_wecom_signature_known_answer() {
        let sig = wecom_msg_signature(
            "QDG6eK",
            "1409659589",
            "263014780",
            "dGVzdF9lbmNyeXB0ZWRfYmxvYg==",
        );
        assert_eq!(sig, "984793529f986275f6c59d4df8af895ce87251f6");
        assert!(verify_wecom_signature(
            "QDG6eK",
            "1409659589",
            "263014780",
            "dGVzdF9lbmNyeXB0ZWRfYmxvYg==",
            "984793529f986275f6c59d4df8af895ce87251f6",
        ));
        // Tampered signature / token rejected (fail-closed).
        assert!(!verify_wecom_signature(
            "QDG6eK",
            "1409659589",
            "263014780",
            "dGVzdF9lbmNyeXB0ZWRfYmxvYg==",
            "deadbeef",
        ));
        assert!(!verify_wecom_signature(
            "wrong-token",
            "1409659589",
            "263014780",
            "dGVzdF9lbmNyeXB0ZWRfYmxvYg==",
            "984793529f986275f6c59d4df8af895ce87251f6",
        ));
    }

    #[test]
    fn test_crypto_round_trip() {
        let key = derive_aes_key(TEST_AES_KEY).unwrap();
        let msg = "<xml><Content><![CDATA[你好，嘟嘟爪 🐾]]></Content></xml>";
        let encrypted = encrypt_wecom_message(&key, msg, TEST_CORP_ID);
        let decrypted = decrypt_wecom_message(&key, &encrypted, TEST_CORP_ID).unwrap();
        assert_eq!(decrypted, msg);
    }

    #[test]
    fn test_decrypt_rejects_wrong_receiveid() {
        let key = derive_aes_key(TEST_AES_KEY).unwrap();
        let encrypted = encrypt_wecom_message(&key, "hello", TEST_CORP_ID);
        let err = decrypt_wecom_message(&key, &encrypted, "other_corp").unwrap_err();
        assert!(err.contains("receiveid"));
    }

    #[test]
    fn test_decrypt_rejects_tampered_ciphertext() {
        let key = derive_aes_key(TEST_AES_KEY).unwrap();
        let encrypted = encrypt_wecom_message(&key, "hello", TEST_CORP_ID);
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&encrypted)
            .unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xff;
        let tampered = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert!(decrypt_wecom_message(&key, &tampered, TEST_CORP_ID).is_err());
    }

    #[test]
    fn test_decrypt_rejects_garbage() {
        let key = derive_aes_key(TEST_AES_KEY).unwrap();
        assert!(decrypt_wecom_message(&key, "not-base64!!!", TEST_CORP_ID).is_err());
        // Valid base64 but not block-aligned.
        assert!(decrypt_wecom_message(&key, "YWJj", TEST_CORP_ID).is_err());
        // Block-aligned zeros → padding check fails.
        let zeros = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        assert!(decrypt_wecom_message(&key, &zeros, TEST_CORP_ID).is_err());
    }

    // ── LOW-B: replay window ──
    #[test]
    fn test_timestamp_freshness_window() {
        let now = 1_700_000_000i64;
        // Fresh: exact, +30 min, -30 min, boundary ±1h.
        assert!(wecom_timestamp_fresh(&now.to_string(), now));
        assert!(wecom_timestamp_fresh(&(now - 1800).to_string(), now));
        assert!(wecom_timestamp_fresh(&(now + 1800).to_string(), now));
        assert!(wecom_timestamp_fresh(&(now - 3600).to_string(), now));
        // Stale in either direction → reject (replay protection).
        assert!(!wecom_timestamp_fresh(&(now - 3601).to_string(), now));
        assert!(!wecom_timestamp_fresh(&(now + 3601).to_string(), now));
        // Non-numeric / empty → reject (fail-closed).
        assert!(!wecom_timestamp_fresh("not-a-number", now));
        assert!(!wecom_timestamp_fresh("", now));
    }

    // ── HIGH-A: reqwest error scrubbing ──
    #[tokio::test]
    async fn test_scrub_reqwest_err_strips_secret_bearing_url() {
        // Connect to the discard port on loopback — fails fast without any
        // outbound network. The request URL deliberately embeds a secret in
        // the query string, mirroring the real gettoken shape.
        let client = reqwest::Client::new();
        let err = client
            .get("http://127.0.0.1:9/gettoken?corpid=x&corpsecret=SECRET_LEAK_MARKER")
            .timeout(std::time::Duration::from_millis(300))
            .send()
            .await
            .expect_err("request to the discard port must fail");
        let scrubbed = scrub_reqwest_err(err);
        assert!(
            !scrubbed.contains("SECRET_LEAK_MARKER"),
            "scrubbed error must not carry the query secret: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("corpsecret"),
            "scrubbed error must not carry the URL at all: {scrubbed}"
        );
        assert!(!scrubbed.is_empty(), "scrub must keep the error kind text");
    }

    #[test]
    fn test_xml_field_extraction() {
        let xml = "<xml><ToUserName><![CDATA[wx5823bf96d3bd56c7]]></ToUserName>\
                   <Encrypt><![CDATA[abc+def/123==]]></Encrypt>\
                   <AgentID>1000002</AgentID></xml>";
        assert_eq!(
            xml_field(xml, "ToUserName").as_deref(),
            Some("wx5823bf96d3bd56c7")
        );
        assert_eq!(xml_field(xml, "Encrypt").as_deref(), Some("abc+def/123=="));
        assert_eq!(xml_field(xml, "AgentID").as_deref(), Some("1000002"));
        assert_eq!(xml_field(xml, "Missing"), None);
    }

    #[test]
    fn test_decrypted_message_xml_fields() {
        let msg = "<xml><ToUserName><![CDATA[corp]]></ToUserName>\
                   <FromUserName><![CDATA[zhangsan]]></FromUserName>\
                   <MsgType><![CDATA[text]]></MsgType>\
                   <Content><![CDATA[午餐吃什麼？]]></Content>\
                   <MsgId>1234567890123456</MsgId></xml>";
        assert_eq!(xml_field(msg, "MsgType").as_deref(), Some("text"));
        assert_eq!(xml_field(msg, "FromUserName").as_deref(), Some("zhangsan"));
        assert_eq!(xml_field(msg, "Content").as_deref(), Some("午餐吃什麼？"));
    }
}
