//! Lifecycle tests against a mocked sysd (design §10.1 gateway items, §10.2
//! "gateway ↔ fake sysd"). No real sysd, systemd or network is touched; each
//! test has its own home directory.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use duduclaw_sysd::SysdOpOutput;
use serde_json::{Value, json};

use super::lifecycle::{boot_reconcile, poll_sysd};
use super::notify::AdminNotifier;
use super::sysd_port::{MaintSysd, SysdAccess, SysdCallError, SysdCallResult, SysdProvider};
use super::*;

const KEY_LINE: &str = include_str!("../../../duduclaw-sysd/tests/fixtures/ssh_keys/ed25519.pub");
const KEY_FP: &str = "SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE";
const HOST_FP: &str = "SHA256:hostkeyfingerprintAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PASSWORD: &str = "correct horse";

// ── Mocks ───────────────────────────────────────────────────

fn ok(stdout: impl Into<String>) -> SysdCallResult {
    Ok(SysdOpOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
    })
}

fn rejected(kind: &str, message: &str) -> SysdCallResult {
    Err(SysdCallError::Rejected {
        kind: kind.into(),
        message: message.into(),
    })
}

fn closed_status() -> String {
    json!({"ready": true, "key_installed": false, "window_id": null, "ssh_active": false,
           "sessions": [], "events": [], "pending_cleanup": false, "problems": []})
    .to_string()
}

fn open_status(window_id: &str, events: Value) -> String {
    json!({"ready": true, "key_installed": true, "key_fingerprint": KEY_FP, "window_id": window_id,
           "expires_in_s": 3000, "fallback_timer_active": true, "ssh_active": true,
           "sessions": [], "events": events, "event_overflow": 0, "auth_failures": 0,
           "host_key_fingerprints": [HOST_FP], "pending_cleanup": false, "problems": []})
    .to_string()
}

#[derive(Default)]
struct MockSysd {
    /// Replies to `ssh_maint_open`, in order; when empty, a success echoing the request.
    open_replies: StdMutex<VecDeque<SysdCallResult>>,
    /// Replies to `ssh_maint_status`, in order; when empty, `default_status`.
    status_replies: StdMutex<VecDeque<SysdCallResult>>,
    default_status: StdMutex<Option<SysdCallResult>>,
    stop_reply: StdMutex<Option<SysdCallResult>>,
    open_delay: Duration,
    status_delay: Duration,
    calls: StdMutex<Vec<String>>,
    opened_with: StdMutex<Vec<(String, u64, String)>>,
}

impl MockSysd {
    fn calls(&self, verb: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == verb)
            .count()
    }
    fn set_status(&self, reply: SysdCallResult) {
        *self.default_status.lock().unwrap() = Some(reply);
    }
    fn push_open(&self, reply: SysdCallResult) {
        self.open_replies.lock().unwrap().push_back(reply);
    }
    fn set_stop(&self, reply: SysdCallResult) {
        *self.stop_reply.lock().unwrap() = Some(reply);
    }
}

#[async_trait]
impl MaintSysd for MockSysd {
    async fn ssh_maint_open(
        &self,
        public_key: &str,
        ttl_seconds: u64,
        window_id: &str,
    ) -> SysdCallResult {
        self.calls.lock().unwrap().push("open".into());
        self.opened_with
            .lock()
            .unwrap()
            .push((public_key.into(), ttl_seconds, window_id.into()));
        if !self.open_delay.is_zero() {
            tokio::time::sleep(self.open_delay).await;
        }
        if let Some(r) = self.open_replies.lock().unwrap().pop_front() {
            return r;
        }
        ok(json!({"login_user": "duduclaw-maint", "window_id": window_id, "key_type": "ssh-ed25519",
                  "key_fingerprint": KEY_FP, "expires_in_s": ttl_seconds, "host_key_fingerprints": [HOST_FP]})
        .to_string())
    }

    async fn ssh_maint_status(&self) -> SysdCallResult {
        self.calls.lock().unwrap().push("status".into());
        if !self.status_delay.is_zero() {
            tokio::time::sleep(self.status_delay).await;
        }
        if let Some(r) = self.status_replies.lock().unwrap().pop_front() {
            return r;
        }
        self.default_status
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| ok(closed_status()))
    }

    async fn ssh_service_stop(&self) -> SysdCallResult {
        self.calls.lock().unwrap().push("stop".into());
        self.stop_reply
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| ok("removed the maintenance key"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Unreachable,
    Available,
}

struct MockProvider {
    mode: StdMutex<Mode>,
    access_calls: AtomicUsize,
    /// `access()` answers Unreachable this many more times before `mode`.
    unreachable_for: AtomicUsize,
    sysd: Arc<MockSysd>,
}

impl MockProvider {
    fn new(mode: Mode, sysd: Arc<MockSysd>) -> Arc<Self> {
        Arc::new(Self {
            mode: StdMutex::new(mode),
            access_calls: AtomicUsize::new(0),
            unreachable_for: AtomicUsize::new(0),
            sysd,
        })
    }
    fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }
}

impl SysdProvider for MockProvider {
    fn access(&self) -> SysdAccess {
        self.access_calls.fetch_add(1, Ordering::SeqCst);
        if self.unreachable_for.load(Ordering::SeqCst) > 0 {
            self.unreachable_for.fetch_sub(1, Ordering::SeqCst);
            return SysdAccess::Unreachable("socket missing (test)".into());
        }
        match *self.mode.lock().unwrap() {
            Mode::Off => SysdAccess::OffAppliance,
            Mode::Unreachable => SysdAccess::Unreachable("socket missing (test)".into()),
            Mode::Available => SysdAccess::Available(self.sysd.clone()),
        }
    }
}

#[derive(Default)]
struct RecordingNotifier {
    sent: StdMutex<Vec<String>>,
    available: bool,
}

impl RecordingNotifier {
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl AdminNotifier for RecordingNotifier {
    async fn notify_admins(&self, text: &str) -> usize {
        self.sent.lock().unwrap().push(text.to_string());
        1
    }
    async fn channel_available(&self) -> bool {
        self.available
    }
}

struct CountingVerifier {
    calls: AtomicUsize,
}

impl CountingVerifier {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ReauthVerifier for CountingVerifier {
    fn verify(&self, _email: &str, password: &str) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        password == PASSWORD
    }
}

struct Harness {
    home: tempfile::TempDir,
    sysd: Arc<MockSysd>,
    provider: Arc<MockProvider>,
    notifier: Arc<RecordingNotifier>,
    env: MaintEnv,
}

fn harness_with(mode: Mode, sysd: MockSysd, push_available: bool) -> Harness {
    let home = tempfile::tempdir().unwrap();
    let sysd = Arc::new(sysd);
    let provider = MockProvider::new(mode, sysd.clone());
    let notifier = Arc::new(RecordingNotifier {
        sent: StdMutex::new(Vec::new()),
        available: push_available,
    });
    let env = MaintEnv {
        home_dir: home.path().to_path_buf(),
        sysd: provider.clone(),
        notifier: notifier.clone(),
        reconcile_retry: Duration::from_millis(5),
        reconcile_deadline: Duration::from_millis(150),
        tick_interval: Duration::from_millis(20),
    };
    Harness {
        home,
        sysd,
        provider,
        notifier,
        env,
    }
}

fn harness(mode: Mode) -> Harness {
    harness_with(mode, MockSysd::default(), false)
}

fn caps(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn request<'a>(
    sub_caps: &'a [String],
    key: Option<&'a str>,
    password: &'a str,
) -> EnableRequest<'a> {
    EnableRequest {
        user_id: "u1",
        email: "admin@local",
        ttl_hours: 4,
        sub_capabilities: sub_caps,
        ssh_public_key: key,
        reauth_password: password,
    }
}

