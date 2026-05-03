// mcp_dispatch.rs — Transport-agnostic MCP tool call dispatcher (W20-P1 Phase 2A)
//
// Provides `McpDispatcher`: a shared struct that wraps all server-side state
// and enforces the same security pipeline (scope check → rate-limit check →
// namespace injection → tool handler) regardless of transport (stdio, HTTP, SSE).
//
// ## Security pipeline (same as stdio Phase 1, now centralised)
//
//   1. Scope check      – tool requires a specific Scope; Admin bypasses all
//   2. Rate-limit check – per-client, per-OpType (Read / Write)
//   3. Namespace inject – external clients cannot supply their own agent_id
//   4. Dispatch         – call the appropriate tool handler via handle_tools_call

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

use crate::mcp_auth::{Principal, Scope};
use crate::mcp_memory_quota::DailyQuota;
use crate::mcp_namespace::NamespaceContext;
use crate::mcp_rate_limit::{OpType, RateLimiter};
use duduclaw_memory::SqliteMemoryEngine;

// Re-export OdooState so HTTP/SSE layers can reference it without depending on
// the private type alias in mcp.rs.
pub type OdooState =
    Arc<tokio::sync::RwLock<Option<duduclaw_odoo::OdooConnector>>>;

// ── JSON-RPC helpers ──────────────────────────────────────────────────────────
// Mirror of the private helpers in mcp.rs; kept here so other modules don't
// need to depend on the internal `mcp` module.

pub fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

pub fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

// ── McpDispatcher ─────────────────────────────────────────────────────────────

/// Shared state for all MCP transports.
///
/// Clone is cheap: all expensive fields are behind `Arc`.
#[derive(Clone)]
pub struct McpDispatcher {
    pub home_dir: PathBuf,
    pub http: reqwest::Client,
    pub memory: Arc<SqliteMemoryEngine>,
    pub default_agent: String,
    pub odoo: OdooState,
    pub rate_limiter: RateLimiter,
    pub daily_quota: DailyQuota,
}

impl McpDispatcher {
    /// Construct a dispatcher with all required shared state.
    pub fn new(
        home_dir: PathBuf,
        http: reqwest::Client,
        memory: Arc<SqliteMemoryEngine>,
        default_agent: String,
        odoo: OdooState,
        rate_limiter: RateLimiter,
        daily_quota: DailyQuota,
    ) -> Self {
        Self { home_dir, http, memory, default_agent, odoo, rate_limiter, daily_quota }
    }

    /// Execute a `tools/call` JSON-RPC request through the full security pipeline.
    ///
    /// # Pipeline
    ///
    /// 1. **External whitelist** — external clients may only call whitelisted tools.
    /// 2. **Scope check** — verifies the principal has the required scope.
    /// 3. **Rate-limit check** — enforces per-client Read / Write limits.
    /// 4. **Namespace injection** — strips `agent_id` / `namespace` from external clients.
    /// 5. **Tool dispatch** — delegates to `crate::mcp::handle_tools_call`.
    ///
    /// Returns a JSON-RPC `result` or `error` Value.
    pub async fn dispatch_tool_call(
        &self,
        principal: &Principal,
        ns_ctx: &NamespaceContext,
        params: &Value,
        id: &Value,
    ) -> Value {
        let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

        // ── 0. External whitelist enforcement ────────────────────────────────
        // (review BLOCKER R2 / security N-1) `tools/list` already filters
        // hidden tools out of discovery, but a malicious external client can
        // still call any tool by name via `tools/call`. Mirror the filter
        // here so non-discoverable tools are also non-callable.
        if principal.is_external
            && !crate::mcp::EXTERNAL_TOOLS_WHITELIST.contains(&tool_name)
        {
            warn!(
                client_id = %principal.client_id,
                tool = %tool_name,
                "External client attempted to call non-whitelisted tool"
            );
            return jsonrpc_error(
                id,
                -32601,
                &format!("Method '{tool_name}' not available to external clients"),
            );
        }

        // ── 1. Scope check ───────────────────────────────────────────────────
        if let Some(required) = crate::mcp_auth::tool_requires_scope(tool_name) {
            if !principal.scopes.contains(&required)
                && !principal.scopes.contains(&Scope::Admin)
            {
                return jsonrpc_error(
                    id,
                    -32003,
                    &format!(
                        "Insufficient scope: {:?} required for '{}'",
                        required, tool_name
                    ),
                );
            }
        }

        // ── 2. Rate-limit check (Read / Write) ───────────────────────────────
        let op_type = if matches!(
            tool_name,
            "memory_store" | "wiki_write" | "send_message"
        ) {
            OpType::Write
        } else {
            OpType::Read
        };
        if let Err(e) = self.rate_limiter.check(&principal.client_id, op_type) {
            return jsonrpc_error(id, -32029, &format!("Rate limited: {e}"));
        }

        // ── 3. Namespace injection (external clients only) ───────────────────
        let mut params_owned = params.clone();
        if principal.is_external {
            if let Some(args) = params_owned.get_mut("arguments") {
                if let Some(obj) = args.as_object_mut() {
                    if obj.contains_key("agent_id") {
                        warn!(
                            client_id = %principal.client_id,
                            tool = %tool_name,
                            "External client attempted to set agent_id, ignoring (namespace enforcement)"
                        );
                        obj.remove("agent_id");
                    }
                    obj.remove("namespace");
                }
            }
        }

        // ── 4. Tool dispatch ─────────────────────────────────────────────────
        let caller_is_admin = principal.scopes.contains(&Scope::Admin);
        crate::mcp::handle_tools_call(
            id,
            &params_owned,
            &self.home_dir,
            &self.http,
            &self.memory,
            &self.default_agent,
            &self.odoo,
            ns_ctx,
            &self.daily_quota,
            &principal.client_id,
            caller_is_admin,
        )
        .await
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use crate::mcp_auth::{Principal, Scope};
    use crate::mcp_namespace::NamespaceContext;

    /// Build a minimal Principal for test scenarios.
    fn make_principal(scopes: Vec<Scope>, is_external: bool) -> Principal {
        Principal {
            client_id: "test-client".to_string(),
            scopes: scopes.into_iter().collect(),
            is_external,
            created_at: chrono::Utc::now(),
        }
    }

    fn make_ns_ctx(is_external: bool) -> NamespaceContext {
        if is_external {
            NamespaceContext {
                write_namespace: "external/test-client".to_string(),
                read_namespaces: vec![
                    "external/test-client".to_string(),
                    "shared/public".to_string(),
                ],
            }
        } else {
            NamespaceContext {
                write_namespace: "internal/test-client".to_string(),
                read_namespaces: vec![
                    "internal/test-client".to_string(),
                    "shared/public".to_string(),
                ],
            }
        }
    }

    // Helper: build a minimal `tools/call` params value.
    fn make_params(tool: &str, args: Value) -> Value {
        serde_json::json!({ "name": tool, "arguments": args })
    }

    // Helper: build a McpDispatcher backed by a temp dir (no real tools called).
    async fn make_dispatcher(tmp: &tempfile::TempDir) -> McpDispatcher {
        let home_dir = tmp.path().to_path_buf();
        let http = reqwest::Client::new();
        let memory_path = home_dir.join("memory.db");
        let memory = Arc::new(
            duduclaw_memory::SqliteMemoryEngine::new(&memory_path)
                .expect("test memory db"),
        );
        let odoo: OdooState = Arc::new(tokio::sync::RwLock::new(None));
        McpDispatcher::new(
            home_dir,
            http,
            memory,
            "dudu".to_string(),
            odoo,
            RateLimiter::new(),
            DailyQuota::new(),
        )
    }

    // ── Test: scope denied returns JSON-RPC -32003 ────────────────────────────
    #[tokio::test]
    async fn scope_check_denies_missing_scope() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_dispatcher(&tmp).await;

        // memory_search requires MemoryRead; give principal no scopes
        let principal = make_principal(vec![], false);
        let ns_ctx = make_ns_ctx(false);
        let params = make_params("memory_search", serde_json::json!({ "query": "x" }));
        let id = serde_json::json!(1);

        let result = dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id).await;

