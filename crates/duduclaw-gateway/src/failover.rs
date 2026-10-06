//! Failover — "who takes over when the current attempt fails".
//!
//! # Module boundary (O1, 2026-09-29)
//!
//! DuDuClaw answers that question at **three** ordered layers. They used to
//! live in three modules whose separation was maintained only by doc comments
//! (a fourth, `duduclaw-llm::FallbackRouter`, was never wired to anything and
//! was deleted). Two of the three now live here, side by side, so the boundary
//! is visible in one file:
//!
//! | Layer | Question | Where | Trigger |
//! |-------|----------|-------|---------|
//! | 1. Account | same model, a different credential? | `account_rotator` (`Failover` strategy + cooldowns) | rate-limit / billing / auth on one account |
//! | 2. Model | same runtime, a **lighter model**? | [`model`] + [`FailoverManager::model_fallback_for`] | hard timeout / 503 / 429 / "overloaded" |
//! | 3. Runtime | a different **provider CLI** entirely? | [`FailoverManager::execute_with_failover`] | primary runtime returned a retryable error |
//!
//! They compose bottom-up: the account rotator exhausts its pool first, then
//! `claude_runner` consults layer 2 before giving up on the runtime, and only
//! `runtime_dispatch` — which owns the [`FailoverManager`] instance — reaches
//! layer 3. Layer 2 is a set of **pure functions plus one associated
//! function**, deliberately not methods on [`FailoverManager`]: it holds no
//! health state, and `claude_runner` has no manager instance to borrow.
//!
//! Tracks provider health and routes to fallback runtime on failure.
//! Non-retryable errors (4xx, content policy) do NOT trigger failover.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{info, warn};

