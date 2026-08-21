// WP-S5b2-F — data model + pure parsing/formatting for the "檔案" page
// (`screens/files.rs`). Split out for the same file-size reason
// `goals_data.rs`/`tasks_data.rs`/`agents_data.rs` are split from their
// sibling UI files (see any of those modules' own header comment for the
// precedent) — no behavior differs from an unsplit version.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/*`, never
// guessed) ────────────────────────────────────────────────────────────────
//   `GET /api/files?agent=<id>&q=<text>` (`server.rs::handle_files_list`
//   ~L3471; dashboard-file-panel WP1.4 + I-2b/I-4 extensions) →
//   `{"files": [{"name","size","mtime","origin"?,"task_id"?,"round"?,
//   "display_name"?}]}`. `agent` omitted ⇒ the shared `<home>/attachments/`
//   bucket; `q` searches archived name / display name / origin
//   server-side. There is NO WS-RPC equivalent (`handlers.rs`'s dispatch
//   match has no `files.*` arm) — this is the one page in this crate that
//   talks to a REST route for its data instead of a `Command::Call` RPC,
//   via the new `Command::RestGet` bridge (`ws_status.rs`, WP-S5b2-F).
//   `GET /api/files/download`/`GET /api/files/preview` (same file, ~L3519/
//   ~L3605) stream a single file — opened via `cx.open_url`, the same
//   pattern `screens::device_backup::backup_download_url` already
//   established for `/api/device/backups/download` (S5b1-A).
//
// ── Honest deviations from the design canvas (`Files.dc.html`) ──────────
// 1. The canvas's left "位置" column groups a lone "共用檔案" row under a
//    "依任務" section header — inconsistent even within the canvas itself
//    (a task-grouped section holding a non-task bucket). This page instead
//    puts 全部檔案/依 AI 員工/共用檔案 in one flat bookmark list and moves
//    the REAL task filter into the header's own pill row (matching `web/
//    src/pages/FilesPage.tsx`'s task `Select`, which is genuinely
//    client-side derived from whichever rows are already loaded — see
//    `task_options` below) rather than reproducing the canvas's
//    inconsistent grouping.
// 2. "全部檔案" has no server-side aggregate endpoint (`/api/files` only
//    ever serves ONE bucket per call: one agent, or the shared dir). This
//    bookmark is served by fanning out a REAL `GET /api/files` per visible
//    agent PLUS the shared bucket, in parallel, merged by mtime desc
//    (`merge_rows`) — genuine data, not a fabricated union. A bucket this
//    caller lacks access to (403) contributes zero rows silently rather
//    than failing the whole aggregate (see `screens::files::fetch_all`).
// 3. "日期範圍" is a static, non-interactive pill (no date-picker widget
//    exists anywhere in this crate yet — building one is out of this
//    batch's scope). Same "assembled, not wired" convention `screens::mcp`
//    applies to its two header buttons.
// 4. "list/icon 檢視切換" IS live — both render the same `Vec<FileRow>`,
//    just laid out differently (`screens::files::render_list`/
//    `render_icon_grid`).

use serde_json::Value;

// ── Row model ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    pub name: String,
    pub display_name: Option<String>,
    pub size: u64,
    /// Unix epoch milliseconds — `0` when the server omitted it (never
    /// invented; renders as "—").
    pub mtime: i64,
    /// I-2b provenance: `"declared"|"swept"|"produced"|"uploaded"|"unknown"`
    /// — `None` when the ledger has no row at all for this file (rendered
    /// the same as `"unknown"`, matching `web/src/pages/FilesPage.tsx`'s
    /// `originLabel`).
    pub origin: Option<String>,
    pub task_id: Option<String>,
    /// Which bucket this row was fetched from — `None` for a single-bucket
    /// fetch (an agent id or the shared bucket), `Some(agent_id)`/`Some("")`
    /// (shared) when merged from the "全部檔案" aggregate, so a caller can
    /// still show provenance across the merge if it ever wants to.
    pub source_agent: Option<String>,
}

