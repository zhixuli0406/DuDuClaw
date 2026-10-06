use super::*;

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use duduclaw_agent::account_rotator::{Account, AccountRotator, AuthMethod, RotationStrategy};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Build a synthetic OAuth account for testing.
    ///
    /// Sets `credentials_dir` to a fake path so `is_available()` returns true
    /// without needing real keychain state.
    fn fake_oauth_account(id: &str, priority: u32) -> Account {
        Account {
            id: id.to_string(),
            auth_method: AuthMethod::OAuth,
            provider: "anthropic".to_string(),
            priority,
            monthly_budget_cents: 0,
            tags: vec![],
            profile: "test".to_string(),
            email: format!("{id}@example.com"),
            subscription: "pro".to_string(),
            label: id.to_string(),
            expires_at: None,
            api_key: String::new(),
            oauth_token: Some(format!("tok_{id}")),
            credentials_dir: Some(PathBuf::from(format!("/tmp/fake/{id}"))),
            is_healthy: true,
            consecutive_errors: 0,
            spent_this_month: 0,
            cooldown_until: None,
            last_used: None,
            total_requests: 0,
            // §D5: a synthetic fixture has never been exercised, so the
            // honest state is `Unverified` (the enum's own default) with a
            // clean strike count — anything else would pre-bias selection.
            credential_state: Default::default(),
            auth_dead_strikes: 0,
            next_probe_at: None,
            probe_failures: 0,
        }
    }

    /// Scenario: first account rate-limited, second succeeds.
    ///
    /// Verifies:
    /// 1. rotate_cli_spawn advances to the second account after a rate-limit error
    /// 2. first account is placed in cooldown via on_rate_limited
    /// 3. successful result is returned from the second account
    #[tokio::test]
    async fn rotation_advances_past_rate_limited_account() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        // Lower priority number = selected first under Priority strategy.
        rotator
            .push_account_for_test(fake_oauth_account("first", 1))
            .await;
        rotator
            .push_account_for_test(fake_oauth_account("second", 2))
            .await;
        assert_eq!(rotator.count().await, 2);

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_cloned = call_count.clone();

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            move |env_vars, retry_hint| {
                let n = call_count_cloned.fetch_add(1, Ordering::SeqCst);
                // First attempt: simulate rate limit.
                // Second attempt: return success with a distinctive body.
                async move {
                    // Sanity: env_vars should contain OAuth token for the selected account.
                    assert!(env_vars.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
                    // Rate limit is an infra failure — retry must NOT get a hint
                    // (prompt stays byte-identical, cache preserved).
                    assert!(retry_hint.is_none(), "no hint expected after rate limit");
                    if n == 0 {
                        Err("Error 429 rate limit reached".to_string())
                    } else {
                        Ok("hello from second".to_string())
                    }
                }
            },
            100,
        )
        .await;

        assert_eq!(result.as_deref(), Ok("hello from second"));
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            2,
            "both accounts should be tried"
        );

        // First account should now be unavailable (cooldown), second still healthy.
        let statuses = rotator.status().await;
        let first = statuses.iter().find(|s| s.id == "first").unwrap();
        let second = statuses.iter().find(|s| s.id == "second").unwrap();
        assert!(
            !first.is_available,
            "first account should be in cooldown after rate-limit"
        );
        assert!(
            second.is_available,
            "second account should remain available"
        );
        assert_eq!(
            second.total_requests, 1,
            "second account should have one success recorded"
        );
    }

    /// Scenario: summarized-failure retry (arXiv:2605.08563).
    ///
    /// First attempt hits a model-behavior failure (hard timeout); the retry
    /// on the second account must receive a deterministic one-line hint so it
    /// doesn't silently re-run the identical prompt into the identical failure.
    #[tokio::test]
    async fn retry_after_timeout_carries_failure_summary() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("first", 1))
            .await;
        rotator
            .push_account_for_test(fake_oauth_account("second", 2))
            .await;

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_cloned = call_count.clone();

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            move |_env_vars, retry_hint| {
                let n = call_count_cloned.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        assert!(retry_hint.is_none(), "first attempt must have no hint");
                        Err("claude CLI hard timeout (1800s, no output)".to_string())
                    } else {
                        let hint = retry_hint.expect("retry after timeout must carry a hint");
                        assert!(
                            hint.contains("timed out"),
                            "hint should describe the failure: {hint}"
                        );
                        Ok("recovered".to_string())
                    }
                }
            },
            100,
        )
        .await;

        assert_eq!(result.as_deref(), Ok("recovered"));
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    /// Wiki/memory injection dedup: facts already present in the prompt
    /// (e.g. via an injected wiki page) are dropped; wiki wins.
    #[test]
    fn facts_already_in_wiki_section_are_deduped() {
        let prompt = "## Wiki Knowledge\n### Wiki — Core\n\n阿明住在台北，喜歡  黑咖啡。\nDeploys go through CI only.\n";
        let facts = vec![
            "阿明住在台北，喜歡 黑咖啡。".to_string(), // whitespace-variant duplicate → dropped
            "阿明的生日是三月".to_string(),            // novel → kept
            "deploys go through ci only.".to_string(), // case-variant duplicate → dropped
            "ok".to_string(),                          // too short to trust containment → kept
        ];
        let kept = filter_facts_not_in_prompt(&facts, prompt);
        assert_eq!(kept, vec!["阿明的生日是三月".to_string(), "ok".to_string()]);
    }

    #[test]
    fn normalize_for_dedup_collapses_whitespace_and_case() {
        assert_eq!(
            normalize_for_dedup("  Hello\n\tWORLD  台北 "),
            "hello world 台北"
        );
    }

    /// retry_hint_for: model-behavior failures get hints, infra failures don't.
    #[test]
    fn retry_hint_only_for_model_behavior_failures() {
        assert!(retry_hint_for("claude CLI hard timeout (1800s, no output)").is_some());
        assert!(retry_hint_for("claude CLI empty response").is_some());
        assert!(retry_hint_for("Error 429 rate limit reached").is_none());
        assert!(retry_hint_for("HTTP 402 insufficient_quota credit balance").is_none());
        assert!(retry_hint_for("Not logged in · Please run /login").is_none());
        assert!(retry_hint_for("claude CLI not found in PATH").is_none());
    }

    /// Scenario: both accounts fail with the same error.
    ///
    /// Verifies:
    /// 1. Both accounts are exercised
    /// 2. Final Err carries the last underlying error string (not a generic message)
    /// 3. The error is classifiable (so the fallback message will be specific)
    #[tokio::test]
    async fn rotation_all_fail_propagates_last_error() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("a", 1))
            .await;
        rotator
            .push_account_for_test(fake_oauth_account("b", 2))
            .await;

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move {
                Err::<String, _>("claude CLI hard timeout (1800s, no output)".to_string())
            },
            100,
        )
        .await;

        let err = result.expect_err("should fail when all accounts fail");
        assert!(
            err.contains("All accounts exhausted"),
            "expected aggregator prefix, got: {err}"
        );
        assert!(
            err.contains("hard timeout"),
            "expected last error to be propagated, got: {err}"
        );

        // Extracted error must still be classifiable as Timeout (not Unknown).
        assert_eq!(classify_cli_failure(&err), FailureReason::Timeout);
    }

    /// Scenario: billing-exhausted error places the account on a 24h cooldown.
    #[tokio::test]
    async fn rotation_billing_error_triggers_long_cooldown() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("broke", 1))
            .await;

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move {
                Err::<String, _>("HTTP 402 insufficient_quota credit balance".to_string())
            },
            100,
        )
        .await;

        assert!(result.is_err());
        let statuses = rotator.status().await;
        let broke = &statuses[0];
        assert!(
            !broke.is_healthy,
            "billing-exhausted account should be marked unhealthy"
        );
        assert!(
            !broke.is_available,
            "should be unavailable during 24h cooldown"
        );
    }

    /// WP10 (2026-08-04 field incident) — the exhaustion chain.
    ///
    /// A single OAuth account shared with the operator's own Claude Code
    /// session made the PTY transport wedge. The wedge was booked against the
    /// ACCOUNT (`on_error`), so three of them took the only account out of
    /// rotation and every later message died with "All accounts exhausted".
    /// A wedged PTY transport says nothing about the account's health — the
    /// same account answers fine over a plain subprocess spawn.
    ///
    /// The incident's own trigger (a stalled interactive REPL) can no longer
    /// happen — the PTY session pool was removed in 2026-09 — but the one-shot
    /// PTY path the Grok runtime uses still produces `PtyError::ReadTimeout`,
    /// so the carve-out stays load-bearing.
    #[tokio::test]
    async fn pty_read_timeout_is_not_charged_to_account_health() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("oauth-default", 1))
            .await;

        for _ in 0..5 {
            let result = rotate_cli_spawn(
                &rotator,
                &[],
                |_env_vars, _retry_hint| async move {
                    Err::<String, _>(
                        duduclaw_cli_runtime::PtyError::ReadTimeout(
                            std::time::Duration::from_secs(120),
                        )
                        .to_string(),
                    )
                },
                100,
            )
            .await;
            assert!(result.is_err());
        }

        let statuses = rotator.status().await;
        let acc = &statuses[0];
        assert!(
            acc.is_healthy,
            "5 PTY read timeouts must NOT mark the sole OAuth account unhealthy"
        );
        assert!(
            acc.is_available,
            "the account must stay selectable so the plain-spawn path can use it"
        );
    }

    /// Genuine account-level failures must still cool the account down —
    /// the WP10 carve-out is narrow, not a blanket amnesty.
    #[tokio::test]
    async fn non_transport_errors_still_mark_account_unhealthy() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("flaky", 1))
            .await;

        for _ in 0..3 {
            let _ = rotate_cli_spawn(
                &rotator,
                &[],
                |_env_vars, _retry_hint| async move {
                    Err::<String, _>("claude CLI spawn error: exit 1".to_string())
                },
                100,
            )
            .await;
        }

        let statuses = rotator.status().await;
        assert!(
            !statuses[0].is_healthy,
            "repeated genuine CLI failures must still take the account out of rotation"
        );
    }

    /// D2 (2026-09-08 incident) — an org-rejected token dies on the FIRST
    /// failure, not the third.
    ///
    /// The pre-fix path was `on_error`: three strikes, then a 2-minute
    /// cooldown, then back into rotation. Anthropic answered
    /// `oauth_org_not_allowed` for 18 hours, so the account was resurrected
    /// every couple of minutes and every scheduled dispatch burned one more
    /// spawn on it. Now a single auth failure books
    /// `AuthDead(OrgDisabled)` with at least the 15-minute base backoff.
    #[tokio::test]
    async fn auth_failure_marks_account_auth_dead_on_the_first_strike() {
        use duduclaw_agent::account_rotator::{AuthFailureKind, CredentialState};

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("org-blocked", 1))
            .await;

        let before = chrono::Utc::now();
        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move {
                Err::<String, _>("claude CLI assistant error: oauth_org_not_allowed".to_string())
            },
            100,
        )
        .await;
        assert!(result.is_err());

        let statuses = rotator.status().await;
        let acc = &statuses[0];
        assert_eq!(
            acc.credential_state,
            CredentialState::AuthDead(AuthFailureKind::OrgDisabled),
            "an org rejection must be recorded as such, not as a generic error"
        );
        assert_eq!(
            acc.auth_dead_strikes, 1,
            "one failure is enough — waiting for three is what burned 18 hours of spawns"
        );
        assert!(!acc.is_healthy && !acc.is_available);

        // …and the cooldown must be the auth-dead ladder's 15-minute base, not
        // the rotator's 2-minute generic-error cooldown.
        let cooled_at_least_15_min = rotator
            .cooldown_until_for_test("org-blocked")
            .await
            .is_some_and(|until| until >= before + chrono::Duration::minutes(15));
        assert!(
            cooled_at_least_15_min,
            "auth-dead cooldown must be >= 15 min (got the generic 2-min cooldown?)"
        );
    }

    /// The counterpart: a *token* rejection is booked as `InvalidToken`, so the
    /// dashboard tells the operator to re-run `claude setup-token` rather than
    /// to go argue with their org admin.
    #[tokio::test]
    async fn invalid_token_failure_is_distinguished_from_an_org_rejection() {
        use duduclaw_agent::account_rotator::{AuthFailureKind, CredentialState};

        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("stale-token", 1))
            .await;

        let _ = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move {
                Err::<String, _>("claude CLI stream error: OAuth access token is invalid".into())
            },
            100,
        )
        .await;

        assert_eq!(
            rotator.status().await[0].credential_state,
            CredentialState::AuthDead(AuthFailureKind::InvalidToken)
        );
    }

    /// WP10 — "no account currently available" must not masquerade as an
    /// empty last error. Before the fix this produced
    /// `All accounts exhausted. Last error: ` (empty tail), which classified
    /// as `Unknown` and told the user to go read debug.log.
    #[tokio::test]
    async fn no_available_account_reports_a_classifiable_reason() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        let mut acc = fake_oauth_account("cooling", 1);
        acc.is_healthy = false;
        rotator.push_account_for_test(acc).await;

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move { Ok::<String, String>("unreachable".into()) },
            100,
        )
        .await;

        let err = result.expect_err("no selectable account ⇒ error");
        // Unhealthy with NO cooldown attached ⇒ not attributable ⇒ hedge.
        assert_eq!(
            classify_cli_failure(&err),
            FailureReason::AccountsCoolingDownUnknown,
            "expected a cooling-down classification, got err: {err}"
        );
        // And the zh-TW surface must explain the wait, not only "go set up an
        // account" — the user HAS an account.
        let msg = format_fallback_message(
            "小助手",
            FailureReason::AccountsCoolingDownUnknown,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(
            msg.contains("冷卻") || msg.contains("恢復"),
            "message should explain the wait: {msg}"
        );
    }

    /// WP10 M4 — a billing-exhausted account is a 24 h wait; saying "a few
    /// minutes" would be a lie the user notices.
    #[tokio::test]
    async fn billing_cooldown_reports_the_long_horizon() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("broke", 1))
            .await;
        rotator.on_billing_exhausted("broke").await; // 24 h

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move { Ok::<String, String>("unreachable".into()) },
            100,
        )
        .await;

        let err = result.expect_err("billing-cooled account ⇒ error");
        assert_eq!(
            classify_cli_failure(&err),
            FailureReason::AccountsCoolingDownLong
        );
        let msg = format_fallback_message(
            "小助手",
            FailureReason::AccountsCoolingDownLong,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(msg.contains("24"), "long horizon must be stated: {msg}");
        assert!(!msg.contains("幾分鐘"), "must not promise minutes: {msg}");
    }

    /// A rate-limit cooldown is minutes, and must NOT borrow the 24 h wording.
    #[tokio::test]
    async fn rate_limit_cooldown_reports_the_short_horizon() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("busy", 1))
            .await;
        rotator.on_rate_limited("busy").await; // 120 s

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move { Ok::<String, String>("unreachable".into()) },
            100,
        )
        .await;

        let err = result.expect_err("rate-limited account ⇒ error");
        assert_eq!(
            classify_cli_failure(&err),
            FailureReason::AccountsCoolingDownShort
        );
        let msg = format_fallback_message(
            "小助手",
            FailureReason::AccountsCoolingDownShort,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(
            msg.contains("幾分鐘"),
            "short horizon must be stated: {msg}"
        );
        assert!(
            !msg.contains("24"),
            "must not threaten 24h for a 2min wait: {msg}"
        );
    }

    /// M2 regression: a PTY-layer failure (here `PtyError::Closed`, which
    /// renders as "PTY closed unexpectedly") reads to the generic classifier
    /// like a spawn failure. It must NOT reach `on_error` and burn account
    /// health — the same account answers fine over a plain subprocess spawn.
    #[tokio::test]
    async fn pty_transport_failure_does_not_burn_account_health() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("solo", 1))
            .await;

        for _ in 0..5 {
            let _ = rotate_cli_spawn(
                &rotator,
                &[],
                |_env_vars, _retry_hint| async move {
                    Err::<String, _>(duduclaw_cli_runtime::PtyError::Closed.to_string())
                },
                100,
            )
            .await;
        }

        let status = &rotator.status().await[0];
        assert!(
            status.is_healthy,
            "a PTY transport failure must not cool the account"
        );
        assert!(status.is_available);
    }

    /// T4.7 smoke replacement: single good OAuth account — no regression.
    ///
    /// When exactly one healthy account exists and the spawn closure succeeds
    /// immediately, we should return that response on the first attempt and
    /// record success.
    #[tokio::test]
    async fn single_account_success_is_first_try() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("only", 1))
            .await;

        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_cloned = attempts.clone();

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            move |_env_vars, _retry_hint| {
                attempts_cloned.fetch_add(1, Ordering::SeqCst);
                async move { Ok::<String, String>("OK".to_string()) }
            },
            50,
        )
        .await;

        assert_eq!(result.as_deref(), Ok("OK"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let status = &rotator.status().await[0];
        assert_eq!(status.total_requests, 1);
        assert!(status.is_available);
    }

    /// T4.9 smoke replacement: forced rate-limit → user sees 忙線中 message.
    ///
    /// End-to-end path from spawn failure → rotator exhaustion → error
    /// propagation → `classify_cli_failure` → `format_fallback_message`.
    /// Asserts the user-facing text is the RateLimited variant, not
    /// the misleading BinaryMissing "please install and auth" hint.
    #[tokio::test]
    async fn end_to_end_rate_limit_yields_busy_message() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("one", 1))
            .await;
        rotator
            .push_account_for_test(fake_oauth_account("two", 2))
            .await;

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move {
                Err::<String, _>("Error 429 rate limit: usage limit exceeded".to_string())
            },
            50,
        )
        .await;

        let err = result.expect_err("should fail");
        let reason = classify_cli_failure(&err);
        assert_eq!(reason, FailureReason::RateLimited);

        let user_msg = format_fallback_message(
            "Agnes",
            reason,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(user_msg.contains("Agnes"));
        assert!(user_msg.contains("忙線中"), "must say busy: {user_msg}");
        assert!(
            !user_msg.contains("auth status"),
            "must NOT suggest re-running auth status on rate limit: {user_msg}"
        );
        assert!(
            !user_msg.contains("找不到"),
            "must NOT say 'binary not found' on rate limit: {user_msg}"
        );
    }

    /// Regression test for the v1.3.12 bug: stream parser used to
    /// swallow `is_error: true` result events as valid text, which led
    /// to "Not logged in · Please run /login" being delivered to users
    /// as Agnes's reply. After the fix, `spawn_claude_cli_with_env`
    /// returns `Err("claude CLI stream error: Not logged in ...")` and
    /// the classifier + message builder surface the AuthFailed reason.
    ///
    /// We exercise the rotator→classifier→message pipeline by having the
    /// spawn closure return exactly the error shape the new stream parser
    /// now produces.
    #[tokio::test]
    async fn end_to_end_not_logged_in_yields_auth_failed_message() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator
            .push_account_for_test(fake_oauth_account("broken", 1))
            .await;

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move {
                Err::<String, _>(
                    "claude CLI stream error: Not logged in · Please run /login".to_string(),
                )
            },
            50,
        )
        .await;

        let err = result.expect_err("auth failure must surface as Err");
        let reason = classify_cli_failure(&err);
        assert_eq!(reason, FailureReason::AuthFailed);

        let msg = format_fallback_message(
            "Agnes",
            reason,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(msg.contains("Agnes"));
        assert!(msg.contains("/login"));
        assert!(
            !msg.contains("Not logged in · Please run /login"),
            "user-facing message must be our zh-TW explanation, not raw CLI text"
        );
    }

    /// T4.8 smoke replacement: empty-rotator → `call_claude_cli_rotated`
    /// fresh-install passthrough. We can't actually spawn `claude`, but the
    /// primitive behaviour of "empty rotator returns exhausted-Err" is
    /// verified below; the outer function's fall-through to
    /// `call_claude_cli` is a one-liner trivially correct by inspection.
    #[tokio::test]
    async fn rotation_empty_rotator_returns_empty_exhausted() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        assert_eq!(rotator.count().await, 0);

        let result = rotate_cli_spawn(
            &rotator,
            &[],
            |_env_vars, _retry_hint| async move { Ok::<String, String>("never called".to_string()) },
            100,
        )
        .await;

        let err = result.expect_err("empty rotator should return err from primitive");
        assert!(err.contains("All accounts exhausted"));
        // Last error is empty because no attempt was made
        assert!(err.ends_with("Last error: "));
    }

    /// N1: a `.mcp.json` spawn-gate refusal is about the employee's file,
    /// not the account. The loop stops after the first attempt, returns the
    /// gate error unchanged, and leaves every account's health as it was.
    #[tokio::test]
    async fn spawn_gate_refusal_is_not_an_account_failure_and_is_not_retried() {
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.push_account_for_test(fake_oauth_account("first", 1)).await;
        rotator.push_account_for_test(fake_oauth_account("second", 2)).await;
        let before = rotator.status().await;

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in = calls.clone();
        let gate_err = duduclaw_agent::mcp_spawn_gate::spawn_gate_error("員工 agnes 的 MCP 設定無法確認");
        let gate_err_in = gate_err.clone();
        let result = rotate_cli_spawn(
            &rotator,
            &[],
            move |_env, _hint| {
                calls_in.fetch_add(1, Ordering::SeqCst);
                let e = gate_err_in.clone();
                async move { Err::<String, String>(e) }
            },
            100,
        )
        .await;

        assert_eq!(result.unwrap_err(), gate_err);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no second account tried");
        let after = rotator.status().await;
        for (b, a) in before.iter().zip(after.iter()) {
            assert_eq!(b.id, a.id);
            assert_eq!(b.is_healthy, a.is_healthy, "{}", a.id);
            assert_eq!(b.is_available, a.is_available, "{}", a.id);
            assert_eq!(b.total_requests, a.total_requests, "{}", a.id);
        }
        // Ten more refusals would have tripped the three-strike cooldown if
        // they were booked with `on_error`.
        for _ in 0..10 {
            let e = gate_err.clone();
            let _ = rotate_cli_spawn(&rotator, &[], move |_env, _hint| {
                let e = e.clone();
                async move { Err::<String, String>(e) }
            }, 100)
            .await;
        }
        assert!(rotator.status().await.iter().all(|s| s.is_available && s.is_healthy));
    }
}

// ── Python SDK subprocess ───────────────────────────────────

// ── Claude Code SDK (claude CLI) ────────────────────────────

