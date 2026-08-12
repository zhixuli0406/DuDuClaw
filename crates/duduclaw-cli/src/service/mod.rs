//! Service management helpers.
//!
//! `install` / `uninstall` / `status` go through `duduclaw_core::autostart` —
//! the same user-level registration (launchd LaunchAgent / systemd user unit /
//! HKCU Run key) the dashboard `system.autostart.*` RPCs use, so the two
//! surfaces can never drift. User-level registration needs no elevation, so
//! these now actually write (the former print-only design, CLI-H1, predates
//! the user-level mechanism). Registration changes never touch a running
//! gateway process — that stays with `start` / `stop`, which keep the original
//! per-platform print-or-kill behaviour.

#[allow(unused_imports)]
use duduclaw_core::error::{DuDuClawError, Result};

/// Actions that can be performed on the background service.
pub enum ServiceAction {
    Install,
    Start,
    Stop,
    Status,
    Logs { lines: usize },
    Uninstall,
}

/// Dispatch a service action to the platform-appropriate implementation.
pub async fn handle_service(action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::Install => install_service().await,
        ServiceAction::Start => start_service().await,
        ServiceAction::Stop => stop_service().await,
        ServiceAction::Status => service_status().await,
        ServiceAction::Logs { lines } => service_logs(lines).await,
        ServiceAction::Uninstall => uninstall_service().await,
    }
}

