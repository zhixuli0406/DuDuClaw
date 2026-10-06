//! Unit tests for [`super`], moved verbatim out of the former `approval.rs` — simulation cases.

use super::*;

/// Fix-2 H4a positive path: a page in a namespace explicitly locked to
/// `read_only` (the operator, not any agent, controls its content) IS
/// eligible grounding evidence.
#[test]
fn grounding_snippets_includes_read_only_shared_namespace() {
    let home = tmp_agent_dir();
    let agent_dir = home.join("agents").join("dudu");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let shared = duduclaw_memory::WikiStore::new_shared(&home);
    let long_body = "退款".repeat(400); // ~2.4KB, forces truncation
    shared
        .write_page(
            "policies/refund-sop.md",
            &format!("# 退款 SOP\n\nsend_email 退款流程如下：{long_body}"),
        )
        .unwrap();
    write_scope_policy(
        &home,
        "[namespaces.\"policies\"]\nmode = \"operator_only\"\n",
    );

    let hits = simulation_grounding_snippets(&home, &agent_dir, "send_email 退款");
    assert!(
        !hits.is_empty(),
        "expected a match against the protected shared wiki page"
    );
    assert!(hits[0].chars().count() <= GROUNDING_SNIPPET_MAX_CHARS);
    assert!(hits[0].contains("退款"));

    let block = render_grounding_block(&hits).unwrap();
    assert!(block.starts_with("<reference>"));
    assert!(block.ends_with("</reference>"));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn grounding_snippets_caps_at_max_and_handles_missing_agent_dir() {
    // No `.scope.toml` at all ⇒ fail-closed empty; must not panic or
    // error even when `agent_dir` was never materialized (the function
    // no longer reads it at all, per H4a, but must stay tolerant of a
    // caller passing a not-yet-materialized directory).
    let home = tmp_agent_dir();
    let agent_dir = home.join("agents").join("ghost-agent");
    let hits = simulation_grounding_snippets(&home, &agent_dir, "anything");
    assert!(hits.len() <= GROUNDING_MAX_SNIPPETS);
    assert!(hits.is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn protected_wiki_namespaces_ignores_agent_writable_and_allowlist_modes() {
    let home = tmp_agent_dir();
    write_scope_policy(
        &home,
        r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"

                [namespaces."policies"]
                mode = "operator_only"

                [namespaces."sop"]
                mode = "agent_writable"

                [namespaces."hr"]
                mode = "agent_allowlist"
                agents = ["agnes"]
            "#,
    );
    let protected = protected_wiki_namespaces(&home);
    assert!(protected.contains("identity"));
    assert!(protected.contains("policies"));
    assert!(
        !protected.contains("sop"),
        "agent_writable must never be protected"
    );
    assert!(
        !protected.contains("hr"),
        "agent_allowlist still lets some agent write it — not protected for grounding purposes"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn protected_wiki_namespaces_empty_on_malformed_or_absent_file() {
    let home = tmp_agent_dir();
    // Absent file.
    assert!(protected_wiki_namespaces(&home).is_empty());
    // Malformed TOML.
    write_scope_policy(&home, "this is :: not = valid = toml ===");
    assert!(protected_wiki_namespaces(&home).is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn rule_requires_approval_parsing() {
    assert!(rule_requires_approval(&json!({"require_approval": true})));
    assert!(!rule_requires_approval(&json!({"require_approval": false})));
    assert!(!rule_requires_approval(&json!({"type": "delegate"})));
    assert!(!rule_requires_approval(&json!({"require_approval": "yes"})));
}

#[test]
fn channel_summary_is_zh_tw_and_xml_safe() {
    let rec = ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "sales-bot".into(),
        action_kind: "autopilot_action".into(),
        summary: "delete <all> records & drop table".into(),
        payload: json!({}),
        status: ApprovalStatus::Pending,
        created_at: Utc::now().to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: 300,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
        request_kind: crate::approval::RequestKind::Approval,
        binding: None,
        answer: None,
        invalidated_reason: None,
    };
    let msg = pending_summary_for_channel(&rec);
    assert!(msg.contains("需要您的核准"));
    assert!(msg.contains("確認"));
    assert!(msg.contains("&lt;all&gt;"));
    assert!(msg.contains("&amp;"));
    assert!(!msg.contains("<all>"));
}

#[test]
fn reminder_fires_once_in_the_last_third_of_the_ttl() {
    let now = Utc::now();
    // 300s TTL ⇒ due from 200s in.
    assert!(!aged(10, 300, false).reminder_due(now), "too early");
    assert!(!aged(199, 300, false).reminder_due(now), "just before ⅔");
    assert!(
        aged(210, 300, false).reminder_due(now),
        "inside the last third"
    );
    assert!(aged(299, 300, false).reminder_due(now));
    // Already expired ⇒ that is a denial, not a nudge.
    assert!(!aged(301, 300, false).reminder_due(now));
    // Already reminded ⇒ never again (the DB column is the once-only guard).
    assert!(!aged(210, 300, true).reminder_due(now));
}

#[test]
fn reminder_is_suppressed_for_very_short_ttls() {
    let now = Utc::now();
    // A 60s gate: the ⅔ mark is 40s in, leaving 20s — the nudge and the
    // auto-denial would arrive back to back for no actionable gain.
    assert!(!aged(50, 60, false).reminder_due(now));
    assert!(!aged(90, 119, false).reminder_due(now));
    // At the floor and above, the nudge is worth sending.
    assert!(aged(90, 120, false).reminder_due(now));
    assert_eq!(REMIND_MIN_TTL_SECONDS, 120);
}

// ── B5: dashboard navigate wired to the same ⅔-TTL reminder point ──

#[test]
fn reminder_navigate_path_matches_the_inbox_deep_link_contract() {
    let id = ApprovalId::from("ap-abc123".to_string());
    assert_eq!(reminder_navigate_path(&id), "/inbox?item=ap-abc123");
}

#[test]
fn reminder_target_write_back_only_when_it_differs() {
    let mut r = aged(210, 300, false);
    // First push found nothing ⇒ the reminder's destination is new.
    assert!(notify_target_changed(&r, "telegram", "555"));
    r.notify_channel = Some("telegram".into());
    r.notify_chat_id = Some("555".into());
    // Same destination as before ⇒ no pointless write.
    assert!(!notify_target_changed(&r, "telegram", "555"));
    // Re-resolved elsewhere (first destination went away) ⇒ write back.
    assert!(notify_target_changed(&r, "telegram", "666"));
    assert!(notify_target_changed(&r, "slack", "555"));
}

#[tokio::test(flavor = "current_thread")]
async fn reminder_retry_leaves_no_stale_target_when_nothing_is_reachable() {
    // An on-disk broker in an empty home: no config, no agents, no users.db
    // ⇒ the reminder finds no destination. It must still consume the
    // once-only slot (no storm) and must NOT invent a notify target.
    let dir = tempfile::tempdir().unwrap();
    let b = ApprovalBroker::open(dir.path()).unwrap();
    let rec = aged(210, 300, false);
    let id = rec.id.clone();
    b.store.insert(&rec).await.unwrap();

    b.maybe_remind(&rec, Utc::now()).await;
    let after = b.get(&id).await.unwrap().unwrap();
    assert!(after.reminded_at.is_some(), "slot consumed");
    assert_eq!(after.notify_channel, None, "no phantom destination");
    assert_eq!(after.notify_chat_id, None);
    // Second call is a no-op (the claim already lost).
    b.maybe_remind(&after, Utc::now()).await;
    assert_eq!(
        b.get(&id).await.unwrap().unwrap().reminded_at,
        after.reminded_at
    );
}

#[test]
fn reminder_never_fires_for_terminal_or_unparseable_rows() {
    let now = Utc::now();
    let mut decided = aged(210, 300, false);
    decided.status = ApprovalStatus::Approved;
    assert!(!decided.reminder_due(now));

    let mut broken = aged(210, 300, false);
    broken.created_at = "not-a-timestamp".into();
    assert!(!broken.reminder_due(now));
}

#[tokio::test(flavor = "current_thread")]
async fn reminder_slot_is_claimed_exactly_once() {
    let b = broker();
    let rec = aged(210, 300, false);
    let id = rec.id.clone();
    b.store.insert(&rec).await.unwrap();
    let now = Utc::now().to_rfc3339();
    assert!(
        b.store.claim_reminder(&id, &now).await.unwrap(),
        "first claim wins"
    );
    assert!(
        !b.store.claim_reminder(&id, &now).await.unwrap(),
        "second claim must lose (no reminder storm)"
    );
    assert!(b.get(&id).await.unwrap().unwrap().reminded_at.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn reminder_slot_cannot_be_claimed_on_a_decided_row() {
    let b = broker();
    let id = b
        .request("a", "mcp_install", "s", json!({}), 300)
        .await
        .unwrap();
    b.decide(&id, true, "u").await.unwrap();
    assert!(
        !b.store
            .claim_reminder(&id, &Utc::now().to_rfc3339())
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn notify_target_round_trips() {
    let b = broker();
    let id = b
        .request("a", "mcp_install", "s", json!({}), 300)
        .await
        .unwrap();
    // Fresh row has no destination (in-memory store never pushes).
    let fresh = b.get(&id).await.unwrap().unwrap();
    assert_eq!(fresh.notify_channel, None);
    assert_eq!(fresh.notify_chat_id, None);

    b.store
        .set_notify_target(&id, "telegram", "555")
        .await
        .unwrap();
    let after = b.get(&id).await.unwrap().unwrap();
    assert_eq!(after.notify_channel.as_deref(), Some("telegram"));
    assert_eq!(after.notify_chat_id.as_deref(), Some("555"));
}

#[test]
fn self_notifying_kinds_skip_the_generic_push() {
    // goal_kickoff owns its own buttoned push (goal_notify) — a second
    // generic push would show two conflicting button sets.
    assert!(SELF_NOTIFYING_KINDS.contains(&"goal_kickoff"));
    assert!(!SELF_NOTIFYING_KINDS.contains(&"mcp_install"));
    assert!(!SELF_NOTIFYING_KINDS.contains(&"capability_grant"));
}

#[test]
fn status_from_db_fails_closed_on_unknown() {
    assert_eq!(ApprovalStatus::from_db("garbage"), ApprovalStatus::Denied);
    assert!(!ApprovalStatus::from_db("garbage").is_granted());
}

// ── expires_at_epoch (dashboard countdown) ────────────────────────────────

#[test]
fn expires_at_epoch_matches_created_at_plus_ttl() {
    let rec = aged(0, 300, false);
    let created = DateTime::parse_from_rfc3339(&rec.created_at)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(rec.expires_at_epoch(), Some((created.timestamp()) + 300));
    // Matches the RFC3339 sibling accessor exactly (same underlying instant).
    assert_eq!(
        rec.expires_at_epoch(),
        rec.deadline_rfc3339()
            .map(|s| DateTime::parse_from_rfc3339(&s).unwrap().timestamp())
    );
}

#[test]
fn expires_at_epoch_none_on_unparseable_created_at() {
    let mut rec = aged(0, 300, false);
    rec.created_at = "not-a-timestamp".into();
    assert_eq!(rec.expires_at_epoch(), None);
}