pub fn parse_files_response(v: &Value, source_agent: Option<&str>) -> Vec<FileRow> {
    v.get("files")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let name = f.get("name")?.as_str()?.to_string();
                    let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
                    let mtime = f.get("mtime").and_then(Value::as_i64).unwrap_or(0);
                    let origin = f.get("origin").and_then(Value::as_str).map(str::to_string);
                    let task_id = f.get("task_id").and_then(Value::as_str).map(str::to_string);
                    let display_name = f.get("display_name").and_then(Value::as_str).map(str::to_string);
                    Some(FileRow {
                        name,
                        display_name,
                        size,
                        mtime,
                        origin,
                        task_id,
                        source_agent: source_agent.map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Merge N per-bucket fetches (the "全部檔案" aggregate) — newest mtime
/// first, matching `runs.list`'s own newest-first convention. Duplicate
/// `name` across buckets (unlikely — each bucket is a distinct directory —
/// but not impossible if a name collides) are kept as separate rows: they
/// really are separate files on disk, not the same row seen twice.
pub fn merge_rows(mut buckets: Vec<Vec<FileRow>>) -> Vec<FileRow> {
    let mut out: Vec<FileRow> = buckets.drain(..).flatten().collect();
    out.sort_by_key(|f| std::cmp::Reverse(f.mtime));
    out
}

/// Human-readable byte size (1 KB = 1024 B) — port of `web/src/pages/
/// FilesPage.tsx`'s `formatSize`.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = bytes as f64 / 1024.0;
    let mut i = 0;
    while value >= 1024.0 && i < UNITS.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    if value >= 100.0 {
        format!("{} {}", value.round() as u64, UNITS[i])
    } else {
        format!("{:.1} {}", value, UNITS[i])
    }
}

/// PDFs and images preview natively — port of `web/src/pages/FilesPage.tsx`'s
/// `isPreviewable`.
pub fn is_previewable(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["pdf", "png", "jpg", "jpeg", "gif", "webp"].iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Office types preview via the gateway's LibreOffice→PDF conversion — port
/// of `isOfficePreviewable`.
pub fn is_office_previewable(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "csv"]
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// i18n key suffix for the origin badge — `files.origin.<suffix>`, mirrors
/// `web/src/pages/FilesPage.tsx`'s `originLabel` (a file the ledger never
/// saw is honestly "unknown", not a guess).
pub fn origin_key_suffix(origin: Option<&str>) -> &'static str {
    match origin {
        Some("declared") => "declared",
        Some("swept") => "swept",
        Some("produced") => "produced",
        Some("uploaded") => "uploaded",
        _ => "unknown",
    }
}

// ── Bookmarks / view mode / task filter ───────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileBookmark {
    All,
    Agent(String),
    Shared,
}

impl FileBookmark {
    /// Stable string key for dedupe/comparison (`FilesState::last_fetch_key`)
    /// — cheaper than deriving `Hash` for a three-variant enum used in one
    /// place.
    pub fn key(&self) -> String {
        match self {
            FileBookmark::All => "all".to_string(),
            FileBookmark::Agent(id) => format!("agent:{id}"),
            FileBookmark::Shared => "shared".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileViewMode {
    List,
    Icon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskFilter {
    All,
    NoTask,
    Task(String),
}

/// The tasks actually represented in the currently-loaded rows — built from
/// the rows themselves (never offers a task filter with nothing behind it),
/// port of `web/src/pages/FilesPage.tsx`'s `taskOptions`.
pub fn task_options(rows: &[FileRow]) -> Vec<String> {
    let mut ids: Vec<String> = rows.iter().filter_map(|f| f.task_id.clone()).collect();
    ids.sort();
    ids.dedup();
    ids
}

pub fn apply_task_filter<'a>(rows: &'a [FileRow], filter: &TaskFilter) -> Vec<&'a FileRow> {
    match filter {
        TaskFilter::All => rows.iter().collect(),
        TaskFilter::NoTask => rows.iter().filter(|f| f.task_id.is_none()).collect(),
        TaskFilter::Task(id) => rows.iter().filter(|f| f.task_id.as_deref() == Some(id.as_str())).collect(),
    }
}

// ── Query-string building ─────────────────────────────────────────────

/// Minimal percent-encoding for a query VALUE — this crate has no
/// `url`/`percent-encoding` dependency (a full RFC 3986 encoder is
/// overkill for the handful of ASCII-ish agent-id/search-text values this
/// page ever sends), so unreserved ASCII passes through verbatim and
/// everything else (space, `&`, `=`, `#`, `%`, non-ASCII UTF-8 bytes) is
/// percent-escaped byte-by-byte.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Builds `/api/files?agent=<id>&q=<text>` for one bucket. `agent = None`
/// means the shared bucket (the REST route's own convention — omitting the
/// param, not sending an empty string).
pub fn build_files_query(agent: Option<&str>, q: &str) -> String {
    let mut parts = Vec::new();
    if let Some(a) = agent.filter(|a| !a.is_empty()) {
        parts.push(format!("agent={}", url_encode(a)));
    }
    let q = q.trim();
    if !q.is_empty() {
        parts.push(format!("q={}", url_encode(q)));
    }
    if parts.is_empty() {
        "/api/files".to_string()
    } else {
        format!("/api/files?{}", parts.join("&"))
    }
}

/// Builds the `/api/files/download` or `/api/files/preview` URL —
/// `base_url` is `crate::api::GATEWAY_BASE_URL`, `token` is `state.jwt`
/// (same "query-param token for a plain `cx.open_url` link" pattern
/// `screens::device_backup::backup_download_url` already established).
pub fn file_action_url(base_url: &str, path: &str, agent: Option<&str>, name: &str, token: Option<&str>) -> String {
    let mut parts = vec![format!("name={}", url_encode(name))];
    if let Some(a) = agent.filter(|a| !a.is_empty()) {
        parts.push(format!("agent={}", url_encode(a)));
    }
    if let Some(t) = token {
        parts.push(format!("token={}", url_encode(t)));
    }
    format!("{base_url}{path}?{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_files_response_reads_every_field() {
        let v = json!({ "files": [
            { "name": "a.pdf", "size": 2048, "mtime": 1_700_000_000_000i64, "origin": "declared",
              "task_id": "task-1", "display_name": "報告.pdf" },
            { "name": "b.png", "size": 512, "mtime": 1_699_999_999_000i64 },
        ]});
        let rows = parse_files_response(&v, Some("duduclaw"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].display_name.as_deref(), Some("報告.pdf"));
        assert_eq!(rows[0].origin.as_deref(), Some("declared"));
        assert_eq!(rows[0].source_agent.as_deref(), Some("duduclaw"));
        assert_eq!(rows[1].origin, None);
        assert_eq!(rows[1].task_id, None);
    }

    #[test]
    fn parse_files_response_missing_array_is_empty_not_panicking() {
        assert!(parse_files_response(&json!({}), None).is_empty());
        assert!(parse_files_response(&json!(null), None).is_empty());
    }

    #[test]
    fn parse_files_response_skips_entries_without_name() {
        let v = json!({ "files": [{ "size": 1 }] });
        assert!(parse_files_response(&v, None).is_empty());
    }

    #[test]
    fn merge_rows_sorts_newest_first_across_buckets() {
        let a = vec![FileRow {
            name: "old.txt".into(), display_name: None, size: 1, mtime: 100, origin: None, task_id: None,
            source_agent: Some("a".into()),
        }];
        let b = vec![FileRow {
            name: "new.txt".into(), display_name: None, size: 1, mtime: 200, origin: None, task_id: None,
            source_agent: Some("b".into()),
        }];
        let merged = merge_rows(vec![a, b]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "new.txt");
        assert_eq!(merged[1].name, "old.txt");
    }

    #[test]
    fn format_size_matches_web_thresholds() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(2150), "2.1 KB");
        assert_eq!(format_size(150 * 1024), "150 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn previewable_extensions_match_web() {
        assert!(is_previewable("cat.PNG"));
        assert!(is_previewable("report.pdf"));
        assert!(!is_previewable("archive.zip"));
        assert!(is_office_previewable("quote.xlsx"));
        assert!(is_office_previewable("data.csv"));
        assert!(!is_office_previewable("cat.png"));
    }

    #[test]
    fn origin_key_suffix_unknown_covers_none_and_unrecognized() {
        assert_eq!(origin_key_suffix(None), "unknown");
        assert_eq!(origin_key_suffix(Some("bogus")), "unknown");
        assert_eq!(origin_key_suffix(Some("uploaded")), "uploaded");
    }

    #[test]
    fn task_options_are_sorted_deduped_and_derived_from_rows() {
        let rows = vec![
            FileRow { name: "a".into(), display_name: None, size: 0, mtime: 0, origin: None, task_id: Some("t2".into()), source_agent: None },
            FileRow { name: "b".into(), display_name: None, size: 0, mtime: 0, origin: None, task_id: Some("t1".into()), source_agent: None },
            FileRow { name: "c".into(), display_name: None, size: 0, mtime: 0, origin: None, task_id: Some("t1".into()), source_agent: None },
            FileRow { name: "d".into(), display_name: None, size: 0, mtime: 0, origin: None, task_id: None, source_agent: None },
        ];
        assert_eq!(task_options(&rows), vec!["t1".to_string(), "t2".to_string()]);
    }

    #[test]
    fn apply_task_filter_variants() {
        let rows = vec![
            FileRow { name: "a".into(), display_name: None, size: 0, mtime: 0, origin: None, task_id: Some("t1".into()), source_agent: None },
            FileRow { name: "b".into(), display_name: None, size: 0, mtime: 0, origin: None, task_id: None, source_agent: None },
        ];
        assert_eq!(apply_task_filter(&rows, &TaskFilter::All).len(), 2);
        assert_eq!(apply_task_filter(&rows, &TaskFilter::NoTask).len(), 1);
        assert_eq!(apply_task_filter(&rows, &TaskFilter::Task("t1".into())).len(), 1);
        assert_eq!(apply_task_filter(&rows, &TaskFilter::Task("missing".into())).len(), 0);
    }

    #[test]
    fn url_encode_escapes_reserved_and_non_ascii() {
        assert_eq!(url_encode("hello-world_1.txt"), "hello-world_1.txt");
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("報告"), "%E5%A0%B1%E5%91%8A");
    }

    #[test]
    fn build_files_query_shapes() {
        assert_eq!(build_files_query(None, ""), "/api/files");
        assert_eq!(build_files_query(Some("duduclaw"), ""), "/api/files?agent=duduclaw");
        assert_eq!(build_files_query(None, "月報"), "/api/files?q=%E6%9C%88%E5%A0%B1");
        assert_eq!(
            build_files_query(Some("sales-bot"), "hi"),
            "/api/files?agent=sales-bot&q=hi"
        );
    }

    #[test]
    fn file_action_url_includes_agent_and_token_when_present() {
        let url = file_action_url("http://127.0.0.1:18789", "/api/files/download", Some("duduclaw"), "a.pdf", Some("tok"));
        assert_eq!(url, "http://127.0.0.1:18789/api/files/download?name=a.pdf&agent=duduclaw&token=tok");
        let shared = file_action_url("http://127.0.0.1:18789", "/api/files/download", None, "a.pdf", None);
        assert_eq!(shared, "http://127.0.0.1:18789/api/files/download?name=a.pdf");
    }

    #[test]
    fn file_bookmark_keys_are_distinct() {
        assert_ne!(FileBookmark::All.key(), FileBookmark::Shared.key());
        assert_ne!(FileBookmark::Agent("a".into()).key(), FileBookmark::Agent("b".into()).key());
    }
}
