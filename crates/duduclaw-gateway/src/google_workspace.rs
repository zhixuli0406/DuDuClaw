//! Native Google Workspace REST client — Gmail + Calendar.
//!
//! This is the "native tools" path for Google integration: instead of shelling
//! out to a third-party npm MCP server, we call the Google REST APIs directly
//! and consume the access token from the existing `mcp_oauth` vault (the
//! `google` provider). Tokens are refreshed in place using the client
//! credentials the user stored during the OAuth flow.
//!
//! Security posture:
//! - Read-class Gmail/Calendar operations only expand into curated response
//!   structs — we never dump the raw Google JSON blob back to the caller.
//! - `gmail_create_draft` deliberately creates a *draft* and never sends. This
//!   is a safety default so an agent can prepare mail for human review without
//!   the ability to actually deliver it.
//! - `calendar_create_event` DOES create a real, externally-visible calendar
//!   event; operators can additionally gate it behind
//!   `agent.toml [capabilities] approval_required_tools`.

use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::google_apps_script;
use crate::google_service_account::{self, ServiceAccountConfig, ServiceAccountError};
use crate::mcp_oauth;

/// Provider id in the `mcp_oauth` vault.
pub const GOOGLE_PROVIDER: &str = "google";

/// Feature gate for the whole Google Workspace integration (tools + dashboard
/// tab + OAuth provider listing). Hidden by default until DuDu Studio's own
/// OAuth app clears Google verification; operators can opt in early via
/// `config.toml [integrations] google_workspace = true`. Missing / unreadable
/// config reads as disabled (fail closed).
pub fn integration_enabled(home_dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return false;
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("integrations")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("google_workspace"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Flip `config.toml [integrations] google_workspace = true` in place.
///
/// Called when the operator completes a Google connection through the
/// dashboard (OAuth consent or saved service-account / Apps Script
/// credentials): connecting IS the opt-in, so the default-hidden gate must not
/// stay closed behind a working credential — that combination dead-ends every
/// tool call while the credential test shows green. toml_edit round-trips the
/// file so operator comments and formatting survive (unlike the wholesale
/// `write_config_table` rewrite, which is fine for explicit saves but not for
/// a side effect). Missing config.toml is created; malformed TOML is an error
/// rather than a silent overwrite. Returns Ok(true) when the file changed,
/// Ok(false) when the flag was already on.
pub fn enable_integration(home_dir: &Path) -> std::io::Result<bool> {
    if integration_enabled(home_dir) {
        return Ok(false);
    }
    let path = home_dir.join("config.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc: toml_edit::DocumentMut = raw.parse().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("config.toml is not valid TOML, refusing to rewrite it: {e}"),
        )
    })?;
    doc.as_table_mut()
        .entry("integrations")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "[integrations] exists but is not a table",
            )
        })?
        .insert("google_workspace", toml_edit::value(true));
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";
const SHEETS_BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";
/// Google Forms API v1. Forms has **no** official remote MCP server (probed
/// 404 on 2026-07-30 and absent from Google's MCP docs), so it is served here
/// as native tools.
const FORMS_BASE: &str = "https://forms.googleapis.com/v1/forms";
/// Google Tasks API v1 — likewise no official MCP server.
const TASKS_BASE: &str = "https://tasks.googleapis.com/tasks/v1";
/// Drive / Docs / Slides v3/v1. Google *does* ship official MCP servers for
/// these three, but they are Developer-Preview-only and their terms forbid
/// exposing Pre-GA APIs to users outside your own domain — unusable in a
/// shipped product. These GA REST APIs carry no such restriction, so the
/// native tools below are the supported path (2026-07-30 decision).
const DRIVE_BASE: &str = "https://www.googleapis.com/drive/v3/files";
const DOCS_BASE: &str = "https://docs.googleapis.com/v1/documents";
const SLIDES_BASE: &str = "https://slides.googleapis.com/v1/presentations";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Max rows returned by `sheets_read` before truncation.
const SHEETS_MAX_ROWS: usize = 200;

/// Max form responses returned by `forms_list_responses` before truncation.
const FORMS_MAX_RESPONSES: usize = 50;

/// Max tasks returned by `tasks_list` before truncation.
const TASKS_MAX_ITEMS: u32 = 100;

/// Max files returned by `drive_search` before truncation.
const DRIVE_MAX_FILES: u32 = 50;

/// Max characters of extracted document / file text returned to an agent.
/// Google's own export cap is 10 MB; this is the prompt-budget cap.
const DOC_TEXT_MAX_CHARS: usize = 20_000;

/// Maximum characters of a mail body returned by `gmail_read` before truncation.
const BODY_MAX_CHARS: usize = 8000;

/// Scopes this integration needs. Used for `google_status` diagnostics and the
/// 403 re-auth guidance message.
///
/// Covers both the native tools here **and** the token reused by the official
/// Google Workspace remote MCP mounts (`preset = "google:<svc>"`, bearer
/// `oauth://google`) — Drive/Docs/Sheets/Slides scopes per Google's
/// configure-mcp-servers page, Forms/Tasks per their REST references. A token
/// granted before a scope was added here yields 403 with re-auth guidance
/// ([`scope_guidance`]) rather than a silent failure.
pub const REQUIRED_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.compose",
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/spreadsheets",
    // Drive: read-only (search + export/download). No `drive.file` — no tool
    // here creates Drive files.
    "https://www.googleapis.com/auth/drive.readonly",
    // Docs: full `documents` because `docs_append` writes; Slides stays
    // read-only (no Slides write tool ships).
    "https://www.googleapis.com/auth/documents",
    "https://www.googleapis.com/auth/presentations.readonly",
    // Forms (read-only: structure + responses) — native tools below.
    "https://www.googleapis.com/auth/forms.body.readonly",
    "https://www.googleapis.com/auth/forms.responses.readonly",
    // Tasks (read/write) — native tools below.
    "https://www.googleapis.com/auth/tasks",
    "https://www.googleapis.com/auth/userinfo.email",
];

// ── Errors ──────────────────────────────────────────────────────────────────

/// Failures obtaining a usable Google access token from the vault.
#[derive(Debug)]
pub enum GoogleAuthError {
    /// No `google` token stored — the user has never connected. Carries the
    /// token-vault path that was searched: the gateway and an agent-side MCP
    /// server each resolve DUDUCLAW_HOME independently, and a mismatch makes a
    /// freshly connected account look like it was never connected. Naming the
    /// path turns that from a mystery into a one-glance diagnosis.
    NotConnected { vault: std::path::PathBuf },
    /// Token expired and no refresh_token is available → full re-auth needed.
    NoRefreshToken,
    /// Token expired, refresh_token present, but the client credentials used to
    /// obtain it were not persisted → re-connect from the dashboard.
    ClientConfigMissing,
    /// A refresh attempt was made and failed.
    RefreshFailed(String),
    /// `[integrations.google_service_account]` is configured but unusable
    /// (missing/malformed key file, bad subject, or Google rejected the
    /// assertion). Carries the already-actionable inner message; never key
    /// material — see [`crate::google_service_account`].
    ServiceAccount(String),
    /// `[integrations.google_apps_script]` is configured but unusable (bad URL,
    /// missing secret). Carries the already-actionable inner message; never the
    /// secret — see [`crate::google_apps_script`].
    AppsScript(String),
}

impl std::fmt::Display for GoogleAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoogleAuthError::NotConnected { vault } => write!(
                f,
                "Google is not connected: no token found in {}. Open the dashboard Integrations → Google page and connect your Google account first. If you already connected there, the dashboard's gateway may be using a different DUDUCLAW_HOME than this process — compare the path above with the gateway's home directory.",
                vault.display()
            ),
            GoogleAuthError::NoRefreshToken => write!(
                f,
                "Google authorization expired and cannot be refreshed automatically. Reconnect Google from the dashboard Integrations → Google page."
            ),
            GoogleAuthError::ClientConfigMissing => write!(
                f,
                "Google authorization expired and the stored client credentials are missing. Reconnect Google from the dashboard Integrations → Google page."
            ),
            GoogleAuthError::RefreshFailed(e) => write!(
                f,
                "Failed to refresh Google authorization ({e}). Reconnect Google from the dashboard Integrations → Google page."
            ),
            GoogleAuthError::ServiceAccount(e) => write!(f, "{e}"),
            GoogleAuthError::AppsScript(e) => write!(f, "{e}"),
        }
    }
}

/// Failures calling the Google REST APIs.
#[derive(Debug)]
pub enum GoogleApiError {
    /// Token acquisition failed (see [`GoogleAuthError`]).
    Auth(GoogleAuthError),
    /// Transport-level failure (DNS, TLS, timeout).
    Http(String),
    /// 401 — the token is invalid/revoked; the user should re-connect.
    Unauthorized,
    /// 403 — insufficient scope; carries re-auth guidance.
    Forbidden(String),
    /// 429 — rate limited after one retry.
    RateLimited,
    /// Any other non-success status.
    Api { status: u16, message: String },
}

impl std::fmt::Display for GoogleApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoogleApiError::Auth(e) => write!(f, "{e}"),
            GoogleApiError::Http(e) => write!(f, "Network error contacting Google: {e}"),
            GoogleApiError::Unauthorized => write!(
                f,
                "Google rejected the request (401 Unauthorized). The authorization is no longer valid — reconnect Google from the dashboard Integrations → Google page."
            ),
            GoogleApiError::Forbidden(msg) => write!(f, "{msg}"),
            GoogleApiError::RateLimited => write!(
                f,
                "Google rate-limited the request (429). Please retry in a moment."
            ),
            GoogleApiError::Api { status, message } => {
                write!(f, "Google API error ({status}): {message}")
            }
        }
    }
}

impl From<GoogleAuthError> for GoogleApiError {
    fn from(e: GoogleAuthError) -> Self {
        GoogleApiError::Auth(e)
    }
}

// ── Response structs (curated, never raw Google JSON) ───────────────────────

#[derive(Debug, Serialize)]
pub struct GmailMessageMeta {
    pub id: String,
    pub thread_id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub snippet: String,
}

#[derive(Debug, Serialize)]
pub struct GmailSearchResult {
    pub count: usize,
    pub messages: Vec<GmailMessageMeta>,
}

