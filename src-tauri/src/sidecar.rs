//! Gateway sidecar manager (TODO-genspark-workspace-shell §D2).
//!
//! Owns the spawned `duduclaw start` process: spawn with an augmented PATH,
//! record a pidfile, reclaim orphans from a previous crashed run, poll health
//! and auto-restart with backoff, and shut down gracefully on exit.
//!
//! NOTE: this targets the Tauri 2 + `tauri-plugin-shell` API. It is written to
//! the documented API but has NOT been compiled in this environment (no Tauri
//! toolchain) — see the TODO doc's Phase D verification notes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

use crate::lifecycle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarStatus {
    Stopped,
    /// Spawned but the gateway has not answered on its port yet. `Running` is
    /// only reported once the port actually accepts connections — the picker's
    /// "本機" card must never say 運行中 for a gateway that refuses connects.
    Starting,
    Running,
    Error,
}

/// How long a freshly-spawned gateway may take to bind its port before we give
/// up and surface Error (cold start loads channels/DB migrations).
const READY_TIMEOUT: Duration = Duration::from_secs(45);

pub struct SidecarManager {
    child: Mutex<Option<CommandChild>>,
    status: Mutex<SidecarStatus>,
    /// The port the gateway is reachable on (spawned or attached).
    port: Mutex<u16>,
    /// Set when the user is intentionally quitting — suppresses auto-restart.
    shutting_down: AtomicBool,
    /// True when we attached to an externally-managed gateway (do not kill it).
    attached: AtomicBool,
    /// Spawn generation. Bumped on every spawn/stop/reset so the event-drain
    /// and readiness tasks of a superseded child can detect they are stale and
    /// must not touch shared state (prevents a killed child's Terminated event
    /// from clobbering the replacement's status or double-restarting).
    generation: AtomicU64,
}

