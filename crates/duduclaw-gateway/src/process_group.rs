//! Kill a spawned CLI together with everything it started.
//!
//! `kill_on_drop` only signals the direct child. The Claude CLI starts its own
//! children (MCP servers, tool subprocesses), and a stub or wrapper script
//! may run the real work in a grandchild; when a generation job is cancelled
//! or times out, those used to survive as orphans. The CLI is spawned as the
//! leader of its own process group ([`isolate`]), and [`GroupKillGuard`]
//! sends `SIGKILL` to the whole group when it is dropped while still armed —
//! which is exactly what happens when the awaiting task is aborted, or when a
//! timeout path returns early. A normal exit disarms the guard first, so a
//! finished group is never signalled.
//!
//! Windows has no process-group signal; there the guard does nothing and
//! `kill_on_drop` remains the only cleanup (documented limitation).

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
    pgid: Option<u32>,
}

impl GroupKillGuard {
    /// `pid` is the child's pid (= its process-group id after [`isolate`]).
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
        if let Some(pgid) = self.pgid.take() {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            match killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(e) => tracing::warn!(pgid, error = %e, "killpg of CLI process group failed"),
            }
        }
        #[cfg(not(unix))]
        {
            self.pgid = None;
        }
    }

    /// The child exited normally and was reaped: nothing left to kill.
    pub fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for GroupKillGuard {
    fn drop(&mut self) {
        self.kill_group();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    /// A grandchild (`sleep` started by the shell) dies with the group.
    #[tokio::test]
    async fn dropping_an_armed_guard_kills_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!("/bin/sleep 30 & echo $! > '{}'; wait", pidfile.display()))
            .kill_on_drop(true);
        isolate(&mut cmd);
        let child = cmd.spawn().unwrap();
        let guard = GroupKillGuard::new(child.id());
        let mut grandchild = 0;
        for _ in 0..100 {
            if let Some(p) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                grandchild = p;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(grandchild > 0 && alive(grandchild));
        drop(guard);
        drop(child);
        // The killed sleep is reparented and reaped by init; wait for it.
        let mut gone = false;
        for _ in 0..100 {
            if !alive(grandchild) {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone, "grandchild {grandchild} survived the group kill");
    }

    #[tokio::test]
    async fn a_disarmed_guard_signals_nothing() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 0");
        isolate(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let mut guard = GroupKillGuard::new(child.id());
        assert!(child.wait().await.unwrap().success());
        guard.disarm();
        drop(guard); // no panic, no signal
    }
}
