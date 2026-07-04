//! Confidence-based query router.
//!
//! Routes incoming queries to the best inference tier:
//!   **LocalFast** (small model, <10ms classify) →
//!   **LocalStrong** (large local model) →
//!   **CloudAPI** (Claude API, highest quality)
//!
//! Routing is based on heuristic confidence scoring:
//! - Token count (short queries → fast tier)
//! - Keyword matching (complexity indicators → cloud tier)
//! - Configurable thresholds

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::RouterConfig;

/// Which tier should handle this query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingTier {
    /// Small, fast local model (e.g., 1-4B params).
    /// For: classification, simple Q&A, translation, formatting.
    LocalFast,
    /// Large, capable local model (e.g., 8-72B params).
    /// For: general conversation, code generation, reasoning.
    LocalStrong,
    /// Cloud API (Claude Opus/Sonnet).
    /// For: complex multi-step reasoning, architecture, security review.
    CloudApi,
}

impl std::fmt::Display for RoutingTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalFast => write!(f, "local-fast"),
            Self::LocalStrong => write!(f, "local-strong"),
            Self::CloudApi => write!(f, "cloud-api"),
        }
    }
}

/// Result of a routing decision.
#[derive(Debug, Clone, Serialize)]
pub struct RoutingDecision {
    /// Which tier to use
    pub tier: RoutingTier,
    /// Confidence score (0.0 = complex, 1.0 = trivial)
    pub confidence: f32,
    /// Human-readable reason for the decision
    pub reason: String,
    /// Which model id to use (if local tier)
    pub model_id: Option<String>,
    /// Post-hoc (cascade) confidence of the produced answer, when a local tier
    /// generated one and the backend returned logprobs. `None` for the ex-ante
    /// decision, when post-hoc is disabled, or when logprobs were unavailable
    /// (fail-safe: behave exactly as before).
    pub post_hoc: Option<PostHocAssessment>,
}

/// Post-hoc confidence of a locally generated answer.
///
/// Derived from the free logprob signal: `p̄ = exp(mean token logprob)` is
/// Platt-scaled into an acceptance probability `g = sigmoid(alpha * p̄ + beta)`.
/// `accepted` is `g >= post_hoc_accept_threshold` — a rejected answer escalates
/// to the next tier (LocalFast → LocalStrong → Cloud API).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PostHocAssessment {
    /// Geometric-mean token probability: exp(mean token logprob), in (0, 1].
    pub p_bar: f32,
    /// Platt-scaled acceptance probability g = sigmoid(alpha * p̄ + beta).
    pub g: f32,
    /// Whether the answer clears the acceptance threshold.
    pub accepted: bool,
}

impl PostHocAssessment {
    /// Calibration inputs as `(p̄, g, accepted)` — for downstream logging.
    pub fn as_tuple(&self) -> (f32, f32, bool) {
        (self.p_bar, self.g, self.accepted)
    }
}

/// Numerically standard logistic sigmoid.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Confidence-based query router.
pub struct ConfidenceRouter {
    config: RouterConfig,
    cloud_keywords_lower: Vec<String>,
    fast_keywords_lower: Vec<String>,
}

impl ConfidenceRouter {
    pub fn new(config: RouterConfig) -> Self {
        let cloud_keywords_lower = config.cloud_keywords.iter().map(|k| k.to_lowercase()).collect();
        let fast_keywords_lower = config.fast_keywords.iter().map(|k| k.to_lowercase()).collect();
        Self { config, cloud_keywords_lower, fast_keywords_lower }
    }