#[derive(Debug, Serialize)]
pub struct GmailAttachment {
    pub filename: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct GmailReadResult {
    pub id: String,
    pub thread_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,
    pub snippet: String,
    pub body: String,
    pub body_truncated: bool,
    pub attachments: Vec<GmailAttachment>,
}

#[derive(Debug, Serialize)]
pub struct GmailDraftResult {
    pub draft_id: String,
    pub message_id: String,
    pub to: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
pub struct CalendarEvent {
    pub id: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub location: Option<String>,
    pub html_link: String,
    pub meet_link: Option<String>,
    pub attendees_count: usize,
}

#[derive(Debug, Serialize)]
pub struct CalendarEventsResult {
    pub count: usize,
    pub events: Vec<CalendarEvent>,
}

#[derive(Debug, Serialize)]
pub struct CalendarCreateResult {
    pub id: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub html_link: String,
    pub meet_link: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SheetsReadResult {
    pub range: String,
    pub row_count: usize,
    pub rows_truncated: bool,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct SheetsAppendResult {
    pub updated_range: String,
    pub updated_rows: u64,
    pub updated_cells: u64,
}

// ── Forms result shapes ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct FormQuestion {
    pub question_id: String,
    pub title: String,
    /// `text` / `choice` / `scale` / `date` / `time` / `file_upload` / `rating`
    /// / `row` / `other`, derived from which `questionItem.question.*` variant
    /// the API returned.
    pub kind: String,
    /// Choice options, when the question is a choice question.
    pub options: Vec<String>,
    pub required: bool,
}

#[derive(Debug, Serialize)]
pub struct FormResult {
    pub form_id: String,
    pub title: String,
    pub description: String,
    pub question_count: usize,
    pub questions: Vec<FormQuestion>,
}

#[derive(Debug, Serialize)]
pub struct FormResponseEntry {
    pub response_id: String,
    pub submitted_at: String,
    pub respondent_email: String,
    /// question_id → the answer text(s) joined with `, `.
    pub answers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct FormResponsesResult {
    pub form_id: String,
    pub count: usize,
    pub truncated: bool,
    pub responses: Vec<FormResponseEntry>,
}

// ── Tasks result shapes ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TaskListEntry {
    pub id: String,
    pub title: String,
    pub updated: String,
}

#[derive(Debug, Serialize)]
pub struct TaskListsResult {
    pub count: usize,
    pub lists: Vec<TaskListEntry>,
}

#[derive(Debug, Serialize)]
pub struct TaskEntry {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// `needsAction` or `completed` (the API's own enum values).
    pub status: String,
    pub due: String,
    pub completed: String,
    /// Parent task id when this is a subtask.
    pub parent: String,
}

#[derive(Debug, Serialize)]
pub struct TasksResult {
    pub task_list_id: String,
    pub count: usize,
    pub truncated: bool,
    pub tasks: Vec<TaskEntry>,
}

// ── Drive / Docs / Slides result shapes ─────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DriveFileMeta {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub modified_time: String,
    /// Byte size as reported by Drive; empty for Google-native docs (they have
    /// no blob size).
    pub size: String,
    pub web_view_link: String,
}

#[derive(Debug, Serialize)]
pub struct DriveSearchResult {
    pub count: usize,
    pub truncated: bool,
    pub files: Vec<DriveFileMeta>,
}

#[derive(Debug, Serialize)]
pub struct DriveReadResult {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    /// The MIME type the file was exported as (Google-native docs), or `None`
    /// when the bytes were downloaded verbatim.
    pub exported_as: Option<String>,
    pub content: String,
    pub truncated: bool,
    /// Set when the file could not be rendered as text (binary type).
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DocsReadResult {
    pub document_id: String,
    pub title: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct DocsAppendResult {
    pub document_id: String,
    pub appended_chars: usize,
}

#[derive(Debug, Serialize)]
pub struct SlideText {
    /// 1-based slide position as presented.
    pub index: usize,
    pub object_id: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SlidesReadResult {
    pub presentation_id: String,
    pub title: String,
    pub slide_count: usize,
    pub slides: Vec<SlideText>,
}

// ── Backend selection ───────────────────────────────────────────────────────

/// Which credential path a Google call should take.
///
/// Three sources exist because no single one covers every customer:
/// `Direct` needs a Google-verified OAuth app (or the customer's own client),
/// `Direct` via service account needs a Workspace domain and a super admin, and
/// [`AppsScript`](GoogleBackend::AppsScript) is the only one a personal
/// `@gmail.com` user can set up alone. Both `Direct` variants collapse into one
/// here because downstream they are identical: a bearer token against the REST
/// APIs.
pub enum GoogleBackend {
    /// A bearer access token — from the OAuth vault or a service-account
    /// assertion. All nineteen tools work.
    Direct(String),
    /// The user-deployed Apps Script web app. Gmail / Calendar / Sheets only.
    AppsScript(google_apps_script::BridgeConfig),
}

/// Resolve the credential path for this home.
///
/// Precedence — most-deliberate configuration first: service account, then
/// OAuth vault, then the Apps Script bridge. The bridge sits last because it
/// covers the fewest tools; a home with both a working OAuth connection and a
/// deployed bridge gets the fuller surface.
pub async fn resolve_backend(home_dir: &Path) -> Result<GoogleBackend, GoogleAuthError> {
    match get_valid_google_token(home_dir).await {
        Ok(token) => Ok(GoogleBackend::Direct(token)),
        Err(direct_err) => {
            match google_apps_script::config_for_home(home_dir).await {
                Ok(Some(cfg)) => Ok(GoogleBackend::AppsScript(cfg)),
                // A broken bridge config is reported as-is: the operator wrote
                // that section on purpose, so its error is more useful than the
                // "you never connected Google" one it would otherwise hide.
                Err(bridge_err) => Err(GoogleAuthError::AppsScript(bridge_err.to_string())),
                Ok(None) => Err(direct_err),
            }
        }
    }
}

// ── Token acquisition ───────────────────────────────────────────────────────

/// Read `[integrations.google_service_account]` from the home's `config.toml`.
///
/// An unreadable config is treated as "not configured" (same fail-safe posture
/// as [`integration_enabled`]) — a present but malformed section is an error, so
/// a typo cannot silently downgrade the operator to the OAuth path.
fn service_account_config(
    home_dir: &Path,
) -> Result<Option<ServiceAccountConfig>, ServiceAccountError> {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return Ok(None);
    };
    google_service_account::parse_config(&raw, home_dir)
}

/// Return a valid Google access token, refreshing in place if expired.
///
/// Two credential sources, checked in this order:
///
/// 1. **Service account + domain-wide delegation**, when
///    `[integrations.google_service_account]` is configured. Deliberate operator
///    configuration wins over a per-user OAuth token — silently preferring a
///    stale vault token would make the operator's intent unobservable. See
///    [`crate::google_service_account`] for why this path exists (it needs no
///    Google app verification) and what it costs (Workspace domains only).
/// 2. **OAuth vault** — the per-user connect flow. Fast path returns the stored
///    non-expired token; on expiry we use the user-stored client credentials
///    (persisted at OAuth-config time) to run a refresh grant, persist the new
///    token, and return it.
///
/// Every failure mode maps to an actionable [`GoogleAuthError`].
pub async fn get_valid_google_token(home_dir: &Path) -> Result<String, GoogleAuthError> {
    // Source 1 — service account. A configured-but-broken service account is an
    // error, never a silent fall-through to OAuth: the operator asked for this
    // credential, so a misconfiguration has to be visible rather than masked by
    // whatever token happens to be in the vault.
    match service_account_config(home_dir) {
        Ok(Some(cfg)) => {
            return google_service_account::get_token(&cfg, REQUIRED_SCOPES)
                .await
                .map_err(|e| GoogleAuthError::ServiceAccount(e.to_string()));
        }
        Ok(None) => {}
        Err(e) => return Err(GoogleAuthError::ServiceAccount(e.to_string())),
    }

    // Source 2 — OAuth vault. Fast path: `get_token` already filters out
    // expired tokens.
    if let Some(t) = mcp_oauth::get_token(home_dir, GOOGLE_PROVIDER) {
        return Ok(t.access_token);
    }

    // Not returned by get_token ⇒ either missing entirely or expired.
    let existing = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == GOOGLE_PROVIDER);
    let existing = match existing {
        Some(t) => t,
        None => {
            return Err(GoogleAuthError::NotConnected {
                vault: home_dir.join(mcp_oauth::TOKEN_FILE),
            })
        }
    };

    let refresh_tok = match existing.refresh_token.as_deref() {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return Err(GoogleAuthError::NoRefreshToken),
    };

    let client_cfg = mcp_oauth::get_client_config(home_dir, GOOGLE_PROVIDER)
        .ok_or(GoogleAuthError::ClientConfigMissing)?;

    let oauth_config = mcp_oauth::McpOAuthConfig {
        provider_id: GOOGLE_PROVIDER.to_string(),
        client_id: client_cfg.client_id,
        client_secret: client_cfg.client_secret,
        auth_url: client_cfg.auth_url,
        token_url: client_cfg.token_url,
        // Preserve the scopes the token was granted with.
        scopes: existing.scopes.clone(),
        redirect_uri: client_cfg.redirect_uri,
    };

    let mut refreshed = mcp_oauth::refresh_token(&oauth_config, &refresh_tok)
        .await
        .map_err(GoogleAuthError::RefreshFailed)?;
    if refreshed.scopes.is_empty() {
        refreshed.scopes = existing.scopes.clone();
    }
    let access = refreshed.access_token.clone();
    mcp_oauth::upsert_token(home_dir, refreshed)
        .map_err(GoogleAuthError::RefreshFailed)?;
    Ok(access)
}

// ── HTTP plumbing ───────────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::Client, GoogleApiError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| GoogleApiError::Http(format!("client build failed: {e}")))
}

/// Perform one Google REST call with a single retry on transport error / 429 /
/// 5xx, then classify the result. Success bodies are parsed as JSON.
async fn google_request(
    token: &str,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value, GoogleApiError> {
    let client = http_client()?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = client.request(method.clone(), url).bearer_auth(token);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < 2 {
                    continue;
                }
                return Err(GoogleApiError::Http(format!("request failed: {e}")));
            }
        };

        let code = resp.status().as_u16();
        if resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text)
                .map_err(|e| GoogleApiError::Http(format!("invalid JSON from Google: {e}")));
        }

        // Retry once on transient failures.
        if (code == 429 || (500..=599).contains(&code)) && attempt < 2 {
            continue;
        }

        let body_text = resp.text().await.unwrap_or_default();
        return Err(match code {
            401 => GoogleApiError::Unauthorized,
            403 => GoogleApiError::Forbidden(scope_guidance(&body_text)),
            429 => GoogleApiError::RateLimited,
            _ => GoogleApiError::Api {
                status: code,
                message: duduclaw_core::truncate_chars(&extract_api_message(&body_text), 240),
            },
        });
    }
}

/// Like [`google_request`] but returns the response body verbatim as text.
/// Needed by Drive export / `alt=media`, whose bodies are plain text or CSV,
/// not JSON. Same retry + status classification.
async fn google_request_text(
    token: &str,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
) -> Result<String, GoogleApiError> {
    let client = http_client()?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = client.request(method.clone(), url).bearer_auth(token);
        if !query.is_empty() {
            req = req.query(query);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < 2 {
                    continue;
                }
                return Err(GoogleApiError::Http(format!("request failed: {e}")));
            }
        };

        let code = resp.status().as_u16();
        if resp.status().is_success() {
            return resp
                .text()
                .await
                .map_err(|e| GoogleApiError::Http(format!("failed reading body: {e}")));
        }

        if (code == 429 || (500..=599).contains(&code)) && attempt < 2 {
            continue;
        }

        let body_text = resp.text().await.unwrap_or_default();
        return Err(match code {
            401 => GoogleApiError::Unauthorized,
            403 => GoogleApiError::Forbidden(scope_guidance(&body_text)),
            429 => GoogleApiError::RateLimited,
            _ => GoogleApiError::Api {
                status: code,
                message: duduclaw_core::truncate_chars(&extract_api_message(&body_text), 240),
            },
        });
    }
}

