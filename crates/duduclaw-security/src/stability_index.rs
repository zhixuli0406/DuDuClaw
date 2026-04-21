//! Agent Stability Index (ASI) — quantitative behavioral drift measurement.
//!
//! Replaces binary SHA-256 change detection with a continuous stability metric.
//! Based on "Agent Behavioral Contracts" (arXiv:2602.22302) Section 4.3.
//!
//! ASI = weighted combination of:
//!   - Structural similarity (section count, header preservation)
//!   - Content similarity (char-bigram Jaccard on behaviors section)
//!   - Semantic stability (keyword/term overlap)
//!   - Temporal velocity (rate of change over recent versions)
//!
//! Score range: 0.0 (completely different) to 1.0 (identical).
//! Recommended thresholds:
//!   - > 0.90: Stable (normal drift from evolution)
//!   - 0.70–0.90: Warning (significant behavioral shift)
//!   - < 0.70: Critical (agent identity may be compromised)

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Configuration for ASI computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsiConfig {
    /// Weight for structural similarity (default: 0.15).
    pub w_structural: f64,
    /// Weight for content similarity (default: 0.40).
    pub w_content: f64,
    /// Weight for semantic keyword overlap (default: 0.30).
    pub w_semantic: f64,
    /// Weight for temporal velocity (default: 0.15).
    pub w_velocity: f64,
    /// ASI below this triggers a warning (default: 0.70).
    pub warning_threshold: f64,
    /// ASI below this triggers critical alert (default: 0.50).
    pub critical_threshold: f64,
}

impl Default for AsiConfig {
    fn default() -> Self {
        Self {
            w_structural: 0.15,
            w_content: 0.40,
            w_semantic: 0.30,
            w_velocity: 0.15,
            warning_threshold: 0.70,
            critical_threshold: 0.50,
        }
    }
}

impl AsiConfig {
    /// Bootstrap configuration for agents with a very small SOUL.md baseline
    /// (< ~1 KB / < 20 lines). With a tiny baseline, any meaningful evolution
    /// append makes the content bigram overlap collapse, so the strict 0.40
    /// content weight would permanently block GVU updates.
    ///
    /// This config shifts weight from `content` (which is dominated by the
    /// append-induced dilution) onto `semantic` (keyword overlap, which
    /// survives an append more gracefully) and relaxes the critical
    /// threshold so first-generation evolutions can pass.
    ///
    /// Call [`Self::for_baseline_size`] to pick the right config based on the
    /// baseline length.
    pub fn bootstrap() -> Self {
        Self {
            w_structural: 0.20,
            w_content: 0.20,
            w_semantic: 0.45,
            w_velocity: 0.15,
            warning_threshold: 0.45,
            critical_threshold: 0.25,
        }
    }

    /// Pick an appropriate config based on the SOUL.md baseline size.
    ///
    /// Uses [`Self::bootstrap`] for very small baselines, [`Self::default`]
    /// otherwise. The threshold is deliberately conservative — once a SOUL.md
    /// has enough content to resist append-induced similarity collapse,
    /// the stricter default is appropriate.
    pub fn for_baseline_size(baseline_bytes: usize) -> Self {
        if baseline_bytes < 1024 {
            Self::bootstrap()
        } else {
            Self::default()
        }
    }
}

/// Result of ASI computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsiResult {
    /// Overall stability index (0.0 – 1.0).
    pub index: f64,
    /// Severity classification.
    pub level: AsiLevel,
    /// Individual component scores.
    pub components: AsiComponents,
    /// Human-readable summary.
    pub summary: String,
}

/// Severity level derived from ASI score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsiLevel {
    /// ASI > 0.90 — normal evolution drift.
    Stable,
    /// ASI 0.70–0.90 — significant shift, review recommended.
    Warning,
    /// ASI < 0.70 — identity may be compromised.
    Critical,
}

