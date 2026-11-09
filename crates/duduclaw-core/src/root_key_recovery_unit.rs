//! Shared installer contract and pinned readback of the root-managed unit.
use std::path::Path;

pub fn protected_unit_text() -> String {
    let recovery_exit = crate::root_key::RECOVERY_RESTART_EXIT_CODE;
    let launcher = "/usr/local/libexec/duduclaw/protected-launcher";
    let binary = "/opt/duduclaw/bin/duduclaw";
    format!(r"[Unit]
Description=DuDuClaw protected gateway
After=network-online.target
Wants=network-online.target
# Root-managed authority: at most four starts per hour, shared by recovery,
# normal failures and manual starts. Even after an old long-lived start expires,
# recovery cannot obtain a fifth start. The home JSON is only advisory.
StartLimitIntervalSec=3600
StartLimitBurst=4

[Service]
Slice=duduclaw-tcb.slice
Type=simple
ExecStart={launcher} {binary} run --yes
# Stop every descendant before a fresh key handoff.
KillMode=control-group
Restart=on-failure
RestartForceExitStatus={recovery_exit}
RestartSec=5
# Only service data and the root key provisioning directory are writable.
ProtectSystem=strict
ReadWritePaths=/var/lib/duduclaw /var/lib/duduclaw-protected
ProtectHome=yes
PrivateTmp=yes
# Hide common engine endpoints; '-' tolerates hosts without those paths.
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
# Gateway tools need pseudo devices, not host hardware or raw block devices.
PrivateDevices=yes
# Neither the launcher nor gateway needs to change kernel settings or modules.
ProtectKernelTunables=yes
ProtectKernelModules=yes
# PID 1 owns the unit cgroup; the gateway must not modify its hierarchy.
ProtectControlGroups=yes
# The launcher drops IDs directly; it does not need to create set-ID files.
RestrictSUIDSGID=yes
# No execution-domain changes or foreign syscall ABIs are needed.
LockPersonality=yes
SystemCallArchitectures=native
# RestrictNamespaces is intentionally omitted: Codex/bwrap task sandboxes
# need user, mount, PID and network namespaces. Do not set PrivateUsers.
UMask=0077

[Install]
WantedBy=multi-user.target
")
}

/// Byte comparison also detects removed directives, resets and duplicate keys.
pub fn protected_content_differs(installed: Option<&[u8]>, expected: &[u8]) -> bool {
    installed != Some(expected)
}

#[cfg(unix)]
fn read_pinned_unit(root: &Path, name: &std::ffi::CStr) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    fn trusted(file: &std::fs::File, directory: bool) -> std::io::Result<()> {
        let m = file.metadata()?;
        if m.uid() != 0 || m.gid() != 0 || m.mode() & 0o7022 != 0
            || (directory && !m.is_dir()) || (!directory && (!m.is_file() || m.nlink() != 1)) {
            return Err(std::io::Error::other("unsafe protected service unit path"));
        }
        Ok(())
    }
    let mut directory = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root)?;
    trusted(&directory, true)?;
    for name in [c"etc", c"systemd", c"system"] {
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
        trusted(&directory, true)?;
    }
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { return Err(std::io::Error::last_os_error()); }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    trusted(&file, false)?;
    let mut text = String::new();
    file.take(16385).read_to_string(&mut text)?;
    if text.len() > 16384 { return Err(std::io::Error::other("unit too large")); }
    Ok(text)
}

pub(super) fn installed_unit_bounded() -> bool {
    #[cfg(target_os = "linux")]
    { [c"duduclaw-gateway.service", c"duduclaw-gateway.recovery-bound"].iter().all(|name|
        read_pinned_unit(Path::new("/"), name).is_ok_and(|text|
            !protected_content_differs(Some(text.as_bytes()), protected_unit_text().as_bytes()))) }
    #[cfg(not(target_os = "linux"))]
    { false }
}

/// Root installer checks PID 1's loaded properties before certifying recovery.
/// Unknown formatting, duplicate properties and additional forced statuses refuse.
pub fn protected_properties_bounded(text: &str) -> bool {
    let mut interval = None;
    let mut burst = None;
    let mut forced = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { return false; };
        let slot = match key {
            "StartLimitIntervalUSec" => &mut interval,
            "StartLimitBurst" => &mut burst,
            "RestartForceExitStatus" => &mut forced,
            _ => return false,
        };
        if slot.replace(value).is_some() { return false; }
    }
    matches!(interval, Some("1h" | "3600s" | "3600000000")) && burst == Some("4") && forced == Some("75")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_key_effective_properties_require_all_bounds() {
        let good = "StartLimitIntervalUSec=1h\nStartLimitBurst=4\nRestartForceExitStatus=75\n";
        assert!(protected_properties_bounded(good));
        for bad in [good.replace("=1h", "=0"), good.replace("=4", "=0"),
            good.replace("=75", "="), good.replace("StartLimitBurst=4\n", ""),
            format!("{good}StartLimitBurst=4\n")] { assert!(!protected_properties_bounded(&bad)); }
    }
    #[test]
    fn root_key_unit_content_diff_decision() {
        let unit = protected_unit_text();
        assert!(!protected_content_differs(Some(unit.as_bytes()), unit.as_bytes()));
        assert!(protected_content_differs(None, unit.as_bytes()));
        for old in [unit.replace("StartLimitBurst=4", "StartLimitBurst=0"),
            unit.replace("StartLimitIntervalSec=3600\n", ""),
            unit.replace("RestartForceExitStatus=75\n", ""), format!("{unit}\n# edited")] {
            assert!(protected_content_differs(Some(old.as_bytes()), unit.as_bytes()));
        }
        assert!(protected_content_differs(Some(b"old launcher"), b"new launcher"));
    }
    #[cfg(unix)]
    #[test]
    fn root_key_unit_readback_missing_untrusted_and_symlink_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_pinned_unit(root.path(), c"duduclaw-gateway.service").is_err());
        std::fs::create_dir_all(root.path().join("etc/systemd/system")).unwrap();
        let unit = root.path().join("etc/systemd/system/duduclaw-gateway.service");
        std::fs::write(&unit, protected_unit_text()).unwrap();
        // Non-root test fixtures cannot confer root authority.
        if unsafe { libc::geteuid() } != 0 { assert!(read_pinned_unit(root.path(), c"duduclaw-gateway.service").is_err()); }
        std::fs::remove_file(&unit).unwrap();
        std::os::unix::fs::symlink("/dev/null", &unit).unwrap();
        assert!(read_pinned_unit(root.path(), c"duduclaw-gateway.service").is_err());
    }
}
