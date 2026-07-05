//! Fallback router — cooldown-aware, context-window-aware candidate
//! selection over a chain of qualified model ids.
//!
//! Pure logic: candidate filtering, cooldown bookkeeping, and failover
//! ordering are all testable without HTTP (the provider lookup is injected).
//! No circuit-breaker dependency — cooldowns are the (simpler) mechanism
//! matching the gateway AccountRotator semantics: rate-limit → short
//! cooldown (Retry-After if given, else 120s), billing exhaustion → 24h,
//! other failover-class errors → 60s.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::LlmError;
use crate::provider::{split_model_id, ChatProvider};
use crate::registry::ModelRegistry;
use crate::types::{ChatRequest, ChatResponse};

/// Default cooldown for rate limits without a Retry-After header.
pub const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(120);
/// Cooldown for billing / credit exhaustion.
pub const BILLING_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
/// Cooldown for other failover-class errors (timeout, 5xx, auth, network).
pub const GENERIC_COOLDOWN: Duration = Duration::from_secs(60);

/// Cooldown duration for a failed call, `None` when the error is not
/// failover-class (no cooldown — the request itself is at fault).
pub fn cooldown_for(err: &LlmError) -> Option<Duration> {
    if !err.is_failover() {
        return None;
    }
    Some(match err {
        LlmError::RateLimited { retry_after } => retry_after.unwrap_or(RATE_LIMIT_COOLDOWN),
        LlmError::Billing => BILLING_COOLDOWN,
        _ => GENERIC_COOLDOWN,
    })
}

/// Why a candidate was skipped or failed, for post-mortem reporting.
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateOutcome {
    SkippedCoolingDown,
    SkippedContextWindow { window: u64, estimated: u64 },
    SkippedNoProvider,
    Failed(LlmError),
}

/// Fallback chain over qualified model ids (`"anthropic/claude-sonnet-5"`).
pub struct FallbackRouter {
    chain: Vec<String>,
    /// (provider, model) → cooled down until.
    cooldowns: HashMap<(String, String), Instant>,
}

impl FallbackRouter {
    pub fn new(chain: Vec<String>) -> Self {
        Self { chain, cooldowns: HashMap::new() }
    }

    pub fn chain(&self) -> &[String] {
        &self.chain
    }

    fn key(model: &str) -> (String, String) {
        let (provider, bare) = split_model_id(model);
        (provider.unwrap_or("").to_string(), bare.to_string())
    }

    /// Is this model currently cooling down?
    pub fn is_cooling(&self, model: &str, now: Instant) -> bool {
        self.cooldowns
            .get(&Self::key(model))
            .is_some_and(|until| *until > now)
    }

    /// Record a failure; applies a cooldown for failover-class errors.
    pub fn note_failure(&mut self, model: &str, err: &LlmError, now: Instant) {
        if let Some(cooldown) = cooldown_for(err) {
            tracing::warn!(model, error = %err, cooldown_secs = cooldown.as_secs(), "llm router: cooling down candidate");
            self.cooldowns.insert(Self::key(model), now + cooldown);
        }
    }

    /// Record a success; clears any cooldown for the model.
    pub fn note_success(&mut self, model: &str) {
        self.cooldowns.remove(&Self::key(model));
    }

    /// Viable candidates for a request: in chain order, skipping models that
    /// are cooling down or whose known context window is smaller than the
    /// estimated input. Models unknown to the registry are kept (no window
    /// metadata → cannot filter; callers wire custom models deliberately).
    pub fn candidates(
        &self,
        estimated_input_tokens: u64,
        registry: &ModelRegistry,
        now: Instant,
    ) -> Vec<String> {
        self.chain
            .iter()
            .filter(|m| !self.is_cooling(m, now))
            .filter(|m| match registry.context_window(m) {
                Some(window) => estimated_input_tokens <= window,
                None => true,
            })
            .cloned()
            .collect()
    }