impl std::fmt::Display for AsiLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stable => write!(f, "STABLE"),
            Self::Warning => write!(f, "WARNING"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Breakdown of ASI into individual components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsiComponents {
    /// Structural similarity (0.0 – 1.0): section headers, document structure.
    pub structural: f64,
    /// Content similarity (0.0 – 1.0): char-bigram Jaccard similarity.
    pub content: f64,
    /// Semantic stability (0.0 – 1.0): keyword/term overlap.
    pub semantic: f64,
    /// Temporal velocity (0.0 – 1.0): 1.0 if stable, lower if changing rapidly.
    pub velocity: f64,
}

/// Compute the Agent Stability Index between a baseline and current SOUL.md.
///
/// `version_distances` is an optional list of ASI scores from recent versions
/// (newest first), used to compute temporal velocity. Pass empty slice if unavailable.
pub fn compute_asi(
    baseline: &str,
    current: &str,
    version_distances: &[f64],
    config: &AsiConfig,
) -> AsiResult {
    let structural = structural_similarity(baseline, current);
    let content = content_similarity(baseline, current);
    let semantic = semantic_similarity(baseline, current);
    let velocity = temporal_velocity(version_distances);

    let index = config.w_structural * structural
        + config.w_content * content
        + config.w_semantic * semantic
        + config.w_velocity * velocity;

    let index = index.clamp(0.0, 1.0);

    let level = if index >= 0.90 {
        AsiLevel::Stable
    } else if index >= config.critical_threshold {
        AsiLevel::Warning
    } else {
        AsiLevel::Critical
    };

    let summary = format!(
        "ASI={:.3} [{}] (structural={:.2}, content={:.2}, semantic={:.2}, velocity={:.2})",
        index, level, structural, content, semantic, velocity,
    );

    AsiResult {
        index,
        level,
        components: AsiComponents {
            structural,
            content,
            semantic,
            velocity,
        },
        summary,
    }
}

// ── Component: Structural Similarity ────────────────────────────────

/// Compare document structure: section headers, their order, and count.
fn structural_similarity(baseline: &str, current: &str) -> f64 {
    let base_headers = extract_headers(baseline);
    let curr_headers = extract_headers(current);

    if base_headers.is_empty() && curr_headers.is_empty() {
        return 1.0;
    }
    if base_headers.is_empty() || curr_headers.is_empty() {
        return 0.0;
    }

    // Jaccard similarity of header sets
    let base_set: HashSet<&str> = base_headers.iter().map(|s| s.as_str()).collect();
    let curr_set: HashSet<&str> = curr_headers.iter().map(|s| s.as_str()).collect();

    let intersection = base_set.intersection(&curr_set).count() as f64;
    let union = base_set.union(&curr_set).count() as f64;

    let jaccard = if union > 0.0 { intersection / union } else { 1.0 };

    // Order preservation: longest common subsequence ratio
    let lcs_len = lcs_length(&base_headers, &curr_headers);
    let order_score = lcs_len as f64 / base_headers.len().max(curr_headers.len()) as f64;

    // Weighted: 60% content overlap + 40% order preservation
    0.6 * jaccard + 0.4 * order_score
}

fn extract_headers(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.starts_with('#'))
        .map(|line| line.trim().to_lowercase())
        .collect()
}

fn lcs_length(a: &[String], b: &[String]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    dp[m][n]
}

// ── Component: Content Similarity ───────────────────────────────────

/// Char-bigram Jaccard similarity (same algorithm as DriftBudget but returns
/// similarity instead of distance).
fn content_similarity(baseline: &str, current: &str) -> f64 {
    let a_chars: Vec<char> = baseline.chars().collect();
    let b_chars: Vec<char> = current.chars().collect();

    if a_chars.is_empty() && b_chars.is_empty() {
        return 1.0;
    }
    if a_chars.len() < 2 || b_chars.len() < 2 {
        return if a_chars == b_chars { 1.0 } else { 0.0 };
    }

    let a_bg = char_bigrams(&a_chars);
    let b_bg = char_bigrams(&b_chars);

    let all_keys: HashSet<(char, char)> = a_bg.keys().chain(b_bg.keys()).copied().collect();

    let mut intersection = 0u32;
    let mut union = 0u32;
    for key in &all_keys {
        let ca = a_bg.get(key).copied().unwrap_or(0);
        let cb = b_bg.get(key).copied().unwrap_or(0);
        intersection += ca.min(cb);
        union += ca.max(cb);
    }

    if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    }
}

