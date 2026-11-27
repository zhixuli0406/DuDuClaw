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

fn bound_chat_type(verified_http: bool, is_dm: bool) -> Option<&'static str> {
    (verified_http && is_dm).then_some("jwt_http_dm")
}

fn handshake_text(event: &serde_json::Value) -> &str {
    event.pointer("/message/argumentText").or_else(|| event.pointer("/message/text"))
        .and_then(serde_json::Value::as_str).unwrap_or("")
}

fn handshake_route(event: &serde_json::Value, verified_http: bool) -> crate::chat_link_intercept::CodeRoute {
    let principal = event.pointer("/message/sender/name").and_then(serde_json::Value::as_str);
    let human = event.pointer("/message/sender/type").and_then(serde_json::Value::as_str) == Some("HUMAN");
    let valid_principal = principal.is_some_and(|name|
        crate::googlechat_identity::normalize_user_id(name).as_deref() == Some(name));
    crate::chat_link_intercept::route(handshake_text(event), verified_http && human && valid_principal
        && event.get("type").and_then(serde_json::Value::as_str) == Some("MESSAGE"))
}

/// Pub/Sub has no handshake or decision authority. Drop before queue admission
/// and repeat at the processing boundary for direct/internal callers.
pub(crate) fn pubsub_control_message(event: &serde_json::Value) -> bool {
    handshake_route(event, false) != crate::chat_link_intercept::CodeRoute::Conversation
        || crate::channel_decision_route::parse_strict_decision(handshake_text(event)).is_some()
        || event.get("type").and_then(serde_json::Value::as_str) == Some("CARD_CLICKED")
}

