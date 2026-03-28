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
        for kw in &self.fast_keywords_lower {
            if combined.contains(kw.as_str()) {
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
