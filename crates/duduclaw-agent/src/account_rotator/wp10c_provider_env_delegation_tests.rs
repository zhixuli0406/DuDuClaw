/// WP-10C: `provider_env_key_names` here is now a thin delegate to
/// `duduclaw_core::provider_env::provider_env_key_names` — the third
/// hand-copied table collapsed onto the WP-8B single source of truth. These
/// tests pin the delegation itself so a future edit to either side can't
/// silently re-diverge without a red test.
use super::provider_env_key_names;

/// Every known provider must resolve to exactly the same name list as the
/// canonical `duduclaw-core` table — this is the whole point of the
/// delegation (not just "non-empty", but byte-identical).
#[test]
fn matches_core_table_for_every_known_provider() {
    for provider in duduclaw_core::provider_env::KNOWN_PROVIDER_IDS {
        assert_eq!(
            provider_env_key_names(provider),
            duduclaw_core::provider_env::provider_env_key_names(provider),
            "agent-crate delegate diverged from duduclaw-core for provider `{provider}`"
        );
    }
}

/// Spot-check the two multi-name providers plus the alias pair, matching
/// the pre-consolidation table's asserted shape.
#[test]
fn known_provider_shapes_are_unchanged() {
    assert_eq!(provider_env_key_names("anthropic"), &["ANTHROPIC_API_KEY"]);
    assert_eq!(provider_env_key_names("openai"), &["OPENAI_API_KEY"]);
    assert_eq!(
        provider_env_key_names("gemini"),
        &["GEMINI_API_KEY", "GOOGLE_API_KEY"]
    );
    assert_eq!(
        provider_env_key_names("gemini"),
        provider_env_key_names("google"),
        "gemini/google must remain aliases"
    );
    assert_eq!(
        provider_env_key_names("qwen"),
        &["DASHSCOPE_API_KEY", "QWEN_API_KEY"]
    );
}

/// Unknown provider ids must still return an empty slice, never a guess.
#[test]
fn unknown_provider_is_empty() {
    assert!(provider_env_key_names("totally-unknown-vendor").is_empty());
}
