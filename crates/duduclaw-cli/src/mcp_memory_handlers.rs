// mcp_memory_handlers.rs — Namespace-aware memory endpoints for MCP server (W19-P0 M1)
//
// Implements three MCP memory endpoints with full namespace isolation:
//
//   memory_store  — server-side namespace injection, daily quota enforcement
//   memory_search — scoped strictly to caller's own namespace
//   memory_read   — ownership verification, 403 on cross-namespace access
//
// TL spec (2026-04-29):
//   • External clients → namespace "external/{client_id}" (server-side injected)
//   • Callers CANNOT supply or override the namespace field
//   • Per-client write quota: 1 000 records / day (→ 429 on exceeded)
//   • internal/* namespaces are inaccessible to external clients (→ 403)

use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::MemoryEntry;
use duduclaw_memory::{CodeMap, CodeMapConfig, SqliteMemoryEngine};
use serde_json::Value;
use std::path::PathBuf;

use crate::mcp_memory_quota::DailyQuota;
use crate::mcp_namespace::NamespaceContext;

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Extract client_id from namespace context.
/// "external/foo" → "foo"; all others → full write_namespace string.
fn client_id_from_ns(ns_ctx: &NamespaceContext) -> &str {
    ns_ctx
        .write_namespace
        .strip_prefix("external/")
        .unwrap_or(&ns_ctx.write_namespace)
}

/// Parse a JSON value (array or comma-separated string) into a `Vec<String>`.
///
/// Accepts both formats for backward compatibility:
///   - `["tag1", "tag2"]`  (array — preferred)
///   - `"tag1, tag2"`       (legacy comma-separated string)
fn parse_tags_value(v: &Value) -> Vec<String> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Build a standard MCP error response (`isError = true`).
fn mcp_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

/// Build a 403 Forbidden MCP error response.
fn mcp_forbidden(detail: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": format!("403 Forbidden: {detail}") }],
        "isError": true,
        "error_code": 403
    })
}

/// Build a 429 Too Many Requests MCP error response.
fn mcp_quota_exceeded(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": format!("429 Too Many Requests: {msg}") }],
        "isError": true,
        "error_code": 429
    })
}

// ── memory_store ──────────────────────────────────────────────────────────────

/// Store a memory entry in the caller's namespace.
///
/// # Namespace enforcement
/// The `namespace` is **always** derived from `ns_ctx.write_namespace`; any
/// `namespace` or `agent_id` field the caller may have supplied was stripped
/// upstream (in `run_mcp_server`) before reaching this function.
///
/// # Parameters (from `params`)
/// - `content`   : `string`   — required
/// - `tags`      : `array`    — optional (also accepts comma-separated string)
/// - `ttl_days`  : `integer`  — optional (stored as metadata tag for now)
///
/// # Returns
/// ```json
/// { "id": "...", "namespace": "...", "stored_at": "<ISO 8601>" }
/// ```
pub async fn handle_memory_store(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
    quota: &DailyQuota,
) -> Value {
    // ── Validate required field ───────────────────────────────────────────────
    let content = match params.get("content").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c,
        _ => return mcp_error("Missing required parameter: content"),
    };

    // ── Optional fields ───────────────────────────────────────────────────────
    let mut tags = params.get("tags").map(parse_tags_value).unwrap_or_default();

    // ttl_days: no TTL column exists in MemoryEntry yet; attach as a tag so the
    // intent survives round-trips until a proper TTL column lands.
    if let Some(ttl) = params.get("ttl_days").and_then(|v| v.as_u64()) {
        tags.push(format!("ttl:{ttl}"));
    }

    // ── Namespace is server-side injected (TL裁定 2026-04-29) ─────────────────
    let namespace = ns_ctx.write_namespace.clone();
    let client_id = client_id_from_ns(ns_ctx);

    // ── Daily quota check → 429 on exceeded ──────────────────────────────────
    if let Err(e) = quota.check_and_increment(client_id) {
        return mcp_quota_exceeded(&e.to_string());
    }

    // ── Classify and build entry ──────────────────────────────────────────────
    let classification = duduclaw_memory::classify(content, "user_input");
    let entry_id = uuid::Uuid::new_v4().to_string();
    let stored_at = chrono::Utc::now();

    // source_event signals provenance for future analytics / auditing.
    let source_event = if namespace.starts_with("external/") {
        "mcp_external".to_string()
    } else {
        "mcp_internal".to_string()
    };

    let entry = MemoryEntry {
        id: entry_id.clone(),
        agent_id: namespace.clone(),
        content: content.to_string(),
        timestamp: stored_at,
        tags,
        embedding: None,
        layer: classification.layer,
        importance: classification.importance,
        access_count: 0,
        last_accessed: None,
        source_event,
    };

    // store() uses the `agent_id` parameter — not entry.agent_id — for the SQL
    // INSERT, so namespace enforcement is doubly guaranteed.
    match memory.store(&namespace, entry).await {
        Ok(()) => {
            // MCP spec requires top-level `memory_id` for client-side chaining
            // (e.g. immediate memory_read after memory_store).
            // `id` is preserved for backward compat; `memory_id` is the canonical field.
            let payload = serde_json::json!({
                "memory_id": entry_id,
                "id": entry_id,
                "namespace": namespace,
                "stored_at": stored_at.to_rfc3339(),
            });
            serde_json::json!({
                "memory_id": entry_id,
                "content": [{ "type": "text", "text": payload.to_string() }]
            })
        }
        Err(e) => mcp_error(&format!("Error storing memory: {e}")),
    }
}

