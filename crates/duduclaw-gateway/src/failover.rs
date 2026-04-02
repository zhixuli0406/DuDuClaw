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
                match runtime.execute(prompt, context).await {
                    Ok(response) => {
                        self.record_success(primary).await;
                        return Ok(response);
                    }
                    Err(e) => {
                        if is_non_retryable(&e) {
                            // Don't failover on user errors
                            return Err(e);
                        }
                        warn!(
                            runtime = ?primary,
                            error = %e,
                            "Primary runtime failed — attempting failover"
                        );
                        self.record_failure(primary).await;
                        crate::metrics::global_metrics().record_failover();
                    }
                }
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
                        Ok(response) => {
                            self.record_success(fb).await;
                            return Ok(response);
                        }
                        Err(e) => {
                            self.record_failure(fb).await;
                            return Err(format!(
                                "Both primary ({primary:?}) and fallback ({fb:?}) failed. Last error: {e}"
                            ));
                        }
                    }
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
}
