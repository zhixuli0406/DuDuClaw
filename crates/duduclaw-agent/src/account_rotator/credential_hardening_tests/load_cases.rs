//! Credential-hardening cases (load cases), moved verbatim out of
//! `account_rotator.rs`.

use super::*;

/// A 500 (or any inconclusive answer) says nothing about the credential:
/// account state must be byte-identical afterwards. Reading a server
/// outage as a dead token would be the mirror image of the original bug.
#[tokio::test]
async fn probe_500_leaves_the_account_untouched() {
    let (base, server) = spawn_repeating_server(RESP_500).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
        .await;
    let before = snapshot(&rotator, "acct").await;

    assert_eq!(rotator.probe_and_restore().await, 0);

    let after = snapshot(&rotator, "acct").await;
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.credential_state, before.credential_state);
    assert_eq!(after.auth_dead_strikes, before.auth_dead_strikes);
    assert_eq!(after.is_healthy, before.is_healthy);

    server.abort();
}

/// An unreachable API (transport error → `Unknown`) is inconclusive too.
#[tokio::test]
async fn probe_transport_failure_leaves_the_account_untouched() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let rotator = AccountRotator::new(RotationStrategy::Priority, 120)
        .with_probe_base_url(format!("http://{addr}"));
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    let before = snapshot(&rotator, "acct").await;

    assert_eq!(rotator.probe_and_restore().await, 0);

    let after = snapshot(&rotator, "acct").await;
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.credential_state, before.credential_state);
}

/// An API-key account is probed with its own key (`x-api-key`), same
/// three-way verdict.
#[tokio::test]
async fn api_key_account_is_probed_with_its_own_key() {
    let (base, server) = spawn_repeating_server(RESP_200).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    let mut acc = token_account("api-acct");
    acc.auth_method = AuthMethod::ApiKey;
    acc.oauth_token = None;
    acc.api_key = "sk-ant-api03-test".to_string();
    acc.monthly_budget_cents = 5000;
    rotator.push_account_for_test(acc).await;
    rotator
        .on_auth_failed("api-acct", AuthFailureKind::InvalidToken)
        .await;

    assert_eq!(rotator.probe_and_restore().await, 1);
    assert_eq!(
        snapshot(&rotator, "api-acct").await.credential_state,
        CredentialState::Ok
    );

    server.abort();
}

/// A foreign-provider seat's credential means nothing to
/// `api.anthropic.com` — probing it there would produce a confident, wrong
/// verdict, so it must not be probe-able at all.
#[test]
fn non_anthropic_seats_are_never_probed_against_anthropic() {
    let mut seat = token_account("copilot-seat");
    seat.provider = "github".to_string();
    assert!(probe_secret_for(&seat).is_none());

    let mut key = token_account("openai-key");
    key.provider = "openai".to_string();
    key.auth_method = AuthMethod::ApiKey;
    key.api_key = "sk-openai".to_string();
    assert!(probe_secret_for(&key).is_none());

    // …while the Anthropic equivalents are.
    assert!(matches!(
        probe_secret_for(&token_account("anth")),
        Some((CredentialKind::OAuthToken, _))
    ));
    let mut anth_key = token_account("anth-key");
    anth_key.auth_method = AuthMethod::ApiKey;
    anth_key.oauth_token = None;
    anth_key.api_key = "sk-ant-api03".to_string();
    assert!(matches!(
        probe_secret_for(&anth_key),
        Some((CredentialKind::ApiKey, _))
    ));
    // An empty credential is not probe-able (and never was usable).
    let mut empty = token_account("empty");
    empty.oauth_token = Some("   ".to_string());
    assert!(probe_secret_for(&empty).is_none());
}

// ── (c) the weak `claude auth status` signal ────────────────────

/// The heart of the incident: `loggedIn: true` must never resurrect an
/// account we have watched fail authentication.
#[test]
fn claude_auth_status_may_not_restore_auth_dead_or_broken() {
    assert!(legacy_status_probe_may_restore(CredentialState::Unverified));
    assert!(legacy_status_probe_may_restore(CredentialState::Ok));
    assert!(!legacy_status_probe_may_restore(CredentialState::Broken));
    assert!(!legacy_status_probe_may_restore(CredentialState::AuthDead(
        AuthFailureKind::InvalidToken
    )));
    assert!(!legacy_status_probe_may_restore(CredentialState::AuthDead(
        AuthFailureKind::OrgDisabled
    )));
}

/// End-to-end version: a keychain account (no probe-able secret) that has
/// been marked auth-dead stays dead across a probe tick, even on a machine
/// where `claude auth status` happily reports `loggedIn: true`.
#[tokio::test]
async fn auth_dead_keychain_account_is_not_restored_by_the_status_probe() {
    // Unroutable probe base: if this account were ever routed to the
    // credential probe (it must not be — it has no secret), the test would
    // notice via a changed state rather than a silent pass.
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120)
        .with_probe_base_url("http://127.0.0.1:1");
    rotator
        .push_account_for_test(keychain_account("keychain"))
        .await;
    rotator
        .on_auth_failed("keychain", AuthFailureKind::OrgDisabled)
        .await;
    let before = snapshot(&rotator, "keychain").await;

    assert_eq!(
        rotator.probe_and_restore().await,
        0,
        "`claude auth status` must not resurrect an auth-dead account"
    );

    let after = snapshot(&rotator, "keychain").await;
    assert!(!after.is_healthy);
    assert_eq!(
        after.credential_state,
        CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
    );
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert!(rotator.select().await.is_none());
}

