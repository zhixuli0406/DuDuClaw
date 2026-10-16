//! End-to-end tests of the maintenance SSH flow against a scripted host and a
//! temp directory (no systemd, nft, dropbear or journald needed).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::maint_ssh::host::{CmdOut, Lines};
use crate::maint_ssh::journal::tests::{FAILED, LOGIN, LOGOUT, TEST_DROPBEAR_EXE, TEST_MENU_EXE, entry};
use crate::maint_ssh::journal::{DROPBEAR_SLICE, MENU_IDENTIFIER};
use crate::maint_ssh::verify::tests as vt;

const JOURNAL_READ_PREFIX: &str =
    "journalctl --no-pager -o json --output-fields=MESSAGE,_UID,_SYSTEMD_SLICE,_COMM,_EXE,_PID,SYSLOG_IDENTIFIER --after-cursor=c";

const ED25519: &str = include_str!("../../tests/fixtures/ssh_keys/ed25519.pub");

fn key_wire() -> String {
    let mut f = ED25519.split_ascii_whitespace();
    format!("{} {}", f.next().unwrap(), f.next().unwrap())
}

fn me() -> (u32, u32) {
    // SAFETY: no preconditions.
    unsafe { (libc::getuid(), libc::getgid()) }
}

/// Scripted OS. Every command sysd may run has an answer; anything else is an
/// error so an unexpected command fails the test.
struct FakeHost {
    calls: Mutex<Vec<String>>,
    exec_start: Mutex<String>,
    socket_binds: Mutex<String>,
    output_chain: Mutex<String>,
    egress_chain: Mutex<String>,
    input_chain: Mutex<String>,
    src4: Mutex<String>,
    user: Mutex<Option<(u32, u32)>>,
    fail_arm: AtomicBool,
    fail_start: AtomicBool,
    fail_journal: AtomicBool,
    timer_active: AtomicBool,
    ssh_running: AtomicBool,
    ssh_start_calls: AtomicUsize,
    ssh_stop_calls: AtomicUsize,
    ssh_start_delay: Mutex<Duration>,
    /// Journal lines; the cursor of line i is `c<i+1>` (`c0` is the position
    /// before any of them).
    journal: Mutex<Vec<String>>,
    boot: Mutex<Duration>,
    real: Mutex<Duration>,
    boot_id: Mutex<String>,
}

impl FakeHost {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            exec_start: Mutex::new(vt::good_exec_start()),
            socket_binds: Mutex::new("sockets.target duduclaw-sysd.service".into()),
            output_chain: Mutex::new(vt::output_chain("")),
            egress_chain: Mutex::new(vt::egress_chain("4800")),
            input_chain: Mutex::new(vt::input_chain(false)),
            src4: Mutex::new(vt::src_set(verify::SRC4_SET, &verify::EXPECTED_SRC4)),
            user: Mutex::new(Some((MAINT_UID, MAINT_GID))),
            fail_arm: AtomicBool::new(false),
            fail_start: AtomicBool::new(false),
            fail_journal: AtomicBool::new(false),
            timer_active: AtomicBool::new(false),
            ssh_running: AtomicBool::new(false),
            ssh_start_calls: AtomicUsize::new(0),
            ssh_stop_calls: AtomicUsize::new(0),
            ssh_start_delay: Mutex::new(Duration::ZERO),
            journal: Mutex::new(Vec::new()),
            boot: Mutex::new(Duration::from_secs(1000)),
            real: Mutex::new(Duration::from_secs(1_760_600_000)),
            boot_id: Mutex::new("0f0e0d0c-0b0a-4908-8706-050403020100".to_string()),
        }
    }

    fn push_journal(&self, slice: &str, uid: u32, ident: &str, pid: u32, msg: &str) {
        let mut j = self.journal.lock().unwrap();
        let cursor = format!("c{}", j.len() + 1);
        j.push(entry(&cursor, slice, uid, ident, pid, msg));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn ok(stdout: &str) -> Result<CmdOut, String> {
        Ok(CmdOut { success: true, stdout: stdout.to_string(), stderr: String::new() })
    }

    fn fail(stderr: &str) -> Result<CmdOut, String> {
        Ok(CmdOut { success: false, stdout: String::new(), stderr: stderr.to_string() })
    }
}

impl MaintHost for FakeHost {
    async fn run(&self, argv: &[String]) -> Result<CmdOut, String> {
        let line = argv.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        let ed = key_wire();
        match line.as_str() {
            "systemctl show dropbear@probe.service -p ExecStart --value" => Self::ok(&self.exec_start.lock().unwrap()),
            "systemctl show dropbear.socket -p BindsTo --value" => Self::ok(&self.socket_binds.lock().unwrap()),
            "systemctl show dropbear@probe.service -p BindsTo --value" => Self::ok("duduclaw-sysd.service"),
            "nft -j list chain inet filter output" => Self::ok(&self.output_chain.lock().unwrap()),
            "nft -j list chain inet filter duduclaw_maint_egress" => Self::ok(&self.egress_chain.lock().unwrap()),
            "nft -j list chain inet filter input" => Self::ok(&self.input_chain.lock().unwrap()),
            "nft -j list set inet filter maintenance_src4" => Self::ok(&self.src4.lock().unwrap()),
            "nft -j list set inet filter maintenance_src6" => Self::ok(&vt::src_set(verify::SRC6_SET, &verify::EXPECTED_SRC6)),
            "systemctl start dropbearkey.service" => Self::ok(""),
            "dropbearkey -y -f /data/system/dropbear/dropbear_ed25519_host_key" => {
                Self::ok(&format!("Public key portion is:\n{ed} root@box\nFingerprint: SHA256:x\n"))
            }
            "journalctl --no-pager -o json -n 1" => {
                let n = self.journal.lock().unwrap().len();
                Self::ok(&format!("{{\"__CURSOR\":\"c{n}\",\"MESSAGE\":\"x\"}}\n"))
            }
            "systemctl reset-failed duduclaw-maint-ssh-expire.timer duduclaw-maint-ssh-expire.service" => Self::ok(""),
            "systemctl stop duduclaw-maint-ssh-expire.timer" => {
                self.timer_active.store(false, Ordering::SeqCst);
                Self::ok("")
            }
            "systemctl is-active duduclaw-maint-ssh-expire.timer" => {
                Self::ok(if self.timer_active.load(Ordering::SeqCst) { "active\n" } else { "inactive\n" })
            }
            "systemctl is-active dropbear.socket" => {
                Self::ok(if self.ssh_running.load(Ordering::SeqCst) { "active\n" } else { "inactive\n" })
            }
            _ if line.starts_with("systemd-run ") => {
                if self.fail_arm.load(Ordering::SeqCst) {
                    return Self::fail("Failed to start transient timer unit");
                }
                self.timer_active.store(true, Ordering::SeqCst);
                Self::ok("Running timer as unit: duduclaw-maint-ssh-expire.timer")
            }
            _ if line.starts_with(JOURNAL_READ_PREFIX) => {
                if self.fail_journal.load(Ordering::SeqCst) {
                    return Self::fail("Failed to seek to cursor");
                }
                let rest = line.trim_start_matches(JOURNAL_READ_PREFIX);
                let (n, filter) = rest.split_once(' ').unwrap();
                assert_eq!(filter, "_SYSTEMD_SLICE=system-dropbear.slice");
                let n: usize = n.parse().unwrap();
                let j = self.journal.lock().unwrap();
                Self::ok(&j.iter().skip(n).cloned().collect::<Vec<_>>().join("\n"))
            }
            _ => Err(format!("unexpected command: {line}")),
        }
    }

