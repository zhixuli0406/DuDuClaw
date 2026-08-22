// WP-C-M2 — local gateway sidecar manager.
//
// Ported from the Tauri shell's `src-tauri/src/{sidecar,lifecycle}.rs`
// design (spawn with an augmented PATH, pidfile, orphan reclaim, health
// poll, backoff restart) but REWRITTEN, not copied: that version is built
// on `tauri_plugin_shell::process::{CommandChild, CommandEvent}` and
// `tauri::async_runtime`, neither of which this crate can depend on (it is
// deliberately detached from Tauri — see this crate's own Cargo.toml
// comment). This version uses plain `std::process::Command` + `std::thread`
// instead: sidecar management here is a handful of low-frequency
// housekeeping tasks (poll a port, poll `try_wait`, sleep-then-retry), none
// of which need tokio's reactor — pulling in tokio's `process` feature (not
// currently enabled — see Cargo.toml) just to avoid three `std::thread::
// spawn` calls would be the actual yak-shave.
//
// Scope, per the WP-C-M2 task brief: "mDNS 若無依賴不硬加" — this module has
// no LAN discovery at all (see `screens/gateway_picker.rs`'s honest empty
// state for that). It owns exactly one thing: a same-machine `duduclaw run`
// child process.
//
// Port/plan resolution, PATH augmentation, and `duduclaw` binary discovery
// live in the sibling `sidecar_target` module (split out purely to keep
// both files under this project's 800-line convention — see that module's
// own header comment).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::sidecar_target::{
    augmented_path, is_listening, known_ports, plan_gateway, sidecar_pidfile, which_duduclaw, GatewayPlan,
    DEFAULT_HOST, DEFAULT_PORT,
};

/// How long a freshly-spawned gateway may take to bind its port before this
/// manager gives up and surfaces `Error` (cold start loads channel bots +
/// DB migrations).
const READY_TIMEOUT: Duration = Duration::from_secs(45);
/// How often the readiness/liveness watcher threads poll.
const POLL_INTERVAL: Duration = Duration::from_millis(300);
const MAX_RESTART_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarStatus {
    Stopped,
    /// Spawned but not yet answering on its port — never reported as
    /// `Running` until a real TCP connect succeeds (the picker's "本機" card
    /// must never claim 運行中 for a gateway that still refuses connects).
    Starting,
    Running,
    Error,
}

pub struct SidecarManager {
    child: Mutex<Option<Child>>,
    status: Mutex<SidecarStatus>,
    port: Mutex<u16>,
    /// Set when the app is intentionally quitting / switching away —
    /// suppresses auto-restart so a deliberate `stop()` doesn't immediately
    /// respawn itself via the crash-recovery path.
    shutting_down: AtomicBool,
    /// True when this manager attached to an externally-managed gateway
    /// rather than spawning its own — `stop()` never kills those.
    attached: AtomicBool,
    /// Bumped on every spawn/stop/reset so a superseded child's readiness/
    /// exit-watcher threads can detect they're stale and must not touch
    /// shared state (prevents a killed child's exit event from clobbering
    /// the replacement's status or double-restarting).
    generation: AtomicU64,
}

