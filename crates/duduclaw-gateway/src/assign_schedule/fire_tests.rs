use std::sync::Arc;

use duduclaw_auth::{UserDb, UserStatus};
use tokio::sync::RwLock;

use super::*;
use crate::assign_schedule::manage::tests::{now, org, weekly_goal};
use crate::assign_schedule::manage::{Creator, create};

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// The fixture registry (dudu, finance, sales-rep) in a fresh home.
async fn home() -> (tempfile::TempDir, Arc<RwLock<AgentRegistry>>) {
    let (dir, reg) = crate::trigger_route::tests::registry(false).await;
    (dir, Arc::new(RwLock::new(reg)))
}

fn audit_kinds(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| {
            v.get("event_type")
                .and_then(|e| e.as_str())
                .map(str::to_string)
        })
        .filter(|k| k.starts_with("assign_schedule"))
        .collect()
}

const MONDAY: &str = "2026-10-26T01:00:00+00:00";

#[tokio::test]
async fn bad_config_refuses_create_resume_and_leaves_due_slots_untouched() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let creator = Creator::new(&o.mgr, false).unwrap();
    let row = create(
        dir.path(),
        &store,
        &creator,
        weekly_goal("keep the slot"),
        now(),
    )
    .await
    .unwrap();
    for text in [
        "[goal_defaults",
        "[goal_defaults]\nassign_schedule_min_interval_minutes = false",
    ] {
        std::fs::write(dir.path().join("config.toml"), text).unwrap();
        assert!(
            create(dir.path(), &store, &creator, weekly_goal("refused"), now())
                .await
                .is_err()
        );
        assert!(tick(dir.path(), &reg, at(MONDAY)).await.is_err());
        let saved = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
        assert_eq!(saved.next_fire_at, row.next_fire_at);
        assert_eq!(saved.status, "active");
        assert_eq!(saved.fire_count, 0);
    }
    // Narrowing remains possible even while the limits cannot be read.
    crate::assign_schedule::manage::manage(
        dir.path(),
        &store,
        &creator,
        &row.id,
        crate::assign_schedule::ManageAction::Pause,
        now(),
    )
    .await
    .unwrap();
    assert!(
        crate::assign_schedule::manage::manage(
            dir.path(),
            &store,
            &creator,
            &row.id,
            crate::assign_schedule::ManageAction::Resume,
            now()
        )
        .await
        .is_err()
    );
    assert_eq!(
        crate::assign_schedule::manage::list(&store, &creator, false)
            .await
            .unwrap()
            .len(),
        1
    );
    crate::assign_schedule::manage::manage(
        dir.path(),
        &store,
        &creator,
        &row.id,
        crate::assign_schedule::ManageAction::Remove,
        now(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_due_snapshot_cannot_create_after_pause_or_remove() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let other = TaskStore::open(dir.path()).unwrap();
    for action in [
        super::super::ManageAction::Pause,
        super::super::ManageAction::Remove,
    ] {
        for kind in [AssignKind::Goal, AssignKind::Task] {
            let mut req = weekly_goal("do not start after cancellation");
            req.kind = kind;
            req.payload.title = "cancelled board task".into();
            let creator = Creator::new(&o.mgr, false).unwrap();
            let row = create(dir.path(), &store, &creator, req, now())
                .await
                .unwrap();
            // Another connection commits the control change after the tick
            // captured its due row, before task insertion.
            crate::assign_schedule::manage::manage(
                dir.path(),
                &other,
                &creator,
                &row.id,
                action,
                at(MONDAY),
            )
            .await
            .unwrap();
            fire_one(dir.path(), &store, &reg, &row, at(MONDAY)).await;
            assert!(
                store
                    .get_task(&task_id_for_slot(&row.id, MONDAY))
                    .await
                    .unwrap()
                    .is_none()
            );
            let saved = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
            assert_eq!(saved.fire_count, 0);
        }
    }
}

#[tokio::test]
async fn raising_the_interval_blocks_existing_schedules_and_resume() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let creator = Creator::new(&o.mgr, false).unwrap();
    let mut req = weekly_goal("hourly report");
    req.spec = ScheduleSpec::Hourly { minute: 0 };
    let row = create(dir.path(), &store, &creator, req, now())
        .await
        .unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[goal_defaults]\nassign_schedule_min_interval_minutes = 120\n",
    )
    .unwrap();
    let report = tick(dir.path(), &reg, at("2026-10-23T03:00:01Z"))
        .await
        .unwrap();
    assert!(report.created.is_empty());
    assert_eq!(report.paused, 1);
    let saved = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(saved.pause_reason.as_deref(), Some("schedule_restricted"));
    assert!(
        crate::assign_schedule::manage::manage(
            dir.path(),
            &store,
            &creator,
            &row.id,
            crate::assign_schedule::ManageAction::Resume,
            now()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn a_team_manager_schedule_fires_a_goal_on_their_behalf_once() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let mut req = weekly_goal("寫週報");
    req.payload.cost_cap_cents = Some(125);
    let row = create(
        dir.path(),
        &store,
        &Creator::new(&o.mgr, false).unwrap(),
        req,
        now(),
    )
    .await
    .unwrap();
    assert!(
        tick(dir.path(), &reg, at("2026-10-25T00:00:00Z"))
            .await
            .unwrap()
            .created
            .is_empty(),
        "not due yet"
    );

    let report = tick(dir.path(), &reg, at("2026-10-26T01:00:20Z"))
        .await
        .unwrap();
    assert_eq!(report.created.len(), 1);
    let task = store.get_task(&report.created[0]).await.unwrap().unwrap();
    assert_eq!(task.id, task_id_for_slot(&row.id, MONDAY));
    assert_eq!(task.assigned_to, "sales-rep");
    assert_eq!(
        task.on_behalf_of.as_deref(),
        Some(format!("user:{}", o.mgr).as_str())
    );
    assert_eq!(task.created_via.as_deref(), Some("dashboard"));
    assert!(task.goal_mode);
    assert_eq!(task.cost_cap_cents, Some(125));
    let after = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(
        after.next_fire_at.as_deref(),
        Some("2026-11-02T01:00:00+00:00")
    );
    assert_eq!(
        (after.fire_count, after.last_task_id.as_deref()),
        (1, Some(task.id.as_str()))
    );
    // The same tick again creates nothing.
    assert!(
        tick(dir.path(), &reg, at("2026-10-26T01:00:50Z"))
            .await
            .unwrap()
            .created
            .is_empty()
    );
    assert!(audit_kinds(dir.path()).contains(&"assign_schedule_fired".to_string()));
    // The run does not count toward the chat `/交辦` caps.
    let (recent, open) = store
        .chat_assign_counts(&format!("user:{}", o.mgr), 10)
        .await
        .unwrap();
    assert_eq!((recent, open), (0, 0));
}

#[tokio::test]
async fn a_creator_who_lost_access_is_skipped_and_paused() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let row = create(
        dir.path(),
        &store,
        &Creator::new(&o.mgr, false).unwrap(),
        weekly_goal("寫週報"),
        now(),
    )
    .await
    .unwrap();
    UserDb::new(&dir.path().join("users.db"))
        .unwrap()
        .set_manager(&o.lead, None)
        .unwrap();

    let report = tick(dir.path(), &reg, at("2026-10-26T01:00:20Z"))
        .await
        .unwrap();
    assert!(report.created.is_empty());
    assert_eq!(report.paused, 1);
    assert!(
        store
            .get_task(&task_id_for_slot(&row.id, MONDAY))
            .await
            .unwrap()
            .is_none()
    );
    let after = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(
        (after.status.as_str(), after.pause_reason.as_deref()),
        ("paused", Some("access_lost"))
    );
    let kinds = audit_kinds(dir.path());
    assert!(
        kinds.contains(&"assign_schedule_skipped".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"assign_schedule_auto_paused".to_string()),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn a_suspended_creator_and_a_demoted_admin_cron_both_pause() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let mgr_row = create(
        dir.path(),
        &store,
        &Creator::new(&o.mgr, false).unwrap(),
        weekly_goal("a"),
        now(),
    )
    .await
    .unwrap();
    let mut cron = weekly_goal("b");
    cron.spec = ScheduleSpec::Cron {
        expr: "0 9 * * 1".into(),
    };
    let admin_row = create(
        dir.path(),
        &store,
        &Creator::new(&o.admin, true).unwrap(),
        cron,
        now(),
    )
    .await
    .unwrap();
    let db = UserDb::new(&dir.path().join("users.db")).unwrap();
    db.set_user_status(&o.mgr, UserStatus::Suspended).unwrap();
    db.update_user(&o.admin, None, Some(duduclaw_auth::UserRole::Manager), None)
        .unwrap();

    let report = tick(dir.path(), &reg, at("2026-10-26T01:00:20Z"))
        .await
        .unwrap();
    assert!(report.created.is_empty(), "{report:?}");
    for id in [&mgr_row.id, &admin_row.id] {
        let r = store.get_assign_schedule(id).await.unwrap().unwrap();
        assert_eq!(r.pause_reason.as_deref(), Some("access_lost"), "{id}");
    }
}

#[tokio::test]
async fn a_peer_cannot_be_given_the_right_by_a_schedule_and_an_unknown_employee_pauses() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    // A row written for a peer who never had the right (as if inserted by a
    // bug): the fire-time check alone refuses it.
    let row = create(
        dir.path(),
        &store,
        &Creator::new(&o.peer, false).unwrap(),
        weekly_goal("x"),
        now(),
    )
    .await
    .unwrap();
    let mut ghost = weekly_goal("y");
    ghost.agent_id = "ghost".into();
    let ghost_row = create(
        dir.path(),
        &store,
        &Creator::new(&o.admin, true).unwrap(),
        ghost,
        now(),
    )
    .await
    .unwrap();
    tick(dir.path(), &reg, at("2026-10-26T01:00:20Z"))
        .await
        .unwrap();
    let r = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(r.pause_reason.as_deref(), Some("access_lost"));
    let g = store
        .get_assign_schedule(&ghost_row.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(g.pause_reason.as_deref(), Some("agent_unavailable"));
}

#[tokio::test]
async fn an_open_previous_run_skips_the_slot_and_a_long_outage_is_not_replayed() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let row = create(
        dir.path(),
        &store,
        &Creator::new(&o.mgr, false).unwrap(),
        weekly_goal("寫週報"),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        tick(dir.path(), &reg, at("2026-10-26T01:00:20Z"))
            .await
            .unwrap()
            .created
            .len(),
        1
    );
    // A week later the first goal is still open.
    let report = tick(dir.path(), &reg, at("2026-11-02T01:00:20Z"))
        .await
        .unwrap();
    assert_eq!((report.created.len(), report.skipped), (0, 1));
    let r = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(r.last_outcome.as_deref(), Some("skipped:previous_open"));
    assert_eq!(r.next_fire_at.as_deref(), Some("2026-11-09T01:00:00+00:00"));
    // The gateway was down for two weeks: the stale slot is skipped and the
    // next one is the first after now.
    let report = tick(dir.path(), &reg, at("2026-11-20T00:00:00Z"))
        .await
        .unwrap();
    assert_eq!(report.skipped, 1);
    let r = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(r.last_outcome.as_deref(), Some("skipped:missed"));
    assert_eq!(r.next_fire_at.as_deref(), Some("2026-11-23T01:00:00+00:00"));
}