    /// Route a query to the best tier based on heuristic confidence.
    pub fn route(&self, system_prompt: &str, user_prompt: &str) -> RoutingDecision {
        if !self.config.enabled {
            // Router disabled — everything goes to LocalStrong (default local)
            return RoutingDecision {
                tier: RoutingTier::LocalStrong,
                confidence: 0.5,
                reason: "Router disabled, using default local".to_string(),
                model_id: self.config.strong_model.clone(),
                post_hoc: None,
            };
        }

        let combined = format!("{system_prompt} {user_prompt}").to_lowercase();
        let prompt_tokens = crate::util::estimate_tokens(user_prompt);

        let mut confidence: f32 = 0.5;
        let mut reasons = Vec::new();

        // 1. Cloud keyword check (pre-lowercased)
        for kw in &self.cloud_keywords_lower {
            if combined.contains(kw.as_str()) {
                confidence -= 0.3;
                reasons.push(format!("cloud keyword: '{kw}'"));
            }
        }

        // 2. Fast keyword check (pre-lowercased)
        // M39: use whole-word matching so "hi" does not match inside "this" and
        // "list" does not match inside "realistic" (which mis-routed to local).
        for kw in &self.fast_keywords_lower {
            if duduclaw_core::word_contains_ci(&combined, kw) {
                confidence += 0.2;
                reasons.push(format!("fast keyword: '{kw}'"));
            }
        }

        // 3. Token count heuristic
        if prompt_tokens <= 50 {
            confidence += 0.2;
            reasons.push("short prompt (<50 tokens)".to_string());
        } else if prompt_tokens <= 200 {
            // neutral
        } else if prompt_tokens <= self.config.max_fast_prompt_tokens as usize {
            confidence -= 0.1;
            reasons.push(format!("medium prompt ({prompt_tokens} tokens)"));
        } else {
            confidence -= 0.25;
            reasons.push(format!("long prompt ({prompt_tokens} tokens)"));
        }

        // 4. Question complexity heuristics
        let question_marks = user_prompt.matches('?').count();
        if question_marks > 2 {
            confidence -= 0.15;
            reasons.push(format!("{question_marks} questions"));
        }

        // 5. Code block detection
        if user_prompt.contains("```") || user_prompt.contains("fn ") || user_prompt.contains("def ") {
            confidence -= 0.1;
            reasons.push("contains code".to_string());
        }

        // 6. Multi-step indicators
        let step_indicators = ["first", "then", "next", "finally", "step", "1.", "2.", "3."];
        let step_count = step_indicators.iter().filter(|s| combined.contains(**s)).count();
        if step_count >= 2 {
            confidence -= 0.2;
            reasons.push(format!("{step_count} step indicators"));
        }

        // 7. System prompt length (complex agent = needs better model)
        let system_tokens = crate::util::estimate_tokens(system_prompt);
        if system_tokens > 500 {
            confidence -= 0.1;
            reasons.push("complex system prompt".to_string());
        }

        // Clamp confidence to [0.0, 1.0]
        confidence = confidence.clamp(0.0, 1.0);

        // Route based on thresholds
        let (tier, model_id) = if confidence >= self.config.fast_threshold {
            (RoutingTier::LocalFast, self.config.fast_model.clone())
        } else if confidence >= self.config.strong_threshold {
            (RoutingTier::LocalStrong, self.config.strong_model.clone())
        } else {
            (RoutingTier::CloudApi, None)
        };

        let reason = if reasons.is_empty() {
            "baseline heuristic".to_string()
        } else {
            reasons.join(", ")
        };

        info!(
            tier = %tier,
            confidence = format!("{confidence:.2}"),
            reason = %reason,
            "Query routed"
        );

        RoutingDecision {
            tier,
            confidence,
            reason,
            model_id,
            post_hoc: None,
        }
    }

    /// Evaluate post-hoc (cascade) confidence for a locally generated answer.
    ///
    /// Returns `None` when post-hoc routing is disabled or the backend did not
    /// return logprobs — the caller must then accept the answer as today
    /// (fail-safe). Otherwise returns the Platt-scaled assessment; callers
    /// escalate to the next tier when `accepted` is false.
    pub fn evaluate_post_hoc(&self, mean_logprob: Option<f32>) -> Option<PostHocAssessment> {
        if !self.config.post_hoc_enabled {
            return None;
        }
        let mean_logprob = mean_logprob?;
        Some(self.assess_post_hoc(mean_logprob))
    }

    /// Platt-scaled acceptance rule (pure math, always computable):
    /// `p̄ = exp(mean_logprob)`, `g = sigmoid(alpha * p̄ + beta)`,
    /// `accepted = g >= post_hoc_accept_threshold`.
    pub fn assess_post_hoc(&self, mean_logprob: f32) -> PostHocAssessment {
        let p_bar = mean_logprob.min(0.0).exp();
        let g = sigmoid(self.config.post_hoc_alpha * p_bar + self.config.post_hoc_beta);
        PostHocAssessment {
            p_bar,
            g,
            accepted: g >= self.config.post_hoc_accept_threshold,
        }
    }

    /// The tier a rejected local answer escalates to.
    ///
    /// LocalFast → LocalStrong when a strong model is configured, otherwise
    /// Cloud API. LocalStrong → Cloud API. CloudApi has no next tier.
    pub fn next_tier(&self, tier: RoutingTier) -> Option<RoutingTier> {
        match tier {
            RoutingTier::LocalFast => {
                if self.config.strong_model.is_some() {
                    Some(RoutingTier::LocalStrong)
                } else {
                    Some(RoutingTier::CloudApi)
                }
            }
            RoutingTier::LocalStrong => Some(RoutingTier::CloudApi),
            RoutingTier::CloudApi => None,
        }
    }

