pub mod agent_guard;
pub mod cron_tz;
pub mod error;
pub mod platform;
pub mod text_utils;
pub mod traits;
pub mod types;

pub use agent_guard::{check_agent_file_write, check_bash_command, GuardDecision, AGENT_STRUCTURE_FILES};
pub use cron_tz::{parse_timezone, should_fire_in_tz};
pub use error::{DuDuClawError, Result};
pub use text_utils::{truncate_bytes, truncate_chars};
pub use traits::{Channel, ContainerRuntime, MemoryEngine};
pub use types::*;

// ── Delegation safety constants ──────────────────────────────

/// Maximum number of agent-to-agent delegation hops before messages are
/// dropped.  Shared across MCP tools (pre-check) and the bus dispatcher
/// (runtime guard).
pub const MAX_DELEGATION_DEPTH: u8 = 5;

/// Environment variable names used to inject delegation context into
/// Claude CLI subprocesses.  The MCP server reads these to track depth
/// without relying on (spoofable) tool parameters.
pub const ENV_DELEGATION_DEPTH: &str = "DUDUCLAW_DELEGATION_DEPTH";
pub const ENV_DELEGATION_ORIGIN: &str = "DUDUCLAW_DELEGATION_ORIGIN";
pub const ENV_DELEGATION_SENDER: &str = "DUDUCLAW_DELEGATION_SENDER";

/// Agent identity injected into Claude CLI subprocesses via per-agent
/// `.mcp.json` so the MCP server knows *which* agent is the current
/// caller for supervisor-relation authorization.
///
/// Without this, the MCP server falls back to
/// `config.toml [general] default_agent` — which is the global default
/// and causes cross-agent delegations to be mis-authorized (e.g. a TL
/// sub-agent spawning its own sub-agent gets rejected because the MCP
/// thinks the caller is the top-level default agent, not TL).
///
/// Populated automatically at gateway startup; see
/// `duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path`.
pub const ENV_AGENT_ID: &str = "DUDUCLAW_AGENT_ID";

/// Channel context for delegation callback.
/// Format: `<channel_type>:<channel_id>[:<thread_id>]`
/// e.g. `telegram:12345` or `discord:thread:98765`
///
/// Set by channel handlers before spawning CLI sessions.
/// Read by `send_to_agent` MCP tool to record a callback so the
/// dispatcher can forward sub-agent responses back to the originating channel.
pub const ENV_REPLY_CHANNEL: &str = "DUDUCLAW_REPLY_CHANNEL";

/// Channel types supported for delegation callback forwarding.
/// Used by both the MCP `send_to_agent` tool and the channel_reply session filter.
pub const SUPPORTED_CHANNEL_TYPES: &[&str] = &[
    "telegram", "line", "discord", "slack", "whatsapp", "feishu",
];

/// Resolve the absolute path to the current DuDuClaw binary.
///
/// Used to populate `.mcp.json` and hook commands so Claude CLI
/// subprocesses can find the MCP server without relying on PATH
/// inheritance (which is frequently incomplete when launched from
/// launchd, Finder, or Dock).
///
/// Preference order:
/// 1. `DUDUCLAW_BIN` env var (test / override hook)
/// 2. `std::env::current_exe()` — the actual binary path
/// 3. Fallback to `"duduclaw"` (PATH-dependent, least robust)
pub fn resolve_duduclaw_bin() -> std::path::PathBuf {
    if let Ok(override_path) = std::env::var("DUDUCLAW_BIN")
        && !override_path.is_empty()
    {
        return std::path::PathBuf::from(override_path);
    }
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("duduclaw"))
}