impl SidecarManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            child: Mutex::new(None),
            status: Mutex::new(SidecarStatus::Stopped),
            port: Mutex::new(DEFAULT_PORT),
            shutting_down: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        })
    }

    pub fn status(&self) -> SidecarStatus {
        *self.status.lock().unwrap()
    }

    pub fn port(&self) -> u16 {
        *self.port.lock().unwrap()
    }

    /// True when this manager attached to a gateway it did not spawn
    /// (`stop()` is then a no-op on the process — see that method). No UI
    /// consumer yet (`screens::gateway_picker`'s "本機" card doesn't
    /// currently distinguish "attached" from "spawned-and-running" in its
    /// status label) — kept as public API surface for whichever page adds
    /// that nuance next, same as this crate's established `#[allow(dead_
    /// code)]`-with-rationale convention (see e.g. `api::AuthUser`'s
    /// fields).
    #[allow(dead_code)]
    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
    }

    fn set_status(&self, s: SidecarStatus) {
        *self.status.lock().unwrap() = s;
    }

    /// Reclaim a sidecar orphaned by a previous crash: if the pidfile
    /// points at a live process that is STILL identifiably a DuDuClaw
    /// binary, kill it so a fresh start doesn't race it for the port.
    /// Identity is verified before any signal is sent — a pidfile can
    /// outlive its process and the OS recycles PIDs, so a stale file may
    /// point at an unrelated process; killing that would be someone else's
    /// data loss. On a mismatch this only clears the pidfile and warns.
    fn reclaim_orphan(&self) {
        let pidfile = sidecar_pidfile();
        if let Ok(contents) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                let probe = probe_pid_identity(pid).unwrap_or_default();
                if probe.trim().is_empty() {
                    eprintln!("[sidecar] stale pidfile pid={pid}: process already gone");
                } else if !pid_identity_matches(&probe) {
                    eprintln!(
                        "[sidecar] pidfile pid={pid} now belongs to an unrelated process ({}) \
                         — refusing to kill it, clearing the stale pidfile only",
                        probe.trim()
                    );
                } else {
                    kill_pid_and_wait(pid);
                    eprintln!("[sidecar] reclaimed orphaned sidecar pid={pid}");
                }
            }
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    fn write_pidfile(&self, pid: u32) {
        let pidfile = sidecar_pidfile();
        if let Some(parent) = pidfile.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pidfile, pid.to_string());
    }

    fn clear_pidfile(&self) {
        let _ = std::fs::remove_file(sidecar_pidfile());
    }

    /// Plan + start the local gateway: attach to one already running, or
    /// spawn the `duduclaw run` sidecar on a free port. Idempotent-ish: a
    /// second call while `Running` (re-verified live) or `Starting` is a
    /// no-op.
    pub fn start(self: &Arc<Self>, preferred_port: u16) -> Result<u16, String> {
        self.shutting_down.store(false, Ordering::SeqCst);
        match self.status() {
            SidecarStatus::Starting => return Ok(self.port()),
            SidecarStatus::Running => {
                // Trust but verify: an ATTACHED gateway can die without any
                // event ever reaching this manager (it holds no child
                // handle for it).
                if is_listening(DEFAULT_HOST, self.port()) {
                    return Ok(self.port());
                }
                eprintln!("[sidecar] gateway on port {} no longer answers — re-planning", self.port());
                self.reset_dead();
            }
            SidecarStatus::Stopped | SidecarStatus::Error => self.reset_dead(),
        }
        self.reclaim_orphan();

        match plan_gateway(&known_ports(), preferred_port) {
            GatewayPlan::Attach { port } => {
                eprintln!("[sidecar] attaching to existing gateway on port {port}");
                *self.port.lock().unwrap() = port;
                self.attached.store(true, Ordering::SeqCst);
                self.set_status(SidecarStatus::Running);
                Ok(port)
            }
            GatewayPlan::Spawn { port } => self.spawn_sidecar(port),
        }
    }

    /// Discard whatever this manager was tracking before re-planning: kill
    /// a spawned child no longer trusted (so a re-spawn can't race it for
    /// the port) and bump the generation so its pending exit event is
    /// ignored. An attached gateway is never killed — only forgotten.
    fn reset_dead(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if !self.attached.load(Ordering::SeqCst) {
            if let Some(mut child) = self.child.lock().unwrap().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.attached.store(false, Ordering::SeqCst);
        self.set_status(SidecarStatus::Stopped);
    }

    fn spawn_sidecar(self: &Arc<Self>, port: u16) -> Result<u16, String> {
        let bin = which_duduclaw().ok_or_else(|| {
            "duduclaw binary not found on PATH or in any known install location".to_string()
        })?;

        let mut cmd = Command::new(&bin);
        // `run --yes`: start the full server (gateway + dashboard +
        // channels) non-interactively — matches the CLI's own `duduclaw run
        // --yes` entry point (there is no separate `start` subcommand).
        cmd.args(["run", "--yes"])
            .env("DUDUCLAW_PORT", port.to_string())
            .env("PATH", augmented_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("spawn '{bin}' failed: {e}"))?;
        let pid = child.id();
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        // Drain stdout/stderr on their own threads so the child's pipe
        // buffers can never fill up and stall it — these threads simply
        // end when the pipe closes (child exits or is killed), no
        // generation check needed (forwarding a stray line after
        // supersession is harmless, unlike touching shared status).
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    eprintln!("[sidecar:stdout] {line}");
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("[sidecar:stderr] {line}");
                }
            });
        }

        self.write_pidfile(pid);
        *self.port.lock().unwrap() = port;
        *self.child.lock().unwrap() = Some(child);
        self.attached.store(false, Ordering::SeqCst);
        self.set_status(SidecarStatus::Starting);
        eprintln!("[sidecar] spawned duduclaw run pid={pid} port={port} — waiting for readiness");

        // Readiness watcher: Starting -> Running only once the port
        // actually answers.
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let deadline = Instant::now() + READY_TIMEOUT;
            loop {
                if me.generation.load(Ordering::SeqCst) != my_gen || me.status() != SidecarStatus::Starting {
                    return; // superseded, or already resolved elsewhere (e.g. crashed -> Error)
                }
                if is_listening(DEFAULT_HOST, port) {
                    if me.generation.load(Ordering::SeqCst) == my_gen {
                        me.set_status(SidecarStatus::Running);
                        eprintln!("[sidecar] ready on port {port}");
                    }
                    return;
                }
                if Instant::now() >= deadline {
                    eprintln!("[sidecar] did not answer on port {port} within {}s", READY_TIMEOUT.as_secs());
                    me.set_status(SidecarStatus::Error);
                    return;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        });

        // Liveness watcher: non-blocking `try_wait()` poll rather than a
        // blocking `wait()` — a blocking wait would have to hold `child`
        // for the process's entire lifetime, starving `stop()`'s own
        // `.take()` + `.kill()` of the same mutex.
        let me = Arc::clone(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(POLL_INTERVAL);
            if me.generation.load(Ordering::SeqCst) != my_gen {
                return; // superseded — the replacement owns state now
            }
            let exited = {
                let mut guard = me.child.lock().unwrap();
                match guard.as_mut() {
                    Some(c) => match c.try_wait() {
                        Ok(Some(status)) => Some(status),
                        Ok(None) => None,
                        Err(e) => {
                            eprintln!("[sidecar] try_wait error, treating as exited: {e}");
                            Some(std::process::ExitStatus::default())
                        }
                    },
                    None => return, // already taken by stop()/reset_dead()
                }
            };
            let Some(status) = exited else { continue };
            if me.generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            *me.child.lock().unwrap() = None;
            me.clear_pidfile();
            if me.shutting_down.load(Ordering::SeqCst) {
                me.set_status(SidecarStatus::Stopped);
            } else {
                eprintln!("[sidecar] exited unexpectedly: {status:?}");
                me.set_status(SidecarStatus::Error);
                me.restart_with_backoff(port, my_gen);
            }
            return;
        });

        Ok(port)
    }

    /// Restart after an unexpected exit, exponential backoff, hard cap so a
    /// crash loop trips into `Error` instead of hammering.
    /// `failed_gen` is the generation that died — if a fresh `start()` (user
    /// action, gateway picker) already spawned a replacement, this must
    /// stand down rather than fight it for the port.
    fn restart_with_backoff(self: &Arc<Self>, port: u16, failed_gen: u64) {
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            for attempt in 1..=MAX_RESTART_ATTEMPTS {
                if me.shutting_down.load(Ordering::SeqCst) || me.generation.load(Ordering::SeqCst) != failed_gen {
                    return;
                }
                let delay = Duration::from_millis(500u64.saturating_mul(1u64 << (attempt - 1)));
                std::thread::sleep(delay);
                eprintln!("[sidecar] restart attempt {attempt}/{MAX_RESTART_ATTEMPTS}");
                if me.spawn_sidecar(port).is_ok() {
                    return;
                }
            }
            eprintln!("[sidecar] failed to recover after {MAX_RESTART_ATTEMPTS} attempts");
            me.set_status(SidecarStatus::Error);
        });
    }

    /// Stop the sidecar (app quit, or the picker switching to a remote
    /// gateway). An attached gateway (one this manager did not spawn) is
    /// never killed — only forgotten, per the task brief's "非自己 spawn 的
    /// 不動".
    pub fn stop(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        // Supersede any in-flight readiness/liveness watcher so a delayed
        // exit event from the OLD child can't clobber a later start()'s
        // fresh state.
        self.generation.fetch_add(1, Ordering::SeqCst);
        if self.attached.load(Ordering::SeqCst) {
            self.set_status(SidecarStatus::Stopped);
            return;
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait(); // reap — kill() alone can leave a zombie
        }
        self.clear_pidfile();
        self.set_status(SidecarStatus::Stopped);
    }
}