    async fn run_lines(&self, argv: &[String], max_lines: usize) -> Result<Lines, String> {
        let out = self.run(argv).await?;
        if !out.success {
            return Err(out.stderr);
        }
        let all: Vec<String> = out.stdout.lines().map(str::to_string).collect();
        let truncated = all.len() > max_lines;
        Ok(Lines { lines: all.into_iter().take(max_lines).collect(), truncated })
    }

    async fn ssh_start(&self) -> DispatchResult {
        self.ssh_start_calls.fetch_add(1, Ordering::SeqCst);
        let delay = *self.ssh_start_delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        if self.fail_start.load(Ordering::SeqCst) {
            return Ok(SysdOpOutput { success: false, stdout: String::new(), stderr: "unit failed".into() });
        }
        self.ssh_running.store(true, Ordering::SeqCst);
        Ok(SysdOpOutput { success: true, stdout: "started dropbear.socket".into(), stderr: String::new() })
    }

    async fn ssh_stop(&self) -> DispatchResult {
        self.ssh_stop_calls.fetch_add(1, Ordering::SeqCst);
        self.ssh_running.store(false, Ordering::SeqCst);
        Ok(SysdOpOutput { success: true, stdout: "stopped dropbear.socket".into(), stderr: String::new() })
    }

    fn boottime(&self) -> Result<Duration, String> {
        Ok(*self.boot.lock().unwrap())
    }

    fn realtime(&self) -> Result<Duration, String> {
        Ok(*self.real.lock().unwrap())
    }

    fn user_ids(&self, name: &str) -> Option<(u32, u32)> {
        if name == MAINT_USER { *self.user.lock().unwrap() } else { None }
    }

    fn group_id(&self, name: &str) -> Option<u32> {
        (name == MAINT_USER).then_some(MAINT_GID)
    }

    fn trusted_exe(&self, pin: &host::ExePin) -> Result<String, String> {
        // The appliance layout: dropbear is the multi-call binary.
        if pin.path.ends_with("dropbear") {
            Ok(TEST_DROPBEAR_EXE.to_string())
        } else {
            Ok(TEST_MENU_EXE.to_string())
        }
    }

    fn boot_id(&self) -> Result<String, String> {
        Ok(self.boot_id.lock().unwrap().clone())
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    cfg: MaintConfig,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let (uid, gid) = me();
    std::fs::create_dir_all(tmp.path().join("run")).unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    let marker = tmp.path().join("maint-ssh.ready");
    std::fs::write(&marker, b"1\n").unwrap();
    let mut executables = Vec::new();
    for name in ["maint-shell", "maint-ssh-close"] {
        let p = tmp.path().join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        executables.push(p);
    }
    let cfg = MaintConfig {
        ready_marker: marker,
        key_dir: tmp.path().join("run/duduclaw-maint-ssh"),
        state_dir: tmp.path().join("data/maint-ssh"),
        state_dir_owner: Ownership { uid, gid, mode: 0o700 },
        dropbear_exe: host::ExePin { path: "/usr/sbin/dropbear".into(), dir: "/usr/sbin".into() },
        menu_exe: host::ExePin { path: MAINT_SHELL.into(), dir: "/usr/libexec/duduclaw".into() },
        audit: AuditLog::new(tmp.path().join("data/maint-ssh-audit.log"), uid),
        dir_owner: Ownership { uid, gid, mode: 0o750 },
        key_owner: Ownership { uid, gid, mode: 0o640 },
        state_owner: Ownership { uid, gid, mode: 0o600 },
        maint_uid: MAINT_UID,
        maint_gid: MAINT_GID,
        required_executables: executables,
        exec_owner_uid: uid,
        open_min_interval: Duration::ZERO,
        open_max_per_hour: usize::MAX,
    };
    Env { _tmp: tmp, cfg }
}

impl Env {
    fn key_path(&self) -> PathBuf {
        self.cfg.key_dir.join(KEY_FILE)
    }
    fn window_path(&self) -> PathBuf {
        self.cfg.state_dir.join(WINDOW_FILE)
    }
    fn audit(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.cfg.audit.path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn audit_events(&self) -> Vec<String> {
        self.audit().iter().map(|r| r["event"].as_str().unwrap().to_string()).collect()
    }
}

async fn open_default(state: &mut MaintState, host: &FakeHost, e: &Env) -> DispatchResult {
    open(state, host, &e.cfg, &key_wire(), 3600, "w-1").await
}

#[tokio::test]
async fn open_writes_the_key_window_timer_and_open_record() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    let out = open_default(&mut state, &host, &e).await.expect("open");
    let body: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(body["login_user"], "duduclaw-maint");
    assert_eq!(body["key_type"], "ssh-ed25519");
    assert_eq!(body["key_fingerprint"], "SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE");
    assert_eq!(body["expires_in_s"], 3600);
    assert_eq!(body["host_key_fingerprints"][0], "SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE");