// ── Gmail ───────────────────────────────────────────────────────────────────

/// Search the user's mailbox. `query` uses Gmail search syntax (e.g.
/// `from:alice is:unread`). `max_results` is clamped to 1..=25.
pub async fn gmail_search(
    token: &str,
    query: &str,
    max_results: u32,
) -> Result<GmailSearchResult, GoogleApiError> {
    let n = clamp(max_results, 1, 25);
    let list = google_request(
        token,
        reqwest::Method::GET,
        &format!("{GMAIL_BASE}/messages"),
        &[("q", query.to_string()), ("maxResults", n.to_string())],
        None,
    )
    .await?;

    let ids: Vec<(String, String)> = list
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    let tid = m
                        .get("threadId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((id, tid))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut messages = Vec::with_capacity(ids.len());
    for (id, thread_id) in ids {
        let meta = google_request(
            token,
            reqwest::Method::GET,
            &format!("{GMAIL_BASE}/messages/{id}"),
            &[
                ("format", "metadata".to_string()),
                ("metadataHeaders", "From".to_string()),
                ("metadataHeaders", "Subject".to_string()),
                ("metadataHeaders", "Date".to_string()),
            ],
            None,
        )
        .await?;

        let headers = meta
            .get("payload")
            .and_then(|p| p.get("headers"))
            .cloned()
            .unwrap_or(Value::Null);

        messages.push(GmailMessageMeta {
            id,
            thread_id,
            from: header_value(&headers, "From").unwrap_or_default(),
            subject: header_value(&headers, "Subject").unwrap_or_default(),
            date: header_value(&headers, "Date").unwrap_or_default(),
            snippet: meta
                .get("snippet")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }

    Ok(GmailSearchResult {
        count: messages.len(),
        messages,
    })
}

/// Read a single message in full: headers, plain-text (or html-stripped) body
/// truncated to [`BODY_MAX_CHARS`], and an attachment manifest (filename + size
/// only — attachments are never downloaded).
pub async fn gmail_read(token: &str, message_id: &str) -> Result<GmailReadResult, GoogleApiError> {
    let msg = google_request(
        token,
        reqwest::Method::GET,
        &format!("{GMAIL_BASE}/messages/{message_id}"),
        &[("format", "full".to_string())],
        None,
    )
    .await?;

    let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
    let headers = payload.get("headers").cloned().unwrap_or(Value::Null);

    let body_raw = extract_message_body(&payload);
    let body = duduclaw_core::truncate_chars(&body_raw, BODY_MAX_CHARS);
    let body_truncated = body.chars().count() < body_raw.chars().count();
    let attachments = collect_attachments(&payload);

    Ok(GmailReadResult {
        id: message_id.to_string(),
        thread_id: msg
            .get("threadId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        from: header_value(&headers, "From").unwrap_or_default(),
        to: header_value(&headers, "To").unwrap_or_default(),
        subject: header_value(&headers, "Subject").unwrap_or_default(),
        date: header_value(&headers, "Date").unwrap_or_default(),
        snippet: msg
            .get("snippet")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        body,
        body_truncated,
        attachments,
    })
}

/// Create a Gmail **draft** (never sends). Body is plain text; subject is
/// RFC 2047-encoded when it contains non-ASCII (CJK-safe).
pub async fn gmail_create_draft(
    token: &str,
    to: &str,
    subject: &str,
    body_text: &str,
    cc: Option<&str>,
) -> Result<GmailDraftResult, GoogleApiError> {
    let raw = build_rfc2822(to, subject, body_text, cc);
    let encoded = encode_raw_message(&raw);
    let payload = json!({ "message": { "raw": encoded } });

    let resp = google_request(
        token,
        reqwest::Method::POST,
        &format!("{GMAIL_BASE}/drafts"),
        &[],
        Some(&payload),
    )
    .await?;

    Ok(GmailDraftResult {
        draft_id: resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        message_id: resp
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        to: to.to_string(),
        subject: subject.to_string(),
    })
}

// ── Calendar ────────────────────────────────────────────────────────────────

/// List events on the primary calendar. Defaults to the next 7 days when
/// `time_min` / `time_max` are omitted. `max_results` clamped to 1..=50.
pub async fn calendar_list_events(
    token: &str,
    time_min: Option<&str>,
    time_max: Option<&str>,
    max_results: u32,
) -> Result<CalendarEventsResult, GoogleApiError> {
    let n = clamp(max_results, 1, 50);
    let (def_min, def_max) = default_time_range();
    let tmin = time_min.map(|s| s.to_string()).unwrap_or(def_min);
    let tmax = time_max.map(|s| s.to_string()).unwrap_or(def_max);

    let resp = google_request(
        token,
        reqwest::Method::GET,
        &format!("{CALENDAR_BASE}/calendars/primary/events"),
        &[
            ("singleEvents", "true".to_string()),
            ("orderBy", "startTime".to_string()),
            ("timeMin", tmin),
            ("timeMax", tmax),
            ("maxResults", n.to_string()),
        ],
        None,
    )
    .await?;

    let events: Vec<CalendarEvent> = resp
        .get("items")
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().map(parse_calendar_event).collect())
        .unwrap_or_default();

    Ok(CalendarEventsResult {
        count: events.len(),
        events,
    })
}

/// Create a real calendar event on the primary calendar. When `with_meet` is
/// true a Google Meet link is requested (`conferenceDataVersion=1`).
pub async fn calendar_create_event(
    token: &str,
    summary: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    description: Option<&str>,
    attendees: Option<&[String]>,
    with_meet: bool,
) -> Result<CalendarCreateResult, GoogleApiError> {
    let mut body = json!({
        "summary": summary,
        "start": { "dateTime": start_rfc3339 },
        "end": { "dateTime": end_rfc3339 },
    });
    if let Some(d) = description {
        if !d.is_empty() {
            body["description"] = json!(d);
        }
    }
    if let Some(atts) = attendees {
        if !atts.is_empty() {
            body["attendees"] = Value::Array(atts.iter().map(|e| json!({ "email": e })).collect());
        }
    }

    let mut query: Vec<(&str, String)> = Vec::new();
    if with_meet {
        body["conferenceData"] = json!({
            "createRequest": {
                "requestId": uuid::Uuid::new_v4().to_string(),
                "conferenceSolutionKey": { "type": "hangoutsMeet" }
            }
        });
        query.push(("conferenceDataVersion", "1".to_string()));
    }

    let resp = google_request(
        token,
        reqwest::Method::POST,
        &format!("{CALENDAR_BASE}/calendars/primary/events"),
        &query,
        Some(&body),
    )
    .await?;

    Ok(CalendarCreateResult {
        id: resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        summary: resp
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or(summary)
            .to_string(),
        start: event_time(&resp, "start"),
        end: event_time(&resp, "end"),
        html_link: resp
            .get("htmlLink")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        meet_link: extract_meet_link(&resp),
    })
}

// ── Google Sheets ───────────────────────────────────────────────────────────

/// Read a rectangular value range from a spreadsheet (read-only). `range` is an
/// A1 notation range, optionally sheet-qualified (`Sheet1!A1:C10`). Returns up
/// to [`SHEETS_MAX_ROWS`] rows; cells are returned as their formatted string
/// values.
pub async fn sheets_read(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
) -> Result<SheetsReadResult, GoogleApiError> {
    let id = extract_spreadsheet_id(spreadsheet_id);
    let url = format!(
        "{SHEETS_BASE}/{}/values/{}",
        encode_path_component(&id),
        encode_path_component(range)
    );
    let resp = google_request(token, reqwest::Method::GET, &url, &[], None).await?;

    let all_rows: Vec<Vec<String>> = resp
        .get("values")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(cell_to_string).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();

    let rows_truncated = all_rows.len() > SHEETS_MAX_ROWS;
    let rows: Vec<Vec<String>> = all_rows.into_iter().take(SHEETS_MAX_ROWS).collect();

    Ok(SheetsReadResult {
        range: resp
            .get("range")
            .and_then(|v| v.as_str())
            .unwrap_or(range)
            .to_string(),
        row_count: rows.len(),
        rows_truncated,
        rows,
    })
}

/// Append one row of values to a spreadsheet (write). Uses `USER_ENTERED` input
/// so numbers/dates/formulas are parsed the way a human typing them would be.
pub async fn sheets_append(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<String>,
) -> Result<SheetsAppendResult, GoogleApiError> {
    let id = extract_spreadsheet_id(spreadsheet_id);
    let url = format!(
        "{SHEETS_BASE}/{}/values/{}:append",
        encode_path_component(&id),
        encode_path_component(range)
    );
    let body = json!({ "values": [values] });
    let resp = google_request(
        token,
        reqwest::Method::POST,
        &url,
        &[
            ("valueInputOption", "USER_ENTERED".to_string()),
            ("insertDataOption", "INSERT_ROWS".to_string()),
        ],
        Some(&body),
    )
    .await?;

    let updates = resp.get("updates").cloned().unwrap_or(Value::Null);
    Ok(SheetsAppendResult {
        updated_range: updates
            .get("updatedRange")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        updated_rows: updates.get("updatedRows").and_then(|v| v.as_u64()).unwrap_or(0),
        updated_cells: updates.get("updatedCells").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

// ── Forms (no official MCP server — native tools) ───────────────────────────

/// Read a form's structure: title, description, and the question list with the
/// `question_id`s needed to interpret [`forms_list_responses`] answers.
/// Read-only (`forms.body.readonly`).
pub async fn forms_get(token: &str, form_id: &str) -> Result<FormResult, GoogleApiError> {
    let id = extract_form_id(form_id);
    let url = format!("{FORMS_BASE}/{}", encode_path_component(&id));
    let resp = google_request(token, reqwest::Method::GET, &url, &[], None).await?;

    let info = resp.get("info").cloned().unwrap_or(Value::Null);
    let questions: Vec<FormQuestion> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_form_item).collect())
        .unwrap_or_default();

    Ok(FormResult {
        form_id: resp
            .get("formId")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string(),
        title: info
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: info
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        question_count: questions.len(),
        questions,
    })
}

/// List a form's submitted responses (read-only, `forms.responses.readonly`).
/// Capped at [`FORMS_MAX_RESPONSES`]; answers are keyed by `question_id` —
/// pair with [`forms_get`] to map ids to question titles.
pub async fn forms_list_responses(
    token: &str,
    form_id: &str,
) -> Result<FormResponsesResult, GoogleApiError> {
    let id = extract_form_id(form_id);
    let url = format!("{FORMS_BASE}/{}/responses", encode_path_component(&id));
    let resp = google_request(
        token,
        reqwest::Method::GET,
        &url,
        &[("pageSize", FORMS_MAX_RESPONSES.to_string())],
        None,
    )
    .await?;

    let all: Vec<&Value> = resp
        .get("responses")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // A nextPageToken means Google has more than one page for us.
    let truncated = resp.get("nextPageToken").and_then(|v| v.as_str()).is_some();
    let responses: Vec<FormResponseEntry> = all
        .into_iter()
        .take(FORMS_MAX_RESPONSES)
        .map(parse_form_response)
        .collect();

    Ok(FormResponsesResult {
        form_id: id,
        count: responses.len(),
        truncated,
        responses,
    })
}

// ── Tasks (no official MCP server — native tools) ───────────────────────────

/// List the user's task lists (each id is the `task_list_id` other task tools
/// take).
pub async fn tasks_list_tasklists(token: &str) -> Result<TaskListsResult, GoogleApiError> {
    let url = format!("{TASKS_BASE}/users/@me/lists");
    let resp = google_request(
        token,
        reqwest::Method::GET,
        &url,
        &[("maxResults", "100".to_string())],
        None,
    )
    .await?;

    let lists: Vec<TaskListEntry> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|l| TaskListEntry {
                    id: str_field(l, "id"),
                    title: str_field(l, "title"),
                    updated: str_field(l, "updated"),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TaskListsResult {
        count: lists.len(),
        lists,
    })
}

/// List tasks in one task list. `show_completed` includes finished tasks
/// (Google also requires `showHidden` for tasks hidden by a list clear).
pub async fn tasks_list(
    token: &str,
    task_list_id: &str,
    show_completed: bool,
    max_results: u32,
) -> Result<TasksResult, GoogleApiError> {
    let n = clamp(max_results, 1, TASKS_MAX_ITEMS);
    let url = format!(
        "{TASKS_BASE}/lists/{}/tasks",
        encode_path_component(task_list_id)
    );
    let mut query = vec![
        ("maxResults", n.to_string()),
        ("showCompleted", show_completed.to_string()),
    ];
    if show_completed {
        // Completed-and-hidden tasks stay invisible without this.
        query.push(("showHidden", "true".to_string()));
    }
    let resp = google_request(token, reqwest::Method::GET, &url, &query, None).await?;

    let tasks: Vec<TaskEntry> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(parse_task).collect())
        .unwrap_or_default();
    let truncated = resp.get("nextPageToken").and_then(|v| v.as_str()).is_some();

    Ok(TasksResult {
        task_list_id: task_list_id.to_string(),
        count: tasks.len(),
        truncated,
        tasks,
    })
}

/// Create a task in one task list (write). `due` is an RFC-3339 timestamp —
/// Google Tasks stores only the date part.
pub async fn tasks_create(
    token: &str,
    task_list_id: &str,
    title: &str,
    notes: Option<&str>,
    due: Option<&str>,
) -> Result<TaskEntry, GoogleApiError> {
    let url = format!(
        "{TASKS_BASE}/lists/{}/tasks",
        encode_path_component(task_list_id)
    );
    let mut body = serde_json::Map::new();
    body.insert("title".into(), json!(title));
    if let Some(n) = notes.filter(|s| !s.trim().is_empty()) {
        body.insert("notes".into(), json!(n));
    }
    if let Some(d) = due.filter(|s| !s.trim().is_empty()) {
        body.insert("due".into(), json!(d));
    }
    let resp = google_request(
        token,
        reqwest::Method::POST,
        &url,
        &[],
        Some(&Value::Object(body)),
    )
    .await?;
    Ok(parse_task(&resp))
}

/// Mark a task completed (write) — `status: "completed"` via PATCH.
pub async fn tasks_complete(
    token: &str,
    task_list_id: &str,
    task_id: &str,
) -> Result<TaskEntry, GoogleApiError> {
    let url = format!(
        "{TASKS_BASE}/lists/{}/tasks/{}",
        encode_path_component(task_list_id),
        encode_path_component(task_id)
    );
    let body = json!({ "status": "completed" });
    let resp = google_request(token, reqwest::Method::PATCH, &url, &[], Some(&body)).await?;
    Ok(parse_task(&resp))
}

// ── Drive / Docs / Slides (GA REST — no preview gate) ───────────────────────

/// Search the user's Drive by free text. Matches file names **and** full text,
/// excludes trashed files, newest first. Read-only (`drive.readonly`).
pub async fn drive_search(
    token: &str,
    query: &str,
    mime_type: Option<&str>,
    max_results: u32,
) -> Result<DriveSearchResult, GoogleApiError> {
    let n = clamp(max_results, 1, DRIVE_MAX_FILES);
    let mut q = format!(
        "trashed = false and (name contains '{}' or fullText contains '{}')",
        escape_drive_query_value(query),
        escape_drive_query_value(query)
    );
    if let Some(m) = mime_type.filter(|s| !s.trim().is_empty()) {
        q.push_str(&format!(
            " and mimeType = '{}'",
            escape_drive_query_value(m)
        ));
    }

    let resp = google_request(
        token,
        reqwest::Method::GET,
        DRIVE_BASE,
        &[
            ("q", q),
            ("pageSize", n.to_string()),
            ("orderBy", "modifiedTime desc".to_string()),
            (
                "fields",
                "nextPageToken,files(id,name,mimeType,modifiedTime,size,webViewLink)".to_string(),
            ),
            // Shared-drive items are invisible without both flags.
            ("includeItemsFromAllDrives", "true".to_string()),
            ("supportsAllDrives", "true".to_string()),
        ],
        None,
    )
    .await?;

    let files: Vec<DriveFileMeta> = resp
        .get("files")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|f| DriveFileMeta {
                    id: str_field(f, "id"),
                    name: str_field(f, "name"),
                    mime_type: str_field(f, "mimeType"),
                    modified_time: str_field(f, "modifiedTime"),
                    size: str_field(f, "size"),
                    web_view_link: str_field(f, "webViewLink"),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(DriveSearchResult {
        count: files.len(),
        truncated: resp.get("nextPageToken").and_then(|v| v.as_str()).is_some(),
        files,
    })
}

/// Read a Drive file as text. Google-native documents are **exported**
/// (Docs → `text/plain`, Sheets → `text/csv` first sheet only, Slides →
/// `text/plain`); text-ish blobs are downloaded verbatim; anything else
/// returns metadata plus a note instead of binary garbage.
pub async fn drive_read(token: &str, file_id: &str) -> Result<DriveReadResult, GoogleApiError> {
    let id = extract_drive_file_id(file_id);
    // Metadata first: the MIME type decides export vs download.
    let meta = google_request(
        token,
        reqwest::Method::GET,
        &format!("{DRIVE_BASE}/{}", encode_path_component(&id)),
        &[
            ("fields", "id,name,mimeType".to_string()),
            ("supportsAllDrives", "true".to_string()),
        ],
        None,
    )
    .await?;
    let name = str_field(&meta, "name");
    let mime = str_field(&meta, "mimeType");

    // Google-native → export; text-ish blob → download; else refuse politely.
    let (url, query, exported_as) = match export_mime_for(&mime) {
        Some(export_mime) => (
            format!("{DRIVE_BASE}/{}/export", encode_path_component(&id)),
            vec![("mimeType", export_mime.to_string())],
            Some(export_mime.to_string()),
        ),
        None if is_text_like_mime(&mime) => (
            format!("{DRIVE_BASE}/{}", encode_path_component(&id)),
            vec![
                ("alt", "media".to_string()),
                ("supportsAllDrives", "true".to_string()),
            ],
            None,
        ),
        None => {
            return Ok(DriveReadResult {
                id,
                name,
                mime_type: mime.clone(),
                exported_as: None,
                content: String::new(),
                truncated: false,
                note: Some(format!(
                    "This file type ({mime}) is not text — DuDuClaw does not download binary \
                     content. Open it via web_view_link, or ask for a Google Docs/Sheets/Slides \
                     file instead."
                )),
            });
        }
    };

    let raw = google_request_text(token, reqwest::Method::GET, &url, &query).await?;
    let (content, truncated) = truncate_text(&raw, DOC_TEXT_MAX_CHARS);
    let note = (exported_as.as_deref() == Some("text/csv"))
        .then(|| "Sheets export covers the FIRST sheet only — use sheets_read for a specific range or tab.".to_string());

    Ok(DriveReadResult {
        id,
        name,
        mime_type: mime,
        exported_as,
        content,
        truncated,
        note,
    })
}

/// Read a Google Doc's text (paragraphs plus table cell text, in document
/// order). Read path of the `documents` scope.
pub async fn docs_read(token: &str, document_id: &str) -> Result<DocsReadResult, GoogleApiError> {
    let id = extract_drive_file_id(document_id);
    let resp = google_request(
        token,
        reqwest::Method::GET,
        &format!("{DOCS_BASE}/{}", encode_path_component(&id)),
        &[],
        None,
    )
    .await?;

    let mut text = String::new();
    collect_doc_text(
        resp.get("body").and_then(|b| b.get("content")).unwrap_or(&Value::Null),
        &mut text,
    );
    let (text, truncated) = truncate_text(&text, DOC_TEXT_MAX_CHARS);

    Ok(DocsReadResult {
        document_id: str_field(&resp, "documentId"),
        title: str_field(&resp, "title"),
        text,
        truncated,
    })
}

/// Append text to the end of a Google Doc's body (write). Deliberately
/// append-only: no tool rewrites or deletes existing document content, so a
/// mistaken call can never destroy the user's text.
pub async fn docs_append(
    token: &str,
    document_id: &str,
    text: &str,
) -> Result<DocsAppendResult, GoogleApiError> {
    let id = extract_drive_file_id(document_id);
    let body = json!({
        "requests": [{
            "insertText": {
                "text": text,
                // Empty EndOfSegmentLocation = end of the document body.
                "endOfSegmentLocation": {}
            }
        }]
    });
    google_request(
        token,
        reqwest::Method::POST,
        &format!("{DOCS_BASE}/{}:batchUpdate", encode_path_component(&id)),
        &[],
        Some(&body),
    )
    .await?;

    Ok(DocsAppendResult {
        document_id: id,
        appended_chars: text.chars().count(),
    })
}

/// Read a Google Slides presentation's text, slide by slide (read-only).
pub async fn slides_read(
    token: &str,
    presentation_id: &str,
) -> Result<SlidesReadResult, GoogleApiError> {
    let id = extract_drive_file_id(presentation_id);
    let resp = google_request(
        token,
        reqwest::Method::GET,
        &format!("{SLIDES_BASE}/{}", encode_path_component(&id)),
        &[],
        None,
    )
    .await?;

    let slides: Vec<SlideText> = resp
        .get("slides")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut text = String::new();
                    collect_slide_text(s.get("pageElements").unwrap_or(&Value::Null), &mut text);
                    SlideText {
                        index: i + 1,
                        object_id: str_field(s, "objectId"),
                        text: truncate_text(&text, DOC_TEXT_MAX_CHARS).0,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SlidesReadResult {
        presentation_id: str_field(&resp, "presentationId"),
        title: str_field(&resp, "title"),
        slide_count: slides.len(),
        slides,
    })
}

// ── Pure helpers (unit-tested) ──────────────────────────────────────────────

fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    n.max(lo).min(hi)
}

/// Truncate to a character budget, reporting whether anything was dropped.
/// Character-based (never byte slicing) so CJK text can't be cut mid-codepoint.
fn truncate_text(s: &str, max_chars: usize) -> (String, bool) {
    if s.chars().count() <= max_chars {
        return (s.to_string(), false);
    }
    (s.chars().take(max_chars).collect(), true)
}

/// Escape a value interpolated into a Drive `q` search string. Drive's query
/// grammar delimits string literals with `'`, escaping `\` and `'` with a
/// backslash — without this a name containing a quote would break out of the
/// literal and change the query's meaning.
fn escape_drive_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            // Control characters have no place in a query literal.
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Export MIME type for a Google-native document, or `None` for blobs.
/// Mapping per Google's export-formats reference: Docs/Slides → `text/plain`,
/// Sheets → `text/csv` (first sheet only).
fn export_mime_for(mime: &str) -> Option<&'static str> {
    match mime {
        "application/vnd.google-apps.document" => Some("text/plain"),
        "application/vnd.google-apps.spreadsheet" => Some("text/csv"),
        "application/vnd.google-apps.presentation" => Some("text/plain"),
        _ => None,
    }
}

/// Whether a blob MIME type is safe to render as text in a tool result.
fn is_text_like_mime(mime: &str) -> bool {
    let base = mime.split(';').next().unwrap_or(mime).trim();
    base.starts_with("text/")
        || matches!(
            base,
            "application/json"
                | "application/xml"
                | "application/x-yaml"
                | "application/yaml"
                | "application/x-ndjson"
                | "application/toml"
        )
}

/// Extract a Drive/Docs/Slides file id from a share URL, or pass a bare id
/// through. Handles `/d/<ID>/edit`, `/d/e/<ID>/…` and `?id=<ID>` shapes.
pub fn extract_drive_file_id(input: &str) -> String {
    let s = input.trim();
    if let Some(after) = s.split("/d/").nth(1) {
        let after = after.strip_prefix("e/").unwrap_or(after);
        let id: String = after
            .chars()
            .take_while(|c| *c != '/' && *c != '#' && *c != '?')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    // Legacy `open?id=<ID>` / `uc?id=<ID>` links.
    if let Some(after) = s.split("id=").nth(1) {
        let id: String = after
            .chars()
            .take_while(|c| *c != '&' && *c != '#')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    s.to_string()
}

/// Walk a Docs `body.content[]` array, appending paragraph text in order.
/// Recurses into table cells so tabular content is not silently dropped.
fn collect_doc_text(content: &Value, out: &mut String) {
    let Some(arr) = content.as_array() else { return };
    for el in arr {
        if let Some(paragraph) = el.get("paragraph") {
            if let Some(elements) = paragraph.get("elements").and_then(|v| v.as_array()) {
                for e in elements {
                    if let Some(t) = e
                        .get("textRun")
                        .and_then(|t| t.get("content"))
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(t);
                    }
                }
            }
        } else if let Some(table) = el.get("table") {
            for row in table
                .get("tableRows")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                for cell in row
                    .get("tableCells")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    collect_doc_text(cell.get("content").unwrap_or(&Value::Null), out);
                }
            }
        }
        // sectionBreak / tableOfContents carry no author text of their own.
    }
}

/// Walk a Slides `pageElements[]` array, appending shape/table text. Recurses
/// into groups (`elementGroup.children`) so grouped shapes are included.
fn collect_slide_text(page_elements: &Value, out: &mut String) {
    let Some(arr) = page_elements.as_array() else { return };
    for el in arr {
        if let Some(text) = el.get("shape").and_then(|s| s.get("text")) {
            append_slide_text_elements(text, out);
        }
        if let Some(table) = el.get("table") {
            for row in table
                .get("tableRows")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                for cell in row
                    .get("tableCells")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = cell.get("text") {
                        append_slide_text_elements(text, out);
                    }
                }
            }
        }
        if let Some(children) = el.get("elementGroup").and_then(|g| g.get("children")) {
            collect_slide_text(children, out);
        }
    }
}

