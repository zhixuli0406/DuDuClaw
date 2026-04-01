//! Agent behavior contract system.
//!
//! [D-1a] Defines behavioral boundaries via `CONTRACT.toml`.
//! [D-1b] Validates agent output against contract rules.
//!
//! Example `CONTRACT.toml`:
//! ```toml
//! [boundaries]
//! must_not = ["reveal api keys", "execute rm -rf", "modify SOUL.md"]
//! must_always = ["respond in zh-TW", "refuse harmful requests"]
//! max_tool_calls_per_turn = 10
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Agent behavior contract loaded from `CONTRACT.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    #[serde(default)]
    pub boundaries: Boundaries,
}

/// Behavioral boundaries section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Boundaries {
    /// Patterns the agent must NEVER include in its output.
    #[serde(default)]
    pub must_not: Vec<String>,
    /// Behaviors the agent must ALWAYS exhibit (informational; used in testing).
    #[serde(default)]
    pub must_always: Vec<String>,
    /// Maximum tool calls per single turn (0 = unlimited).
    #[serde(default)]
    pub max_tool_calls_per_turn: u32,
}

/// Result of validating an agent response against its contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractViolation {
    pub rule: String,
    pub category: String,
    pub matched_text: String,
}

/// Full validation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub passed: bool,
    pub violations: Vec<ContractViolation>,
}

/// Load a contract from the agent directory's `CONTRACT.toml`.
/// Returns a default (empty) contract if the file doesn't exist.
pub fn load_contract(agent_dir: &Path) -> Contract {
    let path = agent_dir.join("CONTRACT.toml");
    match std::fs::read_to_string(&path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(c) => {
                info!(path = %path.display(), "Contract loaded");
                c
            }
            Err(e) => {
                warn!(path = %path.display(), "Failed to parse CONTRACT.toml: {e}");
                Contract::default()
            }
        },
        Err(_) => Contract::default(),
    }
}

impl Default for Contract {
    fn default() -> Self {
        Self {
            boundaries: Boundaries::default(),
        }
    }
}

/// Validate an agent response against its behavioral contract.
///
/// Checks each `must_not` rule against the response text using
/// case-insensitive substring + regex matching.
pub fn validate_response(contract: &Contract, response: &str) -> ValidationResult {
    let lower = response.to_lowercase();
    let mut violations = Vec::new();

    for rule in &contract.boundaries.must_not {
        let rule_lower = rule.to_lowercase();

        // First: simple substring match
        if lower.contains(&rule_lower) {
            violations.push(ContractViolation {
                rule: rule.clone(),
                category: "must_not".to_string(),
                matched_text: extract_context(&lower, &rule_lower),
            });
            continue;
        }

        // Second: try as glob pattern (if it looks like one)
        if rule.contains('*') || rule.contains('?') || rule.contains('[') {
            let pattern = glob_to_regex_pattern(&rule_lower);
            if let Ok(re) = regex_lite_match(&pattern, &lower) {
                if re {
                    violations.push(ContractViolation {
                        rule: rule.clone(),
                        category: "must_not".to_string(),
                        matched_text: "(regex match)".to_string(),
                    });
                }
            }
        }
    }

    let passed = violations.is_empty();
    ValidationResult { passed, violations }
}

/// Convert a glob pattern to a regex-compatible pattern used by `regex_lite_match`.
///
/// Escapes regex special characters (`.`, `+`, `(`, `)`, etc.) except `*` and `?`,
/// which are converted to their regex equivalents (`.*` and `.`).
fn glob_to_regex_pattern(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len() * 2);
    for c in glob.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '.' | '+' | '(' | ')' | '{' | '}' | '[' | ']' | '|' | '^' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Glob-style pattern matching for behavioral contracts (BE-L3).
///
/// Supports:
/// - `.*` — match any sequence of characters (like regex `.*`)
/// - `^` prefix — anchor to start of text
/// - `$` suffix — anchor to end of text
/// - Literal substring matching (case-insensitive)
fn regex_lite_match(pattern: &str, text: &str) -> Result<bool, ()> {
    let text_lower = text.to_lowercase();
    let pattern_lower = pattern.to_lowercase();

    let anchor_start = pattern_lower.starts_with('^');
    let anchor_end = pattern_lower.ends_with('$');

    let trimmed = pattern_lower
        .trim_start_matches('^')
        .trim_end_matches('$');

    let parts: Vec<&str> = trimmed.split(".*").collect();
    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match text_lower[pos..].find(part) {
            Some(idx) => {
                // If anchored to start, first part must match at position 0
                if i == 0 && anchor_start && idx != 0 {
                    return Ok(false);
                }
                pos += idx + part.len();
            }
            None => return Ok(false),
        }
    }

    // If anchored to end, last match must reach end of text
    if anchor_end && pos != text_lower.len() {
        return Ok(false);
    }

    Ok(true)
}

/// Extract a ~60-char context window around a match (UTF-8 safe).
fn extract_context(text: &str, needle: &str) -> String {
    if let Some(idx) = text.find(needle) {
        // Find char-boundary-safe start and end positions
        let start = text[..idx]
            .char_indices()
            .rev()
            .nth(20)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let end_offset = idx + needle.len();
        let end = text[end_offset..]
            .char_indices()
            .nth(20)
            .map(|(i, _)| end_offset + i)
            .unwrap_or(text.len());
        let snippet = &text[start..end];
        if start > 0 || end < text.len() {
            format!("...{snippet}...")
        } else {
            snippet.to_string()
        }
    } else {
        String::new()
    }
}

/// Generate a system prompt addendum from the contract's boundaries.
///
/// This is injected into the agent's system prompt to reinforce the rules.
pub fn contract_to_prompt(contract: &Contract) -> String {
    let b = &contract.boundaries;
    if b.must_not.is_empty() && b.must_always.is_empty() {
        return String::new();
    }

    let mut lines = vec!["## Behavioral Contract".to_string()];

    if !b.must_not.is_empty() {
        lines.push("You must NEVER:".to_string());
        for rule in &b.must_not {
            lines.push(format!("- {rule}"));
        }
    }

    if !b.must_always.is_empty() {
        lines.push("You must ALWAYS:".to_string());
        for rule in &b.must_always {
            lines.push(format!("- {rule}"));
        }
    }

    if b.max_tool_calls_per_turn > 0 {
        lines.push(format!(
            "Maximum tool calls per turn: {}",
            b.max_tool_calls_per_turn
        ));
    }

    lines.join("\n")
}
