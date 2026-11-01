//! Kill a spawned CLI together with everything it started.
//!
//! `kill_on_drop` only signals the direct child. The Claude CLI starts its own
//! children (MCP servers, tool subprocesses), and a stub or wrapper script
//! may run the real work in a grandchild; when a generation job is cancelled
//! or times out, those used to survive as orphans. The CLI is spawned as the
//! leader of its own process group ([`isolate`]), and [`GroupKillGuard`]
//! sends `SIGKILL` to the whole group when it is dropped while still armed —
//! which is exactly what happens when the awaiting task is aborted, or when a
//! timeout path returns early. Normal completion releases the guard; Windows
//! also terminates any remaining descendants when closing its job handle.
//!
//! Windows owns a kill-on-close Job Object assigned before resuming the child. Unix
//! uses an isolated process group. Cancellation also schedules leader reaping.

/// Put the command's child in a new process group led by itself (Unix).
pub fn isolate(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Kills the child's process group on drop unless [`disarm`]ed.
///
/// Declare it AFTER the `Child` it watches, so it drops first: the leader is
/// then still unreaped (at worst a zombie), its pid cannot have been reused,
/// and the group id is still valid.
///
/// [`disarm`]: GroupKillGuard::disarm
pub struct GroupKillGuard {
    #[cfg(not(windows))]
    pgid: Option<u32>,
    #[cfg(windows)]
    job: Option<crate::platform::WindowsJob>,
}

impl GroupKillGuard {
    /// `pid` is the child's pid (= its process-group id after [`isolate`]).
    #[cfg(not(windows))]
    pub fn new(pid: Option<u32>) -> Self {
        Self { pgid: pid }
    }

    /// Kill the whole group now (timeout / error paths that then reap the
    /// leader themselves) and disarm.
    pub fn kill_now(&mut self) {
        self.kill_group();
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take()
            && let Err(error) = crate::platform::kill_process_group(pgid)
            && error.raw_os_error() != Some(libc::ESRCH)
        {
            tracing::warn!(pgid, %error, "killpg of CLI process group failed");
        }
        #[cfg(windows)]
        if let Some(mut job) = self.job.take() {
            if let Err(error) = job.terminate() {
                tracing::warn!(%error, "terminating CLI job failed; closed kill-on-close handle");
            }
        }
        #[cfg(not(any(unix, windows)))]
        { self.pgid = None; }
    }

    /// Release ownership after reaping the leader. Windows closes the job and
    /// terminates any remaining descendants even on a normal exit.
    pub fn disarm(&mut self) {
        #[cfg(not(windows))]
        { self.pgid = None; }
        #[cfg(windows)]
        { self.kill_group(); }
    }
}

impl Drop for GroupKillGuard {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// Spawn with tree ownership established before any child code executes.
/// Declare the returned guard after the child so tree termination runs first.
pub fn spawn_guarded(cmd: &mut tokio::process::Command)
    -> std::io::Result<(tokio::process::Child, GroupKillGuard)>
{
    isolate(cmd);
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::io::BorrowedHandle;
        let job = crate::platform::WindowsJob::new()?;
        // Current managed callers and async_command_for set no Windows flags.
        // Keep creation flags owned here: std/Tokio provide no flags getter.
        cmd.creation_flags(crate::platform::WindowsJob::CREATION_FLAGS);
        let mut child = cmd.spawn()?;
        let result = match child.raw_handle() {
            Some(raw) => {
                // SAFETY: child owns this live handle until after the synchronous call.
                job.assign_and_resume(unsafe { BorrowedHandle::borrow_raw(raw) })
            }
            None => Err(std::io::Error::other("spawned child has no process handle")),
        };
        if let Err(error) = result {
            // Assignment/resume failure must never leave an unmanaged child.
            drop(job);
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { let _ = child.wait().await; });
            }
            return Err(error);
        }
        Ok((child, GroupKillGuard { job: Some(job) }))
    }
    #[cfg(not(windows))]
    {
        let child = cmd.spawn()?;
        let guard = GroupKillGuard::new(child.id());
        Ok((child, guard))
    }
}

/// Own the leader until its process group has been terminated or disarmed.
/// Unlike dropping `wait_with_output`, cancellation explicitly kills the tree
/// before handing the leader to a reaper task.
pub struct ManagedChild {
    child: Option<tokio::process::Child>,
    guard: GroupKillGuard,
}

impl ManagedChild {
    pub fn spawn(cmd: &mut tokio::process::Command) -> std::io::Result<Self> {
        let (child, guard) = spawn_guarded(cmd)?;
        Ok(Self { child: Some(child), guard })
    }

    pub fn child(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("managed leader is present")
    }

    /// The leader has exited; do not retain a reaped leader's PGID while
    /// waiting for pipe EOF. No descendant may outlive this one-shot call.
    pub fn leader_exited(&mut self) {
        self.guard.kill_now();
    }

    pub fn disarm(&mut self) {
        self.guard.disarm();
        self.child = None;
    }

    pub async fn kill_and_reap(&mut self) {
        self.guard.kill_now();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            // Tokio's Child drop remains a final backstop if this wait itself
            // is cancelled. Normal timeout/error paths reap explicitly.
            let _ = child.wait().await;
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.guard.kill_now();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { let _ = child.wait().await; });
            }
        }
    }
}

