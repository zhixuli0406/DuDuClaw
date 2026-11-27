//! 141A-P5/P6: isolated stores, injected UTC clock, no real sleeps or channel sends.
use super::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crate::autopilot_store::{
    AutopilotRunRow, RuleExtensionsPatch, RUN_PENDING, RUN_WAITING, RUN_SETTLED,
    STEP_DONE, STEP_FAILED, STEP_RUNNING, run_ts,
};
use serde_json::json;

struct Harness {
    _dir: tempfile::TempDir,
    home: PathBuf,
    store: Arc<AutopilotStore>,
    tasks: Arc<TaskStore>,
    mq: Arc<MessageQueue>,
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-27T09:00:00Z").unwrap().with_timezone(&Utc)
}

fn fields() -> serde_json::Map<String, Value> {
    json!({"event":"task_created", "task":{"id":"t1", "title":"凍結標題"}}).as_object().unwrap().clone()
}

fn flow() -> Value {
    json!({"type":"steps", "steps":[
        {"type":"delegate", "target_agent":"plain", "prompt":"A {{task.title}}"},
        {"type":"wait", "minutes":1},
        {"type":"delegate", "target_agent":"other", "prompt":"B {{task.title}}"}
    ]})
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        for agent in ["plain", "other"] {
            let path = home.join("agents").join(agent);
            std::fs::create_dir_all(path.join("SKILLS")).unwrap();
            std::fs::write(path.join("agent.toml"), format!("[agent]\nname = {agent:?}\nstatus = \"active\"\n")).unwrap();
            std::fs::write(path.join("SKILLS/test.md"), "Report the supplied order.").unwrap();
        }
        Self {
            store: Arc::new(AutopilotStore::open(&home).unwrap()),
            tasks: Arc::new(TaskStore::open(&home).unwrap()),
            mq: Arc::new(MessageQueue::open(&home).unwrap()), home, _dir: dir,
        }
    }

    fn acquire(&self) {
        duduclaw_core::gateway_instance::acquire(&self.home).unwrap();
    }

    fn engine(&self) -> AutopilotEngine {
        let (_tx, rx) = tokio::sync::broadcast::channel(16);
        AutopilotEngine::new(self.home.clone(), self.store.clone(), self.tasks.clone(), Some(self.mq.clone()), rx)
    }

    async fn rule(&self, action: Value) -> AutopilotRuleRow {
        let row = AutopilotRuleRow {
            id: "r".into(), name: "測試規則".into(), enabled: true, trigger_event: "task_created".into(),
            conditions: "{}".into(), action: action.to_string(), created_at: run_ts(now()),
            last_triggered_at: None, trigger_count: 0, sequence: None, metadata: None,
            active_window: None, run_policy: None, owner_department: None,
            created_by_user_id: None, updated_by_user_id: None, updated_at: None,
        };
        self.store.insert_rule(&row).await.unwrap();
        row
    }

    async fn run(&self) -> AutopilotRunRow {
        let rows = self.store.list_runs_for_rule("r", 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        rows.into_iter().next().unwrap()
    }

    async fn queued(&self) -> Vec<QueueMessage> {
        self.mq.pending_messages(100).await.unwrap()
    }

    async fn assert_outcome(&self, outcome: &str, queued: usize) -> AutopilotRunRow {
        let run = self.run().await;
        assert_eq!(run.state, RUN_SETTLED);
        assert_eq!(run.outcome.as_deref(), Some(outcome));
        assert_eq!(self.queued().await.len(), queued);
        let histories = self.store.list_history(Some("r"), 100).await.unwrap();
        assert_eq!(histories.iter().filter(|h| h.id == format!("run:{}:outcome", run.run_id) && h.result == outcome).count(), 1);
        let (activities, _) = self.tasks.list_activity(None, None, 100, 0).await.unwrap();
        let matching: Vec<_> = activities.iter().filter(|a| a.id == format!("run:{}:outcome", run.run_id)).collect();
        assert_eq!(matching.len(), 1);
        let metadata: Value = serde_json::from_str(matching[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["run_id"].as_str(), Some(run.run_id.as_str()));
        run
    }

    async fn waiting(&self, engine: &AutopilotEngine) -> AutopilotRunRow {
        let rule = self.rule(flow()).await;
        engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
        let run = self.run().await;
        assert_eq!(run.state, RUN_WAITING);
        assert_eq!(run.step_index, 2);
        let steps = self.store.list_run_steps(&run.run_id).await.unwrap();
        assert_eq!(steps[1].state, STEP_DONE);
        assert!(steps.iter().all(|step| step.state != STEP_RUNNING));
        assert_eq!(self.queued().await.len(), 1);
        run
    }

    fn corrupt_run(&self, id: &str, column: &str, value: &str) {
        // Closed set of test-only columns; values always remain bound parameters.
        let sql = match column {
            "state" => "UPDATE autopilot_runs SET state=?1 WHERE run_id=?2",
            "fields_json" => "UPDATE autopilot_runs SET fields_json=?1 WHERE run_id=?2",
            _ => panic!("unsupported test column"),
        };
        rusqlite::Connection::open(self.home.join("autopilot.db")).unwrap()
            .execute(sql, rusqlite::params![value, id]).unwrap();
    }
}

#[tokio::test]
async fn full_flow_delegate_wait_delegate() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    let run = h.waiting(&engine).await;
    assert_eq!(run.next_at.as_deref(), Some(run_ts(now() + ChronoDuration::minutes(1)).as_str()));
    let a = h.queued().await;
    assert!(a[0].id.starts_with("ap-"));
    assert_eq!(a[0].target, "plain");
    assert!(a[0].payload.contains("凍結標題"));
    engine.resume_runs_once(now() + ChronoDuration::seconds(59)).await.unwrap();
    assert_eq!(h.queued().await.len(), 1);
    engine.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
    engine.resume_runs_once(now() + ChronoDuration::seconds(62)).await.unwrap();
    h.assert_outcome("completed", 2).await;
    assert_eq!(engine.performed_actions.load(std::sync::atomic::Ordering::SeqCst), 2);
    let b = h.mq.get_by_id(&runs::queue_id(&run.run_id, 2)).await.unwrap().unwrap();
    assert_eq!(b.target, "other");
    assert!(b.payload.contains("凍結標題"));
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
    assert!(steps.iter().all(|s| s.state == STEP_DONE));
    let history = h.store.list_history(Some("r"), 100).await.unwrap();
    for i in 0..3 {
        assert_eq!(history.iter().filter(|r| r.id == format!("run:{}:step:{i}", run.run_id) && r.result == "step_done").count(), 1);
    }
}

#[tokio::test]
async fn restart_mid_step_no_duplicate_enqueue() {
    for sent in [true, false] {
        let h = Harness::new();
        h.acquire();
        let engine = h.engine();
        let run = h.waiting(&engine).await;
        let started = now() + ChronoDuration::seconds(61);
        assert!(h.store.claim_run(&run.run_id, "crashed-process", started).await.unwrap());
        assert!(h.store.mark_step_running(&run.run_id, 2, "crashed-process", started).await.unwrap());
        let id = runs::queue_id(&run.run_id, 2);
        if sent {
            let rule = h.store.get_rule("r").await.unwrap().unwrap();
            let action: Value = serde_json::from_str(&rule.action).unwrap();
            let frozen_fields = serde_json::from_str(&run.fields_json).unwrap();
            let prepared = engine.prepare_action(&rule.id, &rule.name, &action["steps"][2],
                &frozen_fields, &frozen_fields, None).await.unwrap().unwrap();
            engine.perform_action_with_queue_id(&prepared, Some(&id), started).await.unwrap();
        }
        drop(engine);
        // New engine + reopened connections, retaining only durable state.
        let (_tx, rx) = tokio::sync::broadcast::channel(16);
        let restarted = AutopilotEngine::new(h.home.clone(), Arc::new(AutopilotStore::open(&h.home).unwrap()),
            Arc::new(TaskStore::open(&h.home).unwrap()), Some(Arc::new(MessageQueue::open(&h.home).unwrap())), rx);
        restarted.resume_runs_once(started + ChronoDuration::seconds(61)).await.unwrap();
        restarted.resume_runs_once(started + ChronoDuration::seconds(62)).await.unwrap();
        h.assert_outcome("completed", 2).await;
        assert!(h.mq.get_by_id(&id).await.unwrap().is_some());
        let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
        assert_eq!(steps[2].state, STEP_DONE);
        assert_eq!(steps[2].queue_message_id.as_deref(), Some(id.as_str()));
    }
}

