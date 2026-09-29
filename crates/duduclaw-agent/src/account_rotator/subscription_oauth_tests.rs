// ── Subscription-OAuth breadth (G2 Part A) ──────────────────
//
// The rotator carries consumer subscription seats from providers OTHER than
// Anthropic (ChatGPT Codex / GitHub Copilot / Qwen Portal) as OAuth pool
// members, selectable under any strategy within their provider pool.
use super::*;

/// A subscription OAuth seat for an arbitrary provider. No explicit token
/// and no credentials dir — mirrors the Codex "host login inherited" case.
fn oauth_seat(id: &str, provider: &str) -> Account {
    Account {
        id: id.to_string(),
        auth_method: AuthMethod::OAuth,
        provider: provider.to_string(),
        priority: 1,
        monthly_budget_cents: 0,
        tags: vec![],
        profile: "default".to_string(),
        email: String::new(),
        subscription: "pro".to_string(),
        label: id.to_string(),
        expires_at: None,
        api_key: String::new(),
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

/// A non-Anthropic subscription seat is available with neither an explicit
/// token nor a credentials dir (host-login inheritance) — unlike an
/// Anthropic OAuth account, which requires one of the two.
#[test]
fn non_anthropic_oauth_seat_available_without_token_or_dir() {
    let codex = oauth_seat("codex-1", "openai");
    assert!(
        codex.is_available(),
        "non-Anthropic subscription seat should be available on health alone"
    );
    // Contrast: an Anthropic OAuth account with no token/dir is unavailable.
    let anth = oauth_seat("anth-1", "anthropic");
    assert!(
        !anth.is_available(),
        "Anthropic OAuth needs an explicit token or credentials dir"
    );
}

/// `select_for_provider` isolates by provider across a mixed OAuth pool and
/// carries the provider on the selection.
#[tokio::test]
async fn select_isolates_by_provider_across_oauth_pool() {
    let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
    rotator.push_account_for_test(oauth_seat("codex-1", "openai")).await;
    rotator.push_account_for_test(oauth_seat("copilot-1", "github")).await;

    let sel = rotator
        .select_for_provider("openai")
        .await
        .expect("should select the openai subscription seat");
    assert_eq!(sel.id, "codex-1");
    assert_eq!(sel.provider, "openai");
    // Codex inherits host login — no fabricated token env var is emitted,
    // and (critically) no ANTHROPIC_API_KEY leaks onto the seat.
    assert!(sel.env_vars.is_empty(), "no env vars for host-login-inherited seat");
    // The seat token is NOT exposed as an API key.
    assert!(sel.raw_key.is_none());

    let sel2 = rotator
        .select_for_provider("github")
        .await
        .expect("should select the copilot seat");
    assert_eq!(sel2.id, "copilot-1");
    assert_eq!(sel2.provider, "github");
}

/// `select()` (the Anthropic back-compat shim) never returns a
/// non-Anthropic subscription seat — provider isolation holds.
#[tokio::test]
async fn anthropic_shim_ignores_non_anthropic_seats() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth_seat("codex-1", "openai")).await;
    // No anthropic account present → the anthropic pool is empty.
    assert!(
        rotator.select().await.is_none(),
        "anthropic selection must not fall through to an openai seat"
    );
}

/// LeastCost prefers an OAuth seat (subscription, zero per-token cost) over
/// an API-key account within the SAME provider pool.
#[tokio::test]
async fn least_cost_prefers_oauth_seat_within_provider() {
    let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
    // API-key openai account…
    let mut key_acc = oauth_seat("openai-key", "openai");
    key_acc.auth_method = AuthMethod::ApiKey;
    key_acc.api_key = "sk-openai".to_string();
    key_acc.monthly_budget_cents = 5000;
    rotator.push_account_for_test(key_acc).await;
    // …and an OAuth seat for the same provider.
    rotator.push_account_for_test(oauth_seat("openai-seat", "openai")).await;

    let sel = rotator
        .select_for_provider("openai")
        .await
        .expect("should select within the openai pool");
    assert_eq!(
        sel.id, "openai-seat",
        "LeastCost should prefer the zero-cost subscription seat"
    );
}

/// A stored seat credential (decrypted from `oauth_token_enc` at load) is
/// surfaced on `AccountEnv.seat_token` for a non-Anthropic OAuth seat, but
/// never as `raw_key` (it is not an API key). `has_seat_for_provider`
/// reports it as available.
#[tokio::test]
async fn stored_seat_credential_surfaces_on_seat_token_not_raw_key() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let mut seat = oauth_seat("copilot-seat", "github");
    seat.oauth_token = Some("gho_stored_token".to_string());
    rotator.push_account_for_test(seat).await;

    assert!(rotator.has_seat_for_provider("github").await);
    assert!(!rotator.has_seat_for_provider("qwen").await);

    let sel = rotator
        .select_for_provider("github")
        .await
        .expect("should select the copilot seat");
    assert_eq!(sel.seat_token.as_deref(), Some("gho_stored_token"));
    assert!(sel.raw_key.is_none(), "seat token must NOT be an API key");
}

/// A non-Anthropic OAuth seat WITHOUT a stored token (host-login-inherited,
/// e.g. Codex) has no `seat_token` and is not reported by
/// `has_seat_for_provider` (nothing to forward through the proxy).
#[tokio::test]
async fn host_login_seat_has_no_seat_token() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator
        .push_account_for_test(oauth_seat("codex-1", "openai"))
        .await;
    let sel = rotator.select_for_provider("openai").await.unwrap();
    assert!(sel.seat_token.is_none());
    assert!(!rotator.has_seat_for_provider("openai").await);
}

/// The subscription catalogue exposes the four consumer sources.
#[test]
fn known_subscription_providers_catalogue() {
    let cat = known_subscription_providers();
    let ids: Vec<&str> = cat.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&"anthropic"));
    assert!(ids.contains(&"openai"));
    assert!(ids.contains(&"github"));
    assert!(ids.contains(&"qwen"));
}
