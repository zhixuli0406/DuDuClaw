//! Credential-hardening cases (probe cases), moved verbatim out of
//! `account_rotator.rs`.

use super::*;

// ── (a) auth-dead backoff ladder ────────────────────────────────

/// 15 min → 30 → 60 → 120 → 240 → capped at 360 (6 h) and never beyond.
#[test]
fn auth_dead_backoff_doubles_then_caps_at_six_hours() {
    let expect = [
        (1u32, 15i64),
        (2, 30),
        (3, 60),
        (4, 120),
        (5, 240),
        (6, 360),
        (7, 360),
        (50, 360),
        (u32::MAX, 360),
    ];
    for (strikes, minutes) in expect {
        assert_eq!(
            auth_dead_backoff(strikes).num_minutes(),
            minutes,
            "strike {strikes} should book {minutes} minutes"
        );
    }
    // Defensive: a zero strike count is treated as the first one, never as
    // "no cooldown at all".
    assert_eq!(auth_dead_backoff(0).num_minutes(), 15);
}

/// `on_auth_failed` walks that ladder on the live account, marks it
/// auth-dead, and takes it out of rotation immediately (no three-strike
/// grace period — a dead token does not become alive by being retried).
#[tokio::test]
async fn on_auth_failed_marks_dead_and_escalates_the_cooldown() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(token_account("acct")).await;

    assert!(rotator.select().await.is_some(), "healthy account selects");

    for (nth, expected_minutes) in [(1u32, 15i64), (2, 30), (3, 60), (4, 120)] {
        let before = Utc::now();
        rotator
            .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
            .await;
        let acc = snapshot(&rotator, "acct").await;
        assert!(!acc.is_healthy, "strike {nth}: account must go unhealthy");
        assert_eq!(acc.auth_dead_strikes, nth);
        assert_eq!(
            acc.credential_state,
            CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
        );
        let booked = (acc.cooldown_until.expect("cooldown") - before).num_minutes();
        assert!(
            (expected_minutes - 1..=expected_minutes).contains(&booked),
            "strike {nth}: expected ~{expected_minutes} min, got {booked}"
        );
        assert!(
            rotator.select().await.is_none(),
            "strike {nth}: an auth-dead account must not be selectable"
        );
    }
}

/// `on_success` is the reset: strikes back to zero, state back to `Ok`.
/// Without it the ladder would ratchet forever across unrelated incidents.
#[tokio::test]
async fn on_success_resets_the_auth_dead_ladder() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.push_account_for_test(token_account("acct")).await;

    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    assert_eq!(snapshot(&rotator, "acct").await.auth_dead_strikes, 2);

    // A success can only follow the cooldown elapsing (that is what puts
    // the account back in rotation), so simulate that first. `on_success`
    // deliberately does not clear `cooldown_until` itself — the
    // never-shorten rule predates this work and guards against a stale
    // success overriding a concurrent rate-limit.
    {
        let mut accounts = rotator.accounts.write().await;
        let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
        a.cooldown_until = Some(Utc::now() - chrono::Duration::seconds(1));
    }
    rotator.on_success("acct", 0).await;
    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.auth_dead_strikes, 0);
    assert_eq!(acc.credential_state, CredentialState::Ok);

    // The ladder restarts from the bottom, not from where it left off.
    let before = Utc::now();
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.auth_dead_strikes, 1);
    let booked = (acc.cooldown_until.expect("cooldown") - before).num_minutes();
    assert!(
        (14..=15).contains(&booked),
        "expected ~15 min, got {booked}"
    );
}

/// A `Broken` credential is never "successful", so a stray `on_success`
/// for its id must not launder it back into rotation.
#[tokio::test]
async fn on_success_never_un_breaks_a_broken_credential() {
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let mut acc = token_account("acct");
    acc.credential_state = CredentialState::Broken;
    acc.is_healthy = false;
    rotator.push_account_for_test(acc).await;

    rotator.on_success("acct", 0).await;
    assert_eq!(
        snapshot(&rotator, "acct").await.credential_state,
        CredentialState::Broken
    );
    assert!(rotator.select().await.is_none());
}

// ── (b) health probe drives real credential state ───────────────

