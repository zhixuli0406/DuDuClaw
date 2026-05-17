//! Windows child-group supervision via Job Objects.
//!
//! Windows has no `killpg` equivalent. We achieve the same "kill the whole
//! tree" guarantee by wrapping the child in a [Job Object][jo] with the
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag set. When [`ChildGroup`] is
//! dropped (or `terminate` is called), the OS terminates every process
//! inside the job — including helpers / Bash tools that `claude` spawned
//! after we assigned the job.
//!
//! Lifecycle:
//! 1. [`ChildGroup::new`] creates a fresh, anonymous job object and assigns
//!    the child pid to it.
//! 2. [`ChildGroup::terminate`] calls `TerminateJobObject` to kill the
//!    entire job synchronously.
//! 3. [`Drop`] closes the job handle; with `KILL_ON_JOB_CLOSE` set, any
//!    still-alive children die at this point too.
//!
//! Race window: between `portable-pty`'s `spawn_command` and our
//! `ChildGroup::new`, the child has a small window in which descendants
//! it spawns would NOT inherit the job. This is microseconds in practice
//! and matters only for pathological cases.
//!
//! [jo]: https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects

use std::time::Duration;

use tracing::{debug, warn};

#[cfg(windows)]
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        SetInformationJobObject, TerminateJobObject,
    },
    System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
};

/// Managed handle for a child process tree on Windows.
pub struct ChildGroup {
    pid: u32,
    #[cfg(windows)]
    job: Option<HANDLE>,
}

// SAFETY: HANDLE is a raw pointer that's just an opaque kernel resource id;
// we never dereference it from Rust. The Windows API is thread-safe for
// the Job Object operations we use.
#[cfg(windows)]
unsafe impl Send for ChildGroup {}
#[cfg(windows)]
unsafe impl Sync for ChildGroup {}

impl ChildGroup {
    /// Create a Job Object, set kill-on-close, and assign `pid` to it.
    ///
    /// On any failure (job creation, process open, assign), we log + fall
    /// back to a "pid-only" mode where `terminate` can still attempt
    /// `TerminateProcess`. The struct never panics on construction.
    pub fn new(pid: u32) -> Self {
        #[cfg(windows)]
        unsafe {
            let job = match CreateJobObjectW(None, None) {
                Ok(h) if !h.is_invalid() => h,
                Ok(_) => {
                    warn!(pid, "ChildGroup::new: CreateJobObjectW returned invalid handle");
                    return Self { pid, job: None };
                }
                Err(e) => {
                    warn!(pid, error = %e, "ChildGroup::new: CreateJobObjectW failed");
                    return Self { pid, job: None };
                }
            };

            // Configure KILL_ON_JOB_CLOSE so dropping the handle reaps
            // the entire tree.
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let info_ptr = &info as *const _ as *const std::ffi::c_void;
            let info_size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            if let Err(e) = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                info_ptr,
                info_size,
            ) {
                warn!(pid, error = %e, "ChildGroup::new: SetInformationJobObject failed");
                let _ = CloseHandle(job);
                return Self { pid, job: None };
            }

            // Open the child process with the rights needed for
            // AssignProcessToJobObject + (later) TerminateProcess.
            let proc = match OpenProcess(
                PROCESS_SET_QUOTA | PROCESS_TERMINATE,
                false,
                pid,
            ) {
                Ok(h) if !h.is_invalid() => h,
                Ok(_) | Err(_) => {
                    warn!(pid, "ChildGroup::new: OpenProcess returned invalid handle");
                    let _ = CloseHandle(job);
                    return Self { pid, job: None };
                }
            };

            let assign_result = AssignProcessToJobObject(job, proc);
            let _ = CloseHandle(proc);

            if let Err(e) = assign_result {
                warn!(
                    pid,
                    error = %e,
                    "ChildGroup::new: AssignProcessToJobObject failed (child may have already \
                     spawned grandchildren that won't be reaped)"
                );
                let _ = CloseHandle(job);
                return Self { pid, job: None };
            }

            debug!(pid, "ChildGroup::new: child assigned to Job Object");
            Self { pid, job: Some(job) }
        }

        #[cfg(not(windows))]
        {
            let _ = pid;
            unreachable!("windows.rs only compiled on cfg(windows)");
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Synchronously terminate every process in the job. `_grace` is
    /// intentionally unused — Job Object termination is immediate (no
    /// SIGTERM equivalent on Windows; TerminateJobObject is the moral
    /// equivalent of SIGKILL on a process group). Callers that want a
    /// grace period should sleep BEFORE calling terminate, but typically
    /// the gentle path on Windows is just `WM_CLOSE` to the child's main
    /// window, which doesn't apply to claude CLI.
    pub fn terminate(&self, _grace: Duration) {
        #[cfg(windows)]
        unsafe {
            if let Some(job) = self.job {
                if let Err(e) = TerminateJobObject(job, 1) {
                    warn!(
                        pid = self.pid,
                        error = %e,
                        "ChildGroup::terminate TerminateJobObject failed"
                    );
                } else {
                    debug!(pid = self.pid, "ChildGroup::terminate TerminateJobObject ok");
                }
            } else {
                // No job — best-effort TerminateProcess on the leader.
                if let Ok(proc) = OpenProcess(PROCESS_TERMINATE, false, self.pid) {
                    if !proc.is_invalid() {
                        let _ = windows::Win32::System::Threading::TerminateProcess(proc, 1);
                        let _ = CloseHandle(proc);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ChildGroup {
    fn drop(&mut self) {
        unsafe {
            if let Some(job) = self.job.take() {
                // KILL_ON_JOB_CLOSE ensures any remaining processes die
                // when this handle's last reference is closed.
                let _ = CloseHandle(job);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_group_records_pid() {
        let grp = ChildGroup::new(12345);
        assert_eq!(grp.pid(), 12345);
    }

    #[test]
    fn terminate_against_nonexistent_pid_is_safe() {
        // Doesn't panic; logs may emit.
        let grp = ChildGroup::new(0xFFFFFFFE);
        grp.terminate(Duration::from_millis(0));
    }
}
