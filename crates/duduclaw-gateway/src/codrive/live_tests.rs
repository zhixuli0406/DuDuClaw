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
    ApiActionRequest, CodriveAction, CodriveConsequential, CodriveHighlight, CodriveScript,
    CodriveStep, ConsequentialClass, LocateRequest,
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
    CodriveStep { narration: narration.to_string(), highlight: None, action, consequential: None, api_action: None, locate: None }
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
                locate: None,
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
                locate: None,
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
                locate: None,
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
                locate: None,
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

// ── WP-CD4c (2026-08-22): VM/QMP live acceptance for CD-4a (C-L2 registry)
// + CD-4b (C-L3 AT-SPI2) — see commercial/docs/DESIGN-codrive-desktop-2026-08.md
// §5 CD-4 row. Unlike the CD-1/CD-2 tests above, this round runs the whole
// stack (this test binary + comp + the AT-SPI probe app) natively inside
// the appliance VM's real Debian trixie guest, not bridged from the Mac
// host across a Docker boundary — `DUDUCLAW_CODRIVE_LIVE_SOCK`/`_TOKEN`
// point straight at comp's real socket/token files, no socat/python relay
// needed. This is intentionally NOT committed this round (verification-only
// task); left here uncommitted for the operator to keep or discard.
//
// Playbook (see the task's own evidence trail for the exact commands used):
// 1. Vendor libgtk-4-1 + at-spi2-core + a tiny custom GTK4 probe app
//    (two buttons, "Save"/"Close"; Save writes a proof file — avoids the
//    gnome-text-editor 43.2 packaging bug that blocked CD-4b's own Save-
//    button locate) into the guest (apt is not present on the appliance
//    image; vendored via a matching debian:trixie container + tar).
// 2. Start a bare `dbus-daemon --session` as the test harness's session bus
//    (mirrors CD-4b's own "container 裸 dbus-daemon" evidentiary bar, now
//    on a real booted VM instead of a bare container namespace — this is
//    the R-4-2 question: does `org.a11y.Bus` still D-Bus-service-activate
//    `at-spi-bus-launcher` here).
// 3. Start `duduclaw-comp` nested (winit backend) inside the VM's real
//    `cage` kiosk Wayland session, then the probe app as a Wayland client
//    connecting to comp's socket.
// 4. Cross-compile this test binary for aarch64-unknown-linux and run it
//    natively on the guest with `DBUS_SESSION_BUS_ADDRESS`,
//    `DUDUCLAW_CODRIVE_LIVE_SOCK`, `DUDUCLAW_CODRIVE_LIVE_TOKEN` set.