/// A 200 from `/v1/models` is the ONLY thing that restores an auth-dead
/// account — and it does so completely (healthy, no cooldown, state `Ok`,
/// ladder reset).
#[tokio::test]
async fn probe_200_restores_an_auth_dead_token_account() {
    let (base, server) = spawn_repeating_server(RESP_200).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    assert!(rotator.select().await.is_none());

    assert_eq!(rotator.probe_and_restore().await, 1);

    let acc = snapshot(&rotator, "acct").await;
    assert!(acc.is_healthy);
    assert_eq!(acc.cooldown_until, None);
    assert_eq!(acc.credential_state, CredentialState::Ok);
    assert_eq!(acc.auth_dead_strikes, 0);
    assert!(
        rotator.select().await.is_some(),
        "a verified account is selectable again"
    );

    server.abort();
}

/// The incident itself: a 403 must keep the account dead and push the
/// cooldown OUT, not resurrect it. Same for a 401.
#[tokio::test]
async fn probe_401_and_403_keep_the_account_dead_and_double_the_cooldown() {
    for (response, expected_kind) in [
        (RESP_401, AuthFailureKind::InvalidToken),
        (RESP_403, AuthFailureKind::OrgDisabled),
    ] {
        let (base, server) = spawn_repeating_server(response).await;
        let rotator =
            AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
        rotator.push_account_for_test(token_account("acct")).await;
        rotator
            .on_auth_failed("acct", AuthFailureKind::InvalidToken)
            .await;
        let before = snapshot(&rotator, "acct")
            .await
            .cooldown_until
            .expect("cooldown");

        assert_eq!(
            rotator.probe_and_restore().await,
            0,
            "a rejected credential must never count as restored"
        );

        let acc = snapshot(&rotator, "acct").await;
        assert!(!acc.is_healthy);
        assert_eq!(
            acc.credential_state,
            CredentialState::AuthDead(expected_kind)
        );
        let after = acc.cooldown_until.expect("cooldown still booked");
        let grew = (after - before).num_minutes();
        assert!(
            grew >= 13,
            "cooldown should roughly double (15 → ~30 min); grew only {grew} min"
        );
        assert!(
            (after - Utc::now()).num_minutes() <= AUTH_DEAD_CAP_MINUTES,
            "cooldown must stay under the 6 h cap"
        );
        assert!(rotator.select().await.is_none());

        server.abort();
    }
}

