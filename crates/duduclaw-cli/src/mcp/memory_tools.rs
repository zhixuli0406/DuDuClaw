use super::*;

pub(crate) async fn handle_memory_search_by_layer(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }
    let layer_str = params.get("layer").and_then(|v| v.as_str()).unwrap_or("");
    if layer_str.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: layer is required (episodic or semantic)"}],
            "isError": true
        });
    }
    let layer = duduclaw_core::types::MemoryLayer::parse(layer_str);
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    // v1.83.0: conversation scope (rows not explicitly stored, of other conversations),
    // applied before the limit: a scoped read over-fetches like memory_search.
    let scope = crate::mcp_memory_handlers::mcp_read_scope(agent_id);
    let fetch = match scope {
        crate::mcp_memory_handlers::ReadScope::Employee => limit,
        crate::mcp_memory_handlers::ReadScope::Conversation(_) => limit.saturating_mul(4),
    };
    let found = match memory.search_layer(agent_id, query, &layer, fetch).await {
        Ok(entries) => crate::mcp_memory_handlers::scope_entries(memory, agent_id, entries, &scope)
            .await
            .map(|mut kept| {
                kept.truncate(limit);
                kept
            }),
        Err(e) => Err(e),
    };
    match found {
        Ok(entries) => {
            if entries.is_empty() {
                serde_json::json!({
                    "content": [{"type": "text", "text": format!("No {layer_str} memories found.")}]
                })
            } else {
                let text = entries
                    .iter()
                    .map(|e| {
                        format!(
                            "[{}] [{}] {}",
                            e.timestamp.format("%Y-%m-%d %H:%M"),
                            e.layer.as_str(),
                            e.content
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                })
            }
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error searching memory by layer: {e}")}],
            "isError": true
        }),
    }
}

/// v1.83.0: on a channel conversation turn only rows visible under the
/// conversation scope are returned; the scope applies before the limit, and
/// an unknown conversation gets nothing.
pub(crate) async fn handle_memory_successful_conversations(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let scope = crate::mcp_memory_handlers::mcp_read_scope(agent_id);
    handle_memory_successful_conversations_in(params, memory, agent_id, &scope).await
}

/// [`handle_memory_successful_conversations`] with an explicit scope.
pub(crate) async fn handle_memory_successful_conversations_in(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
    scope: &crate::mcp_memory_handlers::ReadScope,
) -> Value {
    let topic = params.get("topic").and_then(|v| v.as_str()).unwrap_or("");
    if topic.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: topic is required"}],
            "isError": true
        });
    }
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let found = match scope {
        crate::mcp_memory_handlers::ReadScope::Employee => {
            memory.search_successful_conversations(agent_id, topic, limit).await
        }
        crate::mcp_memory_handlers::ReadScope::Conversation(None) => Ok(Vec::new()),
        // Every match (newest first), other conversations' rows dropped, then the limit.
        crate::mcp_memory_handlers::ReadScope::Conversation(Some(session)) => {
            match memory.search_successful_conversation_rows(agent_id, topic, None).await {
                Ok(rows) => {
                    let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
                    match memory.ids_hidden_from_conversation(agent_id, &ids, Some(session.as_str())).await {
                        Ok(hidden) => Ok(rows
                            .into_iter()
                            .filter(|(id, _)| !hidden.contains(id))
                            .map(|(_, content)| content)
                            .take(limit)
                            .collect()),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
    };
    match found {
        Ok(contents) => {
            if contents.is_empty() {
                serde_json::json!({
                    "content": [{"type": "text", "text": "No successful conversations found for this topic."}]
                })
            } else {
                let text = contents.join("\n---\n");
                serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                })
            }
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error searching conversations: {e}")}],
            "isError": true
        }),
    }
}

/// v1.83.0: employee-wide maintenance statistic; refused on a channel
/// conversation turn ([`crate::mcp_memory_handlers::CHANNEL_TURN_REFUSED`]).
pub(crate) async fn handle_memory_episodic_pressure(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let scope = crate::mcp_memory_handlers::mcp_read_scope(agent_id);
    handle_memory_episodic_pressure_in(params, memory, agent_id, &scope).await
}

