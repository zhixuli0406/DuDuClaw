//! Safety word processor — immediate kill-switch detection.
//!
//! Scans incoming messages for configurable safety words (e.g., `!STOP`,
//! `!停止`) and returns the appropriate action. This runs at the very
//! top of the message pipeline with zero LLM cost.

use crate::killswitch::SafetyWordsConfig;

/// The scope affected by a safety word action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyWordScope {
    /// Stop/resume the current agent in the current channel scope.
    CurrentScope,
    /// Stop/resume ALL agents globally.
    Global,
}

/// Action determined by safety word detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyWordAction {
    /// Stop agent(s) in the given scope.
    Stop(SafetyWordScope),
    /// Resume agent(s) in the given scope.
    Resume,
    /// Query current safety/failsafe status.
    Status,
    /// Not a safety word — continue normal processing.
    None,
}

/// Check if the incoming message is a safety word and return the action.
///
/// This is designed to be called as the very first filter in the pipeline.
/// Returns `SafetyWordAction::None` for non-safety-word messages with
/// minimal overhead (just string comparisons).
pub fn check(text: &str, config: &SafetyWordsConfig) -> SafetyWordAction {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return SafetyWordAction::None;
    }

    // Fast path: most messages are not safety words. Check for `!` prefix first
    // to skip the case-insensitive comparison loop for normal messages.
    let first_byte = trimmed.as_bytes()[0];
    if first_byte != b'!' && !config.has_non_bang_prefix() {
        return SafetyWordAction::None;
    }

    // Use case-insensitive comparison without allocating (eq_ignore_ascii_case
    // handles ASCII; for CJK chars uppercase == original so this is correct).
    // Check stop_all first (more specific match before stop)
    for word in &config.stop_all {
        if trimmed.eq_ignore_ascii_case(word) {
            return SafetyWordAction::Stop(SafetyWordScope::Global);
        }
    }

    for word in &config.stop {
        if trimmed.eq_ignore_ascii_case(word) {
            return SafetyWordAction::Stop(SafetyWordScope::CurrentScope);
        }
    }

    for word in &config.resume {
        if trimmed.eq_ignore_ascii_case(word) {
            return SafetyWordAction::Resume;
        }
    }

    for word in &config.status {
        if trimmed.eq_ignore_ascii_case(word) {
            return SafetyWordAction::Status;
        }
    }

    SafetyWordAction::None
}

/// Format a human-readable response for a safety word action.
pub fn format_response(action: &SafetyWordAction, scope_name: &str) -> String {
    match action {
        SafetyWordAction::Stop(SafetyWordScope::CurrentScope) => {
            format!("🛑 Agent stopped in scope: {scope_name}")
        }
        SafetyWordAction::Stop(SafetyWordScope::Global) => {
            "🛑 EMERGENCY STOP — all agents halted".to_string()
        }
        SafetyWordAction::Resume => {
            format!("✅ Agent resumed in scope: {scope_name}")
        }
        SafetyWordAction::Status => {
            // Status response is built by the caller with actual state info
            String::new()
        }
        SafetyWordAction::None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> SafetyWordsConfig {
        SafetyWordsConfig::default()
    }

    #[test]
    fn detects_stop() {
        let config = default_config();
        assert_eq!(check("!STOP", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("!停止", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("!緊急停止", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
    }

    #[test]
    fn detects_stop_all() {
        let config = default_config();
        assert_eq!(check("!STOP ALL", &config), SafetyWordAction::Stop(SafetyWordScope::Global));
        assert_eq!(check("!全部停止", &config), SafetyWordAction::Stop(SafetyWordScope::Global));
    }

    #[test]
    fn detects_resume() {
        let config = default_config();
        assert_eq!(check("!RESUME", &config), SafetyWordAction::Resume);
        assert_eq!(check("!恢復", &config), SafetyWordAction::Resume);
        assert_eq!(check("!繼續", &config), SafetyWordAction::Resume);
    }

    #[test]
    fn detects_status() {
        let config = default_config();
        assert_eq!(check("!STATUS", &config), SafetyWordAction::Status);
        assert_eq!(check("!狀態", &config), SafetyWordAction::Status);
    }

    #[test]
    fn case_insensitive() {
        let config = default_config();
        assert_eq!(check("!stop", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("!Stop", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("!resume", &config), SafetyWordAction::Resume);
        assert_eq!(check("!stop all", &config), SafetyWordAction::Stop(SafetyWordScope::Global));
    }

    #[test]
    fn trims_whitespace() {
        let config = default_config();
        assert_eq!(check("  !STOP  ", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("\n!RESUME\n", &config), SafetyWordAction::Resume);
    }

    #[test]
    fn non_safety_word_returns_none() {
        let config = default_config();
        assert_eq!(check("Hello world", &config), SafetyWordAction::None);
        assert_eq!(check("!STOP please", &config), SafetyWordAction::None); // extra text
        assert_eq!(check("", &config), SafetyWordAction::None);
        assert_eq!(check("STOP", &config), SafetyWordAction::None); // no ! prefix
    }

    #[test]
    fn custom_safety_words() {
        let config = SafetyWordsConfig {
            stop: vec!["!HALT".to_string(), "!暫停".to_string()],
            stop_all: vec!["!KILL ALL".to_string()],
            resume: vec!["!GO".to_string()],
            status: vec!["!CHECK".to_string()],
        };
        assert_eq!(check("!HALT", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("!暫停", &config), SafetyWordAction::Stop(SafetyWordScope::CurrentScope));
        assert_eq!(check("!KILL ALL", &config), SafetyWordAction::Stop(SafetyWordScope::Global));
        assert_eq!(check("!GO", &config), SafetyWordAction::Resume);
        assert_eq!(check("!CHECK", &config), SafetyWordAction::Status);
        // Original words should not match
        assert_eq!(check("!STOP", &config), SafetyWordAction::None);
    }

    #[test]
    fn stop_all_takes_precedence_over_stop() {
        // If "!STOP ALL" starts with "!STOP", ensure "!STOP ALL" matches stop_all, not stop
        let config = default_config();
        assert_eq!(check("!STOP ALL", &config), SafetyWordAction::Stop(SafetyWordScope::Global));
    }

    #[test]
    fn format_response_messages() {
        let msg = format_response(
            &SafetyWordAction::Stop(SafetyWordScope::CurrentScope),
            "telegram:12345",
        );
        assert!(msg.contains("stopped"));
        assert!(msg.contains("telegram:12345"));

        let msg = format_response(&SafetyWordAction::Stop(SafetyWordScope::Global), "");
        assert!(msg.contains("EMERGENCY STOP"));

        let msg = format_response(&SafetyWordAction::Resume, "discord:guild:789");
        assert!(msg.contains("resumed"));
    }
}
