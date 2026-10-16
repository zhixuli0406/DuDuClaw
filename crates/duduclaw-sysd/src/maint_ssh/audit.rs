//! The root audit file of maintenance SSH (design §6.2).
//!
//! # Interface note (for the OS close script `maint-ssh-close` and factory reset)
//!
//! - **File**: `/data/system/maint-ssh-audit.log`, regular file, root:root
//!   0600, opened `O_WRONLY|O_APPEND|O_NOFOLLOW`. One JSON object per line
//!   (`\n`-terminated; JSON escaping keeps every record on one line).
//! - **Lock**: `/data/system/maint-ssh-audit.log.lock`, regular file,
//!   root:root 0600. Every writer takes an exclusive `flock(2)` on it for the
//!   whole append (and sysd also for a rotation). sysd creates it with
//!   `O_CREAT|O_NOFOLLOW` mode 0600 and refuses a lock file or audit file that
//!   is not a regular file owned by root with no group/other bits, so the
//!   script must create it under `umask 077`. busybox `flock` has no `-w`,
//!   so wait with a bounded retry loop:
//!   ```sh
//!   umask 077
//!   exec 9>>/data/system/maint-ssh-audit.log.lock
//!   i=0
//!   until flock -n 9; do
//!       i=$((i + 1)); [ "$i" -ge 10 ] && exit 1   # give up after ~10 s
//!       sleep 1
//!   done
//!   printf '%s\n' "$record" >>/data/system/maint-ssh-audit.log
//!   ```
//!   sysd never blocks on the lock: it retries without blocking for at most
//!   [`LOCK_TIMEOUT`] and otherwise treats the write as failed (an open is
//!   undone; a close records the problem and keeps the record queued).
//! - **Rotation**: only sysd rotates. When the file has reached
//!   [`AUDIT_MAX_BYTES`] (1 MiB) before a write, `<file>.4` → `.5`, …,
//!   `<file>` → `.1` ([`AUDIT_GENERATIONS`] = 5 kept, the oldest dropped), and
//!   the open record of the window in progress is written again at the top of
//!   the new file with `"rewritten_after_rotation": true`. The script only
//!   appends. Factory reset should remove `maint-ssh-audit.log`,
//!   `maint-ssh-audit.log.1` … `.5`, `maint-ssh-audit.log.lock` and the
//!   window state directory `/data/system/maint-ssh/` (see `super::STATE_DIR`).
//! - **Records** (`ts` is always an integer: Unix seconds, UTC):
//!   - `{"event":"open","ts","window_id","key_type","key_fingerprint","ttl_seconds","expires_at","host_key_fingerprints":[…]}`
//!     (`expires_at` Unix seconds; fingerprints `SHA256:<unpadded base64>`).
//!   - `{"event":"login","ts","window_id","source","pid","cursor","fingerprint"}`
//!     and `{"event":"logout","ts","window_id","source","pid","cursor"}`;
//!     `ts` is the journal entry's time, `source` the peer IP or `null`,
//!     `cursor` the journal cursor of the entry.
//!   - `{"event":"open_failed","ts","window_id","key_fingerprint","error","stage"}`
//!     (best effort, when an open is undone after the key was written).
//!   - `{"event":"close","ts","trigger","window_id","removed_fingerprint","done":[…],"problems":[…],"logins_recorded","logins_over_cap","logouts_recorded","logouts_over_cap","auth_failures","auth_failure_sources":[…],"sessions_open_at_close","last_cursor","events_complete"}`;
//!     `trigger` is `stop`, `deadline` or `startup_cleanup` from sysd. The
//!     script writes `{"event":"close","ts","trigger":"fallback_timer", …}`
//!     with whatever of these fields it knows (`window_id` may be `null`);
//!     It should also replace `/data/system/maint-ssh/last-close.json`
//!     (root 0600, written to a temp file under `umask 077` then renamed) with
//!     `{"window_id":null,"trigger":"fallback_timer","events_complete":false,"events_lost":false,"ts":<unix seconds>}`;
//!     sysd keeps that reason when it later cleans the window up.
//!     sysd's next startup cleanup replays the journal from
//!     `/data/system/maint-ssh/window.json`. After a reboot that cleanup
//!     writes the close record with `"trigger": "reboot"`.
//!   - `{"event":"audit_backlog_lost","ts","error"}` when the saved backlog
//!     of owed records could not be read at startup.
//!     A close record whose events could not all be read yet has
//!     `"events_complete": false` and is held back until they are, then
//!     written with `"events_complete": true` after them.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Rotation threshold.
pub const AUDIT_MAX_BYTES: u64 = 1024 * 1024;
/// Rotated files kept (`<file>.1` … `<file>.5`), so a caller that opens and
/// closes windows in a loop needs about 5 MiB of records to push old
/// evidence out (and `ssh_maint_open` is rate-limited on top).
pub const AUDIT_GENERATIONS: u32 = 5;
/// Longest sysd waits for the lock before the write counts as failed.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(3);
const LOCK_RETRY: Duration = Duration::from_millis(25);

