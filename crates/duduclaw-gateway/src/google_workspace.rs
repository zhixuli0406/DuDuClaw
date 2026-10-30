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
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

use crate::google_apps_script;
pub(crate) mod native_call;
pub(crate) mod native_legacy;

/// Provider id in the `mcp_oauth` vault.
pub const GOOGLE_PROVIDER: &str = "google";

/// Feature gate for the Google Workspace tools. Off by default; operators opt in by connecting their own client or via
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
/// file so operator comments and formatting survive (the dashboard writers do
/// the same through `config_commit::render_preserving`). Missing config.toml is created; malformed TOML is an error
/// rather than a silent overwrite. Returns Ok(true) when the file changed,
/// Ok(false) when the flag was already on.
pub fn enable_integration(home_dir: &Path) -> std::io::Result<bool> {
    set_integration_enabled(home_dir, true)
}

/// Set `config.toml [integrations] google_workspace` to `enabled`.
///
/// G6 (2026-09 feature audit): the dashboard needs both directions — the
/// Google page now renders an explicit enable/disable switch instead of
/// leaving the master gate to a hand-edited config file, which was how a
/// fully-configured credential could still 403 every tool call.
/// `Ok(true)` when the file changed, `Ok(false)` when it already said that.
pub fn set_integration_enabled(home_dir: &Path, enabled: bool) -> std::io::Result<bool> {
    let path = home_dir.join("config.toml");
    let raw =
        crate::handlers::config_commit::read_text_or_empty(&path).map_err(std::io::Error::other)?;
    let mut changed = false;
    crate::handlers::config_commit::commit_rendered_locked(
        &path,
        &crate::handlers::config_commit::content_hash(&raw),
        |current| {
            let mut doc: toml_edit::DocumentMut = current
                .parse()
                .map_err(|_| "config.toml is invalid; refusing rewrite".to_string())?;
            let table: toml::Table = current
                .parse()
                .map_err(|_| "config.toml is invalid; refusing rewrite".to_string())?;
            let previous = match table.get("integrations") {
                None => false,
                Some(v) => match v
                    .as_table()
                    .ok_or("integrations is not a table")?
                    .get("google_workspace")
                {
                    None => false,
                    Some(v) => v.as_bool().ok_or("google_workspace is not a boolean")?,
                },
            };
            if previous == enabled {
                return Ok(current.to_owned());
            }
            doc.as_table_mut()
                .entry("integrations")
                .or_insert(toml_edit::table())
                .as_table_mut()
                .ok_or("integrations is not a table")?
                .insert("google_workspace", toml_edit::value(enabled));
            changed = true;
            Ok(doc.to_string())
        },
    )
    .map_err(std::io::Error::other)?;
    Ok(changed)
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
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
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
/// `Direct` accepts the customer's OAuth client, including External Testing
/// for personal Gmail. Service-account delegation needs a Workspace domain
/// and a super admin. [`AppsScript`](GoogleBackend::AppsScript) covers eight
/// actions without a Cloud client. Both `Direct` variants use a bearer token
/// against the REST APIs.
pub enum GoogleBackend {
    /// A bearer access token — from the OAuth vault or a service-account
    /// assertion. Each service still needs its API and scopes enabled.
    Direct(String),
    /// The user-deployed Apps Script web app. Gmail / Calendar / Sheets only.
    AppsScript(google_apps_script::BridgeConfig),
}

/// Resolve the credential path for this home.
///
/// Saved selection wins over retained OAuth tokens. Invalid configuration
/// never falls back to a different identity.
pub async fn resolve_backend(home_dir: &Path) -> Result<GoogleBackend, GoogleAuthError> {
    match selected_path(home_dir)?.as_str() {
        "apps_script" => google_apps_script::config_for_home(home_dir).await
            .map_err(|e| GoogleAuthError::AppsScript(e.to_string()))?
            .map(GoogleBackend::AppsScript)
            .ok_or_else(|| GoogleAuthError::AppsScript("bridge_not_configured".into())),
        "none" => Err(GoogleAuthError::NotConnected { vault: home_dir.join(crate::mcp_oauth::TOKEN_FILE) }),
        _ => get_valid_google_token(home_dir).await.map(GoogleBackend::Direct),
    }
}

/// Metadata only: resolving the effective path must not mint a token.
pub(crate) fn selected_path(home: &Path) -> Result<String, GoogleAuthError> {
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(GoogleAuthError::RefreshFailed("config_unreadable".into())),
    };
    let table: toml::Table = raw.parse().map_err(|_| GoogleAuthError::RefreshFailed("config_invalid".into()))?;
    let i = match table.get("integrations") {
        Some(value) => Some(value.as_table().ok_or_else(|| GoogleAuthError::RefreshFailed("config_invalid".into()))?),
        None => None,
    };
    if let Some(value) = i.and_then(|t| t.get("google_credential_path")) {
        return match value.as_str() {
            Some(mode @ ("oauth" | "service_account" | "apps_script" | "none")) => {
                let sa = i.is_some_and(|t| t.contains_key("google_service_account"));
                let bridge = i.is_some_and(|t| t.contains_key("google_apps_script"));
                if (mode == "service_account" && (!sa || bridge))
                    || (mode == "apps_script" && (!bridge || sa))
                    || (matches!(mode, "oauth" | "none") && (sa || bridge)) {
                    return Err(GoogleAuthError::RefreshFailed("credential_path_conflict".into()));
                }
                Ok(mode.into())
            },
            _ => Err(GoogleAuthError::RefreshFailed("credential_path_invalid".into())),
        };
    }
    if i.is_some_and(|t| t.contains_key("google_service_account")) { return Ok("service_account".into()); }
    if i.is_some_and(|t| t.contains_key("google_apps_script")) { return Ok("apps_script".into()); }
    Ok(if !crate::mcp_oauth::google_token_stamp(home).is_empty() { "oauth" } else { "none" }.into())
}