#[tokio::test]
async fn notify_crash_settles_uncertain() {
    // A malformed timestamp/real channel call would fail this test: recovery
    // must never attempt preparation or delivery for these running steps.
    for step in [
        json!({"type":"notify","channel":"test","chat_id":"admin","text":"hello"}),
        json!({"type":"proactive_notify","channel":"test","chat_id":"admin","text":"hello"}),
    ] {
        let h = Harness::new();
        let engine = h.engine();
        let rule = h.rule(json!({"type":"steps","steps":[step,
            {"type":"delegate","target_agent":"plain","prompt":"never"}]})).await;
        engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
        let run = h.run().await;
        h.acquire();
        assert!(h.store.claim_run(&run.run_id, "crashed", now()).await.unwrap());
        assert!(h.store.mark_step_running(&run.run_id, 0, "crashed", now()).await.unwrap());
        h.engine().resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
        h.assert_outcome("execution_uncertain", 0).await;
        let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
        assert_eq!(steps[0].state, STEP_RUNNING);
        assert_eq!(steps[0].started_at.as_deref(), Some(run_ts(now()).as_str()));
        let (activities, _) = h.tasks.list_activity(None, None, 100, 0).await.unwrap();
        assert!(activities.iter().any(|a| a.summary.contains("為避免重複沒有重送")));
    }
}

#[tokio::test]
async fn edit_during_wait_settles_stale() {
    for outcome in ["stale_rule_changed", "rule_disabled", "rule_removed"] {
        let h = Harness::new();
        h.acquire();
        let engine = h.engine();
        h.waiting(&engine).await;
        match outcome {
            "stale_rule_changed" => {
                let mut changed = flow();
                changed["steps"][2]["prompt"] = json!("changed");
                h.store.update_rule("r", &json!({"action":changed})).await.unwrap();
            }
            "rule_disabled" => { h.store.update_rule("r", &json!({"enabled":false})).await.unwrap(); }
            "rule_removed" => { h.store.remove_rule("r").await.unwrap(); }
            _ => unreachable!(),
        }
        engine.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
        h.assert_outcome(outcome, 1).await;
    }
}

#[tokio::test]
async fn missed_late() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    h.waiting(&engine).await;
    engine.resume_runs_once(now() + ChronoDuration::minutes(71)).await.unwrap();
    h.assert_outcome("missed_late", 1).await;
    let (activities, _) = h.tasks.list_activity(None, None, 100, 0).await.unwrap();
    assert!(activities.iter().any(|a| a.summary.contains("系統停機超過一小時，後續步驟沒有補做")));
}

#[tokio::test]
async fn non_lock_holder_never_advances() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
    engine.resume_runs_once(now() + ChronoDuration::minutes(1)).await.unwrap();
    assert_eq!(h.run().await.state, RUN_PENDING);
    assert!(h.queued().await.is_empty());
    h.acquire();
    engine.resume_runs_once(now() + ChronoDuration::minutes(2)).await.unwrap();
    assert_eq!(h.run().await.state, RUN_WAITING);
    assert_eq!(h.queued().await.len(), 1);
}

#[tokio::test]
async fn run_limit_enforced() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    // Exercise run admission directly: 21 fires in one minute would correctly
    // trip the unrelated trigger breaker before reaching the capacity boundary.
    for i in 0..21 {
        let mut fields = fields();
        fields.insert("event_id".into(), json!(i));
        engine.start_run(&rule, "task_created", &fields, now()).await.unwrap();
    }
    assert_eq!(h.store.list_runs_for_rule("r", 100).await.unwrap().len(), 20);
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 20);
    let history = h.store.list_history(Some("r"), 100).await.unwrap();
    assert_eq!(history.iter().filter(|h| h.result == "run_limit").count(), 1);
    assert!(h.queued().await.is_empty());
}

#[tokio::test]
async fn duplicate_event_creates_one_run() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    for _ in 0..2 {
        engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
    }
    assert_eq!(h.run().await.state, RUN_PENDING);
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    assert_eq!(h.store.list_history(Some("r"), 100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn breaker_open_aborts_next_step() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    h.waiting(&engine).await;
    engine.circuit.lock().await.insert("r".into(), CircuitState::Open {
        opened_at: Instant::now(), opened_at_utc: Some(now()),
    });
    engine.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
    h.assert_outcome("blocked_breaker_open", 1).await;
}

#[tokio::test]
async fn later_step_rechecks_active_window() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    h.rule(flow()).await;
    let rule = h.store.set_rule_extensions("r", &RuleExtensionsPatch {
        active_window: Some(Some(json!({"days":[5],"start":"09:00","end":"09:01","timezone":"UTC"}).to_string())),
        run_policy: None, owner_department: None, updated_by_user_id: None, updated_at: run_ts(now()),
    }).await.unwrap().unwrap();
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
    assert_eq!(h.run().await.state, RUN_WAITING);
    engine.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
    h.assert_outcome("outside_window", 1).await;
}

#[tokio::test]
async fn missing_skill_param_fails_step() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    let rule = h.rule(json!({"type":"steps","steps":[{
        "type":"run_skill","target_agent":"plain","skill_name":"test",
        "params":{"order":"{{task.deadline_at}}"}
    }]})).await;
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
    let run = h.assert_outcome("step_failed", 0).await;
    assert_eq!(run.detail.as_deref(), Some("param_missing:order"));
    let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
    assert_eq!(steps[0].state, STEP_FAILED);
    assert_eq!(steps[0].detail.as_deref(), Some("param_missing:order"));
    assert!(h.store.list_history(Some("r"), 100).await.unwrap().iter().any(|h| h.result == "step_failed" && h.id == format!("run:{}:step:0", run.run_id)));
}

#[tokio::test]
async fn gated_step_without_approval_service_is_blocked() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    let rule = h.rule(json!({"type":"steps","steps":[{
        "type":"delegate","target_agent":"plain","prompt":"never","require_approval":true
    }]})).await;
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
    let run = h.assert_outcome("blocked_no_approval_service", 0).await;
    assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].state, STEP_FAILED);
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    let (activity, _) = h.tasks.list_activity(None, None, 100, 0).await.unwrap();
    assert!(activity.iter().any(|row| row.event_type == gate::ACTIVITY_BLOCKED));
}

#[tokio::test]
async fn dept_rule_denied_until_predicate_wired() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    h.rule(flow()).await;
    let rule = h.store.set_rule_extensions("r", &RuleExtensionsPatch {
        owner_department: Some(Some("sales".into())), active_window: None, run_policy: None,
        updated_by_user_id: None, updated_at: run_ts(now()),
    }).await.unwrap().unwrap();
    // Trigger admission denies without a fire-counter increment or a run.
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), now()).await;
    assert!(h.store.list_runs_for_rule("r", 100).await.unwrap().is_empty());
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 0);
    assert!(h.store.list_history(Some("r"), 100).await.unwrap().is_empty());
    // Direct admission is guarded too, before a run or history can be created.
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    assert!(h.store.list_runs_for_rule("r", 100).await.unwrap().is_empty());
    assert!(h.store.list_history(Some("r"), 100).await.unwrap().is_empty());
    assert_eq!(engine.performed_actions.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(h.queued().await.is_empty());
}

#[tokio::test]
async fn unknown_persisted_run_state_settles_unsupported() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.run().await;
    h.corrupt_run(&run.run_id, "state", "future_state");
    h.acquire();
    engine.resume_runs_once(now()).await.unwrap();
    h.assert_outcome("unsupported_state", 0).await;
}

#[tokio::test]
async fn unreadable_frozen_fields_fail_closed() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    h.corrupt_run(&h.run().await.run_id, "fields_json", "[]");
    h.acquire();
    engine.resume_runs_once(now()).await.unwrap();
    h.assert_outcome("fields_unreadable", 0).await;
}

#[tokio::test]
async fn deterministic_queue_id_requires_exact_delivery() {
    let h = Harness::new();
    let engine = h.engine();
    let id = runs::queue_id("test", 0);
    engine.enqueue_prompt_with_id("plain", "one", None, Some(&id), now()).await.unwrap();
    engine.enqueue_prompt_with_id("plain", "one", None, Some(&id), now()).await.unwrap();
    assert!(engine.enqueue_prompt_with_id("other", "two", None, Some(&id), now()).await.is_err());
    assert!(engine.enqueue_prompt_with_id("plain", "two", None, Some(&id), now()).await.is_err());
    assert!(engine.enqueue_prompt_with_id("plain", "one", Some("explore".into()), Some(&id), now()).await.is_err());
    assert_eq!(h.queued().await.len(), 1);
}