    let key = std::fs::read_to_string(e.key_path()).unwrap();
    assert_eq!(key, format!("no-port-forwarding,no-agent-forwarding,no-X11-forwarding {}\n", key_wire()));
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(e.key_path()).unwrap().mode() & 0o7777, 0o640);
    assert_eq!(std::fs::metadata(&e.cfg.key_dir).unwrap().mode() & 0o7777, 0o750);
    assert_eq!(std::fs::metadata(e.window_path()).unwrap().mode() & 0o7777, 0o600);
    let w: WindowFile = serde_json::from_slice(&std::fs::read(e.window_path()).unwrap()).unwrap();
    assert_eq!(w.window_id, "w-1");
    assert_eq!(w.open_cursor, "c0");
    assert_eq!(w.deadline_boottime_ms, 1000 * 1000 + 3600 * 1000);

    // R7 armed after reset-failed, with both triggers and CollectMode.
    let calls = host.calls();
    let reset = calls.iter().position(|c| c.starts_with("systemctl reset-failed")).unwrap();
    let arm = calls.iter().position(|c| c.starts_with("systemd-run")).unwrap();
    assert!(reset < arm);
    let arm_line = &calls[arm];
    assert!(arm_line.contains("--on-active=3660s"));
    assert!(arm_line.contains("--timer-property=OnCalendar=2025-10-16 08:34:20 UTC"));
    assert!(arm_line.contains("--property=CollectMode=inactive-or-failed"));
    assert!(host.timer_active.load(Ordering::SeqCst));
    assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 1);
    assert_eq!(e.audit_events(), ["open"]);
    assert_eq!(e.audit()[0]["window_id"], "w-1");
}

#[tokio::test]
async fn a_second_open_is_busy_and_keeps_the_first_key() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    let before = std::fs::read(e.key_path()).unwrap();
    let err = open(&mut state, &host, &e.cfg, &key_wire(), 600, "w-2").await.unwrap_err();
    assert_eq!(err.kind, "busy");
    assert_eq!(std::fs::read(e.key_path()).unwrap(), before);
    assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 1);

    // A key left on disk without a window in memory is also `busy`
    // (RENAME_NOREPLACE), and is not replaced.
    let mut fresh = MaintState::new();
    let err = open(&mut fresh, &host, &e.cfg, &key_wire(), 600, "w-3").await.unwrap_err();
    assert_eq!(err.kind, "busy");
    assert_eq!(std::fs::read(e.key_path()).unwrap(), before);
}

#[tokio::test]
async fn bad_parameters_are_refused_before_anything_is_written() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    for (key, ttl, id, want) in [
        (key_wire(), 59, "w", "ttl_seconds"),
        (key_wire(), 86_401, "w", "ttl_seconds"),
        (key_wire(), 600, "", "window_id"),
        (key_wire(), 600, "w/../x", "window_id"),
        (key_wire(), 600, &"a".repeat(65), "window_id"),
        ("ssh-dss AAAA".to_string(), 600, "w", "ssh_key:unsupported_type"),
        ("-----BEGIN OPENSSH PRIVATE KEY-----".to_string(), 600, "w", "ssh_key:private_key"),
        (format!("restrict {}", key_wire()), 600, "w", "ssh_key:options_not_allowed"),
    ] {
        let err = open(&mut state, &host, &e.cfg, &key, ttl, id).await.unwrap_err();
        assert_eq!(err.kind, "bad_request", "{want}");
        assert!(err.message.contains(want), "{} vs {want}", err.message);
        assert!(!err.message.contains("OPENSSH"), "the raw key never comes back");
    }
    assert!(!e.cfg.key_dir.exists());
    assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 0);
    assert!(e.audit().is_empty());
}

#[tokio::test]
async fn an_image_that_is_not_set_up_is_unsupported() {
    type Breaker = Box<dyn Fn(&FakeHost, &Env)>;
    let cases: Vec<(&str, Breaker)> = vec![
        ("marker missing", Box::new(|_, e| std::fs::remove_file(&e.cfg.ready_marker).unwrap())),
        ("marker content", Box::new(|_, e| std::fs::write(&e.cfg.ready_marker, b"0\n").unwrap())),
        ("wrong uid", Box::new(|h, _| *h.user.lock().unwrap() = Some((1001, 4800)))),
        ("no account", Box::new(|h, _| *h.user.lock().unwrap() = None)),
        ("extra token", Box::new(|h, _| {
            let s = vt::good_exec_start().replace(" ; ignore_errors", " -R ; ignore_errors");
            *h.exec_start.lock().unwrap() = s;
        })),
        ("missing -s", Box::new(|h, _| *h.exec_start.lock().unwrap() = vt::good_exec_start().replacen(" -s ", " ", 1))),
        ("no BindsTo", Box::new(|h, _| *h.socket_binds.lock().unwrap() = "sockets.target".into())),
        ("no jump", Box::new(|h, _| *h.output_chain.lock().unwrap() = vt::output_chain("").replace("duduclaw_maint_egress", "x"))),
        ("no skuid rule", Box::new(|h, _| *h.egress_chain.lock().unwrap() = vt::egress_chain("1000"))),
        ("legacy input rule", Box::new(|h, _| *h.input_chain.lock().unwrap() = vt::input_chain(true))),
        ("source set", Box::new(|h, _| *h.src4.lock().unwrap() = vt::src_set(verify::SRC4_SET, &verify::EXPECTED_SRC4[..4]))),
        ("fallback script missing", Box::new(|_, e| std::fs::remove_file(&e.cfg.required_executables[1]).unwrap())),
        ("menu shell group-writable", Box::new(|_, e| {
            std::fs::set_permissions(&e.cfg.required_executables[0], std::os::unix::fs::PermissionsExt::from_mode(0o775)).unwrap()
        })),
    ];
    for (name, breaker) in cases {
        let e = env();
        let host = FakeHost::new();
        breaker(&host, &e);
        let mut state = MaintState::new();
        let err = open_default(&mut state, &host, &e).await.unwrap_err();
        assert_eq!(err.kind, "unsupported", "{name}: {}", err.message);
        assert!(!e.key_path().exists(), "{name}");
        assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 0, "{name}");
        assert!(!host.calls().iter().any(|c| c.starts_with("systemd-run")), "{name}");
    }
}

