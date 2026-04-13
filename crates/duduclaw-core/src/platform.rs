//! Cross-platform utilities for file locking, permissions, and process management.
//!
//! This module abstracts away Unix-specific APIs (flock, chmod, signals) so the
//! rest of the codebase compiles and runs on both Unix and Windows.

use std::fs::File;
use std::path::Path;

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