impl SidecarManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            status: Mutex::new(SidecarStatus::Stopped),
            port: Mutex::new(lifecycle::DEFAULT_PORT),
            shutting_down: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    pub fn status(&self) -> SidecarStatus {
        *self.status.lock().unwrap()
    }

    pub fn port(&self) -> u16 {
        *self.port.lock().unwrap()
    }

    fn set_status(&self, s: SidecarStatus) {
        *self.status.lock().unwrap() = s;
    }

    /// Reclaim a sidecar orphaned by a previous crash: if a pidfile points at a
    /// live process **that is still a DuDuClaw gateway**, kill it so we start
    /// from a clean slate (§D2.1 / §D2.3).
    ///
    /// Identity is verified before any signal is sent: a pidfile can outlive its
    /// process (hard crash, power loss) and the OS recycles PIDs, so a stale
    /// file may well point at an unrelated user process — killing that would be
    /// data loss in someone else's editor. On a mismatch we only clear the
    /// pidfile and warn.
    ///
    /// Blocks until the orphan has actually exited (bounded): its listener stays
    /// bound during the gateway's graceful shutdown, and the attach-vs-spawn
    /// port probe that runs right after this would otherwise latch onto the
    /// dying process — reporting 運行中 for a gateway that is seconds from dead.
    fn reclaim_orphan(&self) {
        let pidfile = lifecycle::sidecar_pidfile();
        if let Ok(contents) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                let probe = probe_pid_identity(pid).unwrap_or_default();
                if probe.trim().is_empty() {
                    // No such process — the pidfile is simply stale.
                    tracing::debug!("stale sidecar pidfile pid={pid}: process already gone");
                } else if !pid_identity_matches(&probe) {
                    tracing::warn!(
                        "sidecar pidfile pid={pid} now belongs to an unrelated process ({}) \
                         — refusing to kill it, clearing the stale pidfile only",
                        probe.trim()
                    );
                } else {
                    #[cfg(unix)]
                    unsafe {
                        // SIGTERM; ignore errors (process may already be gone).
                        libc_kill(pid as i32, 15);
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while libc_kill(pid as i32, 0) == 0 && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(150));
                        }
                        if libc_kill(pid as i32, 0) == 0 {
                            // Refused to die gracefully — force it and give the OS a
                            // beat to release the port.
                            libc_kill(pid as i32, 9);
                            std::thread::sleep(Duration::from_millis(300));
                        }
                    }
                    #[cfg(windows)]
                    {
                        // /F is forceful; the port is released as soon as it returns.
                        let _ = std::process::Command::new("taskkill")
                            .args(["/PID", &pid.to_string(), "/F"])
                            .status();
                    }
                    tracing::info!("reclaimed orphaned sidecar pid={pid}");
                }
            }
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    fn write_pidfile(&self, pid: u32) {
        let pidfile = lifecycle::sidecar_pidfile();
        if let Some(parent) = pidfile.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pidfile, pid.to_string());
    }

    fn clear_pidfile(&self) {
        let _ = std::fs::remove_file(lifecycle::sidecar_pidfile());
    }

    /// Plan + start the gateway. Attaches to an already-running gateway (§D1) or
    /// spawns the bundled sidecar on a free port (§D2.2). Idempotent-ish: a
    /// second call while running (verified live) or still starting is a no-op.
    pub fn start(self: &Arc<Self>, app: &AppHandle) -> Result<u16, String> {
        // A previous stop() (remote pick released the sidecar, tray restart)
        // must not leave auto-restart permanently disabled for this fresh start.
        self.shutting_down.store(false, Ordering::SeqCst);
        match self.status() {
            SidecarStatus::Starting => return Ok(self.port()),
            SidecarStatus::Running => {
                // Trust but verify: an *attached* gateway can die without any
                // event ever reaching us (we hold no child handle), leaving a
                // stale Running that used to make this a permanent no-op — the
                // 連線 button could never revive the gateway.
                if lifecycle::is_listening(lifecycle::DEFAULT_HOST, self.port()) {
                    return Ok(self.port());
                }
                tracing::warn!(
                    "gateway on port {} no longer answers — re-planning",
                    self.port()
                );
                self.reset_dead();
            }
            SidecarStatus::Stopped | SidecarStatus::Error => self.reset_dead(),
        }
        self.reclaim_orphan();

        // Honor the operator's mode override (§D1) and probe every known port
        // (env / config.toml / default) before spawning, so a non-default
        // config.toml port can't make us double-spawn over an existing gateway
        // (§D2.2).
        let mode = lifecycle::desktop_mode();
        let known = lifecycle::known_ports();
        let preferred = lifecycle::configured_port();
        let plan = lifecycle::plan_gateway_with(mode, lifecycle::DEFAULT_HOST, &known, preferred);
        match plan {
            lifecycle::GatewayPlan::Attach { port } => {
                tracing::info!("attaching to existing gateway on port {port}");
                *self.port.lock().unwrap() = port;
                self.attached.store(true, Ordering::SeqCst);
                self.set_status(SidecarStatus::Running);
                Ok(port)
            }
            lifecycle::GatewayPlan::Spawn { port } => self.spawn_sidecar(app, port),
        }
    }

    /// Discard whatever we were tracking before re-planning: kill a spawned
    /// child we no longer trust (so a re-spawn can't race it for the port) and
    /// bump the generation so its pending Terminated event is ignored. Attached
    /// gateways are never killed — we only forget them.
    fn reset_dead(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if !self.attached.load(Ordering::SeqCst) {
            if let Some(child) = self.child.lock().unwrap().take() {
                let _ = child.kill();
            }
        }
        self.attached.store(false, Ordering::SeqCst);
        self.set_status(SidecarStatus::Stopped);
    }

    fn spawn_sidecar(self: &Arc<Self>, app: &AppHandle, port: u16) -> Result<u16, String> {
        let sidecar = app
            .shell()
            .sidecar("duduclaw")
            .map_err(|e| format!("sidecar lookup failed: {e}"))?
            // `run --yes`: start the full server (gateway + dashboard + channels)
            // non-interactively. There is no `start` subcommand — that was the
            // launchd-era typo; the CLI entry point is `duduclaw run`.
            .args(["run", "--yes"])
            .env("DUDUCLAW_PORT", port.to_string())
            // Employee desktop sidecars must NEVER become a LAN-discoverable
            // gateway (design §2.5 "防 gateway 蔓延"). Force mDNS off regardless
            // of config.toml — this env wins over `[server] mdns_advertise`.
            .env("DUDUCLAW_MDNS_ADVERTISE", "0")
            .env("PATH", lifecycle::augmented_path());

        let (mut rx, child) = sidecar
            .spawn()
            .map_err(|e| format!("sidecar spawn failed: {e}"))?;

        let pid = child.pid();
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.write_pidfile(pid);
        *self.port.lock().unwrap() = port;
        *self.child.lock().unwrap() = Some(child);
        self.attached.store(false, Ordering::SeqCst);
        self.set_status(SidecarStatus::Starting);
        tracing::info!("spawned gateway sidecar pid={pid} port={port} — waiting for readiness");

        // Readiness watcher: flip Starting → Running only once the port actually
        // answers, so the "本機" card never claims 運行中 for a gateway that
        // still refuses connections (the old behavior behind the picker's
        // "could not connect" right after app relaunch).
        let me = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || {
            let deadline = Instant::now() + READY_TIMEOUT;
            loop {
                if me.generation.load(Ordering::SeqCst) != my_gen
                    || me.status() != SidecarStatus::Starting
                {
                    return; // superseded or already resolved (crash → Error)
                }
                if lifecycle::is_listening(lifecycle::DEFAULT_HOST, port) {
                    if me.generation.load(Ordering::SeqCst) == my_gen {
                        me.set_status(SidecarStatus::Running);
                        tracing::info!("gateway sidecar ready on port {port}");
                    }
                    return;
                }
                if Instant::now() >= deadline {
                    tracing::error!(
                        "gateway sidecar did not answer on port {port} within {}s",
                        READY_TIMEOUT.as_secs()
                    );
                    me.set_status(SidecarStatus::Error);
                    return;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });

        // Drain sidecar stdout/stderr and react to termination (§D2.5).
        let me = Arc::clone(self);
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(line) => {
                        tracing::debug!(target: "sidecar", "{}", String::from_utf8_lossy(&line));
                    }
                    CommandEvent::Stderr(line) => {
                        tracing::warn!(target: "sidecar", "{}", String::from_utf8_lossy(&line));
                    }
                    CommandEvent::Terminated(payload) => {
                        if me.generation.load(Ordering::SeqCst) != my_gen {
                            break; // we were superseded — the replacement owns state
                        }
                        me.clear_pidfile();
                        *me.child.lock().unwrap() = None;
                        if me.shutting_down.load(Ordering::SeqCst) {
                            me.set_status(SidecarStatus::Stopped);
                        } else {
                            tracing::error!("sidecar exited unexpectedly: {payload:?}");
                            me.set_status(SidecarStatus::Error);
                            me.restart_with_backoff(&app2, my_gen);
                        }
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(port)
    }

    /// Restart the sidecar after an unexpected exit, with exponential backoff and
    /// a hard cap so a crash-loop trips into Error instead of hammering (§D2.5).
    /// `failed_gen` is the generation that died — if someone else (user click,
    /// tray restart) already spawned a replacement, this task must stand down.
    fn restart_with_backoff(self: &Arc<Self>, app: &AppHandle, failed_gen: u64) {
        const MAX_ATTEMPTS: u32 = 5;
        let me = Arc::clone(self);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            for attempt in 1..=MAX_ATTEMPTS {
                if me.shutting_down.load(Ordering::SeqCst)
                    || me.generation.load(Ordering::SeqCst) != failed_gen
                {
                    return;
                }
                let delay = Duration::from_millis(500u64.saturating_mul(1 << (attempt - 1)));
                tokio_sleep(delay).await;
                tracing::info!("sidecar restart attempt {attempt}/{MAX_ATTEMPTS}");
                if me.spawn_sidecar(&app, me.port()).is_ok() {
                    return;
                }
            }
            tracing::error!("sidecar failed to recover after {MAX_ATTEMPTS} attempts");
            me.set_status(SidecarStatus::Error);
            notify_sidecar_failure(&app);
        });
    }

    /// Stop the sidecar gracefully on app exit (§D2.3). We never kill a gateway
    /// we merely attached to.
    pub fn stop(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        // Supersede any in-flight readiness/event tasks so a Terminated event
        // arriving after a later start() can't clobber the fresh state.
        self.generation.fetch_add(1, Ordering::SeqCst);
        if self.attached.load(Ordering::SeqCst) {
            self.set_status(SidecarStatus::Stopped);
            return;
        }
        if let Some(child) = self.child.lock().unwrap().take() {
            // NOTE: `CommandChild::kill` is *not* a graceful terminate —
            // tauri-plugin-shell forwards it to `std::process::Child::kill`,
            // i.e. SIGKILL on Unix / TerminateProcess on Windows. The gateway
            // gets no signal handler run (it has no SIGTERM handler either), so
            // nothing is flushed here; what we buy is that the port is released
            // immediately and app quit can never hang on a wedged sidecar. That
            // trade-off is deliberate for desktop quit.
            // KNOWN DEBT: send SIGTERM + bounded wait first (and add a SIGTERM
            // handler to the gateway) so shutdown flushes state like the
            // Ctrl+C / auto-update path does.
            let _ = child.kill();
        }
        self.clear_pidfile();
        self.set_status(SidecarStatus::Stopped);
    }

    /// Block (briefly) until the gateway answers on its port. Retained as a
    /// readiness primitive; since WP-GW the window boots on the bundled Gateway
    /// picker (which polls `gateway_local_status` instead of blocking on this),
    /// so it currently has no caller.
    #[allow(dead_code)]
    pub fn wait_until_ready(&self, timeout: Duration) -> bool {
        let port = self.port();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if lifecycle::is_listening(lifecycle::DEFAULT_HOST, port) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        false
    }
}

