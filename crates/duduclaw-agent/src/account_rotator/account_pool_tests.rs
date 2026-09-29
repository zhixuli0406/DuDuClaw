// ── G1: agent.toml [model] account_pool → rotation candidate set ─────
//
// Before this, `account_pool` was serialized, editable in the dashboard, and
// read by nobody — a dead setting. These tests pin the three semantics that
// make it safe to turn on: it narrows, it never bricks, and an unset pool is a
// zero-behavior-change no-op.
use super::*;

fn oauth(id: &str, priority: u32) -> Account {
    Account {
        id: id.to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority,
        monthly_budget_cents: 0,
        tags: vec![],
        profile: "default".to_string(),
        email: String::new(),
        subscription: "max".to_string(),
        label: String::new(),
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

fn labeled(id: &str, priority: u32, label: &str) -> Account {
    let mut a = oauth(id, priority);
    a.label = label.to_string();
    a
}

fn pool(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|s| s.to_string()).collect()
}

// ── pure narrowing decision ──────────────────────────────────────

/// An unset / blank-only pool must not even be considered — that is what
/// makes "no pool configured" a byte-identical no-op.
#[test]
fn blank_pool_is_not_requested() {
    let a = oauth("a", 1);
    let avail = vec![&a];
    assert!(matches!(
        narrow_by_pool(&avail, &[]),
        PoolNarrowing::NotRequested
    ));
    assert!(matches!(
        narrow_by_pool(&avail, &pool(&["", "   "])),
        PoolNarrowing::NotRequested
    ));
}

#[test]
fn pool_narrows_by_id() {
    let (a, b, c) = (oauth("a", 1), oauth("b", 2), oauth("c", 3));
    let avail = vec![&a, &b, &c];
    match narrow_by_pool(&avail, &pool(&["b", "c"])) {
        PoolNarrowing::Applied(v) => {
            let ids: Vec<&str> = v.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(ids, vec!["b", "c"]);
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

/// Operators reference accounts by the label the dashboard shows them, not
/// only by the internal id — both must resolve. Labels are frequently CJK.
#[test]
fn pool_matches_label_including_cjk() {
    let a = labeled("acc-1785771258", 1, "工作帳號");
    let b = oauth("b", 2);
    let avail = vec![&a, &b];
    match narrow_by_pool(&avail, &pool(&["工作帳號"])) {
        PoolNarrowing::Applied(v) => {
            assert_eq!(v.len(), 1);
            assert_eq!(v[0].id, "acc-1785771258");
        }
        other => panic!("expected Applied, got {other:?}"),
    }
    // ASCII case folding applies to ids/labels too.
    assert!(matches!(
        narrow_by_pool(&avail, &pool(&["  B  "])),
        PoolNarrowing::Applied(_)
    ));
}

/// Project convention 2: no unanchored substring matching for routing.
/// A pool entry `main` must not capture `main-backup`.
#[test]
fn pool_match_is_exact_not_substring() {
    let backup = oauth("main-backup", 1);
    let avail = vec![&backup];
    assert!(matches!(
        narrow_by_pool(&avail, &pool(&["main"])),
        PoolNarrowing::FailedOpen
    ));
}

/// A pool naming only accounts that do not exist (renamed / deleted /
/// copied from a template) must fail OPEN, not empty the candidate set.
#[test]
fn stale_pool_fails_open() {
    let a = oauth("a", 1);
    let avail = vec![&a];
    assert!(matches!(
        narrow_by_pool(&avail, &pool(&["ghost", "another-ghost"])),
        PoolNarrowing::FailedOpen
    ));
}

// ── end-to-end selection ─────────────────────────────────────────

/// Priority strategy: the pool changes WHICH accounts compete, the
/// strategy still picks the lowest priority number among them.
#[tokio::test]
async fn priority_selects_best_within_pool() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth("a", 1)).await; // best globally
    rotator.push_account_for_test(oauth("b", 5)).await;
    rotator.push_account_for_test(oauth("c", 9)).await;

    let sel = rotator
        .select_with_pool(&pool(&["b", "c"]))
        .await
        .expect("pooled account must be selectable");
    assert_eq!(sel.id, "b", "lowest priority *within the pool*, not globally");
}

/// Failover shares Priority's ordering but is a distinct configured
/// strategy — pin it explicitly so a future divergence is caught.
#[tokio::test]
async fn failover_selects_within_pool() {
    let rotator = AccountRotator::new(RotationStrategy::Failover, 120);
    rotator.push_account_for_test(oauth("primary", 1)).await;
    rotator.push_account_for_test(oauth("secondary", 2)).await;

    let sel = rotator.select_with_pool(&pool(&["secondary"])).await.unwrap();
    assert_eq!(sel.id, "secondary");
}

/// RoundRobin must rotate *inside* the pool and never hand out a
/// non-pooled account while a pooled one is available.
#[tokio::test]
async fn round_robin_stays_within_pool() {
    let rotator = AccountRotator::new(RotationStrategy::RoundRobin, 120);
    rotator.push_account_for_test(oauth("a", 1)).await;
    rotator.push_account_for_test(oauth("b", 1)).await;
    rotator.push_account_for_test(oauth("c", 1)).await;

    let p = pool(&["a", "c"]);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..6 {
        let sel = rotator.select_with_pool(&p).await.unwrap();
        assert_ne!(sel.id, "b", "non-pooled account leaked into rotation");
        seen.insert(sel.id);
    }
    assert_eq!(seen.len(), 2, "both pooled accounts should be used: {seen:?}");
}

/// LeastCost prefers OAuth then least-spent; the pool must gate the field
/// it chooses from without changing that preference order.
#[tokio::test]
async fn least_cost_stays_within_pool() {
    let rotator = AccountRotator::new(RotationStrategy::LeastCost, 120);
    rotator.push_account_for_test(oauth("cheap", 1)).await;
    rotator.push_account_for_test(oauth("pooled", 9)).await;

    for _ in 0..3 {
        let sel = rotator.select_with_pool(&pool(&["pooled"])).await.unwrap();
        assert_eq!(sel.id, "pooled");
        rotator.on_success(&sel.id, 0).await;
    }
}

/// The load-bearing safety property: a pool that resolves to nothing must
/// still produce an account. A stale `account_pool` copied from a template
/// (`["main"]`) is the common real-world shape — it must degrade to the
/// full set, not to "no accounts available".
#[tokio::test]
async fn stale_pool_falls_back_to_full_account_set() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator
        .push_account_for_test(oauth("claude-oauth-1785771258", 1))
        .await;

    let sel = rotator
        .select_with_pool(&pool(&["main"]))
        .await
        .expect("a stale pool must never leave the agent with no account");
    assert_eq!(sel.id, "claude-oauth-1785771258");
}

/// Same fail-open rule when the pooled accounts EXIST but are all
/// unavailable (rate-limited / cooling down): availability wins, and the
/// non-pooled account answers instead of the reply failing.
#[tokio::test]
async fn exhausted_pool_falls_back_to_non_pooled_account() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth("pooled", 1)).await;
    rotator.push_account_for_test(oauth("spare", 2)).await;

    // Pool is honored while its member is healthy.
    assert_eq!(
        rotator.select_with_pool(&pool(&["pooled"])).await.unwrap().id,
        "pooled"
    );

    rotator.on_rate_limited("pooled").await;

    let sel = rotator
        .select_with_pool(&pool(&["pooled"]))
        .await
        .expect("fail-open must survive an exhausted pool");
    assert_eq!(sel.id, "spare");
}

