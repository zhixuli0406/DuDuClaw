//! Round 2 rulings 5 and 6: disable only disarms, enable re-arms each
//! subscription with exactly its original filter and timeout (never a wider
//! one), unrestorable subscriptions stay disarmed and are flagged, and the
//! summary says which subscriptions repeat.

use serde_json::json;

use super::super::service;
use super::super::summary;
use super::*;
use crate::task_store::{RespCas, WakeupRow};

fn applied(c: RespCas) -> ResponsibilityRow {
    match c {
        RespCas::Applied(r) => r,
        RespCas::Conflict(r) => panic!("conflict: {r:?}"),
    }
}

fn filter() -> serde_json::Value {
    json!({"all": [{"field": "priority", "op": "eq", "value": "high"}]})
}

/// Schedule + one filtered event subscription with a timeout.
fn mixed_input(now: DateTime<Utc>, timeout: DateTime<Utc>) -> ResponsibilityInput {
    let mut i = input(now);
    i.event_subscriptions = vec![service::EventSubscription {
        event_name: "task.created".into(),
        filter: Some(filter()),
        timeout_at: Some(timeout),
    }];
    i
}

async fn current(env: &Env, r: &ResponsibilityRow) -> Vec<WakeupRow> {
    env.store
        .list_wakeups(&r.responsibility_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|w| w.control_epoch == r.control_epoch)
        .collect()
}