/// Curated provider diagnostics. Raw messages may contain sensitive data and
/// are never returned by connection probes.
pub(crate) fn permission_failure(body: &str) -> (String, Option<String>) {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = &value["error"];
    let details = error["details"].as_array();
    let reason = details.and_then(|d| d.iter().find_map(|d| d["reason"].as_str()))
        .or_else(|| error["errors"].as_array().and_then(|d| d.iter().find_map(|d| d["reason"].as_str())))
        .or_else(|| (error["message"].as_str() == Some("Request had insufficient authentication scopes.")).then_some("ACCESS_TOKEN_SCOPE_INSUFFICIENT"))
        .unwrap_or("permission_denied");
    let reason = if reason.len() <= 96 && reason.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') { reason } else { "permission_denied" };
    let url = details.and_then(|d| d.iter().find_map(|d| d["metadata"]["activationUrl"].as_str()))
        .filter(|u| u.len() <= 8192)
        .and_then(|u| url::Url::parse(u).ok())
        .filter(|u| u.scheme() == "https" && matches!(u.host_str(), Some("console.developers.google.com" | "console.cloud.google.com"))
            && u.username().is_empty() && u.password().is_none() && u.fragment().is_none() && u.port().is_none_or(|p| p == 443))
        .map(|u| u.to_string());
    (reason.into(), url)
}

#[derive(Serialize)]
pub(crate) struct ServiceProbe {
    service: &'static str,
    pub(crate) ok: bool,
    verified: bool,
    reason: String,
    enable_url: Option<String>,
}

pub(crate) async fn probe_services(token: &str, scopes: &[String]) -> Vec<ServiceProbe> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none()).build();
    let client = client.as_ref().ok();
    let (gmail, calendar, drive) = tokio::join!(
        probe_service(client, token, SERVICE_PROBE_URLS[0].0, SERVICE_PROBE_URLS[0].1),
        probe_service(client, token, SERVICE_PROBE_URLS[1].0, SERVICE_PROBE_URLS[1].1),
        probe_service(client, token, SERVICE_PROBE_URLS[2].0, SERVICE_PROBE_URLS[2].1),
    );
    let mut results = vec![gmail, calendar, drive];
    results.extend(scope_probes(scopes));
    results
}
const SERVICE_PROBE_URLS: [(&str, &str); 3] = [
    ("gmail", "https://gmail.googleapis.com/gmail/v1/users/me/profile"),
    ("calendar", "https://www.googleapis.com/calendar/v3/users/me/calendarList?maxResults=1"),
    ("drive", "https://www.googleapis.com/drive/v3/about?fields=user"),
];
async fn probe_service(client: Option<&reqwest::Client>, token: &str, service: &'static str, url: &str) -> ServiceProbe {
    let mut probe = ServiceProbe { service, ok: false, verified: false, reason: "network_error".into(), enable_url: None };
    if let Some(client) = client {
        if let Ok(response) = client.get(effective_url(url).as_ref()).bearer_auth(token).send().await {
            let status = response.status();
            let mut response = response;
            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= 64 * 1024 => bytes.extend_from_slice(&chunk),
                    Ok(None) => break,
                    _ => { probe.reason = "invalid_response".into(); return probe; }
                }
            }
            if status.is_success() {
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                if body.is_object() && body.get("error").is_none() {
                    probe.ok = true;
                    probe.verified = true;
                    probe.reason = "verified".into();
                } else { probe.reason = "invalid_response".into(); }
            } else if status.as_u16() == 403 {
                (probe.reason, probe.enable_url) = permission_failure(&String::from_utf8_lossy(&bytes));
            } else {
                probe.reason = match status.as_u16() { 401 => "needs_reconnect", 429 => "rate_limited", _ => "api_error" }.into();
            }
        }
    }
    if !token.is_empty() {
        if probe.reason.contains(token) { probe.reason = "permission_denied".into(); }
        if probe.enable_url.as_ref().is_some_and(|url| url.contains(token)) { probe.enable_url = None; }
    }
    probe
}
fn scope_probes(scopes: &[String]) -> Vec<ServiceProbe> {
    let mut results = Vec::new();
    for (service, allowed) in [
        ("sheets", &["https://www.googleapis.com/auth/spreadsheets", "https://www.googleapis.com/auth/spreadsheets.readonly", "https://www.googleapis.com/auth/drive", "https://www.googleapis.com/auth/drive.readonly", "https://www.googleapis.com/auth/drive.file"][..]),
        ("docs", &["https://www.googleapis.com/auth/documents", "https://www.googleapis.com/auth/documents.readonly", "https://www.googleapis.com/auth/drive", "https://www.googleapis.com/auth/drive.readonly", "https://www.googleapis.com/auth/drive.file"][..]),
    ] {
        let ok = scopes.iter().any(|s| allowed.contains(&s.as_str()));
        results.push(ServiceProbe { service, ok, verified: false, reason: if ok { "scope_granted_unverified" } else { "scope_missing" }.into(), enable_url: None });
    }
    results
}

