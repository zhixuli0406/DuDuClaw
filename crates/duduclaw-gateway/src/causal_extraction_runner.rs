//! One-shot, scoped provider invocation for candidate causal extraction.
//!
//! Callers must supply a trusted route authorizer before any source bytes are
//! sent to a provider. The returned text remains an unreviewed proposal.

use duduclaw_llm::{
    ChatMessage, ChatProvider, ChatRequest, ContentPart, LlmError, StopReason, ToolChoice,
};
use duduclaw_memory::causal::{
    CausalClaim, CausalStore, CausalStoreError, EvidenceScope, EvidenceSpan,
};
use duduclaw_memory::causal_extract::{build_extraction_prompt, ingest_extraction_response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const PROMPT_VERSION: &str = "causal-candidate-extraction-v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredScope {
    tenant_id: String,
    acl: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CausalExtractionPolicy {
    enabled: bool,
    provider: String,
    model: String,
    allowed_scopes: Vec<ConfiguredScope>,
}

impl CausalExtractionPolicy {
    /// No setting, malformed setting, or unknown provider fails closed.
    pub fn load(home_dir: &Path) -> Option<Self> {
        let config = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
        let parsed: toml::Value = toml::from_str(&config).ok()?;
        let policy: Self = parsed.get("causal_extraction")?.clone().try_into().ok()?;
        if !policy.enabled
            || !matches!(policy.provider.as_str(), "anthropic" | "openai" | "gemini")
            || policy.model.trim().is_empty()
            || policy.model.len() > MAX_MODEL_ID_BYTES
            || policy.allowed_scopes.is_empty()
            || policy
                .allowed_scopes
                .iter()
                .any(|scope| scope.tenant_id.trim().is_empty() || scope.acl.trim().is_empty())
        {
            return None;
        }
        Some(policy)
    }

    pub fn provider_id(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn allows(&self, scope: &EvidenceScope, provider: &str, model: &str) -> bool {
        provider == self.provider
            && model == self.model
            && self
                .allowed_scopes
                .iter()
                .any(|allowed| allowed.tenant_id == scope.tenant_id && allowed.acl == scope.acl)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CausalExtractionError {
    #[error("provider route is not authorized for this source scope")]
    Unauthorized,
    #[error("invalid or incomplete extraction model response")]
    InvalidResponse,
    #[error("invalid extraction model or provider identity")]
    InvalidRoute,
    #[error("source or candidate validation failed: {0}")]
    Source(#[from] CausalStoreError),
    #[error("causal source changed during extraction; result withheld")]
    SourceRevoked,
    #[error("extraction provider failed: {0}")]
    Provider(#[from] LlmError),
}

#[derive(Debug, Serialize)]
pub struct CausalExtractionRun {
    pub artifact_id: String,
    pub source_sha256: String,
    pub prompt_sha256: String,
    pub response_sha256: String,
    pub provider_id: String,
    pub model_used: String,
    pub extractor_version: String,
    pub candidates: Vec<(CausalClaim, EvidenceSpan)>,
}

/// The authorizer must be backed by the host's trusted provider/ACL policy.
/// It runs before a guarded source read and before the provider receives any
/// content. The source lease remains held through the provider response and
/// candidate checks, so a staged revocation cannot complete while those
/// copied bytes are in use.
pub async fn extract_candidates_with_provider<P, F>(
    store: &CausalStore,
    scope: &EvidenceScope,
    artifact_id: &str,
    question: &str,
    model: &str,
    provider: &P,
    authorize_route: F,
) -> Result<CausalExtractionRun, CausalExtractionError>
where
    P: ChatProvider + ?Sized,
    F: FnOnce(&EvidenceScope, &str, &str) -> bool,
{
    let provider_id = provider.id();
    if model.trim().is_empty()
        || model.len() > MAX_MODEL_ID_BYTES
        || provider_id.trim().is_empty()
        || provider_id.len() > MAX_PROVIDER_ID_BYTES
    {
        return Err(CausalExtractionError::InvalidRoute);
    }
    if !authorize_route(scope, provider_id, model) {
        return Err(CausalExtractionError::Unauthorized);
    }
    let (source, source_lease) = store.source_text_with_delivery_lease(scope, artifact_id)?;
    let prompt = build_extraction_prompt(question, &source)?;
    let mut request = ChatRequest::new(model);
    request.messages.push(ChatMessage::user(&prompt));
    request.tools.clear();
    request.tool_choice = ToolChoice::None;
    request.temperature = Some(0.0);
    request.max_tokens = 8_192;
    if !source_lease.still_valid() {
        return Err(CausalExtractionError::SourceRevoked);
    }
    let response = provider.complete(&request).await?;
    if !source_lease.still_valid() {
        return Err(CausalExtractionError::SourceRevoked);
    }
    if response.stop != StopReason::EndTurn
        || response.model_used.trim().is_empty()
        || response.model_used.len() > MAX_MODEL_ID_BYTES
        || response.model_used != model
    {
        return Err(CausalExtractionError::InvalidResponse);
    }
    let mut raw = String::new();
    for part in &response.parts {
        match part {
            ContentPart::Text(text) => {
                if raw.len().saturating_add(text.len()) > MAX_RESPONSE_BYTES {
                    return Err(CausalExtractionError::InvalidResponse);
                }
                raw.push_str(text);
            }
            ContentPart::Reasoning { .. } => {}
            _ => return Err(CausalExtractionError::InvalidResponse),
        }
    }
    if raw.is_empty() {
        return Err(CausalExtractionError::InvalidResponse);
    }
    if !source_lease.still_valid() {
        return Err(CausalExtractionError::SourceRevoked);
    }
    let extractor_version = format!("{PROMPT_VERSION}:{provider_id}:{}", response.model_used);
    let candidates = ingest_extraction_response(
        store,
        scope,
        artifact_id,
        question,
        &extractor_version,
        &raw,
    )?;
    if !source_lease.still_valid() {
        return Err(CausalExtractionError::SourceRevoked);
    }
    Ok(CausalExtractionRun {
        artifact_id: artifact_id.into(),
        source_sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
        prompt_sha256: format!("{:x}", Sha256::digest(prompt.as_bytes())),
        response_sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
        provider_id: provider_id.into(),
        model_used: response.model_used,
        extractor_version,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use duduclaw_llm::{ChatResponse, NormalizedUsage, StreamEvent};
    use duduclaw_memory::causal::{ClaimModality, EvidenceStance, ProposedCausalClaim};
    use duduclaw_memory::causal_extract::ExtractionEnvelope;
    use futures_util::stream::BoxStream;

    use super::*;

    #[test]
    fn configured_egress_policy_requires_exact_scope_and_known_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(CausalExtractionPolicy::load(dir.path()).is_none());
        std::fs::write(
            &path,
            r#"[causal_extraction]
enabled = true
provider = "anthropic"
model = "claude-test"
allowed_scopes = [{ tenant_id = "demo", acl = "pilot" }]"#,
        )
        .unwrap();
        let policy = CausalExtractionPolicy::load(dir.path()).unwrap();
        let scope = EvidenceScope {
            tenant_id: "demo".into(),
            acl: "pilot".into(),
        };
        assert!(policy.allows(&scope, "anthropic", "claude-test"));
        assert!(!policy.allows(&scope, "openai", "claude-test"));
        assert!(!policy.allows(&scope, "anthropic", "other-model"));
        assert!(!policy.allows(
            &EvidenceScope {
                tenant_id: "other".into(),
                acl: "pilot".into()
            },
            "anthropic",
            "claude-test"
        ));
        std::fs::write(&path, "[causal_extraction]\nenabled = true\nprovider = 'unknown'\nmodel = 'm'\nallowed_scopes = [{tenant_id='demo',acl='pilot'}]").unwrap();
        assert!(CausalExtractionPolicy::load(dir.path()).is_none());
    }

    struct ScriptedProvider {
        response: ChatResponse,
        requests: Mutex<Vec<ChatRequest>>,
        revoke: Option<(CausalStore, EvidenceScope, String)>,
    }

    #[async_trait]
    impl ChatProvider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }

        async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, LlmError> {
            self.requests.lock().unwrap().push(request.clone());
            if let Some((store, scope, artifact_id)) = &self.revoke {
                store.begin_ccr_revocation(scope, artifact_id).unwrap();
            }
            Ok(self.response.clone())
        }

        async fn stream(
            &self,
            _request: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::InvalidRequest(
                "stream unused in extraction".into(),
            ))
        }
    }

    fn fixture() -> (
        tempfile::TempDir,
        CausalStore,
        EvidenceScope,
        String,
        String,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = CausalStore::new(dir.path().join("causal.db"));
        let scope = EvidenceScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let source = "主管說：增加人手可能降低等待時間。".to_string();
        let artifact = store
            .add_artifact(
                &scope,
                "support_note",
                "note-1",
                "v1",
                "lineage-1",
                &source,
                1,
                i64::MAX,
            )
            .unwrap();
        (dir, store, scope, artifact.id, source)
    }

    fn provider(source: &str) -> ScriptedProvider {
        let excerpt = "增加人手可能降低等待時間";
        let start = source.find(excerpt).unwrap();
        let claim = ProposedCausalClaim {
            cause_variable: "staffing".into(),
            effect_variable: "wait_time".into(),
            lag_min_seconds: 0,
            lag_max_seconds: 86_400,
            modality: ClaimModality::Speculated,
            stance: EvidenceStance::Supports,
            span_start: start,
            span_end: start + excerpt.len(),
            excerpt: excerpt.into(),
            speaker_id: Some("manager".into()),
            context: serde_json::json!({ "team": "support" }),
        };
        let raw = serde_json::to_string(&ExtractionEnvelope {
            claims: vec![claim],
        })
        .unwrap();
        ScriptedProvider {
            response: ChatResponse {
                parts: vec![ContentPart::Text(raw)],
                stop: StopReason::EndTurn,
                usage: NormalizedUsage::default(),
                model_used: "model-1".into(),
                provider: "scripted".into(),
            },
            requests: Mutex::new(Vec::new()),
            revoke: None,
        }
    }

    fn authorized(scope: &EvidenceScope, provider: &str, model: &str) -> bool {
        scope.tenant_id == "tenant"
            && scope.acl == "private"
            && provider == "scripted"
            && model == "model-1"
    }

    #[tokio::test]
    async fn authorized_provider_creates_only_source_grounded_candidates() {
        let (_dir, store, scope, artifact_id, source) = fixture();
        let provider = provider(&source);
        let run = extract_candidates_with_provider(
            &store,
            &scope,
            &artifact_id,
            "Why did wait rise?",
            "model-1",
            &provider,
            authorized,
        )
        .await
        .unwrap();
        assert_eq!(run.candidates.len(), 1);
        assert_eq!(run.candidates[0].0.review_state, "candidate");
        assert_eq!(run.candidates[0].0.modality, ClaimModality::Speculated);
        assert_eq!(run.candidates[0].1.speaker_id.as_deref(), Some("manager"));
        assert_eq!(run.candidates[0].1.excerpt, "增加人手可能降低等待時間");
        assert!(run.extractor_version.contains("scripted:model-1"));
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
        assert_eq!(requests[0].tool_choice, ToolChoice::None);
        assert!(requests[0].messages[0].parts.iter().any(|part| {
            matches!(part, ContentPart::Text(text) if text.contains("UTF-8 byte offsets"))
        }));
    }

    #[tokio::test]
    async fn denied_route_never_sends_source_to_provider() {
        let (_dir, store, scope, artifact_id, source) = fixture();
        let provider = provider(&source);
        assert!(matches!(
            extract_candidates_with_provider(
                &store,
                &scope,
                &artifact_id,
                "Why?",
                "model-1",
                &provider,
                |_, _, _| false,
            )
            .await,
            Err(CausalExtractionError::Unauthorized)
        ));
        assert!(provider.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn source_revoked_during_provider_call_cannot_create_candidate() {
        let (_dir, store, scope, artifact_id, source) = fixture();
        let mut provider = provider(&source);
        provider.revoke = Some((store.clone(), scope.clone(), artifact_id.clone()));
        assert!(matches!(
            extract_candidates_with_provider(
                &store,
                &scope,
                &artifact_id,
                "Why?",
                "model-1",
                &provider,
                authorized,
            )
            .await,
            Err(CausalExtractionError::SourceRevoked)
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_claims", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn incomplete_or_tool_call_response_cannot_create_candidate() {
        for invalid in [StopReason::MaxTokens, StopReason::ToolUse] {
            let (_dir, store, scope, artifact_id, source) = fixture();
            let mut provider = provider(&source);
            provider.response.stop = invalid;
            assert!(matches!(
                extract_candidates_with_provider(
                    &store,
                    &scope,
                    &artifact_id,
                    "Why?",
                    "model-1",
                    &provider,
                    authorized,
                )
                .await,
                Err(CausalExtractionError::InvalidResponse)
            ));
            let conn = rusqlite::Connection::open(store.path()).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM causal_claims", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        let (_dir, store, scope, artifact_id, source) = fixture();
        let mut provider = provider(&source);
        provider.response.parts.push(ContentPart::ToolCall {
            id: "unexpected".into(),
            name: "read_file".into(),
            args: serde_json::json!({}),
        });
        assert!(matches!(
            extract_candidates_with_provider(
                &store,
                &scope,
                &artifact_id,
                "Why?",
                "model-1",
                &provider,
                authorized,
            )
            .await,
            Err(CausalExtractionError::InvalidResponse)
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_claims", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn unexpected_model_identity_cannot_create_candidate() {
        let (_dir, store, scope, artifact_id, source) = fixture();
        let mut provider = provider(&source);
        provider.response.model_used = "different-model".into();
        assert!(matches!(
            extract_candidates_with_provider(
                &store,
                &scope,
                &artifact_id,
                "Why?",
                "model-1",
                &provider,
                authorized,
            )
            .await,
            Err(CausalExtractionError::InvalidResponse)
        ));
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM causal_claims", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
