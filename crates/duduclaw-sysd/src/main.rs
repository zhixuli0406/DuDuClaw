//! `duduclaw-sysd` binary entrypoint.
//!
//! Production posture: launched by systemd as `root`, listening on
//! `/run/duduclaw/sysd.sock` (0600, chown'd to the allowed uid; parent dir
//! 0711 — see `server::bind`'s doc comment for why the file layer must
//! admit the same audience the SO_PEERCRED gate does), only ever usable
//! by the uid passed via `--allowed-uid` / `DUDUCLAW_SYSD_ALLOWED_UID`
//! (normally the `duduclaw` service user's uid). Dev/test posture: run as
//! any user, socket path overridden via `--socket` / `DUDUCLAW_SYSD_SOCKET`
//! so nothing needs write access to `/run`.
//!
//! Runs until SIGTERM/SIGINT, matching the shutdown convention
//! the former `duduclaw-cli-worker` binary used (that crate was removed in
//! 2026-09 with the PTY pool; see git history at the v1.65 tag).

use std::path::PathBuf;

use clap::Parser;
use duduclaw_sysd::{SysdServerConfig, bind, serve};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "duduclaw-sysd",
    version,
    about = "DuDuClaw appliance privilege-separated root system service"
)]
struct Cli {
    /// UDS path to bind. Defaults to `$DUDUCLAW_SYSD_SOCKET` if set, else
    /// `/run/duduclaw/sysd.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// The single uid permitted to call this server. Defaults to
    /// `$DUDUCLAW_SYSD_ALLOWED_UID` if set. Omitting BOTH is a legal
    /// startup (the process still runs, as a resident systemd service
    /// should) but denies every single connection — fail-closed, not
    /// fail-to-boot. See `protocol::ALLOWED_UID_ENV`.
    #[arg(long)]
    allowed_uid: Option<u32>,

    /// Also require the caller to be the main process (systemd `MainPID`)
    /// of this unit, e.g. `duduclaw-gateway.service`. Needed once the
    /// gateway's uid is shared with the AI employees' CLIs: a uid check
    /// alone admits them. Verified through the peer's pidfd (Linux 6.5+,
    /// `SO_PEERPIDFD`) plus the unit's cgroup. Fail-closed: a MainPID or
    /// pidfd that cannot be read, a non-Linux host, or an
    /// invalid unit name, denies every call. Absent ⇒ uid check only (older
    /// OS images). Relies on yama `ptrace_scope >= 1`; see `protocol.rs`.
    #[arg(long, value_name = "UNIT")]
    require_main_pid_of: Option<String>,

    /// Refuse `update_rollback` when any entry it could boot is older than
    /// this OS version (or its version cannot be read from the entry's
    /// unified kernel image). The OS sets it to its first release whose
    /// gateway runs unprivileged, so a rollback can never bring back a
    /// root-running gateway. A value that does not parse refuses every
    /// rollback. Absent ⇒ unchanged (older OS images).
    #[arg(long, value_name = "VERSION")]
    min_rollback_version: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let socket_path = cli.socket.unwrap_or_else(duduclaw_sysd::resolve_socket_path);
    let allowed_uid = cli.allowed_uid.or_else(resolve_allowed_uid_from_env);

    if allowed_uid.is_none() {
        warn!(
            "sysd: no --allowed-uid / DUDUCLAW_SYSD_ALLOWED_UID configured — \
             every connection will be denied until this is set"
        );
    }
    #[cfg(unix)]
    {
        // SAFETY: `getuid()` has no preconditions and cannot fail.
        let self_uid = unsafe { libc::getuid() };
        if self_uid != 0 {
            warn!(
                self_uid,
                "sysd: not running as root — reboot/poweroff/sysupdate/hostname \
                 shell-outs will fail with a permission error at the OS level. \
                 Expected in dev/test; must be root in the appliance image."
            );
        }
    }

