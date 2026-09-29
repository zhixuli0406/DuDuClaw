/// WP-A (TODO-ai-runtimes-2026-09.md §3 WP-A item 2) — end-to-end coverage
/// for the exact `[[accounts]]` shape `duduclaw-gateway`'s `build_account_entry`
/// now writes for a non-Anthropic provider: `provider = "<id>"` +
/// `api_key` / `api_key_enc` (never `anthropic_api_key*`, which stays
/// Anthropic-only). `resolve_api_key`'s field-name precedence and
/// `select_for_provider`'s provider filter were already covered separately
/// (`wp8a_secret_ref_consolidation_tests`, `provider_rotation_tests`); this
/// module pins that `load_from_config` — the actual gateway startup / account
/// reload path `claude_runner::resolve_provider_key` relies on — reads that
/// combination correctly end to end, so an OpenAI/Gemini/xAI/DeepSeek key
/// added via `accounts.add` is really found by the Direct-API key resolution
/// path, not just by its lower-level pieces in isolation.
use super::*;

#[tokio::test]
async fn non_anthropic_provider_account_is_loaded_and_selectable() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[[accounts]]
id = "openai-prod"
type = "api_key"
provider = "openai"
api_key = "sk-openai-test-key"
priority = 1
monthly_budget_cents = 5000
"#,
    )
    .unwrap();

    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let loaded = rotator.load_from_config(home.path()).await.unwrap();
    // `>= 1`, not `== 1`: step 2 of `load_from_config` auto-detects a
    // host `claude` login (`claude auth status`) whenever the config has
    // no Anthropic OAuth row, so on a developer machine that is signed
    // in to Claude Code this legitimately loads a second account. The
    // assertions below pin the row this test is about by id.
    assert!(loaded >= 1, "the [[accounts]] entry must be loaded (got {loaded})");

    let sel = rotator
        .select_for_provider("openai")
        .await
        .expect("the openai account must be selectable by provider");
    assert_eq!(sel.id, "openai-prod");
    assert_eq!(
        sel.raw_key.as_deref(),
        Some("sk-openai-test-key"),
        "raw_key must come from the `api_key` field (not `anthropic_api_key`)"
    );
    // Cross-provider filtering itself (an openai-only config must not
    // surface under a different provider's pool) is already covered by
    // `provider_rotation_tests::select_for_provider_filters_by_provider`;
    // not re-asserted here to avoid depending on whether ANTHROPIC_API_KEY
    // happens to be set in the environment running this test (the
    // no-configured-accounts path falls back to synthesizing an ephemeral
    // env-var account — see `env_fallback_account_env` — which is correct
    // behavior but would make an `is_none()` assertion here environment-
    // dependent).
}

/// The `_enc` twin of the same non-Anthropic field-name shape, going
/// through the real per-machine keyfile encryption path (not a plaintext
/// literal), since `build_account_entry` writes `api_key_enc` whenever
/// encryption succeeds.
#[tokio::test]
async fn non_anthropic_provider_account_resolves_encrypted_key() {
    use duduclaw_security::crypto::CryptoEngine;

    let home = tempfile::tempdir().unwrap();
    let keyfile = home.path().join(".keyfile");
    let key = CryptoEngine::generate_key().unwrap();
    std::fs::write(&keyfile, key).unwrap();
    let enc = CryptoEngine::new(&key)
        .unwrap()
        .encrypt_string("sk-deepseek-enc-key")
        .unwrap();

    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
[[accounts]]
id = "deepseek-prod"
type = "api_key"
provider = "deepseek"
api_key_enc = "{enc}"
priority = 1
monthly_budget_cents = 5000
"#
        ),
    )
    .unwrap();

    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.load_from_config(home.path()).await.unwrap();

    let sel = rotator
        .select_for_provider("deepseek")
        .await
        .expect("the deepseek account must be selectable by provider");
    assert_eq!(sel.raw_key.as_deref(), Some("sk-deepseek-enc-key"));
}
