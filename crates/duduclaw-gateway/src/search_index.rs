//! I-5: cross-source content search — the ⌘K backend.
//!
//! Aggregates four surfaces behind one bounded, CJK-safe query: conversation
//! turns (`sessions.db::session_messages`), delivered/received files
//! (`artifacts.jsonl`, the I-2b provenance ledger), agent memory
//! (`SqliteMemoryEngine`), and knowledge pages (agent-local + shared wiki).
//! Every hit is self-describing (`source` + a type-specific `jump` target) so
//! the dashboard can render one merged list and still navigate correctly.
//!
//! ## Module boundary (WP-7C, 2026-08)
//!
//! This module deliberately does not reach into `session.rs` / `artifacts.rs`
//! internals — conversations open a short-lived, independent read-only SQLite
//! connection against the same `sessions.db` file `SessionManager` already
//! writes (WAL mode permits concurrent readers, so this never contends with
//! the live channel-reply path), and artifacts re-reads the `artifacts.jsonl`
//! tail using the exact on-disk shape `artifacts::ArtifactRecord` documents.
//! Memory and wiki need no such duplication: their crates already expose
//! cheap, bounded `search()` calls that this module's callers (in
//! `handlers.rs`) invoke directly and hand the results to the pure converters
//! below.
//!
//! ## Honesty rules
//!
//! - Every source degrades independently: a missing db file, missing
//!   directory, or query error is an EMPTY contribution from that source,
//!   never an error for the whole aggregated query. A fresh install where no
//!   conversation has ever happened must still answer "no results", not 500.
//! - Every source query is capped ([`MAX_PER_SOURCE_LIMIT`]) and every
//!   string field is CJK-safe byte-truncated (`duduclaw_core::truncate_bytes`,
//!   coding convention 1) — one ⌘K keystroke must never be able to pull an
//!   unbounded result set or panic on a multi-byte truncation boundary.

use std::path::Path;

use duduclaw_core::truncate_bytes;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

/// One cross-source search result. `source` names which surface produced it;
/// `jump` carries whatever the dashboard needs to navigate there — shape
/// varies by source (a session id vs. a wiki page path vs. an archived
/// filename), so it stays a loosely-typed JSON object rather than forcing one
/// navigation schema onto four different destinations.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    pub source: &'static str,
    /// Stable-ish identifier within its source (message id, archived name,
    /// memory entry id, wiki page path). Not globally unique across sources.
    pub id: String,
    pub title: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// RFC3339 when the source has a natural timestamp; wiki hits (ranked by
    /// relevance, not recency) leave this `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub jump: serde_json::Value,
}

pub const SOURCE_CONVERSATION: &str = "conversation";
pub const SOURCE_ARTIFACT: &str = "artifact";
pub const SOURCE_MEMORY: &str = "memory";
pub const SOURCE_WIKI: &str = "wiki";
pub const SOURCE_SHARED_WIKI: &str = "shared_wiki";

/// Default per-source row cap when the caller does not ask for a specific
/// limit.
pub const DEFAULT_PER_SOURCE_LIMIT: usize = 8;
/// Hard ceiling on how many rows ANY single source may contribute to one
/// query, regardless of what the caller requests — an unbounded LIKE scan (or
/// a wiki store returning its whole index) must never make one keystroke
/// expensive.
pub const MAX_PER_SOURCE_LIMIT: usize = 20;
/// Hard ceiling on the combined, merged result set across all sources.
pub const MAX_TOTAL_HITS: usize = 40;

/// CJK-safe byte caps (coding convention 1 — never slice by raw byte index).
const SNIPPET_MAX_BYTES: usize = 200;
const TITLE_MAX_BYTES: usize = 120;

/// Bounded tail-read window for `artifacts.jsonl`. Mirrors the order of
/// magnitude `artifacts::TAIL_READ_BYTES` uses for its own queries (that
/// constant is private — see the module-level boundary note — so this is an
/// intentionally independent value, not a re-export).
const ARTIFACTS_TAIL_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Clamp a caller-supplied per-source limit into `[1, MAX_PER_SOURCE_LIMIT]`.
pub fn clamp_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_PER_SOURCE_LIMIT).clamp(1, MAX_PER_SOURCE_LIMIT)
}

