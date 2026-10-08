//! P2-A tests (design §9): ET1 duplicates/races/restart, ET2 a day of
//! waiting with zero occupancy and zero model calls, ET3 steering, ET4 the
//! three stop controls, plus the ET5 data-not-authority cases that the
//! service layer covers. Every case runs on a tempdir home with real SQLite
//! stores and an injected clock; races stop at fixed pause points.

mod control_cases;
mod driver_cases;
mod notify_cases;
mod round3_answer_cases;
mod round3_cases;
mod round4_cases;
mod round5_cases;
mod steering_cases;
mod stop_cases;
mod team_stop_cases;
mod wake_cases;

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::cost::test_support::FixedCost;
use super::service::{self, EventSubscription, ResponsibilityInput, ScheduleSpec};
use super::{WakeContext, WakeReport, wake_pass};
use crate::goal_loop::{GoalLoopConfig, GoalLoopDriver};
use crate::message_queue::MessageQueue;
use crate::task_store::{ResponsibilityRow, TaskRow, TaskStore};

pub(super) const OWNER: &str = "alice";

pub(super) struct Env {
    pub dir: tempfile::TempDir,
    pub store: Arc<TaskStore>,
    pub queue: Arc<MessageQueue>,
    pub cost: Arc<FixedCost>,
}

pub(super) fn config_text(responsibilities: bool, steering: bool) -> String {
    format!(
        "[dispatch]\nenabled = true\n\n[responsibilities]\nenabled = {responsibilities}\n\n\
         [goal_loop]\nsteering_enabled = {steering}\n"
    )
}

impl Env {
    pub fn new() -> Self {
        Self::with_config(&config_text(true, true))
    }

    pub fn with_config(config: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        // S-L12: a responsibility needs an existing owner employee.
        let agent = dir.path().join("agents").join(OWNER);
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname = \"alice\"\n").unwrap();
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let queue = Arc::new(MessageQueue::open(dir.path()).unwrap());
        Self {
            dir,
            store,
            queue,
            cost: Arc::new(FixedCost::default()),
        }
    }

    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    /// A second store handle on the same `tasks.db` (another connection —
    /// what a respawned driver or a concurrent RPC would hold).
    pub fn second_store(&self) -> Arc<TaskStore> {
        Arc::new(TaskStore::open(self.home()).unwrap())
    }

    pub fn driver(&self) -> GoalLoopDriver {
        self.driver_on(Arc::clone(&self.store))
    }

    pub fn driver_on(&self, store: Arc<TaskStore>) -> GoalLoopDriver {
        GoalLoopDriver::new(store, Arc::clone(&self.queue), loop_cfg())
            .with_home_dir(self.home().to_path_buf())
            .with_cost_source(self.cost.clone())
    }

    pub async fn wake(&self, free_slots: usize, now: DateTime<Utc>) -> WakeReport {
        let ctx = WakeContext {
            home: self.home(),
            store: &self.store,
            broker: None,
            cost: self.cost.as_ref(),
            free_slots,
            notifier: None,
        };
        wake_pass(&ctx, now).await.expect("wake pass")
    }

    pub async fn pending_queue(&self) -> usize {
        self.queue.pending_messages(1000).await.unwrap().len()
    }
}

pub(super) fn loop_cfg() -> GoalLoopConfig {
    GoalLoopConfig {
        iteration_cap: 5,
        iteration_cap_simple: 5,
        soft_cap: 3,
        wall_clock_hours: 24,
        max_concurrent: 3,
        tick_secs: 30,
        stalled_secs: 600,
        progress_report_minutes: 0,
        resume_on_restart: "auto".to_string(),
        tool_streak_advisory: true,
    }
}

/// 2026-10-05 01:01 UTC = 09:01 Asia/Taipei.
pub(super) fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 5, 1, 1, 0).unwrap()
}

pub(super) fn input(now: DateTime<Utc>) -> ResponsibilityInput {
    ResponsibilityInput {
        owner_agent_id: OWNER.into(),
        objective: "每天早上整理客服信件並回報".into(),
        acceptance_template: "產出一份摘要".into(),
        source_refs: vec![],
        notification_policy: None,
        schedule: Some(ScheduleSpec {
            cron: "0 0 9 * * *".into(),
            timezone: "Asia/Taipei".into(),
        }),
        event_subscriptions: vec![],
        occurrence_hours: 4,
        occurrence_cost_cap_cents: 100,
        budget_period: "day".into(),
        budget_timezone: "Asia/Taipei".into(),
        period_cost_limit_cents: 1000,
        period_occurrence_limit: 5,
        min_wake_interval_secs: 300,
        max_consecutive_failures: 3,
        stop_at: now + Duration::days(10),
        lane: None,
    }
}

pub(super) fn event_input(now: DateTime<Utc>) -> ResponsibilityInput {
    let mut i = input(now);
    i.schedule = None;
    i.event_subscriptions = vec![EventSubscription {
        event_name: "task.created".into(),
        filter: None,
        timeout_at: None,
    }];
    i
}

pub(super) async fn create(
    env: &Env,
    input: &ResponsibilityInput,
    now: DateTime<Utc>,
) -> ResponsibilityRow {
    service::create(&env.store, env.home(), input, "operator-1", now)
        .await
        .expect("create responsibility")
}

/// Arm a one-shot time subscription due at `due` (operator-armed) and return
/// its id — the simplest deterministic wake source.
pub(super) async fn arm_time(env: &Env, resp: &ResponsibilityRow, due: DateTime<Utc>) -> String {
    let ts = crate::task_store::resp_ts(due);
    let w = crate::task_store::WakeupRow {
        wakeup_id: uuid::Uuid::new_v4().to_string(),
        responsibility_id: resp.responsibility_id.clone(),
        control_epoch: resp.control_epoch,
        kind: "time".into(),
        recurring: false,
        due_at: Some(ts.clone()),
        event_name: None,
        event_filter_json: None,
        approval_id: None,
        armed_by: "operator:test".into(),
        state: "armed".into(),
        created_at: ts.clone(),
        updated_at: ts,
        armed_after_event_id: None,
    };
    env.store.arm_wakeup(&w, None).await.unwrap();
    w.wakeup_id
}

pub(super) async fn occurrence_tasks(env: &Env, resp: &ResponsibilityRow) -> Vec<String> {
    env.store
        .list_occurrences(&resp.responsibility_id)
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.task_id)
        .collect()
}

pub(super) async fn task(env: &Env, id: &str) -> TaskRow {
    env.store.get_task(id).await.unwrap().expect("task")
}

/// A plain goal task (no responsibility) in `status`.
pub(super) async fn goal_task(env: &Env, id: &str, status: &str) -> TaskRow {
    let mut t = TaskRow::new(
        id.into(),
        format!("goal {id}"),
        "do the work".into(),
        "medium".into(),
        OWNER.into(),
        "system".into(),
    );
    t.status = status.into();
    t.goal_mode = true;
    t.acceptance_criteria = Some("must be correct".into());
    t.acceptance_criteria_baseline = Some("must be correct".into());
    env.store.insert_task(&t).await.unwrap();
    task(env, id).await
}