async fn disable_enable(env: &Env, r: &ResponsibilityRow, at: DateTime<Utc>) -> ResponsibilityRow {
    let off = applied(
        service::disable(
            &env.store,
            &r.responsibility_id,
            r.control_epoch,
            "op",
            "test",
            at,
        )
        .await
        .unwrap(),
    );
    assert_eq!(off.state, "disabled");
    assert!(
        current(env, &off).await.iter().all(|w| w.state != "armed"),
        "disable leaves nothing armed"
    );
    applied(
        service::enable(
            &env.store,
            &r.responsibility_id,
            off.control_epoch,
            "op",
            at,
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn enable_rearms_with_the_original_filter_and_timeout() {
    let env = Env::new();
    let now = t0();
    let timeout = now + Duration::days(2);
    let r = create(&env, &mixed_input(now, timeout), now).await;
    let before = current(&env, &r).await;
    let ev_before = before.iter().find(|w| w.kind == "event").unwrap().clone();

    let on = disable_enable(&env, &r, now + Duration::hours(1)).await;
    assert_eq!(on.state, "active");
    let after = current(&env, &on).await;
    let ev = after
        .iter()
        .find(|w| w.kind == "event")
        .expect("event copy");
    assert_eq!(ev.state, "armed");
    assert_eq!(ev.event_name, ev_before.event_name);
    assert_eq!(
        ev.event_filter_json, ev_before.event_filter_json,
        "same filter"
    );
    assert_eq!(ev.due_at, ev_before.due_at, "same timeout");
    let tm = after
        .iter()
        .find(|w| w.kind == "time" && w.recurring)
        .expect("schedule copy");
    assert_eq!(tm.state, "armed");
    assert!(
        tm.due_at.as_deref().unwrap()
            > crate::task_store::resp_ts(now + Duration::hours(1)).as_str()
    );

    let s = summary::summary(&env.store, env.cost.as_ref(), &on.responsibility_id, now)
        .await
        .unwrap()
        .unwrap();
    assert!(
        s.subscriptions.iter().all(|v| !v.unrestored),
        "{:?}",
        s.subscriptions
    );
}

#[tokio::test]
async fn subscription_past_its_timeout_stays_disarmed_and_is_flagged() {
    let env = Env::new();
    let now = t0();
    let r = create(&env, &mixed_input(now, now + Duration::hours(1)), now).await;
    let on = disable_enable(&env, &r, now + Duration::hours(2)).await;
    let after = current(&env, &on).await;
    let ev = after
        .iter()
        .find(|w| w.kind == "event")
        .expect("event copy kept");
    assert_eq!(ev.state, "cancelled", "not armed");
    assert!(
        after
            .iter()
            .all(|w| !(w.kind == "event" && w.state == "armed")),
        "no event subscription armed in its place"
    );
    let s = summary::summary(&env.store, env.cost.as_ref(), &on.responsibility_id, now)
        .await
        .unwrap()
        .unwrap();
    let view = s.subscriptions.iter().find(|v| v.kind == "event").unwrap();
    assert!(view.unrestored);
}

/// A filter that no longer validates is never re-armed — in particular not
/// as an unfiltered (wider) subscription.
#[tokio::test]
async fn unreadable_filter_is_never_armed_unfiltered() {
    let env = Env::new();
    let now = t0();
    let r = create(&env, &mixed_input(now, now + Duration::days(2)), now).await;
    let off = applied(
        service::disable(
            &env.store,
            &r.responsibility_id,
            r.control_epoch,
            "op",
            "t",
            now,
        )
        .await
        .unwrap(),
    );
    let conn = rusqlite::Connection::open(env.home().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE task_wakeups SET event_filter_json = '[\"not\", \"a\", \"tree\"]'
          WHERE responsibility_id = ?1 AND kind = 'event'",
        [&r.responsibility_id],
    )
    .unwrap();
    let on = applied(
        service::enable(
            &env.store,
            &r.responsibility_id,
            off.control_epoch,
            "op",
            now,
        )
        .await
        .unwrap(),
    );
    let after = current(&env, &on).await;
    let ev = after.iter().find(|w| w.kind == "event").unwrap();
    assert_eq!(ev.state, "cancelled");
    assert!(
        after
            .iter()
            .all(|w| !(w.kind == "event" && w.state == "armed" && w.event_filter_json.is_none()))
    );
}

#[tokio::test]
async fn summary_says_which_subscriptions_repeat() {
    let env = Env::new();
    let now = t0();
    let r = create(&env, &mixed_input(now, now + Duration::days(2)), now).await;
    arm_time(&env, &r, now + Duration::hours(3)).await;
    let s = summary::summary(&env.store, env.cost.as_ref(), &r.responsibility_id, now)
        .await
        .unwrap()
        .unwrap();
    let schedule = s
        .subscriptions
        .iter()
        .find(|v| v.kind == "time" && v.repeats);
    assert!(
        schedule.is_some(),
        "schedule repeats: {:?}",
        s.subscriptions
    );
    let ev = s.subscriptions.iter().find(|v| v.kind == "event").unwrap();
    assert!(
        ev.repeats,
        "operator event subscription stays armed after a wake"
    );
    let one_shot = s
        .subscriptions
        .iter()
        .filter(|v| v.kind == "time" && !v.repeats)
        .count();
    assert_eq!(one_shot, 1, "the one-shot time wake-up is not repeating");
    assert_eq!(s.cost_not_counted, summary::COST_NOT_COUNTED.to_vec());
}

/// Ruling 1 (fence conditions): an intent recorded while the responsibility
/// was runnable is refused at the fence once the responsibility has
/// expired, and no new intent can be begun for that occurrence.
#[tokio::test]
async fn fence_refuses_a_round_of_an_expired_responsibility() {
    let env = Env::new();
    let now = t0();
    let r = create(&env, &input(now), now).await;
    arm_time(&env, &r, now).await;
    let occ = env.wake(3, now).await.created.remove(0);
    let crate::task_store::IntentBegin::Ready { intent, .. } =
        env.store.begin_dispatch_intent(&occ, 1, now).await.unwrap()
    else {
        panic!("intent refused while runnable");
    };
    env.store
        .fence_goal_dispatch(&intent.intent_id)
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(env.home().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE responsibilities SET state = 'expired' WHERE responsibility_id = ?1",
        [&r.responsibility_id],
    )
    .unwrap();
    let err = env
        .store
        .fence_goal_dispatch(&intent.intent_id)
        .await
        .unwrap_err();
    assert!(
        err.starts_with(crate::responsibility::FENCE_ERROR_PREFIX),
        "{err}"
    );
    assert!(err.contains("responsibility_not_runnable"), "{err}");
    assert!(matches!(
        env.store.begin_dispatch_intent(&occ, 2, now).await.unwrap(),
        crate::task_store::IntentBegin::Refused(_)
    ));
}

/// The operator CLI's widening requests are decidable only in the dashboard.
#[test]
fn operator_change_requests_are_dashboard_only() {
    assert!(crate::approval_notify::is_dashboard_only_kind(
        super::super::operator_gate::ACTION_KIND
    ));
}
