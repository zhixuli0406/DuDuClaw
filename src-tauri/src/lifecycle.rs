//! Sidecar lifecycle helpers (TODO-genspark-workspace-shell §D1 / §D2).
//!
//! Pure, side-effect-light functions so the tricky parts (port selection, PATH
//! augmentation, pidfile location, health URL) are unit-testable independently
//! of Tauri. `main.rs` wires these into the app + tray lifecycle.

use std::ffi::OsString;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

/// The gateway's default loopback host and port. Mirrors the CLI:
/// `DUDUCLAW_PORT` env override, else 18789 (see duduclaw-cli `lib.rs`).
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 18789;

/// Resolve the configured port: `DUDUCLAW_PORT` if set & valid, else default.
pub fn configured_port() -> u16 {
    std::env::var("DUDUCLAW_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p >= 1024)
        .unwrap_or(DEFAULT_PORT)
}

/// Ordered candidate ports to try when the preferred one is busy: the preferred
/// port first, then a small deterministic fallback band. (§D2.2)
pub fn candidate_ports(preferred: u16) -> Vec<u16> {
    let mut v = vec![preferred];
    for delta in 1..=8u16 {
        if let Some(p) = preferred.checked_add(delta) {
            v.push(p);
        }
    }
    v
}

/// True if *something* is already listening on host:port — used both to detect
/// an existing gateway (attach instead of spawn, §D1) and to find a free port.
pub fn is_listening(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    match addr.to_socket_addrs() {
        Ok(mut addrs) => addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(250)).is_ok()),
        Err(_) => false,
    }
}

/// How the desktop app should obtain a running gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayPlan {
    /// A gateway is already serving on this port — attach, do not spawn (§D1).
    Attach { port: u16 },
    /// No gateway found; spawn the sidecar bound to this free port (§D2.2).
    Spawn { port: u16 },
}

/// Decide whether to attach to an existing gateway or spawn a sidecar, and on
/// which port. The preferred (configured) port is checked first: if it's live
/// we attach; otherwise we pick the first free candidate to spawn on.
pub fn plan_gateway(host: &str, preferred: u16) -> GatewayPlan {
    if is_listening(host, preferred) {
        return GatewayPlan::Attach { port: preferred };
    }
    for port in candidate_ports(preferred) {
        if !is_listening(host, port) {
            return GatewayPlan::Spawn { port };
        }
    }
    // Extremely unlikely: every candidate busy but none is "our" gateway.
    GatewayPlan::Spawn { port: preferred }
}

/// The health endpoint to poll for readiness / liveness (§D2.5). The gateway's
/// dashboard server answers `/healthz` without auth.
pub fn health_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/healthz")
}

/// `~/.duduclaw` — shared by the CLI and the desktop app (§D2.7). Honors
/// `DUDUCLAW_HOME` if set.
pub fn duduclaw_home() -> PathBuf {
    if let Ok(h) = std::env::var("DUDUCLAW_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".duduclaw")
}

/// Pidfile recording the sidecar we spawned, so a later launch (or a crash
/// recovery pass) can reclaim an orphaned process (§D2.1 / §D2.3).
pub fn sidecar_pidfile() -> PathBuf {
    duduclaw_home().join("desktop-sidecar.pid")
}

/// Directories a GUI launch (Finder/Dock) typically misses because it does not
/// inherit the shell PATH. Mirrors the gateway's `which_claude_in_home()` probe
/// so the sidecar can still find Claude CLI / node / containers (§D2.6).
pub fn extra_path_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let h = |s: &str| PathBuf::from(&home).join(s);
    vec![
        PathBuf::from("/opt/homebrew/bin"), // Apple Silicon Homebrew
        PathBuf::from("/usr/local/bin"),    // Intel Homebrew / common
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        h(".local/bin"),
        h(".bun/bin"),
        h(".volta/bin"),
        h(".npm-global/bin"),
        h(".asdf/shims"),
        h(".cargo/bin"),
    ]
}

/// Build a PATH that prepends `extra_path_dirs()` to the inherited PATH,
/// de-duplicated, so the spawned sidecar discovers user-installed tooling even
/// under a Finder/Dock launch (§D2.6).
pub fn augmented_path() -> OsString {
    let mut seen = std::collections::HashSet::new();
    let mut parts: Vec<PathBuf> = Vec::new();
    for d in extra_path_dirs() {
        if seen.insert(d.clone()) {
            parts.push(d);
        }
    }
    if let Some(existing) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&existing) {
            if seen.insert(d.clone()) {
                parts.push(d);
            }
        }
    }
    std::env::join_paths(parts).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_ports_starts_with_preferred_and_bands() {
        let c = candidate_ports(18789);
        assert_eq!(c[0], 18789);
        assert_eq!(c.len(), 9);
        assert_eq!(c[1], 18790);
        assert_eq!(*c.last().unwrap(), 18797);
    }

    #[test]
    fn candidate_ports_saturates_near_u16_max() {
        let c = candidate_ports(u16::MAX - 2);
        // No overflow panic; only valid ports retained.
        assert_eq!(c[0], u16::MAX - 2);
        assert!(c.iter().all(|p| *p >= u16::MAX - 2));
    }

    #[test]
    fn health_url_is_well_formed() {
        assert_eq!(health_url("127.0.0.1", 18789), "http://127.0.0.1:18789/healthz");
    }

    #[test]
    fn configured_port_defaults_when_unset() {
        // Snapshot + restore to avoid cross-test contamination.
        let prev = std::env::var("DUDUCLAW_PORT").ok();
        std::env::remove_var("DUDUCLAW_PORT");
        assert_eq!(configured_port(), DEFAULT_PORT);
        std::env::set_var("DUDUCLAW_PORT", "12345");
        assert_eq!(configured_port(), 12345);
        // Privileged / invalid → default.
        std::env::set_var("DUDUCLAW_PORT", "80");
        assert_eq!(configured_port(), DEFAULT_PORT);
        std::env::set_var("DUDUCLAW_PORT", "not-a-port");
        assert_eq!(configured_port(), DEFAULT_PORT);
        match prev {
            Some(v) => std::env::set_var("DUDUCLAW_PORT", v),
            None => std::env::remove_var("DUDUCLAW_PORT"),
        }
    }

    #[test]
    fn augmented_path_prepends_extra_dirs_without_dupes() {
        let path = augmented_path();
        let dirs: Vec<_> = std::env::split_paths(&path).collect();
        let mut seen = std::collections::HashSet::new();
        for d in &dirs {
            assert!(seen.insert(d.clone()), "duplicate path entry: {d:?}");
        }
    }

    #[test]
    fn pidfile_under_home() {
        let p = sidecar_pidfile();
        assert!(p.ends_with("desktop-sidecar.pid"));
        assert!(p.to_string_lossy().contains(".duduclaw"));
    }
}
