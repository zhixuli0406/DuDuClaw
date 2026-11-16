//! Google Chat channel — HTTP endpoint or authoritative Pub/Sub notifications.
//!
//! Inbound: Google POSTs interaction events to `POST /webhook/googlechat`,
//! authenticated with a JWT signed by `chat@system.gserviceaccount.com`
//! whose audience is the Cloud **project number** (fail-closed verification
//! in `webhook_jwt`). We ACK the POST immediately (Google retries on
//! timeouts and the synchronous window is only 30s — too short for LLM
//! replies) and deliver the reply asynchronously via
//! `spaces.messages.create` using a service-account token (scope
//! `chat.bot`).
//!
//! Inbound attachments: files uploaded to Chat are downloaded through the
//! media API (`GET /v1/media/{resourceName}?alt=media`, service-account
//! token) and saved under the agent's `attachments/`; linked Drive files are
//! skipped explicitly (the `chat.bot` scope cannot read Drive content) with a
//! text note. Attachment failures degrade — they never block the message.
//!
//! UX: Google Chat has no typing indicator API, so the channel posts a
//! placeholder message ("思考中…") right away and PATCHes it in place with
//! progress events (tool activity / TODO board) and finally the reply —
//! the closest native equivalent to typing + edit-in-place progress.
//!
//! Formatting: Chat text messages use Google's own markup (not markdown):
//! `*bold*`, `~strike~`, `<url|text>` links, no headers/tables — the
//! conversion lives in `markdown_render::to_googlechat_text`.
//!
//! Config (`config.toml [channels]`):
//! - `googlechat_project_number` — Cloud project number (JWT audience)
//! - `googlechat_service_account_json` (`_enc`) — service-account JSON key

use crate::isolation_http::LiveRequest;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use duduclaw_core::truncate_bytes;

use crate::channel_reply::{ReplyContext, build_guarded_reply_with_session, set_channel_connected};

const CHAT_API: &str = "https://chat.googleapis.com/v1";
const CHAT_ISSUER: &str = "chat@system.gserviceaccount.com";
const CHAT_JWKS_URL: &str =
    "https://www.googleapis.com/service_accounts/v1/jwk/chat@system.gserviceaccount.com";
const CHAT_SCOPE: &str = "https://www.googleapis.com/auth/chat.bot";

/// Google Chat text messages accept up to 32,000 bytes; chunk well below
/// for display comfort.
const GCHAT_TEXT_CHUNK: usize = 4000;

pub struct GoogleChatState {
    pub(crate) ctx: Arc<ReplyContext>,
    pub(crate) creds: Arc<GoogleChatCreds>,
    /// `Some(id)` when this is an employee's own Chat app: every message is
    /// answered by that employee, in its own session bucket.
    pub(crate) agent: Option<String>,
}

/// Where a creds handle re-reads its service-account key at refresh time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CredSource {
    /// `config.toml [channels] googlechat_service_account_json`.
    Global,
    Candidate,
    /// `agents/<id>/agent.toml [channels.googlechat] service_account_json`.
    Agent(String),
}

/// Said in the space when its queue of waiting messages is full.
const QUEUE_FULL_TEXT: &str = "目前排隊中的訊息太多，這則訊息沒有受理，請稍後再傳一次。";

/// Service-account credentials + token cache — separable from the webhook
/// state so delegation forwarding / Computer Use can send without a
/// `ReplyContext`.
///
/// Credentials doctrine P2 (WP-8A): `client_email` / `private_key` /
/// `token_uri` are the values this handle was constructed with — the values
/// actually used to sign a JWT-bearer assertion are re-read fresh from
/// `home_dir` on every token *refresh* (see `get_token`), so a rotated
/// service-account key takes effect within one refresh cycle instead of
/// requiring the gateway to restart. The cached OAuth `token` itself keeps
/// its TTL — a genuinely network-derived, short-lived session credential,
/// which the doctrine's TTL-with-invalidation allowance (design §2.4)
/// covers.
pub struct GoogleChatCreds {
    home_dir: std::path::PathBuf,
    /// From the service-account JSON key.
    client_email: String,
    private_key: String,
    token_uri: String,
    /// Cached OAuth token (access_token, fetched_at).
    token: RwLock<(String, std::time::Instant)>,
    pubsub_token: RwLock<(String, std::time::Instant)>,
    pub(crate) http: reqwest::Client,
    source: CredSource,
}

impl GoogleChatCreds {
    /// Parse `{client_email, private_key, token_uri}` out of a service-account
    /// JSON key. Shared by construction and by `get_token`'s refresh path so
    /// there is exactly one parser.
    fn parse_service_account_json(sa_json: &str) -> Option<(String, String, String)> {
        let sa: serde_json::Value = serde_json::from_str(sa_json).ok()?;
        Some((
            sa.get("client_email")?.as_str()?.to_string(),
            sa.get("private_key")?.as_str()?.to_string(),
            sa.get("token_uri")
                .and_then(|v| v.as_str())
                .unwrap_or("https://oauth2.googleapis.com/token")
                .to_string(),
        ))
    }

    /// Parse a service-account JSON key into a creds handle.
    pub(crate) fn from_service_account_json(
        sa_json: &str,
        home_dir: &Path,
    ) -> Option<GoogleChatCreds> {
        Self::with_source(sa_json, home_dir, CredSource::Global)
    }

    /// Creds for an employee's own Chat app (re-read from its `agent.toml`).
    pub(crate) fn for_agent(sa_json: &str, home_dir: &Path, agent_id: &str) -> Option<GoogleChatCreds> {
        Self::with_source(sa_json, home_dir, CredSource::Agent(agent_id.to_string()))
    }

    fn with_source(sa_json: &str, home_dir: &Path, source: CredSource) -> Option<GoogleChatCreds> {
        let (client_email, private_key, token_uri) = Self::parse_service_account_json(sa_json)?;
        Some(GoogleChatCreds {
            source,
            home_dir: home_dir.to_path_buf(),
            client_email,
            private_key,
            token_uri,
            token: RwLock::new((String::new(), std::time::Instant::now())),
            pubsub_token: RwLock::new((String::new(), std::time::Instant::now())),
            // 30s request timeout like every other channel client. A bare
            // `Client::new()` has NO request timeout, and `get_token()` is
            // awaited directly on the gateway boot path — an unresponsive
            // oauth2.googleapis.com would stall the whole boot sequence
            // (heartbeat/cron/tick never start) with zero diagnostics.
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
        })
    }

    /// The service account this handle signs as.
    pub(crate) fn client_email(&self) -> &str {
        &self.client_email
    }

    /// Chat REST API origin (`https://chat.googleapis.com/v1`). Test builds
    /// can route one service account to a local fake; debug builds honour
    /// `DUDUCLAW_DEV_GOOGLECHAT_API_BASE` (loopback only) for live tests.
    pub(crate) fn api_base(&self) -> String {
        #[cfg(test)]
        if let Some(origin) = test_routes::api(&self.client_email) {
            return origin;
        }
        #[cfg(debug_assertions)]
        if let Some(origin) =
            crate::agent_channels::dev_override::loopback_origin("DUDUCLAW_DEV_GOOGLECHAT_API_BASE")
        {
            return origin;
        }
        CHAT_API.to_string()
    }

    /// JWKS URL inbound request tokens are verified against (Google's Chat
    /// signer). Same test / debug override rules as [`Self::api_base`].
    pub(crate) fn jwks_url(&self) -> String {
        #[cfg(test)]
        if let Some(url) = test_routes::jwks(&self.client_email) {
            return url;
        }
        #[cfg(debug_assertions)]
        if let Some(url) =
            crate::agent_channels::dev_override::loopback_origin("DUDUCLAW_DEV_GOOGLECHAT_JWKS_URL")
        {
            return url;
        }
        CHAT_JWKS_URL.to_string()
    }

    /// The service-account key as configured right now for this handle's
    /// source. `None` when it cannot be read (or was removed).
    async fn current_service_account_json(&self) -> Option<String> {
        match &self.source {
            CredSource::Candidate => None,
            CredSource::Global => read_config(&self.home_dir, "googlechat_service_account_json")
                .await
                .filter(|j| !j.trim().is_empty()),
            CredSource::Agent(id) => crate::agent_channels::store::agent_chat_app(&self.home_dir, id)
                .await
                .map(|app| app.service_account.expose_owned()),
        }
    }

    /// Build from global config; `None` when the channel isn't configured.
    pub(crate) async fn from_config(home_dir: &std::path::Path) -> Option<GoogleChatCreds> {
        let sa_json = read_config(home_dir, "googlechat_service_account_json").await?;
        if sa_json.trim().is_empty() {
            return None;
        }
        Self::from_service_account_json(&sa_json, home_dir)
    }

    /// The access token (fetched or cached) — used by the credential probe.
    pub(crate) async fn token(&self) -> Result<String, String> {
        self.get_token().await
    }

    /// Get (or refresh) the service-account access token (JWT-bearer grant).
    async fn get_token(&self) -> Result<String, String> {
        self.get_scoped_token(CHAT_SCOPE, &self.token).await
    }

    pub(crate) fn candidate(raw: &str) -> Option<Self> {
        Self::with_source(raw, Path::new(""), CredSource::Candidate)
    }

    pub(crate) async fn pubsub_token(&self) -> Result<String, String> {
        self.get_scoped_token("https://www.googleapis.com/auth/pubsub", &self.pubsub_token).await
    }

