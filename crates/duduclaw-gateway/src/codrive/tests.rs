//! Integration tests for the CD-1 co-drive driver against a fake comp
//! socket server that speaks the exact wire protocol described in the WP
//! brief (`commercial/docs/DESIGN-codrive-desktop-2026-08.md`). unix-only
//! (see `mod.rs`'s `#[cfg(all(test, unix))]`) — `CodriveClient` itself is
//! a no-op stub on non-unix targets.
//!
//! **CD-2 file-size note**: this file is already near this project's
//! per-file convention (200-400 lines typical, 800 max). New test
//! scenarios for a DIFFERENT codrive concern should open their own
//! `tests_<topic>.rs` (declared from `mod.rs`, same pattern as
//! `tests_identity.rs`) instead of growing this file further — reserve
//! additions here for genuine variations on `run_script`'s fake-comp
//! integration tests this file already covers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex as AsyncMutex;

use crate::approval::ApprovalBroker;

use super::driver::run_script;
use super::script::{
    CodriveAction, CodriveConsequential, CodriveScript, CodriveStep, ConsequentialClass,
};

/// Unix domain socket paths are capped at `SUN_LEN` (~104 bytes on macOS) —
/// `std::env::temp_dir()` resolves to a long per-user `$TMPDIR` on macOS
/// (`/var/folders/.../T/`) that alone eats most of that budget, so tests
/// that bind a real `UnixListener` use `/tmp` directly with a short,
/// 8-hex-char suffix instead of a full UUID. `pub(super)` (not private) so
/// `tests_takeover.rs` (CD-3's own "new scenarios get their own file"
/// module, per this file's header comment) can reuse this whole fake-comp
/// harness instead of duplicating an ~80-line fake server a second time —
/// unlike `live_tests.rs`'s much smaller `tempdir` duplicate, this harness
/// is substantial enough that DRY wins over per-file self-containment here.
pub(super) fn tempdir(label: &str) -> PathBuf {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let p = PathBuf::from("/tmp").join(format!("cdt-{label}-{}", &suffix[..8]));
    std::fs::create_dir_all(&p).unwrap();
    p
}

pub(super) fn write_codrive_config(
    home: &Path,
    sock_path: &Path,
    token_path: &Path,
    approval_ttl_secs: i64,
    max_session_secs: u64,
) {
    let content = format!(
        "[codrive]\nsocket_path = \"{}\"\ntoken_path = \"{}\"\nconnect_timeout_secs = 5\napproval_ttl_secs = {approval_ttl_secs}\nmax_session_secs = {max_session_secs}\n",
        sock_path.display(),
        token_path.display(),
    );
    std::fs::write(home.join("config.toml"), content).unwrap();
}

/// Shared state a fake comp server exposes back to the test driving it.
pub(super) struct FakeComp {
    /// Every post-auth command line received, in order (including ones
    /// rejected for being frozen — the test asserts on `outcome`, not on
    /// this log alone, when that distinction matters).
    pub(super) received: Arc<AsyncMutex<Vec<Value>>>,
    pub(super) frozen: Arc<AtomicBool>,
}

