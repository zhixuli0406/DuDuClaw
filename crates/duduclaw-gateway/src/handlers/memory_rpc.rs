//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Memory ──────────────────────────────────────────────

    /// D3 wiring — read `[memory] graph_embed_seed` / `graph_embed_seed_top_k`
    /// from `config.toml` and fold them into a [`RetrievalWeights`]. Absent /
    /// malformed config ⇒ engine defaults (seed off, top-k 5) so ranking stays
    /// byte-identical to the pre-config path. The seed path is additionally
    /// gated on an attached embedder inside the engine, so this is latent
    /// plumbing until a semantic embedder is wired.
    pub(crate) fn memory_retrieval_weights(&self) -> duduclaw_memory::engine::RetrievalWeights {
        let table = std::fs::read_to_string(self.home_dir.join("config.toml"))
            .ok()
            .and_then(|raw| raw.parse::<toml::Table>().ok());
        memory_retrieval_weights_from_table(table.as_ref())
    }

    /// Resolve the memory.db path for an agent's read RPCs.
    /// Prefers `agents/<id>/state/memory.db`, then `agents/<id>/memory.db`,
    /// then the shared `<home>/memory.db` — the live write path
    /// (`server.rs` `.with_memory_db`) points every engine at the shared file,
    /// so per-agent files should not exist on a healthy install: boot runs
    /// `memory_migrate::merge_per_agent_memory_dbs`, which merges any stray /
    /// legacy per-agent file into the shared db and archives it (the
    /// 2026-08-20 關鍵洞察 incident — a stray per-agent file silently hijacked
    /// every memory read RPC for that agent). Every engine query filters by
    /// `agent_id`, so reading the shared file stays agent-scoped.
    pub(crate) fn agent_memory_db_path(&self, agent_id: &str) -> PathBuf {
        let agent_dir = self.home_dir.join("agents").join(agent_id);
        let state_path = agent_dir.join("state").join("memory.db");
        if state_path.exists() {
            return state_path;
        }
        let legacy_path = agent_dir.join("memory.db");
        if legacy_path.exists() {
            return legacy_path;
        }
        self.home_dir.join("memory.db")
    }

    pub(crate) async fn handle_memory_search(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(200) as usize;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) || query.is_empty() {
            return WsFrame::error_response(
                "",
                "Missing or invalid 'agent_id' or 'query' parameter",
            );
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            // Same shape as a populated response (and as `memory.browse`): a
            // caller must never have to special-case a missing `signals` key.
            return WsFrame::ok_response("", json!({ "entries": [], "signals": [] }));
        }

        let mut engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };
        // D3 wiring: config-driven graph seeding weights (default ⇒ unchanged).
        engine.retrieval_weights = self.memory_retrieval_weights();

        match engine.search(agent_id, query, limit).await {
            Ok(entries) => {
                // WP15: same split as `memory.browse` so a search never drops
                // the user back into a telemetry-flooded list. Search already
                // ranks a bounded set, so partitioning in Rust is enough here.
                let (signals, results): (Vec<_>, Vec<_>) = entries
                    .iter()
                    .partition(|e| is_system_signal(&e.source_event, &e.content));
                let (weights, now) = (engine.retrieval_weights.clone(), Utc::now());
                WsFrame::ok_response(
                    "",
                    json!({
                        "entries": results.into_iter().map(|e| memory_entry_row(e, &weights, now)).collect::<Vec<_>>(),
                        "signals": signals.into_iter().map(|e| memory_entry_row(e, &weights, now)).collect::<Vec<_>>(),
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("Memory search failed: {e}")),
        }
    }

    pub(crate) async fn handle_memory_browse(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(200) as usize;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [], "signals": [] }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        // WP15: two lists, two budgets. `entries` is what the user told the
        // agent; `signals` is the platform's own learning telemetry, which the
        // dashboard files under a separate collapsed section instead of
        // letting it bury (and, via the shared LIMIT, evict) real memories.
        match engine.list_recent_split(agent_id, limit).await {
            Ok((memories, signals)) => {
                let (weights, now) = (engine.retrieval_weights.clone(), Utc::now());
                let rows: Vec<Value> = memories
                    .iter()
                    .map(|e| memory_entry_row(e, &weights, now))
                    .collect();
                let signal_rows: Vec<Value> = signals
                    .iter()
                    .map(|e| memory_entry_row(e, &weights, now))
                    .collect();
                WsFrame::ok_response("", json!({ "entries": rows, "signals": signal_rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory browse failed: {e}")),
        }
    }

    /// Aggregate view behind the memory page's decay visualisation: how fresh
    /// this agent's memories are as a whole, which ones are about to fade, which
    /// ones keep getting recalled, and how the pile has grown.
    ///
    /// Read-only — it opens the same db `memory.browse` does and derives every
    /// number from `duduclaw_memory::engine`'s own Ebbinghaus functions. It
    /// scans at most [`MEMORY_DECAY_SCAN_CAP`] entries (newest first) and says
    /// so via `truncated`, rather than silently reporting a partial pile as the
    /// whole one.
    pub(crate) async fn handle_memory_decay_overview(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let window_days = params
            .get("days")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .clamp(7, 90) as i64;
        let top_n = params
            .get("top_n")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;

        let archive_threshold =
            duduclaw_memory::decay::MemoryDecayPolicy::default().min_retrievability;
        let empty = || {
            json!({
                "total": 0,
                "scanned": 0,
                "truncated": false,
                "buckets": memory_freshness_bucket_rows(&HashMap::new()),
                "fading_soon": [],
                "most_recalled": [],
                "trend": [],
                "window_days": window_days,
                "archive_threshold": archive_threshold,
            })
        };

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", empty());
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        // Same list `memory.browse` shows — the overview must describe exactly
        // the pile the user can scroll through, telemetry signals excluded.
        let entries = match engine
            .list_recent_split(agent_id, MEMORY_DECAY_SCAN_CAP)
            .await
        {
            Ok((memories, _signals)) => memories,
            Err(e) => {
                return WsFrame::error_response("", &format!("Memory decay overview failed: {e}"));
            }
        };
        if entries.is_empty() {
            return WsFrame::ok_response("", empty());
        }

        let weights = engine.retrieval_weights.clone();
        let now = Utc::now();

        // One pass: freshness histogram + the decay figure per entry, kept
        // alongside its entry so the two top-N lists never recompute it.
        let mut counts: HashMap<&'static str, u64> = HashMap::new();
        let mut scored: Vec<(&duduclaw_core::types::MemoryEntry, f64)> =
            Vec::with_capacity(entries.len());
        for e in &entries {
            let (r, _) = memory_decay_figures(e, &weights, now);
            *counts.entry(memory_freshness_band(r)).or_insert(0) += 1;
            scored.push((e, r));
        }

        // Faintest first — the ones a user would want to reinforce or let go.
        let mut fading = scored.clone();
        fading.sort_by(|a, b| a.1.total_cmp(&b.1));
        let fading_soon: Vec<Value> = fading
            .iter()
            .take(top_n)
            .map(|(e, _)| memory_entry_row(e, &weights, now))
            .collect();

        // Most-recalled: access_count desc, freshest first on a tie so the list
        // is deterministic instead of depending on scan order.
        let mut recalled: Vec<_> = scored.clone();
        recalled.sort_by(|a, b| {
            b.0.access_count
                .cmp(&a.0.access_count)
                .then(b.1.total_cmp(&a.1))
        });
        let most_recalled: Vec<Value> = recalled
            .iter()
            .filter(|(e, _)| e.access_count > 0)
            .take(top_n)
            .map(|(e, _)| memory_entry_row(e, &weights, now))
            .collect();

        // Daily accumulation over the window. `added` is that day's new
        // memories; `total` is the running pile size at the end of the day,
        // seeded from everything older than the window.
        let window_start = (now - ChronoDuration::days(window_days - 1)).date_naive();
        let mut added_by_day: HashMap<chrono::NaiveDate, u64> = HashMap::new();
        let mut baseline: u64 = 0;
        for e in &entries {
            let day = e.timestamp.date_naive();
            if day < window_start {
                baseline += 1;
            } else {
                *added_by_day.entry(day).or_insert(0) += 1;
            }
        }
        let mut running = baseline;
        let mut trend = Vec::with_capacity(window_days as usize);
        for offset in 0..window_days {
            let day = window_start + ChronoDuration::days(offset);
            let added = added_by_day.get(&day).copied().unwrap_or(0);
            running += added;
            trend.push(json!({
                "date": day.format("%Y-%m-%d").to_string(),
                "added": added,
                "total": running,
            }));
        }

        WsFrame::ok_response(
            "",
            json!({
                "total": entries.len(),
                "scanned": entries.len(),
                "truncated": entries.len() >= MEMORY_DECAY_SCAN_CAP,
                "buckets": memory_freshness_bucket_rows(&counts),
                "fading_soon": fading_soon,
                "most_recalled": most_recalled,
                "trend": trend,
                "window_days": window_days,
                "archive_threshold": archive_threshold,
            }),
        )
    }

    /// Forget one memory entry (2026-07-30 client feedback: per-row delete in
    /// the memory list). Soft delete — the engine archives the row before
    /// removing it from every retrieval surface.
    pub(crate) async fn handle_memory_forget(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let memory_id = params
            .get("memory_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        if memory_id.is_empty() {
            return WsFrame::error_response("", "Missing 'memory_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "success": false, "forgotten": false }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        match engine.forget(agent_id, memory_id).await {
            Ok(forgotten) => {
                WsFrame::ok_response("", json!({ "success": true, "forgotten": forgotten }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory forget failed: {e}")),
        }
    }

    /// RFC-24: list an agent's currently-open decisions for the Dashboard panel.
    pub(crate) async fn handle_decisions_list(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(50) as usize;
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "decisions": [] }));
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };
        match engine.list_open_decisions(agent_id, limit).await {
            Ok(decisions) => {
                let rows: Vec<Value> = decisions
                    .iter()
                    .map(|d| {
                        json!({
                            "id": d.id,
                            "question": d.question,
                            "options": d.options.iter().map(|(k, c)| json!({"key": k, "content": c})).collect::<Vec<_>>(),
                            "created_at": d.created_at,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "decisions": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("List decisions failed: {e}")),
        }
    }

    /// RFC-24: dismiss a wrongly-captured decision (false positive). Closes all
    /// of its still-valid rows and bumps the `decision_false_positive` counter so
    /// detector precision can be tracked from real labels.
    pub(crate) async fn handle_decisions_dismiss(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let decision_id = params
            .get("decision_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) || decision_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'decision_id'");
        }
        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::error_response("", "No memory db for agent");
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };
        match engine.dismiss_decision(agent_id, decision_id).await {
            Ok(true) => {
                crate::metrics::global_metrics().decision_false_positive();
                WsFrame::ok_response("", json!({ "dismissed": true, "decision_id": decision_id }))
            }
            Ok(false) => WsFrame::error_response("", "Decision not found"),
            Err(e) => WsFrame::error_response("", &format!("Dismiss failed: {e}")),
        }
    }

    /// List P2 Key-Fact Accumulator entries (exposed as "Key Insights" in the UI).
    ///
    /// Reads the `key_facts` table directly via raw SQL so that a missing table
    /// resolves to an empty result set instead of surfacing an error — the table
    /// is created on demand by `SqliteMemoryEngine::new`, but we want this RPC to
    /// work even against older databases that were created before P2 landed.
    pub(crate) async fn handle_memory_key_facts(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(200) as i64;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"));
            }
        };

        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, fact, channel, chat_id, source_session, timestamp, access_count
             FROM key_facts
             WHERE agent_id = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(e) => {
                // Graceful: if the `key_facts` table doesn't exist yet in this
                // memory.db (e.g. legacy agent that hasn't triggered P2 bootstrap),
                // fall back to an empty list rather than surfacing the SQL error.
                let msg = e.to_string();
                if msg.contains("no such table") {
                    return WsFrame::ok_response("", json!({ "entries": [] }));
                }
                return WsFrame::error_response(
                    "",
                    &format!("Key facts query prepare failed: {e}"),
                );
            }
        };

        let rows = match stmt.query_map(params![agent_id, limit], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "agent_id": row.get::<_, String>(1)?,
                "fact": row.get::<_, String>(2)?,
                "channel": row.get::<_, String>(3)?,
                "chat_id": row.get::<_, String>(4)?,
                "source_session": row.get::<_, String>(5)?,
                "timestamp": row.get::<_, String>(6)?,
                "access_count": row.get::<_, i64>(7)?,
            }))
        }) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("Key facts query failed: {e}")),
        };

        let mut entries: Vec<Value> = Vec::new();
        for row in rows {
            match row {
                Ok(v) => entries.push(v),
                Err(e) => {
                    return WsFrame::error_response(
                        "",
                        &format!("Key facts row decode failed: {e}"),
                    );
                }
            }
        }
        WsFrame::ok_response("", json!({ "entries": entries }))
    }
}