#[tokio::test]
async fn a_one_shot_board_task_fires_once_and_finishes() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let mut req = weekly_goal("");
    req.kind = AssignKind::Task;
    req.payload = AssignPayload {
        title: "整理客戶名單".into(),
        description: "匯出上週新增的客戶".into(),
        priority: "high".into(),
        ..Default::default()
    };
    req.spec = ScheduleSpec::Once {
        at: "2026-10-23T03:00:00+00:00".into(),
    };
    let row = create(
        dir.path(),
        &store,
        &Creator::new(&o.mgr, false).unwrap(),
        req,
        now(),
    )
    .await
    .unwrap();
    let report = tick(dir.path(), &reg, at("2026-10-23T03:00:05Z"))
        .await
        .unwrap();
    assert_eq!(report.created.len(), 1);
    let task = store.get_task(&report.created[0]).await.unwrap().unwrap();
    assert_eq!(
        (task.title.as_str(), task.priority.as_str()),
        ("整理客戶名單", "high")
    );
    assert_eq!(
        task.created_by, o.mgr,
        "board tasks keep the creator as created_by"
    );
    assert_eq!(
        task.on_behalf_of.as_deref(),
        Some(format!("user:{}", o.mgr).as_str())
    );
    assert!(!task.goal_mode);
    let r = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!((r.status.as_str(), r.next_fire_at), ("done", None));
}