/// An `oauth_token_enc` that cannot be decrypted (wrong / regenerated
/// `.keyfile`) must load as BROKEN and never be selected — instead of
/// quietly spawning children with no credential at all. An OAuth entry
/// with no `_enc` field at all is untouched (it relies on the OS keychain).
#[tokio::test]
async fn undecryptable_oauth_token_loads_as_broken_and_is_never_selected() {
    let home = temp_home("broken-oauth");
    // A real keyfile exists — the ciphertext is simply not ours.
    std::fs::write(
        home.path().join(".keyfile"),
        duduclaw_security::crypto::CryptoEngine::generate_key().unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[[accounts]]
id = "broken-oauth"
type = "oauth"
label = "壞掉的帳號"
oauth_token_enc = "this-is-not-valid-ciphertext"

[[accounts]]
id = "keychain-oauth"
type = "oauth"
profile = "default"
label = "鑰匙圈帳號"
"#,
    )
    .unwrap();

    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.load_from_config(home.path()).await.unwrap();

    let broken = snapshot(&rotator, "broken-oauth").await;
    assert_eq!(broken.credential_state, CredentialState::Broken);
    assert!(!broken.is_healthy, "a broken credential is not healthy");
    assert!(
        !broken.is_available(),
        "a broken credential is never available"
    );
    assert!(
        broken.oauth_token.is_none(),
        "an unusable token must not reach the spawn env"
    );
    assert!(broken.credential_state.credential_detail().is_some());

    // The no-`_enc` sibling keeps its previous behavior exactly.
    let keychain = snapshot(&rotator, "keychain-oauth").await;
    assert_eq!(
        keychain.credential_state,
        CredentialState::Unverified,
        "an OAuth entry with no `_enc` field must NOT be judged broken"
    );

    // Whatever else is selectable, it is never the broken account.
    for _ in 0..5 {
        if let Some(sel) = rotator.select().await {
            assert_ne!(sel.id, "broken-oauth");
        }
    }
}

/// Same rule on the API-key side: an `api_key_enc` that resolves to
/// nothing loads as BROKEN (previously the row was silently skipped, so a
/// wrong `.keyfile` just made the pool quietly smaller).
#[tokio::test]
async fn undecryptable_api_key_loads_as_broken() {
    let home = temp_home("broken-apikey");
    std::fs::write(
        home.path().join(".keyfile"),
        duduclaw_security::crypto::CryptoEngine::generate_key().unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[[accounts]]
id = "broken-key"
type = "api_key"
provider = "anthropic"
api_key_enc = "not-real-ciphertext"
"#,
    )
    .unwrap();

    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.load_from_config(home.path()).await.unwrap();

    let acc = snapshot(&rotator, "broken-key").await;
    assert_eq!(acc.credential_state, CredentialState::Broken);
    assert!(!acc.is_healthy);
    assert!(!acc.is_available());
}

/// The precondition helper: only a *declared, non-empty* encrypted field
/// makes an entry a broken-credential candidate.
#[test]
fn has_nonempty_field_requires_a_real_declaration() {
    let with_enc: toml::Table = "oauth_token_enc = \"abc\"\n".parse().unwrap();
    assert!(has_nonempty_field(&with_enc, OAUTH_TOKEN_ENC_FIELDS));

    let blank: toml::Table = "oauth_token_enc = \"   \"\n".parse().unwrap();
    assert!(!has_nonempty_field(&blank, OAUTH_TOKEN_ENC_FIELDS));

    let absent: toml::Table = "profile = \"default\"\n".parse().unwrap();
    assert!(!has_nonempty_field(&absent, OAUTH_TOKEN_ENC_FIELDS));

    // Either api-key field name counts.
    let alt: toml::Table = "anthropic_api_key_enc = \"abc\"\n".parse().unwrap();
    assert!(has_nonempty_field(&alt, API_KEY_ENC_FIELDS));
    assert!(!has_nonempty_field(&alt, OAUTH_TOKEN_ENC_FIELDS));
}

// ── (e) Broken is a hard exclusion under every strategy ─────────

#[tokio::test]
async fn broken_accounts_are_excluded_under_priority_and_round_robin() {
    for strategy in [RotationStrategy::Priority, RotationStrategy::RoundRobin] {
        let rotator = AccountRotator::new(strategy, 120);
        let mut broken = token_account("broken");
        broken.priority = 1; // would win on Priority
        broken.credential_state = CredentialState::Broken;
        // Deliberately left `is_healthy = true`: the exclusion must come
        // from the credential state alone, not from a health side effect.
        rotator.push_account_for_test(broken).await;
        let mut good = token_account("good");
        good.priority = 9;
        rotator.push_account_for_test(good).await;

        for _ in 0..6 {
            let sel = rotator
                .select()
                .await
                .expect("the healthy account must answer");
            assert_eq!(sel.id, "good", "a Broken account leaked into rotation");
        }
    }
}

