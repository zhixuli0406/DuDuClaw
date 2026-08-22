// WP-C-M2 — gateway target resolution: pure, side-effect-light functions
// deciding WHICH port/host a local gateway should live on, WHICH ports
// might already have one, and WHERE the `duduclaw` binary lives on this
// machine. Split out of `sidecar.rs` (which owns the actual `SidecarManager`
// process — spawn/health-poll/kill) purely to keep both files under this
// project's 800-line convention; conceptually this is still "the sidecar
// subsystem", just its planning half. Mirrors the Tauri shell's own
// `src-tauri/src/lifecycle.rs` split from `sidecar.rs` for the identical
// reason — see that file's header comment.

use std::ffi::OsString;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The gateway's default loopback host and port — mirrors the CLI's own
/// `DUDUCLAW_PORT` env override / 18789 default (`duduclaw-cli`).
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 18789;

// ── Port / plan resolution (pure, unit-testable) ─────────────────────────

/// Resolve the port this app should use for a LOCAL gateway: `DUDUCLAW_PORT`
/// env (if set & non-privileged) > `~/.duduclaw/config.toml` `[gateway]
/// port` > [`DEFAULT_PORT`]. Mirrors the CLI's own persisted intent — the
/// CLI writes its chosen port into `config.toml` on first run, so a sidecar
/// spawned by this app should respect it when the env var is absent.
pub fn configured_port() -> u16 {
    resolve_preferred_port_from(std::env::var("DUDUCLAW_PORT").ok().as_deref(), config_port())
}

/// Pure resolver for [`configured_port`] — split out so the priority chain
/// is testable without touching the environment or filesystem.
pub fn resolve_preferred_port_from(env: Option<&str>, config: Option<u16>) -> u16 {
    if let Some(p) = env.and_then(|v| v.parse::<u16>().ok()).filter(|p| *p >= 1024) {
        return p;
    }
    if let Some(p) = config.filter(|p| *p >= 1024) {
        return p;
    }
    DEFAULT_PORT
}

/// Read `[gateway] port` from `~/.duduclaw/config.toml`, if present and
/// valid. Every failure (missing home, missing file, unreadable, no such
/// section/key) degrades to `None` — never panics, never blocks startup.
pub fn config_port() -> Option<u16> {
    let path = crate::config::duduclaw_home()?.join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    config_port_from_str(&text)
}

/// Extract `[gateway] port = N` from raw `config.toml` text. A minimal,
/// fail-safe line scanner (NOT a TOML parser): tracks the current
/// `[section]` header, returns the first `port` integer found inside
/// `[gateway]`, tolerates inline `#` comments and quoted values.
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
                let v = value.split('#').next().unwrap_or("").trim().trim_matches('"');
                return v.parse::<u16>().ok().filter(|p| *p >= 1024);
            }
        }
    }
    None
}

/// Ordered candidate ports to try when the preferred one is busy: the
/// preferred port first, then a small deterministic fallback band.
pub fn candidate_ports(preferred: u16) -> Vec<u16> {
    let mut v = vec![preferred];
    for delta in 1..=8u16 {
        if let Some(p) = preferred.checked_add(delta) {
            v.push(p);
        }
    }
    v
}

/// True if *something* is already listening on `host:port` — used both to
/// detect an existing gateway (attach instead of spawn) and to find a free
/// port to spawn on.
pub fn is_listening(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    match addr.to_socket_addrs() {
        Ok(mut addrs) => addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(250)).is_ok()),
        Err(_) => false,
    }
}

/// The health endpoint to poll for readiness (the gateway's dashboard
/// server answers this without auth). Retained as the canonical readiness
/// URL (and unit-tested below) even though `SidecarManager`'s actual
/// readiness watcher uses the cheaper [`is_listening`] TCP probe instead —
/// same "documented, not dead" precedent the Tauri shell's own
/// `lifecycle::health_url` established for the identical trade-off.
#[allow(dead_code)]
pub fn health_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/healthz")
}

/// How this app should obtain a running local gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayPlan {
    /// A gateway is already serving on this port — attach, do not spawn.
    Attach { port: u16 },
    /// No gateway found; spawn the sidecar bound to this free port.
    Spawn { port: u16 },
}

/// Decide attach-vs-spawn (always "Auto": attach to the first known port
/// that answers, else spawn on the first free candidate of `preferred`).
/// Unlike the Tauri shell's `lifecycle::DesktopMode`, this crate has no
/// `Attach`-only / `Spawn`-only env override — the WP-C-M2 task brief's two
/// modes (同機 vs 遠端) are handled one layer up, by `config::GatewayMode`
/// deciding whether to consult this function AT ALL, not by a second mode
/// enum inside it.
pub fn plan_gateway(known: &[u16], preferred: u16) -> GatewayPlan {
    decide_plan(known, preferred, |p| is_listening(DEFAULT_HOST, p))
}