// ── memory_search ─────────────────────────────────────────────────────────────

/// Search memories within the caller's namespace only.
///
/// Scope is strictly limited to `ns_ctx.write_namespace`; callers cannot
/// expand the search to other namespaces.
///
/// # Parameters (from `params`)
/// - `query`  : `string`  — required
/// - `limit`  : `integer` — optional (default 10)
/// - `tags`   : `array`   — optional, post-filter on result set
///
/// # Returns
/// ```json
/// { "results": [ { "id": ..., "content": ..., ... } ], "total": N }
/// ```
pub async fn handle_memory_search(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q,
        _ => return mcp_error("Missing required parameter: query"),
    };

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .min(100) as usize; // cap at 100 to prevent runaway queries

    let filter_tags = params.get("tags").map(parse_tags_value).unwrap_or_default();

    // Scope enforced: search only within caller's write namespace.
    let namespace = &ns_ctx.write_namespace;

    match memory.search(namespace, query, limit * 4).await {
        Ok(entries) => {
            // Post-filter by tags (engine search doesn't support tag filter natively).
            let mut results: Vec<Value> = entries
                .into_iter()
                .filter(|e| {
                    if filter_tags.is_empty() {
                        true
                    } else {
                        filter_tags.iter().any(|t| e.tags.contains(t))
                    }
                })
                .take(limit)
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "content": e.content,
                        "namespace": e.agent_id,
                        "tags": e.tags,
                        "layer": format!("{:?}", e.layer),
                        "importance": e.importance,
                        "created_at": e.timestamp.to_rfc3339(),
                        "source_event": e.source_event,
                    })
                })
                .collect();

            results.truncate(limit);
            let total = results.len();

            let payload = serde_json::json!({ "results": results, "total": total });
            serde_json::json!({
                "content": [{ "type": "text", "text": payload.to_string() }]
            })
        }
        Err(e) => mcp_error(&format!("Error searching memory: {e}")),
    }
}

// ── memory_read ───────────────────────────────────────────────────────────────

/// Read a single memory entry by ID.
///
/// # Access control
/// Returns **403 Forbidden** if:
///   - The entry does not exist.
///   - The entry belongs to a different namespace (cross-namespace isolation).
///   - The caller is an external client and the entry is in `internal/*`.
///
/// DB-level enforcement: `get_by_id` filters by `agent_id`, so cross-namespace
/// reads always return `None` regardless of the ID value.
///
/// # Parameters
/// - `id` : `string` — required (UUID returned by memory_store)
///
/// # Returns
/// Complete memory record on success; 403 on access denial.
pub async fn handle_memory_read(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    // Accept both "id" (M1 spec) and "memory_id" (legacy tool definition name)
    // for backward compatibility with existing MCP clients.
    let memory_id = match params
        .get("id")
        .or_else(|| params.get("memory_id"))
        .and_then(|v| v.as_str())
    {
        Some(id) if !id.trim().is_empty() => id,
        _ => return mcp_error("Missing required parameter: id"),
    };

    // Caller's namespace is the lookup key — get_by_id enforces ownership.
    let namespace = &ns_ctx.write_namespace;

    match memory.get_by_id(namespace, memory_id).await {
        Ok(Some(entry)) => {
            // Belt-and-suspenders: even though get_by_id already filters by
            // agent_id, verify the stored agent_id matches exactly.
            if entry.agent_id != *namespace {
                return mcp_forbidden("access denied to this memory entry");
            }
            let payload = serde_json::json!({
                "id":           entry.id,
                "namespace":    entry.agent_id,
                "content":      entry.content,
                "tags":         entry.tags,
                "layer":        format!("{:?}", entry.layer),
                "importance":   entry.importance,
                "access_count": entry.access_count,
                "created_at":   entry.timestamp.to_rfc3339(),
                "source_event": entry.source_event,
            });
            serde_json::json!({
                "content": [{ "type": "text", "text": payload.to_string() }]
            })
        }
        Ok(None) => mcp_forbidden(&format!(
            "memory not found or access denied: {memory_id}"
        )),
        Err(e) => mcp_error(&format!("Error reading memory: {e}")),
    }
}

// ── memory_fetch_batch (F3) ─────────────────────────────────────────────────────

