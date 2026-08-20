//! systemd `sd_notify(3)` support — readiness + watchdog pings for the
//! appliance profile (`Type=notify` + `WatchdogSec=` in the appliance
//! image's `duduclaw-gateway.service`, TODO: not wired up on the unit side
//! yet — see that file's own comment; this module is the gateway-side half
//! it's waiting on).
//!
//! Minimal hand-rolled implementation of the `sd_notify` protocol: connect
//! a `UnixDatagram` to the path named by `$NOTIFY_SOCKET` and send a short
//! ASCII payload (`"READY=1"`, `"WATCHDOG=1"`, ...). No new dependency —
//! the whole protocol is "write bytes to a named AF_UNIX socket".
//!
//! Every function here is a safe no-op whenever `$NOTIFY_SOCKET` is unset —
//! i.e. every non-systemd environment, and every systemd unit that isn't
//! `Type=notify` — so it's safe to call unconditionally from gateway
//! startup regardless of platform or appliance mode. Callers should never
//! branch on `is_appliance()` before calling into this module; the env var
//! gate already does that job.
//!
//! # Known limitation
//!
//! Abstract-namespace notify sockets (`$NOTIFY_SOCKET` starting with `@`,
//! spelling a leading NUL byte per `sd_notify(3)`) are NOT supported — only
//! the ordinary filesystem-path case, which is what systemd sets for a
//! normal service unit (typically `/run/systemd/notify`) and is the
//! overwhelmingly common case. An abstract socket would need
//! `std::os::unix::net::SocketAddr::from_abstract_name` plus
//! `UnixDatagram::send_to_addr`, which this module deliberately doesn't
//! reach for without a concrete need to verify against.

use std::io;

/// Env var systemd sets to the notify socket path for `Type=notify` (or
/// `Type=notify-reload`) units. Absent everywhere else.
pub const NOTIFY_SOCKET_ENV: &str = "NOTIFY_SOCKET";
/// Env var systemd sets (alongside `NOTIFY_SOCKET`) when `WatchdogSec=` is
/// configured — the ping deadline, in microseconds.
pub const WATCHDOG_USEC_ENV: &str = "WATCHDOG_USEC";
/// Optional env var systemd sets when the watchdog ping is scoped to one
/// specific PID (relevant when a unit forks) — if present and it doesn't
/// name this process, this process should NOT send watchdog pings.
pub const WATCHDOG_PID_ENV: &str = "WATCHDOG_PID";

/// Send a raw `sd_notify` payload (e.g. `"READY=1"`) to `$NOTIFY_SOCKET`.
/// `Ok(())` no-op when the env var is unset/empty. Errors are I/O errors
/// from the actual send — callers should treat them as best-effort (log,
/// never fail startup over a missing watchdog channel).
pub fn notify(state: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        notify_to(state, std::env::var(NOTIFY_SOCKET_ENV).ok().as_deref())
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        Ok(())
    }
}

#[cfg(unix)]
fn notify_to(state: &str, socket_path: Option<&str>) -> io::Result<()> {
    let Some(path) = socket_path else { return Ok(()) };
    if path.trim().is_empty() {
        return Ok(());
    }
    let socket = std::os::unix::net::UnixDatagram::unbound()?;
    socket.send_to(state.as_bytes(), path)?;
    Ok(())
}

/// `sd_notify("READY=1")` — tell systemd startup is complete. Call once,
/// right after the gateway has successfully bound its listener (the point
/// at which it's actually ready to serve, matching `Type=notify`'s
/// contract — see `server.rs` for the call site).
pub fn notify_ready() -> io::Result<()> {
    notify("READY=1")
}

/// `sd_notify("STOPPING=1")` — tell systemd graceful shutdown has started.
/// Best-effort; not required by the watchdog contract, just good citizenship.
pub fn notify_stopping() -> io::Result<()> {
    notify("STOPPING=1")
}

