//! P2-A C2 store invariants: fail-closed schema version, monotonic
//! revisions, terminal expiry, one open occurrence, idempotent facts.

use super::*;
use crate::task_store::{ResponsibilityRow, WakeupRow, resp_ts};

fn resp_row(id: &str) -> ResponsibilityRow {
    let now = "2026-10-05T00:00:00Z".to_string();
    ResponsibilityRow {
        responsibility_id: id.into(),
        owner_agent_id: "alice".into(),
        created_by: "op".into(),
        objective: "o".into(),
        acceptance_template: "a".into(),
        scope_json: "{}".into(),
        source_refs_json: "[]".into(),
        notification_policy_json: "{}".into(),
        schedule_json: None,
        occurrence_hours: 4,
        occurrence_cost_cap_cents: 10,
        budget_period: "day".into(),
        budget_timezone: "UTC".into(),
        period_cost_limit_cents: 100,
        period_occurrence_limit: 5,
        min_wake_interval_secs: 300,
        max_consecutive_failures: 3,
        stop_at: "2026-10-10T00:00:00Z".into(),
        state: "active".into(),
        state_reason: None,
        state_changed_by: None,
        state_changed_at: None,
        contract_revision: 1,
        contract_hash: "h".into(),
        control_epoch: 1,
        consecutive_failures: 0,
        last_occurrence_at: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

fn time_wakeup(resp: &str, due: &str) -> WakeupRow {
    WakeupRow {
        wakeup_id: uuid::Uuid::new_v4().to_string(),
        responsibility_id: resp.into(),
        control_epoch: 1,
        kind: "time".into(),
        recurring: false,
        due_at: Some(due.into()),
        event_name: None,
        event_filter_json: None,
        approval_id: None,
        armed_by: "operator:op".into(),
        state: "armed".into(),
        created_at: due.into(),
        updated_at: due.into(),
        armed_after_event_id: None,
    }
}

#[test]
fn unknown_schema_version_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    drop(TaskStore::open(dir.path()).unwrap());
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE responsibility_schema_meta SET version = 2 WHERE owner = 'p2a'",
        [],
    )
    .unwrap();
    drop(conn);
    let err = TaskStore::open(dir.path()).err().expect("must refuse");
    assert!(err.contains("not supported"), "{err}");
}

#[tokio::test]
async fn revisions_never_rewind_and_expiry_is_terminal() {
    let (store, dir) = temp_store();
    store
        .insert_responsibility(&resp_row("r1"), &[], None)
        .await
        .unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    assert!(
        conn.execute(
            "UPDATE responsibilities SET control_epoch = 0 WHERE responsibility_id='r1'",
            []
        )
        .is_err()
    );
    conn.execute(
        "UPDATE responsibilities SET state='expired' WHERE responsibility_id='r1'",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE responsibilities SET state='active' WHERE responsibility_id='r1'",
            []
        )
        .is_err()
    );
}

#[tokio::test]
async fn facts_are_idempotent_and_stale_epochs_write_nothing() {
    let (store, _dir) = temp_store();
    store
        .insert_responsibility(&resp_row("r1"), &[], None)
        .await
        .unwrap();
    let w = time_wakeup("r1", "2026-10-05T01:00:00Z");
    store.arm_wakeup(&w, None).await.unwrap();
    let now = chrono::Utc::now();
    let fire = crate::task_store::NewFire {
        wakeup_id: w.wakeup_id.clone(),
        fire_key: "t:x".into(),
        reason: "time".into(),
        data_json: None,
        guard_flags_json: None,
        dropped: None,
    };
    assert!(store.record_fire(&fire, None, now).await.unwrap());
    assert!(
        !store.record_fire(&fire, None, now).await.unwrap(),
        "same key twice"
    );
    store
        .disable_responsibility("r1", 1, "op", "off", now)
        .await
        .unwrap();
    let mut other = fire.clone();
    other.fire_key = "t:y".into();
    assert!(
        !store.record_fire(&other, None, now).await.unwrap(),
        "cancelled subscription"
    );
    // A wake-up armed for an old epoch is refused outright.
    assert_eq!(
        store
            .arm_wakeup(&time_wakeup("r1", "2026-10-06T01:00:00Z"), None)
            .await
            .unwrap_err(),
        "epoch_changed"
    );
    let _ = resp_ts(now);
}

