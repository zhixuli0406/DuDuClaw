// ── 2026-10-22 field incident regression tests ───────────────────────
//
// A weekly-capped subscription answered every call with a quota refusal.
// The rotator kept selecting it (Priority, equal priorities) and a second,
// working account in the same pool was never tried.
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
        oauth_token: Some(format!("tok_{id}")),
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

async fn two_equal_priority_accounts() -> AccountRotator {
    // Probe target that refuses connections: a test that reaches the network
    // fails loudly instead of passing by accident.
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120)
        .with_probe_base_url("http://127.0.0.1:9");
    rotator.push_account_for_test(oauth_account("capped")).await;
    rotator.push_account_for_test(oauth_account("spare")).await;
    rotator
}

#[tokio::test]
async fn excluding_tried_accounts_reaches_the_second_account() {
    let rotator = two_equal_priority_accounts().await;
    let pool = vec!["capped".to_string(), "spare".to_string()];

    let first = rotator.select_with_pool_excluding(&pool, &[]).await.unwrap();
    assert_eq!(first.id, "capped", "Priority tie-break picks the first account");

    let second = rotator
        .select_with_pool_excluding(&pool, &[first.id.clone()])
        .await
        .unwrap();
    assert_eq!(second.id, "spare");

    assert!(
        rotator
            .select_with_pool_excluding(&pool, &[first.id, second.id])
            .await
            .is_none(),
        "every candidate tried ⇒ the retry loop must end"
    );
}

/// A tried-but-available pool member must not widen the pool: the retry
/// ends instead of spilling onto an account the agent was kept away from.
#[tokio::test]
async fn exhausting_the_pool_by_exclusion_does_not_leave_the_pool() {
    let rotator = two_equal_priority_accounts().await;
    let pool = vec!["capped".to_string()];
    assert_eq!(
        rotator.select_with_pool_excluding(&pool, &[]).await.unwrap().id,
        "capped"
    );
    assert!(
        rotator
            .select_with_pool_excluding(&pool, &["capped".to_string()])
            .await
            .is_none(),
        "the only pool member was tried — `spare` is outside the pool"
    );
}

#[tokio::test]
async fn rate_limited_account_is_skipped_until_its_cooldown_ends() {
    let rotator = two_equal_priority_accounts().await;
    rotator.on_rate_limited("capped").await;
    assert_eq!(rotator.select().await.unwrap().id, "spare");
}

/// The incident's third leg: generic-error strikes had left the capped
/// account unhealthy, the 60 s probe got `200` from `/v1/models` and put it
/// straight back. A quota refusal must leave it healthy-but-cooling, which
/// the probe never touches.
#[tokio::test]
async fn probe_does_not_restore_a_rate_limited_account() {
    let rotator = two_equal_priority_accounts().await;
    rotator.on_error("capped").await;
    rotator.on_error("capped").await;
    rotator.on_error("capped").await;
    rotator.on_rate_limited("capped").await;

    assert_eq!(
        rotator.probe_and_restore().await,
        0,
        "a cooling rate-limited account is not a probe candidate"
    );
    let accounts = rotator.accounts.read().await;
    let acc = accounts.iter().find(|a| a.id == "capped").unwrap();
    assert!(acc.is_healthy, "a quota refusal proves the credential works");
    assert_eq!(acc.consecutive_errors, 0);
    assert!(acc.cooldown_until.is_some_and(|cd| cd > Utc::now()));
    assert!(!acc.is_available());
}

#[tokio::test]
async fn rate_limit_never_shortens_a_longer_cooldown() {
    let rotator = two_equal_priority_accounts().await;
    rotator.on_billing_exhausted("capped").await;
    let billing_until = {
        let accounts = rotator.accounts.read().await;
        accounts.iter().find(|a| a.id == "capped").unwrap().cooldown_until.unwrap()
    };
    rotator.on_rate_limited("capped").await;
    let accounts = rotator.accounts.read().await;
    let acc = accounts.iter().find(|a| a.id == "capped").unwrap();
    assert_eq!(acc.cooldown_until, Some(billing_until));
}
