//! Microsoft Teams channel — Azure Bot / Bot Framework Connector (raw REST).
//!
//! Inbound: the Bot Framework Connector POSTs Activity JSON to
//! `POST /webhook/teams` with a Connector-signed JWT. Verification is
//! fail-closed: RS256 signature against the Bot Framework JWKS
//! (`login.botframework.com`), `aud` = the bot's App ID, and the token's
//! `serviceUrl` claim must equal the activity's `serviceUrl` (blocks
//! token-redirect attacks). Single-tenant registrations may issue
//! Entra-tenant tokens instead, so a tenant-scoped validation is attempted
//! as a fallback when `tenant_id` is configured.
//!
//! Outbound: client_credentials token from `login.microsoftonline.com`
//! (scope `https://api.botframework.com/.default`; single-tenant uses the
//! tenant-specific endpoint), then `POST {serviceUrl}/v3/conversations/
//! {conversationId}/activities[/{activityId}]`.
//!
//! UX: a `{"type":"typing"}` activity is re-sent every 3 seconds while the
//! reply is generated (Teams shows it ~3s; not rendered in channel posts).
//! Progress events (tool activity / TODO board) post one status activity
//! and then edit it in place via `PUT .../activities/{id}`; it is deleted
//! when the final reply arrives.
//!
//! Formatting: Teams markdown has no tables/headings — `to_teams_markdown`
//! downgrades those (tables → monospace fences, headings → bold).
//!
//! Config (`config.toml [channels]`): `teams_app_id`,
//! `teams_app_password` (`_enc`), `teams_tenant_id` (empty = multi-tenant).

use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use duduclaw_core::truncate_bytes;

use crate::channel_reply::{build_reply_with_session, set_channel_connected, ReplyContext};

const BF_JWKS_URL: &str = "https://login.botframework.com/v1/.well-known/keys";
const BF_ISSUER: &str = "https://api.botframework.com";

/// Teams messages allow ~100 KB, but very long single messages render
/// poorly — chunk at a comfortable display size.
const TEAMS_TEXT_CHUNK: usize = 7000;

pub struct TeamsState {
    pub(crate) ctx: Arc<ReplyContext>,
    creds: TeamsCreds,
}

/// Outbound Connector credentials + token cache — separable from the
/// webhook state so delegation forwarding / Computer Use can send without
/// a `ReplyContext`.
pub struct TeamsCreds {
    app_id: String,
    app_password: String,
    /// Entra tenant ID; empty for multi-tenant bots.
    tenant_id: String,
    /// Cached connector token (access_token, fetched_at).
    token: RwLock<(String, std::time::Instant)>,
    http: reqwest::Client,
}

impl TeamsCreds {
    /// Build from global config; `None` when the channel isn't configured.
    pub(crate) async fn from_config(home_dir: &Path) -> Option<TeamsCreds> {
        let app_id = read_config(home_dir, "teams_app_id").await?;
        let app_password = read_config(home_dir, "teams_app_password").await?;
        if app_id.trim().is_empty() || app_password.trim().is_empty() {
            return None;
        }
        let tenant_id = read_config(home_dir, "teams_tenant_id").await.unwrap_or_default();
        Some(TeamsCreds {
            app_id,
            app_password,
            tenant_id,
            token: RwLock::new((String::new(), std::time::Instant::now())),
            http: reqwest::Client::new(),
        })
    }

    /// Get (or refresh) the outbound Bot Connector token.
    async fn get_token(&self) -> Result<String, String> {
        {
            let cached = self.token.read().await;
            if !cached.0.is_empty() && cached.1.elapsed().as_secs() < 3300 {
                return Ok(cached.0.clone());
            }
        }
        let tenant_segment = if self.tenant_id.trim().is_empty() {
            "botframework.com"
        } else {
            self.tenant_id.trim()
        };
        let url = format!("https://login.microsoftonline.com/{tenant_segment}/oauth2/v2.0/token");
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.app_id.as_str()),
                ("client_secret", self.app_password.as_str()),
                ("scope", "https://api.botframework.com/.default"),
            ])
            .send()
            .await
            .map_err(|e| format!("token request: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("token status {status}: {}", truncate_bytes(&body, 200)));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| format!("token parse: {e}"))?;
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or("no access_token in response")?
            .to_string();
        *self.token.write().await = (token.clone(), std::time::Instant::now());
        Ok(token)
    }
}

