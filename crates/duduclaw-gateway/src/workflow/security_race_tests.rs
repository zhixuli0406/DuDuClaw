//! Deterministic service races with separate SQLite connections and real stdio fixtures.
//! The proxy transport is test-owned; this is not external-provider SLA evidence.
use super::pilot_test_factory::Pilot;
use super::*;
use crate::approval::{ApprovalId, CURRENT_DECISION_CONTEXT};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

struct Pause {
    point: &'static str,
    reached: Notify,
    resume: Notify,
}
tokio::task_local! { static PAUSE: Arc<Pause>; }

/// Test processes started with this variable abort at the named checkpoint.
const CRASH_AT_ENV: &str = "DUDUCLAW_TEST_CRASH_AT";

// Task-local so unrelated parallel activations cannot consume this barrier.
pub(super) async fn checkpoint(point: &'static str) {
    if std::env::var(CRASH_AT_ENV).as_deref() == Ok(point) {
        std::process::abort();
    }
    if let Ok(pause) = PAUSE.try_with(Arc::clone) {
        if pause.point == point {
            pause.reached.notify_one();
            tokio::time::timeout(Duration::from_secs(30), pause.resume.notified())
                .await
                .expect("revoker did not release activation checkpoint");
        }
    }
}

/// Run `work` until it reaches `point`, run `during` while it waits there,
/// then let it continue. Used by races driven through the real runner.
pub(super) async fn race_at<T, U>(
    point: &'static str,
    work: impl std::future::Future<Output = T>,
    during: impl std::future::Future<Output = U>,
) -> (T, U) {
    let pause = Arc::new(Pause {
        point,
        reached: Notify::new(),
        resume: Notify::new(),
    });
    let paused = PAUSE.scope(pause.clone(), work);
    let side = async {
        tokio::time::timeout(Duration::from_secs(30), pause.reached.notified())
            .await
            .expect("work did not reach the requested checkpoint");
        let out = during.await;
        pause.resume.notify_one();
        out
    };
    tokio::join!(paused, side)
}

async fn request(pilot: &Pilot) -> ActivationRequest {
    let mut results = Vec::new();
    for index in 0..5 {
        let evidence = CURRENT_DECISION_CONTEXT
            .scope(
                Some(pilot.context.clone()),
                pilot.service.run_fixture(pilot.fixture(index)),
            )
            .await
            .unwrap();
        let assertions = crate::workflow_drafts::evaluate_fixture(
            &pilot.draft.fixtures[index].assertions,
            &evidence,
        );
        assert!(
            assertions
                .iter()
                .all(|a| a.outcome == AssertionOutcome::Matched),
            "fixture {index}: {assertions:?}"
        );
        results.push(evidence);
    }
    pilot.activation_request(results)
}

async fn human_accept(pilot: &Pilot, request: &ActivationRequest) {
    let id = CURRENT_DECISION_CONTEXT
        .scope(
            Some(pilot.context.clone()),
            pilot
                .service
                .request_activation(request.clone(), pilot.binding(request)),
        )
        .await
        .unwrap();
    let principal = crate::review_evidence::audience::trusted_dashboard_principal(
        pilot.home.path(),
        &pilot.context.principal_id,
    )
    .unwrap();
    pilot
        .service
        .broker
        .decide_bound_dashboard(&ApprovalId::from(id), &principal, true)
        .await
        .unwrap();
}

