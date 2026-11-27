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
//! Whether a primary failure may use the configured fallback runtime is
//! decided by ONE rule, `fallback_rule::classify_primary_failure` (RM-181).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{info, warn};

pub mod model;
mod failure;
pub(crate) mod failure_kind;
mod fallback_rule;
pub use failure::{FallbackSkip,RuntimeDispatchFailure,RuntimePreflightFailure,RuntimePreflightKind,RuntimeStep,RuntimeStepOutcome,RuntimeStepRole};
pub use fallback_rule::{FallbackRefusal,FallbackTrigger,WorkLimitKind};
use fallback_rule::{FallbackDecision,PrimaryFailure,classify_primary_failure};

use crate::runtime::{RuntimeContext, RuntimeRegistry, RuntimeResponse, RuntimeType};

/// Claude's caller keeps its existing stop handling when switching is refused.
pub(crate) fn claude_runtime_fallback_eligible(error: &str, fallback: Option<&RuntimeType>) -> bool {
    if error == crate::on_behalf_of::access::MAIN_TURN_REFUSAL { return false; }
    fallback.is_some() && matches!(fallback_rule::apply_fallback_engine_limits(
        classify_primary_failure(PrimaryFailure::Error {
            error, autonomy_denied: crate::autonomous_runtime::was_denied(), kind: None,
        }), fallback), FallbackDecision::Eligible(_))
}

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
        if crate::entry_gate::background_refusal_code(error).is_none()
            && !crate::work_outcome::is_work_result(error) && model::is_llm_fallback_error(error)
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
    /// `model_fallbacks` is the agent's singular fallback followed by its plural chain
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
        self.execute_with_failover_detailed(registry,primary,fallback,prompt,context,model_fallbacks).await.map_err(|error|error.legacy_message())
    }

    /// Same execution/health lifecycle, retaining both attempted and skipped
    /// legs. The String API above never executes a second request.
    pub async fn execute_with_failover_detailed(
        &self, registry:&RuntimeRegistry, primary:&RuntimeType,
        fallback:Option<&RuntimeType>, prompt:&str, context:&RuntimeContext,
        model_fallbacks:&[String],
    ) -> Result<RuntimeResponse,RuntimeDispatchFailure> {
        use RuntimeStepOutcome as Outcome;
        use RuntimeStepRole::Primary;
        let mut failure=RuntimeDispatchFailure::new(*primary,&context.model);
        let policy=crate::product_policy::load(&context.home_dir).map_err(|error|RuntimeDispatchFailure::preflight(Some(*primary),&context.model,RuntimePreflightKind::Policy,error))?;
        let primary_policy=(|| {
            policy.require_runtime(primary.as_str())?;
            crate::product_policy::require_model(policy,&context.model)?;
            if let Some(provider)=context.preferred_provider.as_deref() { policy.require_provider(provider)?; }
            Ok::<_,String>(())
        })();
        if let Err(error)=primary_policy {
            return Err(RuntimeDispatchFailure::preflight(Some(*primary),&context.model,RuntimePreflightKind::Policy,error)
                .skip_fallback(fallback,FallbackRefusal::Policy));
        }
        let primary_available={
            let mut health=self.health.write().await;
            health.entry(*primary).or_insert_with(||ProviderHealth::new(*primary)).check_cooldown()
        };
        // RM-181: every primary outcome goes through the one fallback rule.
        let decision=if !primary_available {
            failure.push(Primary,*primary,Some(&context.model),Outcome::Cooldown);
            classify_primary_failure(PrimaryFailure::Cooldown)
        } else if let Some(runtime)=registry.get(primary) {
            // L2: what the runtime recorded about its failure, bound to the
            // exact error string it returned (see `failure_kind`).
            #[cfg(test)] crate::model_call_probe::record("failover_execute");
            crate::on_behalf_of::access::check_main_runtime_attempt(*primary, context).await
                .map_err(|error| RuntimeDispatchFailure::preflight(Some(*primary), &context.model,
                    RuntimePreflightKind::Config, error))?;
            let (result,recorded)=failure_kind::capture(runtime.execute(prompt,context)).await;
            let result=match result {
                Ok(response) => crate::runtime_dispatch::validate_runtime_response(response,context).await,
                Err(error) => Err(error),
            };
            match result {
                Ok(response) if !response.content.trim().is_empty()=>{
                    self.record_success(primary).await;
                    record_successful_response(primary,context,&response);
                    return Ok(response);
                }
                Ok(_)=>{
                    failure.push(Primary,*primary,Some(&context.model),Outcome::Empty);
                    self.record_failure(primary).await;
                    classify_primary_failure(PrimaryFailure::Empty)
                }
                Err(error)=>{
                    if crate::entry_gate::background_refusal_code(&error).is_some() {
                        return Err(RuntimeDispatchFailure::preflight(Some(*primary), &context.model,
                            RuntimePreflightKind::Config, error).skip_fallback(fallback, FallbackRefusal::Policy));
                    }
                    if let Some(message) = crate::budget::budget_refusal_message(&error) {
                        return Err(RuntimeDispatchFailure::preflight(Some(*primary), &context.model,
                            RuntimePreflightKind::Budget, message.to_owned()).skip_fallback(fallback,FallbackRefusal::Budget));
                    }
                    let decision=classify_primary_failure(PrimaryFailure::Error{
                        error:&error, autonomy_denied:crate::autonomous_runtime::was_denied(),
                        kind:failure_kind::kind_for(recorded.as_ref(),&error),
                    });
                    failure.push(Primary,*primary,Some(&context.model),Outcome::ExecutionError(error));
                    // Only a provider failure counts against the primary's health:
                    // a refusal (autonomy pause, work result, gate) never cools it
                    // down, nor does an engine-local configuration refusal or a
                    // prompt that does not fit, which are re-checked on every call.
                    if matches!(decision,FallbackDecision::Eligible(trigger)
                        if trigger!=FallbackTrigger::EngineLocalConfig && trigger!=FallbackTrigger::PromptTooLarge) {
                        self.record_failure(primary).await;
                    }
                    fallback_rule::apply_fallback_engine_limits(decision,fallback)
                }
            }
        } else {
            failure.push(Primary,*primary,Some(&context.model),Outcome::NotRegistered);
            classify_primary_failure(PrimaryFailure::NotRegistered)
        };
        self.run_fallback_leg(registry, primary, fallback, prompt, context, model_fallbacks,
            policy, failure, decision).await
    }

    /// Continue after the caller exhausted Claude's account rotation. Never run
    /// the primary again; preserve the same decision/health rules as layer 3.
    pub(crate) async fn after_claude_failure(
        &self, registry: &RuntimeRegistry, fallback: Option<&RuntimeType>, prompt: &str,
        context: &RuntimeContext, model_fallbacks: &[String], error: &str,
    ) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
        let primary = RuntimeType::Claude;
        if crate::entry_gate::background_refusal_code(error).is_some() {
            return Err(RuntimeDispatchFailure::preflight(Some(primary), &context.model,
                RuntimePreflightKind::Config, error.to_owned()).skip_fallback(fallback, FallbackRefusal::Policy));
        }
        let mut failure = RuntimeDispatchFailure::new(primary, &context.model);
        let decision = classify_primary_failure(PrimaryFailure::Error {
            error, autonomy_denied: crate::autonomous_runtime::was_denied(), kind: None,
        });
        failure.push(RuntimeStepRole::Primary, primary, Some(&context.model),
            RuntimeStepOutcome::ExecutionError(error.to_owned()));
        if matches!(decision, FallbackDecision::Eligible(trigger)
            if trigger != FallbackTrigger::EngineLocalConfig && trigger != FallbackTrigger::PromptTooLarge) {
            self.record_failure(&primary).await;
        }
        let policy = crate::product_policy::load(&context.home_dir).map_err(|error|
            RuntimeDispatchFailure::preflight(Some(primary), &context.model, RuntimePreflightKind::Policy, error))?;
        policy.require_runtime(primary.as_str())
            .and_then(|_| crate::product_policy::require_model(policy, &context.model))
            .map_err(|error| RuntimeDispatchFailure::preflight(Some(primary), &context.model,
                RuntimePreflightKind::Policy, error).skip_fallback(fallback, FallbackRefusal::Policy))?;
        self.run_fallback_leg(registry, &primary, fallback, prompt, context, model_fallbacks,
            policy, failure, fallback_rule::apply_fallback_engine_limits(decision, fallback)).await
    }

    /// Shared runtime-switch leg; every target executes its ordinary runtime
    /// implementation, including its account selection and spawn gates.
    #[allow(clippy::too_many_arguments)]
    async fn run_fallback_leg(
        &self, registry: &RuntimeRegistry, primary: &RuntimeType, fallback: Option<&RuntimeType>,
        prompt: &str, context: &RuntimeContext, model_fallbacks: &[String],
        policy: crate::product_policy::ProductProfile, mut failure: RuntimeDispatchFailure,
        decision: FallbackDecision,
    ) -> Result<RuntimeResponse, RuntimeDispatchFailure> {
        use RuntimeStepOutcome as Outcome;
        use RuntimeStepRole::Fallback;
        match decision {
            FallbackDecision::Refused(reason)=>return Err(failure.skip_fallback(fallback,reason)),
            FallbackDecision::Eligible(trigger)=>{
                if trigger!=FallbackTrigger::Cooldown { crate::metrics::global_metrics().record_failover(); }
                if let Some(fb)=fallback {
                    info!(agent=%context.agent_id,from_runtime=?primary,to_runtime=?fb,trigger=?trigger,"Primary runtime failed — explicit runtime fallback is eligible");
                }
            }
        }
        let Some(fb)=fallback else { return Err(failure); };
        if let Err(error)=policy.require_runtime(fb.as_str()) {
            failure.push(Fallback,*fb,None,Outcome::PolicyRefused(error));return Err(failure);
        }
        let fb_available={
            let mut health=self.health.write().await;
            health.entry(*fb).or_insert_with(||ProviderHealth::new(*fb)).check_cooldown()
        };
        if !fb_available {failure.push(Fallback,*fb,None,Outcome::Cooldown);return Err(failure);}
        let Some(runtime)=registry.get(fb) else {failure.push(Fallback,*fb,None,Outcome::NotRegistered);return Err(failure);};
        let Some((fb_model,mut model_source))=resolve_fallback_model(fb,&context.model,model_fallbacks) else {
            self.record_failure(fb).await;
            failure.push(Fallback,*fb,None,Outcome::NoCompatibleModel);return Err(failure);
        };
        // The singular setting is first in the runtime candidate list. Tag it
        // only when that candidate actually passed the family checks.
        if model_source == "agent_fallbacks" {
            if let Some(dir) = context.agent_dir.as_deref() {
                let single = crate::runtime_dispatch::chat_fallback_model(dir);
                if let Some(single) = single {
                    let candidate = vec![single];
                    if resolve_fallback_model(fb, "", &candidate)
                        .is_some_and(|(model, source)| source == "agent_fallbacks" && model == fb_model) {
                        model_source = "agent_fallback_model";
                    }
                }
            }
        }
        // F5: a `catalog_default` model is one DuDuClaw substituted, attributed
        // to the fallback runtime's vendor; any other source keeps the v1.83.0 rule.
        let fb_model_policy=if model_source=="catalog_default" {
            crate::product_policy::require_substituted_catalog_model(policy,*fb,&fb_model)
        } else {
            crate::product_policy::require_model(policy,&fb_model)
        };
        if let Err(error)=fb_model_policy {
            failure.push(Fallback,*fb,Some(&fb_model),Outcome::PolicyRefused(error));return Err(failure);
        }
        let family_known=crate::runtime_config::model_family(&context.model).is_some();
        if !context.allow_cross_family_failover && (!family_known || !crate::runtime_config::same_model_family(&context.model,&fb_model)) {
            failure.push(Fallback,*fb,Some(&fb_model),Outcome::CrossFamilyRefused);return Err(failure);
        }
        crate::budget::gate_before_spawn(&context.home_dir, context.agent_dir.as_deref(),
            &context.agent_id, "runtime_fallback").await.map_err(|e|
                RuntimeDispatchFailure::preflight(Some(*primary), &context.model,
                    RuntimePreflightKind::Budget, e.to_string()))?;
        warn!(agent=%context.agent_id,from_runtime=?primary,to_runtime=?fb,from_model=%context.model,to_model=%fb_model,model_source,"Explicit runtime fallback — substituting a compatible model");
        let mut fb_context=context.clone();fb_context.model=fb_model;
        crate::on_behalf_of::access::check_main_runtime_attempt(*fb, &fb_context).await
            .map_err(|error| RuntimeDispatchFailure::preflight(Some(*fb), &fb_context.model,
                RuntimePreflightKind::Config, error))?;
        match {
            #[cfg(test)] crate::model_call_probe::record("failover_execute");
            match runtime.execute(prompt,&fb_context).await {
                Ok(response) => crate::runtime_dispatch::validate_runtime_response(response,&fb_context).await,
                Err(error) => Err(error),
            }
        } {
            Ok(response) if !response.content.trim().is_empty()=>{
                self.record_success(fb).await;
                record_successful_response(fb,&fb_context,&response);
                Ok(response)
            }
            Ok(_)=>{
                self.record_failure(fb).await;
                failure.push(Fallback,*fb,Some(&fb_context.model),Outcome::Empty);Err(failure)
            }
            Err(error)=>{
                if crate::entry_gate::background_refusal_code(&error).is_some() {
                    return Err(RuntimeDispatchFailure::preflight(Some(*fb), &fb_context.model,
                        RuntimePreflightKind::Config, error));
                }
                if let Some(message) = crate::budget::budget_refusal_message(&error) {
                    return Err(RuntimeDispatchFailure::preflight(Some(*primary), &context.model,
                        RuntimePreflightKind::Budget, message.to_owned()));
                }
                if crate::work_outcome::WorkOutcome::decode(&error).is_none()
                    && !crate::autonomous_runtime::was_denied() && !error.starts_with("AUTONOMY_PAUSED:") {
                    self.record_failure(fb).await;
                }
                failure.push(Fallback,*fb,Some(&fb_context.model),Outcome::ExecutionError(error));Err(failure)
            }
        }
    }

    pub(crate) async fn record_success(&self, runtime_type: &RuntimeType) {
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
/// 2. **Keep `context.model`** only when `to` can run it. A target that
///    declares model prefixes (Claude, Codex, Grok, …) keeps it only when it
///    is CONFIDENTLY that target's model — an unknown-family name is not kept
///    (v1.84.0: a Codex agent's `opus` used to be handed to Grok verbatim).
///    A target with no declared family (`openai_compat`, multi-vendor
///    shells) keeps the lenient `model_matches_provider` rule, because it
///    legitimately proxies arbitrary model ids.
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
    if !current.is_empty() && fallback_keeps_current_model(current, *to) {
        return Some((current.to_string(), "context_model"));
    }

    // (3) the catalog's own default for this runtime
    if let Some((id, _label)) = to.spec().fallback_models.first() {
        return Some(((*id).to_string(), "catalog_default"));
    }

    // (4) nothing nameable
    None
}

