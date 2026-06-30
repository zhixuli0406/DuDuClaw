//! Gateway sidecar manager (TODO-genspark-workspace-shell §D2).
//!
//! Owns the spawned `duduclaw start` process: spawn with an augmented PATH,
//! record a pidfile, reclaim orphans from a previous crashed run, poll health
//! and auto-restart with backoff, and shut down gracefully on exit.
//!
//! NOTE: this targets the Tauri 2 + `tauri-plugin-shell` API. It is written to
//! the documented API but has NOT been compiled in this environment (no Tauri
//! toolchain) — see the TODO doc's Phase D verification notes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

use crate::lifecycle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarStatus {
    Stopped,
    Running,
    Error,
}

pub struct SidecarManager {
    child: Mutex<Option<CommandChild>>,
    status: Mutex<SidecarStatus>,
    /// The port the gateway is reachable on (spawned or attached).
    port: Mutex<u16>,
    /// Set when the user is intentionally quitting — suppresses auto-restart.
    shutting_down: AtomicBool,
    /// True when we attached to an externally-managed gateway (do not kill it).
    attached: AtomicBool,
}

impl SidecarManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            status: Mutex::new(SidecarStatus::Stopped),
            port: Mutex::new(lifecycle::DEFAULT_PORT),
            shutting_down: AtomicBool::new(false),
            attached: AtomicBool::new(false),
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
    /// live process, kill it so we start from a clean slate (§D2.1 / §D2.3).
    fn reclaim_orphan(&self) {
        let pidfile = lifecycle::sidecar_pidfile();
        if let Ok(contents) = std::fs::read_to_string(&pidfile) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                #[cfg(unix)]
                unsafe {
                    // SIGTERM; ignore errors (process may already be gone).
                    libc_kill(pid as i32, 15);
                }
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/F"])
                        .status();
                }
                tracing::info!("reclaimed orphaned sidecar pid={pid}");
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
    /// second call while running is a no-op.
    pub fn start(self: &Arc<Self>, app: &AppHandle) -> Result<u16, String> {
        if self.status() == SidecarStatus::Running {
            return Ok(self.port());
        }
        self.reclaim_orphan();

        let plan = lifecycle::plan_gateway(lifecycle::DEFAULT_HOST, lifecycle::configured_port());
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

    fn spawn_sidecar(self: &Arc<Self>, app: &AppHandle, port: u16) -> Result<u16, String> {
        let sidecar = app
            .shell()
            .sidecar("duduclaw")
            .map_err(|e| format!("sidecar lookup failed: {e}"))?
            .args(["start"])
            .env("DUDUCLAW_PORT", port.to_string())
            .env("PATH", lifecycle::augmented_path());

        let (mut rx, child) = sidecar
            .spawn()
            .map_err(|e| format!("sidecar spawn failed: {e}"))?;

        let pid = child.pid();
        self.write_pidfile(pid);
        *self.port.lock().unwrap() = port;
        *self.child.lock().unwrap() = Some(child);
        self.attached.store(false, Ordering::SeqCst);
        self.set_status(SidecarStatus::Running);
        tracing::info!("spawned gateway sidecar pid={pid} port={port}");

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
                        me.clear_pidfile();
                        *me.child.lock().unwrap() = None;
                        if me.shutting_down.load(Ordering::SeqCst) {
                            me.set_status(SidecarStatus::Stopped);
                        } else {
                            tracing::error!("sidecar exited unexpectedly: {payload:?}");
                            me.set_status(SidecarStatus::Error);
                            me.restart_with_backoff(&app2);
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
    fn restart_with_backoff(self: &Arc<Self>, app: &AppHandle) {
        const MAX_ATTEMPTS: u32 = 5;
        let me = Arc::clone(self);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            for attempt in 1..=MAX_ATTEMPTS {
                if me.shutting_down.load(Ordering::SeqCst) {
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
        if self.attached.load(Ordering::SeqCst) {
            self.set_status(SidecarStatus::Stopped);
            return;
        }
        if let Some(child) = self.child.lock().unwrap().take() {
            // CommandChild::kill sends a terminate; the gateway flushes on signal.
            let _ = child.kill();
        }
        self.clear_pidfile();
        self.set_status(SidecarStatus::Stopped);
    }

    /// Block (briefly) until the gateway answers on its port, so we only reveal
    /// the window once the UI can actually load (§D0).
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
