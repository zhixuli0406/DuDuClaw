//! CD-3 fake-comp integration tests: `take_over`/`watch` (task brief items
//! 1/3). Split out of `tests.rs` (already at this project's per-file size
//! convention — see that file's own header comment) into its own file, same
//! "new scenarios get their own `tests_<topic>.rs`" pattern
//! `tests_identity.rs` already established. Reuses `tests.rs`'s fake-comp
//! harness (`spawn_fake_comp`/`tempdir`/`write_codrive_config`/`plain_step`/
//! `consequential_step`, all `pub(super)`) rather than duplicating it —
//! `credential_class_auto_converts_to_take_over_and_never_sends_the_
//! credential_text` (in `tests.rs` itself, next to the CD-1/CD-2 credential
//! test it replaced) covers the `Credential`-class auto-conversion path;
//! this file covers the explicit `CodriveAction::TakeOver` step, the
//! `watch_mode` script flag, and the frozen/timeout/emergency-stop edges of
//! a take_over step specifically.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::approval::ApprovalBroker;

use super::driver::run_script;
use super::script::{CodriveAction, CodriveScript};
use super::tests::{plain_step, spawn_fake_comp, tempdir, write_codrive_config};

#[tokio::test]
async fn explicit_take_over_step_completes_and_skips_approval() {
    let home = tempdir("tk-explicit");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "登入頁面".into(),
        steps: vec![
            plain_step("點擊使用者名稱欄位", CodriveAction::Move { x: 10.0, y: 10.0 }),
            take_over_step("交給人類輸入帳密"),
            plain_step("送出後等待", CodriveAction::Wait { ms: 10 }),
        ],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps.len(), 3);
    assert_eq!(report.steps[0].outcome, "applied");
    assert_eq!(report.steps[1].outcome, "taken_over");
    assert_eq!(report.steps[1].approval_id, None);
    assert_eq!(report.steps[2].outcome, "applied", "the script must continue past the take_over step");

    let received = fake.received.lock().await;
    assert!(received.iter().any(|v| v["op"] == "take_over" && v["reason"] == "交給人類輸入帳密"));

    let broker = ApprovalBroker::open(&home).expect("open broker");
    assert!(broker.list_pending(None).await.expect("list_pending").is_empty());
}

/// A take_over step whose hand-off never gets resumed within the whole-
/// script deadline aborts honestly — same "「交還」是明確動作, never an
/// implicit timeout-based resume" contract the ordinary frozen-retry path
/// (`frozen_forever_aborts_on_session_timeout` in `tests.rs`) already has.
#[tokio::test]
async fn take_over_step_frozen_forever_aborts_on_session_timeout() {
    let home = tempdir("tk-timeout");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 2); // 2s whole-script deadline
    fake.frozen.store(true, Ordering::SeqCst); // never resumed

    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "登入頁面".into(),
        steps: vec![take_over_step("交給人類輸入帳密")],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_frozen_timeout");
    assert_eq!(report.steps[0].outcome, "aborted");
}

/// The frozen→resumed dance: a take_over step's hand-off is polled via
/// `status` (same `wait_for_resume` the ordinary frozen-retry path uses)
/// until a (simulated) human hands back — proving the driver doesn't
/// special-case "wait for take_over" separately from "wait for an ordinary
/// freeze to clear."
#[tokio::test]
async fn take_over_step_waits_for_simulated_human_resume() {
    let home = tempdir("tk-resume");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 15);
    fake.frozen.store(true, Ordering::SeqCst);

    let frozen_flag = fake.frozen.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        frozen_flag.store(false, Ordering::SeqCst);
    });

    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "登入頁面".into(),
        steps: vec![take_over_step("交給人類輸入帳密")],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(report.steps[0].outcome, "taken_over");
    let received = fake.received.lock().await;
    assert!(received.iter().any(|v| v["op"] == "status"), "must have polled status while waiting for hand-back");
}

/// CD-3 item 3: `watch_mode: true` sends `{"op":"watch","enable":true}`
/// right after connect, before any step.
#[tokio::test]
async fn watch_mode_true_enables_watch_before_the_first_step() {
    let home = tempdir("watch-on");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "敏感操作全程需要人在場".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 1.0, y: 1.0 })],
        watch_mode: true,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    let received = fake.received.lock().await;
    assert_eq!(received.len(), 2, "expected exactly [watch, move]: {received:?}");
    assert_eq!(received[0]["op"], "watch");
    assert_eq!(received[0]["enable"], true);
    assert_eq!(received[1]["op"], "move");
}

/// The default (existing scripts, `watch_mode` omitted/false) is byte-
/// identical to pre-CD-3 behavior — no `watch` op sent at all.
#[tokio::test]
async fn watch_mode_false_sends_no_watch_op() {
    let home = tempdir("watch-off");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "一般操作".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 1.0, y: 1.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    let received = fake.received.lock().await;
    assert!(!received.iter().any(|v| v["op"] == "watch"), "watch_mode:false must never send a watch op: {received:?}");
}

/// Test-local constructor for a `CodriveStep` whose action is the explicit
/// CD-3 `TakeOver` variant — same shape as `plain_step`/`consequential_step`.
fn take_over_step(reason: &str) -> super::script::CodriveStep {
    super::script::CodriveStep {
        narration: format!("take_over: {reason}"),
        highlight: None,
        action: CodriveAction::TakeOver { reason: reason.to_string() },
        consequential: None,
        api_action: None,
        locate: None,
    }
}
