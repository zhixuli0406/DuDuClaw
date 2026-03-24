//! Prompt injection detection — rule-based input scanning.
//!
//! [C-2a] Scans incoming user messages for common prompt injection patterns.
//! Returns a risk score (0–100) and matched rule names.

use serde::{Deserialize, Serialize};

/// Result of an input security scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputScanResult {
    /// Risk score from 0 (safe) to 100 (definitely malicious).
    pub risk_score: u32,
    /// Whether the input should be blocked (score >= threshold).
    pub blocked: bool,
    /// Names of matched rules.
    pub matched_rules: Vec<String>,
    /// Human-readable summary.
    pub summary: String,
}

struct Rule {
    name: &'static str,
    weight: u32,
    /// If true, matching this rule alone is enough to block.
    instant_block: bool,
    patterns: &'static [&'static str],
}

const RULES: &[Rule] = &[
    Rule {
        name: "instruction_override",
        weight: 40,
        instant_block: true,
        patterns: &[
            "ignore previous instructions",
            "ignore all previous",
            "disregard your instructions",
            "forget your instructions",
            "override your system prompt",
            "ignore your system prompt",
            "new instructions:",
            "your new role is",
        ],
    },
    Rule {
        name: "system_prompt_extraction",
        weight: 30,
        instant_block: false,
        patterns: &[
            "repeat your system prompt",
            "show me your instructions",
            "what are your instructions",
            "output your system prompt",
            "print your prompt",
            "reveal your prompt",
            "display your rules",
            "show your soul",
        ],
    },
    Rule {
        name: "role_hijack",
        weight: 35,
        instant_block: true,
        patterns: &[
            "you are now",
            "pretend you are",
            "act as if you",
            "from now on you are",
            "roleplay as",
            "you must obey",
            "you are DAN",
            "jailbreak",
        ],
    },
    Rule {
        name: "encoding_bypass",
        weight: 25,
        instant_block: false,
        patterns: &[
            "base64:",
            "decode the following",
            "rot13",
            "hex encode",
            "in pig latin",
        ],
    },
    Rule {
        name: "tool_abuse",
        weight: 30,
        instant_block: true,
        patterns: &[
            "rm -rf",
            "sudo rm",
            "delete all files",
            "format disk",
            "drop table",
            "exec(",
            "eval(",
            "system(",
        ],
    },
    Rule {
        name: "data_exfiltration",
        weight: 25,
        instant_block: true,
        patterns: &[
            "send to my email",
            "upload to",
            "post to webhook",
            "curl http",
            "wget http",
            "exfiltrate",
        ],
    },
];

/// Default risk threshold above which messages are blocked.
pub const DEFAULT_BLOCK_THRESHOLD: u32 = 60;

/// Scan an input message for prompt injection patterns.
pub fn scan_input(text: &str, block_threshold: u32) -> InputScanResult {
    let lower = text.to_lowercase();
    let mut total_score: u32 = 0;
    let mut matched = Vec::new();
    let mut force_block = false;

    for rule in RULES {
        for pattern in rule.patterns {
            if lower.contains(pattern) {
                if !matched.contains(&rule.name.to_string()) {
                    matched.push(rule.name.to_string());
                    total_score = total_score.saturating_add(rule.weight);
                    if rule.instant_block {
                        force_block = true;
                    }
                }
                break; // One match per rule is enough
            }
        }
    }

    // Check for zero-width characters (Unicode injection)
    let zwc_count = text
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            cp == 0x200B // zero-width space
                || cp == 0x200C // zero-width non-joiner
                || cp == 0x200D // zero-width joiner
                || cp == 0xFEFF // BOM
                || cp == 0x2060 // word joiner
        })
        .count();
    if zwc_count > 3 {
        matched.push("unicode_injection".to_string());
        total_score = total_score.saturating_add(20);
    }

    let score = total_score.min(100);
    let blocked = force_block || score >= block_threshold;

    let summary = if matched.is_empty() {
        "No suspicious patterns detected".to_string()
    } else if blocked {
        format!(
            "BLOCKED: Suspicious input (score: {score}/100, rules: {})",
            matched.join(", ")
        )
    } else {
        format!(
            "Warning: Suspicious patterns detected (score: {score}/100, rules: {})",
            matched.join(", ")
        )
    };

    InputScanResult {
        risk_score: score,
        blocked,
        matched_rules: matched,
        summary,
    }
}