/// Fetch multiple memory entries by ID in a single call (F3 batch fetch).
///
/// # Access control
/// Scope is strictly limited to `ns_ctx.write_namespace`; the engine's
/// `get_by_ids` enforces ownership, so entries belonging to another namespace
/// never appear in the result. Missing-vs-forbidden is intentionally
/// indistinguishable: both land in `missing_ids` (no existence leak).
///
/// # Parameters
/// - `ids`              : `array<string>` — required, max 100 IDs
/// - `include_metadata` : `bool`          — optional (default false)
///
/// # Returns
/// ```json
/// { "memories": [...], "missing_ids": [...], "total_found": N, "total_missing": M }
/// ```
/// Partial hits are NOT an error — found entries plus a `missing_ids` list.
pub async fn handle_memory_fetch_batch(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let ids: Vec<String> = match params.get("ids") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => return mcp_error("Missing required parameter: ids (array of memory IDs)"),
    };

    if ids.is_empty() {
        return mcp_error("Parameter 'ids' must be a non-empty array of memory IDs");
    }
    if ids.len() > 100 {
        return mcp_error("Parameter 'ids' exceeds the maximum of 100 entries per request");
    }

    let include_metadata = params
        .get("include_metadata")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let namespace = &ns_ctx.write_namespace;

    match memory.get_by_ids(namespace, &ids).await {
        Ok(entries) => {
            let found_ids: std::collections::HashSet<&str> =
                entries.iter().map(|e| e.id.as_str()).collect();
            let missing_ids: Vec<&String> = ids
                .iter()
                .filter(|id| !found_ids.contains(id.as_str()))
                .collect();

            let memories: Vec<Value> = entries
                .iter()
                .map(|e| {
                    let mut obj = serde_json::json!({
                        "id": e.id,
                        "content": e.content,
                        "namespace": e.agent_id,
                        "layer": format!("{:?}", e.layer),
                        "found": true,
                    });
                    if include_metadata {
                        obj["tags"] = serde_json::json!(e.tags);
                        obj["importance"] = serde_json::json!(e.importance);
                        obj["access_count"] = serde_json::json!(e.access_count);
                        obj["created_at"] = serde_json::json!(e.timestamp.to_rfc3339());
                        obj["source_event"] = serde_json::json!(e.source_event);
                    }
                    obj
                })
                .collect();

            let total_found = memories.len();
            let total_missing = missing_ids.len();
            let payload = serde_json::json!({
                "memories": memories,
                "missing_ids": missing_ids,
                "total_found": total_found,
                "total_missing": total_missing,
            });
            serde_json::json!({
                "content": [{ "type": "text", "text": payload.to_string() }]
            })
        }
        Err(e) => mcp_error(&format!("Error fetching memories: {e}")),
    }
}

// ── memory_get_history / memory_get_at / memory_invalidate_by_origin (D1) ───────

/// Return the full temporal supersession chain for a `(subject, predicate)`
/// triple within the caller's namespace (D1 bi-temporal read).
///
/// Scope is limited to `ns_ctx.write_namespace`; the engine query filters by
/// `agent_id`, so chains from other namespaces never surface.
pub async fn handle_memory_get_history(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let subject = match params.get("subject").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => return mcp_error("Missing required parameter: subject"),
    };
    let predicate = match params.get("predicate").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => return mcp_error("Missing required parameter: predicate"),
    };
    let namespace = &ns_ctx.write_namespace;

    match memory.get_history(namespace, subject, predicate).await {
        Ok(records) => {
            let total = records.len();
            let payload = serde_json::json!({
                "subject": subject,
                "predicate": predicate,
                "records": records,
                "total": total,
            });
            serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
        }
        Err(e) => mcp_error(&format!("Error reading history: {e}")),
    }
}

/// Point-in-time lookup: the fact for a `(subject, predicate)` triple valid at
/// `at` (RFC3339), scoped to the caller's namespace (D1 bi-temporal read).
pub async fn handle_memory_get_at(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let subject = match params.get("subject").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => return mcp_error("Missing required parameter: subject"),
    };
    let predicate = match params.get("predicate").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => return mcp_error("Missing required parameter: predicate"),
    };
    let at = match params.get("at").and_then(|v| v.as_str()) {
        Some(a) if !a.trim().is_empty() => {
            match chrono::DateTime::parse_from_rfc3339(a.trim()) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(e) => return mcp_error(&format!("Invalid 'at' (must be RFC3339): {e}")),
            }
        }
        _ => return mcp_error("Missing required parameter: at (RFC3339 timestamp)"),
    };
    let namespace = &ns_ctx.write_namespace;

    match memory.get_at(namespace, subject, predicate, at).await {
        Ok(Some(record)) => {
            let payload = serde_json::json!({
                "subject": subject,
                "predicate": predicate,
                "at": at.to_rfc3339(),
                "record": record,
                "found": true,
            });
            serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
        }
        Ok(None) => {
            let payload = serde_json::json!({
                "subject": subject,
                "predicate": predicate,
                "at": at.to_rfc3339(),
                "record": Value::Null,
                "found": false,
            });
            serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
        }
        Err(e) => mcp_error(&format!("Error in point-in-time lookup: {e}")),
    }
}

/// Rollback primitive: expire (never delete) every currently-valid fact from an
/// exact `origin` within the caller's namespace, optionally limited to facts
/// learned at/after `since` (RFC3339). Cascades a trust downgrade to derived
/// facts. Admin-scoped at the dispatch layer (D1).
pub async fn handle_memory_invalidate_by_origin(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let origin = match params.get("origin").and_then(|v| v.as_str()) {
        Some(o) if !o.trim().is_empty() => o.trim(),
        _ => return mcp_error("Missing required parameter: origin"),
    };
    let since = match params.get("since").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => {
            match chrono::DateTime::parse_from_rfc3339(s.trim()) {
                Ok(dt) => Some(dt.with_timezone(&chrono::Utc)),
                Err(e) => return mcp_error(&format!("Invalid 'since' (must be RFC3339): {e}")),
            }
        }
        _ => None,
    };
    let namespace = &ns_ctx.write_namespace;

    match memory.invalidate_by_origin(namespace, origin, since).await {
        Ok(expired) => {
            let payload = serde_json::json!({
                "origin": origin,
                "since": since.map(|t| t.to_rfc3339()),
                "expired": expired,
            });
            serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
        }
        Err(e) => mcp_error(&format!("Error invalidating by origin: {e}")),
    }
}

// ── memory_alias_add / memory_alias_list (D3.2 entity alias) ────────────────────