#[tokio::test]
async fn a_fallback_timer_that_cannot_be_armed_undoes_the_open() {
    let e = env();
    let host = FakeHost::new();
    host.fail_arm.store(true, Ordering::SeqCst);
    let mut state = MaintState::new();
    let err = open_default(&mut state, &host, &e).await.unwrap_err();
    assert!(err.message.contains("fallback timer"), "{}", err.message);
    assert!(!e.key_path().exists());
    assert!(!e.window_path().exists());
    assert!(state.window().is_none());
    assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 0);
    // Only the best-effort record of the failed attempt.
    assert_eq!(e.audit_events(), ["open_failed"]);
    assert_eq!(e.audit()[0]["stage"], "TimerArmed");
}

#[tokio::test]
async fn an_ssh_start_failure_undoes_everything() {
    let e = env();
    let host = FakeHost::new();
    host.fail_start.store(true, Ordering::SeqCst);
    let mut state = MaintState::new();
    let err = open_default(&mut state, &host, &e).await.unwrap_err();
    assert!(err.message.contains("could not start SSH"));
    assert!(!e.key_path().exists());
    assert!(!e.window_path().exists());
    assert!(!host.timer_active.load(Ordering::SeqCst), "R7 cancelled");
    assert_eq!(host.ssh_stop_calls.load(Ordering::SeqCst), 1);
    assert!(state.window().is_none());
    // The next open works.
    host.fail_start.store(false, Ordering::SeqCst);
    open_default(&mut state, &host, &e).await.unwrap();
}

#[tokio::test]
async fn stop_writes_pending_events_first_then_closes_everything() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 42, LOGIN);
    host.push_journal(DROPBEAR_SLICE, 4800, "dropbear", 42, LOGOUT);
    host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 43, FAILED);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(out.success, "{}", out.stderr);
    assert!(out.stdout.contains("removed the maintenance key"));
    assert!(!e.key_path().exists());
    assert!(!e.window_path().exists());
    assert!(!host.timer_active.load(Ordering::SeqCst));
    assert!(!host.ssh_running.load(Ordering::SeqCst));
    assert!(state.window().is_none());
    assert_eq!(e.audit_events(), ["open", "login", "logout", "close"]);
    let close_rec = e.audit().pop().unwrap();
    assert_eq!(close_rec["trigger"], "stop");
    assert_eq!(close_rec["removed_fingerprint"], "SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE");
    assert_eq!(close_rec["auth_failures"], 1);
    assert_eq!(close_rec["auth_failure_sources"][0], "100.64.3.4");
    // Events are read before the key goes and once more after dropbear stopped.
    let calls = host.calls();
    let is_read = |c: &String| c.starts_with("journalctl --no-pager -o json --output-fields=");
    let first_read = calls.iter().position(is_read).unwrap();
    let last_read = calls.iter().rposition(is_read).unwrap();
    let stop_timer = calls.iter().rposition(|c| c == "systemctl stop duduclaw-maint-ssh-expire.timer").unwrap();
    assert!(first_read < stop_timer && stop_timer < last_read);

    // A second stop is idempotent and writes no second close record.
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(out.success, "{}", out.stderr);
    assert_eq!(e.audit_events().len(), 4);
}

#[tokio::test]
async fn stop_on_an_image_that_never_opened_a_window_is_the_legacy_close() {
    let e = env();
    std::fs::remove_file(&e.cfg.ready_marker).unwrap();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(out.success, "{}", out.stderr);
    assert_eq!(out.stdout, "stopped dropbear.socket");
    assert_eq!(host.ssh_stop_calls.load(Ordering::SeqCst), 1);
    assert!(e.audit().is_empty());
}

#[tokio::test]
async fn the_tick_records_logins_without_any_status_call_and_closes_at_the_deadline() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open(&mut state, &host, &e.cfg, &key_wire(), 120, "w-1").await.unwrap();
    host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 7, LOGIN);
    tick(&mut state, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["open", "login"]);
    let w: WindowFile = serde_json::from_slice(&std::fs::read(e.window_path()).unwrap()).unwrap();
    assert_eq!(w.last_cursor, "c1", "cursor persisted for R5");
    host.push_journal(DROPBEAR_SLICE, 4800, "dropbear", 7, LOGOUT);
    tick(&mut state, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["open", "login", "logout"]);

    // Moving the wall clock does nothing: the deadline is on CLOCK_BOOTTIME.
    *host.real.lock().unwrap() += Duration::from_secs(10 * 86_400);
    tick(&mut state, &host, &e.cfg).await;
    assert!(e.key_path().exists());
    *host.boot.lock().unwrap() += Duration::from_secs(119);
    tick(&mut state, &host, &e.cfg).await;
    assert!(e.key_path().exists());
    *host.boot.lock().unwrap() += Duration::from_secs(1);
    tick(&mut state, &host, &e.cfg).await;
    assert!(!e.key_path().exists());
    assert!(!host.timer_active.load(Ordering::SeqCst));
    assert_eq!(e.audit().last().unwrap()["trigger"], "deadline");
}

#[tokio::test]
async fn startup_cleanup_replays_from_the_saved_cursor_and_cancels_the_timer() {
    let e = env();
    let host = FakeHost::new();
    {
        let mut old_run = MaintState::new();
        open_default(&mut old_run, &host, &e).await.unwrap();
        host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 9, LOGIN);
        tick(&mut old_run, &host, &e.cfg).await;
        // sysd dies here; a logout arrives while it is down.
        host.push_journal(DROPBEAR_SLICE, 4800, "dropbear", 9, LOGOUT);
    }
    let mut new_run = MaintState::new();
    startup_cleanup(&mut new_run, &host, &e.cfg).await;
    assert!(!e.key_path().exists());
    assert!(!e.window_path().exists());
    assert!(!host.timer_active.load(Ordering::SeqCst));
    assert_eq!(e.audit_events(), ["open", "login", "logout", "close"]);
    assert_eq!(e.audit().last().unwrap()["trigger"], "startup_cleanup");
    assert_eq!(e.audit().last().unwrap()["window_id"], "w-1");
}