/// Validate that an agent ID is safe for filesystem and log use.
///
/// A valid agent ID contains only lowercase alphanumerics, hyphens, and
/// underscores; is non-empty; and is at most 64 characters long.
pub fn is_valid_agent_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Find the `claude` binary in PATH or common locations (BE-L1, BE-M1).
///
/// Discovery sources:
/// 1. `which claude` (Unix) / `where claude` (Windows) — respects current `PATH`
/// 2. Fixed absolute candidate paths covering Homebrew (Intel + Apple Silicon),
///    Bun, Volta, npm-global, user-local installs, asdf shims (Unix) and
///    npm / pnpm / Yarn / Bun / Volta / Scoop / Claude Code native installer
///    locations (Windows)
/// 3. NVM glob expansion (`$HOME/.nvm/versions/node/*/bin/claude`)
///
/// **Windows precedence (CRITICAL — fixes BatBadBut / CVE-2024-24576):**
///
/// Discoveries from sources 1 + 2 are pooled, then ranked **`.exe` ahead of
/// `.cmd`** regardless of source. Spawning a `.exe` is always safe; spawning
/// a `.cmd` triggers Rust's BatBadBut rejection when args contain newlines /
/// quotes / `&` (which user prompts and system prompts routinely do). So a
/// host with both `~/.local/bin/claude.exe` (clean) and a leftover
/// `%APPDATA%\npm\claude.cmd` (BatBadBut hazard) MUST resolve to the `.exe`
/// even when `where.exe claude` returns the `.cmd` first.
///
/// On Unix, the order is preserved (PATH first, then HOME).
///
/// When gateway is launched from launchd / Finder / Dock, `PATH` frequently
/// omits Homebrew and Node version-manager paths, so the fixed candidates
/// are critical for zero-config install discovery.
pub fn which_claude() -> Option<String> {
    // ── 1. Discover via PATH ─────────────────────────────────────
    let mut path_results: Vec<String> = Vec::new();
    let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(lookup_cmd)
        .arg("claude")
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && std::path::Path::new(trimmed).exists() {
                path_results.push(trimmed.to_string());
            }
        }
    }

    // ── 2-3. Discover via HOME-rooted scan ──────────────────────
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let home_result = which_claude_in_home(std::path::Path::new(&home));

    // Combine in source order: PATH first (user's explicit env), then HOME.
    let mut all: Vec<String> = path_results;
    if let Some(h) = home_result {
        if !all.contains(&h) {
            all.push(h);
        }
    }

    if all.is_empty() {
        log_resolved_claude_path_once(None, &[]);
        return None;
    }

    // Unix: source-order is fine (no BatBadBut hazard).
    // Windows: pick by .exe > .cmd > extensionless precedence.
    #[cfg(not(windows))]
    let chosen: Option<String> = all.first().cloned();
    #[cfg(windows)]
    let chosen: Option<String> = pick_windows_preferred(&all);

    log_resolved_claude_path_once(chosen.as_deref(), &all);
    chosen
}

/// Windows-only precedence: `.exe` STRICTLY > `.cmd` > extensionless.
///
/// Even if PATH discovery returned `.cmd` first, an `.exe` found anywhere in
/// the pool wins. This is the **BatBadBut mitigation hinge** — losing this
/// ordering puts every channel reply at risk because Rust 1.77+ rejects
/// spawning `.bat`/`.cmd` files when args contain newlines / quotes / `&`
/// (CVE-2024-24576), which user prompts and system prompts routinely do.
///
/// Compiled cross-platform under `#[cfg(any(windows, test))]` so the
/// precedence logic can be exercised by unit tests on macOS / Linux runners.
#[cfg(any(windows, test))]
fn pick_windows_preferred(all: &[String]) -> Option<String> {
    // Pass 1: any .exe wins (safe to spawn, no BatBadBut)
    all.iter()
        .find(|c| c.to_lowercase().ends_with(".exe"))
        .cloned()
        // Pass 2: .cmd (resolve_cmd_to_node parses to node + cli.js)
        .or_else(|| {
            all.iter()
                .find(|c| c.to_lowercase().ends_with(".cmd"))
                .cloned()
        })
        // Pass 3: extensionless — try appending .exe then .cmd
        .or_else(|| {
            all.iter().find_map(|c| {
                let exe_path = format!("{c}.exe");
                if std::path::Path::new(&exe_path).exists() {
                    return Some(exe_path);
                }
                let cmd_path = format!("{c}.cmd");
                if std::path::Path::new(&cmd_path).exists() {
                    return Some(cmd_path);
                }
                None
            })
        })
        // Last resort: first entry as-is
        .or_else(|| all.first().cloned())
}

/// Emit one INFO log on the first `which_claude` call so operators can see
/// which binary the gateway resolved without needing to enable trace-level
/// logging. Subsequent calls are silent — `which_claude` is invoked many
/// times per session and noisy logs would drown out real signals.
///
/// Logs the chosen path AND the full discovery pool so we can diagnose
/// "wrong .cmd was picked" reports without round-tripping with the user.
fn log_resolved_claude_path_once(chosen: Option<&str>, pool: &[String]) {
    static LOGGED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    LOGGED.get_or_init(|| {
        match chosen {
            Some(path) => {
                tracing::info!(
                    path = %path,
                    candidates = ?pool,
                    "Resolved claude binary"
                );
            }
            None => {
                tracing::warn!(
                    "claude binary not found — checked PATH and HOME candidates"
                );
            }
        }
    });
}

