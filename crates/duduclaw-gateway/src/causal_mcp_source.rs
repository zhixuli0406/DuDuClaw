//! Narrow, independently verified MCP-to-CCR source binding.
//!
//! A configured MCP read route may claim only the exact bytes of an active
//! local causal artifact in the authenticated caller's own ACL. The MCP
//! response, its `_meta`, and model-provided version/ACL fields are never
//! authorities. Other MCP routes keep their existing scoped, unbound CCR.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use duduclaw_llm::{
    CcrRuntime, CcrScope, CcrSourceArtifact, McpSourceVerifier, ToolRegistry, VerifiedMcpSource,
};
use duduclaw_memory::causal::{CausalStore, EvidenceScope};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedCausalRoute {
    server: String,
    tool: String,
}

/// Application-owned authority over one local causal database. It expects a
/// read tool whose only argument is `artifact_id` and whose entire text result
/// equals that artifact's content. This intentionally does not authorize
/// snippets, search summaries, arbitrary metadata envelopes, or wider ACLs.
pub(crate) struct CausalMcpSourceVerifier {
    store: CausalStore,
}

impl CausalMcpSourceVerifier {
    fn new(store: CausalStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl McpSourceVerifier for CausalMcpSourceVerifier {
    async fn verify(
        &self,
        scope: &CcrScope,
        args: &Value,
        content: &str,
    ) -> Result<VerifiedMcpSource, String> {
        let object = args
            .as_object()
            .filter(|object| object.len() == 1)
            .ok_or("expected only artifact_id")?;
        let artifact_id = object
            .get("artifact_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty() && id.len() <= 512)
            .ok_or("invalid artifact_id")?
            .to_owned();
        let source_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.source_acl.clone(),
        };
        let store = self.store.clone();
        let exact_content = content.to_owned();
        tokio::task::spawn_blocking(move || {
            // Read metadata both sides of the source text. Artifact content,
            // version and ACL are immutable through the causal API; an
            // invalidation between reads fails the second active read.
            let first = store
                .read_artifact_metadata(&source_scope, &artifact_id)
                .map_err(|_| "source metadata unavailable")?;
            let trusted_text = store
                .source_text(&source_scope, &artifact_id)
                .map_err(|_| "source content unavailable")?;
            let last = store
                .read_artifact_metadata(&source_scope, &artifact_id)
                .map_err(|_| "source changed during verification")?;
            if first != last || trusted_text != exact_content {
                return Err("MCP result differs from active source");
            }
            if first.version.trim().is_empty() || first.version.len() > 512 {
                return Err("source version cannot be bound");
            }
            let acl_revision = format!(
                "immutable-acl-sha256:{:x}",
                Sha256::digest(format!("{}\0{}", source_scope.tenant_id, source_scope.acl))
            );
            Ok(VerifiedMcpSource {
                artifact: CcrSourceArtifact {
                    connector: "causal".into(),
                    artifact_id,
                    version: first.version,
                    acl_revision,
                },
                retention_at: first.retention_at,
            })
        })
        .await
        .map_err(|_| "source verification task failed".to_owned())?
        .map_err(str::to_owned)
    }
}

/// Register only explicitly configured, mounted, CCR-allowed routes. A bad
/// declared route withholds its result when attestation cannot be registered.
/// A malformed declaration is reported to the caller, which
/// disables CCR across the registry for this request.
pub(crate) fn register_verified_causal_routes(
    home: &Path,
    registry: &mut ToolRegistry,
    runtime: &CcrRuntime,
) -> Result<(), String> {
    let config = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|error| {
            registry.require_source_attestation_for_all_tools();
            error.to_string()
        })?
        .parse::<toml::Table>()
        .map_err(|error| {
            registry.require_source_attestation_for_all_tools();
            error.to_string()
        })?;
    let routes = config
        .get("ccr")
        .and_then(toml::Value::as_table)
        .and_then(|ccr| ccr.get("verified_causal_routes"));
    let Some(routes) = routes else { return Ok(()) };
    let routes: Vec<VerifiedCausalRoute> = routes.clone().try_into().map_err(|error| {
        registry.require_source_attestation_for_all_tools();
        format!("invalid ccr.verified_causal_routes: {error}")
    })?;
    let verifier = Arc::new(CausalMcpSourceVerifier::new(CausalStore::new(
        home.join("memory.db"),
    )));
    for route in routes {
        registry.require_source_attestation_for_tool(&route.tool);
        if runtime
            .source_key_for_call(Some(&route.server), &route.tool)
            .is_none()
        {
            registry.disable_ccr_for_tool(&route.tool);
            tracing::warn!(
                server = %route.server,
                tool = %route.tool,
                "verified causal route absent from ccr.allowed_sources; CCR disabled for tool"
            );
            continue;
        }
        if let Err(error) = registry.register_source_verifier(
            runtime.scope.clone(),
            &route.server,
            &route.tool,
            verifier.clone(),
        ) {
            registry.disable_ccr_for_tool(&route.tool);
            tracing::warn!(
                server = %route.server,
                tool = %route.tool,
                error = %error,
                "verified causal route registration failed; CCR disabled for tool"
            );
        }
    }
    Ok(())
}

/// Declarations remain a disclosure boundary even when CCR itself cannot be
/// constructed (disabled, missing caller scope, or invalid runtime config).
/// A malformed declaration with no identifiable tool fences the registry.
pub(crate) fn fence_declared_verified_routes(home: &Path, registry: &mut ToolRegistry) {
    let config = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(config) => config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            registry.require_source_attestation_for_all_tools();
            return;
        }
    };
    let config: toml::Table = match config.parse() {
        Ok(config) => config,
        Err(_) => {
            registry.require_source_attestation_for_all_tools();
            return;
        }
    };
    let Some(ccr) = config.get("ccr") else {
        return;
    };
    let Some(ccr) = ccr.as_table() else {
        registry.require_source_attestation_for_all_tools();
        return;
    };
    for key in ["verified_causal_routes", "verified_wiki_routes"] {
        let Some(routes) = ccr.get(key) else {
            continue;
        };
        if key == "verified_wiki_routes" {
            registry.require_source_attestation_for_tool("wiki_read");
        }
        let Some(routes) = routes.as_array() else {
            registry.require_source_attestation_for_all_tools();
            return;
        };
        for route in routes {
            let Some(tool) = route
                .as_table()
                .and_then(|route| route.get("tool"))
                .and_then(toml::Value::as_str)
                .filter(|tool| !tool.trim().is_empty())
            else {
                registry.require_source_attestation_for_all_tools();
                return;
            };
            registry.require_source_attestation_for_tool(tool);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};
    use duduclaw_llm::CcrStore;
    use duduclaw_llm::{
        ChatProvider, ChatRequest, ChatResponse, ContentPart, LlmError, McpClient, NormalizedUsage,
        ProvenanceConfig, StopReason, StreamEvent, ToolExecutor,
        run_tool_loop_with_provenance_and_ccr,
    };
    use futures_util::stream::BoxStream;
    use std::sync::Mutex;

    fn scope() -> CcrScope {
        CcrScope {
            tenant_id: "local".into(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            source_acl: format!(
                "agent:agent:session:session:principal-sha256:{}",
                "a".repeat(64)
            ),
        }
    }

    #[tokio::test]
    async fn exact_source_bytes_and_scope_required_for_mcp_attestation() {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("memory.db"));
        let source_scope = EvidenceScope {
            tenant_id: "local".into(),
            acl: scope().source_acl,
        };
        let source = store
            .add_artifact(
                &source_scope,
                "ticket",
                "ticket-1",
                "v1",
                "lineage-1",
                &"trusted evidence\n".repeat(400),
                1,
                i64::MAX / 2,
            )
            .unwrap();
        let verifier = CausalMcpSourceVerifier::new(store.clone());
        let args = serde_json::json!({"artifact_id": source.id});
        let text = store.source_text(&source_scope, &source.id).unwrap();
        let verified = verifier.verify(&scope(), &args, &text).await.unwrap();
        assert_eq!(verified.artifact.connector, "causal");
        assert_eq!(verified.artifact.version, "v1");
        assert_eq!(verified.retention_at, source.retention_at);
        assert!(
            verifier
                .verify(&scope(), &args, "different text")
                .await
                .is_err()
        );
        assert!(
            verifier
                .verify(
                    &scope(),
                    &serde_json::json!({"artifact_id": source.id, "acl": "principal-acl"}),
                    &text,
                )
                .await
                .is_err()
        );
        let other = CcrScope {
            source_acl: "other-principal".into(),
            ..scope()
        };
        assert!(verifier.verify(&other, &args, &text).await.is_err());
        let private_scope = EvidenceScope {
            tenant_id: "local".into(),
            acl: "private".into(),
        };
        let dashboard_style_source = store
            .add_artifact(
                &private_scope,
                "ticket",
                "dashboard-ticket",
                "v1",
                "dashboard-lineage",
                &text,
                1,
                i64::MAX / 2,
            )
            .unwrap();
        assert!(
            verifier
                .verify(
                    &scope(),
                    &serde_json::json!({"artifact_id": dashboard_style_source.id}),
                    &text,
                )
                .await
                .is_err()
        );
        store
            .invalidate_artifact(&source_scope, &source.id)
            .unwrap();
        assert!(verifier.verify(&scope(), &args, &text).await.is_err());
    }

    #[tokio::test]
    async fn optional_verified_routes_preserve_unbound_ccr_until_explicitly_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::from_clients_named(Vec::new(), Vec::new())
            .await
            .unwrap();
        let runtime = CcrRuntime::new(CcrStore::new(dir.path().join("ccr.db")), scope())
            .restrict_sources([("generic-mcp".into(), "search".into())]);
        std::fs::write(dir.path().join("config.toml"), "[ccr]\nenabled = true\n").unwrap();
        assert!(
            register_verified_causal_routes(dir.path(), &mut registry, &runtime).is_ok(),
            "absence of verified-route config must preserve generic CCR"
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[ccr]\nverified_causal_routes = 'not-an-array'\n",
        )
        .unwrap();
        assert!(register_verified_causal_routes(dir.path(), &mut registry, &runtime).is_err());
    }

    struct OneCallProvider {
        artifact_id: String,
        calls: std::sync::atomic::AtomicUsize,
        last_request: Mutex<Option<ChatRequest>>,
    }

    #[async_trait]
    impl ChatProvider for OneCallProvider {
        fn id(&self) -> &str {
            "synthetic"
        }

        async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, LlmError> {
            self.last_request.lock().unwrap().replace(request.clone());
            let first = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            Ok(ChatResponse {
                parts: if first {
                    vec![ContentPart::ToolCall {
                        id: "call-1".into(),
                        name: "get_source".into(),
                        args: serde_json::json!({"artifact_id": self.artifact_id}),
                    }]
                } else {
                    vec![ContentPart::Text("done".into())]
                },
                stop: if first {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
                usage: NormalizedUsage::default(),
                model_used: "synthetic".into(),
                provider: "synthetic".into(),
            })
        }

        async fn stream(
            &self,
            _request: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::InvalidRequest("stream unused".into()))
        }
    }

    async fn synthetic_mcp_rpc(
        State(content): State<String>,
        Json(frame): Json<Value>,
    ) -> Json<Value> {
        let result = match frame["method"].as_str() {
            Some("tools/list") => serde_json::json!({
                "tools": [{
                    "name": "get_source",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"artifact_id": {"type": "string"}},
                        "required": ["artifact_id"]
                    }
                }]
            }),
            Some("tools/call") => serde_json::json!({
                "content": [{"type": "text", "text": content}],
                "isError": false,
                "_meta": {
                    "sourceArtifact": {
                        "connector": "spoofed",
                        "version": "spoofed",
                        "acl_revision": "spoofed"
                    }
                }
            }),
            _ => serde_json::json!({}),
        };
        Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": result
        }))
    }

    #[tokio::test]
    async fn configured_mcp_route_commits_only_verified_causal_source_to_ccr() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let source_scope = EvidenceScope {
            tenant_id: "local".into(),
            acl: scope().source_acl,
        };
        let original = "trusted source line\n".repeat(400);
        let artifact = causal
            .add_artifact(
                &source_scope,
                "ticket",
                "ticket-2",
                "v2",
                "lineage-2",
                &original,
                1,
                i64::MAX / 2,
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(synthetic_mcp_rpc))
            .with_state(original.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = McpClient::connect_http(&endpoint, &[], std::time::Duration::from_secs(5))
            .await
            .unwrap();
        let mut registry =
            ToolRegistry::from_clients_named(vec![("causal-mcp".into(), client)], Vec::new())
                .await
                .unwrap();
        let mut runtime = CcrRuntime::new(CcrStore::new(dir.path().join("ccr.db")), scope())
            .restrict_sources([("causal-mcp".into(), "get_source".into())])
            .with_bound_source_validator(crate::ccr_runtime::bound_source_validator(dir.path()));
        runtime.min_compress_bytes = 1_024;
        std::fs::write(
            dir.path().join("config.toml"),
            "[ccr]\n[[ccr.verified_causal_routes]]\nserver='causal-mcp'\ntool='get_source'\n",
        )
        .unwrap();
        register_verified_causal_routes(dir.path(), &mut registry, &runtime).unwrap();
        let provider = OneCallProvider {
            artifact_id: artifact.id.clone(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            last_request: Mutex::new(None),
        };
        let outcome = run_tool_loop_with_provenance_and_ccr(
            &provider,
            ChatRequest::new("synthetic"),
            &registry,
            3,
            ProvenanceConfig::default(),
            None,
            Some(runtime.clone()),
        )
        .await
        .unwrap();
        assert!(
            !outcome.ccr_delivery_guards.is_empty()
                && outcome.ccr_delivery_guards.still_valid().await,
            "the first verified source delivery must retain a live source lease"
        );
        let saved = outcome.ccr_saved_results.first().expect("bound CCR handle");
        assert_eq!(outcome.ccr_saved_results.len(), 1);
        let seen = provider.last_request.lock().unwrap().clone().unwrap();
        let ContentPart::ToolResult {
            content, is_error, ..
        } = &seen.messages.last().unwrap().parts[0]
        else {
            panic!("missing tool result")
        };
        assert!(!is_error);
        assert!(content.contains("id="));
        assert!(content.len() < original.len());
        assert!(
            runtime
                .retrieve(&saved.id, None, 0, 100)
                .unwrap()
                .text
                .starts_with("trusted")
        );
        assert_eq!(
            runtime
                .store
                .revoke_artifact_version("local", "causal", &artifact.id, "v2")
                .unwrap(),
            1
        );
        assert!(runtime.retrieve(&saved.id, None, 0, 100).is_err());

        // One malformed route entry leaves no identifiable tool name. The
        // registry must refuse all unverified results instead of falling
        // back to ordinary MCP delivery of the raw source.
        let client = McpClient::connect_http(&endpoint, &[], std::time::Duration::from_secs(5))
            .await
            .unwrap();
        let mut malformed_registry =
            ToolRegistry::from_clients_named(vec![("causal-mcp".into(), client)], Vec::new())
                .await
                .unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[ccr]\nverified_causal_routes = [{ server='causal-mcp', tool=1 }]\n",
        )
        .unwrap();
        assert!(
            register_verified_causal_routes(dir.path(), &mut malformed_registry, &runtime).is_err()
        );
        let refused = malformed_registry
            .call(
                "get_source",
                serde_json::json!({"artifact_id": artifact.id}),
            )
            .await
            .unwrap();
        assert!(refused.is_error && refused.ccr_revoke_call);
        assert!(!refused.content.contains("trusted source line"));

        // Disabling CCR must not erase an explicit trusted-route promise.
        // The gateway applies this fence before attempting to build a CCR
        // runtime, so a missing runtime still cannot deliver raw bytes.
        let client = McpClient::connect_http(&endpoint, &[], std::time::Duration::from_secs(5))
            .await
            .unwrap();
        let mut disabled_registry =
            ToolRegistry::from_clients_named(vec![("causal-mcp".into(), client)], Vec::new())
                .await
                .unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[ccr]\nenabled=false\n[[ccr.verified_causal_routes]]\nserver='causal-mcp'\ntool='get_source'\n",
        )
        .unwrap();
        assert!(crate::ccr_runtime::for_agent(dir.path(), "agent").is_none());
        fence_declared_verified_routes(dir.path(), &mut disabled_registry);
        let refused = disabled_registry
            .call(
                "get_source",
                serde_json::json!({"artifact_id": artifact.id}),
            )
            .await
            .unwrap();
        assert!(refused.is_error && refused.ccr_revoke_call);
        assert!(!refused.content.contains("trusted source line"));
        server.abort();
    }
}
