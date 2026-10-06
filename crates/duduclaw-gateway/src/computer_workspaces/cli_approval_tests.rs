//! Dashboard approval of terminal workspace actions (review H2).

use super::*;
use crate::computer_workspaces::{WorkspaceState, WorkspaceStore, paths};

fn revoked_workspace(home: &std::path::Path) -> (WorkspaceStore, String) {
    std::fs::create_dir_all(home.join("agents/alice")).unwrap();
    std::fs::write(
        home.join("agents/alice/agent.toml"),
        "[agent]\nname = \"alice\"\n",
    )
    .unwrap();
    let store = WorkspaceStore::open(home).unwrap();
    let id = store.create("alice", "local-docker:x", 1, 3).unwrap();
    paths::create_workspace_dirs(home, &id).unwrap();
    store
        .transition(
            &id,
            &[WorkspaceState::Creating],
            WorkspaceState::Ready,
            "t",
            "created",
            None,
        )
        .unwrap();
    store
        .transition(
            &id,
            &[WorkspaceState::Ready],
            WorkspaceState::Revoked,
            "t",
            "revoked",
            None,
        )
        .unwrap();
    (store, id)
}

#[tokio::test]
async fn unapproved_is_refused_and_files_one_dashboard_only_request() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let first = gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap();
    let Gate::Requested(req) = first else {
        panic!("expected a new request, got {first:?}");
    };
    // Re-running while pending neither acts nor files a second request.
    assert_eq!(
        gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap(),
        Gate::Pending(req.clone())
    );
    let all = broker.list_by_kind(ACTION_KIND).await.unwrap();
    assert_eq!(all.len(), 1);
    let rec = &all[0];
    assert!(crate::approval_notify::is_dashboard_only_kind(
        &rec.action_kind
    ));
    assert!(rec.summary.contains(&id) && rec.summary.contains("alice"));
    assert!(rec.summary.contains(CARD_NOTE));
    assert_eq!(rec.payload["requested_by"], UNVERIFIED_ACTOR);
    // The workspace is untouched.
    assert_eq!(
        store.get(&id).unwrap().unwrap().state,
        WorkspaceState::Revoked
    );
}

#[tokio::test]
async fn approved_in_the_dashboard_proceeds_exactly_once() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    broker
        .decide(&req, true, "dashboard:admin-1")
        .await
        .unwrap();
    assert_eq!(
        gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap(),
        Gate::Proceed(req.clone())
    );
    // Consumed: the same approval never authorises a second run.
    assert!(matches!(
        gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap(),
        Gate::Requested(other) if other != req
    ));
}

#[tokio::test]
async fn approved_but_state_changed_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Delete, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    broker
        .decide(&req, true, "dashboard:admin-1")
        .await
        .unwrap();
    // The state moves (regrant + a fence) after the approval.
    store
        .transition(
            &id,
            &[WorkspaceState::Revoked],
            WorkspaceState::Ready,
            "t",
            "regranted",
            None,
        )
        .unwrap();
    let moved = store.get(&id).unwrap().unwrap();
    assert_ne!(state_version(&moved), state_version(&row));
    let again = gate(&broker, GatedAction::Delete, &moved, 30)
        .await
        .unwrap();
    assert!(
        matches!(again, Gate::Requested(ref other) if *other != req),
        "{again:?}"
    );
    let old = broker.get(&req).await.unwrap().unwrap();
    assert_eq!(old.status, ApprovalStatus::Invalidated);
    assert_eq!(old.invalidated_reason.as_deref(), Some("state_changed"));
}

#[tokio::test]
async fn an_approval_not_decided_in_the_dashboard_does_not_count() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Renew, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    broker
        .decide(&req, true, "channel:telegram:42")
        .await
        .unwrap();
    assert!(!matches!(
        gate(&broker, GatedAction::Renew, &row, 30).await.unwrap(),
        Gate::Proceed(_)
    ));
}

#[tokio::test]
async fn a_channel_decision_is_refused_and_the_request_stays_pending() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    let decided =
        crate::approval_notify::apply_decision(home.path(), "telegram", "42", req.as_str(), true)
            .await;
    assert!(decided.is_err(), "{decided:?}");
    let rec = broker.get(&req).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Pending);
}

// ── second review (M-3, M-6) ─────────────────────────────────────────────

