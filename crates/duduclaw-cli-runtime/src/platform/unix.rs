//! Unix child-group supervision.
//!
//! When `portable-pty` spawns a child under a PTY, the slave side runs in a
//! new session (`setsid`) and the child becomes its own process-group
//! leader. We exploit that here: `killpg(pid, SIG)` then targets every
//! descendant of the child too, so a long-running `claude` that itself
//! spawned helper subprocesses (Bash tools, Python eval, etc.) gets cleaned
//! up by a single signal.
//!
//! If `killpg` returns `ESRCH` / `EPERM` (e.g. the child wasn't actually a
//! pgrp leader for some reason), we fall back to a direct `kill(pid, SIG)`
//! so the main process at least gets the signal. Final SIGKILL escalation
//! is handled by `portable-pty::Child::kill` inside `PtyHandle::shutdown` —
//! `ChildGroup::terminate` only owns the gentle SIGTERM step.

use std::time::Duration;

use tracing::{debug, warn};

/// Managed handle for a child process group spawned under a PTY.
///
/// The underlying child process is owned by `portable-pty`; this struct
/// only tracks the pid for signal delivery. It is safe to construct
/// multiple instances pointing at the same pid (idempotent terminate).
#[derive(Debug, Clone, Copy)]
pub struct ChildGroup {
    pid: u32,
}

impl ChildGroup {
    pub fn new(pid: u32) -> Self {
        Self { pid }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Send a gentle termination signal to the child + its process group.
    ///
    /// `_grace` is intentionally unused inside this method — the caller is
    /// expected to `tokio::time::sleep(grace).await` *between* this call
    /// and the SIGKILL escalation in `PtyHandle::shutdown`. We don't sleep
    /// here because this is a sync entry point and blocking would freeze
    /// the caller's runtime.
    pub fn terminate(&self, _grace: Duration) {
        use nix::sys::signal::{Signal, killpg, kill};
        use nix::unistd::Pid;

        let pid = Pid::from_raw(self.pid as i32);

        // Try the whole pgrp first.
        match killpg(pid, Signal::SIGTERM) {
            Ok(()) => {
                debug!(pid = self.pid, "ChildGroup::terminate killpg SIGTERM sent");
                return;
            }
            Err(nix::errno::Errno::ESRCH) => {
                // Group doesn't exist — child probably already exited.
                debug!(pid = self.pid, "ChildGroup::terminate pgrp gone (ESRCH)");
                return;
            }
            Err(e) => {
                debug!(
                    pid = self.pid,
                    error = %e,
                    "ChildGroup::terminate killpg failed; falling back to direct kill"
                );
            }
        }

        // Fallback: signal the leader directly. If it's not actually a
        // pgrp leader the descendants may leak; portable-pty's slave-fd
        // close will SIGHUP them as a last-resort cleanup.
        match kill(pid, Signal::SIGTERM) {
            Ok(()) => debug!(pid = self.pid, "ChildGroup::terminate fallback kill SIGTERM sent"),
            Err(nix::errno::Errno::ESRCH) => {
                debug!(pid = self.pid, "ChildGroup::terminate fallback ESRCH — already gone");
            }
            Err(e) => {
                warn!(
                    pid = self.pid,
                    error = %e,
                    "ChildGroup::terminate SIGTERM delivery failed"
                );
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
        // PID 0xFFFFFFFE (~ max u32) is essentially guaranteed not to be
        // alive. We just want to confirm the function doesn't panic and
        // tolerates ESRCH.
        let grp = ChildGroup::new(0xFFFFFFFE);
        grp.terminate(Duration::from_millis(0));
    }

    #[test]
    fn terminate_is_idempotent_for_dead_child() {
        let grp = ChildGroup::new(0xFFFFFFFE);
        for _ in 0..3 {
            grp.terminate(Duration::from_millis(0));
        }
    }
}