/// Add an entity alias for the caller's namespace (D3.2). Collapses a surface
/// form (`alias`) onto a `canonical` entity so graph seeding treats
/// "老闆/李老闆/zhixu" as one node. Both sides are normalized (trim + lowercase)
/// and alias chains are flattened by the engine.
///
/// # Parameters
/// - `canonical` : `string` — required, the entity to keep
/// - `alias`     : `string` — required, the surface form to fold in
pub async fn handle_memory_alias_add(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let canonical = match params.get("canonical").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c,
        _ => return mcp_error("Missing required parameter: canonical"),
    };
    let alias = match params.get("alias").and_then(|v| v.as_str()) {
        Some(a) if !a.trim().is_empty() => a,
        _ => return mcp_error("Missing required parameter: alias"),
    };
    let namespace = &ns_ctx.write_namespace;
    match memory.add_entity_alias(namespace, canonical, alias).await {
        Ok(()) => {
            let payload = serde_json::json!({
                "namespace": namespace,
                "canonical": canonical.trim().to_lowercase(),
                "alias": alias.trim().to_lowercase(),
                "added": true,
            });
            serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
        }
        Err(e) => mcp_error(&format!("Error adding entity alias: {e}")),
    }
}

/// List the caller namespace's entity aliases (D3.2) as `(canonical, alias)`
/// pairs. No parameters.
pub async fn handle_memory_alias_list(
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let namespace = &ns_ctx.write_namespace;
    match memory.list_entity_aliases(namespace).await {
        Ok(pairs) => {
            let aliases: Vec<Value> = pairs
                .iter()
                .map(|(canonical, alias)| {
                    serde_json::json!({ "canonical": canonical, "alias": alias })
                })
                .collect();
            let payload = serde_json::json!({
                "namespace": namespace,
                "aliases": aliases,
                "total": pairs.len(),
            });
            serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
        }
        Err(e) => mcp_error(&format!("Error listing entity aliases: {e}")),
    }
}

// ── memory_improve (RFC-26 §4.4 / P6.4) ────────────────────────────────────────

/// Group memory entries by their tags into `(tag, contents)` clusters, largest
/// cluster first. Untagged entries collect under `"(untagged)"`. Pure + testable.
fn cluster_by_tag(entries: &[MemoryEntry]) -> Vec<(String, Vec<String>)> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in entries {
        let snippet = duduclaw_core::truncate_bytes(&e.content, 240).to_string();
        if e.tags.is_empty() {
            map.entry("(untagged)".to_string()).or_default().push(snippet);
        } else {
            for t in &e.tags {
                map.entry(t.clone()).or_default().push(snippet.clone());
            }
        }
    }
    let mut clusters: Vec<(String, Vec<String>)> = map.into_iter().collect();
    // Largest clusters first (most repeated theme = best consolidation candidate).
    clusters.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    clusters
}

/// `memory_improve` — reflection data-provider. Gathers memories related to a
/// `topic`, clusters them, and returns a **proposal scaffold** for the calling
/// agent to draft consolidated MEMORY/SOUL rules. Writes nothing: the agent
/// reviews then persists via `memory_store` (propose-not-apply).
pub async fn handle_memory_improve(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let topic = match params.get("topic").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim(),
        _ => return mcp_error("Missing required parameter: topic (the area to reflect on)"),
    };
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(40)
        .min(100) as usize;
    let namespace = &ns_ctx.write_namespace;

    let entries = match memory.search(namespace, topic, limit).await {
        Ok(e) => e,
        Err(e) => return mcp_error(&format!("Error gathering memories: {e}")),
    };
    if entries.is_empty() {
        let payload = serde_json::json!({
            "topic": topic,
            "clusters": [],
            "proposal_scaffold": format!(
                "No memories found for '{topic}'. Nothing to consolidate yet."
            ),
        });
        return serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] });
    }

    let clusters = cluster_by_tag(&entries);
    let cluster_json: Vec<Value> = clusters
        .iter()
        .map(|(tag, contents)| {
            serde_json::json!({ "tag": tag, "count": contents.len(), "samples": contents })
        })
        .collect();

    let payload = serde_json::json!({
        "topic": topic,
        "memories_examined": entries.len(),
        "clusters": cluster_json,
        "proposal_scaffold": format!(
            "Reflexion over {} memories about '{topic}'. For each cluster above, draft ONE \
             consolidated rule capturing the recurring lesson, then — after the user confirms — \
             persist it with memory_store (layer=semantic) or propose a SOUL.md edit. \
             Do NOT auto-apply; these are candidates for review.",
            entries.len()
        ),
    });
    serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
}

// ── code_map: Aider-style repo symbol graph ────────────────────────────────────

