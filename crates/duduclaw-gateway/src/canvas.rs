//! G15 Live Canvas — an agent-pushed HTML visual workspace the user views
//! live in the dashboard (OpenClaw's Canvas, TODO doc §1.3 G15).
//!
//! ## Security posture (this is an XSS-adjacent surface)
//!
//! The canvas HTML is authored by an agent (i.e. by an LLM, i.e. potentially
//! by whatever prompt-injected the LLM) and rendered in the operator's
//! dashboard. Defense is layered and **fail-closed**:
//!
//! 1. **Server-side sanitization at WRITE time** ([`sanitize_canvas_html`],
//!    `ammonia` allowlist): every byte that reaches `canvas.db` has already
//!    been cleaned — scripts, event handlers, iframes/objects/embeds, form
//!    elements and non-https/data URLs never get stored. A sanitizer rejection
//!    (oversize, empty result, post-clean overflow) rejects the whole push.
//! 2. **Client-side sandboxed rendering**: the dashboard renders the stored
//!    HTML inside `<iframe srcdoc sandbox="">` — no `allow-scripts`, no
//!    `allow-same-origin` — so even a sanitizer bypass executes nothing and
//!    reads nothing (opaque origin).
//!
//! ## Storage
//!
//! One SQLite DB at `<home>/canvas.db` (WAL, 0600). Rows are append-only per
//! push; the newest row per agent is the "current" canvas and the last
//! [`HISTORY_KEEP`] rows are retained as history (older rows are trimmed on
//! push). `canvas_clear` appends an empty tombstone row so "cleared" is itself
//! a version and the seq watermark stays monotonic — which is what lets
//! [`ensure_broadcast_bridge`] detect clears with the same `MAX(seq)` poll.
//!
//! ## Live updates
//!
//! `canvas_push` runs in the MCP subprocess, not the gateway, so it cannot
//! reach the dashboard WebSocket broadcast channel directly. Instead the
//! gateway lazily spawns a tiny poll task ([`ensure_broadcast_bridge`], first
//! `canvas.get` call) that watches `MAX(seq)` (one indexed scalar query every
//! 2s) and broadcasts `canvas.updated { agent_id, seq }` frames — following
//! the `plan.updated` pattern — whenever new rows land. Open viewers refetch
//! on that event; a 30s client poll is the fallback when the WS is down.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Hard cap on a single canvas push (raw bytes, pre-sanitization). Also
/// enforced post-sanitization: entity-escaping can only grow the document, and
/// stored bytes must stay bounded too.
pub const MAX_CANVAS_BYTES: usize = 256 * 1024;

/// How many versions (including the current one) are kept per agent.
pub const HISTORY_KEEP: usize = 5;

/// Max canvas title length in Unicode codepoints (CJK-safe, never byte-sliced).
pub const MAX_TITLE_CHARS: usize = 200;

/// One stored canvas version. `seq` is the SQLite AUTOINCREMENT rowid —
/// globally monotonic, never reused, so it doubles as the broadcast watermark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasRow {
    pub seq: i64,
    pub agent_id: String,
    pub title: String,
    /// Sanitized HTML. Empty string ⇒ cleared canvas (tombstone version).
    pub html: String,
    pub updated_at: String,
}

/// History metadata (no HTML body — versions are up to 256 KB each, and the
/// list endpoint must not ship `HISTORY_KEEP × 256 KB` per call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasVersionMeta {
    pub seq: i64,
    pub title: String,
    pub updated_at: String,
    /// Sanitized HTML size in bytes (0 ⇒ cleared tombstone).
    pub bytes: usize,
}

// ─── Sanitizer ───────────────────────────────────────────────────