/// Branch (2) of [`resolve_fallback_model`].
fn fallback_keeps_current_model(model: &str, to: RuntimeType) -> bool {
    if to.spec().model_prefixes.is_empty() {
        return crate::runtime_config::model_matches_provider(model, to);
    }
    duduclaw_core::runtime_catalog::runtime_for_model(model).is_some()
        && crate::runtime_config::model_matches_provider(model, to)
}

fn record_successful_response(runtime:&RuntimeType,context:&RuntimeContext,response:&RuntimeResponse) {
    crate::runtime::record_runtime_outcome(*runtime,&context.model);
    if response.input_tokens>0 || response.output_tokens>0 {
        crate::runtime::record_role_usage(crate::role_turns::RoleTurnUsage {
            usage_input_tokens:Some(response.input_tokens),usage_output_tokens:Some(response.output_tokens),usage_cache_read_tokens:None,usage_legs:None,
        });
    }
}

// RM-181 (v1.84.0): `is_non_retryable` (written for account/model rotation)
// no longer decides layer 3; see `fallback_rule::classify_primary_failure`.

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
        let mut health = ProviderHealth::new(RuntimeType::Grok);
        health.record_failure(300);
        health.record_failure(300);
        health.record_failure(300); // 3rd failure → cooldown
        assert!(!health.is_available);
        assert!(health.cooldown_until.is_some());
    }

    #[test]
    fn legacy_non_retryable_rows_keep_their_runtime_fallback_decision() {
        // The rows the removed `is_non_retryable` test pinned, through the rule
        // that replaced it (the full table lives in `fallback_rule::tests`).
        let decide = |error: &str| classify_primary_failure(PrimaryFailure::Error { error, autonomy_denied: false, kind: None });
        assert!(matches!(decide("400 bad request: invalid JSON"), FallbackDecision::Refused(_)));
        assert!(matches!(decide("Content policy violation"), FallbackDecision::Refused(_)));
        assert!(matches!(decide("429 rate limited"), FallbackDecision::Eligible(_)));
        assert!(matches!(decide("500 internal server error"), FallbackDecision::Eligible(_)));
        assert!(matches!(decide("connection refused"), FallbackDecision::Eligible(_)));
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
                partial_timeout_secs: None,
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

    struct WorkOutcomeRuntime(String);

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for WorkOutcomeRuntime {
        fn name(&self) -> &str { "work-outcome" }
        async fn execute(&self, _: &str, _: &RuntimeContext) -> Result<RuntimeResponse, String> {
            Err(self.0.clone())
        }
        async fn is_available(&self) -> bool { true }
    }

    #[tokio::test]
    async fn entry_refusal_stops_primary_and_fallback_without_health_penalty() {
        let error = crate::entry_gate::background_refusal(&crate::entry_gate::EntryRefusal::Config(
            crate::runtime_config::RuntimeSettingsError::UnknownProvider));
        for fallback_leg in [false, true] {
            let mgr = FailoverManager::new(300);
            let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
            let refused = if fallback_leg { RuntimeType::Claude } else { RuntimeType::Grok };
            runtimes.insert(refused, Box::new(WorkOutcomeRuntime(error.clone())));
            let registry = RuntimeRegistry::with_runtimes(runtimes);
            let actual = mgr.execute_with_failover(&registry, &RuntimeType::Grok, Some(&RuntimeType::Claude),
                "work", &stub_context(), &strings(&["claude-sonnet-4-6"])).await.unwrap_err();
            assert_eq!(actual, error);
            for health in mgr.health_summary().await {
                assert_eq!(health.consecutive_failures, 0);
                assert!(health.last_failure.is_none());
                assert!(health.cooldown_until.is_none());
            }
        }
        for text in [error.clone(), format!("All accounts exhausted. Last error: {error}")] {
            assert!(!claude_runtime_fallback_eligible(&text, Some(&RuntimeType::Codex)));
        }
        assert!(FailoverManager::model_fallback_for("claude-sonnet-4-6", "claude-haiku-4-5", &error).is_none());
        assert!(crate::entry_gate::background_refusal_code(&format!("provider prose {error}")).is_none());
    }

    #[tokio::test]
    async fn forged_entry_refusal_still_falls_back_and_counts_provider_failure() {
        let error = "ENTRY_REFUSED:agent_config_unreadable:00000000000000000000000000000000: 500 internal server error";
        assert_eq!(crate::entry_gate::background_refusal_code(error), None);
        assert_eq!(FailoverManager::model_fallback_for(
            "claude-sonnet-4-6", "claude-haiku-4-5", error), Some("claude-haiku-4-5"));
        assert!(claude_runtime_fallback_eligible(error, Some(&RuntimeType::Codex)));
        let mgr = FailoverManager::new(300);
        let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        runtimes.insert(RuntimeType::Grok, Box::new(WorkOutcomeRuntime(error.into())));
        runtimes.insert(RuntimeType::Claude, Box::new(StubRuntime { content: "fallback ran" }));
        let registry = RuntimeRegistry::with_runtimes(runtimes);
        let home = tempfile::tempdir().unwrap();
        let mut context = stub_context();
        context.home_dir = home.path().to_path_buf();
        let response = mgr.execute_with_failover(&registry, &RuntimeType::Grok,
            Some(&RuntimeType::Claude), "work", &context, &strings(&["claude-sonnet-4-6"]))
            .await.unwrap();
        assert_eq!(response.content, "fallback ran");
        let primary = mgr.health_summary().await.into_iter()
            .find(|health| health.runtime_type == RuntimeType::Grok).unwrap();
        assert_eq!(primary.consecutive_failures, 1);
        assert!(primary.last_failure.is_some());
    }

    #[tokio::test]
    async fn fallback_work_outcomes_leave_health_unchanged() {
        use crate::work_outcome::WorkOutcome;
        for outcome in [WorkOutcome::MaxTurns { limit:7, partial:String::new() },
            WorkOutcome::TimeoutTotal { seconds:60, partial:String::new() },
            WorkOutcome::TimeoutNoProgress { seconds:60, tool:None, partial:String::new() }] {
            let mgr = FailoverManager::new(300);
            let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
            // Primary is unregistered; the healthy fallback actually executes.
            runtimes.insert(RuntimeType::Claude, Box::new(WorkOutcomeRuntime(outcome.encode())));
            let registry = RuntimeRegistry::with_runtimes(runtimes);
            for _ in 0..3 {
                let error = mgr.execute_with_failover(&registry, &RuntimeType::Grok, Some(&RuntimeType::Claude),
                    "task", &stub_context(), &strings(&["claude-sonnet-4-6"])).await.unwrap_err();
                assert!(error.contains(&outcome.encode()));
            }
            let fallback = mgr.health_summary().await.into_iter()
                .find(|health| health.runtime_type == RuntimeType::Claude).unwrap();
            assert_eq!(fallback.consecutive_failures, 0);
            assert!(fallback.is_available);
            assert!(fallback.cooldown_until.is_none());
            assert!(fallback.last_failure.is_none());
            assert!(fallback.last_success.is_none());
        }
    }

    /// RM-181 (v1.84.0) — this test used to pin that ANY untyped timeout
    /// (including Grok's "Grok CLI hard timeout after Ns") falls back. Grok,
    /// generic CLI and OpenAI-compatible run-limit timeouts are now the same
    /// typed work result as Codex/Claude, so only a network timeout still
    /// hands over; the engine's own run limit does not.
    #[tokio::test]
    async fn network_timeout_still_uses_runtime_fallback_but_a_run_limit_does_not() {
        let mgr = FailoverManager::new(300);
        let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        runtimes.insert(RuntimeType::Grok, Box::new(WorkOutcomeRuntime("connection timed out".into())));
        runtimes.insert(RuntimeType::Claude, Box::new(StubRuntime { content:"recovered" }));
        let registry = RuntimeRegistry::with_runtimes(runtimes);
        let response = mgr.execute_with_failover(&registry, &RuntimeType::Grok, Some(&RuntimeType::Claude),
            "task", &stub_context(), &strings(&["claude-sonnet-4-6"])).await.unwrap();
        assert_eq!(response.content, "recovered");
        let primary = mgr.health_summary().await.into_iter()
            .find(|health| health.runtime_type == RuntimeType::Grok).unwrap();
        assert_eq!(primary.consecutive_failures, 1);

        let run_limit = crate::work_outcome::WorkOutcome::TimeoutTotal { seconds: 1800, partial: String::new() }.encode();
        let mgr = FailoverManager::new(300);
        let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        runtimes.insert(RuntimeType::Grok, Box::new(WorkOutcomeRuntime(run_limit.clone())));
        runtimes.insert(RuntimeType::Claude, Box::new(StubRuntime { content:"must not answer" }));
        let registry = RuntimeRegistry::with_runtimes(runtimes);
        let failure = mgr.execute_with_failover_detailed(&registry, &RuntimeType::Grok, Some(&RuntimeType::Claude),
            "task", &stub_context(), &strings(&["claude-sonnet-4-6"])).await.unwrap_err();
        assert_eq!(failure.steps.len(), 1);
        assert_eq!(failure.fallback_skipped, Some(FallbackSkip { runtime: RuntimeType::Claude,
            reason: FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutTotal) }));
        assert_eq!(failure.legacy_message(), run_limit);
        assert!(mgr.health_summary().await.iter().all(|h| h.consecutive_failures == 0));
    }

    // ── RM-203: the fallback leg receives the conversation ──────────────

    /// Records the history each call saw, then answers (or fails).
    struct HistoryRecorder {
        seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
        result: Result<&'static str, &'static str>,
    }

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for HistoryRecorder {
        fn name(&self) -> &str { "history-recorder" }
        async fn execute(&self, _: &str, context: &RuntimeContext) -> Result<RuntimeResponse, String> {
            self.seen.lock().unwrap().push(context.conversation_history.iter()
                .map(|t| (t.role.clone(), t.content.clone())).collect());
            self.result.map(|content| RuntimeResponse {
                content: content.into(), model_used: context.model.clone(),
                runtime_name: "claude".into(), ..Default::default()
            }).map_err(str::to_owned)
        }
        async fn is_available(&self) -> bool { true }
    }

    #[tokio::test]
    async fn claude_fallback_leg_sees_the_same_conversation_history() {
        let primary_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let fallback_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Grok, Box::new(HistoryRecorder { seen: primary_seen.clone(), result: Err("500 internal server error") }));
        map.insert(RuntimeType::Claude, Box::new(HistoryRecorder { seen: fallback_seen.clone(), result: Ok("代號是藍鯨") }));
        let reg = RuntimeRegistry::with_runtimes(map);
        let ctx = RuntimeContext {
            conversation_history: vec![
                crate::runtime::ConversationTurn { role: "user".into(), content: "請記住代號是藍鯨".into() },
                crate::runtime::ConversationTurn { role: "assistant".into(), content: "好的。".into() },
            ],
            ..stub_context()
        };
        let res = FailoverManager::new(300)
            .execute_with_failover(&reg, &RuntimeType::Grok, Some(&RuntimeType::Claude), "代號是什麼？", &ctx, &[])
            .await
            .expect("fallback answers");
        assert_eq!(res.content, "代號是藍鯨");
        let expected: Vec<(String, String)> = ctx.conversation_history.iter()
            .map(|t| (t.role.clone(), t.content.clone())).collect();
        assert_eq!(primary_seen.lock().unwrap().as_slice(), &[expected.clone()]);
        assert_eq!(fallback_seen.lock().unwrap().as_slice(), &[expected], "the fallback leg lost the conversation");
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
        // Only the Gemini model belongs to the Grok runtime.
        let chain = strings(&["claude-opus-4-6", "gpt-5.4", "grok-4.3"]);
        let (model, source) = resolve_fallback_model(&RuntimeType::Grok, "gpt-5.4", &chain)
            .expect("gemini entry");
        assert_eq!(model, "grok-4.3");
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

    // ── v1.84.0 distributor report: Codex→Grok fallback kept `opus` ─────

    #[test]
    fn fallback_model_claude_alias_is_never_handed_to_another_engine() {
        let grok_default = RuntimeType::Grok.spec().fallback_models[0].0;
        for alias in ["opus", "sonnet", "haiku", "fable", "Opus"] {
            let (model, source) = resolve_fallback_model(&RuntimeType::Grok, alias, &[]).expect("grok default");
            assert_eq!((model.as_str(), source), (grok_default, "catalog_default"), "{alias}");
            let (model, source) = resolve_fallback_model(&RuntimeType::Codex, alias, &[]).expect("codex default");
            assert_eq!((model.as_str(), source), (RuntimeType::Codex.spec().fallback_models[0].0, "catalog_default"));
            // Claude itself serves its CLI aliases.
            let (model, source) = resolve_fallback_model(&RuntimeType::Claude, alias, &[]).expect("kept");
            assert_eq!((model.as_str(), source), (alias, "context_model"));
        }
        // An unknown-family name is not kept by a target that declares a family…
        let (model, source) = resolve_fallback_model(&RuntimeType::Grok, "house-model-v1", &[]).unwrap();
        assert_eq!((model.as_str(), source), (grok_default, "catalog_default"));
        // …but a familyless target still proxies anything (branch 4 tests).
        let (model, source) = resolve_fallback_model(&RuntimeType::OpenAiCompat, "house-model-v1", &[]).unwrap();
        assert_eq!((model.as_str(), source), ("house-model-v1", "context_model"));
        // Unchanged: a Codex model on a Codex→Claude fallback.
        let (model, source) = resolve_fallback_model(&RuntimeType::Claude, "gpt-5.4", &[]).unwrap();
        assert_eq!((model.as_str(), source), (RuntimeType::Claude.spec().fallback_models[0].0, "catalog_default"));
    }

    #[tokio::test]
    async fn codex_to_grok_fallback_with_opus_runs_grok_with_its_own_model() {
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Codex, Box::new(StubRuntime { content: "" }));
        map.insert(RuntimeType::Grok, Box::new(EchoModelRuntime));
        let reg = RuntimeRegistry::with_runtimes(map);
        let ctx = RuntimeContext { model: "opus".into(), ..stub_context() };
        let res = FailoverManager::new(300)
            .execute_with_failover(&reg, &RuntimeType::Codex, Some(&RuntimeType::Grok), "hi", &ctx, &[])
            .await
            .expect("grok answers");
        assert_eq!(res.model_used, RuntimeType::Grok.spec().fallback_models[0].0);
        assert_ne!(res.model_used, "opus");
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
                partial_timeout_secs: None,
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

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_dispatch_honours_singular_fallback_model_and_source() {
        use crate::runtime_dispatch::{AgentPrompt, SpawnPurpose, run_agent_prompt_detailed};
        #[derive(Clone)]
        struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Log {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let log = Log(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_writer(move || writer.clone()).finish();
        let _logs = tracing::subscriber::set_default(subscriber);
        for (single, chain, expected, source) in [
            ("grok-4", "", "grok-4", "agent_fallback_model"),
            ("grok-4", "'grok-4.7'", "grok-4", "agent_fallback_model"),
            ("claude-haiku-4-5", "'grok-4.7'", "grok-4.7", "agent_fallbacks"),
            ("unknown-house-model", "", RuntimeType::Grok.spec().fallback_models[0].0, "catalog_default"),
        ] {
            log.0.lock().unwrap().clear();
            let home = tempfile::tempdir().unwrap();
            let dir = home.path().join("agents/worker");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.toml"), format!(
                "[runtime]\nprovider='codex'\nfallback='grok'\n[model]\npreferred='gpt-5.4'\nfallback='{single}'\nfallbacks=[{chain}]\n")).unwrap();
            let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
            map.insert(RuntimeType::Codex, Box::new(StubRuntime { content: "" }));
            map.insert(RuntimeType::Grok, Box::new(EchoModelRuntime));
            let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
                home.path(), RuntimeRegistry::with_runtimes(map)).await;
            let result = run_agent_prompt_detailed(AgentPrompt {
                agent_dir: Some(&dir), home_dir: home.path(), agent_id: "worker", prompt: "hello",
                system_prompt: "system", model: "gpt-5.4", max_tokens: 128, provider_override: None,
                conversation_history: &[], request_type: crate::cost_telemetry::RequestType::Chat,
                runtime_settings: None, effort: None, allow_cross_family_failover: true,
                purpose: SpawnPurpose::EmployeeTurn,
            }).await.unwrap();
            assert_eq!(result.model_used, expected);
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert!(logs.contains(&format!("model_source=\"{source}\"")), "{logs}");
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

    /// Refuses through the autonomy gate at transport time, wrapping the
    /// refusal the way non-Claude runtimes report spawn errors.
    struct PausedRuntime;

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for PausedRuntime {
        fn name(&self) -> &str {
            "paused"
        }
        async fn execute(
            &self,
            _prompt: &str,
            context: &RuntimeContext,
        ) -> Result<RuntimeResponse, String> {
            let refusal = crate::autonomous_runtime::require_utility_context(&context.home_dir)
                .await
                .expect_err("paused scope must refuse");
            Err(format!("Failed to run stub: {refusal}"))
        }
        async fn is_available(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn autonomy_refusal_never_puts_a_runtime_into_cooldown() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"alice\"\nstatus = \"active\"\n").unwrap();
        duduclaw_core::autonomy_gate::set_pause(home.path(), None, "pause", "operator").unwrap();
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Grok, Box::new(PausedRuntime));
        map.insert(RuntimeType::Claude, Box::new(PausedRuntime));
        let reg = RuntimeRegistry::with_runtimes(map);
        let mgr = FailoverManager::new(300);
        let mut ctx = stub_context();
        ctx.home_dir = home.path().to_path_buf();
        // More refusals than the three-failure cooldown threshold.
        for _ in 0..4 {
            crate::autonomous_runtime::scope(
                home.path(),
                "alice",
                &[],
                mgr.execute_with_failover(
                    &reg,
                    &RuntimeType::Grok,
                    Some(&RuntimeType::Claude),
                    "hi",
                    &ctx,
                    &[],
                ),
            )
            .await
            .expect_err("paused scope must refuse");
        }
        for health in mgr.health_summary().await {
            assert_eq!(health.consecutive_failures, 0, "{:?}", health.runtime_type);
            assert!(health.is_available, "{:?}", health.runtime_type);
        }

        // The fallback branch: an unregistered primary hands over to a
        // fallback that is refused at transport time.
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Claude, Box::new(PausedRuntime));
        let reg = RuntimeRegistry::with_runtimes(map);
        let mgr = FailoverManager::new(300);
        for _ in 0..4 {
            crate::autonomous_runtime::scope(
                home.path(),
                "alice",
                &[],
                mgr.execute_with_failover(
                    &reg,
                    &RuntimeType::Grok,
                    Some(&RuntimeType::Claude),
                    "hi",
                    &ctx,
                    &[],
                ),
            )
            .await
            .expect_err("paused scope must refuse");
        }
        let fallback = mgr
            .health_summary()
            .await
            .into_iter()
            .find(|h| h.runtime_type == RuntimeType::Claude)
            .expect("fallback health tracked");
        assert_eq!(fallback.consecutive_failures, 0);
        assert!(fallback.is_available);
    }
}

#[cfg(test)]
#[path="failover/routing_failure_tests.rs"]
mod routing_failure_tests;

#[cfg(test)]
mod work_limit_tests {
    use super::*;
    #[test]
    fn work_limits_refuse_runtime_and_model_fallbacks() {
        use crate::work_outcome::WorkOutcome;
        for outcome in [WorkOutcome::MaxTurns { limit:7, partial:String::new() },
            WorkOutcome::TimeoutTotal { seconds:60, partial:String::new() },
            WorkOutcome::TimeoutNoProgress { seconds:60, tool:None, partial:String::new() }] {
            let error = outcome.encode();
            assert!(matches!(classify_primary_failure(PrimaryFailure::Error { error: &error, autonomy_denied: false, kind: None }),
                FallbackDecision::Refused(FallbackRefusal::WorkLimit(_))));
            assert!(FailoverManager::model_fallback_for("claude-opus-4-6", "claude-sonnet-4-6", &error).is_none());
        }
    }
}
