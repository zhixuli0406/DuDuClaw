//! Native Notion REST client — search, page read, page append.
//!
//! The "native tools" path for Notion: instead of shelling out to a third-party
//! npm MCP server, we call the Notion REST API directly and consume the access
//! token from the existing `mcp_oauth` vault (the `notion` provider).
//!
//! Notion specifics (see also `mcp_oauth::build_exchange_request`):
//! - The access token is **long-lived and has no refresh_token**. `expires_at`
//!   is `None` — that is the normal, healthy state, not an error. There is no
//!   in-place refresh: an invalid token means the user must re-authorize.
//! - Every request must carry the `Notion-Version` header.
//!
//! Security posture:
//! - Read tools (`notion_search`, `notion_page_read`) only expand into curated
//!   response structs / plain text — we never dump the raw Notion JSON blob.
//! - `notion_page_append` is the only write; it appends paragraph blocks to an
//!   existing page and never deletes or overwrites. Operators can additionally
//!   gate it behind `agent.toml [capabilities] approval_required_tools`.
//! - Notion content is an *external knowledge source* — it is surfaced for
//!   query/citation only and is never auto-written into the shared wiki.

use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::mcp_oauth;

/// Provider id in the `mcp_oauth` vault.
pub const NOTION_PROVIDER: &str = "notion";

const NOTION_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Max blocks pulled by `notion_page_read` before we stop paginating.
const MAX_BLOCKS: usize = 200;
/// Per-block plain-text truncation budget (CJK-safe codepoint count).
const BLOCK_MAX_CHARS: usize = 4000;
/// Max characters of a page's concatenated body before truncation.
const PAGE_BODY_MAX_CHARS: usize = 16000;

// ── Errors ──────────────────────────────────────────────────────────────────

/// Failures obtaining a usable Notion access token from the vault.
#[derive(Debug)]
pub enum NotionAuthError {
    /// No `notion` token stored — the user has never connected (or revoked).
    NotConnected,
}

impl std::fmt::Display for NotionAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotionAuthError::NotConnected => write!(
                f,
                "Notion is not connected. Open the dashboard Integrations → Notion page and connect your Notion workspace first."
            ),
        }
    }
}

/// Failures calling the Notion REST API.
#[derive(Debug)]
pub enum NotionApiError {
    Auth(NotionAuthError),
    Http(String),
    /// 401 — token invalid/revoked; the user should reconnect.
    Unauthorized,
    /// 403 — the integration lacks access to the requested resource.
    Forbidden(String),
    /// 404 — page/resource not found or not shared with the integration.
    NotFound(String),
    /// 429 — rate limited after one retry.
    RateLimited,
    Api { status: u16, message: String },
}

impl std::fmt::Display for NotionApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotionApiError::Auth(e) => write!(f, "{e}"),
            NotionApiError::Http(e) => write!(f, "Network error contacting Notion: {e}"),
            NotionApiError::Unauthorized => write!(
                f,
                "Notion rejected the request (401 Unauthorized). The authorization is no longer valid — reconnect Notion from the dashboard Integrations → Notion page."
            ),
            NotionApiError::Forbidden(msg) => write!(
                f,
                "Notion denied the request (403): {msg}. Make sure the page/database is shared with your integration (open the page in Notion → ••• → Connections → add your integration)."
            ),
            NotionApiError::NotFound(msg) => write!(
                f,
                "Notion could not find the resource (404): {msg}. It may not exist, or it has not been shared with your integration."
            ),
            NotionApiError::RateLimited => write!(
                f,
                "Notion rate-limited the request (429). Please retry in a moment."
            ),
            NotionApiError::Api { status, message } => {
                write!(f, "Notion API error ({status}): {message}")
            }
        }
    }
}

impl From<NotionAuthError> for NotionApiError {
    fn from(e: NotionAuthError) -> Self {
        NotionApiError::Auth(e)
    }
}

// ── Response structs (curated, never raw Notion JSON) ───────────────────────

