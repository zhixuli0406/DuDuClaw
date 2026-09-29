//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Shared Wiki ─────────────────────────────────────────

    pub(crate) async fn handle_shared_wiki_pages(&self) -> WsFrame {
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
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
            Err(e) => {
                WsFrame::error_response("", &format!("Failed to list shared wiki pages: {e}"))
            }
        }
    }

    pub(crate) async fn handle_shared_wiki_read(&self, params: Value) -> WsFrame {
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.read_raw(page_path) {
            Ok(content) => {
                WsFrame::ok_response("", json!({ "content": content, "path": page_path }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read shared wiki page: {e}")),
        }
    }

    pub(crate) async fn handle_shared_wiki_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let conversation_id = params
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let result = match (conversation_id, !agent_id.is_empty()) {
            (Some(conv_id), true) => {
                let tracker = duduclaw_memory::feedback::global_tracker();
                store.search_with_citation(query, limit, agent_id, conv_id, None, &tracker)
            }
            _ => store.search(query, limit),
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
            Err(e) => WsFrame::error_response("", &format!("Shared wiki search failed: {e}")),
        }
    }

    pub(crate) async fn handle_shared_wiki_stats(&self) -> WsFrame {
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let pages = match store.list_pages() {
            Ok(p) => p,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Failed to list shared wiki pages: {e}"),
                );
            }
        };

        let mut by_author: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for p in &pages {
            // Count by author from the WikiPage.author field
            let author = p.author.as_deref().unwrap_or("unknown");
            *by_author.entry(author.to_string()).or_default() += 1;

            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_default() += 1;
        }

        let most_recent = pages.first().map(|p| {
            json!({
                "title": p.title,
                "path": p.path,
                "updated": p.updated.to_rfc3339(),
                "author": p.author,
            })
        });

        WsFrame::ok_response(
            "",
            json!({
                "exists": true,
                "total_pages": pages.len(),
                "by_author": by_author,
                "by_directory": by_dir,
                "most_recent": most_recent,
            }),
        )
    }
}