/// Repeated 403s escalate but never blow past the 6 h ceiling.
#[tokio::test]
async fn repeated_probe_failures_saturate_at_the_six_hour_cap() {
    let (base, hits, server) = spawn_counting_server(RESP_403).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    let mut acc = token_account("acct");
    // Start already dead with a short *live* cooldown. It must stay in the
    // future: an expired cooldown makes `is_available()` true, and the
    // candidate filter skips available accounts — which is how this test
    // used to run zero probes and assert the cap vacuously. `doubled_cooldown`
    // doubles whatever remains, so the ladder still climbs to the cap
    // without a second of wall clock.
    acc.is_healthy = false;
    acc.credential_state = CredentialState::AuthDead(AuthFailureKind::OrgDisabled);
    acc.cooldown_until = Some(Utc::now() + chrono::Duration::minutes(1));
    rotator.push_account_for_test(acc).await;

    for _ in 0..12 {
        {
            // Clear only the probe schedule, so this stays a test about
            // *repeated* probe failures rather than silently becoming a
            // single-probe test under the new backoff.
            let mut accounts = rotator.accounts.write().await;
            let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
            a.next_probe_at = None;
        }
        rotator.probe_and_restore().await;
        let a = snapshot(&rotator, "acct").await;
        let minutes = (a.cooldown_until.expect("cooldown") - Utc::now()).num_minutes();
        assert!(
            minutes <= AUTH_DEAD_CAP_MINUTES,
            "cooldown {minutes} min exceeded the 6 h cap"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        12,
        "every round must actually have reached the probe — otherwise the \
             cap assertion above passes vacuously"
    );
    server.abort();
}

// ── (b2) probe schedule backoff ─────────────────────────────────
//
// A conclusively-dead credential used to be re-asked every 60 s forever.
// Free in dollars, but pointless traffic and one alarming log line a
// minute. These pin the widening schedule that replaced it.

/// 1 min → 2 → 4 → 8 → 16 → capped at 30 and never beyond.
#[test]
fn probe_backoff_doubles_then_caps_at_thirty_minutes() {
    let expect = [
        (1u32, 1i64),
        (2, 2),
        (3, 4),
        (4, 8),
        (5, 16),
        (6, 30),
        (7, 30),
        (50, 30),
        (u32::MAX, 30),
    ];
    for (failures, minutes) in expect {
        assert_eq!(
            probe_backoff(failures).num_minutes(),
            minutes,
            "failure {failures} should book {minutes} minutes"
        );
    }
    // Defensive: a zero count is treated as the first failure, never as
    // "probe again immediately".
    assert_eq!(probe_backoff(0).num_minutes(), 1);
    // The probe ceiling is deliberately far below the rotation ceiling —
    // a probe is free, so only the noise is being rationed.
    assert!(PROBE_BACKOFF_CAP_MINUTES < AUTH_DEAD_CAP_MINUTES);
}

/// (a) One 401 books a ~1-minute schedule, and the very next tick must not
/// reach the wire at all. The request counter is the assertion that
/// matters: account state alone cannot tell "asked and nothing changed"
/// apart from "never asked".
#[tokio::test]
async fn a_conclusive_probe_failure_suppresses_the_next_tick() {
    let (base, hits, server) = spawn_counting_server(RESP_401).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;

    let before = Utc::now();
    assert_eq!(rotator.probe_and_restore().await, 0);
    // Bracket the booking between the two clock reads instead of a fixed
    // 5-second slack: `next_probe_at` is stamped *inside* the probe, and a
    // loaded parallel test run has been observed to spend more than 5s on
    // the local HTTP round trip, which turned this into a flaky test.
    let after = Utc::now();
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the first tick probes");

    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.probe_failures, 1);
    let next = acc.next_probe_at.expect("probe schedule booked");
    let one_minute = chrono::Duration::seconds(60);
    assert!(
        next >= before + one_minute && next <= after + one_minute,
        "expected ~1 min until the next probe, got {}s after the call started",
        (next - before).num_seconds()
    );

    // Second tick, immediately — this is the once-a-minute loop the
    // schedule exists to break.
    assert_eq!(rotator.probe_and_restore().await, 0);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a scheduled account must not be re-probed before its time"
    );
    assert_eq!(
        snapshot(&rotator, "acct").await.probe_failures,
        1,
        "a skipped tick must not count as a failure"
    );

    server.abort();
}

/// (b) One integration step on the ladder: consecutive conclusive
/// failures book 1, then 2, then 4 minutes on the live account.
#[tokio::test]
async fn consecutive_conclusive_failures_walk_the_probe_ladder() {
    let (base, hits, server) = spawn_counting_server(RESP_403).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
        .await;

    for (nth, expected_minutes) in [(1u32, 1i64), (2, 2), (3, 4)] {
        {
            // Let the previous booking elapse without burning wall clock.
            let mut accounts = rotator.accounts.write().await;
            let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
            a.next_probe_at = None;
        }
        let before = Utc::now();
        assert_eq!(rotator.probe_and_restore().await, 0);
        // Same bracketing as the single-failure test above: the rung is
        // asserted against the call window, not a fixed 5-second slack.
        let after = Utc::now();
        let acc = snapshot(&rotator, "acct").await;
        assert_eq!(acc.probe_failures, nth);
        let next = acc.next_probe_at.expect("booked");
        let want = chrono::Duration::seconds(expected_minutes * 60);
        assert!(
            next >= before + want && next <= after + want,
            "failure {nth}: expected ~{expected_minutes} min, got {}s after the call started",
            (next - before).num_seconds()
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    server.abort();
}

/// (c) A 200 (the operator re-issued the token) clears the schedule
/// completely, so recovery is never held back by a stale backoff.
#[tokio::test]
async fn a_valid_probe_clears_the_probe_schedule() {
    // 401 first (books the backoff), 200 afterwards.
    let (base, hits, server) = spawn_sequenced_server(RESP_401, RESP_200).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;

    assert_eq!(rotator.probe_and_restore().await, 0);
    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.probe_failures, 1);
    assert!(acc.next_probe_at.is_some());

    {
        let mut accounts = rotator.accounts.write().await;
        let a = accounts.iter_mut().find(|a| a.id == "acct").unwrap();
        a.next_probe_at = Some(Utc::now() - chrono::Duration::seconds(1));
    }
    assert_eq!(rotator.probe_and_restore().await, 1);
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.probe_failures, 0);
    assert_eq!(acc.next_probe_at, None);
    assert_eq!(acc.credential_state, CredentialState::Ok);
    assert!(rotator.select().await.is_some());

    server.abort();
}

