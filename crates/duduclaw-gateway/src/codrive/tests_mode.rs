//! A2 integration tests: the driving-mode block on the run report, the
//! DESIGN §3.5 plan-approval card, and the auth-handshake timeout that
//! keeps a queued second agent from hanging on the exclusive driving seat.
//!
//! Own file per this module tree's convention (`tests.rs` / `driver.rs`
//! are both already at the per-file size convention — see their header
//! comments). Reuses `tests.rs`'s `tempdir` and step constructors; the
//! fake comp here is a SEPARATE, A2-aware server rather than a parameter
//! bolted onto `spawn_fake_comp`, because several tests below need the
//! pre-A2 server's exact byte-for-byte behavior as the control group.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex as AsyncMutex;

use crate::approval::{ApprovalBroker, ApprovalId};

use super::client::{CodriveClient, CodriveClientError};
use super::driver::run_script;
use super::mode::{CodriveDrivingMode, CodriveHandoverReason};
use super::script::{CodriveAction, CodriveScript};
use super::tests::{plain_step, spawn_fake_comp, tempdir, write_codrive_config};

/// How an A2-aware fake comp should describe itself.
struct A2Behavior {
    /// Mode token pushed as a `driving_mode` event right after a
    /// successful auth (A2 §3.2: the human→codrive transition IS a real
    /// mode change, so comp pushes one). `None` = push nothing.
    push_on_auth: Option<&'static str>,
    /// Mode token reported on a `status` ack while NOT frozen.
    live_mode: &'static str,
    /// Mode token + reason reported on a `status` ack while frozen.
    frozen_mode: &'static str,
    frozen_reason: &'static str,
}

impl Default for A2Behavior {
    fn default() -> Self {
        Self {
            push_on_auth: Some("codrive"),
            live_mode: "codrive",
            frozen_mode: "handover",
            frozen_reason: "human_input",
        }
    }
}

struct FakeA2Comp {
    received: Arc<AsyncMutex<Vec<Value>>>,
    frozen: Arc<AtomicBool>,
}

/// Fake comp that speaks the A2 §3.1/§3.2 shapes on top of the CD-1 wire
/// protocol. Everything not related to the driving-mode block behaves
/// exactly like `tests::spawn_fake_comp`.
fn spawn_fake_a2_comp(
    home: &Path,
    token: &str,
    behavior: A2Behavior,
) -> (PathBuf, PathBuf, FakeA2Comp) {
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
                if got != expected_token {
                    let _ = write_half
                        .write_all(b"{\"ok\":false,\"error\":\"auth_failed\"}\n")
                        .await;
                    break;
                }
                // The push comes FIRST, exactly as comp would emit it the
                // moment `session_active` flips: the client must stash the
                // event while looking for its auth ack.
                if let Some(mode) = behavior.push_on_auth {
                    let ev = json!({"event": "driving_mode", "mode": mode, "reason": null});
                    let _ = write_half.write_all(format!("{ev}\n").as_bytes()).await;
                }
                let _ = write_half
                    .write_all(b"{\"ok\":true,\"authenticated\":true}\n")
                    .await;
                continue;
            }

            received2.lock().await.push(v);
            let is_frozen = frozen2.load(Ordering::SeqCst);
            let ack = match op.as_str() {
                "status" if is_frozen => json!({
                    "ok": true, "frozen": true, "terminated": false, "takeover": false,
                    "mode": behavior.frozen_mode, "handover_reason": behavior.frozen_reason,
                    "shadow": false, "watch_active": false, "watch_paused": false
                }),
                "status" => json!({
                    "ok": true, "frozen": false, "terminated": false, "takeover": false,
                    "mode": behavior.live_mode, "handover_reason": null,
                    "shadow": false, "watch_active": false, "watch_paused": false
                }),
                "resume" => json!({"ok": false, "error": "resume_is_human_only"}),
                _ if is_frozen => {
                    json!({"ok": false, "frozen": true, "reason": "agent_seat_frozen"})
                }
                _ => json!({"ok": true, "frozen": false}),
            };
            let _ = write_half.write_all(format!("{ack}\n").as_bytes()).await;
        }
    });

    (sock_path, token_path, FakeA2Comp { received, frozen })
}

