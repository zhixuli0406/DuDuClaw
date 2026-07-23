//! Cross-provider failover — automatic fallback when primary runtime fails.
//!
//! Tracks provider health and routes to fallback runtime on failure.
//! Non-retryable errors (4xx, content policy) do NOT trigger failover.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::runtime::{RuntimeContext, RuntimeRegistry, RuntimeResponse, RuntimeType};

/// Health status for a single provider/runtime.
#[derive(Debug, Clone)]
pub struct ProviderHealth {
    pub runtime_type: RuntimeType,
    pub consecutive_failures: u32,
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
    pub is_available: bool,
    pub cooldown_until: Option<DateTime<Utc>>,
}

impl ProviderHealth {
    fn new(runtime_type: RuntimeType) -> Self {
        Self {
            runtime_type,
            consecutive_failures: 0,
            last_success: None,
            last_failure: None,
            is_available: true,
            cooldown_until: None,
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_success = Some(Utc::now());
        self.is_available = true;
        self.cooldown_until = None;
    }

    fn record_failure(&mut self, cooldown_seconds: i64) {
        self.consecutive_failures += 1;
        self.last_failure = Some(Utc::now());
        if self.consecutive_failures >= 3 {
            self.is_available = false;
            self.cooldown_until = Some(Utc::now() + chrono::Duration::seconds(cooldown_seconds));
            warn!(
                runtime = ?self.runtime_type,
                failures = self.consecutive_failures,
                "Provider marked unavailable — cooldown {cooldown_seconds}s"
            );
        }
    }

    fn check_cooldown(&mut self) -> bool {
        if let Some(until) = self.cooldown_until {
            if Utc::now() >= until {
                self.is_available = true;
                self.cooldown_until = None;
                self.consecutive_failures = 0;
                info!(runtime = ?self.runtime_type, "Provider cooldown expired — re-enabled");
                return true;
            }
        }
        self.is_available
    }
}

/// Manages failover across multiple runtimes.
pub struct FailoverManager {
    health: RwLock<HashMap<RuntimeType, ProviderHealth>>,
    cooldown_seconds: i64,
}

impl FailoverManager {
    pub fn new(cooldown_seconds: i64) -> Self {
        Self {
            health: RwLock::new(HashMap::new()),
            cooldown_seconds,
        }
    }

    /// Execute a prompt with automatic failover.
    ///
    /// Tries primary runtime first. If it fails with a retryable error,
    /// falls back to fallback_runtime. If that also fails, returns error.
    pub async fn execute_with_failover(
        &self,
        registry: &RuntimeRegistry,
        primary: &RuntimeType,
        fallback: Option<&RuntimeType>,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        // Check primary health (with cooldown recovery)
        let primary_available = {
            let mut health = self.health.write().await;
            let entry = health
                .entry(primary.clone())
                .or_insert_with(|| ProviderHealth::new(primary.clone()));
            entry.check_cooldown()
        };

        // Try primary
        if primary_available {
            if let Some(runtime) = registry.get(primary) {
                // Empty content is a FAILURE, not a success: an "Ok" with no text
                // would be silently dropped by every channel (they skip empty
                // sends) and an empty assistant turn would poison the session
                // history — the user sees nothing and the conversation chain
                // breaks. Route it through the same failover + classified-error
                // path as a hard error.
                match runtime.execute(prompt, context).await {
                    Ok(response) if !response.content.trim().is_empty() => {
                        self.record_success(primary).await;
                        return Ok(response);
                    }
                    Ok(_) => {
                        warn!(
                            runtime = ?primary,
                            model = %context.model,
                            reason = "empty_response",
                            "Primary runtime returned empty response — attempting failover"
                        );
                        self.record_failure(primary).await;
                        crate::metrics::global_metrics().record_failover();
                    }
                    Err(e) => {
                        if is_non_retryable(&e) {
                            // Don't failover on user errors
                            return Err(e);
                        }
                        warn!(
                            runtime = ?primary,
                            error = %e,
                            reason = "execution_failed",
                            "Primary runtime failed — attempting failover"
                        );
                        self.record_failure(primary).await;
                        crate::metrics::global_metrics().record_failover();
                    }
                }
            } else {
                // 2026-07-23 distributor incident: the primary runtime's CLI
                // was never registered (binary missing / detection failed at
                // startup) — `registry.get()` returns `None` silently and this
                // branch used to do nothing at all, falling through to the
                // fallback with zero signal. The user believed they were
                // talking to `primary`; a different runtime answered instead.
                // Distinguished from the above cases via `reason =
                // "not_registered"` (vs. `"execution_failed"` /
                // `"empty_response"`) so operators can tell "never had this
                // backend available" apart from "backend errored this call".
                warn!(
                    runtime = ?primary,
                    reason = "not_registered",
                    "Primary runtime not registered (CLI missing or unavailable at startup) \
                     — falling through to fallback"
                );
                crate::metrics::global_metrics().record_failover();
            }
        }

        // Try fallback
        if let Some(fb) = fallback {
            let fb_available = {
                let mut health = self.health.write().await;
                let entry = health
                    .entry(fb.clone())
                    .or_insert_with(|| ProviderHealth::new(fb.clone()));
                entry.check_cooldown()
            };

            if fb_available {
                if let Some(runtime) = registry.get(fb) {
                    info!(runtime = ?fb, "Trying fallback runtime");
                    match runtime.execute(prompt, context).await {
                        Ok(response) if !response.content.trim().is_empty() => {
                            self.record_success(fb).await;
                            return Ok(response);
                        }
                        Ok(_) => {
                            self.record_failure(fb).await;
                            return Err(format!(
                                "Empty response from both primary ({primary:?}) and fallback ({fb:?}) runtimes"
                            ));
                        }
                        Err(e) => {
                            self.record_failure(fb).await;
                            return Err(format!(
                                "Both primary ({primary:?}) and fallback ({fb:?}) failed. Last error: {e}"
                            ));
                        }
                    }
                } else {
                    warn!(
                        runtime = ?fb,
                        reason = "not_registered",
                        "Fallback runtime also not registered — no backend available"
                    );
                }
            }
        }

        Err(format!("Runtime {primary:?} unavailable and no fallback configured"))
    }

