//! Skill relevance scoring — determines which skills to load at each layer.
//!
//! Uses keyword Jaccard overlap (same as prediction::metrics) to rank skills
//! by relevance to the current user message. Zero LLM cost.

use std::collections::HashSet;

use super::compression::CompressedSkill;

/// Configuration for relevance thresholds.
pub struct RelevanceConfig {
    /// Minimum relevance for Layer 1 injection (default 0.1).
    pub layer1_threshold: f64,
    /// Minimum relevance for Layer 2 injection (default 0.4).
    pub layer2_threshold: f64,
    /// Maximum number of skills at Layer 1 (default 5).
    pub max_layer1: usize,
    /// Maximum number of skills at Layer 2 (default 2).
    pub max_layer2: usize,
}

impl Default for RelevanceConfig {
    fn default() -> Self {
        Self {
            layer1_threshold: 0.1,
            layer2_threshold: 0.4,
            max_layer1: 5,
            max_layer2: 2,
        }
    }
}

/// Rank skills by relevance to a user message.
///
/// Returns `(index_into_skills, relevance_score)` sorted by relevance descending.
/// Zero LLM cost — pure keyword overlap.
pub fn rank_skills(message: &str, skills: &[CompressedSkill]) -> Vec<(usize, f64)> {
    if message.is_empty() || skills.is_empty() {
        return Vec::new();
    }

    let msg_keywords = extract_keywords_set(message);
    if msg_keywords.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, f64)> = skills
        .iter()
        .enumerate()
        .map(|(i, skill)| {
            let skill_keywords = extract_keywords_set(&skill.full_content);
            let score = jaccard(&msg_keywords, &skill_keywords);
            (i, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// Select which skills to inject at each layer.
pub fn select_layers(
    ranked: &[(usize, f64)],
    active_skills: &HashSet<String>,
    skills: &[CompressedSkill],
    config: &RelevanceConfig,
) -> LayerSelection {
    let mut layer1 = Vec::new();
    let mut layer2 = Vec::new();

    // Active skills always get Layer 2
    for (idx, _) in ranked {
        let skill = &skills[*idx];
        if active_skills.contains(&skill.name) && layer2.len() < config.max_layer2 + active_skills.len() {
            layer2.push(*idx);
        }
    }

    // Top relevant non-active skills
    for (idx, score) in ranked {
        let skill = &skills[*idx];
        if active_skills.contains(&skill.name) {
            continue; // already in layer2
        }

        if *score >= config.layer2_threshold && layer2.len() < config.max_layer2 {
            layer2.push(*idx);
        } else if *score >= config.layer1_threshold && layer1.len() < config.max_layer1 {
            layer1.push(*idx);
        }
    }

    LayerSelection { layer1, layer2 }
}

/// Result of layer selection.
pub struct LayerSelection {
    /// Skill indices for Layer 1 (summary only).
    pub layer1: Vec<usize>,
    /// Skill indices for Layer 2 (full content).
    pub layer2: Vec<usize>,
}

// ── Keyword extraction (shared logic) ──────────────────────

fn extract_keywords_set(text: &str) -> HashSet<String> {
    let mut keywords = HashSet::new();
    let lower = text.to_lowercase();

    // ASCII words (skip stopwords)
    let stopwords: HashSet<&str> = [
        "the", "a", "an", "is", "are", "was", "be", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "it", "this", "that", "i", "you",
        "and", "or", "but", "not", "if", "so", "do", "can", "will",
    ].into_iter().collect();

    for word in lower.split_whitespace() {
        let cleaned: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if cleaned.len() >= 2 && !stopwords.contains(cleaned.as_str()) && cleaned.is_ascii() {
            keywords.insert(cleaned);
        }
    }

    // CJK bigrams
    let chars: Vec<char> = lower.chars().collect();
    for w in chars.windows(2) {
        if is_cjk(w[0] as u32) && is_cjk(w[1] as u32) {
            keywords.insert(w.iter().collect());
        }
    }

    keywords
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn is_cjk(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
}