// ── Orphan identity verification ─────────────────────────────────────────

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
fn kill_pid_and_wait(pid: u32) {
    unsafe {
        libc_kill(pid as i32, 15); // SIGTERM
        let deadline = Instant::now() + Duration::from_secs(5);
        while libc_kill(pid as i32, 0) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(150));
        }
        if libc_kill(pid as i32, 0) == 0 {
            libc_kill(pid as i32, 9); // refused to die gracefully — force it
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

#[cfg(windows)]
fn kill_pid_and_wait(pid: u32) {
    // `/F` is forceful; the port is released as soon as it returns.
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
}

#[cfg(not(any(unix, windows)))]
fn kill_pid_and_wait(_pid: u32) {}

/// Ask the OS what process currently owns `pid`. `None` means the probe
/// itself could not run; an empty string means no such process.
fn probe_pid_identity(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

/// Does the process-name probe output identify a DuDuClaw binary?
/// Fail-closed: anything not positively identified (empty output, the
/// Windows "no tasks" notice, an unrelated binary name) returns `false`, so
/// the caller never signals a process it did not spawn.
fn pid_identity_matches(probe_output: &str) -> bool {
    probe_output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with("INFO:"))
        .any(|l| l.to_ascii_lowercase().contains("duduclaw"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_identity_matches_unix_and_windows_shapes() {
        assert!(pid_identity_matches("duduclaw\n"));
        assert!(pid_identity_matches("/usr/local/bin/duduclaw\n"));
        assert!(pid_identity_matches("\"duduclaw.exe\",\"4242\",\"Console\",\"1\",\"81,234 K\"\n"));
    }

    #[test]
    fn pid_identity_rejects_recycled_pid_owned_by_another_process() {
        // The exact scenario this guard exists for: the pidfile survived,
        // the OS handed the PID to something else. Killing these would be
        // someone else's data loss.
        assert!(!pid_identity_matches("Code Helper\n"));
        assert!(!pid_identity_matches("/usr/bin/ssh\n"));
        assert!(!pid_identity_matches(""));
        assert!(!pid_identity_matches("INFO: No tasks are running which match the specified criteria.\n"));
    }

    /// Live-ish: spawn a real short-lived child (`sleep 1` — always present
    /// on the CI/dev Unix box this crate builds on) through `SidecarManager`
    /// internals is out of scope for a unit test (that needs the real
    /// `duduclaw` binary — see this task's own "活體" verification instead).
    /// What IS unit-testable without a real gateway: a freshly constructed
    /// manager starts `Stopped`, unattached, on the default port.
    #[test]
    fn fresh_manager_starts_stopped_and_unattached() {
        let mgr = SidecarManager::new();
        assert_eq!(mgr.status(), SidecarStatus::Stopped);
        assert!(!mgr.is_attached());
        assert_eq!(mgr.port(), DEFAULT_PORT);
    }

    /// `stop()` on a manager that never started anything must be a safe
    /// no-op (no child to kill, no pidfile it owns).
    #[test]
    fn stop_before_start_is_a_safe_no_op() {
        let mgr = SidecarManager::new();
        mgr.stop();
        assert_eq!(mgr.status(), SidecarStatus::Stopped);
    }

    /// Live, end-to-end `SidecarManager` lifecycle: spawn -> Starting ->
    /// Running (readiness watcher actually observes the port opening) ->
    /// stop -> Stopped (liveness watcher / `stop()` actually tears the
    /// process down + pidfile cleared). Deliberately does NOT touch the
    /// real `duduclaw` binary or the default port 18789 — this dev
    /// machine has a real gateway already running there (see this task's
    /// own verification report), and spawning a second instance or racing
    /// it for the port would be exactly the double-instance hazard this
    /// module exists to prevent. Instead: `DUDUCLAW_BIN` is pointed at a
    /// tiny stand-in script (`python3 -m http.server`, present on every
    /// macOS/most Linux dev boxes) that binds whatever port it's told —
    /// enough to exercise the REAL process-spawn/health-poll/kill/pidfile
    /// mechanics this file owns, without needing the actual multi-second
    /// gateway cold-start or touching anything the user's real session
    /// depends on. `#[ignore]`d (not part of the default `cargo test` run):
    /// it spawns a real subprocess and takes a few real seconds.
    #[test]
    #[ignore = "spawns a real subprocess; run explicitly with `cargo test -- --ignored`"]
    fn live_spawn_ready_stop_lifecycle_against_a_stub_binary() {
        let home = std::env::temp_dir().join(format!("ddc-ng-sidecar-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        let stub = home.join("fake-duduclaw.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\nexec python3 -m http.server \"$DUDUCLAW_PORT\" --bind 127.0.0.1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let prev_home = std::env::var("DUDUCLAW_HOME").ok();
        let prev_bin = std::env::var("DUDUCLAW_BIN").ok();
        unsafe {
            std::env::set_var("DUDUCLAW_HOME", &home);
            std::env::set_var("DUDUCLAW_BIN", &stub);
        }

        // A port unlikely to collide with anything else already listening
        // on this dev machine (well clear of both 18789 and its ±8 fallback
        // band).
        let port = 28900u16 + (std::process::id() % 500) as u16;

        let mgr = SidecarManager::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Calls `spawn_sidecar` directly, NOT `start()`: `start()` runs
            // the full attach-vs-spawn plan over `known_ports()`, which
            // ALWAYS includes the real default port 18789 (by design — see
            // `known_ports_from`'s doc comment) — on THIS dev machine a
            // real gateway is listening there, so `start()` would correctly
            // ATTACH to it instead of spawning the stub (verified: an
            // earlier draft of this test called `start()` and observed
            // exactly that attach, port 18789, not the requested stub
            // port). `spawn_sidecar` is the actual unit under test here —
            // process spawn/readiness/pidfile/kill mechanics — independent
            // of that (separately unit-tested) planning decision.
            let started_port =
                mgr.spawn_sidecar(port).expect("spawn_sidecar should succeed with a valid stub binary");
            assert_eq!(started_port, port);

            let deadline = Instant::now() + Duration::from_secs(10);
            while mgr.status() == SidecarStatus::Starting && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(mgr.status(), SidecarStatus::Running, "stub server never became reachable in time");
            assert!(is_listening(DEFAULT_HOST, port));
            assert!(sidecar_pidfile().exists(), "pidfile should exist while the sidecar is running");

            mgr.stop();
            assert_eq!(mgr.status(), SidecarStatus::Stopped);

            // The port must actually be released (the process was really
            // killed, not just marked stopped in memory) and the pidfile
            // cleared — poll briefly since OS port release isn't
            // instantaneous.
            let deadline = Instant::now() + Duration::from_secs(5);
            while is_listening(DEFAULT_HOST, port) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(!is_listening(DEFAULT_HOST, port), "port should be released after stop()");
            assert!(!sidecar_pidfile().exists(), "pidfile should be cleared after stop()");
        }));

        match prev_home {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_HOME", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_HOME") },
        }
        match prev_bin {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_BIN", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_BIN") },
        }
        let _ = std::fs::remove_dir_all(&home);

        result.unwrap();
    }
}
