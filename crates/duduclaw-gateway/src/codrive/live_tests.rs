//! Permanent `#[ignore]` live-bridge harness: the REAL CD-1 driver
//! (`driver::run_script` + `client::CodriveClient` + the real
//! `ApprovalBroker`) against the REAL `duduclaw-comp` compositor running in
//! its BUILD.md container stack (weston headless → comp → foot), bridged
//! across the Docker boundary with a TCP relay. This is the acceptance
//! evidence for DESIGN-codrive-desktop-2026-08.md §5's CD-1 row: "一次後果
//! 性動作走 ApprovalBroker 審批卡核准後才執行；拒絕路徑同驗" — the fake-comp
//! integration tests in `tests.rs` pin the driver's behavior, this pins that
//! both real endpoints actually speak the same wire protocol.
//!
//! Why a bridge: comp is Linux-only (detached workspace, container-built per
//! its BUILD.md) while this workspace's tests run on the Mac host, and
//! Docker-for-Mac cannot share a Unix socket across the VM boundary. The
//! relay carries the byte stream verbatim — both protocol endpoints are the
//! real implementations, only the transport hop in between is test rigging.
//!
//! ## Playbook (run manually; see comp's BUILD.md "CD-1 live-bridge
//! verification" for the copy-paste container commands)
//!
//! 1. Start the comp container stack with `cargo build`, weston, comp, foot,
//!    and `socat TCP-LISTEN:7777,fork,reuseaddr UNIX-CONNECT:$XDG_RUNTIME_DIR/duduclaw-codrive.sock`,
//!    publishing 127.0.0.1:7777.
//! 2. `docker cp <cid>:/tmp/xdg-runtime/duduclaw-codrive.token /tmp/cd1-live-token`
//! 3. On the host, bridge a local Unix socket to that TCP port (python3
//!    one-liner in BUILD.md) at e.g. `/tmp/cd1-live.sock`.
//! 4. `DUDUCLAW_CODRIVE_LIVE_SOCK=/tmp/cd1-live.sock \
//!     DUDUCLAW_CODRIVE_LIVE_TOKEN=/tmp/cd1-live-token \
//!     cargo test -p duduclaw-gateway --lib codrive::live_tests -- --ignored --nocapture`
//! 5. Verify the side effects inside the container: `/tmp/cd1-live.txt`
//!    exists with the expected content (approve path), `/tmp/cd1-deny.txt`
//!    does NOT exist (deny path), and the comp audit log
//!    (`$XDG_RUNTIME_DIR/duduclaw-codrive-audit.jsonl`) shows the injected
//!    events in order.

use std::path::PathBuf;
use std::time::Duration;

use crate::approval::ApprovalBroker;

use super::driver::run_script;
use super::script::{
    CodriveAction, CodriveConsequential, CodriveHighlight, CodriveScript, CodriveStep,
    ConsequentialClass,
};

/// Same short-path rationale as `tests.rs`'s helper (macOS `SUN_LEN` cap):
/// duplicated locally rather than shared because `tests.rs` sits near the
/// per-file size cap and this harness must stay self-contained enough to
/// read as a playbook.
fn tempdir(label: &str) -> PathBuf {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let p = PathBuf::from("/tmp").join(format!("cdt-{label}-{}", &suffix[..8]));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Poll the real broker until exactly one pending `codrive_action` shows up,
/// then decide it. Returns the decided approval id. Panics (failing the
/// test) if nothing shows up within ~30s — a silent decider would otherwise
/// turn an approval-plumbing regression into an opaque TTL timeout.
fn spawn_decider(home: PathBuf, approve: bool) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let broker = ApprovalBroker::open(&home).expect("decider: broker open failed");
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let pending = broker.list_pending(None).await.unwrap_or_default();
            if let Some(rec) = pending.iter().find(|r| r.action_kind == "codrive_action") {
                broker
                    .decide(&rec.id, approve, "live-test-human")
                    .await
                    .expect("decider: decide failed");
                return rec.id.to_string();
            }
        }
        panic!("decider: no pending codrive_action appeared within 30s");
    })
}