    async fn get_scoped_token(&self, scope: &str, cache: &RwLock<(String, std::time::Instant)>) -> Result<String, String> {
        {
            let cached = cache.read().await;
            // Tokens last 3600s; refresh 5 minutes early.
            if !cached.0.is_empty() && cached.1.elapsed().as_secs() < 3300 {
                return Ok(cached.0.clone());
            }
        }

        // WP-8A: re-read the service-account JSON fresh at refresh time
        // instead of trusting the fields this handle was constructed with —
        // a rotated key (e.g. after a leak) takes effect on the very next
        // refresh. Falls back to the construction-time values only when the
        // current config can't be read at all (transient I/O, or a
        // `home_dir`-less handle from `from_service_account_json` used
        // directly) — this is an outbound credential, not a security gate,
        // so failing open to "try with the last known key" is preferable to
        // breaking every send on a momentary read hiccup.
        //
        // An employee's own app fails closed instead: when its key was
        // removed (or the employee deleted) nothing is sent as that app.
        let (client_email, private_key, token_uri) = match self.current_service_account_json().await {
            Some(json) => Self::parse_service_account_json(&json).ok_or_else(|| {
                "googlechat_service_account_json is not a valid service-account key".to_string()
            })?,
            None if matches!(self.source, CredSource::Global | CredSource::Candidate) => (
                self.client_email.clone(),
                self.private_key.clone(),
                self.token_uri.clone(),
            ),
            None => return Err("this AI employee's Google Chat app is no longer configured".into()),
        };
        // An employee's key may only post its assertion to Google (B11 L2).
        if !crate::agent_channels::googlechat_config::token_uri_allowed(&token_uri)
        {
            return Err("service-account token_uri is not Google's token endpoint".into());
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let claims = serde_json::json!({
            "iss": client_email,
            "scope": scope,
            "aud": token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key.as_bytes())
            .map_err(|e| format!("service-account key: {e}"))?;
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| format!("assertion sign: {e}"))?;

        let resp = self
            .http
            .post(&token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send_authorized()
            .await
            .map_err(|e| format!("token request: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "token status {status}: {}",
                truncate_bytes(&duduclaw_security::credential_detect::mask(&body), 200)
            ));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| format!("token parse: {e}"))?;
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or("no access_token in response")?
            .to_string();
        *cache.write().await = (token.clone(), std::time::Instant::now());
        Ok(token)
    }
}

/// Verify the candidate key itself before saving; never reuse the installed key.
pub async fn verify_import_key(raw: &str, pubsub: bool) -> Result<(), String> {
    crate::agent_channels::googlechat_config::validate_service_account_json(raw)?;
    let creds = GoogleChatCreds::with_source(raw, Path::new(""), CredSource::Candidate)
        .ok_or("服務帳戶金鑰格式不正確")?;
    creds.token().await?;
    if pubsub { creds.pubsub_token().await?; }
    Ok(())
}

/// Read config and build the Google Chat webhook router. Returns `None`
/// when the channel isn't configured.
pub async fn start_googlechat_webhook(home_dir: &Path, ctx: Arc<ReplyContext>) -> Option<Router> {
    if !shared_settings_valid(home_dir).await || !crate::googlechat_pubsub::shared_http_enabled(home_dir).await { return None; }
    let project_number = read_config(home_dir, "googlechat_project_number").await?;
    if project_number.trim().is_empty() {
        return None;
    }
    let Some(creds) = GoogleChatCreds::from_config(home_dir).await else {
        error!(
            "Google Chat: googlechat_service_account_json missing or not a valid service-account key"
        );
        return None;
    };
    // project_number above (checked non-empty) is used only to decide
    // whether to mount the router at all — the webhook handler re-reads it
    // fresh on every request instead of trusting this value for the task's
    // entire lifetime (WP-8A: see its call site's comment).

    let state = Arc::new(GoogleChatState {
        ctx: ctx.clone(),
        creds: Arc::new(creds),
        agent: None,
    });

    // Verify credentials eagerly so the dashboard shows real status.
    match state.creds.get_token().await {
        Ok(_) => {
            info!("✅ Google Chat webhook ready at /webhook/googlechat");
            set_channel_connected(
                &ctx.channel_status,
                "googlechat",
                true,
                None,
                Some(&ctx.event_tx),
            )
            .await;
        }
        Err(e) => {
            warn!("Google Chat: service-account auth failed (webhook still mounted): {e}");
            set_channel_connected(
                &ctx.channel_status,
                "googlechat",
                false,
                Some(e),
                Some(&ctx.event_tx),
            )
            .await;
        }
    }

    Some(
        Router::new()
            .route("/webhook/googlechat", post(webhook_handler))
            .with_state(state),
    )
}

pub(crate) async fn shared_settings_valid(home_dir: &Path) -> bool {
    let Ok(raw) = tokio::fs::read_to_string(home_dir.join("config.toml")).await else { return false; };
    let Ok(table) = raw.parse::<toml::Table>() else { return false; };
    let Some(channels) = table.get("channels").and_then(toml::Value::as_table) else { return false; };
    let fields = channels.keys().filter_map(|field| field.strip_prefix("googlechat_"));
    if let Err(error) = crate::agent_channels::googlechat_config::validate_fields(fields,
        &["project_number", "service_account_json", "service_account_json_enc", "connection", "pubsub_subscription"]) {
        warn!("{error}");
        return false;
    }
    true
}

async fn read_config(home_dir: &Path, field: &str) -> Option<String> {
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field).await
}

async fn webhook_handler(
    State(state): State<Arc<GoogleChatState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !shared_settings_valid(&state.ctx.home_dir).await || !crate::googlechat_pubsub::shared_http_enabled(&state.ctx.home_dir).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    // ── Verify the Google-signed JWT (fail closed) ──
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let Some(token) = crate::webhook_jwt::bearer_token(auth) else {
        warn!("Google Chat webhook: missing bearer token");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };
    // WP-8A: re-read fresh for this request instead of trusting a value
    // captured at task-spawn time. Fail-closed if it has since been unset.
    let project_number = match read_config(&state.ctx.home_dir, "googlechat_project_number").await {
        Some(p) if !p.trim().is_empty() => p,
        _ => {
            warn!("Google Chat webhook: project_number not configured — rejecting request");
            return (StatusCode::UNAUTHORIZED, "not configured").into_response();
        }
    };
    if let Err(e) = verify_request(&state.creds, token, &project_number).await {
        warn!("Google Chat webhook: JWT verification failed: {e}");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let _bind = crate::agent_channels::binding_lock().await;
    if !shared_settings_valid(&state.ctx.home_dir).await
        || !crate::googlechat_pubsub::shared_http_enabled(&state.ctx.home_dir).await
        || read_config(&state.ctx.home_dir, "googlechat_project_number").await.as_deref() != Some(project_number.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    dispatch_event(state, body).await
}

/// Verify a Chat request token: RS256 by Google's Chat signer, audience =
/// the app's Cloud project number. Fail closed on every error.
pub(crate) async fn verify_request(
    creds: &GoogleChatCreds,
    token: &str,
    project_number: &str,
) -> Result<serde_json::Value, String> {
    crate::webhook_jwt::verify_rs256(
        &creds.http,
        token,
        &creds.jwks_url(),
        CHAT_ISSUER,
        project_number,
    )
    .await
}

/// Handle an already-verified Chat event (shared by the shared app and the
/// employees' own apps).
pub(crate) async fn dispatch_event(state: Arc<GoogleChatState>, body: Bytes) -> axum::response::Response {
    dispatch_transport_event(state, body).await
}

pub(crate) const ADDON_DIAGNOSTIC: &str = "Google Chat 應用程式使用了不支援的 Workspace 外掛程式事件格式。建立時請取消勾選『以 Google Workspace 外掛程式建構這個 Chat 應用程式』（Build this Chat app as a Google Workspace add-on），再改用 Chat API 互動事件。";

pub(crate) fn is_addon_event(event: &serde_json::Value) -> bool {
    event.get("commonEventObject").is_some() || event.pointer("/chat/messagePayload").is_some()
}

pub(crate) async fn report_addon(state: &GoogleChatState) {
    let label = state.agent.as_deref().map(|id| crate::agent_channels::label("googlechat", id))
        .unwrap_or_else(|| "googlechat".into());
    warn!(channel = label, "{ADDON_DIAGNOSTIC}");
    set_channel_connected(&state.ctx.channel_status, &label, false,
        Some(ADDON_DIAGNOSTIC.into()), Some(&state.ctx.event_tx)).await;
}

/// Only the Pub/Sub worker supplies this event, rebuilt from spaces.messages.get.
pub(crate) async fn process_pubsub_event(state: &Arc<GoogleChatState>, event: &serde_json::Value) {
    let space = event_space(event);
    crate::agent_channels::gchat_spaces::record(&state.ctx.home_dir, &space, state.agent.as_deref());
    handle_message(state, event).await;
}

async fn dispatch_transport_event(state: Arc<GoogleChatState>, body: Bytes) -> axum::response::Response {
    let event: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Google Chat webhook parse error: {e}");
            return (StatusCode::BAD_REQUEST, "bad request").into_response();
        }
    };

    if is_addon_event(&event) {
        report_addon(&state).await;
        return (StatusCode::BAD_REQUEST, ADDON_DIAGNOSTIC).into_response();
    }
    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    // Verified event: remember which app is in this space, so sends with no
    // explicit sender (approvals, goals, reminders) use an app that is there.
    if let Some(space) = event
        .pointer("/space/name")
        .or_else(|| event.pointer("/message/space/name"))
        .and_then(|v| v.as_str())
    {
        crate::agent_channels::gchat_spaces::record(&state.ctx.home_dir, space, state.agent.as_deref());
    }
    match event_type {
        "MESSAGE" => {
            // One sender's messages in a space are answered one at a time,
            // in order (they used to run concurrently, B10).
            let space = event_space(&event);
            if space.is_empty() {
                warn!("Google Chat: MESSAGE event without space name — refused");
                return (StatusCode::BAD_REQUEST, "bad request").into_response();
            }
            if !event.get("message").is_some_and(is_human_message) {
                // ACK so Google does not redeliver; nothing is enqueued and
                // the sender id is never mapped to an account.
                info!("Google Chat: MESSAGE from a non-human sender ignored");
                return (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response();
            }
            let sender = event
                .pointer("/message/sender/name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let st = state.clone();
            let queued = event.clone();
            // The queue outlives each request. Include the app identity so
            // two apps in one space do not block each other or share caps.
            let (group, conversation) = queue_keys(state.agent.as_deref(), &space, &sender);
            let message_name = event.pointer("/message/name").and_then(|v| v.as_str()).filter(|name| !name.is_empty() && name.len() <= 1024);
            let dedupe_key = message_name.map(|name| serde_json::json!([
                state.ctx.home_dir, state.agent, state.creds.client_email(), name
            ]).to_string());
            let accepted = enqueue_once(dedupe_key.as_deref(), || state.ctx.googlechat_queue
                .submit_in(&group, &conversation, async move { handle_message(&st, &queued).await }).is_ok());
            if !accepted
            {
                let is_dm = crate::googlechat_space_kind::is_one_to_one(&state.creds, &space, &event["message"], &event).await;
                let thread = reply_thread(&state, &event, is_dm).await;
                notify_queue_full(&state, &group, &conversation, &space, thread.as_deref());
                // ACK and drop this event: the notice asks the user to resend.
                return (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response();
            }
            // ACK synchronously with an empty body — the reply arrives
            // asynchronously via the REST API.
            (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response()
        }
        "ADDED_TO_SPACE" => {
            let space = event
                .pointer("/space/displayName")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            info!("Google Chat: added to space {space}");
            let product = match &state.agent {
                Some(id) => crate::trigger_route::display_name(&*state.ctx.registry.read().await, id),
                None => "DuDuClaw".to_string(),
            };
            let welcome = format!("{product} 已加入！直接傳訊息即可對話。");
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "text": welcome })),
            )
                .into_response()
        }
        _ => (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response(),
    }
}

/// Cache only successful admission, under the same lock as submit so a
/// redelivery cannot race the original. Bounded, process-local, 24-hour TTL.
fn enqueue_once(key: Option<&str>, submit: impl FnOnce() -> bool) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static ACCEPTED: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    let Some(key) = key else { return submit(); };
    let mut accepted = ACCEPTED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    accepted.retain(|_, at| at.elapsed().as_secs() < 86400);
    if accepted.contains_key(key) { return true; }
    if !submit() { return false; }
    if accepted.len() >= 10000 {
        if let Some(oldest) = accepted.iter().min_by_key(|(_, at)| **at).map(|(key, _)| key.clone()) {
            accepted.remove(&oldest);
        }
    }
    accepted.insert(key.to_string(), std::time::Instant::now());
    true
}