fn write_config(
    home: &Path,
    sock: &Path,
    tok: &Path,
    approval_ttl_secs: i64,
    max_session_secs: u64,
    plan_approval: bool,
) {
    let content = format!(
        "[codrive]\nsocket_path = \"{}\"\ntoken_path = \"{}\"\nconnect_timeout_secs = 5\n\
         approval_ttl_secs = {approval_ttl_secs}\nmax_session_secs = {max_session_secs}\n\
         plan_approval = {plan_approval}\n",
        sock.display(),
        tok.display(),
    );
    std::fs::write(home.join("config.toml"), content).unwrap();
}

fn one_step_script(app: &str, summary: &str) -> CodriveScript {
    CodriveScript {
        target_app: app.to_string(),
        task_summary: summary.to_string(),
        steps: vec![plain_step("移動", CodriveAction::Move { x: 1.0, y: 2.0 })],
        watch_mode: false,
    }
}

// ── driving mode on the run report ──────────────────────────────────────

/// The §3.2 push comp emits when the session goes live is what fills
/// `driving_mode_at_start` — no `status` query is involved.
#[tokio::test]
async fn driving_mode_push_at_session_start_lands_in_the_report() {
    let home = tempdir("a2-start");
    let (sock, tok, _fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 30, 30, false);

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(
        report.final_state, "completed",
        "detail: {:?}",
        report.detail
    );
    assert_eq!(
        report.driving_mode_at_start,
        Some(CodriveDrivingMode::CoDrive)
    );
    assert_eq!(
        report.driving_mode_at_end,
        Some(CodriveDrivingMode::CoDrive)
    );
    assert_eq!(report.handover_reason_at_end, None);
}

/// Reporting a mode must cost ZERO extra wire ops — the whole reason the
/// client observes passively instead of probing with `status`. A one-step
/// script must still put exactly one op on the socket.
#[tokio::test]
async fn reporting_the_mode_adds_no_wire_ops() {
    let home = tempdir("a2-noops");
    let (sock, tok, fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 30, 30, false);

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    let received = fake.received.lock().await;
    assert_eq!(
        received.len(),
        1,
        "a one-step script must send exactly [move] — no status probe may be added: {received:?}"
    );
    assert_eq!(received[0]["op"], "move");
}

/// Version skew, gateway ahead of comp: a pre-A2 compositor reports no
/// mode, and the report says so honestly instead of defaulting to `human`.
#[tokio::test]
async fn a_pre_a2_comp_yields_none_not_a_fabricated_human() {
    let home = tempdir("a2-legacy");
    let (sock, tok, _fake) = spawn_fake_comp(&home, "goodtoken", None);
    write_codrive_config(&home, &sock, &tok, 30, 30);

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(report.driving_mode_at_start, None);
    assert_eq!(report.driving_mode_at_end, None);
    assert_eq!(report.handover_reason_at_end, None);
}

/// Version skew the other way: a comp that knows a mode this gateway does
/// not must not brick the run, and must not be laundered into `human`.
#[tokio::test]
async fn an_unknown_mode_token_is_reported_verbatim_and_the_run_still_completes() {
    let home = tempdir("a2-unknown");
    let (sock, tok, _fake) = spawn_fake_a2_comp(
        &home,
        "goodtoken",
        A2Behavior {
            push_on_auth: Some("teleop"),
            live_mode: "teleop",
            ..A2Behavior::default()
        },
    );
    write_config(&home, &sock, &tok, 30, 30, false);

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(
        report.driving_mode_at_start,
        Some(CodriveDrivingMode::Unknown("teleop".into()))
    );
    assert_ne!(
        report.driving_mode_at_start,
        Some(CodriveDrivingMode::Human)
    );
}

