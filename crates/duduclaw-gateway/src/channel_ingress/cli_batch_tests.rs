use super::*;
use crate::approval::{ApprovalId, ApprovalStatus};
use crate::channel_ingress::AcceptedEvent;

async fn uncertain(home: &Path, event_id: &str) -> IngressRow {
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
                payload: "{}".into(),
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

fn close_all(note: &str) -> BatchRequest {
    BatchRequest {
        action: CliAction::Close,
        status: "uncertain".into(),
        reason: Some("dispatch_receipt_missing".into()),
        note: note.into(),
        confirm_duplicate_risk: false,
        limit: DEFAULT_BATCH,
    }
}

#[tokio::test]
async fn one_approval_closes_the_unchanged_events_and_reports_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [
        uncertain(dir.path(), "b1").await,
        uncertain(dir.path(), "b2").await,
        uncertain(dir.path(), "b3").await,
    ];
    let req = close_all("incident 10-05: answers confirmed by phone");
    let CliOutcome::Requested(id) = batch_request_or_apply(dir.path(), &req).await.unwrap() else {
        panic!("first run files one request");
    };
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    let rec = broker
        .get(&ApprovalId::from(id.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec.payload["count"], 3);
    assert_eq!(rec.payload["items"].as_array().unwrap().len(), 3);
    assert!(rec.summary.contains("共 3 則"));
    broker
        .decide(&ApprovalId::from(id), true, "dashboard:admin-1")
        .await
        .unwrap();
    // One event changes after the approval: it is skipped, not acted on.
    let store = IngressStore::open(dir.path()).unwrap();
    store
        .resolve(
            &rows[1].id,
            "r1",
            "rerun",
            true,
            "dashboard-admin",
            "rpc",
            20,
            rows[1].attempt,
        )
        .await
        .unwrap();
    let CliOutcome::Applied(report) = batch_request_or_apply(dir.path(), &req).await.unwrap()
    else {
        panic!("approved batch applies");
    };
    assert_eq!(report["applied"].as_array().unwrap().len(), 2);
    assert_eq!(report["skipped_changed"][0], short(&rows[1].id));
    assert_eq!(
        store.get(&rows[0].id).await.unwrap().unwrap().status,
        "closed"
    );
    assert_eq!(
        store.get(&rows[1].id).await.unwrap().unwrap().status,
        "ready"
    );
    assert_eq!(
        store.get(&rows[2].id).await.unwrap().unwrap().status,
        "closed"
    );
    // Consumed: nothing left to select and no approval to use.
    assert!(batch_request_or_apply(dir.path(), &req).await.is_err());
}

#[tokio::test]
async fn a_waiting_card_for_a_selection_that_changed_is_withdrawn_and_refiled() {
    let dir = tempfile::tempdir().unwrap();
    uncertain(dir.path(), "c1").await;
    let req = close_all("cleanup");
    let CliOutcome::Requested(first) = batch_request_or_apply(dir.path(), &req).await.unwrap()
    else {
        panic!()
    };
    // Same selection: the same card keeps waiting.
    let CliOutcome::Pending(again) = batch_request_or_apply(dir.path(), &req).await.unwrap() else {
        panic!()
    };
    assert_eq!(first, again);
    uncertain(dir.path(), "c2").await;
    let CliOutcome::Requested(second) = batch_request_or_apply(dir.path(), &req).await.unwrap()
    else {
        panic!()
    };
    assert_ne!(first, second);
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    let old = broker.get(&ApprovalId::from(first)).await.unwrap().unwrap();
    assert_ne!(
        old.status,
        ApprovalStatus::Pending,
        "old card withdrawn, not rewritten"
    );
    let new = broker
        .get(&ApprovalId::from(second))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new.payload["count"], 2);
}

#[tokio::test]
async fn invalid_batches_file_nothing() {
    let dir = tempfile::tempdir().unwrap();
    uncertain(dir.path(), "d1").await;
    let mut bad = close_all("n");
    bad.status = "completed".into();
    assert!(batch_request_or_apply(dir.path(), &bad).await.is_err());
    let mut bad = close_all("n");
    bad.action = CliAction::Rerun;
    assert!(
        batch_request_or_apply(dir.path(), &bad).await.is_err(),
        "needs confirmation"
    );
    let mut bad = close_all("n");
    bad.limit = MAX_BATCH + 1;
    assert!(batch_request_or_apply(dir.path(), &bad).await.is_err());
    let mut none = close_all("n");
    none.reason = Some("no_such_reason".into());
    assert!(batch_request_or_apply(dir.path(), &none).await.is_err());
    let broker = ApprovalBroker::open(dir.path()).unwrap();
    assert!(
        broker
            .list_by_kind(cli_approval::ACTION_KIND)
            .await
            .unwrap()
            .is_empty()
    );
}