fn events(home: &Path) -> Vec<Value> {
    history_json(home, 1000, 0).unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}

fn kinds(home: &Path) -> Vec<String> {
    events(home)
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

async fn open_ssh_window(h: &Harness) -> MaintenanceWindow {
    let ssh = caps(&["ssh"]);
    enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap()
}

async fn expire_now(home: &Path, id: &str) {
    let store = MaintenanceStore::open(home).unwrap();
    let conn = store.conn.lock().await;
    conn.execute(
        "UPDATE maintenance_windows SET expires_at = ?1 WHERE id = ?2",
        rusqlite::params![
            (Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(),
            id
        ],
    )
    .unwrap();
}

// ── Enable ──────────────────────────────────────────────────

#[tokio::test]
async fn ssh_needs_a_key_and_a_key_needs_ssh() {
    let h = harness(Mode::Available);
    let verifier = CountingVerifier::new();
    let ssh = caps(&["ssh"]);
    let err = enable(&h.env, &request(&ssh, None, PASSWORD), &verifier)
        .await
        .unwrap_err();
    assert_eq!(err.code, error_code::SSH_KEY_REQUIRED);
    let err = enable(&h.env, &request(&ssh, Some("   "), PASSWORD), &verifier)
        .await
        .unwrap_err();
    assert_eq!(err.code, error_code::SSH_KEY_REQUIRED);
    let details = caps(&["show_details"]);
    let err = enable(
        &h.env,
        &request(&details, Some(KEY_LINE), PASSWORD),
        &verifier,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_KEY_UNEXPECTED);
    assert_eq!(h.sysd.calls("open"), 0);
    assert_eq!(
        verifier.calls(),
        0,
        "parameter errors come before the password check"
    );
}

#[tokio::test]
async fn ssh_open_writes_opening_then_open_and_tells_every_admin_both_fingerprints() {
    let h = harness(Mode::Available);
    let window = open_ssh_window(&h).await;
    assert_eq!(window.state, WindowState::Open);
    assert_eq!(window.ssh_key_fingerprint.as_deref(), Some(KEY_FP));
    assert_eq!(window.ssh_key_label.as_deref(), Some("fix-ed25519"));
    assert_eq!(window.host_key_fingerprints, vec![HOST_FP.to_string()]);

    // Only "<type> <base64>" reaches sysd (no comment), with the window id and TTL.
    let (wire, ttl, id) = h.sysd.opened_with.lock().unwrap()[0].clone();
    let mut fields = KEY_LINE.split_ascii_whitespace();
    assert_eq!(
        wire,
        format!("{} {}", fields.next().unwrap(), fields.next().unwrap())
    );
    assert_eq!(ttl, 4 * 3600);
    assert_eq!(id, window.id);

    let store = MaintenanceStore::open(h.home.path()).unwrap();
    assert!(store.active_window().await.unwrap().is_some());
    let k = kinds(h.home.path());
    assert!(
        k.contains(&"enabled".to_string()) && k.contains(&"ssh_key_installed".to_string()),
        "{k:?}"
    );

    let sent = h.notifier.sent();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains(KEY_FP) && sent[0].contains(HOST_FP) && sent[0].contains("admin@local")
    );
}

#[tokio::test]
async fn a_bad_key_is_refused_with_its_code_and_never_recorded() {
    let h = harness(Mode::Available);
    let ssh = caps(&["ssh"]);
    let private = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";
    let err = enable(
        &h.env,
        &request(&ssh, Some(private), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_KEY_INVALID);
    assert_eq!(err.extra["reason"], "private_key");
    let err = enable(
        &h.env,
        &request(&ssh, Some("ssh-dss AAAAB3NzaC1kc3M="), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.extra["reason"], "unsupported_type");
    assert_eq!(h.sysd.calls("open"), 0);
    let audit = std::fs::read_to_string(h.home.path().join("maintenance_audit.jsonl")).unwrap();
    assert!(
        !audit.contains("b3BlbnNzaC1rZXktdjEAAAAA"),
        "the pasted key must never reach the audit file"
    );
    assert!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .current_row()
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn busy_records_open_failed_without_a_rollback_stop() {
    let h = harness(Mode::Available);
    h.sysd.push_open(rejected(
        "busy",
        "a maintenance window is already open or still being cleaned up",
    ));
    // sysd is still cleaning up: no window, nothing for the reconciliation to close.
    h.sysd.set_status(ok(
        json!({"ready": true, "key_installed": false, "window_id": null,
                                 "pending_cleanup": true})
        .to_string(),
    ));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_BUSY);
    assert_eq!(err.extra["reason"], "cleanup_pending");
    assert_eq!(
        h.sysd.calls("stop"),
        0,
        "busy must never be rolled back with a stop"
    );
    assert_eq!(
        h.sysd.calls("status"),
        1,
        "busy triggers one reconciliation"
    );
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, WindowState::OpenFailed);
    assert_eq!(
        latest.revoke_reason.as_deref(),
        Some(REVOKE_REASON_OPEN_FAILED)
    );
    assert!(latest.close_error.is_none());
}

#[tokio::test]
async fn busy_reconciliation_closes_a_key_no_window_explains() {
    let h = harness(Mode::Available);
    h.sysd
        .push_open(rejected("busy", "a maintenance key is already installed"));
    h.sysd
        .set_status(ok(open_status("left-over-window", json!([]))));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_BUSY);
    assert_eq!(err.extra["reason"], "key_installed");
    assert_eq!(h.sysd.calls("stop"), 1);
    let orphan = events(h.home.path())
        .into_iter()
        .find(|e| e["kind"] == "ssh_orphan_closed")
        .unwrap();
    assert_eq!(orphan["sysd_window_id"], "left-over-window");
    assert_eq!(orphan["context"], "busy");
}

#[tokio::test]
async fn busy_while_sysd_owes_its_audit_file_is_reported_as_such() {
    let h = harness(Mode::Available);
    h.sysd.push_open(rejected(
        "busy",
        "a maintenance window is already open or still being cleaned up",
    ));
    h.sysd.set_status(ok(
        json!({"ready": true, "key_installed": false, "window_id": null,
                                 "pending_cleanup": false, "pending_audit": true})
        .to_string(),
    ));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_BUSY);
    assert_eq!(err.extra["reason"], "audit_pending");
    assert_eq!(h.sysd.calls("stop"), 0);
    // An audit backlog is not an open SSH path.
    let s = status_json(&h.env, None).await.unwrap();
    assert_eq!(s["ssh"]["pending_audit"], true);
    assert_eq!(s["close_unconfirmed"], false);
}

#[tokio::test]
async fn sysd_rate_limit_is_a_clear_refusal_without_a_rollback() {
    let h = harness(Mode::Available);
    h.sysd.push_open(rejected(
        "rate_limited",
        "too many maintenance SSH opens: at most 12 per hour, 5 s apart",
    ));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_RATE_LIMITED);
    assert_eq!(h.sysd.calls("stop"), 0);
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, WindowState::OpenFailed);
    assert!(latest.close_error.is_none());
}

