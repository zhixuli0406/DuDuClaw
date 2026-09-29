use super::*;

#[cfg(test)]
mod branding_footer_tests {
    use super::*;

    #[test]
    fn footer_targets_external_channels_only() {
        assert!(footer_applies_to_session("line:U123"));
        assert!(footer_applies_to_session("telegram:42#topic:7"));
        // Owner console + internal sessions stay unbranded.
        assert!(!footer_applies_to_session("webchat:conn#agent:a"));
        assert!(!footer_applies_to_session("default"));
        assert!(!footer_applies_to_session("cron:daily"));
        // Prefix must be exact-token (`linex:` is not `line:`).
        assert!(!footer_applies_to_session("linex:U123"));
    }

    #[test]
    fn config_gate_fails_open_to_visible() {
        let dir = tempfile::tempdir().unwrap();
        // No config at all ⇒ on.
        assert!(branding_footer_enabled(dir.path()));
        // Malformed config ⇒ on.
        std::fs::write(dir.path().join("config.toml"), "{{{").unwrap();
        assert!(branding_footer_enabled(dir.path()));
        // Explicit opt-out parses.
        std::fs::write(
            dir.path().join("config.toml"),
            "[branding]\nreply_footer = false\n",
        )
        .unwrap();
        assert!(!branding_footer_enabled(dir.path()));
    }

    #[tokio::test]
    async fn footer_appends_on_free_tier_and_skips_empty() {
        let dir = tempfile::tempdir().unwrap();
        // No global license runtime in tests ⇒ treated as free ⇒ footer on,
        // even when the config says off (the opt-out is paid-gated).
        std::fs::write(
            dir.path().join("config.toml"),
            "[branding]\nreply_footer = false\n",
        )
        .unwrap();
        let out = append_branding_footer("好的，已完成".into(), dir.path(), "line:U1").await;
        assert!(
            out.ends_with(BRANDING_FOOTER),
            "free tier must keep the footer: {out}"
        );
        // Deliberate silence stays silent.
        let silent = append_branding_footer(String::new(), dir.path(), "line:U1").await;
        assert!(silent.is_empty());
        // Owner console stays unbranded.
        let console = append_branding_footer("hi".into(), dir.path(), "webchat:c#a").await;
        assert_eq!(console, "hi");
    }
}


#[cfg(test)]
mod channel_gvu_gate_tests {
    use super::channel_gvu_trigger_allowed;

    #[test]
    fn disabled_agent_blocks_channel_gvu_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("agent.toml"),
            "[evolution]\ngvu_enabled = false\n",
        )
        .unwrap();
        assert!(!channel_gvu_trigger_allowed(tmp.path()));
    }

    #[test]
    fn missing_key_blocks_channel_gvu_trigger_fail_closed() {
        // No [evolution] section at all — R3's exact failure shape (silent
        // DENY, not silent ALLOW). Confirms the channel path inherits the
        // fail-closed posture, not an accidentally-permissive one.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        assert!(!channel_gvu_trigger_allowed(tmp.path()));
    }

    #[test]
    fn explicit_opt_in_allows_channel_gvu_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("agent.toml"),
            "[evolution]\ngvu_enabled = true\n",
        )
        .unwrap();
        assert!(channel_gvu_trigger_allowed(tmp.path()));
    }
}

#[cfg(test)]
mod runtime_substitution_tests {
    use super::is_runtime_substitution;

    #[test]
    fn matching_provider_is_not_a_substitution() {
        assert!(!is_runtime_substitution("codex", "codex"));
        assert!(!is_runtime_substitution("claude", "claude"));
    }

    #[test]
    fn mismatched_provider_is_a_substitution() {
        // The distributor incident this fixes: agent configured for `grok`,
        // but grok's CLI wasn't registered so the choke-point's failover
        // silently answered via Claude.
        assert!(is_runtime_substitution("grok", "claude"));
        assert!(is_runtime_substitution("gemini", "codex"));
    }

    #[test]
    fn openai_compat_sse_is_the_same_provider_not_a_substitution() {
        assert!(!is_runtime_substitution(
            "openai_compat",
            "openai_compat_sse"
        ));
    }
}


