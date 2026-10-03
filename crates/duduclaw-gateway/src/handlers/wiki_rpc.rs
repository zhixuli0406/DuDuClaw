//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Wiki Knowledge Base ──────────────────────────────────

    pub(crate) async fn handle_wiki_pages(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.list_pages() {
            Ok(pages) => {
                let items: Vec<Value> = pages
                    .iter()
                    .map(|p| {
                        json!({
                            "path": p.path,
                            "title": p.title,
                            "updated": p.updated.to_rfc3339(),
                            "tags": p.tags,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "pages": items, "exists": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to list wiki pages: {e}")),
        }
    }

    // ── WP5c: auto-filed knowledge pages (curation station audit tab) ──────
    //
    // `wiki.pages` cannot serve this tab: it returns neither `author` nor
    // `sources`, so the client would need an N+1 `wiki.read` per page just to
    // tell an auto page from a human one. One RPC, one pass.

    /// Resolve `<home>/agents/<id>/wiki`, validating the agent id first.
    pub(crate) fn agent_wiki_dir(&self, agent_id: &str) -> Option<PathBuf> {
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return None;
        }
        Some(self.home_dir.join("agents").join(agent_id).join("wiki"))
    }

    pub(crate) async fn handle_wiki_auto_pages(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(wiki_dir) = self.agent_wiki_dir(agent_id) else {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        };
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }
        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match crate::auto_wiki_page::list_auto_pages(&store) {
            Ok(rows) => WsFrame::ok_response("", json!({ "pages": rows, "exists": true })),
            Err(e) => WsFrame::error_response("", &format!("Failed to list auto pages: {e}")),
        }
    }

    /// Promote one auto page to curated knowledge (§6.2). Human-only, one-way.
    pub(crate) async fn handle_wiki_promote(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(wiki_dir) = self.agent_wiki_dir(agent_id) else {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        };
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }
        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match crate::auto_wiki_page::promote_page(&store, page_path) {
            Ok(()) => WsFrame::ok_response("", json!({ "promoted": true, "path": page_path })),
            Err(e) => WsFrame::error_response("", &format!("Promote failed: {e}")),
        }
    }

    /// Remove one auto page: archive the file (restorable from `_archive/`)
    /// AND expire its memory pointer.
    ///
    /// The pointer is expired by **exact subject** rather than by origin.
    /// `memory.invalidate_origin(agent, "channel")` would expire every
    /// conversationally-learned memory the agent has — correct as a "clear all
    /// auto-collected knowledge" button (which the UI still offers, with an
    /// explicit warning), catastrophic as "remove this one page".
    pub(crate) async fn handle_wiki_archive(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(wiki_dir) = self.agent_wiki_dir(agent_id) else {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        };
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }
        if !crate::auto_wiki_page::is_auto_path(page_path) {
            return WsFrame::error_response("", "Only auto-filed pages can be removed here");
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let archived = match store.archive_page(page_path) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("Archive failed: {e}")),
        };

        // Expire the pointer so retrieval stops surfacing a page that is gone.
        let mut expired = 0usize;
        let db_path = self.agent_memory_db_path(agent_id);
        if db_path.exists() {
            let subject = crate::auto_wiki_page::pointer_subject(page_path);
            match SqliteMemoryEngine::new(&db_path) {
                Ok(engine) => match engine
                    .expire_by_subject(agent_id, &subject, "auto_page_removed")
                    .await
                {
                    Ok(n) => expired = n,
                    Err(e) => warn!(agent = agent_id, "auto page pointer expiry failed: {e}"),
                },
                Err(e) => warn!(
                    agent = agent_id,
                    "auto page pointer expiry: open db failed: {e}"
                ),
            }
        }

        WsFrame::ok_response(
            "",
            json!({ "archived": archived, "pointers_expired": expired, "path": page_path }),
        )
    }

    /// P2 = C: copy one auto page into the shared wiki so other AI staff can
    /// use it. Explicitly a human action from the dashboard — the automatic
    /// path only ever writes the agent's own `auto/` namespace.
    pub(crate) async fn handle_wiki_share(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(wiki_dir) = self.agent_wiki_dir(agent_id) else {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        };
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }
        // Same gate as promote/archive: this button lives on the auto-filing
        // audit tab and its whole contract is "an auto page, its provenance
        // intact". Without the check it would become a general
        // copy-any-page-to-shared primitive with none of the shared-wiki
        // safeguards (`.scope.toml`, department visibility, secret scanning)
        // that the real `wiki_write` (scope shared) / `wiki_share` paths apply.
        if !crate::auto_wiki_page::is_auto_path(page_path) {
            return WsFrame::error_response("", "Only auto-filed pages can be shared here");
        }
        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let page = match store.read_page(page_path) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Failed to read page: {e}")),
        };
        if page.author.as_deref() != Some(crate::auto_wiki_page::AUTO_PAGE_AUTHOR) {
            return WsFrame::error_response("", "Only auto-filed pages can be shared here");
        }

        // `sources/` is the shared wiki's provenance namespace and derives
        // `SourceType::RawDialogue`, so a shared auto page keeps the same low
        // ranking weight it had locally.
        let stem = std::path::Path::new(page_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("page");
        let shared_path = format!("sources/{agent_id}--{stem}.md");
        let now = Utc::now();
        let shared = duduclaw_memory::WikiPage {
            path: shared_path.clone(),
            title: page.title.clone(),
            created: now,
            updated: now,
            tags: {
                let mut t = page.tags.clone();
                if !t.iter().any(|x| x == "shared") {
                    t.push("shared".to_string());
                }
                t.push(format!("from-{agent_id}"));
                t
            },
            related: Vec::new(),
            sources: page.sources.clone(),
            author: Some(agent_id.to_string()),
            layer: duduclaw_memory::WikiLayer::Context,
            trust: page.trust,
            source_type: duduclaw_memory::SourceType::RawDialogue,
            last_verified: None,
            citation_count: 0,
            error_signal_count: 0,
            success_signal_count: 0,
            do_not_inject: false,
            body: format!(
                "{}\n\n---\n*由「{agent_id}」的知識庫分享（`{page_path}`）*\n",
                page.body
            ),
        };
        let shared_store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        if let Err(e) = shared_store.ensure_scaffold() {
            return WsFrame::error_response("", &format!("Shared wiki unavailable: {e}"));
        }
        match shared_store.write_page_with_author(
            &shared_path,
            &duduclaw_memory::serialize_page(&shared),
            agent_id,
        ) {
            Ok(()) => WsFrame::ok_response("", json!({ "shared": true, "path": shared_path })),
            Err(e) => WsFrame::error_response("", &format!("Share failed: {e}")),
        }
    }

    pub(crate) async fn handle_wiki_read(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'page_path' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        let store = duduclaw_memory::WikiStore::new(wiki_dir);

        // Allow reading reserved files like _index.md, _schema.md
        match store.read_raw(page_path) {
            Ok(content) => {
                WsFrame::ok_response("", json!({ "content": content, "path": page_path }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read page: {e}")),
        }
    }

    pub(crate) async fn handle_wiki_search(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);
        // Optional conversation_id — when supplied, every returned hit is
        // recorded into the global CitationTracker so the prediction-error
        // feedback bus can later attribute trust deltas to the cited pages.
        let conversation_id = params
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if agent_id.is_empty() || query.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'query' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let result = match conversation_id {
            Some(conv_id) => {
                let tracker = duduclaw_memory::feedback::global_tracker();
                store.search_with_citation(query, limit, agent_id, conv_id, None, &tracker)
            }
            None => store.search(query, limit),
        };
        match result {
            Ok(hits) => {
                let items: Vec<Value> = hits
                    .iter()
                    .map(|h| {
                        json!({
                            "path": h.path,
                            "title": h.title,
                            "score": h.score,
                            "weighted_score": h.weighted_score,
                            "trust": h.trust,
                            "layer": h.layer.to_string(),
                            "source_type": h.source_type.to_string(),
                            "context_lines": h.context_lines,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "hits": items }))
            }
            Err(e) => WsFrame::error_response("", &format!("Wiki search failed: {e}")),
        }
    }

    pub(crate) async fn handle_wiki_lint(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "total_pages": 0, "healthy": true }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.lint() {
            Ok(report) => WsFrame::ok_response(
                "",
                json!({
                    "total_pages": report.total_pages,
                    "index_entries": report.index_entries,
                    "orphan_pages": report.orphan_pages,
                    "broken_links": report.broken_links,
                    "stale_pages": report.stale_pages,
                    "healthy": report.orphan_pages.is_empty() && report.broken_links.is_empty(),
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("Wiki lint failed: {e}")),
        }
    }

    pub(crate) async fn handle_wiki_stats(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let pages = match store.list_pages() {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Failed to list pages: {e}")),
        };

        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for p in &pages {
            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_insert(0) += 1;
        }

        let most_recent = pages.first().map(|p| {
            json!({
                "title": p.title,
                "path": p.path,
                "updated": p.updated.to_rfc3339(),
            })
        });

        WsFrame::ok_response(
            "",
            json!({
                "exists": true,
                "total_pages": pages.len(),
                "by_directory": by_dir,
                "most_recent": most_recent,
            }),
        )
    }

    // ── Phase 4: Wiki RL Trust inspection / override ────────

    /// `wiki.trust_audit` — list low-trust pages for an agent, with citation
    /// + signal counters. Read-only; safe for any authenticated user.
    pub(crate) async fn handle_wiki_trust_audit(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let max_trust = params
            .get("max_trust")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.3) as f32;
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize).min(500);

        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let store = match duduclaw_memory::trust_store::global_trust_store() {
            Some(s) => s,
            None => {
                return WsFrame::ok_response(
                    "",
                    json!({
                        "rows": [],
                        "available": false,
                        "note": "Trust store not initialized — wiki trust feedback disabled",
                    }),
                );
            }
        };

        match store.list_low_trust(agent_id, max_trust, limit) {
            Ok(rows) => {
                let items: Vec<Value> = rows
                    .iter()
                    .map(|s| {
                        json!({
                            "page_path": s.page_path,
                            "agent_id": s.agent_id,
                            "trust": s.trust,
                            "citation_count": s.citation_count,
                            "error_signal_count": s.error_signal_count,
                            "success_signal_count": s.success_signal_count,
                            "last_signal_at": s.last_signal_at.map(|d| d.to_rfc3339()),
                            "last_verified": s.last_verified.map(|d| d.to_rfc3339()),
                            "do_not_inject": s.do_not_inject,
                            "locked": s.locked,
                            "updated_at": s.updated_at.to_rfc3339(),
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "rows": items, "available": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("trust audit failed: {e}")),
        }
    }

    /// `wiki.trust_override` — manually set trust for a page; optional `lock`
    /// makes the page immune to subsequent automatic adjustments.
    /// Admin-only because it can mask drift the feedback loop is trying to
    /// communicate.
    pub(crate) async fn handle_wiki_trust_override(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let trust = params
            .get("trust")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32);
        let lock = params
            .get("lock")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let do_not_inject = params.get("do_not_inject").and_then(|v| v.as_bool());
        let reason = params.get("reason").and_then(|v| v.as_str());

        if agent_id.is_empty() || page_path.is_empty() || trust.is_none() {
            return WsFrame::error_response(
                "",
                "Missing 'agent_id', 'page_path', or 'trust' parameter",
            );
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        // page_path is admin-controlled but still belongs in audit log; reject
        // path traversal and other shapes that won't survive janitor file ops
        // (review H2/M6).
        if !is_safe_wiki_page_path(page_path) {
            return WsFrame::error_response("", "Invalid page_path");
        }
        let trust = trust.unwrap();
        if !(0.0..=1.0).contains(&trust) {
            return WsFrame::error_response("", "trust must be in [0.0, 1.0]");
        }
        // Cap audit-log strings to bound history table growth and block CR/LF
        // injection into log lines (review M3).
        let reason_clean: Option<String> = reason.map(|r| {
            r.chars()
                .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
                .take(512)
                .collect::<String>()
        });
        let reason_ref = reason_clean.as_deref();

        let store = match duduclaw_memory::trust_store::global_trust_store() {
            Some(s) => s,
            None => return WsFrame::error_response("", "Trust store not initialized"),
        };

        match store.manual_set(page_path, agent_id, trust, lock, do_not_inject, reason_ref) {
            Ok(outcome) => WsFrame::ok_response(
                "",
                json!({
                    "page_path": outcome.page_path,
                    "agent_id": outcome.agent_id,
                    "old_trust": outcome.old_trust,
                    "new_trust": outcome.new_trust,
                    "applied_delta": outcome.applied_delta,
                    "locked": outcome.locked,
                    "became_archived": outcome.became_archived,
                    "became_recovered": outcome.became_recovered,
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("trust override failed: {e}")),
        }
    }

    /// `wiki.trust_history` — recent audit log rows for a page, useful for
    /// dashboards or post-mortem analysis.
    pub(crate) async fn handle_wiki_trust_history(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize).min(500);

        if agent_id.is_empty() || page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'page_path' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        if !is_safe_wiki_page_path(page_path) {
            return WsFrame::error_response("", "Invalid page_path");
        }

        let store = match duduclaw_memory::trust_store::global_trust_store() {
            Some(s) => s,
            None => return WsFrame::ok_response("", json!({ "rows": [], "available": false })),
        };

        match store.history(agent_id, page_path, limit) {
            Ok(rows) => {
                let items: Vec<Value> = rows
                    .iter()
                    .map(|h| {
                        json!({
                            "ts": h.ts.to_rfc3339(),
                            "old_trust": h.old_trust,
                            "new_trust": h.new_trust,
                            "applied_delta": h.applied_delta,
                            "trigger": h.trigger,
                            "conversation_id": h.conversation_id,
                            "composite_error": h.composite_error,
                            "signal_kind": h.signal_kind,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "rows": items, "available": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("trust history failed: {e}")),
        }
    }
}
