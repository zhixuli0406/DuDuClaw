use super::*;

/// Build an available API-key account for a given provider.
fn api_account(id: &str, provider: &str, key: &str) -> Account {
    Account {
        id: id.to_string(),
        auth_method: AuthMethod::ApiKey,
        provider: provider.to_string(),
        priority: 10,
        monthly_budget_cents: 5000,
        tags: vec![],
        profile: String::new(),
        email: String::new(),
        subscription: String::new(),
        label: id.to_string(),
        expires_at: None,
        api_key: key.to_string(),
        oauth_token: None,
        credentials_dir: None,
        is_healthy: true,
        consecutive_errors: 0,
        spent_this_month: 0,
        cooldown_until: None,
        last_used: None,
        total_requests: 0,
        credential_state: CredentialState::Unverified,
        auth_dead_strikes: 0,
        next_probe_at: None,
        probe_failures: 0,
    }
}

/// An account parsed WITHOUT a `provider` field must default to "anthropic"
/// so existing configs behave byte-identically.
#[test]
fn absent_provider_defaults_to_anthropic() {
    let toml_src = r#"
            id = "a"
            type = "api_key"
            api_key = "sk-test"
        "#;
    let table: toml::Table = toml_src.parse().unwrap();
    // Round-trip the default via serde: an Account deserialized from a table
    // missing `provider` gets the default.
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default = "default_provider")]
        provider: String,
    }
    let p: Probe = table.clone().try_into().unwrap();
    assert_eq!(p.provider, "anthropic");
}

/// A `provider = "openai"` field is parsed and preserved.
#[test]
fn present_provider_is_parsed() {
    let toml_src = r#"
            provider = "openai"
        "#;
    let table: toml::Table = toml_src.parse().unwrap();
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default = "default_provider")]
        provider: String,
    }
    let p: Probe = table.try_into().unwrap();
    assert_eq!(p.provider, "openai");
}

/// `select_for_provider` only considers accounts of the requested provider.
#[tokio::test]
async fn select_for_provider_filters_by_provider() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator
        .push_account_for_test(api_account("anthropic-1", "anthropic", "sk-ant"))
        .await;
    rotator
        .push_account_for_test(api_account("openai-1", "openai", "sk-openai"))
        .await;

    let sel = rotator
        .select_for_provider("openai")
        .await
        .expect("should select the openai account");
    assert_eq!(sel.id, "openai-1");
    assert_eq!(sel.provider, "openai");
    // The openai account emits OPENAI_API_KEY (not ANTHROPIC_API_KEY).
    assert_eq!(
        sel.env_vars.get("OPENAI_API_KEY").map(String::as_str),
        Some("sk-openai")
    );
    assert!(!sel.env_vars.contains_key("ANTHROPIC_API_KEY"));
    // Raw key is exposed for direct-API callers.
    assert_eq!(sel.raw_key.as_deref(), Some("sk-openai"));
}

/// Back-compat: `select()` == `select_for_provider("anthropic")` and emits
/// the unchanged ANTHROPIC_API_KEY var for an anthropic API-key account.
#[tokio::test]
async fn select_is_anthropic_back_compat() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator
        .push_account_for_test(api_account("anthropic-1", "anthropic", "sk-ant"))
        .await;
    rotator
        .push_account_for_test(api_account("openai-1", "openai", "sk-openai"))
        .await;

    let sel = rotator.select().await.expect("should select anthropic");
    assert_eq!(sel.id, "anthropic-1");
    assert_eq!(sel.provider, "anthropic");
    assert_eq!(
        sel.env_vars.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-ant")
    );
    assert!(!sel.env_vars.contains_key("OPENAI_API_KEY"));
}

/// When a provider has configured accounts that are all unavailable,
/// selection returns None (does NOT fall through to env-var fallback).
#[tokio::test]
async fn unavailable_configured_accounts_do_not_env_fallback() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    // Budget-exhausted API key → unavailable, but still "configured".
    let mut acc = api_account("openai-1", "openai", "sk-openai");
    acc.monthly_budget_cents = 100;
    acc.spent_this_month = 200;
    rotator.push_account_for_test(acc).await;

    assert!(rotator.select_for_provider("openai").await.is_none());
}

/// env-var fallback synthesizes exactly one ephemeral account when the
/// config declares no accounts for the requested provider.
#[tokio::test]
async fn env_var_fallback_synthesizes_single_account() {
    // groq is not referenced by any other test in this crate, so mutating
    // its env var here is isolated within this test binary.
    unsafe { std::env::set_var("GROQ_API_KEY", "gsk-test") };
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);

    let sel = rotator
        .select_for_provider("groq")
        .await
        .expect("env var fallback should synthesize an account");
    assert_eq!(sel.id, "groq-env");
    assert_eq!(sel.provider, "groq");
    assert_eq!(
        sel.env_vars.get("GROQ_API_KEY").map(String::as_str),
        Some("gsk-test")
    );
    assert_eq!(sel.raw_key.as_deref(), Some("gsk-test"));

    unsafe { std::env::remove_var("GROQ_API_KEY") };
}

/// Unknown provider with no env var → no fallback, returns None.
#[tokio::test]
async fn unknown_provider_with_no_env_returns_none() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    assert!(rotator
        .select_for_provider("not-a-real-provider")
        .await
        .is_none());
}

/// Gemini env-var fallback accepts the GOOGLE_API_KEY alias but always
/// emits the canonical GEMINI_API_KEY name.
#[tokio::test]
async fn gemini_alias_env_emits_canonical_name() {
    unsafe { std::env::set_var("GOOGLE_API_KEY", "goog-test") };
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);

    let sel = rotator
        .select_for_provider("gemini")
        .await
        .expect("GOOGLE_API_KEY alias should satisfy gemini fallback");
    assert_eq!(
        sel.env_vars.get("GEMINI_API_KEY").map(String::as_str),
        Some("goog-test"),
        "canonical GEMINI_API_KEY must be emitted even when read via alias"
    );
    assert!(!sel.env_vars.contains_key("GOOGLE_API_KEY"));

    unsafe { std::env::remove_var("GOOGLE_API_KEY") };
}