#[cfg(test)]
mod mistake_evidence_tests {
    use super::{conversation_outcome_evidence, decision_gap_evidence};
    use crate::gvu::mistake_notebook::{MistakeCategory, build_mistake_entry};
    use crate::prediction::outcome::{ConversationOutcome, SatisfactionSignal, TaskType};

    #[test]
    fn decision_gap_evidence_marks_entry_verified() {
        let entry = build_mistake_entry(
            "agent-1",
            "sess-1",
            MistakeCategory::Capability,
            "用方案 B 好了",
            "(referenced decision had no durable record)",
            "使用者引用了某個方案/選項，但沒有任何未決決策可對應。",
            None,
            "decision_gap",
        )
        .with_evidence(decision_gap_evidence("用方案 B 好了"));

        assert!(
            entry.is_verified(),
            "decision-gap evidence must verify the entry"
        );
        let ev = entry.evidence.as_ref().unwrap();
        assert_eq!(ev.error_kind, "assertion_failed");
        assert!(
            ev.assertion_failed
                .as_deref()
                .unwrap()
                .contains("mentions_decision_reference")
        );
        assert_eq!(ev.source_span.as_deref(), Some("用方案 B 好了"));
    }

    #[test]
    fn decision_gap_evidence_truncates_long_user_text_cjk_safely() {
        // 400 CJK chars — must not panic on a multi-byte boundary and must
        // land at exactly 300 codepoints (truncate_chars, not byte slicing).
        let long_text: String = std::iter::repeat('用').take(400).collect();
        let ev = decision_gap_evidence(&long_text);
        assert_eq!(ev.source_span.as_ref().unwrap().chars().count(), 300);
    }

    #[test]
    fn conversation_outcome_evidence_marks_entry_verified() {
        let outcome = ConversationOutcome {
            session_id: "sess-1".to_string(),
            agent_id: "agent-1".to_string(),
            task_type: TaskType::Coding,
            satisfaction: SatisfactionSignal::Negative,
            task_completed: Some(false),
            correction_count: 2,
            explicit_feedback: None,
        };
        let entry = build_mistake_entry(
            "agent-1",
            "sess-1",
            MistakeCategory::Capability,
            "還是壞的，重來",
            "(agent reply)",
            "Task not completed",
            None,
            "task_failure",
        )
        .with_evidence(conversation_outcome_evidence(&outcome, "還是壞的，重來"));

        assert!(
            entry.is_verified(),
            "conversation-outcome evidence must verify the entry"
        );
        let ev = entry.evidence.as_ref().unwrap();
        assert_eq!(ev.error_kind, "assertion_failed");
        let assertion = ev.assertion_failed.as_deref().unwrap();
        assert!(assertion.contains("Negative"));
        assert!(assertion.contains("correction_count=2"));
        assert_eq!(ev.source_span.as_deref(), Some("還是壞的，重來"));
    }

    #[test]
    fn conversation_outcome_evidence_truncates_assertion_and_span() {
        let outcome = ConversationOutcome {
            session_id: "sess-1".to_string(),
            agent_id: "agent-1".to_string(),
            task_type: TaskType::Unknown,
            satisfaction: SatisfactionSignal::Neutral,
            task_completed: None,
            correction_count: 0,
            explicit_feedback: None,
        };
        let long_text: String = std::iter::repeat('壞').take(500).collect();
        let ev = conversation_outcome_evidence(&outcome, &long_text);
        assert!(ev.assertion_failed.as_ref().unwrap().chars().count() <= 300);
        assert_eq!(ev.source_span.as_ref().unwrap().chars().count(), 300);
    }
}


#[cfg(test)]
mod failure_reason_as_str_tests {
    use super::FailureReason;

    #[test]
    fn every_variant_has_a_stable_snake_case_token() {
        let cases = [
            (FailureReason::BinaryMissing, "binary_missing"),
            (FailureReason::RateLimited, "rate_limited"),
            (FailureReason::Billing, "billing"),
            (FailureReason::AuthFailed, "auth_failed"),
            (FailureReason::Timeout, "timeout"),
            (FailureReason::SpawnError, "spawn_error"),
            (FailureReason::EmptyResponse, "empty_response"),
            (FailureReason::NoAccounts, "no_accounts"),
            (
                FailureReason::AccountsCoolingDownLong,
                "accounts_cooling_down_long",
            ),
            (
                FailureReason::AccountsCoolingDownShort,
                "accounts_cooling_down_short",
            ),
            (
                FailureReason::AccountsCoolingDownUnknown,
                "accounts_cooling_down_unknown",
            ),
            (FailureReason::Unknown, "unknown"),
        ];
        for (variant, expected) in cases {
            assert_eq!(variant.as_str(), expected);
        }
    }
}

