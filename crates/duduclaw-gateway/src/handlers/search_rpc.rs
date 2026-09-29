//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── I-5: cross-source content search (⌘K backend) ────────

    /// `search.query` — one bounded, CJK-safe query fanned out across four
    /// surfaces: conversation turns, delivered/received files (the I-2b
    /// provenance ledger), agent memory, and knowledge (agent-local + shared
    /// wiki). Every hit is self-labelled (`source`) and carries a `jump`
    /// target so the dashboard can navigate straight to it.
    ///
    /// Sources degrade independently — a missing db/dir/file contributes
    /// zero rows, never an error for the whole query (walk-through 4 in the
    /// design doc: "找回上週的產物" must work even on a fresh install where
    /// nothing has happened yet). Admin gate is enforced by the dispatch
    /// arm's `check_agent_filter!`; the re-check here is defense-in-depth so
    /// a future call site cannot reach this method by skipping that macro.
    pub(crate) async fn handle_search_query(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let query = params
            .get("q")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return WsFrame::error_response("", "q is required");
        }
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(a) = agent_id
            && !is_valid_agent_id(a)
        {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        if !ctx.is_admin() && agent_id.is_none() {
            return WsFrame::error_response("", "agent_id parameter is required");
        }

        let per_source_limit = crate::search_index::clamp_limit(
            params
                .get("limit")
                .and_then(|v| v.as_u64())
                .and_then(|n| usize::try_from(n).ok()),
        );

        // Optional source allowlist (`["conversations","artifacts","memory","wiki"]`).
        // Absent ⇒ every source is queried.
        let wanted: Option<Vec<String>> =
            params.get("sources").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            });
        let wants = |s: &str| wanted.as_ref().is_none_or(|w| w.iter().any(|x| x == s));

        let mut hits: Vec<crate::search_index::SearchHit> = Vec::new();

        if wants("conversations") {
            let session_db = self.home_dir.join("sessions.db");
            hits.extend(crate::search_index::search_conversations(
                &session_db,
                agent_id,
                query,
                per_source_limit,
            ));
        }

        if wants("artifacts") {
            hits.extend(crate::search_index::search_artifacts(
                &self.home_dir,
                agent_id,
                query,
                per_source_limit,
            ));
        }

        // Memory is one SQLite db per agent — a true cross-agent memory
        // search would mean opening every agent's db per keystroke, so (like
        // `memory.search` itself) this source requires a bound agent even
        // for admins. Omitting `agent_id` simply yields zero memory hits,
        // never an error.
        if wants("memory")
            && let Some(a) = agent_id
        {
            let db_path = self.agent_memory_db_path(a);
            if db_path.exists()
                && let Ok(mut engine) = SqliteMemoryEngine::new(&db_path)
            {
                engine.retrieval_weights = self.memory_retrieval_weights();
                if let Ok(entries) = engine.search(a, query, per_source_limit).await {
                    hits.extend(crate::search_index::memory_hits(
                        a,
                        &entries,
                        per_source_limit,
                    ));
                }
            }
        }

        if wants("wiki") {
            if let Some(a) = agent_id {
                let wiki_dir = self.home_dir.join("agents").join(a).join("wiki");
                if wiki_dir.exists() {
                    let store = duduclaw_memory::WikiStore::new(wiki_dir);
                    if let Ok(wh) = store.search(query, per_source_limit) {
                        hits.extend(crate::search_index::wiki_hits(
                            crate::search_index::SOURCE_WIKI,
                            Some(a),
                            &wh,
                            per_source_limit,
                        ));
                    }
                }
            }
            let shared_dir = self.home_dir.join("shared").join("wiki");
            if shared_dir.exists() {
                let shared = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
                if let Ok(wh) = shared.search(query, per_source_limit) {
                    hits.extend(crate::search_index::wiki_hits(
                        crate::search_index::SOURCE_SHARED_WIKI,
                        None,
                        &wh,
                        per_source_limit,
                    ));
                }
            }
        }

        let (hits, truncated) = crate::search_index::merge_and_cap(hits);
        let rows: Vec<Value> = hits
            .iter()
            .map(|h| serde_json::to_value(h).unwrap_or_else(|_| json!({})))
            .collect();
        WsFrame::ok_response(
            "",
            json!({ "query": query, "hits": rows, "truncated": truncated }),
        )
    }
}