/// Pure decision core — generic over the liveness probe so attach-vs-spawn
/// is unit-testable without opening sockets.
pub fn decide_plan<F: Fn(u16) -> bool>(known: &[u16], preferred: u16, is_live: F) -> GatewayPlan {
    if let Some(port) = known.iter().copied().find(|p| is_live(*p)) {
        return GatewayPlan::Attach { port };
    }
    candidate_ports(preferred)
        .into_iter()
        .find(|p| !is_live(*p))
        .map(|port| GatewayPlan::Spawn { port })
        .unwrap_or(GatewayPlan::Spawn { port: preferred })
}

/// The ordered, de-duplicated set of ports an externally-managed gateway
/// might already be serving on — env override, then config.toml, then
/// [`DEFAULT_PORT`] — so a non-default `config.toml` port can't make this
/// app miss (and double-spawn over) a gateway already running on the
/// default port.
pub fn known_ports() -> Vec<u16> {
    let env = std::env::var("DUDUCLAW_PORT").ok().and_then(|v| v.parse::<u16>().ok());
    known_ports_from(env, config_port())
}

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

// ── Binary discovery + PATH augmentation ─────────────────────────────────

/// Directories a GUI launch (Finder/Dock) typically misses because it does
/// not inherit the shell PATH. Mirrors `duduclaw-core::which_claude_in_
/// home`'s own candidate list (hand-duplicated for the same isolation
/// reason `config::duduclaw_home`'s doc comment gives — this crate cannot
/// depend on `duduclaw-core`).
fn extra_path_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let h = |s: &str| PathBuf::from(&home).join(s);
    vec![
        PathBuf::from("/opt/homebrew/bin"), // Apple Silicon Homebrew
        PathBuf::from("/usr/local/bin"),    // Intel Homebrew / common Unix
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        h(".local/bin"),
        h(".bun/bin"),
        h(".volta/bin"),
        h(".npm-global/bin"),
        h(".asdf/shims"),
        h(".cargo/bin"), // `cargo install duduclaw` / dev builds
    ]
}

/// Build a PATH that prepends [`extra_path_dirs`] to the inherited PATH,
/// de-duplicated, so the spawned sidecar can find Claude CLI / node /
/// containers even under a Finder/Dock launch whose PATH omits them.
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

/// Find the `duduclaw` binary — same shape as `duduclaw-core::which_claude`
/// (PATH lookup first, then a fixed candidate scan under HOME), targeting
/// `duduclaw` instead of `claude`. Hand-duplicated rather than depending on
/// `duduclaw-core` (this crate's established isolation boundary — see
/// `config::duduclaw_home`'s doc comment).
///
/// Priority:
/// 1. `DUDUCLAW_BIN` env var — same override name `duduclaw-core::
///    resolve_duduclaw_bin` uses elsewhere in this project, so a user who
///    already set it for another purpose gets consistent behavior here too.
/// 2. `which duduclaw` (Unix) / `where duduclaw` (Windows) — respects the
///    CURRENT process's PATH (not yet augmented).
/// 3. Fixed candidates under HOME + common install prefixes.
pub fn which_duduclaw() -> Option<String> {
    if let Ok(over) = std::env::var("DUDUCLAW_BIN") {
        if !over.trim().is_empty() {
            return Some(over);
        }
    }

    let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = Command::new(lookup_cmd).arg("duduclaw").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && std::path::Path::new(trimmed).exists() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    which_duduclaw_in_home(std::path::Path::new(&home))
}

