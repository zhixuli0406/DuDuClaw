//! CD-4 (WP-CD4b) fake-comp integration tests: the C-L3 AT-SPI2 locate
//! checkpoint `step::run_one_step` inserts one rung below the C-L2 registry
//! checkpoint (`tests_registry.rs`, same fake-comp harness reuse pattern).
//!
//! `cargo test` — on this project's macOS dev loop AND on a normal Linux CI
//! runner with no accessibility bus/`at-spi2-registryd` running — can never
//! reliably produce a `LocateOutcome::Located` hit: on macOS,
//! `atspi_locate::locate` is the honest non-Linux stub (`Failed`
//! unconditionally); on a bare Linux box, `AccessibilityConnection::new()`
//! fails fast (`Failed`) or, on the rare box that DOES have a working a11y
//! bus but no `codrive-test-fixture`-named application connected to it,
//! resolves to `Miss`. Either way the OBSERVABLE behavior these tests pin
//! down — a MISS/FAILED locate falls straight back to the step's own
//! literal C-L1 coordinates, unchanged — is identical, so assertions below
//! accept either `locate_outcome` rather than asserting one specific value.
//! A live `Located` hit against a real `gnome-text-editor` AT-SPI tree is
//! container/live-fire verification, not a permanent unit test — see the
//! WP-CD4b report.

use std::time::Duration;

use super::driver::run_script;
use super::script::{CodriveAction, CodriveScript, LocateRequest};
use super::tests::{locate_step, plain_step, spawn_fake_comp, tempdir, write_codrive_config};

fn loc_req(role: &str, name: &str) -> LocateRequest {
    LocateRequest { role: role.to_string(), name: name.to_string() }
}

// ── miss/failed: unchanged C-L1 injection ───────────────────────────────

#[tokio::test]
async fn locate_miss_or_failed_falls_back_and_injects_normally() {
    let home = tempdir("locate-fallback");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "codrive-test-fixture".into(),
        task_summary: "C-L3 locate miss/failed".into(),
        steps: vec![locate_step(
            "點擊儲存按鈕",
            CodriveAction::Click { x: 42.0, y: 43.0, btn: super::client::CodriveButton::Left },
            loc_req("button", "儲存"),
        )],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(15), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "applied", "a locate MISS/FAILED must fall back to the ordinary C-L1 outcome");

    // The fallback must carry the step's ORIGINAL literal coordinates —
    // not something a partially-applied locate mutated in place.
    let received = fake.received.lock().await;
    assert_eq!(received.len(), 3, "click = move + press + release: {received:?}");
    assert_eq!(received[0]["op"], "move");
    assert_eq!(received[0]["x"], 42.0);
    assert_eq!(received[0]["y"], 43.0);
    drop(received);

    assert_locate_audit_row(&home, &["locate_miss_fallback", "locate_failed_fallback"]);
}

// ── consequential + locate: approval gate still applies ─────────────────

#[tokio::test]
async fn consequential_locate_step_still_waits_for_approval() {
    use crate::approval::ApprovalBroker;

    let home = tempdir("locate-approve");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let mut step = locate_step(
        "點擊送出按鈕（需審批）",
        CodriveAction::Click { x: 7.0, y: 8.0, btn: super::client::CodriveButton::Left },
        loc_req("button", "送出"),
    );
    step.consequential =
        Some(super::script::CodriveConsequential { class: super::script::ConsequentialClass::Submit, description: "送出表單".to_string() });

    let script = CodriveScript { target_app: "codrive-test-fixture".into(), task_summary: "C-L3 + approval".into(), steps: vec![step], watch_mode: false };

    let home2 = home.clone();
    let handle = tokio::spawn(async move { run_script(&home2, "agent1", script).await });

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let approval_id = wait_for_one_pending(&broker, Duration::from_secs(5)).await;
    assert!(fake.received.lock().await.is_empty(), "nothing must reach comp before approval");

    broker.decide(&approval_id, true, "test").await.expect("decide");

    let report = tokio::time::timeout(Duration::from_secs(15), handle).await.expect("join").expect("no panic");
    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "applied");
    assert!(report.steps[0].approval_id.is_some());

    let received = fake.received.lock().await;
    assert!(!received.is_empty(), "the fallback coordinate action must reach comp after approval");
    assert_eq!(received[0]["op"], "move");
}

async fn wait_for_one_pending(broker: &crate::approval::ApprovalBroker, timeout: Duration) -> crate::approval::ApprovalId {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let pending = broker.list_pending(None).await.expect("list_pending");
        if let Some(rec) = pending.into_iter().next() {
            return rec.id;
        }
        assert!(tokio::time::Instant::now() < deadline, "approval row never appeared");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── plain step (no `locate`): unaffected — regression pin ───────────────

#[tokio::test]
async fn plain_step_without_locate_is_unaffected() {
    let home = tempdir("locate-unset");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "codrive-test-fixture".into(),
        task_summary: "no locate set".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 11.0, y: 12.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(report.steps[0].outcome, "applied");
    let received = fake.received.lock().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["op"], "move");
    assert_eq!(received[0]["x"], 11.0);
    assert_eq!(received[0]["y"], 12.0);
}

fn assert_locate_audit_row(home: &std::path::Path, expected_any_of: &[&str]) {
    let rows = duduclaw_security::audit::read_tool_calls_since(home, "agent1", "2020-01-01T00:00:00Z");
    let hit = rows.iter().find(|r| {
        r.get("tool_name").and_then(|v| v.as_str()) == Some("codrive_run")
            && r.get("locate_outcome")
                .and_then(|v| v.as_str())
                .is_some_and(|v| expected_any_of.contains(&v))
    });
    assert!(hit.is_some(), "expected a tool_calls.jsonl row with locate_outcome in {expected_any_of:?}, got: {rows:?}");
}
