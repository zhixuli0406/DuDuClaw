//! Round 2 ruling 8: a Team-as-Agent round never touches the message queue,
//! so the stop report reads the team signals. Seen running ⇒ `cancel_pending`;
//! possibly running but not visible from here ⇒ held until the lease/hold
//! window ends, then `stopped_uncertain` — never a plain `stopped`.

use super::super::stop::{stop_status, stop_task};
use super::super::team_activity::{RoundGuard, pin_registry_live_for_test};
use super::*;
use crate::approval::ApprovalBroker;

async fn stop(env: &Env, id: &str, now: DateTime<Utc>) -> super::super::stop::StopStatus {
    let rev = task(env, id).await.authority_revision;
    let broker = ApprovalBroker::open(env.home()).unwrap();
    stop_task(
        &env.store,
        &env.queue,
        Some(&broker),
        None,
        env.home(),
        id,
        rev,
        "op",
        false,
        now,
    )
    .await
    .unwrap()
}

async fn status(env: &Env, id: &str, now: DateTime<Utc>) -> super::super::stop::StopStatus {
    let broker = ApprovalBroker::open(env.home()).unwrap();
    stop_status(&env.store, &env.queue, Some(&broker), None, id, now)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn registered_team_round_keeps_the_stop_pending() {
    pin_registry_live_for_test(Some(true));
    let env = Env::new();
    goal_task(&env, "team-reg", "in_progress").await;
    let guard = RoundGuard::register("team-reg");
    let st = stop(&env, "team-reg", t0()).await;
    assert_eq!(st.state, "cancel_pending");
    assert_eq!(st.detail.team_rounds_running, 1);
    drop(guard);
    let fin = status(&env, "team-reg", t0() + Duration::seconds(5)).await;
    assert_eq!(fin.state, "stopped", "{fin:?}");
    pin_registry_live_for_test(None);
}

#[tokio::test]
async fn role_member_scaffold_on_disk_keeps_the_stop_pending() {
    pin_registry_live_for_test(Some(true));
    let env = Env::new();
    goal_task(&env, "team-disk", "in_progress").await;
    let member = env.home().join("agents").join(".ephemeral").join("role-x");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(
        member.join("agent.toml"),
        "[team_member]\nrole = \"executor\"\ntask_id = \"team-disk\"\nround = 1\nparent = \"alice\"\n",
    )
    .unwrap();
    let st = stop(&env, "team-disk", t0()).await;
    assert_eq!(st.state, "cancel_pending");
    assert_eq!(st.detail.team_rounds_running, 1);
    std::fs::remove_dir_all(&member).unwrap();
    let fin = status(&env, "team-disk", t0() + Duration::seconds(5)).await;
    assert_eq!(fin.state, "stopped", "{fin:?}");
    pin_registry_live_for_test(None);
}

/// No live registry in this process (e.g. the stop was requested from a
/// process that did not dispatch the round) and the task has a frozen team
/// spec: possibly running, unverifiable. Held, then `stopped_uncertain`.
#[tokio::test]
async fn unverifiable_team_round_is_held_then_reported_uncertain() {
    pin_registry_live_for_test(Some(false));
    let env = Env::new();
    let mut t = TaskRow::new(
        "team-unk".into(),
        "t".into(),
        "w".into(),
        "medium".into(),
        OWNER.into(),
        "system".into(),
    );
    t.status = "in_progress".into();
    t.goal_mode = true;
    t.team_spec_json = Some("{}".into());
    env.store.insert_task(&t).await.unwrap();
    let st = stop(&env, "team-unk", t0()).await;
    assert_eq!(st.state, "cancel_pending", "{st:?}");
    assert!(st.detail.unverified_work_possible);
    let hold = st.detail.unverified_hold_until.clone().expect("hold until");
    assert_eq!(
        hold,
        crate::task_store::resp_ts(
            t0() + Duration::seconds(super::super::stop::UNVERIFIED_WORK_HOLD_SECS)
        )
    );
    let still = status(&env, "team-unk", t0() + Duration::minutes(30)).await;
    assert_eq!(still.state, "cancel_pending");
    let fin = status(&env, "team-unk", t0() + Duration::hours(2)).await;
    assert_eq!(
        fin.state, "stopped_uncertain",
        "never claims more than it can prove"
    );
    pin_registry_live_for_test(None);
}