#[tokio::test]
async fn startup_cleanup_removes_a_key_without_window_state() {
    let e = env();
    let host = FakeHost::new();
    let mut s = MaintState::new();
    open_default(&mut s, &host, &e).await.unwrap();
    std::fs::remove_file(e.window_path()).unwrap();
    let mut new_run = MaintState::new();
    startup_cleanup(&mut new_run, &host, &e.cfg).await;
    assert!(!e.key_path().exists());
    let rec = e.audit().pop().unwrap();
    assert_eq!(rec["trigger"], "startup_cleanup");
    assert_eq!(rec["removed_fingerprint"], "SHA256:YoPXM1Ypqr+D/3+TBuMYB3Z4IZH0k0ErqeSny62rhCE");
    assert_eq!(rec["window_id"], Value::Null);
}

#[tokio::test]
async fn logins_beyond_fifty_are_counted_not_written() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    for pid in 0..60 {
        host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 1000 + pid, LOGIN);
    }
    tick(&mut state, &host, &e.cfg).await;
    let logins = e.audit_events().iter().filter(|k| *k == "login").count();
    assert_eq!(logins, LOGIN_RECORD_CAP as usize);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(out.success);
    let rec = e.audit().pop().unwrap();
    assert_eq!(rec["logins_recorded"], 50);
    assert_eq!(rec["logins_over_cap"], 10);
}

#[tokio::test]
async fn forged_journal_events_never_reach_the_audit_file() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    // uid 1000 with `logger -t dropbear`, and a root process outside the slice.
    host.push_journal("user-1000.slice", 1000, "dropbear", 5, LOGIN);
    host.push_journal("system.slice", 0, "dropbear", 5, LOGIN);
    host.push_journal(DROPBEAR_SLICE, 1000, "dropbear", 5, LOGIN);
    tick(&mut state, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["open"]);
    // The cursor still moved past them.
    assert_eq!(state.window().unwrap().last_cursor, "c3");
}

