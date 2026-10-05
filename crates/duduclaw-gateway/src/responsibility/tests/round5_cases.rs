//! Round 5 (appendix F): the third review's findings that are not pinned
//! elsewhere (M3-1 lives in `round4_cases`, M3-3 in `dispatcher_fence_tests`,
//! M3-4 in the cli `round_parent_tests`, M3-5 in the agent heartbeat tests).

use super::*;
use crate::task_store::RespCas;

fn sql(env: &Env, stmt: &str, args: &[&dyn rusqlite::ToSql]) {
    let conn = rusqlite::Connection::open(env.home().join("tasks.db")).unwrap();
    conn.execute(stmt, args).unwrap();
}

fn applied(r: RespCas) -> ResponsibilityRow {
    match r {
        RespCas::Applied(row) => row,
        other => panic!("expected applied, got {other:?}"),
    }
}

/// Third review LOW: disable then enable must not reset the failure streak
/// (only `clear_failures`, which needs Manager, does). A streak at the limit
/// comes back paused for failures.
#[tokio::test]
async fn re_enabling_keeps_the_failure_streak() {
    let env = Env::new();
    let now = t0();
    let r = create(&env, &input(now), now).await;
    sql(
        &env,
        "UPDATE responsibilities SET consecutive_failures = max_consecutive_failures,
                state = 'failure_paused' WHERE responsibility_id = ?1",
        &[&r.responsibility_id],
    );
    let cur = env
        .store
        .get_responsibility(&r.responsibility_id)
        .await
        .unwrap()
        .unwrap();
    let off = applied(
        service::disable(
            &env.store,
            &r.responsibility_id,
            cur.control_epoch,
            "op",
            "test",
            now,
        )
        .await
        .unwrap(),
    );
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
    assert_eq!(on.consecutive_failures, cur.max_consecutive_failures);
    assert_eq!(on.state, "failure_paused");

    // Below the limit the responsibility comes back active, streak kept.
    sql(
        &env,
        "UPDATE responsibilities SET consecutive_failures = 1, state = 'active'
          WHERE responsibility_id = ?1",
        &[&r.responsibility_id],
    );
    let off = applied(
        service::disable(
            &env.store,
            &r.responsibility_id,
            on.control_epoch,
            "op",
            "t",
            now,
        )
        .await
        .unwrap(),
    );
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
    assert_eq!((on.state.as_str(), on.consecutive_failures), ("active", 1));
}