/// Rank a repository's source files by relevance to `query` using the
/// Personalized-PageRank code symbol graph (tree-sitter + `graph_rank` PPR).
///
/// # Parameters (from `params`)
/// - `query`       : `string` — required; natural-language / identifier query.
/// - `root`        : `string` — optional; repo root to scan (default: cwd).
/// - `max_files`   : `integer` — optional (default 15, cap 100).
/// - `chat_files`  : `array<string>` — optional; repo-relative paths already in
///   context; their defined symbols seed the walk (Aider personalization).
///
/// # Returns
/// ```json
/// { "map": "<text>", "files": [ { "path", "score", "symbols":[...] } ],
///   "indexed_files": N, "indexed_symbols": M }
/// ```
///
/// Note: the map is rebuilt per call (no cache yet); the scan is gitignore-aware
/// and bounded by per-file size. Runs the CPU-bound parse on a blocking thread.
pub async fn handle_code_map(params: &Value) -> Value {
    let query = params
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    // A non-empty query OR chat_files is required to have something to seed on.
    let chat_files: Vec<String> = params
        .get("chat_files")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if query.trim().is_empty() && chat_files.is_empty() {
        return mcp_error("code_map requires a non-empty 'query' or 'chat_files'");
    }

    let root = params
        .get("root")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let max_files = params
        .get("max_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(15)
        .clamp(1, 100) as usize;

    let cfg = CodeMapConfig::new(root);
    // CPU-bound directory walk + parse: keep it off the async reactor.
    let built = tokio::task::spawn_blocking(move || CodeMap::build(&cfg)).await;

    let map = match built {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => return mcp_error(&format!("code_map build failed: {e}")),
        Err(e) => return mcp_error(&format!("code_map task join failed: {e}")),
    };

    let ranked = map.rank(&query, &chat_files, max_files);
    let text = map.render_map(
        &query,
        &chat_files,
        max_files,
        duduclaw_memory::code_map::DEFAULT_SYMBOLS_PER_FILE,
    );

    let payload = serde_json::json!({
        "map": text,
        "files": ranked,
        "indexed_files": map.file_count(),
        "indexed_symbols": map.symbol_count(),
    });
    serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
}

// ── user_profile: cross-session per-user preference traits ─────────────────────

/// Wrap a JSON payload in the MCP text-content envelope.
fn mcp_text(payload: Value) -> Value {
    serde_json::json!({ "content": [{ "type": "text", "text": payload.to_string() }] })
}

/// Record (or update) one preference trait about a user. Re-recording the same
/// `predicate` supersedes the prior value via the temporal chain. The agent is
/// the server-injected write namespace (never client-supplied).
///
/// Params: `user_id`, `predicate`, `value` (all required, non-empty);
/// `origin_trust` (optional f64 in `[0,1]`, default 1.0).
pub async fn handle_user_profile_record(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return mcp_error("Missing required parameter: user_id"),
    };
    let predicate = match params.get("predicate").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return mcp_error("Missing required parameter: predicate"),
    };
    let value = match params.get("value").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return mcp_error("Missing required parameter: value"),
    };
    let origin_trust = params
        .get("origin_trust")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let namespace = ns_ctx.write_namespace.clone();
    match duduclaw_memory::user_profile::record_trait(
        memory,
        &namespace,
        user_id,
        predicate,
        value,
        origin_trust,
    )
    .await
    {
        Ok(id) => mcp_text(serde_json::json!({
            "memory_id": id,
            "user_id": user_id,
            "predicate": predicate,
        })),
        Err(e) => mcp_error(&format!("user_profile_record failed: {e}")),
    }
}

/// Fetch a user's currently-valid profile traits + the rendered
/// `## About This User` block (the same bytes injected into the reply prompt).
///
/// Params: `user_id` (required). Agent = the server-injected namespace.
pub async fn handle_user_profile_get(
    params: &Value,
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return mcp_error("Missing required parameter: user_id"),
    };
    let namespace = ns_ctx.write_namespace.clone();
    match duduclaw_memory::user_profile::profile_traits(memory, &namespace, user_id).await {
        Ok(traits) => {
            let items: Vec<Value> = traits
                .iter()
                .map(|t| serde_json::json!({ "predicate": t.predicate, "value": t.value }))
                .collect();
            let block = duduclaw_memory::user_profile::render_profile_block(&traits);
            mcp_text(serde_json::json!({
                "user_id": user_id,
                "traits": items,
                "block": block,
            }))
        }
        Err(e) => mcp_error(&format!("user_profile_get failed: {e}")),
    }
}

