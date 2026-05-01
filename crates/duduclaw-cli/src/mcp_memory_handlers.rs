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
use duduclaw_memory::SqliteMemoryEngine;
use serde_json::Value;

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
}