/// …and so does a completed request, which is even stronger evidence than
/// a probe.
#[tokio::test]
async fn on_success_clears_the_probe_schedule() {
    let (base, _hits, server) = spawn_counting_server(RESP_401).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    rotator.probe_and_restore().await;
    assert!(snapshot(&rotator, "acct").await.next_probe_at.is_some());

    rotator.on_success("acct", 0).await;
    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.probe_failures, 0);
    assert_eq!(acc.next_probe_at, None);

    server.abort();
}

/// (d) A real spawn failure clears the schedule so the next tick can
/// classify *that* failure — and must not itself bump the probe counter
/// (it is a spawn outcome, not a probe verdict).
#[tokio::test]
async fn on_auth_failed_reopens_the_probe_schedule() {
    let (base, hits, server) = spawn_counting_server(RESP_403).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
        .await;

    assert_eq!(rotator.probe_and_restore().await, 0);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(
        snapshot(&rotator, "acct")
            .await
            .next_probe_at
            .is_some_and(|t| t > Utc::now()),
        "a conclusive failure books a future probe"
    );

    rotator
        .on_auth_failed("acct", AuthFailureKind::OrgDisabled)
        .await;
    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(
        acc.next_probe_at, None,
        "a fresh spawn failure must let the next tick re-classify"
    );
    assert_eq!(
        acc.probe_failures, 1,
        "on_auth_failed counts spawn failures, not probe verdicts"
    );

    assert_eq!(rotator.probe_and_restore().await, 0);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "the next tick must actually probe again"
    );
    assert_eq!(snapshot(&rotator, "acct").await.probe_failures, 2);

    server.abort();
}

/// (e) An inconclusive answer (500) must not slow the probe down: an API
/// outage saying nothing about the credential is the mirror image of the
/// original bug, and delaying recovery on it would be a real cost.
#[tokio::test]
async fn an_inconclusive_probe_schedules_no_backoff() {
    let (base, hits, server) = spawn_counting_server(RESP_500).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;
    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;

    assert_eq!(rotator.probe_and_restore().await, 0);
    let acc = snapshot(&rotator, "acct").await;
    assert_eq!(acc.next_probe_at, None);
    assert_eq!(acc.probe_failures, 0);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // …so the very next tick probes again, exactly as before this change.
    assert_eq!(rotator.probe_and_restore().await, 0);
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    server.abort();
}

/// `status()` (and therefore `accounts.list`) carries the schedule, so an
/// operator can see a dead token is being re-checked on a backoff rather
/// than silently forgotten.
#[tokio::test]
async fn status_surfaces_the_probe_schedule() {
    let (base, _hits, server) = spawn_counting_server(RESP_401).await;
    let rotator =
        AccountRotator::new(RotationStrategy::Priority, 120).with_probe_base_url(&base);
    rotator.push_account_for_test(token_account("acct")).await;

    // Never probed → nothing scheduled.
    let row = rotator.status().await.remove(0);
    assert_eq!(row.next_probe_at, None);
    assert_eq!(row.probe_failures, 0);

    rotator
        .on_auth_failed("acct", AuthFailureKind::InvalidToken)
        .await;
    rotator.probe_and_restore().await;

    let row = rotator.status().await.remove(0);
    assert_eq!(row.probe_failures, 1);
    let stamp = row.next_probe_at.expect("schedule surfaced");
    let parsed = stamp
        .parse::<DateTime<Utc>>()
        .expect("next_probe_at must be RFC 3339");
    assert!(parsed > Utc::now());

    server.abort();
}
