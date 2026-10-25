//! One native Google tool implementation shared by MCP and host execution.
//! Access decisions stay with the caller; no credential is a tool result.
use crate::google_workspace::GoogleBackend;
use serde_json::Value;
use std::path::Path;

#[async_trait::async_trait]
pub trait NativeGoogleAccess: Send + Sync {
    async fn backend(&self, home: &Path, tool: &str) -> Result<GoogleBackend, String>;
    async fn token(&self, home: &Path, tool: &str) -> Result<String, String>;
}

fn tool_text(text: &str) -> Value {
    serde_json::json!({"content":[{"type":"text","text":text}]})
}
fn tool_error(text: &str) -> Value {
    serde_json::json!({"content":[{"type":"text","text":text}],"isError":true})
}

/// Read a u32 argument that may arrive as a JSON number or string.
pub fn arg_u32(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .or_else(|| {
            args.get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .unwrap_or(default)
}

pub async fn handle_gmail_search(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("gmail_search", args, home_dir, g).await
}

pub async fn handle_gmail_read(args: &Value, home_dir: &Path, g: &dyn NativeGoogleAccess) -> Value {
    crate::google_workspace::native_legacy::handle("gmail_read", args, home_dir, g).await
}

pub async fn handle_gmail_create_draft(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("gmail_create_draft", args, home_dir, g).await
}

pub async fn handle_calendar_list_events(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("calendar_list_events", args, home_dir, g).await
}

pub async fn handle_calendar_create_event(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("calendar_create_event", args, home_dir, g).await
}

// ─────────────────────────────────────────────────────────────────
// Google Sheets tool handlers. Consume the OAuth vault token via the gateway
// `google_workspace` module (shared `google` provider — same connect flow as
// Gmail/Calendar; requires the spreadsheets scope, so a token authorized before
// Sheets shipped will 403 and guide the user to reconnect).
// ─────────────────────────────────────────────────────────────────

/// Read a u64 argument that may arrive as a JSON number or string.
pub fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64()).or_else(|| {
        args.get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| s.trim().parse::<u64>().ok())
    })
}

pub async fn handle_sheets_read(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("sheets_read", args, home_dir, g).await
}

pub async fn handle_sheets_append(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("sheets_append", args, home_dir, g).await
}

// ─────────────────────────────────────────────────────────────────
// Google Forms + Tasks handlers. Neither service has an official Google
// remote MCP server (probed 404 on 2026-07-30, absent from Google's docs), so
// DuDuClaw serves them natively off the same connected Google account.
// ─────────────────────────────────────────────────────────────────

/// Default Google Tasks list alias — the API accepts `@default` in place of an
/// id, so an agent can create/list tasks without first calling `tasks_lists`.
pub const TASKS_DEFAULT_LIST: &str = "@default";

pub fn task_list_arg(args: &Value) -> &str {
    args.get("task_list_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(TASKS_DEFAULT_LIST)
}

pub async fn handle_forms_get(args: &Value, home_dir: &Path, g: &dyn NativeGoogleAccess) -> Value {
    crate::google_workspace::native_legacy::handle("forms_get", args, home_dir, g).await
}

pub async fn handle_forms_list_responses(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("forms_list_responses", args, home_dir, g).await
}

pub async fn handle_gtasks_lists(home_dir: &Path, g: &dyn NativeGoogleAccess) -> Value {
    crate::google_workspace::native_legacy::handle(
        "gtasks_lists",
        &serde_json::json!({}),
        home_dir,
        g,
    )
    .await
}

pub async fn handle_gtasks_list(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("gtasks_list", args, home_dir, g).await
}

pub async fn handle_gtasks_create(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("gtasks_create", args, home_dir, g).await
}

pub async fn handle_gtasks_complete(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("gtasks_complete", args, home_dir, g).await
}

// ─────────────────────────────────────────────────────────────────
// Google Drive / Docs / Slides handlers. Google's official MCP servers for
// these three are Developer-Preview-only and their terms forbid exposing
// Pre-GA APIs outside your own domain, so DuDuClaw uses the GA REST APIs
// natively (2026-07-30 decision) — no preview enrollment for any customer.
// ─────────────────────────────────────────────────────────────────

pub async fn handle_drive_search(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("drive_search", args, home_dir, g).await
}

pub async fn handle_drive_read(args: &Value, home_dir: &Path, g: &dyn NativeGoogleAccess) -> Value {
    crate::google_workspace::native_legacy::handle("drive_read", args, home_dir, g).await
}

pub async fn handle_docs_read(args: &Value, home_dir: &Path, g: &dyn NativeGoogleAccess) -> Value {
    crate::google_workspace::native_legacy::handle("docs_read", args, home_dir, g).await
}

pub async fn handle_docs_append(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("docs_append", args, home_dir, g).await
}

pub async fn handle_slides_read(
    args: &Value,
    home_dir: &Path,
    g: &dyn NativeGoogleAccess,
) -> Value {
    crate::google_workspace::native_legacy::handle("slides_read", args, home_dir, g).await
}

// ─────────────────────────────────────────────────────────────────
// Notion tool handlers. Consume the OAuth vault token via the gateway
// `notion_workspace` module. Notion content is an external knowledge source —
// surfaced for query/citation only, never auto-written into the shared wiki.
// ─────────────────────────────────────────────────────────────────