    /// Get the router config.
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    /// Check if the router is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> RouterConfig {
        RouterConfig {
            enabled: true,
            fast_threshold: 0.7,
            strong_threshold: 0.35,
            fast_model: Some("small-model".to_string()),
            strong_model: Some("large-model".to_string()),
            max_fast_prompt_tokens: 1000,
            cloud_keywords: vec!["refactor".to_string(), "architect".to_string()],
            fast_keywords: vec!["hello".to_string(), "translate".to_string(), "翻譯".to_string()],
            ..RouterConfig::default()
        }
    }

    #[test]
    fn simple_greeting_routes_to_fast() {
        let router = ConfidenceRouter::new(test_config());
        let decision = router.route("", "hello, how are you?");
        assert_eq!(decision.tier, RoutingTier::LocalFast);
    }

    #[test]
    fn complex_query_routes_to_cloud() {
        let router = ConfidenceRouter::new(test_config());
        let decision = router.route(
            "You are a senior architect.",
            "Please refactor the entire authentication module. First analyze the current structure, then propose a new architecture, next implement the migration plan.",
        );
        assert_eq!(decision.tier, RoutingTier::CloudApi);
    }

    #[test]
    fn medium_query_routes_to_strong() {
        let router = ConfidenceRouter::new(test_config());
        let decision = router.route(
            "You are a helpful coding assistant with deep knowledge of algorithms and data structures.",
            "Write a recursive and iterative function that calculates fibonacci numbers, then compare their time complexity and explain when each approach is better. Include error handling for negative inputs.",
        );
        // Medium complexity should land in LocalStrong or LocalFast (not Cloud)
        assert_ne!(decision.tier, RoutingTier::CloudApi);
        // Confidence should be in the middle range
        assert!(decision.confidence > 0.3, "confidence {:.2} too low", decision.confidence);
        assert!(decision.confidence < 0.9, "confidence {:.2} too high", decision.confidence);
    }

    #[test]
    fn disabled_router_defaults_to_strong() {
        let mut config = test_config();
        config.enabled = false;
        let router = ConfidenceRouter::new(config);
        let decision = router.route("", "anything");
        assert_eq!(decision.tier, RoutingTier::LocalStrong);
    }

    #[test]
    fn cjk_fast_keyword_routes_locally() {
        let router = ConfidenceRouter::new(test_config());
        let decision = router.route("", "請幫我翻譯這段文字");
        assert_ne!(decision.tier, RoutingTier::CloudApi, "CJK fast keyword should route locally");
    }

    #[test]
    fn fast_keyword_matches_whole_word_only() {
        // M39: "hi"/"list" must not match inside "this"/"realistic".
        let mut config = test_config();
        config.fast_keywords = vec!["hi".to_string(), "list".to_string()];
        let router = ConfidenceRouter::new(config);
        // A genuinely complex prompt that merely *contains* the substrings.
        let decision = router.route(
            "You are a senior architect.",
            "Please refactor this realistic distributed architecture. First analyze the current structure, then propose a new design, and implement the migration?",
        );
        // No whole-word fast keyword present, so the fast-keyword boost must not
        // fire and the complex prompt should not be routed to LocalFast.
        assert_ne!(decision.tier, RoutingTier::LocalFast);
        assert!(
            !decision.reason.contains("fast keyword"),
            "substring should not count as a fast keyword: {}",
            decision.reason
        );
    }

    #[test]
    fn fast_keyword_matches_real_word() {
        // The whole-word keyword still matches when present as a real word.
        let mut config = test_config();
        config.fast_keywords = vec!["hi".to_string()];
        let router = ConfidenceRouter::new(config);
        let decision = router.route("", "hi there");
        assert!(decision.reason.contains("fast keyword"));
    }

    // ── Post-hoc (calibrated cascade) tests ─────────────────────

    fn post_hoc_config() -> RouterConfig {
        RouterConfig {
            post_hoc_enabled: true,
            ..test_config()
        }
    }

    #[test]
    fn sigmoid_platt_math() {
        // sigmoid(0) = 0.5, symmetric, monotonic.
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!((sigmoid(10.0) - 1.0).abs() < 1e-4);
        assert!(sigmoid(-10.0) < 1e-4);
        assert!((sigmoid(2.0) + sigmoid(-2.0) - 1.0).abs() < 1e-6);

        // Platt rule with defaults alpha=4, beta=-2:
        // perfect answer (mean logprob 0 → p̄=1) → g = sigmoid(2) ≈ 0.8808.
        let router = ConfidenceRouter::new(post_hoc_config());
        let a = router.assess_post_hoc(0.0);
        assert!((a.p_bar - 1.0).abs() < 1e-6);
        assert!((a.g - 0.880_797).abs() < 1e-4);
        assert!(a.accepted);

        // Very uncertain answer (mean logprob -3 → p̄ ≈ 0.0498)
        // → g = sigmoid(4*0.0498 - 2) ≈ sigmoid(-1.8008) ≈ 0.1417.
        let a = router.assess_post_hoc(-3.0);
        assert!((a.p_bar - 0.049_787).abs() < 1e-4);
        assert!(a.g < 0.15);
        assert!(!a.accepted);

        // Positive mean logprob is clamped to p̄ = 1 (probabilities can't exceed 1).
        let a = router.assess_post_hoc(0.5);
        assert!((a.p_bar - 1.0).abs() < 1e-6);

        // as_tuple exposes calibration inputs.
        let (p_bar, g, accepted) = router.assess_post_hoc(0.0).as_tuple();
        assert!((p_bar - 1.0).abs() < 1e-6);
        assert!(g > 0.5);
        assert!(accepted);
    }

    #[test]
    fn post_hoc_disabled_returns_none() {
        // Regression guard: with post_hoc disabled (the default), evaluation
        // yields None so callers behave exactly as before.
        let router = ConfidenceRouter::new(test_config());
        assert!(!router.config().post_hoc_enabled);
        assert!(router.evaluate_post_hoc(Some(-5.0)).is_none());
        assert!(router.evaluate_post_hoc(None).is_none());
    }

    #[test]
    fn post_hoc_absent_logprobs_returns_none() {
        // Enabled but the server returned no logprobs → fail-safe None.
        let router = ConfidenceRouter::new(post_hoc_config());
        assert!(router.evaluate_post_hoc(None).is_none());
    }

    #[test]
    fn post_hoc_low_pbar_rejects_high_pbar_accepts() {
        let router = ConfidenceRouter::new(post_hoc_config());
        let low = router.evaluate_post_hoc(Some(-4.0)).expect("assessment");
        assert!(!low.accepted, "low p̄ (g={:.3}) must escalate", low.g);
        let high = router.evaluate_post_hoc(Some(-0.05)).expect("assessment");
        assert!(high.accepted, "high p̄ (g={:.3}) must be accepted", high.g);
    }

    #[test]
    fn ex_ante_route_has_no_post_hoc_and_is_unchanged() {
        // Regression guard: the ex-ante decision is identical to the legacy
        // router regardless of post_hoc settings.
        let legacy = ConfidenceRouter::new(test_config());
        let cascade = ConfidenceRouter::new(post_hoc_config());
        for (system, user) in [
            ("", "hello, how are you?"),
            ("You are a senior architect.", "Please refactor the entire authentication module. First analyze the current structure, then propose a new architecture, next implement the migration plan."),
            ("", "請幫我翻譯這段文字"),
        ] {
            let a = legacy.route(system, user);
            let b = cascade.route(system, user);
            assert_eq!(a.tier, b.tier);
            assert_eq!(a.confidence, b.confidence);
            assert_eq!(a.model_id, b.model_id);
            assert!(a.post_hoc.is_none());
            assert!(b.post_hoc.is_none());
        }
    }

    #[test]
    fn next_tier_escalation_ladder() {
        let router = ConfidenceRouter::new(post_hoc_config());
        assert_eq!(router.next_tier(RoutingTier::LocalFast), Some(RoutingTier::LocalStrong));
        assert_eq!(router.next_tier(RoutingTier::LocalStrong), Some(RoutingTier::CloudApi));
        assert_eq!(router.next_tier(RoutingTier::CloudApi), None);

        // Without a strong model, LocalFast escalates straight to Cloud API.
        let mut config = post_hoc_config();
        config.strong_model = None;
        let router = ConfidenceRouter::new(config);
        assert_eq!(router.next_tier(RoutingTier::LocalFast), Some(RoutingTier::CloudApi));
    }

    #[test]
    fn post_hoc_config_parsing_defaults() {
        // Empty [router] section → all post-hoc defaults.
        let config: RouterConfig = toml::from_str("").expect("parse empty");
        assert!(!config.post_hoc_enabled);
        assert_eq!(config.post_hoc_alpha, 4.0);
        assert_eq!(config.post_hoc_beta, -2.0);
        assert_eq!(config.post_hoc_accept_threshold, 0.5);

        // Explicit values are honoured.
        let config: RouterConfig = toml::from_str(
            r#"
            post_hoc_enabled = true
            post_hoc_alpha = 6.0
            post_hoc_beta = -3.0
            post_hoc_accept_threshold = 0.6
            "#,
        )
        .expect("parse explicit");
        assert!(config.post_hoc_enabled);
        assert_eq!(config.post_hoc_alpha, 6.0);
        assert_eq!(config.post_hoc_beta, -3.0);
        assert_eq!(config.post_hoc_accept_threshold, 0.6);
    }

    #[test]
    fn cjk_token_estimation() {
        use crate::util::estimate_tokens;
        assert!(estimate_tokens("你好世界") > 0);
        assert!(estimate_tokens("hello world") > 0);
        let cjk = estimate_tokens("測試中文字串");
        let ascii = estimate_tokens("test string here");
        assert!(cjk > 0);
        assert!(ascii > 0);
    }
}