async fn tokio_sleep(d: Duration) {
    // Use Tauri's bundled async runtime sleep without pulling tokio directly.
    tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d))
        .await
        .ok();
}

fn notify_sidecar_failure(app: &AppHandle) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app
        .notification()
        .builder()
        .title("DuDuClaw")
        .body("背景服務無法啟動,請查看日誌。")
        .show();
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// Ask the OS what process currently owns `pid`, returning the raw probe output
/// (`ps -p <pid> -o comm=` on Unix, `tasklist /FI "PID eq <pid>" /FO CSV` on
/// Windows). `None` means the probe itself could not run; an empty string means
/// no such process.
fn probe_pid_identity(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    #[cfg(windows)]
    {
        // tasklist prints a CSV row per match, or an "INFO: No tasks..." line
        // when the filter matches nothing. Note the filter text itself never
        // contains our binary name, so scanning the whole output is safe.
        let out = std::process::Command::new("tasklist")
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

/// Does the process-name probe output identify a DuDuClaw gateway?
///
/// Fail-closed: anything we cannot positively identify (empty output, the
/// Windows "no tasks" notice, an unrelated binary name) returns `false`, so the
/// caller never signals a process it did not spawn.
fn pid_identity_matches(probe_output: &str) -> bool {
    probe_output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        // The Windows no-match notice is informational text, not a process row.
        .filter(|l| !l.starts_with("INFO:"))
        .any(|l| l.to_ascii_lowercase().contains("duduclaw"))
}

#[cfg(test)]
mod tests {
    use super::pid_identity_matches;

    #[test]
    fn matches_unix_comm_output() {
        assert!(pid_identity_matches("duduclaw\n"));
        assert!(pid_identity_matches("/usr/local/bin/duduclaw\n"));
        // macOS bundle path form.
        assert!(pid_identity_matches(
            "/Applications/DuDuClaw.app/Contents/MacOS/duduclaw\n"
        ));
    }

    #[test]
    fn matches_windows_tasklist_row() {
        assert!(pid_identity_matches(
            "\"duduclaw.exe\",\"4242\",\"Console\",\"1\",\"81,234 K\"\n"
        ));
    }

    #[test]
    fn rejects_recycled_pid_owned_by_another_process() {
        // The exact scenario this guard exists for: pidfile survived, the OS
        // handed the PID to something else. Killing these would be user data loss.
        assert!(!pid_identity_matches("Code Helper\n"));
        assert!(!pid_identity_matches("/usr/bin/ssh\n"));
        assert!(!pid_identity_matches(
            "\"chrome.exe\",\"4242\",\"Console\",\"1\",\"512,000 K\"\n"
        ));
    }

    #[test]
    fn rejects_absent_process() {
        assert!(!pid_identity_matches(""));
        assert!(!pid_identity_matches("   \n\n"));
        assert!(!pid_identity_matches(
            "INFO: No tasks are running which match the specified criteria.\n"
        ));
    }
}
