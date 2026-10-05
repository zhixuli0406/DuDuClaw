use super::*;
use crate::channel_ingress::AcceptedEvent;

async fn uncertain_event(home: &Path, event_id: &str) -> IngressRow {
    let store = IngressStore::open(home).unwrap();
    store
        .append(
            &[AcceptedEvent {
                decision_fastlane: false,
                decision_binding: None,
                event_id: event_id.into(),
                account: "account".into(),
                revision: "r1".into(),
                authorization_revision: "a1".into(),
                conversation: format!("chat-{event_id}"),
                payload: "{\"replyToken\":\"SECRET\"}".into(),
            }],
            10,
        )
        .await
        .unwrap();
    let row = store.claim(10).await.unwrap().unwrap();
    store
        .transition(&row, "claimed", "dispatching", None)
        .await
        .unwrap();
    store
        .transition(
            &row,
            "dispatching",
            "uncertain",
            Some("dispatch_receipt_missing"),
        )
        .await
        .unwrap();
    store.get(&row.id).await.unwrap().unwrap()
}

fn close(id: &str, note: &str) -> CliRequest {
    CliRequest {
        action: CliAction::Close,
        ingress_id: id.into(),
        note: note.into(),
        provider_receipt: Some("line-req-9".into()),
        confirm_duplicate_risk: false,
    }
}