#[test]
fn slot_task_ids_are_stable_and_distinct() {
    let a = task_id_for_slot("s1", MONDAY);
    assert_eq!(a, task_id_for_slot("s1", MONDAY));
    assert_ne!(a, task_id_for_slot("s1", "2026-11-02T01:00:00+00:00"));
    assert_ne!(a, task_id_for_slot("s2", MONDAY));
    assert_eq!(uuid::Uuid::parse_str(&a).unwrap().get_version_num(), 5);
}

#[tokio::test]
async fn a_non_admin_schedule_only_narrows_the_run_limit_and_it_reaches_the_goal() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let manager = Creator::new(&o.mgr, false).unwrap();
    // `sales-rep` has no `[runtime] timeout_secs`: its own limit is 300 s.
    let mut wider = weekly_goal("寫週報");
    wider.payload.timeout_secs = Some(600);
    assert!(create(dir.path(), &store, &manager, wider, now()).await.is_err());

    let mut narrower = weekly_goal("寫週報");
    narrower.payload.timeout_secs = Some(120);
    create(dir.path(), &store, &manager, narrower, now()).await.unwrap();
    let report = tick(dir.path(), &reg, at("2026-10-26T01:00:20Z")).await.unwrap();
    assert_eq!(report.created.len(), 1);
    let task = store.get_task(&report.created[0]).await.unwrap().unwrap();
    assert_eq!(task.timeout_secs, Some(120));
}