#[tokio::test]
async fn recovery_queue_target_mismatch_fails_closed() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine();
    let run = h.waiting(&engine).await;
    let started = now() + ChronoDuration::seconds(61);
    assert!(h.store.claim_run(&run.run_id, "crashed", started).await.unwrap());
    assert!(h.store.mark_step_running(&run.run_id, 2, "crashed", started).await.unwrap());
    engine.enqueue_prompt_with_id("plain", "wrong recipient", None, Some(&runs::queue_id(&run.run_id, 2)), started).await.unwrap();
    engine.resume_runs_once(started + ChronoDuration::seconds(61)).await.unwrap();
    h.assert_outcome("step_failed", 2).await;
    assert!(h.queued().await.iter().all(|m| m.target == "plain"));
}

#[tokio::test]
async fn proactive_suppression_skips_step_and_continues() {
    let h = Harness::new();
    h.acquire();
    let engine = h.engine(); // No proactive gate wired: normal suppression.
    let rule = h.rule(json!({"type":"steps","steps":[
        {"type":"proactive_notify","channel":"test","chat_id":"admin","text":"suppressed"},
        {"type":"delegate","target_agent":"plain","prompt":"continue"}
    ]})).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.assert_outcome("completed", 1).await;
    let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
    assert_eq!(steps[0].state, "skipped");
    assert!(h.store.list_history(Some("r"), 100).await.unwrap().iter().any(|r| r.result == "step_skipped"));
}

#[tokio::test]
async fn later_step_rechecks_global_target_and_event_autonomy() {
    for paused in [None, Some("other"), Some("plain")] {
        let h = Harness::new();
        h.acquire();
        let engine = h.engine();
        let rule = h.rule(flow()).await;
        let mut fields = fields();
        fields.insert("agent_id".into(), json!("plain"));
        engine.start_run(&rule, "task_created", &fields, now()).await.unwrap();
        assert_eq!(h.run().await.state, RUN_WAITING);
        duduclaw_core::autonomy_gate::set_pause(&h.home, paused, "test pause", "admin").unwrap();
        engine.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
        h.assert_outcome("autonomy_paused", 1).await;
    }
}

#[tokio::test]
async fn run_skill_recovery_reconciles_or_requeues_once() {
    for sent in [true, false] {
        let h = Harness::new();
        let engine = h.engine();
        let rule = h.rule(json!({"type":"steps","steps":[{
            "type":"run_skill","target_agent":"plain","skill_name":"test"
        }]})).await;
        engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
        let run = h.run().await;
        h.acquire();
        assert!(h.store.claim_run(&run.run_id, "crashed", now()).await.unwrap());
        assert!(h.store.mark_step_running(&run.run_id, 0, "crashed", now()).await.unwrap());
        let id = runs::queue_id(&run.run_id, 0);
        if sent {
            let action: Value = serde_json::from_str(&rule.action).unwrap();
            let frozen_fields = serde_json::from_str(&run.fields_json).unwrap();
            let prepared = engine.prepare_action(&rule.id, &rule.name, &action["steps"][0],
                &frozen_fields, &frozen_fields, None).await.unwrap().unwrap();
            engine.perform_action_with_queue_id(&prepared, Some(&id), now()).await.unwrap();
        }
        let restarted = h.engine();
        restarted.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
        restarted.resume_runs_once(now() + ChronoDuration::seconds(62)).await.unwrap();
        h.assert_outcome("completed", 1).await;
        assert_eq!(h.queued().await[0].id, id);
    }
}

#[tokio::test]
async fn oversized_fields_do_not_create_or_count_run() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    let mut fields = fields();
    fields.insert("oversized".into(), json!("x".repeat(crate::autopilot_approval::MAX_FROZEN_PAYLOAD_BYTES)));
    engine.start_run(&rule, "task_created", &fields, now()).await.unwrap();
    assert!(h.store.list_runs_for_rule("r", 100).await.unwrap().is_empty());
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 0);
    let history = h.store.list_history(Some("r"), 100).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].result, "fields_too_large");
    assert!(h.queued().await.is_empty());
}

#[tokio::test]
async fn malformed_stored_steps_fail_closed() {
    for action in [
        json!({"type":"steps","steps":[]}),
        json!({"type":"steps","steps":[{"type":"future_action"}]}),
        json!({"type":"steps","steps":[{"type":"wait","minutes":1}]}),
        json!({"type":"steps","require_approval":true,"steps":[
            {"type":"delegate","target_agent":"plain","prompt":"never"}]}),
    ] {
        let h = Harness::new();
        h.acquire();
        let engine = h.engine();
        let rule = h.rule(action).await;
        engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
        assert!(h.store.list_runs_for_rule("r", 100).await.unwrap().is_empty());
        assert_eq!(h.store.list_history(Some("r"), 100).await.unwrap()[0].result, "invalid_action");
        assert!(h.queued().await.is_empty());
    }
}

#[tokio::test]
async fn rule_change_after_render_prevents_step_start() {
    for outcome in ["rule_disabled", "stale_rule_changed", "rule_removed"] {
        let h = Harness::new();
        h.acquire();
        let engine = h.engine();
        let rule = h.rule(json!({"type":"steps","steps":[
            {"type":"run_skill","target_agent":"plain","skill_name":"test"}
        ]})).await;
        let home = h.home.clone();
        *engine.after_claim_hook.lock().unwrap() = Some(Box::new(move || {
            let conn = rusqlite::Connection::open(home.join("autopilot.db")).unwrap();
            match outcome {
                "rule_disabled" => { conn.execute("UPDATE autopilot_rules SET enabled=0 WHERE id='r'", []).unwrap(); }
                "stale_rule_changed" => {
                    conn.execute("UPDATE autopilot_rules SET action=?1 WHERE id='r'",
                        rusqlite::params![json!({"type":"steps","steps":[
                            {"type":"delegate","target_agent":"other","prompt":"changed"}
                        ]}).to_string()]).unwrap();
                }
                "rule_removed" => { conn.execute("DELETE FROM autopilot_rules WHERE id='r'", []).unwrap(); }
                _ => unreachable!(),
            }
        }));
        engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
        let run = h.assert_outcome(outcome, 0).await;
        let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
        assert_eq!(steps[0].state, "pending");
        assert!(steps[0].started_at.is_none());
        assert!(engine.after_claim_hook.lock().unwrap().is_none());
    }
}

#[tokio::test]
async fn running_wait_recovery_restores_deadline_or_settles_late() {
    for elapsed_minutes in [30, 180] {
        let h = Harness::new();
        let engine = h.engine();
        let rule = h.rule(json!({"type":"steps","steps":[
            {"type":"wait","minutes":60},
            {"type":"delegate","target_agent":"plain","prompt":"after wait"}
        ]})).await;
        engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
        let run = h.run().await;
        assert!(h.store.claim_run(&run.run_id, "crashed", now()).await.unwrap());
        assert!(h.store.mark_step_running(&run.run_id, 0, "crashed", now()).await.unwrap());
        rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap()
            .execute("UPDATE autopilot_runs SET next_at=?1 WHERE run_id=?2",
                rusqlite::params![run_ts(now() - ChronoDuration::hours(3)), run.run_id]).unwrap();
        h.acquire();
        h.engine().resume_runs_once(now() + ChronoDuration::minutes(elapsed_minutes)).await.unwrap();
        if elapsed_minutes == 30 {
            let recovered = h.run().await;
            assert_eq!(recovered.state, RUN_WAITING);
            assert_eq!(recovered.step_index, 1);
            assert_eq!(recovered.next_at, Some(run_ts(now() + ChronoDuration::minutes(60))));
            let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
            assert_eq!(steps[0].state, STEP_DONE);
            assert_eq!(steps[0].started_at, Some(run_ts(now())));
        } else {
            h.assert_outcome("missed_late", 0).await;
        }
        assert!(h.queued().await.is_empty());
    }
}

