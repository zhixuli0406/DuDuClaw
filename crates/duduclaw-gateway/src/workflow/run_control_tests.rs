//! F1a run control without a CLI binary: failure classes, the breaker query,
//! cancellation finality and the queue wake-up namespace.
use super::run_control::*;
use super::*;
use crate::approval::payload_hash;
use chrono::Utc;
use rusqlite::params;
use serde_json::json;
use std::sync::Arc;

fn run(id: &str, status: RunStatus, class: Option<&str>, created_at: &str) -> WorkflowRun {
    let trigger = Trigger::Manual {
        request_id: id.into(),
    };
    WorkflowRun {
        run_id: id.into(),
        trigger_key: trigger.key("wf", 1).unwrap(),
        trigger,
        workflow_id: "wf".into(),
        revision: 1,
        workflow_hash: "h".into(),
        skill_hash: "s".into(),
        actor: "alice".into(),
        creator_grant: CreatorGrantSnapshot {
            actor: "alice".into(),
            allowed_tools: Default::default(),
            policy_revision: "p".into(),
        },
        audience: vec![],
        task: None,
        input: json!({}),
        input_hash: payload_hash(&json!({})),
        input_observed_at: created_at.into(),
        policy_revision: "p".into(),
        environment_hash: "e".into(),
        grant: None,
        activation_id: Some("act".into()),
        deadline_at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        budget: CostBudget {
            per_run_micros: 1,
            monthly_micros: 1,
            max_consecutive_failures: 3,
        },
        status,
        cost: CostBreakdown::default(),
        error_code: None,
        created_at: created_at.into(),
        decision_context: None,
        failure_class: class.map(str::to_string),
        cancelled_by: None,
    }
}

async fn insert(store: &WorkflowStore, run: &WorkflowRun) {
    let status = serde_json::to_value(run.status).unwrap();
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    run.run_id,
                    run.trigger_key,
                    run.workflow_id,
                    run.revision,
                    run.workflow_hash,
                    serde_json::to_string(run).unwrap(),
                    status.as_str().unwrap(),
                    run.created_at
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn counted(store: &WorkflowStore) -> Vec<String> {
    let sql = consecutive_failure_sql("");
    store
        .with_connection(|c| {
            let mut q = c.prepare(&sql).map_err(|e| e.to_string())?;
            q.query_map(params!["wf", "current", "act", 10], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap()
}

#[test]
fn transient_errors_are_infrastructure_only() {
    for error in [
        runner::WORKFLOW_BUSY,
        runner::WORKFLOW_LEASE_LOST,
        runner::WORKFLOW_SEGMENT_EXPIRED,
        runner::WORKFLOW_EFFECT_NOT_STARTED,
        "workflow_step_timeout",
        "workflow stdio unavailable: spawn failed",
        "query: database is locked",
    ] {
        assert!(is_transient_error(error), "{error}");
    }
    for error in [
        "workflow_policy_changed",
        "workflow_approval_denied",
        "workflow_effect_refused:-32003",
        "workflow_effect_timeout_outcome_unconfirmed",
        "effective workflow authority drift",
        "database is locked by policy text",
    ] {
        assert!(!is_transient_error(error), "{error}");
    }
}

#[test]
fn wake_up_ids_share_the_workflow_queue_namespace() {
    let (id, payload) = resume_outbox("run-1", "alice", "appr-1");
    assert_eq!(id, "workflow:run-1:resume:appr-1");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap(),
        json!({"run_id":"run-1","actor":"alice","approval_id":"appr-1"})
    );
    assert_eq!(enqueue_outbox_id("run-1"), "workflow:run-1");
}

#[tokio::test]
async fn breaker_ignores_transient_exhaustion_cancellation_and_reset_history() {
    let store = WorkflowStore::open_in_memory().unwrap();
    insert(
        &store,
        &run(
            "a",
            RunStatus::Blocked,
            Some(FAILURE_GATE),
            "2026-10-01T00:00:00+00:00",
        ),
    )
    .await;
    insert(
        &store,
        &run(
            "b",
            RunStatus::Blocked,
            Some(FAILURE_TRANSIENT),
            "2026-10-01T00:01:00+00:00",
        ),
    )
    .await;
    insert(
        &store,
        &run("c", RunStatus::Cancelled, None, "2026-10-01T00:02:00+00:00"),
    )
    .await;
    insert(
        &store,
        &run(
            "d",
            RunStatus::Failed,
            Some(FAILURE_DEFINITE),
            "2026-10-01T00:03:00+00:00",
        ),
    )
    .await;
    insert(
        &store,
        &run(
            "e",
            RunStatus::Uncertain,
            Some(FAILURE_UNCERTAIN),
            "2026-10-01T00:04:00+00:00",
        ),
    )
    .await;
    assert_eq!(
        counted(&store).await,
        vec!["uncertain", "failed", "blocked"]
    );
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_failure_resets VALUES('r','act','2026-10-01T00:03:30+00:00','admin','fixed')",
                [],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(counted(&store).await, vec!["uncertain"]);
    // Reset history is append-only.
    assert!(
        store
            .with_transaction(|tx| tx
                .execute("DELETE FROM workflow_failure_resets", [])
                .map_err(|e| e.to_string()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_run_is_final_for_every_writer() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    let store = Arc::new(WorkflowStore::open(home.path()).unwrap());
    let broker = Arc::new(crate::approval::ApprovalBroker::open(home.path()).unwrap());
    let service = WorkflowService::new(
        home.path().to_path_buf(),
        std::env::current_exe().unwrap(),
        store.clone(),
        broker,
    )
    .unwrap();
    let mut waiting = run(
        "w",
        RunStatus::WaitingApproval,
        None,
        &Utc::now().to_rfc3339(),
    );
    insert(&store, &waiting).await;
    let cancelled = service.cancel_run("w", "dashboard:op").await.unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(cancelled.error_code.as_deref(), Some(RUN_CANCELLED));
    assert_eq!(cancelled.cancelled_by.as_deref(), Some("dashboard:op"));
    // A runner that raced the cancellation cannot write the run back.
    waiting.status = RunStatus::Running;
    assert_eq!(
        service.runner.save_run(&waiting).await.unwrap_err(),
        RUN_CANCELLED
    );
    // Cancelling again reports the final state unchanged.
    let again = service.cancel_run("w", "dashboard:other").await.unwrap();
    assert_eq!(again.cancelled_by.as_deref(), Some("dashboard:op"));
    // The runner treats it as finished.
    assert_eq!(
        service.runner.execute("w").await.unwrap().status,
        RunStatus::Cancelled
    );
    // Fixture runs have no activation and are not cancellable here.
    let mut fixture = run("f", RunStatus::Running, None, &Utc::now().to_rfc3339());
    fixture.activation_id = None;
    insert(&store, &fixture).await;
    assert!(service.cancel_run("f", "dashboard:op").await.is_err());
}