// ── Token acquisition ───────────────────────────────────────────────────────

/// Original public adapter; the sealed Home owner now resolves OAuth.
pub use crate::google_identity::home_source::get_valid_google_token;

// ── HTTP plumbing ───────────────────────────────────────────────────────────

/// The URL actually requested. Production: unchanged. A `live-test-mocks`
/// build (never a release: see the `compile_error!` in `lib.rs`) may send
/// every Google REST call to a local mock named by
/// `DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE` (`https://gmail.googleapis.com/x` →
/// `<base>/gmail.googleapis.com/x`).
fn effective_url(url: &str) -> std::borrow::Cow<'_, str> {
    #[cfg(test)]
    {
        if let Some(base) = test_api_base::get()
            && let Some(rest) = url.strip_prefix("https://")
        {
            return std::borrow::Cow::Owned(format!("{base}/{rest}"));
        }
    }
    #[cfg(feature = "live-test-mocks")]
    {
        if let Ok(base) = std::env::var("DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE")
            && let Some(rest) = url.strip_prefix("https://")
        {
            return std::borrow::Cow::Owned(format!("{}/{rest}", base.trim_end_matches('/')));
        }
    }
    std::borrow::Cow::Borrowed(url)
}

/// Unit tests' mock Google (set once by the test that starts it).
#[cfg(test)]
pub(crate) mod test_api_base {
    use std::sync::OnceLock;
    static BASE: OnceLock<String> = OnceLock::new();
    pub(crate) fn set(base: String) {
        let _ = BASE.set(base);
    }
    pub(crate) fn get() -> Option<&'static str> {
        BASE.get().map(String::as_str)
    }
}

/// One read-only GET (identity checks). Same retry and status
/// classification as every tool call.
pub async fn api_get(token: &str, url: &str) -> Result<Value, GoogleApiError> {
    google_request(token, reqwest::Method::GET, url, &[], None).await
}

fn http_client() -> Result<reqwest::Client, GoogleApiError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| GoogleApiError::Http(format!("client build failed: {e}")))
}

tokio::task_local! {
    static DEDICATED_WRITE_POLICY: ();
}

/// Host-only scope for the dedicated execute route. Writes may have landed
/// before a transient error, so they must never be automatically repeated.
/// Box before constructing the scope to preserve the default-stack boundary.
pub(crate) fn with_dedicated_write_policy<F: std::future::Future>(
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    DEDICATED_WRITE_POLICY.scope((), Box::pin(future))
}

fn automatic_retry_allowed(method: &reqwest::Method) -> bool {
    *method == reqwest::Method::GET
        || *method == reqwest::Method::HEAD
        || DEDICATED_WRITE_POLICY.try_with(|_| ()).is_err()
}