/// Only a message Google reports as written by a person (`sender.type ==
/// "HUMAN"`) may start a turn or be mapped to a dashboard account — the same
/// rule the Pub/Sub worker applies to its authoritative re-read
/// (`googlechat_pubsub::authoritative_event`). A `BOT` sender (another Chat
/// app, possibly echoing a person's id) or a missing/unknown type is dropped.
pub(crate) fn is_human_message(message: &serde_json::Value) -> bool {
    message.pointer("/sender/type").and_then(|v| v.as_str()) == Some("HUMAN")
}

/// Queue keys shared by the HTTP and Pub/Sub paths: one per app+space and
/// one per app+space+sender (also the throttle keys of the queue-full notice).
pub(crate) fn queue_keys(agent: Option<&str>, space: &str, sender: &str) -> (String, String) {
    (
        serde_json::json!([agent, space]).to_string(),
        serde_json::json!([agent, space, sender]).to_string(),
    )
}

/// Tell the space its message was not accepted because the queue is full,
/// at most once a minute per sender and once every 10 s per space. Both the
/// HTTP endpoint and the Pub/Sub worker use this, with the same keys.
/// Returns whether a notice was sent (spawned).
pub(crate) fn notify_queue_full(
    state: &Arc<GoogleChatState>,
    group: &str,
    conversation: &str,
    space: &str,
    thread: Option<&str>,
) -> bool {
    let notify = state.ctx.googlechat_full_notices.allow(&[
        (&format!("sender:{conversation}"), std::time::Duration::from_secs(60)),
        (&format!("space:{group}"), std::time::Duration::from_secs(10)),
    ]);
    warn!(notify, "Google Chat: queue full for a space or sender");
    if notify && !space.is_empty() {
        let st = state.clone();
        let space = space.to_string();
        let thread = thread.map(str::to_string);
        tokio::spawn(async move {
            create_message(&st.creds, &space, thread.as_deref(), QUEUE_FULL_TEXT).await;
        });
    }
    notify
}