#[tokio::test]
async fn status_reports_sessions_events_and_timer_state() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    let out = status(&mut state, &host, &e.cfg).await.unwrap();
    let idle: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(idle["ready"], true);
    assert_eq!(idle["key_installed"], false);
    assert_eq!(idle["window_id"], Value::Null);
    assert_eq!(idle["fallback_timer_active"], false);

    open_default(&mut state, &host, &e).await.unwrap();
    host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 11, LOGIN);
    host.push_journal(DROPBEAR_SLICE, 4800, MENU_IDENTIFIER, 12, "status");
    *host.boot.lock().unwrap() += Duration::from_secs(600);
    let out = status(&mut state, &host, &e.cfg).await.unwrap();
    let s: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(s["key_installed"], true);
    assert_eq!(s["window_id"], "w-1");
    assert_eq!(s["expires_in_s"], 3000);
    assert_eq!(s["fallback_timer_active"], true);
    assert_eq!(s["ssh_active"], true);
    assert_eq!(s["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(s["sessions"][0]["source"], "192.168.1.20");
    assert_eq!(s["events"].as_array().unwrap().len(), 2);
    assert_eq!(s["events"][1]["kind"], "menu_action");
    // Status wrote the login itself (it never lags the audit file).
    assert_eq!(e.audit_events(), ["open", "login"]);
}

#[test]
fn the_status_buffer_keeps_logins_when_full() {
    let mk = |kind, n: usize| SshEvent {
        cursor: format!("c{n}"),
        kind,
        realtime_usec: 0,
        pid: None,
        source: None,
        fingerprint: None,
        message: String::new(),
    };
    let mut b = EventBuffer::new();
    for i in 0..STATUS_EVENT_CAP {
        b.push(mk(EventKind::AuthFailed, i));
    }
    b.push(mk(EventKind::AuthFailed, 999));
    assert_eq!(b.items.len(), STATUS_EVENT_CAP);
    assert_eq!(b.overflow, 1);
    b.push(mk(EventKind::Login, 1000));
    assert_eq!(b.items.len(), STATUS_EVENT_CAP);
    assert_eq!(b.items.last().unwrap().kind, EventKind::Login);
    assert_eq!(b.overflow, 2);
}

#[tokio::test]
async fn open_and_stop_interleaved_end_closed() {
    let e = std::sync::Arc::new(env());
    let host = std::sync::Arc::new(FakeHost::new());
    *host.ssh_start_delay.lock().unwrap() = Duration::from_millis(200);
    let lock = std::sync::Arc::new(tokio::sync::Mutex::new(MaintState::new()));

    let (l1, h1, e1) = (lock.clone(), host.clone(), e.clone());
    let opener = tokio::spawn(async move {
        let mut s = l1.lock().await;
        open(&mut s, &*h1, &e1.cfg, &key_wire(), 600, "w-1").await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (l2, h2, e2) = (lock.clone(), host.clone(), e.clone());
    let stopper = tokio::spawn(async move {
        let mut s = l2.lock().await;
        close(&mut s, &*h2, &e2.cfg, CloseTrigger::Stop).await
    });
    assert!(opener.await.unwrap().is_ok());
    assert!(stopper.await.unwrap().success);
    assert!(!e.key_path().exists(), "the stop queued behind the open and closed it");
    assert_eq!(e.audit_events(), ["open", "close"]);
}

#[tokio::test]
async fn two_concurrent_opens_give_one_window_and_one_busy() {
    let e = std::sync::Arc::new(env());
    let host = std::sync::Arc::new(FakeHost::new());
    *host.ssh_start_delay.lock().unwrap() = Duration::from_millis(100);
    let lock = std::sync::Arc::new(tokio::sync::Mutex::new(MaintState::new()));
    let mut handles = Vec::new();
    for id in ["a", "b"] {
        let (l, h, e) = (lock.clone(), host.clone(), e.clone());
        handles.push(tokio::spawn(async move {
            let mut s = l.lock().await;
            open(&mut s, &*h, &e.cfg, &key_wire(), 600, id).await
        }));
    }
    // Status at the same time sees either nothing or the whole window.
    let (l, h, e3) = (lock.clone(), host.clone(), e.clone());
    let st = tokio::spawn(async move {
        let mut s = l.lock().await;
        status(&mut s, &*h, &e3.cfg).await
    });
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let busy = results.iter().filter(|r| matches!(r, Err(e) if e.kind == "busy")).count();
    assert_eq!((ok, busy), (1, 1));
    assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 1);
    let s: Value = serde_json::from_str(&st.await.unwrap().unwrap().stdout).unwrap();
    assert_eq!(s["key_installed"] == true, s["window_id"].is_string());
}

#[test]
fn the_legacy_start_is_refused_once_the_os_is_ready() {
    let t = tempfile::tempdir().unwrap();
    let marker = t.path().join("maint-ssh.ready");
    assert!(legacy_start_allowed(&marker).is_ok(), "older images keep the keyless start");
    std::fs::write(&marker, b"1\n").unwrap();
    assert_eq!(legacy_start_allowed(&marker).unwrap_err().kind, "unsupported");
    std::fs::write(&marker, b"garbage").unwrap();
    assert_eq!(legacy_start_allowed(&marker).unwrap_err().kind, "unsupported");
    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    assert_eq!(legacy_start_allowed(&marker).unwrap_err().kind, "unsupported");
}

#[test]
fn window_ids_and_ttls_are_bounded() {
    assert!(validate_window_id("0b9c7d2e-1f3a-4b5c-8d9e-0f1a2b3c4d5e").is_ok());
    for bad in ["", "a b", "a_b", "a\n", "ａ", &"a".repeat(65)] {
        assert!(validate_window_id(bad).is_err(), "{bad:?}");
    }
    assert!(validate_ttl(60).is_ok());
    assert!(validate_ttl(86_400).is_ok());
    assert!(validate_ttl(59).is_err());
    assert!(validate_ttl(86_401).is_err());
}

#[tokio::test]
async fn a_close_that_leaves_the_timer_running_is_retried_by_the_tick() {
    struct StuckTimer(FakeHost, AtomicBool);
    impl MaintHost for StuckTimer {
        async fn run(&self, argv: &[String]) -> Result<CmdOut, String> {
            let line = argv.join(" ");
            if self.1.load(Ordering::SeqCst) && line == "systemctl stop duduclaw-maint-ssh-expire.timer" {
                return FakeHost::fail("stop job failed");
            }
            self.0.run(argv).await
        }
        async fn run_lines(&self, argv: &[String], max: usize) -> Result<Lines, String> { self.0.run_lines(argv, max).await }
        async fn ssh_start(&self) -> DispatchResult { self.0.ssh_start().await }
        async fn ssh_stop(&self) -> DispatchResult { self.0.ssh_stop().await }
        fn boottime(&self) -> Result<Duration, String> { self.0.boottime() }
        fn realtime(&self) -> Result<Duration, String> { self.0.realtime() }
        fn user_ids(&self, n: &str) -> Option<(u32, u32)> { self.0.user_ids(n) }
        fn group_id(&self, n: &str) -> Option<u32> { self.0.group_id(n) }
        fn trusted_exe(&self, pin: &host::ExePin) -> Result<String, String> { self.0.trusted_exe(pin) }
        fn boot_id(&self) -> Result<String, String> { self.0.boot_id() }
    }
    let e = env();
    let host = StuckTimer(FakeHost::new(), AtomicBool::new(false));
    let mut state = MaintState::new();
    open(&mut state, &host, &e.cfg, &key_wire(), 600, "w-1").await.unwrap();
    host.1.store(true, Ordering::SeqCst);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(!out.success, "the timer is still active, so the close is not clean");
    assert!(!e.key_path().exists(), "the key is gone regardless");
    // A new open is refused while the cleanup is pending.
    assert_eq!(open(&mut state, &host, &e.cfg, &key_wire(), 600, "w-2").await.unwrap_err().kind, "busy");
    host.1.store(false, Ordering::SeqCst);
    tick(&mut state, &host, &e.cfg).await;
    assert!(!host.0.timer_active.load(Ordering::SeqCst));
    open(&mut state, &host, &e.cfg, &key_wire(), 600, "w-2").await.expect("open after the retry");
}

#[tokio::test]
async fn a_held_audit_lock_fails_the_open_and_rolls_back_without_hanging() {
    let mut e = env();
    e.cfg.audit.lock_timeout = Duration::from_millis(200);
    let host = FakeHost::new();
    let held = crate::maint_ssh::audit::tests::hold_lock(&e.cfg.audit);
    let mut state = MaintState::new();
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(Duration::from_secs(20), open_default(&mut state, &host, &e))
        .await
        .expect("open must not hang on the audit lock")
        .unwrap_err();
    assert!(err.message.contains("held by another process"), "{}", err.message);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!e.key_path().exists());
    assert!(!e.window_path().exists());
    assert!(!host.timer_active.load(Ordering::SeqCst));
    assert!(!host.ssh_running.load(Ordering::SeqCst));
    assert_eq!(host.ssh_start_calls.load(Ordering::SeqCst), 0, "SSH never starts before the open record is written");
    assert!(state.window().is_none());
    drop(held);
    // The undone open left nothing that blocks the next one.
    open_default(&mut state, &host, &e).await.expect("open once the lock is free");
}

#[tokio::test]
async fn a_close_whose_events_cannot_be_read_keeps_them_for_later() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 42, LOGIN);
    host.fail_journal.store(true, Ordering::SeqCst);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(!out.success);
    assert!(!e.key_path().exists(), "the key goes regardless");
    assert_eq!(e.audit_events(), ["open"], "close record held back behind the missing events");
    assert!(e.cfg.state_dir.join(PENDING_FILE).exists());
    // An open waits for the owed audit work.
    assert_eq!(open(&mut state, &host, &e.cfg, &key_wire(), 600, "w-2").await.unwrap_err().kind, "busy");

    // A restarted sysd picks the work up from the file.
    host.fail_journal.store(false, Ordering::SeqCst);
    let mut restarted = MaintState::new();
    startup_cleanup(&mut restarted, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["open", "login", "close"]);
    let rec = e.audit().pop().unwrap();
    assert_eq!(rec["events_complete"], true);
    assert_eq!(rec["logins_recorded"], 1);
    assert_eq!(rec["last_cursor"], "c1");
    assert!(!e.cfg.state_dir.join(PENDING_FILE).exists());
    tick(&mut restarted, &host, &e.cfg).await;
    open(&mut restarted, &host, &e.cfg, &key_wire(), 600, "w-2").await.expect("open after the backfill");
}