#[tokio::test]
async fn settlement_outbox_survives_restart_and_duplicate_audit_writes() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.run().await;
    assert!(h.store.claim_run(&run.run_id, "crashed", now()).await.unwrap());
    // Crash after the settlement transaction, before any announcement.
    assert!(h.store.settle_claimed_run(&run.run_id, "crashed", "missed_late", None, now()).await.unwrap());
    drop(engine);
    let reopened = AutopilotStore::open(&h.home).unwrap();
    let persisted = reopened.get_run(&run.run_id).await.unwrap().unwrap();
    assert!(persisted.notice_pending);
    assert_eq!(persisted.notice_push_state.as_deref(), Some("pending"));
    assert!(!h.store.list_history(Some("r"), 100).await.unwrap().iter()
        .any(|row| row.id == format!("run:{}:outcome", run.run_id)));
    assert!(h.tasks.list_activity(None, None, 100, 0).await.unwrap().0.is_empty());
    h.acquire();
    let restarted = h.engine();
    restarted.announce_run_notices_once(now()).await.unwrap();
    assert!(!h.run().await.notice_pending);
    // Also simulate a crash after both audit inserts but before clearing the flag.
    rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap()
        .execute("UPDATE autopilot_runs SET notice_pending=1 WHERE run_id=?1",
            rusqlite::params![run.run_id]).unwrap();
    restarted.announce_run_notices_once(now()).await.unwrap();
    restarted.announce_run_notices_once(now()).await.unwrap();
    h.assert_outcome("missed_late", 0).await;
    assert!(!h.run().await.notice_pending);
}

#[tokio::test]
async fn run_notice_failed_push_retries_until_gave_up() {
    use crate::autopilot_approval::{NoticePush, MAX_NOTICE_PUSH_ATTEMPTS, notice_push_backoff};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.run().await;
    assert!(h.store.settle_unclaimed_run(&run.run_id, &[RUN_PENDING], "execution_uncertain", None, now()).await.unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    *engine.notice_pusher.lock().unwrap() = Some(Arc::new(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        NoticePush::Failed
    }));
    h.acquire();
    let mut due = now();
    for attempt in 1..=MAX_NOTICE_PUSH_ATTEMPTS {
        engine.announce_run_notices_once(due).await.unwrap();
        let row = h.run().await;
        assert_eq!(row.notice_push_attempts, attempt);
        assert_eq!(calls.load(Ordering::SeqCst), attempt as usize);
        if attempt < MAX_NOTICE_PUSH_ATTEMPTS {
            let next = due + notice_push_backoff(attempt);
            assert_eq!(row.notice_push_state.as_deref(), Some("pending"));
            assert_eq!(row.notice_push_next_at, Some(run_ts(next)));
            engine.announce_run_notices_once(next - ChronoDuration::milliseconds(1)).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), attempt as usize);
            due = next;
        }
    }
    let row = h.assert_outcome("execution_uncertain", 0).await;
    assert_eq!(row.notice_push_state.as_deref(), Some("gave_up"));
    assert!(row.notice_push_next_at.is_none());
    engine.announce_run_notices_once(due + ChronoDuration::days(1)).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), MAX_NOTICE_PUSH_ATTEMPTS as usize);
}