/// [`handle_memory_episodic_pressure`] with an explicit scope.
pub(crate) async fn handle_memory_episodic_pressure_in(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
    scope: &crate::mcp_memory_handlers::ReadScope,
) -> Value {
    if let Some(refused) = crate::mcp_memory_handlers::refused_on_channel_turn(scope) {
        return refused;
    }
    let hours_ago = params
        .get("hours_ago")
        .and_then(|v| v.as_u64())
        .unwrap_or(24);
    let since = chrono::Utc::now() - chrono::Duration::hours(hours_ago as i64);
    let pressure = memory.episodic_pressure(agent_id, since).await;

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Episodic pressure (last {hours_ago}h): {pressure:.2}\n\
             Threshold for Meso reflection: 10.0\n\
             Status: {}",
            if pressure > 10.0 { "⚠ Above threshold — reflection recommended" }
            else { "✓ Below threshold" }
        )}]
    })
}

/// v1.83.0: employee-wide maintenance statistic; refused on a channel
/// conversation turn ([`crate::mcp_memory_handlers::CHANNEL_TURN_REFUSED`]).
pub(crate) async fn handle_memory_consolidation_status(memory: &SqliteMemoryEngine, agent_id: &str) -> Value {
    let scope = crate::mcp_memory_handlers::mcp_read_scope(agent_id);
    handle_memory_consolidation_status_in(memory, agent_id, &scope).await
}

/// [`handle_memory_consolidation_status`] with an explicit scope.
pub(crate) async fn handle_memory_consolidation_status_in(
    memory: &SqliteMemoryEngine,
    agent_id: &str,
    scope: &crate::mcp_memory_handlers::ReadScope,
) -> Value {
    if let Some(refused) = crate::mcp_memory_handlers::refused_on_channel_turn(scope) {
        return refused;
    }
    let conflict_count = memory.semantic_conflict_count(agent_id).await;

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Semantic conflict count: {conflict_count}\n\
             High-importance episodic memories not yet consolidated into semantic knowledge.\n\
             Status: {}",
            if conflict_count > 0 {
                format!("⚠ {conflict_count} unconsolidated observations — consolidation recommended")
            } else {
                "✓ No conflicts detected".to_string()
            }
        )}]
    })
}

/// Fix-2 M1: `[memory] novelty_gate` in `config.toml`, default `true`.
///
/// `DUDUCLAW_SEMANTIC_VECTORS=1` (below) controls one thing only — whether a
/// semantic embedder is attached at all, which gates the `w_vec`
/// **retrieval** ranking signal. Until this config key existed, that same
/// env var was ALSO the only lever controlling the B1 novelty gate
/// (`duduclaw_memory::novelty_gate`, arXiv:2606.29182) — a WRITE-time
/// rejection of near-duplicate semantic-layer memories — because
/// `SqliteMemoryEngine`'s default `NoveltyGateConfig` is `enabled: true` and
/// nothing ever overrode it. An operator who wanted better retrieval ranking
/// had no way to opt OUT of the separate, stricter behavior of writes
/// silently being rejected as duplicates. This key decouples the two:
/// semantic vectors can be on for retrieval while the write-time gate is
/// off, or vice versa (the gate is a documented no-op without an attached
/// embedder either way — see `novelty_gate.rs`'s "Hard invariant").
pub(crate) fn novelty_gate_enabled_from_config(home_dir: &Path) -> bool {
    let default = true;
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return default;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return default;
    };
    table
        .get("memory")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("novelty_gate"))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// `[memory] supersession_trust_guard` in `config.toml`, default `true`
/// (fails closed: absent / malformed / non-boolean all keep the guard on).
/// Mirrors `duduclaw_gateway::memory_factory::supersession_trust_guard_enabled_from_config`
/// — duplicated for the same reason as [`novelty_gate_enabled_from_config`].
pub(crate) fn supersession_trust_guard_enabled_from_config(home_dir: &Path) -> bool {
    let default = true;
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return default;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return default;
    };
    table
        .get("memory")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("supersession_trust_guard"))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Run the MCP server, reading JSON-RPC from stdin and writing responses to stdout.
