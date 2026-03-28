//! Memory router — automatic layer classification for incoming memories.
//!
//! Based on CoALA (arXiv 2309.02427) cognitive architecture:
//! - Episodic: specific experiences (conversation summaries, reflections, feedback)
//! - Semantic: generalised knowledge (user preferences, domain rules, principles)
//!
//! Classification is rule-based (zero LLM cost).

use duduclaw_core::types::MemoryLayer;

/// Classification result: which layer and what importance.
#[derive(Debug, Clone)]
pub struct Classification {
    pub layer: MemoryLayer,
    pub importance: f64,
}

/// Classify a memory entry into the appropriate cognitive layer.
///
/// Uses the `source` event type and content heuristics.
/// Zero LLM cost — pure rule-based.
pub fn classify(content: &str, source: &str) -> Classification {
    let (layer, base_importance) = match source {
        // Prediction engine outputs — per-conversation observations
        "prediction_observation" | "conversation_summary" => (MemoryLayer::Episodic, 5.0),

        // GVU reflection outputs — pattern consolidation from evolution
        "gvu_reflection" => (MemoryLayer::Episodic, 7.0),

        // Evolution report outputs — high-level conclusions → semantic
        "evolution_report" | "gvu_outcome" => (MemoryLayer::Semantic, 8.0),

        // User feedback — episodic, importance varies by type
        "user_feedback_positive" => (MemoryLayer::Episodic, 4.0),
        "user_feedback_negative" => (MemoryLayer::Episodic, 7.0),
        "user_feedback_correction" => (MemoryLayer::Episodic, 8.0),
        "user_feedback" => (MemoryLayer::Episodic, 6.0),

        // Explicitly tagged as generalised knowledge
        "user_preference" | "generalized_rule" | "domain_knowledge" => (MemoryLayer::Semantic, 6.0),

        // Security events — high importance episodic
        "security_event" => (MemoryLayer::Episodic, 9.0),

        // Default: episodic
        _ => (MemoryLayer::Episodic, 5.0),
    };

    // Content-based importance boost
    let importance_boost = content_importance_boost(content);

    Classification {
        layer: content_layer_override(content, layer),
        importance: (base_importance + importance_boost).clamp(1.0, 10.0),
    }
}

/// Check if content contains patterns that suggest semantic (generalised) knowledge.
fn content_layer_override(content: &str, default: MemoryLayer) -> MemoryLayer {
    let lower = content.to_lowercase();

    // Patterns that suggest generalised rules / semantic knowledge
    let semantic_indicators = [
        "always ", "never ", "rule:", "principle:", "pattern:",
        "the user prefers", "the user always", "the user never",
        "general rule", "key insight", "important:",
        // Chinese indicators
        "\u{7e3d}\u{662f}", // 總是
        "\u{6c38}\u{9060}", // 永遠
        "\u{539f}\u{5247}", // 原則
        "\u{898f}\u{5247}", // 規則
        "\u{504f}\u{597d}", // 偏好
    ];

    if semantic_indicators.iter().any(|p| lower.contains(p)) {
        return MemoryLayer::Semantic;
    }

    default
}

/// Boost importance based on content signals.
fn content_importance_boost(content: &str) -> f64 {
    let lower = content.to_lowercase();
    let mut boost = 0.0;

    // Urgency indicators
    if lower.contains("critical") || lower.contains("urgent") || lower.contains("\u{7dca}\u{6025}") {
        boost += 2.0;
    }

    // Insight/learning indicators
    if lower.contains("learned") || lower.contains("discovered") || lower.contains("\u{767c}\u{73fe}") {
        boost += 1.0;
    }

    // Negative signals (problems worth remembering)
    if lower.contains("failed") || lower.contains("error") || lower.contains("\u{5931}\u{6557}") {
        boost += 1.0;
    }

    boost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prediction_observation_is_episodic() {
        let c = classify("The user asked about Rust", "prediction_observation");
        assert_eq!(c.layer, MemoryLayer::Episodic);
        assert!((c.importance - 5.0).abs() < 1.0);
    }

    #[test]
    fn evolution_report_is_semantic() {
        let c = classify("Agent performance improved", "evolution_report");
        assert_eq!(c.layer, MemoryLayer::Semantic);
        assert!(c.importance >= 8.0);
    }

    #[test]
    fn negative_feedback_high_importance() {
        let c = classify("Response was wrong", "user_feedback_negative");
        assert_eq!(c.layer, MemoryLayer::Episodic);
        assert!(c.importance >= 7.0);
    }

    #[test]
    fn content_with_always_becomes_semantic() {
        let c = classify("The user always prefers concise answers", "prediction_observation");
        assert_eq!(c.layer, MemoryLayer::Semantic);
    }

    #[test]
    fn critical_content_gets_importance_boost() {
        let c = classify("Critical security event detected", "security_event");
        assert!(c.importance >= 9.0);
    }

    #[test]
    fn chinese_rule_indicator_becomes_semantic() {
        // 原則：使用者偏好簡潔回覆
        let c = classify("\u{539f}\u{5247}\u{ff1a}\u{4f7f}\u{7528}\u{8005}\u{504f}\u{597d}\u{7c21}\u{6f54}\u{56de}\u{8986}", "gvu_reflection");
        assert_eq!(c.layer, MemoryLayer::Semantic);
    }

    #[test]
    fn importance_clamped_to_range() {
        let c = classify("Critical urgent failed error discovered", "security_event");
        assert!(c.importance <= 10.0);
        assert!(c.importance >= 1.0);
    }

    #[test]
    fn unknown_source_defaults_to_episodic() {
        let c = classify("some content", "unknown_source");
        assert_eq!(c.layer, MemoryLayer::Episodic);
        assert!((c.importance - 5.0).abs() < 1.0);
    }
}