#[derive(Debug, Serialize)]
pub struct NotionSearchHit {
    pub id: String,
    pub title: String,
    /// "page" or "database".
    pub object_type: String,
    pub last_edited: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct NotionSearchResult {
    pub count: usize,
    pub results: Vec<NotionSearchHit>,
}

#[derive(Debug, Serialize)]
pub struct NotionPageResult {
    pub id: String,
    pub title: String,
    pub url: String,
    pub last_edited: String,
    pub text: String,
    pub text_truncated: bool,
    pub block_count: usize,
    pub blocks_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct NotionAppendResult {
    pub page_id: String,
    pub appended_blocks: usize,
}

// ── Token acquisition ───────────────────────────────────────────────────────

/// Return the stored Notion access token, or an actionable error.
///
/// Notion tokens do not expire and carry no refresh_token, so there is nothing
/// to refresh: a missing token means the user has never connected (or revoked),
/// and an invalid token surfaces later as a 401 that guides them to reconnect.
pub async fn get_valid_notion_token(home_dir: &Path) -> Result<String, NotionAuthError> {
    match mcp_oauth::get_token(home_dir, NOTION_PROVIDER) {
        Some(t) => Ok(t.access_token),
        None => Err(NotionAuthError::NotConnected),
    }
}

// ── HTTP plumbing ───────────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::Client, NotionApiError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| NotionApiError::Http(format!("client build failed: {e}")))
}

/// One Notion REST call with a single retry on transport error / 429 / 5xx.
async fn notion_request(
    token: &str,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value, NotionApiError> {
    let client = http_client()?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = client
            .request(method.clone(), url)
            .bearer_auth(token)
            .header("Notion-Version", NOTION_VERSION);
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
                return Err(NotionApiError::Http(format!("request failed: {e}")));
            }
        };

        let code = resp.status().as_u16();
        if resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text)
                .map_err(|e| NotionApiError::Http(format!("invalid JSON from Notion: {e}")));
        }

        if (code == 429 || (500..=599).contains(&code)) && attempt < 2 {
            continue;
        }

        let body_text = resp.text().await.unwrap_or_default();
        let msg = duduclaw_core::truncate_chars(&extract_api_message(&body_text), 240);
        return Err(match code {
            401 => NotionApiError::Unauthorized,
            403 => NotionApiError::Forbidden(msg),
            404 => NotionApiError::NotFound(msg),
            429 => NotionApiError::RateLimited,
            _ => NotionApiError::Api { status: code, message: msg },
        });
    }
}

// ── Search ──────────────────────────────────────────────────────────────────

/// Search pages and databases shared with the integration. `max_results` is
/// clamped to 1..=25.
pub async fn notion_search(
    token: &str,
    query: &str,
    max_results: u32,
) -> Result<NotionSearchResult, NotionApiError> {
    let n = clamp(max_results, 1, 25);
    let body = json!({
        "query": query,
        "page_size": n,
    });
    let resp = notion_request(
        token,
        reqwest::Method::POST,
        &format!("{NOTION_BASE}/search"),
        &[],
        Some(&body),
    )
    .await?;

    let results: Vec<NotionSearchHit> = resp
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().map(parse_search_hit).collect())
        .unwrap_or_default();

    Ok(NotionSearchResult {
        count: results.len(),
        results,
    })
}

// ── Page read ───────────────────────────────────────────────────────────────

/// Retrieve a page's metadata + its child blocks flattened to plain text.
/// Pagination is followed up to [`MAX_BLOCKS`] blocks.
pub async fn notion_page_read(
    token: &str,
    page_id: &str,
) -> Result<NotionPageResult, NotionApiError> {
    let page = notion_request(
        token,
        reqwest::Method::GET,
        &format!("{NOTION_BASE}/pages/{page_id}"),
        &[],
        None,
    )
    .await?;

    let title = extract_notion_title(&page);
    let url = page.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let last_edited = page
        .get("last_edited_time")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Walk block children with cursor pagination.
    let mut lines: Vec<String> = Vec::new();
    let mut block_count = 0usize;
    let mut cursor: Option<String> = None;
    let mut blocks_truncated = false;

    loop {
        let mut query: Vec<(&str, String)> = vec![("page_size", "100".to_string())];
        if let Some(c) = &cursor {
            query.push(("start_cursor", c.clone()));
        }
        let resp = notion_request(
            token,
            reqwest::Method::GET,
            &format!("{NOTION_BASE}/blocks/{page_id}/children"),
            &query,
            None,
        )
        .await?;

        let arr = resp
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        for block in &arr {
            block_count += 1;
            if let Some(text) = block_to_text(block) {
                lines.push(duduclaw_core::truncate_chars(&text, BLOCK_MAX_CHARS));
            }
            if block_count >= MAX_BLOCKS {
                blocks_truncated = resp
                    .get("has_more")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    || cursor.is_some();
                break;
            }
        }

        if block_count >= MAX_BLOCKS {
            blocks_truncated = true;
            break;
        }
        let has_more = resp.get("has_more").and_then(|v| v.as_bool()).unwrap_or(false);
        cursor = resp
            .get("next_cursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if !has_more || cursor.is_none() {
            break;
        }
    }

    let joined = lines.join("\n");
    let text = duduclaw_core::truncate_chars(&joined, PAGE_BODY_MAX_CHARS);
    let text_truncated = text.chars().count() < joined.chars().count();

    Ok(NotionPageResult {
        id: page_id.to_string(),
        title,
        url,
        last_edited,
        text,
        text_truncated,
        block_count,
        blocks_truncated,
    })
}