// ── Conversation reference store ───────────────────────────────
//
// The Connector base URL (`serviceUrl`) is per-conversation and only
// arrives on inbound activities. Delegation forwarding and the Computer
// Use sender need to reach a conversation later, so every inbound message
// persists `conversation.id → {service_url, bot, user}` — the standard
// Bot Framework "conversation reference" pattern for proactive messages.

const CONV_STORE_FILE: &str = "teams_conversations.json";
/// Cap the store; oldest entries are pruned past this.
const CONV_STORE_CAP: usize = 500;

/// A stored conversation reference.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ConversationRef {
    pub service_url: String,
    pub bot_account: serde_json::Value,
    pub user_account: serde_json::Value,
    pub updated_at: u64,
}

fn conv_store_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join(CONV_STORE_FILE)
}

fn load_conv_store(home_dir: &Path) -> std::collections::HashMap<String, ConversationRef> {
    std::fs::read_to_string(conv_store_path(home_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the conversation store owner-only (`0600`) — it carries per-tenant
/// serviceUrls + account objects. Same pattern as
/// `a2a_signing::write_key_owner_only`: mode applied at `open` time (no
/// `write` → `chmod` window), then re-asserted for pre-existing files.
fn write_store_owner_only(path: &Path, json: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(json.as_bytes())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, json)
    }
}

/// Persist a conversation reference (advisory-locked read-modify-write —
/// the file is shared with future adapters per the repo convention).
fn save_conversation_ref(home_dir: &Path, conversation_id: &str, conv: ConversationRef) {
    let path = conv_store_path(home_dir);
    let cid = conversation_id.to_string();
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut store = load_conv_store(home_dir);
        store.insert(cid.clone(), conv.clone());
        // Prune oldest entries past the cap.
        if store.len() > CONV_STORE_CAP {
            let mut by_age: Vec<(String, u64)> =
                store.iter().map(|(k, v)| (k.clone(), v.updated_at)).collect();
            by_age.sort_by_key(|(_, t)| *t);
            for (k, _) in by_age.into_iter().take(store.len() - CONV_STORE_CAP) {
                store.remove(&k);
            }
        }
        let json = serde_json::to_string(&store).map_err(std::io::Error::other)?;
        write_store_owner_only(&path, &json)
    });
    if let Err(e) = result {
        warn!("Teams: failed to persist conversation reference: {e}");
    }
}

/// Look up a stored conversation reference by conversation id.
pub fn lookup_conversation_ref(home_dir: &Path, conversation_id: &str) -> Option<ConversationRef> {
    load_conv_store(home_dir).get(conversation_id).cloned()
}

/// Send markdown text to a previously-seen conversation (proactive /
/// delegation-forwarding path). Requires a stored conversation reference.
pub async fn send_text_to_conversation(
    home_dir: &Path,
    conversation_id: &str,
    markdown: &str,
) -> Result<(), String> {
    let conv = lookup_conversation_ref(home_dir, conversation_id).ok_or_else(|| {
        format!("no stored conversation reference for {conversation_id} (bot must receive a message there first)")
    })?;
    let creds = TeamsCreds::from_config(home_dir)
        .await
        .ok_or("Teams channel not configured")?;
    let target = TeamsTarget {
        service_url: conv.service_url,
        conversation_id: conversation_id.to_string(),
        reply_to_id: String::new(),
        bot_account: conv.bot_account,
        user_account: conv.user_account,
    };
    let formatted = crate::markdown_render::to_teams_markdown(markdown);
    for chunk in crate::channel_format::split_text(&formatted, TEAMS_TEXT_CHUNK) {
        let body = message_activity(&target, &chunk, false);
        if send_activity(&creds, &target, &body).await.is_none() {
            return Err("Teams send failed".into());
        }
    }
    Ok(())
}

/// Read config and build the Teams webhook router. `None` when unconfigured.
pub async fn start_teams_webhook(home_dir: &Path, ctx: Arc<ReplyContext>) -> Option<Router> {
    let creds = TeamsCreds::from_config(home_dir).await?;
    let state = Arc::new(TeamsState { ctx: ctx.clone(), creds });

    match state.creds.get_token().await {
        Ok(_) => {
            info!("✅ Microsoft Teams webhook ready at /webhook/teams");
            set_channel_connected(&ctx.channel_status, "teams", true, None, Some(&ctx.event_tx)).await;
        }
        Err(e) => {
            warn!("Teams: connector auth failed (webhook still mounted): {e}");
            set_channel_connected(&ctx.channel_status, "teams", false, Some(e), Some(&ctx.event_tx)).await;
        }
    }

    Some(
        Router::new()
            .route("/webhook/teams", post(webhook_handler))
            .with_state(state),
    )
}