/// Zero-behavior-change guarantee: an agent with no pool selects exactly
/// what the legacy `select()` selects.
#[tokio::test]
async fn empty_pool_is_identical_to_legacy_select() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(oauth("a", 1)).await;
    rotator.push_account_for_test(oauth("b", 2)).await;

    let legacy = rotator.select().await.unwrap();
    let pooled = rotator.select_with_pool(&[]).await.unwrap();
    assert_eq!(legacy.id, pooled.id);
    assert_eq!(legacy.env_vars, pooled.env_vars);
}

/// The pool is provider-scoped like every other rotator filter: an
/// anthropic pool must not reach into another provider's accounts, and a
/// provider with no configured accounts still gets its env-var fallback.
#[tokio::test]
async fn pool_does_not_cross_provider_boundaries() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let mut foreign = oauth("shared-name", 1);
    foreign.provider = "github".to_string();
    rotator.push_account_for_test(foreign).await;
    rotator.push_account_for_test(oauth("anthropic-1", 1)).await;

    // Pool names the github account, but we are selecting for anthropic:
    // it must NOT be reachable — fail-open hands back anthropic's own set.
    let sel = rotator.select_with_pool(&pool(&["shared-name"])).await.unwrap();
    assert_eq!(sel.id, "anthropic-1");
    assert_eq!(sel.provider, "anthropic");
}