#[tokio::test]
async fn an_open_window_is_never_touched_by_a_second_enable() {
    let h = harness(Mode::Available);
    let first = open_ssh_window(&h).await;
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::ALREADY_ACTIVE);
    assert_eq!(h.sysd.calls("open"), 1);
    assert_eq!(h.sysd.calls("stop"), 0);
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    assert_eq!(store.active_window().await.unwrap().unwrap().id, first.id);
}

#[tokio::test]
async fn concurrent_enables_reach_sysd_once() {
    let h = harness_with(
        Mode::Available,
        MockSysd {
            open_delay: Duration::from_millis(60),
            ..Default::default()
        },
        false,
    );
    let ssh = caps(&["ssh"]);
    let (v1, v2) = (CountingVerifier::new(), CountingVerifier::new());
    let r1 = request(&ssh, Some(KEY_LINE), PASSWORD);
    let r2 = request(&ssh, Some(KEY_LINE), PASSWORD);
    let (a, b) = tokio::join!(enable(&h.env, &r1, &v1), enable(&h.env, &r2, &v2));
    assert_eq!(h.sysd.calls("open"), 1);
    assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|x| **x).count(), 1);
    let err = a.err().or(b.err()).unwrap();
    assert_eq!(err.code, error_code::ALREADY_ACTIVE);
    assert_eq!(h.sysd.calls("stop"), 0);
}

#[tokio::test]
async fn old_sysd_is_unsupported_and_never_falls_back_to_a_port_only_open() {
    let h = harness(Mode::Available);
    h.sysd.push_open(Err(SysdCallError::UnknownVerb));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_UNSUPPORTED);
    assert_eq!(err.extra["reason"], "sysd_too_old");
    assert_eq!(h.sysd.calls("open"), 1);
    assert_eq!(h.sysd.calls("stop"), 0);

    h.sysd.push_open(rejected(
        "unsupported",
        "maintenance SSH is not available on this image: marker missing",
    ));
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_UNSUPPORTED);
    assert_eq!(err.extra["reason"], "image_not_ready");
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, WindowState::OpenFailed);
}

#[tokio::test]
async fn off_appliance_and_unreachable_sysd_refuse_ssh_without_a_row() {
    for (mode, code) in [
        (Mode::Off, error_code::SSH_UNSUPPORTED),
        (Mode::Unreachable, error_code::SSH_UNREACHABLE),
    ] {
        let h = harness(mode);
        let ssh = caps(&["ssh"]);
        let err = enable(
            &h.env,
            &request(&ssh, Some(KEY_LINE), PASSWORD),
            &CountingVerifier::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code);
        assert!(
            MaintenanceStore::open(h.home.path())
                .unwrap()
                .latest_window()
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn an_unknown_open_outcome_is_rolled_back_and_a_failed_rollback_is_a_close_error() {
    let h = harness(Mode::Available);
    h.sysd.push_open(Err(SysdCallError::Transport(
        "sysd call timed out after 120s".into(),
    )));
    h.sysd.set_stop(Ok(SysdOpOutput {
        success: false,
        stdout: String::new(),
        stderr: "port 22 still open".into(),
    }));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_OPEN_FAILED);
    assert_eq!(h.sysd.calls("stop"), 1);
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.state, WindowState::OpenFailed);
    assert!(latest.close_error.unwrap().contains("port 22 still open"));
    assert!(
        h.notifier
            .sent()
            .iter()
            .any(|t| t.contains("無法確認遠端連線已關閉"))
    );
}

#[tokio::test]
async fn a_reply_for_another_key_is_not_accepted() {
    let h = harness(Mode::Available);
    h.sysd.push_open(ok(
        json!({"key_type": "ssh-ed25519", "key_fingerprint": "SHA256:someone-else",
                               "host_key_fingerprints": [HOST_FP]})
        .to_string(),
    ));
    let ssh = caps(&["ssh"]);
    let err = enable(
        &h.env,
        &request(&ssh, Some(KEY_LINE), PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, error_code::SSH_OPEN_FAILED);
    assert_eq!(h.sysd.calls("stop"), 1);
    assert!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .active_window()
            .await
            .unwrap()
            .is_none()
    );
}

// ── Re-auth lockout ─────────────────────────────────────────

#[tokio::test]
async fn five_wrong_passwords_lock_and_a_locked_account_is_never_verified() {
    let h = harness(Mode::Available);
    let verifier = CountingVerifier::new();
    let details = caps(&["show_details"]);
    for left in (1..=4).rev() {
        let err = enable(&h.env, &request(&details, None, "wrong"), &verifier)
            .await
            .unwrap_err();
        assert_eq!(err.code, error_code::REAUTH_FAILED);
        assert_eq!(err.extra["remaining_attempts"], left);
    }
    let err = enable(&h.env, &request(&details, None, "wrong"), &verifier)
        .await
        .unwrap_err();
    assert_eq!(err.code, error_code::REAUTH_LOCKED);
    assert!(err.extra["retry_after_seconds"].as_i64().unwrap() > 14 * 60);
    assert_eq!(verifier.calls(), 5);
    assert_eq!(
        h.notifier.sent().len(),
        1,
        "the lock is pushed to every admin"
    );

    // Locked: even the right password is not checked.
    let err = enable(&h.env, &request(&details, None, PASSWORD), &verifier)
        .await
        .unwrap_err();
    assert_eq!(err.code, error_code::REAUTH_LOCKED);
    assert_eq!(
        verifier.calls(),
        5,
        "verify_password must not run while locked"
    );

    let status = status_json(&h.env, Some("u1")).await.unwrap();
    assert_eq!(status["reauth"]["locked"], true);
    let other = status_json(&h.env, Some("u2")).await.unwrap();
    assert_eq!(other["reauth"]["locked"], false);
    assert!(kinds(h.home.path()).contains(&"reauth_locked".to_string()));
}

#[tokio::test]
async fn a_right_password_resets_the_failure_run() {
    let h = harness(Mode::Off);
    let verifier = CountingVerifier::new();
    let details = caps(&["show_details"]);
    for _ in 0..4 {
        enable(&h.env, &request(&details, None, "wrong"), &verifier)
            .await
            .unwrap_err();
    }
    let w = enable(&h.env, &request(&details, None, PASSWORD), &verifier)
        .await
        .unwrap();
    disable(&h.env, "u1", None).await.unwrap();
    let err = enable(&h.env, &request(&details, None, "wrong"), &verifier)
        .await
        .unwrap_err();
    assert_eq!(err.extra["remaining_attempts"], 4);
    assert_eq!(w.state, WindowState::Open);
}

// ── Close paths ─────────────────────────────────────────────

#[tokio::test]
async fn details_windows_cannot_hide_an_unconfirmed_ssh_close() {
    let h = harness(Mode::Available);
    let old = open_ssh_window(&h).await;
    h.provider.set_mode(Mode::Unreachable);
    disable(&h.env, "u1", None).await.unwrap();
    let details = caps(&["show_details"]);
    let fresh = enable(
        &h.env,
        &request(&details, None, PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap();
    let status = status_json(&h.env, None).await.unwrap();
    assert_eq!(status["window"]["id"], fresh.id);
    assert_eq!(status["close_unconfirmed"], true);

    // A positive closed status removes the warning immediately; the writer
    // poll persists that confirmation without deleting the historical error.
    h.provider.set_mode(Mode::Available);
    let mut pending: Value = serde_json::from_str(&closed_status()).unwrap();
    pending["pending_cleanup"] = json!(true);
    h.sysd.set_status(ok(pending.to_string()));
    poll_sysd(&h.env).await;
    assert!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .has_unconfirmed_close()
            .await
            .unwrap(),
        "a pending cleanup cannot acknowledge the earlier close error"
    );
    h.sysd.set_status(ok(closed_status()));
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    poll_sysd(&h.env).await;
    h.provider.set_mode(Mode::Unreachable);
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false,
        "an already confirmed old close must not reappear on a later outage"
    );
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    assert!(
        store
            .recent_windows(10)
            .await
            .unwrap()
            .iter()
            .any(|w| { w.id == old.id && w.close_error.is_some() }),
        "retain the original failure for history"
    );
}

#[tokio::test]
async fn disable_with_sysd_unreachable_on_an_appliance_is_a_close_error() {
    let h = harness(Mode::Available);
    open_ssh_window(&h).await;
    h.provider.set_mode(Mode::Unreachable);
    let window = disable(&h.env, "u1", Some("done")).await.unwrap().unwrap();
    assert!(
        window
            .close_error
            .as_deref()
            .unwrap()
            .contains("sysd unreachable")
    );
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert!(
        latest.close_error.is_some(),
        "never recorded as a clean close"
    );
    assert!(
        h.notifier
            .sent()
            .iter()
            .any(|t| t.contains("無法確認遠端連線已關閉"))
    );
    assert!(!kinds(h.home.path()).contains(&"ssh_key_removed".to_string()));

    let status = status_json(&h.env, None).await.unwrap();
    assert_eq!(status["close_unconfirmed"], true);
    assert_eq!(status["ssh_observed_active"], Value::Null);
    assert_eq!(status["ssh"]["reachable"], false);

    // Once sysd answers and confirms nothing is open, the warning clears.
    h.provider.set_mode(Mode::Available);
    let status = status_json(&h.env, None).await.unwrap();
    assert_eq!(status["close_unconfirmed"], false);
}

#[tokio::test]
async fn disable_with_a_reachable_sysd_records_the_key_removal() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    let closed = disable(&h.env, "u1", None).await.unwrap().unwrap();
    assert!(closed.close_error.is_none());
    assert_eq!(h.sysd.calls("stop"), 1);
    let removed = events(h.home.path())
        .into_iter()
        .find(|e| e["kind"] == "ssh_key_removed")
        .unwrap();
    assert_eq!(removed["window_id"], w.id);
    assert_eq!(removed["key_fingerprint"], KEY_FP);
}

