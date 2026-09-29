// ── WP10 (2026-08-04 field incident) regression tests ────────────────
use super::*;

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
        // An anthropic OAuth account is only "available" with a setup
        // token or an OS-keychain credentials dir — mirror the real
        // single-account install (keychain OAuth, no explicit token).
        oauth_token: None,
        credentials_dir: Some(PathBuf::from("/tmp/wp10-fake-credentials")),
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

/// The incident shape: ONE OAuth account. Three generic errors used to
/// mark it unhealthy with `cooldown_until = None`, and `is_available()`
/// only forgives an unhealthy account whose cooldown has EXPIRED — so a
/// `None` cooldown meant permanently unavailable, and every later message
/// died with "All accounts exhausted".
#[tokio::test]
async fn single_account_recovers_after_generic_errors() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth_account("oauth-default")).await;

    for _ in 0..3 {
        rotator.on_error("oauth-default").await;
    }

    // Unhealthy right now — that part is intended.
    assert!(
        rotator.select().await.is_none(),
        "3 consecutive errors should take the account out of rotation"
    );

    // ...but the outage must be BOUNDED. A cooldown has to exist, or the
    // account can never come back on its own.
    {
        let accounts = rotator.accounts.read().await;
        let acc = accounts.iter().find(|a| a.id == "oauth-default").unwrap();
        assert!(!acc.is_healthy);
        let cd = acc
            .cooldown_until
            .expect("on_error must attach a cooldown so recovery is automatic");
        assert!(cd > Utc::now(), "cooldown should be in the future");
    }

    // Simulate the cooldown elapsing: the account becomes available again
    // with no operator intervention and no gateway restart.
    {
        let mut accounts = rotator.accounts.write().await;
        let acc = accounts.iter_mut().find(|a| a.id == "oauth-default").unwrap();
        acc.cooldown_until = Some(Utc::now() - chrono::Duration::seconds(1));
    }
    assert!(
        rotator.select().await.is_some(),
        "an expired cooldown must return the sole account to rotation"
    );
}

/// WP10 M4 — the tier must follow the actual cooldown horizon, because
/// "a few minutes" and "up to 24 hours" are what the user plans around.
#[tokio::test]
async fn unavailable_reason_tiers_by_cooldown_length() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth_account("acc")).await;

    // Healthy ⇒ nothing to attribute.
    assert_eq!(
        rotator.unavailable_reason().await,
        UnavailableReason::Unknown
    );

    // Rate limit books `cooldown_seconds` (120 s) ⇒ short.
    rotator.on_rate_limited("acc").await;
    assert_eq!(
        rotator.unavailable_reason().await,
        UnavailableReason::ShortCooldown
    );

    // Billing books 24 h ⇒ long, and must win over the short window.
    rotator.on_billing_exhausted("acc").await;
    assert_eq!(
        rotator.unavailable_reason().await,
        UnavailableReason::LongCooldown
    );
}

/// Unhealthy with no cooldown at all is NOT attributable — the caller must
/// hedge rather than promise a horizon it cannot know.
#[tokio::test]
async fn unavailable_reason_is_unknown_without_a_cooldown() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let mut acc = oauth_account("acc");
    acc.is_healthy = false;
    rotator.push_account_for_test(acc).await;
    assert_eq!(
        rotator.unavailable_reason().await,
        UnavailableReason::Unknown
    );
}

/// A generic error must never shorten a longer billing cooldown.
#[tokio::test]
async fn on_error_never_shortens_an_existing_longer_cooldown() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth_account("acc")).await;

    rotator.on_billing_exhausted("acc").await; // 24 h
    let billing_until = {
        let accounts = rotator.accounts.read().await;
        accounts.iter().find(|a| a.id == "acc").unwrap().cooldown_until.unwrap()
    };

    for _ in 0..3 {
        rotator.on_error("acc").await; // 120 s — must not win
    }

    let accounts = rotator.accounts.read().await;
    let cd = accounts.iter().find(|a| a.id == "acc").unwrap().cooldown_until.unwrap();
    assert_eq!(
        cd, billing_until,
        "a 120s generic cooldown must not override the 24h billing cooldown"
    );
}
