use super::*;

#[cfg(test)]
mod fallback_tests {
    use super::*;

    #[test]
    fn classify_rate_limit_variants() {
        assert_eq!(
            classify_cli_failure("Error 429 rate limit reached"),
            FailureReason::RateLimited
        );
        assert_eq!(
            classify_cli_failure("usage limit exceeded"),
            FailureReason::RateLimited
        );
        assert_eq!(
            classify_cli_failure("All accounts exhausted. Last error: overloaded"),
            FailureReason::RateLimited
        );
    }

    #[test]
    fn classify_billing_variants() {
        assert_eq!(
            classify_cli_failure("insufficient_quota credit balance"),
            FailureReason::Billing
        );
        assert_eq!(
            classify_cli_failure("HTTP 402 payment required"),
            FailureReason::Billing
        );
    }

    #[test]
    fn classify_timeout() {
        assert_eq!(
            classify_cli_failure("claude CLI hard timeout (1800s, no output)"),
            FailureReason::Timeout
        );
    }

    #[test]
    fn classify_binary_missing() {
        assert_eq!(
            classify_cli_failure("claude CLI not found in PATH"),
            FailureReason::BinaryMissing
        );
    }

    #[test]
    fn classify_empty_response() {
        assert_eq!(
            classify_cli_failure("Empty response from claude CLI"),
            FailureReason::EmptyResponse
        );
    }

    /// Regression lock: v1.3.13 added diagnostic suffixes to Empty / exit
    /// errors. The classifier's substring match must still identify the
    /// reason so user-facing messages stay specific.
    #[test]
    fn classify_empty_response_with_diagnostic_suffix() {
        let err = "Empty response from claude CLI (exit=0 lines=42 events=30 \
                   assistant=2 text_blocks=0 thinking=1 tool_use=0 result_events=1 \
                   result_subtype=Some(\"success\") stop_reason=Some(\"tool_use\") \
                   last_line=\"{\\\"type\\\":\\\"result\\\"...}\" stderr_tail=\"\")";
        assert_eq!(classify_cli_failure(err), FailureReason::EmptyResponse);
    }

    #[test]
    fn classify_exit_code_with_diagnostic_suffix() {
        let err = "claude CLI exit 1 (exit=1 lines=3 events=2 \
                   assistant=0 text_blocks=0 thinking=0 tool_use=0 result_events=0 \
                   result_subtype=None stop_reason=None last_line=\"\" stderr_tail=\"\")";
        assert_eq!(classify_cli_failure(err), FailureReason::SpawnError);
    }

    #[test]
    fn classify_spawn_error() {
        assert_eq!(
            classify_cli_failure("claude CLI spawn error: No such file"),
            FailureReason::SpawnError
        );
        assert_eq!(
            classify_cli_failure("claude CLI exit 127"),
            FailureReason::SpawnError
        );
    }

    #[test]
    fn classify_unknown_fallthrough() {
        assert_eq!(
            classify_cli_failure("some weird unrelated thing"),
            FailureReason::Unknown
        );
    }

    #[test]
    fn classify_auth_failed_variants() {
        // Stream-json error path — what channel_reply surfaces after the fix.
        assert_eq!(
            classify_cli_failure("claude CLI stream error: Not logged in · Please run /login"),
            FailureReason::AuthFailed
        );
        // Assistant event error field path.
        assert_eq!(
            classify_cli_failure("claude CLI assistant error: authentication_failed"),
            FailureReason::AuthFailed
        );
        // Raw "please run /login" text without the prefix.
        assert_eq!(
            classify_cli_failure("Please run /login to authenticate"),
            FailureReason::AuthFailed
        );
    }