/// Append a Slides `text.textElements[]` run sequence. `paragraphMarker`
/// entries carry no `textRun` and are skipped.
fn append_slide_text_elements(text: &Value, out: &mut String) {
    let Some(elements) = text.get("textElements").and_then(|v| v.as_array()) else {
        return;
    };
    for e in elements {
        if let Some(t) = e
            .get("textRun")
            .and_then(|r| r.get("content"))
            .and_then(|v| v.as_str())
        {
            out.push_str(t);
        }
    }
}

/// Read a string field, defaulting to empty.
fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// Extract a form id from a full Google Forms URL, or pass a bare id through.
/// Handles both the editor (`/forms/d/<ID>/edit`) and the viewer
/// (`/forms/d/e/<LONG_ID>/viewform`) shapes.
pub fn extract_form_id(input: &str) -> String {
    let s = input.trim();
    if let Some(after) = s.split("/d/").nth(1) {
        // The viewer form embeds the published id under an extra `e/` segment.
        let after = after.strip_prefix("e/").unwrap_or(after);
        let id: String = after
            .chars()
            .take_while(|c| *c != '/' && *c != '#' && *c != '?')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    s.to_string()
}

/// Map one `items[]` entry from `forms.get` to a [`FormQuestion`]. Non-question
/// items (page breaks, images, text blocks) return `None`.
fn parse_form_item(item: &Value) -> Option<FormQuestion> {
    let question = item.get("questionItem")?.get("question")?;
    let (kind, options) = if let Some(choice) = question.get("choiceQuestion") {
        let opts = choice
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| {
                        // An "Other" option has no `value`, only `isOther`.
                        o.get("value")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| "(other)".to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        ("choice", opts)
    } else if question.get("textQuestion").is_some() {
        ("text", Vec::new())
    } else if question.get("scaleQuestion").is_some() {
        ("scale", Vec::new())
    } else if question.get("dateQuestion").is_some() {
        ("date", Vec::new())
    } else if question.get("timeQuestion").is_some() {
        ("time", Vec::new())
    } else if question.get("fileUploadQuestion").is_some() {
        ("file_upload", Vec::new())
    } else if question.get("ratingQuestion").is_some() {
        ("rating", Vec::new())
    } else if question.get("rowQuestion").is_some() {
        ("row", Vec::new())
    } else {
        ("other", Vec::new())
    };

    Some(FormQuestion {
        question_id: str_field(question, "questionId"),
        title: str_field(item, "title"),
        kind: kind.to_string(),
        options,
        required: question
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Flatten one `responses[]` entry: `answers` is a map of question_id →
/// `textAnswers.answers[].value`, joined for multi-select questions.
fn parse_form_response(resp: &Value) -> FormResponseEntry {
    let mut answers = std::collections::BTreeMap::new();
    if let Some(map) = resp.get("answers").and_then(|v| v.as_object()) {
        for (qid, ans) in map {
            let joined = ans
                .get("textAnswers")
                .and_then(|t| t.get("answers"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get("value").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            answers.insert(qid.clone(), joined);
        }
    }
    FormResponseEntry {
        response_id: str_field(resp, "responseId"),
        // `lastSubmittedTime` is the edit-aware timestamp; fall back to create.
        submitted_at: {
            let last = str_field(resp, "lastSubmittedTime");
            if last.is_empty() {
                str_field(resp, "createTime")
            } else {
                last
            }
        },
        respondent_email: str_field(resp, "respondentEmail"),
        answers,
    }
}

/// Map one Tasks API task object to a [`TaskEntry`].
fn parse_task(t: &Value) -> TaskEntry {
    TaskEntry {
        id: str_field(t, "id"),
        title: str_field(t, "title"),
        notes: str_field(t, "notes"),
        status: str_field(t, "status"),
        due: str_field(t, "due"),
        completed: str_field(t, "completed"),
        parent: str_field(t, "parent"),
    }
}

/// Extract a spreadsheet id from a full Google Sheets URL, or pass a bare id
/// through unchanged. Handles `.../spreadsheets/d/<ID>/edit#gid=0`.
pub fn extract_spreadsheet_id(input: &str) -> String {
    let s = input.trim();
    if let Some(after) = s.split("/d/").nth(1) {
        // Take up to the next path separator or fragment/query.
        let id: String = after
            .chars()
            .take_while(|c| *c != '/' && *c != '#' && *c != '?')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    s.to_string()
}

/// Percent-encode a single URL path component (sheet ranges contain `!`, `:`,
/// spaces, and single quotes that must not break the path).
fn encode_path_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

/// Render a spreadsheet cell (string/number/bool) as a plain string.
fn cell_to_string(cell: &Value) -> String {
    match cell {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Now .. now+7d as RFC-3339 strings — the default calendar list window.
fn default_time_range() -> (String, String) {
    let now = chrono::Utc::now();
    let later = now + chrono::Duration::days(7);
    (now.to_rfc3339(), later.to_rfc3339())
}

/// Validate an RFC-3339 timestamp (used by the create-event handler).
pub fn is_rfc3339(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

/// RFC 2047 encoded-word for a header value; passes ASCII through unchanged.
fn encode_header_word(s: &str) -> String {
    if s.is_ascii() && !s.chars().any(|c| c.is_control()) {
        s.to_string()
    } else {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        format!("=?UTF-8?B?{b64}?=")
    }
}

/// Build an RFC 2822 message. CRLF line endings; UTF-8 body declared 8bit.
fn build_rfc2822(to: &str, subject: &str, body: &str, cc: Option<&str>) -> String {
    let mut msg = String::new();
    msg.push_str(&format!("To: {to}\r\n"));
    if let Some(cc) = cc {
        if !cc.is_empty() {
            msg.push_str(&format!("Cc: {cc}\r\n"));
        }
    }
    msg.push_str(&format!("Subject: {}\r\n", encode_header_word(subject)));
    msg.push_str("MIME-Version: 1.0\r\n");
    msg.push_str("Content-Type: text/plain; charset=\"UTF-8\"\r\n");
    msg.push_str("Content-Transfer-Encoding: 8bit\r\n");
    msg.push_str("\r\n");
    msg.push_str(body);
    msg
}

/// base64url (no padding) — Gmail's `raw` field encoding.
fn encode_raw_message(raw: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// Decode Gmail's web-safe base64 body data (padded or not).
fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE
        .decode(s)
        .ok()
        .or_else(|| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok())
}

/// Case-insensitive header lookup over a Gmail `headers` array.
fn header_value(headers: &Value, name: &str) -> Option<String> {
    headers.as_array()?.iter().find_map(|h| {
        let hn = h.get("name").and_then(|n| n.as_str())?;
        if hn.eq_ignore_ascii_case(name) {
            h.get("value").and_then(|v| v.as_str()).map(|s| s.to_string())
        } else {
            None
        }
    })
}

/// Best-effort HTML → text: strip tags, decode a few common entities.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Walk a MIME tree, preferring text/plain, falling back to html-stripped text.
fn extract_message_body(payload: &Value) -> String {
    if let Some(t) = find_part_text(payload, "text/plain") {
        return t;
    }
    if let Some(h) = find_part_text(payload, "text/html") {
        return html_to_text(&h);
    }
    String::new()
}

fn find_part_text(node: &Value, want_mime: &str) -> Option<String> {
    let mime = node.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
    if mime == want_mime {
        if let Some(data) = node
            .get("body")
            .and_then(|b| b.get("data"))
            .and_then(|d| d.as_str())
        {
            if let Some(bytes) = decode_b64url(data) {
                return Some(String::from_utf8_lossy(&bytes).to_string());
            }
        }
    }
    if let Some(parts) = node.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            if let Some(t) = find_part_text(p, want_mime) {
                return Some(t);
            }
        }
    }
    None
}

fn collect_attachments(payload: &Value) -> Vec<GmailAttachment> {
    let mut out = Vec::new();
    collect_attachments_rec(payload, &mut out);
    out
}

fn collect_attachments_rec(node: &Value, out: &mut Vec<GmailAttachment>) {
    let filename = node.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    if !filename.is_empty() {
        let size = node
            .get("body")
            .and_then(|b| b.get("size"))
            .and_then(|s| s.as_u64())
            .unwrap_or(0);
        out.push(GmailAttachment {
            filename: filename.to_string(),
            size,
        });
    }
    if let Some(parts) = node.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            collect_attachments_rec(p, out);
        }
    }
}

/// A calendar event start/end can be `dateTime` (timed) or `date` (all-day).
fn event_time(node: &Value, key: &str) -> String {
    node.get(key)
        .and_then(|s| s.get("dateTime").or_else(|| s.get("date")))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_meet_link(event: &Value) -> Option<String> {
    if let Some(link) = event.get("hangoutLink").and_then(|v| v.as_str()) {
        if !link.is_empty() {
            return Some(link.to_string());
        }
    }
    event
        .get("conferenceData")
        .and_then(|c| c.get("entryPoints"))
        .and_then(|e| e.as_array())
        .and_then(|eps| {
            eps.iter().find_map(|ep| {
                let kind = ep.get("entryPointType").and_then(|v| v.as_str()).unwrap_or("");
                if kind == "video" {
                    ep.get("uri").and_then(|v| v.as_str()).map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
}

fn parse_calendar_event(item: &Value) -> CalendarEvent {
    CalendarEvent {
        id: item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        summary: item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no title)")
            .to_string(),
        start: event_time(item, "start"),
        end: event_time(item, "end"),
        location: item
            .get("location")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        html_link: item
            .get("htmlLink")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        meet_link: extract_meet_link(item),
        attendees_count: item
            .get("attendees")
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
    }
}

/// Pull a human-readable message out of a Google error JSON body.
fn extract_api_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| body.to_string())
}

/// Build the 403 re-auth guidance, echoing Google's reason plus the scopes we
/// need the user to re-grant.
fn scope_guidance(api_error_body: &str) -> String {
    let reason = duduclaw_core::truncate_chars(&extract_api_message(api_error_body), 200);
    format!(
        "Google denied the request (403): {reason}. The authorization is missing required scopes. \
         Reconnect Google from the dashboard Integrations → Google page to grant: {}.",
        REQUIRED_SCOPES.join(", ")
    )
}

// ── Backend-aware wrappers ──────────────────────────────────────────────────
//
// One entry point per tool that both credential paths can serve. Handlers call
// these instead of the raw `token`-taking functions, so adding the Apps Script
// bridge did not mean teaching nineteen call sites about backends.
//
// The bridge speaks its own JSON shape (it is a hand-written Apps Script, not
// the Google REST API), so each wrapper maps that shape onto the SAME result
// struct the direct path returns. The agent — and the tool schema — cannot tell
// which credential answered, which is the point: one tool surface, three ways
// to authorize it.

/// Extract a string field from a bridge response, defaulting to empty.
fn bs(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

/// Extract a u64 field from a bridge response, defaulting to 0.
fn bu(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// Map a bridge failure onto the shared API-error type.
fn bridge_err(e: google_apps_script::BridgeError) -> GoogleApiError {
    GoogleApiError::Api { status: 502, message: e.to_string() }
}

/// Search mail through whichever backend is configured.
pub async fn gmail_search_via(
    backend: &GoogleBackend,
    query: &str,
    max_results: u32,
) -> Result<GmailSearchResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => gmail_search(token, query, max_results).await,
        GoogleBackend::AppsScript(cfg) => {
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::GmailSearch,
                json!({ "query": query, "limit": max_results }),
            )
            .await
            .map_err(bridge_err)?;
            let messages: Vec<GmailMessageMeta> = v
                .get("messages")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|m| GmailMessageMeta {
                            id: bs(m, "message_id"),
                            // Apps Script's GmailApp exposes threads, but the
                            // bridge returns the first message of each; there is
                            // no separate thread id to report.
                            thread_id: String::new(),
                            from: bs(m, "from"),
                            subject: bs(m, "subject"),
                            date: bs(m, "date"),
                            snippet: bs(m, "snippet"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(GmailSearchResult { count: messages.len(), messages })
        }
    }
}

/// Read one message through whichever backend is configured.
pub async fn gmail_read_via(
    backend: &GoogleBackend,
    message_id: &str,
) -> Result<GmailReadResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => gmail_read(token, message_id).await,
        GoogleBackend::AppsScript(cfg) => {
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::GmailRead,
                json!({ "message_id": message_id }),
            )
            .await
            .map_err(bridge_err)?;
            let attachments = v
                .get("attachments")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|a| GmailAttachment { filename: bs(a, "name"), size: bu(a, "size") })
                        .collect()
                })
                .unwrap_or_default();
            let body = bs(&v, "body");
            Ok(GmailReadResult {
                id: bs(&v, "message_id"),
                thread_id: String::new(),
                from: bs(&v, "from"),
                to: bs(&v, "to"),
                subject: bs(&v, "subject"),
                date: bs(&v, "date"),
                // The bridge has no separate snippet concept; the first line of
                // the body is the honest equivalent rather than a fabricated one.
                snippet: duduclaw_core::truncate_chars(body.lines().next().unwrap_or(""), 200),
                body,
                body_truncated: v.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false),
                attachments,
            })
        }
    }
}

/// Create a draft through whichever backend is configured.
pub async fn gmail_create_draft_via(
    backend: &GoogleBackend,
    to: &str,
    subject: &str,
    body_text: &str,
    cc: Option<&str>,
) -> Result<GmailDraftResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => {
            gmail_create_draft(token, to, subject, body_text, cc).await
        }
        GoogleBackend::AppsScript(cfg) => {
            // The shipped bridge script takes no cc field. Rather than silently
            // dropping a recipient the user asked for, refuse.
            if cc.is_some_and(|c| !c.trim().is_empty()) {
                return Err(bridge_err(google_apps_script::BridgeError::Unsupported(
                    "gmail_create_draft with cc".into(),
                )));
            }
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::GmailCreateDraft,
                json!({ "to": to, "subject": subject, "body": body_text }),
            )
            .await
            .map_err(bridge_err)?;
            Ok(GmailDraftResult {
                draft_id: bs(&v, "draft_id"),
                message_id: bs(&v, "message_id"),
                to: to.to_string(),
                subject: subject.to_string(),
            })
        }
    }
}

/// List calendar events through whichever backend is configured.
pub async fn calendar_list_events_via(
    backend: &GoogleBackend,
    time_min: Option<&str>,
    time_max: Option<&str>,
    max_results: u32,
) -> Result<CalendarEventsResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => {
            calendar_list_events(token, time_min, time_max, max_results).await
        }
        GoogleBackend::AppsScript(cfg) => {
            // The bridge takes a forward-looking day count, not a range. A
            // caller-supplied window is honoured as "days from now"; anything
            // else falls back to the same 7-day default the direct path uses.
            let days = days_until(time_max).unwrap_or(7);
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::CalendarListEvents,
                json!({ "days": days }),
            )
            .await
            .map_err(bridge_err)?;
            let events: Vec<CalendarEvent> = v
                .get("events")
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .take(max_results.max(1) as usize)
                        .map(|e| CalendarEvent {
                            id: bs(e, "id"),
                            summary: bs(e, "title"),
                            start: bs(e, "start"),
                            end: bs(e, "end"),
                            location: Some(bs(e, "location")).filter(|s| !s.is_empty()),
                            html_link: String::new(),
                            meet_link: None,
                            attendees_count: 0,
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(CalendarEventsResult { count: events.len(), events })
        }
    }
}

