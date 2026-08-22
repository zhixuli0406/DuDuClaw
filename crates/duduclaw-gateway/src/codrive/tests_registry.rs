//! CD-4 (WP-CD4a) fake-comp integration tests: the C-L2 registry
//! checkpoint `step::run_one_step` inserts ahead of the ordinary
//! coordinate-based dispatch. Reuses `tests.rs`'s fake-comp harness
//! (`spawn_fake_comp`/`tempdir`/`write_codrive_config`/`plain_step`/
//! `api_action_step`, all `pub(super)`) — same "new scenarios get their own
//! `tests_<topic>.rs`" pattern `tests_takeover.rs` already established.
//!
//! `TEST_FIXTURE` (`registry.rs`, `cfg(test)`-only) stands in for a real
//! third-party app: a single `touch_file` `Cli` action shelling out to
//! `/usr/bin/touch`, present on every unix this suite runs on. It exists so
//! these tests can exercise the FULL `run_script` → `step` →
//! `registry::dispatch` path — including the "a hit sends zero comp
//! events" claim — without depending on Chromium or a live NetworkManager/
//! D-Bus session being installed wherever `cargo test` happens to run.

use std::time::Duration;

use super::driver::run_script;
use super::script::{ApiActionRequest, CodriveAction, CodriveScript};
use super::tests::{api_action_step, plain_step, spawn_fake_comp, tempdir, write_codrive_config};

fn api_req(action: &str, params: serde_json::Value) -> ApiActionRequest {
    ApiActionRequest { action: action.to_string(), params }
}

// ── hit: registry serves the step, comp never sees it ──────────────────

#[tokio::test]
async fn registry_hit_step_executes_without_touching_comp() {
    let home = tempdir("reg-hit");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let dir = std::env::temp_dir().join(format!("codrive-reg-hit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("touched").to_string_lossy().to_string();

    let script = CodriveScript {
        target_app: "codrive-test-fixture".into(),
        task_summary: "C-L2 registry hit".into(),
        steps: vec![api_action_step(
            "建立標記檔（C-L2）",
            CodriveAction::Move { x: 1.0, y: 1.0 }, // never reached on a hit
            api_req("touch_file", serde_json::json!({"path": target})),
        )],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].outcome, "api_action");
    assert!(std::path::Path::new(&target).exists(), "the C-L2 side effect must be real");

    let received = fake.received.lock().await;
    assert!(
        received.is_empty(),
        "a registry hit must never send anything to comp (no highlight/move/click/status): {received:?}"
    );
    drop(received);

    assert_registry_audit_row(&home, "executed");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── miss: unchanged C-L1 injection ──────────────────────────────────────

#[tokio::test]
async fn registry_miss_step_falls_back_and_injects_normally() {
    let home = tempdir("reg-miss");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "C-L2 registry miss".into(),
        steps: vec![api_action_step(
            "移動游標",
            CodriveAction::Move { x: 3.0, y: 4.0 },
            api_req("no_such_action", serde_json::Value::Null),
        )],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "applied", "a registry MISS must fall back to the ordinary C-L1 outcome");

    let received = fake.received.lock().await;
    assert_eq!(received.len(), 1, "the fallback coordinate action must still reach comp: {received:?}");
    assert_eq!(received[0]["op"], "move");
    drop(received);

    assert_registry_audit_row(&home, "registry_miss_fallback");
}

// ── exec failure: same fallback contract as a miss ──────────────────────

#[tokio::test]
async fn registry_exec_failure_falls_back_and_injects_normally() {
    let home = tempdir("reg-fail");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    // `touch_file`'s params_schema requires `path` — omitting it makes
    // `render_argv` fail deterministically (no filesystem/process
    // dependency), exercising the FAILED branch rather than the MISS one.
    let script = CodriveScript {
        target_app: "codrive-test-fixture".into(),
        task_summary: "C-L2 registry exec failure".into(),
        steps: vec![api_action_step(
            "移動游標",
            CodriveAction::Move { x: 5.0, y: 6.0 },
            api_req("touch_file", serde_json::Value::Null),
        )],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "applied", "an exec FAILURE must fall back to the ordinary C-L1 outcome");

    let received = fake.received.lock().await;
    assert_eq!(received.len(), 1, "the fallback coordinate action must still reach comp: {received:?}");
    assert_eq!(received[0]["op"], "move");
    drop(received);

    assert_registry_audit_row(&home, "exec_failed_fallback");
}

// ── consequential + api_action: approval gate still applies ────────────

#[tokio::test]
async fn consequential_api_action_step_still_waits_for_approval() {
    use crate::approval::ApprovalBroker;

    let home = tempdir("reg-approve");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let dir = std::env::temp_dir().join(format!("codrive-reg-approve-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("touched").to_string_lossy().to_string();

    let mut step = api_action_step(
        "建立備忘檔（C-L2，需審批）",
        CodriveAction::Move { x: 1.0, y: 1.0 },
        api_req("touch_file", serde_json::json!({"path": target.clone()})),
    );
    step.consequential = Some(super::script::CodriveConsequential {
        class: super::script::ConsequentialClass::Other,
        description: "建立本機標記檔".to_string(),
    });

    let script = CodriveScript {
        target_app: "codrive-test-fixture".into(),
        task_summary: "C-L2 + approval".into(),
        steps: vec![step],
        watch_mode: false,
    };

    let home2 = home.clone();
    let handle = tokio::spawn(async move { run_script(&home2, "agent1", script).await });

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let approval_id = wait_for_one_pending(&broker, Duration::from_secs(5)).await;
    assert!(!std::path::Path::new(&target).exists(), "the C-L2 side effect must not happen before approval");

    broker.decide(&approval_id, true, "test").await.expect("decide");

    let report = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("join")
        .expect("no panic");
    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "api_action");
    assert!(report.steps[0].approval_id.is_some());
    assert!(std::path::Path::new(&target).exists(), "the C-L2 side effect must happen after approval");
    assert!(fake.received.lock().await.is_empty(), "an approved C-L2 hit still never touches comp");

    let _ = std::fs::remove_dir_all(&dir);
}

async fn wait_for_one_pending(
    broker: &crate::approval::ApprovalBroker,
    timeout: Duration,
) -> crate::approval::ApprovalId {
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

/// Plain step (no `api_action`) must be byte-identical to the pre-C-L2
/// behavior — this is the "existing scripts see zero behavior change"
/// regression pin, using this file's own fixture app id (which no
/// pre-existing test targets) so a future registry entry named "foot" or
/// similar can't silently start intercepting it.
#[tokio::test]
async fn plain_step_without_api_action_is_unaffected_by_the_registry() {
    let home = tempdir("reg-unset");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "codrive-test-fixture".into(),
        task_summary: "no api_action set".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 9.0, y: 9.0 })],
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
}

fn assert_registry_audit_row(home: &std::path::Path, expected_registry_outcome: &str) {
    let rows = duduclaw_security::audit::read_tool_calls_since(home, "agent1", "2020-01-01T00:00:00Z");
    let hit = rows.iter().find(|r| {
        r.get("tool_name").and_then(|v| v.as_str()) == Some("codrive_run")
            && r.get("registry_outcome").and_then(|v| v.as_str()) == Some(expected_registry_outcome)
    });
    assert!(
        hit.is_some(),
        "expected a tool_calls.jsonl row with registry_outcome={expected_registry_outcome:?}, got: {rows:?}"
    );
}