async fn read_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

/// Verify the inbound Connector JWT. Tries the Bot Framework issuer first;
/// single-tenant bots may receive Entra-tenant tokens, so a tenant-scoped
/// validation runs as fallback when configured. Fail-closed.
async fn verify_inbound_jwt(state: &TeamsState, token: &str) -> Result<serde_json::Value, String> {
    let creds = &state.creds;
    let bf = crate::webhook_jwt::verify_rs256(
        &creds.http,
        token,
        BF_JWKS_URL,
        BF_ISSUER,
        &creds.app_id,
    )
    .await;
    match bf {
        Ok(claims) => Ok(claims),
        Err(bf_err) => {
            let tid = creds.tenant_id.trim();
            if tid.is_empty() {
                return Err(bf_err);
            }
            // Entra v2 tenant-scoped issuer fallback (single-tenant bots).
            let issuer = format!("https://login.microsoftonline.com/{tid}/v2.0");
            let jwks = format!("https://login.microsoftonline.com/{tid}/discovery/v2.0/keys");
            crate::webhook_jwt::verify_rs256(&creds.http, token, &jwks, &issuer, &creds.app_id)
                .await
                .map_err(|e| format!("botframework: {bf_err}; entra: {e}"))
        }
    }
}

async fn webhook_handler(
    State(state): State<Arc<TeamsState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let Some(token) = crate::webhook_jwt::bearer_token(auth) else {
        warn!("Teams webhook: missing bearer token");
        return StatusCode::UNAUTHORIZED;
    };
    let claims = match verify_inbound_jwt(&state, token).await {
        Ok(c) => c,
        Err(e) => {
            warn!("Teams webhook: JWT verification failed: {e}");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let activity: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Teams webhook parse error: {e}");
            return StatusCode::BAD_REQUEST;
        }
    };

    // serviceUrl claim must match the activity's serviceUrl (fail closed).
    let activity_service_url = activity
        .get("serviceUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(claim_url) = claims.get("serviceurl").or_else(|| claims.get("serviceUrl")).and_then(|v| v.as_str()) {
        // Compare ignoring a single trailing slash.
        if claim_url.trim_end_matches('/') != activity_service_url.trim_end_matches('/') {
            warn!("Teams webhook: serviceUrl claim mismatch");
            return StatusCode::UNAUTHORIZED;
        }
    }
    if !activity_service_url.starts_with("https://") {
        warn!("Teams webhook: non-HTTPS serviceUrl rejected");
        return StatusCode::UNAUTHORIZED;
    }

    if activity.get("type").and_then(|v| v.as_str()) == Some("message") {
        let st = state.clone();
        tokio::spawn(async move { handle_message(&st, &activity).await });
    }
    StatusCode::OK
}