async fn assert_fenced(
    pilot: &Pilot,
    service: &WorkflowService,
    request: &ActivationRequest,
    before: usize,
    premint: bool,
) {
    let id = &request.activation_id;
    assert_eq!(
        service.activation(id).await.unwrap().unwrap().state,
        ActivationState::Revoked
    );
    match service.broker.current_revision_grant(id).await.unwrap() {
        None => assert!(premint),
        Some((reference, _, state)) => {
            assert!(
                !premint,
                "premint revoke must prohibit creation of any grant"
            );
            assert_eq!(state, "revoked");
            assert_eq!(reference.epoch, 3, "prepare 1, activate 2, revoke 3");
        }
    }
    let ledger = rusqlite::Connection::open(pilot.home.path().join("approvals.db")).unwrap();
    let counts: (i64, i64) = ledger
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(state='active'),0) FROM workflow_revision_grants WHERE activation_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (if premint { 0 } else { 1 }, 0));
    let tombstones: i64 = ledger
        .query_row(
            "SELECT COUNT(*) FROM workflow_activation_revocations WHERE activation_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstones, 1);
    let cron = crate::cron_store::CronStore::open(pilot.home.path())
        .unwrap()
        .get(&request.cron.as_ref().unwrap().cron_id)
        .await
        .unwrap();
    assert!(cron.is_none_or(|row| !row.enabled));
    assert_eq!(
        service
            .enqueue_trigger(
                id,
                Trigger::Manual {
                    request_id: "after-revoke".into()
                },
                json!({})
            )
            .await
            .unwrap_err(),
        "workflow activation not active"
    );
    assert_eq!(
        service.commit_activation(id).await.unwrap_err(),
        "activation no longer eligible"
    );
    let queued: i64 = service
        .store
        .with_connection(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM workflow_outbox WHERE kind='enqueue'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(queued, 0);
    assert_eq!(
        pilot.call_count(),
        before,
        "no post-revocation HTTP handler invocation"
    );
}

async fn commit_race(point: &'static str) {
    let pilot = Pilot::new().await;
    let request = request(&pilot).await;
    human_accept(&pilot, &request).await;
    let before = pilot.call_count();
    let premint = matches!(point, "prepared_read" | "current_none" | "before_prepare");
    let pause = Arc::new(Pause {
        point,
        reached: Notify::new(),
        resume: Notify::new(),
    });
    let committer = pilot.reopen();
    let revoker = pilot.reopen();
    assert!(!Arc::ptr_eq(&committer.broker, &revoker.broker));
    let id = &request.activation_id;
    let commit = PAUSE.scope(pause.clone(), committer.commit_activation(id));
    let revoke = async {
        tokio::time::timeout(Duration::from_secs(30), pause.reached.notified())
            .await
            .expect("commit did not reach requested checkpoint");
        let revoked = revoker
            .revoke_activation(id, "deterministic service race")
            .await
            .unwrap();
        assert_eq!(revoked.state, ActivationState::Revoked);
        pause.resume.notify_one();
        revoked
    };
    let (result, revoked) = tokio::join!(commit, revoke);
    let error = result.unwrap_err();
    assert_eq!(
        error,
        if premint {
            "workflow activation authoritatively revoked"
        } else {
            "workflow grant missing or changed"
        }
    );
    assert_fenced(&pilot, &revoker, &request, before, premint).await;
    drop(committer);
    drop(revoker);
    let reopened = pilot.reopen();
    reopened.reconcile_queue_outbox().await.unwrap();
    assert_fenced(&pilot, &reopened, &request, before, premint).await;
    pilot.record_evidence(
        point,
        json!({"request":request,"revoked":revoked,
        "commit_error":error,"post_revoke_http_calls":pilot.call_count()-before,
        "reopen_fenced":true,"separate_broker_connections":true}),
    );
}

#[tokio::test]
async fn security_race_prepared_read_revoke_fences_premint() {
    commit_race("prepared_read").await;
}
#[tokio::test]
async fn security_race_current_none_revoke_fences_premint() {
    commit_race("current_none").await;
}
#[tokio::test]
async fn security_race_before_prepare_revoke_fences_premint() {
    commit_race("before_prepare").await;
}
#[tokio::test]
async fn security_race_before_cron_enable_revoke_compensates() {
    commit_race("before_cron_enable").await;
}
#[tokio::test]
async fn security_race_after_cron_enable_revoke_compensates() {
    commit_race("after_cron_enable").await;
}

#[tokio::test]
async fn security_race_request_approval_write_cannot_revive_terminal_projection() {
    let pilot = Pilot::new().await;
    let request = request(&pilot).await;
    let before = pilot.call_count();
    let pause = Arc::new(Pause {
        point: "request_prepared",
        reached: Notify::new(),
        resume: Notify::new(),
    });
    let revoker = pilot.reopen();
    let pending = CURRENT_DECISION_CONTEXT.scope(
        Some(pilot.context.clone()),
        PAUSE.scope(
            pause.clone(),
            pilot
                .service
                .request_activation(request.clone(), pilot.binding(&request)),
        ),
    );
    let revoke = async {
        tokio::time::timeout(Duration::from_secs(30), pause.reached.notified())
            .await
            .unwrap();
        revoker
            .revoke_activation(&request.activation_id, "request write race")
            .await
            .unwrap();
        pause.resume.notify_one();
    };
    let (result, ()) = tokio::join!(pending, revoke);
    assert_eq!(
        result.unwrap_err(),
        "activation projection terminal or changed"
    );
    let reopened = pilot.reopen();
    assert_eq!(
        CURRENT_DECISION_CONTEXT
            .scope(
                Some(pilot.context.clone()),
                reopened.request_activation(request.clone(), pilot.binding(&request))
            )
            .await
            .unwrap_err(),
        "activation projection terminal or changed"
    );
    assert_fenced(&pilot, &reopened, &request, before, true).await;
    pilot.record_evidence(
        "request-prepared-revoke",
        json!({"request":request,"reopen_fenced":true}),
    );
}

mod boot_crash;