    /// Run the fallback chain: try each viable candidate in order, failing
    /// over on `LlmError::is_failover()` errors (with cooldown bookkeeping)
    /// and returning immediately on success or on a terminal error.
    ///
    /// `providers` maps a provider id (the prefix of a qualified model id)
    /// to a [`ChatProvider`] — injected so this stays testable without HTTP.
    /// The request's `model` field is rewritten to each candidate in turn.
    ///
    /// Fail-closed: an empty candidate list or a chain entry without a
    /// registered provider is an error, never a silent skip of the whole
    /// mechanism.
    pub async fn complete(
        &mut self,
        req: &ChatRequest,
        registry: &ModelRegistry,
        providers: &dyn Fn(&str) -> Option<Arc<dyn ChatProvider>>,
    ) -> Result<ChatResponse, (Vec<(String, CandidateOutcome)>, LlmError)> {
        let now = Instant::now();
        let estimated = req.estimate_input_tokens();
        let mut outcomes: Vec<(String, CandidateOutcome)> = Vec::new();

        // Snapshot skips for reporting.
        for m in &self.chain {
            if self.is_cooling(m, now) {
                outcomes.push((m.clone(), CandidateOutcome::SkippedCoolingDown));
            } else if let Some(window) = registry.context_window(m) {
                if estimated > window {
                    outcomes.push((
                        m.clone(),
                        CandidateOutcome::SkippedContextWindow { window, estimated },
                    ));
                }
            }
        }

        let candidates = self.candidates(estimated, registry, now);
        if candidates.is_empty() {
            return Err((
                outcomes,
                LlmError::InvalidRequest(
                    "no viable model candidates (all cooling down or context-window filtered)"
                        .to_string(),
                ),
            ));
        }

        let mut last_err: Option<LlmError> = None;
        for model in candidates {
            let (provider_id, _bare) = split_model_id(&model);
            let Some(provider) = provider_id.and_then(|p| providers(p)) else {
                outcomes.push((model.clone(), CandidateOutcome::SkippedNoProvider));
                continue;
            };

            let mut attempt = req.clone();
            attempt.model = model.clone();
            match provider.complete(&attempt).await {
                Ok(resp) => {
                    self.note_success(&model);
                    return Ok(resp);
                }
                Err(e) if e.is_failover() => {
                    self.note_failure(&model, &e, Instant::now());
                    outcomes.push((model.clone(), CandidateOutcome::Failed(e.clone())));
                    last_err = Some(e);
                }
                Err(e) => {
                    // Terminal (InvalidRequest / ContentFilter / Parse) —
                    // surfacing beats masking with a different model's output.
                    outcomes.push((model.clone(), CandidateOutcome::Failed(e.clone())));
                    return Err((outcomes, e));
                }
            }
        }

        let err = last_err.unwrap_or_else(|| {
            LlmError::InvalidRequest("no provider registered for any chain candidate".to_string())
        });
        Err((outcomes, err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LlmError;
    use crate::types::{NormalizedUsage, StopReason, StreamEvent};
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use std::sync::Mutex;

    /// Mock provider: scripted responses per model id.
    struct MockProvider {
        id: String,
        script: Mutex<HashMap<String, Vec<Result<String, LlmError>>>>,
        calls: Mutex<Vec<String>>,
    }

    impl MockProvider {
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: id.to_string(),
                script: Mutex::new(HashMap::new()),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn on(self: &Arc<Self>, model: &str, result: Result<&str, LlmError>) {
            self.script
                .lock()
                .unwrap()
                .entry(model.to_string())
                .or_default()
                .push(result.map(|s| s.to_string()));
        }
    }

    #[async_trait]
    impl ChatProvider for MockProvider {
        fn id(&self) -> &str {
            &self.id
        }

        async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
            self.calls.lock().unwrap().push(req.model.clone());
            let mut script = self.script.lock().unwrap();
            let queue = script.get_mut(&req.model).expect("unscripted model");
            match queue.remove(0) {
                Ok(text) => Ok(ChatResponse {
                    parts: vec![crate::types::ContentPart::Text(text)],
                    stop: StopReason::EndTurn,
                    usage: NormalizedUsage::default(),
                    model_used: req.model.clone(),
                    provider: self.id.clone(),
                }),
                Err(e) => Err(e),
            }
        }

        async fn stream(
            &self,
            _req: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            unimplemented!("not used in router tests")
        }
    }

    fn lookup(
        providers: Vec<Arc<MockProvider>>,
    ) -> impl Fn(&str) -> Option<Arc<dyn ChatProvider>> {
        move |id: &str| {
            providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| Arc::clone(p) as Arc<dyn ChatProvider>)
        }
    }

    #[test]
    fn cooldown_durations_match_failure_class() {
        assert_eq!(
            cooldown_for(&LlmError::RateLimited { retry_after: Some(Duration::from_secs(7)) }),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            cooldown_for(&LlmError::RateLimited { retry_after: None }),
            Some(RATE_LIMIT_COOLDOWN)
        );
        assert_eq!(cooldown_for(&LlmError::Billing), Some(BILLING_COOLDOWN));
        assert_eq!(cooldown_for(&LlmError::Timeout), Some(GENERIC_COOLDOWN));
        // Non-failover errors never cool a model down.
        assert_eq!(cooldown_for(&LlmError::InvalidRequest("x".into())), None);
        assert_eq!(cooldown_for(&LlmError::ContentFilter), None);
    }

    #[test]
    fn cooling_model_is_skipped_then_recovers() {
        let mut router = FallbackRouter::new(vec![
            "anthropic/claude-sonnet-5".to_string(),
            "openai/gpt-5.4".to_string(),
        ]);
        let reg = ModelRegistry::vendored();
        let now = Instant::now();
        router.note_failure(
            "anthropic/claude-sonnet-5",
            &LlmError::RateLimited { retry_after: Some(Duration::from_secs(120)) },
            now,
        );
        assert!(router.is_cooling("anthropic/claude-sonnet-5", now));
        let c = router.candidates(1000, &reg, now);
        assert_eq!(c, vec!["openai/gpt-5.4".to_string()]);
        // After the cooldown elapses the model is viable again.
        let later = now + Duration::from_secs(121);
        assert!(!router.is_cooling("anthropic/claude-sonnet-5", later));
        assert_eq!(router.candidates(1000, &reg, later).len(), 2);
        // note_success clears immediately.
        router.note_failure("openai/gpt-5.4", &LlmError::Billing, now);
        router.note_success("openai/gpt-5.4");
        assert!(!router.is_cooling("openai/gpt-5.4", now));
    }

    #[test]
    fn context_window_filtering_skips_small_models_keeps_unknown() {
        let router = FallbackRouter::new(vec![
            "anthropic/claude-haiku-4-5".to_string(), // 200k window
            "xai/grok-4.1-fast".to_string(),          // 2M window
            "local/custom-model".to_string(),         // unknown → kept
        ]);
        let reg = ModelRegistry::vendored();
        let c = router.candidates(500_000, &reg, Instant::now());
        assert_eq!(
            c,
            vec!["xai/grok-4.1-fast".to_string(), "local/custom-model".to_string()]
        );
    }

    #[tokio::test]
    async fn failover_moves_down_the_chain_in_order() {
        let anthropic = MockProvider::new("anthropic");
        anthropic.on(
            "anthropic/claude-sonnet-5",
            Err(LlmError::RateLimited { retry_after: None }),
        );
        let openai = MockProvider::new("openai");
        openai.on("openai/gpt-5.4", Ok("fallback answer"));

        let mut router = FallbackRouter::new(vec![
            "anthropic/claude-sonnet-5".to_string(),
            "openai/gpt-5.4".to_string(),
        ]);
        let reg = ModelRegistry::vendored();
        let get = lookup(vec![Arc::clone(&anthropic), Arc::clone(&openai)]);

        let req = ChatRequest::new("anthropic/claude-sonnet-5");
        let resp = router.complete(&req, &reg, &get).await.expect("failover succeeds");
        assert_eq!(resp.text(), "fallback answer");
        assert_eq!(resp.provider, "openai");
        // First candidate is now cooling down.
        assert!(router.is_cooling("anthropic/claude-sonnet-5", Instant::now()));
    }

    #[tokio::test]
    async fn terminal_error_stops_the_chain() {
        let anthropic = MockProvider::new("anthropic");
        anthropic.on(
            "anthropic/claude-sonnet-5",
            Err(LlmError::InvalidRequest("bad schema".into())),
        );
        let openai = MockProvider::new("openai");

        let mut router = FallbackRouter::new(vec![
            "anthropic/claude-sonnet-5".to_string(),
            "openai/gpt-5.4".to_string(),
        ]);
        let reg = ModelRegistry::vendored();
        let get = lookup(vec![Arc::clone(&anthropic), Arc::clone(&openai)]);

        let req = ChatRequest::new("anthropic/claude-sonnet-5");
        let err = router.complete(&req, &reg, &get).await.expect_err("terminal");
        assert!(matches!(err.1, LlmError::InvalidRequest(_)));
        // The second provider must never have been called.
        assert!(openai.calls.lock().unwrap().is_empty());
        // And InvalidRequest does not cool the model down.
        assert!(!router.is_cooling("anthropic/claude-sonnet-5", Instant::now()));
    }

    #[tokio::test]
    async fn all_candidates_exhausted_returns_last_error_and_outcomes() {
        let anthropic = MockProvider::new("anthropic");
        anthropic.on("anthropic/claude-sonnet-5", Err(LlmError::Timeout));
        let openai = MockProvider::new("openai");
        openai.on("openai/gpt-5.4", Err(LlmError::Billing));

        let mut router = FallbackRouter::new(vec![
            "anthropic/claude-sonnet-5".to_string(),
            "openai/gpt-5.4".to_string(),
        ]);
        let reg = ModelRegistry::vendored();
        let get = lookup(vec![anthropic, openai]);

        let req = ChatRequest::new("anthropic/claude-sonnet-5");
        let (outcomes, err) = router.complete(&req, &reg, &get).await.expect_err("exhausted");
        assert_eq!(err, LlmError::Billing);
        let failed: Vec<_> = outcomes
            .iter()
            .filter(|(_, o)| matches!(o, CandidateOutcome::Failed(_)))
            .collect();
        assert_eq!(failed.len(), 2);
    }

    #[tokio::test]
    async fn empty_viable_set_is_an_error() {
        let mut router = FallbackRouter::new(vec!["anthropic/claude-haiku-4-5".to_string()]);
        let reg = ModelRegistry::vendored();
        let get = lookup(vec![]);
        // Estimated input far beyond haiku's 200k window → filtered out.
        let mut req = ChatRequest::new("anthropic/claude-haiku-4-5");
        req.system.push(crate::types::SystemBlock::cached("x".repeat(2_000_000)));
        let (_, err) = router.complete(&req, &reg, &get).await.expect_err("no candidates");
        assert!(matches!(err, LlmError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn missing_provider_is_skipped_not_fatal() {
        let openai = MockProvider::new("openai");
        openai.on("openai/gpt-5.4", Ok("ok"));
        let mut router = FallbackRouter::new(vec![
            "gemini/gemini-3.1-pro".to_string(), // no provider registered
            "openai/gpt-5.4".to_string(),
        ]);
        let reg = ModelRegistry::vendored();
        let get = lookup(vec![openai]);
        let req = ChatRequest::new("gemini/gemini-3.1-pro");
        let resp = router.complete(&req, &reg, &get).await.expect("skips to openai");
        assert_eq!(resp.text(), "ok");
    }
}
