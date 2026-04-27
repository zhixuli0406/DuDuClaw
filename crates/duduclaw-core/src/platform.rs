//! Cross-platform utilities for file locking, permissions, and process management.
//!
//! This module abstracts away Unix-specific APIs (flock, chmod, signals) so the
//! rest of the codebase compiles and runs on both Unix and Windows.

use std::fs::File;
use std::path::Path;

// ── Home directory ───────────────────────────────────────────

/// Get the user's home directory, cross-platform.
///
/// Returns `$HOME` on Unix, `%USERPROFILE%` on Windows.
pub fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

/// Return the Python 3 command name for the current platform.
///
/// On Windows, Python is often installed as `python` (the Microsoft Store
/// `python3` stub is unreliable). On Unix, `python3` is preferred.
pub fn python3_command() -> &'static str {
    #[cfg(windows)]
    { "python" }
    #[cfg(not(windows))]
    { "python3" }
}

// ── Command execution helpers ────────────────────────────────

/// Create a `std::process::Command` for a program, handling Windows npm shims.
///
/// On Windows, `.cmd` npm shims cannot be run directly (os error 193) and
/// `cmd /C` mangles arguments (special chars, quotes, long system prompts).
/// Instead, we parse the `.cmd` shim to find the underlying `node.exe` +
/// script path and invoke Node directly — clean argument passing, no shell.
///
/// On Unix, this is a simple pass-through to `Command::new(program)`.
pub fn command_for(program: &str) -> std::process::Command {
    #[cfg(windows)]
    if let Some((node, script)) = resolve_cmd_to_node(program) {
        let mut cmd = std::process::Command::new(node);
        cmd.arg(script);
        return cmd;
    }
    std::process::Command::new(program)
}

/// Create a `tokio::process::Command`, handling Windows npm shims.
pub fn async_command_for(program: &str) -> tokio::process::Command {
    #[cfg(windows)]
    if let Some((node, script)) = resolve_cmd_to_node(program) {
        let mut cmd = tokio::process::Command::new(node);
        cmd.arg(script);
        return cmd;
    }
    tokio::process::Command::new(program)
}

/// On Windows, resolve a `.cmd`/`.bat` shim (npm, Bun, pnpm, yarn …) to its
/// underlying `(node, script)` pair so we can spawn `node.exe script.mjs`
/// directly — avoiding `cmd /C` (mangles arguments) and Rust 1.77+'s
/// `BatBadBut` rejection (CVE-2024-24576) which surfaces as the IO error
/// `"batch file arguments are invalid"` when args contain `"`, `&`, newlines,
/// etc. — a common case here since user prompts and system prompts often do.
///
/// Two strategies, in order:
///
/// 1. **Parse the shim file** — search every line of the shim for a quoted
///    or whitespace-separated token ending in `.js`/`.mjs`/`.cjs`, expand
///    common shim variables (`%~dp0`, `%dp0%`, `%~dpn0`, `%CD%`), and join
///    the relative result with the shim's directory.
///
/// 2. **Probe known package layouts** — if the shim doesn't parse (Bun
///    binary wrappers, custom shims), check the well-known relative paths
///    where `@anthropic-ai/claude-code/cli.js` lives for npm / Bun / yarn /
///    pnpm global installs.
///
/// Returns `None` only if neither strategy resolves a real `.js`. In that
/// case the caller falls back to spawning the `.cmd` directly, which may
/// then trigger the BatBadBut error — but at least we tried both clean paths.
#[cfg(windows)]
fn resolve_cmd_to_node(program: &str) -> Option<(String, String)> {
    let lower = program.to_lowercase();
    // Direct .exe is fine — Rust spawns it cleanly without cmd.exe.
    if lower.ends_with(".exe") {
        return None;
    }

    // Try the .cmd version if given an extensionless path
    let cmd_path = if !lower.ends_with(".cmd") && !lower.ends_with(".bat") {
        let with_cmd = format!("{program}.cmd");
        if std::path::Path::new(&with_cmd).exists() {
            with_cmd
        } else {
            program.to_string()
        }
    } else {
        program.to_string()
    };

    if !std::path::Path::new(&cmd_path).exists() {
        return None;
    }

    let dir = std::path::Path::new(&cmd_path).parent()?;

    // Strategy A: parse the shim's invocation line.
    if let Ok(content) = std::fs::read_to_string(&cmd_path)
        && let Some(rel) = parse_shim_script_path(&content)
    {
        let candidate = dir.join(&rel);
        if candidate.exists() {
            return Some((locate_node(dir), candidate.to_string_lossy().to_string()));
        }
    }

    // Strategy B: probe well-known cli.js layouts relative to the shim dir.
    for parts in known_cli_subpaths() {
        let mut p = dir.to_path_buf();
        for seg in *parts {
            p.push(seg);
        }
        if p.exists() {
            return Some((locate_node(dir), p.to_string_lossy().to_string()));
        }
    }

    None
}