/// Strip `<at>Bot Name</at>` mention markup that Teams embeds in channel
/// messages that @mention the bot.
fn strip_mention_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<at>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</at>") {
            Some(end_rel) => rest = &rest[start + end_rel + 5..],
            None => {
                rest = &rest[start + 4..];
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

async fn handle_message(state: &Arc<TeamsState>, activity: &serde_json::Value) {
    let raw_text = activity.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let text = strip_mention_tags(raw_text);
    if text.is_empty() {
        return;
    }

    let service_url = activity
        .get("serviceUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    let conversation_id = activity
        .pointer("/conversation/id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if service_url.is_empty() || conversation_id.is_empty() {
        warn!("Teams: message activity missing serviceUrl/conversation.id");
        return;
    }
    let activity_id = activity.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let sender_name = activity
        .pointer("/from/name")
        .and_then(|v| v.as_str())
        .unwrap_or("someone")
        .to_string();
    let sender_id = activity
        .pointer("/from/id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    // Swap from/recipient for outbound activities.
    let bot_account = activity.get("recipient").cloned().unwrap_or_default();
    let user_account = activity.get("from").cloned().unwrap_or_default();

    info!("📩 Teams [{sender_name}]: {}", truncate_bytes(&text, 80));

    let target = TeamsTarget {
        service_url,
        conversation_id,
        reply_to_id: activity_id,
        bot_account,
        user_account,
    };

    // Persist the conversation reference so proactive sends (delegation
    // forwarding, Computer Use) can reach this conversation later.
    save_conversation_ref(
        &state.ctx.home_dir,
        &target.conversation_id,
        ConversationRef {
            service_url: target.service_url.clone(),
            bot_account: target.bot_account.clone(),
            user_account: target.user_account.clone(),
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        },
    );

    // ── Typing indicator (Teams renders ~3s; refresh every 3s) ──
    let typing_state = state.clone();
    let typing_target = target.clone();
    let typing_guard = crate::channel_typing::TypingGuard::start(
        std::time::Duration::from_secs(3),
        move || {
            let st = typing_state.clone();
            let tg = typing_target.clone();
            async move {
                let body = serde_json::json!({
                    "type": "typing",
                    "from": tg.bot_account,
                    "recipient": tg.user_account,
                    "conversation": { "id": tg.conversation_id },
                });
                let _ = send_activity(&st.creds, &tg, &body).await;
            }
        },
    );

    // ── Progress: post one status activity, then edit it in place ──
    let progress_state = state.clone();
    let progress_target = target.clone();
    let progress_activity_id: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let progress_cleanup = progress_activity_id.clone();
    let last_progress = Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        // Step / ModelInfo events are dashboard-only signals — never rendered
        // as channel text (would be an empty message).
        if matches!(
            event,
            crate::channel_reply::ProgressEvent::Step { .. }
                | crate::channel_reply::ProgressEvent::ModelInfo { .. }
        ) {
            return;
        }
        let is_todo = matches!(event, crate::channel_reply::ProgressEvent::TodoUpdate { .. });
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            if !is_todo && last.elapsed().as_secs() < 30 {
                return;
            }
            *last = std::time::Instant::now();
        }
        let st = progress_state.clone();
        let tg = progress_target.clone();
        let aid = progress_activity_id.clone();
        let msg_text = event.to_display();
        tokio::spawn(async move {
            let mut guard = aid.lock().await;
            let body = message_activity(&tg, &msg_text, false);
            match guard.as_deref() {
                Some(existing) => update_activity(&st.creds, &tg, existing, &body).await,
                None => *guard = send_activity(&st.creds, &tg, &body).await,
            }
        });
    });

    // ── Chat commands ──
    let session_id = format!("teams:{}", target.conversation_id);
    if crate::chat_commands::is_command(&text) {
        if let Some(cmd) = crate::chat_commands::parse_command(&text, None) {
            let agent_id = {
                let reg = state.ctx.registry.read().await;
                reg.main_agent().map(|a| a.config.agent.name.clone()).unwrap_or_default()
            };
            let reply =
                crate::chat_commands::handle_command(&cmd, &state.ctx, &session_id, &agent_id, true).await;
            drop(typing_guard);
            deliver_reply(&state.creds, &target, &reply).await;
            return;
        }
    }

    let reply = build_reply_with_session(&text, &state.ctx, &session_id, &sender_id, Some(on_progress)).await;
    drop(typing_guard);

    // Remove the interim progress activity — the final reply supersedes it.
    if let Some(aid) = progress_cleanup.lock().await.take() {
        delete_activity(&state.creds, &target, &aid).await;
    }

    if reply.trim().is_empty() {
        warn!("Teams: reply is empty — skipping send");
        return;
    }
    deliver_reply(&state.creds, &target, &reply).await;
}

/// Outbound delivery coordinates for one conversation.
#[derive(Clone)]
struct TeamsTarget {
    service_url: String,
    conversation_id: String,
    reply_to_id: String,
    bot_account: serde_json::Value,
    user_account: serde_json::Value,
}

/// Build a markdown message activity.
fn message_activity(target: &TeamsTarget, text: &str, reply: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "type": "message",
        "textFormat": "markdown",
        "text": text,
        "from": target.bot_account,
        "recipient": target.user_account,
        "conversation": { "id": target.conversation_id },
    });
    if reply && !target.reply_to_id.is_empty() {
        body["replyToId"] = serde_json::json!(target.reply_to_id);
    }
    body
}

/// Render markdown for Teams and send, chunked.
async fn deliver_reply(creds: &TeamsCreds, target: &TeamsTarget, reply_markdown: &str) {
    let formatted = crate::markdown_render::to_teams_markdown(reply_markdown);
    for (i, chunk) in crate::channel_format::split_text(&formatted, TEAMS_TEXT_CHUNK)
        .iter()
        .enumerate()
    {
        let body = message_activity(target, chunk, i == 0);
        send_activity(creds, target, &body).await;
    }
}