#[tokio::test]
async fn off_appliance_close_has_nothing_to_close() {
    let h = harness(Mode::Off);
    // A legacy SSH row (no sysd involved here), closed off the appliance.
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    store.enable("u1", "a@b", 4, &caps(&["ssh"])).await.unwrap();
    let closed = disable(&h.env, "u1", None).await.unwrap().unwrap();
    assert!(closed.close_error.is_none());
}

#[tokio::test]
async fn ttl_expiry_with_sysd_unreachable_is_a_close_error() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    expire_now(h.home.path(), &w.id).await;
    h.provider.set_mode(Mode::Unreachable);
    sweep_expired(&h.env).await;
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.revoke_reason.as_deref(), Some(REVOKE_REASON_TTL));
    assert!(latest.close_error.is_some());
    let line = events(h.home.path())
        .into_iter()
        .find(|e| e["kind"] == "ttl_expired")
        .unwrap();
    assert!(line["close_error"].is_string());
}

// ── Boot reconciliation ─────────────────────────────────────

#[tokio::test]
async fn boot_waits_for_a_late_sysd_and_closes_an_orphan_key() {
    let h = harness(Mode::Available);
    h.sysd
        .set_status(ok(open_status("orphan-window", json!([]))));
    // sysd's socket shows up after a few retries (well inside the deadline).
    h.provider.unreachable_for.store(5, Ordering::SeqCst);
    boot_reconcile(&h.env, Vec::new()).await;
    assert_eq!(h.sysd.calls("stop"), 1);
    let k = kinds(h.home.path());
    assert!(k.contains(&"ssh_orphan_closed".to_string()), "{k:?}");
    assert!(
        !k.contains(&"ssh_reconcile_unconfirmed".to_string()),
        "a late sysd is not an error: {k:?}"
    );
}

#[tokio::test]
async fn boot_with_a_dead_sysd_records_close_error_after_the_deadline() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.provider.set_mode(Mode::Unreachable);
    let revoked = revoke_on_boot(h.home.path()).await;
    assert_eq!(revoked.len(), 1);
    // The ledger is closed before the dashboard can ask.
    assert!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .active_window()
            .await
            .unwrap()
            .is_none()
    );
    let started = std::time::Instant::now();
    boot_reconcile(&h.env, revoked).await;
    assert!(started.elapsed() >= h.env.reconcile_deadline);
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, w.id);
    assert_eq!(latest.revoke_reason.as_deref(), Some(REVOKE_REASON_RESTART));
    assert!(latest.close_error.is_some());
    let k = kinds(h.home.path());
    assert!(
        k.contains(&"ssh_reconcile_unconfirmed".to_string())
            && k.contains(&"gateway_restart_revoked".to_string())
    );
}

#[tokio::test]
async fn boot_counts_a_key_from_an_unfinished_open_as_an_orphan() {
    let h = harness(Mode::Available);
    // A gateway killed between the sysd open and `mark_open` (L13).
    let key = duduclaw_sysd::ssh_key::parse(KEY_LINE).unwrap();
    let ssh = caps(&["ssh"]);
    let row = MaintenanceStore::open(h.home.path())
        .unwrap()
        .insert(NewWindow {
            enabled_by: "u1",
            enabled_by_email: "a@b",
            ttl_hours: 4,
            sub_capabilities: &ssh,
            state: WindowState::Opening,
            ssh_key: Some(&key),
        })
        .await
        .unwrap();
    h.sysd.set_status(ok(open_status(&row.id, json!([]))));
    let revoked = revoke_on_boot(h.home.path()).await;
    boot_reconcile(&h.env, revoked).await;
    let k = kinds(h.home.path());
    assert!(k.contains(&"ssh_orphan_closed".to_string()), "{k:?}");
    let line = events(h.home.path())
        .into_iter()
        .find(|e| e["kind"] == "gateway_restart_revoked")
        .unwrap();
    assert!(line["close_error"].is_null());
}

#[tokio::test]
async fn boot_reconciliation_finishes_old_cleanup_before_a_new_enable() {
    let h = harness_with(
        Mode::Available,
        MockSysd {
            status_delay: Duration::from_millis(25),
            ..Default::default()
        },
        false,
    );
    let old = open_ssh_window(&h).await;
    let revoked = revoke_on_boot(h.home.path()).await;
    let ((), fresh) = tokio::join!(
        async {
            boot_reconcile(&h.env, revoked).await;
        },
        async {
            // Join polls boot first: its status call is already in flight.
            while h.sysd.calls("status") == 0 {
                tokio::task::yield_now().await;
            }
            open_ssh_window(&h).await
        }
    );
    assert_ne!(old.id, fresh.id);
    assert_eq!(
        *h.sysd.calls.lock().unwrap(),
        vec!["open", "status", "stop", "open"],
        "boot's unscoped stop must finish before a new key is opened"
    );
    assert_eq!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .active_window()
            .await
            .unwrap()
            .unwrap()
            .id,
        fresh.id
    );
}