/// The space a MESSAGE event belongs to (the queue key; same lookup as
/// [`handle_message`]).
fn event_space(event: &serde_json::Value) -> String {
    event
        .pointer("/message/space/name")
        .or_else(|| event.pointer("/space/name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Assemble smart chips before the shared input guard sees the inbound message.
pub(crate) fn inbound_text(message: &serde_json::Value) -> String {
    let mut text = message.get("argumentText").or_else(|| message.get("text"))
        .and_then(serde_json::Value::as_str).unwrap_or("").trim().to_owned();
    let links = crate::google_workspace::chat_read::rich_links(message, &text);
    if !links.is_empty() { text.push_str(&format!("\n\n（訊息裡的預覽連結：{}）", serde_json::to_string(&links).expect("string links serialize"))); }
    text
}

pub(crate) async fn reply_thread(state: &GoogleChatState, event: &serde_json::Value, is_dm: bool) -> Option<String> {
    let message = &event["message"];
    let always = state.ctx.channel_settings.get("googlechat", "global", "dm_reply_placement").await.as_deref() == Some("thread");
    if is_dm && !always && message.get("threadReply").and_then(serde_json::Value::as_bool) != Some(true) { return None; }
    message.pointer("/thread/name").and_then(serde_json::Value::as_str).map(str::to_owned)
}

async fn handle_message(state: &Arc<GoogleChatState>, event: &serde_json::Value) {
    let message = event.get("message").cloned().unwrap_or_default();
    // Defence in depth: every entry already requires a human sender.
    if !is_human_message(&message) {
        return;
    }
    let text = inbound_text(&message);
    // Attachment-only messages carry no text — parse attachments before the
    // empty-text bail so a bare file upload still reaches the agent.
    let attachments = parse_gchat_attachments(&message);
    if text.is_empty() && attachments.is_empty() {
        return;
    }

    let space = message
        .pointer("/space/name")
        .or_else(|| event.pointer("/space/name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if space.is_empty() {
        warn!("Google Chat: MESSAGE event without space name");
        return;
    }
    let is_dm = crate::googlechat_space_kind::is_one_to_one(&state.creds, &space, &message, event).await;
    let thread = reply_thread(state, event, is_dm).await;
    let sender = message
        .pointer("/sender/displayName")
        .and_then(|v| v.as_str())
        .unwrap_or("someone")
        .to_string();
    let sender_id = message
        .pointer("/sender/name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let group_turn = crate::channel_reply::group_buffer::GroupTurn::capture(&state.ctx, message.pointer("/space/type").or_else(|| event.pointer("/space/type")).and_then(|v| v.as_str()).is_some_and(|kind| matches!(kind, "ROOM" | "SPACE" | "GROUP_CHAT")), "googlechat", "global", &space, thread.as_deref(), message.get("name").and_then(|v| v.as_str()).unwrap_or(""), &sender_id, &sender, if text.is_empty() { "[附件]" } else { &text }).await;

    let raw_text = message.get("argumentText").or_else(|| message.get("text"))
        .and_then(serde_json::Value::as_str).unwrap_or("").trim();
    let link_count = crate::google_workspace::chat_read::rich_links(&message, raw_text).len();
    info!(sender = %sender, message_length = text.len(), link_count, "Google Chat inbound message");

    // ── Inbound attachments (WP1.3 parity with the other 8 channels) ──
    // Uploaded-to-Chat files are fetched through the media API with the
    // service-account token and saved under the agent's `attachments/`; Drive
    // links need a Drive scope the `chat.bot` token does not carry, so they
    // degrade to an explicit skip note. Every failure degrades to a text
    // reference — attachment problems never block the message itself.
    let mut attachment_lines: Vec<String> = Vec::new();
    if !attachments.is_empty() {
        let attach_base =
            crate::channel_reply::resolve_attachment_base(state.ctx.as_ref(), state.agent.as_deref())
                .await;
        for att in &attachments {
            let filename = att.effective_filename();
            match &att.reference {
                GchatAttachmentRef::Uploaded { resource_name } => {
                    match download_media(&state.creds, resource_name).await {
                        Ok(bytes) => {
                            let mt = crate::media::media_type_from_mime(&att.content_type);
                            match crate::media::save_attachment_in_base(
                                &attach_base,
                                &bytes,
                                &filename,
                            )
                            .await
                            {
                                Ok(path) => attachment_lines.push(
                                    crate::media::format_attachment_ref(&mt, &filename, &path),
                                ),
                                Err(e) => {
                                    warn!("Google Chat: failed to save attachment {filename}: {e}");
                                    attachment_lines
                                        .push(format!("[Attached file: {filename} (save failed)]"));
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Google Chat: failed to download attachment {filename}: {e}");
                            attachment_lines
                                .push(format!("[Attached file: {filename} (download failed)]"));
                        }
                    }
                }
                GchatAttachmentRef::Drive { file_id } => {
                    attachment_lines.push(drive_attachment_note(&filename, file_id));
                }
            }
        }
    }

    // Combine text + attachment references (same convention as telegram/discord).
    let input_text = if attachment_lines.is_empty() {
        text.clone()
    } else if text.is_empty() {
        attachment_lines.join("\n")
    } else {
        format!("{text}\n\n{}", attachment_lines.join("\n"))
    };
    if input_text.trim().is_empty() {
        return;
    }

    // Resolve the durable identity before creating any visible placeholder.
    let session_id = format!("googlechat:{space}");
    let reply_session = match &state.agent {
        Some(agent) => crate::trigger_route::called_session(&session_id, agent),
        None => session_id.clone(),
    };
    let employee_id = match &state.agent {
        Some(agent) => agent.clone(),
        None => state.ctx.registry.read().await.main_agent()
            .map(|a| a.config.agent.name.clone()).unwrap_or_default(),
    };
    // Fail closed: never create a placeholder when recovery cannot be persisted.
    let store = match crate::googlechat_inbox::Inbox::shared(&state.ctx.home_dir) {
        Ok(store) => store,
        Err(_) => { warn!(failed = 1, "Google Chat placeholder store unavailable"); return; }
    };
    let name = create_message(&state.creds, &space, thread.as_deref(), "🤔 思考中…").await;
    let registration = if let Some(name) = name.as_ref() {
        let row = crate::googlechat_inbox::PendingPlaceholder {
            message_name: name.clone(), space: space.clone(), app_agent: state.agent.clone(),
            app_email: state.creds.client_email().to_owned(), employee_id,
            session_id: reply_session.clone(), thread_name: thread.clone(),
            created_at: chrono::Utc::now().timestamp(),
        };
        match crate::googlechat_inbox::PlaceholderRegistration::new(store, &row) {
            Ok(registration) => Some(registration),
            Err(_) => {
                delete_placeholder(&state.creds, name).await;
                warn!(failed = 1, "Google Chat placeholder persistence failed");
                return;
            }
        }
    } else { None };
    let placeholder = Arc::new(tokio::sync::Mutex::new(name));

    // ── Progress: PATCH the placeholder in place ──
    let progress_state = state.clone();
    let progress_name = placeholder.clone();
    let last_progress = Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        // Allowlist: dashboard-only events (Step / ModelInfo / Thinking) never
        // reach the channel — and never consume the progress throttle.
        if !forwards_progress(&event) {
            return;
        }
        let is_todo = matches!(
            event,
            crate::channel_reply::ProgressEvent::TodoUpdate { .. }
        );
        {
            let mut last = last_progress.lock().unwrap_or_else(|e| e.into_inner());
            let throttle =
                crate::channel_capabilities::progress_throttle_secs("googlechat").unwrap_or(30);
            if !is_todo && last.elapsed().as_secs() < throttle {
                return;
            }
            *last = std::time::Instant::now();
        }
        let st = progress_state.clone();
        let msg_text = event.to_display();
        let slot = progress_name.clone();
        tokio::spawn(async move {
            // Hold the lock until PATCH completes. Final delivery waits, then
            // takes the name; queued callbacks observe None and cannot overwrite it.
            let name = slot.lock().await;
            if let Some(name) = name.as_deref() {
                if let Err(error) = update_message(&st.creds, name, &msg_text).await {
                    warn!(status = ?error.status, "Google Chat progress update failed");
                }
            }
        });
    });

    // ── Chat commands ──
    let delivery_target = DeliveryTarget {
        home: &state.ctx.home_dir,
        space: &space,
        agent: state.agent.as_deref(),
    };
    if crate::chat_commands::is_command(&text) {
        if let Some(cmd) = crate::chat_commands::parse_command(&text, None) {
            // The same access gate the reply pipeline runs (on the space, with
            // the same laundered sender id), then the shared-space rule for
            // clearing history: the session is shared by every member and
            // thread of the space, so only a verified Admin/Manager may clear
            // it there; a 1:1 DM keeps resetting its own conversation.
            // The space type comes from the trusted event / read-back, else
            // `spaces.get` (cached); unknown = shared. Only resolved for the
            // commands the rule applies to.
            let one_to_one = crate::chat_commands::clears_conversation(&cmd)
                && crate::googlechat_space_kind::is_one_to_one(
                    &state.creds,
                    &space,
                    &message,
                    event,
                )
                .await;
            let origin = crate::chat_commands::CommandOrigin {
                channel: "googlechat",
                gate_session: &session_id,
                command_session: &reply_session,
                sender: crate::ccr_runtime::reply_principal_for_sender(&sender_id),
                text: &text,
                shared: !one_to_one,
            };
            let reply = match crate::chat_commands::command_preflight(&cmd, &state.ctx, &origin).await {
                Some(reply) => reply,
                None => {
                    let agent_id = match &state.agent {
                        Some(agent) => agent.clone(),
                        None => {
                            let reg = state.ctx.registry.read().await;
                            reg.main_agent()
                                .map(|a| a.config.agent.name.clone())
                                .unwrap_or_default()
                        }
                    };
                    crate::chat_commands::handle_command_for_sender(
                        &cmd,
                        &state.ctx,
                        "googlechat",
                        &reply_session,
                        &agent_id,
                        origin.sender,
                    )
                    .await
                }
            };
            // An empty reply (blocked sender) removes the placeholder: silence.
            report_delivery(delivery_target, deliver_reply(
                &state.creds,
                &space,
                thread.as_deref(),
                &placeholder,
                registration.as_ref(),
                &reply,
                None,
            ).await);
            return;
        }
    }

    // `sender_id` falls back to a literal placeholder when the message has no
    // `sender.name`; it keeps that value everywhere it is an addressing or
    // logging key (the document sender, the audit trail), but it must never
    // become the CCR principal — every unidentified sender would hash to one
    // shared retrieval scope. `reply_principal_for_sender` yields "" there,
    // which turns CCR off for the turn (fail-closed).
    // Google Chat already shows a "思考中…" placeholder that the answer
    // replaces, so thinking display adds one line to the answer instead of a
    // second status message.
    // Chat's placeholder is reserved for progress/final delivery. Thinking
    // contributes at most a staff-only duration; never a summary in the answer.
    let thinking = crate::thinking::channel_presenter::ChannelThinking::new(
        "googlechat",
        !is_dm,
        crate::trigger_route::texts::lang_for(&state.ctx.home_dir),
        None,
    );
    let on_progress = thinking.wrap(on_progress);
    let (guarded, deliberate_silence) = crate::channel_reply::track_silence(group_turn.scope(async { match &state.agent {
        // Own app: deterministic routing to its employee, in the employee's
        // bucket; the access gate and takeover still run on the space.
        Some(agent) => {
            thinking
                .scope(crate::trigger_route::as_own_bot_turn(
                    crate::channel_reply::build_guarded_reply_for_agent(
                        &input_text,
                        &state.ctx,
                        agent,
                        &session_id,
                        crate::ccr_runtime::reply_principal_for_sender(&sender_id),
                        Some(on_progress),
                    ),
                ))
                .await
        }
        None => {
            thinking
                .scope(build_guarded_reply_with_session(
                    &input_text,
                    &state.ctx,
                    &session_id,
                    crate::ccr_runtime::reply_principal_for_sender(&sender_id),
                    Some(on_progress),
                ))
                .await
        }
    } })).await;
    let thinking_footer = thinking.finish().await;
    if !guarded.still_valid().await {
        report_delivery(delivery_target, deliver_reply(
            &state.creds,
            &space,
            thread.as_deref(),
            &placeholder,
            registration.as_ref(),
            crate::channel_reply::CCR_DELIVERY_REFUSED_TEXT,
            None,
        ).await);
        return;
    }

    // WP1.3: 📎DELIVER: — Google Chat attachment upload is not wired, so the
    // sender's default `send_document` degrades to a text notice (→ dashboard
    // Files panel); the marker is stripped from the reply.
    let reply = {
        let doc_sender = crate::channel_sender::create_googlechat_sender_for(
            state.ctx.home_dir.clone(),
            space.clone(),
            sender_id.clone(),
            state.agent.clone(),
        );
        crate::channel_reply::deliver_documents_for_reply_guarded(
            state.ctx.as_ref(),
            state.agent.as_deref(),
            guarded.text.clone(),
            doc_sender.as_ref(),
            Some(&guarded),
        )
        .await
    };

    if reply.trim().is_empty() {
        if deliberate_silence {
            if let Some(name) = placeholder.lock().await.take() {
                delete_placeholder(&state.creds, &name).await;
            }
            if let Some(registration) = registration.as_ref() { registration.retire(); }
        } else {
            report_delivery(delivery_target, deliver_reply(&state.creds, &space, thread.as_deref(),
                &placeholder, registration.as_ref(), "⚠️ 沒有產生回覆，請再試一次。", None).await);
        }
        return;
    }
    let reply = crate::thinking::channel_presenter::with_footer(reply, thinking_footer.as_ref());

    let answered = DeliveryTarget {
        agent: guarded.answered_by.as_ref().map(|who| who.id.as_str()).or(delivery_target.agent),
        ..delivery_target
    };
    report_delivery(answered, group_turn.delivery(&state.ctx, guarded.answered_by.as_ref(), thread.as_deref()).scope(deliver_reply(
        &state.creds,
        &space,
        thread.as_deref(),
        &placeholder,
        registration.as_ref(),
        &reply,
        Some(&guarded),
    )).await);
}

/// Deliver the final reply: first chunk replaces the placeholder (PATCH),
/// remaining chunks are new messages in the same thread.
type Placeholder = Arc<tokio::sync::Mutex<Option<String>>>;

/// The `channel_failures.jsonl` event for a reply Google Chat could not
/// deliver even after the new-message fallback. Counted by channel alerts
/// (`channel_alerts::SEND_FAILURE_EVENTS`).
pub(crate) const SEND_FAILED_EVENT: &str = "googlechat_send_failed";

/// Where a final reply was going, for the failure record (never content).
#[derive(Clone, Copy)]
struct DeliveryTarget<'a> {
    home: &'a Path,
    space: &'a str,
    /// The employee whose answer it was, when known.
    agent: Option<&'a str>,
}

fn report_delivery(target: DeliveryTarget<'_>, result: Result<(), Option<u16>>) {
    if let Err(status) = result {
        warn!(?status, "Google Chat final delivery failed");
        let rec = send_failure_record(target.space, target.agent, status);
        if let Err(e) = crate::trajectory_guard::append_anomaly(target.home, &rec) {
            warn!(error = %e, "googlechat: 寫入 channel_failures.jsonl 失敗");
        }
    }
}

/// The failure line: identifiers and the HTTP status only — no message text.
fn send_failure_record(space: &str, agent: Option<&str>, status: Option<u16>) -> serde_json::Value {
    let mut rec = serde_json::json!({
        "event": SEND_FAILED_EVENT,
        "channel": "googlechat",
        "space": space,
        "status": status,
        "reason": "final_delivery_failed",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(agent) = agent.filter(|a| !a.is_empty()) {
        rec["agent"] = serde_json::json!(agent);
    }
    rec
}

async fn deliver_reply(
    creds: &GoogleChatCreds,
    space: &str,
    thread: Option<&str>,
    placeholder: &Placeholder,
    registration: Option<&crate::googlechat_inbox::PlaceholderRegistration>,
    reply_markdown: &str,
    guarded: Option<&crate::channel_reply::GuardedReply>,
) -> Result<(), Option<u16>> {
    // Taking the name waits for any in-flight progress PATCH and permanently
    // disables all queued progress updates, including during retry back-off.
    let placeholder = placeholder.lock().await.take();
    if reply_markdown.trim().is_empty() {
        if let Some(name) = placeholder.as_deref() { delete_placeholder(creds, name).await; }
        if let Some(registration) = registration { registration.retire(); }
        return Ok(());
    }
    let formatted = crate::markdown_render::to_googlechat_text(reply_markdown);
    let chunks = crate::channel_format::split_text(&formatted, GCHAT_TEXT_CHUNK);
    let mut failure = None;
    for (index, chunk) in chunks.iter().enumerate() {
        if crate::channel_reply::guard_lost(guarded).await {
            let refusal = crate::channel_reply::CCR_DELIVERY_REFUSED_TEXT;
            let result = if index == 0 && let Some(name) = placeholder.as_deref() {
                replace_placeholder(name,
                    || update_message(creds, name, refusal),
                    || post_message(creds, space, thread, refusal),
                    || update_message(creds, name, "處理已結束。"),
                ).await.map(|_| ())
            } else {
                post_message(creds, space, thread, refusal).await.map(|_| ())
            };
            if index == 0 { if let Some(registration) = registration { registration.retire(); } }
            if let Err(status) = result {
                warn!(?status, "Google Chat refusal delivery failed");
                return Err(status);
            }
            // The refusal replaced the answer on purpose; an earlier chunk
            // that failed is still a delivery failure.
            return match failure { Some(status) => Err(status), None => Ok(()) };
        }
        let result = if index == 0 && let Some(name) = placeholder.as_deref() {
            replace_placeholder(name,
                || async {
                    if crate::channel_reply::guard_lost(guarded).await {
                        return Err(PatchFailure { status: None, retry_after: std::time::Duration::ZERO });
                    }
                    update_message(creds, name, chunk).await
                },
                || async {
                    if crate::channel_reply::guard_lost(guarded).await { return Err(None); }
                    post_message(creds, space, thread, chunk).await
                },
                || update_message(creds, name, "處理已結束。"),
            ).await
        } else {
            post_message(creds, space, thread, chunk).await
        };
        if index == 0 { if let Some(registration) = registration { registration.retire(); } }
        match result {
            Ok(id) => crate::channel_reply::group_buffer::delivered(chunk, id.as_deref()).await,
            Err(status) => {
                warn!(?status, "Google Chat chunk delivery failed");
                failure.get_or_insert(status);
                // Still attempt every remaining chunk, rather than losing the answer tail.
            }
        }
    }
    if let Some(registration) = registration { registration.retire(); }
    match failure { Some(status) => Err(status), None => Ok(()) }
}

#[derive(Debug, Clone)]
pub(crate) struct PatchFailure {
    pub(crate) status: Option<u16>,
    retry_after: std::time::Duration,
}

fn patch_backoff(status: u16, retry_after: Option<&str>) -> std::time::Duration {
    let seconds = if status == 429 {
        retry_after.and_then(|value| value.parse::<u64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value).ok().map(|at| {
                (at.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds().max(0) as u64
            })
        })).unwrap_or(1).min(3)
    } else { 1 };
    std::time::Duration::from_secs(seconds)
}

/// The first chunk always has a new-message fallback after exactly one retry.
/// Small injectable operations let tests exercise failure ordering without Chat credentials.
async fn replace_placeholder<P, PF, S, SF, C, CF>(
    name: &str, mut patch: P, post: S, clear: C,
) -> Result<Option<String>, Option<u16>>
where
    P: FnMut() -> PF, PF: std::future::Future<Output = Result<(), PatchFailure>>,
    S: FnOnce() -> SF, SF: std::future::Future<Output = Result<Option<String>, Option<u16>>>,
    C: FnOnce() -> CF, CF: std::future::Future<Output = Result<(), PatchFailure>>,
{
    match patch().await {
        Ok(()) => return Ok(Some(name.to_string())),
        Err(error) => {
            warn!(status = ?error.status, "Google Chat final PATCH failed; retrying");
            tokio::time::sleep(error.retry_after).await;
        }
    }
    match patch().await {
        Ok(()) => Ok(Some(name.to_string())),
        Err(error) => {
            warn!(status = ?error.status, "Google Chat final PATCH failed; sending new message");
            let sent = post().await;
            if let Err(error) = clear().await {
                warn!(status = ?error.status, "Google Chat placeholder cleanup failed");
            }
            sent
        }
    }
}

async fn delete_placeholder(creds: &GoogleChatCreds, name: &str) {
    let Ok(token) = creds.get_token().await else {
        warn!(status = ?Option::<u16>::None, "Google Chat placeholder removal token unavailable");
        return;
    };
    match creds.http.delete(format!("{}/{name}", creds.api_base()))
        .bearer_auth(token).send_authorized().await
    {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => warn!(status = response.status().as_u16(), "Google Chat placeholder removal failed"),
        Err(_) => warn!(status = ?Option::<u16>::None, "Google Chat placeholder removal transport failure"),
    }
}

/// Only provider resource IDs, never arbitrary text, may enter the tool hint.
fn drive_attachment_note(filename: &str, file_id: &str) -> String {
    let valid = (1..=256).contains(&file_id.len())
        && file_id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
    if valid {
        format!("[Attached Google Drive file: {filename} — not downloaded automatically. driveFileId: {file_id}. Open this id with the Drive read tool (drive_read); access depends on the connected Google identity and file type.]")
    } else {
        format!("[Attached Google Drive file: {filename} — not downloaded automatically; no valid Drive file id available.]")
    }
}

/// Send failures need app-installation guidance rather than read-tool authorization guidance.
fn send_error_text(status: u16, lang: crate::trigger_route::texts::Lang) -> &'static str {
    use crate::trigger_route::texts::Lang;
    let row = match status { 403 | 404 => 0, 429 | 500..=599 => 1, _ => 2 };
    let messages = match lang {
        Lang::ZhTw => [
            "對方尚未與此 Chat 應用程式建立對話，或應用程式不在該聊天室。請對方先私訊此應用程式一次，或請管理員為對方安裝，或將應用程式加入該聊天室。",
            "Google Chat 暫時無法傳送訊息，請稍後再試。",
            "Google Chat 拒絕傳送訊息，請管理員確認應用程式設定。",
        ],
        Lang::En => [
            "The person has not started a conversation with this Chat app, or the app is not in the space. Ask the person to DM the app once, or ask an administrator to install it for them or add it to the space.",
            "Google Chat temporarily cannot send messages. Try again later.",
            "Google Chat rejected the message. Ask an administrator to check the app configuration.",
        ],
        Lang::Ja => [
            "相手がこの Chat アプリとの会話を開始していないか、アプリがスペースに参加していません。相手に一度アプリへ DM を送ってもらうか、管理者にアプリのインストールまたはスペースへの追加を依頼してください。",
            "Google Chat で一時的にメッセージを送信できません。しばらくしてから再試行してください。",
            "Google Chat がメッセージを拒否しました。管理者にアプリの設定を確認してもらってください。",
        ],
    };
    messages[row]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleChatSendError {
    InvalidTarget,
    Configuration(String),
    Http { status: u16 },
    Transport,
}

impl GoogleChatSendError {
    pub(crate) fn user_message(&self, home: &Path) -> String {
        match self {
            Self::Http { status } => send_error_text(*status, crate::trigger_route::texts::lang_for(home)).to_owned(),
            _ => self.to_string(),
        }
    }
}

impl std::fmt::Display for GoogleChatSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget => f.write_str("Google Chat 目標必須是有效的 spaces/<id>，不能使用使用者 ID 或電子郵件。"),
            Self::Configuration(reason) => f.write_str(reason),
            Self::Http { status } => f.write_str(send_error_text(*status, crate::trigger_route::texts::Lang::ZhTw)),
            Self::Transport => f.write_str("Google Chat 暫時無法連線或取得授權，請稍後重試。"),
        }
    }
}

impl std::error::Error for GoogleChatSendError {}

/// Send markdown text to a space (proactive / delegation-forwarding path).
/// The Chat app must already be a member of the space.
pub async fn send_text_to_space(
    home_dir: &Path,
    space: &str,
    markdown: &str,
) -> Result<(), String> {
    send_text_to_space_as(home_dir, None, space, markdown).await.map_err(|e| e.user_message(home_dir))
}

/// [`send_text_to_space`] as an employee's own Chat app when `agent` has one
/// (its app is the member of the spaces it talks in), else as the shared app.
pub async fn send_text_to_space_as(
    home_dir: &Path,
    agent: Option<&str>,
    space: &str,
    markdown: &str,
) -> Result<(), GoogleChatSendError> {
    if !crate::google_workspace::chat_read::valid_space(space) {
        return Err(GoogleChatSendError::InvalidTarget);
    }
    let (creds, member) = sending_creds(home_dir, agent, space).await
        .map_err(GoogleChatSendError::Configuration)?;
    let formatted = crate::markdown_render::to_googlechat_text(markdown);
    for chunk in crate::channel_format::split_text(&formatted, GCHAT_TEXT_CHUNK) {
        if let Err(status) = post_message(&creds, space, None, &chunk).await {
            // The registry picked an app that is no longer in the space
            // (403 / 404): forget it so the next send picks another (L-3).
            if let Some(member) = member.as_deref()
                && matches!(status, Some(403) | Some(404))
            {
                warn!(space, member, ?status, "Google Chat: removing inaccessible app from space roster");
                crate::agent_channels::gchat_spaces::forget(home_dir, space, member);
            }
            return Err(match status {
                Some(status) => GoogleChatSendError::Http { status },
                None => GoogleChatSendError::Transport,
            });
        }
    }
    Ok(())
}

/// The app a send uses. `agent` names the app the conversation came in
/// through: that employee's own app when it has one — and then a failure to
/// load it is an error, never a silent switch to the shared app (B11 M1).
/// Without it (or for an employee with no app) the space's recorded member
/// app decides, falling back to the shared app.
/// The second value is the registry member the app was picked by (`None`
/// when the caller named the app explicitly or nothing was recorded).
async fn sending_creds(
    home_dir: &Path,
    agent: Option<&str>,
    space: &str,
) -> Result<(Arc<GoogleChatCreds>, Option<String>), String> {
    use crate::agent_channels::gchat_spaces::{self, SHARED, SpaceApp};
    let (own, member) = match agent {
        Some(id) if crate::agent_channels::store::agent_has_googlechat_section(home_dir, id) => {
            (Some(id.to_string()), None)
        }
        _ => match gchat_spaces::app_for_space(home_dir, space) {
            Some(SpaceApp::Agent(id)) => (Some(id.clone()), Some(id)),
            Some(SpaceApp::Shared) => (None, Some(SHARED.to_string())),
            None => (None, None),
        },
    };
    let creds = match own {
        Some(id) => crate::googlechat_agents::agent_creds(home_dir, &id)
            .await
            .ok_or_else(|| format!("AI employee {id}'s own Google Chat app could not be loaded"))?,
        None => GoogleChatCreds::from_config(home_dir)
            .await
            .map(Arc::new)
            .ok_or_else(|| "Google Chat channel not configured".to_string())?,
    };
    Ok((creds, member))
}

/// spaces.messages.create — returns the created message `name` for edits.
async fn create_message(
    creds: &GoogleChatCreds,
    space: &str,
    thread: Option<&str>,
    text: &str,
) -> Option<String> {
    post_message(creds, space, thread, text).await.ok().flatten()
}

/// [`create_message`] keeping the failure's HTTP status (`None` for a
/// token or transport error).
async fn post_message(
    creds: &GoogleChatCreds,
    space: &str,
    thread: Option<&str>,
    text: &str,
) -> Result<Option<String>, Option<u16>> {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(e) => {
            error!("Google Chat token error: {e}");
            return Err(None);
        }
    };
    let mut body = serde_json::json!({ "text": text });
    let mut url = format!("{}/{space}/messages", creds.api_base());
    if let Some(th) = thread {
        body["thread"] = serde_json::json!({ "name": th });
        url.push_str("?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD");
    }
    match creds
        .http
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send_authorized()
        .await
    {
        Ok(resp) if resp.status().is_success() => Ok(resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))),
        Ok(resp) => {
            let status = resp.status();
            warn!(status = status.as_u16(), "Google Chat send failed");
            Err(Some(status.as_u16()))
        }
        Err(e) => {
            error!("Google Chat send error: {e}");
            Err(None)
        }
    }
}