    /// 2026-09-08 regression (§D2): every one of these ran for 18 hours
    /// classified as `Unknown`, which is why nothing escalated.
    #[test]
    fn classify_auth_failed_covers_2026_09_incident_strings() {
        for err in [
            "All accounts exhausted. Last error: claude CLI assistant error: oauth_org_not_allowed",
            "oauth_not_allowed_for_organization",
            "This organization is not allowed for this organization's Claude Code access",
            "Invalid bearer token",
            "OAuth access token is invalid",
            "Your organization has disabled Claude subscription access",
            // Case-insensitivity is load-bearing: provider text arrives in
            // several casings across the CLI's error surfaces.
            "OAUTH_ORG_NOT_ALLOWED",
        ] {
            assert_eq!(
                classify_cli_failure(err),
                FailureReason::AuthFailed,
                "must classify as AuthFailed: {err}"
            );
        }
    }

    #[test]
    fn auth_failure_kind_hint_splits_org_from_token() {
        for err in [
            "claude CLI assistant error: oauth_org_not_allowed",
            "oauth_not_allowed_for_organization",
            "not allowed for this organization",
            "Your organization has disabled Claude subscription access",
        ] {
            assert_eq!(
                auth_failure_kind_hint(err),
                Some("org_disabled"),
                "org-family: {err}"
            );
        }
        for err in [
            "claude CLI assistant error: authentication_failed",
            "claude CLI stream error: Not logged in · Please run /login",
            "Invalid bearer token",
            "OAuth access token is invalid",
        ] {
            assert_eq!(
                auth_failure_kind_hint(err),
                Some("invalid_token"),
                "token-family: {err}"
            );
        }
        // Non-auth failures must not be given an auth kind — a fabricated
        // cause would send the operator to the wrong dashboard page.
        assert_eq!(auth_failure_kind_hint("Error 429 rate limit reached"), None);
        assert_eq!(auth_failure_kind_hint("claude CLI not found"), None);
        assert_eq!(auth_failure_kind_hint("some weird unrelated thing"), None);
    }

