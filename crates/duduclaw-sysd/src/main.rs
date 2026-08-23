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
//! `duduclaw-cli-worker`'s binary uses (see its `main.rs`).

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

    info!(socket = %socket_path.display(), allowed_uid = ?allowed_uid, "sysd: starting");
    let listener = bind(&socket_path, allowed_uid)?;
    info!(socket = %socket_path.display(), "sysd: listening");

    let config = SysdServerConfig { socket_path: socket_path.clone(), allowed_uid };
    serve(listener, config, shutdown_signal()).await?;
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