// ── Inbound attachments ──────────────────────────────────────────────────────

/// How an inbound Chat attachment's bytes can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GchatAttachmentRef {
    /// Uploaded directly to Chat — downloadable via
    /// `GET /v1/media/{resourceName}?alt=media` with the bot's own `chat.bot`
    /// scope token.
    Uploaded { resource_name: String },
    /// A linked Google Drive file — downloading needs a Drive API scope the
    /// Chat service account is not granted, so callers degrade explicitly.
    Drive { file_id: String },
}

/// One parsed attachment from a Chat `MESSAGE` event.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GchatAttachment {
    /// `contentName` as sent by Chat; empty when absent.
    filename: String,
    content_type: String,
    reference: GchatAttachmentRef,
}

impl GchatAttachment {
    /// Filename to save under: `contentName`, or a MIME-derived fallback for
    /// nameless uploads so the extension-based skill router still keys on it.
    fn effective_filename(&self) -> String {
        let name = self.filename.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        format!(
            "file.{}",
            crate::media::extension_from_mime(&self.content_type)
        )
    }
}

/// Extract attachments from a Chat message JSON. The REST field is
/// `attachment[]` (singular key, array value); `attachments` is accepted
/// defensively. Entries with neither an `attachmentDataRef` nor a
/// `driveDataRef` are unactionable and skipped.
fn parse_gchat_attachments(message: &serde_json::Value) -> Vec<GchatAttachment> {
    let Some(arr) = message
        .get("attachment")
        .or_else(|| message.get("attachments"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|att| {
            let filename = att
                .get("contentName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content_type = att
                .get("contentType")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("application/octet-stream")
                .to_string();
            let uploaded = att
                .pointer("/attachmentDataRef/resourceName")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let drive = att
                .pointer("/driveDataRef/driveFileId")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let reference = if let Some(rn) = uploaded {
                GchatAttachmentRef::Uploaded {
                    resource_name: rn.to_string(),
                }
            } else if let Some(id) = drive {
                GchatAttachmentRef::Drive {
                    file_id: id.to_string(),
                }
            } else {
                return None;
            };
            Some(GchatAttachment {
                filename,
                content_type,
                reference,
            })
        })
        .collect()
}

/// Build the media-download URL for an uploaded attachment's `resourceName`.
/// The resource name is a slash-separated path segment
/// (`spaces/…/messages/…/attachments/…`) used verbatim in the URL path.
fn media_download_url(api_base: &str, resource_name: &str) -> String {
    format!(
        "{api_base}/media/{}?alt=media",
        resource_name.trim_start_matches('/')
    )
}

/// Download an uploaded attachment's bytes via the Chat media API using the
/// service-account token. Size-capped at `media::MAX_FILE_SIZE`; the URL goes
/// through the shared SSRF-validated downloader.
async fn download_media(creds: &GoogleChatCreds, resource_name: &str) -> Result<Vec<u8>, String> {
    let token = creds.get_token().await?;
    let url = media_download_url(&creds.api_base(), resource_name);
    let bearer = format!("Bearer {token}");
    crate::media::download_url(
        &url,
        Some(("Authorization", &bearer)),
        crate::media::MAX_FILE_SIZE as usize,
    )
    .await
}

/// spaces.messages.patch — edit a message's text in place.
pub(crate) async fn update_message(creds: &GoogleChatCreds, message_name: &str, text: &str) -> Result<(), PatchFailure> {
    let token = creds.get_token().await.map_err(|_| PatchFailure {
        status: None, retry_after: std::time::Duration::from_secs(1),
    })?;
    let url = format!("{}/{message_name}?updateMask=text", creds.api_base());
    let body = serde_json::json!({ "text": text });
    match creds.http.patch(&url).bearer_auth(&token).json(&body).send_authorized().await {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => {
            let status = resp.status().as_u16();
            let retry_after = patch_backoff(status, resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()));
            Err(PatchFailure { status: Some(status), retry_after })
        }
        Err(_) => Err(PatchFailure { status: None, retry_after: std::time::Duration::from_secs(1) }),
    }
}


/// Progress-callback filter: only events a text channel can render
/// (allowlist — Step / ModelInfo / Thinking are dashboard-only and would
/// otherwise reach the customer as an empty or unintended message).
/// Test-only routing of one service account's Chat API / JWKS to a local
/// fake, keyed by the account's `client_email` (parallel tests never share
/// an override).
#[cfg(test)]
pub(crate) mod test_routes {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn routes() -> &'static Mutex<HashMap<String, (String, String)>> {
        static R: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
        R.get_or_init(Default::default)
    }

    pub(crate) fn set(client_email: &str, api: &str, jwks: &str) {
        routes()
            .lock()
            .unwrap()
            .insert(client_email.into(), (api.into(), jwks.into()));
    }

    pub(crate) fn api(client_email: &str) -> Option<String> {
        routes().lock().unwrap().get(client_email).map(|r| r.0.clone())
    }

    pub(crate) fn jwks(client_email: &str) -> Option<String> {
        routes().lock().unwrap().get(client_email).map(|r| r.1.clone())
    }
}