#[tokio::test]
async fn run_notice_no_target_stops_retrying() {
    use crate::autopilot_approval::NoticePush;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(flow()).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.run().await;
    assert!(h.store.settle_unclaimed_run(&run.run_id, &[RUN_PENDING], "missed_late", None, now()).await.unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    *engine.notice_pusher.lock().unwrap() = Some(Arc::new(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        NoticePush::NoTarget
    }));
    // A non-holder cannot drain the outbox or call the test pusher.
    engine.announce_run_notices_once(now()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(h.run().await.notice_pending);
    h.acquire();
    engine.resume_runs_once(now()).await.unwrap();
    engine.resume_runs_once(now() + ChronoDuration::days(1)).await.unwrap();
    let row = h.assert_outcome("missed_late", 0).await;
    assert_eq!(row.notice_push_state.as_deref(), Some("no_target"));
    assert!(row.notice_push_next_at.is_none());
    assert_eq!(row.notice_push_attempts, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// Per-step approval fixtures use the broker's clock for card decisions; run
// deadlines remain injectable. No real channels or employee execution are used.
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord};
use crate::autopilot_approval::{self as gate, FrozenAction, FrozenDispatch};
use crate::autopilot_store::{
    AutopilotDispatchRow, StepApprovalBindMode, RUN_AWAITING_APPROVAL,
    DISPATCH_REQUESTING, DISPATCH_AWAITING, DISPATCH_EXECUTING, DISPATCH_SETTLED,
};

fn gated_flow(skill: bool, tail: bool) -> Value {
    let second = if skill {
        json!({"type":"run_skill", "target_agent":"other", "skill_name":"test",
            "params":{"order":"{{task.title}}"}, "require_approval":true})
    } else {
        json!({"type":"delegate", "target_agent":"other", "prompt":"B {{task.title}}", "require_approval":true})
    };
    let mut steps = vec![json!({"type":"delegate", "target_agent":"plain", "prompt":"A"}), second];
    if tail { steps.push(json!({"type":"delegate", "target_agent":"plain", "prompt":"C"})); }
    json!({"type":"steps", "steps":steps})
}

impl Harness {
    fn broker(&self) -> ApprovalBroker { ApprovalBroker::open(&self.home).unwrap() }

    /// Stop at the actual crash boundary: card filed, ledger still requesting,
    /// step running but unbound, run retaining the filing process's lease.
    async fn filed_step_before_bind(
        &self, engine: &AutopilotEngine, owner: &str, at: DateTime<Utc>,
    ) -> (AutopilotRunRow, AutopilotDispatchRow, ApprovalId) {
        let rule = self.rule(json!({"type":"steps", "steps":[{
            "type":"delegate", "target_agent":"plain", "prompt":"A {{task.title}}", "require_approval":true
        }]})).await;
        engine.start_run(&rule, "task_created", &fields(), at).await.unwrap();
        let run = self.run().await;
        self.acquire();
        assert!(self.store.claim_run(&run.run_id, owner, at).await.unwrap());
        assert!(self.store.mark_step_running(&run.run_id, 0, owner, at).await.unwrap());
        let action: Value = serde_json::from_str(&rule.action).unwrap();
        let frozen_fields = serde_json::from_str(&run.fields_json).unwrap();
        let prepared = engine.prepare_action(&rule.id, &rule.name, &action["steps"][0],
            &frozen_fields, &frozen_fields, None).await.unwrap().unwrap();
        let frozen = FrozenDispatch {
            dispatch_id: uuid::Uuid::new_v4().to_string(), rule_id: rule.id.clone(), rule_name: rule.name.clone(),
            rule_hash: run.rule_hash.clone(), event: run.event_name.clone(), event_ref: run.event_ref.clone(),
            source_agent: None, action: prepared,
        };
        let mut row = frozen.ledger_row(&at.to_rfc3339(),
            &gate::step_dispatch_dedupe_key(&run.rule_id, &run.rule_hash, &run.run_id, 0));
        row.run_id = Some(run.run_id.clone());
        row.step_index = Some(0);
        assert!(self.store.insert_dispatch(&row).await.unwrap());
        let latest = gate::latest_execution_at(Utc::now(), crate::approval::DEFAULT_TTL_SECONDS);
        let id = self.broker().request(gate::REQUESTER, gate::ACTION_KIND, &frozen.summary(&latest),
            frozen.approval_payload(&latest), crate::approval::DEFAULT_TTL_SECONDS).await.unwrap();
        (self.run().await, row, id)
    }

    async fn assert_awaiting_dispatches_bound(&self) {
        for row in self.store.list_dispatches(DISPATCH_AWAITING, 100).await.unwrap() {
            if let Some(id) = &row.run_id {
                let run = self.store.get_run(id).await.unwrap().unwrap();
                assert_eq!(run.state, RUN_AWAITING_APPROVAL);
                assert_eq!(Some(run.step_index), row.step_index);
                assert!(run.lease_owner.is_none() && run.lease_until.is_none());
                let steps = self.store.list_run_steps(id).await.unwrap();
                let step = steps.iter().find(|s| Some(s.step_index) == row.step_index).unwrap();
                assert_eq!(step.state, STEP_RUNNING);
                assert_eq!(step.dispatch_id.as_deref(), Some(row.dispatch_id.as_str()));
                assert!(step.finished_at.is_none());
            }
        }
    }

    fn approval_engine(&self) -> AutopilotEngine {
        let engine = self.engine().with_approval_broker(Arc::new(self.broker()));
        *engine.notice_pusher.lock().unwrap() = Some(Arc::new(|_, _| gate::NoticePush::NoTarget));
        engine
    }

    async fn cards(&self) -> Vec<ApprovalRecord> {
        self.broker().list_by_kind(gate::ACTION_KIND).await.unwrap()
    }

    async fn pending_approval(&self, engine: &AutopilotEngine, skill: bool, tail: bool) -> (AutopilotRunRow, ApprovalId) {
        let rule = self.rule(gated_flow(skill, tail)).await;
        engine.fire_matched_rule_at(&rule, "task_created", &fields(), Utc::now()).await;
        let run = self.run().await;
        assert_eq!(run.state, RUN_AWAITING_APPROVAL);
        assert_eq!(run.step_index, 1);
        assert_eq!(self.queued().await.len(), 1);
        let cards = self.cards().await;
        assert_eq!(cards.len(), 1);
        (run, cards[0].id.clone())
    }

    async fn step_dispatch(&self, run: &AutopilotRunRow) -> AutopilotDispatchRow {
        self.store.get_step_dispatch(&run.run_id, run.step_index).await.unwrap().unwrap()
    }
}

#[tokio::test]
async fn gated_step_waits_for_dashboard_approval() {
    for skill in [false, true] {
        let h = Harness::new();
        h.acquire();
        let engine = h.approval_engine();
        let (run, id) = h.pending_approval(&engine, skill, false).await;
        h.assert_awaiting_dispatches_bound().await;
        let card = h.broker().get(&id).await.unwrap().unwrap();
        assert!(card.summary.contains("第 2 步"));
        assert!(card.payload[gate::PAYLOAD_KEY]["action"]["prompt"].as_str().unwrap().contains("凍結標題"));
        let steps = h.store.list_run_steps(&run.run_id).await.unwrap();
        assert_eq!(steps[0].state, STEP_DONE);
        assert_eq!(steps[1].state, STEP_RUNNING);
        assert_eq!(steps[1].dispatch_id.as_deref(), Some(h.step_dispatch(&run).await.dispatch_id.as_str()));
        engine.resume_runs_once(Utc::now()).await.unwrap();
        assert_eq!(h.cards().await.len(), 1);
        assert_eq!(h.queued().await.len(), 1);
        h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
        assert_eq!(engine.resume_approvals_once().await, 1);
        engine.resume_runs_once(Utc::now()).await.unwrap();
        assert_eq!(engine.resume_approvals_once().await, 0);
        engine.resume_runs_once(Utc::now()).await.unwrap();
        h.assert_outcome("completed", 2).await;
        let queued = h.mq.get_by_id(&runs::queue_id(&run.run_id, 1)).await.unwrap().unwrap();
        assert_eq!(queued.target, "other");
        assert!(queued.payload.contains("凍結標題"));
        assert!(h.store.list_run_steps(&run.run_id).await.unwrap().iter().all(|s| s.state == STEP_DONE));
        assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
        let history = h.store.list_history(Some("r"), 100).await.unwrap();
        assert_eq!(history.iter().filter(|r| r.id == format!("run:{}:step:1", run.run_id) && r.result == "step_done").count(), 1);
    }
}

#[tokio::test]
async fn gated_step_denied_or_expired_settles_run() {
    for expire in [false, true] {
        let h = Harness::new();
        h.acquire();
        let engine = h.approval_engine();
        let (run, id) = h.pending_approval(&engine, false, true).await;
        if expire {
            rusqlite::Connection::open(h.home.join("approvals.db")).unwrap().execute(
                "UPDATE approvals SET created_at='2000-01-01T00:00:00+00:00' WHERE id=?1", [id.as_str()]).unwrap();
            engine.resume_approvals_once().await;
        } else {
            h.broker().decide(&id, false, "dashboard:admin").await.unwrap();
        }
        engine.resume_approvals_once().await;
        engine.resume_runs_once(Utc::now()).await.unwrap();
        let outcome = if expire { "expired" } else { "denied" };
        h.assert_outcome(outcome, 1).await;
        assert_eq!(h.step_dispatch(&run).await.outcome.as_deref(), Some(outcome));
        assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[1].state, STEP_FAILED);
        assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    }
}

#[tokio::test]
async fn gated_step_edited_rule_is_stale_after_approval() {
    let h = Harness::new();
    h.acquire();
    let engine = h.approval_engine();
    let (run, id) = h.pending_approval(&engine, false, false).await;
    h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
    let mut action = gated_flow(false, false);
    action["steps"][1]["prompt"] = json!("changed");
    h.store.update_rule("r", &json!({"action":action})).await.unwrap();
    engine.resume_approvals_once().await;
    h.assert_outcome("stale_rule_changed", 1).await;
    assert_eq!(h.step_dispatch(&run).await.outcome.as_deref(), Some(gate::RESULT_STALE));
}

#[tokio::test]
async fn cancelled_run_retires_approved_step_without_execution() {
    let h = Harness::new();
    h.acquire();
    let engine = h.approval_engine();
    let (run, id) = h.pending_approval(&engine, false, false).await;
    assert!(h.store.settle_unclaimed_run(&run.run_id, &[RUN_AWAITING_APPROVAL], "cancelled", None, Utc::now()).await.unwrap());
    h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
    engine.resume_approvals_once().await;
    engine.resume_runs_once(Utc::now()).await.unwrap();
    h.assert_outcome("cancelled", 1).await;
    assert_eq!(h.step_dispatch(&run).await.outcome.as_deref(), Some(gate::RESULT_STALE));
    assert!(h.broker().get(&id).await.unwrap().unwrap().invalidated_reason.unwrap().starts_with(gate::RETIRED_PREFIX));
}

#[tokio::test]
async fn settled_step_reconciles_after_crash_exactly_once() {
    let h = Harness::new();
    h.acquire();
    let engine = h.approval_engine();
    let (run, id) = h.pending_approval(&engine, false, true).await;
    h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
    engine.crash_after_settle.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(engine.resume_approvals_once().await, 1);
    assert_eq!(h.run().await.state, RUN_AWAITING_APPROVAL);
    assert_eq!(h.step_dispatch(&run).await.outcome.as_deref(), Some(gate::RESULT_SUCCESS));
    assert_eq!(h.queued().await.len(), 2);
    let restarted = h.approval_engine();
    restarted.resume_runs_once(Utc::now()).await.unwrap();
    restarted.resume_runs_once(Utc::now()).await.unwrap();
    assert_eq!(restarted.resume_approvals_once().await, 0);
    h.assert_outcome("completed", 3).await;
    assert_eq!(h.queued().await.iter().filter(|q| q.target == "other").count(), 1);
    assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
}

#[tokio::test]
async fn duplicate_step_filing_reuses_one_card() {
    // Exercise both insert_dispatch's recent dedupe and its permanent UNIQUE
    // constraint after the dedupe window has passed.
    for old in [false, true] {
        let h = Harness::new();
        h.acquire();
        let engine = h.approval_engine();
        let (run, _) = h.pending_approval(&engine, false, false).await;
        let original = h.step_dispatch(&run).await;
        let at = Utc::now();
        {
            let conn = rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap();
            if old {
                conn.execute("UPDATE autopilot_approval_dispatch SET created_at='2000-01-01T00:00:00+00:00' WHERE dispatch_id=?1",
                    [&original.dispatch_id]).unwrap();
            }
        }
        let rule = h.store.get_rule("r").await.unwrap().unwrap();
        let frozen: FrozenDispatch = serde_json::from_str(&original.frozen_json).unwrap();
        engine.gate_step_on_approval(&rule, &run, frozen.action, &fields(), "filing-test", at).await.unwrap();
        assert_eq!(h.cards().await.len(), 1);
        assert_eq!(h.run().await.state, RUN_AWAITING_APPROVAL);
        assert_eq!(h.step_dispatch(&run).await.dispatch_id, original.dispatch_id);
        assert_eq!(h.queued().await.len(), 1);
        assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    }
}

#[tokio::test]
async fn department_gated_run_never_files_approval() {
    let h = Harness::new();
    h.acquire();
    let engine = h.approval_engine();
    h.rule(gated_flow(false, false)).await;
    let rule = h.store.set_rule_extensions("r", &RuleExtensionsPatch {
        owner_department: Some(Some("sales".into())), active_window: None, run_policy: None,
        updated_by_user_id: None, updated_at: run_ts(Utc::now()),
    }).await.unwrap().unwrap();
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), Utc::now()).await;
    assert!(h.store.list_runs_for_rule("r", 100).await.unwrap().is_empty());
    engine.start_run(&rule, "task_created", &fields(), Utc::now()).await.unwrap();
    assert!(h.store.list_runs_for_rule("r", 100).await.unwrap().is_empty());
    assert!(h.store.list_history(Some("r"), 100).await.unwrap().is_empty());
    assert!(h.cards().await.is_empty());
}