/// Create a calendar event through whichever backend is configured.
pub async fn calendar_create_event_via(
    backend: &GoogleBackend,
    summary: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    description: Option<&str>,
    attendees: Option<&[String]>,
    with_meet: bool,
) -> Result<CalendarCreateResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => {
            calendar_create_event(
                token,
                summary,
                start_rfc3339,
                end_rfc3339,
                description,
                attendees,
                with_meet,
            )
            .await
        }
        GoogleBackend::AppsScript(cfg) => {
            // Attendees and Meet links are outside the shipped script's action
            // surface. Creating the event minus the guests the user listed would
            // look like success while quietly failing the actual request.
            if attendees.is_some_and(|a| !a.is_empty()) {
                return Err(bridge_err(google_apps_script::BridgeError::Unsupported(
                    "calendar_create_event with attendees".into(),
                )));
            }
            if with_meet {
                return Err(bridge_err(google_apps_script::BridgeError::Unsupported(
                    "calendar_create_event with a Meet link".into(),
                )));
            }
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::CalendarCreateEvent,
                json!({
                    "title": summary,
                    "start": start_rfc3339,
                    "end": end_rfc3339,
                    "description": description.unwrap_or(""),
                }),
            )
            .await
            .map_err(bridge_err)?;
            Ok(CalendarCreateResult {
                id: bs(&v, "id"),
                summary: bs(&v, "title"),
                start: bs(&v, "start"),
                end: end_rfc3339.to_string(),
                html_link: String::new(),
                meet_link: None,
            })
        }
    }
}

