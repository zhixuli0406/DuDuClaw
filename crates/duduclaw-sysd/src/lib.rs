//! `duduclaw-sysd` — the DuDuClaw appliance image's privilege-separated
//! root system service.
//!
//! See `commercial/docs/specs/DESIGN-agent-os-native-apps-2026-08.md` §2 L1 for
//! the design context (internal doc, not part of this public crate — this
//! crate's own docs are self-contained). In short: the DuDuClaw gateway
//! process runs as an unprivileged `duduclaw` user and has no business
//! holding root — but a handful of operations (reboot, poweroff, applying
//! a `systemd-sysupdate` OS image, re-arming first-boot provisioning for a
//! factory reset, changing the hostname) genuinely require root. Rather
//! than run the whole gateway as root, or grant it broad sudo/polkit
//! rules, this crate is a small standalone root daemon exposing a closed
//! set of fixed verbs over a Unix domain socket gated by peer-credential
//! (`SO_PEERCRED`) verification. Only the configured uid can reach it, and
//! that uid can only ever ask for one of the fixed things, never an
//! arbitrary command.
//!
//! The configured uid is the gateway's, which on DuDuClaw OS is also the
//! uid the AI employees' CLIs run as, so a uid match alone admits any agent.
//! `--require-main-pid-of <unit>` narrows the caller to the gateway process
//! itself (its systemd MainPID); see [`protocol`]'s "threat model" section
//! for what that relies on. Without it, an agent reaching the socket can
//! trigger every verb, including `update_rollback` (boots the previous
//! image, which may still run its gateway as root) and `ssh_service_start`.
//! Parameterised verbs validate hostile input either way.
//!
//! Module map:
//! - [`protocol`] — the wire schema (closed request enum, response
//!   envelope, socket-path/allowed-uid env resolution). Shared by both
//!   halves of the socket.
//! - [`dispatch`] — verb → hardcoded `Command::new(...)` shell-out. Only
//!   reached AFTER a request is both peer-cred-authorized and successfully
//!   parsed against the closed enum.
//! - [`server`] — UDS bind + accept loop + per-connection peer-cred check
//!   + audit tracing. Runs as root in production (see `main.rs`).
//! - [`client`] — the gateway-side (or test-side) caller SDK.
//! - [`ssh_key`] — the strict parser for the one public key a maintenance
//!   SSH window accepts (shared with the gateway's early check).
//! - `maint_ssh` (Unix) — the maintenance SSH window: OS checks, key file,
//!   deadline, PID 1 fallback timer, journal events into the root audit file,
//!   and the `SSH_LOCK` every SSH verb, the tick and the startup cleanup
//!   share.

pub mod client;
#[cfg(unix)]
pub mod dispatch;
#[cfg(unix)]
pub mod maint_ssh;
pub mod release;
#[cfg(unix)]
pub mod rollback_floor;
#[cfg(unix)]
pub mod staged_update;
pub mod protocol;
#[cfg(unix)]
pub mod server;
pub mod ssh_key;

/// Non-unix stand-in for [`server`] — `duduclaw-sysd` is a Linux-appliance
/// root daemon (UDS + `SO_PEERCRED`), so on other targets the server surface
/// compiles to a fail-closed stub: `bind` refuses at startup with a clear
/// error, never a silent no-op. Exists so workspace-wide builds (the Windows
/// release CI matrix — the v1.62.0 tag build broke here) keep compiling,
/// matching `main.rs`'s already-stated "keep this crate compiling
/// everywhere" intent.
#[cfg(not(unix))]
pub mod server {
    use std::io;
    use std::path::Path;

    #[derive(Debug, Clone)]
    pub struct SysdServerConfig {
        pub socket_path: std::path::PathBuf,
        pub allowed_uid: Option<u32>,
        pub require_main_pid_of: Option<String>,
    }

    /// Never constructible — [`bind`] always errors on non-unix targets.
    pub struct SysdListener(());

    /// Signature mirrors the unix `server::bind(socket_path, allowed_uid)`
    /// exactly — `main.rs` calls it unconditionally, so a drift here is a
    /// Windows-only compile error the unix CI legs never see (the v1.63.0
    /// / v1.64.0 windows CI leg broke on exactly that).
    pub fn bind(_socket_path: &Path, _allowed_uid: Option<u32>) -> io::Result<SysdListener> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "duduclaw-sysd requires Unix domain sockets — it only runs on the Linux appliance image",
        ))
    }

    pub async fn serve(
        _listener: SysdListener,
        _config: SysdServerConfig,
        _shutdown: impl std::future::Future<Output = ()>,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "duduclaw-sysd requires Unix domain sockets — it only runs on the Linux appliance image",
        ))
    }
}

pub use client::{SysdClient, SysdClientError};
pub use protocol::{
    ALLOWED_UID_ENV, DEFAULT_SOCKET_PATH, FACTORY_RESET_MARKER, FACTORY_RESET_UNIT_FILE, GATEWAY_HOME_DIR,
    LOCAL_MODEL_UNIT, LocalModelAction, SOCKET_PATH_ENV,
    SSH_PORT, SSH_UNITS, SshUnit, SysdError,
    SysdOpOutput, SysdRequest, SysdResponse, resolve_socket_path,
};
pub use server::{SysdServerConfig, bind, serve};