pub(crate) fn forwards_progress(event: &crate::channel_reply::ProgressEvent) -> bool {
    event.is_channel_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploaded_attachment_line_uses_shared_format_and_history_parser() {
        let message = serde_json::json!({"attachment":[{
            "contentName":"季度報表.csv", "contentType":"text/csv",
            "attachmentDataRef":{"resourceName":"spaces/a/messages/b/attachments/c"}
        }]});
        let attachments = parse_gchat_attachments(&message);
        let att = &attachments[0];
        let filename = att.effective_filename();
        let mt = crate::media::media_type_from_mime(&att.content_type);
        let path = Path::new("/private/attachments/stored-report.csv");
        let line = crate::media::format_attachment_ref(&mt, &filename, path);
        assert_eq!(line, "[📎 季度報表.csv (file)](/private/attachments/stored-report.csv) 請用 csv_read／xlsx_read／file_read 讀取");
        let (display, references) = crate::chat_attachments::parse(&line);
        assert_eq!(references, vec![(filename, path.to_string_lossy().into_owned())]);
        assert!(!display.contains("/private/"));
        assert!(display.contains("csv_read／xlsx_read／file_read"));
    }

    #[test]
    fn inbound_links_use_json_array_even_with_trailer_delimiters() {
        let uri = "https://example.com/）extra?value=\"quoted\"&next=）";
        let message = serde_json::json!({"text":"title", "annotations":[
            {"type":"RICH_LINK", "richLinkMetadata":{"uri":uri}},
            {"type":"RICH_LINK", "richLinkMetadata":{"uri":"https://example.com/second"}},
            {"type":"RICH_LINK", "richLinkMetadata":{"uri":uri}}
        ]});
        let text = inbound_text(&message);
        let serialized = text.strip_prefix("title\n\n（訊息裡的預覽連結：").unwrap()
            .strip_suffix('）').unwrap();
        let links: Vec<String> = serde_json::from_str(serialized).unwrap();
        assert_eq!(links, vec![uri, "https://example.com/second"]);
        assert_eq!(serialized, serde_json::to_string(&links).unwrap());
    }

    #[test]
    fn drive_attachment_note_only_includes_valid_bounded_id() {
        assert!(drive_attachment_note("report", "Abc_123-xyz").contains("driveFileId: Abc_123-xyz"));
        assert!(drive_attachment_note("report", "Abc_123-xyz").contains("drive_read"));
        for id in ["", "abc\ninstructions", "../secret", "abc?x=y", "中文", &"a".repeat(257)] {
            let note = drive_attachment_note("report", id);
            assert!(!note.contains("driveFileId:"), "{note}");
            assert!(!note.contains("scope not granted"));
        }
    }

    #[tokio::test]
    async fn proactive_rejects_invalid_target_before_credentials_or_network() {
        let home = tempfile::tempdir().unwrap();
        for target in ["users/123", "a@example.com", "spaces/a/messages", "spaces/a?x=y", "spaces/..", "spaces/a\n", "spaces/中文"] {
            assert_eq!(send_text_to_space_as(home.path(), None, target, "hello").await,
                Err(GoogleChatSendError::InvalidTarget));
        }
    }

    #[test]
    fn proactive_errors_explain_membership_and_temporary_failures() {
        for status in [403, 404] {
            let error = GoogleChatSendError::Http { status };
            let text = error.to_string();
            for guidance in ["先私訊此應用程式一次", "請管理員為對方安裝", "將應用程式加入該聊天室"] {
                assert!(text.contains(guidance));
            }
            let en = send_error_text(status, crate::trigger_route::texts::Lang::En);
            for guidance in ["DM the app once", "administrator to install", "add it to the space"] {
                assert!(en.contains(guidance));
            }
            let ja = send_error_text(status, crate::trigger_route::texts::Lang::Ja);
            for guidance in ["一度アプリへ DM", "管理者", "インストール", "スペースへの追加"] {
                assert!(ja.contains(guidance));
            }
            assert_eq!(error, GoogleChatSendError::Http { status });
        }
        for status in [429, 500, 502, 503, 599] {
            assert!(GoogleChatSendError::Http { status }.to_string().contains("稍後再試"));
        }
        assert!(!GoogleChatSendError::Http { status: 400 }.to_string().contains("尚未"));
    }

    #[test]
    fn googlechat_dedupe_only_records_accepted_messages() {
        let key = format!("test:{}", uuid::Uuid::new_v4());
        assert!(!enqueue_once(Some(&key), || false));
        assert!(enqueue_once(Some(&key), || true));
        assert!(enqueue_once(Some(&key), || panic!("duplicate submitted twice")));
        assert!(enqueue_once(Some("different-app-test"), || true));
    }

    use serde_json::json;

    #[test]
    fn parses_uploaded_attachment_ref() {
        let message = json!({
            "text": "here you go",
            "attachment": [{
                "name": "spaces/AAA/messages/BBB/attachments/CCC",
                "contentName": "report.pdf",
                "contentType": "application/pdf",
                "source": "UPLOADED_CONTENT",
                "attachmentDataRef": { "resourceName": "spaces/AAA/messages/BBB/attachments/CCC" }
            }]
        });
        let atts = parse_gchat_attachments(&message);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].filename, "report.pdf");
        assert_eq!(atts[0].content_type, "application/pdf");
        assert_eq!(
            atts[0].reference,
            GchatAttachmentRef::Uploaded {
                resource_name: "spaces/AAA/messages/BBB/attachments/CCC".into()
            }
        );
    }

    #[test]
    fn parses_drive_attachment_ref() {
        let message = json!({
            "attachment": [{
                "contentName": "budget.xlsx",
                "source": "DRIVE_FILE",
                "driveDataRef": { "driveFileId": "drive-id-123" }
            }]
        });
        let atts = parse_gchat_attachments(&message);
        assert_eq!(atts.len(), 1);
        // No contentType in the event → octet-stream default.
        assert_eq!(atts[0].content_type, "application/octet-stream");
        assert_eq!(
            atts[0].reference,
            GchatAttachmentRef::Drive {
                file_id: "drive-id-123".into()
            }
        );
    }

    #[test]
    fn skips_unactionable_and_accepts_plural_key() {
        // Neither attachmentDataRef nor driveDataRef → skipped; the plural
        // "attachments" key is accepted defensively.
        let message = json!({
            "attachments": [
                { "contentName": "ghost.bin" },
                {
                    "contentName": "ok.png",
                    "contentType": "image/png",
                    "attachmentDataRef": { "resourceName": "spaces/S/messages/M/attachments/A" }
                }
            ]
        });
        let atts = parse_gchat_attachments(&message);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].filename, "ok.png");
        // No attachment field at all → empty, no panic.
        assert!(parse_gchat_attachments(&json!({ "text": "hi" })).is_empty());
        // Empty resourceName → unactionable.
        let empty_ref = json!({
            "attachment": [{
                "contentName": "x.pdf",
                "attachmentDataRef": { "resourceName": "  " }
            }]
        });
        assert!(parse_gchat_attachments(&empty_ref).is_empty());
    }

    #[test]
    fn media_download_url_is_wellformed() {
        assert_eq!(
            media_download_url(CHAT_API, "spaces/A/messages/B/attachments/C"),
            "https://chat.googleapis.com/v1/media/spaces/A/messages/B/attachments/C?alt=media"
        );
        // A leading slash must not produce a double slash.
        assert_eq!(
            media_download_url(CHAT_API, "/spaces/A/messages/B/attachments/C"),
            "https://chat.googleapis.com/v1/media/spaces/A/messages/B/attachments/C?alt=media"
        );
    }

    #[test]
    fn effective_filename_falls_back_to_mime_extension() {
        let named = GchatAttachment {
            filename: "photo.jpg".into(),
            content_type: "image/jpeg".into(),
            reference: GchatAttachmentRef::Uploaded {
                resource_name: "r".into(),
            },
        };
        assert_eq!(named.effective_filename(), "photo.jpg");
        let nameless = GchatAttachment {
            filename: "".into(),
            content_type: "image/png".into(),
            reference: GchatAttachmentRef::Uploaded {
                resource_name: "r".into(),
            },
        };
        assert_eq!(nameless.effective_filename(), "file.png");
        let unknown = GchatAttachment {
            filename: "  ".into(),
            content_type: "application/x-mystery".into(),
            reference: GchatAttachmentRef::Drive {
                file_id: "d".into(),
            },
        };
        assert_eq!(unknown.effective_filename(), "file.bin");
    }

    // ── WP-8A / credentials doctrine P2 ─────────────────────────────────

    const TEST_SA_JSON: &str = r#"{
        "client_email": "bot@my-project.iam.gserviceaccount.com",
        "private_key": "-----BEGIN PRIVATE KEY-----\nFAKE\n-----END PRIVATE KEY-----\n",
        "token_uri": "https://oauth2.googleapis.com/token"
    }"#;

    #[test]
    fn parse_service_account_json_extracts_the_three_fields() {
        let (email, key, uri) =
            GoogleChatCreds::parse_service_account_json(TEST_SA_JSON).expect("valid SA JSON");
        assert_eq!(email, "bot@my-project.iam.gserviceaccount.com");
        assert!(key.contains("FAKE"));
        assert_eq!(uri, "https://oauth2.googleapis.com/token");
    }

    #[test]
    fn parse_service_account_json_defaults_token_uri_when_absent() {
        let json = r#"{"client_email":"e","private_key":"k"}"#;
        let (_, _, uri) = GoogleChatCreds::parse_service_account_json(json).unwrap();
        assert_eq!(uri, "https://oauth2.googleapis.com/token");
    }

    #[test]
    fn parse_service_account_json_rejects_missing_fields_and_garbage() {
        assert!(GoogleChatCreds::parse_service_account_json("{}").is_none());
        assert!(
            GoogleChatCreds::parse_service_account_json(r#"{"client_email":"e"}"#).is_none(),
            "missing private_key"
        );
        assert!(GoogleChatCreds::parse_service_account_json("not json").is_none());
    }

    /// `get_token`'s refresh path re-reads the service-account JSON from
    /// `home_dir` fresh (WP-8A) — this pins down the primitive it depends on:
    /// two resolves against a changed config see different credentials, with
    /// no process-lifetime caching of the key material itself (only the
    /// derived OAuth token is cached, on its own TTL).
    #[tokio::test]
    async fn from_config_reflects_a_rotated_service_account_key_without_any_cache() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // `encrypt_value` only cares about the plaintext bytes — the JSON's
        // own internal newlines (both the pretty-printed structure and the
        // PEM key's `\n` escapes) survive the round trip untouched; no
        // TOML-unsafe characters ever land in config.toml because only the
        // base64 ciphertext is written there.
        let enc = crate::config_crypto::encrypt_value(TEST_SA_JSON, home).unwrap();
        tokio::fs::write(
            home.join("config.toml"),
            format!("[channels]\ngooglechat_service_account_json_enc = \"{enc}\"\n"),
        )
        .await
        .unwrap();

        let creds1 = GoogleChatCreds::from_config(home)
            .await
            .expect("configured");
        assert_eq!(
            creds1.client_email,
            "bot@my-project.iam.gserviceaccount.com"
        );

        // Rotate to a different service account.
        let rotated_json = TEST_SA_JSON.replace("bot@my-project", "rotated-bot@my-project");
        let enc2 = crate::config_crypto::encrypt_value(&rotated_json, home).unwrap();
        tokio::fs::write(
            home.join("config.toml"),
            format!("[channels]\ngooglechat_service_account_json_enc = \"{enc2}\"\n"),
        )
        .await
        .unwrap();

        let creds2 = GoogleChatCreds::from_config(home)
            .await
            .expect("still configured");
        assert_eq!(
            creds2.client_email, "rotated-bot@my-project.iam.gserviceaccount.com",
            "a fresh from_config must see the rotated key, not a cached first read"
        );
    }
}