fn char_bigrams(chars: &[char]) -> HashMap<(char, char), u32> {
    let mut freq = HashMap::new();
    for w in chars.windows(2) {
        *freq.entry((w[0], w[1])).or_insert(0) += 1;
    }
    freq
}

// ── Component: Semantic Similarity ──────────────────────────────────

/// Keyword/term overlap: extract meaningful words (3+ chars, not stopwords)
/// and compute Jaccard similarity.
fn semantic_similarity(baseline: &str, current: &str) -> f64 {
    let base_terms = extract_terms(baseline);
    let curr_terms = extract_terms(current);

    if base_terms.is_empty() && curr_terms.is_empty() {
        return 1.0;
    }
    if base_terms.is_empty() || curr_terms.is_empty() {
        return 0.0;
    }

    let intersection = base_terms.intersection(&curr_terms).count() as f64;
    let union = base_terms.union(&curr_terms).count() as f64;

    if union > 0.0 {
        intersection / union
    } else {
        1.0
    }
}

/// Extract meaningful terms from text: lowercase words 3+ chars, skip Markdown syntax.
fn extract_terms(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "had",
    "her", "was", "one", "our", "out", "has", "his", "how", "its", "may",
    "new", "now", "old", "see", "way", "who", "did", "get", "let", "say",
    "she", "too", "use", "that", "this", "with", "have", "from", "they",
    "been", "will", "when", "what", "your", "each", "make", "like", "than",
    "them", "then", "into", "some", "could", "other", "about", "which",
    "their", "there", "these", "would", "should",
];

// ── Component: Temporal Velocity ────────────────────────────────────