#[tokio::test]
async fn approved_run_step_scope_denied_under_fence() {
    let h = Harness::new();
    let engine = h.approval_engine();
    let company_rule = h.rule(gated_flow(false, false)).await;
    let at = Utc::now();
    engine.start_run(&company_rule, "task_created", &fields(), at).await.unwrap();
    let rule = h.store.set_rule_extensions("r", &RuleExtensionsPatch {
        owner_department: Some(Some("sales".into())), active_window: None, run_policy: None,
        updated_by_user_id: None, updated_at: run_ts(Utc::now()),
    }).await.unwrap().unwrap();
    // Construct a bound ledger directly: admission must not be the check that
    // prevents execution in this test.
    rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap().execute(
        "UPDATE autopilot_runs SET owner_department='sales', rule_hash=?1 WHERE rule_id='r'",
        [gate::rule_revision_hash(&rule)],
    ).unwrap();
    let run = h.run().await;
    h.acquire();
    assert!(h.store.claim_run(&run.run_id, "fixture", at).await.unwrap());
    assert!(h.store.mark_step_running(&run.run_id, 0, "fixture", at).await.unwrap());
    let frozen = FrozenDispatch {
        dispatch_id: uuid::Uuid::new_v4().to_string(), rule_id: rule.id.clone(), rule_name: rule.name.clone(),
        rule_hash: run.rule_hash.clone(), event: run.event_name.clone(), event_ref: run.event_ref.clone(),
        source_agent: None, action: FrozenAction::Delegate { target_agent: "plain".into(), prompt: "never".into(), lane: None },
    };
    let mut ledger = frozen.ledger_row(&at.to_rfc3339(), &gate::step_dispatch_dedupe_key(&rule.id, &run.rule_hash, &run.run_id, 0));
    ledger.run_id = Some(run.run_id.clone());
    ledger.step_index = Some(0);
    assert!(h.store.insert_dispatch(&ledger).await.unwrap());
    let latest = gate::latest_execution_at(at, crate::approval::DEFAULT_TTL_SECONDS);
    let id = h.broker().request(gate::REQUESTER, gate::ACTION_KIND, &frozen.summary(&latest),
        frozen.approval_payload(&latest), crate::approval::DEFAULT_TTL_SECONDS).await.unwrap();
    assert!(h.store.bind_step_dispatch_awaiting(&ledger.dispatch_id, id.as_str(), &run.run_id,
        0, "fixture", at, StepApprovalBindMode::Filing).await.unwrap());
    h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
    engine.resume_approvals_once().await;
    assert_eq!(h.run().await.state, RUN_AWAITING_APPROVAL);
    assert!(h.queued().await.is_empty());
    assert_eq!(engine.performed_actions.load(std::sync::atomic::Ordering::SeqCst), 0);
    let settled = h.step_dispatch(&run).await;
    assert_eq!(settled.outcome.as_deref(), Some(gate::RESULT_SKIPPED));
    assert_eq!(settled.detail.as_deref(), Some(DEPARTMENT_CONTAINMENT_SKIP));
}

#[tokio::test]
async fn step_ledger_recovery_preserves_requesting_and_executing_protocol() {
    for state in [DISPATCH_REQUESTING, DISPATCH_EXECUTING] {
        for consumed in [false, true] {
            if state == DISPATCH_REQUESTING && consumed { continue; }
            let h = Harness::new();
            h.acquire();
            let engine = h.approval_engine();
            let (run, id) = h.pending_approval(&engine, false, false).await;
            let row = h.step_dispatch(&run).await;
            if state == DISPATCH_REQUESTING {
                let conn = rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap();
                conn.execute(
                    "UPDATE autopilot_approval_dispatch SET state='requesting', approval_id=NULL WHERE dispatch_id=?1",
                    [&row.dispatch_id]).unwrap();
                conn.execute("UPDATE autopilot_run_steps SET dispatch_id=NULL WHERE run_id=?1 AND step_index=?2",
                    rusqlite::params![run.run_id, run.step_index]).unwrap();
                conn.execute("UPDATE autopilot_runs SET state='running', lease_owner='crashed',
                    lease_until='2000-01-01T00:00:00.000Z' WHERE run_id=?1", [&run.run_id]).unwrap();
            } else {
                h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
                assert!(h.store.transition_dispatch(&row.dispatch_id, DISPATCH_AWAITING, DISPATCH_EXECUTING).await.unwrap());
                if consumed {
                    let rec = h.broker().get(&id).await.unwrap().unwrap();
                    assert!(h.broker().consume_autopilot_approval(&rec, &format!("{}{}", gate::CONSUMED_PREFIX, row.dispatch_id)).await.unwrap());
                }
            }
            let restarted = h.approval_engine();
            restarted.recover_approval_dispatches().await;
            h.assert_awaiting_dispatches_bound().await;
            if consumed {
                h.assert_outcome("execution_uncertain", 1).await;
                assert_eq!(h.step_dispatch(&run).await.state, DISPATCH_SETTLED);
            } else {
                assert_eq!(h.step_dispatch(&run).await.state, DISPATCH_AWAITING);
                if state == DISPATCH_REQUESTING {
                    h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
                }
                restarted.resume_approvals_once().await;
                h.assert_outcome("completed", 2).await;
            }
            assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
        }
    }
}

#[tokio::test]
async fn approval_run_reconciliation_maps_outcomes_and_missing_dispatch() {
    for (outcome, detail, expected) in [
        (Some(gate::RESULT_DENIED), "denied", "denied"),
        (Some(gate::RESULT_EXPIRED), "expired", "expired"),
        (Some(gate::RESULT_STALE), "edited", "stale_rule_changed"),
        (Some(gate::RESULT_BREAKER_OPEN), "breaker", "blocked_breaker_open"),
        (Some(gate::RESULT_SKIPPED), "pause", "autonomy_paused"),
        (Some(gate::RESULT_SKIPPED), "scope_denied", "scope_denied"),
        (Some(gate::RESULT_FAILURE), "failed", "step_failed"),
        (Some(gate::RESULT_APPROVAL_ERROR), "filing failed", "approval_error"),
        (Some(gate::RESULT_UNCERTAIN), "uncertain", "execution_uncertain"),
        (Some("unknown"), "unknown", "approval_error"),
        (None, "missing", "approval_error"),
    ] {
        let h = Harness::new();
        h.acquire();
        let engine = h.approval_engine();
        let (run, _) = h.pending_approval(&engine, false, true).await;
        let row = h.step_dispatch(&run).await;
        if let Some(outcome) = outcome {
            // Even a caller mistakenly asking to count the dispatch cannot
            // increment a run's fire counter a second time.
            assert!(h.store.settle_dispatch(&row.dispatch_id, DISPATCH_AWAITING, outcome, detail, false, true).await.unwrap());
        } else {
            rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap().execute(
                "DELETE FROM autopilot_approval_dispatch WHERE dispatch_id=?1", [&row.dispatch_id]).unwrap();
        }
        engine.resume_runs_once(Utc::now()).await.unwrap();
        engine.resume_runs_once(Utc::now()).await.unwrap();
        h.assert_outcome(expected, 1).await;
        assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    }
}

#[tokio::test]
async fn gated_step_payload_refusal_fails_without_filing() {
    for prompt in [
        "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        "x".repeat(gate::MAX_FROZEN_PAYLOAD_BYTES + 1),
    ] {
        let h = Harness::new();
        h.acquire();
        let engine = h.approval_engine();
        let rule = h.rule(json!({"type":"steps", "steps":[{
            "type":"delegate", "target_agent":"plain", "prompt":prompt, "require_approval":true
        }]})).await;
        engine.fire_matched_rule_at(&rule, "task_created", &fields(), Utc::now()).await;
        let run = h.assert_outcome("approval_error", 0).await;
        assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].state, STEP_FAILED);
        assert!(h.cards().await.is_empty());
        assert!(h.store.get_step_dispatch(&run.run_id, 0).await.unwrap().is_none());
        assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    }
}

