//! Model registry — vendored seed table (embedded `models.toml`) plus an
//! optional user override / refresh loader (models.dev-style; the loader is
//! wired here, network fetch is a later wave).
//!
//! Prices are integer **millicents per MTok** ($1/MTok = 100_000 mc/MTok) so
//! cost math is exact until a price-cliff multiplier applies. Unknown models
//! return `None` — callers decide (the gateway will fall back to its legacy
//! pricing table).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::moa::{MoaSpec, DEFAULT_PROPOSER_MAX_TOKENS};
use crate::provider::split_model_id;
use crate::types::NormalizedUsage;

/// Vendored seed table, embedded at compile time.
const VENDORED_MODELS_TOML: &str = include_str!("models.toml");

/// Capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct ModelCaps {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub caching: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub structured: bool,
}

/// Queryable capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Tools,
    Vision,
    Caching,
    Reasoning,
    Structured,
}

/// Long-context price cliff: input tokens beyond `threshold_tokens` are
/// billed at `input_mult` × the base input rate, and when the prompt crosses
/// the threshold ALL output tokens are billed at `output_mult` × the base
/// output rate (Gemini long-context semantics; `output_mult = 1.0` for
/// providers that only reprice input).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct PriceCliff {
    pub threshold_tokens: u64,
    pub input_mult: f64,
    #[serde(default = "default_output_mult")]
    pub output_mult: f64,
}

fn default_output_mult() -> f64 {
    1.0
}

/// One registry entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub context_window: u64,
    pub max_output_tokens: u64,
    /// Millicents per MTok of fresh input.
    pub input_mc: u64,
    /// Millicents per MTok of output (reasoning tokens bill at this rate).
    pub output_mc: u64,
    /// Millicents per MTok of cache reads. `0` = not separately priced —
    /// cost math falls back to `input_mc`.
    #[serde(default)]
    pub cache_read_mc: u64,
    /// Millicents per MTok of cache writes. `0` = fall back to `input_mc`.
    #[serde(default)]
    pub cache_write_mc: u64,
    #[serde(default)]
    pub price_cliff: Option<PriceCliff>,
    #[serde(default)]
    pub caps: ModelCaps,
}