/// Measure rate of change over recent versions.
///
/// `recent_scores` are ASI scores of the last N versions (newest first).
/// Returns 1.0 if stable (no rapid changes), lower if drifting fast.
fn temporal_velocity(recent_scores: &[f64]) -> f64 {
    if recent_scores.len() < 2 {
        return 1.0; // Not enough history
    }

    // Compute average delta between consecutive versions
    let deltas: Vec<f64> = recent_scores
        .windows(2)
        .map(|w| (w[0] - w[1]).abs())
        .collect();

    let avg_delta = deltas.iter().sum::<f64>() / deltas.len() as f64;

    // Map: 0 delta → 1.0, 0.3+ delta → 0.0
    (1.0 - avg_delta / 0.3).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASELINE: &str = "\
# Agent Name

## Identity

A helpful customer service agent.

## Personality

- Warm and welcoming
- Efficient and precise
- Knowledgeable about products

## Language

- Primary: Traditional Chinese
- Secondary: English
";

    #[test]
    fn identical_content_gives_perfect_score() {
        let result = compute_asi(BASELINE, BASELINE, &[], &AsiConfig::default());
        assert!((result.index - 1.0).abs() < 0.001);
        assert_eq!(result.level, AsiLevel::Stable);
    }

    #[test]
    fn minor_change_stays_stable() {
        let modified = BASELINE.replace(
            "Warm and welcoming",
            "Warm, welcoming, and friendly",
        );
        let result = compute_asi(BASELINE, &modified, &[], &AsiConfig::default());
        assert!(result.index > 0.90, "ASI={:.3} should be > 0.90", result.index);
        assert_eq!(result.level, AsiLevel::Stable);
    }

    #[test]
    fn major_change_triggers_warning() {
        let modified = "# Different Agent\n\n## Purpose\n\nCompletely different content here.\n";
        let result = compute_asi(BASELINE, modified, &[], &AsiConfig::default());
        assert!(result.index < 0.90, "ASI={:.3} should be < 0.90", result.index);
        assert!(matches!(result.level, AsiLevel::Warning | AsiLevel::Critical));
    }

    #[test]
    fn empty_vs_content_is_critical() {
        let result = compute_asi(BASELINE, "", &[], &AsiConfig::default());
        assert!(result.index < 0.50, "ASI={:.3} should be < 0.50", result.index);
        assert_eq!(result.level, AsiLevel::Critical);
    }

    #[test]
    fn both_empty_is_stable() {
        let result = compute_asi("", "", &[], &AsiConfig::default());
        assert!((result.index - 1.0).abs() < 0.001);
        assert_eq!(result.level, AsiLevel::Stable);
    }

    #[test]
    fn temporal_velocity_stable() {
        // All recent versions are very similar
        let scores = vec![0.95, 0.96, 0.94, 0.95];
        let v = temporal_velocity(&scores);
        assert!(v > 0.9, "velocity={:.3} should be > 0.9", v);
    }

    #[test]
    fn temporal_velocity_rapid_change() {
        // Versions are oscillating wildly
        let scores = vec![0.5, 0.9, 0.4, 0.8];
        let v = temporal_velocity(&scores);
        assert!(v < 0.5, "velocity={:.3} should be < 0.5", v);
    }

    #[test]
    fn structural_preserves_order() {
        let a = "# Title\n## A\n## B\n## C\n";
        let b = "# Title\n## C\n## B\n## A\n"; // reversed
        let sim = structural_similarity(a, b);
        // Same headers but different order — should be less than perfect
        assert!(sim > 0.5 && sim < 1.0, "structural={:.3}", sim);
    }

    #[test]
    fn semantic_overlap() {
        let a = "The agent is warm, welcoming, and knowledgeable about products.";
        let b = "The agent is cold, distant, and knowledgeable about services.";
        let sim = semantic_similarity(a, b);
        // Some overlap (agent, knowledgeable) but not all
        assert!(sim > 0.1 && sim < 0.9, "semantic={:.3}", sim);
    }

    #[test]
    fn config_thresholds() {
        let config = AsiConfig {
            critical_threshold: 0.60,
            ..Default::default()
        };
        let modified = "# Different\n\nSomething else entirely.\n";
        let result = compute_asi(BASELINE, modified, &[], &config);
        // Should be Critical with default content
        assert!(result.index < 0.70);
    }

    #[test]
    fn for_baseline_size_picks_bootstrap_for_tiny_baselines() {
        let tiny = AsiConfig::for_baseline_size(512);
        let big = AsiConfig::for_baseline_size(8192);
        assert!(tiny.critical_threshold < big.critical_threshold);
        assert!(tiny.w_content < big.w_content);
    }

    #[test]
    fn bootstrap_accepts_append_on_tiny_soul_that_default_rejects() {
        // Simulate the agnes case: ~400-char SOUL.md gets a GVU append that
        // doubles its size. Under the default strict config, content
        // similarity collapses and the proposal is rejected as CRITICAL.
        // Under the bootstrap config sized for small baselines, the same
        // evolution should pass.
        let baseline = "# Agnes — 你的 AI 助理\n\n我是 Agnes，一個溫暖、可靠的 AI 助理，由 DuDuClaw 驅動。\n\n## 核心價值\n\n- 用心傾聽，真誠回應\n- 撰寫乾淨、可維護的程式碼\n- 清晰解釋我的思考過程\n- 需要時主動詢問釐清\n\n## 個性特質\n\n- 專業但不冰冷\n- 高效但不急躁\n- 精準但有溫度\n";
        let appended = format!(
            "{baseline}\n\n<!-- Evolution update (2026-04-21) -->\n## 學習到的原則\n\n- 主動釐清需求比直接動手更有價值\n- 回覆時優先條列重點\n- 技術術語附上中文說明\n"
        );

        let default = compute_asi(baseline, &appended, &[], &AsiConfig::default());
        let bootstrap = compute_asi(baseline, &appended, &[], &AsiConfig::bootstrap());

        // Sanity: both configs should see the same component scores —
        // only the thresholds + weights differ.
        assert!(bootstrap.index >= default.index || bootstrap.level != AsiLevel::Critical);

        // Via `for_baseline_size` dispatch, small baselines get bootstrap
        // and bootstrap should NOT classify this as critical.
        let dispatched = compute_asi(
            baseline,
            &appended,
            &[],
            &AsiConfig::for_baseline_size(baseline.len()),
        );
        assert_ne!(
            dispatched.level,
            AsiLevel::Critical,
            "Bootstrap config should accept plain SOUL.md appends (got {:.3})",
            dispatched.index,
        );
    }
}
