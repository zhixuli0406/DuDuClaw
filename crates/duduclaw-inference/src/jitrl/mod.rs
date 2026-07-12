//! JitRL — zero-gradient continual learning on the local inference path.
//!
//! Implements a DuDuClaw-shaped v1 of **Just-In-Time Reinforcement Learning**
//! (arXiv:2601.18510, code: <https://github.com/liushiliushi/JitRL>): a
//! training-free framework that keeps a non-parametric memory of past
//! experiences, retrieves the ones similar to the current prompt (the paper
//! uses Jaccard n-gram matching), and **additively modulates the model's
//! output logits** at decode time — the paper proves this additive update is
//! the closed-form solution to KL-constrained policy optimization. This needs
//! logits access, which only a local inference engine has; cloud-API-only
//! stacks structurally cannot do it.
//!
//! ## v1 scope — explicit feedback only
//!
//! The paper scores trajectory steps with an LLM judge. v1 deliberately does
//! **not** infer rewards automatically (that would be fabricated signal):
//! experiences enter the store only through an explicit
//! [`JitrlEngine::record_feedback`] call carrying a caller-supplied reward in
//! `[-1, 1]`. The response is tokenized with the **active model's own
//! tokenizer** (see [`tokenizer`]) — token ids are vocabulary-specific, so
//! records are keyed by `model_id` and never applied across models.
//!
//! ## Per-backend tier support (verified against this crate, 2026-07-11)
//!
//! | Backend            | Tier | Mechanism                                                        |
//! |--------------------|------|------------------------------------------------------------------|
//! | OpenAI-compat HTTP | B    | request-level `logit_bias` map (llama.cpp server / vLLM / SGLang) |
//! | llama.cpp in-proc  | A (seam) | `generate()` is currently a stub — [`bias::StepBiasAdjuster`] is the tested per-step surface for when the sampling loop lands |
//! | mistral.rs         | none in v1 | feature-gated dep not compiled here; upstream `SamplingParams` bias surface unverified — feature is transparently absent |
//!
//! A backend without a bias surface simply ignores
//! `GenerationParams::logit_bias`; behavior is byte-identical to today.
//!
//! ## Off by default
//!
//! `[jitrl] enabled = false` in `inference.toml`. When disabled the engine
//! constructs no [`JitrlEngine`] at all — the hot path carries zero JitRL
//! code and requests are untouched.

pub mod bias;
pub mod fingerprint;
pub mod store;
pub mod tokenizer;

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{InferenceError, Result};

pub use bias::StepBiasAdjuster;
pub use store::{ExperienceRecord, ExperienceStore};
pub use tokenizer::{HttpTokenizer, JitrlTokenizer};

/// `[jitrl]` section of `inference.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JitrlConfig {
    /// Master switch — DEFAULT FALSE (experimental).
    pub enabled: bool,
    /// Hard cap on the absolute per-token logit adjustment.
    pub max_bias: f32,
    /// How many of the most similar experiences to aggregate.
    pub top_k: usize,
    /// Minimum Jaccard similarity for an experience to count.
    pub min_similarity: f32,
    /// Ebbinghaus-style exponential decay half-life for outcome weights.
    pub half_life_days: f32,
    /// Maximum number of tokens carried in one bias map.
    pub max_bias_tokens: usize,
    /// Maximum experience records kept in the store (oldest dropped).
    pub max_records: usize,
    /// Store path override; default `<home>/jitrl_experience.jsonl`
    /// (a sibling of `<home>/models/`).
    pub store_path: Option<String>,
}

impl Default for JitrlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bias: 2.0,
            top_k: 8,
            min_similarity: 0.3,
            half_life_days: 14.0,
            max_bias_tokens: 128,
            max_records: 2000,
            store_path: None,
        }
    }
}