/// Escape SQLite LIKE metacharacters so a search term containing `%`/`_`
/// matches literally under an `ESCAPE '\'` clause — the same convention
/// `session.rs::search_hidden_messages` and `gdpr_like_escape` already use.
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Search conversation turns in `sessions.db`.
///
/// Opens a short-lived, read-only connection independent of
/// `SessionManager`'s own writer pool — WAL mode permits concurrent readers,
/// so this never contends with (or blocks behind) the live channel-reply
/// path. `agent_id: None` searches across every agent (admin-only at the RPC
/// layer, enforced by the caller); `Some` scopes to one agent. Hidden
/// (Sculptor hide/restore) and undone (`/rollback`) turns are excluded, and
/// archived sessions are skipped — the same visibility rules
/// `chat.sessions.*` already applies. A missing/corrupt db, or an empty
/// query, is an empty result, never an error.
pub fn search_conversations(
    session_db_path: &Path,
    agent_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Vec<SearchHit> {
    let query = query.trim();
    if query.is_empty() || !session_db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = Connection::open_with_flags(
        session_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let pattern = format!("%{}%", like_escape(query));
    let cap = limit.clamp(1, MAX_PER_SOURCE_LIMIT) as i64;

    const BASE_SQL: &str = "SELECT m.id, m.session_id, s.agent_id, m.role, m.content, m.timestamp
         FROM session_messages m
         JOIN sessions s ON s.id = m.session_id
         WHERE m.hidden = 0 AND m.undone_at IS NULL AND s.archived_at IS NULL
           AND m.content LIKE ?1 ESCAPE '\\'";

    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<SearchHit> {
        let msg_id: i64 = row.get(0)?;
        let session_id: String = row.get(1)?;
        let agent: String = row.get(2)?;
        let role: String = row.get(3)?;
        let content: String = row.get(4)?;
        let ts: String = row.get(5)?;
        let clean = crate::channel_reply::strip_sender_prefix(&content).trim();
        Ok(SearchHit {
            source: SOURCE_CONVERSATION,
            id: format!("{session_id}:{msg_id}"),
            title: truncate_bytes(clean, TITLE_MAX_BYTES).to_string(),
            snippet: truncate_bytes(clean, SNIPPET_MAX_BYTES).to_string(),
            agent_id: Some(agent),
            timestamp: Some(ts),
            jump: serde_json::json!({
                "session_id": session_id,
                "message_id": msg_id,
                "role": role,
            }),
        })
    };

    let rows = match agent_id {
        Some(a) => {
            let sql = format!("{BASE_SQL} AND s.agent_id = ?3 ORDER BY m.id DESC LIMIT ?2");
            let Ok(mut stmt) = conn.prepare(&sql) else {
                return Vec::new();
            };
            let Ok(iter) = stmt.query_map(rusqlite::params![pattern, cap, a], map_row) else {
                return Vec::new();
            };
            iter.filter_map(Result::ok).collect::<Vec<_>>()
        }
        None => {
            let sql = format!("{BASE_SQL} ORDER BY m.id DESC LIMIT ?2");
            let Ok(mut stmt) = conn.prepare(&sql) else {
                return Vec::new();
            };
            let Ok(iter) = stmt.query_map(rusqlite::params![pattern, cap], map_row) else {
                return Vec::new();
            };
            iter.filter_map(Result::ok).collect::<Vec<_>>()
        }
    };
    rows
}

/// Search the I-2b artifacts ledger (`artifacts.jsonl`) by display name,
/// archived name, origin, and channel.
///
/// Re-reads a bounded tail window rather than the whole file — a query's
/// cost is bounded by bytes, not by how many files an agent has ever
/// produced. Newest rows are matched first (the ledger is append-only, file
/// order = chronological), so a capped result set is the most recent
/// matches, not the oldest. A missing ledger file, or an empty query, is an
/// empty result.
pub fn search_artifacts(
    home_dir: &Path,
    agent_id: Option<&str>,
    query: &str,
    limit: usize,
) -> Vec<SearchHit> {
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() {
        return Vec::new();
    }
    let limit = limit.clamp(1, MAX_PER_SOURCE_LIMIT);

    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = home_dir.join("artifacts.jsonl");
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(ARTIFACTS_TAIL_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&raw);
    let text: &str = if start > 0 {
        match text.find('\n') {
            Some(pos) => &text[pos + 1..],
            None => return Vec::new(),
        }
    } else {
        &text
    };

    let mut hits: Vec<SearchHit> = Vec::new();
    // Newest rows are last in file order; walk backwards so a capped result
    // is the most recent matches.
    for line in text.lines().rev() {
        if hits.len() >= limit {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let row_agent = v.get("agent_id").and_then(|x| x.as_str()).unwrap_or("");
        if let Some(want) = agent_id
            && row_agent != want
        {
            continue;
        }
        let display_name = v.get("display_name").and_then(|x| x.as_str()).unwrap_or("");
        let archived_name = v.get("archived_name").and_then(|x| x.as_str()).unwrap_or("");
        let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
        let channel = v.get("channel").and_then(|x| x.as_str()).unwrap_or("");
        let task_id = v.get("task_id").and_then(|x| x.as_str());
        let haystack = format!("{display_name} {archived_name} {origin} {channel}").to_lowercase();
        if !haystack.contains(&query_lower) {
            continue;
        }
        let name = if display_name.is_empty() { archived_name } else { display_name };
        let produced_at = v.get("produced_at").and_then(|x| x.as_str()).unwrap_or("");
        hits.push(SearchHit {
            source: SOURCE_ARTIFACT,
            id: archived_name.to_string(),
            title: truncate_bytes(name, TITLE_MAX_BYTES).to_string(),
            snippet: truncate_bytes(origin, SNIPPET_MAX_BYTES).to_string(),
            agent_id: if row_agent.is_empty() { None } else { Some(row_agent.to_string()) },
            timestamp: if produced_at.is_empty() { None } else { Some(produced_at.to_string()) },
            jump: serde_json::json!({
                "agent_id": if row_agent.is_empty() { None } else { Some(row_agent) },
                "archived_name": archived_name,
                "task_id": task_id,
            }),
        });
    }
    hits
}

/// Convert already-fetched memory entries (`MemoryEngine::search`) into
/// cross-source hits. Pure — the caller performs the (async) DB read; this
/// only shapes the result.
pub fn memory_hits(
    agent_id: &str,
    entries: &[duduclaw_core::types::MemoryEntry],
    limit: usize,
) -> Vec<SearchHit> {
    entries
        .iter()
        .take(limit.clamp(1, MAX_PER_SOURCE_LIMIT))
        .map(|e| SearchHit {
            source: SOURCE_MEMORY,
            id: e.id.clone(),
            title: truncate_bytes(e.content.trim(), TITLE_MAX_BYTES).to_string(),
            snippet: truncate_bytes(e.content.trim(), SNIPPET_MAX_BYTES).to_string(),
            agent_id: Some(agent_id.to_string()),
            timestamp: Some(e.timestamp.to_rfc3339()),
            jump: serde_json::json!({ "agent_id": agent_id, "entry_id": e.id }),
        })
        .collect()
}

/// Convert wiki search hits (agent-local or shared) into cross-source hits.
/// `agent_id: None` marks a shared-wiki hit — no single owning agent.
pub fn wiki_hits(
    source: &'static str,
    agent_id: Option<&str>,
    hits: &[duduclaw_memory::wiki::SearchHit],
    limit: usize,
) -> Vec<SearchHit> {
    hits.iter()
        .take(limit.clamp(1, MAX_PER_SOURCE_LIMIT))
        .map(|h| {
            let snippet = h.context_lines.join(" ");
            SearchHit {
                source,
                id: h.path.clone(),
                title: truncate_bytes(&h.title, TITLE_MAX_BYTES).to_string(),
                snippet: truncate_bytes(&snippet, SNIPPET_MAX_BYTES).to_string(),
                agent_id: agent_id.map(str::to_string),
                timestamp: None,
                jump: serde_json::json!({ "agent_id": agent_id, "path": h.path }),
            }
        })
        .collect()
}

/// Merge hits from multiple sources into one list and cap the total.
///
/// Sorted newest-first by timestamp; hits without one (wiki, ranked by
/// relevance rather than recency) sort after every timestamped hit but keep
/// their original relative order (stable sort) — the source engine's own
/// ranking survives instead of being scrambled. Returns whether the cap
/// actually dropped anything, so a caller can report an honest `truncated`
/// flag rather than pretending the list is complete.
pub fn merge_and_cap(mut hits: Vec<SearchHit>) -> (Vec<SearchHit>, bool) {
    hits.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let truncated = hits.len() > MAX_TOTAL_HITS;
    hits.truncate(MAX_TOTAL_HITS);
    (hits, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_entry(id: &str, content: &str) -> duduclaw_core::types::MemoryEntry {
        duduclaw_core::types::MemoryEntry {
            id: id.to_string(),
            agent_id: "sales".to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec![],
            embedding: None,
            layer: Default::default(),
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: String::new(),
        }
    }

    fn wiki_hit(path: &str, title: &str, lines: &[&str]) -> duduclaw_memory::wiki::SearchHit {
        duduclaw_memory::wiki::SearchHit {
            path: path.to_string(),
            title: title.to_string(),
            score: 1,
            weighted_score: 1.0,
            trust: 0.5,
            layer: Default::default(),
            source_type: Default::default(),
            context_lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── search_conversations ────────────────────────────────────────────

    async fn seed_sessions_db(path: &Path) -> crate::session::SessionManager {
        let mgr = crate::session::SessionManager::new(path).unwrap();
        mgr.get_or_create("sess-sales-1", "sales").await.unwrap();
        mgr.append_message("sess-sales-1", "user", "報價單在哪裡？", 5).await.unwrap();
        mgr.append_message("sess-sales-1", "assistant", "報價單已經寄出了", 5).await.unwrap();
        mgr.get_or_create("sess-hr-1", "hr").await.unwrap();
        mgr.append_message("sess-hr-1", "user", "請假流程是什麼", 5).await.unwrap();
        mgr
    }

    #[tokio::test]
    async fn search_conversations_matches_and_scopes_by_agent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let _mgr = seed_sessions_db(&db_path).await;

        // Cross-agent (admin) search finds the sales hit.
        let hits = search_conversations(&db_path, None, "報價單", 10);
        assert_eq!(hits.len(), 2, "both turns mention 報價單");
        assert!(hits.iter().all(|h| h.source == SOURCE_CONVERSATION));
        assert!(hits.iter().all(|h| h.agent_id.as_deref() == Some("sales")));

        // Agent-scoped search never sees another agent's conversation.
        let hits = search_conversations(&db_path, Some("hr"), "報價單", 10);
        assert!(hits.is_empty());
        let hits = search_conversations(&db_path, Some("hr"), "請假", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].jump["session_id"], "sess-hr-1");
    }

    #[tokio::test]
    async fn search_conversations_excludes_hidden_turns() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let mgr = seed_sessions_db(&db_path).await;
        // Directly flip the `hidden` flag the way `hide_message` does,
        // targeting the row by content since `append_message` does not
        // return an id.
        {
            let raw = Connection::open(&db_path).unwrap();
            raw.execute(
                "UPDATE session_messages SET hidden = 1 WHERE content = ?1",
                rusqlite::params!["報價單已經寄出了"],
            )
            .unwrap();
        }
        drop(mgr);

        let hits = search_conversations(&db_path, None, "寄出", 10);
        assert!(hits.is_empty(), "hidden turns must never surface in search");
    }

    #[test]
    fn search_conversations_missing_db_and_empty_query_are_honest_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        assert!(search_conversations(&missing, None, "x", 10).is_empty());

        let db_path = dir.path().join("sessions.db");
        crate::session::SessionManager::new(&db_path).unwrap();
        assert!(search_conversations(&db_path, None, "   ", 10).is_empty());
    }

    // ── search_artifacts ────────────────────────────────────────────────

    fn write_ledger(home: &Path, rows: &[serde_json::Value]) {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(home.join("artifacts.jsonl"), body).unwrap();
    }

    #[test]
    fn search_artifacts_matches_name_and_origin_and_scopes_by_agent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_ledger(
            home,
            &[
                serde_json::json!({
                    "produced_at": "2026-08-15T10:00:00+00:00",
                    "agent_id": "sales",
                    "archived_name": "1_a.docx",
                    "display_name": "季度報告.docx",
                    "size": 12,
                    "origin": "declared",
                    "task_id": "task-1",
                }),
                serde_json::json!({
                    "produced_at": "2026-08-15T11:00:00+00:00",
                    "agent_id": "hr",
                    "archived_name": "2_b.pdf",
                    "display_name": "onboarding.pdf",
                    "size": 20,
                    "origin": "uploaded",
                }),
            ],
        );

        let hits = search_artifacts(home, None, "季度", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, SOURCE_ARTIFACT);
        assert_eq!(hits[0].jump["task_id"], "task-1");

        // Agent scoping excludes the other agent's row even on a match.
        let hits = search_artifacts(home, Some("hr"), "季度", 10);
        assert!(hits.is_empty());
        let hits = search_artifacts(home, Some("hr"), "onboarding", 10);
        assert_eq!(hits.len(), 1);

        // Origin is also searchable.
        let hits = search_artifacts(home, None, "uploaded", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "2_b.pdf");
    }

    #[test]
    fn search_artifacts_missing_ledger_and_empty_query_are_honest_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(search_artifacts(dir.path(), None, "x", 10).is_empty());
        write_ledger(dir.path(), &[serde_json::json!({"agent_id": "a"})]);
        assert!(search_artifacts(dir.path(), None, "  ", 10).is_empty());
    }

    // ── memory_hits / wiki_hits converters ──────────────────────────────

    #[test]
    fn memory_hits_shapes_and_caps() {
        let entries: Vec<_> = (0..5).map(|i| mem_entry(&format!("m{i}"), "客戶反饋內容")).collect();
        let hits = memory_hits("sales", &entries, 3);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|h| h.source == SOURCE_MEMORY));
        assert!(hits.iter().all(|h| h.agent_id.as_deref() == Some("sales")));
    }

    #[test]
    fn wiki_hits_shapes_snippet_from_context_lines_and_marks_shared() {
        let raw = vec![wiki_hit("sop/onboarding.md", "Onboarding SOP", &["line one", "line two"])];
        let hits = wiki_hits(SOURCE_WIKI, Some("hr"), &raw, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].snippet, "line one line two");
        assert_eq!(hits[0].agent_id.as_deref(), Some("hr"));

        let shared = wiki_hits(SOURCE_SHARED_WIKI, None, &raw, 10);
        assert_eq!(shared[0].source, SOURCE_SHARED_WIKI);
        assert!(shared[0].agent_id.is_none());
    }

    // ── merge_and_cap / clamp_limit ──────────────────────────────────────

    fn hit_with_ts(ts: Option<&str>) -> SearchHit {
        SearchHit {
            source: SOURCE_ARTIFACT,
            id: ts.unwrap_or("none").to_string(),
            title: "t".into(),
            snippet: "s".into(),
            agent_id: None,
            timestamp: ts.map(str::to_string),
            jump: serde_json::json!({}),
        }
    }

    #[test]
    fn merge_and_cap_sorts_newest_first_and_reports_truncation() {
        let hits = vec![
            hit_with_ts(Some("2026-08-15T10:00:00+00:00")),
            hit_with_ts(Some("2026-08-15T12:00:00+00:00")),
            hit_with_ts(None),
            hit_with_ts(Some("2026-08-15T11:00:00+00:00")),
        ];
        let (merged, truncated) = merge_and_cap(hits);
        assert!(!truncated);
        let timestamps: Vec<Option<&str>> = merged.iter().map(|h| h.timestamp.as_deref()).collect();
        assert_eq!(
            timestamps,
            vec![
                Some("2026-08-15T12:00:00+00:00"),
                Some("2026-08-15T11:00:00+00:00"),
                Some("2026-08-15T10:00:00+00:00"),
                None,
            ]
        );
    }

    #[test]
    fn merge_and_cap_enforces_total_ceiling() {
        let hits: Vec<_> = (0..(MAX_TOTAL_HITS + 5))
            .map(|i| {
                let mut h = hit_with_ts(Some("2026-08-15T10:00:00+00:00"));
                h.id = format!("id-{i}");
                h
            })
            .collect();
        let (merged, truncated) = merge_and_cap(hits);
        assert_eq!(merged.len(), MAX_TOTAL_HITS);
        assert!(truncated);
    }

    #[test]
    fn clamp_limit_bounds() {
        assert_eq!(clamp_limit(None), DEFAULT_PER_SOURCE_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1000)), MAX_PER_SOURCE_LIMIT);
        assert_eq!(clamp_limit(Some(5)), 5);
    }
}