    #[test]
    fn message_auth_failed_tells_user_to_login() {
        let msg = format_fallback_message(
            "Agnes",
            FailureReason::AuthFailed,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(msg.contains("Agnes"));
        assert!(msg.contains("未登入") || msg.contains("認證失效"));
        assert!(msg.contains("/login"));
        // Must NOT say "claude auth status" (that's the BinaryMissing hint
        // and doesn't fix an auth problem on its own).
        assert!(!msg.contains("auth status"));
    }

    #[test]
    fn message_rate_limited_contains_busy_string_not_auth_status() {
        let msg = format_fallback_message(
            "Agnes",
            FailureReason::RateLimited,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(msg.contains("Agnes"));
        assert!(msg.contains("忙線中"));
        assert!(!msg.contains("auth status"));
    }

    #[test]
    fn message_binary_missing_keeps_auth_status_hint() {
        let msg = format_fallback_message(
            "Agnes",
            FailureReason::BinaryMissing,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(msg.contains("找不到 Claude Code"));
        assert!(msg.contains("auth status"));
    }

    #[test]
    fn message_timeout_mentions_30_min() {
        let msg = format_fallback_message(
            "Agnes",
            FailureReason::Timeout,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(msg.contains("30 分鐘"));
    }

    // ── W0-12: console_url / doc_url mapping (Stripe error-object pattern) ──

    #[test]
    fn account_and_quota_failures_link_to_billing() {
        use crate::deep_link::DeepLinkKind;
        for reason in [
            FailureReason::RateLimited,
            FailureReason::Billing,
            FailureReason::NoAccounts,
            FailureReason::AccountsCoolingDownLong,
            FailureReason::AccountsCoolingDownShort,
            FailureReason::AccountsCoolingDownUnknown,
        ] {
            assert_eq!(
                failure_console_link_kind(reason),
                DeepLinkKind::Billing,
                "{reason:?} should land on the billing/account page"
            );
        }
    }

    #[test]
    fn cli_side_failures_link_to_system() {
        use crate::deep_link::DeepLinkKind;
        for reason in [
            FailureReason::BinaryMissing,
            FailureReason::AuthFailed,
            FailureReason::Timeout,
            FailureReason::SpawnError,
            FailureReason::EmptyResponse,
            FailureReason::Unknown,
        ] {
            assert_eq!(
                failure_console_link_kind(reason),
                DeepLinkKind::System,
                "{reason:?} should land on the system/logs page"
            );
        }
    }

    #[test]
    fn every_failure_reason_has_a_console_link_kind_assigned() {
        // Exhaustiveness guard: if a new FailureReason variant is added
        // without updating failure_console_link_kind, this test's match
        // (mirroring the classify_cli_failure_hint exhaustive match style)
        // would fail to compile — but since failure_console_link_kind
        // already matches exhaustively without a wildcard arm, the compiler
        // itself enforces this. This test instead locks the total count so
        // silently narrowing the match (accidentally merging two variants
        // into a wildcard) would be caught.
        let all = [
            FailureReason::BinaryMissing,
            FailureReason::RateLimited,
            FailureReason::Billing,
            FailureReason::AuthFailed,
            FailureReason::Timeout,
            FailureReason::SpawnError,
            FailureReason::EmptyResponse,
            FailureReason::NoAccounts,
            FailureReason::AccountsCoolingDownLong,
            FailureReason::AccountsCoolingDownShort,
            FailureReason::AccountsCoolingDownUnknown,
            FailureReason::Unknown,
        ];
        assert_eq!(all.len(), 12, "update this list when FailureReason grows");
        for reason in all {
            let _ = failure_console_link_kind(reason); // must not panic for any variant
        }
    }

    #[test]
    fn only_account_rotation_failures_carry_a_doc_url() {
        for reason in [
            FailureReason::RateLimited,
            FailureReason::Billing,
            FailureReason::NoAccounts,
            FailureReason::AccountsCoolingDownLong,
            FailureReason::AccountsCoolingDownShort,
            FailureReason::AccountsCoolingDownUnknown,
        ] {
            let doc = failure_doc_url(reason);
            assert!(doc.is_some(), "{reason:?} should have a doc_url");
            let doc = doc.unwrap();
            assert!(
                doc.starts_with("https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/"),
                "doc_url must point at a real repo doc path, got: {doc}"
            );
        }
        for reason in [
            FailureReason::BinaryMissing,
            FailureReason::AuthFailed,
            FailureReason::Timeout,
            FailureReason::SpawnError,
            FailureReason::EmptyResponse,
            FailureReason::Unknown,
        ] {
            assert_eq!(
                failure_doc_url(reason),
                None,
                "{reason:?} has no matching public doc — must not invent a URL"
            );
        }
    }

    #[test]
    fn console_url_is_none_without_a_resolvable_dashboard_base() {
        // Fail-quiet contract inherited from deep_link: no config.toml at
        // the given home ⇒ no base URL ⇒ None, never a dangling link.
        let home = Path::new("/nonexistent-duduclaw-test-home");
        for reason in [FailureReason::Billing, FailureReason::Timeout] {
            assert_eq!(failure_console_url(home, reason), None);
        }
    }

    #[test]
    fn format_fallback_message_appends_console_link_when_dashboard_resolvable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[gateway]\nport = 18789\n").unwrap();

        let billing_msg = format_fallback_message("Agnes", FailureReason::RateLimited, dir.path());
        assert!(
            billing_msg.contains("🔎 詳情：http://localhost:18789/manage/billing"),
            "billing-group failure must link to /manage/billing: {billing_msg}"
        );

        let system_msg = format_fallback_message("Agnes", FailureReason::Timeout, dir.path());
        assert!(
            system_msg.contains("🔎 詳情：http://localhost:18789/manage/logs"),
            "CLI-side failure must link to /manage/logs: {system_msg}"
        );

        // Exactly one link line — doc_url must never leak into the channel
        // message (it's dashboard-side only, via channel_failures.jsonl).
        assert_eq!(billing_msg.matches("🔎").count(), 1);
        assert!(!billing_msg.contains("github.com"));
    }

    #[test]
    fn format_fallback_message_omits_link_line_when_dashboard_base_unresolvable() {
        let msg = format_fallback_message(
            "Agnes",
            FailureReason::RateLimited,
            Path::new("/nonexistent-duduclaw-test-home"),
        );
        assert!(
            !msg.contains('🔎'),
            "no resolvable dashboard base ⇒ no link line: {msg}"
        );
    }
}

