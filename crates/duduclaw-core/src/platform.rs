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

/// Create a `std::process::Command` for a program, handling Windows shims.
///
/// On Windows, `.cmd`/`.bat` shims trigger Rust 1.77+'s BatBadBut rejection
/// (CVE-2024-24576) when args contain newlines / quotes / `&`. Instead of
/// spawning the shim directly we parse it to find the underlying real
/// executable (`.exe` for native binaries, or `node.exe + cli.js` for
/// JavaScript CLIs) and invoke it directly — clean argument passing, no
/// shell, no BatBadBut.
///
/// On Unix, this is a pass-through to `Command::new(program)`.
pub fn command_for(program: &str) -> std::process::Command {
    #[cfg(windows)]
    if let Some((real, prefix)) = resolve_cmd_shim(program) {
        let mut cmd = std::process::Command::new(real);
        for arg in &prefix {
            cmd.arg(arg);
        }
        return cmd;
    }
    std::process::Command::new(program)
}

/// Create a `tokio::process::Command`, handling Windows shims (see
/// [`command_for`] for the rationale).
pub fn async_command_for(program: &str) -> tokio::process::Command {
    #[cfg(windows)]
    if let Some((real, prefix)) = resolve_cmd_shim(program) {
        let mut cmd = tokio::process::Command::new(real);
        for arg in &prefix {
            cmd.arg(arg);
        }
        return cmd;
    }
    tokio::process::Command::new(program)
}

/// On Windows, resolve a `.cmd`/`.bat` shim (npm, Bun, pnpm, yarn …) to a
/// real spawnable executable plus any prefix args, so callers can avoid
/// handing user content to `cmd.exe`. Returns `(program, prefix_args)`:
///
/// - **Native-binary shim** → `(<path/to/foo.exe>, vec![])`. New-style
///   `@anthropic-ai/claude-code` (≥ 2.x) ships a `.exe` inside the npm
///   package and the shim is just a transfer shim — we follow it to the
///   `.exe` and spawn directly.
/// - **JavaScript shim** → `(<path/to/node.exe>, vec![<path/to/cli.js>])`
///   for older / pure-JS CLIs.
///
/// Two strategies in order, each returning either kind:
///
/// 1. **Parse the shim file** — scan every quoted segment + whitespace
///    token for a path ending in `.exe` / `.js` / `.mjs` / `.cjs`, expand
///    common shim variables (`%~dp0`, `%dp0%`, `%~dpn0`, `%~f0`, `%CD%`)
///    and join with the shim's directory. `.exe` references take
///    precedence over JS references when both appear.
///
/// 2. **Probe known package layouts** — if shim parsing fails (binary
///    wrappers, custom shims), check well-known relative paths where
///    `@anthropic-ai/claude-code` keeps either `bin/claude.exe` (≥ 2.x)
///    or `cli.js` / `cli.mjs` (legacy) for npm / Bun / yarn / pnpm.
///
/// Returns `None` only if neither strategy resolves a real file. In that
/// case the caller falls back to spawning the `.cmd` directly — which may
/// then trip BatBadBut, but at least both clean paths were attempted.
#[cfg(windows)]
fn resolve_cmd_shim(program: &str) -> Option<(String, Vec<String>)> {
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
        && let Some(target) = parse_shim_target(&content)
    {
        let candidate = dir.join(target.relative_path());
        if candidate.exists() {
            return Some(invocation_for(target.kind(), &candidate, dir));
        }
    }

    // Strategy B: probe well-known package layouts.
    for probe in known_target_subpaths() {
        let mut p = dir.to_path_buf();
        for seg in probe.parts {
            p.push(seg);
        }
        if p.exists() {
            return Some(invocation_for(probe.kind, &p, dir));
        }
    }

    None
}

/// Build the `(program, prefix_args)` invocation tuple for a resolved
/// shim target — either a direct `.exe` or a `node.exe + cli.js` pair.
#[cfg(windows)]
fn invocation_for(
    kind: ShimKind,
    target: &std::path::Path,
    shim_dir: &std::path::Path,
) -> (String, Vec<String>) {
    match kind {
        ShimKind::Exe => (target.to_string_lossy().to_string(), Vec::new()),
        ShimKind::Script => (
            locate_node(shim_dir),
            vec![target.to_string_lossy().to_string()],
        ),
    }
}

/// What the shim ultimately invokes — a native binary, or a JS script via Node.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShimKind {
    Exe,
    Script,
}

/// A resolved (kind, relative-path) pair from the shim parser.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShimTarget {
    kind: ShimKind,
    rel: String,
}