/// Scan fixed absolute paths and HOME-rooted candidates for the `claude` binary.
///
/// Extracted so tests can exercise candidate discovery deterministically
/// (without depending on the ambient `PATH`, which `which_claude` consults first).
/// Returns the first candidate that exists as a real filesystem entry.
pub fn which_claude_in_home(home: &std::path::Path) -> Option<String> {
    let home_str = home.to_string_lossy();

    // Platform-specific candidates
    #[cfg(not(windows))]
    let candidates = vec![
        // macOS Apple Silicon Homebrew
        "/opt/homebrew/bin/claude".to_string(),
        // macOS Intel / Linux Homebrew
        "/usr/local/bin/claude".to_string(),
        // Bun (increasingly common for Node CLIs)
        format!("{home_str}/.bun/bin/claude"),
        // Volta
        format!("{home_str}/.volta/bin/claude"),
        // npm global (default for many Node installs)
        format!("{home_str}/.npm-global/bin/claude"),
        // Claude Code native installer
        format!("{home_str}/.claude/bin/claude"),
        // User-local
        format!("{home_str}/.local/bin/claude"),
        // asdf shim
        format!("{home_str}/.asdf/shims/claude"),
    ];

    #[cfg(windows)]
    let candidates = {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let localappdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            // ── .exe candidates first ────────────────────────────
            // .exe spawns cleanly via std::process::Command — no
            // BatBadBut (CVE-2024-24576) hazard. When a host has both
            // a clean .exe install AND a leftover npm .cmd shim, we
            // MUST prefer the .exe to avoid Rust 1.77+'s rejection of
            // .cmd args containing newlines / quotes / `&` etc.
            //
            // Claude Code native installer (XDG-style on Windows):
            //   ~/.local/bin/claude.exe — the most common location
            //   on machines installed via the official installer.
            format!("{home_str}\\.local\\bin\\claude.exe"),
            // Claude Code legacy / desktop-installer locations
            format!("{home_str}\\.claude\\bin\\claude.exe"),
            format!("{localappdata}\\Programs\\claude\\claude.exe"),
            // Bun on Windows
            format!("{home_str}\\.bun\\bin\\claude.exe"),
            // Volta on Windows
            format!("{home_str}\\.volta\\bin\\claude.exe"),
            // Scoop
            format!("{home_str}\\scoop\\shims\\claude.exe"),
            // pnpm global (modern default)
            format!("{localappdata}\\pnpm\\claude.exe"),
            // Yarn classic global
            format!("{localappdata}\\Yarn\\bin\\claude.exe"),

            // ── .cmd candidates (rely on resolve_cmd_to_node) ────
            // Each .cmd is parsed into (node.exe, cli.js) at spawn
            // time so we never hand args directly to cmd.exe.
            // npm global (default Windows npm install location)
            format!("{appdata}\\npm\\claude.cmd"),
            format!("{appdata}\\npm\\claude"),
            // pnpm global .cmd shim
            format!("{localappdata}\\pnpm\\claude.cmd"),
            // Yarn classic global .cmd shim
            format!("{localappdata}\\Yarn\\bin\\claude.cmd"),
            // Bun on Windows (older versions ship .cmd shims)
            format!("{home_str}\\.bun\\bin\\claude.cmd"),
            // Volta .cmd (older releases)
            format!("{home_str}\\.volta\\bin\\claude.cmd"),
            // Scoop
            format!("{home_str}\\scoop\\shims\\claude.cmd"),
            // ~/.local/bin extensionless / .cmd fallback
            format!("{home_str}\\.local\\bin\\claude.cmd"),
            format!("{home_str}\\.local\\bin\\claude"),
        ]
    };

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    // NVM: scan all node versions for claude binary
    #[cfg(not(windows))]
    {
        let nvm_root = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join("claude");
                if candidate.exists() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
        }
    }

    #[cfg(windows)]
    {
        // NVM for Windows: %APPDATA%\nvm\<version>\claude.cmd
        let nvm_root = std::path::Path::new(&std::env::var("APPDATA").unwrap_or_default()).join("nvm");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                for name in ["claude.cmd", "claude.exe"] {
                    let candidate = entry.path().join(name);
                    if candidate.exists() {
                        return Some(candidate.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod which_claude_tests {
    use super::{pick_windows_preferred, which_claude_in_home};
    use std::fs;
    use std::path::Path;

    // ── pick_windows_preferred precedence (BatBadBut hinge) ──────
    //
    // These tests verify the v1.8.32 fix: even when PATH discovery
    // returns a `.cmd` first (e.g. `where.exe claude` finds an npm
    // shim), an `.exe` discovered anywhere in the candidate pool
    // MUST win. Losing this ordering = every channel reply on
    // Windows fails with "batch file arguments are invalid".

    #[test]
    fn windows_pref_exe_beats_cmd_even_when_cmd_listed_first() {
        let pool = vec![
            "C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd".to_string(),
            "C:\\Users\\X\\.local\\bin\\claude.exe".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\Users\\X\\.local\\bin\\claude.exe"),
        );
    }

    #[test]
    fn windows_pref_picks_cmd_when_no_exe_exists() {
        let pool = vec!["C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd".to_string()];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd"),
        );
    }

    #[test]
    fn windows_pref_returns_none_for_empty_pool() {
        assert!(pick_windows_preferred(&[]).is_none());
    }

    #[test]
    fn windows_pref_first_exe_wins_among_multiple_exes() {
        let pool = vec![
            "C:\\a\\claude.exe".to_string(),
            "C:\\b\\claude.exe".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\a\\claude.exe"),
        );
    }

    #[test]
    fn windows_pref_first_cmd_wins_among_multiple_cmds_when_no_exe() {
        let pool = vec![
            "C:\\a\\claude.cmd".to_string(),
            "C:\\b\\claude.cmd".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\a\\claude.cmd"),
        );
    }

    #[test]
    fn windows_pref_extension_check_is_case_insensitive() {
        // Some installers / users have uppercase extensions in PATHEXT order.
        let pool = vec![
            "C:\\a\\claude.CMD".to_string(),
            "C:\\b\\claude.EXE".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\b\\claude.EXE"),
        );
    }

    #[test]
    fn windows_pref_falls_back_to_first_for_extensionless_when_no_fs_match() {
        // Pass 3 (FS append) misses; Pass 4 returns first entry as-is.
        let pool = vec![
            "/nonexistent/claude".to_string(),
            "/another/claude".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("/nonexistent/claude"),
        );
    }

    /// Create an executable shim at `path` so `.exists()` returns true.
    fn write_shim(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        crate::platform::set_executable(path).unwrap();
    }

    /// Guard: skip tests that rely on HOME-rooted candidates winning when the
    /// host already has a system-level claude install (which takes priority).
    fn host_has_system_claude() -> bool {
        Path::new("/opt/homebrew/bin/claude").exists()
            || Path::new("/usr/local/bin/claude").exists()
    }

    #[test]
    fn discovers_bun_candidate() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".bun/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn discovers_volta_candidate() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".volta/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn discovers_asdf_shim() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".asdf/shims/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn discovers_npm_global() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".npm-global/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn nvm_version_directory_is_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".nvm/versions/node/v20.10.0/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        // Expect the nvm candidate since no fixed candidate matches in this tempdir
        // (and /opt/homebrew won't exist under a random tempdir HOME either, unless
        // the host happens to have it — which still satisfies the contract: a valid
        // absolute path to `claude` is returned).
        let found = found.expect("should find some claude candidate");
        let path = Path::new(&found);
        assert!(path.exists(), "returned path must exist: {found}");
        assert!(
            found.ends_with("bin/claude"),
            "returned path must end with bin/claude: {found}"
        );
    }

    #[test]
    fn no_candidates_returns_none_when_no_fixed_paths_present() {
        // Only valid if the host has none of /opt/homebrew/bin/claude or
        // /usr/local/bin/claude installed. Guarded accordingly so the test
        // remains deterministic on CI and dev machines alike.
        if Path::new("/opt/homebrew/bin/claude").exists()
            || Path::new("/usr/local/bin/claude").exists()
        {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let found = which_claude_in_home(tmp.path());
        assert!(found.is_none(), "empty HOME should return None, got {:?}", found);
    }

    #[test]
    fn fixed_candidate_order_bun_beats_npm_global() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        // When both .bun/bin/claude and .npm-global/bin/claude exist,
        // Bun should win because it's earlier in the candidate list.
        let tmp = tempfile::tempdir().unwrap();
        let bun = tmp.path().join(".bun/bin/claude");
        let npm = tmp.path().join(".npm-global/bin/claude");
        write_shim(&bun);
        write_shim(&npm);
        let found = which_claude_in_home(tmp.path()).unwrap();
        assert_eq!(found, bun.to_string_lossy());
    }
}