/// Opt-in: attach the local char-n-gram semantic embedder (the `w_vec` memory
/// retrieval signal) when `DUDUCLAW_SEMANTIC_VECTORS=1`. Off by default →
/// ranking is byte-identical to the FTS/graph-only path. Zero API cost, fully
/// local. The dense-model (EmbeddingGemma) and sqlite-vec `vec0` backends are
/// the documented quality/scale upgrades.
///
/// Fix-2 M1: also wires `[memory] novelty_gate` (`config.toml`,
/// `home_dir`-scoped — see [`novelty_gate_enabled_from_config`]) into the
/// engine's `NoveltyGateConfig` unconditionally, independent of whether an
/// embedder ends up attached, so the config key's effect never silently
/// depends on call order.
pub(crate) fn maybe_with_semantic_embedder(
    engine: SqliteMemoryEngine,
    home_dir: &Path,
) -> SqliteMemoryEngine {
    let enabled = std::env::var("DUDUCLAW_SEMANTIC_VECTORS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let engine = if enabled {
        tracing::info!("semantic vector retrieval (w_vec) enabled: ngram-hash-v1");
        engine.with_embedder(std::sync::Arc::new(
            duduclaw_memory::NgramHashEmbedder::new(),
        ))
    } else {
        engine
    };
    let novelty_gate_enabled = novelty_gate_enabled_from_config(home_dir);
    if !novelty_gate_enabled {
        tracing::info!(
            "[memory] novelty_gate = false — B1 write-time near-duplicate rejection disabled"
        );
    }
    let mut engine = engine
        .with_novelty_gate_config(duduclaw_memory::NoveltyGateConfig {
            enabled: novelty_gate_enabled,
            ..duduclaw_memory::NoveltyGateConfig::default()
        })
        .with_supersession_trust_guard(supersession_trust_guard_enabled_from_config(home_dir));
    // v1.68.0: `[memory] graph_embed_seed` reaches agent recall (it used to
    // change only the dashboard's own memory search).
    engine.retrieval_weights =
        duduclaw_gateway::memory_factory::retrieval_weights_from_config(home_dir);
    engine
}

#[cfg(test)]
mod conversation_scope_tests {
    use super::*;
    use crate::mcp_memory_handlers::{ReadScope, CHANNEL_TURN_REFUSED};

    fn in_session(s: &str) -> ReadScope {
        ReadScope::Conversation(Some(s.to_string()))
    }

    fn text(v: &Value) -> String {
        v["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    /// An episodic, important row of conversation `session`, `minutes_ago` old.
    async fn episode(mem: &SqliteMemoryEngine, content: &str, session: &str, minutes_ago: i64) -> String {
        let entry = duduclaw_core::types::MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: "a1".into(),
            content: content.into(),
            timestamp: chrono::Utc::now() - chrono::Duration::minutes(minutes_ago),
            tags: vec![],
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Episodic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: "prediction_episodic".into(),
        };
        let prov = duduclaw_memory::lineage::Provenance::source(
            duduclaw_memory::SourceRef::channel_message(session, 1, chrono::Utc::now(), None),
        );
        mem.store_temporal("a1", entry, duduclaw_memory::TemporalMeta::default(), prov)
            .await
            .unwrap()
    }

    async fn successful(mem: &SqliteMemoryEngine, limit: u64, scope: &ReadScope) -> Value {
        handle_memory_successful_conversations_in(
            &serde_json::json!({ "topic": "narwhalgrove", "limit": limit }),
            mem,
            "a1",
            scope,
        )
        .await
    }

    /// (a) a turn in B gets none of A's rows; (b) a turn in A gets its own,
    /// and the scope applies before the limit (B's row is the newest match);
    /// an unknown conversation gets nothing; (c) a non-channel caller still
    /// gets every row.
    #[tokio::test]
    async fn successful_conversations_follow_the_conversation_scope() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        episode(&mem, "narwhalgrove beta secret", "telegram:B", 0).await;
        episode(&mem, "narwhalgrove alpha newer", "telegram:A", 10).await;
        episode(&mem, "narwhalgrove alpha older", "telegram:A", 20).await;

        let in_b = text(&successful(&mem, 10, &in_session("telegram:B")).await);
        assert!(in_b.contains("beta secret") && !in_b.contains("alpha"), "{in_b}");

        let in_a = text(&successful(&mem, 1, &in_session("telegram:A")).await);
        assert_eq!(in_a, "narwhalgrove alpha newer");

        let unknown = text(&successful(&mem, 10, &ReadScope::Conversation(None)).await);
        assert_eq!(unknown, "No successful conversations found for this topic.");

        let employee = text(&successful(&mem, 10, &ReadScope::Employee).await);
        for row in ["beta secret", "alpha newer", "alpha older"] {
            assert!(employee.contains(row), "{employee}");
        }
        assert_eq!(text(&successful(&mem, 1, &ReadScope::Employee).await), "narwhalgrove beta secret");
    }