// ── Page append (write) ─────────────────────────────────────────────────────

/// Append `text` as one or more paragraph blocks to an existing page. Each
/// non-empty line becomes its own paragraph block. Never deletes/overwrites.
pub async fn notion_page_append(
    token: &str,
    page_id: &str,
    text: &str,
) -> Result<NotionAppendResult, NotionApiError> {
    let children = text_to_paragraph_blocks(text);
    if children.is_empty() {
        return Err(NotionApiError::Api {
            status: 400,
            message: "nothing to append: text is empty".to_string(),
        });
    }
    let count = children.len();
    let body = json!({ "children": children });

    notion_request(
        token,
        reqwest::Method::PATCH,
        &format!("{NOTION_BASE}/blocks/{page_id}/children"),
        &[],
        Some(&body),
    )
    .await?;

    Ok(NotionAppendResult {
        page_id: page_id.to_string(),
        appended_blocks: count,
    })
}

// ── Pure helpers (unit-tested) ──────────────────────────────────────────────

fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    n.max(lo).min(hi)
}

/// Join the `plain_text` of a Notion `rich_text` array.
fn rich_text_plain(rich: &Value) -> String {
    rich.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|rt| rt.get("plain_text").and_then(|v| v.as_str()))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Extract a human title from a page or database object.