    async fn record_success(&self, runtime_type: &RuntimeType) {
        let mut health = self.health.write().await;
        let entry = health
            .entry(runtime_type.clone())
            .or_insert_with(|| ProviderHealth::new(runtime_type.clone()));
        entry.record_success();
    }

    async fn record_failure(&self, runtime_type: &RuntimeType) {
        let mut health = self.health.write().await;
        let entry = health
            .entry(runtime_type.clone())
            .or_insert_with(|| ProviderHealth::new(runtime_type.clone()));
        entry.record_failure(self.cooldown_seconds);
    }

    /// Get health status for all tracked providers.
    pub async fn health_summary(&self) -> Vec<ProviderHealth> {
        self.health.read().await.values().cloned().collect()
    }
}

/// Determine if an error is non-retryable (should NOT trigger failover).
///
/// Checks for HTTP 4xx status codes and specific API error codes.
/// Avoids broad substring matches like "safety" or "content policy" that
/// could unintentionally match legitimate error descriptions.
fn is_non_retryable(error: &str) -> bool {
    let lower = error.to_lowercase();
    // 4xx client errors (except 429 rate limit which is retryable)
    lower.contains("400 ") || lower.contains("401 ") || lower.contains("403 ")
        || lower.contains("404 ") || lower.contains("422 ")
        // Structured API error codes (more specific than free-form message matching)
        || lower.contains("content_policy_violation")
        || lower.contains("content policy violation") // free-form variant (full phrase — avoid matching transient "content policy filter" errors)
        || lower.contains("invalid_api_key")
        || lower.contains("billing_hard_limit")
        // Legacy formatted messages kept for compatibility
        || lower.contains("400 bad request")
        || lower.contains("401 unauthorized")
        || lower.contains("403 forbidden")
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_health_success() {
        let mut health = ProviderHealth::new(RuntimeType::Claude);
        health.record_failure(300);
        health.record_failure(300);
        assert_eq!(health.consecutive_failures, 2);
        assert!(health.is_available);

        health.record_success();
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn test_provider_health_cooldown_trigger() {
        let mut health = ProviderHealth::new(RuntimeType::Gemini);
        health.record_failure(300);
        health.record_failure(300);
        health.record_failure(300); // 3rd failure → cooldown
        assert!(!health.is_available);
        assert!(health.cooldown_until.is_some());
    }

    #[test]
    fn test_is_non_retryable() {
        assert!(is_non_retryable("400 bad request: invalid JSON"));
        assert!(is_non_retryable("Content policy violation"));
        assert!(!is_non_retryable("429 rate limited"));
        assert!(!is_non_retryable("500 internal server error"));
        assert!(!is_non_retryable("connection refused"));
    }

    // ── Empty-response failover (2026-07-22 distributor bug) ─────────
    //
    // A runtime returning Ok("") used to be recorded as a SUCCESS; the empty
    // reply was then silently skipped by every channel (user saw nothing) and
    // an empty assistant turn poisoned the session. These tests pin the fix:
    // empty content ⇒ failover to the fallback runtime, and if that is also
    // empty ⇒ a classifiable "Empty response" error.

    struct StubRuntime {
        content: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for StubRuntime {
        fn name(&self) -> &str {
            "stub"
        }
        async fn execute(
            &self,
            _prompt: &str,
            context: &RuntimeContext,
        ) -> Result<RuntimeResponse, String> {
            Ok(RuntimeResponse {
                content: self.content.to_string(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                model_used: context.model.clone(),
                runtime_name: "stub".to_string(),
            })
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    fn stub_context() -> RuntimeContext {
        RuntimeContext {
            agent_dir: None,
            system_prompt: String::new(),
            model: "grok-4.1-fast".to_string(),
            max_tokens: 1024,
            home_dir: std::path::PathBuf::from("/tmp"),
            agent_id: "test".to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
        }
    }

    fn stub_registry(primary_content: &'static str, fallback_content: &'static str) -> RuntimeRegistry {
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Grok, Box::new(StubRuntime { content: primary_content }));
        map.insert(RuntimeType::Claude, Box::new(StubRuntime { content: fallback_content }));
        RuntimeRegistry::with_runtimes(map)
    }

    #[tokio::test]
    async fn empty_primary_fails_over_to_fallback() {
        let mgr = FailoverManager::new(300);
        let reg = stub_registry("", "real answer");
        let res = mgr
            .execute_with_failover(&reg, &RuntimeType::Grok, Some(&RuntimeType::Claude), "hi", &stub_context())
            .await
            .expect("fallback should answer");
        assert_eq!(res.content, "real answer");
    }

    #[tokio::test]
    async fn empty_primary_and_fallback_is_classifiable_error() {
        let mgr = FailoverManager::new(300);
        let reg = stub_registry("", "   ");
        let err = mgr
            .execute_with_failover(&reg, &RuntimeType::Grok, Some(&RuntimeType::Claude), "hi", &stub_context())
            .await
            .expect_err("double-empty must be an error");
        // Must contain "empty response" (case-insensitive) so
        // channel_reply::classify_cli_failure maps it to FailureReason::EmptyResponse
        // and the user gets the 空回應 fallback message instead of silence.
        assert!(err.to_lowercase().contains("empty response"), "got: {err}");
    }

    #[tokio::test]
    async fn nonempty_primary_still_succeeds() {
        let mgr = FailoverManager::new(300);
        let reg = stub_registry("primary answer", "unused");
        let res = mgr
            .execute_with_failover(&reg, &RuntimeType::Grok, Some(&RuntimeType::Claude), "hi", &stub_context())
            .await
            .expect("primary should answer");
        assert_eq!(res.content, "primary answer");
    }

    // ── Unregistered-primary failover observability (2026-07-23 distributor
    // incident) ──────────────────────────────────────────────────────────
    //
    // A distributor's container had no `grok` CLI installed, so `GrokRuntime`
    // never registered. `registry.get(primary)` returned `None` and the old
    // code fell straight through to the fallback with zero signal — the user
    // believed they were talking to Grok while Claude silently answered.
    // These tests pin: (a) the fall-through still works (availability is
    // unaffected), (b) it is now observable via the failover metric, and
    // (c) it's still a clean, classifiable error when no fallback exists.

    fn registry_without_primary(fallback_content: &'static str) -> RuntimeRegistry {
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Claude, Box::new(StubRuntime { content: fallback_content }));
        RuntimeRegistry::with_runtimes(map)
    }

    #[tokio::test]
    async fn unregistered_primary_falls_through_to_fallback_and_is_recorded() {
        let mgr = FailoverManager::new(300);
        // RuntimeType::Grok is deliberately absent — simulates the missing CLI.
        let reg = registry_without_primary("fallback answered");
        let before = crate::metrics::global_metrics()
            .failover_total
            .load(std::sync::atomic::Ordering::Relaxed);

        let res = mgr
            .execute_with_failover(&reg, &RuntimeType::Grok, Some(&RuntimeType::Claude), "hi", &stub_context())
            .await
            .expect("fallback should answer when primary CLI isn't registered");
        assert_eq!(res.content, "fallback answered");

        let after = crate::metrics::global_metrics()
            .failover_total
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            after > before,
            "an unregistered primary must still record a failover event (was silent before this fix)"
        );
    }

    #[tokio::test]
    async fn unregistered_primary_with_no_fallback_is_a_classifiable_error() {
        let mgr = FailoverManager::new(300);
        let reg = RuntimeRegistry::with_runtimes(HashMap::new());
        let err = mgr
            .execute_with_failover(&reg, &RuntimeType::Grok, None, "hi", &stub_context())
            .await
            .expect_err("no primary registered and no fallback configured ⇒ error, not a panic");
        assert!(err.contains("unavailable"), "got: {err}");
    }
}
