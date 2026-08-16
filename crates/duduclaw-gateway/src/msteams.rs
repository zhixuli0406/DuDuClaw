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
///
/// Credentials doctrine P2 (WP-8A): `app_id` / `app_password` / `tenant_id`
/// are the values this handle was constructed with. The values actually used
/// to verify inbound JWTs and to obtain outbound tokens are re-read fresh
/// from `home_dir` — every request for inbound verification (security gate,
/// see `verify_inbound_jwt`), every refresh cycle for the outbound token
/// (see `get_token`) — so a rotated App Secret (or App ID / tenant change)
/// takes effect without restarting the gateway. The cached outbound `token`
/// itself keeps its TTL — a genuinely network-derived, short-lived session
/// credential, which the doctrine's TTL-with-invalidation allowance (design
/// §2.4) covers.
pub struct TeamsCreds {
    home_dir: std::path::PathBuf,
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
            home_dir: home_dir.to_path_buf(),
            app_id,
            app_password,
            tenant_id,
            token: RwLock::new((String::new(), std::time::Instant::now())),
            // 30s request timeout like every other channel client. A bare
            // `Client::new()` has NO request timeout, and `get_token()` is
            // awaited directly on the gateway boot path — an unresponsive
            // login.microsoftonline.com would stall the whole boot sequence
            // (heartbeat/cron/tick never start) with zero diagnostics.
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        })
    }

    /// Re-read `app_id` / `app_password` / `tenant_id` fresh from config.
    /// Falls back to the values this handle was constructed with when the
    /// current config can't be read (transient I/O) — outbound credentials
    /// fail open to "try with the last known secret" rather than break every
    /// send; the inbound gate (`verify_inbound_jwt`) fails closed instead.
    async fn resolve_fresh(&self) -> (String, String, String) {
        let app_id = read_config(&self.home_dir, "teams_app_id").await;
        let app_password = read_config(&self.home_dir, "teams_app_password").await;
        match (app_id, app_password) {
            (Some(id), Some(pw)) if !id.trim().is_empty() && !pw.trim().is_empty() => {
                let tenant_id = read_config(&self.home_dir, "teams_tenant_id")
                    .await
                    .unwrap_or_default();
                (id, pw, tenant_id)
            }
            _ => (
                self.app_id.clone(),
                self.app_password.clone(),
                self.tenant_id.clone(),
            ),
        }
    }

    /// Get (or refresh) the outbound Bot Connector token.
    async fn get_token(&self) -> Result<String, String> {
        {
            let cached = self.token.read().await;
            if !cached.0.is_empty() && cached.1.elapsed().as_secs() < 3300 {
                return Ok(cached.0.clone());
            }
        }
        // WP-8A: re-read fresh at refresh time rather than trust the values
        // captured at construction.
        let (app_id, app_password, tenant_id) = self.resolve_fresh().await;
        let tenant_segment = if tenant_id.trim().is_empty() {
            "botframework.com"
        } else {
            tenant_id.trim()
        };
        let url = format!("https://login.microsoftonline.com/{tenant_segment}/oauth2/v2.0/token");
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", app_id.as_str()),
                ("client_secret", app_password.as_str()),
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

    // ── W2-7 deep-link coordinates ──────────────────────────
    // Teams' `/l/channel/...` deep link needs the Team's Office 365 group id
    // and the channel's display name, neither of which every activity
    // carries — `channelData` reliably includes `team.aadGroupId` /
    // `tenant.id` for channel messages but Teams does not consistently send
    // the channel's display name on every activity. `#[serde(default)]` so
    // every reference persisted before this field existed (and every 1:1
    // chat, which has no team/channel at all) deserializes to `None` rather
    // than failing — honest gap, never fabricated (see `channel_link.rs`).
    #[serde(default)]
    pub teams_group_id: Option<String>,
    #[serde(default)]
    pub teams_channel_name: Option<String>,
    #[serde(default)]
    pub teams_tenant_id: Option<String>,
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

/// W2-7: best-effort extraction of Teams deep-link coordinates from an
/// inbound activity's `channelData`. Channel messages reliably carry
/// `team.aadGroupId` (the O365 group id the deep link's `groupId=` param
/// needs) and `tenant.id`; the channel's own display name is NOT
/// consistently present on every activity (Teams only sometimes includes
/// it), and 1:1 chat activities carry no `team`/`channel` block at all — a
/// missing field maps to `None` here, never guessed at.
fn extract_teams_coords(activity: &serde_json::Value) -> (Option<String>, Option<String>, Option<String>) {
    let non_empty = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let group_id = activity.pointer("/channelData/team/aadGroupId").and_then(non_empty);
    let channel_name = activity.pointer("/channelData/channel/name").and_then(non_empty);
    let tenant_id = activity
        .pointer("/channelData/tenant/id")
        .or_else(|| activity.pointer("/conversation/tenantId"))
        .and_then(non_empty);
    (group_id, channel_name, tenant_id)
}

/// Send markdown text to a previously-seen conversation (proactive /
/// delegation-forwarding path). Requires a stored conversation reference.
pub async fn send_text_to_conversation(
    home_dir: &Path,
    conversation_id: &str,
    markdown: &str,
) -> Result<(), String> {
    send_text_to_conversation_with_id(home_dir, conversation_id, markdown).await.map(|_| ())
}

/// Like [`send_text_to_conversation`] but returns the FIRST chunk's activity
/// id — the message head a user quotes when replying, which is how
/// text-verdict decisions (WP1.6) find their card. `Ok(None)` only when the
/// platform response carried no id (delivered regardless).
pub(crate) async fn send_text_to_conversation_with_id(
    home_dir: &Path,
    conversation_id: &str,
    markdown: &str,
) -> Result<Option<String>, String> {
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
    let mut first_id: Option<String> = None;
    for chunk in crate::channel_format::split_text(&formatted, TEAMS_TEXT_CHUNK) {
        match send_activity(&creds, &target, &message_activity(&target, &chunk, false)).await {
            Some(id) => {
                if first_id.is_none() {
                    first_id = Some(id);
                }
            }
            None => return Err("Teams send failed".into()),
        }
    }
    Ok(first_id)
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
///
/// WP-8A: `app_id` / `tenant_id` are re-read fresh from config for every
/// request rather than trusting `state.creds`'s construction-time values —
/// this is the inbound security gate, so unlike the outbound token refresh
/// it fails closed (rejects) if the current config can't be read, instead of
/// silently falling back to a possibly-stale App ID.
async fn verify_inbound_jwt(state: &TeamsState, token: &str) -> Result<serde_json::Value, String> {
    let creds = &state.creds;
    let app_id = read_config(&state.ctx.home_dir, "teams_app_id")
        .await
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "teams_app_id not configured".to_string())?;
    let tenant_id = read_config(&state.ctx.home_dir, "teams_tenant_id")
        .await
        .unwrap_or_default();
    let bf =
        crate::webhook_jwt::verify_rs256(&creds.http, token, BF_JWKS_URL, BF_ISSUER, &app_id).await;
    match bf {
        Ok(claims) => Ok(claims),
        Err(bf_err) => {
            let tid = tenant_id.trim();
            if tid.is_empty() {
                return Err(bf_err);
            }
            // Entra v2 tenant-scoped issuer fallback (single-tenant bots).
            let issuer = format!("https://login.microsoftonline.com/{tid}/v2.0");
            let jwks = format!("https://login.microsoftonline.com/{tid}/discovery/v2.0/keys");
            crate::webhook_jwt::verify_rs256(&creds.http, token, &jwks, &issuer, &app_id)
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

/// Strip HTML tags and decode the handful of entities Teams emits, keeping
/// text content only. Tag boundaries become spaces so adjacent block elements
/// don't glue words together.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Quoted-reply context from a Teams message activity. When a user quotes a
/// message, Teams delivers the quoted content inside an `attachments` item
/// with `contentType == "text/html"` as a `<blockquote>` — `activity.text`
/// carries only the user's new words. Without this the quote is silently
/// dropped.
fn teams_quoted_context(activity: &serde_json::Value) -> Option<String> {
    let arr = activity.get("attachments")?.as_array()?;
    for a in arr {
        let ctype = a.get("contentType").and_then(|v| v.as_str()).unwrap_or("");
        if ctype != "text/html" {
            continue;
        }
        let html = a.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let Some(open) = html.find("<blockquote") else { continue };
        let Some(tag_end) = html[open..].find('>') else { continue };
        let body_start = open + tag_end + 1;
        let Some(body_len) = html[body_start..].find("</blockquote>") else { continue };
        let quote = html_to_text(&html[body_start..body_start + body_len]);
        if !quote.is_empty() {
            return Some(crate::channel_format::format_quoted_context(
                "對話中先前的訊息",
                &quote,
            ));
        }
    }
    None
}

/// WP1.6: the quoted activity id of a Teams quoted reply. Teams embeds the
/// quote as a `text/html` attachment whose `<blockquote>` open tag carries
/// `itemid="<activity id>"` — the id of the message being replied to.
fn teams_quoted_reply_id(activity: &serde_json::Value) -> Option<String> {
    let arr = activity.get("attachments")?.as_array()?;
    for a in arr {
        if a.get("contentType").and_then(|v| v.as_str()) != Some("text/html") {
            continue;
        }
        let html = a.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let Some(open) = html.find("<blockquote") else { continue };
        let Some(tag_end) = html[open..].find('>') else { continue };
        let tag = &html[open..open + tag_end];
        let Some(id_pos) = tag.find("itemid=\"") else { continue };
        let rest = &tag[id_pos + "itemid=\"".len()..];
        let Some(end) = rest.find('"') else { continue };
        let id = rest[..end].trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

async fn handle_message(state: &Arc<TeamsState>, activity: &serde_json::Value) {
    let raw_text = activity.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let text = strip_mention_tags(raw_text);
    // WP1.3: collect Teams file attachments (file.download.info carries a
    // pre-authenticated `downloadUrl`, fetchable without a bearer token).
    let file_attachments: Vec<(String, String)> = activity
        .get("attachments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let ctype = a.get("contentType").and_then(|v| v.as_str()).unwrap_or("");
                    if ctype == "application/vnd.microsoft.teams.file.download.info" {
                        let url = a.pointer("/content/downloadUrl").and_then(|v| v.as_str())?;
                        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("file");
                        Some((name.to_string(), url.to_string()))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if text.is_empty() && file_attachments.is_empty() {
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

    // W2-7: best-effort deep-link coordinates from this activity's
    // `channelData`. Merged against whatever was already stored so a later
    // activity that doesn't carry `channelData` (a bare follow-up message,
    // or Teams simply not sending it that time) never regresses an
    // already-known coordinate back to `None`.
    let (mut teams_group_id, mut teams_channel_name, mut teams_tenant_id) = extract_teams_coords(activity);
    if let Some(existing) = lookup_conversation_ref(&state.ctx.home_dir, &target.conversation_id) {
        teams_group_id = teams_group_id.or(existing.teams_group_id);
        teams_channel_name = teams_channel_name.or(existing.teams_channel_name);
        teams_tenant_id = teams_tenant_id.or(existing.teams_tenant_id);
    }

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
            teams_group_id,
            teams_channel_name,
            teams_tenant_id,
        },
    );

    // WP1.6 (ecosystem): quoted-replying to a decision card with a bare
    // verdict（「同意」／「拒絕」…）counts as pressing its button — watch and
    // mobile surfaces render the card text but not always the buttons. Same
    // dispatch (auth + idempotency + accounting) as a physical press;
    // anything that isn't a whole-message verdict on a live card falls
    // through to normal chat. Channel-thread replies carry `;messageid=…` on
    // `conversation.id` while the card was recorded under the base id, so
    // both forms are tried (the quoted activity id pins the exact card).
    if !text.is_empty() {
        if let Some(quoted_id) = teams_quoted_reply_id(activity) {
            let base_conv =
                target.conversation_id.split(';').next().unwrap_or("").to_string();
            let mut outcome = crate::decision_text::route_text_reply(
                &state.ctx.home_dir,
                "teams",
                &sender_id,
                &target.conversation_id,
                &quoted_id,
                &text,
            )
            .await;
            if outcome.is_none() && base_conv != target.conversation_id {
                outcome = crate::decision_text::route_text_reply(
                    &state.ctx.home_dir,
                    "teams",
                    &sender_id,
                    &base_conv,
                    &quoted_id,
                    &text,
                )
                .await;
            }
            if let Some(result) = outcome {
                let ack = match result {
                    Ok(m) => m,
                    Err(e) => format!("⚠ {e}"),
                };
                let body = message_activity(&target, &ack, true);
                let _ = send_activity(&state.creds, &target, &body).await;
                return;
            }
        }
    }

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
            let throttle = crate::channel_capabilities::progress_throttle_secs("teams").unwrap_or(30);
            if !is_todo && last.elapsed().as_secs() < throttle {
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
            let reply = crate::chat_commands::handle_command(
                &cmd, &state.ctx, &session_id, &agent_id, true, &sender_id,
            )
            .await;
            drop(typing_guard);
            deliver_reply(&state.creds, &target, &reply).await;
            return;
        }
    }

    // WP1.3: download inbound file attachments to the resolved agent's dir.
    let mut attachment_lines: Vec<String> = Vec::new();
    if !file_attachments.is_empty() {
        let attach_base =
            crate::channel_reply::resolve_attachment_base(state.ctx.as_ref(), None).await;
        for (name, url) in &file_attachments {
            match crate::media::download_url(
                &state.ctx.http, url, None, crate::media::MAX_FILE_SIZE as usize,
            )
            .await
            {
                Ok(bytes) => match crate::media::save_attachment_in_base(&attach_base, &bytes, name).await {
                    Ok(path) => attachment_lines.push(crate::media::format_attachment_ref(
                        &crate::media::MediaType::File, name, &path,
                    )),
                    Err(e) => warn!("Teams: failed to save attachment {name}: {e}"),
                },
                Err(e) => warn!("Teams: failed to download attachment {name}: {e}"),
            }
        }
    }
    // ── Quoted-reply context ──
    let text = match teams_quoted_context(activity) {
        Some(quote_block) if text.is_empty() => quote_block,
        Some(quote_block) => format!("{quote_block}\n{text}"),
        None => text,
    };

    let input_text = if attachment_lines.is_empty() {
        text.clone()
    } else if text.is_empty() {
        attachment_lines.join("\n")
    } else {
        format!("{text}\n\n{}", attachment_lines.join("\n"))
    };

    let reply = build_reply_with_session(&input_text, &state.ctx, &session_id, &sender_id, Some(on_progress)).await;
    drop(typing_guard);

    // WP1.3: 📎DELIVER: — Teams file upload is not wired, so the sender's
    // default `send_document` degrades to a text notice (→ dashboard Files
    // panel) via the persisted conversation reference; the marker is stripped.
    let reply = {
        let doc_sender = crate::channel_sender::create_teams_sender(
            state.ctx.home_dir.clone(),
            target.conversation_id.clone(),
            sender_id.clone(),
        );
        crate::channel_reply::deliver_documents_for_reply(
            state.ctx.as_ref(), None, reply, doc_sender.as_ref(),
        ).await
    };

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

    // ── W2-7: extract_teams_coords ───────────────────────────

    #[test]
    fn extract_teams_coords_reads_channel_message_channel_data() {
        let activity = serde_json::json!({
            "channelData": {
                "team": { "id": "19:xxxx@thread.tacv2", "aadGroupId": "grp-1" },
                "channel": { "id": "19:xxxx@thread.tacv2", "name": "General" },
                "tenant": { "id": "tenant-1" }
            }
        });
        assert_eq!(
            extract_teams_coords(&activity),
            (Some("grp-1".to_string()), Some("General".to_string()), Some("tenant-1".to_string()))
        );
    }

    #[test]
    fn extract_teams_coords_falls_back_to_conversation_tenant_id() {
        let activity = serde_json::json!({
            "conversation": { "id": "conv-1", "tenantId": "tenant-2" }
        });
        assert_eq!(extract_teams_coords(&activity), (None, None, Some("tenant-2".to_string())));
    }

    #[test]
    fn extract_teams_coords_missing_channel_data_is_all_none() {
        // A 1:1 chat activity — no `team`/`channel` block at all. Must not
        // fabricate anything.
        let activity = serde_json::json!({ "conversation": { "id": "a:1abc" } });
        assert_eq!(extract_teams_coords(&activity), (None, None, None));
    }

    #[test]
    fn extract_teams_coords_treats_empty_strings_as_absent() {
        let activity = serde_json::json!({
            "channelData": {
                "team": { "aadGroupId": "" },
                "channel": { "name": "" },
                "tenant": { "id": "" }
            }
        });
        assert_eq!(extract_teams_coords(&activity), (None, None, None));
    }

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
                teams_group_id: None,
                teams_channel_name: None,
                teams_tenant_id: None,
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
                    teams_group_id: None,
                    teams_channel_name: None,
                    teams_tenant_id: None,
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
                teams_group_id: None,
                teams_channel_name: None,
                teams_tenant_id: None,
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

    // ── WP-8A / credentials doctrine P2 ─────────────────────────────────

    async fn write_config(home: &Path, body: &str) {
        tokio::fs::write(home.join("config.toml"), body).await.unwrap();
    }

    #[tokio::test]
    async fn from_config_reads_app_id_password_and_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_config(
            home,
            "[channels]\nteams_app_id = \"app-1\"\nteams_app_password = \"secret-1\"\nteams_tenant_id = \"tenant-1\"\n",
        )
        .await;
        let creds = TeamsCreds::from_config(home).await.expect("configured");
        assert_eq!(creds.app_id, "app-1");
        assert_eq!(creds.app_password, "secret-1");
        assert_eq!(creds.tenant_id, "tenant-1");
    }

    #[tokio::test]
    async fn from_config_is_none_without_app_id_or_password() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_config(home, "[channels]\nteams_app_id = \"app-1\"\n").await;
        assert!(TeamsCreds::from_config(home).await.is_none(), "missing password");

        write_config(home, "[channels]\nteams_app_password = \"secret-1\"\n").await;
        assert!(TeamsCreds::from_config(home).await.is_none(), "missing app_id");
    }

    /// `resolve_fresh` is what `get_token`'s refresh path (WP-8A) calls
    /// instead of trusting the fields captured at construction — this pins
    /// down that a rotated App Secret is picked up without any caching, and
    /// that it falls back to the construction-time values only when the
    /// config can no longer be read at all (not merely when one of the two
    /// required fields is blank — that still counts as "rotated to nothing
    /// configured", which falls back exactly the same way, matching the
    /// documented "fail open to last known secret" outbound posture).
    #[tokio::test]
    async fn resolve_fresh_reflects_a_rotated_app_password_without_any_cache() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_config(
            home,
            "[channels]\nteams_app_id = \"app-1\"\nteams_app_password = \"original-secret\"\n",
        )
        .await;
        let creds = TeamsCreds::from_config(home).await.expect("configured");
        let (_, pw, _) = creds.resolve_fresh().await;
        assert_eq!(pw, "original-secret");

        write_config(
            home,
            "[channels]\nteams_app_id = \"app-1\"\nteams_app_password = \"rotated-secret\"\n",
        )
        .await;
        let (_, pw2, _) = creds.resolve_fresh().await;
        assert_eq!(
            pw2, "rotated-secret",
            "resolve_fresh must see the rotated secret, not the construction-time value"
        );
    }

    #[tokio::test]
    async fn resolve_fresh_falls_back_to_construction_time_values_when_config_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_config(
            home,
            "[channels]\nteams_app_id = \"app-1\"\nteams_app_password = \"original-secret\"\n",
        )
        .await;
        let creds = TeamsCreds::from_config(home).await.expect("configured");

        // Simulate config.toml becoming unreadable (e.g. deleted mid-flight)
        // — an outbound credential fails open to the last known secret
        // rather than breaking every send.
        tokio::fs::remove_file(home.join("config.toml")).await.unwrap();
        let (id, pw, _) = creds.resolve_fresh().await;
        assert_eq!(id, "app-1");
        assert_eq!(pw, "original-secret");
    }
}

#[cfg(test)]
mod quoted_context_tests {
    use super::*;

    #[test]
    fn blockquote_in_html_attachment_is_extracted() {
        let activity = serde_json::json!({
            "text": "那這件事處理了嗎",
            "attachments": [{
                "contentType": "text/html",
                "content": "<div><blockquote itemtype=\"http://schema.skype.com/Reply\"><strong>Amy</strong><p>請記得下午三點前回覆客戶&nbsp;A</p></blockquote><p>那這件事處理了嗎</p></div>"
            }]
        });
        let block = teams_quoted_context(&activity).expect("quote block");
        assert!(block.contains("回覆客戶 A"));
        assert!(block.contains("〔引用訊息"));
    }

    #[test]
    fn html_without_blockquote_yields_none() {
        let activity = serde_json::json!({
            "text": "hi",
            "attachments": [{ "contentType": "text/html", "content": "<p>hi</p>" }]
        });
        assert!(teams_quoted_context(&activity).is_none());
        assert!(teams_quoted_context(&serde_json::json!({"text": "x"})).is_none());
    }

    #[test]
    fn html_to_text_strips_tags_and_decodes_entities() {
        assert_eq!(html_to_text("<p>a&nbsp;&amp;&nbsp;b</p>"), "a & b");
        assert_eq!(html_to_text("<strong>粗體</strong>文字"), "粗體 文字");
    }

    // ── WP1.6: quoted-reply id extraction (text-verdict decisions) ──

    #[test]
    fn quoted_reply_id_comes_from_blockquote_itemid() {
        // Realistic Teams quoted-reply attachment shape: the <blockquote>
        // open tag carries itemtype + itemid (the quoted activity id).
        let activity = serde_json::json!({
            "text": "同意",
            "attachments": [{
                "contentType": "text/html",
                "content": "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" \
                            itemid=\"1755083112345\"><strong>DuDuClaw</strong>\
                            <p>需要你的決定…</p></blockquote><p>同意</p>"
            }]
        });
        assert_eq!(teams_quoted_reply_id(&activity).as_deref(), Some("1755083112345"));
    }

    #[test]
    fn quoted_reply_id_absent_or_malformed_yields_none() {
        // No attachments at all.
        assert!(teams_quoted_reply_id(&serde_json::json!({"text": "同意"})).is_none());
        // HTML without a blockquote (plain formatted message).
        let plain = serde_json::json!({
            "attachments": [{ "contentType": "text/html", "content": "<p>同意</p>" }]
        });
        assert!(teams_quoted_reply_id(&plain).is_none());
        // Blockquote without an itemid (not a reply quote).
        let no_id = serde_json::json!({
            "attachments": [{ "contentType": "text/html",
                              "content": "<blockquote><p>引文</p></blockquote>" }]
        });
        assert!(teams_quoted_reply_id(&no_id).is_none());
        // Empty itemid is refused, not returned as an empty key.
        let empty_id = serde_json::json!({
            "attachments": [{ "contentType": "text/html",
                              "content": "<blockquote itemid=\"\"><p>x</p></blockquote>" }]
        });
        assert!(teams_quoted_reply_id(&empty_id).is_none());
        // Non-html attachments are ignored.
        let wrong_type = serde_json::json!({
            "attachments": [{ "contentType": "application/json",
                              "content": "<blockquote itemid=\"9\"></blockquote>" }]
        });
        assert!(teams_quoted_reply_id(&wrong_type).is_none());
    }

    #[test]
    fn teams_card_roundtrips_through_message_store_with_colon_chat_id() {
        // Teams conversation ids carry colons (`a:1abc…`) — the store's
        // reverse lookup must survive them (suffix-anchored key parse).
        let dir = tempfile::tempdir().unwrap();
        let conv = "a:1AbCdEf:GhIjKl";
        crate::decision_message_store::record_card_message(
            dir.path(),
            "approval",
            "req-42",
            "teams",
            conv,
            &crate::decision_card::PushedMessage {
                edit_chat_id: conv.into(),
                message_id: "1755083112345".into(),
            },
        );
        let hit = crate::decision_message_store::lookup_decision_by_message(
            dir.path(),
            "teams",
            conv,
            "1755083112345",
        );
        assert_eq!(hit, Some(("approval".to_string(), "req-42".to_string())));
        // Wrong message id → no match.
        assert!(
            crate::decision_message_store::lookup_decision_by_message(
                dir.path(),
                "teams",
                conv,
                "999"
            )
            .is_none()
        );
    }
}
