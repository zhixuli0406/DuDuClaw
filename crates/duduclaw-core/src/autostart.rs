//! Gateway login/boot autostart registration — the single implementation shared
//! by the CLI (`duduclaw service install/uninstall/status`) and the dashboard
//! RPC (`system.autostart.*`).
//!
//! Per-platform mechanism (all user-level — no elevation required):
//! - **macOS**: LaunchAgent plist at `~/Library/LaunchAgents/com.duduclaw.gateway.plist`
//! - **Linux**: systemd user unit at `$XDG_CONFIG_HOME/systemd/user/duduclaw.service`
//!   enabled via the `default.target.wants` symlink (written directly so it works
//!   even when `systemctl` is unavailable; `daemon-reload` is best-effort)
//! - **Windows**: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value via `reg.exe`
//!
//! Design rule: `enable`/`disable` manage the **registration only** and never
//! start or stop a running gateway. In particular `disable` must not
//! `launchctl unload` / `systemctl stop` — the currently running gateway may be
//! the very process serving the dashboard RPC that asked for the change.
//! Starting/stopping the live process stays with `duduclaw service start/stop`.
//!
//! The registration *content* generators are pure functions so every platform's
//! output is unit-testable from any host OS.

use std::path::{Path, PathBuf};

use crate::error::{DuDuClawError, Result};

/// Primary launchd label (matches `scripts/box-setup/setup-macos.sh`).
pub const LAUNCHD_LABEL: &str = "com.duduclaw.gateway";
/// Older label once printed by `duduclaw service install`; cleaned up on
/// enable/disable so the two registrations can never coexist.
pub const LEGACY_LAUNCHD_LABEL: &str = "dev.duduclaw";
/// systemd user unit file name.
pub const SYSTEMD_UNIT: &str = "duduclaw.service";
/// Windows `Run` key value name.
pub const WINDOWS_RUN_VALUE: &str = "DuDuClaw";
/// Windows `Run` key path (HKCU — per-user, no elevation).
pub const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Snapshot of the autostart registration, platform-agnostic shape for the
/// dashboard and CLI to render.
#[derive(Debug, Clone)]
pub struct AutostartStatus {
    /// Whether this platform has an implementation at all.
    pub supported: bool,
    /// Whether the gateway is currently registered to start at login/boot.
    pub enabled: bool,
    /// Mechanism identifier: `launchd` / `systemd-user` / `windows-run-key` / `unsupported`.
    pub method: &'static str,
    /// Human-readable location of the registration (plist/unit path, registry key).
    pub detail: String,
}

// ── Pure content generators (unit-testable on every host) ────────────────────

/// Escape the five XML special characters for safe plist embedding.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Generate the LaunchAgent plist. `home` is the user home directory,
/// `duduclaw_home` the resolved state root (`DUDUCLAW_HOME` override honoured
/// by embedding it so the login-launched gateway uses the same instance).
pub fn launchd_plist(exe: &Path, home: &Path, duduclaw_home: &Path) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let home_s = xml_escape(&home.display().to_string());
    let state = xml_escape(&duduclaw_home.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
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
    <key>ThrottleInterval</key>
    <integer>30</integer>
    <key>WorkingDirectory</key>
    <string>{state}</string>
    <key>StandardOutPath</key>
    <string>{state}/logs/gateway.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{state}/logs/gateway.stderr.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:{home_s}/.cargo/bin:{home_s}/.local/bin</string>
        <key>DUDUCLAW_HOME</key>
        <string>{state}</string>
    </dict>
</dict>
</plist>
"#
    )
}

