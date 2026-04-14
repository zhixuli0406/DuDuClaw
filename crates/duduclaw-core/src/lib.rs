pub mod agent_guard;
pub mod error;
pub mod platform;
pub mod traits;
pub mod types;

pub use agent_guard::{check_agent_file_write, check_bash_command, GuardDecision, AGENT_STRUCTURE_FILES};
pub use error::{DuDuClawError, Result};
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
/// Search order:
/// 1. `which claude` (respects current `PATH`)
/// 2. Fixed absolute candidate paths covering Homebrew (Intel + Apple Silicon),
///    Bun, Volta, npm-global, user-local installs, and asdf shims
/// 3. NVM glob expansion (`$HOME/.nvm/versions/node/*/bin/claude`)
///
/// This is the single shared implementation — replaces 4+ duplicates.
/// When gateway is launched from launchd / Finder / Dock, `PATH` frequently
/// omits Homebrew and Node version-manager paths, so the fixed candidates
/// are critical for zero-config install discovery.
pub fn which_claude() -> Option<String> {
    // 1. Check PATH via `which` (Unix) or `where` (Windows)
    let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(lookup_cmd)
        .arg("claude")
        .output()
        && output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);

            if cfg!(windows) {
                // `where` returns multiple lines. On Windows, prefer .cmd/.exe
                // over extensionless npm shims (which are shell scripts that
                // cause os error 193 or argument-passing issues with cmd /C).
                let lines: Vec<&str> = stdout.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();

                // First pass: prefer .exe or .cmd
                for line in &lines {
                    let lower = line.to_lowercase();
                    if (lower.ends_with(".exe") || lower.ends_with(".cmd"))
                        && std::path::Path::new(line).exists()
                    {
                        return Some(line.to_string());
                    }
                }
                // Fallback: take the first result and try appending .cmd
                if let Some(first) = lines.first() {
                    let cmd_path = format!("{first}.cmd");
                    if std::path::Path::new(&cmd_path).exists() {
                        return Some(cmd_path);
                    }
                    if std::path::Path::new(first).exists() {
                        return Some(first.to_string());
                    }
                }
            } else {
                let path = stdout.lines().next().unwrap_or("").trim().to_string();
                if !path.is_empty() && std::path::Path::new(&path).exists() {
                    return Some(path);
                }
            }
        }

    // 2-3. Scan fixed + dynamic HOME-rooted candidates
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    which_claude_in_home(std::path::Path::new(&home))
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
            // npm global (default Windows npm install location)
            format!("{appdata}\\npm\\claude.cmd"),
            format!("{appdata}\\npm\\claude"),
            // Bun on Windows
            format!("{home_str}\\.bun\\bin\\claude.exe"),
            // Volta on Windows
            format!("{home_str}\\.volta\\bin\\claude.exe"),
            // Claude Code native installer
            format!("{localappdata}\\Programs\\claude\\claude.exe"),
            format!("{home_str}\\.claude\\bin\\claude.exe"),
            // Scoop
            format!("{home_str}\\scoop\\shims\\claude.exe"),
            format!("{home_str}\\scoop\\shims\\claude.cmd"),
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
        let nvm_root = home.join(".nvm/versions/node");
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
    use super::which_claude_in_home;
    use std::fs;
    use std::path::Path;

    /// Create an executable shim at `path` so `.exists()` returns true.
    fn write_shim(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        crate::platform::set_executable(path).unwrap();
    }

    #[test]
    fn discovers_bun_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".bun/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn discovers_volta_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".volta/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn discovers_asdf_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".asdf/shims/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[test]
    fn discovers_npm_global() {
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