/// Pure: given the raw `$WATCHDOG_USEC`/`$WATCHDOG_PID` env values and this
/// process's own PID, decide the ping interval — or `None` if this process
/// is not under watchdog supervision.
///
/// - `WATCHDOG_PID` present and not equal to `my_pid` → not supervised
///   (the ping is meant for a different process in the unit).
/// - `WATCHDOG_USEC` absent, zero, or unparseable → not supervised.
/// - Otherwise → half the deadline, the standard "ping at 2x the required
///   rate" practice recommended by `sd_watchdog_enabled(3)`.
fn watchdog_interval_from(usec: Option<&str>, pid: Option<&str>, my_pid: u32) -> Option<std::time::Duration> {
    if let Some(want_pid) = pid.and_then(|s| s.trim().parse::<u32>().ok())
        && want_pid != my_pid
    {
        return None;
    }
    let usec: u64 = usec?.trim().parse().ok()?;
    if usec == 0 {
        return None;
    }
    Some(std::time::Duration::from_micros(usec / 2))
}

/// Real-env wrapper around [`watchdog_interval_from`].
fn watchdog_interval() -> Option<std::time::Duration> {
    watchdog_interval_from(
        std::env::var(WATCHDOG_USEC_ENV).ok().as_deref(),
        std::env::var(WATCHDOG_PID_ENV).ok().as_deref(),
        std::process::id(),
    )
}

/// Spawn the periodic `WATCHDOG=1` ping task, if (and only if) this process
/// is under systemd watchdog supervision (`WatchdogSec=` configured on a
/// `Type=notify` unit). Returns `None` — spawns nothing — everywhere else,
/// including every non-appliance install. Call once, after
/// [`notify_ready`], from gateway startup.
pub fn spawn_watchdog_pings() -> Option<tokio::task::JoinHandle<()>> {
    let interval = watchdog_interval()?;
    Some(tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        // The first tick fires immediately; the actual READY=1 already
        // happened at startup, so an immediate WATCHDOG=1 here is harmless
        // (systemd treats any ping as "still alive", not just the first).
        loop {
            tick.tick().await;
            if let Err(e) = notify("WATCHDOG=1") {
                tracing::warn!("sd_notify watchdog ping failed: {e}");
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── watchdog_interval_from (pure) ───────────────────────────────

    #[test]
    fn no_usec_means_not_supervised() {
        assert_eq!(watchdog_interval_from(None, None, 100), None);
    }

    #[test]
    fn zero_usec_means_not_supervised() {
        assert_eq!(watchdog_interval_from(Some("0"), None, 100), None);
    }

    #[test]
    fn unparseable_usec_means_not_supervised() {
        assert_eq!(watchdog_interval_from(Some("nope"), None, 100), None);
    }

    #[test]
    fn valid_usec_pings_at_half_the_deadline() {
        let interval = watchdog_interval_from(Some("20000000"), None, 100).unwrap();
        assert_eq!(interval, std::time::Duration::from_micros(10_000_000));
    }

    #[test]
    fn pid_mismatch_means_not_supervised() {
        assert_eq!(watchdog_interval_from(Some("20000000"), Some("999"), 100), None);
    }

    #[test]
    fn pid_match_is_supervised() {
        let interval = watchdog_interval_from(Some("20000000"), Some("100"), 100).unwrap();
        assert_eq!(interval, std::time::Duration::from_micros(10_000_000));
    }

    #[test]
    fn malformed_pid_is_ignored_not_fatal() {
        // A garbage WATCHDOG_PID shouldn't crash the decision — falls back
        // to "no PID constraint" rather than refusing to supervise.
        let interval = watchdog_interval_from(Some("20000000"), Some("not-a-pid"), 100).unwrap();
        assert_eq!(interval, std::time::Duration::from_micros(10_000_000));
    }

    // ── notify (real env, no real systemd socket) ───────────────────

    #[test]
    fn notify_is_noop_without_notify_socket_env() {
        // No env manipulation needed — CI/dev hosts never have
        // NOTIFY_SOCKET set (that's exclusively a systemd Type=notify
        // thing), so this exercises the real no-op path deterministically.
        assert!(std::env::var(NOTIFY_SOCKET_ENV).is_err(), "precondition: not running under systemd notify");
        assert!(notify("READY=1").is_ok());
    }

    #[test]
    fn spawn_watchdog_pings_is_none_without_watchdog_usec() {
        assert!(std::env::var(WATCHDOG_USEC_ENV).is_err(), "precondition: no watchdog configured");
        assert!(spawn_watchdog_pings().is_none());
    }
}
