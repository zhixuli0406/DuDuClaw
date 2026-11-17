//! Root-managed native Linux service. All privileged operations use fixed paths.
use duduclaw_core::error::{DuDuClawError, Result};
use std::path::PathBuf;

#[cfg(target_os = "linux")]
#[path = "protected_linux_install.rs"]
mod linux;
#[cfg(any(target_os = "linux", test))]
#[path = "protected_linux_layout.rs"]
mod layout;
#[cfg(any(target_os = "linux", test))]
#[path = "protected_linux_lifecycle.rs"]
mod lifecycle;
#[cfg(target_os = "linux")]
#[path = "protected_linux_migration.rs"]
mod native_migration;
// v1.83 (owner decision Q7): `--migrate-from` is refused for every protected
// install, so the copier is no longer called. It stays compiled (and tested)
// until R5 decides whether any of it is reused by the reviewed migration.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "protected_linux_migrate.rs"]
mod migrate;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[path = "protected_linux_runtime.rs"]
pub(crate) mod runtime;

#[path = "protected_linux_policy.rs"]
pub(crate) mod policy;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct InstallOptions {
    pub enable_now: bool,
    pub i_verified_this_binary: bool,
    pub migrate_from: Option<PathBuf>,
    pub force_migrate: bool,
    pub allow_missing_runtime: bool,
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
const LAUNCHER_TEXT: &str = include_str!("../../assets/native-protected-launcher.sh");

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

pub(crate) async fn upgrade(version: Option<&str>, allow_missing_runtime: bool) -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::upgrade(version, allow_missing_runtime).await }
    #[cfg(not(target_os = "linux"))]
    { let _ = (version, allow_missing_runtime); Err(refused("--protected-linux requires native Linux")) }
}

pub(crate) fn uninstall() -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::uninstall() }
    #[cfg(not(target_os = "linux"))]
    { Err(refused("--protected-linux requires native Linux")) }
}

pub(crate) fn readiness() -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::readiness() }
    #[cfg(not(target_os = "linux"))]
    { Err(refused("--protected-linux requires native Linux")) }
}

/// Typed arguments of the hidden `service bootstrap-continue` command, exactly
/// as `duduclaw-bootstrap` forwards them (no path is ever accepted).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct BootstrapContinueArgs {
    /// `install`, `upgrade` or `rollback`.
    pub mode: String,
    /// The staged generation: 64 lowercase hex digits.
    pub generation: String,
    pub dns_upstream: Option<std::net::SocketAddr>,
    pub host_public_address: Vec<std::net::IpAddr>,
    pub enable_now: bool,
}

/// v1.83 isolation bootstrap continuation (plan §4.3), run by the verified
/// gateway that `duduclaw-bootstrap` executes from a sealed memfd.
pub(crate) fn bootstrap_continue(args: BootstrapContinueArgs) -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::bootstrap_continue(linux::ContinueOptions::from_args(args)?) }
    #[cfg(not(target_os = "linux"))]
    { let _ = args; Err(refused("--protected-linux requires native Linux")) }
}

/// Roll back an uncommitted isolation bootstrap (plan §4.3).
pub(crate) fn bootstrap_rollback() -> Result<()> {
    operator_gate()?;
    #[cfg(target_os = "linux")]
    { linux::bootstrap_rollback() }
    #[cfg(not(target_os = "linux"))]
    { Err(refused("--protected-linux requires native Linux")) }
}

fn operator_gate() -> Result<()> {
    if !crate::ai_session_guard::command_markers().is_empty() {
        return Err(refused("AI 員工不能安裝、升級、移除受保護服務或變更其政策，請由管理者在自己的終端機操作。"));
    }
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unit_text() -> String {
    // The canonical text (including the TCB slice spawnd verifies) lives in core.
    duduclaw_core::root_key::recovery::protected_unit_text()
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