/// Scan fixed absolute paths for the `duduclaw` binary. Extracted so the
/// candidate list is testable deterministically, independent of the
/// ambient PATH `which_duduclaw` consults first.
pub fn which_duduclaw_in_home(home: &std::path::Path) -> Option<String> {
    let home_str = home.to_string_lossy();

    #[cfg(not(windows))]
    let candidates = vec![
        "/opt/homebrew/bin/duduclaw".to_string(),
        "/usr/local/bin/duduclaw".to_string(),
        format!("{home_str}/.cargo/bin/duduclaw"),
        format!("{home_str}/.local/bin/duduclaw"),
        format!("{home_str}/.npm-global/bin/duduclaw"),
        format!("{home_str}/.bun/bin/duduclaw"),
        format!("{home_str}/.volta/bin/duduclaw"),
        format!("{home_str}/.asdf/shims/duduclaw"),
    ];

    #[cfg(windows)]
    let candidates = {
        let localappdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            // .exe first: spawning a `.cmd` risks the BatBadBut rejection
            // (CVE-2024-24576) `duduclaw-core::which_claude`'s own doc
            // comment documents for the analogous `claude` lookup.
            format!("{home_str}\\.cargo\\bin\\duduclaw.exe"),
            format!("{home_str}\\.local\\bin\\duduclaw.exe"),
            format!("{localappdata}\\Programs\\duduclaw\\duduclaw.exe"),
            format!("{home_str}\\.npm-global\\duduclaw.cmd"),
            format!("{home_str}\\.bun\\bin\\duduclaw.exe"),
            format!("{home_str}\\.volta\\bin\\duduclaw.exe"),
        ]
    };

    if let Some(found) = candidates.into_iter().find(|c| std::path::Path::new(c).exists()) {
        return Some(found);
    }
    // NVM glob expansion (`$HOME/.nvm/versions/node/*/bin/duduclaw`) — the
    // fixed-candidate list above has no single fixed path for this because
    // the version directory name varies; `duduclaw-core::which_claude`'s
    // own doc comment lists the identical source (#3) for the analogous
    // `claude` lookup. Confirmed load-bearing, not speculative: this exact
    // shape (`duduclaw` installed as an npm package under nvm, `duduclaw`
    // on PATH only via the nvm-managed `bin/` symlink) is this project's
    // OWN real npm-distributed install (`scripts/*` / release tooling), so
    // a Finder/Dock-launched `duduclaw-native-gui` (PATH has no nvm dirs)
    // would otherwise fail to find its own project's gateway binary.
    which_duduclaw_via_nvm(home)
}