#[tokio::test]
async fn gated_skill_render_failure_fails_step_without_filing() {
    let h = Harness::new();
    h.acquire();
    let engine = h.approval_engine();
    let rule = h.rule(json!({"type":"steps", "steps":[{
        "type":"run_skill", "target_agent":"plain", "skill_name":"test",
        "params":{"order":"{{task.deadline_at}}"}, "require_approval":true
    }]})).await;
    engine.fire_matched_rule_at(&rule, "task_created", &fields(), Utc::now()).await;
    let run = h.assert_outcome("step_failed", 0).await;
    assert_eq!(run.detail.as_deref(), Some("param_missing:order"));
    assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].state, STEP_FAILED);
    assert!(h.cards().await.is_empty());
}

#[test]
fn step_dispatch_dedupe_is_v2_and_separates_runs_and_steps() {
    use sha2::{Digest, Sha256};
    let key = gate::step_dispatch_dedupe_key("r", "hash", "run", 1);
    let body = crate::approval::action_grant::canonical_json(&json!({
        "v":2, "rule_id":"r", "rule_hash":"hash", "run_id":"run", "step_index":1,
    }));
    assert_eq!(key, hex::encode(Sha256::digest(body.as_bytes())));
    assert_ne!(key, gate::step_dispatch_dedupe_key("r", "hash", "run", 2));
    assert_ne!(key, gate::step_dispatch_dedupe_key("r", "hash", "another", 1));
    assert_ne!(key, gate::step_dispatch_dedupe_key("r", "edited", "run", 1));
    assert_ne!(key, gate::step_dispatch_dedupe_key("another", "hash", "run", 1));
}

#[tokio::test]
async fn requesting_step_recovery_binds_before_executing_downtime_approval_once() {
    for approved_during_downtime in [false, true] {
        let h = Harness::new();
        let engine = h.approval_engine();
        let (run, row, id) = h.filed_step_before_bind(&engine, "dead-process",
            Utc::now() - ChronoDuration::seconds(61)).await;
        assert_eq!(row.state, DISPATCH_REQUESTING);
        assert!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].dispatch_id.is_none());
        drop(engine);
        if approved_during_downtime {
            h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
        }
        let restarted = h.approval_engine();
        restarted.recover_approval_dispatches().await;
        h.assert_awaiting_dispatches_bound().await;
        assert_eq!(h.run().await.state, RUN_AWAITING_APPROVAL);
        assert_eq!(h.step_dispatch(&run).await.approval_id.as_deref(), Some(id.as_str()));
        assert!(h.queued().await.is_empty());
        if !approved_during_downtime {
            h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
        }
        assert_eq!(restarted.resume_approvals_once().await, 1);
        restarted.recover_approval_dispatches().await;
        assert_eq!(restarted.resume_approvals_once().await, 0);
        restarted.resume_runs_once(Utc::now()).await.unwrap();
        h.assert_outcome("completed", 1).await;
        assert_eq!(h.step_dispatch(&run).await.outcome.as_deref(), Some(gate::RESULT_SUCCESS));
        assert_eq!(h.store.get_rule("r").await.unwrap().unwrap().trigger_count, 1);
    }
}

#[tokio::test]
async fn atomic_step_bind_enforces_filing_and_recovery_leases() {
    for (mode, owner, expired, allowed) in [
        (StepApprovalBindMode::Filing, "owner", false, true),
        (StepApprovalBindMode::Filing, "owner", true, false),
        (StepApprovalBindMode::Filing, "other", false, false),
        (StepApprovalBindMode::Recovery, "owner", false, true),
        (StepApprovalBindMode::Recovery, "owner", true, true),
        (StepApprovalBindMode::Recovery, "other", false, false),
        (StepApprovalBindMode::Recovery, "other", true, true),
    ] {
        let h = Harness::new();
        let engine = h.approval_engine();
        let at = Utc::now();
        let (run, row, id) = h.filed_step_before_bind(&engine, "owner", at).await;
        let before_steps = h.store.list_run_steps(&run.run_id).await.unwrap();
        // Neither legacy entry point can publish a run's unbound dispatch.
        assert!(!h.store.mark_dispatch_awaiting(&row.dispatch_id, id.as_str()).await.unwrap());
        assert!(!h.store.transition_dispatch(&row.dispatch_id, DISPATCH_REQUESTING, DISPATCH_AWAITING).await.unwrap());
        let check_at = if expired { at + ChronoDuration::seconds(61) } else { at };
        assert_eq!(h.store.bind_step_dispatch_awaiting(&row.dispatch_id, id.as_str(),
            &run.run_id, 0, owner, check_at, mode).await.unwrap(), allowed);
        if allowed {
            h.assert_awaiting_dispatches_bound().await;
            assert_eq!(h.step_dispatch(&run).await.state, DISPATCH_AWAITING);
        } else {
            assert_eq!(h.run().await, run);
            assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap(), before_steps);
            assert_eq!(h.step_dispatch(&run).await, row);
        }
    }
}

#[tokio::test]
async fn atomic_step_bind_rolls_back_all_writes_on_error() {
    let h = Harness::new();
    let engine = h.approval_engine();
    let at = Utc::now();
    let (run, row, id) = h.filed_step_before_bind(&engine, "owner", at).await;
    let before_steps = h.store.list_run_steps(&run.run_id).await.unwrap();
    let conn = rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_approval_bind BEFORE UPDATE OF state ON autopilot_runs
        WHEN NEW.state='awaiting_approval' BEGIN SELECT RAISE(ABORT, 'injected bind failure'); END;").unwrap();
    assert!(h.store.bind_step_dispatch_awaiting(&row.dispatch_id, id.as_str(), &run.run_id,
        0, "owner", at, StepApprovalBindMode::Filing).await.is_err());
    assert_eq!(h.run().await, run);
    assert_eq!(h.step_dispatch(&run).await, row);
    assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap(), before_steps);
    assert!(h.store.list_dispatches(DISPATCH_AWAITING, 100).await.unwrap().is_empty());
    conn.execute_batch("DROP TRIGGER reject_approval_bind;").unwrap();
    assert!(h.store.bind_step_dispatch_awaiting(&row.dispatch_id, id.as_str(), &run.run_id,
        0, "owner", at, StepApprovalBindMode::Filing).await.unwrap());
    h.assert_awaiting_dispatches_bound().await;
}

#[tokio::test]
async fn requesting_recovery_rejects_live_foreign_lease_and_settles_run() {
    let h = Harness::new();
    let engine = h.approval_engine();
    let at = Utc::now();
    let (run, row, id) = h.filed_step_before_bind(&engine, "live-foreign-process", at).await;
    h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
    engine.recover_approval_dispatches().await;
    assert_eq!(h.run().await, run);
    assert!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].dispatch_id.is_none());
    let failed = h.store.get_dispatch(&row.dispatch_id).await.unwrap().unwrap();
    assert_eq!(failed.state, DISPATCH_SETTLED);
    assert_eq!(failed.outcome.as_deref(), Some(gate::RESULT_APPROVAL_ERROR));
    assert_eq!(engine.resume_approvals_once().await, 0);
    engine.resume_runs_once(at + ChronoDuration::seconds(61)).await.unwrap();
    h.assert_outcome("approval_error", 0).await;
    assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].state, STEP_FAILED);
    assert_eq!(h.cards().await.len(), 1);
}

fn change_delivery(msg: &mut QueueMessage, field: &str) {
    match field {
        "target" => msg.target = "plain".into(),
        "payload" => msg.payload = "different prompt".into(),
        "lane" => msg.lane = Some("explore".into()),
        "sender" => msg.sender = "another-sender".into(),
        "origin_agent" => msg.origin_agent = None,
        "sender_agent" => msg.sender_agent = None,
        "delegation_depth" => msg.delegation_depth = 1,
        _ => panic!("unknown fixture field"),
    }
}