/// Regression guard for the anonymous-sender CCR leak: this adapter used to
/// pass its `"unknown"` placeholder straight into the reply pipeline's
/// `user_id`, so every sender the webhook could not identify hashed to the
/// same `source_acl` and could retrieve the others' saved tool originals.
#[cfg(test)]
mod ccr_principal_tests {
    use crate::ccr_runtime::source_scan::call_args_at;
    use crate::ccr_runtime::{reply_principal_for_sender, source_acl_for_principal};

    const SRC: &str = include_str!("googlechat.rs");
    const AGENT: &str = "agent-a";
    const SESSION: &str = "googlechat:spaces/AAA";

    /// Structural: every `build_guarded_reply_with_session` call in this file
    /// must launder its principal. Checked over the real source because the
    /// call site lives inside a long async webhook handler that cannot be
    /// driven from a unit test.
    #[test]
    fn every_guarded_reply_call_launders_the_ccr_principal() {
        let args = call_args_at(SRC, "build_guarded_reply_with_session(", 3);
        assert!(
            !args.is_empty(),
            "no guarded-reply call found — did the call site move?"
        );
        for arg in args {
            assert!(
                arg.starts_with("crate::ccr_runtime::reply_principal_for_sender("),
                "the CCR principal argument must be laundered, found `{arg}`"
            );
        }
    }

    #[test]
    fn an_unidentified_sender_disables_ccr_instead_of_sharing_one_scope() {
        let anonymous = reply_principal_for_sender("unknown");
        assert!(anonymous.is_empty());
        assert!(
            source_acl_for_principal(AGENT, SESSION, anonymous).is_none(),
            "an unidentified sender must disable CCR, never pool into one scope"
        );

        let alice = reply_principal_for_sender("users/alice");
        let bob = reply_principal_for_sender("users/bob");
        assert_ne!(
            source_acl_for_principal(AGENT, SESSION, alice).unwrap(),
            source_acl_for_principal(AGENT, SESSION, bob).unwrap()
        );
    }
}

#[cfg(test)]
#[path = "googlechat_command_access_tests.rs"]
mod command_access_tests;

#[cfg(test)]
mod access_notice_tests {
    use super::*;
    use serde_json::json;

    fn test_state(home: &Path) -> Arc<GoogleChatState> {
        let registry = Arc::new(RwLock::new(duduclaw_agent::AgentRegistry::new(home.join("agents"))));
        let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
        let status: crate::channel_reply::ChannelStatusMap =
            Arc::new(RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let ctx = Arc::new(ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx));
        // A candidate key never signs, so the spawned notice fails locally
        // without any network access.
        let creds = GoogleChatCreds::candidate(
            r#"{"client_email":"queue-test@example.invalid","private_key":"not-a-key"}"#,
        )
        .unwrap();
        Arc::new(GoogleChatState { ctx, creds: Arc::new(creds), agent: None })
    }