#[tokio::test]
async fn old_task_and_goal_schedules_pause_after_restricted_preset_and_cannot_resume() {
    use crate::assign_schedule::manage::{manage, ManageError};
    use crate::assign_schedule::ManageAction;
    use duduclaw_core::preset;
    for kind in [AssignKind::Task, AssignKind::Goal] {
        let (dir, reg) = home().await;
        let h = dir.path();
        let o = org(h);
        let store = TaskStore::open(h).unwrap();
        let creator = Creator::new(&o.mgr, false).unwrap();
        let mut req = weekly_goal("old schedule");
        req.kind = kind;
        req.payload.title = "old task".into();
        let row = create(h, &store, &creator, req, now()).await.unwrap();
        preset::install_system_operator_preset(h, false).unwrap();
        preset::bind(h, "sales-rep", &h.join("agents/sales-rep"), "system-operator", "test", "test").unwrap();
        assert!(matches!(fire_one(h, &store, &reg, &row, at(MONDAY)).await, Fired::Paused));
        let saved = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
        assert_eq!(saved.status, "paused");
        assert_eq!(saved.pause_reason.as_deref(), Some("access_lost"));
        assert_eq!(saved.fire_count, 0);
        assert!(store.get_task(&task_id_for_slot(&row.id, MONDAY)).await.unwrap().is_none());
        for actor in [&creator, &Creator::new(&o.admin, true).unwrap()] {
            assert_eq!(manage(h, &store, actor, &row.id, ManageAction::Resume, now()).await.unwrap_err(), ManageError::CreatorLostAccess);
        }
        assert!(store.get_task(&task_id_for_slot(&row.id, MONDAY)).await.unwrap().is_none());
        preset::unbind(h, "sales-rep", "test", "test").unwrap();
        assert!(manage(h, &store, &creator, &row.id, ManageAction::Resume, now()).await.is_ok());
    }
}

#[tokio::test]
async fn autonomy_pause_assign_schedule_skips_slot_and_resumes_next_slot() {
    let (dir, reg) = home().await;
    let o = org(dir.path());
    let store = TaskStore::open(dir.path()).unwrap();
    let row = create(dir.path(), &store, &Creator::new(&o.mgr, false).unwrap(), weekly_goal("週報"), now()).await.unwrap();
    duduclaw_core::autonomy_gate::set_pause(dir.path(), Some("sales-rep"), "pause", &o.mgr).unwrap();
    let paused = tick(dir.path(), &reg, at("2026-10-26T01:00:20Z")).await.unwrap();
    assert!(paused.created.is_empty());
    assert_eq!(paused.skipped, 1);
    let after = store.get_assign_schedule(&row.id).await.unwrap().unwrap();
    assert_eq!(after.fire_count, 0);
    assert_eq!(after.next_fire_at.as_deref(), Some("2026-11-02T01:00:00+00:00"));
    duduclaw_core::autonomy_gate::resume(dir.path(), Some("sales-rep")).unwrap();
    let resumed = tick(dir.path(), &reg, at("2026-11-02T01:00:20Z")).await.unwrap();
    assert_eq!(resumed.created.len(), 1);
}
