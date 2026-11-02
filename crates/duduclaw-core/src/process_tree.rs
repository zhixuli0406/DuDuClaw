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

use std::future::Future;
use std::sync::Arc;

/// An optional owner notified only after process-tree cleanup is confirmed.
/// The factory runs when the guard is created; its one-shot callback receives
/// false when cleanup fails, times out, or its observation task is dropped.
pub type CleanupObserver = Arc<dyn Fn() -> Box<dyn FnOnce(bool) + Send> + Send + Sync>;

tokio::task_local! {
    static CLEANUP_OBSERVER: CleanupObserver;
}

/// Attach cleanup accounting to every process guard created while polling
/// this future. The callback is captured by the guard and survives scope drop.
pub async fn with_cleanup_observer<F: Future>(observer: CleanupObserver, future: F) -> F::Output {
    CLEANUP_OBSERVER.scope(observer, future).await
}

struct CleanupReport(Option<Box<dyn FnOnce(bool) + Send>>);

impl CleanupReport {
    fn capture() -> Option<Self> {
        CLEANUP_OBSERVER.try_with(|observer| Self(Some(observer()))).ok()
    }

    fn finish(mut self, gone: bool) {
        if let Some(callback) = self.0.take() {
            callback(gone);
        }
    }
}

impl Drop for CleanupReport {
    fn drop(&mut self) {
        if let Some(callback) = self.0.take() {
            callback(false);
        }
    }
}

#[cfg(unix)]
fn group_is_gone(pgid: u32) -> std::io::Result<bool> {
    let group = i32::try_from(pgid).ok().filter(|group| *group > 1)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid process group"))?;
    // SAFETY: signal 0 only checks the isolated group's existence; it never
    // sends a signal or dereferences memory. Reserved group ids are refused.
    if unsafe { libc::kill(-group, 0) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) { Ok(true) } else { Err(error) }
}

#[cfg(unix)]
async fn observe_group(pgid: u32, report: CleanupReport, wait: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match group_is_gone(pgid) {
            Ok(true) => return report.finish(true),
            Ok(false) | Err(_) if tokio::time::Instant::now() >= deadline => return report.finish(false),
            // macOS can transiently refuse a group query while its leader
            // is being reaped. Retry observation, never the kill signal.
            Ok(false) | Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
}

#[cfg(unix)]
fn confirm_group(pgid: u32, report: Option<CleanupReport>) {
    let Some(report) = report else { return; };
    match group_is_gone(pgid) {
        Ok(true) => report.finish(true),
        Ok(false) | Err(_) => {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // Reaping is owned by ManagedChild/the caller. Poll existence
                // without repeating SIGKILL: a reused PGID must not be killed.
                handle.spawn(observe_group(pgid, report, std::time::Duration::from_secs(14)));
            }
            // No runtime: dropping the report records unconfirmed cleanup.
        }
    }
}

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
    report: Option<CleanupReport>,
}

impl GroupKillGuard {
    /// `pid` is the child's pid (= its process-group id after [`isolate`]).
    #[cfg(not(windows))]
    pub fn new(pid: Option<u32>) -> Self {
        // Establish ownership before calling the observer: even a panicking
        // observer factory must not leave an unmanaged process behind.
        let mut guard = Self { pgid: pid, report: None };
        guard.report = CleanupReport::capture();
        guard
    }

    /// Kill the whole group now (timeout / error paths that then reap the
    /// leader themselves) and disarm.
    pub fn kill_now(&mut self) {
        self.kill_group();
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            let report = self.report.take();
            match crate::platform::kill_process_group(pgid) {
                Ok(()) => confirm_group(pgid, report),
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                    if let Some(report) = report { report.finish(true); }
                }
                Err(error) => {
                    tracing::warn!(pgid, %error, "killpg of CLI process group failed");
                    // Dropping the report records a failed confirmation.
                }
            }
        }
        #[cfg(windows)]
        if let Some(mut job) = self.job.take() {
            if let Err(error) = job.terminate() {
                tracing::warn!(%error, "terminating CLI job failed; closed kill-on-close handle");
            }
            // WindowsJob closes its handle on termination and exposes no
            // active-process query. A signal/handle close is not proof of exit.
            drop(self.report.take());
        }
        #[cfg(not(any(unix, windows)))]
        { self.pgid = None; drop(self.report.take()); }
    }

    /// Release ownership after reaping the leader. Windows closes the job and
    /// terminates any remaining descendants even on a normal exit.
    pub fn disarm(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            // A reaped leader alone does not prove its descendants are gone.
            // Preserve disarm semantics, but never report success without proof.
            confirm_group(pgid, self.report.take());
        }
        #[cfg(windows)]
        { self.kill_group(); }
        #[cfg(not(any(unix, windows)))]
        { self.pgid = None; drop(self.report.take()); }
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
        let mut guard = GroupKillGuard { job: Some(job), report: None };
        guard.report = CleanupReport::capture();
        Ok((child, guard))
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