/// Called only by dispatch_event, after the shared/employee HTTP entry verifies
/// the JWT. Return a synchronous plain-text reply, never a card or model turn.
async fn http_handshake_intercept(
    state: &GoogleChatState, event: &serde_json::Value,
) -> Option<axum::response::Response> {
    use crate::chat_link_intercept::{self, CodeRoute};
    let ack = || (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response();
    match handshake_route(event, true) {
        CodeRoute::Conversation => return None,
        CodeRoute::Consume => return Some(ack()),
        CodeRoute::Match => {},
    }
    let space = event_space(event);
    if !crate::google_workspace::chat_read::valid_space(&space)
        || ["/space/name", "/message/space/name"].iter().any(|path|
            event.pointer(path).is_some_and(|name| name.as_str() != Some(space.as_str()))) {
        return Some(ack());
    }
    // No conversation/thread is created just to classify a handshake DM.
    let is_private = crate::googlechat_space_kind::is_one_to_one(
        &state.creds, &space, &event["message"], event,
    ).await;
    let principal = event.pointer("/message/sender/name").and_then(serde_json::Value::as_str)
        .unwrap_or_default().to_owned();
    let display = event.pointer("/message/sender/displayName").and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let reply = chat_link_intercept::handle_at_home(
        &state.ctx.home_dir, "googlechat", chat_link_intercept::account_label("googlechat", state.agent.as_deref()),
        principal, space, duduclaw_core::truncate_chars(display, 200), is_private, handshake_text(event),
    ).await;
    Some(match reply {
        Some(Some(text)) => (StatusCode::OK, axum::Json(serde_json::json!({"text": text}))).into_response(),
        _ => ack(),
    })
}

/// Only the JWT-verified HTTP entry point calls this. Pub/Sub never records
/// a decision DM or invokes the bound decision fastlane.
async fn http_dm_decision(state: &Arc<GoogleChatState>, event: &serde_json::Value, conversation: &crate::googlechat_conversation::GoogleChatConversation) -> Option<Result<String, String>> {
    let space = conversation.space().to_owned();
    let principal = event.pointer("/message/sender/name").and_then(serde_json::Value::as_str)?;
    let is_dm = conversation.is_dm();
    let text = event.pointer("/message/argumentText").or_else(|| event.pointer("/message/text"))
        .and_then(serde_json::Value::as_str).unwrap_or("");
    if is_dm {
        if let Ok(Some(db)) = crate::decision_notify::open_user_db(&state.ctx.home_dir) {
            let _ = db.record_googlechat_dm(state.creds.client_email(), &space, principal);
        }
    }
    crate::channel_decision_route::parse_strict_decision(text)?;
    if !is_dm { return Some(Err(crate::channel_decision_route::DECISION_REFUSED.into())); }
    let thread = conversation.delivery_thread();
    let context = crate::approval::DecisionContext {
        channel: "googlechat".into(), account_id: state.creds.client_email().to_string(),
        conversation_id: thread.map_or_else(|| space.clone(), |t| format!("{space}:{t}")),
        principal_id: principal.to_string(),
    };
    crate::ccr_runtime::ATTESTED_ONE_TO_ONE.scope(
        crate::ccr_runtime::OneToOneAttestation::googlechat_dm(&space, principal),
        crate::decision_notify::route_trusted_decision_fastlane_with_scope(&state.ctx, &context, text,
            crate::decision_notify::DecisionAccessScope { channel_id: None, guild_id: None, session_id: None }),
    ).await
}

#[cfg(test)]
pub(crate) async fn process_pubsub_event(state: &Arc<GoogleChatState>, event: &serde_json::Value) {
    if pubsub_control_message(event) { return; }
    if let Ok(conversation) = resolve_conversation(state, event).await {
        process_pubsub_conversation(state, event, &conversation).await;
    }
}

/// Only the Pub/Sub worker supplies this event, rebuilt from spaces.messages.get.
pub(crate) async fn process_pubsub_conversation(state: &Arc<GoogleChatState>, event: &serde_json::Value, conversation: &crate::googlechat_conversation::GoogleChatConversation) {
    if pubsub_control_message(event) { return; }
    let space = event_space(event);
    crate::agent_channels::gchat_spaces::record(&state.ctx.home_dir, &space, state.agent.as_deref());
    handle_message(state, event, conversation, false).await;
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
    // RM-143 control-plane exemption: before space recording, conversation
    // resolution, decisions, access/pairing replies, and model queue admission.
    if let Some(response) = http_handshake_intercept(&state, &event).await {
        return response;
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
            // One thread is processed in order, including different senders.
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
            let verified_conversation = match resolve_conversation(&state, &event).await {
                Ok(conversation) => conversation,
                Err(_) => {
                    warn!("Google Chat: invalid conversation refused");
                    return (StatusCode::BAD_REQUEST, "bad request").into_response();
                }
            };
            // Decisions must bypass the per-conversation model queue: the
            // running turn may itself be waiting for this exact approval.
            if let Some(result) = http_dm_decision(&state, &event, &verified_conversation).await {
                let text = result.unwrap_or_else(|e| e);
                return (StatusCode::OK, axum::Json(serde_json::json!({"text": text}))).into_response();
            }
            let st = state.clone();
            let queued = event.clone();
            let queued_conversation = verified_conversation.clone();
            let (group, conversation) = verified_conversation.queue_keys(state.agent.as_deref());
            let message_name = event.pointer("/message/name").and_then(|v| v.as_str()).filter(|name| !name.is_empty() && name.len() <= 1024);
            let dedupe_key = message_name.map(|name| serde_json::json!([
                state.ctx.home_dir, state.agent, state.creds.client_email(), name
            ]).to_string());
            let accepted = enqueue_once(dedupe_key.as_deref(), || state.ctx.googlechat_queue
                .submit_in(&group, &conversation, async move { handle_message(&st, &queued, &queued_conversation, true).await }).is_ok());
            if !accepted
            {
                notify_queue_full(&state, &group, &conversation, &space, verified_conversation.delivery_thread());
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
        // RM-192: a decision button press. Only the HTTP transport reaches
        // this function, and both of its callers (`webhook_handler`,
        // `googlechat_agents::handle`) verified the request's JWT first; the
        // Pub/Sub worker handles `MESSAGE` only and never calls it.
        "CARD_CLICKED" => card_clicked(&state, &event).await,
        _ => (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response(),
    }
}

/// One decision button press, as read from a verified `CARD_CLICKED` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardPress {
    /// The space the click arrived in.
    pub space: String,
    /// The HUMAN presser, exactly `users/<digits>`.
    pub user: String,
    /// The `decision_action::encode(..)` the button carried (Goal or Approval).
    pub action: String,
}

/// Read a decision press from a `CARD_CLICKED` event, or `None` for
/// anything else: a non-human or malformed presser, a malformed or
/// inconsistent space, another function, a missing, repeated or
/// disagreeing decision parameter, or a source other than Goal or Approval.
pub(crate) fn parse_card_press(event: &serde_json::Value) -> Option<CardPress> {
    use crate::channel_format::{GOOGLECHAT_DECISION_FUNCTION, GOOGLECHAT_DECISION_PARAM};
    use serde_json::Value;
    if event.pointer("/user/type").and_then(Value::as_str) != Some("HUMAN") {
        return None;
    }
    let user = event.pointer("/user/name").and_then(Value::as_str)?;
    if crate::googlechat_identity::normalize_user_id(user).as_deref() != Some(user) {
        return None;
    }
    let space = event.pointer("/space/name").and_then(Value::as_str)?;
    if !crate::google_workspace::chat_read::valid_space(space) {
        return None;
    }
    if let Some(other) = event.pointer("/message/space/name") {
        if other.as_str() != Some(space) {
            return None;
        }
    }
    // The two space objects must not contradict each other (RM-192 review):
    // a press whose `message.space` and `space` disagree is refused, so it
    // can never reach the DM destination match on a mixed proof.
    if crate::googlechat_space_kind::carried_spaces_conflict(space, &event["message"], event) {
        return None;
    }
    let functions: Vec<&str> = ["/common/invokedFunction", "/action/actionMethodName"]
        .iter()
        .filter_map(|p| event.pointer(p).and_then(Value::as_str))
        .collect();
    if functions.is_empty() || functions.iter().any(|f| *f != GOOGLECHAT_DECISION_FUNCTION) {
        return None;
    }
    let mut values: Vec<&str> = Vec::new();
    if let Some(params) = event.pointer("/action/parameters") {
        for p in params.as_array()? {
            if p.get("key").and_then(Value::as_str) == Some(GOOGLECHAT_DECISION_PARAM) {
                values.push(p.get("value").and_then(Value::as_str)?);
            }
        }
    }
    let from_action = values.len();
    if let Some(params) = event.pointer("/common/parameters") {
        if let Some(value) = params.as_object()?.get(GOOGLECHAT_DECISION_PARAM) {
            values.push(value.as_str()?);
        }
    }
    let action = *values.first()?;
    if from_action > 1 || values.iter().any(|v| *v != action) || action.len() > 512 {
        return None;
    }
    let decoded = crate::decision_action::parse(action)?;
    if !matches!(decoded.source, crate::decision_action::DecisionSource::Goal
        | crate::decision_action::DecisionSource::Approval) {
        return None;
    }
    Some(CardPress { space: space.to_string(), user: user.to_string(), action: action.to_string() })
}

/// Apply a verified decision press and answer it in the space. Anything
/// unreadable or unknown is acknowledged with an empty 200 and changes
/// nothing (no detail goes back to the caller).
async fn card_clicked(
    state: &Arc<GoogleChatState>,
    event: &serde_json::Value,
) -> axum::response::Response {
    let ack = || (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response();
    let Some(press) = parse_card_press(event) else {
        info!("Google Chat: CARD_CLICKED without a usable decision ignored");
        return ack();
    };
    let Some(outcome) = route_card_press(state, event, &press).await else { return ack(); };
    (StatusCode::OK, axum::Json(card_press_response(&press, &outcome))).into_response()
}

fn card_press_response(
    _press: &CardPress,
    outcome: &crate::approval::chat_admin::press::PressOutcome,
) -> serde_json::Value {
    if let Some(text) = &outcome.card_text {
        // Non-add-on HTTP apps return a Message, not a Workspace add-on action.
        // UPDATE_MESSAGE is only valid for CARD_CLICKED on the app's BOT message:
        // https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages#ResponseType
        // The receipt binds that exact message to the receiving app. An empty
        // cardsV2 removes its buttons; never copy event text/cards into the reply.
        let cards = outcome.card_link.as_ref().map(|link| serde_json::json!([{
            "cardId":"approval-step-up", "card":{"sections":[{"widgets":[{"buttonList":{
                "buttons":[{"text":link.label,"onClick":{"openLink":{"url":link.url}}}]
            }}]}]}
        }])).unwrap_or_else(|| serde_json::json!([]));
        serde_json::json!({"actionResponse":{"type":"UPDATE_MESSAGE"}, "text":text, "cardsV2":cards})
    } else {
        let reply = if outcome.refused_reason == Some("legacy_refused") {
            format!("⚠️ {}", outcome.reply)
        } else { outcome.reply.clone() };
        serde_json::json!({"text":reply})
    }
}

fn card_press_origin(
    state: &GoogleChatState,
    event: &serde_json::Value,
    press: &CardPress,
    is_dm: bool,
) -> crate::approval::chat_admin::press::PressOrigin {
    // Non-add-on CARD_CLICKED carries the clicked message at event.message.
    // Keep its resource name verbatim: no aliases or callback-supplied IDs.
    let message_id = event.pointer("/message/name").and_then(serde_json::Value::as_str)
        .filter(|name| crate::googlechat_inbox::parse_message_resource_name(name)
            .is_some_and(|(space, _)| format!("spaces/{space}") == press.space))
        .map(str::to_owned);
    crate::approval::chat_admin::press::PressOrigin {
        message_id, private: is_dm,
        account: crate::chat_link_intercept::account_label("googlechat", state.agent.as_deref()),
        dm: press.space.clone(),
    }
}

async fn route_card_press(
    state: &Arc<GoogleChatState>,
    event: &serde_json::Value,
    press: &CardPress,
) -> Option<crate::approval::chat_admin::press::PressOutcome> {
    // Research §9: Chat user ID == OIDC sub, ID stability after rename/recreate,
    // and user.email presence are not documented and need live testing. This
    // adapter uses only the verified HUMAN user.name and receipt/handshake proof;
    // it neither infers a sub nor uses email/displayName as identity.
    if crate::decision_action::parse(&press.action)
        .is_some_and(|action| action.source == crate::decision_action::DecisionSource::Approval)
        && event.pointer("/message/sender/type").and_then(serde_json::Value::as_str) != Some("BOT")
    {
        return Some(crate::approval::chat_admin::press::PressOutcome::refused(
            crate::channel_decision_route::DECISION_REFUSED.into(), "message_not_bot", false));
    }
    // RM-192 B3: the trusted classifier decides whether this is the
    // presser's 1:1 DM with the app (unknown or failed = shared). Only then
    // is the press attested as coming from that DM's human member; the goal
    // gate compares it with the space the card was sent to.
    let is_dm = crate::googlechat_space_kind::is_one_to_one(
        &state.creds,
        &press.space,
        &event["message"],
        event,
    )
    .await;
    let attestation = if is_dm {
        crate::ccr_runtime::OneToOneAttestation::googlechat_dm(&press.space, &press.user)
    } else {
        None
    };
    let context = crate::approval::DecisionContext {
        channel: crate::googlechat_identity::CHANNEL.into(),
        account_id: state.creds.client_email().to_string(),
        conversation_id: press.space.clone(),
        principal_id: press.user.clone(),
    };
    let origin = card_press_origin(state, event, press, is_dm);
    crate::ccr_runtime::ATTESTED_ONE_TO_ONE
        .scope(
            attestation,
            crate::decision_notify::route_verified_bound_press(
                &state.ctx,
                &context,
                &press.action,
                crate::decision_notify::DecisionAccessScope {
                    channel_id: (!is_dm).then_some(press.space.as_str()),
                    guild_id: None,
                    session_id: None,
                },
                &origin,
            ),
        )
        .await
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

/// Tell the space its message was not accepted because the queue is full,
/// at most once a minute per thread and once every 10 s per app/space. Both the
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

/// Resolve once before queue admission; queued work retains the same placement
/// and DM classification even if settings change while another turn is running.
pub(crate) async fn resolve_conversation(
    state: &GoogleChatState, event: &serde_json::Value,
) -> Result<crate::googlechat_conversation::GoogleChatConversation, &'static str> {
    let space = event_space(event);
    let is_dm = crate::googlechat_space_kind::is_one_to_one(&state.creds, &space, &event["message"], event).await;
    let always = state.ctx.channel_settings.get("googlechat", "global", "dm_reply_placement").await.as_deref() == Some("thread");
    crate::googlechat_conversation::GoogleChatConversation::from_event(event, is_dm, always)
}

async fn handle_message(
    state: &Arc<GoogleChatState>, event: &serde_json::Value,
    conversation: &crate::googlechat_conversation::GoogleChatConversation, verified_http: bool,
) {
    // No internal caller may reclassify handshake text as model input.
    if handshake_route(event, false) != crate::chat_link_intercept::CodeRoute::Conversation
        || (!verified_http && pubsub_control_message(event)) {
        return;
    }
    let message = event.get("message").cloned().unwrap_or_default();
    // Defence in depth: every entry already requires a human sender.
    if !is_human_message(&message) {
        return;
    }
    let text = inbound_text(&message);
    // Attachment-only messages carry no text — parse attachments before the
    // empty-text bail so a bare file upload still reaches the agent.
    let mut attachments = parse_gchat_attachments(&message);
    if text.is_empty() && attachments.is_empty() {
        return;
    }

    let _turn = match conversation.turn_lease(&state.ctx.home_dir, state.agent.is_some()).await {
        Ok(lease) => lease,
        Err(_) => { warn!("Google Chat thread lock unavailable; turn refused"); return; }
    };

    let space = conversation.space().to_owned();
    let is_dm = conversation.is_dm();
    let thread = conversation.delivery_thread().map(str::to_owned);
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

    let session_id = conversation.session_key().to_owned();
    let route = crate::googlechat_shared::route(
        &state.ctx, state.agent.is_some(), conversation, &sender_id, &text,
    ).await;
    let (routed_agent, own_turn, called_turn) = match route {
        crate::googlechat_shared::Route::Legacy => (state.agent.clone(), state.agent.is_some(), false),
        crate::googlechat_shared::Route::Avatar(agent) => (Some(agent), true, false),
        crate::googlechat_shared::Route::Called { agent, via_trigger } => (Some(agent), false, via_trigger),
        crate::googlechat_shared::Route::Direct(reply) => {
            if !reply.trim().is_empty() {
                let reply = crate::markdown_render::to_googlechat_text(&reply);
                if post_message(&state.creds, &space, thread.as_deref(), &reply).await.is_err() {
                    warn!(failed = 1, "Google Chat direct reply delivery failed");
                }
            }
            return;
        }
        crate::googlechat_shared::Route::Unbound(reply) => {
            // Keep blocked/taken-over conversations silent. Never forward an
            // access-gate diagnostic or command response to an unbound sender.
            if crate::channel_reply::check_user_access_gate(
                &state.ctx, conversation.gate_session(),
                crate::ccr_runtime::reply_principal_for_sender(&sender_id), &text,
            ).await.is_none()
                && !crate::takeover::is_target_paused(&state.ctx.home_dir, "googlechat", &space)
                && !reply.trim().is_empty()
                && crate::googlechat_shared::allow_unbound_reply(&state.ctx.home_dir, &sender_id)
            {
                if post_message(&state.creds, &space, thread.as_deref(), &reply).await.is_err() {
                    warn!(failed = 1, "Google Chat unbound reply delivery failed");
                }
            }
            return;
        }
    };

    let group_turn = crate::channel_reply::group_buffer::GroupTurn::capture(&state.ctx, message.pointer("/space/type").or_else(|| event.pointer("/space/type")).and_then(|v| v.as_str()).is_some_and(|kind| matches!(kind, "ROOM" | "SPACE" | "GROUP_CHAT")), "googlechat", "global", &space, thread.as_deref(), message.get("name").and_then(|v| v.as_str()).unwrap_or(""), &sender_id, &sender, if text.is_empty() { "[附件]" } else { &text }).await;

    let raw_text = message.get("argumentText").or_else(|| message.get("text"))
        .and_then(serde_json::Value::as_str).unwrap_or("").trim();
    let link_count = crate::google_workspace::chat_read::rich_links(&message, raw_text).len();
    info!(sender = %sender, message_length = text.len(), link_count, "Google Chat inbound message");

    if !attachments.is_empty() {
        let reply_session = match &routed_agent {
            Some(agent) if own_turn || called_turn => conversation.session_for(Some(agent)),
            _ => session_id.clone(),
        };
        match crate::channel_reply::attachment_gate::check(
            &state.ctx, conversation.gate_session(), &reply_session,
            crate::ccr_runtime::reply_principal_for_sender(&sender_id),
            &text, routed_agent.as_deref(), None,
        ).await {
            crate::channel_reply::attachment_gate::AttachmentEntry::Proceed => {}
            crate::channel_reply::attachment_gate::AttachmentEntry::SkipFiles
            | crate::channel_reply::attachment_gate::AttachmentEntry::DropFilesAndContinue(_) => {
                attachments.clear();
            }
            crate::channel_reply::attachment_gate::AttachmentEntry::Refuse(reply) => {
                let reply = crate::markdown_render::to_googlechat_text(&reply);
                if post_message(&state.creds, &space, thread.as_deref(), &reply).await.is_err() {
                    warn!("Google Chat attachment refusal delivery failed");
                }
                return;
            }
        }
    }

    // ── Inbound attachments (WP1.3 parity with the other 8 channels) ──
    // Uploaded-to-Chat files are fetched through the media API with the
    // service-account token and saved under the agent's `attachments/`; Drive
    // links need a Drive scope the `chat.bot` token does not carry, so they
    // degrade to an explicit skip note. Every failure degrades to a text
    // reference — attachment problems never block the message itself.
    let mut attachment_lines: Vec<String> = Vec::new();
    if !attachments.is_empty() {
        let attach_base =
            crate::channel_reply::resolve_attachment_base(state.ctx.as_ref(), routed_agent.as_deref())
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
    let reply_session = match &routed_agent {
        Some(agent) if own_turn || called_turn => conversation.session_for(Some(agent)),
        _ => session_id.clone(),
    };
    let employee_id = match &routed_agent {
        Some(agent) => agent.clone(),
        None => state.ctx.registry.read().await.main_agent()
            .map(|a| a.config.agent.name.clone()).unwrap_or_default(),
    };
    let (recovery_employee, recovery_session) = if routed_agent.is_none()
        && !crate::chat_commands::is_command(&text)
    {
        crate::googlechat_shared::recovery_identity(
            &state.ctx, conversation, &sender_id, &input_text,
        ).await
    } else {
        (employee_id.clone(), reply_session.clone())
    };
    // Session tool references and durable recovery have a 512-byte key budget.
    if recovery_session.len() > 512 || reply_session.len() > 512 {
        warn!("Google Chat: conversation bucket exceeds storage limit");
        return;
    }
    // Fail closed: never create a placeholder when recovery cannot be persisted.
    let store = match crate::googlechat_inbox::Inbox::shared(&state.ctx.home_dir) {
        Ok(store) => store,
        Err(_) => { warn!(failed = 1, "Google Chat placeholder store unavailable"); return; }
    };
    let (name, registration) = match create_placeholder(
        &state.creds, store, &space, thread.as_deref(), state.agent.clone(), recovery_employee, recovery_session,
    ).await {
        Ok(created) => created,
        Err(_) => { warn!(failed = 1, "Google Chat placeholder persistence failed"); return; }
    };
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
            // History is shared by this thread's participants. The permission
            // and takeover scope is still the whole space; reset only this bucket.
            let one_to_one = is_dm;
            let origin = crate::chat_commands::CommandOrigin {
                channel: "googlechat",
                gate_session: conversation.gate_session(),
                command_session: &reply_session,
                sender: crate::ccr_runtime::reply_principal_for_sender(&sender_id),
                text: &text,
                shared: !one_to_one,
            };
            let reply = match crate::chat_commands::command_preflight(&cmd, &state.ctx, &origin).await {
                Some(reply) => reply,
                None => {
                    let agent_id = employee_id.clone();
                    if let Some(refusal) = crate::chat_commands::command_entry_refusal(
                        &cmd, &state.ctx, &origin, &agent_id,
                    ).await {
                        refusal
                    } else {
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
    // RM-190: a 1:1 DM (trusted classifier above; unknown = shared) of a
    // human `users/<digits>` sender counts as a one-to-one conversation for
    // the knowledge ACL and the wiki, exactly like a LINE/Telegram DM. The
    // space id can never equal the sender id, so the adapter attests it for
    // this space and sender only; what the sender may read is still decided
    // by their verified binding and grants.
    let one_to_one = if is_dm {
        crate::ccr_runtime::OneToOneAttestation::googlechat_dm(&space, &sender_id)
    } else {
        None
    };
    let trusted_target = if verified_http && is_dm {
        match state.creds.token().await {
            Ok(token) => crate::approval::TrustedReplyTarget::new(
                crate::approval::DecisionContext {
                    channel: "googlechat".into(), account_id: state.creds.client_email().to_string(),
                    conversation_id: thread.as_ref().map_or_else(|| space.clone(), |t| format!("{space}:{t}")),
                    principal_id: sender_id.clone(),
                }, token, space.clone(), thread.clone(),
            ).map(|target| target.with_chat_type(bound_chat_type(verified_http, is_dm))
                .with_decision_access_scope(crate::decision_notify::DecisionAccessScope {
                    channel_id: None, guild_id: None, session_id: None,
                })),
            Err(e) => Err(e),
        }
    } else { Err("Google Chat bound approvals require a verified HTTP DM".into()) };
    let recovery_binding = registration.as_ref().map(|r| r.reply_binding());
    let (guarded, deliberate_silence) = crate::googlechat_inbox::REPLY_BINDING.scope(
        recovery_binding, crate::approval::scope_trusted_reply(trusted_target,
        crate::channel_reply::track_silence(group_turn.scope(crate::ccr_runtime::ATTESTED_ONE_TO_ONE.scope(one_to_one, Box::pin(async { match &routed_agent {
        // Avatars use the same own-bot bucket as employee apps; explicit @
        // calls retain their existing label and called-session semantics.
        Some(agent) => {
            let reply = crate::channel_reply::build_guarded_reply_for_agent(
                &input_text, &state.ctx, agent, &session_id,
                crate::ccr_runtime::reply_principal_for_sender(&sender_id), Some(on_progress),
            );
            if own_turn {
                thinking.scope(crate::trigger_route::as_own_bot_turn(reply)).await
            } else {
                thinking.scope(crate::trigger_route::as_called_turn(called_turn, reply)).await
            }
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
    } })))))).await;
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
                if delete_placeholder(&state.creds, &name).await.is_terminal() {
                    if let Some(registration) = registration.as_ref() { registration.retire(); }
                }
            }
        } else {
            report_delivery(delivery_target, deliver_reply(&state.creds, &space, thread.as_deref(),
                &placeholder, registration.as_ref(), "⚠️ 沒有產生回覆，請再試一次。", None).await);
        }
        return;
    }
    let reply = match guarded.answered_by.as_ref() {
        Some(who) if called_turn && who.via_trigger => {
            crate::trigger_route::with_reply_prefix(reply, &who.display_name)
        }
        _ => reply,
    };
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
        if let Some(name) = placeholder.as_deref() {
            if delete_placeholder(creds, name).await.is_terminal() {
                if let Some(registration) = registration { registration.retire(); }
            }
        }
        return Ok(());
    }
    let cleanup_text = crate::trigger_route::texts::googlechat_replied(
        registration.map(|r| crate::trigger_route::texts::lang_for(r.home()))
            .unwrap_or(crate::trigger_route::texts::Lang::ZhTw));
    let formatted = crate::markdown_render::to_googlechat_text(reply_markdown);
    let chunks = crate::channel_format::split_text(&formatted, GCHAT_TEXT_CHUNK);
    let mut failure = None;
    for (index, chunk) in chunks.iter().enumerate() {
        if crate::channel_reply::guard_lost(guarded).await {
            let refusal = crate::channel_reply::CCR_DELIVERY_REFUSED_TEXT;
            let result = if index == 0 && let Some(name) = placeholder.as_deref() {
                replace_placeholder(name, registration,
                    || update_message(creds, name, refusal),
                    || post_message(creds, space, thread, refusal),
                    || cleanup_placeholder(creds, name, cleanup_text),
                ).await.map(|_| ())
            } else {
                post_message(creds, space, thread, refusal).await.map(|_| ())
            };
            if let Err(status) = result {
                warn!(?status, "Google Chat refusal delivery failed");
                return Err(status);
            }
            // The refusal replaced the answer on purpose; an earlier chunk
            // that failed is still a delivery failure.
            return match failure { Some(status) => Err(status), None => Ok(()) };
        }
        let result = if index == 0 && let Some(name) = placeholder.as_deref() {
            replace_placeholder(name, registration,
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
                || cleanup_placeholder(creds, name, cleanup_text),
            ).await
        } else {
            post_message(creds, space, thread, chunk).await
        };
        match result {
            Ok(id) => crate::channel_reply::group_buffer::delivered(chunk, id.as_deref()).await,
            Err(status) => {
                warn!(?status, "Google Chat chunk delivery failed");
                failure.get_or_insert(status);
                // Still attempt every remaining chunk, rather than losing the answer tail.
            }
        }
    }
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
    name: &str, registration: Option<&crate::googlechat_inbox::PlaceholderRegistration>,
    mut patch: P, post: S, clear: C,
) -> Result<Option<String>, Option<u16>>
where
    P: FnMut() -> PF, PF: std::future::Future<Output = Result<(), PatchFailure>>,
    S: FnOnce() -> SF, SF: std::future::Future<Output = Result<Option<String>, Option<u16>>>,
    C: FnOnce() -> CF, CF: std::future::Future<Output = PlaceholderResolution>,
{
    let mut terminal = false;
    for attempt in 0..2 {
        let result = patch().await;
        terminal |= result.as_ref().err().is_some_and(|e| matches!(e.status, Some(403 | 404)));
        match result {
            Ok(()) => {
                if let Some(registration) = registration { registration.retire(); }
                return Ok(Some(name.to_string()));
            }
            Err(error) if attempt == 0 => {
                warn!(status = ?error.status, "Google Chat final PATCH failed; retrying");
                tokio::time::sleep(error.retry_after).await;
            }
            Err(error) => warn!(status = ?error.status, "Google Chat final PATCH failed; sending new message"),
        }
    }
    if terminal {
        if let Some(registration) = registration { registration.retire(); }
    }
    let sent = post().await;
    // A definite fallback refusal delivered nothing; retain interruption recovery.
    if matches!(sent, Err(Some(_))) {
        return sent;
    }
    if !terminal {
        if let Some(registration) = registration {
            // A crash between a successful POST and this state write leaves
            // `pending`, so recovery may show an interruption beside a delivered
            // answer. This is accepted; the reverse must never happen.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            let mut persisted = false;
            loop {
                if registration.cleanup_only().is_ok() {
                    persisted = true;
                    break;
                }
                if tokio::time::Instant::now() >= deadline { break; }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if !persisted {
                warn!(failed = 1, "Google Chat placeholder cleanup state update failed; skipping cleanup");
                return sent;
            }
        }
    }
    if clear().await.is_terminal() {
        if let Some(registration) = registration { registration.retire(); }
    }
    sent
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaceholderResolution { Terminal, Retry(Option<u16>) }
impl PlaceholderResolution {
    pub(crate) fn is_terminal(self) -> bool { matches!(self, Self::Terminal) }
}

pub(crate) async fn delete_placeholder(creds: &GoogleChatCreds, name: &str) -> PlaceholderResolution {
    let Some((space, message)) = crate::googlechat_inbox::parse_message_resource_name(name) else {
        return PlaceholderResolution::Retry(None);
    };
    let Ok(token) = creds.get_token().await else { return PlaceholderResolution::Retry(None); };
    match creds.http.delete(format!("{}/spaces/{space}/messages/{message}", creds.api_base()))
        .bearer_auth(token).send_authorized().await
    {
        Ok(response) if response.status().is_success() || matches!(response.status().as_u16(), 403 | 404) => PlaceholderResolution::Terminal,
        Ok(response) => PlaceholderResolution::Retry(Some(response.status().as_u16())),
        Err(_) => PlaceholderResolution::Retry(None),
    }
}

/// Cleanup-only recovery never claims the answer was interrupted. If DELETE
/// is refused (403), try a neutral PATCH before treating the refusal as terminal.
pub(crate) async fn cleanup_placeholder(creds: &GoogleChatCreds, name: &str, text: &str) -> PlaceholderResolution {
    // Keep the raw status here so 403 can still attempt the neutral edit.
    let Some((space, message)) = crate::googlechat_inbox::parse_message_resource_name(name) else {
        return PlaceholderResolution::Retry(None);
    };
    let Ok(token) = creds.get_token().await else { return PlaceholderResolution::Retry(None); };
    let status = match creds.http.delete(format!("{}/spaces/{space}/messages/{message}", creds.api_base()))
        .bearer_auth(token).send_authorized().await {
        Ok(response) if response.status().is_success() => return PlaceholderResolution::Terminal,
        Ok(response) => Some(response.status().as_u16()),
        Err(_) => None,
    };
    if status == Some(404) { return PlaceholderResolution::Terminal; }
    match update_message(creds, name, text).await {
        Ok(()) => PlaceholderResolution::Terminal,
        Err(error) if status == Some(403) || matches!(error.status, Some(403 | 404)) => PlaceholderResolution::Terminal,
        Err(error) => PlaceholderResolution::Retry(error.status),
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

/// A knowledge approval card is already rendered as inert plain fields.
/// Send its final payload directly; do not reinterpret previews as Markdown.
pub(crate) async fn send_untrusted_card(
    home_dir: &Path, space: &str, body: serde_json::Value,
) -> Result<Option<String>, String> {
    if !crate::google_workspace::chat_read::valid_space(space) {
        return Err(GoogleChatSendError::InvalidTarget.user_message(home_dir));
    }
    let (creds, _) = sending_creds(home_dir, None, space).await?;
    post_message_json(&creds, space, None, body, None).await
        .map_err(|status| match status {
            Some(status) => GoogleChatSendError::Http { status },
            None => GoogleChatSendError::Transport,
        }.user_message(home_dir))
}

/// A proven account is authoritative: None means the shared app, never the
/// space roster or another employee's app. All RM-143 sends/edits use this.
async fn proven_app_creds(home: &Path, agent: Option<&str>) -> Result<Arc<GoogleChatCreds>, String> {
    match agent {
        Some(id) => crate::googlechat_agents::agent_creds(home, id).await,
        None => GoogleChatCreds::from_config(home).await.map(Arc::new),
    }.ok_or_else(|| "proven Google Chat app unavailable".into())
}

pub(crate) async fn send_untrusted_card_as(
    home: &Path, agent: Option<&str>, space: &str, body: serde_json::Value,
) -> Result<String, String> {
    if !crate::google_workspace::chat_read::valid_space(space) {
        return Err("invalid Google Chat destination".into());
    }
    let creds = proven_app_creds(home, agent).await?;
    // Knowing the name before POST also lets us neutralize an ambiguous send.
    let id = format!("client-{}", uuid::Uuid::new_v4().simple());
    let name = format!("{space}/messages/{id}");
    match post_message_json(&creds, space, None, body, Some(&id)).await {
        Ok(Some(created)) if crate::googlechat_inbox::parse_message_resource_name(&created)
            .is_some_and(|(s, _)| format!("spaces/{s}") == space) => Ok(created),
        Ok(_) => Ok(name),
        Err(_) => {
            let _ = replace_approval_card_as(home, agent, &name, "請到儀表板處理").await;
            Err("Google Chat card send failed".into())
        }
    }
}

pub(crate) async fn replace_approval_card_as(
    home: &Path, agent: Option<&str>, name: &str, text: &str,
) -> Result<(), String> {
    let (space, message) = crate::googlechat_inbox::parse_message_resource_name(name)
        .ok_or_else(|| "invalid Google Chat message".to_string())?;
    let creds = proven_app_creds(home, agent).await?;
    let token = creds.get_token().await.map_err(|_| "Google Chat authorization failed")?;
    let url = format!("{}/spaces/{space}/messages/{message}?updateMask=text,cardsV2", creds.api_base());
    let response = creds.http.patch(url).bearer_auth(token)
        .json(&serde_json::json!({"text":text, "cardsV2":[]}))
        .send_authorized().await.map_err(|_| "Google Chat card replacement failed")?;
    if response.status().is_success() { Ok(()) } else { Err("Google Chat card replacement failed".into()) }
}

pub(crate) async fn remind_approval_card_as(
    home: &Path, agent: Option<&str>, space: &str, message: Option<&str>, text: &str,
) -> Result<(), String> {
    if !crate::google_workspace::chat_read::valid_space(space) {
        return Err("invalid Google Chat destination".into());
    }
    let creds = proven_app_creds(home, agent).await?;
    // A message name does not encode its thread. Read the message, and use
    // only a thread in the proven DM; an unavailable thread falls back to DM.
    let mut thread = None;
    if let Some(name) = message.filter(|name| crate::googlechat_inbox::parse_message_resource_name(name)
        .is_some_and(|(s, _)| format!("spaces/{s}") == space)) {
        let token = creds.get_token().await.map_err(|_| "Google Chat authorization failed")?;
        if let Ok(response) = creds.http.get(format!("{}/{name}", creds.api_base()))
            .bearer_auth(token).send_authorized().await {
            if response.status().is_success() {
                if let Ok(body) = response.json::<serde_json::Value>().await {
                    thread = body["thread"]["name"].as_str().filter(|name| {
                        let parts: Vec<_> = name.split('/').collect();
                        parts.len() == 4 && parts[0] == "spaces" && parts[2] == "threads"
                            && format!("spaces/{}", parts[1]) == space
                            && !parts[3].is_empty() && parts[3].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    }).map(str::to_owned);
                }
            }
        }
    }
    post_message_json(&creds, space, thread.as_deref(), serde_json::json!({"text":text}), None)
        .await.map(|_| ()).map_err(|_| "Google Chat reminder failed".into())
}

/// RM-192: whether the app a send to `space` as `agent` would use (the same
/// selection as [`sending_creds`]) receives Chat interaction events over the
/// HTTP transport — the only transport whose `CARD_CLICKED` events are
/// JWT-verified (`webhook_handler`, `googlechat_agents::handle`). A Pub/Sub
/// app gets no click it can trust, so it must not be sent buttons. Fail
/// closed on anything unclear. `exact_account` pins None to the shared app
/// and Some(agent) to that employee, bypassing the space roster for RM-143.
pub(crate) async fn sending_app_takes_http_clicks(
    home_dir: &Path,
    agent: Option<&str>,
    space: &str,
    exact_account: bool,
) -> bool {
    use crate::agent_channels::gchat_spaces::{self, SpaceApp};
    let own = if exact_account { agent.map(str::to_owned) } else { match agent {
        Some(id) if crate::agent_channels::store::agent_has_googlechat_section(home_dir, id) => {
            Some(id.to_string())
        }
        _ => match gchat_spaces::app_for_space(home_dir, space) {
            Some(SpaceApp::Agent(id)) => Some(id),
            Some(SpaceApp::Shared) | None => None,
        },
    }};
    match own {
        Some(id) => {
            let home = home_dir.to_path_buf();
            tokio::task::spawn_blocking(move || crate::agent_channels::store::agent_uses_http(&home, &id))
                .await.unwrap_or(false)
        },
        None => {
            shared_settings_valid(home_dir).await
                && crate::googlechat_pubsub::shared_http_enabled(home_dir).await
        }
    }
}

/// [`send_text_to_space_as`] with a `cardsV2` card (RM-192 decision buttons)
/// on the message carrying the text. A text longer than one Chat message is
/// sent as plain chunks first; the card rides on the last one.
pub(crate) async fn send_card_to_space_as(
    home_dir: &Path,
    agent: Option<&str>,
    space: &str,
    markdown: &str,
    cards_v2: serde_json::Value,
) -> Result<(), GoogleChatSendError> {
    if !crate::google_workspace::chat_read::valid_space(space) || !cards_v2.is_array() {
        return Err(GoogleChatSendError::InvalidTarget);
    }
    let (creds, member) = sending_creds(home_dir, agent, space).await
        .map_err(GoogleChatSendError::Configuration)?;
    let formatted = crate::markdown_render::to_googlechat_text(markdown);
    let chunks = crate::channel_format::split_text(&formatted, GCHAT_TEXT_CHUNK);
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let mut body = serde_json::json!({ "text": chunk });
        if i == last {
            body["cardsV2"] = cards_v2.clone();
        }
        if let Err(status) = post_message_json(&creds, space, None, body, None).await {
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

/// Persist the fully known client-assigned resource before any POST.
async fn create_placeholder(
    creds: &GoogleChatCreds, store: Arc<crate::googlechat_inbox::Inbox>, space: &str,
    thread: Option<&str>, app_agent: Option<String>, employee_id: String, session_id: String,
) -> Result<(Option<String>, Option<crate::googlechat_inbox::PlaceholderRegistration>), String> {
    let id = format!("client-{}", uuid::Uuid::new_v4().simple());
    let name = format!("{space}/messages/{id}");
    let row = crate::googlechat_inbox::PendingPlaceholder {
        message_name: name.clone(), space: space.to_owned(), app_agent,
        app_email: creds.client_email().to_owned(), employee_id, session_id,
        thread_name: thread.map(str::to_owned), created_at: chrono::Utc::now().timestamp(), state: "pending".into(),
    };
    let registration = crate::googlechat_inbox::PlaceholderRegistration::new(store, &row)?;
    match post_message_with_id(creds, space, thread, "🤔 思考中…", Some(&id)).await {
        Err(Some(_)) => {
            registration.retire();
            Ok((None, None))
        }
        // Success bodies need not be parseable; transport failures are ambiguous.
        _ => Ok((Some(name), Some(registration))),
    }
}

/// spaces.messages.create — returns the created message name for edits.
async fn create_message(creds: &GoogleChatCreds, space: &str, thread: Option<&str>, text: &str) -> Option<String> {
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
    post_message_with_id(creds, space, thread, text, None).await
}

async fn post_message_with_id(
    creds: &GoogleChatCreds, space: &str, thread: Option<&str>, text: &str, message_id: Option<&str>,
) -> Result<Option<String>, Option<u16>> {
    post_message_json(creds, space, thread, serde_json::json!({ "text": text }), message_id).await
}

/// spaces.messages.create with a prepared message body (`text`, optionally
/// `cardsV2`).
async fn post_message_json(
    creds: &GoogleChatCreds, space: &str, thread: Option<&str>, mut body: serde_json::Value,
    message_id: Option<&str>,
) -> Result<Option<String>, Option<u16>> {
    let token = match creds.get_token().await {
        Ok(t) => t,
        Err(_) => {
            error!("Google Chat authorization failed");
            return Err(None);
        }
    };
    let mut url = format!("{}/{space}/messages", creds.api_base());
    if let Some(th) = thread {
        body["thread"] = serde_json::json!({ "name": th });
        url.push_str("?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD");
    }
    if let Some(id) = message_id {
        url.push_str(if thread.is_some() { "&messageId=" } else { "?messageId=" });
        url.push_str(id);
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
        Err(_) => {
            error!("Google Chat send transport failed");
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
    let (space, message) = crate::googlechat_inbox::parse_message_resource_name(message_name)
        .ok_or(PatchFailure { status: None, retry_after: std::time::Duration::ZERO })?;
    let token = creds.get_token().await.map_err(|_| PatchFailure {
        status: None, retry_after: std::time::Duration::from_secs(1),
    })?;
    let url = format!("{}/spaces/{space}/messages/{message}?updateMask=text", creds.api_base());
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
        use crate::googlechat_space_kind::{SpaceKind, carried_kind};
        let msg = |space: serde_json::Value| json!({ "space": space });
        let none = json!({});
        let kind = |m: &serde_json::Value, e: &serde_json::Value| carried_kind("spaces/A", m, e);
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
        assert!(pubsub.contains("crate::googlechat::process_pubsub_conversation(&st, &event, &queued_conversation)"));
    }

    fn conversation(space: &str, thread: &str) -> crate::googlechat_conversation::GoogleChatConversation {
        crate::googlechat_conversation::GoogleChatConversation::from_event(
            &json!({"space":{"name":space}, "message":{"thread":{"name":thread}}}), false, false,
        ).unwrap()
    }

    #[test]
    fn queue_keys_match_both_transports() {
        let c = conversation("spaces/A", "spaces/A/threads/t");
        let (group, key) = c.queue_keys(Some("sales"));
        assert_eq!(group, json!(["sales", "spaces/A"]).to_string());
        assert_eq!(key, json!(["sales", c.session_key()]).to_string());
        assert_ne!(c.queue_keys(None).1, key);
    }

    #[tokio::test]
    async fn queue_full_notice_is_throttled_per_sender_and_space() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let (group, alice) = conversation("spaces/A", "spaces/A/threads/a").queue_keys(None);
        let (_, bob) = conversation("spaces/A", "spaces/A/threads/b").queue_keys(None);
        let (other_group, carol) = conversation("spaces/B", "spaces/B/threads/c").queue_keys(None);
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
        assert!(src.contains("verified_conversation.queue_keys("));
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
mod card_click_tests {
    use super::*;
    use crate::decision_action::{DecisionAct, DecisionSource, encode};
    use serde_json::json;

    fn test_state(home: &Path) -> Arc<GoogleChatState> {
        let registry = Arc::new(RwLock::new(duduclaw_agent::AgentRegistry::new(home.join("agents"))));
        let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
        let status: crate::channel_reply::ChannelStatusMap =
            Arc::new(RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let ctx = Arc::new(ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx));
        let creds = GoogleChatCreds::candidate(
            r#"{"client_email":"click-test@example.invalid","private_key":"not-a-key"}"#,
        )
        .unwrap();
        Arc::new(GoogleChatState { ctx, creds: Arc::new(creds), agent: None })
    }

    fn click(action: &str) -> serde_json::Value {
        json!({
            "type": "CARD_CLICKED",
            "user": { "name": "users/111", "type": "HUMAN" },
            "space": { "name": "spaces/DM1", "spaceType": "DIRECT_MESSAGE", "singleUserBotDm": true },
            "message": { "name": "spaces/DM1/messages/m1", "space": { "name": "spaces/DM1" } },
            "action": {
                "actionMethodName": crate::channel_format::GOOGLECHAT_DECISION_FUNCTION,
                "parameters": [{ "key": crate::channel_format::GOOGLECHAT_DECISION_PARAM, "value": action }]
            },
            "common": {
                "invokedFunction": crate::channel_format::GOOGLECHAT_DECISION_FUNCTION,
                "parameters": { "decision": action }
            }
        })
    }

    fn mutated(action: &str, mutate: impl FnOnce(&mut serde_json::Value)) -> Option<CardPress> {
        let mut e = click(action);
        mutate(&mut e);
        parse_card_press(&e)
    }

    #[test]
    fn a_human_goal_press_is_read_and_everything_else_is_refused() {
        let retry = encode(DecisionSource::Goal, DecisionAct::Retry, "t1");
        assert_eq!(
            parse_card_press(&click(&retry)),
            Some(CardPress { space: "spaces/DM1".into(), user: "users/111".into(), action: retry.clone() })
        );
        // Either parameter form alone is enough.
        let mut only_common = click(&retry);
        only_common["action"].as_object_mut().unwrap().remove("parameters");
        assert!(parse_card_press(&only_common).is_some());
        let mut only_action = click(&retry);
        only_action.as_object_mut().unwrap().remove("common");
        assert!(parse_card_press(&only_action).is_some());

        assert_eq!(crate::channel_format::GOOGLECHAT_DECISION_PARAM, "decision");
        let r = retry.as_str();
        assert_eq!(mutated(r, |e| e["user"]["type"] = json!("BOT")), None);
        assert_eq!(mutated(r, |e| { e["user"].as_object_mut().unwrap().remove("type"); }), None);
        for bad in ["users/app", "users/a@example.com", "111", "", "users/"] {
            assert_eq!(mutated(r, |e| e["user"]["name"] = json!(bad)), None, "{bad}");
        }
        assert_eq!(mutated(r, |e| e["space"]["name"] = json!("spaces/../x")), None);
        assert_eq!(mutated(r, |e| e["message"]["space"]["name"] = json!("spaces/OTHER")), None);
        assert_eq!(mutated(r, |e| e["common"]["invokedFunction"] = json!("other")), None);
        assert_eq!(mutated(r, |e| e["action"]["actionMethodName"] = json!("other")), None);
        assert_eq!(mutated(r, |e| e["common"]["parameters"]["decision"] = json!("duduclaw:x")), None);
        assert_eq!(
            mutated(r, |e| e["action"]["parameters"].as_array_mut().unwrap().push(json!({"key":"decision","value":"y"}))),
            None,
            "a repeated parameter"
        );
        let mut none = click(&retry);
        none["action"].as_object_mut().unwrap().remove("parameters");
        none["common"].as_object_mut().unwrap().remove("parameters");
        assert_eq!(parse_card_press(&none), None);
        // Undecodable, or a decision Google Chat is never sent.
        assert_eq!(parse_card_press(&click("not-a-decision")), None);
        let approve = encode(DecisionSource::Approval, DecisionAct::Approve, "a1");
        assert!(parse_card_press(&click(&approve)).is_some());
        let install = encode(DecisionSource::Install, DecisionAct::Approve, "a1");
        assert_eq!(parse_card_press(&click(&install)), None);
    }

    #[test]
    fn a_press_with_mismatched_or_conflicting_spaces_is_refused() {
        let r = encode(DecisionSource::Goal, DecisionAct::Retry, "t1");
        let r = r.as_str();
        // Mismatched names, in either direction of who is complete.
        assert_eq!(
            mutated(r, |e| e["message"]["space"] = json!({"name":"spaces/DM2","spaceType":"DIRECT_MESSAGE","singleUserBotDm":true})),
            None
        );
        assert_eq!(mutated(r, |e| e["message"]["space"]["name"] = json!(7)), None);
        assert_eq!(mutated(r, |e| e["message"]["space"] = json!("spaces/DM1")), None);
        // The event-level space is a complete DM proof; message.space says
        // shared → refused, not merged.
        for shared in [
            json!({"name":"spaces/DM1","spaceType":"SPACE"}),
            json!({"name":"spaces/DM1","spaceType":"GROUP_CHAT"}),
            json!({"name":"spaces/DM1","type":"ROOM"}),
            json!({"name":"spaces/DM1","singleUserBotDm":false}),
        ] {
            assert_eq!(mutated(r, |e| e["message"]["space"] = shared.clone()), None, "{shared}");
            // And the other way round.
            assert_eq!(
                mutated(r, |e| {
                    e["message"]["space"] = json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE","singleUserBotDm":true});
                    e["space"] = shared.clone();
                }),
                None,
                "{shared}"
            );
        }
        // A split proof (type in one object, flag in the other) is not a
        // conflict: the press is read, and the resolver treats it as unknown
        // (lookup, else shared) — never as a DM proof.
        let split = mutated(r, |e| {
            e["space"] = json!({"name":"spaces/DM1","singleUserBotDm":true});
            e["message"]["space"] = json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE"});
        });
        assert!(split.is_some());
        let mut event = click(r);
        event["space"] = json!({"name":"spaces/DM1","singleUserBotDm":true});
        event["message"]["space"] = json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE"});
        assert_eq!(
            crate::googlechat_space_kind::carried_kind("spaces/DM1", &event["message"], &event),
            crate::googlechat_space_kind::SpaceKind::Unknown
        );
        // Agreeing objects are still read.
        assert!(mutated(r, |e| {
            let top = e["space"].clone();
            e["message"]["space"] = top;
        })
        .is_some());
    }

    #[tokio::test]
    async fn card_clicks_answer_in_the_space_and_malformed_ones_do_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let call = |event: serde_json::Value| {
            let state = state.clone();
            async move {
                let response = dispatch_event(state, Bytes::from(event.to_string())).await;
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 1 << 16).await.unwrap();
                (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap())
            }
        };
        // Malformed → 200, empty body, no detail.
        let mut bot = click(&encode(DecisionSource::Goal, DecisionAct::Done, "t1"));
        bot["user"]["type"] = json!("BOT");
        assert_eq!(call(bot).await, (StatusCode::OK, json!({})));
        assert_eq!(call(click("garbage")).await, (StatusCode::OK, json!({})));
        // A well-formed press for a task that does not exist is refused in
        // the reply, and nothing is created.
        let (status, body) = call(click(&encode(DecisionSource::Goal, DecisionAct::Done, "missing"))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["text"].as_str().unwrap().starts_with("⚠️"), "{body}");
    }

    #[tokio::test]
    async fn approval_cards_pin_shared_app_even_when_space_roster_names_an_employee() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("config.toml"), "[channels]\ngooglechat_project_number='1'\n").unwrap();
        std::fs::create_dir_all(home.join("agents/sales")).unwrap();
        std::fs::write(home.join("agents/sales/agent.toml"),
            "[agent]\nname='sales'\n[channels.googlechat]\nproject_number='1'\nconnection='pubsub'\npubsub_subscription='projects/p/subscriptions/s'\n").unwrap();
        crate::agent_channels::gchat_spaces::record(home, "spaces/A", Some("sales"));
        assert!(!sending_app_takes_http_clicks(home, None, "spaces/A", false).await);
        assert!(sending_app_takes_http_clicks(home, None, "spaces/A", true).await);
        assert!(!sending_app_takes_http_clicks(home, Some("sales"), "spaces/A", true).await);
        assert!(!sending_app_takes_http_clicks(home, Some("missing"), "spaces/A", true).await);
    }

    #[tokio::test]
    async fn buttons_follow_the_sending_apps_transport() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Shared app, HTTP (no connection setting) → buttons.
        std::fs::write(home.join("config.toml"), "[channels]\ngooglechat_project_number = \"1\"\n").unwrap();
        assert!(sending_app_takes_http_clicks(home, None, "spaces/A", false).await);
        // Shared app over Pub/Sub → text only.
        std::fs::write(
            home.join("config.toml"),
            "[channels]\ngooglechat_connection = \"pubsub\"\ngooglechat_pubsub_subscription = \"projects/p/subscriptions/s\"\n",
        )
        .unwrap();
        assert!(!sending_app_takes_http_clicks(home, None, "spaces/A", false).await);
        // No config at all → fail closed.
        std::fs::remove_file(home.join("config.toml")).unwrap();
        assert!(!sending_app_takes_http_clicks(home, None, "spaces/A", false).await);
        // An employee's own app decides for itself.
        let agent = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent).unwrap();
        let write = |section: &str| {
            std::fs::write(
                agent.join("agent.toml"),
                format!("[agent]\nname = \"sales\"\n[channels.googlechat]\nproject_number = \"1\"\n{section}"),
            )
            .unwrap()
        };
        write("");
        assert!(sending_app_takes_http_clicks(home, Some("sales"), "spaces/A", false).await);
        write("connection = \"pubsub\"\npubsub_subscription = \"projects/p/subscriptions/s\"\n");
        assert!(!sending_app_takes_http_clicks(home, Some("sales"), "spaces/A", false).await);
        write("connection = 7\n");
        assert!(!sending_app_takes_http_clicks(home, Some("sales"), "spaces/A", false).await);
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
        let result = replace_placeholder("old", None, || async {
            order.lock().unwrap().push("patch");
            Err(PatchFailure { status: Some(429), retry_after: std::time::Duration::ZERO })
        }, || async {
            order.lock().unwrap().push("post");
            Ok(Some("new".into()))
        }, || async {
            order.lock().unwrap().push("clear");
            PlaceholderResolution::Terminal
        }).await;
        assert_eq!(result.unwrap().as_deref(), Some("new"));
        assert_eq!(*order.lock().unwrap(), ["patch", "patch", "post", "clear"]);
    }

    #[tokio::test]
    async fn successful_retry_avoids_duplicate_and_failed_post_is_reported() {
        let calls = AtomicUsize::new(0);
        let result = replace_placeholder("old", None, || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(PatchFailure { status: Some(503), retry_after: std::time::Duration::ZERO })
            } else { Ok(()) }
        }, || async { panic!("must not post after successful PATCH") },
        || async { panic!("must not clear after successful PATCH") }).await;
        assert_eq!(result.unwrap().as_deref(), Some("old"));
        let result = replace_placeholder("old", None, || async {
            Err(PatchFailure { status: Some(500), retry_after: std::time::Duration::ZERO })
        }, || async { Err(Some(403)) }, || async { PlaceholderResolution::Terminal }).await;
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
        let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/GUARD", None,
            Some("sales".into()), "sales".into(), "googlechat:spaces/GUARD".into()).await.unwrap();
        let name = name.unwrap();
        let registration = registration.unwrap();
        let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
        assert_eq!(rows.len(), 1);
        let row = rows[0].clone();
        assert_eq!(row.message_name, name);
        let guarded = crate::channel_reply::build_guarded_reply_for_agent(
            "hello", &ctx, "sales", &row.session_id, "users/42", None).await;
        guarded.refuse_delivery();
        let placeholder = Arc::new(tokio::sync::Mutex::new(Some(name)));
        deliver_reply(&creds, &row.space, None, &placeholder, Some(&registration), "SECRET ANSWER", Some(&guarded)).await.unwrap();
        // Retirement happens inside delivery, before registration itself is dropped.
        assert!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap().is_empty());
        let text = chat::texts_with_bearer(&fake, &format!("Bearer tok-{email}")).join("\n");
        assert!(text.contains(crate::channel_reply::CCR_DELIVERY_REFUSED_TEXT));
        assert!(!text.contains("SECRET ANSWER"));
        drop(registration);
        let cancelled = PlaceholderRegistration::new(store.clone(), &row).unwrap();
        drop(cancelled);
        assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap(), vec![row]);
    }

    async fn fixture() -> (tempfile::TempDir, chat::Fake, GoogleChatCreds, Arc<Inbox>) {
        let home = tempfile::tempdir().unwrap();
        let fake = chat::start_fake().await;
        let email = format!("placeholder-{}@test.invalid", uuid::Uuid::new_v4().simple());
        let sa = chat::service_account(&fake, &email);
        crate::trigger_route::tests::write_agent(&home.path().join("agents"), "sales", "sales", "", "specialist",
            &chat::googlechat_section(home.path(), "444444443", &sa));
        let creds = GoogleChatCreds::for_agent(&sa, home.path(), "sales").unwrap();
        creds.get_token().await.unwrap();
        let store = Inbox::shared(home.path()).unwrap();
        (home, fake, creds, store)
    }

    #[tokio::test]
    async fn client_placeholder_is_durable_and_active_before_post_and_sends_message_id() {
        let (_home, fake, creds, store) = fixture().await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        chat::gate_placeholder_post(&fake, gate.clone());
        let create = create_placeholder(&creds, store.clone(), "spaces/CLIENT", Some("spaces/CLIENT/threads/T"),
            Some("sales".into()), "sales".into(), "session".into());
        let observe = async {
            chat::placeholder_post_started(&fake).await;
            let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
            assert_eq!(rows.len(), 1, "insert must precede POST at the fake server");
            assert!(store.placeholder_active(&rows[0].message_name));
            let (_, id) = crate::googlechat_inbox::parse_message_resource_name(&rows[0].message_name).unwrap();
            assert_eq!(id.len(), 39);
            assert!(id.strip_prefix("client-").unwrap().bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            let queries = chat::chat_queries(&fake);
            let query = &queries.iter().find(|(p, _)| p == "/spaces/CLIENT/messages").unwrap().1;
            assert!(query.split('&').any(|q| q == format!("messageId={id}")));
            assert!(query.contains("messageReplyOption="));
            gate.add_permits(1);
            rows[0].message_name.clone()
        };
        let (created, known_name) = tokio::join!(create, observe);
        let (name, registration) = created.unwrap();
        assert_eq!(name.as_deref(), Some(known_name.as_str()));
        drop(registration);
        assert!(!store.placeholder_active(&known_name));
        assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn definite_post_failure_retires_but_timeout_and_unparseable_success_keep_known_name() {
        let (_home, fake, mut creds, store) = fixture().await;
        creds.http = reqwest::Client::builder().timeout(std::time::Duration::from_millis(100))
            .redirect(reqwest::redirect::Policy::none()).build().unwrap();
        for mode in ["http", "timeout", "unparseable"] {
            chat::placeholder_statuses(&fake, None,
                (mode == "http").then_some(axum::http::StatusCode::SERVICE_UNAVAILABLE), None);
            chat::placeholder_post_behavior(&fake,
                (mode == "timeout").then_some(std::time::Duration::from_secs(2)), mode == "unparseable");
            let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/CLIENT", None,
                Some("sales".into()), "sales".into(), "session".into()).await.unwrap();
            let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
            if mode == "http" {
                assert!(name.is_none() && registration.is_none() && rows.is_empty());
            } else {
                let name = name.unwrap();
                assert_eq!(rows.len(), 1, "{mode}");
                assert_eq!(rows[0].message_name, name);
                assert!(store.placeholder_active(&name));
                drop(registration);
                assert!(!store.placeholder_active(&name));
                store.remove_placeholder(&name).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn final_delivery_retires_only_on_terminal_patch_or_delete() {
        use axum::http::StatusCode;
        let (_home, fake, creds, store) = fixture().await;
        for (reply, patch, post, delete, kept) in [
            ("answer", None, None, None, false),
            ("answer", Some(StatusCode::SERVICE_UNAVAILABLE), Some(StatusCode::SERVICE_UNAVAILABLE), Some(StatusCode::SERVICE_UNAVAILABLE), true),
            ("answer", Some(StatusCode::FORBIDDEN), Some(StatusCode::SERVICE_UNAVAILABLE), Some(StatusCode::SERVICE_UNAVAILABLE), false),
            ("", None, None, Some(StatusCode::SERVICE_UNAVAILABLE), true),
            ("", None, None, Some(StatusCode::FORBIDDEN), false),
            ("", None, None, Some(StatusCode::NOT_FOUND), false),
            ("", None, None, None, false),
        ] {
            chat::placeholder_statuses(&fake, None, None, None);
            let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/TERMINAL", None,
                Some("sales".into()), "sales".into(), "session".into()).await.unwrap();
            let name = name.unwrap();
            chat::placeholder_statuses(&fake, patch, post, delete);
            let slot = Arc::new(tokio::sync::Mutex::new(Some(name.clone())));
            let _ = deliver_reply(&creds, "spaces/TERMINAL", None, &slot, registration.as_ref(), reply, None).await;
            let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
            assert_eq!(!rows.is_empty(), kept, "reply={reply}, patch={patch:?}, delete={delete:?}");
            if kept { assert_eq!(rows[0].state, "pending"); }
            drop(registration);
            store.remove_placeholder(&name).unwrap();
        }
    }

    #[tokio::test]
    async fn definite_fallback_http_failure_keeps_pending_placeholder_without_delete() {
        use axum::http::StatusCode;
        let (_home, fake, creds, store) = fixture().await;
        let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/FAILED", None,
            Some("sales".into()), "sales".into(), "session".into()).await.unwrap();
        let name = name.unwrap();
        chat::placeholder_statuses(&fake, Some(StatusCode::SERVICE_UNAVAILABLE),
            Some(StatusCode::SERVICE_UNAVAILABLE), None);
        let slot = Arc::new(tokio::sync::Mutex::new(Some(name.clone())));
        let result = deliver_reply(&creds, "spaces/FAILED", None, &slot, registration.as_ref(), "ANSWER", None).await;
        assert!(result.is_err());
        assert!(!chat::chat_calls(&fake).iter().any(|(method, _, _, _)| method == "DELETE"));
        let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_name, name);
        assert_eq!(rows[0].state, "pending");
    }

    #[tokio::test]
    async fn fallback_delivered_with_failed_cleanup_is_deleted_by_sweep_without_interruption_notice() {
        use axum::http::StatusCode;
        let (home, fake, creds, store) = fixture().await;
        let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/CLEANUP", None,
            Some("sales".into()), "sales".into(), "session".into()).await.unwrap();
        let name = name.unwrap();
        chat::placeholder_statuses(&fake, Some(StatusCode::SERVICE_UNAVAILABLE), None, Some(StatusCode::SERVICE_UNAVAILABLE));
        let slot = Arc::new(tokio::sync::Mutex::new(Some(name.clone())));
        deliver_reply(&creds, "spaces/CLEANUP", None, &slot, registration.as_ref(), "ANSWER VIA FALLBACK", None).await.unwrap();
        assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap()[0].state, "cleanup_only");
        drop(registration);
        rusqlite::Connection::open(home.path().join("googlechat_inbox.db")).unwrap()
            .execute("UPDATE pending_placeholders SET created_at=?1", [chrono::Utc::now().timestamp() - 70]).unwrap();
        chat::placeholder_statuses(&fake, None, None, None);
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let before = chat::chat_calls(&fake).len();
        crate::googlechat_recovery::startup(home.path()).await;
        assert!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap().is_empty());
        let calls: Vec<_> = chat::chat_calls(&fake).into_iter().skip(before).collect();
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].0.as_str(), calls[0].1.as_str()), ("DELETE", format!("/{name}").as_str()));
    }

    #[tokio::test]
    async fn failed_cleanup_state_write_after_fallback_post_skips_delete_and_keeps_pending() {
        use axum::http::StatusCode;
        let (_home, fake, creds, store) = fixture().await;
        let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/LOCKED", None,
            Some("sales".into()), "sales".into(), "session".into()).await.unwrap();
        let name = name.unwrap();
        chat::placeholder_statuses(&fake, Some(StatusCode::SERVICE_UNAVAILABLE), None, None);
        let lock_conn = rusqlite::Connection::open(_home.path().join("googlechat_inbox.db")).unwrap();
        lock_conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        lock_conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let slot = Arc::new(tokio::sync::Mutex::new(Some(name.clone())));
        let result = deliver_reply(&creds, "spaces/LOCKED", None, &slot, registration.as_ref(), "ANSWER", None).await;
        lock_conn.execute_batch("ROLLBACK").unwrap();
        assert!(result.is_ok());
        let calls = chat::chat_calls(&fake);
        assert!(calls.iter().any(|(method, _, _, _)| method == "POST"));
        assert!(!calls.iter().any(|(method, _, _, _)| method == "DELETE"));
        let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_name, name);
        assert_eq!(rows[0].state, "pending");
    }

    #[tokio::test]
    async fn cleanup_delete_refusal_attempts_localized_neutral_patch() {
        use axum::http::StatusCode;
        let (home, fake, creds, store) = fixture().await;
        let (name, registration) = create_placeholder(&creds, store.clone(), "spaces/CLEANUP", None,
            Some("sales".into()), "sales".into(), "session".into()).await.unwrap();
        let registration = registration.unwrap();
        registration.cleanup_only().unwrap();
        drop(registration);
        rusqlite::Connection::open(home.path().join("googlechat_inbox.db")).unwrap()
            .execute("UPDATE pending_placeholders SET created_at=?1", [chrono::Utc::now().timestamp() - 70]).unwrap();
        chat::placeholder_statuses(&fake, None, None, Some(StatusCode::FORBIDDEN));
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let before = chat::chat_calls(&fake).len();
        crate::googlechat_recovery::startup(home.path()).await;
        let calls: Vec<_> = chat::chat_calls(&fake).into_iter().skip(before).collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "DELETE");
        assert_eq!(calls[1].0, "PATCH");
        assert_eq!(calls[1].1, format!("/{}", name.unwrap()));
        assert_eq!(calls[1].3["text"], "（已另行回覆）");
        assert!(store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap().is_empty());
    }


    #[tokio::test]
    async fn periodic_recovery_settles_panicked_reply_while_queue_runs_and_ignores_active_registration() {
        let (home, fake, creds, store) = fixture().await;
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let active_row = PendingPlaceholder {
            message_name: format!("spaces/PERIODIC/messages/client-{}", "a".repeat(32)), space: "spaces/PERIODIC".into(),
            app_agent: Some("sales".into()), app_email: creds.client_email().into(), employee_id: "sales".into(),
            session_id: "active-session".into(), thread_name: None,
            created_at: chrono::Utc::now().timestamp() - 100_000, state: "pending".into(),
        };
        let active_registration = PlaceholderRegistration::new(store.clone(), &active_row).unwrap();
        let queue = crate::conversation_queue::ConversationQueue::default();
        let turn_store = store.clone();
        let (created, got_name) = tokio::sync::oneshot::channel();
        queue.submit("reply", async move {
            let (name, registration) = create_placeholder(&creds, turn_store, "spaces/PERIODIC", None,
                Some("sales".into()), "sales".into(), "panicking-session".into()).await.unwrap();
            let _registration = registration;
            created.send(name.unwrap()).unwrap();
            panic!("reply panics after creating its placeholder");
        }).unwrap();
        let (continued, running) = tokio::sync::oneshot::channel();
        queue.submit("reply", async move { continued.send(()).unwrap(); }).unwrap();
        let orphan = got_name.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), running).await.unwrap().unwrap();
        assert!(!store.placeholder_active(&orphan), "panic must release the registration");
        assert!(store.placeholder_active(&active_row.message_name));
        rusqlite::Connection::open(home.path().join("googlechat_inbox.db")).unwrap()
            .execute("UPDATE pending_placeholders SET created_at=?1 WHERE message_name=?2",
                rusqlite::params![chrono::Utc::now().timestamp() - 70, orphan]).unwrap();
        let task = crate::googlechat_recovery::spawn(home.path().to_path_buf()).unwrap();
        tokio::time::pause();
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        tokio::time::resume();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let rows = store.stale_placeholders(chrono::Utc::now().timestamp() + 10).unwrap();
                if rows == vec![active_row.clone()] { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        task.abort();
        let _ = task.await;
        let patches: Vec<_> = chat::chat_calls(&fake).into_iter().filter(|c| c.0 == "PATCH").collect();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].1, format!("/{orphan}"));
        assert_eq!(patches[0].3["text"], "這次回覆中斷了，請再傳一次。");
        drop(active_registration);
    }

}