/// R-4-2 + C-L3 end-to-end: locate the probe app's "Save" button by
/// (role=button, name=Save) over a REAL a11y bus reached via a REAL VM boot
/// sequence, inject the resolved click through the REAL comp socket, and
/// verify the REAL file side effect the click's `clicked` handler writes —
/// closing CD-4b's own documented debt ("Save 鈕因 gnome-text-editor 43.2
/// 套件 bug 未定位...comp 注入觸發存檔（bonus）未做").
///
/// The literal `action` coordinates are deliberately (1.0, 1.0) — nowhere
/// near either button in the probe app's 420x200 layout — so a locate MISS
/// silently falling back to C-L1 would hit empty window chrome, not Save,
/// and the proof file would never appear. The audit log's `locate_outcome`
/// field is checked too, so a false pass (some unrelated reason the file
/// exists) can't hide behind a coordinate that happened to land right.
///
/// ## WP-CD4b-fix (B3) — what changed under this test
///
/// The first VM run of this test produced `locate_outcome":"located"` AND
/// no proof file: the locate hit the right accessible and then clicked
/// (33.5, 17) instead of ≈(58, 107), because GTK4 hard-codes
/// `CoordType::Screen` to `x=0,y=0` and the click point was therefore just
/// `(button_width/2, button_height/2)`. `atspi_locate` now reads
/// `CoordType::Window` and adds the window origin comp reports over the new
/// read-only `{"op":"window_geometry"}` query, so THIS test is the
/// end-to-end proof of the fix — a pass now requires the two halves to
/// agree, not merely for a locate to report success.
///
/// When it fails, read in this order:
/// 1. `tool_calls.jsonl`'s `params_summary` for this step — a `located`
///    row now carries `window_extents=(x,y,w,h) window_origin=(x,y)
///    window_size=(WxH) pid=N -> global=(X,Y)`, which says exactly which
///    half was wrong.
/// 2. A `locate_failed_fallback` row naming `ambiguous AT-SPI match` means
///    `(role,name)` was not unique in the probe's tree (the CSD "✕" and a
///    content "Close" are both `(button, "Close")`, for instance) — that is
///    a deliberate refusal, not a regression.
/// 3. A `locate_failed_fallback` row naming `window_not_found` /
///    `ambiguous_window` comes from comp: the a11y-reported pid did not
///    match exactly one mapped toplevel. `duduclaw-comp`'s log has the
///    `codrive: window_geometry — resolving against currently mapped
///    windows` debug line listing every candidate's pid/app_id/title.
#[tokio::test]
#[ignore = "live bridge harness — needs the CD-4c VM rig: comp + a11y bus + the cd4c-gtk-probe app, see this module's CD-4c playbook doc"]
async fn live_bridge_cd4c_atspi_locate_click_save_writes_file() {
    let (sock, token) = live_env();
    let home = live_home("cd4c-atspi", &sock, &token);

    let proof_path = std::env::var("CD4C_PROOF_PATH").unwrap_or_else(|_| "/tmp/cd4c-atspi-proof.txt".to_string());
    let _ = std::fs::remove_file(&proof_path); // start from a clean slate — a stale proof file must not fake a PASS

    let target_app = std::env::var("CD4C_TARGET_APP").unwrap_or_else(|_| "cd4c-gtk-probe".to_string());

    let script = CodriveScript {
        target_app: target_app.clone(),
        task_summary: "CD-4c 活測：AT-SPI 定位 Save 鈕並注入點擊，驗證真檔案副作用".to_string(),
        steps: vec![CodriveStep {
            narration: "AT-SPI 定位並點擊 Save 鈕".to_string(),
            highlight: None,
            action: CodriveAction::Click { x: 1.0, y: 1.0, btn: super::client::CodriveButton::Left },
            consequential: None,
            api_action: None,
            locate: Some(LocateRequest { role: "button".to_string(), name: "Save".to_string() }),
        }],
        watch_mode: false,
    };

    let report = run_script(&home, "live-test-agent", script).await;
    println!("cd4c atspi-locate report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "completed", "locate+click round must complete: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "applied");

    // Audit-log ground truth: the step must have actually gone through
    // AT-SPI (`locate_outcome":"located"`), not silently fallen back.
    let audit = std::fs::read_to_string(home.join("tool_calls.jsonl")).expect("tool_calls.jsonl must exist after a run");
    assert!(
        audit.contains("\"locate_outcome\":\"located\"") || audit.contains("\"locate_outcome\": \"located\""),
        "audit log must show a real AT-SPI locate hit, not a fallback — audit:\n{audit}"
    );

    // Real file side effect from the injected click, read directly off the
    // guest filesystem (this test binary runs natively on the VM).
    for _ in 0..20 {
        if std::path::Path::new(&proof_path).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let proof = std::fs::read_to_string(&proof_path)
        .unwrap_or_else(|e| panic!("proof file {proof_path} was not written by the injected click: {e}"));
    assert!(proof.starts_with("cd4c-saved-by-atspi-click"), "unexpected proof file content: {proof}");
}

/// CD-4a C-L2 real machine round, NetworkManager half: system-bus DBus call
/// — closing CD-4a's own documented debt ("真 NM 服務留特權容器輪": this VM
/// has a real, non-privileged-container `NetworkManager` already managing
/// the guest's real NIC, so this is that deferred round). The step's
/// literal `action` coordinates are an inert placeholder (a registry hit
/// never reaches comp) — a REGISTRY MISS would fall back to it and fail the
/// assertion below instead of silently passing.
///
/// Split from the Chromium half (see `live_bridge_cd4c_c_l2_chromium_real_
/// dispatch`'s doc comment for why) so this one's pass/fail is never
/// entangled with that unrelated, already-understood limitation.
#[tokio::test]
#[ignore = "live bridge harness — needs the CD-4c VM rig (comp reachable + a real NetworkManager on the system bus), see this module's CD-4c playbook doc"]
async fn live_bridge_cd4c_c_l2_networkmanager_real_dispatch() {
    let (sock, token) = live_env();
    let home = live_home("cd4c-l2-nm", &sock, &token);
    let script = CodriveScript {
        target_app: "networkmanager".to_string(),
        task_summary: "CD-4c 活測：C-L2 註冊表真派工 NetworkManager state".to_string(),
        steps: vec![CodriveStep {
            narration: "透過註冊表讀取 NetworkManager 狀態（system D-Bus）".to_string(),
            highlight: None,
            action: CodriveAction::Wait { ms: 1 },
            consequential: None,
            api_action: Some(ApiActionRequest { action: "state".to_string(), params: serde_json::Value::Null }),
            locate: None,
        }],
        watch_mode: false,
    };
    let report = run_script(&home, "live-test-agent", script).await;
    println!("cd4c C-L2 networkmanager report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "completed", "networkmanager api_action round must complete: {:?}", report.detail);
    assert_eq!(report.steps[0].outcome, "api_action", "must be served by the C-L2 registry, not fall back to C-L1");
    let audit = std::fs::read_to_string(home.join("tool_calls.jsonl")).expect("tool_calls.jsonl must exist after a run");
    assert!(
        audit.contains("\"registry_outcome\":\"executed\"") || audit.contains("\"registry_outcome\": \"executed\""),
        "audit log must show a real DBus exec against the real system bus — audit:\n{audit}"
    );
}

/// CD-4a C-L2 real machine round, Chromium half: Cli subprocess exec.
///
/// ## What this test asserted before, and why it no longer does
///
/// The CD-4c live round found — and this test used to PIN — a design gap
/// in `registry::execute_cli`: it waited for the child's full *exit*
/// before reporting success, but a genuinely successful `chromium
/// --new-window` launch is a long-lived GUI process that never exits on
/// its own. A real, working launch was therefore structurally
/// indistinguishable from a wedged child: both ran out `EXEC_TIMEOUT`
/// (10s), both audited `registry_outcome: "exec_failed_fallback"`, and
/// `kill_on_drop` then killed the window the successful one had just
/// opened. The C-L2 Chromium half could not report success at all, so this
/// test asserted the timeout as the known-good fingerprint.
///
/// **That gap is now closed (WP-B1).** `Exec::Cli` carries an explicit
/// `registry::WaitMode`; both Chromium actions are `Detach { liveness_ms }`
/// — spawn, no `kill_on_drop`, null stdio, then one non-blocking poll after
/// the grace period. A launch that is still running is now a real
/// `registry_outcome: "executed"`, and only a child that actually died
/// inside the grace window degrades to C-L1. The assertions below are
/// updated to the new contract: a successful launch, no timeout, and no
/// fallback.
///
/// The two REAL environment bugs found live in the earlier round are
/// unchanged and still pinned here: (1) Chromium's default Ozone/X11
/// backend has no XWayland session to attach to on this Wayland-only
/// target, fixed with `--ozone-platform=wayland`; (2) `execute_cli` reused
/// the AI-CLI `AGENT_CLI_ENV_ALLOWLIST`, which strips `XDG_RUNTIME_DIR`/
/// `WAYLAND_DISPLAY` — vars no AI-CLI subprocess needs but a GUI client
/// cannot find a display without — fixed by layering those three vars back
/// on top for this call site only.
///
/// NOTE for whoever runs the rig next: this test now leaves a real
/// Chromium window open on the VM (that is the success condition, not a
/// leak) — close it between runs so the next launch is a fresh instance
/// rather than a hand-off to the live one.
#[tokio::test]
#[ignore = "live bridge harness — needs the CD-4c VM rig (comp reachable + a real, non-root Wayland session for chromium), see this module's CD-4c playbook doc"]
async fn live_bridge_cd4c_c_l2_chromium_real_dispatch() {
    let (sock, token) = live_env();
    let home = live_home("cd4c-l2-chromium", &sock, &token);
    let script = CodriveScript {
        target_app: "chromium".to_string(),
        task_summary: "CD-4c 活測：C-L2 註冊表真派工 Chromium open_window".to_string(),
        steps: vec![CodriveStep {
            narration: "透過註冊表開新 Chromium 視窗（CLI）".to_string(),
            highlight: None,
            action: CodriveAction::Wait { ms: 1 },
            consequential: None,
            api_action: Some(ApiActionRequest { action: "open_window".to_string(), params: serde_json::Value::Null }),
            locate: None,
        }],
        watch_mode: false,
    };
    let report = run_script(&home, "live-test-agent", script).await;
    println!("cd4c C-L2 chromium report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "completed", "detail: {:?}", report.detail);
    assert_eq!(
        report.steps[0].outcome, "api_action",
        "post-WP-B1 a real Chromium launch is a genuine C-L2 hit — an \"applied\" outcome here means it degraded to the C-L1 coordinate path again: {:?}",
        report.detail
    );
    let audit = std::fs::read_to_string(home.join("tool_calls.jsonl")).expect("tool_calls.jsonl must exist after a run");
    assert!(!audit.contains("Missing X server"), "the --ozone-platform=wayland fix must prevent the X11/XWayland error — audit:\n{audit}");
    assert!(!audit.contains("XDG_RUNTIME_DIR is invalid"), "the env-passthrough fix must prevent this error — audit:\n{audit}");
    assert!(
        !audit.contains("timed out after 10s"),
        "EXEC_TIMEOUT must never be reached by a detached launcher action — seeing it means the Chromium entries lost their WaitMode::Detach — audit:\n{audit}"
    );
    assert!(
        !audit.contains("exec_failed_fallback"),
        "a working launch must no longer degrade to C-L1 — audit:\n{audit}"
    );
    assert!(
        audit.contains("\"registry_outcome\":\"executed\"") || audit.contains("\"registry_outcome\": \"executed\""),
        "the audit trail must record a real C-L2 execution — audit:\n{audit}"
    );
    assert!(
        audit.contains("still running after") || audit.contains("launch grace"),
        "the detach path's own honest wording must reach the audit detail (never \"exited\" for a live window) — audit:\n{audit}"
    );
}

/// One script mixing all three rungs of the execution ladder in a single
/// run — the degradation-chain end-to-end row from the task brief
/// ("C-L1→C-L2→C-L3 一條 script 三種步驟混合跑一次，稽核逐筆核對"):
/// step 0 is a pure C-L1 coordinate move (comp only, no api_action/locate
/// at all — not even an attempt), step 1 DELIBERATELY ATTEMPTS a C-L2
/// registry lookup that MISSES, step 2 is a real C-L3 AT-SPI locate that
/// resolves onto the probe app's "Close" button and clicks it through comp.
///
/// Step 1 is a genuine MISS, not a shortcut: `CodriveScript::target_app` is
/// ONE field shared by every step in the script (`step::try_registry_action`
/// takes it as a plain `&str`, not a per-step override), so a script can
/// only ever have ONE real chance to hit the C-L2 registry — either the
/// registered app (`chromium`/`networkmanager`) for a real registry hit, or
/// the AT-SPI-visible app (`cd4c-gtk-probe`) for a real C-L3 locate, never
/// both in the same run (confirmed live: an earlier draft of this test
/// tried `target_app="cd4c-gtk-probe"` with a `networkmanager`-shaped C-L2
/// step and got `registry_miss_fallback`, exactly as designed). Given that
/// constraint, this test picks the more informative half to prove for the
/// C-L2 rung here — the registry's own safety net catching an app it has
/// never heard of and falling cleanly through to C-L1, which
/// `live_bridge_cd4c_c_l2_chromium_real_dispatch` and `..._networkmanager_
/// real_dispatch` above already cover the real-registry-hit half of.
#[tokio::test]
#[ignore = "live bridge harness — needs the full CD-4c VM rig (comp + a11y bus + cd4c-gtk-probe + NetworkManager), see this module's CD-4c playbook doc"]
async fn live_bridge_cd4c_mixed_ladder_one_script() {
    let (sock, token) = live_env();
    let home = live_home("cd4c-ladder", &sock, &token);
    let target_app = std::env::var("CD4C_TARGET_APP").unwrap_or_else(|_| "cd4c-gtk-probe".to_string());

    let script = CodriveScript {
        target_app: target_app.clone(),
        task_summary: "CD-4c 活測：C-L1→C-L2→C-L3 混合階梯一次跑完".to_string(),
        steps: vec![
            // C-L1: plain coordinate move, comp only, no api_action/locate.
            CodriveStep {
                narration: "C-L1：純座標移動".to_string(),
                highlight: None,
                action: CodriveAction::Move { x: 10.0, y: 10.0 },
                consequential: None,
                api_action: None,
                locate: None,
            },
            // C-L2: deliberate MISS (see module doc above) — target_app is
            // "cd4c-gtk-probe" for this whole script (needed by step 2's
            // locate), which has no registry entry, so this honestly falls
            // back to C-L1's literal Wait{ms:1} action instead of a fake
            // "executed" outcome.
            CodriveStep {
                narration: "C-L2：註冊表查無此 app（示範降級）".to_string(),
                highlight: None,
                action: CodriveAction::Wait { ms: 1 },
                consequential: None,
                api_action: Some(ApiActionRequest {
                    action: "connectivity".to_string(),
                    params: serde_json::Value::Null,
                }),
                locate: None,
            },
            // C-L3: AT-SPI locate resolves onto "Close" and clicks via comp.
            CodriveStep {
                narration: "C-L3：AT-SPI 定位 Close 鈕並點擊".to_string(),
                highlight: None,
                action: CodriveAction::Click { x: 1.0, y: 1.0, btn: super::client::CodriveButton::Left },
                consequential: None,
                api_action: None,
                locate: Some(LocateRequest { role: "button".to_string(), name: "Close".to_string() }),
            },
        ],
        watch_mode: false,
    };

    let report = run_script(&home, "live-test-agent", script).await;
    println!("cd4c mixed-ladder report: {}", serde_json::to_string_pretty(&serde_json::to_value(&report).unwrap()).unwrap());
    assert_eq!(report.final_state, "completed", "mixed-ladder round must complete: {:?}", report.detail);
    assert_eq!(report.steps.len(), 3);
    assert_eq!(report.steps[0].outcome, "applied", "step 0 (C-L1) must be a plain comp-applied move");
    assert_eq!(report.steps[1].outcome, "applied", "step 1 (C-L2 miss) must fall through to a plain comp-applied Wait, not error out");
    assert_eq!(report.steps[2].outcome, "applied", "step 2 (C-L3) still reports applied — locate resolves coords, comp still does the click");

    let audit = std::fs::read_to_string(home.join("tool_calls.jsonl")).expect("tool_calls.jsonl must exist after a run");
    assert!(
        audit.contains("\"registry_outcome\":\"registry_miss_fallback\"") || audit.contains("\"registry_outcome\": \"registry_miss_fallback\""),
        "step 1 must show an honest C-L2 registry MISS in the audit trail (see module doc — cd4c-gtk-probe was never meant to be registered) — audit:\n{audit}"
    );
    assert!(
        audit.contains("\"locate_outcome\":\"located\"") || audit.contains("\"locate_outcome\": \"located\""),
        "step 2 must show a real C-L3 AT-SPI locate hit in the audit trail — audit:\n{audit}"
    );
}
