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

const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";
const SHEETS_BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Max rows returned by `sheets_read` before truncation.
const SHEETS_MAX_ROWS: usize = 200;

/// Maximum characters of a mail body returned by `gmail_read` before truncation.
const BODY_MAX_CHARS: usize = 8000;

/// Scopes this integration needs. Used for `google_status` diagnostics and the
/// 403 re-auth guidance message.
pub const REQUIRED_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.compose",
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/spreadsheets",
    "https://www.googleapis.com/auth/userinfo.email",
];

// ── Errors ──────────────────────────────────────────────────────────────────

/// Failures obtaining a usable Google access token from the vault.
#[derive(Debug)]
pub enum GoogleAuthError {
    /// No `google` token stored — the user has never connected.
    NotConnected,
    /// Token expired and no refresh_token is available → full re-auth needed.
    NoRefreshToken,
    /// Token expired, refresh_token present, but the client credentials used to
    /// obtain it were not persisted → re-connect from the dashboard.
    ClientConfigMissing,
    /// A refresh attempt was made and failed.
    RefreshFailed(String),
}

impl std::fmt::Display for GoogleAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoogleAuthError::NotConnected => write!(
                f,
                "Google is not connected. Open the dashboard Integrations → Google page and connect your Google account first."
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

// ── Token acquisition ───────────────────────────────────────────────────────

/// Return a valid Google access token, refreshing in place if expired.
///
/// Fast path returns the stored non-expired token. On expiry we use the
/// user-stored client credentials (persisted at OAuth-config time) to run a
/// refresh grant, persist the new token, and return it. Every failure mode maps
/// to an actionable [`GoogleAuthError`] that guides the user back to the
/// dashboard.
pub async fn get_valid_google_token(home_dir: &Path) -> Result<String, GoogleAuthError> {
    // Fast path: `get_token` already filters out expired tokens.
    if let Some(t) = mcp_oauth::get_token(home_dir, GOOGLE_PROVIDER) {
        return Ok(t.access_token);
    }

    // Not returned by get_token ⇒ either missing entirely or expired.
    let existing = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == GOOGLE_PROVIDER);
    let existing = match existing {
        Some(t) => t,
        None => return Err(GoogleAuthError::NotConnected),
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

// ── Pure helpers (unit-tested) ──────────────────────────────────────────────

fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    n.max(lo).min(hi)
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