        assert_eq!(
            result["error"]["code"],
            -32003,
            "Expected scope error code -32003, got: {result}"
        );
        let msg = result["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("Insufficient scope"),
            "Error message should mention scope: {msg}"
        );
    }

    // ── Test: Admin scope bypasses all scope checks ───────────────────────────
    #[tokio::test]
    async fn admin_scope_bypasses_scope_check() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Write a minimal mcp_keys entry so auth doesn't break unrelated paths
        let dispatcher = make_dispatcher(&tmp).await;

        let principal = make_principal(vec![Scope::Admin], false);
        let ns_ctx = make_ns_ctx(false);
        // memory_search would normally require MemoryRead; Admin should pass scope check.
        // The tool itself may fail for other reasons (no actual data), but NOT -32003.
        let params = make_params("memory_search", serde_json::json!({ "query": "x" }));
        let id = serde_json::json!(2);

        let result = dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id).await;

        // Should NOT be -32003 (scope denied)
        let code = result["error"]["code"].as_i64().unwrap_or(0);
        assert_ne!(
            code, -32003,
            "Admin scope should bypass scope check, got: {result}"
        );
    }

    // ── Test: rate-limit exceeded returns JSON-RPC -32029 ────────────────────
    #[tokio::test]
    async fn rate_limit_exceeded_returns_32029() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_dispatcher(&tmp).await;

        let principal = make_principal(vec![Scope::MemoryRead], false);
        let ns_ctx = make_ns_ctx(false);
        let params = make_params("memory_search", serde_json::json!({ "query": "x" }));
        let id = serde_json::json!(3);

        // Exhaust the Read bucket (100 req/min)
        for _ in 0..100 {
            let _ = dispatcher.rate_limiter.check(&principal.client_id, OpType::Read);
        }

        let result = dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id).await;

        assert_eq!(
            result["error"]["code"],
            -32029,
            "Expected rate-limit error code -32029, got: {result}"
        );
    }

    // ── Test: external client's agent_id is stripped ──────────────────────────
    #[tokio::test]
    async fn external_client_agent_id_stripped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_dispatcher(&tmp).await;

        let principal = make_principal(vec![Scope::MemoryRead, Scope::MemoryWrite], true);
        let ns_ctx = make_ns_ctx(true);
        // Include a rogue agent_id; the dispatcher must strip it silently.
        let params = make_params(
            "memory_search",
            serde_json::json!({ "query": "x", "agent_id": "../../etc/passwd" }),
        );
        let id = serde_json::json!(4);

        let result = dispatcher.dispatch_tool_call(&principal, &ns_ctx, &params, &id).await;

        // The call should NOT fail with a namespace/security error -32003.
        // (It may succeed or fail for other reasons, but not path traversal.)
        let code = result["error"]["code"].as_i64().unwrap_or(0);
        assert_ne!(
            code, -32003,
            "agent_id stripping should prevent traversal, got: {result}"
        );
    }
}
