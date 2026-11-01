//! C8 / ET5.4: a notice is always recorded in the Activity Feed; a push needs
//! the policy, `[proactive] enabled`, the per-window cap and the scorer, in
//! that order, and never carries a result summary.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::super::notify::{
    NOTICE, NOTIFIED, NoticeEvent, NoticeOutcome, NoticeScorer, NoticeSender, Notifier,
};
use super::*;
use crate::goal_notify::NotifyOutcome;
use crate::proactive_gate::{GateDecision, ProactiveConfig};

#[derive(Default)]
struct Scorer {
    calls: AtomicUsize,
    suppress: Option<&'static str>,
}

#[async_trait]
impl NoticeScorer for Scorer {
    async fn decide(&self, _: &Path, _: &str, _: &ProactiveConfig, _: &str) -> GateDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.suppress {
            Some(reason) => GateDecision::Suppress { reason },
            None => GateDecision::Allow,
        }
    }
}

#[derive(Default)]
struct Sender {
    sent: Mutex<Vec<String>>,
}

#[async_trait]
impl NoticeSender for Sender {
    async fn send(&self, _: &Path, _: &str, text: &str) -> NotifyOutcome {
        self.sent.lock().unwrap().push(text.to_string());
        NotifyOutcome::Sent
    }
}

fn notify_env(cap: i64, proactive: bool) -> Env {
    let env = Env::with_config(&format!(
        "[dispatch]\nenabled = true\n\n[responsibilities]\nenabled = true\n\
         max_notifications_per_period = {cap}\n"
    ));
    let dir = env.home().join("agents").join(OWNER);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = {OWNER:?}\nstatus = \"active\"\n[proactive]\nenabled = {proactive}\n"),
    )
    .unwrap();
    env
}

async fn resp_with_policy(env: &Env, on: bool) -> ResponsibilityRow {
    let now = t0();
    let mut i = input(now);
    if on {
        i.notification_policy = Some(serde_json::json!({"enabled": true, "on": ["result"]}));
    }
    create(env, &i, now).await
}

async fn count(env: &Env, kind: &str) -> usize {
    env.store
        .list_activity(None, Some(kind), 1000, 0)
        .await
        .unwrap()
        .0
        .len()
}

async fn run(
    env: &Env,
    resp: &ResponsibilityRow,
    scorer: &Scorer,
    sender: &Sender,
) -> NoticeOutcome {
    run_for(env, resp, scorer, sender, "occ-1").await
}

async fn run_for(
    env: &Env,
    resp: &ResponsibilityRow,
    scorer: &Scorer,
    sender: &Sender,
    task_id: &str,
) -> NoticeOutcome {
    if env.store.get_task(task_id).await.unwrap().is_none() {
        env.store.insert_task(&crate::task_store::TaskRow::new(
            task_id.into(), "notice fixture".into(), "description".into(),
            "medium".into(), OWNER.into(), "system".into(),
        )).await.unwrap();
    }
    let n = Notifier {
        home: env.home(),
        store: &env.store,
        scorer,
        sender,
    };
    let ev = NoticeEvent::Result {
        task_id: task_id.into(),
        outcome: "done".into(),
    };
    n.notify(resp, &ev, t0()).await
}

#[tokio::test]
async fn policy_off_records_activity_only() {
    let env = notify_env(10, true);
    let resp = resp_with_policy(&env, false).await;
    let (scorer, sender) = (Scorer::default(), Sender::default());
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Suppressed("policy_off")
    );
    assert_eq!(count(&env, NOTICE).await, 1);
    assert!(sender.sent.lock().unwrap().is_empty());
    assert_eq!(scorer.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn proactive_disabled_never_calls_scorer_or_sender() {
    let env = notify_env(10, false);
    let resp = resp_with_policy(&env, true).await;
    let (scorer, sender) = (Scorer::default(), Sender::default());
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Suppressed("proactive_disabled")
    );
    assert_eq!(count(&env, NOTICE).await, 1);
    assert_eq!(scorer.calls.load(Ordering::SeqCst), 0);
    assert!(sender.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scorer_suppression_stops_the_push() {
    let env = notify_env(10, true);
    let resp = resp_with_policy(&env, true).await;
    let scorer = Scorer {
        suppress: Some("llm_error"),
        ..Scorer::default()
    };
    let sender = Sender::default();
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Suppressed("llm_error")
    );
    assert!(sender.sent.lock().unwrap().is_empty());
    assert_eq!(count(&env, NOTIFIED).await, 0);
}

#[tokio::test]
async fn push_has_no_result_summary_and_respects_the_period_cap() {
    let env = notify_env(1, true);
    let resp = resp_with_policy(&env, true).await;
    let mut t = crate::task_store::TaskRow::new(
        "occ-1".into(),
        "t".into(),
        "d".into(),
        "medium".into(),
        OWNER.into(),
        "s".into(),
    );
    t.result_summary = Some("SECRET-RESULT-TEXT".into());
    env.store.insert_task(&t).await.unwrap();
    let (scorer, sender) = (Scorer::default(), Sender::default());
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Pushed
    );
    let text = sender.sent.lock().unwrap()[0].clone();
    assert!(!text.contains("SECRET-RESULT-TEXT"), "{text}");
    assert!(text.contains("已完成"), "{text}");
    assert_eq!(count(&env, NOTIFIED).await, 1);
    // A different notice in the same window meets the cap. (Round 3: the
    // same notice repeated is now a duplicate, see the next test.)
    assert_eq!(
        run_for(&env, &resp, &scorer, &sender, "occ-2").await,
        NoticeOutcome::Suppressed("period_cap")
    );
    assert_eq!(sender.sent.lock().unwrap().len(), 1);
    assert_eq!(count(&env, NOTICE).await, 2, "both notices recorded");
}