/// Best-effort NVM version-directory scan — see [`which_duduclaw_in_home`]'s
/// doc comment. Sorts version directory names lexicographically and prefers
/// the last (rev-sorted) one as a "probably newest" heuristic; unlike
/// `duduclaw-core::which_claude`'s full `--version`-probing precedence this
/// does NOT actually parse semver (a jump from `v9.x` to `v10.x` would sort
/// the wrong way lexicographically) — a deliberate simplification for a
/// fallback path that only matters when NEITHER `PATH` NOR any of the fixed
/// candidates above already found it; the common case (one Node version
/// installed) is unaffected either way.
fn which_duduclaw_via_nvm(home: &std::path::Path) -> Option<String> {
    let nvm_root = home.join(".nvm").join("versions").join("node");
    let mut dirs: Vec<std::path::PathBuf> =
        std::fs::read_dir(&nvm_root).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs.into_iter().rev() {
        #[cfg(not(windows))]
        let candidate = dir.join("bin").join("duduclaw");
        #[cfg(windows)]
        let candidate = dir.join("duduclaw.cmd");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

// ── Pidfile ───────────────────────────────────────────────────────────────

/// Pidfile recording the sidecar this app spawned, so a later launch (or a
/// crash-recovery pass on the same launch) can reclaim an orphaned process.
pub fn sidecar_pidfile() -> PathBuf {
    crate::config::duduclaw_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("native-gui-sidecar.pid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_ports_starts_with_preferred_and_bands() {
        let c = candidate_ports(18789);
        assert_eq!(c[0], 18789);
        assert_eq!(c.len(), 9);
        assert_eq!(*c.last().unwrap(), 18797);
    }

    #[test]
    fn candidate_ports_saturates_near_u16_max() {
        let c = candidate_ports(u16::MAX - 2);
        assert_eq!(c[0], u16::MAX - 2);
        assert!(c.iter().all(|p| *p >= u16::MAX - 2));
    }

    #[test]
    fn health_url_is_well_formed() {
        assert_eq!(health_url("127.0.0.1", 18789), "http://127.0.0.1:18789/healthz");
    }

    #[test]
    fn resolve_preferred_port_priority_env_over_config_over_default() {
        assert_eq!(resolve_preferred_port_from(Some("18900"), Some(18950)), 18900);
        assert_eq!(resolve_preferred_port_from(Some("80"), Some(18950)), 18950);
        assert_eq!(resolve_preferred_port_from(Some("nope"), Some(18950)), 18950);
        assert_eq!(resolve_preferred_port_from(None, Some(18950)), 18950);
        assert_eq!(resolve_preferred_port_from(None, Some(80)), DEFAULT_PORT);
        assert_eq!(resolve_preferred_port_from(None, None), DEFAULT_PORT);
    }

    #[test]
    fn config_port_parses_gateway_section_only() {
        let cfg = "[general]\nport = 9999\n\n[gateway]\nbind = \"127.0.0.1\"\nport = 18950\n";
        assert_eq!(config_port_from_str(cfg), Some(18950));
        assert_eq!(config_port_from_str("[general]\nport = 9999\n"), None);
        assert_eq!(config_port_from_str("[gateway]\nport = 18950 # chosen\n"), Some(18950));
        assert_eq!(config_port_from_str("[gateway]\nport = \"18950\"\n"), Some(18950));
        assert_eq!(config_port_from_str("[gateway]\nport = 80\n"), None);
        assert_eq!(config_port_from_str("[gateway]\nport = wat\n"), None);
        assert_eq!(config_port_from_str(""), None);
    }

    #[test]
    fn known_ports_ordered_deduped_and_filtered() {
        assert_eq!(known_ports_from(Some(18900), Some(18950)), vec![18900, 18950, DEFAULT_PORT]);
        assert_eq!(known_ports_from(None, Some(DEFAULT_PORT)), vec![DEFAULT_PORT]);
        assert_eq!(known_ports_from(Some(80), None), vec![DEFAULT_PORT]);
        assert_eq!(known_ports_from(Some(18900), Some(18900)), vec![18900, DEFAULT_PORT]);
    }

    #[test]
    fn decide_plan_attaches_to_any_live_known_port() {
        // config points at 18950 but the live gateway is on the default
        // 18789 — must attach to 18789, NOT spawn a competing instance
        // over 18950.
        let known = vec![18950u16, DEFAULT_PORT];
        let plan = decide_plan(&known, 18950, |p| p == DEFAULT_PORT);
        assert_eq!(plan, GatewayPlan::Attach { port: DEFAULT_PORT });
    }

    #[test]
    fn decide_plan_spawns_first_free_when_nothing_live() {
        let known = vec![18950u16, DEFAULT_PORT];
        let plan = decide_plan(&known, 18950, |_| false);
        assert_eq!(plan, GatewayPlan::Spawn { port: 18950 });
        let plan = decide_plan(&[], 18950, |p| p == 18950);
        assert_eq!(plan, GatewayPlan::Spawn { port: 18951 });
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
        assert!(p.ends_with("native-gui-sidecar.pid"));
    }

    #[test]
    fn which_duduclaw_env_override_wins_without_touching_filesystem() {
        let prev = std::env::var("DUDUCLAW_BIN").ok();
        unsafe { std::env::set_var("DUDUCLAW_BIN", "/nonexistent/but/trusted/duduclaw") };
        let found = which_duduclaw();
        match prev {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_BIN", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_BIN") },
        }
        assert_eq!(found, Some("/nonexistent/but/trusted/duduclaw".to_string()));
    }

    #[test]
    fn which_duduclaw_in_home_finds_nothing_under_an_empty_dir() {
        let dir = std::env::temp_dir().join(format!("ddc-ng-sidecar-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(which_duduclaw_in_home(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression guard for the exact real-world install shape found on the
    /// machine this pass was verified on (`~/.nvm/versions/node/v24.15.0/
    /// bin/duduclaw` -> `../lib/node_modules/duduclaw/bin/duduclaw`, no
    /// entry on any OTHER candidate path this module checks) — a Finder/
    /// Dock launch (PATH excludes nvm dirs) must still find it via the
    /// HOME-rooted fallback.
    #[test]
    fn which_duduclaw_in_home_finds_nvm_installed_binary() {
        let dir = std::env::temp_dir().join(format!("ddc-ng-sidecar-nvm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin_dir = dir.join(".nvm").join("versions").join("node").join("v24.15.0").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("duduclaw"), b"#!/bin/sh\necho stub\n").unwrap();

        let found = which_duduclaw_in_home(&dir);
        assert_eq!(found, Some(bin_dir.join("duduclaw").to_string_lossy().into_owned()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two version directories present — the rev-sorted heuristic
    /// documented on [`which_duduclaw_via_nvm`] must pick the
    /// lexicographically LAST one, not just whichever `read_dir` happens to
    /// yield first (filesystem iteration order is not guaranteed).
    #[test]
    fn which_duduclaw_in_home_prefers_lexicographically_last_nvm_version() {
        let dir = std::env::temp_dir().join(format!("ddc-ng-sidecar-nvm-multi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let node_root = dir.join(".nvm").join("versions").join("node");
        for v in ["v18.20.0", "v22.1.0"] {
            let bin_dir = node_root.join(v).join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            std::fs::write(bin_dir.join("duduclaw"), b"stub").unwrap();
        }

        let found = which_duduclaw_in_home(&dir).unwrap();
        assert!(found.contains("v22.1.0"), "expected the v22.1.0 install, got {found}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