/// Sanitize agent-supplied canvas HTML with a conservative `ammonia`
/// allowlist. Returns the cleaned HTML or an error (fail-closed: any error
/// here must reject the push — callers never store the raw input).
///
/// Profile:
/// - **Allowed tags**: structural / formatting HTML (headings, lists, tables,
///   `div`/`span`/`section`/…, `pre`/`code`, `details`/`summary`, `a`, `img`)
///   plus a static inline-SVG subset (shapes, paths, text, gradients — no
///   `use`, no `foreignObject`, no `script`/`animate` family).
/// - **Attributes**: `style`/`class`/`id`/`title`/`dir`/`lang` generics,
///   `aria-*`/`data-*` prefixes, table spans, and SVG presentation/geometry
///   attributes. Everything else — including every `on*` event handler — is
///   dropped by the allowlist.
/// - **URLs**: `a[href]` must be absolute http(s) (rel is force-stamped
///   `noopener noreferrer nofollow`); `img[src]` must be absolute https or a
///   `data:image/*` payload; relative URLs are denied outright.
/// - **Stripped with content**: `<script>` and `<style>` elements (ammonia
///   `clean_content_tags` default). `<style>` was considered and rejected:
///   ammonia HTML-escapes text content (breaking CSS like `a > b`), and
///   stylesheet selectors add exfiltration surface — inline `style`
///   attributes cover the styling need instead.
/// - **Never allowed**: `iframe` / `object` / `embed` / `form` family /
///   `link` / `meta` / `base` — not in the allowlist, so removed.
pub fn sanitize_canvas_html(raw: &str) -> Result<String, String> {
    if raw.len() > MAX_CANVAS_BYTES {
        return Err(format!(
            "canvas HTML too large: {} bytes (max {} KB)",
            raw.len(),
            MAX_CANVAS_BYTES / 1024
        ));
    }
    if raw.trim().is_empty() {
        return Err("canvas HTML is empty".to_string());
    }

    let cleaned = canvas_builder().clean(raw).to_string();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        // e.g. the input was a single <script> block. Storing "" would be
        // indistinguishable from a deliberate clear — reject instead so the
        // agent gets an actionable error.
        return Err(
            "canvas HTML is empty after sanitization (only disallowed markup was supplied)"
                .to_string(),
        );
    }
    if trimmed.len() > MAX_CANVAS_BYTES {
        // Entity escaping can grow the document past the raw-size gate.
        return Err(format!(
            "canvas HTML too large after sanitization: {} bytes (max {} KB)",
            trimmed.len(),
            MAX_CANVAS_BYTES / 1024
        ));
    }
    Ok(trimmed.to_string())
}