#[derive(Debug, Clone)]
pub struct AuditLog {
    pub path: PathBuf,
    /// Required owner of the file and its lock (root in production).
    pub owner_uid: u32,
    pub max_bytes: u64,
    pub lock_timeout: Duration,
}

impl AuditLog {
    pub fn new(path: impl Into<PathBuf>, owner_uid: u32) -> Self {
        Self { path: path.into(), owner_uid, max_bytes: AUDIT_MAX_BYTES, lock_timeout: LOCK_TIMEOUT }
    }

    pub fn lock_path(&self) -> PathBuf {
        suffixed(&self.path, ".lock")
    }

    fn generation(&self, n: u32) -> PathBuf {
        suffixed(&self.path, &format!(".{n}"))
    }

    /// `<file>.4` → `.5` (the oldest is dropped), …, `<file>` → `.1`.
    fn rotate(&self) -> std::io::Result<()> {
        for n in (1..AUDIT_GENERATIONS).rev() {
            match std::fs::rename(self.generation(n), self.generation(n + 1)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        std::fs::rename(&self.path, self.generation(1))
    }

    /// Append `records` on a blocking thread, giving up after the lock
    /// timeout (plus a margin for the write itself). After a rotation
    /// `reopen` (the open record of the window in progress) is written again
    /// before anything else.
    pub async fn append(&self, records: Vec<Value>, reopen: Option<Value>) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        let this = self.clone();
        let budget = self.lock_timeout + Duration::from_secs(5);
        // Set when the caller has given up, so a writer that only gets the
        // lock afterwards writes nothing the caller believes failed.
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cancelled.clone();
        let task = tokio::task::spawn_blocking(move || this.append_blocking(&records, reopen.as_ref(), &flag));
        match tokio::time::timeout(budget, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(format!("audit writer failed: {e}")),
            Err(_) => {
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(format!("writing {} did not finish in time", self.path.display()))
            }
        }
    }

    fn append_blocking(&self, records: &[Value], reopen: Option<&Value>, cancelled: &std::sync::atomic::AtomicBool) -> Result<(), String> {
        let _lock = self.acquire_lock()?;
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the caller gave up before the audit lock was taken".to_string());
        }
        let result = (|| -> std::io::Result<()> {
            let mut file = open_append(&self.path, self.owner_uid)?;
            for record in records {
                if file.metadata()?.len() >= self.max_bytes {
                    self.rotate()?;
                    file = open_append(&self.path, self.owner_uid)?;
                    if let Some(open) = reopen {
                        let mut again = open.clone();
                        if let Some(obj) = again.as_object_mut() {
                            obj.insert("rewritten_after_rotation".to_string(), Value::Bool(true));
                        }
                        write_line(&mut file, &again)?;
                    }
                }
                write_line(&mut file, record)?;
            }
            file.sync_data()
        })();
        result.map_err(|e| format!("writing {}: {e}", self.path.display()))
    }

    /// Open (creating 0600, never following a symlink) and verify the lock
    /// file, then take an exclusive `flock` without blocking, retrying until
    /// the lock timeout.
    fn acquire_lock(&self) -> Result<std::fs::File, String> {
        let path = self.lock_path();
        let file = open_verified(&path, libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC, self.owner_uid)
            .map_err(|e| format!("audit lock {}: {e}", path.display()))?;
        let deadline = Instant::now() + self.lock_timeout;
        loop {
            // SAFETY: valid open descriptor owned by `file`.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(file);
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) && err.raw_os_error() != Some(libc::EINTR) {
                return Err(format!("audit lock {}: {err}", path.display()));
            }
            if Instant::now() >= deadline {
                return Err(format!("audit lock {} is held by another process", path.display()));
            }
            std::thread::sleep(LOCK_RETRY);
        }
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn write_line(file: &mut std::fs::File, record: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes())
}

fn open_append(path: &Path, owner_uid: u32) -> std::io::Result<std::fs::File> {
    open_verified(path, libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC, owner_uid)
}

