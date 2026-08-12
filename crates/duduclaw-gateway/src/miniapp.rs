//! Telegram Mini App — the channel-embedded approval detail card (D-S1 spike).
//!
//! ## What this is
//!
//! `03b-channel-embedded-gui-capabilities.md` established that only four of
//! the nine channels can host a real webview with a免登入 identity, and that a
//! cross-channel decision surface therefore has to be **dual-track**: a micro
//! view where a webview exists, plain buttons everywhere else. This module is
//! the first track, deliberately scoped to exactly one screen — the detail of
//! a single pending approval — so the architecture can be judged before it is
//! generalised.
//!
//! Nothing here replaces the buttons. The Telegram card keeps its 同意/拒絕
//! inline keyboard; the Mini App is an **additional** third button ("查看詳
//! 情") that opens the same decision with the context that does not fit in a
//! chat bubble (the full summary, the simulated trajectory, a live countdown).
//!
//! ## Two endpoints
//!
//! - `GET  /miniapp/approval?id=<approval_id>` — one self-contained HTML page
//!   (inline CSS/JS, no third-party libraries, no CDN). It carries **no**
//!   approval data: the id in the query string is not a capability, so the
//!   page fetches everything through the POST endpoint below with the
//!   Telegram-signed `initData` attached.
//! - `POST /miniapp/api/approval` — verify `initData`, then return the detail.
//! - `POST /miniapp/api/approval/decide` — verify `initData`, then hand the
//!   decision to [`crate::decision_notify::route_press`], the exact same entry
//!   every channel dispatcher calls for a button press.
//!
//! ## Why the decision does not get its own authorization
//!
//! A Mini App session is identified by the Telegram user id inside `initData`
//! — the same `channel_user_id` a `callback_query` reports. So the press and
//! the Mini App decision are the *same* act by the *same* identity, and both
//! go through `route_press`. A second authorization path here would be a
//! second place for the matrix to drift, which is exactly the defect
//! `decision_notify` was created to remove.
//!
//! The detail endpoint has no `route_press` to hide behind, so it applies the
//! same predicate directly ([`crate::decision_notify::authorize_press`] with
//! the same three inputs) rather than inventing a looser read rule.
//!
//! ## Fail-closed posture
//!
//! - `config.toml [miniapp] enabled` defaults to **false**; every route 404s
//!   while off (a spike must not widen the attack surface of an install that
//!   never asked for it).
//! - A missing/short/malformed `initData`, a hash that does not verify against
//!   the bot token, an `auth_date` older than [`INIT_DATA_MAX_AGE_SECS`] or
//!   implausibly in the future, or a `user` field without a numeric id — each
//!   returns 401 **before any approval data is read into the response**.
//! - Hash comparison is constant-time (`ring::constant_time`).
//! - The bot token is used only as HMAC key material. It is never logged,
//!   never echoed, and never reaches the page.
//!
//! ## The verification algorithm (verbatim from the official docs)
//!
//! <https://core.telegram.org/bots/webapps> §Validating data received via the
//! Mini App:
//!
//! ```text
//! data_check_string = ...
//! secret_key = HMAC_SHA256(<bot_token>, "WebAppData")
//! if (hex(HMAC_SHA256(data_check_string, secret_key)) == hash) {
//!   // data is from Telegram
//! }
//! ```
//!
//! where *data-check-string is a chain of all received fields, sorted
//! alphabetically, in the format `key=<value>` with a line feed character
//! (`\n`, `0x0A`) used as separator*, and `hash` itself is excluded.
//!
//! Two details the prose leaves ambiguous, both settled against the reference
//! implementation published by the Telegram Mini Apps organisation
//! (`Telegram-Mini-Apps/telegram-apps`, `packages/init-data-node`):
//!
//! 1. **`signature` stays in the string.** `validation.ts::validateFp` skips
//!    only the `hash` key when building `pairs`; the Bot API 8.0 `signature`
//!    field is an ordinary member of the data-check-string. (Its sibling
//!    `validate3rdFp`, which checks the Ed25519 `signature` instead, is the
//!    one that skips both.) Dropping `signature` here would reject every
//!    real payload from a current client.
//! 2. **Key order.** `hashToken` calls `createHmac(token, 'WebAppData')` and
//!    `CreateHmacFn` is `(data, key)`, i.e. the constant is the **key** and
//!    the bot token is the **message** — matching Telegram's
//!    `HMAC_SHA256(<bot_token>, "WebAppData")` notation.
//!
//! ## Known platform limits (documented, not worked around)
//!
//! - `InlineKeyboardButton.web_app` is *"Available only in private chats
//!   between a user and the bot"* (Bot API). A card delivered to a group
//!   therefore gets no Mini App button — [`approval_web_app_url`] returns
//!   `None` for a non-positive Telegram chat id, so the existing keyboard
//!   goes out unchanged instead of the whole send failing on
//!   `BUTTON_TYPE_INVALID`.
//! - `WebAppInfo.url` must be **HTTPS**. `http://localhost:<port>` is a valid
//!   dashboard base URL but not a valid Mini App URL, so the button is
//!   attached only when `[dashboard] public_url` is https.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, SimulationNarrative};
use crate::decision_action::{DecisionAct, DecisionSource};
use crate::decision_notify::PressAuth;

type HmacSha256 = Hmac<Sha256>;

/// The constant Telegram derives the HMAC key from. Not a secret — it is
/// published in the Bot API docs; it is what binds a signature to the Mini App
/// surface rather than to Login Widget data (which uses `sha256(bot_token)`).
const WEB_APP_DATA: &[u8] = b"WebAppData";

/// How old an `auth_date` may be before the session is refused. Telegram's own
/// guidance is only *"validate `auth_date` to prevent the use of outdated
/// data"*; one hour is the tightest value that still survives a person opening
/// the card, getting interrupted, and coming back — a decision surface that
/// logs you out mid-thought trains people to rush.
pub const INIT_DATA_MAX_AGE_SECS: i64 = 3600;

/// Tolerance for a client clock running ahead of the gateway. Beyond this an
/// `auth_date` in the future is treated as expired (fail-closed) rather than
/// silently accepted.
const INIT_DATA_FUTURE_SKEW_SECS: i64 = 300;