#[cfg(any(windows, test))]
impl ShimTarget {
    fn kind(&self) -> ShimKind {
        self.kind
    }
    fn relative_path(&self) -> &str {
        &self.rel
    }
}

/// Parse the invocation line of a Windows shim and return what it ultimately
/// runs — either a native `.exe` or a JS script — with shim variables
/// expanded to empty strings (so the path is relative to the shim's
/// directory).
///
/// **Target selection rule** (per line, walking from the bottom):
///
/// - When the line mentions BOTH a `.exe` AND a `.js`/`.mjs`/`.cjs`, the JS
///   path wins. The `.exe` in that case is almost always a runtime
///   (`node.exe`, `bun.exe`) and the script is what we actually want to run.
///   Example: Bun's `@"%~dp0\..\bun.exe" "%~dp0\..\packages\…\cli.js" %*`.
///
/// - When the line mentions ONLY a `.exe` (no script), the `.exe` is the
///   real target. Example: new-style `@anthropic-ai/claude-code` ≥ 2.x
///   shim — `"%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe" %*`.
///
/// **Pass strategy**:
///
/// 1. Scan double-quoted segments (most reliable for npm/Bun/pnpm/yarn).
/// 2. ONLY if pass 1 found nothing for this line, scan whitespace-separated
///    tokens (handles unquoted hand-written shims). Skipping pass 2 when
///    pass 1 succeeded prevents noisy tokens like `@"…\bun.exe` (a single
///    whitespace token containing embedded quotes) from contaminating the
///    cleanly-extracted result.
///
/// Cross-platform-compiled so unit tests can exercise it on any host.
#[cfg(any(windows, test))]
fn parse_shim_target(content: &str) -> Option<ShimTarget> {
    // Walk lines in reverse — the actual invocation is at the bottom of the
    // shim (after all the `IF EXIST` / `SETLOCAL` boilerplate).
    for line in content.lines().rev() {
        let mut last_exe: Option<String> = None;
        let mut last_script: Option<String> = None;

        // Pass 1: every double-quoted segment (odd indices when split by `"`).
        for (i, segment) in line.split('"').enumerate() {
            if i % 2 == 1 {
                let _ = stash_token(segment, &mut last_exe, &mut last_script);
            }
        }
        // Pass 2: whitespace tokens — only when pass 1 yielded nothing.
        if last_exe.is_none() && last_script.is_none() {
            for token in line.split_whitespace() {
                let unquoted = token.trim_matches(['"', '\'']);
                let _ = stash_token(unquoted, &mut last_exe, &mut last_script);
            }
        }

        // Script wins over .exe: when both appear, .exe is the runtime
        // (node.exe / bun.exe) and the script is the actual target. The
        // .exe path is only used when there's no script in the line.
        if let Some(rel) = last_script {
            return Some(ShimTarget {
                kind: ShimKind::Script,
                rel,
            });
        }
        if let Some(rel) = last_exe {
            return Some(ShimTarget {
                kind: ShimKind::Exe,
                rel,
            });
        }
    }
    None
}

/// If `raw` looks like a relevant invocation target, store its cleaned path
/// into the appropriate slot. `.exe` and `.js`/`.mjs`/`.cjs` are recognized;
/// other extensions and uninteresting tokens (variables, control words) are
/// skipped silently.
#[cfg(any(windows, test))]
fn stash_token(
    raw: &str,
    last_exe: &mut Option<String>,
    last_script: &mut Option<String>,
) -> Option<()> {
    let trimmed = raw.trim();
    let lower = trimmed.to_lowercase();
    let is_exe = lower.ends_with(".exe");
    let is_script =
        lower.ends_with(".mjs") || lower.ends_with(".js") || lower.ends_with(".cjs");
    if !is_exe && !is_script {
        return None;
    }
    let cleaned = clean_shim_token_path(trimmed)?;
    if is_exe {
        *last_exe = Some(cleaned);
    } else {
        *last_script = Some(cleaned);
    }
    Some(())
}

/// Strip shim variables from a path token and normalize separators. Used by
/// [`stash_token`] after the extension classification.
#[cfg(any(windows, test))]
fn clean_shim_token_path(raw: &str) -> Option<String> {
    let expanded = raw
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

/// A known-layout probe: relative path segments from the shim's directory
/// plus what kind of target lives there.
#[cfg(any(windows, test))]
struct KnownProbe {
    kind: ShimKind,
    parts: &'static [&'static str],
}