/// Compile the calling agent's user-as-code profile: typed preference rules
/// (deterministically parsed from currently-valid `user:*` SPO triples and
/// `user-profile`-tagged entries), unresolved conflicts, and the count of
/// rows the parsers could not type. Read-only; the agent is the
/// server-injected write namespace (never client-supplied). No parameters.
pub async fn handle_user_code_profile(
    memory: &SqliteMemoryEngine,
    ns_ctx: &NamespaceContext,
) -> Value {
    match duduclaw_memory::compile_user_profile(&ns_ctx.write_namespace, memory).await {
        Ok(profile) => match serde_json::to_value(&profile) {
            Ok(v) => mcp_text(v),
            Err(e) => mcp_error(&format!("user_code_profile serialization failed: {e}")),
        },
        Err(e) => mcp_error(&format!("user_code_profile failed: {e}")),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_memory::SqliteMemoryEngine;

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn external_ns(client_id: &str) -> NamespaceContext {
        NamespaceContext {
            write_namespace: format!("external/{client_id}"),
            read_namespaces: vec![
                format!("external/{client_id}"),
                "shared/public".to_string(),
            ],
        }
    }

    fn mk_entry(content: &str, tags: &[&str]) -> MemoryEntry {
        MemoryEntry {
            id: "x".into(),
            agent_id: "internal/a1".into(),
            content: content.into(),
            timestamp: chrono::Utc::now(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Episodic,
            importance: 0.5,
            access_count: 0,
            last_accessed: None,
            source_event: String::new(),
        }
    }

    #[test]
    fn cluster_by_tag_groups_and_orders_by_size() {
        let entries = vec![
            mk_entry("a", &["billing", "refund"]),
            mk_entry("b", &["billing"]),
            mk_entry("c", &["billing"]),
            mk_entry("d", &[]),
        ];
        let clusters = super::cluster_by_tag(&entries);
        // billing (3) should come before refund (1) and (untagged) (1).
        assert_eq!(clusters[0].0, "billing");
        assert_eq!(clusters[0].1.len(), 3);
        assert!(clusters.iter().any(|(t, _)| t == "(untagged)"));
        assert!(clusters.iter().any(|(t, _)| t == "refund"));
    }

    #[test]
    fn cluster_by_tag_empty_is_empty() {
        assert!(super::cluster_by_tag(&[]).is_empty());
    }

    fn internal_ns(agent_id: &str) -> NamespaceContext {
        NamespaceContext {
            write_namespace: format!("internal/{agent_id}"),
            read_namespaces: vec![
                format!("internal/{agent_id}"),
                "shared/public".to_string(),
            ],
        }
    }

    fn params(json: serde_json::Value) -> Value {
        json
    }

    fn text_of(v: &Value) -> String {
        v["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn is_error(v: &Value) -> bool {
        v.get("isError")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
    }

    fn error_code(v: &Value) -> Option<u64> {
        v.get("error_code").and_then(|x| x.as_u64())
    }

    // ── code_map handler: live end-to-end over real crate source ──────────────
    #[tokio::test]
    async fn code_map_handler_ranks_real_source() {
        // Point at this crate's sibling memory crate src (stable real Rust).
        let root = format!("{}/../duduclaw-memory/src", env!("CARGO_MANIFEST_DIR"));
        let resp = handle_code_map(&params(serde_json::json!({
            "query": "TripleGraph personalized_pagerank",
            "root": root,
            "max_files": 5
        })))
        .await;
        let payload: Value = serde_json::from_str(&text_of(&resp)).unwrap();
        assert!(payload["indexed_symbols"].as_u64().unwrap() >= 50);
        let files = payload["files"].as_array().unwrap();
        assert!(!files.is_empty(), "should rank real files");
        assert_eq!(files[0]["path"].as_str().unwrap(), "graph_rank.rs");
        assert!(payload["map"].as_str().unwrap().contains("graph_rank.rs"));
    }

    #[tokio::test]
    async fn code_map_handler_requires_query_or_chat_files() {
        let resp = handle_code_map(&params(serde_json::json!({}))).await;
        assert!(text_of(&resp).contains("requires") || is_error(&resp));
    }

    // ── M1-T1: Namespace injected as "external/{client_id}" ───────────────────
    // Verifies server-side namespace injection: the stored entry's namespace
    // must match ns_ctx.write_namespace, not any value supplied by the caller.
    #[tokio::test]
    async fn namespace_is_injected_as_external_client_id() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns = external_ns("claude-desktop");

        let resp = handle_memory_store(
            &params(serde_json::json!({ "content": "test memory" })),
            &mem,
            &ns,
            &quota,
        )
        .await;

        assert!(!is_error(&resp), "store should succeed: {resp}");

        // Parse the returned payload
        let text = text_of(&resp);
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            payload["namespace"].as_str().unwrap(),
            "external/claude-desktop",
            "namespace must be server-injected, not caller-supplied"
        );
        assert!(!payload["id"].as_str().unwrap().is_empty());
        assert!(!payload["stored_at"].as_str().unwrap().is_empty());
    }

    // ── M1-T2: Caller cannot override namespace ───────────────────────────────
    // The upstream dispatcher strips any caller-supplied "namespace" field, but
    // even if one arrives, the handler must use ns_ctx, not params.
    #[tokio::test]
    async fn caller_cannot_override_namespace() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns = external_ns("trusted-client");

        // Adversarial: caller tries to write into "internal/admin"
        let resp = handle_memory_store(
            &params(serde_json::json!({
                "content": "injected content",
                "namespace": "internal/admin"  // must be ignored
            })),
            &mem,
            &ns,
            &quota,
        )
        .await;

        assert!(!is_error(&resp));
        let text = text_of(&resp);
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            payload["namespace"].as_str().unwrap(),
            "external/trusted-client",
            "handler must use ns_ctx, not caller-supplied namespace"
        );
    }

    // ── M1-T3: Cross-namespace isolation → 403 ───────────────────────────────
    // Client A stores a record. Client B tries to read it by ID → must get 403.
    #[tokio::test]
    async fn cross_namespace_read_returns_403() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns_a = external_ns("client-a");
        let ns_b = external_ns("client-b");

        // Client A stores a memory
        let store_resp = handle_memory_store(
            &params(serde_json::json!({ "content": "secret of client-a" })),
            &mem,
            &ns_a,
            &quota,
        )
        .await;

        let store_text = text_of(&store_resp);
        let store_payload: Value = serde_json::from_str(&store_text).unwrap();
        let entry_id = store_payload["id"].as_str().unwrap().to_string();

        // Client B tries to read Client A's entry by ID
        let read_resp = handle_memory_read(
            &params(serde_json::json!({ "id": entry_id })),
            &mem,
            &ns_b,
        )
        .await;

        assert!(is_error(&read_resp), "cross-namespace read must fail");
        assert_eq!(
            error_code(&read_resp),
            Some(403),
            "must return 403, not some other error"
        );
    }

    // ── M1-T4: Write quota exceeded → 429 ────────────────────────────────────
    #[tokio::test]
    async fn write_quota_exceeded_returns_429() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        // Tiny quota for fast testing
        let quota = DailyQuota::with_limit(2);
        let ns = external_ns("quota-test");

        // First two writes succeed
        for _ in 0..2 {
            let r = handle_memory_store(
                &params(serde_json::json!({ "content": "fill quota" })),
                &mem,
                &ns,
                &quota,
            )
            .await;
            assert!(!is_error(&r), "write within quota should succeed");
        }

        // Third write must be rejected with 429
        let r = handle_memory_store(
            &params(serde_json::json!({ "content": "over limit" })),
            &mem,
            &ns,
            &quota,
        )
        .await;

        assert!(is_error(&r), "write beyond quota must fail");
        assert_eq!(
            error_code(&r),
            Some(429),
            "must return 429 Too Many Requests"
        );
        assert!(
            text_of(&r).contains("429"),
            "error message must mention 429"
        );
    }

    // ── M1-T5: Reading internal/* namespace as external → 403 ────────────────
    // An external client who obtains an ID from the internal namespace
    // (e.g., through a side-channel) must be denied.
    #[tokio::test]
    async fn external_client_cannot_read_internal_namespace() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let internal = internal_ns("system");
        let external = external_ns("attacker");

        // Internal agent stores a record
        let store_resp = handle_memory_store(
            &params(serde_json::json!({ "content": "internal secret" })),
            &mem,
            &internal,
            &quota,
        )
        .await;
        let store_text = text_of(&store_resp);
        let store_payload: Value = serde_json::from_str(&store_text).unwrap();
        let internal_id = store_payload["id"].as_str().unwrap().to_string();

        // External client attempts to read internal entry
        let read_resp = handle_memory_read(
            &params(serde_json::json!({ "id": internal_id })),
            &mem,
            &external, // external namespace — wrong agent_id
        )
        .await;

        assert!(is_error(&read_resp), "external client must not read internal memory");
        assert_eq!(
            error_code(&read_resp),
            Some(403),
            "must return 403 Forbidden"
        );
    }

    // ── T6: memory_store missing content returns error ────────────────────────
    #[tokio::test]
    async fn store_missing_content_returns_error() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::new();
        let ns = external_ns("test");

        let resp = handle_memory_store(
            &params(serde_json::json!({})),
            &mem,
            &ns,
            &quota,
        )
        .await;

        assert!(is_error(&resp));
        assert!(text_of(&resp).contains("content"));
    }

    // ── T7: memory_search returns structured results ───────────────────────────
    #[tokio::test]
    async fn search_returns_structured_results() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns = external_ns("searcher");

        // Store a memory first
        handle_memory_store(
            &params(serde_json::json!({ "content": "rust programming language" })),
            &mem,
            &ns,
            &quota,
        )
        .await;

        let resp = handle_memory_search(
            &params(serde_json::json!({ "query": "rust" })),
            &mem,
            &ns,
        )
        .await;

        assert!(!is_error(&resp));
        let text = text_of(&resp);
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert!(
            payload["results"].is_array(),
            "results must be an array"
        );
        assert!(
            payload["total"].is_number(),
            "total must be a number"
        );
    }

    // ── T8: memory_search is scoped to caller namespace ───────────────────────
    #[tokio::test]
    async fn search_scoped_to_caller_namespace() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns_a = external_ns("searcher-a");
        let ns_b = external_ns("searcher-b");

        // Client A stores a memory
        handle_memory_store(
            &params(serde_json::json!({ "content": "unique keyword xqzwvp" })),
            &mem,
            &ns_a,
            &quota,
        )
        .await;

        // Client B searches for that keyword — must find nothing
        let resp = handle_memory_search(
            &params(serde_json::json!({ "query": "xqzwvp" })),
            &mem,
            &ns_b,
        )
        .await;

        let text = text_of(&resp);
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            payload["total"].as_u64().unwrap_or(999),
            0,
            "client-b must not find client-a's memories"
        );
    }

    // ── T9: memory_search missing query returns error ─────────────────────────
    #[tokio::test]
    async fn search_missing_query_returns_error() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let ns = external_ns("test");

        let resp = handle_memory_search(
            &params(serde_json::json!({})),
            &mem,
            &ns,
        )
        .await;

        assert!(is_error(&resp));
    }

    // ── T10: memory_read success returns complete record ──────────────────────
    #[tokio::test]
    async fn read_success_returns_complete_record() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns = external_ns("reader");

        let store_resp = handle_memory_store(
            &params(serde_json::json!({
                "content": "stored content",
                "tags": ["important", "test"]
            })),
            &mem,
            &ns,
            &quota,
        )
        .await;

        let store_text = text_of(&store_resp);
        let store_payload: Value = serde_json::from_str(&store_text).unwrap();
        let id = store_payload["id"].as_str().unwrap();

        let read_resp = handle_memory_read(
            &params(serde_json::json!({ "id": id })),
            &mem,
            &ns,
        )
        .await;

        assert!(!is_error(&read_resp), "read should succeed: {read_resp}");
        let text = text_of(&read_resp);
        let record: Value = serde_json::from_str(&text).unwrap();

        assert_eq!(record["id"].as_str().unwrap(), id);
        assert_eq!(record["content"].as_str().unwrap(), "stored content");
        assert_eq!(
            record["namespace"].as_str().unwrap(),
            "external/reader"
        );
        assert!(record["created_at"].is_string());
    }

    // ── T11: memory_read missing id returns error ─────────────────────────────
    #[tokio::test]
    async fn read_missing_id_returns_error() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let ns = external_ns("test");

        let resp = handle_memory_read(
            &params(serde_json::json!({})),
            &mem,
            &ns,
        )
        .await;

        assert!(is_error(&resp));
        assert!(text_of(&resp).contains("id"));
    }

    // ── T12: tags are stored and returned correctly ───────────────────────────
    #[tokio::test]
    async fn store_tags_array_persisted_and_returned() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns = external_ns("tagger");

        let store_resp = handle_memory_store(
            &params(serde_json::json!({
                "content": "tagged memory",
                "tags": ["alpha", "beta"]
            })),
            &mem,
            &ns,
            &quota,
        )
        .await;

        let id = {
            let text = text_of(&store_resp);
            let p: Value = serde_json::from_str(&text).unwrap();
            p["id"].as_str().unwrap().to_string()
        };

        let read_resp = handle_memory_read(
            &params(serde_json::json!({ "id": id })),
            &mem,
            &ns,
        )
        .await;

        let text = text_of(&read_resp);
        let record: Value = serde_json::from_str(&text).unwrap();
        let tags = record["tags"].as_array().unwrap();
        assert!(
            tags.iter().any(|t| t.as_str() == Some("alpha")),
            "tag 'alpha' must be present"
        );
        assert!(
            tags.iter().any(|t| t.as_str() == Some("beta")),
            "tag 'beta' must be present"
        );
    }

    // ── F3-T1: memory_fetch_batch partial hit ─────────────────────────────────
    #[tokio::test]
    async fn fetch_batch_partial_hit() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns = external_ns("batcher");

        let mut ids = Vec::new();
        for c in ["one", "two"] {
            let r = handle_memory_store(
                &params(serde_json::json!({ "content": c })),
                &mem,
                &ns,
                &quota,
            )
            .await;
            let p: Value = serde_json::from_str(&text_of(&r)).unwrap();
            ids.push(p["id"].as_str().unwrap().to_string());
        }

        let resp = handle_memory_fetch_batch(
            &params(serde_json::json!({ "ids": [ids[0], "missing-xyz", ids[1]] })),
            &mem,
            &ns,
        )
        .await;

        assert!(!is_error(&resp));
        let payload: Value = serde_json::from_str(&text_of(&resp)).unwrap();
        assert_eq!(payload["total_found"].as_u64().unwrap(), 2);
        assert_eq!(payload["total_missing"].as_u64().unwrap(), 1);
        assert_eq!(
            payload["missing_ids"][0].as_str().unwrap(),
            "missing-xyz"
        );
    }

    // ── F3-T2: empty / missing ids param → error ──────────────────────────────
    #[tokio::test]
    async fn fetch_batch_missing_ids_errors() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let ns = external_ns("batcher");
        let resp = handle_memory_fetch_batch(&params(serde_json::json!({})), &mem, &ns).await;
        assert!(is_error(&resp));

        let resp2 =
            handle_memory_fetch_batch(&params(serde_json::json!({ "ids": [] })), &mem, &ns).await;
        assert!(is_error(&resp2), "empty ids array must error");
    }

    // ── F3-T3: over 100 ids → error ───────────────────────────────────────────
    #[tokio::test]
    async fn fetch_batch_over_limit_errors() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let ns = external_ns("batcher");
        let ids: Vec<String> = (0..101).map(|i| format!("id-{i}")).collect();
        let resp =
            handle_memory_fetch_batch(&params(serde_json::json!({ "ids": ids })), &mem, &ns).await;
        assert!(is_error(&resp));
    }

    // ── F3-T4: cross-namespace isolation ──────────────────────────────────────
    #[tokio::test]
    async fn fetch_batch_cross_namespace_isolation() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let quota = DailyQuota::with_limit(100);
        let ns_a = external_ns("owner-a");
        let ns_b = external_ns("intruder-b");

        let store = handle_memory_store(
            &params(serde_json::json!({ "content": "a's secret" })),
            &mem,
            &ns_a,
            &quota,
        )
        .await;
        let id = {
            let p: Value = serde_json::from_str(&text_of(&store)).unwrap();
            p["id"].as_str().unwrap().to_string()
        };

        // Client B batch-fetches A's id → must land in missing_ids, no content.
        let resp = handle_memory_fetch_batch(
            &params(serde_json::json!({ "ids": [id] })),
            &mem,
            &ns_b,
        )
        .await;
        let payload: Value = serde_json::from_str(&text_of(&resp)).unwrap();
        assert_eq!(payload["total_found"].as_u64().unwrap(), 0);
        assert_eq!(payload["total_missing"].as_u64().unwrap(), 1);
    }

    // ── user_code_profile ─────────────────────────────────────────────────

    #[tokio::test]
    async fn user_code_profile_empty_store_returns_empty_profile() {
        let mem = SqliteMemoryEngine::in_memory().unwrap();
        let ns = internal_ns("a1");
        let resp = handle_user_code_profile(&mem, &ns).await;
        assert!(resp.get("isError").is_none(), "must not error: {resp}");
        let payload: Value = serde_json::from_str(&text_of(&resp)).unwrap();
        assert_eq!(payload["agent_id"].as_str().unwrap(), "internal/a1");
        assert!(payload["rules"].as_array().unwrap().is_empty());
        assert!(payload["conflicts"].as_array().unwrap().is_empty());
        assert_eq!(payload["unparsed_count"].as_u64().unwrap(), 0);
    }
}