#[tokio::test]
async fn one_open_occurrence_is_enforced_by_the_index() {
    let (store, dir) = temp_store();
    store
        .insert_responsibility(&resp_row("r1"), &[], None)
        .await
        .unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let ins = |key: &str, task: &str| {
        conn.execute(
            "INSERT INTO responsibility_occurrences (responsibility_id, occurrence_key, task_id,
                    contract_revision, control_epoch, period_key, reserved_cents, created_at)
             VALUES ('r1', ?1, ?2, 1, 1, 'p', 10, 'now')",
            rusqlite::params![key, task],
        )
    };
    ins("k1", "t1").unwrap();
    assert!(ins("k2", "t2").is_err(), "second open occurrence refused");
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn responsibility_contracts_and_wakeup_filters_refuse_credentials() {
    let (store, _dir) = temp_store();
    let mut row = resp_row("credential-responsibility");
    row.objective = "password: fixture-objective".into();
    assert!(store.insert_responsibility(&row, &[], None).await.is_err());
    assert!(store.get_responsibility(&row.responsibility_id).await.unwrap().is_none());
    row.objective = "Safe objective".into();
    store.insert_responsibility(&row, &[], None).await.unwrap();
    let mut wakeup = time_wakeup(&row.responsibility_id, "2026-10-05T01:00:00Z");
    wakeup.event_filter_json = Some(r#"{"password":"fixture-filter"}"#.into());
    assert!(store.arm_wakeup(&wakeup, None).await.is_err());
    assert!(store.list_wakeups(&row.responsibility_id).await.unwrap().is_empty());
    let contract = crate::task_store::ResponsibilityContract {
        objective: row.objective.clone(), acceptance_template: row.acceptance_template.clone(),
        scope_json: r#"{"password":"fixture-contract"}"#.into(),
        source_refs_json: row.source_refs_json.clone(),
        notification_policy_json: row.notification_policy_json.clone(), schedule_json: None,
        occurrence_hours: row.occurrence_hours, occurrence_cost_cap_cents: row.occurrence_cost_cap_cents,
        budget_period: row.budget_period.clone(), budget_timezone: row.budget_timezone.clone(),
        period_cost_limit_cents: row.period_cost_limit_cents,
        period_occurrence_limit: row.period_occurrence_limit,
        min_wake_interval_secs: row.min_wake_interval_secs,
        max_consecutive_failures: row.max_consecutive_failures, stop_at: row.stop_at.clone(),
        contract_hash: "unchanged-authority".into(),
    };
    assert!(store.update_responsibility_contract(&row.responsibility_id, 1, &contract, None,
        chrono::Utc::now()).await.is_err());
    let stored = store.get_responsibility(&row.responsibility_id).await.unwrap().unwrap();
    assert_eq!(stored.contract_hash, row.contract_hash);
    assert_eq!(stored.contract_revision, 1);
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn wake_facts_mask_nested_data_and_reasons() {
    let (store, _dir) = temp_store();
    let row = resp_row("credential-facts");
    let wakeup = time_wakeup(&row.responsibility_id, "2026-10-05T01:00:00Z");
    store.insert_responsibility(&row, &[wakeup.clone()], None).await.unwrap();
    let mut fire = crate::task_store::NewFire {
        wakeup_id: wakeup.wakeup_id, fire_key: "fixture-fact".into(),
        reason: "password: fixture-reason".into(),
        data_json: Some(r#"{"nested":{"password":"fixture-data"}}"#.into()),
        guard_flags_json: Some(r#"{"note":"token: fixture-flags"}"#.into()),
        dropped: Some("password: fixture-drop".into()),
    };
    // The event reason is a closed enum, not a free-text masking sink.
    assert!(store.record_fire(&fire, None, chrono::Utc::now()).await.is_err());
    assert!(store.list_fires(&row.responsibility_id, 10).await.unwrap().is_empty());
    fire.reason = "time".into();
    assert!(store.record_fire(&fire, None, chrono::Utc::now()).await.unwrap());
    let facts = store.list_fires(&row.responsibility_id, 10).await.unwrap();
    let stored = &facts[0];
    assert_eq!(stored.reason, "time");
    assert!(!stored.data_json.as_deref().unwrap().contains("fixture-data"));
    assert!(!stored.guard_flags_json.as_deref().unwrap().contains("fixture-flags"));
    assert!(!stored.drop_reason.as_deref().unwrap().contains("fixture-drop"));
}

// Written for F1; not run (the review task prohibits builds/tests).
#[tokio::test]
async fn steering_refuses_credentials_without_rewriting_body_hash() {
    let (store, _dir) = temp_store();
    let task = goal_review_task("credential-steering");
    store.insert_task(&task).await.unwrap();
    let steering = crate::task_store::NewSteering {
        task_id: &task.id, body: "password: fixture-steering", body_hash: "bound-hash",
        guard_flags_json: "[]", submitted_by: "operator", client_request_id: "request-1",
    };
    assert!(store.submit_steering(&steering, chrono::Utc::now()).await.is_err());
    assert!(store.list_steering(&task.id).await.unwrap().is_empty());
    assert_eq!(steering.body_hash, "bound-hash");
}