#[tokio::test]
async fn status_remains_available_during_the_boot_readiness_wait() {
    let h = harness(Mode::Unreachable);
    let ((), ()) = tokio::join!(
        async {
            boot_reconcile(&h.env, Vec::new()).await;
        },
        async {
            // boot is waiting for sysd's readiness, not serialising readers.
            while h.provider.access_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            let status = tokio::time::timeout(Duration::from_millis(50), status_json(&h.env, None))
                .await
                .expect("status must not wait for boot reconciliation")
                .unwrap();
            assert_eq!(status["active"], false);
            assert_eq!(status["ssh"]["reason"], "sysd_unreachable");
            h.provider.set_mode(Mode::Available);
        }
    );
}

#[tokio::test]
async fn boot_closes_a_completed_window_as_a_restart_not_an_orphan() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.sysd.set_status(ok(open_status(&w.id, json!([]))));
    let revoked = revoke_on_boot(h.home.path()).await;
    boot_reconcile(&h.env, revoked).await;
    let k = kinds(h.home.path());
    assert!(!k.contains(&"ssh_orphan_closed".to_string()), "{k:?}");
    assert!(k.contains(&"gateway_restart_revoked".to_string()));
    assert_eq!(h.sysd.calls("stop"), 1);
}

#[tokio::test]
async fn after_a_reboot_sysd_reports_the_old_window_closed_by_reboot() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    // sysd's startup cleanup already closed the window across the reboot.
    let st = json!({"ready": true, "key_installed": false, "window_id": null, "ssh_active": false,
        "last_close": {"window_id": w.id, "trigger": "reboot", "events_complete": false, "events_lost": true}});
    h.sysd.set_status(ok(st.to_string()));
    let revoked = revoke_on_boot(h.home.path()).await;
    boot_reconcile(&h.env, revoked).await;
    let all = events(h.home.path());
    assert!(!all.iter().any(|e| e["kind"] == "ssh_orphan_closed"));
    let rec: Vec<_> = all
        .iter()
        .filter(|e| e["kind"] == "ssh_os_close_record")
        .collect();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0]["trigger"], "reboot");
    assert_eq!(rec[0]["events_lost"], true);
    let restart = all
        .iter()
        .find(|e| e["kind"] == "gateway_restart_revoked")
        .unwrap();
    assert!(restart["close_error"].is_null());
    // Seen again by the poll and by a later window: recorded only once.
    poll_sysd(&h.env).await;
    assert_eq!(
        events(h.home.path())
            .iter()
            .filter(|e| e["kind"] == "ssh_os_close_record")
            .count(),
        1
    );
    let s = status_json(&h.env, None).await.unwrap();
    assert_eq!(s["ssh"]["last_close"]["trigger"], "reboot");
    assert_eq!(s["close_unconfirmed"], false);
}

#[tokio::test]
async fn an_os_close_carries_sysds_trigger() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.sysd.set_status(ok(
        json!({"ready": true, "key_installed": false, "window_id": null, "ssh_active": false,
        "last_close": {"window_id": w.id, "trigger": "firewall_changed", "events_complete": true}})
        .to_string(),
    ));
    poll_sysd(&h.env).await;
    let line = events(h.home.path())
        .into_iter()
        .find(|e| e["kind"] == "ssh_closed_by_os")
        .unwrap();
    assert_eq!(line["os_trigger"], "firewall_changed");
}

#[tokio::test]
async fn boot_off_the_appliance_only_closes_the_ledger() {
    let h = harness(Mode::Off);
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    store
        .enable("u1", "a@b", 24, &caps(&["show_details"]))
        .await
        .unwrap();
    let revoked = revoke_on_boot(h.home.path()).await;
    boot_reconcile(&h.env, revoked).await;
    assert!(store.active_window().await.unwrap().is_none());
    assert_eq!(kinds(h.home.path())[0], "gateway_restart_revoked");
    assert!(
        revoke_on_boot(h.home.path()).await.is_empty(),
        "a clean boot is a no-op"
    );
}

// ── Poll: mirror, OS-side close ────────────────────────────

#[tokio::test]
async fn the_poll_mirrors_events_once_and_treats_their_text_as_data() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    let evs = json!([
        {"cursor": "c1", "kind": "login", "realtime_usec": 1_000_000, "pid": 7, "source": "10.0.0.2", "fingerprint": KEY_FP, "message": "m"},
        {"cursor": "c2", "kind": "auth_failed", "realtime_usec": 2_000_000, "source": "10.0.0.9", "message": "x"},
        {"cursor": "c3", "kind": "auth_failed", "realtime_usec": 3_000_000, "source": "10.0.0.9", "message": "x"},
        {"cursor": "c4", "kind": "auth_failed", "realtime_usec": 4_000_000, "source": "\u{1b}[31m10.0.0.8", "message": "x"},
        {"cursor": "c5", "kind": "menu_action", "realtime_usec": 5_000_000, "pid": 7, "message": format!("status\u{0007}\u{202E}{}", "A".repeat(400))},
        {"cursor": "c6", "kind": "logout", "realtime_usec": 6_000_000, "pid": 7, "source": "10.0.0.2", "message": "m"},
        {"cursor": "bad cursor", "kind": "login", "realtime_usec": 7_000_000, "message": "skipped"},
        {"cursor": "c7", "kind": "something_new", "message": "ignored"}
    ]);
    h.sysd.set_status(ok(open_status(&w.id, evs)));
    poll_sysd(&h.env).await;
    poll_sysd(&h.env).await;
    let all = events(h.home.path());
    let count = |k: &str| all.iter().filter(|e| e["kind"] == k).count();
    assert_eq!(count("ssh_session_started"), 1);
    assert_eq!(count("ssh_session_ended"), 1);
    assert_eq!(count("ssh_menu_action"), 1);
    assert_eq!(count("ssh_auth_failed"), 2, "aggregated per source");
    let nine = all
        .iter()
        .find(|e| e["kind"] == "ssh_auth_failed" && e["source"] == "10.0.0.9")
        .unwrap();
    assert_eq!(nine["count"], 2);
    let menu = all.iter().find(|e| e["kind"] == "ssh_menu_action").unwrap();
    let text = menu["message"].as_str().unwrap();
    assert!(
        text.chars().count() <= 128 && !text.chars().any(|c| c.is_control() || c == '\u{202E}')
    );
    let raw = std::fs::read_to_string(h.home.path().join("maintenance_audit.jsonl")).unwrap();
    assert!(
        !raw.contains("\\u001b"),
        "control characters never reach the audit file"
    );
    assert_eq!(
        h.sysd.calls("stop"),
        0,
        "a window this gateway opened is not an orphan"
    );
}

#[tokio::test]
async fn the_poll_records_a_window_the_os_closed_by_itself() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.sysd.set_status(ok(closed_status()));
    assert_eq!(
        poll_sysd(&h.env).await,
        super::lifecycle::SysdSupport::Current
    );
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, w.id);
    assert_eq!(latest.revoke_reason.as_deref(), Some(REVOKE_REASON_OS));
    assert!(latest.close_error.is_none());
    assert_eq!(
        h.sysd.calls("stop"),
        0,
        "a clean OS close needs no second stop"
    );
    assert!(kinds(h.home.path()).contains(&"ssh_closed_by_os".to_string()));
}