#[cfg(all(test, unix))]
mod downstream_cleanup_tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn observer() -> (CleanupObserver, oneshot::Receiver<bool>) {
        let (sender, receiver) = oneshot::channel();
        let sender = std::sync::Mutex::new(Some(sender));
        let observer: CleanupObserver = Arc::new(move || {
            let sender = sender.lock().unwrap().take().expect("one process guard");
            Box::new(move |gone| { let _ = sender.send(gone); })
        });
        (observer, receiver)
    }

    fn tree(dir: &std::path::Path) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", dir).current_dir(dir)
            .args(["-c", "/bin/sleep 30 & echo $! > grandchild; wait"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd
    }

    async fn grandchild(dir: &std::path::Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(dir.join("grandchild"))
                    && let Ok(pid) = text.trim().parse() {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("fixture starts its grandchild")
    }

    #[tokio::test]
    async fn downstream_drop_reports_only_after_real_process_group_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (observer, receiver) = observer();
        let (group, descendant) = with_cleanup_observer(observer, async {
            let mut child = ManagedChild::spawn(&mut tree(dir.path())).unwrap();
            let group = child.child().id().unwrap();
            let descendant = grandchild(dir.path()).await;
            assert!(!group_is_gone(group).unwrap());
            drop(child);
            (group, descendant)
        }).await;
        assert!(tokio::time::timeout(Duration::from_secs(15), receiver).await.unwrap().unwrap());
        assert!(group_is_gone(group).unwrap());
        assert!(!crate::platform::process_alive(descendant));
    }

    #[tokio::test]
    async fn downstream_panic_still_kills_and_confirms_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let (observer, receiver) = observer();
        let task = tokio::spawn(with_cleanup_observer(observer, async move {
            let _child = ManagedChild::spawn(&mut tree(&path)).unwrap();
            let _ = grandchild(&path).await;
            panic!("fixture panic while dispatch is running");
        }));
        assert!(task.await.unwrap_err().is_panic());
        assert!(tokio::time::timeout(Duration::from_secs(15), receiver).await.unwrap().unwrap());
        let descendant = grandchild(dir.path()).await;
        assert!(!crate::platform::process_alive(descendant));
    }

    #[tokio::test]
    async fn downstream_failed_kill_is_never_reported_as_stopped() {
        let (observer, receiver) = observer();
        with_cleanup_observer(observer, async {
            // Reserved ids are refused before any signal is sent.
            drop(GroupKillGuard::new(Some(1)));
        }).await;
        assert!(!receiver.await.unwrap());
    }

    #[tokio::test]
    async fn downstream_confirmation_timeout_leaves_a_live_group_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = ManagedChild::spawn(&mut tree(dir.path())).unwrap();
        let group = child.child().id().unwrap();
        let _ = grandchild(dir.path()).await;
        let (sender, receiver) = oneshot::channel();
        let report = CleanupReport(Some(Box::new(move |gone| { let _ = sender.send(gone); })));
        observe_group(group, report, Duration::from_millis(10)).await;
        assert!(!receiver.await.unwrap());
        assert!(!group_is_gone(group).unwrap(), "observation never sends a kill");
        child.kill_and_reap().await;
    }

    #[tokio::test]
    async fn downstream_aborted_confirmation_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = ManagedChild::spawn(&mut tree(dir.path())).unwrap();
        let group = child.child().id().unwrap();
        let _ = grandchild(dir.path()).await;
        let (sender, receiver) = oneshot::channel();
        let report = CleanupReport(Some(Box::new(move |gone| { let _ = sender.send(gone); })));
        let task = tokio::spawn(observe_group(group, report, Duration::from_secs(14)));
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!receiver.await.unwrap());
        child.kill_and_reap().await;
    }

    #[tokio::test]
    async fn downstream_normal_exit_confirms_without_claiming_signal_delivery_is_exit() {
        let (observer, receiver) = observer();
        with_cleanup_observer(observer, async {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", "exit 0"]);
            let mut child = ManagedChild::spawn(&mut command).unwrap();
            assert!(child.child().wait().await.unwrap().success());
            child.disarm();
        }).await;
        assert!(tokio::time::timeout(Duration::from_secs(15), receiver).await.unwrap().unwrap());
    }
}