/// Read a sheet range through whichever backend is configured.
pub async fn sheets_read_via(
    backend: &GoogleBackend,
    spreadsheet_id: &str,
    range: &str,
) -> Result<SheetsReadResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => sheets_read(token, spreadsheet_id, range).await,
        GoogleBackend::AppsScript(cfg) => {
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::SheetsRead,
                json!({ "spreadsheet": spreadsheet_id, "range": range }),
            )
            .await
            .map_err(bridge_err)?;
            let rows: Vec<Vec<String>> = v
                .get("values")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|row| {
                            row.as_array()
                                .map(|cells| cells.iter().map(cell_to_string).collect())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(SheetsReadResult {
                range: range.to_string(),
                row_count: rows.len(),
                // The bridge caps at 200 rows server-side; a full page back is
                // the only signal available that more may exist.
                rows_truncated: rows.len() >= 200,
                rows,
            })
        }
    }
}

/// Append a sheet row through whichever backend is configured.
pub async fn sheets_append_via(
    backend: &GoogleBackend,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<String>,
) -> Result<SheetsAppendResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => {
            sheets_append(token, spreadsheet_id, range, values).await
        }
        GoogleBackend::AppsScript(cfg) => {
            let cells = values.len() as u64;
            let v = google_apps_script::call(
                cfg,
                google_apps_script::BridgeAction::SheetsAppend,
                json!({ "spreadsheet": spreadsheet_id, "values": values }),
            )
            .await
            .map_err(bridge_err)?;
            let row = bu(&v, "row");
            Ok(SheetsAppendResult {
                updated_range: if row > 0 { format!("row {row}") } else { range.to_string() },
                updated_rows: 1,
                updated_cells: cells,
            })
        }
    }
}

