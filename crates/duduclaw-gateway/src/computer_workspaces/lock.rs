//! One writer per workspace, across processes (review H1).
//!
//! Every operation that changes a workspace's files or its unfinished
//! registry state (a write, a create, the cleanup of a `creating` /
//! `failed_create` / `deleting` row, the settling of a write intent) holds
//! an exclusive `flock` on `<home>/computer_workspaces/.locks/<id>.lock`.
//! Writers and creators wait for it (bounded); reconciliation only *tries*
//! it and skips a busy workspace, so whatever reconciliation finds while it
//! holds the lock has no live holder — the process that started it is gone.
//!
//! `flock` locks belong to the open file description, so two handles in the
//! same process exclude each other just like two processes do, and the lock
//! is dropped by the kernel when its holder dies.

use std::io;
#[cfg_attr(not(unix), allow(unused_imports))]
use std::path::{Path, PathBuf};
#[cfg_attr(not(unix), allow(unused_imports))]
use std::time::{Duration, Instant};

use super::paths::{ROOT_DIR, canonical_home, valid_workspace_id};
use super::state::WorkspaceState;
use super::store::{StoreError, WorkspaceStore};

/// Directory (under the workspace root) holding the lock files.
pub const LOCK_DIR: &str = ".locks";
/// How long a writer or creator waits for a busy workspace.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

/// A held lock; released on drop.
#[derive(Debug)]
pub struct WorkspaceLock {
    _file: std::fs::File,
}

#[cfg_attr(not(unix), allow(dead_code))]
fn lock_path(home: &Path, id: &str) -> io::Result<PathBuf> {
    if !valid_workspace_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid workspace id",
        ));
    }
    let dir = canonical_home(home)?.join(ROOT_DIR).join(LOCK_DIR);
    crate::discovery::workspace::create_private_directory(&dir)?;
    Ok(dir.join(format!("{id}.lock")))
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// `Ok(true)` when the exclusive lock was taken without waiting.
#[cfg(unix)]
fn try_flock(file: &std::fs::File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: the fd is open for the lifetime of `file`.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// Take the lock of workspace `id`, waiting at most `wait` (`ZERO` = try
/// once). `Ok(None)` when it is still busy after `wait`.
#[cfg(unix)]
pub fn lock_workspace(home: &Path, id: &str, wait: Duration) -> io::Result<Option<WorkspaceLock>> {
    let file = open_lock_file(&lock_path(home, id)?)?;
    let deadline = Instant::now() + wait;
    loop {
        if try_flock(&file)? {
            return Ok(Some(WorkspaceLock { _file: file }));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Lock file name for registry initialisation (not a workspace id, so it can
/// never collide with `<id>.lock`).
pub const REGISTRY_INIT_LOCK: &str = "registry-init.lock";

/// Serialise [`WorkspaceStore::open`] across threads and processes.
///
/// Opening a *fresh* registry switches it to WAL and creates the schema;
/// SQLite answers a second connection that arrives during that window with
/// `SQLITE_BUSY` ("database is locked") instead of invoking the busy handler
/// (the journal-mode change needs the file to itself), so two concurrent
/// first opens — two gateway threads, or the gateway and the operator CLI —
/// used to fail one of them (`review2::concurrent_creates_never_exceed_max_per_agent`
/// reproduced it 4 times in 5 when run alone). The exclusive flock on
/// `<root>/.locks/registry-init.lock` is held only for the open sequence and
/// waits at most [`LOCK_WAIT`]; a still-busy lock is reported as unavailable,
/// never as a silent fall-through.
#[cfg(unix)]
pub fn lock_registry_init(home: &Path) -> io::Result<WorkspaceLock> {
    let dir = canonical_home(home)?.join(ROOT_DIR).join(LOCK_DIR);
    crate::discovery::workspace::create_private_directory(&dir)?;
    let file = open_lock_file(&dir.join(REGISTRY_INIT_LOCK))?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        if try_flock(&file)? {
            return Ok(WorkspaceLock { _file: file });
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "workspace registry is being initialised by another process",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Not supported off unix (the whole feature refuses there).
#[cfg(not(unix))]
pub fn lock_workspace(
    _home: &Path,
    _id: &str,
    _wait: Duration,
) -> io::Result<Option<WorkspaceLock>> {
    Err(io::Error::other(
        "computer workspaces are not supported on this platform",
    ))
}

/// [`lock_workspace`] with [`LOCK_WAIT`]; busy or broken maps to a store error.
pub fn lock_for_change(home: &Path, id: &str) -> Result<WorkspaceLock, StoreError> {
    match lock_workspace(home, id, LOCK_WAIT) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(StoreError::Busy),
        Err(e) => Err(StoreError::Unavailable(format!("workspace lock: {e}"))),
    }
}

/// Reconciliation's view: the lock when nobody holds it, `None` otherwise
/// (busy, or the lock cannot be taken at all — then nothing is touched).
pub fn try_lock_idle(home: &Path, id: &str) -> Option<WorkspaceLock> {
    lock_workspace(home, id, Duration::ZERO).ok().flatten()
}

/// One step of [`create_workspace`], for tests that pause there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateStep {
    /// The `creating` row exists, the directories do not yet.
    AfterInsert,
    /// The directories exist, the row is still `creating`.
    AfterMkdir,
}

/// Create a workspace for `owner` under its lock: insert the `creating`
/// row with a fresh owner credential, make `<root>/<id>/data`, record the
/// credential in the owner's state file, move it to `ready`. A reconciliation that
/// runs meanwhile sees the lock held and leaves the row alone.
pub fn create_workspace(
    home: &Path,
    store: &WorkspaceStore,
    owner: &str,
    runner_id: &str,
    now: i64,
    max_per_agent: u32,
    step: &dyn Fn(CreateStep),
) -> Result<String, StoreError> {
    let id = super::paths::new_workspace_id();
    let _lock = lock_for_change(home, &id)?;
    let credential = super::owner_cred::new_credential();
    store.create_with_id(&id, owner, runner_id, now, max_per_agent, Some(&credential))?;
    step(CreateStep::AfterInsert);
    let actor = format!("agent:{owner}");
    let made = super::paths::create_workspace_dirs(home, &id).is_ok()
        && super::owner_cred::record(home, owner, &id, &credential).is_ok();
    if !made {
        let _ = store.transition(
            &id,
            &[WorkspaceState::Creating],
            WorkspaceState::FailedCreate,
            &actor,
            "failed_create",
            Some("mkdir"),
        );
        let _ = super::paths::remove_workspace_dir(home, &id);
        return Err(StoreError::Unavailable("workspace directory".into()));
    }
    step(CreateStep::AfterMkdir);
    store.transition(
        &id,
        &[WorkspaceState::Creating],
        WorkspaceState::Ready,
        &actor,
        "created",
        None,
    )?;
    Ok(id)
}
