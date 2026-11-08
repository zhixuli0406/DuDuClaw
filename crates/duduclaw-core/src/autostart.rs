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

#[cfg(any(target_os = "macos", test))]
pub mod update_restart;

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
    launchd_plist_for(exe, home, duduclaw_home, None)
}

/// [`launchd_plist`] plus `AssociatedBundleIdentifiers` when the gateway
/// runs from `DuDuClaw Gateway.app`, so Login Items shows the app's name.
pub fn launchd_plist_for(
    exe: &Path,
    home: &Path,
    duduclaw_home: &Path,
    bundle_id: Option<&str>,
) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let home_s = xml_escape(&home.display().to_string());
    let state = xml_escape(&duduclaw_home.display().to_string());
    let associated = bundle_id
        .map(|id| {
            format!(
                "\n    <key>AssociatedBundleIdentifiers</key>\n    <array>\n        <string>{}</string>\n    </array>",
                xml_escape(id)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>{associated}
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

#[cfg(not(target_os = "macos"))]
fn current_exe() -> Result<PathBuf> {
    std::env::current_exe()
        .map_err(|e| DuDuClawError::Gateway(format!("cannot resolve current executable: {e}")))
}

/// macOS: the canonical executable (symlinks resolved), so the LaunchAgent
/// names the real file — inside `DuDuClaw Gateway.app` for bundle installs.
#[cfg(target_os = "macos")]
fn current_exe() -> Result<PathBuf> {
    let exe = crate::platform::executable_path();
    if exe.as_os_str().is_empty() {
        return Err(DuDuClawError::Gateway("cannot resolve current executable".into()));
    }
    Ok(exe)
}

// ── LaunchAgent boot migration (RM-155) ─────────────────────────────────────

/// What [`plan_plist_migration`] decided for one LaunchAgent plist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlistMigration {
    /// Already names `target` exactly (and carries the wanted bundle key).
    UpToDate,
    /// Names `target` through another spelling (symlink, old bare path that
    /// now links into the bundle) — rewrite `ProgramArguments[0]` (and
    /// `Program` when present) to `target`.
    Rewrite { program_key: bool },
    /// Points at a different binary — left alone (logged by the caller).
    Foreign(String),
}

/// Pure decision for the boot-time plist migration. `resolves_to_target`
/// answers "does this path canonicalize to the running executable?".
pub fn plan_plist_migration(
    program_arg0: Option<&str>,
    program: Option<&str>,
    has_associated_bundle: bool,
    target: &Path,
    want_associated_bundle: bool,
    resolves_to_target: impl Fn(&Path) -> bool,
) -> PlistMigration {
    let Some(arg0) = program_arg0 else {
        return PlistMigration::Foreign("plist has no ProgramArguments[0]".into());
    };
    let target_s = target.to_str();
    let names_target = |p: &str| Some(p) == target_s || resolves_to_target(Path::new(p));
    if !names_target(arg0) {
        return PlistMigration::Foreign(format!("ProgramArguments[0] is {arg0}"));
    }
    if let Some(program) = program
        && !names_target(program)
    {
        return PlistMigration::Foreign(format!("Program is {program}"));
    }
    let spelled_exactly = Some(arg0) == target_s && program.is_none_or(|p| Some(p) == target_s);
    if spelled_exactly && (has_associated_bundle || !want_associated_bundle) {
        return PlistMigration::UpToDate;
    }
    PlistMigration::Rewrite { program_key: program.is_some() }
}

/// Boot-time LaunchAgent migration: for each registered plist whose
/// `ProgramArguments[0]` canonicalizes to the running executable but is
/// spelled differently, rewrite it to the canonical path (and add
/// `AssociatedBundleIdentifiers` when running from the gateway bundle).
/// Plists naming another binary are left untouched. Registration only —
/// launchd picks the new path up at the next bootstrap.
#[cfg(target_os = "macos")]
pub fn migrate_launchd_plists() -> Vec<(PathBuf, std::result::Result<PlistMigration, String>)> {
    let exe = crate::platform::executable_path();
    if exe.as_os_str().is_empty() {
        return Vec::new();
    }
    let bundle_id = crate::macos_bundle::running_gateway_bundle().map(|_| crate::macos_bundle::BUNDLE_ID);
    migrate_launchd_plists_to(&exe, bundle_id)
}

/// [`migrate_launchd_plists`] for an explicit target — the updater calls this
/// after a bare → bundle migration, once the old bare path has become a
/// symlink into the bundle (so the old plist's path canonicalizes to `exe`).
#[cfg(target_os = "macos")]
pub fn migrate_launchd_plists_to(
    exe: &Path,
    bundle_id: Option<&str>,
) -> Vec<(PathBuf, std::result::Result<PlistMigration, String>)> {
    let exe = exe.to_path_buf();
    let home = home_dir();
    [LAUNCHD_LABEL, LEGACY_LAUNCHD_LABEL]
        .into_iter()
        .map(|label| launchd_plist_path_in(&home, label))
        .filter(|path| path.exists())
        .map(|path| {
            let outcome = migrate_one_plist(&path, &exe, bundle_id);
            match &outcome {
                Ok(PlistMigration::Rewrite { .. }) => tracing::info!(plist = %path.display(),
                    exe = %exe.display(), "LaunchAgent now names the canonical gateway executable"),
                Ok(PlistMigration::Foreign(why)) => tracing::info!(plist = %path.display(),
                    reason = %why, "LaunchAgent names another binary; left unchanged"),
                Ok(PlistMigration::UpToDate) => {}
                Err(e) => tracing::warn!(plist = %path.display(), error = %e, "LaunchAgent migration failed"),
            }
            (path, outcome)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn plutil(args: &[&std::ffi::OsStr]) -> std::result::Result<String, String> {
    let output = std::process::Command::new("/usr/bin/plutil")
        .args(args)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("/usr/bin/plutil: {e}"))?;
    if !output.status.success() {
        return Err(format!("/usr/bin/plutil failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Rewrite one plist in place (temp copy in the same directory → plutil
/// edits → lint → fsync → rename), so a crash never leaves a torn plist.
#[cfg(target_os = "macos")]
pub fn migrate_one_plist(
    plist: &Path,
    exe: &Path,
    bundle_id: Option<&str>,
) -> std::result::Result<PlistMigration, String> {
    use std::ffi::OsStr;
    let json = plutil(&[OsStr::new("-convert"), OsStr::new("json"), OsStr::new("-o"),
        OsStr::new("-"), OsStr::new("--"), plist.as_os_str()])?;
    let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let canonical_exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let decision = plan_plist_migration(
        value["ProgramArguments"][0].as_str(),
        value.get("Program").and_then(|v| v.as_str()),
        value.get("AssociatedBundleIdentifiers").is_some(),
        exe,
        bundle_id.is_some(),
        |p| std::fs::canonicalize(p).is_ok_and(|c| c == canonical_exe),
    );
    let program_key = match decision {
        PlistMigration::Rewrite { program_key } => program_key,
        other => return Ok(other),
    };
    let decision = PlistMigration::Rewrite { program_key };
    let exe_s = exe.to_str().ok_or("Non-UTF-8 executable path")?;
    let dir = plist.parent().ok_or("plist has no parent directory")?;
    let tmp = dir.join(format!(".{}.{}.tmp",
        plist.file_name().and_then(|n| n.to_str()).unwrap_or("plist"), uuid::Uuid::new_v4()));
    let result = (|| -> std::result::Result<(), String> {
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let bytes = std::fs::read(plist).map_err(|e| e.to_string())?;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644)
                .open(&tmp).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        let t = tmp.as_os_str();
        plutil(&[OsStr::new("-replace"), OsStr::new("ProgramArguments.0"), OsStr::new("-string"),
            OsStr::new(exe_s), OsStr::new("--"), t])?;
        if program_key {
            plutil(&[OsStr::new("-replace"), OsStr::new("Program"), OsStr::new("-string"),
                OsStr::new(exe_s), OsStr::new("--"), t])?;
        }
        if let Some(id) = bundle_id {
            let ids = serde_json::json!([id]).to_string();
            plutil(&[OsStr::new("-replace"), OsStr::new("AssociatedBundleIdentifiers"),
                OsStr::new("-json"), OsStr::new(&ids), OsStr::new("--"), t])?;
        }
        plutil(&[OsStr::new("-convert"), OsStr::new("xml1"), OsStr::new("--"), t])?;
        plutil(&[OsStr::new("-lint"), OsStr::new("-s"), OsStr::new("--"), t])?;
        std::fs::File::open(&tmp).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, plist).map_err(|e| e.to_string())?;
        std::fs::File::open(dir).and_then(|f| f.sync_all()).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| decision)
}

/// Write a LaunchAgent plist safely: `plist` must sit directly in the real
/// (canonical, non-symlinked, user-owned) `<home>/Library/LaunchAgents`, an
/// existing destination must be a regular file (never a symlink — writing
/// through one would let it redirect the write), and the content goes to an
/// exclusively created 0600 temp file in the same directory → fsync → rename
/// → directory fsync. The result is re-checked: a regular file owned by this
/// user with mode 0600.
#[cfg(unix)]
pub fn write_launch_agent_plist(home: &Path, plist: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let refuse = |why: String| Error::new(ErrorKind::PermissionDenied, why);
    let uid = unsafe { libc::getuid() };
    let dir = plist.parent().ok_or_else(|| refuse("LaunchAgent plist has no parent directory".into()))?;
    let name = plist.file_name().and_then(|n| n.to_str())
        .filter(|n| n.ends_with(".plist") && !n.starts_with('.'))
        .ok_or_else(|| refuse(format!("{} is not a LaunchAgent plist name", plist.display())))?;
    let expected = std::fs::canonicalize(home)?.join("Library").join("LaunchAgents");
    let dir_meta = std::fs::symlink_metadata(dir)?;
    if !dir_meta.is_dir() || std::fs::canonicalize(dir)? != expected {
        return Err(refuse(format!("{} is not the user's ~/Library/LaunchAgents", dir.display())));
    }
    if dir_meta.uid() != uid || dir_meta.mode() & 0o022 != 0 {
        return Err(refuse(format!("{} must be owned by this user and not group/world-writable", dir.display())));
    }
    match std::fs::symlink_metadata(plist) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(refuse(format!("{} exists and is not a regular file", plist.display())));
        }
        Ok(meta) if meta.uid() != uid => {
            return Err(refuse(format!("{} is owned by another user", plist.display())));
        }
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        // Exactly 0600 regardless of the umask.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, plist)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    let meta = std::fs::symlink_metadata(plist)?;
    if !meta.file_type().is_file() || meta.uid() != uid || meta.mode() & 0o7777 != 0o600 {
        return Err(refuse(format!("{} was not written as a private regular file", plist.display())));
    }
    Ok(())
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
        let bundle_id = crate::macos_bundle::running_gateway_bundle()
            .map(|_| crate::macos_bundle::BUNDLE_ID);
        write_launch_agent_plist(&home, &plist_path,
            launchd_plist_for(&exe, &home, &state, bundle_id).as_bytes())?;
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

    // ── LaunchAgent plist writer ─────────────────────────────────────────

    #[cfg(unix)]
    fn launch_agents_home() -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        (home, dir)
    }

    #[cfg(unix)]
    #[test]
    fn launch_agent_plist_is_written_private_and_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let (home, dir) = launch_agents_home();
        let plist = dir.join("com.duduclaw.gateway.plist");
        write_launch_agent_plist(home.path(), &plist, b"first").unwrap();
        assert_eq!(std::fs::read(&plist).unwrap(), b"first");
        std::fs::set_permissions(&plist, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_launch_agent_plist(home.path(), &plist, b"second").unwrap();
        assert_eq!(std::fs::read(&plist).unwrap(), b"second");
        assert_eq!(std::fs::metadata(&plist).unwrap().permissions().mode() & 0o7777, 0o600);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp files left behind");
    }

    #[cfg(unix)]
    #[test]
    fn launch_agent_plist_refuses_symlinks_and_foreign_directories() {
        let (home, dir) = launch_agents_home();
        let plist = dir.join("com.duduclaw.gateway.plist");
        // A symlinked destination is never written through.
        let victim = home.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, &plist).unwrap();
        assert!(write_launch_agent_plist(home.path(), &plist, b"evil").is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(std::fs::symlink_metadata(&plist).unwrap().file_type().is_symlink());
        std::fs::remove_file(&plist).unwrap();
        // A directory at the destination is refused.
        std::fs::create_dir(&plist).unwrap();
        assert!(write_launch_agent_plist(home.path(), &plist, b"x").is_err());
        std::fs::remove_dir(&plist).unwrap();
        // Any other directory than ~/Library/LaunchAgents is refused.
        let other = home.path().join("Library/Other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(write_launch_agent_plist(home.path(), &other.join("x.plist"), b"x").is_err());
        assert!(!other.join("x.plist").exists());
        // ~/Library/LaunchAgents itself being a symlink elsewhere is refused.
        std::fs::remove_dir(&dir).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &dir).unwrap();
        assert!(write_launch_agent_plist(home.path(), &plist, b"x").is_err());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn launch_agent_plist_refuses_a_world_writable_directory() {
        use std::os::unix::fs::PermissionsExt;
        let (home, dir) = launch_agents_home();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(write_launch_agent_plist(home.path(), &dir.join("a.plist"), b"x").is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

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
    fn launchd_plist_names_associated_bundle_only_for_bundle_installs() {
        let bare = launchd_plist(Path::new("/x/duduclaw"), Path::new("/Users/kai"), Path::new("/s"));
        assert!(!bare.contains("AssociatedBundleIdentifiers"));
        let exe = Path::new("/Users/kai/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw");
        let app = launchd_plist_for(exe, Path::new("/Users/kai"), Path::new("/s"),
            Some(crate::macos_bundle::BUNDLE_ID));
        assert!(app.contains("<key>AssociatedBundleIdentifiers</key>\n    <array>\n        <string>ai.duduclaw.gateway</string>\n    </array>"));
        assert!(app.contains("<string>/Users/kai/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw</string>"));
    }

    #[test]
    fn plist_migration_rewrites_only_aliases_of_the_running_exe() {
        let target = Path::new("/A/DuDuClaw Gateway.app/Contents/MacOS/duduclaw");
        let alias = |p: &Path| p == Path::new("/Users/k/.duduclaw/bin/duduclaw");
        let t = target.to_str().unwrap();
        assert_eq!(plan_plist_migration(Some(t), None, true, target, true, alias), PlistMigration::UpToDate);
        assert_eq!(plan_plist_migration(Some(t), None, false, target, false, alias), PlistMigration::UpToDate);
        // Exact path but missing the bundle key → rewrite (adds the key).
        assert_eq!(plan_plist_migration(Some(t), None, false, target, true, alias),
            PlistMigration::Rewrite { program_key: false });
        assert_eq!(plan_plist_migration(Some("/Users/k/.duduclaw/bin/duduclaw"), None, false, target, true, alias),
            PlistMigration::Rewrite { program_key: false });
        assert_eq!(plan_plist_migration(Some("/Users/k/.duduclaw/bin/duduclaw"),
            Some("/Users/k/.duduclaw/bin/duduclaw"), false, target, false, alias),
            PlistMigration::Rewrite { program_key: true });
        // Another binary is never touched.
        assert!(matches!(plan_plist_migration(Some("/opt/other/duduclaw"), None, false, target, true, alias),
            PlistMigration::Foreign(_)));
        assert!(matches!(plan_plist_migration(Some(t), Some("/tmp/evil"), false, target, true, alias),
            PlistMigration::Foreign(_)));
        assert!(matches!(plan_plist_migration(None, None, false, target, true, alias),
            PlistMigration::Foreign(_)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_migration_rewrites_symlinked_program_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-duduclaw");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.path().join("link-duduclaw");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let real = std::fs::canonicalize(&real).unwrap();
        let plist = dir.path().join("com.duduclaw.gateway.plist");
        std::fs::write(&plist, launchd_plist(&link, Path::new("/Users/kai"), dir.path())).unwrap();
        let outcome = migrate_one_plist(&plist, &real, Some(crate::macos_bundle::BUNDLE_ID)).unwrap();
        assert_eq!(outcome, PlistMigration::Rewrite { program_key: false });
        let text = std::fs::read_to_string(&plist).unwrap();
        assert!(text.contains(real.to_str().unwrap()));
        assert!(text.contains("AssociatedBundleIdentifiers"));
        assert!(text.contains("<string>--yes</string>"));
        assert_eq!(migrate_one_plist(&plist, &real, Some(crate::macos_bundle::BUNDLE_ID)).unwrap(),
            PlistMigration::UpToDate);
        let other = dir.path().join("other");
        std::fs::write(&other, b"y").unwrap();
        assert!(matches!(migrate_one_plist(&plist, &std::fs::canonicalize(&other).unwrap(), None).unwrap(),
            PlistMigration::Foreign(_)));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
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
