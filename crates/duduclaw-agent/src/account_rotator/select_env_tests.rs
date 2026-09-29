use super::*;

fn account_with_credentials_dir(dir: PathBuf) -> Account {
    Account {
        id: "test".to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority: 1,
        monthly_budget_cents: 0,
        tags: vec![],
        profile: "default".to_string(),
        email: String::new(),
        subscription: "max".to_string(),
        label: "test".to_string(),
        expires_at: None,
        api_key: String::new(),
        oauth_token: None,
        credentials_dir: Some(dir),
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

/// Regression test for the bug where the auto-detected default OAuth
/// session would have `CLAUDE_CONFIG_DIR=~/.claude` injected into the
/// subprocess env, which makes `claude` CLI stop looking at the OS
/// keychain and return "Not logged in · Please run /login" forever.
///
/// Fix: when `credentials_dir == ~/.claude` (the default location),
/// `select()` must NOT set `CLAUDE_CONFIG_DIR` at all. Claude CLI then
/// uses its normal default config + keychain lookup.
#[tokio::test]
async fn default_keychain_session_does_not_set_claude_config_dir() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    // Mimic what `detect_default_oauth_session()` produces.
    let default_dir = dirs::home_dir().expect("home").join(".claude");
    rotator
        .push_account_for_test(account_with_credentials_dir(default_dir))
        .await;

    let env = rotator.select().await.expect("should select account");
    assert!(
        !env.env_vars.contains_key("CLAUDE_CONFIG_DIR"),
        "CLAUDE_CONFIG_DIR must not be set for default keychain session; \
             setting it — even to the same path — breaks Claude CLI auth \
             lookup. Got env_vars: {:?}",
        env.env_vars
    );
    // ANTHROPIC_API_KEY must still be set empty to prevent ambient
    // api key from overriding OAuth.
    assert_eq!(env.env_vars.get("ANTHROPIC_API_KEY").map(String::as_str), Some(""));
}

/// A non-default profile directory (e.g. `~/.claude/profiles/work`)
/// MUST still have `CLAUDE_CONFIG_DIR` injected, otherwise claude CLI
/// wouldn't know to pick up that profile's credentials.
#[tokio::test]
async fn non_default_profile_dir_still_sets_claude_config_dir() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let profile_dir = dirs::home_dir()
        .expect("home")
        .join(".claude/profiles/work");
    rotator
        .push_account_for_test(account_with_credentials_dir(profile_dir.clone()))
        .await;

    let env = rotator.select().await.expect("should select account");
    assert_eq!(
        env.env_vars.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some(profile_dir.to_string_lossy().as_ref())
    );
}

/// v1.61.0 regression guard: a `setup-token` session must put the token ON
/// the account, not rely on the child inheriting it.
///
/// The P3 env scrub drops `*_TOKEN` from the spawn environment, so an
/// account with `oauth_token: None` and no real keychain hands the spawned
/// CLI nothing — every dispatch failed `authentication_failed` while a
/// manual `claude -p` in the same container still worked. Asserting on the
/// built env (not on the detection function, which shells out to `claude`)
/// keeps this hermetic.
#[test]
fn setup_token_account_injects_the_token_into_spawn_env() {
    let mut acct = oauth_account("oauth-default");
    acct.oauth_token = Some("sk-ant-oat01-test".to_string());
    acct.credentials_dir = Some(std::path::PathBuf::from("/home/x/.claude"));

    let env = build_account_env(&acct);

    assert_eq!(
        env.env_vars.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some("sk-ant-oat01-test"),
        "a setup-token account must inject its token explicitly — the spawn \
             env allowlist will not carry it ambiently"
    );
}

/// Build an available OAuth account with an explicit setup-token so it
/// passes `is_available()` without touching the OS keychain.
fn oauth_account(id: &str) -> Account {
    Account {
        id: id.to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority: 1,
        monthly_budget_cents: 0,
        tags: vec![],
        profile: "default".to_string(),
        email: String::new(),
        subscription: "max".to_string(),
        label: id.to_string(),
        expires_at: None,
        api_key: String::new(),
        oauth_token: Some(format!("token-{id}")),
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

/// L4 regression: the `LeastCost` strategy must rotate fairly among
/// equal-cost (equal-spend) OAuth accounts instead of always returning
/// the first one. We simulate the realistic flow where each selection is
/// followed by `on_success`, which stamps `last_used` and makes the
/// least-recently-used tiebreaker advance to the next account.
#[tokio::test]
async fn least_cost_rotates_among_equal_cost_oauth_accounts() {
    let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
    rotator.push_account_for_test(oauth_account("a")).await;
    rotator.push_account_for_test(oauth_account("b")).await;
    rotator.push_account_for_test(oauth_account("c")).await;

    let mut seen = std::collections::HashSet::new();
    for _ in 0..3 {
        let env = rotator.select().await.expect("should select account");
        seen.insert(env.id.clone());
        // Report success with zero cost so all accounts stay equal-cost;
        // this updates `last_used` so the next select picks a different one.
        rotator.on_success(&env.id, 0).await;
    }

    assert_eq!(
        seen.len(),
        3,
        "LeastCost should rotate across all three equal-cost OAuth accounts, \
             not repeatedly pick the first; saw {seen:?}"
    );
}

/// HIGH-C regression: a foreign-provider OAuth seat (added via
/// `duduclaw auth device`, e.g. copilot/qwen) must NOT suppress the
/// Anthropic host-login auto-detect — otherwise the anthropic pool is
/// empty and every channel reply fails NoAccounts.
#[test]
fn foreign_oauth_seat_does_not_suppress_anthropic_autodetect() {
    let mut seat = oauth_account("copilot-seat");
    seat.provider = "github".to_string();
    assert!(
        should_autodetect_anthropic_oauth(&[seat]),
        "a github OAuth seat alone must still trigger anthropic auto-detect"
    );

    let mut qwen = oauth_account("qwen-seat");
    qwen.provider = "qwen".to_string();
    let mut codex = oauth_account("codex-seat");
    codex.provider = "openai".to_string();
    assert!(
        should_autodetect_anthropic_oauth(&[qwen, codex]),
        "multiple foreign seats must still trigger anthropic auto-detect"
    );
}

/// The auto-detect gate closes only when an Anthropic OAuth account is
/// already configured; an Anthropic API-key account does not close it
/// (API-key and OAuth are distinct pools by design).
#[test]
fn anthropic_oauth_account_suppresses_autodetect() {
    let anth = oauth_account("anthropic-oauth"); // provider = "anthropic"
    assert!(!should_autodetect_anthropic_oauth(&[anth]));

    // Empty pool → detect.
    assert!(should_autodetect_anthropic_oauth(&[]));

    // Mixed: foreign seat + anthropic OAuth → no detect needed.
    let mut seat = oauth_account("copilot-seat");
    seat.provider = "github".to_string();
    let anth2 = oauth_account("anthropic-oauth-2");
    assert!(!should_autodetect_anthropic_oauth(&[seat, anth2]));
}