/// Spawn a fake comp server listening on `home/codrive.sock`, with token
/// file `home/codrive.token` containing `token`. Speaks the exact wire
/// protocol from the WP brief. `emergency_stop_after` — if set, sends
/// `{"event":"emergency_stop"}` then closes the connection right after
/// that many post-auth commands, instead of acking the triggering one.
pub(super) fn spawn_fake_comp(
    home: &Path,
    token: &str,
    emergency_stop_after: Option<usize>,
) -> (PathBuf, PathBuf, FakeComp) {
    let sock_path = home.join("codrive.sock");
    let token_path = home.join("codrive.token");
    std::fs::write(&token_path, token).unwrap();

    let listener = UnixListener::bind(&sock_path).unwrap();
    let received = Arc::new(AsyncMutex::new(Vec::new()));
    let frozen = Arc::new(AtomicBool::new(false));
    let received2 = received.clone();
    let frozen2 = frozen.clone();
    let expected_token = token.to_string();

    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut count = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
                break;
            };
            let op = v
                .get("op")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();

            if op == "auth" {
                let got = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
                let ack = if got == expected_token {
                    json!({"ok": true, "authenticated": true})
                } else {
                    json!({"ok": false, "error": "auth_failed"})
                };
                let _ = write_half.write_all(format!("{ack}\n").as_bytes()).await;
                if got != expected_token {
                    break; // comp closes the connection on auth failure
                }
                continue;
            }

            received2.lock().await.push(v);
            count += 1;
            if emergency_stop_after == Some(count) {
                let _ = write_half
                    .write_all(b"{\"event\":\"emergency_stop\"}\n")
                    .await;
                break; // client's next read sees EOF -> Terminated
            }

            let ack = match op.as_str() {
                "status" => {
                    json!({"ok": true, "frozen": frozen2.load(Ordering::SeqCst), "terminated": false})
                }
                "resume" => json!({"ok": false, "error": "resume_is_human_only"}),
                _ if frozen2.load(Ordering::SeqCst) => {
                    json!({"ok": false, "frozen": true, "reason": "agent_seat_frozen"})
                }
                _ => json!({"ok": true, "frozen": false}),
            };
            let _ = write_half.write_all(format!("{ack}\n").as_bytes()).await;
        }
    });

    (sock_path, token_path, FakeComp { received, frozen })
}

pub(super) fn plain_step(narration: &str, action: CodriveAction) -> CodriveStep {
    CodriveStep {
        narration: narration.to_string(),
        highlight: None,
        action,
        consequential: None,
        api_action: None,
        locate: None,
    }
}

pub(super) fn consequential_step(
    narration: &str,
    action: CodriveAction,
    class: ConsequentialClass,
    desc: &str,
) -> CodriveStep {
    CodriveStep {
        narration: narration.to_string(),
        highlight: None,
        action,
        consequential: Some(CodriveConsequential {
            class,
            description: desc.to_string(),
        }),
        api_action: None,
        locate: None,
    }
}

/// C-L2 (WP-CD4a) variant: an ordinary plain step (same fallback `action`
/// `plain_step` would build) that ALSO carries an `api_action` request —
/// `tests_registry.rs` uses this to exercise the registry-check checkpoint
/// `step::run_one_step` inserts ahead of the ordinary coordinate dispatch.
/// `pub(super)` for the same reason the rest of this harness is: reused
/// from a sibling `tests_<topic>.rs` file, not duplicated.
pub(super) fn api_action_step(
    narration: &str,
    fallback_action: CodriveAction,
    api_action: super::script::ApiActionRequest,
) -> CodriveStep {
    CodriveStep {
        narration: narration.to_string(),
        highlight: None,
        action: fallback_action,
        consequential: None,
        api_action: Some(api_action),
        locate: None,
    }
}

/// C-L3 (WP-CD4b) variant: an ordinary plain step that ALSO carries a
/// `locate` request — `tests_atspi_locate.rs` uses this to exercise the
/// locate checkpoint `step::run_one_step` inserts ahead of the ordinary
/// coordinate dispatch, mirroring `api_action_step` one rung down.
pub(super) fn locate_step(
    narration: &str,
    fallback_action: CodriveAction,
    locate: super::script::LocateRequest,
) -> CodriveStep {
    CodriveStep {
        narration: narration.to_string(),
        highlight: None,
        action: fallback_action,
        consequential: None,
        api_action: None,
        locate: Some(locate),
    }
}

// ── auth ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn auth_success_completes_simple_script() {
    let home = tempdir("auth-ok");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "測試".into(),
        steps: vec![plain_step("move", CodriveAction::Move { x: 1.0, y: 2.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].outcome, "applied");
    let received = fake.received.lock().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["op"], "move");
}