/// Round 3: the same notice is handled once, whatever calls it again.
#[tokio::test]
async fn the_same_notice_is_handled_once() {
    let env = notify_env(10, true);
    let resp = resp_with_policy(&env, true).await;
    let (scorer, sender) = (Scorer::default(), Sender::default());
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Pushed
    );
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Suppressed("already_handled")
    );
    assert_eq!(sender.sent.lock().unwrap().len(), 1);
    assert_eq!(count(&env, NOTICE).await, 1);
}

/// S-M6: the period cap counts the dedicated notice log, so Activity rows an
/// employee can post (`activity_post`) neither use up nor reset it.
#[tokio::test]
async fn forged_activity_rows_do_not_move_the_cap() {
    let env = notify_env(1, true);
    let resp = resp_with_policy(&env, true).await;
    for i in 0..5 {
        env.store
            .append_activity(&crate::task_store::ActivityRow {
                id: format!("forged-{i}"),
                event_type: NOTIFIED.into(),
                agent_id: OWNER.into(),
                task_id: None,
                summary: "x".into(),
                timestamp: crate::task_store::resp_ts(t0()),
                metadata: Some(
                    serde_json::json!({"responsibility_id": resp.responsibility_id}).to_string(),
                ),
            })
            .await
            .unwrap();
    }
    let (scorer, sender) = (Scorer::default(), Sender::default());
    assert_eq!(
        run(&env, &resp, &scorer, &sender).await,
        NoticeOutcome::Pushed
    );
}

#[tokio::test]
async fn autonomy_pause_responsibility_notice_does_not_score_or_push_and_next_notice_resumes() {
    let env = notify_env(10, true);
    let resp = resp_with_policy(&env, true).await;
    let (scorer, sender) = (Scorer::default(), Sender::default());
    duduclaw_core::autonomy_gate::set_pause(env.home(), Some(OWNER), "pause", "user").unwrap();
    assert_eq!(run(&env, &resp, &scorer, &sender).await, NoticeOutcome::Suppressed("autonomy_paused"));
    assert_eq!(scorer.calls.load(Ordering::SeqCst), 0);
    assert!(sender.sent.lock().unwrap().is_empty());
    assert_eq!(count(&env, NOTICE).await, 1);
    duduclaw_core::autonomy_gate::resume(env.home(), Some(OWNER)).unwrap();
    assert_eq!(run_for(&env, &resp, &scorer, &sender, "occ-2").await, NoticeOutcome::Pushed);
    assert_eq!(scorer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(sender.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pause_after_notice_preflight_prevents_scorer_model_handoff() {
    struct PausingScorer {
        paused: Option<String>,
        entered: AtomicUsize,
        model_calls: AtomicUsize,
    }
    #[async_trait]
    impl NoticeScorer for PausingScorer {
        async fn decide(&self, home: &Path, _: &str, _: &ProactiveConfig, _: &str) -> GateDecision {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let outcome = duduclaw_core::autonomy_gate::mutate_async(
                home, self.paused.as_deref(),
                Some(duduclaw_core::autonomy_gate::PauseEntry {
                    reason: "between check and send".into(),
                    who: "operator".into(),
                    at: chrono::Utc::now().to_rfc3339(),
                }),
            ).await;
            assert!(matches!(outcome, duduclaw_core::autonomy_gate::MutationOutcome::Applied(_)));
            match duduclaw_core::autonomy_handoff::admit().await {
                Ok(admission) => {
                    self.model_calls.fetch_add(1, Ordering::SeqCst);
                    admission.handed_off();
                    GateDecision::Allow
                }
                Err(_) => GateDecision::Suppress { reason: "autonomy_paused" },
            }
        }
    }
    for paused in [Some(OWNER), Some("notice-creator"), None] {
        let env = notify_env(10, true);
        let dir = env.home().join("agents/notice-creator");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nstatus = \"active\"\n").unwrap();
        let resp = resp_with_policy(&env, true).await;
        let mut task = crate::task_store::TaskRow::new(
            "notice-race".into(), "title".into(), "description".into(),
            "medium".into(), OWNER.into(), "notice-creator".into(),
        );
        task.created_via = Some("agent".into());
        env.store.insert_task(&task).await.unwrap();
        let scorer = PausingScorer {
            paused: paused.map(str::to_string),
            entered: AtomicUsize::new(0),
            model_calls: AtomicUsize::new(0),
        };
        let sender = Sender::default();
        let notifier = Notifier { home: env.home(), store: &env.store, scorer: &scorer, sender: &sender };
        let event = NoticeEvent::Result { task_id: task.id.clone(), outcome: "done".into() };
        assert_eq!(notifier.notify(&resp, &event, t0()).await, NoticeOutcome::Suppressed("autonomy_paused"));
        assert_eq!(scorer.entered.load(Ordering::SeqCst), 1);
        assert_eq!(scorer.model_calls.load(Ordering::SeqCst), 0);
        assert!(sender.sent.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn missing_notice_task_refuses_before_scoring() {
    let env = notify_env(10, true);
    let resp = resp_with_policy(&env, true).await;
    let scorer = Scorer::default();
    let sender = Sender::default();
    let notifier = Notifier { home: env.home(), store: &env.store, scorer: &scorer, sender: &sender };
    let event = NoticeEvent::Result { task_id: "missing-task".into(), outcome: "done".into() };
    assert_eq!(notifier.notify(&resp, &event, t0()).await, NoticeOutcome::Suppressed("task_unreadable"));
    assert_eq!(scorer.calls.load(Ordering::SeqCst), 0);
    assert!(sender.sent.lock().unwrap().is_empty());
}