/// Parse the invocation line of a Windows shim and return the relative path
/// to the `.js`/`.mjs`/`.cjs` script it invokes, with shim variables expanded
/// to empty strings (so the path is relative to the shim's directory).
///
/// Cross-platform-compiled so unit tests can exercise it on any host.
#[cfg(any(windows, test))]
fn parse_shim_script_path(content: &str) -> Option<String> {
    // Walk lines in reverse — the actual invocation is at the bottom of the
    // shim (after all the `IF EXIST` / `SETLOCAL` boilerplate).
    for line in content.lines().rev() {
        let mut last_match: Option<String> = None;

        // Pass 1: every double-quoted segment (odd indices when split by `"`).
        for (i, segment) in line.split('"').enumerate() {
            if i % 2 == 1
                && let Some(p) = clean_shim_token(segment)
            {
                last_match = Some(p);
            }
        }
        // Pass 2: whitespace tokens (handles unquoted shims like Bun's).
        for token in line.split_whitespace() {
            let unquoted = token.trim_matches(['"', '\'']);
            if let Some(p) = clean_shim_token(unquoted) {
                last_match = Some(p);
            }
        }

        if last_match.is_some() {
            return last_match;
        }
    }
    None
}

/// Return `Some(relative_path)` if `raw` looks like a JS script reference in a
/// shim (ends in `.js`/`.mjs`/`.cjs` after expanding common shim variables),
/// otherwise `None`. Path separators are normalized to `/` so that
/// `Path::join` works on both Windows and Unix.
#[cfg(any(windows, test))]
fn clean_shim_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_lowercase();
    if !(lower.ends_with(".mjs") || lower.ends_with(".js") || lower.ends_with(".cjs")) {
        return None;
    }
    // %~dp0 / %dp0% / %~dpn0 / %~f0 / %CD% all expand to "the shim's dir";
    // by replacing them with empty we get a path relative to that dir.
    let expanded = trimmed
        .replace("%~dp0", "")
        .replace("%dp0%", "")
        .replace("%~dpn0", "")
        .replace("%~f0", "")
        .replace("%CD%", "");
    let normalized = expanded.replace('\\', "/");
    let cleaned = normalized.trim_start_matches('/').to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Well-known relative paths from a shim directory to
/// `@anthropic-ai/claude-code/cli.js`. Used as a fallback when shim parsing
/// fails (e.g. binary wrappers, custom installers).
#[cfg(any(windows, test))]
fn known_cli_subpaths() -> &'static [&'static [&'static str]] {
    &[
        // npm global: %APPDATA%\npm\claude.cmd → ./node_modules/...
        &["node_modules", "@anthropic-ai", "claude-code", "cli.js"],
        &["node_modules", "@anthropic-ai", "claude-code", "cli.mjs"],
        // npm prefix (Node native installer): <prefix>/bin/claude.cmd → <prefix>/lib/node_modules/...
        &["..", "lib", "node_modules", "@anthropic-ai", "claude-code", "cli.js"],
        // yarn global / generic ../node_modules
        &["..", "node_modules", "@anthropic-ai", "claude-code", "cli.js"],
        // Bun global: <bun>/bin/claude.cmd → <bun>/install/global/node_modules/...
        &["..", "install", "global", "node_modules", "@anthropic-ai", "claude-code", "cli.js"],
        // Bun packages layout
        &["..", "packages", "@anthropic-ai", "claude-code", "cli.js"],
    ]
}

/// Find a usable `node.exe` near a shim. Falls back to bare `"node"` (relying
/// on `PATH`) so the spawn still succeeds when Node isn't co-located.
#[cfg(windows)]
fn locate_node(dir: &std::path::Path) -> String {
    let alongside = dir.join("node.exe");
    if alongside.exists() {
        return alongside.to_string_lossy().to_string();
    }
    if let Some(parent) = dir.parent() {
        let up = parent.join("node.exe");
        if up.exists() {
            return up.to_string_lossy().to_string();
        }
    }
    "node".to_string()
}

// ── File locking ─────────────────────────────────────────────