pub mod model;

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

    /// **Layer 2** — decide whether a failed attempt should be retried with a
    /// lighter model on the *same* runtime, and with which model.
    ///
    /// Returns `Some(fallback)` exactly when both layer-2 predicates hold:
    /// the error is a transient infrastructure failure a different model might
    /// avoid ([`model::is_llm_fallback_error`]) **and** the pair is a usable,
    /// non-looping one ([`model::should_attempt_model_fallback`]). `None`
    /// means "do not retry at this layer" — the caller propagates the original
    /// error, or falls through to layer 3.
    ///
    /// An **associated** function, not a method: layer 2 holds no health state,
    /// and its three call sites in `claude_runner` have no manager instance to
    /// borrow (the only instance is `runtime_dispatch`'s leaked static, which
    /// exists to serve layer 3).
    pub fn model_fallback_for<'a>(
        primary: &str,
        fallback: &'a str,
        error: &str,
    ) -> Option<&'a str> {
        if model::is_llm_fallback_error(error)
            && model::should_attempt_model_fallback(primary, fallback)
        {
            Some(fallback)
        } else {
            None
        }
    }

    /// Execute a prompt with automatic failover.
    ///
    /// Tries primary runtime first. If it fails with a retryable error,
    /// falls back to fallback_runtime. If that also fails, returns error.
    ///
    /// `model_fallbacks` is the agent's `agent.toml [model] fallbacks` chain
    /// (see [`crate::runtime_config::agent_model_fallbacks`]). It exists so the
    /// *fallback* runtime is handed a model it can actually serve — see
    /// [`resolve_fallback_model`] for the four-branch resolution. Passing an
    /// empty slice keeps the pre-WP-A behavior for branches (2)/(3)/(4) only:
    /// a foreign `context.model` is never forwarded verbatim any more.
    ///
    /// It is a parameter rather than a [`RuntimeContext`] field deliberately:
    /// `RuntimeContext` is built by struct literal in several runtime test
    /// modules, and a new required field would be a wide, unrelated edit for
    /// what only this one call chain consumes.
    pub async fn execute_with_failover(
        &self,
        registry: &RuntimeRegistry,
        primary: &RuntimeType,
        fallback: Option<&RuntimeType>,
        prompt: &str,
        context: &RuntimeContext,
        model_fallbacks: &[String],
    ) -> Result<RuntimeResponse, String> {
        // Check primary health (with cooldown recovery)
        let primary_available = {
            let mut health = self.health.write().await;
            let entry = health
                .entry(primary.clone())
                .or_insert_with(|| ProviderHealth::new(primary.clone()));
            entry.check_cooldown()
        };

        // P0/WP-B: the primary's own error, kept so a *refused* cross-family
        // failover can return the real root cause instead of a generic
        // "unavailable" (the live test's codex argv bug must stay visible in
        // the judge-seam audit, not be replaced by the guard's own message).
        let mut primary_error: Option<String> = None;

        // Try primary
        if primary_available {
            if let Some(runtime) = registry.get(primary) {
                // Empty content is a FAILURE, not a success: an "Ok" with no text
                // would be silently dropped by every channel (they skip empty
                // sends) and an empty assistant turn would poison the session
                // history — the user sees nothing and the conversation chain
                // breaks. Route it through the same failover + classified-error
                // path as a hard error.
                match {
                    #[cfg(test)]
                    crate::model_call_probe::record("failover_execute");
                    runtime.execute(prompt, context).await
                } {
                    Ok(response) if !response.content.trim().is_empty() => {
                        self.record_success(primary).await;
                        // Truthful attribution (live round 3 E2): tell a
                        // scoped caller which runtime/model really answered.
                        // On this leg they equal what was requested; the
                        // fallback leg below overwrites when they do not.
                        crate::runtime::record_runtime_outcome(primary.clone(), &context.model);
                        if response.input_tokens > 0 || response.output_tokens > 0 {
                            crate::runtime::record_role_usage(crate::role_turns::RoleTurnUsage {
                                usage_input_tokens: Some(response.input_tokens),
                                usage_output_tokens: Some(response.output_tokens),
                                usage_cache_read_tokens: None,
                                usage_legs: None,
                            });
                        }
                        return Ok(response);
                    }
                    Ok(_) => {
                        warn!(
                            runtime = ?primary,
                            model = %context.model,
                            reason = "empty_response",
                            "Primary runtime returned empty response — attempting failover"
                        );
                        primary_error = Some(format!("Empty response from primary ({primary:?})"));
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
                        primary_error = Some(e);
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
                    // WP-A1: the fallback runtime must NOT inherit the primary's
                    // model. Before this, a codex agent whose `[model] preferred`
                    // is `gpt-5.4` failed over to Claude and the Claude runtime
                    // was spawned with `--model gpt-5.4` — a foreign model id
                    // that the backend rejects (or, worse, silently substitutes),
                    // and `model_used` then reported `gpt-5.4` for a Claude call.
                    let Some((fb_model, model_source)) =
                        resolve_fallback_model(fb, &context.model, model_fallbacks)
                    else {
                        // (4) Nothing nameable for this runtime — refuse to
                        // spawn rather than send a foreign model id. Counted as
                        // a failed attempt so a permanently mis-configured
                        // fallback cools down instead of being retried forever.
                        warn!(
                            agent = %context.agent_id,
                            from_runtime = ?primary,
                            to_runtime = ?fb,
                            from_model = %context.model,
                            reason = "no_model_for_fallback_runtime",
                            "Fallback runtime has no configured or catalog model — refusing to spawn"
                        );
                        self.record_failure(fb).await;
                        return Err(format!(
                            "Primary ({primary:?}) failed and no model configured for fallback runtime {fb:?} \
                             — set `agent.toml [model] fallbacks` to a model that runtime serves"
                        ));
                    };

                    // P0/WP-B: a call that cares WHICH FAMILY answers (the
                    // decorrelated acceptance judge) opts out of cross-family
                    // failover. Compared at model level, not runtime level, so
                    // another Claude tier still counts as same-family and is
                    // allowed — it is the substituted model that decides whose
                    // blind spots the answer inherits.
                    //
                    // Fail-closed on "cannot prove same family": an unknown
                    // model id on either side blocks the hop. The caller then
                    // degrades explicitly (audited) to the resolution it would
                    // have used anyway, so refusing costs nothing and never
                    // stalls a verdict. `fb` is NOT recorded as a failure — it
                    // did not fail, it was declined.
                    // P2 live probe (2026-09-25): an UNKNOWN model id kept verbatim
                    // across the hop (`kimi:no-such-model` → Claude) compared equal
                    // to itself and slipped through. "Same family" therefore also
                    // requires the family to be *known* — fail-closed as documented.
                    let family_known =
                        crate::runtime_config::model_family(&context.model).is_some();
                    if !context.allow_cross_family_failover
                        && (!family_known
                            || !crate::runtime_config::same_model_family(&context.model, &fb_model))
                    {
                        warn!(
                            agent = %context.agent_id,
                            from_runtime = ?primary,
                            to_runtime = ?fb,
                            from_model = %context.model,
                            to_model = %fb_model,
                            reason = "cross_family_failover_blocked",
                            "Refusing cross-family failover — this call requires its own model family"
                        );
                        return Err(match primary_error {
                            Some(e) => format!(
                                "{e} (cross-family failover to {fb:?}/{fb_model} refused for this call)"
                            ),
                            None => format!(
                                "Runtime {primary:?} unavailable and cross-family failover to \
                                 {fb:?}/{fb_model} refused for this call"
                            ),
                        });
                    }

                    warn!(
                        agent = %context.agent_id,
                        from_runtime = ?primary,
                        to_runtime = ?fb,
                        from_model = %context.model,
                        to_model = %fb_model,
                        model_source,
                        "Failing over to a different runtime — substituting a model it serves"
                    );

                    // Clone-and-override rather than mutating the caller's
                    // context: the caller (and the cost-telemetry attribution
                    // that reads `resp.model_used`) must still see the model it
                    // originally requested for the primary attempt.
                    let fb_context = if fb_model == context.model {
                        None
                    } else {
                        let mut c = context.clone();
                        c.model = fb_model;
                        Some(c)
                    };
                    let exec_context = fb_context.as_ref().unwrap_or(context);

                    info!(runtime = ?fb, "Trying fallback runtime");
                    match {
                        #[cfg(test)]
                        crate::model_call_probe::record("failover_execute");
                        runtime.execute(prompt, exec_context).await
                    } {
                        Ok(response) if !response.content.trim().is_empty() => {
                            self.record_success(fb).await;
                            // Truthful attribution (live round 3 E2): this
                            // answer came from `fb` on the SUBSTITUTED model,
                            // not from the requested pair. A caller that
                            // records a ledger row must be able to say so —
                            // `role_turns.jsonl` used to record the requested
                            // codex/gpt-5.6-sol for work Claude actually did.
                            crate::runtime::record_runtime_outcome(fb.clone(), &exec_context.model);
                            if response.input_tokens > 0 || response.output_tokens > 0 {
                                crate::runtime::record_role_usage(
                                    crate::role_turns::RoleTurnUsage {
                                        usage_input_tokens: Some(response.input_tokens),
                                        usage_output_tokens: Some(response.output_tokens),
                                        usage_cache_read_tokens: None,
                                        usage_legs: None,
                                    },
                                );
                            }
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

        Err(format!(
            "Runtime {primary:?} unavailable and no fallback configured"
        ))
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

/// Pick the model to hand a **fallback** runtime, in four ordered branches.
///
/// Returns `(model_id, source_tag)`, or `None` when no model can honestly be
/// named for `to` — in which case the caller must NOT spawn (sending a foreign
/// model id is the bug this function exists to prevent).
///
/// 1. **Agent-configured chain** — the first `agent.toml [model] fallbacks`
///    entry whose family *confidently* belongs to `to`. "Confidently" means
///    [`runtime_catalog::runtime_for_model`] recognizes the family AND
///    [`crate::runtime_config::model_matches_provider`] accepts it; an
///    unrecognized family is skipped here (it is exactly the case where
///    `model_matches_provider`'s lenient "unknown ⇒ never a mismatch" rule
///    would otherwise let a foreign id through).
///    A qualified entry (`"openai/gpt-5.4"`, `"compat:deepseek/deepseek-v3.2"`)
///    is unqualified with [`duduclaw_llm::split_model_id`] — the same dialect
///    the Direct-API chain uses (`claude_runner::provider_and_bare`), so this
///    introduces no second parsing convention.
/// 2. **Keep `context.model`** when it already matches `to`. Uses
///    `model_matches_provider` unchanged, so an *unknown* family stays
///    permitted (repo doctrine: only confident mismatches are flagged) — that
///    covers `openai_compat`, which declares no family and legitimately proxies
///    arbitrary model ids.
/// 3. **Catalog default** — the first entry of the runtime's
///    [`RuntimeSpec::fallback_models`], i.e. the same static list the dashboard
///    offers when live model discovery fails.
/// 4. Otherwise `None`.
///
/// [`RuntimeSpec::fallback_models`]: duduclaw_core::runtime_catalog::RuntimeSpec::fallback_models
/// [`runtime_catalog::runtime_for_model`]: duduclaw_core::runtime_catalog::runtime_for_model
fn resolve_fallback_model(
    to: &RuntimeType,
    current_model: &str,
    model_fallbacks: &[String],
) -> Option<(String, &'static str)> {
    // (1) the agent's own cross-provider chain
    for raw in model_fallbacks {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        if duduclaw_core::runtime_catalog::runtime_for_model(entry).is_none() {
            continue;
        }
        if !crate::runtime_config::model_matches_provider(entry, *to) {
            continue;
        }
        let (_qualifier, bare) = duduclaw_llm::split_model_id(entry);
        return Some((bare.to_string(), "agent_fallbacks"));
    }

    // (2) the requested model is already one this runtime serves
    let current = current_model.trim();
    if !current.is_empty() && crate::runtime_config::model_matches_provider(current, *to) {
        return Some((current.to_string(), "context_model"));
    }

    // (3) the catalog's own default for this runtime
    if let Some((id, _label)) = to.spec().fallback_models.first() {
        return Some(((*id).to_string(), "catalog_default"));
    }

    // (4) nothing nameable
    None
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
            account_pool: vec![],
            effort: None,
            allow_cross_family_failover: true,
        }
    }

    fn stub_registry(
        primary_content: &'static str,
        fallback_content: &'static str,
    ) -> RuntimeRegistry {
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(
            RuntimeType::Grok,
            Box::new(StubRuntime {
                content: primary_content,
            }),
        );
        map.insert(
            RuntimeType::Claude,
            Box::new(StubRuntime {
                content: fallback_content,
            }),
        );
        RuntimeRegistry::with_runtimes(map)
    }

    #[tokio::test]
    async fn empty_primary_fails_over_to_fallback() {
        let mgr = FailoverManager::new(300);
        let reg = stub_registry("", "real answer");
        let res = mgr
            .execute_with_failover(
                &reg,
                &RuntimeType::Grok,
                Some(&RuntimeType::Claude),
                "hi",
                &stub_context(),
                &[],
            )
            .await
            .expect("fallback should answer");
        assert_eq!(res.content, "real answer");
    }

    #[tokio::test]
    async fn empty_primary_and_fallback_is_classifiable_error() {
        let mgr = FailoverManager::new(300);
        let reg = stub_registry("", "   ");
        let err = mgr
            .execute_with_failover(
                &reg,
                &RuntimeType::Grok,
                Some(&RuntimeType::Claude),
                "hi",
                &stub_context(),
                &[],
            )
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
            .execute_with_failover(
                &reg,
                &RuntimeType::Grok,
                Some(&RuntimeType::Claude),
                "hi",
                &stub_context(),
                &[],
            )
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
        map.insert(
            RuntimeType::Claude,
            Box::new(StubRuntime {
                content: fallback_content,
            }),
        );
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
            .execute_with_failover(
                &reg,
                &RuntimeType::Grok,
                Some(&RuntimeType::Claude),
                "hi",
                &stub_context(),
                &[],
            )
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
            .execute_with_failover(&reg, &RuntimeType::Grok, None, "hi", &stub_context(), &[])
            .await
            .expect_err("no primary registered and no fallback configured ⇒ error, not a panic");
        assert!(err.contains("unavailable"), "got: {err}");
    }

    // ── WP-A1: cross-runtime failover must not carry the primary's model ──
    //
    // A codex agent (`[model] preferred = "gpt-5.4"`) failing over to Claude
    // used to spawn the Claude runtime with `--model gpt-5.4` and then report
    // `model_used = "gpt-5.4"` for a Claude call. These pin all four branches
    // of `resolve_fallback_model` plus the end-to-end substitution.

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fallback_model_branch1_first_matching_agent_fallback_wins() {
        // Chain order matters: the deepseek entry comes first but its family is
        // unrecognized, so it must be SKIPPED rather than handed to Claude.
        let chain = strings(&[
            "compat:deepseek/deepseek-v3.2",
            "anthropic/claude-sonnet-4-6",
            "claude-haiku-4-5",
        ]);
        let (model, source) =
            resolve_fallback_model(&RuntimeType::Claude, "gpt-5.4", &chain).expect("claude entry");
        // Qualified entries are unqualified with the Direct-API dialect.
        assert_eq!(model, "claude-sonnet-4-6");
        assert_eq!(source, "agent_fallbacks");
    }

    #[test]
    fn fallback_model_branch1_skips_entries_of_a_foreign_family() {
        // Only the gemini entry belongs to the Gemini runtime.
        let chain = strings(&["claude-opus-4-6", "gpt-5.4", "gemini-3-pro"]);
        let (model, source) =
            resolve_fallback_model(&RuntimeType::Gemini, "gpt-5.4", &chain).expect("gemini entry");
        assert_eq!(model, "gemini-3-pro");
        assert_eq!(source, "agent_fallbacks");
    }

    #[test]
    fn fallback_model_branch2_keeps_a_model_the_target_already_serves() {
        // No chain configured, and the requested model is already a Claude one
        // (e.g. a Claude agent whose PTY/CLI attempt failed over to the
        // openai-compat backend and back) — keep it verbatim.
        let (model, source) =
            resolve_fallback_model(&RuntimeType::Claude, "claude-haiku-4-5", &[]).expect("kept");
        assert_eq!(model, "claude-haiku-4-5");
        assert_eq!(source, "context_model");
    }

    #[test]
    fn fallback_model_branch3_uses_the_catalog_default() {
        // THE BUG: a codex agent's `gpt-5.4` must never reach the Claude
        // runtime. With no chain configured, the catalog's first Claude model
        // is substituted instead.
        let (model, source) =
            resolve_fallback_model(&RuntimeType::Claude, "gpt-5.4", &[]).expect("catalog default");
        assert_ne!(
            model, "gpt-5.4",
            "foreign model id leaked to the fallback runtime"
        );
        assert_eq!(source, "catalog_default");
        assert_eq!(
            model,
            RuntimeType::Claude.spec().fallback_models[0].0,
            "must be the catalog's own first model, not a hand-written constant"
        );
    }

    #[test]
    fn fallback_model_branch4_none_when_nothing_is_nameable() {
        // Kiro declares neither a model family nor a static model list; with no
        // chain and no requested model there is nothing honest to send.
        // (This is the fail-closed guard — with a non-empty `current_model`,
        // Kiro's empty `model_prefixes` makes branch 2 accept it.)
        assert!(resolve_fallback_model(&RuntimeType::Kiro, "", &[]).is_none());
        assert!(resolve_fallback_model(&RuntimeType::Kiro, "   ", &[]).is_none());
    }

    #[test]
    fn fallback_model_branch4_openai_compat_keeps_arbitrary_models() {
        // Counterpart to the above: openai_compat proxies arbitrary ids, so a
        // foreign-looking model is legitimately kept (branch 2), never refused.
        let (model, source) =
            resolve_fallback_model(&RuntimeType::OpenAiCompat, "gpt-5.4", &[]).expect("kept");
        assert_eq!(model, "gpt-5.4");
        assert_eq!(source, "context_model");
    }

    /// Stub that reports back the model it was actually invoked with, so the
    /// end-to-end substitution is observable.
    struct EchoModelRuntime;

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for EchoModelRuntime {
        fn name(&self) -> &str {
            "echo-model"
        }
        async fn execute(
            &self,
            _prompt: &str,
            context: &RuntimeContext,
        ) -> Result<RuntimeResponse, String> {
            Ok(RuntimeResponse {
                content: format!("answered with {}", context.model),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                model_used: context.model.clone(),
                runtime_name: "echo-model".to_string(),
            })
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    fn codex_context() -> RuntimeContext {
        RuntimeContext {
            model: "gpt-5.4".to_string(),
            ..stub_context()
        }
    }

    // ── P0/WP-B: cross-family failover opt-out ──────────────────────────

    /// A codex primary that always fails, with Claude registered as fallback.
    fn cross_family_registry() -> RuntimeRegistry {
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Codex, Box::new(StubRuntime { content: "" }));
        map.insert(RuntimeType::Claude, Box::new(EchoModelRuntime));
        RuntimeRegistry::with_runtimes(map)
    }

    #[tokio::test]
    async fn cross_family_failover_is_blocked_when_opted_out() {
        // The live-test hole: a hinted codex judge fails, failover substitutes
        // claude-opus-4-6, and the verdict silently comes back from the
        // worker's own family. With the opt-out set, the hop is refused.
        let ctx = RuntimeContext {
            allow_cross_family_failover: false,
            ..codex_context()
        };
        let err = FailoverManager::new(300)
            .execute_with_failover(
                &cross_family_registry(),
                &RuntimeType::Codex,
                Some(&RuntimeType::Claude),
                "hi",
                &ctx,
                &strings(&["anthropic/claude-sonnet-4-6"]),
            )
            .await
            .expect_err("cross-family failover must be refused, not silently taken");
        assert!(
            err.contains("cross-family failover"),
            "the refusal must be nameable in the audit: {err}"
        );
        // The primary's own failure is preserved as the root cause.
        assert!(err.contains("Empty response from primary"), "got: {err}");
    }

    #[tokio::test]
    async fn same_family_failover_is_still_allowed_when_opted_out() {
        // Only the CROSS-family hop is refused. A Claude primary falling back
        // to another Claude tier keeps working — the family that answers is
        // unchanged, which is all the opt-out cares about.
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Claude, Box::new(EchoModelRuntime));
        let reg = RuntimeRegistry::with_runtimes(map);
        let ctx = RuntimeContext {
            model: "claude-opus-4-6".to_string(),
            allow_cross_family_failover: false,
            ..stub_context()
        };
        let res = FailoverManager::new(300)
            .execute_with_failover(
                &reg,
                // Primary is not registered ⇒ falls through to the fallback,
                // which resolves a claude-family model for a claude runtime.
                &RuntimeType::Codex,
                Some(&RuntimeType::Claude),
                "hi",
                &ctx,
                &strings(&["anthropic/claude-sonnet-4-6"]),
            )
            .await
            .expect("same-family fallback must still answer");
        assert!(
            crate::runtime_config::same_model_family(&ctx.model, &res.model_used),
            "fallback stayed in-family: {} -> {}",
            ctx.model,
            res.model_used
        );
    }

    #[tokio::test]
    async fn default_true_keeps_cross_family_failover_working() {
        // Every pre-existing caller is byte-identical: the same scenario as
        // the blocked test, with the flag left at its default.
        let res = FailoverManager::new(300)
            .execute_with_failover(
                &cross_family_registry(),
                &RuntimeType::Codex,
                Some(&RuntimeType::Claude),
                "hi",
                &codex_context(), // allow_cross_family_failover: true
                &strings(&["anthropic/claude-sonnet-4-6"]),
            )
            .await
            .expect("default behavior still fails over across families");
        assert_eq!(res.model_used, "claude-sonnet-4-6");
    }

    #[tokio::test]
    async fn failover_to_claude_substitutes_the_model_end_to_end() {
        let mgr = FailoverManager::new(300);
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        // Codex primary returns empty ⇒ failover; Claude echoes its model back.
        map.insert(RuntimeType::Codex, Box::new(StubRuntime { content: "" }));
        map.insert(RuntimeType::Claude, Box::new(EchoModelRuntime));
        let reg = RuntimeRegistry::with_runtimes(map);

        let res = mgr
            .execute_with_failover(
                &reg,
                &RuntimeType::Codex,
                Some(&RuntimeType::Claude),
                "hi",
                &codex_context(),
                &strings(&["anthropic/claude-sonnet-4-6"]),
            )
            .await
            .expect("fallback should answer");
        assert_eq!(res.model_used, "claude-sonnet-4-6");
        assert!(!res.content.contains("gpt-5.4"), "got: {}", res.content);
    }

    #[tokio::test]
    async fn failover_without_a_nameable_model_refuses_to_spawn() {
        let mgr = FailoverManager::new(300);
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Codex, Box::new(StubRuntime { content: "" }));
        map.insert(RuntimeType::Kiro, Box::new(EchoModelRuntime));
        let reg = RuntimeRegistry::with_runtimes(map);

        let ctx = RuntimeContext {
            model: String::new(),
            ..stub_context()
        };
        let err = mgr
            .execute_with_failover(
                &reg,
                &RuntimeType::Codex,
                Some(&RuntimeType::Kiro),
                "hi",
                &ctx,
                &[],
            )
            .await
            .expect_err("no nameable model ⇒ refuse, never send a foreign id");
        assert!(
            err.contains("no model configured for fallback runtime"),
            "got: {err}"
        );

        // Recorded as a failed attempt so a permanently mis-configured fallback
        // cools down instead of being retried forever.
        let health = mgr.health_summary().await;
        let kiro = health
            .iter()
            .find(|h| h.runtime_type == RuntimeType::Kiro)
            .expect("kiro health recorded");
        assert_eq!(kiro.consecutive_failures, 1);
    }
}