/// Build the canvas `ammonia` profile (see [`sanitize_canvas_html`] docs).
fn canvas_builder() -> ammonia::Builder<'static> {
    use std::collections::{HashMap, HashSet};

    const TAGS: &[&str] = &[
        // Structure / sections
        "div", "span", "p", "br", "hr", "section", "article", "header", "footer", "main",
        "aside", "nav", "figure", "figcaption", "details", "summary", "blockquote",
        // Headings
        "h1", "h2", "h3", "h4", "h5", "h6",
        // Lists
        "ul", "ol", "li", "dl", "dt", "dd",
        // Inline formatting
        "strong", "em", "b", "i", "u", "s", "small", "sup", "sub", "mark", "abbr", "cite",
        "q", "time", "wbr", "kbd", "samp", "var", "code", "pre",
        // Links / media
        "a", "img",
        // Tables
        "table", "caption", "thead", "tbody", "tfoot", "tr", "th", "td", "colgroup", "col",
        // Inline SVG (static subset — no use/foreignObject/script/animate)
        "svg", "g", "path", "circle", "ellipse", "rect", "line", "polyline", "polygon",
        "text", "tspan", "defs", "linearGradient", "radialGradient", "stop", "desc",
    ];

    // SVG presentation + geometry attributes are allowed generically: they are
    // inert on HTML elements (no CSS/scripting can weaponize them here) and
    // per-tag enumeration across 18 SVG tags adds noise, not safety. Both the
    // spec camelCase and lowercase forms are listed — html5ever adjusts case
    // in foreign content, and being case-generous on an inert allowlist is
    // safer than silently dropping `viewBox`.
    const GENERIC_ATTRS: &[&str] = &[
        // HTML generics
        "style", "class", "id", "title", "dir", "lang", "role",
        // SVG geometry
        "d", "cx", "cy", "r", "rx", "ry", "x", "y", "x1", "y1", "x2", "y2", "dx", "dy",
        "width", "height", "points", "offset", "transform", "viewbox", "viewBox",
        "preserveaspectratio", "preserveAspectRatio", "pathlength", "pathLength", "xmlns",
        // SVG presentation
        "fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin",
        "stroke-dasharray", "stroke-dashoffset", "stroke-opacity", "fill-opacity",
        "fill-rule", "opacity", "stop-color", "stop-opacity", "gradientunits",
        "gradientUnits", "gradienttransform", "gradientTransform", "text-anchor",
        "dominant-baseline", "font-size", "font-family", "font-weight", "vector-effect",
    ];

    let mut tag_attributes: HashMap<&str, HashSet<&str>> = HashMap::new();
    tag_attributes.insert("a", ["href"].into_iter().collect());
    tag_attributes.insert("img", ["src", "alt", "width", "height"].into_iter().collect());
    tag_attributes.insert("th", ["colspan", "rowspan", "scope"].into_iter().collect());
    tag_attributes.insert("td", ["colspan", "rowspan"].into_iter().collect());
    tag_attributes.insert("ol", ["start", "type", "reversed"].into_iter().collect());
    tag_attributes.insert("li", ["value"].into_iter().collect());
    tag_attributes.insert("col", ["span"].into_iter().collect());
    tag_attributes.insert("colgroup", ["span"].into_iter().collect());
    tag_attributes.insert("time", ["datetime"].into_iter().collect());
    tag_attributes.insert("details", ["open"].into_iter().collect());

    let mut builder = ammonia::Builder::default();
    builder
        .tags(TAGS.iter().copied().collect())
        .tag_attributes(tag_attributes)
        .generic_attributes(GENERIC_ATTRS.iter().copied().collect())
        .generic_attribute_prefixes(["aria-", "data-"].into_iter().collect())
        .url_schemes(["http", "https", "data"].into_iter().collect())
        // Relative URLs would resolve against the dashboard origin inside the
        // srcdoc frame — deny them; the canvas must be self-contained.
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer nofollow"))
        .strip_comments(true)
        // Per-attribute URL policy that scheme allowlisting alone can't
        // express. Returning None drops the attribute (tag survives).
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("a", "href") => {
                let lower = value.to_ascii_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") {
                    Some(std::borrow::Cow::Borrowed(value))
                } else {
                    None
                }
            }
            ("img", "src") => {
                let lower = value.to_ascii_lowercase();
                if lower.starts_with("https://") || lower.starts_with("data:image/") {
                    Some(std::borrow::Cow::Borrowed(value))
                } else {
                    None
                }
            }
            _ => Some(std::borrow::Cow::Borrowed(value)),
        });
    builder
}

// ─── Store ───────────────────────────────────────────────────────