/// Acquire an exclusive (write) lock on an open file handle.
///
/// On Unix, uses POSIX `flock(LOCK_EX)`. On Windows, uses `LockFileEx`.
/// The lock is advisory on Unix and mandatory on Windows.
/// The lock is automatically released when the `File` is dropped.
pub fn flock_exclusive(file: &File) -> std::io::Result<()> {
    sys::flock_exclusive(file)
}

/// Acquire a shared (read) lock on an open file handle.
pub fn flock_shared(file: &File) -> std::io::Result<()> {
    sys::flock_shared(file)
}

// ── File permissions ─────────────────────────────────────────

/// Set file permissions to owner-only read/write (0o600 on Unix).
///
/// On Windows, this is a no-op — NTFS ACLs handle permissions differently
/// and the file is already restricted to the current user by default.
pub fn set_owner_only(path: &Path) -> std::io::Result<()> {
    sys::set_owner_only(path)
}

/// Set file permissions to owner read/write + executable (0o755 on Unix).
///
/// On Windows, this is a no-op — executability is determined by file extension.
pub fn set_executable(path: &Path) -> std::io::Result<()> {
    sys::set_executable(path)
}

/// Check if a directory has group/other write bits set (insecure).
///
/// On Windows, always returns `false` (not applicable).
pub fn is_world_writable(path: &Path) -> bool {
    sys::is_world_writable(path)
}

/// Check if a file has group/other permission bits set.
///
/// On Windows, always returns `false` (not applicable).
pub fn has_loose_permissions(path: &Path) -> bool {
    sys::has_loose_permissions(path)
}

// ── Process management ───────────────────────────────────────

/// Send a graceful termination signal to a process (SIGTERM on Unix, TerminateProcess on Windows).
pub fn terminate_process(pid: u32) -> std::io::Result<()> {
    sys::terminate_process(pid)
}

/// Forcefully kill a process (SIGKILL on Unix, TerminateProcess on Windows).
pub fn kill_process(pid: u32) -> std::io::Result<()> {
    sys::kill_process(pid)
}

/// Send SIGINT to the current process for graceful self-shutdown.
///
/// On Windows, uses `GenerateConsoleCtrlEvent(CTRL_C_EVENT)`.
pub fn self_interrupt() {
    sys::self_interrupt();
}