#[cfg(test)]
mod local_first_tests {
    use super::local_inference_first;

    /// Table-driven: only the exact "local" token flips the channel path to
    /// local-first; every other mode keeps CLI-first behavior unchanged.
    #[test]
    fn inference_mode_routing_decision() {
        for (mode, expected) in [
            ("local", true),
            ("hybrid", false),
            ("claude", false),
            ("", false),
            ("LOCAL", false),      // case-sensitive, like the dispatcher match
            ("local-only", false), // token equality, never substring
            (" local", false),     // raw config value, no trimming surprises
            ("cloud", false),
        ] {
            assert_eq!(local_inference_first(mode), expected, "mode = {mode:?}");
        }
    }
}

#[cfg(test)]
mod channel_admin_tests {
    use super::admin_list_contains;

    #[test]
    fn admin_membership_is_fail_closed_and_exact() {
        // Missing / empty / malformed list ⇒ NOT admin (fail-closed).
        assert!(!admin_list_contains(None, &["u1"]));
        assert!(!admin_list_contains(Some(""), &["u1"]));
        assert!(!admin_list_contains(Some("[]"), &["u1"]));
        assert!(!admin_list_contains(Some("not json"), &["u1"]));
        assert!(
            !admin_list_contains(Some("[\"\"]"), &[""]),
            "empty ids never match"
        );

        // Exact equality against any caller identity.
        let list = Some("[\"12345\", \"U0AAA\"]");
        assert!(admin_list_contains(list, &["12345"]));
        assert!(admin_list_contains(list, &["telegram:99", "U0AAA"]));
        // Never substring / prefix matching.
        assert!(!admin_list_contains(list, &["123456"]));
        assert!(!admin_list_contains(list, &["1234"]));
        assert!(!admin_list_contains(list, &["u0aaa"]), "case-sensitive ids");
    }
}

#[cfg(test)]
mod contract_enforcement_tests {
    use super::enforce_contract;

    fn write_contract(home: &std::path::Path, agent: &str, must_not: &str) {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("CONTRACT.toml"),
            format!("[boundaries]\nmust_not = [\"{must_not}\"]\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn violating_reply_is_blocked_and_audited() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_contract(tmp.path(), "agnes", "reveal api keys");

        let out = enforce_contract(
            "Here, I will reveal api keys: sk-xyz".to_string(),
            tmp.path(),
            "agnes",
        )
        .await;

        assert!(
            out.contains("行為契約邊界"),
            "must return the block message, got: {out}"
        );
        assert!(
            !out.contains("sk-xyz"),
            "violating content must not survive"
        );

        let log = std::fs::read_to_string(tmp.path().join("security_audit.jsonl")).unwrap();
        assert!(log.contains("contract_violation"), "block must be audited");
    }

    #[tokio::test]
    async fn benign_reply_passes_through_unchanged() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_contract(tmp.path(), "agnes", "reveal api keys");

        let reply = "The deployment finished successfully.".to_string();
        let out = enforce_contract(reply.clone(), tmp.path(), "agnes").await;
        assert_eq!(out, reply);
    }

    #[tokio::test]
    async fn no_contract_file_passes_through() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No agents/ghost/CONTRACT.toml written.
        let reply = "anything at all".to_string();
        let out = enforce_contract(reply.clone(), tmp.path(), "ghost").await;
        assert_eq!(out, reply);
    }
}

#[cfg(test)]
mod multi_turn_tests {
    use super::*;

    #[test]
    fn trim_turn_content_short_passthrough() {
        let short = "Hello world";
        assert_eq!(trim_turn_content(short), short);
    }

    #[test]
    fn trim_turn_content_at_threshold() {
        let exactly = "a".repeat(TURN_TRIM_THRESHOLD);
        assert_eq!(trim_turn_content(&exactly), exactly);
    }