fn live_env() -> (PathBuf, PathBuf) {
    let sock = std::env::var("DUDUCLAW_CODRIVE_LIVE_SOCK")
        .expect("set DUDUCLAW_CODRIVE_LIVE_SOCK to the host-side bridge socket (see module playbook)");
    let token = std::env::var("DUDUCLAW_CODRIVE_LIVE_TOKEN")
        .expect("set DUDUCLAW_CODRIVE_LIVE_TOKEN to the copied token file (see module playbook)");
    (PathBuf::from(sock), PathBuf::from(token))
}

fn live_home(label: &str, sock: &std::path::Path, token: &std::path::Path) -> PathBuf {
    let home = tempdir(label);
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[codrive]\nsocket_path = \"{}\"\ntoken_path = \"{}\"\nconnect_timeout_secs = 10\napproval_ttl_secs = 120\nmax_session_secs = 120\n",
            sock.display(),
            token.display(),
        ),
    )
    .unwrap();
    home
}

fn step(narration: &str, action: CodriveAction) -> CodriveStep {
    CodriveStep { narration: narration.to_string(), highlight: None, action, consequential: None, api_action: None }
}

/// One sequential test (not two) on purpose: comp accepts a single
/// connection at a time, and `cargo test`'s default parallelism would make
/// two live tests race for it.
#[tokio::test]
#[ignore = "live bridge harness — needs the comp container stack + relay, see module playbook"]
async fn live_bridge_approve_then_deny_round() {
    let (sock, token) = live_env();

    // ── Approve path: the consequential Enter is approved, the command the
    // agent typed into foot's real shell executes, the file appears. ──
    let home = live_home("live-ap", &sock, &token);
    let decider = spawn_decider(home.clone(), true);
    let script = CodriveScript {
        target_app: "foot".to_string(),
        task_summary: "在終端機建立標記檔（live 活測·核准路徑）".to_string(),
        steps: vec![
            CodriveStep {
                narration: "點擊終端機輸入區".to_string(),
                highlight: Some(CodriveHighlight { x: 150.0, y: 100.0, w: 300.0, h: 100.0 }),
                action: CodriveAction::Click { x: 200.0, y: 150.0, btn: super::client::CodriveButton::Left },
                consequential: None,
                api_action: None,
            },
            step("輸入建檔指令", CodriveAction::Text { s: "echo cd1live > /tmp/cd1-live.txt".to_string() }),
            CodriveStep {
                narration: "送出指令".to_string(),
                highlight: None,
                action: CodriveAction::KeyName { name: "enter".to_string() },
                consequential: Some(CodriveConsequential {
                    class: ConsequentialClass::Submit,
                    description: "在共享桌面的終端機執行已輸入的指令".to_string(),
                }),
                api_action: None,
            },
            step("等待畫面更新", CodriveAction::Wait { ms: 500 }),
        ],
        watch_mode: false,
    };
    let report = run_script(&home, "live-test-agent", script).await;
    let decided_id = decider.await.expect("decider task panicked");
    println!("approve-path report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "completed", "approve path must complete: {:?}", report.detail);
    let enter_step = &report.steps[2];
    assert_eq!(enter_step.outcome, "applied");
    assert_eq!(enter_step.approval_id.as_deref(), Some(decided_id.as_str()), "the applied consequential step must carry the approval the decider granted");

    // ── Deny path: the typed command sits un-submitted; the denied Enter is
    // never sent, so /tmp/cd1-deny.txt must never exist in the container. ──
    let home = live_home("live-dn", &sock, &token);
    let decider = spawn_decider(home.clone(), false);
    let script = CodriveScript {
        target_app: "foot".to_string(),
        task_summary: "在終端機建立標記檔（live 活測·拒絕路徑）".to_string(),
        steps: vec![
            step("輸入建檔指令", CodriveAction::Text { s: "echo cd1deny > /tmp/cd1-deny.txt".to_string() }),
            CodriveStep {
                narration: "送出指令".to_string(),
                highlight: None,
                action: CodriveAction::KeyName { name: "enter".to_string() },
                consequential: Some(CodriveConsequential {
                    class: ConsequentialClass::Submit,
                    description: "在共享桌面的終端機執行已輸入的指令".to_string(),
                }),
                api_action: None,
            },
        ],
        watch_mode: false,
    };
    let report = run_script(&home, "live-test-agent", script).await;
    let decided_id = decider.await.expect("decider task panicked");
    println!("deny-path report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "aborted_approval_denied", "deny path must abort: {:?}", report.detail);
    assert_eq!(report.steps.len(), 2, "the script must stop at the denied step");
    let enter_step = &report.steps[1];
    assert_eq!(enter_step.outcome, "denied");
    assert_eq!(enter_step.approval_id.as_deref(), Some(decided_id.as_str()));
}

/// CD-2 VM/QMP acceptance round: the real driver against the real comp,
/// bridged the same way `live_bridge_approve_then_deny_round` is (see this
/// module's playbook doc — only the far end differs: a QEMU VM's real
/// `cage`+comp seat instead of a Docker container's headless weston, so
/// the freeze this test relies on is fired by an ACTUAL QMP-injected
/// keyboard event landing on real hardware (`input.rs::process_input_
/// event`), never the container round's `DUDUCLAW_CODRIVE_DEBUG_STDIN`
/// shortcut, which has no real input path to freeze from at all.
///
/// No `consequential` step here (freeze/resume needs no `ApprovalBroker`),
/// so unlike the approve/deny test this one drives no decider — the ONLY
/// external actor is whoever fires the QMP events during the script's
/// `Wait` step (this test's own module-doc playbook, or an operator's
/// shell, timed against the `Wait` step's window).
///
/// Expects a `Click` step to focus the target app's shell, a `Wait` long
/// enough to fire real QMP human input during it (freezing the seat before
/// the next step's own actions are attempted), then a `Text` step whose
/// first attempt gets dropped-frozen and is only actually applied after a
/// real QMP Super+Enter clears `frozen` — `driver::run_script`'s
/// `wait_for_resume` (`step.rs`) polls `status` once a second for exactly
/// this.
#[tokio::test]
#[ignore = "live bridge harness — needs the comp VM stack + relay, and a real QMP human-input event fired during the script's Wait step (see this module's doc + duduclaw-comp/BUILD.md's CD-2 VM round)"]
async fn live_bridge_real_human_freeze_and_resume() {
    let (sock, token) = live_env();
    let home = live_home("live-freeze", &sock, &token);

    let script = CodriveScript {
        target_app: "foot".to_string(),
        task_summary: "VM 真人凍結/交還活測（CD-2 收官）".to_string(),
        steps: vec![
            CodriveStep {
                narration: "點擊終端機輸入區".to_string(),
                highlight: None,
                action: CodriveAction::Click { x: 200.0, y: 150.0, btn: super::client::CodriveButton::Left },
                consequential: None,
                api_action: None,
            },
            step(
                "等待外部注入真人輸入（QMP 真鍵盤事件，非 debug stdin）",
                CodriveAction::Wait { ms: 20000 },
            ),
            step(
                "輸入驗證指令（預期先被凍結，真人 Super+Enter 交還後重發）",
                CodriveAction::Text { s: "echo cd2vmfreeze123 > /tmp/cd2-freeze-proof.txt\n".to_string() },
            ),
        ],
        watch_mode: false,
    };

    let report = run_script(&home, "live-test-agent", script).await;
    println!("freeze/resume report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "completed", "freeze/resume round must complete: {:?}", report.detail);
    assert_eq!(report.steps.len(), 3);
    assert_eq!(
        report.steps[2].outcome, "dropped_frozen_reapplied",
        "the Text step must have been dropped once (frozen) then reapplied after a real Super+Enter"
    );
}
