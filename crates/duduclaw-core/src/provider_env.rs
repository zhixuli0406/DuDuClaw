//! Canonical provider → API-key environment-variable name table.
//!
//! Single source of truth for "which env var(s) hold provider X's API key".
//! Before WP-8B (credentials doctrine P3, `commercial/docs/DESIGN-credentials-doctrine-2026-08.md`
//! §3 P3) this table was copy-pasted three times and had already started to
//! drift in shape (identical values, three independent definitions):
//!
//! 1. `duduclaw-agent::account_rotator::provider_env_key_names` (private fn) —
//!    collapsed onto this module in WP-10C (was left untouched by WP-8B
//!    because it belonged to a parallel work package that wave); it is now a
//!    thin delegate, verified byte-identical to this table before the switch.
//! 2. `duduclaw-llm::provider::resolve_env_key` — now a thin wrapper over
//!    this module.
//! 3. `duduclaw-gateway::runtime::openai_compat` — `PROVIDERS`-derived
//!    `is_available()` key list and the per-provider `resolve_provider_config`
//!    lookup — now both consult this module instead of re-deriving names.
//!
//! Every crate in the workspace already depends on `duduclaw-core`
//! (`duduclaw-llm/Cargo.toml` even documents "depend only on duduclaw-core,
//! never on duduclaw-llm" as the intended layering), so this is the natural
//! single home with zero new dependency edges.

/// Every provider id this table has an opinion about, in table order. Useful
/// for iteration (dashboards, consistency tests) without hand-maintaining a
/// second list that can drift from the match arms below.
pub const KNOWN_PROVIDER_IDS: &[&str] = &[
    "anthropic",
    "openai",
    "gemini",
    "google",
    "deepseek",
    "minimax",
    "groq",
    "together",
    "mistral",
    "openrouter",
    "xai",
    "qwen",
];

/// Known env var name(s) for a provider's API key, in preference order (the
/// first entry is the canonical / most common name; later entries are
/// accepted fallbacks for ecosystems that settled on more than one
/// convention). Unknown provider ids return an empty slice — this function
/// never guesses a name.
pub fn provider_env_key_names(provider: &str) -> &'static [&'static str] {
    match provider {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        // GOOGLE_API_KEY predates GEMINI_API_KEY; both are still accepted.
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "together" => &["TOGETHER_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        // DASHSCOPE_API_KEY is Alibaba's own SDK convention; QWEN_API_KEY is
        // the community convention some proxies use instead.
        "qwen" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        _ => &[],
    }
}

/// Resolve a provider's API key from the first present + non-empty env var
/// among [`provider_env_key_names`]. Unknown provider ids and providers with
/// no configured env var present both return `None` — never an empty string
/// (empty-is-unset is enforced by the `find(|v| !v.is_empty())` filter).
pub fn resolve_env_key(provider: &str) -> Option<String> {
    provider_env_key_names(provider)
        .iter()
        .filter_map(|n| std::env::var(n).ok())
        .find(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_all_resolve_at_least_one_name() {
        for provider in KNOWN_PROVIDER_IDS {
            let names = provider_env_key_names(provider);
            assert!(
                !names.is_empty(),
                "provider `{provider}` listed in KNOWN_PROVIDER_IDS but has no env names"
            );
        }
    }

    #[test]
    fn unknown_provider_returns_empty_not_a_guess() {
        assert!(provider_env_key_names("totally-unknown-vendor").is_empty());
        assert_eq!(resolve_env_key("totally-unknown-vendor"), None);
    }

    #[test]
    fn every_name_follows_the_api_key_convention() {
        // Sanity guard against a future typo (e.g. a lowercase name or a
        // name missing the `_API_KEY` suffix) slipping into the canonical
        // table unnoticed.
        for provider in KNOWN_PROVIDER_IDS {
            for name in provider_env_key_names(provider) {
                assert!(
                    name.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                    "`{name}` (provider `{provider}`) is not SCREAMING_SNAKE_CASE"
                );
                assert!(
                    name.ends_with("_API_KEY"),
                    "`{name}` (provider `{provider}`) does not end in _API_KEY"
                );
            }
        }
    }

    #[test]
    fn gemini_and_google_are_aliases() {
        assert_eq!(
            provider_env_key_names("gemini"),
            provider_env_key_names("google")
        );
    }

    // Serialize the few env-mutating tests so they don't race each other
    // (matches the idiom in `platform.rs::home_tests`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_env_key_prefers_first_present_name_and_skips_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev_dashscope = std::env::var("DASHSCOPE_API_KEY").ok();
        let prev_qwen = std::env::var("QWEN_API_KEY").ok();

        // Neither set → None.
        unsafe {
            std::env::remove_var("DASHSCOPE_API_KEY");
            std::env::remove_var("QWEN_API_KEY");
        }
        assert_eq!(resolve_env_key("qwen"), None);

        // Only the fallback set → fallback wins (nothing else present).
        unsafe { std::env::set_var("QWEN_API_KEY", "qwen-value") };
        assert_eq!(resolve_env_key("qwen").as_deref(), Some("qwen-value"));

        // Both set → canonical (first) name wins.
        unsafe { std::env::set_var("DASHSCOPE_API_KEY", "dashscope-value") };
        assert_eq!(resolve_env_key("qwen").as_deref(), Some("dashscope-value"));

        // Canonical set but empty → falls through to the non-empty fallback.
        unsafe { std::env::set_var("DASHSCOPE_API_KEY", "") };
        assert_eq!(resolve_env_key("qwen").as_deref(), Some("qwen-value"));

        unsafe {
            match prev_dashscope {
                Some(v) => std::env::set_var("DASHSCOPE_API_KEY", v),
                None => std::env::remove_var("DASHSCOPE_API_KEY"),
            }
            match prev_qwen {
                Some(v) => std::env::set_var("QWEN_API_KEY", v),
                None => std::env::remove_var("QWEN_API_KEY"),
            }
        }
    }
}