/// Largest `initData` accepted, in bytes. Real payloads are a few hundred
/// bytes; the cap keeps an unbounded string out of the sort/HMAC path.
const INIT_DATA_MAX_BYTES: usize = 8 * 1024;

/// Summary text is free-form and may come from a tool description. Capped for
/// the card the same way the channel push caps it (CJK-safe).
const SUMMARY_MAX_CHARS: usize = 600;

/// Per-IP request budget, one minute window. The page makes two calls per
/// open (detail + optional decide), so 60 leaves ample room for a person
/// re-opening the card while making a brute-force loop pointless.
const RATE_LIMIT_PER_MIN: u32 = 60;

static MINIAPP_RATE_LIMITER: LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// ── Router ──────────────────────────────────────────────────────

#[derive(Clone)]
struct MiniAppState {
    home_dir: PathBuf,
}

/// Mount the Mini App routes. Always mounted; each handler self-gates on
/// `[miniapp] enabled` and 404s while off, so a stock install exposes no
/// behaviour and no evidence the endpoints exist.
pub fn router(home_dir: PathBuf) -> Router {
    Router::new()
        .route("/miniapp/approval", get(page_handler))
        .route("/miniapp/api/approval", post(detail_handler))
        .route("/miniapp/api/approval/decide", post(decide_handler))
        // `initData` is a few hundred bytes; 64 KiB is generous and bounds the
        // body well before the per-field cap below matters.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(MiniAppState { home_dir })
}

// ── Config gates ────────────────────────────────────────────────

/// `config.toml [miniapp] enabled`. Absent, unparseable or non-boolean ⇒
/// `false` (spike posture: opt-in only).
pub fn enabled(home_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return false;
    };
    enabled_from_toml(&content)
}

/// The pure half of [`enabled`], so the default and every malformed shape are
/// testable without a filesystem.
pub fn enabled_from_toml(content: &str) -> bool {
    content
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| {
            t.get("miniapp")
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("enabled"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false)
}

/// The dashboard base URL **only when it is https** — Telegram refuses a
/// `WebAppInfo.url` that is not. `None` covers both "no public_url configured"
/// (the `http://localhost:<port>` fallback) and "configured but plain http",
/// and both mean the same thing to the caller: no Mini App button.
pub(crate) fn https_base_url(home_dir: &Path) -> Option<String> {
    let base = crate::deep_link::dashboard_base_url(home_dir)?;
    https_base(&base)
}

/// Pure: keep `base` only if it is an https origin.
fn https_base(base: &str) -> Option<String> {
    let b = base.trim().trim_end_matches('/');
    // Scheme comparison is anchored and case-insensitive — never a `contains`
    // check (a URL like `http://x/https://y` must not pass).
    let lower = b.to_ascii_lowercase();
    (lower.starts_with("https://") && lower.len() > "https://".len()).then(|| b.to_string())
}

/// Telegram chat ids are positive for a private chat with a user and negative
/// for groups/supergroups/channels. `web_app` inline buttons are private-chat
/// only, so this is the gate that keeps a group card from failing to send.
///
/// A `@username`-style chat id (valid for `sendMessage`, never for a decision
/// destination in this codebase) parses as non-numeric and is refused.
pub fn telegram_chat_is_private(chat_id: &str) -> bool {
    matches!(chat_id.trim().parse::<i64>(), Ok(n) if n > 0)
}

/// The Mini App URL to hang off an approval card, or `None` when any of the
/// conditions is unmet: feature off, not Telegram, not a generic approval,
/// not a private chat, or no https public URL.
///
/// Every `None` branch is an honest degradation — the card goes out with the
/// buttons it already had.
pub fn approval_web_app_url(
    home_dir: &Path,
    source: DecisionSource,
    channel: &str,
    chat_id: &str,
    approval_id: &str,
) -> Option<String> {
    if channel != "telegram" || source != DecisionSource::Approval {
        return None;
    }
    if !telegram_chat_is_private(chat_id) {
        return None;
    }
    if !enabled(home_dir) {
        return None;
    }
    let base = https_base_url(home_dir)?;
    Some(approval_url(&base, approval_id))
}

/// Pure URL composition, so the encoding is testable without config.
pub fn approval_url(base: &str, approval_id: &str) -> String {
    let id: String = url::form_urlencoded::byte_serialize(approval_id.as_bytes()).collect();
    format!("{}/miniapp/approval?id={id}", base.trim_end_matches('/'))
}

// ── initData verification ───────────────────────────────────────

/// Why an `initData` was refused. Every variant is a refusal; there is no
/// "probably fine" state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitDataError {
    /// Empty, oversized, or carrying no `hash` field.
    Malformed,
    /// The HMAC did not match under any candidate bot token.
    BadHash,
    /// `auth_date` missing, unparseable, too old, or implausibly ahead.
    Expired,
    /// No `user` object, or a `user` without a numeric `id`.
    NoUser,
}

impl InitDataError {
    /// The wire code shown to the page. Deliberately coarse — the page tells a
    /// person what to do, not which check tripped.
    fn code(self) -> &'static str {
        match self {
            Self::Malformed | Self::BadHash | Self::NoUser => "invalid_session",
            Self::Expired => "expired_session",
        }
    }
}

/// The only thing verification is allowed to hand downstream: who pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInitData {
    /// Telegram user id as a string — identical in form to the
    /// `channel_user_id` a `callback_query` reports, which is what makes the
    /// authorization shared rather than parallel.
    pub user_id: String,
    pub auth_date: i64,
}

/// Build the data-check-string and pull out the received `hash`.
///
/// Returns `(data_check_string, hash)`. Every field except `hash` participates
/// — including `signature` (see module docs).
///
/// Sorting matches the reference implementation: the assembled `key=value`
/// lines are sorted, not the keys. For Telegram's field set the two orders
/// agree (no key is a prefix of another followed by a character below `=`),
/// and matching the implementation that real clients are validated against is
/// worth more than matching the prose.
pub fn data_check_string(init_data: &str) -> Option<(String, String)> {
    if init_data.is_empty() || init_data.len() > INIT_DATA_MAX_BYTES {
        return None;
    }
    let mut pairs: Vec<String> = Vec::new();
    let mut hash: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(init_data.as_bytes()) {
        if k == "hash" {
            hash = Some(v.into_owned());
            continue;
        }
        pairs.push(format!("{k}={v}"));
    }
    let hash = hash?;
    if hash.is_empty() {
        return None;
    }
    pairs.sort();
    Some((pairs.join("\n"), hash))
}

