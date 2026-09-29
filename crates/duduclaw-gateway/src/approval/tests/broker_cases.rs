//! Unit tests for [`super`], moved verbatim out of the former `approval.rs` — broker cases.

use super::*;

#[test]
fn default_direction_tool_lists_absent_are_empty_not_everything() {
    for body in [
        "",                                    // no sections at all
        "[agent]\nname = \"a\"\n",             // no [capabilities]
        "[capabilities]\n",                    // section, no keys
        "[capabilities]\nscoped_tools = []\n", // unrelated sibling only
        "this is not toml {{{",                // malformed file
    ] {
        let dir = with_toml(body);
        assert!(approval_required_tools(&dir).is_empty(), "for {body:?}");
        assert!(irreversible_tools(&dir).is_empty(), "for {body:?}");
        assert!(maybe_irreversible_tools(&dir).is_empty(), "for {body:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // Missing file entirely — same direction.
    let dir = tmp_agent_dir();
    assert!(approval_required_tools(&dir).is_empty());
    assert!(irreversible_tools(&dir).is_empty());
    assert!(maybe_irreversible_tools(&dir).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn default_direction_tool_lists_survive_wrong_types() {
    // The old `.and_then(|t| t.as_array())` ignored a non-array, and
    // `filter_map(as_str)` dropped non-string elements without failing.
    let dir = with_toml("[capabilities]\napproval_required_tools = \"Bash\"\n");
    assert!(
        approval_required_tools(&dir).is_empty(),
        "non-array ⇒ empty, not error"
    );
    std::fs::remove_dir_all(&dir).unwrap();

    let dir = with_toml("[capabilities]\nirreversible_tools = [\"a\", 7, \"b\"]\n");
    let set = irreversible_tools(&dir);
    assert_eq!(set.len(), 2, "non-string elements dropped, not fatal");
    assert!(set.contains("a") && set.contains("b"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn default_direction_auto_approve_install_is_fail_closed() {
    // Opposite direction from its section-mates: only an explicit `true`
    // disables the install-class approval gate.
    for body in [
        "",
        "[capabilities]\n",
        "[capabilities]\nauto_approve_install = false\n",
        "[capabilities]\nauto_approve_install = \"true\"\n", // wrong type
        "[capabilities]\nauto_approve_install = 1\n",        // wrong type
        "not toml [[[",
    ] {
        let dir = with_toml(body);
        assert!(
            !auto_approve_install(&dir),
            "gate must stay ON for {body:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    let dir = with_toml("[capabilities]\nauto_approve_install = true\n");
    assert!(
        auto_approve_install(&dir),
        "explicit true is the sole opt-out"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn default_direction_tool_lists_coexist_in_one_section() {
    // All four keys live in the SAME `[capabilities]` table as the
    // long-typed fields. Reading one must not disturb the others.
    let dir = with_toml(
        "[capabilities]\n\
             computer_use = true\n\
             approval_required_tools = [\"send_email\"]\n\
             irreversible_tools = [\"wire_transfer\"]\n\
             maybe_irreversible_tools = [\"post_message\"]\n\
             auto_approve_install = true\n",
    );
    assert!(tool_requires_approval(&dir, "send_email"));
    assert!(
        !tool_requires_approval(&dir, "send_email_draft"),
        "exact match only"
    );
    assert!(tool_is_irreversible(&dir, "wire_transfer"));
    assert!(tool_is_maybe_irreversible(&dir, "post_message"));
    assert!(auto_approve_install(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn request_creates_pending() {
    let b = broker();
    let id = b
        .request(
            "agent-1",
            "mcp_tool",
            "run Bash rm -rf",
            json!({"tool":"Bash"}),
            60,
        )
        .await
        .unwrap();
    assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Pending);
    let rec = b.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.agent_id, "agent-1");
    assert_eq!(rec.payload, json!({"tool":"Bash"}));
    assert_eq!(rec.ttl_seconds, 60);
}

#[tokio::test(flavor = "current_thread")]
async fn non_positive_ttl_falls_back_to_default() {
    let b = broker();
    let id = b.request("a", "bus_task", "s", json!({}), 0).await.unwrap();
    let rec = b.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.ttl_seconds, DEFAULT_TTL_SECONDS);
}

#[tokio::test(flavor = "current_thread")]
async fn decide_approve_transition() {
    let b = broker();
    let id = b
        .request("a", "mcp_tool", "s", json!({}), 60)
        .await
        .unwrap();
    b.decide(&id, true, "dashboard:alice").await.unwrap();
    assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Approved);
    let rec = b.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.decided_by.as_deref(), Some("dashboard:alice"));
    assert!(rec.decided_at.is_some());
    assert!(rec.status.is_granted());
}

#[tokio::test(flavor = "current_thread")]
async fn decide_deny_transition() {
    let b = broker();
    let id = b
        .request("a", "mcp_tool", "s", json!({}), 60)
        .await
        .unwrap();
    b.decide(&id, false, "channel:user").await.unwrap();
    let status = b.poll(&id).await.unwrap();
    assert_eq!(status, ApprovalStatus::Denied);
    assert!(!status.is_granted());
}

#[tokio::test(flavor = "current_thread")]
async fn double_approve_refused() {
    let b = broker();
    let id = b
        .request("a", "mcp_tool", "s", json!({}), 60)
        .await
        .unwrap();
    b.decide(&id, true, "u1").await.unwrap();
    // Second decide on a terminal state is refused (no silent flip).
    let err = b.decide(&id, true, "u2").await.unwrap_err();
    assert!(err.contains("terminal"), "unexpected: {err}");
    // And a contradictory decision is likewise refused.
    assert!(b.decide(&id, false, "u3").await.is_err());
    // Original decider is preserved.
    let rec = b.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.decided_by.as_deref(), Some("u1"));
    assert_eq!(rec.status, ApprovalStatus::Approved);
}

#[tokio::test(flavor = "current_thread")]
async fn decide_missing_id_errs() {
    let b = broker();
    let ghost = ApprovalId::new();
    assert!(b.decide(&ghost, true, "u").await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn ttl_expiry_treated_as_deny() {
    let b = broker();
    // ttl of 0 would default; force an already-expired row via -1 stored
    // directly is not possible through request(), so insert manually.
    let rec = ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "a".into(),
        action_kind: "bus_task".into(),
        summary: "s".into(),
        payload: json!({}),
        status: ApprovalStatus::Pending,
        // created 10 minutes ago with 1s ttl ⇒ long expired.
        created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: 1,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
    };
    let id = rec.id.clone();
    b.store.insert(&rec).await.unwrap();

    // expire_stale sweeps it.
    let n = b.expire_stale().await.unwrap();
    assert_eq!(n, 1);
    let status = b.poll(&id).await.unwrap();
    assert_eq!(status, ApprovalStatus::Expired);
    assert!(
        !status.is_granted(),
        "expired must NOT be granted (fail-closed)"
    );
    let stored = b.get(&id).await.unwrap().unwrap();
    assert_eq!(stored.decided_by.as_deref(), Some(DECIDED_BY_TTL));
}

#[tokio::test(flavor = "current_thread")]
async fn poll_expires_stale_on_read() {
    let b = broker();
    let rec = ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "a".into(),
        action_kind: "bus_task".into(),
        summary: "s".into(),
        payload: json!({}),
        status: ApprovalStatus::Pending,
        created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: 1,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
    };
    let id = rec.id.clone();
    b.store.insert(&rec).await.unwrap();
    // poll() alone (no explicit sweep) must observe Expired.
    assert_eq!(b.poll(&id).await.unwrap(), ApprovalStatus::Expired);
}

#[tokio::test(flavor = "current_thread")]
async fn list_pending_filters_by_agent() {
    let b = broker();
    b.request("agent-a", "mcp_tool", "s", json!({}), 60)
        .await
        .unwrap();
    b.request("agent-a", "bus_task", "s", json!({}), 60)
        .await
        .unwrap();
    b.request("agent-b", "mcp_tool", "s", json!({}), 60)
        .await
        .unwrap();

    let all = b.list_pending(None).await.unwrap();
    assert_eq!(all.len(), 3);
    let only_a = b.list_pending(Some("agent-a")).await.unwrap();
    assert_eq!(only_a.len(), 2);
    assert!(only_a.iter().all(|r| r.agent_id == "agent-a"));

    // Decided rows drop out of pending.
    let id = only_a[0].id.clone();
    b.decide(&id, true, "u").await.unwrap();
    assert_eq!(b.list_pending(Some("agent-a")).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn list_pending_sweeps_expired() {
    let b = broker();
    // one live, one already-expired
    b.request("a", "mcp_tool", "live", json!({}), 60)
        .await
        .unwrap();
    let stale = ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "a".into(),
        action_kind: "bus_task".into(),
        summary: "stale".into(),
        payload: json!({}),
        status: ApprovalStatus::Pending,
        created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: 1,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
    };
    b.store.insert(&stale).await.unwrap();
    let pending = b.list_pending(None).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].summary, "live");
}

#[tokio::test(flavor = "current_thread")]
async fn await_decision_returns_promptly_when_decided() {
    let b = broker();
    let id = b
        .request("a", "mcp_tool", "s", json!({}), 60)
        .await
        .unwrap();
    let b2 = b.clone();
    let id2 = id.clone();
    // decide almost immediately from another task
    tokio::spawn(async move {
        b2.decide(&id2, true, "u").await.unwrap();
    });
    let status = b
        .await_decision(&id, Duration::from_millis(5))
        .await
        .unwrap();
    assert_eq!(status, ApprovalStatus::Approved);
}

#[tokio::test(flavor = "current_thread")]
async fn await_decision_returns_expired_past_ttl() {
    let b = broker();
    // insert an already-expired pending row
    let rec = ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "a".into(),
        action_kind: "bus_task".into(),
        summary: "s".into(),
        payload: json!({}),
        status: ApprovalStatus::Pending,
        created_at: (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: 1,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
    };
    let id = rec.id.clone();
    b.store.insert(&rec).await.unwrap();
    let status = b
        .await_decision(&id, Duration::from_millis(5))
        .await
        .unwrap();
    assert_eq!(status, ApprovalStatus::Expired);
}