#[tokio::test]
async fn a_close_record_that_cannot_be_written_is_retried() {
    let mut e = env();
    e.cfg.audit.lock_timeout = Duration::from_millis(100);
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    let held = crate::maint_ssh::audit::tests::hold_lock(&e.cfg.audit);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(!out.success);
    assert!(out.stderr.contains("close record kept"), "{}", out.stderr);
    assert_eq!(e.audit_events(), ["open"]);
    drop(held);
    tick(&mut state, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["open", "close"]);
    assert_eq!(e.audit().pop().unwrap()["last_cursor"], "c0");
}

#[tokio::test]
async fn opens_are_rate_limited() {
    let mut e = env();
    e.cfg.open_min_interval = Duration::from_secs(5);
    e.cfg.open_max_per_hour = 2;
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    assert!(close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await.success);
    assert_eq!(open_default(&mut state, &host, &e).await.unwrap_err().kind, "rate_limited");
    *host.boot.lock().unwrap() += Duration::from_secs(6);
    open_default(&mut state, &host, &e).await.unwrap();
    assert!(close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await.success);
    *host.boot.lock().unwrap() += Duration::from_secs(60);
    assert_eq!(open_default(&mut state, &host, &e).await.unwrap_err().kind, "rate_limited", "two per hour");
    *host.boot.lock().unwrap() += Duration::from_secs(3600);
    open_default(&mut state, &host, &e).await.expect("the hour has passed");
    // Requests refused before the rate limit (bad key) do not use it up.
    assert!(e.audit_events().iter().filter(|k| *k == "open").count() == 3);
}

#[tokio::test]
async fn a_reboot_with_the_window_open_is_closed_and_replayed_at_the_next_boot() {
    let e = env();
    let host = FakeHost::new();
    {
        let mut before = MaintState::new();
        open_default(&mut before, &host, &e).await.unwrap();
        // Log in and reboot before any tick ran: nothing was written yet.
        host.push_journal(DROPBEAR_SLICE, 0, "dropbear", 9, LOGIN);
        assert_eq!(e.audit_events(), ["open"]);
    }
    // The reboot: tmpfs (key directory) and the transient timer are gone.
    std::fs::remove_dir_all(&e.cfg.key_dir).unwrap();
    host.timer_active.store(false, Ordering::SeqCst);
    *host.boot_id.lock().unwrap() = "11111111-2222-4333-8444-555555555555".to_string();
    let mut after = MaintState::new();
    startup_cleanup(&mut after, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["open", "login", "close"]);
    let rec = e.audit().pop().unwrap();
    assert_eq!(rec["trigger"], "reboot");
    assert_eq!(rec["window_id"], "w-1");
    assert_eq!(rec["logins_recorded"], 1);
    assert_eq!(rec["events_complete"], true);
    assert!(!e.window_path().exists());
}

#[tokio::test]
async fn a_cursor_that_cannot_be_replayed_is_given_up_with_events_complete_false() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    host.fail_journal.store(true, Ordering::SeqCst);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(!out.success);
    for _ in 0..MAX_BACKFILL_ATTEMPTS {
        tick(&mut state, &host, &e.cfg).await;
    }
    assert_eq!(e.audit_events(), ["open", "close"]);
    let rec = e.audit().pop().unwrap();
    assert_eq!(rec["events_complete"], false);
    assert_eq!(rec["events_lost"], true);
    assert!(!e.cfg.state_dir.join(PENDING_FILE).exists());
}

#[tokio::test]
async fn the_tick_closes_the_window_when_the_egress_rule_disappears() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    tick(&mut state, &host, &e.cfg).await;
    assert!(e.key_path().exists());
    // `nftables` reload without the maint drop-in: the chain is empty.
    *host.egress_chain.lock().unwrap() =
        r#"{"nftables":[{"chain":{"family":"inet","table":"filter","name":"duduclaw_maint_egress","handle":9}}]}"#.to_string();
    tick(&mut state, &host, &e.cfg).await;
    assert!(!e.key_path().exists());
    assert_eq!(e.audit().pop().unwrap()["trigger"], "firewall_changed");
}