/// Thread-safe SQLite canvas store at `<home>/canvas.db`.
///
/// Writers: the MCP subprocess (`canvas_push` / `canvas_clear`). Readers: the
/// gateway (`canvas.get` RPC + broadcast bridge). WAL + busy_timeout make the
/// cross-process mix safe, same as `events_store::EventBusStore`.
pub struct CanvasStore {
    conn: tokio::sync::Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl CanvasStore {
    /// Open (or create) the canvas store at `<home>/canvas.db` (0600).
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("canvas.db");
        let existed = db_path.exists();
        let conn = Connection::open(&db_path).map_err(|e| format!("open canvas.db: {e}"))?;
        Self::init_schema(&conn)?;
        // Owner-only: canvas bodies are agent output and may quote user data.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))
            {
                warn!(error = %e, "canvas.db: failed to set 0600 permissions");
            }
        }
        if !existed {
            info!(?db_path, "CanvasStore initialized");
        }
        Ok(Self { conn: tokio::sync::Mutex::new(conn), db_path })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS canvas (
                 seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                 agent_id   TEXT NOT NULL,
                 title      TEXT NOT NULL DEFAULT '',
                 html       TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_canvas_agent_seq
                 ON canvas(agent_id, seq DESC);",
        )
        .map_err(|e| format!("init canvas schema: {e}"))?;
        Ok(())
    }

    /// Sanitize + store a new canvas version for `agent_id`, trimming history
    /// beyond [`HISTORY_KEEP`]. Fail-closed: a sanitizer rejection stores
    /// nothing. Returns the stored row (sanitized HTML, assigned seq).
    pub async fn push(
        &self,
        agent_id: &str,
        title: &str,
        raw_html: &str,
    ) -> Result<CanvasRow, String> {
        // WRITE-time sanitization — the only path by which HTML enters the DB.
        let html = sanitize_canvas_html(raw_html)?;
        let title = duduclaw_core::truncate_chars(title.trim(), MAX_TITLE_CHARS);
        self.insert_version(agent_id, &title, &html).await
    }

    /// Clear the agent's canvas by appending an empty tombstone version.
    /// (Appending — not deleting — keeps `MAX(seq)` monotonic so the broadcast
    /// bridge and dashboard viewers observe the clear like any other update.)
    pub async fn clear(&self, agent_id: &str) -> Result<CanvasRow, String> {
        self.insert_version(agent_id, "", "").await
    }

    async fn insert_version(
        &self,
        agent_id: &str,
        title: &str,
        html: &str,
    ) -> Result<CanvasRow, String> {
        let updated_at = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO canvas (agent_id, title, html, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, title, html, updated_at],
        )
        .map_err(|e| format!("insert canvas: {e}"))?;
        let seq = conn.last_insert_rowid();
        // Trim: keep only the newest HISTORY_KEEP versions for this agent.
        conn.execute(
            "DELETE FROM canvas WHERE agent_id = ?1 AND seq NOT IN (
                 SELECT seq FROM canvas WHERE agent_id = ?1
                 ORDER BY seq DESC LIMIT ?2
             )",
            params![agent_id, HISTORY_KEEP as i64],
        )
        .map_err(|e| format!("trim canvas history: {e}"))?;
        debug!(agent_id, seq, bytes = html.len(), "canvas version stored");
        Ok(CanvasRow {
            seq,
            agent_id: agent_id.to_string(),
            title: title.to_string(),
            html: html.to_string(),
            updated_at,
        })
    }

    /// The newest version for `agent_id` (None ⇒ never pushed).
    pub async fn current(&self, agent_id: &str) -> Result<Option<CanvasRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT seq, agent_id, title, html, updated_at FROM canvas
             WHERE agent_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![agent_id],
            row_to_canvas,
        )
        .optional()
        .map_err(|e| format!("canvas current: {e}"))
    }

    /// One specific retained version (agent-scoped so a caller can never read
    /// another agent's version by guessing seq numbers).
    pub async fn get_version(
        &self,
        agent_id: &str,
        seq: i64,
    ) -> Result<Option<CanvasRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT seq, agent_id, title, html, updated_at FROM canvas
             WHERE agent_id = ?1 AND seq = ?2",
            params![agent_id, seq],
            row_to_canvas,
        )
        .optional()
        .map_err(|e| format!("canvas get_version: {e}"))
    }

    /// Version metadata for `agent_id`, newest first (≤ [`HISTORY_KEEP`] rows,
    /// no HTML bodies).
    pub async fn history(&self, agent_id: &str) -> Result<Vec<CanvasVersionMeta>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT seq, title, updated_at, LENGTH(CAST(html AS BLOB)) FROM canvas
                 WHERE agent_id = ?1 ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(|e| format!("prepare canvas history: {e}"))?;
        let rows = stmt
            .query_map(params![agent_id, HISTORY_KEEP as i64], |r| {
                Ok(CanvasVersionMeta {
                    seq: r.get(0)?,
                    title: r.get(1)?,
                    updated_at: r.get(2)?,
                    bytes: r.get::<_, i64>(3)?.max(0) as usize,
                })
            })
            .map_err(|e| format!("query canvas history: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect canvas history: {e}"))?;
        Ok(rows)
    }

    /// Global `MAX(seq)` watermark (0 when empty) — broadcast-bridge cursor.
    pub async fn max_seq(&self) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM canvas", [], |r| r.get(0))
            .map_err(|e| format!("canvas max_seq: {e}"))
    }

    /// `(seq, agent_id)` pairs newer than `after_seq`, ascending — what the
    /// broadcast bridge fans out as `canvas.updated` events.
    pub async fn versions_after(&self, after_seq: i64) -> Result<Vec<(i64, String)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT seq, agent_id FROM canvas WHERE seq > ?1 ORDER BY seq ASC")
            .map_err(|e| format!("prepare canvas versions_after: {e}"))?;
        let rows = stmt
            .query_map(params![after_seq], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("query canvas versions_after: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect canvas versions_after: {e}"))?;
        Ok(rows)
    }
}

