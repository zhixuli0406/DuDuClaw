use std::path::Path;

use duduclaw_auth::{AccessLevel, UserDb, UserRole};

use super::*;
use crate::assign_schedule::{AssignKind, AssignPayload};

/// mgr (Manager) → lead (Manager) → emp1 (Owner of sales-rep);
/// peer (Manager, no reports); admin.
pub(crate) struct Org {
    pub mgr: String,
    pub lead: String,
    pub peer: String,
    pub admin: String,
}

pub(crate) fn org(home: &Path) -> Org {
    crate::team_edition::record(home, duduclaw_core::EditionProfile::Enterprise);
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let mk = |name: &str, role| {
        db.create_user(&format!("{name}@test.local"), name, "TeamPass-12345", role)
            .unwrap()
            .id
    };
    let mgr = mk("mgr", UserRole::Manager);
    let lead = mk("lead", UserRole::Manager);
    let emp1 = mk("emp1", UserRole::Employee);
    let peer = mk("peer", UserRole::Manager);
    let admin = mk("boss", UserRole::Admin);
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.bind_agent(&emp1, "sales-rep", AccessLevel::Owner)
        .unwrap();
    Org {
        mgr,
        lead,
        peer,
        admin,
    }
}

pub(crate) fn now() -> DateTime<Utc> {
    // Friday 2026-10-23 10:00 Taipei.
    "2026-10-23T02:00:00Z".parse().unwrap()
}

pub(crate) fn weekly_goal(text: &str) -> NewSchedule {
    NewSchedule {
        agent_id: "sales-rep".into(),
        kind: AssignKind::Goal,
        payload: AssignPayload {
            description: text.into(),
            ..Default::default()
        },
        spec: ScheduleSpec::Weekly {
            days: vec![1],
            hour: 9,
            minute: 0,
        },
        timezone: Some("Asia/Taipei".into()),
        created_via: "dashboard".into(),
        source: None,
    }
}

fn who(id: &str, admin: bool) -> Creator {
    Creator::new(id, admin).unwrap()
}

#[tokio::test]
async fn the_first_fire_is_computed_and_the_creator_is_the_account() {
    let dir = tempfile::tempdir().unwrap();
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let row = create(
        dir.path(),
        &store,
        &who(&o.mgr, false),
        weekly_goal("週報"),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(row.creator_user_id, o.mgr);
    assert_eq!(
        row.next_fire_at.as_deref(),
        Some("2026-10-26T01:00:00+00:00")
    );
    assert_eq!(row.status, "active");
    assert!(
        Creator::new("system", true).is_none(),
        "the admin token names nobody"
    );
}

#[tokio::test]
async fn non_admins_get_no_cron_no_risk_boundary_and_a_cap() {
    let dir = tempfile::tempdir().unwrap();
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let mgr = who(&o.mgr, false);
    let mut cron = weekly_goal("x");
    cron.spec = ScheduleSpec::Cron {
        expr: "0 9 * * 1".into(),
    };
    assert_eq!(
        create(dir.path(), &store, &mgr, cron.clone(), now())
            .await
            .unwrap_err(),
        CreateError::Spec(SpecError::CronAdminOnly)
    );
    assert!(
        create(dir.path(), &store, &who(&o.admin, true), cron, now())
            .await
            .is_ok()
    );
    let mut risky = weekly_goal("x");
    risky.payload.risk_boundary = Some("可以寄信".into());
    assert_eq!(
        create(dir.path(), &store, &mgr, risky, now())
            .await
            .unwrap_err(),
        CreateError::RiskBoundaryAdminOnly
    );
    let mut empty = weekly_goal("  ");
    empty.payload.description = "  ".into();
    assert_eq!(
        create(dir.path(), &store, &mgr, empty, now())
            .await
            .unwrap_err(),
        CreateError::Empty
    );

    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_defaults]\nassign_schedule_max_active = 2\n",
    )
    .unwrap();
    for i in 0..2 {
        create(
            dir.path(),
            &store,
            &mgr,
            weekly_goal(&format!("g{i}")),
            now(),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        create(dir.path(), &store, &mgr, weekly_goal("g3"), now())
            .await
            .unwrap_err(),
        CreateError::TooMany { max: 2 }
    );
}

#[tokio::test]
async fn only_the_creator_or_an_admin_controls_a_schedule() {
    let dir = tempfile::tempdir().unwrap();
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let row = create(
        dir.path(),
        &store,
        &who(&o.mgr, false),
        weekly_goal("週報"),
        now(),
    )
    .await
    .unwrap();
    let peer = who(&o.peer, false);
    assert_eq!(
        manage(
            dir.path(),
            &store,
            &peer,
            &row.id,
            ManageAction::Pause,
            now()
        )
        .await
        .unwrap_err(),
        ManageError::NotFound,
        "someone else's schedule reads as missing"
    );
    assert!(
        list(&store, &peer, true).await.unwrap().is_empty(),
        "a non-admin cannot list everyone's"
    );
    let paused = manage(
        dir.path(),
        &store,
        &who(&o.mgr, false),
        &row.id,
        ManageAction::Pause,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        (paused.status.as_str(), paused.pause_reason.as_deref()),
        ("paused", Some("by_user"))
    );
    let admin = who(&o.admin, true);
    assert_eq!(list(&store, &admin, true).await.unwrap().len(), 1);
    let resumed = manage(
        dir.path(),
        &store,
        &admin,
        &row.id,
        ManageAction::Resume,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.status, "active");
    manage(
        dir.path(),
        &store,
        &admin,
        &row.id,
        ManageAction::Remove,
        now(),
    )
    .await
    .unwrap();
    assert!(
        list(&store, &who(&o.mgr, false), false)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn resume_rechecks_the_creators_right_not_the_actors() {
    let dir = tempfile::tempdir().unwrap();
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let row = create(
        dir.path(),
        &store,
        &who(&o.mgr, false),
        weekly_goal("週報"),
        now(),
    )
    .await
    .unwrap();
    store
        .pause_assign_schedule(&row.id, "access_lost", now())
        .await
        .unwrap();
    // lead leaves mgr's team: mgr may no longer assign to sales-rep.
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    db.set_manager(&o.lead, None).unwrap();
    let admin = who(&o.admin, true);
    assert_eq!(
        manage(
            dir.path(),
            &store,
            &admin,
            &row.id,
            ManageAction::Resume,
            now()
        )
        .await
        .unwrap_err(),
        ManageError::CreatorLostAccess
    );
}

#[tokio::test]
async fn a_one_shot_whose_time_passed_cannot_resume() {
    let dir = tempfile::tempdir().unwrap();
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let mut once = weekly_goal("一次");
    once.spec = ScheduleSpec::Once {
        at: "2026-10-23T03:00:00+00:00".into(),
    };
    let row = create(dir.path(), &store, &who(&o.mgr, false), once, now())
        .await
        .unwrap();
    let mgr = who(&o.mgr, false);
    manage(
        dir.path(),
        &store,
        &mgr,
        &row.id,
        ManageAction::Pause,
        now(),
    )
    .await
    .unwrap();
    let later: DateTime<Utc> = "2026-10-23T04:00:00Z".parse().unwrap();
    assert_eq!(
        manage(
            dir.path(),
            &store,
            &mgr,
            &row.id,
            ManageAction::Resume,
            later
        )
        .await
        .unwrap_err(),
        ManageError::TimePassed
    );
}