/// Quote a path for a systemd `ExecStart=` line (handles spaces; systemd uses
/// double-quoted words with `\` and `"` backslash-escaped).
fn systemd_quote(path: &Path) -> String {
    let s = path.display().to_string();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Generate the systemd user unit.
pub fn systemd_user_unit(exe: &Path, duduclaw_home: &Path) -> String {
    let exec = systemd_quote(exe);
    let state = duduclaw_home.display();
    format!(
        r#"[Unit]
Description=DuDuClaw Gateway
Documentation=https://github.com/duduclaw/duduclaw
After=network-online.target

[Service]
ExecStart={exec} run --yes
# `always` (not `on-failure`): self-update exits 0 after graceful shutdown
# and relies on the supervisor to relaunch if in-process re-exec fails.
# `systemctl --user stop` still works — systemd never restarts a unit that
# was stopped explicitly.
Restart=always
RestartSec=10
Environment=DUDUCLAW_HOME={state}

[Install]
WantedBy=default.target
"#
    )
}

/// Generate the Windows `Run`-key command line (quoted exe + args).
pub fn windows_run_command(exe: &Path) -> String {
    format!("\"{}\" run --yes", exe.display())
}

/// `reg.exe` argument vectors for enable / status / disable — pure so they can
/// be asserted from any host.
pub fn windows_reg_add_args(exe: &Path) -> Vec<String> {
    vec![
        "add".into(),
        WINDOWS_RUN_KEY.into(),
        "/v".into(),
        WINDOWS_RUN_VALUE.into(),
        "/t".into(),
        "REG_SZ".into(),
        "/d".into(),
        windows_run_command(exe),
        "/f".into(),
    ]
}

pub fn windows_reg_query_args() -> Vec<String> {
    vec![
        "query".into(),
        WINDOWS_RUN_KEY.into(),
        "/v".into(),
        WINDOWS_RUN_VALUE.into(),
    ]
}

pub fn windows_reg_delete_args() -> Vec<String> {
    vec![
        "delete".into(),
        WINDOWS_RUN_KEY.into(),
        "/v".into(),
        WINDOWS_RUN_VALUE.into(),
        "/f".into(),
    ]
}

// ── Pure path resolvers (base dirs injected for testability) ─────────────────

/// LaunchAgent plist path for a given label under `home`.
pub fn launchd_plist_path_in(home: &Path, label: &str) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{label}.plist"))
}

/// systemd user unit path under a config dir (`$XDG_CONFIG_HOME` or `~/.config`).
pub fn systemd_unit_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("systemd/user").join(SYSTEMD_UNIT)
}

/// The `default.target.wants` enablement symlink for the unit.
pub fn systemd_wants_path_in(config_dir: &Path) -> PathBuf {
    config_dir
        .join("systemd/user/default.target.wants")
        .join(SYSTEMD_UNIT)
}

fn home_dir() -> PathBuf {
    PathBuf::from(crate::platform::home_dir())
}

#[cfg(target_os = "linux")]
fn xdg_config_dir() -> PathBuf {
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home_dir().join(".config"),
    }
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe()
        .map_err(|e| DuDuClawError::Gateway(format!("cannot resolve current executable: {e}")))
}

// ── Platform implementations ─────────────────────────────────────────────────