    /// (a) every channel turn (known or unknown conversation) is refused with
    /// the fixed error; (c) a non-channel caller gets the statistics as before.
    #[tokio::test]
    async fn maintenance_statistics_refused_on_channel_turns() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        episode(&mem, "narwhalgrove alpha", "telegram:A", 0).await;
        for scope in [in_session("telegram:A"), in_session("telegram:B"), ReadScope::Conversation(None)] {
            let pressure = handle_memory_episodic_pressure_in(&serde_json::json!({}), &mem, "a1", &scope).await;
            assert_eq!(pressure["isError"], true, "{scope:?}");
            assert_eq!(text(&pressure), CHANNEL_TURN_REFUSED, "{scope:?}");
            let status = handle_memory_consolidation_status_in(&mem, "a1", &scope).await;
            assert_eq!(status["isError"], true, "{scope:?}");
            assert_eq!(text(&status), CHANNEL_TURN_REFUSED, "{scope:?}");
        }
        let pressure = handle_memory_episodic_pressure_in(&serde_json::json!({}), &mem, "a1", &ReadScope::Employee).await;
        assert!(pressure.get("isError").is_none(), "{pressure}");
        assert!(text(&pressure).starts_with("Episodic pressure (last 24h):"), "{pressure}");
        let status = handle_memory_consolidation_status_in(&mem, "a1", &ReadScope::Employee).await;
        assert!(status.get("isError").is_none(), "{status}");
        assert!(text(&status).starts_with("Semantic conflict count:"), "{status}");
    }
}

#[cfg(test)]
mod novelty_gate_config_tests {
    use super::*;

    #[test]
    fn defaults_to_enabled_when_config_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(novelty_gate_enabled_from_config(dir.path()));
    }

    #[test]
    fn defaults_to_enabled_when_section_or_key_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[other]\nfoo = 1\n").unwrap();
        assert!(novelty_gate_enabled_from_config(dir.path()));

        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\nother_key = true\n",
        )
        .unwrap();
        assert!(novelty_gate_enabled_from_config(dir.path()));
    }

    #[test]
    fn reads_explicit_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\nnovelty_gate = false\n",
        )
        .unwrap();
        assert!(!novelty_gate_enabled_from_config(dir.path()));
    }

    #[test]
    fn reads_explicit_true() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\nnovelty_gate = true\n",
        )
        .unwrap();
        assert!(novelty_gate_enabled_from_config(dir.path()));
    }

    #[test]
    fn malformed_config_fails_safe_to_enabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "this is :: not = valid = toml ===",
        )
        .unwrap();
        assert!(novelty_gate_enabled_from_config(dir.path()));
    }

    #[test]
    fn maybe_with_semantic_embedder_wires_supersession_trust_guard() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let on = maybe_with_semantic_embedder(SqliteMemoryEngine::new(&db).unwrap(), dir.path());
        assert!(on.supersession_trust_guard, "default must be on");
        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\nsupersession_trust_guard = false\n",
        )
        .unwrap();
        let off = maybe_with_semantic_embedder(SqliteMemoryEngine::new(&db).unwrap(), dir.path());
        assert!(!off.supersession_trust_guard);
    }

    #[test]
    fn maybe_with_semantic_embedder_applies_graph_embed_seed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\ngraph_embed_seed = true\n",
        )
        .unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let engine = maybe_with_semantic_embedder(engine, dir.path());
        assert!(engine.retrieval_weights.graph_embed_seed);
    }

    /// Fix-2 M1 end-to-end: `maybe_with_semantic_embedder` wires the config
    /// key into the engine's `NoveltyGateConfig` regardless of whether the
    /// semantic embedder itself is attached (env var untouched here — this
    /// only exercises the novelty-gate half of the function).
    #[test]
    fn maybe_with_semantic_embedder_wires_novelty_gate_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\nnovelty_gate = false\n",
        )
        .unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let engine = maybe_with_semantic_embedder(engine, dir.path());
        assert!(
            !engine.novelty_gate.enabled,
            "novelty_gate=false in config.toml must reach the engine"
        );
    }
}