#[tokio::test]
async fn exact_enqueue_reuses_only_identical_masked_delivery() {
    use crate::message_queue::EnqueueOutcome;
    let h = Harness::new();
    let prompt = format!("Process sk-{}", "x".repeat(40));
    let msg = AutopilotEngine::prompt_queue_message("other", &prompt, None, Some("exact-id"), now());
    assert_eq!(h.mq.enqueue_or_verify_exact(&msg).await.unwrap(), EnqueueOutcome::Inserted);
    let stored = h.mq.get_by_id(&msg.id).await.unwrap().unwrap();
    assert_ne!(stored.payload, prompt);
    assert_eq!(stored.payload, duduclaw_security::credential_detect::mask(&prompt));
    // A completed row still proves the identical delivery, without a new row.
    h.mq.complete(&msg.id, "done").await.unwrap();
    assert_eq!(h.mq.enqueue_or_verify_exact(&msg).await.unwrap(), EnqueueOutcome::AlreadyPresent);
    h.engine().enqueue_prompt_with_id("other", &prompt, None, Some(&msg.id), now()).await.unwrap();
    for field in ["target", "payload", "lane", "sender", "origin_agent", "sender_agent", "delegation_depth"] {
        let mut different = msg.clone();
        change_delivery(&mut different, field);
        assert!(h.mq.enqueue_or_verify_exact(&different).await.is_err(), "{field}");
    }
    assert_eq!(h.mq.statuses_with_prefix("exact-id").await.unwrap().len(), 1);
    assert_eq!(h.mq.get_by_id(&msg.id).await.unwrap().unwrap().payload, stored.payload);
}

#[tokio::test]
async fn recovery_queue_delivery_mismatch_fails_step() {
    for field in ["payload", "lane", "sender", "origin_agent", "sender_agent", "delegation_depth"] {
        let h = Harness::new();
        h.acquire();
        let engine = h.engine();
        let run = h.waiting(&engine).await;
        let started = now() + ChronoDuration::seconds(61);
        assert!(h.store.claim_run(&run.run_id, "crashed", started).await.unwrap());
        assert!(h.store.mark_step_running(&run.run_id, 2, "crashed", started).await.unwrap());
        let id = runs::queue_id(&run.run_id, 2);
        let rule = h.store.get_rule("r").await.unwrap().unwrap();
        let action: Value = serde_json::from_str(&rule.action).unwrap();
        let frozen_fields = serde_json::from_str(&run.fields_json).unwrap();
        let prepared = engine.prepare_action(&rule.id, &rule.name, &action["steps"][2],
            &frozen_fields, &frozen_fields, None).await.unwrap().unwrap();
        let FrozenAction::Delegate { target_agent, prompt, lane } = prepared else { panic!("delegate expected") };
        let mut conflict = AutopilotEngine::prompt_queue_message(&target_agent, &prompt, lane, Some(&id), started);
        change_delivery(&mut conflict, field);
        h.mq.enqueue(&conflict).await.unwrap();
        h.engine().resume_runs_once(started + ChronoDuration::seconds(61)).await.unwrap();
        h.assert_outcome("step_failed", 2).await;
        assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[2].state, STEP_FAILED, "{field}");
    }
}

#[tokio::test]
async fn approved_step_queue_delivery_mismatch_fails_dispatch() {
    for field in ["payload", "lane", "sender"] {
        let h = Harness::new();
        h.acquire();
        let engine = h.approval_engine();
        let (run, id) = h.pending_approval(&engine, false, false).await;
        let row = h.step_dispatch(&run).await;
        let frozen: FrozenDispatch = serde_json::from_str(&row.frozen_json).unwrap();
        let FrozenAction::Delegate { target_agent, prompt, lane } = frozen.action else { panic!("delegate expected") };
        let queue_id = runs::queue_id(&run.run_id, run.step_index);
        let mut conflict = AutopilotEngine::prompt_queue_message(&target_agent, &prompt, lane, Some(&queue_id), Utc::now());
        change_delivery(&mut conflict, field);
        h.mq.enqueue(&conflict).await.unwrap();
        h.broker().decide(&id, true, "dashboard:admin").await.unwrap();
        assert_eq!(engine.resume_approvals_once().await, 1);
        assert_eq!(h.step_dispatch(&run).await.outcome.as_deref(), Some(gate::RESULT_FAILURE), "{field}");
        h.assert_outcome("step_failed", 2).await;
        assert_eq!(engine.resume_approvals_once().await, 0);
        assert_eq!(h.mq.get_by_id(&queue_id).await.unwrap().unwrap().payload, conflict.payload);
    }
}

#[tokio::test]
async fn recovery_render_failure_cannot_accept_existing_queue_row() {
    let h = Harness::new();
    let engine = h.engine();
    let rule = h.rule(json!({"type":"steps","steps":[{
        "type":"run_skill","target_agent":"plain","skill_name":"test"
    }]})).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.run().await;
    h.acquire();
    assert!(h.store.claim_run(&run.run_id, "crashed", now()).await.unwrap());
    assert!(h.store.mark_step_running(&run.run_id, 0, "crashed", now()).await.unwrap());
    let action: Value = serde_json::from_str(&rule.action).unwrap();
    let prepared = engine.prepare_action(&rule.id, &rule.name, &action["steps"][0],
        &fields(), &fields(), None).await.unwrap().unwrap();
    engine.perform_action_with_queue_id(&prepared, Some(&runs::queue_id(&run.run_id, 0)), now()).await.unwrap();
    std::fs::remove_file(h.home.join("agents/plain/SKILLS/test.md")).unwrap();
    engine.resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
    h.assert_outcome("step_failed", 1).await;
    assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].state, STEP_FAILED);
}

#[tokio::test]
async fn recovery_matches_masked_frozen_prompt() {
    let h = Harness::new();
    let engine = h.engine();
    let prompt = format!("Process sk-{} for {{{{task.title}}}}", "x".repeat(40));
    let rule = h.rule(json!({"type":"steps","steps":[{
        "type":"delegate","target_agent":"plain","prompt":prompt
    }]})).await;
    engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
    let run = h.run().await;
    h.acquire();
    assert!(h.store.claim_run(&run.run_id, "crashed", now()).await.unwrap());
    assert!(h.store.mark_step_running(&run.run_id, 0, "crashed", now()).await.unwrap());
    let action: Value = serde_json::from_str(&rule.action).unwrap();
    let frozen_fields = serde_json::from_str(&run.fields_json).unwrap();
    let prepared = engine.prepare_action(&rule.id, &rule.name, &action["steps"][0],
        &frozen_fields, &frozen_fields, None).await.unwrap().unwrap();
    let id = runs::queue_id(&run.run_id, 0);
    engine.perform_action_with_queue_id(&prepared, Some(&id), now()).await.unwrap();
    let FrozenAction::Delegate { prompt, .. } = prepared else { panic!("delegate expected") };
    let stored = h.mq.get_by_id(&id).await.unwrap().unwrap();
    assert_ne!(stored.payload, prompt);
    assert_eq!(stored.payload, duduclaw_security::credential_detect::mask(&prompt));
    h.engine().resume_runs_once(now() + ChronoDuration::seconds(61)).await.unwrap();
    h.assert_outcome("completed", 1).await;
    assert_eq!(h.store.list_run_steps(&run.run_id).await.unwrap()[0].state, STEP_DONE);
}

#[tokio::test]
async fn persisted_department_runs_do_not_resume_from_live_or_frozen_scope() {
    for frozen in [false, true] {
        let h = Harness::new();
        let engine = h.engine();
        let rule = h.rule(flow()).await;
        // No gateway lock: persist a pending run without executing its first step.
        engine.start_run(&rule, "task_created", &fields(), now()).await.unwrap();
        let run = h.run().await;
        if frozen {
            rusqlite::Connection::open(h.home.join("autopilot.db")).unwrap().execute(
                "UPDATE autopilot_runs SET owner_department='sales' WHERE run_id=?1", [&run.run_id],
            ).unwrap();
        } else {
            h.store.set_rule_extensions("r", &RuleExtensionsPatch {
                owner_department: Some(Some("sales".into())), active_window: None, run_policy: None,
                updated_by_user_id: None, updated_at: run_ts(now()),
            }).await.unwrap();
        }
        let before = h.store.list_history(Some("r"), 100).await.unwrap().len();
        h.acquire();
        engine.resume_runs_once(now()).await.unwrap();
        assert_eq!(h.run().await.state, RUN_PENDING);
        assert!(h.queued().await.is_empty());
        assert_eq!(engine.performed_actions.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(h.store.list_history(Some("r"), 100).await.unwrap().len(), before);
    }
}