/// Open with `flags` (a new file starts at 0600) and require a regular file
/// owned by `owner_uid` with no group/other permission bits.
fn open_verified(path: &Path, flags: libc::c_int, owner_uid: u32) -> std::io::Result<std::fs::File> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: valid NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), flags, 0o600 as libc::c_uint) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor owned here.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != owner_uid || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "{} must be a regular file owned by uid {owner_uid} with no group/other access",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn me() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getuid() }
    }

    fn lines(p: &Path) -> Vec<Value> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Hold the audit lock from a separate open file description (flock
    /// conflicts across descriptions even within one process).
    pub(crate) fn hold_lock(log: &AuditLog) -> std::fs::File {
        let f = open_verified(&log.lock_path(), libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC, log.owner_uid).unwrap();
        // SAFETY: valid descriptor.
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        f
    }

    #[tokio::test]
    async fn appends_one_json_line_per_record_with_0600_files() {
        let t = tempfile::tempdir().unwrap();
        let log = AuditLog::new(t.path().join("audit.log"), me());
        log.append(vec![serde_json::json!({"event":"open","m":"a\nb"})], None).await.unwrap();
        log.append(vec![serde_json::json!({"event":"close"})], None).await.unwrap();
        let got = lines(&log.path);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["m"], "a\nb");
        assert_eq!(std::fs::metadata(&log.path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(log.lock_path()).unwrap().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn rotation_keeps_generations_and_rewrites_the_open_record() {
        let t = tempfile::tempdir().unwrap();
        let mut log = AuditLog::new(t.path().join("audit.log"), me());
        log.max_bytes = 400;
        let open = serde_json::json!({"event":"open","window_id":"w1"});
        log.append(vec![open.clone()], Some(open.clone())).await.unwrap();
        let logins: Vec<Value> = (0..40).map(|i| serde_json::json!({"event":"login","n":i})).collect();
        log.append(logins, Some(open.clone())).await.unwrap();
        let current = lines(&log.path);
        assert_eq!(current[0]["event"], "open");
        assert_eq!(current[0]["window_id"], "w1");
        assert_eq!(current[0]["rewritten_after_rotation"], true);
        assert_eq!(current.last().unwrap()["n"], 39);
        assert!(log.generation(1).exists() && log.generation(2).exists());
        let many: Vec<Value> = (0..400).map(|i| serde_json::json!({"event":"login","n":i})).collect();
        log.append(many, Some(open)).await.unwrap();
        assert!(log.generation(AUDIT_GENERATIONS).exists());
        assert!(!log.generation(AUDIT_GENERATIONS + 1).exists());
    }

    #[tokio::test]
    async fn a_held_lock_fails_the_write_in_bounded_time() {
        let t = tempfile::tempdir().unwrap();
        let mut log = AuditLog::new(t.path().join("audit.log"), me());
        log.lock_timeout = Duration::from_millis(200);
        let held = hold_lock(&log);
        let started = Instant::now();
        let err = log.append(vec![serde_json::json!({"event":"x"})], None).await.unwrap_err();
        assert!(err.contains("held by another process"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(lines(&log.path).is_empty());
        drop(held);
        log.append(vec![serde_json::json!({"event":"x"})], None).await.unwrap();
    }

    #[tokio::test]
    async fn untrusted_files_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let log = AuditLog::new(t.path().join("audit.log"), me());
        std::fs::write(&log.path, b"").unwrap();
        std::fs::set_permissions(&log.path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        assert!(log.append(vec![serde_json::json!({})], None).await.is_err());
        std::fs::remove_file(&log.path).unwrap();
        let target = t.path().join("elsewhere");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, &log.path).unwrap();
        assert!(log.append(vec![serde_json::json!({})], None).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"");
        std::fs::remove_file(&log.path).unwrap();
        // A world-readable lock file (what a umask-022 creator would leave) and
        // a symlinked lock file are refused too.
        std::fs::write(log.lock_path(), b"").unwrap();
        std::fs::set_permissions(log.lock_path(), std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        assert!(log.append(vec![serde_json::json!({})], None).await.unwrap_err().contains("audit lock"));
        std::fs::remove_file(log.lock_path()).unwrap();
        std::os::unix::fs::symlink(&target, log.lock_path()).unwrap();
        assert!(log.append(vec![serde_json::json!({})], None).await.is_err());
        let wrong_owner = AuditLog::new(t.path().join("other.log"), me().wrapping_add(1));
        assert!(wrong_owner.append(vec![serde_json::json!({})], None).await.is_err());
    }
}