#[tokio::test]
async fn an_unreadable_backlog_is_recorded_as_lost() {
    let e = env();
    let host = FakeHost::new();
    std::fs::create_dir_all(&e.cfg.state_dir).unwrap();
    std::fs::set_permissions(&e.cfg.state_dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let p = e.cfg.state_dir.join(PENDING_FILE);
    std::fs::write(&p, b"{not json").unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let mut state = MaintState::new();
    startup_cleanup(&mut state, &host, &e.cfg).await;
    assert_eq!(e.audit_events(), ["audit_backlog_lost"]);
    assert!(!p.exists());
}

#[tokio::test]
async fn logouts_written_while_dropbear_stops_are_still_recorded() {
    struct LogoutOnStop(FakeHost);
    impl MaintHost for LogoutOnStop {
        async fn run(&self, argv: &[String]) -> Result<CmdOut, String> { self.0.run(argv).await }
        async fn run_lines(&self, argv: &[String], max: usize) -> Result<Lines, String> { self.0.run_lines(argv, max).await }
        async fn ssh_start(&self) -> DispatchResult { self.0.ssh_start().await }
        async fn ssh_stop(&self) -> DispatchResult {
            // The killed session logs its exit as dropbear goes down.
            self.0.push_journal(DROPBEAR_SLICE, 4800, "dropbear", 42, LOGOUT);
            self.0.ssh_stop().await
        }
        fn boottime(&self) -> Result<Duration, String> { self.0.boottime() }
        fn realtime(&self) -> Result<Duration, String> { Ok(Duration::from_secs(1_760_600_001)) }
        fn user_ids(&self, n: &str) -> Option<(u32, u32)> { self.0.user_ids(n) }
        fn group_id(&self, n: &str) -> Option<u32> { self.0.group_id(n) }
        fn trusted_exe(&self, pin: &host::ExePin) -> Result<String, String> { self.0.trusted_exe(pin) }
        fn boot_id(&self) -> Result<String, String> { self.0.boot_id() }
    }
    let e = env();
    let host = LogoutOnStop(FakeHost::new());
    let mut state = MaintState::new();
    open(&mut state, &host, &e.cfg, &key_wire(), 600, "w-1").await.unwrap();
    host.0.push_journal(DROPBEAR_SLICE, 0, "dropbear", 42, LOGIN);
    let out = close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await;
    assert!(out.success, "{}", out.stderr);
    assert_eq!(e.audit_events(), ["open", "login", "logout", "close"]);
}

async fn status_json(state: &mut MaintState, host: &FakeHost, e: &Env) -> Value {
    serde_json::from_str(&status(state, host, &e.cfg).await.unwrap().stdout).unwrap()
}

#[tokio::test]
async fn status_reports_the_last_close() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    assert_eq!(status_json(&mut state, &host, &e).await["last_close"], Value::Null);
    open_default(&mut state, &host, &e).await.unwrap();
    assert!(close(&mut state, &host, &e.cfg, CloseTrigger::Stop).await.success);
    let s = status_json(&mut state, &host, &e).await;
    assert_eq!(
        s["last_close"],
        serde_json::json!({"window_id":"w-1","trigger":"stop","events_complete":true,"events_lost":false,"ts":1_760_600_000u64})
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(e.cfg.state_dir.join(LAST_CLOSE_FILE)).unwrap().mode() & 0o777, 0o600);
    assert_eq!(s["audit_backlog_lost"], Value::Null);
}

#[tokio::test]
async fn last_close_follows_a_held_back_close_record() {
    let e = env();
    let host = FakeHost::new();
    let mut state = MaintState::new();
    open_default(&mut state, &host, &e).await.unwrap();
    host.fail_journal.store(true, Ordering::SeqCst);
    close(&mut state, &host, &e.cfg, CloseTrigger::Deadline).await;
    host.fail_journal.store(false, Ordering::SeqCst);
    let s = status_json(&mut state, &host, &e).await;
    assert_eq!(s["last_close"]["trigger"], "deadline");
    assert_eq!(s["last_close"]["events_complete"], false);
    tick(&mut state, &host, &e.cfg).await;
    let s = status_json(&mut state, &host, &e).await;
    assert_eq!(s["last_close"]["events_complete"], true);
    assert_eq!(s["last_close"]["events_lost"], false);
}

#[tokio::test]
async fn a_fallback_timer_close_keeps_its_reason_through_the_startup_cleanup() {
    let e = env();
    let host = FakeHost::new();
    let mut before = MaintState::new();
    open_default(&mut before, &host, &e).await.unwrap();
    // The OS script ran: key gone, its own last-close written.
    std::fs::remove_file(e.key_path()).unwrap();
    let p = e.cfg.state_dir.join(LAST_CLOSE_FILE);
    std::fs::write(&p, br#"{"window_id":null,"trigger":"fallback_timer","events_complete":false,"events_lost":false,"ts":1760603700}"#).unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let mut after = MaintState::new();
    startup_cleanup(&mut after, &host, &e.cfg).await;
    let s = status_json(&mut after, &host, &e).await;
    assert_eq!(s["last_close"]["trigger"], "fallback_timer");
    assert_eq!(s["last_close"]["window_id"], "w-1");
    assert_eq!(s["last_close"]["events_complete"], true);
}

#[tokio::test]
async fn a_bad_last_close_file_is_not_reported() {
    let e = env();
    let host = FakeHost::new();
    std::fs::create_dir_all(&e.cfg.state_dir).unwrap();
    std::fs::set_permissions(&e.cfg.state_dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let p = e.cfg.state_dir.join(LAST_CLOSE_FILE);
    std::fs::write(&p, br#"{"window_id":"w x","trigger":"<script>","events_complete":true,"events_lost":false,"ts":1}"#).unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let mut state = MaintState::new();
    let s = status_json(&mut state, &host, &e).await;
    assert_eq!(s["last_close"], Value::Null);
    assert!(s["problems"][0].as_str().unwrap().contains("unknown trigger"));
    // A known trigger with a malformed window id: the id is dropped.
    std::fs::write(&p, br#"{"window_id":"w x","trigger":"stop","events_complete":true,"events_lost":false,"ts":1}"#).unwrap();
    let s = status_json(&mut state, &host, &e).await;
    assert_eq!(s["last_close"]["window_id"], Value::Null);
    assert_eq!(s["last_close"]["trigger"], "stop");
    // World-readable files are refused.
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    assert_eq!(status_json(&mut state, &host, &e).await["last_close"], Value::Null);
}

#[tokio::test]
async fn status_reports_a_backlog_lost_in_this_boot_only() {
    let e = env();
    let host = FakeHost::new();
    std::fs::create_dir_all(&e.cfg.state_dir).unwrap();
    std::fs::set_permissions(&e.cfg.state_dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let p = e.cfg.state_dir.join(PENDING_FILE);
    std::fs::write(&p, b"{broken").unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let mut state = MaintState::new();
    startup_cleanup(&mut state, &host, &e.cfg).await;
    let s = status_json(&mut state, &host, &e).await;
    assert_eq!(s["audit_backlog_lost"], serde_json::json!({"ts": 1_760_600_000u64}));
    *host.boot_id.lock().unwrap() = "11111111-2222-4333-8444-555555555555".to_string();
    assert_eq!(status_json(&mut state, &host, &e).await["audit_backlog_lost"], Value::Null);
}