    #[test]
    fn only_a_single_user_dm_is_one_to_one() {
        use crate::googlechat_space_kind::{SpaceKind, carried_space, classify_space};
        let msg = |space: serde_json::Value| json!({ "space": space });
        let none = json!({});
        let kind = |m: &serde_json::Value, e: &serde_json::Value| classify_space(&carried_space(m, e));
        assert_eq!(kind(&msg(json!({"type":"DM","singleUserBotDm":true})), &none), SpaceKind::OneToOneDm);
        // The event-level space is consulted when the message lacks it.
        assert_eq!(
            kind(&json!({}), &json!({"space":{"spaceType":"DIRECT_MESSAGE","singleUserBotDm":true}})),
            SpaceKind::OneToOneDm
        );
        for space in [
            json!({"type":"ROOM"}),
            json!({"type":"SPACE"}),
            json!({"spaceType":"GROUP_CHAT"}),
            json!({"spaceType":"SPACE","type":"DM"}),
            json!({"type":"DM","singleUserBotDm":false}),
        ] {
            assert_eq!(kind(&msg(space.clone()), &none), SpaceKind::Shared, "{space}");
        }
        // Not stated → resolved by spaces.get (googlechat_space_kind tests).
        for space in [json!({"spaceType":"DIRECT_MESSAGE"}), json!({"type":"DM"}), json!({"name":"spaces/A"})] {
            assert_eq!(kind(&msg(space.clone()), &none), SpaceKind::Unknown, "{space}");
        }
    }

    #[test]
    fn command_space_type_uses_the_resolver() {
        // Structural: the command intercept takes `shared` from the resolver
        // (event fields, then spaces.get), on both transports.
        let src = include_str!("googlechat.rs");
        let resolve = src.find("crate::googlechat_space_kind::is_one_to_one(").unwrap();
        let origin = src.find("shared: !one_to_one,").unwrap();
        assert!(resolve < origin);
        let pubsub = include_str!("googlechat_pubsub.rs");
        assert!(pubsub.contains("crate::googlechat::process_pubsub_event(&st, &event)"));
    }

    #[test]
    fn queue_keys_match_both_transports() {
        let (group, conversation) = queue_keys(Some("sales"), "spaces/A", "users/x");
        let agent: Option<String> = Some("sales".into());
        assert_eq!(group, json!([agent, "spaces/A"]).to_string());
        assert_eq!(conversation, json!([agent, "spaces/A", "users/x"]).to_string());
        assert_eq!(queue_keys(None, "spaces/A", "u").0, json!([null, "spaces/A"]).to_string());
    }

    #[tokio::test]
    async fn queue_full_notice_is_throttled_per_sender_and_space() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (group, alice) = queue_keys(None, "spaces/A", "users/alice");
        let (_, bob) = queue_keys(None, "spaces/A", "users/bob");
        let (other_group, carol) = queue_keys(None, "spaces/B", "users/carol");
        assert!(notify_queue_full(&state, &group, &alice, "spaces/A", None));
        // Same sender again, and another sender in the same space within 10 s.
        assert!(!notify_queue_full(&state, &group, &alice, "spaces/A", None));
        assert!(!notify_queue_full(&state, &group, &bob, "spaces/A", Some("spaces/A/threads/t")));
        // Another space is throttled separately.
        assert!(notify_queue_full(&state, &other_group, &carol, "spaces/B", None));
    }

    #[test]
    fn pubsub_queue_full_uses_the_shared_notice() {
        let src = include_str!("googlechat_pubsub.rs");
        assert!(src.contains("crate::googlechat::notify_queue_full(state, &group, &conversation"));
        assert!(src.contains("crate::googlechat::queue_keys("));
    }

    #[test]
    fn failed_final_delivery_is_recorded_without_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = DeliveryTarget { home: dir.path(), space: "spaces/A", agent: Some("sales") };
        report_delivery(target, Ok(()));
        assert!(!dir.path().join("channel_failures.jsonl").exists());
        report_delivery(target, Err(Some(403)));
        report_delivery(DeliveryTarget { agent: None, ..target }, Err(None));
        let raw = std::fs::read_to_string(dir.path().join("channel_failures.jsonl")).unwrap();
        let lines: Vec<serde_json::Value> =
            raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], SEND_FAILED_EVENT);
        assert_eq!(lines[0]["channel"], "googlechat");
        assert_eq!(lines[0]["status"], 403);
        assert_eq!(lines[0]["agent"], "sales");
        assert!(lines[1].get("agent").is_none());
        assert!(lines[1]["status"].is_null());
        for line in &lines {
            for key in ["text", "reply", "message", "content"] {
                assert!(line.get(key).is_none(), "{line}");
            }
        }
        assert!(crate::channel_alerts::SEND_FAILURE_EVENTS.contains(&SEND_FAILED_EVENT));
    }

    #[test]
    fn commands_run_behind_the_access_gate() {
        // Structural: the command intercept must run the shared preflight
        // (access gate + shared-space rule) before any command handler.
        let src = include_str!("googlechat.rs");
        let preflight = src.find("chat_commands::command_preflight(&cmd").unwrap();
        let handler = src.find("chat_commands::handle_command_for_sender(").unwrap();
        assert!(preflight < handler);
    }
}

#[cfg(test)]
mod delivery_display_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn final_take_waits_for_progress_and_blocks_late_updates() {
        let slot: Placeholder = Arc::new(tokio::sync::Mutex::new(Some("message".into())));
        let progress = slot.lock().await;
        let final_slot = slot.clone();
        let final_send = tokio::spawn(async move { final_slot.lock().await.take() });
        tokio::task::yield_now().await;
        assert!(!final_send.is_finished());
        drop(progress);
        assert_eq!(final_send.await.unwrap().as_deref(), Some("message"));
        assert!(slot.lock().await.is_none());
    }

    #[tokio::test]
    async fn failed_patch_retries_then_posts_and_clears() {
        let order = std::sync::Mutex::new(Vec::new());
        let result = replace_placeholder("old", || async {
            order.lock().unwrap().push("patch");
            Err(PatchFailure { status: Some(429), retry_after: std::time::Duration::ZERO })
        }, || async {
            order.lock().unwrap().push("post");
            Ok(Some("new".into()))
        }, || async {
            order.lock().unwrap().push("clear");
            Ok(())
        }).await;
        assert_eq!(result.unwrap().as_deref(), Some("new"));
        assert_eq!(*order.lock().unwrap(), ["patch", "patch", "post", "clear"]);
    }

    #[tokio::test]
    async fn successful_retry_avoids_duplicate_and_failed_post_is_reported() {
        let calls = AtomicUsize::new(0);
        let result = replace_placeholder("old", || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(PatchFailure { status: Some(503), retry_after: std::time::Duration::ZERO })
            } else { Ok(()) }
        }, || async { panic!("must not post after successful PATCH") },
        || async { panic!("must not clear after successful PATCH") }).await;
        assert_eq!(result.unwrap().as_deref(), Some("old"));
        let result = replace_placeholder("old", || async {
            Err(PatchFailure { status: Some(500), retry_after: std::time::Duration::ZERO })
        }, || async { Err(Some(403)) }, || async { Ok(()) }).await;
        assert_eq!(result, Err(Some(403)));
    }

    #[test]
    fn retry_after_is_bounded() {
        assert_eq!(patch_backoff(429, Some("60")).as_secs(), 3);
        assert_eq!(patch_backoff(429, Some("2")).as_secs(), 2);
        assert_eq!(patch_backoff(429, Some("invalid")).as_secs(), 1);
        assert_eq!(patch_backoff(503, Some("60")).as_secs(), 1);
    }
}

#[cfg(all(test, unix))]
mod placeholder_lifecycle_tests {
    use super::*;
    use crate::agent_channels::{e2e_gchat_tests as chat, e2e_tests, e2e_seam};
    use crate::googlechat_inbox::{Inbox, PendingPlaceholder, PlaceholderRegistration};

    #[tokio::test]
    async fn lost_guard_retires_placeholder_at_delivery_and_cancellation_preserves_it() {
        let home = tempfile::tempdir().unwrap();
        let fake = chat::start_fake().await;
        let email = format!("guard-{}@test.invalid", uuid::Uuid::new_v4().simple());
        let sa = chat::service_account(&fake, &email);
        crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
            &chat::googlechat_section(home.path(), "444444443", &sa));
        let ctx = e2e_tests::reply_ctx(home.path()).await;
        e2e_seam::arm(home.path());
        let creds = crate::googlechat_agents::agent_creds(home.path(), "sales").await.unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        let name = create_message(&creds, "spaces/GUARD", None, "🤔 思考中…").await.unwrap();
        let row = PendingPlaceholder {
            message_name: name.clone(), space: "spaces/GUARD".into(), app_agent: Some("sales".into()),
            app_email: email.clone(), employee_id: "sales".into(), session_id: "googlechat:spaces/GUARD".into(),
            thread_name: None, created_at: chrono::Utc::now().timestamp() - 10,
        };
        let registration = PlaceholderRegistration::new(store.clone(), &row).unwrap();
        assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap(), vec![row.clone()]);
        let guarded = crate::channel_reply::build_guarded_reply_for_agent(
            "hello", &ctx, "sales", &row.session_id, "users/42", None).await;
        guarded.refuse_delivery();
        let placeholder = Arc::new(tokio::sync::Mutex::new(Some(name)));
        deliver_reply(&creds, &row.space, None, &placeholder, Some(&registration), "SECRET ANSWER", Some(&guarded)).await.unwrap();
        // Retirement happens inside delivery, before registration itself is dropped.
        assert!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap().is_empty());
        let text = chat::texts_with_bearer(&fake, &format!("Bearer tok-{email}")).join("\n");
        assert!(text.contains(crate::channel_reply::CCR_DELIVERY_REFUSED_TEXT));
        assert!(!text.contains("SECRET ANSWER"));
        drop(registration);
        let cancelled = PlaceholderRegistration::new(store.clone(), &row).unwrap();
        drop(cancelled);
        assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap(), vec![row]);
    }
}