    let require_main_pid_of = cli.require_main_pid_of;
    #[cfg(unix)]
    {
        let floor = duduclaw_sysd::rollback_floor::RollbackFloor::from_flag(cli.min_rollback_version.as_deref());
        if let duduclaw_sysd::rollback_floor::RollbackFloor::Invalid(raw) = &floor {
            warn!(value = %raw, "sysd: --min-rollback-version is not a version; every rollback will be refused");
        }
        info!(floor = ?floor, "sysd: rollback floor");
        duduclaw_sysd::rollback_floor::set_floor(floor);
    }
    // Maintenance SSH: take SSH_LOCK before the socket accepts anything, so
    // the startup cleanup of a window left by a previous run (R5) happens
    // before any SSH verb; the same task then runs the 15 s tick (R4).
    #[cfg(unix)]
    {
        let first = duduclaw_sysd::maint_ssh::SSH_LOCK.lock().await;
        let supervisor = tokio::spawn(duduclaw_sysd::maint_ssh::run_supervisor(first));
        // The supervisor never returns; if it panics, nobody enforces window
        // deadlines any more, so exit and let systemd restart sysd (whose
        // startup cleanup then closes the window).
        tokio::spawn(async move {
            let outcome = supervisor.await;
            tracing::error!(?outcome, "sysd: maintenance SSH supervisor stopped; exiting so systemd restarts sysd");
            std::process::exit(70);
        });
    }
    info!(socket = %socket_path.display(), allowed_uid = ?allowed_uid, require_main_pid_of = ?require_main_pid_of, "sysd: starting");
    let listener = bind(&socket_path, allowed_uid)?;
    info!(socket = %socket_path.display(), "sysd: listening");

    let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid, require_main_pid_of };
    serve(listener, config, shutdown_signal()).await?;
    // A reboot stops sysd with SIGTERM: write the maintenance SSH events read
    // so far and save the cursor before going (bounded; the next startup
    // replays from the saved cursor anyway).
    #[cfg(unix)]
    duduclaw_sysd::maint_ssh::shutdown_flush(std::time::Duration::from_secs(5)).await;
    info!("sysd: clean shutdown");
    Ok(())
}

fn resolve_allowed_uid_from_env() -> Option<u32> {
    std::env::var(duduclaw_sysd::ALLOWED_UID_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
}

fn init_tracing() {
    // Plain stderr writer, no ANSI, no target module noise beyond the
    // default — systemd captures a unit's stdout/stderr into the journal
    // by default (StandardOutput=/StandardError=journal is systemd's own
    // default per systemd.exec(5)), so nothing beyond writing to stderr is
    // needed to get these events into `journalctl -u duduclaw-sysd`.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => info!("sysd: SIGTERM received"),
        _ = sigint.recv() => info!("sysd: SIGINT received"),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    // duduclaw-sysd is Linux-appliance-only in production, but keep this
    // crate compiling everywhere for workspace-wide `cargo build`/CI
    // parity with every other crate here.
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases live in ONE test (rather than two) because both mutate
    // the same process-global env var — cargo runs tests in a binary
    // concurrently by default, and two separate tests each doing
    // set/assert/restore around the same var could interleave and flip
    // each other's value mid-assertion.
    #[test]
    fn resolve_allowed_uid_from_env_parses_valid_uid_and_rejects_garbage() {
        let saved = std::env::var_os(duduclaw_sysd::ALLOWED_UID_ENV);

        unsafe {
            std::env::set_var(duduclaw_sysd::ALLOWED_UID_ENV, "1000");
        }
        assert_eq!(resolve_allowed_uid_from_env(), Some(1000));

        unsafe {
            std::env::set_var(duduclaw_sysd::ALLOWED_UID_ENV, "not-a-uid");
        }
        assert_eq!(resolve_allowed_uid_from_env(), None);

        unsafe {
            match &saved {
                Some(v) => std::env::set_var(duduclaw_sysd::ALLOWED_UID_ENV, v),
                None => std::env::remove_var(duduclaw_sysd::ALLOWED_UID_ENV),
            }
        }
    }
}