/// Perform a Google REST call and classify its result. Legacy callers and
/// GET/HEAD may retry once on transport errors / 429 / 5xx. Dedicated writes
/// make one attempt. Success bodies are parsed as JSON.
async fn google_request(
    token: &str,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value, GoogleApiError> {
    let client = http_client()?;
    let retry_allowed = automatic_retry_allowed(&method);

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = client
            .request(method.clone(), effective_url(url).as_ref())
            .bearer_auth(token);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if retry_allowed && attempt < 2 {
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
        if (code == 429 || (500..=599).contains(&code)) && retry_allowed && attempt < 2 {
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
    let retry_allowed = automatic_retry_allowed(&method);

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = client
            .request(method.clone(), effective_url(url).as_ref())
            .bearer_auth(token);
        if !query.is_empty() {
            req = req.query(query);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if retry_allowed && attempt < 2 {
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

        if (code == 429 || (500..=599).contains(&code)) && retry_allowed && attempt < 2 {
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
    native_legacy::gmail_search(token, query, max_results).await
}

/// Read a single message in full: headers, plain-text (or html-stripped) body
/// truncated to [`BODY_MAX_CHARS`], and an attachment manifest (filename + size
/// only — attachments are never downloaded).
pub async fn gmail_read(token: &str, message_id: &str) -> Result<GmailReadResult, GoogleApiError> {
    native_legacy::gmail_read(token, message_id).await
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
    native_legacy::gmail_create_draft(token, to, subject, body_text, cc).await
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
    native_legacy::calendar_list_events(token, time_min, time_max, max_results).await
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
    native_legacy::calendar_create_event(
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
    native_legacy::sheets_read(token, spreadsheet_id, range).await
}

/// Append one row of values to a spreadsheet (write). Uses `USER_ENTERED` input
/// so numbers/dates/formulas are parsed the way a human typing them would be.
pub async fn sheets_append(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<String>,
) -> Result<SheetsAppendResult, GoogleApiError> {
    native_legacy::sheets_append(token, spreadsheet_id, range, values).await
}

// ── Forms (no official MCP server — native tools) ───────────────────────────

/// Read a form's structure: title, description, and the question list with the
/// `question_id`s needed to interpret [`forms_list_responses`] answers.
/// Read-only (`forms.body.readonly`).
pub async fn forms_get(token: &str, form_id: &str) -> Result<FormResult, GoogleApiError> {
    native_legacy::forms_get(token, form_id).await
}

/// List a form's submitted responses (read-only, `forms.responses.readonly`).
/// Capped at [`FORMS_MAX_RESPONSES`]; answers are keyed by `question_id` —
/// pair with [`forms_get`] to map ids to question titles.
pub async fn forms_list_responses(
    token: &str,
    form_id: &str,
) -> Result<FormResponsesResult, GoogleApiError> {
    native_legacy::forms_list_responses(token, form_id).await
}

// ── Tasks (no official MCP server — native tools) ───────────────────────────

/// List the user's task lists (each id is the `task_list_id` other task tools
/// take).
pub async fn tasks_list_tasklists(token: &str) -> Result<TaskListsResult, GoogleApiError> {
    native_legacy::tasks_list_tasklists(token).await
}

/// List tasks in one task list. `show_completed` includes finished tasks
/// (Google also requires `showHidden` for tasks hidden by a list clear).
pub async fn tasks_list(
    token: &str,
    task_list_id: &str,
    show_completed: bool,
    max_results: u32,
) -> Result<TasksResult, GoogleApiError> {
    native_legacy::tasks_list(token, task_list_id, show_completed, max_results).await
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
    native_legacy::tasks_create(token, task_list_id, title, notes, due).await
}

/// Mark a task completed (write) — `status: "completed"` via PATCH.
pub async fn tasks_complete(
    token: &str,
    task_list_id: &str,
    task_id: &str,
) -> Result<TaskEntry, GoogleApiError> {
    native_legacy::tasks_complete(token, task_list_id, task_id).await
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
    native_legacy::drive_search(token, query, mime_type, max_results).await
}

/// Read a Drive file as text. Google-native documents are **exported**
/// (Docs → `text/plain`, Sheets → `text/csv` first sheet only, Slides →
/// `text/plain`); text-ish blobs are downloaded verbatim; anything else
/// returns metadata plus a note instead of binary garbage.
pub async fn drive_read(token: &str, file_id: &str) -> Result<DriveReadResult, GoogleApiError> {
    native_legacy::drive_read(token, file_id).await
}

/// Read a Google Doc's text (paragraphs plus table cell text, in document
/// order). Read path of the `documents` scope.
pub async fn docs_read(token: &str, document_id: &str) -> Result<DocsReadResult, GoogleApiError> {
    native_legacy::docs_read(token, document_id).await
}

/// Append text to the end of a Google Doc's body (write). Deliberately
/// append-only: no tool rewrites or deletes existing document content, so a
/// mistaken call can never destroy the user's text.
pub async fn docs_append(
    token: &str,
    document_id: &str,
    text: &str,
) -> Result<DocsAppendResult, GoogleApiError> {
    native_legacy::docs_append(token, document_id, text).await
}

/// Read a Google Slides presentation's text, slide by slide (read-only).
pub async fn slides_read(
    token: &str,
    presentation_id: &str,
) -> Result<SlidesReadResult, GoogleApiError> {
    native_legacy::slides_read(token, presentation_id).await
}

// ── Pure helpers (unit-tested) ──────────────────────────────────────────────

fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    native_legacy::clamp(n, lo, hi)
}

/// Truncate to a character budget, reporting whether anything was dropped.
/// Character-based (never byte slicing) so CJK text can't be cut mid-codepoint.
fn truncate_text(s: &str, max_chars: usize) -> (String, bool) {
    native_legacy::truncate_text(s, max_chars)
}

/// Escape a value interpolated into a Drive `q` search string. Drive's query
/// grammar delimits string literals with `'`, escaping `\` and `'` with a
/// backslash — without this a name containing a quote would break out of the
/// literal and change the query's meaning.
fn escape_drive_query_value(s: &str) -> String {
    native_legacy::escape_drive_query_value(s)
}

/// Export MIME type for a Google-native document, or `None` for blobs.
/// Mapping per Google's export-formats reference: Docs/Slides → `text/plain`,
/// Sheets → `text/csv` (first sheet only).
fn export_mime_for(mime: &str) -> Option<&'static str> {
    native_legacy::export_mime_for(mime)
}

/// Whether a blob MIME type is safe to render as text in a tool result.
fn is_text_like_mime(mime: &str) -> bool {
    native_legacy::is_text_like_mime(mime)
}

/// Extract a Drive/Docs/Slides file id from a share URL, or pass a bare id
/// through. Handles `/d/<ID>/edit`, `/d/e/<ID>/…` and `?id=<ID>` shapes.
pub fn extract_drive_file_id(input: &str) -> String {
    native_legacy::extract_drive_file_id(input)
}

/// Walk a Docs `body.content[]` array, appending paragraph text in order.
/// Recurses into table cells so tabular content is not silently dropped.
fn collect_doc_text(content: &Value, out: &mut String) {
    native_legacy::collect_doc_text(content, out)
}

/// Walk a Slides `pageElements[]` array, appending shape/table text. Recurses
/// into groups (`elementGroup.children`) so grouped shapes are included.
fn collect_slide_text(page_elements: &Value, out: &mut String) {
    native_legacy::collect_slide_text(page_elements, out)
}

/// Append a Slides `text.textElements[]` run sequence. `paragraphMarker`
/// entries carry no `textRun` and are skipped.
fn append_slide_text_elements(text: &Value, out: &mut String) {
    native_legacy::append_slide_text_elements(text, out)
}

/// Read a string field, defaulting to empty.
fn str_field(v: &Value, key: &str) -> String {
    native_legacy::str_field(v, key)
}

/// Extract a form id from a full Google Forms URL, or pass a bare id through.
/// Handles both the editor (`/forms/d/<ID>/edit`) and the viewer
/// (`/forms/d/e/<LONG_ID>/viewform`) shapes.
pub fn extract_form_id(input: &str) -> String {
    native_legacy::extract_form_id(input)
}

/// Map one `items[]` entry from `forms.get` to a [`FormQuestion`]. Non-question
/// items (page breaks, images, text blocks) return `None`.
fn parse_form_item(item: &Value) -> Option<FormQuestion> {
    native_legacy::parse_form_item(item)
}

/// Flatten one `responses[]` entry: `answers` is a map of question_id →
/// `textAnswers.answers[].value`, joined for multi-select questions.
fn parse_form_response(resp: &Value) -> FormResponseEntry {
    native_legacy::parse_form_response(resp)
}

/// Map one Tasks API task object to a [`TaskEntry`].
fn parse_task(t: &Value) -> TaskEntry {
    native_legacy::parse_task(t)
}

/// Extract a spreadsheet id from a full Google Sheets URL, or pass a bare id
/// through unchanged. Handles `.../spreadsheets/d/<ID>/edit#gid=0`.
pub fn extract_spreadsheet_id(input: &str) -> String {
    native_legacy::extract_spreadsheet_id(input)
}

/// Percent-encode a single URL path component (sheet ranges contain `!`, `:`,
/// spaces, and single quotes that must not break the path).
fn encode_path_component(s: &str) -> String {
    native_legacy::encode_path_component(s)
}

/// Render a spreadsheet cell (string/number/bool) as a plain string.
fn cell_to_string(cell: &Value) -> String {
    native_legacy::cell_to_string(cell)
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
    native_legacy::encode_header_word(s)
}

/// Build an RFC 2822 message. CRLF line endings; UTF-8 body declared 8bit.
fn build_rfc2822(to: &str, subject: &str, body: &str, cc: Option<&str>) -> String {
    native_legacy::build_rfc2822(to, subject, body, cc)
}

/// base64url (no padding) — Gmail's `raw` field encoding.
fn encode_raw_message(raw: &str) -> String {
    native_legacy::encode_raw_message(raw)
}

/// Decode Gmail's web-safe base64 body data (padded or not).
fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    native_legacy::decode_b64url(s)
}

/// Case-insensitive header lookup over a Gmail `headers` array.
fn header_value(headers: &Value, name: &str) -> Option<String> {
    native_legacy::header_value(headers, name)
}

/// Best-effort HTML → text: strip tags, decode a few common entities.
fn html_to_text(html: &str) -> String {
    native_legacy::html_to_text(html)
}

/// Walk a MIME tree, preferring text/plain, falling back to html-stripped text.
fn extract_message_body(payload: &Value) -> String {
    native_legacy::extract_message_body(payload)
}

fn find_part_text(node: &Value, want_mime: &str) -> Option<String> {
    native_legacy::find_part_text(node, want_mime)
}

fn collect_attachments(payload: &Value) -> Vec<GmailAttachment> {
    native_legacy::collect_attachments(payload)
}

fn collect_attachments_rec(node: &Value, out: &mut Vec<GmailAttachment>) {
    native_legacy::collect_attachments_rec(node, out)
}

/// A calendar event start/end can be `dateTime` (timed) or `date` (all-day).
fn event_time(node: &Value, key: &str) -> String {
    native_legacy::event_time(node, key)
}

fn extract_meet_link(event: &Value) -> Option<String> {
    native_legacy::extract_meet_link(event)
}

fn parse_calendar_event(item: &Value) -> CalendarEvent {
    native_legacy::parse_calendar_event(item)
}

/// Pull a human-readable message out of a Google error JSON body.
fn extract_api_message(body: &str) -> String {
    native_legacy::extract_api_message(body)
}

/// Build the 403 re-auth guidance, echoing Google's reason plus the scopes we
/// need the user to re-grant.
fn scope_guidance(api_error_body: &str) -> String {
    native_legacy::scope_guidance(api_error_body)
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

/// Search mail through whichever backend is configured.
pub async fn gmail_search_via(
    backend: &GoogleBackend,
    query: &str,
    max_results: u32,
) -> Result<GmailSearchResult, GoogleApiError> {
    match backend {
        GoogleBackend::Direct(token) => gmail_search(token, query, max_results).await,
        GoogleBackend::AppsScript(cfg) => {
            legacy_bridge::gmail_search(&mut legacy_bridge::LegacyIo(cfg), query, max_results).await
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
            legacy_bridge::gmail_read(&mut legacy_bridge::LegacyIo(cfg), message_id).await
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
        GoogleBackend::Direct(token) => gmail_create_draft(token, to, subject, body_text, cc).await,
        GoogleBackend::AppsScript(cfg) => {
            legacy_bridge::gmail_create_draft(
                &mut legacy_bridge::LegacyIo(cfg),
                to,
                subject,
                body_text,
                cc,
            )
            .await
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
            legacy_bridge::calendar_list_events(
                &mut legacy_bridge::LegacyIo(cfg),
                time_min,
                time_max,
                max_results,
            )
            .await
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
            legacy_bridge::calendar_create_event(
                &mut legacy_bridge::LegacyIo(cfg),
                summary,
                start_rfc3339,
                end_rfc3339,
                description,
                attendees,
                with_meet,
            )
            .await
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
            legacy_bridge::sheets_read(&mut legacy_bridge::LegacyIo(cfg), spreadsheet_id, range)
                .await
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
        GoogleBackend::Direct(token) => sheets_append(token, spreadsheet_id, range, values).await,
        GoogleBackend::AppsScript(cfg) => {
            legacy_bridge::sheets_append(
                &mut legacy_bridge::LegacyIo(cfg),
                spreadsheet_id,
                range,
                values,
            )
            .await
        }
    }
}

/// Days from now until an RFC3339 timestamp, for translating the direct path's
/// time range onto the bridge's day-count parameter. `None` when absent or
/// unparseable — the caller then uses its own default rather than guessing.
#[cfg(test)]
fn days_until(time_max: Option<&str>) -> Option<u32> {
    legacy_bridge::days_until(time_max)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dedicated_write_policy_preserves_read_and_legacy_status_retries() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for code in [429u16, 503] {
            for method in [
                reqwest::Method::GET,
                reqwest::Method::HEAD,
                reqwest::Method::POST,
            ] {
                for text in [false, true] {
                    let count = Arc::new(AtomicUsize::new(0));
                    let seen = count.clone();
                    let app = axum::Router::new().fallback(axum::routing::any(move || {
                        let seen = seen.clone();
                        async move {
                            let status = if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                                code
                            } else {
                                200
                            };
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                axum::Json(json!({"fixture":true})),
                            )
                        }
                    }));
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let url = format!("http://{}/", listener.local_addr().unwrap());
                    let server = tokio::spawn(async move {
                        axum::serve(listener, app).await.unwrap();
                    });
                    // POST here is intentionally legacy: it keeps its original retry.
                    let dedicated = method != reqwest::Method::POST;
                    let call = async {
                        if text {
                            google_request_text("synthetic-unit-token", method.clone(), &url, &[])
                                .await
                                .map(|_| ())
                        } else {
                            google_request("synthetic-unit-token", method.clone(), &url, &[], None)
                                .await
                                .map(|_| ())
                        }
                    };
                    let result = if dedicated {
                        with_dedicated_write_policy(call).await
                    } else {
                        call.await
                    };
                    server.abort();
                    let _ = server.await;
                    assert!(
                        result.is_ok(),
                        "{method} text={text} status={code}: {result:?}"
                    );
                    assert_eq!(
                        count.load(Ordering::SeqCst),
                        2,
                        "{method} text={text} status={code}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn dedicated_write_policy_never_retries_transport_failure_but_reads_and_legacy_do() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::io::AsyncReadExt;
        for (method, dedicated, expected) in [
            (reqwest::Method::POST, true, 1),
            (reqwest::Method::GET, true, 2),
            (reqwest::Method::HEAD, true, 2),
            (reqwest::Method::POST, false, 2),
        ] {
            for text in [false, true] {
                let count = Arc::new(AtomicUsize::new(0));
                let seen = count.clone();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}/", listener.local_addr().unwrap());
                let server = tokio::spawn(async move {
                    loop {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let mut request = [0u8; 4096];
                        assert!(socket.read(&mut request).await.unwrap() > 0);
                        seen.fetch_add(1, Ordering::SeqCst);
                        // No HTTP response: the request arrived but its outcome is unknown.
                        drop(socket);
                    }
                });
                let call = async {
                    if text {
                        google_request_text("synthetic-unit-token", method.clone(), &url, &[])
                            .await
                            .map(|_| ())
                    } else {
                        google_request(
                            "synthetic-unit-token",
                            method.clone(),
                            &url,
                            &[],
                            Some(&json!({"fixture":true})),
                        )
                        .await
                        .map(|_| ())
                    }
                };
                let result = if dedicated {
                    with_dedicated_write_policy(call).await
                } else {
                    call.await
                };
                server.abort();
                let _ = server.await;
                assert!(result.is_err());
                assert_eq!(
                    count.load(Ordering::SeqCst),
                    expected,
                    "{method} dedicated={dedicated} text={text}"
                );
            }
        }
    }

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
        let inner = enc.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(inner)
            .unwrap();
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
        assert_eq!(
            header_value(&headers, "from").as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(
            header_value(&headers, "Subject").as_deref(),
            Some("Hi there")
        );
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
        let body =
            r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes."}}"#;
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
        assert_eq!(
            parse_form_response(&r2).submitted_at,
            "2026-07-01T00:00:00Z"
        );
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
            extract_drive_file_id(
                "https://docs.google.com/presentation/d/1DeckId/edit?usp=sharing"
            ),
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
        assert!(
            REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/presentations.readonly")
        );
        assert!(!REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/presentations"));
        assert!(REQUIRED_SCOPES.contains(&"https://www.googleapis.com/auth/documents"));
    }

    #[test]
    fn integration_gate_defaults_closed_and_opens_only_on_explicit_true() {
        let dir = tempfile::tempdir().unwrap();
        // No config.toml at all → closed.
        assert!(!integration_enabled(dir.path()));
        // Config without the section → closed.
        std::fs::write(
            dir.path().join("config.toml"),
            "[general]\nlog_level = \"info\"\n",
        )
        .unwrap();
        assert!(!integration_enabled(dir.path()));
        // Explicit false → closed.
        std::fs::write(
            dir.path().join("config.toml"),
            "[integrations]\ngoogle_workspace = false\n",
        )
        .unwrap();
        assert!(!integration_enabled(dir.path()));
        // Malformed toml → closed (fail closed).
        std::fs::write(dir.path().join("config.toml"), "[integrations\n???").unwrap();
        assert!(!integration_enabled(dir.path()));
        // Explicit true → open.
        std::fs::write(
            dir.path().join("config.toml"),
            "[integrations]\ngoogle_workspace = true\n",
        )
        .unwrap();
        assert!(integration_enabled(dir.path()));
    }
}

mod legacy_bridge {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_workspace/bridge_program.inc.rs"
    ));
    fn bridge_cell_to_string(value: &serde_json::Value) -> String {
        super::native_legacy::cell_to_string(value)
    }
    pub(super) struct LegacyIo<'a>(pub(super) &'a crate::google_apps_script::BridgeConfig);
    #[async_trait::async_trait]
    impl LocalIo for LegacyIo<'_> {
        async fn call(
            &mut self,
            action: crate::google_apps_script::BridgeAction,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, crate::google_workspace::GoogleApiError> {
            crate::google_apps_script::call(self.0, action, params)
                .await
                .map_err(bridge_err)
        }
    }
}

#[cfg(test)]
mod google_setup_tests {
    use super::*;

    #[test]
    fn google_api_disabled_guidance_accepts_only_console_origins() {
        for host in ["console.developers.google.com", "console.cloud.google.com"] {
            let body = json!({"error":{"details":[{"reason":"SERVICE_DISABLED","metadata":{"activationUrl":format!("https://{host}/apis/api/gmail.googleapis.com/overview")}}]}}).to_string();
            let (reason, url) = permission_failure(&body);
            assert_eq!(reason, "SERVICE_DISABLED");
            assert!(url.unwrap().starts_with(&format!("https://{host}/")));
            assert!(scope_guidance(&body).contains("Enable this API"));
            assert!(!scope_guidance(&body).contains("Reconnect"));
        }
        for url in ["https://console.cloud.google.com.evil.test/", "https://console.cloud.google.com@evil.test/", "http://console.cloud.google.com/", "https://console.cloud.google.com:444/", "https://evil@console.cloud.google.com/"] {
            let body = json!({"error":{"details":[{"reason":"SERVICE_DISABLED","metadata":{"activationUrl":url}}]}}).to_string();
            assert!(permission_failure(&body).1.is_none(), "{url}");
            assert!(!scope_guidance(&body).contains(url));
        }
    }

    #[test]
    fn google_invalid_selected_path_never_falls_back() {
        let home = tempfile::tempdir().unwrap();
        for config in [
            "[integrations]\ngoogle_credential_path='service_account'\n",
            "[integrations]\ngoogle_credential_path='apps_script'\n",
            "[integrations]\ngoogle_credential_path='oauth'\n[integrations.google_apps_script]\nurl='x'\n",
            "[integrations]\ngoogle_credential_path='none'\n[integrations.google_service_account]\nkey_file='x'\n",
        ] {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            assert!(selected_path(home.path()).is_err());
        }
    }

    #[test]
    fn google_403_reconnects_only_for_scope_failures() {
        let disabled = r#"{"error":{"errors":[{"reason":"accessNotConfigured"}]}}"#;
        assert!(scope_guidance(disabled).contains("Enable"));
        let scopes = r#"{"error":{"details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#;
        assert!(scope_guidance(scopes).contains("Reconnect"));
        let denied = r#"{"error":{"message":"private-token-value","errors":[{"reason":"domainPolicy"}]}}"#;
        assert!(scope_guidance(denied).contains("domainPolicy"));
        assert!(!scope_guidance(denied).contains("Reconnect"));
        assert!(!scope_guidance(denied).contains("private-token-value"));
    }

    #[test]
    fn google_sheets_docs_scope_results_are_never_live_verified() {
        for (scopes, expected) in [(vec![], false), (vec!["https://www.googleapis.com/auth/drive.file".into()], true)] {
            let probes = scope_probes(&scopes);
            assert_eq!(probes.len(), 2);
            assert_eq!(probes[0].service, "sheets");
            assert_eq!(probes[1].service, "docs");
            for probe in probes {
                assert_eq!(probe.ok, expected);
                assert!(!probe.verified);
            }
        }
        assert_eq!(SERVICE_PROBE_URLS[0].1, "https://gmail.googleapis.com/gmail/v1/users/me/profile");
        assert!(SERVICE_PROBE_URLS[1].1.ends_with("calendarList?maxResults=1"));
        assert!(SERVICE_PROBE_URLS[2].1.ends_with("about?fields=user"));
    }

    #[tokio::test]
    async fn google_service_probe_uses_read_get_and_redacts_failures() {
        use axum::{routing::get, Router};
        let app = Router::new().route("/profile", get(|| async { axum::Json(json!({"emailAddress":"test@example.test"})) }))
            .route("/disabled", get(|| async { (axum::http::StatusCode::FORBIDDEN, axum::Json(json!({"error":{"message":"secret-bearer-token","details":[{"reason":"SERVICE_DISABLED","metadata":{"activationUrl":"https://console.cloud.google.com/apis/library/gmail.googleapis.com"}}]}}))) }))
            .route("/invalid", get(|| async { "secret-bearer-token" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let client = reqwest::Client::new();
        let ok = probe_service(Some(&client), "secret-bearer-token", "gmail", &format!("{base}/profile")).await;
        assert!(ok.ok && ok.verified);
        let denied = probe_service(Some(&client), "secret-bearer-token", "calendar", &format!("{base}/disabled")).await;
        assert!(!denied.ok && !denied.verified);
        assert_eq!(denied.reason, "SERVICE_DISABLED");
        assert!(denied.enable_url.is_some());
        assert!(!serde_json::to_string(&denied).unwrap().contains("secret-bearer-token"));
        let invalid = probe_service(Some(&client), "secret-bearer-token", "drive", &format!("{base}/invalid")).await;
        assert!(!invalid.ok);
        assert_eq!(invalid.reason, "invalid_response");
        server.abort();
    }
}