/// POST an activity; returns the created activity id.
async fn send_activity(
    creds: &TeamsCreds,
    target: &TeamsTarget,
    body: &serde_json::Value,
) -> Option<String> {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Teams token error: {e}");
            return None;
        }
    };
    let url = format!(
        "{}/v3/conversations/{}/activities",
        target.service_url, target.conversation_id
    );
    match creds.http.post(&url).bearer_auth(&token).json(body).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string())),
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            error!("Teams send failed ({status}): {}", truncate_bytes(&text, 200));
            None
        }
        Err(e) => {
            error!("Teams send error: {e}");
            None
        }
    }
}

/// PUT — edit an existing activity in place.
async fn update_activity(
    creds: &TeamsCreds,
    target: &TeamsTarget,
    activity_id: &str,
    body: &serde_json::Value,
) {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Teams token error: {e}");
            return;
        }
    };
    let url = format!(
        "{}/v3/conversations/{}/activities/{}",
        target.service_url, target.conversation_id, activity_id
    );
    if let Err(e) = creds.http.put(&url).bearer_auth(&token).json(body).send().await {
        warn!("Teams update error: {e}");
    }
}

/// DELETE an activity (used to clean up the progress message).
async fn delete_activity(creds: &TeamsCreds, target: &TeamsTarget, activity_id: &str) {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Teams token error: {e}");
            return;
        }
    };
    let url = format!(
        "{}/v3/conversations/{}/activities/{}",
        target.service_url, target.conversation_id, activity_id
    );
    let _ = creds.http.delete(&url).bearer_auth(&token).send().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_ref_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert!(lookup_conversation_ref(home, "a:1abc").is_none());
        save_conversation_ref(
            home,
            "a:1abc",
            ConversationRef {
                service_url: "https://smba.trafficmanager.net/amer".into(),
                bot_account: serde_json::json!({"id": "28:bot"}),
                user_account: serde_json::json!({"id": "29:user"}),
                updated_at: 100,
            },
        );
        let got = lookup_conversation_ref(home, "a:1abc").expect("stored ref");
        assert_eq!(got.service_url, "https://smba.trafficmanager.net/amer");
        assert_eq!(got.user_account["id"], "29:user");
    }

    #[test]
    fn conversation_store_prunes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        for i in 0..(CONV_STORE_CAP + 10) {
            save_conversation_ref(
                home,
                &format!("conv-{i}"),
                ConversationRef {
                    service_url: "https://x".into(),
                    bot_account: serde_json::json!({}),
                    user_account: serde_json::json!({}),
                    updated_at: i as u64,
                },
            );
        }
        let store = load_conv_store(home);
        assert!(store.len() <= CONV_STORE_CAP);
        // Oldest entries pruned; newest kept.
        assert!(store.contains_key(&format!("conv-{}", CONV_STORE_CAP + 9)));
        assert!(!store.contains_key("conv-0"));
    }

    /// LOW-A: the conversation store carries tokened serviceUrls — owner-only.
    #[cfg(unix)]
    #[test]
    fn conv_store_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Pre-create with loose perms to prove they get re-asserted to 0600.
        let store = conv_store_path(home);
        std::fs::write(&store, "{}").unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_conversation_ref(
            home,
            "conv-perm",
            ConversationRef {
                service_url: "https://smba.trafficmanager.net/amer".into(),
                bot_account: serde_json::json!({"id": "28:bot"}),
                user_account: serde_json::json!({"id": "29:user"}),
                updated_at: 1,
            },
        );
        let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "conversation store must be owner-only, got {mode:o}");
    }

    #[test]
    fn mention_tags_stripped() {
        assert_eq!(strip_mention_tags("<at>DuDu</at> 你好"), "你好");
        assert_eq!(strip_mention_tags("hello"), "hello");
        assert_eq!(strip_mention_tags("<at>Bot</at>"), "");
    }

    #[test]
    fn message_activity_shape() {
        let target = TeamsTarget {
            service_url: "https://smba.trafficmanager.net/amer".into(),
            conversation_id: "a:1".into(),
            reply_to_id: "42".into(),
            bot_account: serde_json::json!({"id": "28:bot"}),
            user_account: serde_json::json!({"id": "29:user"}),
        };
        let m = message_activity(&target, "hi", true);
        assert_eq!(m["type"], "message");
        assert_eq!(m["textFormat"], "markdown");
        assert_eq!(m["replyToId"], "42");
        assert_eq!(m["conversation"]["id"], "a:1");
    }
}