#[tokio::test]
async fn auth_failure_aborts_before_any_action() {
    let home = tempdir("auth-fail");
    let (sock, tok, _fake) = spawn_fake_comp(&home, "goodtoken", None);
    // The driver reads whatever is in the token file — write a mismatching
    // value so the server rejects the auth handshake.
    std::fs::write(&tok, "wrongtoken").unwrap();
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "test".into(),
        steps: vec![plain_step("move", CodriveAction::Move { x: 1.0, y: 2.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_connect");
    assert!(report.detail.unwrap().contains("auth"));
    assert!(report.steps.is_empty());
}

// ── refuse-list / credential — never even connect ───────────────────────

#[tokio::test]
async fn deny_keyword_refuses_before_connect() {
    let home = tempdir("deny-kw");
    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "填寫 captcha 驗證碼".into(),
        steps: vec![plain_step("click", CodriveAction::Move { x: 1.0, y: 1.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(5), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "refused_denylist");
    assert!(report.steps.is_empty());
}

// CD-3 (task brief item 1): a `Credential`-classed step no longer refuses
// the whole script — it auto-converts to a `take_over` hand-off. This
// replaces the CD-1/CD-2-era `credential_class_refuses_before_connect_and_
// creates_no_approval_row` test, which pinned the superseded behavior; the
// invariant it protected ("credential class must never create an approval
// row") is preserved below, just via a different final_state.
#[tokio::test]
async fn credential_class_auto_converts_to_take_over_and_never_sends_the_credential_text() {
    let home = tempdir("cred-takeover");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "chrome".into(),
        task_summary: "登入銀行".into(),
        steps: vec![consequential_step(
            "輸入密碼",
            CodriveAction::Text {
                s: "hunter2".into(),
            },
            ConsequentialClass::Credential,
            "輸入登入密碼",
        )],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].outcome, "taken_over");
    assert_eq!(report.steps[0].approval_id, None, "a take_over step must never carry an approval id");

    let received = fake.received.lock().await;
    // [take_over, status] — `wait_for_resume` always polls `status` at
    // least once (see `step::wait_for_resume`), even against this fake
    // server, which starts unfrozen and so hands back on the first poll.
    assert_eq!(received.len(), 2, "expected exactly [take_over, status]: {received:?}");
    assert_eq!(received[0]["op"], "take_over");
    assert_eq!(received[0]["reason"], "輸入登入密碼", "the reason should be the consequential description, not the credential text");
    assert!(
        !received.iter().any(|v| v["op"] == "text"),
        "the credential text must NEVER be sent to comp: {received:?}"
    );
    drop(received);

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let pending = broker.list_pending(None).await.expect("list_pending");
    assert!(
        pending.is_empty(),
        "credential class must never create an approval row"
    );
}

// ── approval gate ────────────────────────────────────────────────────

#[tokio::test]
async fn approved_consequential_step_sends_action_only_after_decision() {
    let home = tempdir("approve-ok");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "建立備忘檔".into(),
        steps: vec![
            plain_step("先移動", CodriveAction::Move { x: 1.0, y: 1.0 }),
            consequential_step(
                "送出指令",
                CodriveAction::KeyName {
                    name: "enter".into(),
                },
                ConsequentialClass::Submit,
                "執行輸入的指令",
            ),
        ],
        watch_mode: false,
    };

    let home2 = home.clone();
    let handle = tokio::spawn(async move { run_script(&home2, "agent1", script).await });

    // Wait for the approval row to appear, then confirm the consequential
    // step's action has NOT reached comp yet (only step0's move has).
    let broker = ApprovalBroker::open(&home).expect("open broker");
    let approval_id = wait_for_one_pending(&broker, Duration::from_secs(5)).await;
    assert_eq!(
        fake.received.lock().await.len(),
        1,
        "consequential action must not be sent before approval"
    );

    broker
        .decide(&approval_id, true, "test")
        .await
        .expect("decide");

    let report = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("join")
        .expect("no panic");
    assert_eq!(report.final_state, "completed");
    assert_eq!(report.steps.len(), 2);
    assert_eq!(report.steps[1].outcome, "applied");
    assert!(report.steps[1].approval_id.is_some());

    let received = fake.received.lock().await;
    assert_eq!(received.len(), 3); // move, key_name press, key_name release
    assert_eq!(received[1]["op"], "key_name");
    assert_eq!(received[1]["state"], "press");
    assert_eq!(received[2]["state"], "release");
}

#[tokio::test]
async fn denied_consequential_step_never_sends_action_and_aborts() {
    let home = tempdir("approve-deny");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "刪除檔案".into(),
        steps: vec![consequential_step(
            "刪除",
            CodriveAction::KeyName {
                name: "delete".into(),
            },
            ConsequentialClass::Delete,
            "刪除選取的檔案",
        )],
        watch_mode: false,
    };

    let home2 = home.clone();
    let handle = tokio::spawn(async move { run_script(&home2, "agent1", script).await });

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let approval_id = wait_for_one_pending(&broker, Duration::from_secs(5)).await;
    broker
        .decide(&approval_id, false, "test")
        .await
        .expect("decide");

    let report = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("join")
        .expect("no panic");
    assert_eq!(report.final_state, "aborted_approval_denied");
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].outcome, "denied");
    assert!(
        fake.received.lock().await.is_empty(),
        "denied action must never reach comp"
    );
}