#[tokio::test]
async fn terminal_change_waits_for_dashboard_admin_and_applies_once() {
    let dir = tempfile::tempdir().unwrap();
    let row = uncertain_event(dir.path(), "e1").await;
    let req = close(&row.id, "customer confirmed they got the answer");
    let CliOutcome::Requested(id) = request_or_apply(dir.path(), &req).await.unwrap() else {
        panic!("first run must file a request");
    };
    assert_eq!(
        request_or_apply(dir.path(), &req).await.unwrap(),
        CliOutcome::Pending(id.clone())
    );
    let store = IngressStore::open(dir.path()).unwrap();
    assert_eq!(
        store.get(&row.id).await.unwrap().unwrap().status,
        "uncertain"
    );
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    let rec = broker
        .get(&ApprovalId::from(id.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(rec.summary.contains(CARD_NOTE));
    assert!(!notice_body(&rec, false).contains("customer confirmed"));
    broker
        .decide(&ApprovalId::from(id.clone()), true, "dashboard:admin-1")
        .await
        .unwrap();
    let CliOutcome::Applied(_) = request_or_apply(dir.path(), &req).await.unwrap() else {
        panic!("approved request must apply");
    };
    let after = store.get(&row.id).await.unwrap().unwrap();
    assert_eq!(after.status, "closed");
    let summary = store.summary().await.unwrap();
    assert_eq!(summary["resolutions"][0]["provider_receipt"], "line-req-9");
    assert!(
        summary["resolutions"][0]["actor"]
            .as_str()
            .unwrap()
            .contains("dashboard:admin-1")
    );
    // The approval was consumed: running again cannot apply twice.
    assert!(request_or_apply(dir.path(), &req).await.is_err());
}

#[tokio::test]
async fn approvals_not_from_dashboard_or_for_another_note_or_state_are_void() {
    let dir = tempfile::tempdir().unwrap();
    let row = uncertain_event(dir.path(), "e2").await;
    let req = close(&row.id, "first reason");
    let CliOutcome::Requested(id) = request_or_apply(dir.path(), &req).await.unwrap() else {
        panic!()
    };
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    // A channel or system decision never authorizes a terminal change.
    broker
        .decide(&ApprovalId::from(id.clone()), true, "channel:line:someone")
        .await
        .unwrap();
    assert!(matches!(
        request_or_apply(dir.path(), &req).await.unwrap(),
        CliOutcome::Requested(_)
    ));
    // Another note is another request; the approval of the first does not cover it.
    let other = close(&row.id, "a different reason");
    let CliOutcome::Requested(other_id) = request_or_apply(dir.path(), &other).await.unwrap()
    else {
        panic!()
    };
    broker
        .decide(&ApprovalId::from(other_id), true, "dashboard:admin")
        .await
        .unwrap();
    // The event changes before the approved request is run.
    let store = IngressStore::open(dir.path()).unwrap();
    store
        .resolve(
            &row.id,
            "r1",
            "rerun",
            true,
            "dashboard-admin",
            "rpc rerun",
            20,
            row.attempt,
        )
        .await
        .unwrap();
    assert!(request_or_apply(dir.path(), &other).await.is_err());
    assert_eq!(store.get(&row.id).await.unwrap().unwrap().status, "ready");
}

#[tokio::test]
async fn inapplicable_actions_and_bad_ids_file_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let row = uncertain_event(dir.path(), "e3").await;
    let retry = CliRequest {
        action: CliAction::Retry,
        ..close(&row.id, "try again")
    };
    assert!(request_or_apply(dir.path(), &retry).await.is_err());
    let rerun_unconfirmed = CliRequest {
        action: CliAction::Rerun,
        ..close(&row.id, "run it again")
    };
    assert!(
        request_or_apply(dir.path(), &rerun_unconfirmed)
            .await
            .is_err()
    );
    assert!(
        request_or_apply(dir.path(), &close("../../x", "n"))
            .await
            .is_err()
    );
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    assert!(broker.list_by_kind(ACTION_KIND).await.unwrap().is_empty());
    let listed = list(dir.path(), None).await.unwrap();
    assert!(!listed.to_string().contains("SECRET"));
    assert!(show(dir.path(), &row.id).await.is_ok());
}

#[tokio::test]
async fn list_does_not_create_or_migrate_a_database() {
    let dir = tempfile::tempdir().unwrap();
    assert!(list(dir.path(), None).await.is_err());
    assert!(!dir.path().join("channel_ingress.db").exists());
}

#[tokio::test]
async fn only_a_current_admin_can_decide_and_channels_cannot() {
    assert!(crate::approval_notify::is_dashboard_only_kind(ACTION_KIND));
    let dir = tempfile::tempdir().unwrap();
    let row = uncertain_event(dir.path(), "e4").await;
    let req = close(&row.id, "verified on the LINE OA manager");
    let CliOutcome::Requested(id) = request_or_apply(dir.path(), &req).await.unwrap() else {
        panic!()
    };
    let handler = crate::handlers::MethodHandler::new(dir.path().to_path_buf()).await;
    let admin = duduclaw_auth::UserContext::admin_fallback();
    let mut employee = admin.clone();
    employee.role = duduclaw_auth::UserRole::Employee;
    let params = json!({"id": id, "approve": true});
    assert!(matches!(
        handler
            .handle("approvals.decide", params.clone(), &employee)
            .await,
        crate::protocol::WsFrame::Response { ok: false, .. }
    ));
    assert!(matches!(
        handler.handle("approvals.decide", params, &admin).await,
        crate::protocol::WsFrame::Response { ok: true, .. }
    ));
    assert!(matches!(
        request_or_apply(dir.path(), &req).await.unwrap(),
        CliOutcome::Applied(_)
    ));
}

#[tokio::test]
async fn fail_policy_refuses_a_terminal_rerun_before_filing_anything() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[channel_ingress]\nline_late_reply='fail'\n",
    )
    .unwrap();
    // Received at t=10: long past the reply window.
    let row = uncertain_event(dir.path(), "e5").await;
    let rerun = CliRequest {
        action: CliAction::Rerun,
        confirm_duplicate_risk: true,
        ..close(&row.id, "run it again")
    };
    let err = request_or_apply(dir.path(), &rerun).await.unwrap_err();
    assert!(err.contains("line_late_reply"), "{err}");
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    assert!(broker.list_by_kind(ACTION_KIND).await.unwrap().is_empty());
    // Closing is still possible.
    assert!(matches!(
        request_or_apply(dir.path(), &close(&row.id, "give up"))
            .await
            .unwrap(),
        CliOutcome::Requested(_)
    ));
}

#[tokio::test]
async fn terminal_views_never_print_raw_line_ids() {
    let dir = tempfile::tempdir().unwrap();
    let row = uncertain_event(dir.path(), "ids").await;
    for out in [
        list(dir.path(), None).await.unwrap(),
        show(dir.path(), &row.id).await.unwrap(),
    ] {
        let text = out.to_string();
        assert!(!text.contains("chat-ids"), "{text}");
        assert!(!text.contains("\"account\":\"account\""), "{text}");
        assert!(text.contains(&duduclaw_core::truncate_chars(&row.id, 12).to_string()));
    }
}