///
/// - Database objects carry a top-level `title` rich_text array.
/// - Page objects store the title inside the property whose `type == "title"`.
fn extract_notion_title(obj: &Value) -> String {
    // Database: top-level title array.
    if let Some(t) = obj.get("title") {
        let s = rich_text_plain(t);
        if !s.is_empty() {
            return s;
        }
    }
    // Page: find the title-typed property.
    if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
        for (_name, prop) in props {
            let is_title = prop.get("type").and_then(|v| v.as_str()) == Some("title");
            if is_title {
                if let Some(arr) = prop.get("title") {
                    let s = rich_text_plain(arr);
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    "(untitled)".to_string()
}

fn parse_search_hit(obj: &Value) -> NotionSearchHit {
    NotionSearchHit {
        id: obj.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        title: extract_notion_title(obj),
        object_type: obj
            .get("object")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        last_edited: obj
            .get("last_edited_time")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        url: obj.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    }
}

/// Convert a single Notion block into a plain-text line. Returns `None` for
/// block types that carry no textual content (dividers, images, …).
fn block_to_text(block: &Value) -> Option<String> {
    let btype = block.get("type").and_then(|v| v.as_str())?;
    let inner = block.get(btype)?;

    // The common text-bearing blocks all keep their content under `rich_text`.
    let rich = inner.get("rich_text");
    let base = rich.map(rich_text_plain).unwrap_or_default();

    let line = match btype {
        "paragraph" => base,
        "heading_1" => format!("# {base}"),
        "heading_2" => format!("## {base}"),
        "heading_3" => format!("### {base}"),
        "bulleted_list_item" => format!("- {base}"),
        "numbered_list_item" => format!("1. {base}"),
        "to_do" => {
            let checked = inner.get("checked").and_then(|v| v.as_bool()).unwrap_or(false);
            format!("[{}] {base}", if checked { "x" } else { " " })
        }
        "quote" => format!("> {base}"),
        "callout" => format!("💡 {base}"),
        "code" => {
            let lang = inner.get("language").and_then(|v| v.as_str()).unwrap_or("");
            format!("```{lang}\n{base}\n```")
        }
        "table_row" => {
            // cells: array of arrays of rich_text.
            let cells = inner
                .get("cells")
                .and_then(|c| c.as_array())
                .map(|rows| {
                    rows.iter()
                        .map(|cell| rich_text_plain(cell))
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default();
            format!("| {cells} |")
        }
        // Unknown / non-text block that still happens to have rich_text.
        _ => {
            if base.is_empty() {
                return None;
            }
            base
        }
    };

    // A structural block (e.g. an empty paragraph) yields an empty base; keep
    // headings/list markers, drop truly empty paragraphs.
    if line.trim().is_empty() {
        None
    } else {
        Some(line)
    }
}

/// Turn free text into Notion paragraph blocks — one block per non-empty line.
fn text_to_paragraph_blocks(text: &str) -> Vec<Value> {
    text.lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            json!({
                "object": "block",
                "type": "paragraph",
                "paragraph": {
                    "rich_text": [
                        { "type": "text", "text": { "content": line } }
                    ]
                }
            })
        })
        .collect()
}

/// Pull a human-readable message out of a Notion error JSON body.
fn extract_api_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| body.to_string())
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
    }

    #[test]
    fn rich_text_join() {
        let rt = json!([
            {"plain_text": "Hello "},
            {"plain_text": "世界"},
        ]);
        assert_eq!(rich_text_plain(&rt), "Hello 世界");
        assert_eq!(rich_text_plain(&json!(null)), "");
    }

    #[test]
    fn title_from_database_object() {
        let db = json!({
            "object": "database",
            "title": [{"plain_text": "My DB"}]
        });
        assert_eq!(extract_notion_title(&db), "My DB");
    }

    #[test]
    fn title_from_page_property() {
        let page = json!({
            "object": "page",
            "properties": {
                "Name": {
                    "type": "title",
                    "title": [{"plain_text": "會議記錄"}]
                },
                "Tags": { "type": "multi_select", "multi_select": [] }
            }
        });
        assert_eq!(extract_notion_title(&page), "會議記錄");
    }

    #[test]
    fn title_falls_back_to_untitled() {
        let page = json!({ "object": "page", "properties": {} });
        assert_eq!(extract_notion_title(&page), "(untitled)");
    }

    #[test]
    fn block_conversions_cover_common_types() {
        let para = json!({"type": "paragraph", "paragraph": {"rich_text": [{"plain_text": "hi"}]}});
        assert_eq!(block_to_text(&para).as_deref(), Some("hi"));

        let h1 = json!({"type": "heading_1", "heading_1": {"rich_text": [{"plain_text": "Title"}]}});
        assert_eq!(block_to_text(&h1).as_deref(), Some("# Title"));

        let bullet = json!({"type": "bulleted_list_item", "bulleted_list_item": {"rich_text": [{"plain_text": "point"}]}});
        assert_eq!(block_to_text(&bullet).as_deref(), Some("- point"));

        let todo_checked = json!({"type": "to_do", "to_do": {"rich_text": [{"plain_text": "done"}], "checked": true}});
        assert_eq!(block_to_text(&todo_checked).as_deref(), Some("[x] done"));

        let todo_open = json!({"type": "to_do", "to_do": {"rich_text": [{"plain_text": "todo"}], "checked": false}});
        assert_eq!(block_to_text(&todo_open).as_deref(), Some("[ ] todo"));

        let quote = json!({"type": "quote", "quote": {"rich_text": [{"plain_text": "q"}]}});
        assert_eq!(block_to_text(&quote).as_deref(), Some("> q"));

        let code = json!({"type": "code", "code": {"rich_text": [{"plain_text": "let x=1;"}], "language": "rust"}});
        assert_eq!(block_to_text(&code).as_deref(), Some("```rust\nlet x=1;\n```"));

        let row = json!({"type": "table_row", "table_row": {"cells": [[{"plain_text": "a"}], [{"plain_text": "b"}]]}});
        assert_eq!(block_to_text(&row).as_deref(), Some("| a | b |"));
    }

    #[test]
    fn empty_paragraph_and_divider_dropped() {
        let empty = json!({"type": "paragraph", "paragraph": {"rich_text": []}});
        assert_eq!(block_to_text(&empty), None);
        let divider = json!({"type": "divider", "divider": {}});
        assert_eq!(block_to_text(&divider), None);
    }

    #[test]
    fn paragraph_blocks_from_multiline_text() {
        let blocks = text_to_paragraph_blocks("line one\n\n  \nline two");
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0]["paragraph"]["rich_text"][0]["text"]["content"],
            "line one"
        );
        assert_eq!(
            blocks[1]["paragraph"]["rich_text"][0]["text"]["content"],
            "line two"
        );
    }

    #[test]
    fn empty_text_yields_no_blocks() {
        assert!(text_to_paragraph_blocks("   \n\n").is_empty());
    }

    #[test]
    fn search_hit_parsing() {
        let hit = json!({
            "object": "page",
            "id": "abc-123",
            "url": "https://notion.so/abc-123",
            "last_edited_time": "2026-07-26T00:00:00.000Z",
            "properties": {
                "Name": {"type": "title", "title": [{"plain_text": "Doc"}]}
            }
        });
        let h = parse_search_hit(&hit);
        assert_eq!(h.id, "abc-123");
        assert_eq!(h.title, "Doc");
        assert_eq!(h.object_type, "page");
        assert_eq!(h.url, "https://notion.so/abc-123");
    }

    #[test]
    fn api_message_extraction() {
        let body = r#"{"object":"error","status":404,"code":"object_not_found","message":"Could not find page"}"#;
        assert_eq!(extract_api_message(body), "Could not find page");
        assert_eq!(extract_api_message("plain"), "plain");
    }
}
