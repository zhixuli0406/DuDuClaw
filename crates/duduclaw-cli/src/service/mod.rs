//! Service management helpers — generates systemd/launchd configuration.
//!
//! **NOTE**: These functions intentionally only *print* commands and config
//! snippets for the user to run manually. They do NOT execute system-level
//! service commands, because that requires elevated privileges and varies
//! by OS/distribution. This is by design (CLI-H1).

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

/// Detect the current platform and print service manager info.
#[allow(dead_code)] // Public API — will be called from future `service install` command
pub fn detect_platform() {
    #[cfg(target_os = "macos")]
    println!("Platform: macOS — will use launchd");

    #[cfg(target_os = "linux")]
    println!("Platform: Linux — will use systemd");

    #[cfg(target_os = "windows")]
    println!("Platform: Windows — will use Windows Service");

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    println!("Platform: unsupported for service management");
}

// ---------------------------------------------------------------------------
// Linux — systemd
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod systemd {
    use duduclaw_core::error::Result;

    /// Generate and print the systemd unit file.
    pub async fn install() -> Result<()> {
        let exe = std::env::current_exe().unwrap_or_default();
        let service_content = format!(
            r#"[Unit]
Description=DuDuClaw AI Assistant
After=network.target docker.service
Wants=docker.service

[Service]
Type=simple
User=duduclaw
Group=duduclaw
ExecStart={exe} run --yes
ExecStop=/bin/kill -SIGTERM $MAINPID
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
"#,
            exe = exe.display()
        );

        let path = "/etc/systemd/system/duduclaw.service";
        println!("Systemd service unit:\n");
        println!("{}", service_content);
        println!("Run with sudo to install:");
        println!("  sudo tee {} <<'EOF'\n{}EOF", path, service_content);
        println!("  sudo systemctl daemon-reload");
        println!("  sudo systemctl enable duduclaw");
        Ok(())
    }

    pub async fn start() -> Result<()> {
        println!("Run: sudo systemctl start duduclaw");
        Ok(())
    }

    pub async fn stop() -> Result<()> {
        println!("Run: sudo systemctl stop duduclaw");
        Ok(())
    }

    pub async fn status() -> Result<()> {
        println!("Run: systemctl status duduclaw");
        Ok(())
    }

    pub async fn logs() -> Result<()> {
        println!("Run: journalctl -u duduclaw -f");
        Ok(())
    }

    pub async fn uninstall() -> Result<()> {
        println!("Run:");
        println!("  sudo systemctl stop duduclaw");
        println!("  sudo systemctl disable duduclaw");
        println!("  sudo rm /etc/systemd/system/duduclaw.service");
        println!("  sudo systemctl daemon-reload");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// macOS — launchd
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod launchd {
    use duduclaw_core::error::Result;

    /// Generate and print the launchd plist.
    pub async fn install() -> Result<()> {
        let exe = std::env::current_exe().unwrap_or_default();
        let home = dirs::home_dir().unwrap_or_default();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.duduclaw</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>run</string>
        <string>--yes</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/Library/Logs/duduclaw.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/Library/Logs/duduclaw.stderr.log</string>
</dict>
</plist>"#,
            exe = exe.display(),
            home = home.display(),
        );
        let plist_path = home.join("Library/LaunchAgents/dev.duduclaw.plist");
        println!("LaunchAgent plist for: {}\n", plist_path.display());
        println!("{}", plist);
        println!(
            "\nTo install, save the above to {} and run:",
            plist_path.display()
        );
        println!("  launchctl load {}", plist_path.display());
        Ok(())
    }

    pub async fn start() -> Result<()> {
        let home = dirs::home_dir().unwrap_or_default();
        let plist_path = home.join("Library/LaunchAgents/dev.duduclaw.plist");
        println!("Run: launchctl load {}", plist_path.display());
        Ok(())
    }

    pub async fn stop() -> Result<()> {
        let home = dirs::home_dir().unwrap_or_default();
        let plist_path = home.join("Library/LaunchAgents/dev.duduclaw.plist");
        println!("Run: launchctl unload {}", plist_path.display());
        Ok(())
    }

    pub async fn status() -> Result<()> {
        println!("Run: launchctl list | grep duduclaw");
        Ok(())
    }

    pub async fn logs() -> Result<()> {
        println!("Run: tail -f /tmp/duduclaw.stdout.log /tmp/duduclaw.stderr.log");
        Ok(())
    }

    pub async fn uninstall() -> Result<()> {
        let home = dirs::home_dir().unwrap_or_default();
        let plist_path = home.join("Library/LaunchAgents/dev.duduclaw.plist");
        println!("Run:");
        println!("  launchctl unload {}", plist_path.display());
        println!("  rm {}", plist_path.display());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Windows — Windows Service via sc.exe
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
mod windows_svc {
    use duduclaw_core::error::Result;

    pub async fn install() -> Result<()> {
        let exe = std::env::current_exe().unwrap_or_default();
        println!("Windows Service installation requires administrator privileges.");
        println!(
            "Run: sc create DuDuClaw binPath= \"{}\" start= auto",
            exe.display()
        );
        println!("     sc description DuDuClaw \"DuDuClaw AI Assistant\"");
        Ok(())
    }

    pub async fn start() -> Result<()> {
        println!("Run (as admin): sc start DuDuClaw");
        Ok(())
    }

    pub async fn stop() -> Result<()> {
        println!("Run (as admin): sc stop DuDuClaw");
        Ok(())
    }

    pub async fn status() -> Result<()> {
        println!("Run: sc query DuDuClaw");
        Ok(())
    }

    pub async fn logs() -> Result<()> {
        println!("Run: Get-EventLog -LogName Application -Source DuDuClaw");
        Ok(())
    }

    pub async fn uninstall() -> Result<()> {
        println!("Run (as admin):");
        println!("  sc stop DuDuClaw");
        println!("  sc delete DuDuClaw");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Platform dispatch
// ---------------------------------------------------------------------------

async fn install_service() -> Result<()> {
    #[cfg(target_os = "linux")]
    return systemd::install().await;
    #[cfg(target_os = "macos")]
    return launchd::install().await;
    #[cfg(target_os = "windows")]
    return windows_svc::install().await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(DuDuClawError::Config(
        "Unsupported platform for service installation".into(),
    ));
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
    #[cfg(target_os = "linux")]
    return systemd::status().await;
    #[cfg(target_os = "macos")]
    return launchd::status().await;
    #[cfg(target_os = "windows")]
    return windows_svc::status().await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(DuDuClawError::Config(
        "Unsupported platform for service management".into(),
    ));
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
    #[cfg(target_os = "linux")]
    return systemd::uninstall().await;
    #[cfg(target_os = "macos")]
    return launchd::uninstall().await;
    #[cfg(target_os = "windows")]
    return windows_svc::uninstall().await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(DuDuClawError::Config(
        "Unsupported platform for service management".into(),
    ));
}
