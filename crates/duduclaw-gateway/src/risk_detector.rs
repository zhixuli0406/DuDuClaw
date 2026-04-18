//! Rule-based risk detection for Computer Use actions.
//!
//! Determines whether an action requires channel confirmation before execution.
//! Pure rules — no LLM calls — so decisions are instantaneous and deterministic.

use crate::computer_use::ComputerAction;
use crate::computer_use_orchestrator::ComputerUseConfig;
use serde::{Deserialize, Serialize};

/// Risk level for a computer use action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskLevel {
    /// Safe to execute without notification.
    Low,
    /// Execute but send a screenshot notification to the channel.
    Medium,
    /// Pause and send screenshot + confirmation request to the channel.
    High,
    /// Reject entirely — violates CONTRACT.toml or hard safety rules.
    Blocked,
}

/// Context about what the model wants to do (extracted from the action + vision).
#[derive(Debug, Clone)]
pub struct ActionContext {
    /// The action the model wants to execute.
    pub action: ComputerAction,
    /// Text reasoning from the model (if available).
    pub model_reasoning: Option<String>,
    /// Whether the target element looks like a sensitive input.
    pub targets_sensitive_input: bool,
    /// The application/window title currently focused.
    pub active_window_title: Option<String>,
}

/// Assess the risk level of an action given the configuration.
pub fn assess_risk(ctx: &ActionContext, config: &ComputerUseConfig) -> RiskLevel {
    // ── Hard blocks ──
    if is_blocked_action(ctx, config) {
        return RiskLevel::Blocked;
    }

    // ── High risk: sensitive input ──
    if ctx.targets_sensitive_input {
        return RiskLevel::High;
    }

    // ── High risk: typing into unknown context ──
    if let ComputerAction::Type { ref text } = ctx.action {
        if looks_like_sensitive_input(text) {
            return RiskLevel::High;
        }
    }

    // ── Medium risk: interaction with non-whitelisted app ──
    if !config.allowed_apps.is_empty() {
        if let Some(ref title) = ctx.active_window_title {
            let in_whitelist = config
                .allowed_apps
                .iter()
                .any(|app| title.to_lowercase().contains(&app.to_lowercase()));
            if !in_whitelist {
                return RiskLevel::High;
            }
        }
    }

    // ── Medium risk: key combos that could be destructive ──
    if let ComputerAction::Key { ref text } = ctx.action {
        if is_dangerous_key_combo(text) {
            return RiskLevel::Medium;
        }
    }

    // ── Default: low risk ──
    RiskLevel::Low
}

/// Check if the action is completely blocked by config.
fn is_blocked_action(ctx: &ActionContext, config: &ComputerUseConfig) -> bool {
    // Check active window title FIRST (structural check, more reliable than reasoning)
    if let Some(ref title) = ctx.active_window_title {
        let lower = title.to_lowercase();
        for blocked in &config.blocked_actions {
            match blocked.as_str() {
                "terminal" => {
                    if lower.contains("terminal")
                        || lower.contains("iterm")
                        || lower.contains("bash")
                        || lower.contains("zsh")
                        || lower.contains("console")
                        || lower.contains("powershell")
                        || lower.contains("cmd.exe")
                    {
                        return true;
                    }
                }
                "system_preferences" => {
                    if lower.contains("system preferences")
                        || lower.contains("system settings")
                        || lower.contains("control panel")
                        || lower.contains("設定")
                    {
                        return true;
                    }
                }
                // Generic blocked action: match window title directly
                other => {
                    if lower.contains(&other.to_lowercase()) {
                        return true;
                    }
                }
            }
        }
    }

    // Secondary heuristic: check model reasoning text.
    // NOTE: This is a best-effort supplement to structural checks above.
    // Model reasoning can be manipulated by prompt injection — the window
    // title checks above are the primary defense.
    if let Some(ref reasoning) = ctx.model_reasoning {
        let lower = reasoning.to_lowercase();
        for blocked in &config.blocked_actions {
            let blocked_lower = blocked.to_lowercase();
            // Only match if the blocked keyword is a significant word (4+ chars)
            // to reduce false positives from short common words
            if blocked_lower.len() >= 4 && lower.contains(&blocked_lower) {
                return true;
            }
        }
    }

    false
}

/// Heuristic: does the text look like it might be a password or credit card?
fn looks_like_sensitive_input(text: &str) -> bool {
    // Credit card pattern: 13-19 digits possibly with spaces/dashes
    let digits_only: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if (13..=19).contains(&digits_only.len()) {
        return true;
    }

    // CVV: exactly 3-4 digits (if that's all the text is)
    let trimmed = text.trim();
    if (3..=4).contains(&trimmed.len()) && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }

    false
}