// ── Unix implementation ──────────────────────────────────────

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    pub fn flock_exclusive(file: &File) -> std::io::Result<()> {
        // SAFETY: fd is a valid, open file descriptor.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn flock_shared(file: &File) -> std::io::Result<()> {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
        if rc != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn set_owner_only(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }

    pub fn set_executable(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
    }

    pub fn is_world_writable(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .map(|m| m.mode() & 0o022 != 0)
            .unwrap_or(false)
    }

    pub fn has_loose_permissions(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .map(|m| m.mode() & 0o077 != 0)
            .unwrap_or(false)
    }

    pub fn terminate_process(pid: u32) -> std::io::Result<()> {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn kill_process(pid: u32) -> std::io::Result<()> {
        let rc = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if rc != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn self_interrupt() {
        unsafe { libc::kill(libc::getpid(), libc::SIGINT); }
    }
}

// ── Windows implementation ───────────────────────────────────

#[cfg(windows)]
mod sys {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    pub fn flock_exclusive(file: &File) -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCK_FILE_FLAGS,
        };
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let handle = file.as_raw_handle() as HANDLE;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        // LockFileEx: flags = LOCKFILE_EXCLUSIVE_LOCK for exclusive lock
        let result = unsafe {
            windows_sys::Win32::Storage::FileSystem::LockFileEx(
                handle,
                LOCKFILE_EXCLUSIVE_LOCK as LOCK_FILE_FLAGS,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn flock_shared(file: &File) -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let handle = file.as_raw_handle() as HANDLE;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        // flags = 0 means shared lock (no LOCKFILE_EXCLUSIVE_LOCK flag)
        let result = unsafe {
            windows_sys::Win32::Storage::FileSystem::LockFileEx(
                handle,
                0,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn set_owner_only(_path: &Path) -> std::io::Result<()> {
        // On Windows, files are owned by the creating user by default.
        // Fine-grained ACL manipulation is out of scope; skip.
        Ok(())
    }

    pub fn set_executable(_path: &Path) -> std::io::Result<()> {
        // On Windows, executability is determined by file extension (.exe).
        Ok(())
    }

    pub fn is_world_writable(_path: &Path) -> bool {
        false
    }

    pub fn has_loose_permissions(_path: &Path) -> bool {
        false
    }

    pub fn terminate_process(pid: u32) -> std::io::Result<()> {
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        use windows_sys::Win32::Foundation::CloseHandle;

        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let result = unsafe { TerminateProcess(handle, 1) };
        unsafe { CloseHandle(handle); }
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn kill_process(pid: u32) -> std::io::Result<()> {
        // On Windows, TerminateProcess is always forceful (no graceful equivalent).
        terminate_process(pid)
    }

    pub fn self_interrupt() {
        use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
        // CTRL_C_EVENT = 0
        unsafe { GenerateConsoleCtrlEvent(0, 0); }
    }
}

// ── Shim parser tests ─────────────────────────────────────────
//
// These tests are cross-platform-compiled — they exercise pure string
// parsing (`parse_shim_script_path`) and static data (`known_cli_subpaths`)
// without touching the filesystem or invoking any Windows APIs, so the
// host can be macOS or Linux.
#[cfg(test)]
mod shim_parser_tests {
    use super::{known_cli_subpaths, parse_shim_script_path};

    #[test]
    fn parses_npm_v9_shim() {
        // Real npm@9 shim format (slightly trimmed for brevity).
        let content = r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0

IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
  SET PATHEXT=%PATHEXT:;.JS;=;%
)

endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@anthropic-ai\claude-code\cli.mjs" %*
"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("node_modules/@anthropic-ai/claude-code/cli.mjs"),
        );
    }

    #[test]
    fn parses_bun_shim_with_relative_packages_path() {
        // Bun's `bun install -g` shim format.
        let content = r#"@"%~dp0\..\bun.exe" "%~dp0\..\packages\@anthropic-ai\claude-code\cli.js" %*"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("../packages/@anthropic-ai/claude-code/cli.js"),
        );
    }

    #[test]
    fn parses_pnpm_global_shim() {
        let content = r#"@"%~dp0\node.exe" "%~dp0\..\global\5\node_modules\@anthropic-ai\claude-code\cli.js" %*"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("../global/5/node_modules/@anthropic-ai/claude-code/cli.js"),
        );
    }

    #[test]
    fn parses_yarn_classic_global_shim() {
        let content = r#"@node "%~dp0..\lib\node_modules\@anthropic-ai\claude-code\cli.js" %*"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("../lib/node_modules/@anthropic-ai/claude-code/cli.js"),
        );
    }

    #[test]
    fn picks_js_target_when_node_exe_also_in_line() {
        // The "node.exe" token must NOT win — only `.js`/`.mjs`/`.cjs`.
        let content = r#"@"%~dp0\node.exe" "%~dp0\node_modules\foo\cli.js" %*"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("node_modules/foo/cli.js"),
        );
    }

    #[test]
    fn returns_none_for_pure_exe_wrapper() {
        // Scoop / Volta shims that just call a `.exe` directly — not a Node script.
        let content = r#"@"%~dp0\..\apps\claude\current\bin\claude.exe" %*"#;
        assert!(parse_shim_script_path(content).is_none());
    }

    #[test]
    fn returns_none_for_empty_shim() {
        assert!(parse_shim_script_path("").is_none());
    }

    #[test]
    fn handles_unquoted_token() {
        // Some hand-written shims omit quotes. Whitespace fallback should still match.
        let content = "@node %~dp0\\cli.mjs %*";
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("cli.mjs"),
        );
    }

    #[test]
    fn handles_cjs_extension() {
        let content = r#"@node "%~dp0\node_modules\foo\bar.cjs" %*"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("node_modules/foo/bar.cjs"),
        );
    }

    #[test]
    fn picks_last_js_when_multiple_in_line() {
        // If a shim mentions multiple JS paths on one line (e.g. wrapper +
        // delegate), the last one is the actual invocation target.
        let content = r#"@node "%~dp0\wrapper.js" "%~dp0\real-cli.js" %*"#;
        assert_eq!(
            parse_shim_script_path(content).as_deref(),
            Some("real-cli.js"),
        );
    }

    #[test]
    fn known_cli_subpaths_cover_major_package_managers() {
        // Light sanity check — make sure none of the entries are accidentally
        // empty and that we cover at least npm + Bun (different parent depths).
        let paths = known_cli_subpaths();
        assert!(paths.len() >= 4, "expected coverage for npm/yarn/pnpm/Bun");
        assert!(paths.iter().all(|p| !p.is_empty()));
        assert!(
            paths.iter().any(|p| p.first() == Some(&"node_modules")),
            "missing direct node_modules entry (npm global)",
        );
        assert!(
            paths.iter().any(|p| p.first() == Some(&"..")),
            "missing parent-relative entry (Bun/yarn/pnpm)",
        );
    }
}