#[cfg(test)]
mod kbadd_transport_tests {
    #[test]
    fn only_http_dm_can_attest_a_bound_private_audience() {
        use crate::approval::ConversationAudience::{Private, MultiParty};
        for (http, dm, expected) in [(true, true, Private), (true, false, MultiParty),
            (false, true, MultiParty), (false, false, MultiParty)] {
            assert_eq!(crate::approval::mail_requester::adapter_audience("googlechat", super::bound_chat_type(http, dm)), expected);
        }
    }
}

#[cfg(test)]
#[path = "googlechat_attachment_gate_tests.rs"]
mod attachment_gate_tests;

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use serde_json::json;
    use crate::chat_link_intercept::CodeRoute;
    use duduclaw_auth::{UserDb, UserRole, chat_link::{self, HandshakeStatus}};

    fn event(text: &str) -> serde_json::Value {
        json!({"type": "MESSAGE",
            "space": {"name": "spaces/HANDSHAKE", "type": "DM", "singleUserBotDm": true},
            "message": {"text": text, "sender": {"name": "users/123", "type": "HUMAN", "displayName": "王小明"},
                "space": {"name": "spaces/HANDSHAKE", "type": "DM", "singleUserBotDm": true}}})
    }

    fn state(home: &Path, agent: Option<&str>) -> Arc<GoogleChatState> {
        let registry = Arc::new(RwLock::new(duduclaw_agent::AgentRegistry::new(home.join("agents"))));
        let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
        let status = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let ctx = Arc::new(ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx));
        let creds = GoogleChatCreds::candidate(
            r#"{"client_email":"handshake@example.invalid","private_key":"not-a-key"}"#,
        ).unwrap();
        Arc::new(GoogleChatState { ctx, creds: Arc::new(creds), agent: agent.map(str::to_owned) })
    }

    #[test]
    fn only_verified_http_human_message_can_match() {
        let base = event("DUDU-0123-4567");
        assert_eq!(handshake_route(&base, true), CodeRoute::Match);
        for sender in [
            json!({"name": "users/123", "type": "BOT"}),
            json!({"name": "users/123"}),
            json!({"name": "123", "type": "HUMAN"}),
            json!({"name": "users/123/other", "type": "HUMAN"}),
            json!({"name": "users/１２３", "type": "HUMAN"}),
            json!({"name": "users/", "type": "HUMAN"}),
        ] {
            let mut value = base.clone();
            value["message"]["sender"] = sender;
            assert_eq!(handshake_route(&value, true), CodeRoute::Consume);
        }
        let mut other = base;
        other["type"] = json!("UPDATED_MESSAGE");
        assert_eq!(handshake_route(&other, true), CodeRoute::Consume);
        assert_eq!(handshake_route(&event("hello"), true), CodeRoute::Conversation);
    }

    #[tokio::test]
    async fn pubsub_never_handshakes_or_decides() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
        let started = chat_link::start(&db, &admin.id, "googlechat", "session").unwrap();
        let state = state(home.path(), None);
        let code = event(&started.code);
        assert_eq!(handshake_route(&code, false), CodeRoute::Consume);
        assert!(pubsub_control_message(&code));
        process_pubsub_event(&state, &code).await;
        assert_eq!(chat_link::status(&db, &admin.id, &started.handshake_id).unwrap().unwrap().status,
            HandshakeStatus::Waiting);
        assert!(db.channel_identities_for_user(&admin.id).unwrap().is_empty());
        let decision = event("approve 00000000-0000-4000-8000-000000000001");
        assert!(pubsub_control_message(&decision));
        process_pubsub_event(&state, &decision).await;
        assert_eq!(bound_chat_type(false, true), None);
        assert!(pubsub_control_message(&json!({"type": "CARD_CLICKED"})));
        assert!(!pubsub_control_message(&event("hello")));
    }

    #[tokio::test]
    async fn http_handshake_returns_plain_reply_and_canonical_account_without_binding() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
        for agent in [None, Some("shared")] {
            let started = chat_link::start(&db, &admin.id, "googlechat", "session").unwrap();
            let response = dispatch_event(state(home.path(), agent),
                Bytes::from(serde_json::to_vec(&event(&started.code)).unwrap())).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body, json!({"text": "已收到，請回到儀表板確認。"}));
            let status = chat_link::status(&db, &admin.id, &started.handshake_id).unwrap().unwrap();
            assert_eq!(status.status, HandshakeStatus::Matched);
            assert_eq!(status.matched_account, Some(crate::chat_link_intercept::account_label("googlechat", agent)));
            assert_eq!(status.matched_principal.as_deref(), Some("users/123"));
            assert_eq!(status.matched_dm.as_deref(), Some("spaces/HANDSHAKE"));
            assert!(db.channel_identities_for_user(&admin.id).unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn group_code_without_thread_is_voided_before_conversation_resolution() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
        let started = chat_link::start(&db, &admin.id, "googlechat", "session").unwrap();
        let mut value = event(&started.code);
        for key in ["space", "message"] {
            let space = if key == "space" { &mut value["space"] } else { &mut value["message"]["space"] };
            space["type"] = json!("ROOM");
            space["singleUserBotDm"] = json!(false);
        }
        let response = dispatch_event(state(home.path(), None), Bytes::from(serde_json::to_vec(&value).unwrap())).await;
        let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, json!({"text": "請在私訊貼上代碼。"}));
        assert_eq!(chat_link::status(&db, &admin.id, &started.handshake_id).unwrap().unwrap().status,
            HandshakeStatus::Rejected);
    }
}

#[cfg(test)]
#[path = "googlechat_chat_admin_tests.rs"]
mod chat_admin_tests;