/// Key combos that could be destructive.
fn is_dangerous_key_combo(key: &str) -> bool {
    let lower = key.to_lowercase();
    // Delete-related
    lower.contains("delete") && (lower.contains("ctrl") || lower.contains("super"))
        // Format-related
        || lower.contains("ctrl+shift+delete")
        // System shortcuts
        || lower == "ctrl+alt+delete"
        || lower == "super+l" // lock screen
        || lower == "ctrl+alt+t" // open terminal
}

// ---------------------------------------------------------------------------
// Natural-language emergency stop detection
// ---------------------------------------------------------------------------

/// Stop words that should immediately halt a computer use session.
const STOP_WORDS: &[&str] = &[
    "停", "停止", "別動", "不要繼續", "不要做了",
    "やめて", "止めて", "ストップ",
    "stop", "halt", "abort", "cancel", "quit",
];

/// Check if a message is an emergency stop command.
///
/// Returns `true` if the message matches a known stop word exactly
/// (ignoring surrounding whitespace).
pub fn is_emergency_stop(text: &str) -> bool {
    let trimmed = text.trim().to_lowercase();

    // Exact match against stop words
    for word in STOP_WORDS {
        if trimmed == *word {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(action: ComputerAction) -> ActionContext {
        ActionContext {
            action,
            model_reasoning: None,
            targets_sensitive_input: false,
            active_window_title: None,
        }
    }

    #[test]
    fn low_risk_simple_click() {
        let ctx = make_ctx(ComputerAction::LeftClick {
            coordinate: [100, 200],
        });
        assert_eq!(assess_risk(&ctx, &ComputerUseConfig::default()), RiskLevel::Low);
    }

    #[test]
    fn high_risk_sensitive_input() {
        let mut ctx = make_ctx(ComputerAction::LeftClick {
            coordinate: [100, 200],
        });
        ctx.targets_sensitive_input = true;
        assert_eq!(assess_risk(&ctx, &ComputerUseConfig::default()), RiskLevel::High);
    }

    #[test]
    fn high_risk_credit_card_input() {
        let ctx = make_ctx(ComputerAction::Type {
            text: "4242 4242 4242 4242".to_string(),
        });
        assert_eq!(assess_risk(&ctx, &ComputerUseConfig::default()), RiskLevel::High);
    }

    #[test]
    fn blocked_terminal_action() {
        let mut ctx = make_ctx(ComputerAction::LeftClick {
            coordinate: [100, 200],
        });
        ctx.active_window_title = Some("Terminal — zsh".to_string());
        assert_eq!(assess_risk(&ctx, &ComputerUseConfig::default()), RiskLevel::Blocked);
    }

    #[test]
    fn high_risk_non_whitelisted_app() {
        let config = ComputerUseConfig {
            allowed_apps: vec!["Chromium".to_string()],
            ..Default::default()
        };
        let mut ctx = make_ctx(ComputerAction::LeftClick {
            coordinate: [100, 200],
        });
        ctx.active_window_title = Some("TextEdit".to_string());
        assert_eq!(assess_risk(&ctx, &config), RiskLevel::High);
    }

    #[test]
    fn low_risk_whitelisted_app() {
        let config = ComputerUseConfig {
            allowed_apps: vec!["Chromium".to_string()],
            ..Default::default()
        };
        let mut ctx = make_ctx(ComputerAction::LeftClick {
            coordinate: [100, 200],
        });
        ctx.active_window_title = Some("Chromium - Google".to_string());
        assert_eq!(assess_risk(&ctx, &config), RiskLevel::Low);
    }

    #[test]
    fn emergency_stop_chinese() {
        assert!(is_emergency_stop("停"));
        assert!(is_emergency_stop("停止"));
        assert!(is_emergency_stop("  停  "));
        assert!(is_emergency_stop("別動"));
    }

    #[test]
    fn emergency_stop_english() {
        assert!(is_emergency_stop("stop"));
        assert!(is_emergency_stop("STOP"));
        assert!(is_emergency_stop("abort"));
        assert!(is_emergency_stop("cancel"));
    }

    #[test]
    fn emergency_stop_japanese() {
        assert!(is_emergency_stop("やめて"));
        assert!(is_emergency_stop("止めて"));
    }

    #[test]
    fn not_emergency_stop() {
        assert!(!is_emergency_stop("please stop doing that"));
        assert!(!is_emergency_stop("hello"));
        assert!(!is_emergency_stop("continue"));
    }

    #[test]
    fn medium_risk_dangerous_key() {
        let ctx = make_ctx(ComputerAction::Key {
            text: "ctrl+alt+delete".to_string(),
        });
        assert_eq!(assess_risk(&ctx, &ComputerUseConfig::default()), RiskLevel::Medium);
    }

    #[test]
    fn cvv_detection() {
        assert!(looks_like_sensitive_input("123"));
        assert!(looks_like_sensitive_input("1234"));
        assert!(!looks_like_sensitive_input("12345")); // too long for CVV, too short for CC
        assert!(!looks_like_sensitive_input("hello"));
    }
}