/// Well-known package layouts to probe when shim parsing fails. Each entry
/// targets either `@anthropic-ai/claude-code/bin/claude.exe` (new-style) or
/// `cli.js` / `cli.mjs` (legacy) across npm / Bun / yarn / pnpm globals.
#[cfg(any(windows, test))]
fn known_target_subpaths() -> &'static [KnownProbe] {
    use ShimKind::{Exe, Script};
    &[
        // ── New-style: native .exe inside the npm package ────────
        // npm global: %APPDATA%\npm\claude.cmd → ./node_modules/.../bin/claude.exe
        KnownProbe {
            kind: Exe,
            parts: &[
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "bin",
                "claude.exe",
            ],
        },
        // npm prefix (Node native installer)
        KnownProbe {
            kind: Exe,
            parts: &[
                "..",
                "lib",
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "bin",
                "claude.exe",
            ],
        },
        // yarn global / generic ../node_modules
        KnownProbe {
            kind: Exe,
            parts: &[
                "..",
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "bin",
                "claude.exe",
            ],
        },
        // Bun global
        KnownProbe {
            kind: Exe,
            parts: &[
                "..",
                "install",
                "global",
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "bin",
                "claude.exe",
            ],
        },
        // Bun packages layout
        KnownProbe {
            kind: Exe,
            parts: &[
                "..",
                "packages",
                "@anthropic-ai",
                "claude-code",
                "bin",
                "claude.exe",
            ],
        },
        // ── Legacy: pure-JS CLI invoked via Node ─────────────────
        KnownProbe {
            kind: Script,
            parts: &["node_modules", "@anthropic-ai", "claude-code", "cli.js"],
        },
        KnownProbe {
            kind: Script,
            parts: &["node_modules", "@anthropic-ai", "claude-code", "cli.mjs"],
        },
        KnownProbe {
            kind: Script,
            parts: &[
                "..",
                "lib",
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "cli.js",
            ],
        },
        KnownProbe {
            kind: Script,
            parts: &[
                "..",
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "cli.js",
            ],
        },
        KnownProbe {
            kind: Script,
            parts: &[
                "..",
                "install",
                "global",
                "node_modules",
                "@anthropic-ai",
                "claude-code",
                "cli.js",
            ],
        },
        KnownProbe {
            kind: Script,
            parts: &[
                "..",
                "packages",
                "@anthropic-ai",
                "claude-code",
                "cli.js",
            ],
        },
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
// parsing (`parse_shim_target`) and static data (`known_target_subpaths`)
// without touching the filesystem or invoking any Windows APIs, so the
// host can be macOS or Linux.
#[cfg(test)]
mod shim_parser_tests {
    use super::{ShimKind, known_target_subpaths, parse_shim_target};

    fn assert_exe(content: &str, expected: &str) {
        let target = parse_shim_target(content).expect("expected a shim target");
        assert_eq!(target.kind(), ShimKind::Exe, "expected ShimKind::Exe");
        assert_eq!(target.relative_path(), expected);
    }

    fn assert_script(content: &str, expected: &str) {
        let target = parse_shim_target(content).expect("expected a shim target");
        assert_eq!(target.kind(), ShimKind::Script, "expected ShimKind::Script");
        assert_eq!(target.relative_path(), expected);
    }

    // ── New-style native-binary shims (the v1.8.33 bug fix) ──────

    #[test]
    fn parses_native_exe_npm_shim_for_claude_code_v2() {
        // The exact shim format that broke v1.8.32 in production:
        // @anthropic-ai/claude-code ≥ 2.x ships a real .exe inside the npm
        // package and the cmd shim is just a transfer wrapper.
        let content = r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0
"%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe"   %*
"#;
        assert_exe(
            content,
            "node_modules/@anthropic-ai/claude-code/bin/claude.exe",
        );
    }

    #[test]
    fn script_wins_over_exe_when_both_present_on_line() {
        // Bun's typical shim: `bun.exe` is the runtime, `cli.js` is the
        // target. This is the common case where naive ".exe wins" would
        // pick the wrong target. With the v1.8.33 rule (Script > Exe
        // when both are on a line), Script wins.
        let content =
            r#"@"%~dp0\..\bun.exe" "%~dp0\..\packages\foo\cli.js" %*"#;
        assert_script(content, "../packages/foo/cli.js");
    }

    #[test]
    fn last_exe_wins_when_only_exes_in_line() {
        // Two `.exe` tokens, no script: the LAST .exe is the actual target
        // (the first is typically a runtime check or boilerplate). This
        // mirrors the existing `picks_last_exe_when_multiple_exes_in_line`
        // case but uses a realistic claude-code path for clarity.
        let content =
            r#"@"%~dp0\node.exe" "%~dp0\node_modules\@anthropic-ai\claude-code\bin\claude.exe" %*"#;
        assert_exe(
            content,
            "node_modules/@anthropic-ai/claude-code/bin/claude.exe",
        );
    }

    // ── Legacy JS-script shims (npm / Bun / pnpm / yarn classic) ──

    #[test]
    fn parses_npm_v9_legacy_js_shim() {
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
        assert_script(content, "node_modules/@anthropic-ai/claude-code/cli.mjs");
    }

    #[test]
    fn parses_bun_shim_with_relative_packages_path() {
        let content = r#"@"%~dp0\..\bun.exe" "%~dp0\..\packages\@anthropic-ai\claude-code\cli.js" %*"#;
        assert_script(content, "../packages/@anthropic-ai/claude-code/cli.js");
    }

    #[test]
    fn parses_pnpm_global_shim() {
        let content = r#"@"%~dp0\node.exe" "%~dp0\..\global\5\node_modules\@anthropic-ai\claude-code\cli.js" %*"#;
        assert_script(
            content,
            "../global/5/node_modules/@anthropic-ai/claude-code/cli.js",
        );
    }

    #[test]
    fn parses_yarn_classic_global_shim() {
        let content =
            r#"@node "%~dp0..\lib\node_modules\@anthropic-ai\claude-code\cli.js" %*"#;
        assert_script(
            content,
            "../lib/node_modules/@anthropic-ai/claude-code/cli.js",
        );
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn returns_none_for_pure_exe_wrapper_to_external_path() {
        // Scoop-style absolute-.exe wrapper — looks like an exe target but
        // we only follow shims when the target lies UNDER the shim dir
        // (after %~dp0 stripping). This case still parses, but if the
        // resolved path doesn't exist the caller falls back to the
        // probes / direct-spawn — which for Scoop is fine, the shim's
        // own .exe lookup works in PATH.
        //
        // The parser intentionally still returns a candidate; existence
        // is verified by `resolve_cmd_shim` against the filesystem.
        let content = r#"@"%~dp0\..\apps\claude\current\bin\claude.exe" %*"#;
        assert_exe(content, "../apps/claude/current/bin/claude.exe");
    }

    #[test]
    fn returns_none_for_empty_shim() {
        assert!(parse_shim_target("").is_none());
    }

    #[test]
    fn handles_unquoted_token() {
        let content = "@node %~dp0\\cli.mjs %*";
        assert_script(content, "cli.mjs");
    }

    #[test]
    fn handles_cjs_extension() {
        let content = r#"@node "%~dp0\node_modules\foo\bar.cjs" %*"#;
        assert_script(content, "node_modules/foo/bar.cjs");
    }

    #[test]
    fn picks_last_js_when_multiple_js_in_line() {
        // Wrapper.js + real-cli.js → last one wins (only relevant when no
        // .exe is present, since .exe takes precedence over .js).
        let content = r#"@node "%~dp0\wrapper.js" "%~dp0\real-cli.js" %*"#;
        assert_script(content, "real-cli.js");
    }

    #[test]
    fn picks_last_exe_when_multiple_exes_in_line() {
        let content = r#"@node "%~dp0\first.exe" "%~dp0\second.exe" %*"#;
        assert_exe(content, "second.exe");
    }

    // ── known_target_subpaths sanity ─────────────────────────────

    #[test]
    fn known_target_subpaths_cover_native_and_legacy() {
        let probes = known_target_subpaths();

        let exe_count = probes.iter().filter(|p| p.kind == ShimKind::Exe).count();
        let script_count = probes
            .iter()
            .filter(|p| p.kind == ShimKind::Script)
            .count();

        // Need both kinds — new-style native .exe AND legacy .js coverage.
        assert!(
            exe_count >= 4,
            "expected ≥4 native-.exe probes (npm/yarn/Bun/pnpm), got {exe_count}"
        );
        assert!(
            script_count >= 4,
            "expected ≥4 JS-script probes (npm/yarn/Bun/pnpm), got {script_count}"
        );

        // Every probe targets @anthropic-ai/claude-code under some layout.
        assert!(
            probes.iter().all(|p| p.parts.contains(&"@anthropic-ai")
                && p.parts.contains(&"claude-code")),
            "every probe should target @anthropic-ai/claude-code"
        );

        // Native .exe probes end with claude.exe; script probes end with cli.{js,mjs}.
        for p in probes {
            let last = *p.parts.last().expect("non-empty");
            match p.kind {
                ShimKind::Exe => assert_eq!(
                    last, "claude.exe",
                    "exe probe must terminate at claude.exe"
                ),
                ShimKind::Script => assert!(
                    last == "cli.js" || last == "cli.mjs",
                    "script probe must terminate at cli.js/cli.mjs"
                ),
            }
        }
    }
}