#[tokio::test]
async fn a_state_change_updates_the_one_pending_request_in_place() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Delete, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    // A fence moves the state version; repeated runs keep one request.
    for _ in 0..3 {
        store.fence(&id, "operator:t", "loop").unwrap();
        let moved = store.get(&id).unwrap().unwrap();
        assert_eq!(
            gate(&broker, GatedAction::Delete, &moved, 30)
                .await
                .unwrap(),
            Gate::Pending(req.clone())
        );
    }
    let all = broker.list_by_kind(ACTION_KIND).await.unwrap();
    assert_eq!(all.len(), 1, "one request per (action, workspace)");
    let now = store.get(&id).unwrap().unwrap();
    assert_eq!(all[0].payload["state_version"], state_version(&now));
}

#[tokio::test]
async fn an_approval_older_than_the_window_is_spent_not_used() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Regrant, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    broker
        .decide(&req, true, "dashboard:admin-1")
        .await
        .unwrap();
    // A zero-minute window: any decision is already too old.
    let again = gate(&broker, GatedAction::Regrant, &row, 0).await.unwrap();
    assert!(
        matches!(again, Gate::Requested(ref other) if *other != req),
        "{again:?}"
    );
    let old = broker.get(&req).await.unwrap().unwrap();
    assert_eq!(old.status, ApprovalStatus::Invalidated);
    assert_eq!(old.invalidated_reason.as_deref(), Some("approval_expired"));
}

#[tokio::test]
async fn a_non_dashboard_approval_is_invalidated_so_rows_do_not_pile_up() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    let Gate::Requested(req) = gate(&broker, GatedAction::Renew, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    broker
        .decide(&req, true, "channel:telegram:42")
        .await
        .unwrap();
    gate(&broker, GatedAction::Renew, &row, 30).await.unwrap();
    let old = broker.get(&req).await.unwrap().unwrap();
    assert_eq!(
        old.invalidated_reason.as_deref(),
        Some("not_dashboard_decision")
    );
}

#[test]
fn the_channel_notice_names_the_workspace_request_not_a_knowledge_review() {
    let rec = crate::approval::ApprovalRecord {
        id: ApprovalId::new(),
        agent_id: "alice".into(),
        action_kind: ACTION_KIND.into(),
        summary: "x".into(),
        payload: serde_json::json!({
            "action": "delete",
            "workspace_id": "ws-0123456789abcdef0123456789abcdef",
            "owner": "alice",
        }),
        status: ApprovalStatus::Pending,
        created_at: chrono::Utc::now().to_rfc3339(),
        decided_at: None,
        decided_by: None,
        ttl_seconds: TTL_SECS,
        notify_channel: None,
        notify_chat_id: None,
        reminded_at: None,
        simulation: None,
        request_kind: Default::default(),
        binding: None,
        answer: None,
        invalidated_reason: None,
    };
    for reminder in [false, true] {
        let body = crate::approval_notify::dashboard_only_notice_body(&rec, reminder);
        assert!(!body.contains("知識"), "{body}");
        assert!(
            body.contains("電腦操作工作區") && body.contains("ws-0123456789abcdef0123456789abcdef")
        );
        assert!(body.contains(CARD_NOTE) && body.contains("儀表板"));
        // No reply verb is offered.
        assert!(!body.contains("回覆同意"));
    }
    assert!(!crate::approval_notify::DASHBOARD_ONLY_REFUSAL.contains("知識"));
    assert_eq!(
        crate::approval_notify::dashboard_only_expired_text(ACTION_KIND),
        EXPIRED_TEXT
    );
    assert_eq!(
        crate::approval_notify::zh_action_kind(ACTION_KIND),
        "管理電腦操作工作區"
    );
}

#[tokio::test]
async fn pushes_per_workspace_are_capped_per_hour_and_the_excess_is_audited() {
    let home = tempfile::tempdir().unwrap();
    let (store, id) = revoked_workspace(home.path());
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let row = store.get(&id).unwrap().unwrap();
    // Two earlier requests this hour that were pushed (denied since).
    for _ in 0..PUSH_CAP_PER_HOUR {
        let Gate::Requested(r) = gate(&broker, GatedAction::Delete, &row, 30).await.unwrap() else {
            panic!("no request");
        };
        broker
            .set_notify_target_for_test(&r, "telegram", "1")
            .await
            .unwrap();
        broker.decide(&r, false, "dashboard:admin-1").await.unwrap();
    }
    let Gate::Requested(next) = gate(&broker, GatedAction::Delete, &row, 30).await.unwrap() else {
        panic!("no request");
    };
    let rec = broker.get(&next).await.unwrap().unwrap();
    assert!(!push_allowed(home.path(), &rec).await);
    assert_eq!(
        store
            .ids_with_event("admin_approval_push_suppressed")
            .unwrap(),
        vec![id]
    );
}
