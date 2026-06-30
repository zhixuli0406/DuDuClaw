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

/// Resolve the configured port with the full priority chain (§D2.2):
/// `DUDUCLAW_PORT` env (if set & non-privileged) > `~/.duduclaw/config.toml`
/// `[gateway] port` > [`DEFAULT_PORT`]. This mirrors the operator's persisted
/// intent: the CLI writes the chosen port into `config.toml` on first run, so an
/// attached/spawned sidecar should respect it when the env var is absent.
pub fn configured_port() -> u16 {
    resolve_preferred_port_from(std::env::var("DUDUCLAW_PORT").ok().as_deref(), config_port())
}

/// Pure resolver for [`configured_port`] — split out so the priority chain is
/// unit-testable without touching the environment or filesystem.
pub fn resolve_preferred_port_from(env: Option<&str>, config: Option<u16>) -> u16 {
    if let Some(p) = env.and_then(|v| v.parse::<u16>().ok()).filter(|p| *p >= 1024) {
        return p;
    }
    if let Some(p) = config.filter(|p| *p >= 1024) {
        return p;
    }
    DEFAULT_PORT
}

/// Read `[gateway] port` from `~/.duduclaw/config.toml`, if present and valid.
/// Defensive (every failure → `None`) and dependency-free so `lifecycle.rs`
/// stays compilable under a bare `rustc --test` (no `toml` crate).
pub fn config_port() -> Option<u16> {
    let path = duduclaw_home().join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    config_port_from_str(&text)
}

/// Extract `[gateway] port = N` from raw `config.toml` text. A minimal, fail-safe
/// line scanner: tracks the current `[section]` header, returns the first `port`
/// integer found inside `[gateway]`. Ignores comments and inline `#` tails.
/// Returns `None` on anything it doesn't understand (never panics).
pub fn config_port_from_str(text: &str) -> Option<u16> {
    let mut in_gateway = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_gateway = section.trim().eq_ignore_ascii_case("gateway");
            continue;
        }
        if !in_gateway {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("port") {
                // Strip an inline comment and surrounding whitespace/quotes.
                let v = value.split('#').next().unwrap_or("").trim().trim_matches('"');
                return v.parse::<u16>().ok().filter(|p| *p >= 1024);
            }
        }
    }
    None
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

/// Operator override for how the desktop shell relates to a gateway (§D1).
/// Replaces the unbuilt settings-panel toggle with a testable env override —
/// a settings UI can later just write `DUDUCLAW_DESKTOP_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopMode {
    /// Attach to an existing gateway if one is found, else spawn one (default).
    Auto,
    /// Never spawn — always attach to the externally-managed gateway (e.g.
    /// launchd / CLI). Avoids double-instance when the user runs their own.
    Attach,
    /// Always spawn a self-contained sidecar, ignoring any existing gateway.
    Spawn,
}

/// Resolve the desktop mode from `DUDUCLAW_DESKTOP_MODE` (case-insensitive),
/// defaulting to [`DesktopMode::Auto`] on absent / unrecognized values.
pub fn desktop_mode() -> DesktopMode {
    desktop_mode_from(std::env::var("DUDUCLAW_DESKTOP_MODE").ok().as_deref())
}

/// Pure parser for [`desktop_mode`] — unit-testable without the environment.
pub fn desktop_mode_from(value: Option<&str>) -> DesktopMode {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("attach") => DesktopMode::Attach,
        Some("spawn") => DesktopMode::Spawn,
        _ => DesktopMode::Auto,
    }
}

/// The ordered, de-duplicated set of ports an externally-managed gateway might
/// already be serving on — used by attach-detection so config.toml pointing at a
/// non-default port can't make us miss (and double-spawn over) a gateway running
/// on the default. Order: env override, then config.toml, then [`DEFAULT_PORT`].
pub fn known_ports_from(env: Option<u16>, config: Option<u16>) -> Vec<u16> {
    let mut v = Vec::new();
    let mut push = |p: Option<u16>| {
        if let Some(p) = p.filter(|p| *p >= 1024) {
            if !v.contains(&p) {
                v.push(p);
            }
        }
    };
    push(env);
    push(config);
    push(Some(DEFAULT_PORT));
    v
}

/// Read the live known-port set from the environment + config.toml.
pub fn known_ports() -> Vec<u16> {
    let env = std::env::var("DUDUCLAW_PORT").ok().and_then(|v| v.parse::<u16>().ok());
    known_ports_from(env, config_port())
}