/// An agent whose `account_pool` names ONLY the broken account must still
/// not get it — the pool's fail-open rule widens the candidate set, it
/// never re-admits a hard-excluded account.
#[tokio::test]
async fn broken_account_is_not_reachable_through_the_account_pool() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let mut broken = token_account("broken");
    broken.credential_state = CredentialState::Broken;
    rotator.push_account_for_test(broken).await;
    rotator.push_account_for_test(token_account("good")).await;

    let sel = rotator
        .select_with_pool(&["broken".to_string()])
        .await
        .expect("fail-open must still hand back a usable account");
    assert_eq!(sel.id, "good");
}

/// With nothing but a broken account configured, selection returns None
/// rather than falling through to the ambient `ANTHROPIC_API_KEY`.
#[tokio::test]
async fn only_broken_accounts_means_no_selection() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let mut broken = token_account("broken");
    broken.credential_state = CredentialState::Broken;
    rotator.push_account_for_test(broken).await;
    assert!(rotator.select().await.is_none());
}

// ── (D5) the shapes the dashboard / gateway wave codes against ──

#[test]
fn credential_state_display_and_serialization_are_stable() {
    assert_eq!(CredentialState::Ok.to_string(), "ok");
    assert_eq!(CredentialState::Unverified.to_string(), "unverified");
    assert_eq!(CredentialState::Broken.to_string(), "broken");
    assert_eq!(
        CredentialState::AuthDead(AuthFailureKind::InvalidToken).to_string(),
        "auth_dead:invalid_token"
    );
    assert_eq!(
        CredentialState::AuthDead(AuthFailureKind::OrgDisabled).to_string(),
        "auth_dead:org_disabled"
    );

    let json = |s: CredentialState| serde_json::to_string(&s).unwrap();
    assert_eq!(json(CredentialState::Ok), "\"ok\"");
    assert_eq!(json(CredentialState::Unverified), "\"unverified\"");
    assert_eq!(json(CredentialState::Broken), "\"broken\"");
    assert_eq!(
        json(CredentialState::AuthDead(AuthFailureKind::OrgDisabled)),
        "\"auth_dead\""
    );
    assert_eq!(
        serde_json::to_string(&AuthFailureKind::OrgDisabled).unwrap(),
        "\"org_disabled\""
    );
    assert_eq!(
        serde_json::to_string(&AuthFailureKind::InvalidToken).unwrap(),
        "\"invalid_token\""
    );

    // `Unverified` is the honest default — a credential we have not seen
    // work is neither Ok nor dead.
    assert_eq!(CredentialState::default(), CredentialState::Unverified);
    assert!(CredentialState::Broken.is_blocking());
    assert!(!CredentialState::AuthDead(AuthFailureKind::OrgDisabled).is_blocking());
}

/// `accounts.list` (via `status()`) must carry the new fields, or the
/// dashboard badge has nothing to render.
#[tokio::test]
async fn status_surfaces_credential_state_and_detail() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
        .await;

    let rows = rotator.status().await;
    let row = rows.iter().find(|r| r.id == "acct").expect("row");
    assert_eq!(
        row.credential_state,
        CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
    );
    assert_eq!(row.auth_dead_strikes, 1);
    let detail = row
        .credential_detail
        .expect("a bad state must explain itself");
    assert!(
        detail.contains("403"),
        "detail should name the failure: {detail}"
    );
    assert!(!row.is_available);

    // Serialized shape (what the gateway forwards to the dashboard).
    let json = serde_json::to_value(row).unwrap();
    assert_eq!(json["credential_state"], serde_json::json!("auth_dead"));
    assert_eq!(json["auth_dead_strikes"], serde_json::json!(1));
}

/// `doubled_cooldown` never returns something shorter than the base and
/// never exceeds the cap — including from an absent / already-expired
/// cooldown, where "double nothing" would mean "retry immediately".
#[test]
fn doubled_cooldown_restarts_at_base_and_respects_the_cap() {
    let now = Utc::now();

    let from_none = (doubled_cooldown(None) - now).num_minutes();
    assert!((14..=15).contains(&from_none), "got {from_none}");

    let expired =
        (doubled_cooldown(Some(now - chrono::Duration::hours(3))) - now).num_minutes();
    assert!((14..=15).contains(&expired), "got {expired}");

    let doubled =
        (doubled_cooldown(Some(now + chrono::Duration::minutes(30))) - now).num_minutes();
    assert!((59..=60).contains(&doubled), "got {doubled}");

    let capped = (doubled_cooldown(Some(now + chrono::Duration::hours(5))) - now).num_minutes();
    assert_eq!(capped, AUTH_DEAD_CAP_MINUTES);
}
