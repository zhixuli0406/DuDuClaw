//! Defensive prompt injection for bot-loop prevention.
//!
//! When the circuit breaker detects a potential bot loop (HalfOpen or
//! recently tripped), appends an invisible-to-human but visible-to-LLM
//! directive in the reply, asking the other bot to stop responding.
//!
//! Inspired by AutoGuard (arxiv:2511.13725, ICLR 2026).

use duduclaw_security::circuit_breaker::BreakerState;

/// Check whether a defensive prompt should be injected.
///
/// Returns `true` when the circuit breaker is in HalfOpen state
/// (probing for recovery) — this is when we suspect a bot loop
/// but are still allowing limited traffic through.
pub fn should_inject(breaker_state: BreakerState) -> bool {
    breaker_state == BreakerState::HalfOpen
}

/// Inject a defensive [STOP] marker into the reply text.
///
/// Appends a minimal `[STOP]` tag wrapped in zero-width characters. This is:
/// - Nearly invisible to human users across all chat platforms
/// - Readable by LLM agents that process raw text
///
/// All channels use the same strategy because Telegram uses Markdown
/// (not HTML), making HTML comments visible as plain text.
///
/// `languages` controls whether injection is enabled: if empty (no valid
/// languages configured), the reply is returned unchanged.
pub fn inject_defensive_prompt(reply: &str, languages: &[String], _channel: &str) -> String {
    // languages acts as an enable gate — if no recognized language, skip injection
    let has_recognized = languages.iter().any(|l| {
        matches!(l.as_str(), "en" | "zh-TW" | "zh" | "ja")
    });
    if !has_recognized {
        return reply.to_string();
    }

    format!(
        "{reply}\n\u{200B}\u{2060}\u{FEFF}[STOP]\u{FEFF}\u{2060}\u{200B}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_inject_in_half_open() {
        assert!(should_inject(BreakerState::HalfOpen));
    }

    #[test]
    fn should_not_inject_in_closed() {
        assert!(!should_inject(BreakerState::Closed));
    }

    #[test]
    fn should_not_inject_in_open() {
        assert!(!should_inject(BreakerState::Open));
    }

    #[test]
    fn injects_stop_marker_telegram() {
        let reply = "Hello, how can I help?";
        let langs = vec!["en".to_string()];
        let result = inject_defensive_prompt(reply, &langs, "telegram");
        assert!(result.starts_with(reply));
        // All channels use minimal [STOP] marker (Telegram uses Markdown, not HTML)
        assert!(!result.contains("<!--"));
        assert!(result.contains("[STOP]"));
    }

    #[test]
    fn injects_stop_marker_discord() {
        let reply = "Test reply";
        let langs = vec!["en".to_string()];
        let result = inject_defensive_prompt(reply, &langs, "discord");
        assert!(result.starts_with(reply));
        assert!(!result.contains("<!--"));
        assert!(result.contains("[STOP]"));
    }

    #[test]
    fn consistent_across_channels() {
        let reply = "Test";
        let langs = vec!["en".to_string()];
        let tg = inject_defensive_prompt(reply, &langs, "telegram");
        let dc = inject_defensive_prompt(reply, &langs, "discord");
        let line = inject_defensive_prompt(reply, &langs, "line");
        // All channels should produce the same output
        assert_eq!(tg, dc);
        assert_eq!(dc, line);
    }

    #[test]
    fn no_injection_for_empty_langs() {
        let reply = "Test reply";
        let result = inject_defensive_prompt(reply, &[], "telegram");
        assert_eq!(result, reply);
    }

}