/// Decide whether to attach or spawn, honoring [`DesktopMode`] and probing every
/// known port for an existing gateway before spawning. Thin wrapper over
/// [`decide_plan`] with the real [`is_listening`] probe.
pub fn plan_gateway_with(mode: DesktopMode, host: &str, known: &[u16], preferred: u16) -> GatewayPlan {
    decide_plan(mode, known, preferred, |p| is_listening(host, p))
}

/// Back-compat entry point: [`DesktopMode::Auto`] over just the preferred port.
/// Prefer [`plan_gateway_with`], which also scans `known_ports()`.
pub fn plan_gateway(host: &str, preferred: u16) -> GatewayPlan {
    plan_gateway_with(DesktopMode::Auto, host, &[preferred], preferred)
}

/// Pure decision core (§D1/§D2.2): generic over the liveness probe so the whole
/// attach-vs-spawn matrix is unit-testable without opening sockets.
///
/// - `Attach`: attach to the first live known port; if none is up, still
///   `Attach { preferred }` (trust the external manager to bring it up — never
///   spawn a competing instance).
/// - `Spawn`: always spawn on the first free candidate of `preferred`.
/// - `Auto`: attach to the first live known port, else spawn on the first free
///   candidate of `preferred`.
pub fn decide_plan<F: Fn(u16) -> bool>(
    mode: DesktopMode,
    known: &[u16],
    preferred: u16,
    is_live: F,
) -> GatewayPlan {
    let first_live = || known.iter().copied().find(|p| is_live(*p));
    let spawn_free = || {
        candidate_ports(preferred)
            .into_iter()
            .find(|p| !is_live(*p))
            .map(|port| GatewayPlan::Spawn { port })
            // Every candidate busy but none is "our" gateway — fall back.
            .unwrap_or(GatewayPlan::Spawn { port: preferred })
    };
    match mode {
        DesktopMode::Spawn => spawn_free(),
        DesktopMode::Attach => GatewayPlan::Attach {
            port: first_live().unwrap_or(preferred),
        },
        DesktopMode::Auto => match first_live() {
            Some(port) => GatewayPlan::Attach { port },
            None => spawn_free(),
        },
    }
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
        // Snapshot + restore to avoid cross-test contamination. Point
        // DUDUCLAW_HOME at an empty temp dir so config_port() is None and the
        // test stays deterministic regardless of the developer's real config.
        let prev_port = std::env::var("DUDUCLAW_PORT").ok();
        let prev_home = std::env::var("DUDUCLAW_HOME").ok();
        let empty = std::env::temp_dir().join(format!("ddc-lc-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("DUDUCLAW_HOME", &empty);

        std::env::remove_var("DUDUCLAW_PORT");
        assert_eq!(configured_port(), DEFAULT_PORT);
        std::env::set_var("DUDUCLAW_PORT", "12345");
        assert_eq!(configured_port(), 12345);
        // Privileged / invalid → default (no config.toml present).
        std::env::set_var("DUDUCLAW_PORT", "80");
        assert_eq!(configured_port(), DEFAULT_PORT);
        std::env::set_var("DUDUCLAW_PORT", "not-a-port");
        assert_eq!(configured_port(), DEFAULT_PORT);

        let _ = std::fs::remove_dir_all(&empty);
        match prev_port {
            Some(v) => std::env::set_var("DUDUCLAW_PORT", v),
            None => std::env::remove_var("DUDUCLAW_PORT"),
        }
        match prev_home {
            Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
            None => std::env::remove_var("DUDUCLAW_HOME"),
        }
    }

    #[test]
    fn resolve_preferred_port_priority_env_over_config_over_default() {
        // env wins when valid.
        assert_eq!(resolve_preferred_port_from(Some("18900"), Some(18950)), 18900);
        // env invalid/privileged → fall to config.
        assert_eq!(resolve_preferred_port_from(Some("80"), Some(18950)), 18950);
        assert_eq!(resolve_preferred_port_from(Some("nope"), Some(18950)), 18950);
        // no env, config present.
        assert_eq!(resolve_preferred_port_from(None, Some(18950)), 18950);
        // privileged config ignored → default.
        assert_eq!(resolve_preferred_port_from(None, Some(80)), DEFAULT_PORT);
        // nothing → default.
        assert_eq!(resolve_preferred_port_from(None, None), DEFAULT_PORT);
    }

    #[test]
    fn config_port_parses_gateway_section_only() {
        let cfg = "[general]\nport = 9999\n\n[gateway]\nbind = \"127.0.0.1\"\nport = 18950\n";
        assert_eq!(config_port_from_str(cfg), Some(18950));
        // port outside [gateway] is ignored.
        assert_eq!(config_port_from_str("[general]\nport = 9999\n"), None);
        // inline comment + quotes tolerated.
        assert_eq!(config_port_from_str("[gateway]\nport = 18950 # chosen\n"), Some(18950));
        assert_eq!(config_port_from_str("[gateway]\nport = \"18950\"\n"), Some(18950));
        // privileged / garbage → None.
        assert_eq!(config_port_from_str("[gateway]\nport = 80\n"), None);
        assert_eq!(config_port_from_str("[gateway]\nport = wat\n"), None);
        assert_eq!(config_port_from_str(""), None);
    }

    #[test]
    fn desktop_mode_parses_case_insensitive_with_safe_default() {
        assert_eq!(desktop_mode_from(Some("attach")), DesktopMode::Attach);
        assert_eq!(desktop_mode_from(Some("  SPAWN ")), DesktopMode::Spawn);
        assert_eq!(desktop_mode_from(Some("Auto")), DesktopMode::Auto);
        assert_eq!(desktop_mode_from(Some("garbage")), DesktopMode::Auto);
        assert_eq!(desktop_mode_from(None), DesktopMode::Auto);
    }

    #[test]
    fn known_ports_ordered_deduped_and_filtered() {
        // env, config, default — order preserved, dupes/privileged dropped.
        assert_eq!(known_ports_from(Some(18900), Some(18950)), vec![18900, 18950, DEFAULT_PORT]);
        // config == default collapses.
        assert_eq!(known_ports_from(None, Some(DEFAULT_PORT)), vec![DEFAULT_PORT]);
        // privileged env ignored.
        assert_eq!(known_ports_from(Some(80), None), vec![DEFAULT_PORT]);
        // env == config collapses but keeps default.
        assert_eq!(known_ports_from(Some(18900), Some(18900)), vec![18900, DEFAULT_PORT]);
    }

    #[test]
    fn decide_plan_auto_attaches_to_any_live_known_port() {
        // config points at 18950 but the live gateway is on the default 18789;
        // Auto must attach to 18789, NOT spawn a competing instance over 18950.
        let known = vec![18950u16, DEFAULT_PORT];
        let plan = decide_plan(DesktopMode::Auto, &known, 18950, |p| p == DEFAULT_PORT);
        assert_eq!(plan, GatewayPlan::Attach { port: DEFAULT_PORT });
    }

    #[test]
    fn decide_plan_auto_spawns_first_free_when_nothing_live() {
        // Nothing live anywhere → spawn on the first free candidate of preferred.
        let known = vec![18950u16, DEFAULT_PORT];
        let plan = decide_plan(DesktopMode::Auto, &known, 18950, |_| false);
        assert_eq!(plan, GatewayPlan::Spawn { port: 18950 });
        // Preferred busy by a non-gateway → next free candidate.
        let plan = decide_plan(DesktopMode::Auto, &[], 18950, |p| p == 18950);
        assert_eq!(plan, GatewayPlan::Spawn { port: 18951 });
    }

    #[test]
    fn decide_plan_attach_never_spawns() {
        // Attach mode with a live known port → attach to it.
        let plan = decide_plan(DesktopMode::Attach, &[DEFAULT_PORT], 18950, |p| p == DEFAULT_PORT);
        assert_eq!(plan, GatewayPlan::Attach { port: DEFAULT_PORT });
        // Attach mode with nothing live → still attach to preferred, never spawn.
        let plan = decide_plan(DesktopMode::Attach, &[DEFAULT_PORT], 18950, |_| false);
        assert_eq!(plan, GatewayPlan::Attach { port: 18950 });
    }

    #[test]
    fn decide_plan_spawn_ignores_existing_gateway() {
        // Spawn mode ignores a live known port and spawns on a free candidate.
        let plan = decide_plan(DesktopMode::Spawn, &[DEFAULT_PORT], 18950, |p| p == DEFAULT_PORT);
        assert_eq!(plan, GatewayPlan::Spawn { port: 18950 });
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