fn row_to_canvas(r: &rusqlite::Row<'_>) -> rusqlite::Result<CanvasRow> {
    Ok(CanvasRow {
        seq: r.get(0)?,
        agent_id: r.get(1)?,
        title: r.get(2)?,
        html: r.get(3)?,
        updated_at: r.get(4)?,
    })
}

// ─── Broadcast bridge ────────────────────────────────────────────

/// Bridge-started latch: one bridge per gateway process, spawned lazily by
/// the first `canvas.get` RPC (i.e. exactly when a viewer opens the page — a
/// gateway whose dashboard never shows a canvas pays zero cost).
static BRIDGE_STARTED: AtomicBool = AtomicBool::new(false);

/// Lazily spawn the canvas → dashboard-WS bridge (idempotent).
///
/// `canvas_push` / `canvas_clear` run in the MCP subprocess, so unlike the
/// in-gateway `plan.updated` broadcasts there is no direct handle to
/// `event_tx` at mutation time. This task polls the `MAX(seq)` watermark
/// (single indexed scalar SELECT, every 2s — same cadence as the events.db
/// poll) and broadcasts one `canvas.updated { agent_id, seq }` frame per new
/// version, deduped per agent within a batch.
pub fn ensure_broadcast_bridge(
    home_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<String>,
) {
    if BRIDGE_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let store = match CanvasStore::open(&home_dir) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "canvas broadcast bridge: open failed; live canvas.updated events disabled (30s client poll still works)");
                return;
            }
        };
        // Seed to the current watermark — never replay history on startup.
        let mut watermark = store.max_seq().await.unwrap_or(0);
        info!(seed_seq = watermark, "canvas broadcast bridge started");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match store.versions_after(watermark).await {
                Ok(rows) if !rows.is_empty() => {
                    // Newest seq per agent within this batch (dedup).
                    let mut latest: std::collections::HashMap<String, i64> =
                        std::collections::HashMap::new();
                    for (seq, agent_id) in &rows {
                        latest.insert(agent_id.clone(), *seq);
                    }
                    for (agent_id, seq) in latest {
                        let frame = crate::protocol::WsFrame::Event {
                            event: "canvas.updated".to_string(),
                            payload: serde_json::json!({ "agent_id": agent_id, "seq": seq }),
                            seq: None,
                            state_version: None,
                        };
                        if let Ok(json) = serde_json::to_string(&frame) {
                            let _ = event_tx.send(json);
                        }
                    }
                    if let Some((seq, _)) = rows.last() {
                        watermark = *seq;
                    }
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "canvas broadcast bridge poll failed"),
            }
        }
    });
}