/// `hex(HMAC_SHA256(data_check_string, HMAC_SHA256(bot_token, "WebAppData")))`
/// — the value Telegram puts in `hash`.
///
/// Exposed so a test can produce a valid `initData` for a synthetic bot token
/// and prove the round trip, rather than asserting against a constant nobody
/// can re-derive.
pub fn expected_hash(bot_token: &str, data_check_string: &str) -> String {
    let mut key_mac =
        <HmacSha256 as Mac>::new_from_slice(WEB_APP_DATA).expect("hmac accepts any key length");
    key_mac.update(bot_token.as_bytes());
    let secret = key_mac.finalize().into_bytes();

    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(&secret).expect("hmac accepts any key length");
    mac.update(data_check_string.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Length-independent, data-independent byte equality.
///
/// Hand-rolled rather than pulled from a crate: `ring::constant_time` is
/// deprecated and `subtle` is not a workspace dependency. The loop accumulates
/// every difference before returning, so no byte position can be recovered
/// from timing; the length check leaks only a length, which is a fixed 32
/// bytes for a SHA-256 digest.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Constant-time equality for two hex digests, tolerant of case.
///
/// Compares the decoded bytes rather than the strings so an uppercase digest
/// is not mistaken for a forgery; a non-hex `hash` simply fails.
fn hash_matches(expected_hex: &str, received_hex: &str) -> bool {
    let (Ok(a), Ok(b)) = (hex::decode(expected_hex), hex::decode(received_hex)) else {
        return false;
    };
    constant_time_eq(&a, &b)
}

/// Verify one `initData` against one bot token.
///
/// Order is deliberate: signature first, then freshness, then identity. A
/// caller learns nothing about `auth_date` or `user` from an unsigned blob.
pub fn verify_init_data(
    init_data: &str,
    bot_token: &str,
    now_epoch: i64,
    max_age_secs: i64,
) -> Result<VerifiedInitData, InitDataError> {
    if bot_token.trim().is_empty() {
        return Err(InitDataError::BadHash);
    }
    let (dcs, received) = data_check_string(init_data).ok_or(InitDataError::Malformed)?;
    if !hash_matches(&expected_hash(bot_token, &dcs), &received) {
        return Err(InitDataError::BadHash);
    }

    // Signature verified — the fields below are now trustworthy enough to read.
    let mut auth_date: Option<i64> = None;
    let mut user_raw: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(init_data.as_bytes()) {
        match k.as_ref() {
            "auth_date" => auth_date = v.trim().parse::<i64>().ok(),
            "user" => user_raw = Some(v.into_owned()),
            _ => {}
        }
    }

    let auth_date = auth_date.ok_or(InitDataError::Expired)?;
    if max_age_secs > 0 && now_epoch.saturating_sub(auth_date) > max_age_secs {
        return Err(InitDataError::Expired);
    }
    if auth_date.saturating_sub(now_epoch) > INIT_DATA_FUTURE_SKEW_SECS {
        return Err(InitDataError::Expired);
    }

    let user_id = user_raw
        .as_deref()
        .and_then(parse_user_id)
        .ok_or(InitDataError::NoUser)?;
    Ok(VerifiedInitData {
        user_id,
        auth_date,
    })
}

/// Pull the numeric `id` out of the `user` JSON object. Non-numeric or absent
/// ⇒ `None`; a Telegram user id is always an integer, and accepting anything
/// else would let a forged-shape payload address an arbitrary
/// `channel_user_id` string.
fn parse_user_id(user_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(user_json).ok()?;
    v.get("id").and_then(|i| i.as_i64()).map(|i| i.to_string())
}

/// Verify against each candidate token in turn, returning the first success.
///
/// There are two candidates because the card may have been pushed by an
/// agent-specific bot (`agent.toml [channels.telegram]`, resolved up the
/// `reports_to` chain) or by the deployment-wide bot. Which one signed the
/// session is not knowable in advance, and guessing wrong would refuse a
/// legitimate person.
fn verify_against_any(
    init_data: &str,
    tokens: &[String],
    now_epoch: i64,
) -> Result<VerifiedInitData, InitDataError> {
    let mut last = InitDataError::BadHash;
    for t in tokens {
        match verify_init_data(init_data, t, now_epoch, INIT_DATA_MAX_AGE_SECS) {
            Ok(v) => return Ok(v),
            // A signature that verified but was stale/identity-less is a
            // terminal answer for that token; keep it rather than reporting a
            // generic bad hash.
            Err(e @ (InitDataError::Expired | InitDataError::NoUser)) => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

// ── Handlers ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DetailRequest {
    #[serde(default)]
    init_data: String,
    #[serde(default)]
    approval_id: String,
}

#[derive(Deserialize)]
struct DecideRequest {
    #[serde(default)]
    init_data: String,
    #[serde(default)]
    approval_id: String,
    #[serde(default)]
    approve: bool,
}

#[derive(Serialize)]
struct ApprovalDetail {
    id: String,
    reason_prefix: String,
    agent: String,
    action: String,
    summary: String,
    /// The same "若核准，接下來預計：1)…2)…" text the channel card renders,
    /// taken from the same function so the two surfaces cannot drift.
    trajectory: Option<String>,
    status: String,
    settled: bool,
    expires_at_epoch: Option<i64>,
    ttl_seconds: i64,
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
}

fn refused(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

/// `GET /miniapp/approval` — the page shell. Carries no approval data, and
/// deliberately does not read `?id=` server-side: the id is not a capability,
/// so the page picks it up from `location.search` and presents it alongside a
/// signed `initData` on the POST that actually returns anything.
async fn page_handler(
    State(state): State<MiniAppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if !enabled(&state.home_dir) {
        return not_found();
    }
    if !crate::license_serve::within_rate_limit(&MINIAPP_RATE_LIMITER, addr.ip(), RATE_LIMIT_PER_MIN)
    {
        return (StatusCode::TOO_MANY_REQUESTS, "").into_response();
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let html = APPROVAL_PAGE_HTML.replace("__CSP_NONCE__", &nonce);

    let mut headers = HeaderMap::new();
    // Everything the page needs is inline and nonce-tagged; the one external
    // origin is telegram.org, which serves the platform SDK (see module docs
    // and `docs/features/43-telegram-miniapp.md` — the page still works
    // without it by reading `tgWebAppData` out of the URL fragment).
    let csp = format!(
        "default-src 'none'; \
         script-src 'nonce-{nonce}' https://telegram.org; \
         style-src 'nonce-{nonce}'; \
         connect-src 'self'; \
         img-src 'none'; \
         base-uri 'none'; \
         form-action 'none'; \
         frame-ancestors https://telegram.org https://*.telegram.org"
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    (headers, Html(html)).into_response()
}

/// Resolve the bot tokens a session could legitimately have been signed with.
async fn candidate_tokens(home_dir: &Path, rec: Option<&ApprovalRecord>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(r) = rec {
        if let Some(t) = crate::goal_notify::channel_token(home_dir, &r.agent_id, "telegram").await {
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    if let Some(t) =
        crate::config_crypto::read_encrypted_config_field(home_dir, "channels", "telegram_bot_token")
            .await
    {
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Look up the approval without revealing anything about it yet.
async fn load_record(home_dir: &Path, approval_id: &str) -> Option<ApprovalRecord> {
    if approval_id.is_empty() {
        return None;
    }
    let broker = ApprovalBroker::open(home_dir).ok()?;
    broker
        .get(&ApprovalId::from(approval_id.to_string()))
        .await
        .ok()
        .flatten()
}

/// Everything both POST handlers do before they diverge: rate limit, feature
/// gate, signature check. Returns the verified Telegram user id and the
/// record (which may be `None` — reported only after verification succeeds).
async fn verified_context(
    state: &MiniAppState,
    addr: SocketAddr,
    init_data: &str,
    approval_id: &str,
) -> Result<(VerifiedInitData, Option<ApprovalRecord>), Response> {
    if !enabled(&state.home_dir) {
        return Err(not_found());
    }
    if !crate::license_serve::within_rate_limit(&MINIAPP_RATE_LIMITER, addr.ip(), RATE_LIMIT_PER_MIN)
    {
        return Err(refused(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "操作太頻繁，請稍候再試。",
        ));
    }
    // The record is read *before* the signature check, purely to learn which
    // bot could have signed the session. Nothing about it is returned on any
    // failure path — an unverified caller cannot tell an existing approval
    // from a made-up id, and the rate limiter above bounds the read.
    let rec = load_record(&state.home_dir, approval_id).await;
    let tokens = candidate_tokens(&state.home_dir, rec.as_ref()).await;
    if tokens.is_empty() {
        // No bot token means nothing can be verified. Refuse rather than
        // degrade — an unverifiable session is not a session.
        tracing::warn!("miniapp: no telegram bot token available; refusing");
        return Err(refused(
            StatusCode::UNAUTHORIZED,
            "invalid_session",
            "無法驗證此次開啟，請從通訊軟體上的按鈕重新開啟。",
        ));
    }
    match verify_against_any(init_data, &tokens, chrono::Utc::now().timestamp()) {
        Ok(v) => Ok((v, rec)),
        Err(e) => Err(refused(
            StatusCode::UNAUTHORIZED,
            e.code(),
            match e {
                InitDataError::Expired => "這個畫面開太久了，請回到對話重新開啟。",
                _ => "無法驗證此次開啟，請從通訊軟體上的按鈕重新開啟。",
            },
        )),
    }
}

/// The same three inputs [`crate::decision_notify::authorize_press`] gets on a
/// button press — role, whether any approver identity exists, and whether the
/// reader IS the account this card was delivered to.
///
/// Every input is taken from the function the press path already uses
/// (including `approval_notify::delivered_targets`), so "may read the detail"
/// and "may decide it" cannot drift apart. A looser read rule would be a data
/// leak with extra steps: the detail IS the thing worth protecting.
fn authorize_read(home_dir: &Path, rec: &ApprovalRecord, user_id: &str) -> PressAuth {
    let role = crate::decision_notify::mapped_role(home_dir, "telegram", user_id);
    let identity_active = crate::decision_notify::identity_system_active(home_dir);
    let targets = crate::approval_notify::delivered_targets(rec);
    let destination_match =
        crate::decision_notify::destination_matches_any(&targets, "telegram", user_id);
    crate::decision_notify::authorize_press(role, identity_active, destination_match)
}

/// `POST /miniapp/api/approval` — the detail, only after the session verifies
/// AND the verified person is one who may decide this approval.
async fn detail_handler(
    State(state): State<MiniAppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<DetailRequest>,
) -> Response {
    let (verified, rec) =
        match verified_context(&state, addr, &req.init_data, &req.approval_id).await {
            Ok(v) => v,
            Err(r) => return r,
        };
    let Some(rec) = rec else {
        return refused(
            StatusCode::NOT_FOUND,
            "not_found",
            "找不到這筆待辦決定，可能已經處理完了。",
        );
    };
    // A refusal here says "you may not", not "no such thing". That does tell a
    // verified Telegram user whether an id exists — accepted knowingly: ids are
    // v4 UUIDs (unguessable), and telling a colleague who legitimately received
    // the link "找不到" when the real answer is "你沒有權限" sends them looking
    // for a bug instead of a person.
    let auth = authorize_read(&state.home_dir, &rec, &verified.user_id);
    if auth != PressAuth::Allow {
        return refused(
            StatusCode::FORBIDDEN,
            "forbidden",
            &crate::decision_notify::refusal_text(auth, "查看這筆決定"),
        );
    }

    let trajectory = rec
        .simulation
        .as_ref()
        .map(SimulationNarrative::from_json)
        .and_then(|n| n.as_trajectory());

    let detail = ApprovalDetail {
        id: rec.id.as_str().to_string(),
        reason_prefix: crate::decision_notify::reason_prefix(DecisionSource::Approval).to_string(),
        agent: rec.agent_id.clone(),
        action: crate::approval_notify::zh_action_kind(&rec.action_kind).to_string(),
        summary: duduclaw_core::truncate_chars(&rec.summary, SUMMARY_MAX_CHARS),
        trajectory,
        status: rec.status.as_str().to_string(),
        settled: rec.status.is_terminal(),
        expires_at_epoch: rec.expires_at_epoch(),
        ttl_seconds: rec.ttl_seconds,
    };
    (StatusCode::OK, Json(json!({ "approval": detail }))).into_response()
}

/// `POST /miniapp/api/approval/decide` — verify, then hand off to the unified
/// decision router. No authorization happens here: `route_press` owns it, and
/// a Mini App decision is the same act by the same identity as a button press.
async fn decide_handler(
    State(state): State<MiniAppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<DecideRequest>,
) -> Response {
    let (verified, _rec) =
        match verified_context(&state, addr, &req.init_data, &req.approval_id).await {
            Ok(v) => v,
            Err(r) => return r,
        };
    let act = if req.approve {
        DecisionAct::Approve
    } else {
        DecisionAct::Deny
    };
    let action_data =
        crate::decision_action::encode(DecisionSource::Approval, act, &req.approval_id);
    match crate::decision_notify::route_press(
        &state.home_dir,
        "telegram",
        &verified.user_id,
        &action_data,
    )
    .await
    {
        Some(Ok(msg)) => (StatusCode::OK, Json(json!({ "ok": true, "message": msg }))).into_response(),
        Some(Err(msg)) => refused(StatusCode::FORBIDDEN, "refused", &msg),
        // `route_press` only returns `None` for an action id it cannot decode,
        // which this handler composed itself — treat as a bug, not a refusal.
        None => refused(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "這筆決定的編號無法辨識。",
        ),
    }
}

// ── The page ────────────────────────────────────────────────────

/// One self-contained page: inline CSS, inline JS, no third-party library, no
/// bundler, no framework. `__CSP_NONCE__` is substituted per response.
///
/// The only external resource is Telegram's own `telegram-web-app.js`, which
/// is what supplies `Telegram.WebApp` (`initData`, `themeParams`, `ready`,
/// `expand`, `close`). It is loaded from `telegram.org` inside the Telegram
/// webview — there is no supported way to obtain that object otherwise. The
/// page does not *depend* on it: if the script is unavailable, the JS falls
/// back to reading `tgWebAppData` / `tgWebAppThemeParams` out of the URL
/// fragment, which is where Telegram puts them and what the SDK itself parses.
/// That fallback is also what makes the page renderable in a plain browser
/// during verification.
///
/// All server-supplied text is written with `textContent`, never `innerHTML`.
const APPROVAL_PAGE_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>待辦決定</title>
<script nonce="__CSP_NONCE__" src="https://telegram.org/js/telegram-web-app.js"></script>
<style nonce="__CSP_NONCE__">
:root{
  --bg:#fafaf9; --fg:#1c1917; --hint:#78716c; --card:#ffffff;
  --line:#e7e5e4; --accent:#f59e0b; --accent-fg:#ffffff; --danger:#e11d48;
}
*{box-sizing:border-box}
body{
  margin:0; padding:16px 16px calc(16px + env(safe-area-inset-bottom));
  background:var(--bg); color:var(--fg);
  font:16px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang TC","Noto Sans TC",sans-serif;
  -webkit-text-size-adjust:100%;
}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px;max-width:640px;margin:0 auto}
h1{font-size:19px;margin:0 0 14px;line-height:1.4}
dl{margin:0 0 14px;display:grid;grid-template-columns:auto 1fr;gap:6px 12px}
dt{color:var(--hint);font-size:14px;white-space:nowrap}
dd{margin:0;font-size:15px;word-break:break-word}
.block{margin:14px 0;padding:12px;border-radius:10px;border:1px solid var(--line)}
.block h2{font-size:14px;color:var(--hint);margin:0 0 6px;font-weight:600}
.pre{white-space:pre-wrap;font-size:15px;margin:0}
.countdown{font-variant-numeric:tabular-nums;font-weight:600}
.countdown.soon{color:var(--danger)}
.actions{display:flex;gap:10px;margin-top:20px}
button{
  flex:1;min-height:52px;font-size:17px;font-weight:600;border-radius:12px;
  border:1px solid transparent;cursor:pointer;font-family:inherit;
}
button:disabled{opacity:.5;cursor:default}
.ok{background:var(--accent);color:var(--accent-fg)}
.no{background:transparent;color:var(--danger);border-color:var(--danger)}
.note{color:var(--hint);font-size:14px;margin-top:14px}
.err{color:var(--danger)}
.hide{display:none}
@media (prefers-reduced-motion:no-preference){.card{animation:in .18s ease-out}}
@keyframes in{from{opacity:0;transform:translateY(4px)}to{opacity:1;transform:none}}
</style>
</head>
<body>
<main class="card">
  <h1 id="title">載入中…</h1>
  <div id="body" class="hide">
    <dl>
      <dt>AI 員工</dt><dd id="agent"></dd>
      <dt>想做的事</dt><dd id="action"></dd>
    </dl>
    <div class="block"><h2>內容</h2><p id="summary" class="pre"></p></div>
    <div class="block hide" id="simwrap"><h2>如果同意，接下來會發生什麼</h2><p id="sim" class="pre"></p></div>
    <p class="note">剩餘時間：<span id="countdown" class="countdown">—</span>；逾時未回覆會自動拒絕。</p>
    <div class="actions">
      <button class="ok" id="approve" type="button">同意這個動作</button>
      <button class="no" id="deny" type="button">拒絕這個動作</button>
    </div>
  </div>
  <p id="msg" class="note"></p>
</main>
<script nonce="__CSP_NONCE__">
(function () {
  "use strict";
  var tg = (window.Telegram && window.Telegram.WebApp) || null;

  // Telegram passes the signed payload and the theme in the URL fragment
  // (`tgWebAppData`, `tgWebAppThemeParams`); the official SDK parses the very
  // same values. Reading them directly is what lets this page work when the
  // SDK script is unavailable. Decoding mirrors the SDK's own `urlSafeDecode`
  // — `+` to space first, then decodeURIComponent, with a bad escape falling
  // back to the raw text instead of throwing.
  function dec(s) {
    try { return decodeURIComponent(s.replace(/\+/g, "%20")); } catch (e) { return s; }
  }
  function fragment() {
    var out = {};
    var h = (window.location.hash || "").replace(/^#/, "");
    if (!h) { return out; }
    h.split("&").forEach(function (kv) {
      var i = kv.indexOf("=");
      if (i > 0) { out[dec(kv.slice(0, i))] = dec(kv.slice(i + 1)); }
    });
    return out;
  }
  var frag = fragment();
  var initData = (tg && tg.initData) || frag.tgWebAppData || "";

  function theme() {
    if (tg && tg.themeParams && tg.themeParams.bg_color) { return tg.themeParams; }
    try { return JSON.parse(frag.tgWebAppThemeParams || "{}"); } catch (e) { return {}; }
  }
  function applyTheme() {
    var t = theme();
    var map = {
      "--bg": t.secondary_bg_color || t.bg_color,
      "--card": t.bg_color,
      "--fg": t.text_color,
      "--hint": t.hint_color,
      "--accent": t.button_color,
      "--accent-fg": t.button_text_color,
      "--danger": t.destructive_text_color
    };
    Object.keys(map).forEach(function (k) { if (map[k]) { document.documentElement.style.setProperty(k, map[k]); } });
    if (t.bg_color && t.text_color) { document.documentElement.style.setProperty("--line", t.hint_color ? t.hint_color + "40" : "transparent"); }
  }
  applyTheme();
  if (tg) { try { tg.ready(); tg.expand(); } catch (e) {} }

  var $ = function (id) { return document.getElementById(id); };
  function say(text, isError) {
    var el = $("msg");
    el.textContent = text || "";
    el.className = isError ? "note err" : "note";
  }
  function approvalId() {
    var m = /[?&]id=([^&#]*)/.exec(window.location.search || "");
    return m ? decodeURIComponent(m[1]) : "";
  }
  var id = approvalId();
  var deadline = null;
  var ttl = 0;

  function post(path, payload) {
    return fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload)
    }).then(function (r) {
      return r.json().catch(function () { return {}; }).then(function (j) {
        return { ok: r.ok, data: j };
      });
    });
  }

  function pad(n) { return (n < 10 ? "0" : "") + n; }
  function tickCountdown() {
    if (deadline === null) { return; }
    var left = Math.max(0, deadline - Math.floor(Date.now() / 1000));
    var el = $("countdown");
    el.textContent = left <= 0
      ? "已逾時"
      : (left >= 3600 ? Math.floor(left / 3600) + " 小時 " : "") + pad(Math.floor((left % 3600) / 60)) + ":" + pad(left % 60);
    var soon = ttl > 0 ? left < ttl / 3 : left < 60;
    el.className = soon ? "countdown soon" : "countdown";
  }

  function render(a) {
    $("title").textContent = a.reason_prefix || "待辦決定";
    $("agent").textContent = a.agent || "—";
    $("action").textContent = a.action || "—";
    $("summary").textContent = a.summary || "（沒有補充說明）";
    if (a.trajectory) { $("sim").textContent = a.trajectory; $("simwrap").className = "block"; }
    ttl = a.ttl_seconds || 0;
    if (a.expires_at_epoch) { deadline = a.expires_at_epoch; tickCountdown(); setInterval(tickCountdown, 1000); }
    else { $("countdown").textContent = "—"; }
    $("body").className = "";
    if (a.settled) {
      $("approve").disabled = true;
      $("deny").disabled = true;
      say("這筆決定已經處理過了。");
    }
  }

  function decide(approve) {
    $("approve").disabled = true;
    $("deny").disabled = true;
    say("處理中…");
    post("/miniapp/api/approval/decide", { init_data: initData, approval_id: id, approve: approve })
      .then(function (res) {
        if (res.ok && res.data && res.data.ok) {
          say(res.data.message || "已送出。");
          if (tg && tg.close) { setTimeout(function () { try { tg.close(); } catch (e) {} }, 1200); }
        } else {
          say((res.data && res.data.message) || "沒有完成，請回到對話再試一次。", true);
          $("approve").disabled = false;
          $("deny").disabled = false;
        }
      })
      .catch(function () {
        say("連線失敗，請回到對話再試一次。", true);
        $("approve").disabled = false;
        $("deny").disabled = false;
      });
  }

  $("approve").addEventListener("click", function () { decide(true); });
  $("deny").addEventListener("click", function () { decide(false); });

  if (!initData) {
    $("title").textContent = "請從對話中開啟";
    say("這個畫面要從通訊軟體訊息上的「查看詳情」按鈕開啟才能確認身分。", true);
    return;
  }
  if (!id) {
    $("title").textContent = "缺少決定編號";
    say("請回到對話，改按訊息上的按鈕。", true);
    return;
  }

  post("/miniapp/api/approval", { init_data: initData, approval_id: id })
    .then(function (res) {
      if (res.ok && res.data && res.data.approval) {
        say("");
        render(res.data.approval);
      } else {
        $("title").textContent = "無法顯示";
        say((res.data && res.data.message) || "讀不到這筆決定，請回到對話操作。", true);
      }
    })
    .catch(function () {
      $("title").textContent = "無法顯示";
      say("連線失敗，請回到對話操作。", true);
    });
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway token shaped like a real one. Not a credential — it exists
    /// only so the round trip can be computed and asserted.
    const TEST_TOKEN: &str = "123456:test-bot-token-not-a-real-credential";

    /// Build a signed `initData` for `TEST_TOKEN` from a set of fields, the
    /// way Telegram would: percent-encode, sort, HMAC, append `hash`.
    fn sign(fields: &[(&str, &str)], token: &str) -> String {
        let mut pairs: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
        pairs.sort();
        let dcs = pairs.join("\n");
        let hash = expected_hash(token, &dcs);
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in fields {
            ser.append_pair(k, v);
        }
        ser.append_pair("hash", &hash);
        ser.finish()
    }

    fn user_field(id: i64) -> String {
        format!(r#"{{"id":{id},"first_name":"測試","username":"tester"}}"#)
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    // ── the documented algorithm ───────────────────────────

    #[test]
    fn data_check_string_excludes_hash_sorts_and_newline_joins() {
        // Deliberately supplied out of order.
        let raw = "user=%7B%22id%22%3A7%7D&auth_date=1700000000&query_id=AAA&hash=deadbeef";
        let (dcs, hash) = data_check_string(raw).expect("parses");
        assert_eq!(hash, "deadbeef");
        assert_eq!(
            dcs,
            "auth_date=1700000000\nquery_id=AAA\nuser={\"id\":7}",
            "fields sorted alphabetically, joined by \\n, `hash` excluded"
        );
    }

    #[test]
    fn data_check_string_keeps_signature_field() {
        // Bot API 8.0's `signature` participates in the bot-token HMAC; only
        // the third-party Ed25519 check drops it. Dropping it here would
        // reject every payload from a current client.
        let raw = "auth_date=1&signature=abc&user=%7B%22id%22%3A1%7D&hash=ff";
        let (dcs, _) = data_check_string(raw).unwrap();
        assert!(dcs.contains("signature=abc"), "got: {dcs}");
    }

    #[test]
    fn data_check_string_refuses_missing_or_empty_hash() {
        assert!(data_check_string("auth_date=1&user=x").is_none());
        assert!(data_check_string("auth_date=1&hash=").is_none());
        assert!(data_check_string("").is_none());
        // Oversized payloads never reach the sort/HMAC path.
        let huge = format!("hash=ff&pad={}", "a".repeat(INIT_DATA_MAX_BYTES));
        assert!(data_check_string(&huge).is_none());
    }

    #[test]
    fn expected_hash_matches_the_documented_two_step_derivation() {
        // secret_key = HMAC_SHA256(<bot_token>, "WebAppData")
        // hash       = hex(HMAC_SHA256(data_check_string, secret_key))
        let dcs = "auth_date=1\nuser={\"id\":1}";
        let mut k = <HmacSha256 as Mac>::new_from_slice(b"WebAppData").unwrap();
        k.update(TEST_TOKEN.as_bytes());
        let secret = k.finalize().into_bytes();
        let mut m = <HmacSha256 as Mac>::new_from_slice(&secret).unwrap();
        m.update(dcs.as_bytes());
        assert_eq!(expected_hash(TEST_TOKEN, dcs), hex::encode(m.finalize().into_bytes()));
    }

    // ── round trip + rejections ────────────────────────────

    #[test]
    fn valid_init_data_round_trips_to_the_user_id() {
        let ts = now().to_string();
        let raw = sign(
            &[
                ("query_id", "AAHdF6IQAAAAAN0XohDhrOrc"),
                ("user", &user_field(555)),
                ("auth_date", &ts),
            ],
            TEST_TOKEN,
        );
        let v = verify_init_data(&raw, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS).expect("verifies");
        assert_eq!(v.user_id, "555");
    }

    #[test]
    fn wrong_bot_token_is_refused() {
        let ts = now().to_string();
        let raw = sign(&[("user", &user_field(1)), ("auth_date", &ts)], TEST_TOKEN);
        assert_eq!(
            verify_init_data(&raw, "999:some-other-bot", now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::BadHash)
        );
    }

    #[test]
    fn tampered_field_is_refused() {
        let ts = now().to_string();
        let raw = sign(&[("user", &user_field(1)), ("auth_date", &ts)], TEST_TOKEN);
        // Swap the signed user for somebody else's, keeping the hash.
        let forged = raw.replace(
            &url::form_urlencoded::byte_serialize(user_field(1).as_bytes()).collect::<String>(),
            &url::form_urlencoded::byte_serialize(user_field(2).as_bytes()).collect::<String>(),
        );
        assert_ne!(forged, raw, "the fixture must actually differ");
        assert_eq!(
            verify_init_data(&forged, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::BadHash)
        );
    }

    #[test]
    fn flipped_hash_is_refused() {
        let ts = now().to_string();
        let raw = sign(&[("user", &user_field(1)), ("auth_date", &ts)], TEST_TOKEN);
        let broken = format!("{raw}").replace("hash=", "hash=0");
        assert_eq!(
            verify_init_data(&broken, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::BadHash)
        );
    }

    #[test]
    fn stale_auth_date_is_refused() {
        let old = (now() - INIT_DATA_MAX_AGE_SECS - 5).to_string();
        let raw = sign(&[("user", &user_field(1)), ("auth_date", &old)], TEST_TOKEN);
        assert_eq!(
            verify_init_data(&raw, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::Expired)
        );
        // Just inside the window still passes — the boundary is not a cliff
        // one second early.
        let fresh = (now() - INIT_DATA_MAX_AGE_SECS + 30).to_string();
        let ok = sign(&[("user", &user_field(1)), ("auth_date", &fresh)], TEST_TOKEN);
        assert!(verify_init_data(&ok, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS).is_ok());
    }

    #[test]
    fn far_future_auth_date_is_refused() {
        let ahead = (now() + INIT_DATA_FUTURE_SKEW_SECS + 60).to_string();
        let raw = sign(&[("user", &user_field(1)), ("auth_date", &ahead)], TEST_TOKEN);
        assert_eq!(
            verify_init_data(&raw, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::Expired)
        );
    }

    #[test]
    fn missing_auth_date_is_refused_even_when_signed() {
        let raw = sign(&[("user", &user_field(1))], TEST_TOKEN);
        assert_eq!(
            verify_init_data(&raw, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::Expired)
        );
    }

    #[test]
    fn signed_payload_without_a_numeric_user_id_is_refused() {
        let ts = now().to_string();
        for bad in [r#"{"first_name":"x"}"#, r#"{"id":"555"}"#, "not json"] {
            let raw = sign(&[("user", bad), ("auth_date", &ts)], TEST_TOKEN);
            assert_eq!(
                verify_init_data(&raw, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS),
                Err(InitDataError::NoUser),
                "user={bad}"
            );
        }
    }

    #[test]
    fn empty_bot_token_never_verifies() {
        let ts = now().to_string();
        let raw = sign(&[("user", &user_field(1)), ("auth_date", &ts)], TEST_TOKEN);
        assert_eq!(
            verify_init_data(&raw, "   ", now(), INIT_DATA_MAX_AGE_SECS),
            Err(InitDataError::BadHash)
        );
    }

    #[test]
    fn uppercase_hash_still_verifies() {
        let ts = now().to_string();
        let raw = sign(&[("user", &user_field(9)), ("auth_date", &ts)], TEST_TOKEN);
        let (dcs, h) = data_check_string(&raw).unwrap();
        let upper = raw.replace(&h, &h.to_ascii_uppercase());
        assert_eq!(expected_hash(TEST_TOKEN, &dcs), h);
        assert!(verify_init_data(&upper, TEST_TOKEN, now(), INIT_DATA_MAX_AGE_SECS).is_ok());
    }

    #[test]
    fn candidate_tokens_try_every_bot_before_refusing() {
        let ts = now().to_string();
        let raw = sign(&[("user", &user_field(77)), ("auth_date", &ts)], "second:bot");
        let tokens = vec![TEST_TOKEN.to_string(), "second:bot".to_string()];
        let v = verify_against_any(&raw, &tokens, now()).expect("second candidate matches");
        assert_eq!(v.user_id, "77");
        assert_eq!(
            verify_against_any(&raw, &[TEST_TOKEN.to_string()], now()),
            Err(InitDataError::BadHash)
        );
        assert_eq!(verify_against_any(&raw, &[], now()), Err(InitDataError::BadHash));
    }

    // ── config / URL gates ─────────────────────────────────

    #[test]
    fn feature_is_off_unless_explicitly_enabled() {
        assert!(!enabled_from_toml(""));
        assert!(!enabled_from_toml("[gateway]\nport = 1\n"));
        assert!(!enabled_from_toml("[miniapp]\n"));
        assert!(!enabled_from_toml("[miniapp]\nenabled = false\n"));
        assert!(!enabled_from_toml("[miniapp]\nenabled = \"true\"\n"));
        assert!(!enabled_from_toml("this is not [ valid toml"));
        assert!(enabled_from_toml("[miniapp]\nenabled = true\n"));
    }

    #[test]
    fn only_https_bases_qualify() {
        assert_eq!(
            https_base("https://ai.example.com/"),
            Some("https://ai.example.com".to_string())
        );
        assert_eq!(
            https_base("HTTPS://Ai.Example.com"),
            Some("HTTPS://Ai.Example.com".to_string()),
            "scheme compare is case-insensitive but the URL is passed through verbatim"
        );
        assert_eq!(https_base("http://localhost:18789"), None);
        // No substring shortcuts: an http URL that merely mentions https fails.
        assert_eq!(https_base("http://evil.test/https://ai.example.com"), None);
        assert_eq!(https_base("https://"), None);
        assert_eq!(https_base(""), None);
    }

    #[test]
    fn web_app_buttons_are_private_chat_only() {
        assert!(telegram_chat_is_private("555"));
        assert!(telegram_chat_is_private(" 555 "));
        // Groups / supergroups / channels.
        assert!(!telegram_chat_is_private("-1001234567890"));
        assert!(!telegram_chat_is_private("-555"));
        assert!(!telegram_chat_is_private("0"));
        assert!(!telegram_chat_is_private("@mychannel"));
        assert!(!telegram_chat_is_private(""));
    }

    #[test]
    fn approval_url_percent_encodes_the_id() {
        assert_eq!(
            approval_url("https://ai.example.com/", "ap-1"),
            "https://ai.example.com/miniapp/approval?id=ap-1"
        );
        assert_eq!(
            approval_url("https://ai.example.com", "a b&c=d"),
            "https://ai.example.com/miniapp/approval?id=a+b%26c%3Dd"
        );
    }

    #[test]
    fn button_url_requires_enabled_https_private_and_the_approval_source() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");

        // Enabled + https + private chat + Approval ⇒ a URL.
        std::fs::write(
            &cfg,
            "[miniapp]\nenabled = true\n\n[dashboard]\npublic_url = \"https://ai.example.com\"\n",
        )
        .unwrap();
        assert_eq!(
            approval_web_app_url(dir.path(), DecisionSource::Approval, "telegram", "555", "ap-1"),
            Some("https://ai.example.com/miniapp/approval?id=ap-1".to_string())
        );

        // Wrong channel / wrong source / group chat ⇒ None.
        assert_eq!(
            approval_web_app_url(dir.path(), DecisionSource::Approval, "slack", "555", "ap-1"),
            None
        );
        assert_eq!(
            approval_web_app_url(dir.path(), DecisionSource::Goal, "telegram", "555", "ap-1"),
            None
        );
        assert_eq!(
            approval_web_app_url(
                dir.path(),
                DecisionSource::Approval,
                "telegram",
                "-1001234567890",
                "ap-1"
            ),
            None
        );

        // http public_url ⇒ None (Telegram requires https).
        std::fs::write(
            &cfg,
            "[miniapp]\nenabled = true\n\n[gateway]\nport = 18789\n",
        )
        .unwrap();
        assert_eq!(
            approval_web_app_url(dir.path(), DecisionSource::Approval, "telegram", "555", "ap-1"),
            None
        );

        // Feature off ⇒ None even with a perfect https URL.
        std::fs::write(
            &cfg,
            "[dashboard]\npublic_url = \"https://ai.example.com\"\n",
        )
        .unwrap();
        assert_eq!(
            approval_web_app_url(dir.path(), DecisionSource::Approval, "telegram", "555", "ap-1"),
            None
        );
    }

    // ── the page ───────────────────────────────────────────

    #[test]
    fn page_is_self_contained_and_nonce_substituted() {
        let nonce = "abc123";
        let html = APPROVAL_PAGE_HTML.replace("__CSP_NONCE__", nonce);
        assert!(!html.contains("__CSP_NONCE__"), "every nonce slot substituted");
        assert!(html.contains("nonce=\"abc123\""));
        // The only external origin is telegram.org (the platform SDK).
        for (i, _) in html.match_indices("src=\"http") {
            assert!(
                html[i..].starts_with("src=\"https://telegram.org/"),
                "unexpected external script at byte {i}"
            );
        }
        assert!(!html.contains("href=\"http"), "no external stylesheets");
        assert!(!html.contains("innerHTML"), "server text is written with textContent only");
    }

    #[test]
    fn page_copy_stays_in_zh_tw_and_hides_internals() {
        let html = APPROVAL_PAGE_HTML;
        for leaked in [
            "approvals.db",
            "ApprovalBroker",
            "initData 驗證失敗",
            "bot_token",
            "HMAC",
        ] {
            assert!(!html.contains(leaked), "internal term leaked to the page: {leaked}");
        }
        assert!(html.contains("同意這個動作"));
        assert!(html.contains("拒絕這個動作"));
    }
}