/// Days from now until an RFC3339 timestamp, for translating the direct path's
/// time range onto the bridge's day-count parameter. `None` when absent or
/// unparseable — the caller then uses its own default rather than guessing.
fn days_until(time_max: Option<&str>) -> Option<u32> {
    let ts = time_max?;
    let target = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let now = chrono::Utc::now();
    let delta = target.with_timezone(&chrono::Utc) - now;
    let days = delta.num_days();
    if days <= 0 { Some(1) } else { Some(days.min(365) as u32) }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bounds() {
        assert_eq!(clamp(0, 1, 25), 1);
        assert_eq!(clamp(100, 1, 25), 25);
        assert_eq!(clamp(10, 1, 25), 10);
        assert_eq!(clamp(60, 1, 50), 50);
    }

    #[test]
    fn ascii_subject_passes_through() {
        assert_eq!(encode_header_word("Hello World"), "Hello World");
    }

    #[test]
    fn cjk_subject_is_rfc2047_encoded() {
        let enc = encode_header_word("測試主旨");
        assert!(enc.starts_with("=?UTF-8?B?"), "got {enc}");
        assert!(enc.ends_with("?="));
        // Round-trip the base64 payload back to the original bytes.
        use base64::Engine;
        let inner = enc
            .trim_start_matches("=?UTF-8?B?")
            .trim_end_matches("?=");
        let decoded = base64::engine::general_purpose::STANDARD.decode(inner).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "測試主旨");
    }

    #[test]
    fn rfc2822_has_headers_and_cc() {
        let msg = build_rfc2822("a@b.com", "Hi", "body line", Some("c@d.com"));
        assert!(msg.contains("To: a@b.com\r\n"));
        assert!(msg.contains("Cc: c@d.com\r\n"));
        assert!(msg.contains("Subject: Hi\r\n"));
        assert!(msg.contains("\r\n\r\nbody line"));
    }

    #[test]
    fn rfc2822_omits_empty_cc() {
        let msg = build_rfc2822("a@b.com", "Hi", "body", None);
        assert!(!msg.contains("Cc:"));
        let msg2 = build_rfc2822("a@b.com", "Hi", "body", Some(""));
        assert!(!msg2.contains("Cc:"));
    }

    #[test]
    fn raw_message_is_base64url_no_pad() {
        let enc = encode_raw_message("To: a@b.com\r\n\r\nbody");
        // URL-safe alphabet: no '+' '/' '=' characters.
        assert!(!enc.contains('+') && !enc.contains('/') && !enc.contains('='));
    }

    #[test]
    fn b64url_decodes_padded_and_unpadded() {
        use base64::Engine;
        let padded = base64::engine::general_purpose::URL_SAFE.encode(b"hello world");
        let unpadded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"hello world");
        assert_eq!(decode_b64url(&padded).unwrap(), b"hello world");
        assert_eq!(decode_b64url(&unpadded).unwrap(), b"hello world");
    }

    #[test]
    fn default_range_is_seven_days() {
        let (min, max) = default_time_range();
        let a = chrono::DateTime::parse_from_rfc3339(&min).unwrap();
        let b = chrono::DateTime::parse_from_rfc3339(&max).unwrap();
        let days = (b - a).num_days();
        assert_eq!(days, 7);
    }

    #[test]
    fn rfc3339_validation() {
        assert!(is_rfc3339("2026-07-26T10:00:00Z"));
        assert!(is_rfc3339("2026-07-26T10:00:00+08:00"));
        assert!(!is_rfc3339("2026-07-26 10:00"));
        assert!(!is_rfc3339("not a date"));
    }

    #[test]
    fn html_stripping_and_entities() {
        let t = html_to_text("<p>Hello&nbsp;<b>world</b> &amp; more &lt;x&gt;</p>");
        assert_eq!(t, "Hello world & more <x>");
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let headers = json!([
            {"name": "From", "value": "alice@example.com"},
            {"name": "subject", "value": "Hi there"},
        ]);
        assert_eq!(header_value(&headers, "from").as_deref(), Some("alice@example.com"));
        assert_eq!(header_value(&headers, "Subject").as_deref(), Some("Hi there"));
        assert_eq!(header_value(&headers, "Cc"), None);
    }

    #[test]
    fn body_extraction_prefers_plain_then_html() {
        use base64::Engine;
        let plain = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"plain body");
        let html = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"<p>html body</p>");
        let payload = json!({
            "mimeType": "multipart/alternative",
            "parts": [
                {"mimeType": "text/plain", "body": {"data": plain}},
                {"mimeType": "text/html", "body": {"data": html}},
            ]
        });
        assert_eq!(extract_message_body(&payload), "plain body");

        let html_only = json!({
            "mimeType": "text/html",
            "body": {"data": html}
        });
        assert_eq!(extract_message_body(&html_only), "html body");
    }

    #[test]
    fn attachment_manifest_lists_filename_and_size() {
        let payload = json!({
            "mimeType": "multipart/mixed",
            "parts": [
                {"mimeType": "text/plain", "body": {"data": ""}, "filename": ""},
                {"mimeType": "application/pdf", "filename": "report.pdf", "body": {"size": 12345}},
            ]
        });
        let atts = collect_attachments(&payload);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].filename, "report.pdf");
        assert_eq!(atts[0].size, 12345);
    }

    #[test]
    fn meet_link_from_hangout_and_entrypoints() {
        let with_hangout = json!({ "hangoutLink": "https://meet.google.com/abc-defg-hij" });
        assert_eq!(
            extract_meet_link(&with_hangout).as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );

        let with_entrypoints = json!({
            "conferenceData": {
                "entryPoints": [
                    {"entryPointType": "phone", "uri": "tel:+123"},
                    {"entryPointType": "video", "uri": "https://meet.google.com/xyz"}
                ]
            }
        });
        assert_eq!(
            extract_meet_link(&with_entrypoints).as_deref(),
            Some("https://meet.google.com/xyz")
        );

        let none = json!({ "summary": "no meet" });
        assert_eq!(extract_meet_link(&none), None);
    }

    #[test]
    fn calendar_event_parsing_all_day_and_timed() {
        let timed = json!({
            "id": "evt1",
            "summary": "Standup",
            "start": {"dateTime": "2026-07-26T09:00:00Z"},
            "end": {"dateTime": "2026-07-26T09:15:00Z"},
            "htmlLink": "https://calendar.google.com/evt1",
            "attendees": [{"email": "a@b.com"}, {"email": "c@d.com"}]
        });
        let e = parse_calendar_event(&timed);
        assert_eq!(e.start, "2026-07-26T09:00:00Z");
        assert_eq!(e.attendees_count, 2);

        let all_day = json!({
            "id": "evt2",
            "start": {"date": "2026-07-27"},
            "end": {"date": "2026-07-28"}
        });
        let e2 = parse_calendar_event(&all_day);
        assert_eq!(e2.start, "2026-07-27");
        assert_eq!(e2.summary, "(no title)");
        assert_eq!(e2.attendees_count, 0);
    }

    #[test]
    fn scope_guidance_lists_required_scopes() {
        let body = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes."}}"#;
        let g = scope_guidance(body);
        assert!(g.contains("gmail.compose"));
        assert!(g.contains("calendar.events"));
        assert!(g.contains("insufficient authentication scopes"));
    }

    #[test]
    fn api_message_extraction() {
        let body = r#"{"error":{"code":400,"message":"Invalid start time"}}"#;
        assert_eq!(extract_api_message(body), "Invalid start time");
        // Non-JSON falls back to the raw body.
        assert_eq!(extract_api_message("plain error"), "plain error");
    }

    #[test]
    fn spreadsheet_id_from_url_and_bare() {
        assert_eq!(
            extract_spreadsheet_id(
                "https://docs.google.com/spreadsheets/d/1AbC_dEf-123/edit#gid=0"
            ),
            "1AbC_dEf-123"
        );
        assert_eq!(
            extract_spreadsheet_id("https://docs.google.com/spreadsheets/d/XYZ/edit?usp=sharing"),
            "XYZ"
        );
        // Bare id passes through.
        assert_eq!(extract_spreadsheet_id("1AbC_dEf-123"), "1AbC_dEf-123");
        // Trailing whitespace trimmed.
        assert_eq!(extract_spreadsheet_id("  bareId  "), "bareId");
    }

    #[test]
    fn path_component_encoding_for_ranges() {
        // Sheet-qualified A1 range: `!` and space must be encoded, `:` too.
        assert_eq!(encode_path_component("Sheet1!A1:C10"), "Sheet1%21A1%3AC10");
        assert_eq!(encode_path_component("My Sheet!A1"), "My%20Sheet%21A1");
        assert_eq!(encode_path_component("A1:B2"), "A1%3AB2");
    }

    #[test]
    fn cell_rendering_covers_types() {
        assert_eq!(cell_to_string(&json!("hi")), "hi");
        assert_eq!(cell_to_string(&json!(42)), "42");
        assert_eq!(cell_to_string(&json!(true)), "true");
        assert_eq!(cell_to_string(&Value::Null), "");
    }

    #[test]
    fn required_scopes_include_sheets() {
        assert!(REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/spreadsheets"));
    }

    #[test]
    fn required_scopes_cover_all_eight_services() {
        // All eight Workspace services are served by NATIVE tools off this one
        // connection (no Developer-Preview MCP mounts required), so every
        // service must have its scope granted here. Scope strings verified
        // against each API's REST reference.
        for s in [
            // Gmail, Calendar, Sheets
            "https://www.googleapis.com/auth/gmail.readonly",
            "https://www.googleapis.com/auth/calendar.events",
            "https://www.googleapis.com/auth/spreadsheets",
            // Drive, Docs, Slides
            "https://www.googleapis.com/auth/drive.readonly",
            "https://www.googleapis.com/auth/documents",
            "https://www.googleapis.com/auth/presentations.readonly",
            // Forms, Tasks
            "https://www.googleapis.com/auth/forms.body.readonly",
            "https://www.googleapis.com/auth/forms.responses.readonly",
            "https://www.googleapis.com/auth/tasks",
        ] {
            assert!(REQUIRED_SCOPES.contains(&s), "missing scope: {s}");
        }
    }

    // ── Forms helpers ──

    #[test]
    fn extract_form_id_handles_urls_and_bare_ids() {
        assert_eq!(
            extract_form_id("https://docs.google.com/forms/d/1AbC_dEf/edit#responses"),
            "1AbC_dEf"
        );
        // The viewer URL nests the published id under an extra `e/` segment.
        assert_eq!(
            extract_form_id("https://docs.google.com/forms/d/e/1FAIpQLSxyz/viewform?usp=sf_link"),
            "1FAIpQLSxyz"
        );
        assert_eq!(extract_form_id("  1BareId  "), "1BareId");
    }

    #[test]
    fn parse_form_item_classifies_question_kinds() {
        let choice = json!({
            "itemId": "i1", "title": "方案",
            "questionItem": { "question": {
                "questionId": "q1", "required": true,
                "choiceQuestion": { "type": "RADIO", "options": [
                    {"value": "A"}, {"value": "B"}, {"isOther": true}
                ]}
            }}
        });
        let q = parse_form_item(&choice).expect("choice question");
        assert_eq!(q.question_id, "q1");
        assert_eq!(q.title, "方案");
        assert_eq!(q.kind, "choice");
        assert_eq!(q.options, vec!["A", "B", "(other)"]);
        assert!(q.required);

        let text = json!({
            "itemId": "i2", "title": "備註",
            "questionItem": { "question": { "questionId": "q2", "textQuestion": {"paragraph": true} } }
        });
        assert_eq!(parse_form_item(&text).unwrap().kind, "text");

        let scale = json!({
            "questionItem": { "question": { "questionId": "q3", "scaleQuestion": {"low": 1, "high": 5} } }
        });
        assert_eq!(parse_form_item(&scale).unwrap().kind, "scale");

        // Non-question items (page break, image, text block) are skipped.
        assert!(parse_form_item(&json!({"itemId": "i9", "pageBreakItem": {}})).is_none());
    }

    #[test]
    fn parse_form_response_flattens_answers() {
        let r = json!({
            "responseId": "r1",
            "createTime": "2026-07-01T00:00:00Z",
            "lastSubmittedTime": "2026-07-02T00:00:00Z",
            "respondentEmail": "a@b.c",
            "answers": {
                "q1": { "textAnswers": { "answers": [{"value": "A"}, {"value": "B"}] } },
                "q2": { "textAnswers": { "answers": [{"value": "只有一個"}] } }
            }
        });
        let e = parse_form_response(&r);
        assert_eq!(e.response_id, "r1");
        // lastSubmittedTime wins over createTime (edit-aware).
        assert_eq!(e.submitted_at, "2026-07-02T00:00:00Z");
        assert_eq!(e.answers.get("q1").unwrap(), "A, B");
        assert_eq!(e.answers.get("q2").unwrap(), "只有一個");

        // Missing lastSubmittedTime falls back to createTime.
        let r2 = json!({"responseId": "r2", "createTime": "2026-07-01T00:00:00Z"});
        assert_eq!(parse_form_response(&r2).submitted_at, "2026-07-01T00:00:00Z");
    }

    // ── Tasks helpers ──

    #[test]
    fn parse_task_maps_api_fields() {
        let t = json!({
            "id": "t1", "title": "回覆客戶", "notes": "先看報價",
            "status": "needsAction", "due": "2026-08-01T00:00:00.000Z",
            "parent": "t0"
        });
        let e = parse_task(&t);
        assert_eq!(e.id, "t1");
        assert_eq!(e.title, "回覆客戶");
        assert_eq!(e.status, "needsAction");
        assert_eq!(e.due, "2026-08-01T00:00:00.000Z");
        assert_eq!(e.parent, "t0");
        // Absent fields render as empty strings, never null/panic.
        assert_eq!(e.completed, "");
    }

    #[test]
    fn tasks_max_items_clamped() {
        assert_eq!(clamp(999, 1, TASKS_MAX_ITEMS), TASKS_MAX_ITEMS);
        assert_eq!(clamp(0, 1, TASKS_MAX_ITEMS), 1);
    }

    // ── Drive / Docs / Slides helpers ──

    #[test]
    fn drive_query_escaping_prevents_literal_breakout() {
        // A quote in the search term must stay inside the string literal —
        // otherwise the user's text could alter the query's meaning.
        assert_eq!(escape_drive_query_value("O'Brien"), "O\\'Brien");
        assert_eq!(escape_drive_query_value(r"back\slash"), r"back\\slash");
        // Control characters are neutralized.
        assert_eq!(escape_drive_query_value("a\nb"), "a b");
        // CJK passes through untouched.
        assert_eq!(escape_drive_query_value("報價單"), "報價單");
    }

    #[test]
    fn export_mime_mapping_matches_google_reference() {
        assert_eq!(
            export_mime_for("application/vnd.google-apps.document"),
            Some("text/plain")
        );
        assert_eq!(
            export_mime_for("application/vnd.google-apps.spreadsheet"),
            Some("text/csv")
        );
        assert_eq!(
            export_mime_for("application/vnd.google-apps.presentation"),
            Some("text/plain")
        );
        // Blobs are not exportable — they take the download path.
        assert_eq!(export_mime_for("application/pdf"), None);
        assert_eq!(export_mime_for("text/plain"), None);
    }

    #[test]
    fn text_like_mime_detection() {
        assert!(is_text_like_mime("text/plain"));
        assert!(is_text_like_mime("text/csv; charset=utf-8"));
        assert!(is_text_like_mime("application/json"));
        assert!(!is_text_like_mime("application/pdf"));
        assert!(!is_text_like_mime("image/png"));
        assert!(!is_text_like_mime("application/vnd.google-apps.document"));
    }

    #[test]
    fn extract_drive_file_id_handles_all_link_shapes() {
        assert_eq!(
            extract_drive_file_id("https://docs.google.com/document/d/1DocId/edit#heading=h.x"),
            "1DocId"
        );
        assert_eq!(
            extract_drive_file_id("https://docs.google.com/presentation/d/1DeckId/edit?usp=sharing"),
            "1DeckId"
        );
        assert_eq!(
            extract_drive_file_id("https://drive.google.com/file/d/1FileId/view"),
            "1FileId"
        );
        // Legacy id= query links.
        assert_eq!(
            extract_drive_file_id("https://drive.google.com/open?id=1LegacyId&authuser=0"),
            "1LegacyId"
        );
        assert_eq!(extract_drive_file_id("  1BareId "), "1BareId");
    }

    #[test]
    fn doc_text_extraction_includes_tables_in_order() {
        let content = json!([
            { "paragraph": { "elements": [
                { "textRun": { "content": "第一段\n" } },
                { "textRun": { "content": "續行\n" } }
            ]}},
            { "table": { "tableRows": [
                { "tableCells": [
                    { "content": [ { "paragraph": { "elements": [ { "textRun": { "content": "格A\n" } } ] } } ] },
                    { "content": [ { "paragraph": { "elements": [ { "textRun": { "content": "格B\n" } } ] } } ] }
                ]}
            ]}},
            { "sectionBreak": {} },
            { "paragraph": { "elements": [ { "textRun": { "content": "結尾\n" } } ] } }
        ]);
        let mut out = String::new();
        collect_doc_text(&content, &mut out);
        assert_eq!(out, "第一段\n續行\n格A\n格B\n結尾\n");
    }

    #[test]
    fn slide_text_extraction_covers_shapes_groups_and_tables() {
        let page_elements = json!([
            { "shape": { "text": { "textElements": [
                { "paragraphMarker": {} },
                { "textRun": { "content": "標題\n" } }
            ]}}},
            { "elementGroup": { "children": [
                { "shape": { "text": { "textElements": [ { "textRun": { "content": "群組內文\n" } } ] } } }
            ]}},
            { "table": { "tableRows": [
                { "tableCells": [
                    { "text": { "textElements": [ { "textRun": { "content": "表格\n" } } ] } }
                ]}
            ]}},
            // An image has no text and must not break extraction.
            { "image": { "contentUrl": "https://example.com/x.png" } }
        ]);
        let mut out = String::new();
        collect_slide_text(&page_elements, &mut out);
        assert_eq!(out, "標題\n群組內文\n表格\n");
    }

    #[test]
    fn truncate_text_is_cjk_safe_and_flags_truncation() {
        let (s, t) = truncate_text("短", 10);
        assert_eq!(s, "短");
        assert!(!t);
        let long = "字".repeat(30);
        let (s, t) = truncate_text(&long, 10);
        assert_eq!(s.chars().count(), 10);
        assert!(t);
        // Boundary: exactly the budget is not truncation.
        let (_, t) = truncate_text(&"a".repeat(10), 10);
        assert!(!t);
    }

    #[test]
    fn required_scopes_least_privilege_for_drive_and_slides() {
        // Drive stays read-only (no tool creates Drive files) and Slides
        // read-only (no Slides write tool); Docs needs full `documents`
        // because docs_append writes.
        assert!(REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/drive.readonly"));
        assert!(!REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/drive.file"));
        assert!(!REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/drive"));
        assert!(REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/presentations.readonly"));
        assert!(!REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/presentations"));
        assert!(REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/documents"));
    }

    #[test]
    fn integration_gate_defaults_closed_and_opens_only_on_explicit_true() {
        let dir = tempfile::tempdir().unwrap();
        // No config.toml at all → closed.
        assert!(!integration_enabled(dir.path()));
        // Config without the section → closed.
        std::fs::write(dir.path().join("config.toml"), "[general]\nlog_level = \"info\"\n").unwrap();
        assert!(!integration_enabled(dir.path()));
        // Explicit false → closed.
        std::fs::write(dir.path().join("config.toml"), "[integrations]\ngoogle_workspace = false\n").unwrap();
        assert!(!integration_enabled(dir.path()));
        // Malformed toml → closed (fail closed).
        std::fs::write(dir.path().join("config.toml"), "[integrations\n???").unwrap();
        assert!(!integration_enabled(dir.path()));
        // Explicit true → open.
        std::fs::write(dir.path().join("config.toml"), "[integrations]\ngoogle_workspace = true\n").unwrap();
        assert!(integration_enabled(dir.path()));
    }
}