#[tokio::test]
async fn expired_consequential_step_never_sends_action_and_aborts() {
    let home = tempdir("approve-expire");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 1, 30); // 1s TTL, nobody decides

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "送出付款".into(),
        steps: vec![consequential_step(
            "送出",
            CodriveAction::KeyName {
                name: "enter".into(),
            },
            ConsequentialClass::Purchase,
            "送出付款表單",
        )],
        watch_mode: false,
    };

    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_approval_expired");
    assert_eq!(report.steps[0].outcome, "denied");
    assert!(fake.received.lock().await.is_empty());
}

async fn wait_for_one_pending(
    broker: &ApprovalBroker,
    timeout: Duration,
) -> crate::approval::ApprovalId {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let pending = broker.list_pending(None).await.expect("list_pending");
        if let Some(rec) = pending.into_iter().next() {
            return rec.id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "approval row never appeared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── freeze / resume ──────────────────────────────────────────────────

#[tokio::test]
async fn frozen_action_is_reapplied_after_resume() {
    let home = tempdir("frozen-resume");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 15);
    fake.frozen.store(true, Ordering::SeqCst);

    // Simulate the human resuming (Super+Enter) after ~1.2s — comp would
    // clear its own frozen flag; here that's the fake server's flag.
    let frozen_flag = fake.frozen.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        frozen_flag.store(false, Ordering::SeqCst);
    });

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "移動游標".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 5.0, y: 5.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(report.steps[0].outcome, "dropped_frozen_reapplied");
    let received = fake.received.lock().await;
    assert!(
        received.iter().any(|v| v["op"] == "status"),
        "must have polled status while frozen"
    );
}

#[tokio::test]
async fn frozen_forever_aborts_on_session_timeout() {
    let home = tempdir("frozen-timeout");
    let (sock, tok, fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 2); // 2s whole-script deadline
    fake.frozen.store(true, Ordering::SeqCst); // never resumed

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "移動游標".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 5.0, y: 5.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_frozen_timeout");
    assert_eq!(report.steps[0].outcome, "aborted");
}

// ── emergency stop ───────────────────────────────────────────────────

#[tokio::test]
async fn emergency_stop_event_aborts_the_script() {
    let home = tempdir("estop");
    let (sock, tok, _fake) = spawn_fake_comp(&home, "goodtoken", Some(1));
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "移動游標".into(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 5.0, y: 5.0 })],
        watch_mode: false,
    };
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_emergency_stop");
    assert_eq!(report.steps[0].outcome, "aborted");
}