impl JitrlConfig {
    /// Validate ranges. Only called when `enabled` — a disabled section can
    /// never break startup.
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=10.0).contains(&self.max_bias) || self.max_bias == 0.0 {
            return Err(InferenceError::Config(
                "jitrl.max_bias must be in (0.0, 10.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&self.min_similarity) {
            return Err(InferenceError::Config(
                "jitrl.min_similarity must be within [0.0, 1.0]".to_string(),
            ));
        }
        if self.top_k == 0 {
            return Err(InferenceError::Config(
                "jitrl.top_k must be >= 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// Orchestrator: experience ingestion + request-time bias preparation.
pub struct JitrlEngine {
    cfg: JitrlConfig,
    store: ExperienceStore,
}

impl JitrlEngine {
    /// Build from config. Returns `None` when disabled — callers hold an
    /// `Option<JitrlEngine>` so the disabled hot path has zero JitRL code.
    pub fn new(cfg: JitrlConfig, home_dir: &Path) -> Option<Self> {
        if !cfg.enabled {
            return None;
        }
        let path = match cfg.store_path {
            Some(ref p) => crate::util::expand_tilde(p),
            None => home_dir.join("jitrl_experience.jsonl"),
        };
        let store = ExperienceStore::new(path, cfg.max_records);
        Some(Self { cfg, store })
    }

    pub fn config(&self) -> &JitrlConfig {
        &self.cfg
    }

    /// Compute the logit-bias map for a prompt, or `None` when no stored
    /// experience is similar enough. Fail-soft: store read errors are logged
    /// and yield `None` — retrieval must never block generation.
    pub fn prepare_bias(&self, prompt: &str, model_id: &str) -> Option<HashMap<u32, f32>> {
        if model_id.is_empty() {
            // Without a model identity, token ids cannot be trusted.
            return None;
        }
        let records = match self.store.load_for_model(model_id) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "jitrl: failed to load experience store, skipping bias");
                return None;
            }
        };
        if records.is_empty() {
            return None;
        }
        let sketch = fingerprint::shingle_sketch(prompt);
        let now = chrono::Utc::now().timestamp();
        let bias = bias::aggregate_bias(&sketch, &records, now, &self.cfg);
        if let Some(ref b) = bias {
            debug!(
                model = model_id,
                tokens = b.len(),
                "jitrl: injecting logit bias"
            );
        }
        bias
    }

    /// Record explicit feedback for a `(prompt, response)` pair.
    ///
    /// `reward` must be finite and non-zero (positive = reinforce the
    /// response's tokens on similar prompts, negative = suppress); it is
    /// clamped to `[-1, 1]`. The response is tokenized with `tokenizer` —
    /// which must belong to `model_id`'s vocabulary — and the deduplicated
    /// token ids are stored with the reward as their weight. Returns the
    /// number of distinct tokens recorded.
    pub async fn record_feedback(
        &self,
        tokenizer: &dyn JitrlTokenizer,
        prompt: &str,
        response: &str,
        reward: f32,
        model_id: &str,
    ) -> Result<usize> {
        if !reward.is_finite() || reward == 0.0 {
            return Err(InferenceError::Config(
                "jitrl: reward must be finite and non-zero".to_string(),
            ));
        }
        if model_id.is_empty() {
            return Err(InferenceError::Config(
                "jitrl: model_id required — token ids are vocabulary-specific".to_string(),
            ));
        }
        if prompt.trim().is_empty() || response.trim().is_empty() {
            return Err(InferenceError::Config(
                "jitrl: prompt and response must be non-empty".to_string(),
            ));
        }

        let reward = reward.clamp(-1.0, 1.0);
        let tokens = tokenizer.encode(response).await?;
        if tokens.is_empty() {
            return Err(InferenceError::Other(
                "jitrl: tokenizer returned no tokens for response".to_string(),
            ));
        }

        let mut token_weights: HashMap<u32, f32> = HashMap::new();
        for t in tokens {
            token_weights.entry(t).or_insert(reward);
        }
        let count = token_weights.len();

        let record = ExperienceRecord {
            id: uuid::Uuid::new_v4().to_string(),
            model_id: model_id.to_string(),
            sketch: fingerprint::shingle_sketch(prompt),
            token_weights,
            reward,
            created_at: chrono::Utc::now().timestamp(),
        };
        self.store.append(&record)?;
        debug!(
            model = model_id,
            tokens = count,
            reward,
            "jitrl: recorded feedback"
        );
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Deterministic mock tokenizer: one token per whitespace-separated word,
    /// id = word length (stable, vocabulary-free — test only).
    struct MockTokenizer;

    #[async_trait]
    impl JitrlTokenizer for MockTokenizer {
        async fn encode(&self, text: &str) -> Result<Vec<u32>> {
            Ok(text
                .split_whitespace()
                .map(|w| w.chars().count() as u32)
                .collect())
        }
    }

    fn enabled_cfg() -> JitrlConfig {
        JitrlConfig {
            enabled: true,
            ..JitrlConfig::default()
        }
    }

    #[test]
    fn disabled_config_builds_no_engine() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(JitrlEngine::new(JitrlConfig::default(), tmp.path()).is_none());
        assert!(JitrlEngine::new(enabled_cfg(), tmp.path()).is_some());
    }

    #[tokio::test]
    async fn feedback_then_bias_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let engine = JitrlEngine::new(enabled_cfg(), tmp.path()).unwrap();

        let n = engine
            .record_feedback(
                &MockTokenizer,
                "please summarize this quarterly report",
                "revenue grew twelve percent",
                1.0,
                "model-a",
            )
            .await
            .unwrap();
        assert!(n > 0);

        // Similar prompt → bias present, positive, clamped.
        let bias = engine
            .prepare_bias("please summarize this quarterly report for me", "model-a")
            .expect("similar prompt should retrieve the experience");
        assert!(!bias.is_empty());
        assert!(bias.values().all(|v| *v > 0.0 && *v <= 2.0));

        // Dissimilar prompt → no bias.
        assert!(
            engine
                .prepare_bias("write a haiku about mountains in winter", "model-a")
                .is_none()
        );

        // Different model → no bias (vocabulary isolation).
        assert!(
            engine
                .prepare_bias("please summarize this quarterly report for me", "model-b")
                .is_none()
        );

        // Empty model id → no bias.
        assert!(
            engine
                .prepare_bias("please summarize this quarterly report", "")
                .is_none()
        );
    }

    #[tokio::test]
    async fn negative_feedback_produces_negative_bias() {
        let tmp = tempfile::TempDir::new().unwrap();
        let engine = JitrlEngine::new(enabled_cfg(), tmp.path()).unwrap();
        engine
            .record_feedback(
                &MockTokenizer,
                "translate this sentence into french",
                "bonjour le monde",
                -1.0,
                "m",
            )
            .await
            .unwrap();
        let bias = engine
            .prepare_bias("translate this sentence into french please", "m")
            .unwrap();
        assert!(bias.values().all(|v| *v < 0.0 && *v >= -2.0));
    }

    #[tokio::test]
    async fn invalid_feedback_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let engine = JitrlEngine::new(enabled_cfg(), tmp.path()).unwrap();
        for (prompt, response, reward, model) in [
            ("p", "r", 0.0, "m"),      // zero reward
            ("p", "r", f32::NAN, "m"), // NaN reward
            ("p", "r", 1.0, ""),       // missing model
            ("", "r", 1.0, "m"),       // empty prompt
            ("p", "  ", 1.0, "m"),     // empty response
        ] {
            assert!(
                engine
                    .record_feedback(&MockTokenizer, prompt, response, reward, model)
                    .await
                    .is_err(),
                "expected rejection for ({prompt:?}, {response:?}, {reward}, {model:?})"
            );
        }
    }

    #[test]
    fn config_validation_ranges() {
        let mut c = enabled_cfg();
        assert!(c.validate().is_ok());
        c.max_bias = 0.0;
        assert!(c.validate().is_err());
        c.max_bias = 11.0;
        assert!(c.validate().is_err());
        c.max_bias = 2.0;
        c.min_similarity = 1.5;
        assert!(c.validate().is_err());
        c.min_similarity = 0.3;
        c.top_k = 0;
        assert!(c.validate().is_err());
    }
}
