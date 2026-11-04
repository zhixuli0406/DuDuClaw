//! Root-managed native Linux service. All privileged operations use fixed paths.
use duduclaw_core::error::{DuDuClawError, Result};
use std::path::PathBuf;

#[cfg(target_os = "linux")]
#[path = "protected_linux_install.rs"]
mod linux;
#[cfg(target_os = "linux")]
#[path = "protected_linux_migrate.rs"]
mod migrate;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct InstallOptions {
    pub enable_now: bool,
    pub i_verified_this_binary: bool,
    pub migrate_from: Option<PathBuf>,
    pub force_migrate: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const UNIT: &str = "duduclaw-gateway.service";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const BINARY: &str = "/opt/duduclaw/bin/duduclaw";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LAUNCHER: &str = "/usr/local/libexec/duduclaw/protected-launcher";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const UNIT_PATH: &str = "/etc/systemd/system/duduclaw-gateway.service";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LAUNCHER_TEXT: &str = include_str!("../../../../container/native-protected-launcher.sh");

fn refused(reason: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Config(format!("Protected Linux service refused: {reason}"))
}

pub(crate) fn install(options: InstallOptions) -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::install(options) }
    #[cfg(not(target_os = "linux"))]
    { let _ = options; Err(refused("--protected-linux requires native Linux")) }
}

pub(crate) async fn upgrade(version: Option<&str>) -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::upgrade(version).await }
    #[cfg(not(target_os = "linux"))]
    { let _ = version; Err(refused("--protected-linux requires native Linux")) }
}

pub(crate) fn uninstall() -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::uninstall() }
    #[cfg(not(target_os = "linux"))]
    { Err(refused("--protected-linux requires native Linux")) }
}

fn operator_gate() -> Result<()> {
    if !crate::ai_session_guard::command_markers().is_empty() {
        return Err(refused("AI 員工不能安裝、升級或移除受保護服務，請由管理者在自己的終端機操作。"));
    }
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unit_text() -> String {
    format!(r"[Unit]
Description=DuDuClaw protected gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={LAUNCHER} {BINARY} run --yes
# Stop every descendant before a fresh key handoff.
KillMode=control-group
Restart=on-failure
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn validate_account(uid: u32, gid: u32, shell: &str, groups: &str) -> Result<()> {
    if uid == 0 || gid == 0 { return Err(refused("duduclaw must have a nonzero uid and gid")); }
    if !matches!(shell, "/usr/sbin/nologin" | "/sbin/nologin" | "/bin/false" | "/usr/bin/false") {
        return Err(refused("duduclaw must have a nologin or false shell"));
    }
    if groups.split_whitespace().next().is_none() { return Err(refused("cannot verify account groups")); }
    for group in groups.split_whitespace() {
        if matches!(group, "sudo" | "wheel" | "admin" | "adm" | "docker" | "lxd" | "incus" | "libvirt") {
            return Err(refused(format!("duduclaw belongs to privileged group {group}")));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "protected_linux_tests.rs"]
mod tests;
