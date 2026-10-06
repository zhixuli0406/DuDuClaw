//! Round 3 (appendix D), continued: answers to `responsibility_ask` and
//! settling after the feature is switched back on. Split from
//! `round3_cases.rs` to keep each file under 800 lines.

use super::*;
use crate::approval::ApprovalBroker;

fn set_config(env: &Env, text: &str) {
    std::fs::write(env.home().join("config.toml"), text).unwrap();
}

fn sql(env: &Env, stmt: &str, args: &[&dyn rusqlite::ToSql]) {
    let conn = rusqlite::Connection::open(env.home().join("tasks.db")).unwrap();
    conn.execute(stmt, args).unwrap();
}

/// Coordinator decision 1: an answer to `responsibility_ask` (here from a
/// chat channel) is data that wakes one run. It changes no limit, no state,
/// and decides no other pending approval.
#[tokio::test]
async fn an_answer_is_data_and_never_an_approval() {
    use crate::approval::ApprovalStatus;
    use crate::responsibility::operator_gate::{self as gate, Gate, GateRequest, GatedAction};
    let env = Env::new();
    let now = t0();
    let resp = create(&env, &input(now), now).await;
    arm_time(&env, &resp, now).await;
    let occ = env.wake(3, now).await.created.remove(0);
    let broker = ApprovalBroker::open(env.home()).unwrap();
    let w = service::ask(
        &env.store,
        &broker,
        &resp.responsibility_id,
        OWNER,
        "這次要先處理 A 還是 B？",
        &["A".into(), "B".into()],
        600,
        None,
        now,
    )
    .await
    .unwrap();
    // An unrelated operator request waiting in the inbox.
    let before = env
        .store
        .get_responsibility(&resp.responsibility_id)
        .await
        .unwrap()
        .unwrap();
    let state = gate::state_fingerprint(Some(&before));
    let args = serde_json::json!({"reason": "x"});
    let req = GateRequest {
        action: GatedAction::Resume,
        target: &resp.responsibility_id,
        owner: OWNER,
        args: &args,
        state: &state,
        card: "card",
        valid_minutes: 30,
    };
    let Gate::Requested(other) = gate::gate(&broker, &req).await.unwrap() else {
        panic!("expected a request");
    };
    let aid = crate::approval::ApprovalId::from(w.approval_id.clone().unwrap());
    broker
        .decide(&aid, true, "channel:telegram:42")
        .await
        .unwrap();
    sql(
        &env,
        "UPDATE tasks SET status='done' WHERE id = ?1",
        &[&occ],
    );
    let ctx = WakeContext {
        home: env.home(),
        store: &env.store,
        broker: Some(&broker),
        cost: env.cost.as_ref(),
        free_slots: 0,
        notifier: None,
    };
    let r = wake_pass(&ctx, now + Duration::minutes(1)).await.unwrap();
    assert_eq!(r.decision_fires, 1, "{r:?}");
    let after = env
        .store
        .get_responsibility(&resp.responsibility_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.control_epoch, before.control_epoch);
    assert_eq!(after.contract_revision, before.contract_revision);
    assert_eq!(after.contract_hash, before.contract_hash);
    assert_eq!(
        after.occurrence_cost_cap_cents,
        before.occurrence_cost_cap_cents
    );
    assert_eq!(
        after.period_cost_limit_cents,
        before.period_cost_limit_cents
    );
    assert_eq!(
        after.period_occurrence_limit,
        before.period_occurrence_limit
    );
    assert_eq!(after.stop_at, before.stop_at);
    assert_eq!(
        broker.get(&other).await.unwrap().unwrap().status,
        ApprovalStatus::Pending,
        "the other request is untouched"
    );
    // The resume gate still waits for a dashboard decision.
    assert!(matches!(
        gate::gate(&broker, &req).await.unwrap(),
        Gate::Pending(_)
    ));
}

/// Docs check: a run that ends while the feature is off is settled by the
/// first pass after the feature is switched back on.
#[tokio::test]
async fn a_run_finished_while_off_is_settled_after_switching_on() {
    let env = Env::new();
    let now = t0();
    let resp = create(&env, &input(now), now).await;
    arm_time(&env, &resp, now).await;
    let occ = env.wake(3, now).await.created.remove(0);
    set_config(&env, &config_text(false, true));
    sql(
        &env,
        "UPDATE tasks SET status='done' WHERE id = ?1",
        &[&occ],
    );
    let r = env.wake(3, now + Duration::minutes(1)).await;
    assert!(!r.ran);
    assert_eq!(
        env.store
            .list_occurrences(&resp.responsibility_id)
            .await
            .unwrap()[0]
            .outcome,
        None
    );
    set_config(&env, &config_text(true, true));
    let r = env.wake(3, now + Duration::minutes(2)).await;
    assert_eq!(r.settled, 1);
    assert_eq!(
        env.store
            .list_occurrences(&resp.responsibility_id)
            .await
            .unwrap()[0]
            .outcome
            .as_deref(),
        Some("done")
    );
}