#[tokio::test]
async fn an_os_close_that_left_ssh_running_is_stopped_and_a_failed_stop_is_a_close_error() {
    let h = harness(Mode::Available);
    open_ssh_window(&h).await;
    // Key gone, but sysd's own close failed half-way: socket still up.
    h.sysd.set_status(ok(
        json!({"ready": true, "key_installed": false, "window_id": null,
                                 "ssh_active": true, "pending_cleanup": true})
        .to_string(),
    ));
    h.sysd.set_stop(Ok(SysdOpOutput {
        success: false,
        stdout: String::new(),
        stderr: "port 22 still open".into(),
    }));
    poll_sysd(&h.env).await;
    assert_eq!(h.sysd.calls("stop"), 1);
    let latest = MaintenanceStore::open(h.home.path())
        .unwrap()
        .latest_window()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.revoke_reason.as_deref(), Some(REVOKE_REASON_OS));
    assert!(latest.close_error.unwrap().contains("port 22 still open"));
    let line = events(h.home.path())
        .into_iter()
        .find(|e| e["kind"] == "ssh_closed_by_os")
        .unwrap();
    assert_eq!(line["os_close_clean"], false);
}

#[tokio::test]
async fn the_poll_closes_an_orphan_key() {
    let h = harness(Mode::Available);
    h.sysd.set_status(ok(open_status("not-ours", json!([]))));
    poll_sysd(&h.env).await;
    assert_eq!(h.sysd.calls("stop"), 1);
    assert!(
        h.notifier
            .sent()
            .iter()
            .any(|t| t.contains("沒有對應維修視窗"))
    );
}

// ── Supervisor ──────────────────────────────────────────────

#[tokio::test]
async fn supervisor_reserves_boot_cleanup_before_its_first_poll() {
    let h = harness(Mode::Available);
    open_ssh_window(&h).await;
    let revoked = revoke_on_boot(h.home.path()).await;
    let env = Arc::new(MaintEnv {
        home_dir: h.home.path().to_path_buf(),
        sysd: h.provider.clone(),
        notifier: h.notifier.clone(),
        reconcile_retry: h.env.reconcile_retry,
        reconcile_deadline: h.env.reconcile_deadline,
        tick_interval: Duration::from_secs(60),
    });
    let task = supervisor::spawn(env, revoked).await;
    // The current-thread test runtime has not polled the spawned task yet.
    assert_eq!(h.sysd.calls("status"), 0);
    let fresh = open_ssh_window(&h).await;
    task.abort();
    assert_eq!(
        *h.sysd.calls.lock().unwrap(),
        vec!["open", "status", "stop", "open"],
        "a new enable must wait even before the supervisor's first poll"
    );
    assert_eq!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .active_window()
            .await
            .unwrap()
            .unwrap()
            .id,
        fresh.id
    );
}

#[tokio::test]
async fn the_supervisor_expires_windows_with_no_dispatch_engine_running() {
    // No dispatch engine exists in this test at all: the supervisor alone
    // must sweep the TTL.
    let h = harness(Mode::Off);
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    let w = store
        .enable("u1", "a@b", 4, &caps(&["show_details"]))
        .await
        .unwrap();
    expire_now(h.home.path(), &w.id).await;
    let env = Arc::new(MaintEnv {
        home_dir: h.home.path().to_path_buf(),
        sysd: h.provider.clone(),
        notifier: h.notifier.clone(),
        reconcile_retry: Duration::from_millis(5),
        reconcile_deadline: Duration::from_millis(50),
        tick_interval: Duration::from_millis(20),
    });
    let task = supervisor::spawn(env, Vec::new()).await;
    let mut swept = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if store
            .latest_window()
            .await
            .unwrap()
            .unwrap()
            .revoke_reason
            .as_deref()
            == Some(REVOKE_REASON_TTL)
        {
            swept = true;
            break;
        }
    }
    task.abort();
    assert!(swept, "the supervisor must run the TTL sweep on its own");
}

#[tokio::test]
async fn an_old_sysd_is_not_polled_again() {
    let h = harness(Mode::Available);
    h.sysd.set_status(Err(SysdCallError::UnknownVerb));
    assert_eq!(
        boot_reconcile(&h.env, Vec::new()).await,
        super::lifecycle::SysdSupport::Old
    );
    let polls = h.sysd.calls("status");
    let next = supervisor::tick(&h.env, super::lifecycle::SysdSupport::Old).await;
    assert_eq!(next, super::lifecycle::SysdSupport::Old);
    assert_eq!(
        h.sysd.calls("status"),
        polls,
        "no poll once sysd is known to be old"
    );
    assert_eq!(poll_sysd(&h.env).await, super::lifecycle::SysdSupport::Old);
}

#[test]
fn the_dispatch_tick_no_longer_sweeps_maintenance() {
    let src = include_str!("../dispatch_engine/mod.rs");
    assert!(
        !src.contains("maintenance::sweep"),
        "the TTL sweep must not ride the dispatch tick"
    );
}

// ── Status ──────────────────────────────────────────────────

fn unconfirmed_notices(h: &Harness) -> usize {
    h.notifier
        .sent()
        .iter()
        .filter(|t| t.contains("無法確認遠端連線已關閉"))
        .count()
}

#[tokio::test]
async fn live_keyed_window_losing_sysd_is_unconfirmed_before_the_next_poll() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.provider.set_mode(Mode::Unreachable);
    let status = status_json(&h.env, None).await.unwrap();
    assert_eq!(status["window"]["id"], w.id);
    assert_eq!(
        status["active"], true,
        "ledger authority does not imply observed SSH state"
    );
    assert_eq!(status["ssh_observed_active"], Value::Null);
    assert_eq!(status["close_unconfirmed"], true);
    assert_eq!(
        unconfirmed_notices(&h),
        0,
        "status reads never push notices"
    );
}

#[tokio::test]
async fn lost_sysd_poll_persists_one_notice_until_a_reliable_same_window_recovery() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.provider.set_mode(Mode::Unreachable);
    poll_sysd(&h.env).await;
    poll_sysd(&h.env).await;
    assert_eq!(unconfirmed_notices(&h), 1);
    assert!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .has_unconfirmed_close()
            .await
            .unwrap()
    );
    assert!(
        MaintenanceStore::open(h.home.path())
            .unwrap()
            .current_row()
            .await
            .unwrap()
            .unwrap()
            .close_error
            .is_none()
    );
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        true
    );
    assert_eq!(unconfirmed_notices(&h), 1);

    h.provider.set_mode(Mode::Available);
    h.sysd.set_status(ok(open_status(&w.id, json!([]))));
    poll_sysd(&h.env).await;
    assert!(
        !MaintenanceStore::open(h.home.path())
            .unwrap()
            .has_unconfirmed_close()
            .await
            .unwrap()
    );
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    h.provider.set_mode(Mode::Unreachable);
    poll_sysd(&h.env).await;
    assert_eq!(
        unconfirmed_notices(&h),
        2,
        "a later loss after proven recovery is a new episode"
    );

    h.provider.set_mode(Mode::Available);
    h.sysd.set_status(ok(closed_status()));
    poll_sysd(&h.env).await;
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    assert!(store.current_row().await.unwrap().is_none());
    assert!(!store.has_unconfirmed_close().await.unwrap());
    h.provider.set_mode(Mode::Unreachable);
    poll_sysd(&h.env).await;
    assert_eq!(
        unconfirmed_notices(&h),
        2,
        "a revoked window cannot become uncertain again"
    );
}