/// Register the gateway to start at login/boot. Never touches a running
/// process; on success the change takes effect at the next login.
pub fn enable() -> Result<AutostartStatus> {
    #[cfg(target_os = "macos")]
    {
        let exe = current_exe()?;
        let home = home_dir();
        let state = crate::platform::duduclaw_home();
        std::fs::create_dir_all(state.join("logs"))?;
        let plist_path = launchd_plist_path_in(&home, LAUNCHD_LABEL);
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&plist_path, launchd_plist(&exe, &home, &state))?;
        // A stale legacy-label plist would register a second copy — remove it.
        let _ = std::fs::remove_file(launchd_plist_path_in(&home, LEGACY_LAUNCHD_LABEL));
        return Ok(AutostartStatus {
            supported: true,
            enabled: true,
            method: "launchd",
            detail: plist_path.display().to_string(),
        });
    }
    #[cfg(target_os = "linux")]
    {
        let exe = current_exe()?;
        let state = crate::platform::duduclaw_home();
        let config = xdg_config_dir();
        let unit_path = systemd_unit_path_in(&config);
        if let Some(parent) = unit_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&unit_path, systemd_user_unit(&exe, &state))?;
        // Enable = the default.target.wants symlink, written directly so this
        // works even without a reachable user systemd (e.g. over SSH without
        // a session bus). `systemctl --user daemon-reload` is best-effort.
        let wants = systemd_wants_path_in(&config);
        if let Some(parent) = wants.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(&wants);
        std::os::unix::fs::symlink(&unit_path, &wants)?;
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        return Ok(AutostartStatus {
            supported: true,
            enabled: true,
            method: "systemd-user",
            detail: unit_path.display().to_string(),
        });
    }
    #[cfg(target_os = "windows")]
    {
        let exe = current_exe()?;
        let status = std::process::Command::new("reg")
            .args(windows_reg_add_args(&exe))
            .status()
            .map_err(|e| DuDuClawError::Gateway(format!("reg.exe not available: {e}")))?;
        if !status.success() {
            return Err(DuDuClawError::Gateway(format!(
                "reg add {WINDOWS_RUN_KEY} failed with {status}"
            )));
        }
        return Ok(AutostartStatus {
            supported: true,
            enabled: true,
            method: "windows-run-key",
            detail: format!("{WINDOWS_RUN_KEY}\\{WINDOWS_RUN_VALUE}"),
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(DuDuClawError::Gateway(
            "autostart is not supported on this platform".into(),
        ))
    }
}

/// Remove the login/boot registration. The running gateway (if any) keeps
/// running — only the next-login behaviour changes.
pub fn disable() -> Result<AutostartStatus> {
    #[cfg(target_os = "macos")]
    {
        let home = home_dir();
        for label in [LAUNCHD_LABEL, LEGACY_LAUNCHD_LABEL] {
            let _ = std::fs::remove_file(launchd_plist_path_in(&home, label));
        }
        return Ok(AutostartStatus {
            supported: true,
            enabled: false,
            method: "launchd",
            detail: launchd_plist_path_in(&home, LAUNCHD_LABEL)
                .display()
                .to_string(),
        });
    }
    #[cfg(target_os = "linux")]
    {
        let config = xdg_config_dir();
        let unit_path = systemd_unit_path_in(&config);
        let _ = std::fs::remove_file(systemd_wants_path_in(&config));
        let _ = std::fs::remove_file(&unit_path);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        return Ok(AutostartStatus {
            supported: true,
            enabled: false,
            method: "systemd-user",
            detail: unit_path.display().to_string(),
        });
    }
    #[cfg(target_os = "windows")]
    {
        // `reg delete` exits non-zero when the value is already absent — that
        // is the desired end state, so only surface a spawn failure.
        let _ = std::process::Command::new("reg")
            .args(windows_reg_delete_args())
            .status()
            .map_err(|e| DuDuClawError::Gateway(format!("reg.exe not available: {e}")))?;
        return Ok(AutostartStatus {
            supported: true,
            enabled: false,
            method: "windows-run-key",
            detail: format!("{WINDOWS_RUN_KEY}\\{WINDOWS_RUN_VALUE}"),
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(DuDuClawError::Gateway(
            "autostart is not supported on this platform".into(),
        ))
    }
}

/// Report whether the gateway is registered to start at login/boot.
pub fn status() -> AutostartStatus {
    #[cfg(target_os = "macos")]
    {
        let home = home_dir();
        let primary = launchd_plist_path_in(&home, LAUNCHD_LABEL);
        let legacy = launchd_plist_path_in(&home, LEGACY_LAUNCHD_LABEL);
        let enabled = primary.exists() || legacy.exists();
        return AutostartStatus {
            supported: true,
            enabled,
            method: "launchd",
            detail: primary.display().to_string(),
        };
    }
    #[cfg(target_os = "linux")]
    {
        let config = xdg_config_dir();
        let unit_path = systemd_unit_path_in(&config);
        let enabled = unit_path.exists() && systemd_wants_path_in(&config).exists();
        return AutostartStatus {
            supported: true,
            enabled,
            method: "systemd-user",
            detail: unit_path.display().to_string(),
        };
    }
    #[cfg(target_os = "windows")]
    {
        let enabled = std::process::Command::new("reg")
            .args(windows_reg_query_args())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        return AutostartStatus {
            supported: true,
            enabled,
            method: "windows-run-key",
            detail: format!("{WINDOWS_RUN_KEY}\\{WINDOWS_RUN_VALUE}"),
        };
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        AutostartStatus {
            supported: false,
            enabled: false,
            method: "unsupported",
            detail: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── macOS launchd plist ──────────────────────────────────────────────

    #[test]
    fn launchd_plist_contains_label_exe_and_run_args() {
        let plist = launchd_plist(
            Path::new("/usr/local/bin/duduclaw"),
            Path::new("/Users/kai"),
            Path::new("/Users/kai/.duduclaw"),
        );
        assert!(plist.contains("<string>com.duduclaw.gateway</string>"));
        assert!(plist.contains("<string>/usr/local/bin/duduclaw</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(plist.contains("<string>--yes</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains("/Users/kai/.duduclaw/logs/gateway.stdout.log"));
        assert!(plist.contains("<key>DUDUCLAW_HOME</key>"));
    }

    #[test]
    fn launchd_plist_keeps_alive_with_throttle() {
        // KeepAlive=true so a clean exit-0 self-update gets relaunched by
        // launchd; ThrottleInterval bounds any crash/port-conflict loop.
        let plist = launchd_plist(
            Path::new("/x/duduclaw"),
            Path::new("/Users/kai"),
            Path::new("/Users/kai/.duduclaw"),
        );
        assert!(plist.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(plist.contains("<key>ThrottleInterval</key>"));
    }

    #[test]
    fn launchd_plist_escapes_xml_special_chars_in_paths() {
        let plist = launchd_plist(
            Path::new("/Users/a&b/<bin>/duduclaw"),
            Path::new("/Users/a&b"),
            Path::new("/Users/a&b/.duduclaw"),
        );
        assert!(plist.contains("/Users/a&amp;b/&lt;bin&gt;/duduclaw"));
        assert!(!plist.contains("a&b"));
    }

    #[test]
    fn launchd_plist_path_uses_label() {
        assert_eq!(
            launchd_plist_path_in(Path::new("/Users/kai"), LAUNCHD_LABEL),
            PathBuf::from("/Users/kai/Library/LaunchAgents/com.duduclaw.gateway.plist")
        );
        assert_eq!(
            launchd_plist_path_in(Path::new("/Users/kai"), LEGACY_LAUNCHD_LABEL),
            PathBuf::from("/Users/kai/Library/LaunchAgents/dev.duduclaw.plist")
        );
    }

    // ── Linux systemd user unit ──────────────────────────────────────────

    #[test]
    fn systemd_unit_contains_exec_restart_and_install() {
        let unit = systemd_user_unit(Path::new("/usr/bin/duduclaw"), Path::new("/home/kai/.duduclaw"));
        assert!(unit.contains("ExecStart=\"/usr/bin/duduclaw\" run --yes"));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Environment=DUDUCLAW_HOME=/home/kai/.duduclaw"));
    }

    #[test]
    fn systemd_quote_escapes_spaces_and_quotes() {
        let unit = systemd_user_unit(
            Path::new("/opt/my tools/du\"du/duduclaw"),
            Path::new("/home/kai/.duduclaw"),
        );
        assert!(unit.contains(r#"ExecStart="/opt/my tools/du\"du/duduclaw" run --yes"#));
    }

    #[test]
    fn systemd_paths_are_under_config_dir() {
        let cfg = Path::new("/home/kai/.config");
        assert_eq!(
            systemd_unit_path_in(cfg),
            PathBuf::from("/home/kai/.config/systemd/user/duduclaw.service")
        );
        assert_eq!(
            systemd_wants_path_in(cfg),
            PathBuf::from("/home/kai/.config/systemd/user/default.target.wants/duduclaw.service")
        );
    }

    // ── Windows Run key ──────────────────────────────────────────────────

    #[test]
    fn windows_run_command_quotes_exe() {
        assert_eq!(
            windows_run_command(Path::new(r"C:\Program Files\DuDuClaw\duduclaw.exe")),
            r#""C:\Program Files\DuDuClaw\duduclaw.exe" run --yes"#
        );
    }

    #[test]
    fn windows_reg_args_target_hkcu_run_value() {
        let add = windows_reg_add_args(Path::new(r"C:\d\duduclaw.exe"));
        assert_eq!(add[0], "add");
        assert_eq!(add[1], WINDOWS_RUN_KEY);
        assert!(add.contains(&"/f".to_string()));
        assert!(add.contains(&r#""C:\d\duduclaw.exe" run --yes"#.to_string()));

        let query = windows_reg_query_args();
        assert_eq!(query, vec!["query", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE]);

        let del = windows_reg_delete_args();
        assert_eq!(del, vec!["delete", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE, "/f"]);
    }
}