// ---------------------------------------------------------------------------
// Linux — systemd
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod systemd {
    use duduclaw_core::error::Result;

    pub async fn start() -> Result<()> {
        println!("Run: systemctl --user start duduclaw");
        Ok(())
    }

    pub async fn stop() -> Result<()> {
        println!("Run: systemctl --user stop duduclaw");
        Ok(())
    }

    pub async fn logs() -> Result<()> {
        println!("Run: journalctl --user -u duduclaw -f");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// macOS — launchd
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod launchd {
    use duduclaw_core::error::Result;

    pub async fn start() -> Result<()> {
        let home = dirs::home_dir().unwrap_or_default();
        let plist_path = duduclaw_core::autostart::launchd_plist_path_in(
            &home,
            duduclaw_core::autostart::LAUNCHD_LABEL,
        );
        if plist_path.exists() {
            println!("Run: launchctl load {}", plist_path.display());
        } else {
            println!("No LaunchAgent registered. Run `duduclaw service install` first,");
            println!("or start in the foreground with `duduclaw run`.");
        }
        Ok(())
    }

    pub async fn stop() -> Result<()> {
        // Same priority resolution `duduclaw run` uses (env > config.toml
        // [gateway] port > default) — this used to read only `DUDUCLAW_PORT`
        // and default to 18789, so a gateway actually running on a
        // config.toml-configured port would go un-found: `service stop`
        // would report "no process found" while the gateway kept running.
        let (port, _source) =
            duduclaw_core::gateway_port_for_home(&duduclaw_core::duduclaw_home());

        // 1. Unload from launchctl (stops auto-restart via KeepAlive). Both the
        //    current and the legacy label, whichever is registered.
        let home = dirs::home_dir().unwrap_or_default();
        println!("Unloading LaunchAgent...");
        for label in [
            duduclaw_core::autostart::LAUNCHD_LABEL,
            duduclaw_core::autostart::LEGACY_LAUNCHD_LABEL,
        ] {
            let plist_path = duduclaw_core::autostart::launchd_plist_path_in(&home, label);
            if plist_path.exists() {
                let _ = std::process::Command::new("launchctl")
                    .args(["unload", &plist_path.to_string_lossy()])
                    .status();
            }
        }

        // 2. Find process occupying the port.
        //    M13: `lsof -ti :PORT` returns whatever happens to hold the port —
        //    which may be an unrelated process if duduclaw already died and the
        //    port was reused. Only target PIDs whose executable is duduclaw so
        //    we never SIGKILL a bystander.
        let pids: Vec<i32> = find_pids_on_port(port)
            .into_iter()
            .filter(|&pid| is_duduclaw_process(pid))
            .collect();
        if pids.is_empty() {
            println!("✓ No duduclaw process found on port {port}. Service stopped.");
            return Ok(());
        }

        // 3. Send SIGTERM and wait for graceful exit (up to 5 seconds)
        println!("Sending SIGTERM to PID(s): {:?}", pids);
        for &pid in &pids {
            duduclaw_core::platform::terminate_process(pid as u32).ok();
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            // M13: re-filter to duduclaw PIDs each tick — a different process
            // grabbing the freed port must not keep us in the loop or get killed.
            let remaining: Vec<i32> = find_pids_on_port(port)
                .into_iter()
                .filter(|&pid| is_duduclaw_process(pid))
                .collect();
            if remaining.is_empty() {
                println!("✓ Service stopped gracefully. Port {port} released.");
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                // 4. SIGKILL remaining processes
                println!("Graceful shutdown timed out. Sending SIGKILL to: {:?}", remaining);
                for &pid in &remaining {
                    duduclaw_core::platform::kill_process(pid as u32).ok();
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                let still_alive: Vec<i32> = find_pids_on_port(port)
                    .into_iter()
                    .filter(|&pid| is_duduclaw_process(pid))
                    .collect();
                if still_alive.is_empty() {
                    println!("✓ Service killed. Port {port} released.");
                } else {
                    eprintln!("⚠ Could not kill process(es): {:?}. Try: kill -9 {}", still_alive, still_alive.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" "));
                }
                return Ok(());
            }
        }
    }

    /// Find PIDs of processes listening on the given port via `lsof`.
    fn find_pids_on_port(port: u16) -> Vec<i32> {
        let output = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{port}")])
            .output();
        match output {
            Ok(out) => {
                String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .filter_map(|s| s.parse::<i32>().ok())
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// M13: verify a PID actually belongs to a duduclaw process before we kill it.
    /// Reads the process command via `ps -o comm=` and matches "duduclaw" in the
    /// executable's basename. Fail-closed: if we can't determine the command, we
    /// do NOT treat it as duduclaw (better to leave it alone than kill a stranger).
    fn is_duduclaw_process(pid: i32) -> bool {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let comm = String::from_utf8_lossy(&out.stdout);
                comm.trim()
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .contains("duduclaw")
            }
            _ => false,
        }
    }

    pub async fn logs() -> Result<()> {
        // The LaunchAgent writes StandardOutPath/StandardErrorPath under the
        // DuDuClaw state root (see duduclaw_core::autostart::launchd_plist).
        let logs = duduclaw_core::platform::duduclaw_home().join("logs");
        println!(
            "Run: tail -f {} {}",
            logs.join("gateway.stdout.log").display(),
            logs.join("gateway.stderr.log").display()
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Windows — per-user Run key (registration) + process hints
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod windows_svc {
    use duduclaw_core::error::Result;

    pub async fn start() -> Result<()> {
        let exe = std::env::current_exe().unwrap_or_default();
        println!("Run: \"{}\" run --yes", exe.display());
        Ok(())
    }

    pub async fn stop() -> Result<()> {
        println!("Run: taskkill /IM duduclaw.exe");
        Ok(())
    }

    pub async fn logs() -> Result<()> {
        let logs = duduclaw_core::platform::duduclaw_home().join("logs");
        println!("Log directory: {}", logs.display());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Platform dispatch
// ---------------------------------------------------------------------------

/// Render an [`AutostartStatus`] for the console.
fn print_autostart(s: &duduclaw_core::autostart::AutostartStatus) {
    let state = if !s.supported {
        "unsupported"
    } else if s.enabled {
        "enabled"
    } else {
        "disabled"
    };
    println!("Autostart: {state} (method: {}, {})", s.method, s.detail);
}

async fn install_service() -> Result<()> {
    let status = duduclaw_core::autostart::enable()?;
    println!("✓ Registered DuDuClaw to start at login.");
    print_autostart(&status);
    println!("The gateway will start automatically at your next login.");
    println!("To start it right now: duduclaw run  (or `duduclaw service start`)");
    Ok(())
}

async fn start_service() -> Result<()> {
    #[cfg(target_os = "linux")]
    return systemd::start().await;
    #[cfg(target_os = "macos")]
    return launchd::start().await;
    #[cfg(target_os = "windows")]
    return windows_svc::start().await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(DuDuClawError::Config(
        "Unsupported platform for service management".into(),
    ));
}

async fn stop_service() -> Result<()> {
    #[cfg(target_os = "linux")]
    return systemd::stop().await;
    #[cfg(target_os = "macos")]
    return launchd::stop().await;
    #[cfg(target_os = "windows")]
    return windows_svc::stop().await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(DuDuClawError::Config(
        "Unsupported platform for service management".into(),
    ));
}

async fn service_status() -> Result<()> {
    print_autostart(&duduclaw_core::autostart::status());
    #[cfg(target_os = "macos")]
    println!("Live process check: launchctl list | grep duduclaw");
    #[cfg(target_os = "linux")]
    println!("Live process check: systemctl --user status duduclaw");
    #[cfg(target_os = "windows")]
    println!("Live process check: tasklist | findstr duduclaw");
    Ok(())
}

async fn service_logs(lines: usize) -> Result<()> {
    let _ = lines; // TODO: pass to platform-specific impl
    #[cfg(target_os = "linux")]
    return systemd::logs().await;
    #[cfg(target_os = "macos")]
    return launchd::logs().await;
    #[cfg(target_os = "windows")]
    return windows_svc::logs().await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(DuDuClawError::Config(
        "Unsupported platform for service management".into(),
    ));
}

async fn uninstall_service() -> Result<()> {
    let status = duduclaw_core::autostart::disable()?;
    println!("✓ Removed the login autostart registration.");
    print_autostart(&status);
    println!("A running gateway is NOT affected — stop it with `duduclaw service stop`.");
    Ok(())
}