#[tokio::test]
async fn failed_unreadable_and_transport_statuses_share_the_persistent_loss_episode() {
    let h = harness(Mode::Available);
    open_ssh_window(&h).await;
    for reply in [
        Err(SysdCallError::Transport("reply cut off".into())),
        ok("not JSON"),
        Ok(SysdOpOutput {
            success: false,
            stdout: String::new(),
            stderr: "refused".into(),
        }),
    ] {
        h.sysd.set_status(reply);
        assert_eq!(
            status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
            true
        );
        poll_sysd(&h.env).await;
    }
    assert_eq!(unconfirmed_notices(&h), 1);
    assert_eq!(
        h.sysd.calls("stop"),
        0,
        "an unavailable observation is not proof of an orphan"
    );
}

#[tokio::test]
async fn missing_legacy_non_ssh_and_off_appliance_windows_do_not_create_loss_notices() {
    let h = harness(Mode::Unreachable);
    poll_sysd(&h.env).await;
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    let details = caps(&["show_details"]);
    enable(
        &h.env,
        &request(&details, None, PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap();
    poll_sysd(&h.env).await;
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    disable(&h.env, "u1", None).await.unwrap();
    MaintenanceStore::open(h.home.path())
        .unwrap()
        .enable("u1", "admin@local", 1, &caps(&["ssh"]))
        .await
        .unwrap();
    poll_sysd(&h.env).await;
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    assert_eq!(unconfirmed_notices(&h), 0);

    let keyed = harness(Mode::Available);
    open_ssh_window(&keyed).await;
    keyed.sysd.set_status(Err(SysdCallError::UnknownVerb));
    assert_eq!(
        poll_sysd(&keyed.env).await,
        super::lifecycle::SysdSupport::Old
    );
    assert_eq!(
        status_json(&keyed.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    keyed.provider.set_mode(Mode::Off);
    poll_sysd(&keyed.env).await;
    assert_eq!(
        status_json(&keyed.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
    assert_eq!(unconfirmed_notices(&keyed), 0);
}

#[tokio::test]
async fn incomplete_json_cannot_hide_the_loss_of_a_keyed_window_observation() {
    for missing in [
        "all",
        "key_installed",
        "ssh_active",
        "window_id",
        "key_fingerprint",
    ] {
        let h = harness(Mode::Available);
        let w = open_ssh_window(&h).await;
        let mut body: Value = serde_json::from_str(&open_status(&w.id, json!([]))).unwrap();
        if missing == "all" {
            body = json!({});
        } else {
            body.as_object_mut().unwrap().remove(missing);
        }
        h.sysd.set_status(ok(body.to_string()));
        assert_eq!(
            status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
            true,
            "missing {missing} is unknown, even when it is valid JSON"
        );
        poll_sysd(&h.env).await;
        poll_sysd(&h.env).await;
        assert_eq!(unconfirmed_notices(&h), 1, "missing {missing}");
        let store = MaintenanceStore::open(h.home.path()).unwrap();
        assert!(store.has_unconfirmed_close().await.unwrap());
        let row = store.current_row().await.unwrap().unwrap();
        assert_eq!(row.id, w.id);
        assert_eq!(row.state, WindowState::Open);
        assert_eq!(
            h.sysd.calls("stop"),
            0,
            "missing {missing} is not proof of an orphan key"
        );
    }
}

#[tokio::test]
async fn status_recovery_does_not_acknowledge_other_close_errors_or_a_different_key() {
    let h = harness(Mode::Available);
    let w = open_ssh_window(&h).await;
    h.provider.set_mode(Mode::Unreachable);
    poll_sysd(&h.env).await;
    h.provider.set_mode(Mode::Available);
    let mut different_key: Value = serde_json::from_str(&open_status(&w.id, json!([]))).unwrap();
    different_key["key_fingerprint"] = json!("SHA256:different-key");
    h.sysd.set_status(ok(different_key.to_string()));
    poll_sysd(&h.env).await;
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    assert!(store.has_unconfirmed_close().await.unwrap());

    store
        .set_close_error(&w.id, "an independent failed close")
        .await;
    h.sysd.set_status(ok(open_status(&w.id, json!([]))));
    poll_sysd(&h.env).await;
    assert!(store.has_unconfirmed_close().await.unwrap());
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        true
    );
    h.provider.set_mode(Mode::Unreachable);
    poll_sysd(&h.env).await;
    assert_eq!(
        unconfirmed_notices(&h),
        2,
        "matching recovery ended only the prior observation outage"
    );
    h.provider.set_mode(Mode::Available);
    h.sysd.set_status(ok(closed_status()));
    poll_sysd(&h.env).await;
    assert!(!store.has_unconfirmed_close().await.unwrap());
    assert_eq!(
        store
            .latest_window()
            .await
            .unwrap()
            .unwrap()
            .close_error
            .as_deref(),
        Some("an independent failed close")
    );
}

#[tokio::test]
async fn an_unavailable_poll_waits_for_enable_to_commit_its_window() {
    let h = harness_with(
        Mode::Available,
        MockSysd {
            open_delay: Duration::from_millis(25),
            ..Default::default()
        },
        true,
    );
    h.sysd
        .set_status(Err(SysdCallError::Transport("lost status".into())));
    let ssh = caps(&["ssh"]);
    let req = request(&ssh, Some(KEY_LINE), PASSWORD);
    let verifier = CountingVerifier::new();
    let poll = async {
        for _ in 0..1000 {
            if h.sysd.calls("open") != 0 {
                return poll_sysd(&h.env).await;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("enable never reached sysd");
    };
    let (opened, _) = tokio::join!(enable(&h.env, &req, &verifier), poll);
    let w = opened.unwrap();
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    let row = store.current_row().await.unwrap().unwrap();
    assert_eq!(row.id, w.id);
    assert_eq!(row.state, WindowState::Open);
    assert!(store.has_unconfirmed_close().await.unwrap());
    assert_eq!(unconfirmed_notices(&h), 1);
    assert_eq!(h.sysd.calls("stop"), 0);
}

#[tokio::test]
async fn an_uncommitted_opening_row_does_not_reserve_a_loss_notice() {
    let h = harness(Mode::Unreachable);
    let key = duduclaw_sysd::ssh_key::parse(KEY_LINE).unwrap();
    let ssh = caps(&["ssh"]);
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    store
        .insert(NewWindow {
            enabled_by: "u1",
            enabled_by_email: "a@b",
            ttl_hours: 1,
            sub_capabilities: &ssh,
            state: WindowState::Opening,
            ssh_key: Some(&key),
        })
        .await
        .unwrap();
    poll_sysd(&h.env).await;
    assert!(!store.has_unconfirmed_close().await.unwrap());
    assert_eq!(unconfirmed_notices(&h), 0);
    assert_eq!(
        status_json(&h.env, None).await.unwrap()["close_unconfirmed"],
        false
    );
}

#[tokio::test]
async fn status_always_asks_sysd_and_reports_what_it_says() {
    let h = harness_with(Mode::Available, MockSysd::default(), true);
    let w = open_ssh_window(&h).await;
    h.sysd.set_status(ok(json!({"ready": true, "key_installed": true, "key_fingerprint": KEY_FP, "window_id": w.id,
        "expires_in_s": 100, "fallback_timer_active": true, "ssh_active": true,
        "sessions": [{"pid": 7, "source": "10.0.0.2", "fingerprint": KEY_FP, "since_usec": 1_000_000}],
        "events": [], "auth_failures": 3, "host_key_fingerprints": [HOST_FP],
        "pending_cleanup": false, "problems": ["nft:\u{1b} odd"]}).to_string()));
    let before = h.sysd.calls("status");
    let s = status_json(&h.env, Some("u1")).await.unwrap();
    assert_eq!(h.sysd.calls("status"), before + 1);
    assert_eq!(s["active"], true);
    assert_eq!(s["window"]["state"], "open");
    assert_eq!(s["window"]["ssh_key_fingerprint"], KEY_FP);
    assert_eq!(s["window"]["host_key_fingerprints"][0], HOST_FP);
    assert_eq!(s["ssh"]["supported"], true);
    assert_eq!(s["ssh"]["active_sessions"], 1);
    assert_eq!(s["ssh"]["auth_failures"], 3);
    assert_eq!(s["ssh"]["expires_in_s"], 100);
    assert!(!s["ssh"]["problems"][0].as_str().unwrap().contains('\u{1b}'));
    assert_eq!(s["ssh_observed_active"], true);
    assert_eq!(s["push_channel_available"], true);
    assert_eq!(s["close_unconfirmed"], false);
    assert_eq!(s["reauth"]["locked"], false);
    // Never the raw close_error text.
    assert!(s["window"].get("close_error").is_none());
}

#[tokio::test]
async fn status_marks_an_old_sysd_unsupported_and_an_unreachable_one_unknown() {
    let h = harness(Mode::Available);
    h.sysd.set_status(Err(SysdCallError::UnknownVerb));
    let s = status_json(&h.env, None).await.unwrap();
    assert_eq!(s["ssh"]["supported"], false);
    assert_eq!(s["ssh"]["reason"], "sysd_too_old");
    assert_eq!(s["push_channel_available"], false);

    h.sysd.set_status(ok(
        json!({"ready": false, "key_installed": false, "window_id": null}).to_string(),
    ));
    let s = status_json(&h.env, None).await.unwrap();
    assert_eq!(s["ssh"]["supported"], false);
    assert_eq!(s["ssh"]["reason"], "image_not_ready");

    h.provider.set_mode(Mode::Unreachable);
    let s = status_json(&h.env, None).await.unwrap();
    assert_eq!(s["ssh"]["supported"], Value::Null);
    assert_eq!(s["ssh_observed_active"], Value::Null);
    assert!(s.get("reauth").is_none());
}

#[tokio::test]
async fn pending_cleanup_counts_as_an_unconfirmed_close() {
    let h = harness(Mode::Available);
    h.sysd.set_status(ok(
        json!({"ready": true, "key_installed": false, "window_id": null, "pending_cleanup": true})
            .to_string(),
    ));
    let s = status_json(&h.env, None).await.unwrap();
    assert_eq!(s["close_unconfirmed"], true);
    assert_eq!(s["ssh"]["pending_cleanup"], true);
}

// ── Show-details windows (no sysd involved) ────────────────

#[tokio::test]
async fn a_show_details_window_round_trips_with_audit() {
    let h = harness(Mode::Off);
    let details = caps(&["show_details"]);
    let window = enable(
        &h.env,
        &request(&details, None, PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap();
    let status = status_json(&h.env, None).await.unwrap();
    assert_eq!(status["active"], true);
    assert_eq!(status["window"]["id"], window.id);
    let disabled = disable(&h.env, "u1", Some("done for the day"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(disabled.id, window.id);
    assert_eq!(status_json(&h.env, None).await.unwrap()["active"], false);
    let k = kinds(h.home.path());
    assert_eq!(k[0], "disabled");
    assert_eq!(k[1], "enabled");
    assert!(disable(&h.env, "u1", None).await.unwrap().is_none());
}

#[tokio::test]
async fn sweep_closes_an_expired_window_once() {
    let h = harness(Mode::Off);
    let details = caps(&["show_details"]);
    let window = enable(
        &h.env,
        &request(&details, None, PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap();
    expire_now(h.home.path(), &window.id).await;
    sweep_expired(&h.env).await;
    assert_eq!(kinds(h.home.path())[0], "ttl_expired");
    let n = kinds(h.home.path()).len();
    sweep_expired(&h.env).await;
    assert_eq!(kinds(h.home.path()).len(), n, "a no-op sweep adds nothing");
}

#[tokio::test]
async fn log_access_requires_an_open_window_with_show_details() {
    let h = harness(Mode::Off);
    let mut fields = serde_json::Map::new();
    fields.insert("operation".to_string(), json!("device.update_apply"));
    assert!(
        log_access(
            h.home.path(),
            "u1",
            "a@b",
            "access_view_detail",
            fields.clone()
        )
        .await
        .is_err()
    );

    // An `opening` row is not an open window.
    let store = MaintenanceStore::open(h.home.path()).unwrap();
    let details = caps(&["show_details"]);
    store
        .insert(NewWindow {
            enabled_by: "u1",
            enabled_by_email: "a@b",
            ttl_hours: 4,
            sub_capabilities: &details,
            state: WindowState::Opening,
            ssh_key: None,
        })
        .await
        .unwrap();
    assert!(
        log_access(
            h.home.path(),
            "u1",
            "a@b",
            "access_view_detail",
            fields.clone()
        )
        .await
        .is_err()
    );
    store.disable().await.unwrap();

    enable(
        &h.env,
        &request(&details, None, PASSWORD),
        &CountingVerifier::new(),
    )
    .await
    .unwrap();
    log_access(h.home.path(), "u1", "a@b", "access_view_detail", fields)
        .await
        .unwrap();
    let first = &events(h.home.path())[0];
    assert_eq!(first["kind"], "access_view_detail");
    assert_eq!(first["viewed_by_email"], "a@b");
}

#[tokio::test]
async fn an_older_database_gains_the_new_columns() {
    let home = tempfile::tempdir().unwrap();
    {
        let conn = rusqlite::Connection::open(home.path().join("maintenance.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE maintenance_windows (
                 id TEXT PRIMARY KEY, enabled_by TEXT NOT NULL, enabled_by_email TEXT NOT NULL,
                 enabled_at TEXT NOT NULL, ttl_seconds INTEGER NOT NULL, expires_at TEXT NOT NULL,
                 sub_capabilities TEXT NOT NULL DEFAULT '[]', revoked_at TEXT, revoke_reason TEXT,
                 reminded_at TEXT, close_error TEXT);
             INSERT INTO maintenance_windows VALUES ('old-closed','u','a','2026-01-01T00:00:00+00:00',3600,
                 '2026-01-01T01:00:00+00:00','[\"ssh\"]','2026-01-01T00:30:00+00:00','admin_disabled',NULL,NULL);",
        )
        .unwrap();
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        conn.execute(
            "INSERT INTO maintenance_windows (id, enabled_by, enabled_by_email, enabled_at, ttl_seconds, expires_at, sub_capabilities)
             VALUES ('old-open','u','a',?1,3600,?2,'[\"show_details\"]')",
            rusqlite::params![Utc::now().to_rfc3339(), future],
        )
        .unwrap();
    }
    let store = MaintenanceStore::open(home.path()).unwrap();
    let active = store.active_window().await.unwrap().unwrap();
    assert_eq!(active.id, "old-open");
    assert_eq!(active.state, WindowState::Open);
    let all = store.recent_windows(10).await.unwrap();
    let closed = all.iter().find(|w| w.id == "old-closed").unwrap();
    assert_eq!(closed.state, WindowState::Closed);
    // Reopening runs the migration again harmlessly.
    drop(store);
    assert!(MaintenanceStore::open(home.path()).is_ok());
}