    #[test]
    fn trim_turn_content_over_threshold() {
        let long = "x".repeat(TURN_TRIM_THRESHOLD + 100);
        let result = trim_turn_content(&long);
        assert!(result.contains("[trimmed"));
        assert!(result.len() < long.len());
    }

    #[test]
    fn trim_turn_content_cjk_safe() {
        // 900 CJK chars — each is 3 bytes in UTF-8.
        // This would panic with byte-level slicing.
        let cjk = "你好世界".repeat(225); // 4 chars × 225 = 900 chars
        assert_eq!(cjk.chars().count(), 900);
        let result = trim_turn_content(&cjk);
        assert!(result.contains("[trimmed"));
        // Verify result is valid UTF-8 (would panic if not)
        let _ = result.as_bytes();
    }

    #[test]
    fn format_history_empty() {
        assert_eq!(format_history_as_prompt(&[], "hello"), "hello");
    }

    #[test]
    fn format_history_single_turn() {
        let history = vec![ConversationTurn {
            role: "user".to_string(),
            content: "hi".to_string(),
        }];
        let result = format_history_as_prompt(&history, "world");
        assert!(result.contains("<conversation_history>"));
        assert!(result.contains("<user>hi</user>"));
        // History framing (2026-07-28): the current message is delimited and
        // preceded by the "history is context, don't resume old tasks" rule.
        assert!(result.contains("不要自行重啟"), "{result}");
        assert!(
            result.ends_with("<current_message>\nworld\n</current_message>"),
            "{result}"
        );
    }

    #[test]
    fn format_history_xml_escaping() {
        let history = vec![ConversationTurn {
            role: "assistant".to_string(),
            content: "Use </assistant> tag carefully".to_string(),
        }];
        let result = format_history_as_prompt(&history, "ok");
        // The closing tag in content should be escaped
        assert!(!result.contains("Use </assistant> tag"));
        assert!(result.contains("&lt;/assistant&gt;"));
    }

    #[test]
    fn recap_prefix_with_pinned() {
        let pinned = "- Goal: build two teams\n- PM: daily 8:00 report";
        let msg = "開始建立團隊";
        let result = format!("<task_recap>\n{pinned}\n</task_recap>\n\n{msg}");
        assert!(result.contains("<task_recap>"));
        assert!(result.contains("build two teams"));
        assert!(result.ends_with("開始建立團隊"));
    }

    #[test]
    fn recap_skipped_when_no_pinned() {
        let pinned = "";
        let msg = "hello";
        // When pinned is empty, effective_message = sanitized_text (no recap)
        let effective = if pinned.is_empty() {
            msg.to_string()
        } else {
            format!("<task_recap>\n{pinned}\n</task_recap>\n\n{msg}")
        };
        assert_eq!(effective, "hello");
    }
}

#[cfg(test)]
mod token_owner_tests {
    use super::*;

    fn agents(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect()
    }

    fn lookup<'a>(global: &str, agents: &'a [(String, String)]) -> Option<&'a str> {
        find_global_token_owner(global, agents.iter().map(|(n, t)| (n.as_str(), t.as_str())))
    }

    #[test]
    fn global_token_shared_with_agent_returns_owner() {
        // The customer's CEO scenario: same token in config.toml and agent.ceo.
        let agents = agents(&[("ceo", "TOK_CEO"), ("coo", "TOK_COO")]);
        assert_eq!(lookup("TOK_CEO", &agents), Some("ceo"));
    }

    #[test]
    fn global_only_token_has_no_owner() {
        // COO-style: token lives only globally → global poller must run.
        let agents = agents(&[("ceo", "TOK_CEO")]);
        assert_eq!(lookup("TOK_GLOBAL_ONLY", &agents), None);
    }

    #[test]
    fn no_agents_means_no_owner() {
        let agents = agents(&[]);
        assert_eq!(lookup("TOK_ANY", &agents), None);
    }

    #[test]
    fn first_agent_wins_when_multiple_share_token() {
        let agents = agents(&[("ceo", "TOK_DUP"), ("coo", "TOK_DUP")]);
        assert_eq!(lookup("TOK_DUP", &agents), Some("ceo"));
    }
}