impl ModelInfo {
    /// Fully-qualified id: `"anthropic/claude-sonnet-5"`.
    pub fn qualified_id(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// Raw `[moa.<name>]` section shape (name comes from the table key).
#[derive(Debug, Deserialize)]
struct MoaSpecToml {
    proposers: Vec<String>,
    aggregator: String,
    #[serde(default)]
    max_parallel: Option<usize>,
    #[serde(default)]
    proposer_max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ModelsFile {
    #[serde(default)]
    models: Vec<ModelInfo>,
    /// Named Mixture-of-Agents ensembles (G7): `[moa.<name>]` sections in the
    /// same override file. Absent ⇒ the MoA feature is invisible.
    /// BTreeMap for deterministic iteration order.
    #[serde(default)]
    moa: BTreeMap<String, MoaSpecToml>,
}

/// The model registry. Lookup accepts both `"provider/model"` and bare
/// `"model"` ids (bare ids resolve via a secondary index; on a bare-id
/// collision, the first-loaded entry wins).
#[derive(Debug, Default)]
pub struct ModelRegistry {
    /// qualified id → info
    models: HashMap<String, ModelInfo>,
    /// bare id → qualified id
    bare_index: HashMap<String, String>,
    /// insertion order for deterministic iteration
    order: Vec<String>,
    /// Named MoA ensembles from `[moa.<name>]` override sections (G7).
    /// Empty ⇒ `moa:<name>` ids never resolve (feature invisible).
    moa: BTreeMap<String, MoaSpec>,
}

impl ModelRegistry {
    /// Empty registry (tests / fully user-driven setups).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Registry seeded from the vendored `models.toml`.
    pub fn vendored() -> Self {
        let mut reg = Self::default();
        // The vendored table is compile-time embedded; a parse failure is a
        // build defect, surfaced loudly in tests. Fail safe at runtime by
        // returning whatever parsed (empty on total failure).
        if let Err(e) = reg.merge_toml_str(VENDORED_MODELS_TOML) {
            tracing::error!(error = %e, "vendored models.toml failed to parse");
        }
        reg
    }

    /// Merge a models.toml-format string. Later entries override earlier
    /// ones with the same qualified id (this is also the refresh loader —
    /// a models.dev-style fetched document converted to this schema merges
    /// through the same path; network wiring is a later wave).
    ///
    /// `[moa.<name>]` sections merge idempotently by name (a re-merge of the
    /// same name replaces the spec). All MoA specs are validated BEFORE
    /// anything is inserted, so an invalid spec rejects the whole document
    /// and leaves the registry unchanged (fail-closed, no partial merge).
    /// Returns the number of merged entries (models + MoA specs).
    pub fn merge_toml_str(&mut self, toml_str: &str) -> Result<usize, String> {
        let parsed: ModelsFile =
            toml::from_str(toml_str).map_err(|e| format!("models.toml parse error: {e}"))?;

        // Validate MoA specs up front — before mutating anything.
        let mut moa_specs: Vec<MoaSpec> = Vec::with_capacity(parsed.moa.len());
        for (name, raw) in parsed.moa {
            let proposer_count = raw.proposers.len();
            let spec = MoaSpec {
                name,
                proposers: raw.proposers,
                aggregator: raw.aggregator,
                max_parallel: raw.max_parallel.unwrap_or_else(|| proposer_count.max(1)),
                proposer_max_tokens: raw
                    .proposer_max_tokens
                    .unwrap_or(DEFAULT_PROPOSER_MAX_TOKENS),
            };
            spec.validate()?;
            moa_specs.push(spec);
        }

        let n = parsed.models.len() + moa_specs.len();
        for info in parsed.models {
            self.insert(info);
        }
        for spec in moa_specs {
            self.moa.insert(spec.name.clone(), spec);
        }
        Ok(n)
    }

    /// Merge a user override file (e.g. `~/.duduclaw/models.toml`).
    /// A missing file is not an error (returns 0 merged).
    pub fn load_override(&mut self, path: &Path) -> Result<usize, String> {
        if !path.exists() {
            return Ok(0);
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        self.merge_toml_str(&content)
    }

    fn insert(&mut self, info: ModelInfo) {
        let qualified = info.qualified_id();
        let bare = info.id.clone();
        if !self.models.contains_key(&qualified) {
            self.order.push(qualified.clone());
        }
        // First-loaded wins for bare-id collisions; same-provider overrides
        // keep pointing at the (replaced) qualified entry.
        self.bare_index.entry(bare).or_insert_with(|| qualified.clone());
        self.models.insert(qualified, info);
    }

    /// Look up by qualified (`"anthropic/claude-sonnet-5"`) or bare
    /// (`"claude-sonnet-5"`) id. Unknown → `None` (callers decide; the
    /// gateway falls back to legacy pricing for unknown models).
    pub fn get(&self, model_id: &str) -> Option<&ModelInfo> {
        if let Some(info) = self.models.get(model_id) {
            return Some(info);
        }
        let (provider, bare) = split_model_id(model_id);
        if provider.is_some() {
            // Qualified but unknown provider/model combination.
            return None;
        }
        self.bare_index.get(bare).and_then(|q| self.models.get(q))
    }

    /// Context window in tokens, `None` for unknown models.
    pub fn context_window(&self, model_id: &str) -> Option<u64> {
        self.get(model_id).map(|m| m.context_window)
    }

    /// Capability check. Unknown model → `false` (fail closed).
    pub fn supports(&self, model_id: &str, feature: Feature) -> bool {
        let Some(info) = self.get(model_id) else {
            return false;
        };
        match feature {
            Feature::Tools => info.caps.tools,
            Feature::Vision => info.caps.vision,
            Feature::Caching => info.caps.caching,
            Feature::Reasoning => info.caps.reasoning,
            Feature::Structured => info.caps.structured,
        }
    }

    /// All known models in insertion order.
    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.order.iter().filter_map(|q| self.models.get(q))
    }

    /// Look up a MoA ensemble by name (the `<name>` in `moa:<name>`).
    /// Unknown → `None`; the MoA executor turns that into an explicit
    /// fail-closed error (never a silent single-model fallback).
    pub fn moa_spec(&self, name: &str) -> Option<&MoaSpec> {
        self.moa.get(name)
    }

    /// All configured MoA ensembles, in name order (deterministic).
    pub fn moa_specs(&self) -> impl Iterator<Item = &MoaSpec> {
        self.moa.values()
    }

    /// Cost of a usage record in millicents.
    ///
    /// - Fresh input at `input_mc`; input beyond a price cliff threshold at
    ///   `input_mult` × base (only the excess is multiplied).
    /// - Cache reads/writes at their own rates (fall back to `input_mc`
    ///   when unpriced). Cache tokens count toward the cliff threshold
    ///   (total prompt size is what the provider bills on).
    /// - Output at `output_mc`; `reasoning_tokens` (reported separately by
    ///   e.g. Gemini) also bill at the output rate. When the prompt crossed
    ///   the cliff, all output bills at `output_mult` × base.
    pub fn cost_millicents(&self, usage: &NormalizedUsage, info: &ModelInfo) -> u64 {
        const MTOK: f64 = 1_000_000.0;

        let total_prompt = usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens;
        let (input_mult, output_mult) = match info.price_cliff {
            Some(cliff) if total_prompt > cliff.threshold_tokens => {
                (Some(cliff), cliff.output_mult)
            }
            _ => (None, 1.0),
        };

        // Fresh input, with only the excess beyond the threshold multiplied.
        // The cliff excess is attributed to fresh input tokens (cache tokens
        // keep their own rates — matching how providers bill cached prefixes).
        let input_cost = match input_mult {
            Some(cliff) => {
                let below = usage
                    .input_tokens
                    .min(cliff.threshold_tokens.saturating_sub(
                        usage.cache_read_tokens + usage.cache_write_tokens,
                    ));
                let above = usage.input_tokens - below;
                below as f64 * info.input_mc as f64 / MTOK
                    + above as f64 * info.input_mc as f64 * cliff.input_mult / MTOK
            }
            None => usage.input_tokens as f64 * info.input_mc as f64 / MTOK,
        };

        let cache_read_rate = if info.cache_read_mc > 0 { info.cache_read_mc } else { info.input_mc };
        let cache_write_rate = if info.cache_write_mc > 0 { info.cache_write_mc } else { info.input_mc };
        let cache_cost = usage.cache_read_tokens as f64 * cache_read_rate as f64 / MTOK
            + usage.cache_write_tokens as f64 * cache_write_rate as f64 / MTOK;

        let output_tokens = usage.output_tokens + usage.reasoning_tokens;
        let output_cost = output_tokens as f64 * info.output_mc as f64 * output_mult / MTOK;

        (input_cost + cache_cost + output_cost).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> NormalizedUsage {
        NormalizedUsage { input_tokens: input, output_tokens: output, ..Default::default() }
    }

    #[test]
    fn vendored_seed_parses_and_has_all_models() {
        let reg = ModelRegistry::vendored();
        assert_eq!(reg.models().count(), 15, "expected 15 vendored models");
    }

    #[test]
    fn get_accepts_qualified_and_bare_ids() {
        let reg = ModelRegistry::vendored();
        let a = reg.get("anthropic/claude-sonnet-5").expect("qualified");
        let b = reg.get("claude-sonnet-5").expect("bare");
        assert_eq!(a, b);
        assert_eq!(a.context_window, 1_000_000);
        assert!(reg.get("anthropic/no-such-model").is_none());
        assert!(reg.get("no-such-model").is_none());
    }

    #[test]
    fn context_window_and_supports() {
        let reg = ModelRegistry::vendored();
        assert_eq!(reg.context_window("xai/grok-4.1-fast"), Some(2_000_000));
        assert_eq!(reg.context_window("unknown"), None);
        assert!(reg.supports("deepseek/deepseek-v3.2", Feature::Tools));
        assert!(!reg.supports("deepseek/deepseek-v3.2", Feature::Vision));
        assert!(!reg.supports("qwen/qwen3.7-max", Feature::Reasoning));
        // Unknown model → fail closed.
        assert!(!reg.supports("unknown-model", Feature::Tools));
    }

    #[test]
    fn cost_simple_input_output() {
        let reg = ModelRegistry::vendored();
        let info = reg.get("anthropic/claude-haiku-4-5").unwrap();
        // 1 MTok in @ $1 (100_000 mc) + 1 MTok out @ $5 (500_000 mc)
        let cost = reg.cost_millicents(&usage(1_000_000, 1_000_000), info);
        assert_eq!(cost, 600_000);
    }

    #[test]
    fn cost_cache_read_write_rates() {
        let reg = ModelRegistry::vendored();
        let info = reg.get("anthropic/claude-sonnet-5").unwrap();
        let u = NormalizedUsage {
            input_tokens: 100_000,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,  // $0.30 → 30_000 mc
            cache_write_tokens: 1_000_000, // $3.75 → 375_000 mc
            reasoning_tokens: 0,
        };
        // input: 100k @ 300_000/MTok = 30_000 mc; but wait — cliff:
        // total prompt = 2.1M > 200k threshold. Cache tokens keep their own
        // rates; fresh input is fully above the residual threshold (200k -
        // 2M < 0 → below = 0) so bills at 2× = 60_000 mc.
        let cost = reg.cost_millicents(&u, info);
        assert_eq!(cost, 60_000 + 30_000 + 375_000);
    }

    #[test]
    fn cost_price_cliff_input_excess_multiplied() {
        let reg = ModelRegistry::vendored();
        let info = reg.get("anthropic/claude-sonnet-5").unwrap();
        // 300k fresh input: 200k @ $3 + 100k @ $6 = 600 + 600 = $1.20 = 120_000 mc
        let cost = reg.cost_millicents(&usage(300_000, 0), info);
        assert_eq!(cost, 120_000);
        // Below threshold: no multiplier.
        let cost = reg.cost_millicents(&usage(200_000, 0), info);
        assert_eq!(cost, 60_000);
    }

    #[test]
    fn cost_gemini_cliff_repricing_output() {
        let reg = ModelRegistry::vendored();
        let info = reg.get("gemini/gemini-3.1-pro").unwrap();
        // 250k input crosses the 200k cliff → output bills at 1.5×.
        // input: 200k @ $2 + 50k @ $4 = 400 + 200 = $0.60 = 60_000 mc
        // output: 100k @ $12 × 1.5 = $1.80 = 180_000 mc
        let cost = reg.cost_millicents(&usage(250_000, 100_000), info);
        assert_eq!(cost, 60_000 + 180_000);
        // Below the cliff, output at base rate: 100k @ $12 = 120_000 mc.
        let cost = reg.cost_millicents(&usage(100_000, 100_000), info);
        assert_eq!(cost, 20_000 + 120_000);
    }

    #[test]
    fn cost_reasoning_tokens_billed_as_output() {
        let reg = ModelRegistry::vendored();
        let info = reg.get("gemini/gemini-3.5-flash").unwrap();
        let u = NormalizedUsage {
            input_tokens: 0,
            output_tokens: 100_000,
            reasoning_tokens: 100_000,
            ..Default::default()
        };
        // 200k total output @ $9/MTok = $1.80 = 180_000 mc
        assert_eq!(reg.cost_millicents(&u, info), 180_000);
    }

    #[test]
    fn cost_unpriced_cache_falls_back_to_input_rate() {
        let reg = ModelRegistry::vendored();
        let info = reg.get("deepseek/deepseek-v3.2").unwrap();
        let u = NormalizedUsage {
            cache_read_tokens: 1_000_000,
            ..Default::default()
        };
        // No cache_read_mc → bills at input rate $0.23 = 23_000 mc.
        assert_eq!(reg.cost_millicents(&u, info), 23_000);
    }

    #[test]
    fn override_merges_and_replaces() {
        let mut reg = ModelRegistry::vendored();
        let n = reg
            .merge_toml_str(
                r#"
                [[models]]
                id = "claude-sonnet-5"
                provider = "anthropic"
                context_window = 1000000
                max_output_tokens = 128000
                input_mc = 111
                output_mc = 222
                caps = { tools = true }

                [[models]]
                id = "my-local-model"
                provider = "local"
                context_window = 32000
                max_output_tokens = 4096
                input_mc = 0
                output_mc = 0
                "#,
            )
            .expect("merge");
        assert_eq!(n, 2);
        assert_eq!(reg.get("anthropic/claude-sonnet-5").unwrap().input_mc, 111);
        assert_eq!(reg.get("local/my-local-model").unwrap().context_window, 32_000);
        // Bare lookup still resolves after override.
        assert_eq!(reg.get("claude-sonnet-5").unwrap().input_mc, 111);
        // Model count: 15 vendored + 1 new.
        assert_eq!(reg.models().count(), 16);
    }

    #[test]
    fn load_override_from_file_and_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");
        std::fs::write(
            &path,
            r#"
            [[models]]
            id = "override-model"
            provider = "custom"
            context_window = 8000
            max_output_tokens = 1000
            input_mc = 5
            output_mc = 10
            "#,
        )
        .unwrap();
        let mut reg = ModelRegistry::vendored();
        assert_eq!(reg.load_override(&path).expect("load"), 1);
        assert!(reg.get("custom/override-model").is_some());
        // Missing file → Ok(0), not an error.
        assert_eq!(reg.load_override(&dir.path().join("nope.toml")).unwrap(), 0);
    }

    #[test]
    fn malformed_override_is_an_error_not_a_panic() {
        let mut reg = ModelRegistry::empty();
        assert!(reg.merge_toml_str("this is [ not toml").is_err());
        assert_eq!(reg.models().count(), 0);
    }

    // ── MoA spec loading (G7) ──────────────────────────────────────────────

    #[test]
    fn moa_sections_parse_with_defaults_and_are_idempotent() {
        let mut reg = ModelRegistry::vendored();
        assert!(reg.moa_spec("planner").is_none(), "no spec ⇒ invisible");

        let doc = r#"
            [moa.planner]
            proposers = ["anthropic/claude-sonnet-5", "deepseek/deepseek-v3.2", "xai/grok-4.1-fast"]
            aggregator = "anthropic/claude-sonnet-5"

            [moa.coder]
            proposers = ["deepseek/deepseek-v3.2"]
            aggregator = "openai/gpt-5.4"
            max_parallel = 1
            proposer_max_tokens = 512
        "#;
        // 0 models + 2 moa specs merged.
        assert_eq!(reg.merge_toml_str(doc).expect("merge"), 2);

        let planner = reg.moa_spec("planner").expect("planner spec");
        assert_eq!(planner.proposers.len(), 3);
        assert_eq!(planner.aggregator, "anthropic/claude-sonnet-5");
        // Defaults: max_parallel = all proposers; token cap = default const.
        assert_eq!(planner.max_parallel, 3);
        assert_eq!(planner.proposer_max_tokens, DEFAULT_PROPOSER_MAX_TOKENS);

        let coder = reg.moa_spec("coder").expect("coder spec");
        assert_eq!(coder.max_parallel, 1);
        assert_eq!(coder.proposer_max_tokens, 512);

        // Deterministic name-ordered iteration.
        let names: Vec<&str> = reg.moa_specs().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["coder", "planner"]);

        // Idempotent re-merge: same name replaces, count stays 2.
        let doc2 = r#"
            [moa.planner]
            proposers = ["openai/gpt-5.4"]
            aggregator = "openai/gpt-5.4"
        "#;
        reg.merge_toml_str(doc2).expect("re-merge");
        assert_eq!(reg.moa_specs().count(), 2);
        assert_eq!(reg.moa_spec("planner").unwrap().proposers, vec!["openai/gpt-5.4"]);
    }

    #[test]
    fn moa_and_models_coexist_in_one_override_file() {
        let mut reg = ModelRegistry::vendored();
        let n = reg
            .merge_toml_str(
                r#"
                [[models]]
                id = "my-local-model"
                provider = "local"
                context_window = 32000
                max_output_tokens = 4096
                input_mc = 0
                output_mc = 0

                [moa.hybrid]
                proposers = ["local/my-local-model"]
                aggregator = "anthropic/claude-sonnet-5"
                "#,
            )
            .expect("merge");
        assert_eq!(n, 2); // 1 model + 1 moa spec
        assert!(reg.get("local/my-local-model").is_some());
        assert!(reg.moa_spec("hybrid").is_some());
    }

    #[test]
    fn invalid_moa_spec_rejects_whole_document_no_partial_merge() {
        let mut reg = ModelRegistry::vendored();
        let before = reg.models().count();
        // Empty proposers is invalid — the model in the same doc must NOT land.
        let err = reg
            .merge_toml_str(
                r#"
                [[models]]
                id = "should-not-land"
                provider = "local"
                context_window = 1000
                max_output_tokens = 100
                input_mc = 0
                output_mc = 0

                [moa.broken]
                proposers = []
                aggregator = "anthropic/claude-sonnet-5"
                "#,
            )
            .expect_err("invalid spec");
        assert!(err.contains("broken"), "got: {err}");
        assert_eq!(reg.models().count(), before);
        assert!(reg.get("local/should-not-land").is_none());
        assert!(reg.moa_spec("broken").is_none());

        // Nested moa: member is rejected too.
        assert!(reg
            .merge_toml_str(
                r#"
                [moa.nested]
                proposers = ["moa:other"]
                aggregator = "anthropic/claude-sonnet-5"
                "#
            )
            .is_err());
        // max_parallel = 0 is rejected.
        assert!(reg
            .merge_toml_str(
                r#"
                [moa.zero]
                proposers = ["anthropic/claude-sonnet-5"]
                aggregator = "anthropic/claude-sonnet-5"
                max_parallel = 0
                "#
            )
            .is_err());
    }
}