/// The frozen-retry loop's `status` polls carry the A2 block, so a session
/// that ends while the human still holds the wheel reports `handover` plus
/// its reason — again with no query added for the purpose.
#[tokio::test]
async fn a_session_that_ends_in_handover_reports_the_mode_and_reason() {
    let home = tempdir("a2-handover");
    let (sock, tok, fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 30, 3, false); // 3s whole-script deadline
    fake.frozen.store(true, Ordering::SeqCst); // never handed back

    let report = tokio::time::timeout(
        Duration::from_secs(15),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_frozen_timeout");
    assert_eq!(
        report.driving_mode_at_end,
        Some(CodriveDrivingMode::Handover)
    );
    assert_eq!(
        report.handover_reason_at_end,
        Some(CodriveHandoverReason::HumanInput)
    );
}

// ── DESIGN §3.5 plan-approval card ──────────────────────────────────────

/// Default OFF: an existing deployment must behave byte-identically — no
/// card, no wait, no new failure mode.
#[tokio::test]
async fn plan_approval_off_files_no_card_at_all() {
    let home = tempdir("plan-off");
    let (sock, tok, fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 30, 30, false);

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(report.final_state, "completed");
    assert_eq!(fake.received.lock().await.len(), 1);

    let broker = ApprovalBroker::open(&home).expect("open broker");
    assert!(
        broker
            .list_pending(None)
            .await
            .expect("list_pending")
            .is_empty(),
        "plan_approval = false must never file a session card"
    );
}

#[tokio::test]
async fn plan_approval_approved_lets_the_session_run() {
    let home = tempdir("plan-ok");
    let (sock, tok, fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 60, 30, true);

    let home2 = home.clone();
    let handle = tokio::spawn(async move {
        run_script(&home2, "agent1", one_step_script("foot", "測試")).await
    });

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let id = wait_for_plan_card(&broker, Duration::from_secs(5)).await;
    assert!(
        fake.received.lock().await.is_empty(),
        "the socket must not be touched until the plan is approved"
    );
    broker.decide(&id, true, "test").await.expect("decide");

    let report = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("join")
        .expect("no panic");
    assert_eq!(
        report.final_state, "completed",
        "detail: {:?}",
        report.detail
    );
    assert_eq!(fake.received.lock().await.len(), 1);
}

#[tokio::test]
async fn plan_approval_denied_aborts_without_ever_connecting() {
    let home = tempdir("plan-deny");
    let (sock, tok, fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 60, 30, true);

    let home2 = home.clone();
    let handle = tokio::spawn(async move {
        run_script(&home2, "agent1", one_step_script("foot", "測試")).await
    });

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let id = wait_for_plan_card(&broker, Duration::from_secs(5)).await;
    broker.decide(&id, false, "test").await.expect("decide");

    let report = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("join")
        .expect("no panic");
    assert_eq!(report.final_state, "aborted_plan_denied");
    assert!(report.steps.is_empty());
    assert!(
        fake.received.lock().await.is_empty(),
        "a denied plan must never open a co-drive session"
    );
    // No session ⇒ nothing to report a mode about; must stay honest `None`.
    assert_eq!(report.driving_mode_at_start, None);
}

/// TTL expiry is a denial (fail-closed), same doctrine as the per-step gate.
#[tokio::test]
async fn plan_approval_expiry_is_a_denial() {
    let home = tempdir("plan-expire");
    let (sock, tok, fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 1, 30, true); // 1s TTL, nobody decides

    let report = tokio::time::timeout(
        Duration::from_secs(15),
        run_script(&home, "agent1", one_step_script("foot", "測試")),
    )
    .await
    .expect("run_script must finish");

    assert_eq!(report.final_state, "aborted_plan_denied");
    assert!(fake.received.lock().await.is_empty());
}

/// The refuse-list still wins outright: a banned script is refused before
/// any human is asked, so the card is never filed (design §8-3 — "not even
/// an approval row is created").
#[tokio::test]
async fn denylist_still_refuses_before_the_plan_card_is_filed() {
    let home = tempdir("plan-denylist");
    let (sock, tok, _fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 60, 30, true);

    let script = one_step_script("chrome", "填寫 captcha 驗證碼");
    let report = tokio::time::timeout(Duration::from_secs(10), run_script(&home, "agent1", script))
        .await
        .expect("run_script must finish");

    assert_eq!(report.final_state, "refused_denylist");
    let broker = ApprovalBroker::open(&home).expect("open broker");
    assert!(
        broker
            .list_pending(None)
            .await
            .expect("list_pending")
            .is_empty()
    );
}

/// The card carries a digest, never the script body — a `text` step's
/// payload must not end up in `approvals.db`, the dashboard, or a channel
/// push.
#[tokio::test]
async fn the_plan_card_never_carries_the_script_body() {
    let home = tempdir("plan-nobody");
    let (sock, tok, _fake) = spawn_fake_a2_comp(&home, "goodtoken", A2Behavior::default());
    write_config(&home, &sock, &tok, 60, 30, true);

    const SECRET: &str = "super-secret-passphrase";
    let script = CodriveScript {
        target_app: "foot".into(),
        task_summary: "填表".into(),
        steps: vec![plain_step("輸入", CodriveAction::Text { s: SECRET.into() })],
        watch_mode: false,
    };

    let home2 = home.clone();
    let handle = tokio::spawn(async move { run_script(&home2, "agent1", script).await });

    let broker = ApprovalBroker::open(&home).expect("open broker");
    let id = wait_for_plan_card(&broker, Duration::from_secs(5)).await;
    let rec = broker
        .get(&id)
        .await
        .expect("get")
        .expect("record must exist");
    assert_eq!(rec.action_kind, "codrive_session");
    assert!(
        !rec.summary.contains(SECRET),
        "summary leaked: {}",
        rec.summary
    );
    assert!(
        !rec.payload.to_string().contains(SECRET),
        "payload leaked: {}",
        rec.payload
    );
    assert!(
        rec.summary.contains("foot"),
        "summary must name the app: {}",
        rec.summary
    );

    broker.decide(&id, false, "test").await.expect("decide");
    let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
}

async fn wait_for_plan_card(broker: &ApprovalBroker, timeout: Duration) -> ApprovalId {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let pending = broker.list_pending(None).await.expect("list_pending");
        if let Some(rec) = pending
            .into_iter()
            .find(|r| r.action_kind == "codrive_session")
        {
            return rec.id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "plan approval card never appeared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── exclusive driving seat: the queued client must not hang ─────────────

/// comp accepts co-drive connections one at a time, so a second agent's
/// connection sits in the kernel backlog: `connect()` succeeds, the auth
/// line is buffered, and the auth ACK never arrives. This pins that the
/// handshake read is bounded by `[codrive] connect_timeout_secs` — i.e.
/// the queued client fails honestly instead of hanging forever.
#[tokio::test]
async fn auth_handshake_read_is_bounded_when_nobody_accepts() {
    let home = tempdir("a2-authwait");
    let sock = home.join("codrive.sock");
    // Bound but NEVER accepted — exactly what a busy comp looks like from
    // the outside while another session holds the seat.
    let _listener = UnixListener::bind(&sock).unwrap();

    let started = std::time::Instant::now();
    // `expect_err` is unavailable here — `CodriveClient` is deliberately not
    // `Debug` (it owns a live socket), so unwrap the Result by hand.
    let connected = tokio::time::timeout(
        Duration::from_secs(10),
        CodriveClient::connect(&sock, "goodtoken", Duration::from_secs(1)),
    )
    .await
    .expect("connect must not hang past the test's own guard");
    let Err(err) = connected else {
        panic!("an un-accepted handshake must fail, not succeed");
    };

    assert!(
        matches!(err, CodriveClientError::Timeout(_)),
        "expected a timeout, got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the handshake must be bounded by connect_timeout_secs, took {:?}",
        started.elapsed()
    );
}