// ─── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    // ── Sanitizer: hostile input ──

    #[test]
    fn sanitizer_strips_script_but_keeps_benign_markup() {
        let out = sanitize_canvas_html(
            "<h1>報表</h1><script>alert(1)</script><p>本週營收 <strong>NT$12,000</strong></p>",
        )
        .unwrap();
        assert!(!out.contains("<script"), "got: {out}");
        assert!(!out.contains("alert(1)"), "script content must be removed, got: {out}");
        assert!(out.contains("<h1>報表</h1>"), "got: {out}");
        assert!(out.contains("<strong>NT$12,000</strong>"), "got: {out}");
    }

    #[test]
    fn sanitizer_strips_img_onerror_but_keeps_https_img() {
        let out = sanitize_canvas_html(
            r#"<img src="https://example.com/a.png" onerror="alert(1)" alt="chart">"#,
        )
        .unwrap();
        assert!(!out.contains("onerror"), "got: {out}");
        assert!(out.contains(r#"src="https://example.com/a.png""#), "got: {out}");
        assert!(out.contains(r#"alt="chart""#), "got: {out}");
    }

    #[test]
    fn sanitizer_drops_javascript_href_and_stamps_rel() {
        let out = sanitize_canvas_html(
            r#"<a href="javascript:alert(1)">bad</a><a href="https://example.com">ok</a>"#,
        )
        .unwrap();
        assert!(!out.contains("javascript:"), "got: {out}");
        assert!(out.contains(r#"href="https://example.com""#), "got: {out}");
        assert!(out.contains("noopener"), "rel must be stamped, got: {out}");
    }

    #[test]
    fn sanitizer_removes_iframe_object_embed_and_forms() {
        let out = sanitize_canvas_html(
            r#"<p>before</p><iframe src="https://evil"></iframe><object data="x"></object>
               <embed src="x"><form action="https://evil.example/post"><input name="q"></form><p>after</p>"#,
        )
        .unwrap();
        for banned in ["<iframe", "<object", "<embed", "<form", "<input", "action="] {
            assert!(!out.contains(banned), "{banned} must be stripped, got: {out}");
        }
        assert!(out.contains("<p>before</p>") && out.contains("<p>after</p>"), "got: {out}");
    }

    #[test]
    fn sanitizer_strips_event_handlers_everywhere() {
        let out = sanitize_canvas_html(
            r#"<div onclick="x()" onmouseover="y()" style="color:red" class="k">hi</div>"#,
        )
        .unwrap();
        assert!(!out.contains("onclick") && !out.contains("onmouseover"), "got: {out}");
        assert!(out.contains(r#"style="color:red""#), "style attr allowed, got: {out}");
        assert!(out.contains(r#"class="k""#), "got: {out}");
    }

    #[test]
    fn sanitizer_keeps_inline_svg_and_tables() {
        let out = sanitize_canvas_html(
            r##"<table><tr><th colspan="2">Q1</th></tr><tr><td>營收</td><td>100</td></tr></table>
               <svg viewBox="0 0 10 10" width="100"><rect x="1" y="1" width="8" height="8" fill="#f59e0b"/></svg>"##,
        )
        .unwrap();
        assert!(out.contains("<table") && out.contains(r#"colspan="2""#), "got: {out}");
        assert!(out.contains("<svg") && out.contains("<rect"), "got: {out}");
        assert!(out.contains(r##"fill="#f59e0b""##), "got: {out}");
    }

    #[test]
    fn sanitizer_svg_use_and_foreignobject_are_removed() {
        let out = sanitize_canvas_html(
            r#"<svg><use href="https://evil/x.svg#p"/><foreignObject><body onload="x()"></body></foreignObject><circle cx="5" cy="5" r="4"/></svg>"#,
        )
        .unwrap();
        assert!(!out.contains("<use") && !out.to_lowercase().contains("foreignobject"), "got: {out}");
        assert!(!out.contains("onload"), "got: {out}");
        assert!(out.contains("<circle"), "got: {out}");
    }

    #[test]
    fn sanitizer_allows_data_image_and_drops_http_image() {
        let out = sanitize_canvas_html(
            r#"<img src="data:image/png;base64,iVBORw0KGgo="><img src="http://insecure.example/x.png" alt="keepme">"#,
        )
        .unwrap();
        assert!(out.contains("data:image/png;base64"), "got: {out}");
        // http:// (non-TLS) image source is dropped; the tag itself survives.
        assert!(!out.contains("http://insecure.example"), "got: {out}");
        assert!(out.contains(r#"alt="keepme""#), "got: {out}");
    }

    #[test]
    fn sanitizer_drops_data_url_on_links_and_relative_urls() {
        let out = sanitize_canvas_html(
            r#"<a href="data:text/html,<script>x</script>">l</a><img src="/relative.png" alt="r">"#,
        )
        .unwrap();
        assert!(!out.contains("data:text/html"), "got: {out}");
        assert!(!out.contains("/relative.png"), "relative URLs denied, got: {out}");
    }

    #[test]
    fn sanitizer_preserves_cjk_content() {
        let input = "<p>本週任務完成率 87%，剩餘 3 件待辦。繁體中文與 emoji 🐾 應原樣保留。</p>";
        let out = sanitize_canvas_html(input).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn sanitizer_rejects_oversize_input() {
        let big = format!("<p>{}</p>", "字".repeat(MAX_CANVAS_BYTES / 3 + 1));
        let err = sanitize_canvas_html(&big).unwrap_err();
        assert!(err.contains("too large"), "got: {err}");
    }

    #[test]
    fn sanitizer_rejects_empty_and_script_only_input() {
        assert!(sanitize_canvas_html("   ").is_err());
        let err = sanitize_canvas_html("<script>alert(1)</script>").unwrap_err();
        assert!(err.contains("empty after sanitization"), "got: {err}");
    }

    #[test]
    fn sanitizer_strips_style_element_with_content() {
        let out =
            sanitize_canvas_html("<style>body{background:url(https://evil/x)}</style><p>ok</p>")
                .unwrap();
        assert!(!out.contains("<style") && !out.contains("background:url"), "got: {out}");
        assert!(out.contains("<p>ok</p>"), "got: {out}");
    }

    // ── Store: roundtrip / history trim / clear / scoping ──

    #[tokio::test]
    async fn store_push_current_roundtrip() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        let pushed = store
            .push("agnes", "週報", "<h1>週報</h1><p>進度 <em>80%</em></p>")
            .await
            .unwrap();
        assert!(pushed.seq > 0);
        let cur = store.current("agnes").await.unwrap().expect("current");
        assert_eq!(cur.seq, pushed.seq);
        assert_eq!(cur.title, "週報");
        assert!(cur.html.contains("<em>80%</em>"));
        // Unknown agent has no canvas.
        assert!(store.current("nobody").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_push_sanitizes_at_write_time() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        let row = store
            .push("agnes", "t", "<p>ok</p><script>document.location='https://evil'</script>")
            .await
            .unwrap();
        assert!(!row.html.contains("script"), "stored html must be sanitized: {}", row.html);
        let cur = store.current("agnes").await.unwrap().unwrap();
        assert!(!cur.html.contains("script"), "read-back must be sanitized: {}", cur.html);
    }

    #[tokio::test]
    async fn store_push_rejects_oversize_and_stores_nothing() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        let big = format!("<p>{}</p>", "x".repeat(MAX_CANVAS_BYTES + 1));
        assert!(store.push("agnes", "t", &big).await.is_err());
        assert!(store.current("agnes").await.unwrap().is_none(), "fail-closed: no row stored");
    }

    #[tokio::test]
    async fn store_history_trims_to_keep_limit() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        for i in 0..(HISTORY_KEEP + 3) {
            store.push("agnes", &format!("v{i}"), &format!("<p>v{i}</p>")).await.unwrap();
        }
        let hist = store.history("agnes").await.unwrap();
        assert_eq!(hist.len(), HISTORY_KEEP);
        // Newest first; oldest retained is v3 (v0..v2 trimmed).
        assert_eq!(hist[0].title, format!("v{}", HISTORY_KEEP + 2));
        assert_eq!(hist[hist.len() - 1].title, "v3");
        // Trimmed versions are really gone.
        let oldest_seq = hist[hist.len() - 1].seq;
        assert!(store.get_version("agnes", oldest_seq - 1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_clear_appends_tombstone_and_bumps_watermark() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        store.push("agnes", "v1", "<p>v1</p>").await.unwrap();
        let before = store.max_seq().await.unwrap();
        let tomb = store.clear("agnes").await.unwrap();
        assert!(tomb.seq > before, "clear must advance the watermark");
        let cur = store.current("agnes").await.unwrap().unwrap();
        assert_eq!(cur.html, "", "current after clear is the empty tombstone");
        // Previous version is still reachable as history.
        let hist = store.history("agnes").await.unwrap();
        assert_eq!(hist.len(), 2);
        assert!(hist.iter().any(|v| v.title == "v1" && v.bytes > 0));
    }

    #[tokio::test]
    async fn store_get_version_is_agent_scoped() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        let a = store.push("alpha", "a", "<p>alpha</p>").await.unwrap();
        store.push("beta", "b", "<p>beta</p>").await.unwrap();
        // beta cannot fetch alpha's version by guessing its seq.
        assert!(store.get_version("beta", a.seq).await.unwrap().is_none());
        assert!(store.get_version("alpha", a.seq).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn store_versions_after_reports_new_rows_per_agent() {
        let home = tmp_home();
        let store = CanvasStore::open(home.path()).unwrap();
        let mark = store.max_seq().await.unwrap();
        store.push("alpha", "a", "<p>a</p>").await.unwrap();
        store.push("beta", "b", "<p>b</p>").await.unwrap();
        let rows = store.versions_after(mark).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "alpha");
        assert_eq!(rows[1].1, "beta");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn store_db_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let home = tmp_home();
        let _store = CanvasStore::open(home.path()).unwrap();
        let mode = std::fs::metadata(home.path().join("canvas.db")).unwrap().permissions().mode()
            & 0o777;
        assert_eq!(mode, 0o600, "canvas.db must be owner-only, got {mode:o}");
    }
}
