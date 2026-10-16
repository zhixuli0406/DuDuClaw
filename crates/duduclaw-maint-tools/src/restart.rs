//! The helper's two restarts: `restart-gateway` and `restart-model`.
//!
//! Owner decision 2026-10-16: no polkit (adding it to the base edition can
//! renumber existing system accounts), so the root helper runs
//! `systemctl --no-pager --no-ask-password restart <fixed unit>` itself,
//! after the same `SO_PEERCRED` uid 4800 check as the log requests.
//!
//! A restart storm is stopped by a per-unit minimum interval
//! ([`RESTART_MIN_INTERVAL_SECS`]). Every helper connection is a separate
//! process (`Accept=yes`), so the last restart time lives in a stamp file per
//! unit under the preserved runtime directory, read and rewritten under an
//! exclusive `flock` so two simultaneous requests cannot both pass. The time
//! is `CLOCK_BOOTTIME` seconds: it does not move when the wall clock is
//! changed, and `/run` is emptied on reboot together with the clock's origin.
//! If the stamp cannot be read or written the restart is refused (the limit
//! cannot be enforced, so fail closed). An attempt counts even if systemctl
//! then fails, so a failing restart cannot be retried in a loop either.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{REPLY_OK_PREFIX, RESTART_MIN_INTERVAL_SECS, RestartTarget};
use crate::tools::{CHILD_ENV, Tool, resolve_tool};

/// How long the helper waits for `systemctl restart` before answering that
/// the restart is still running (the job itself continues in PID 1).
pub const RESTART_WAIT: Duration = Duration::from_secs(120);

/// Outcome of one restart request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartResult {
    Done,
    /// `systemctl` exited non-zero (`None`: killed by a signal).
    Failed(Option<i32>),
    /// Still running after [`RESTART_WAIT`].
    StillRunning,
    /// The same unit was restarted less than the minimum interval ago.
    RateLimited {
        retry_after_secs: u64,
    },
    /// The restart could not be attempted (stamp unusable, systemctl missing).
    Unavailable(String),
}

impl RestartResult {
    /// The single status line sent to the client (no trailing newline).
    /// Fixed text only; the internal reason of `Unavailable` goes to the
    /// journal, not to the client.
    pub fn reply(&self, target: RestartTarget) -> String {
        match self {
            RestartResult::Done => format!("{REPLY_OK_PREFIX} restarted {}", target.unit()),
            RestartResult::Failed(Some(code)) => format!("error: restart failed (exit {code})"),
            RestartResult::Failed(None) => "error: restart failed (signal)".to_string(),
            RestartResult::StillRunning => format!(
                "error: restart still running after {} s",
                RESTART_WAIT.as_secs()
            ),
            RestartResult::RateLimited { retry_after_secs } => {
                format!("error: too soon, try again in {retry_after_secs} s")
            }
            RestartResult::Unavailable(_) => "error: restart unavailable".to_string(),
        }
    }
}

/// Per-unit minimum interval, kept in stamp files under `dir`.
pub struct RestartLimiter {
    pub dir: PathBuf,
    pub min_interval_secs: u64,
    /// Seconds on a clock that does not follow wall-clock changes.
    pub now: fn() -> io::Result<u64>,
    /// Required owner of the state directory and stamp files (root in
    /// production).
    pub owner_uid: u32,
    /// Where anomalies worth an operator's attention are reported (the
    /// journal in production).
    pub on_note: fn(&str),
}

/// Holds the stamp file's exclusive lock until dropped (i.e. until the
/// restart finished), so a concurrent request for the same unit waits and
/// then sees the fresh stamp.
pub struct RestartSlot {
    _file: File,
}

impl RestartLimiter {
    /// Production limiter: [`crate::protocol::HELPER_STATE_DIR`], 60 s,
    /// `CLOCK_BOOTTIME`.
    pub fn production() -> RestartLimiter {
        RestartLimiter {
            dir: PathBuf::from(crate::protocol::HELPER_STATE_DIR),
            min_interval_secs: RESTART_MIN_INTERVAL_SECS,
            now: boottime_secs,
            owner_uid: 0,
            on_note: note_to_journal,
        }
    }

    /// Take the slot for `target` or say why not. On success the stamp has
    /// already been updated to now.
    pub fn acquire(&self, target: RestartTarget) -> Result<RestartSlot, RestartResult> {
        let unavailable =
            |what: &str, e: io::Error| RestartResult::Unavailable(format!("{what}: {e}"));
        // The directory must be what systemd's RuntimeDirectory= made: a real
        // directory (not a symlink), owned by root, mode 0700. Otherwise the
        // stamps cannot be trusted, so refuse.
        let dir_meta = std::fs::symlink_metadata(&self.dir)
            .map_err(|e| unavailable("stat state directory", e))?;
        if !dir_meta.file_type().is_dir()
            || dir_meta.uid() != self.owner_uid
            || dir_meta.mode() & 0o7777 != 0o700
        {
            return Err(RestartResult::Unavailable(format!(
                "state directory must be a {}-owned 0700 directory (uid {}, mode {:o})",
                self.owner_uid,
                dir_meta.uid(),
                dir_meta.mode() & 0o7777
            )));
        }
        let path = self.dir.join(format!("restart-{}.stamp", target.key()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| unavailable("open restart stamp", e))?;
        let meta = file
            .metadata()
            .map_err(|e| unavailable("stat restart stamp", e))?;
        // A regular file with one link, owned by root, mode 0600: anything
        // else (a hard link to some other file, a file someone else made)
        // is not trusted.
        if !meta.is_file()
            || meta.uid() != self.owner_uid
            || meta.mode() & 0o7777 != 0o600
            || meta.nlink() != 1
        {
            return Err(RestartResult::Unavailable(format!(
                "restart stamp must be a single-link {}-owned 0600 regular file \
                 (uid {}, mode {:o}, links {})",
                self.owner_uid,
                meta.uid(),
                meta.mode() & 0o7777,
                meta.nlink()
            )));
        }
        lock_exclusive(&file).map_err(|e| unavailable("lock restart stamp", e))?;

        let mut text = String::new();
        (&mut file)
            .take(64)
            .read_to_string(&mut text)
            .map_err(|e| unavailable("read restart stamp", e))?;
        let now = (self.now)().map_err(|e| unavailable("read clock", e))?;
        let last =
            match text.trim() {
                "" => None,
                t => Some(t.parse::<u64>().map_err(|_| {
                    RestartResult::Unavailable("restart stamp is not a number".into())
                })?),
            };
        if let Some(last) = last {
            if last > now {
                // Should not happen (/run and the boot clock reset together).
                // Clamp it once: rewrite the stamp to now and treat this
                // request as coming right after a restart, so a bogus value
                // cannot block restarts until the next reboot.
                (self.on_note)(&format!(
                    "maint-helper: restart stamp for {} was in the future ({last} > {now}); clamped to now",
                    target.key()
                ));
                write_stamp(&mut file, now).map_err(|e| unavailable("write restart stamp", e))?;
                return Err(RestartResult::RateLimited {
                    retry_after_secs: self.min_interval_secs,
                });
            }
            let elapsed = now - last;
            if elapsed < self.min_interval_secs {
                return Err(RestartResult::RateLimited {
                    retry_after_secs: self.min_interval_secs - elapsed,
                });
            }
        }
        write_stamp(&mut file, now).map_err(|e| unavailable("write restart stamp", e))?;
        Ok(RestartSlot { _file: file })
    }
}

fn write_stamp(file: &mut File, now: u64) -> io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(now.to_string().as_bytes())?;
    file.sync_all()
}

/// Production `on_note`: a warning in the journal.
fn note_to_journal(message: &str) {
    let _ = crate::journal::send(&crate::journal::entry_payload(
        crate::journal::HELPER_IDENTIFIER,
        crate::journal::PRIORITY_WARNING,
        message,
        &[],
    ));
}

fn lock_exclusive(file: &File) -> io::Result<()> {
    loop {
        // SAFETY: flock on a valid, owned descriptor; no memory is passed.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Seconds since boot, including suspend (`CLOCK_BOOTTIME`) on Linux;
/// `CLOCK_MONOTONIC` on development hosts that lack it.
pub fn boottime_secs() -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::clock_gettime(clock, &mut ts) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    u64::try_from(ts.tv_sec).map_err(|_| io::Error::other("negative clock"))
}

/// Run `systemctl --no-pager --no-ask-password restart <unit>` with the fixed
/// argv, cleared environment and all three standard streams on `/dev/null`
/// (its messages would otherwise reach the client unfiltered).
pub fn run_systemctl_restart(target: RestartTarget, wait: Duration) -> RestartResult {
    let Some(systemctl) = resolve_tool(Tool::Systemctl, |p| p.is_file()) else {
        return RestartResult::Unavailable("systemctl not found".into());
    };
    let mut child = match Command::new(systemctl)
        .args(target.systemctl_args())
        .env_clear()
        .envs(CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return RestartResult::Unavailable(format!("spawn systemctl: {e}")),
    };
    let deadline = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return RestartResult::Done,
            Ok(Some(status)) => return RestartResult::Failed(status.code()),
            Ok(None) if Instant::now() >= deadline => return RestartResult::StillRunning,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => return RestartResult::Unavailable(format!("wait systemctl: {e}")),
        }
    }
}

/// Production restart: limiter first, then systemctl while holding the slot.
pub fn restart_with_limit(limiter: &RestartLimiter, target: RestartTarget) -> RestartResult {
    match limiter.acquire(target) {
        Ok(_slot) => run_systemctl_restart(target, RESTART_WAIT),
        Err(refused) => refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FAKE_NOW: AtomicU64 = AtomicU64::new(1_000);

    fn fake_now() -> io::Result<u64> {
        Ok(FAKE_NOW.load(Ordering::SeqCst))
    }

    static NOTES: AtomicU64 = AtomicU64::new(0);

    fn count_note(_: &str) {
        NOTES.fetch_add(1, Ordering::SeqCst);
    }

    fn my_uid() -> u32 {
        // SAFETY: getuid has no preconditions.
        #[allow(unsafe_code)]
        unsafe {
            libc::getuid()
        }
    }

    fn limiter(dir: &std::path::Path) -> RestartLimiter {
        RestartLimiter {
            dir: dir.to_path_buf(),
            min_interval_secs: 60,
            now: fake_now,
            owner_uid: my_uid(),
            on_note: count_note,
        }
    }

    /// A state directory shaped like systemd's RuntimeDirectory= (0700).
    fn state_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        mode(d.path(), 0o700);
        d
    }

    fn mode(path: &std::path::Path, m: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(m)).unwrap();
    }

    // One test drives the shared fake clock, so the steps stay in order.
    #[test]
    fn per_unit_minimum_interval() {
        let d = state_dir();
        let l = limiter(d.path());
        FAKE_NOW.store(1_000, Ordering::SeqCst);

        // First restart of each unit passes.
        drop(l.acquire(RestartTarget::Model).unwrap());
        drop(l.acquire(RestartTarget::Gateway).unwrap());

        // Same unit 10 s later: refused with the remaining wait.
        FAKE_NOW.store(1_010, Ordering::SeqCst);
        assert_eq!(
            l.acquire(RestartTarget::Model).err(),
            Some(RestartResult::RateLimited {
                retry_after_secs: 50
            })
        );
        // A refused attempt does not move the stamp.
        FAKE_NOW.store(1_059, Ordering::SeqCst);
        assert_eq!(
            l.acquire(RestartTarget::Model).err(),
            Some(RestartResult::RateLimited {
                retry_after_secs: 1
            })
        );
        // After the interval it passes again, and the new stamp counts.
        FAKE_NOW.store(1_060, Ordering::SeqCst);
        drop(l.acquire(RestartTarget::Model).unwrap());
        FAKE_NOW.store(1_061, Ordering::SeqCst);
        assert!(matches!(
            l.acquire(RestartTarget::Model),
            Err(RestartResult::RateLimited {
                retry_after_secs: 59
            })
        ));
        // A stamp from the future is clamped once: refused now, rewritten to
        // now, noted, and the next attempt after the interval passes.
        let stamp = d.path().join("restart-gateway.stamp");
        std::fs::write(&stamp, "999999").unwrap();
        let notes = NOTES.load(Ordering::SeqCst);
        assert!(matches!(
            l.acquire(RestartTarget::Gateway),
            Err(RestartResult::RateLimited {
                retry_after_secs: 60
            })
        ));
        assert_eq!(std::fs::read_to_string(&stamp).unwrap(), "1061");
        assert_eq!(NOTES.load(Ordering::SeqCst), notes + 1);
        FAKE_NOW.store(1_121, Ordering::SeqCst);
        drop(l.acquire(RestartTarget::Gateway).unwrap());
    }

    #[test]
    fn unusable_state_fails_closed() {
        let d = state_dir();
        // Missing directory.
        let l = limiter(&d.path().join("missing"));
        assert!(matches!(
            l.acquire(RestartTarget::Model),
            Err(RestartResult::Unavailable(_))
        ));
        // Garbage in the stamp.
        let l = limiter(d.path());
        std::fs::write(d.path().join("restart-model.stamp"), "not-a-number").unwrap();
        mode(&d.path().join("restart-model.stamp"), 0o600);
        assert!(matches!(
            l.acquire(RestartTarget::Model),
            Err(RestartResult::Unavailable(_))
        ));
        // A symlink planted as the stamp is not followed.
        let target = d.path().join("elsewhere");
        std::fs::write(&target, "").unwrap();
        std::os::unix::fs::symlink(&target, d.path().join("restart-gateway.stamp")).unwrap();
        assert!(matches!(
            l.acquire(RestartTarget::Gateway),
            Err(RestartResult::Unavailable(_))
        ));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
    }

    #[test]
    fn untrusted_directory_or_stamp_fails_closed() {
        // Directory too open.
        let d = state_dir();
        mode(d.path(), 0o755);
        assert!(matches!(
            limiter(d.path()).acquire(RestartTarget::Model),
            Err(RestartResult::Unavailable(_))
        ));
        mode(d.path(), 0o700);
        // Directory owned by someone else.
        let mut l = limiter(d.path());
        l.owner_uid = my_uid().wrapping_add(1);
        assert!(matches!(
            l.acquire(RestartTarget::Model),
            Err(RestartResult::Unavailable(_))
        ));
        // Directory reached through a symlink.
        let link = d.path().join("dirlink");
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        mode(&real, 0o700);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            limiter(&link).acquire(RestartTarget::Model),
            Err(RestartResult::Unavailable(_))
        ));
        assert!(limiter(&real).acquire(RestartTarget::Model).is_ok());
        // Stamp with the wrong mode.
        let l = limiter(d.path());
        let stamp = d.path().join("restart-model.stamp");
        std::fs::write(&stamp, "").unwrap();
        mode(&stamp, 0o644);
        assert!(matches!(
            l.acquire(RestartTarget::Model),
            Err(RestartResult::Unavailable(_))
        ));
        mode(&stamp, 0o600);
        assert!(l.acquire(RestartTarget::Model).is_ok());
        // Stamp with a second hard link.
        let gw = d.path().join("restart-gateway.stamp");
        std::fs::write(&gw, "").unwrap();
        mode(&gw, 0o600);
        std::fs::hard_link(&gw, d.path().join("other-name")).unwrap();
        assert!(matches!(
            l.acquire(RestartTarget::Gateway),
            Err(RestartResult::Unavailable(_))
        ));
    }

    #[test]
    fn replies_are_fixed_lines() {
        let t = RestartTarget::Gateway;
        assert_eq!(
            RestartResult::Done.reply(t),
            "ok: restarted duduclaw-gateway.service"
        );
        assert_eq!(
            RestartResult::RateLimited {
                retry_after_secs: 42
            }
            .reply(t),
            "error: too soon, try again in 42 s"
        );
        assert_eq!(
            RestartResult::Failed(Some(1)).reply(t),
            "error: restart failed (exit 1)"
        );
        assert_eq!(
            RestartResult::Unavailable("secret internal path".into()).reply(t),
            "error: restart unavailable"
        );
        for r in [
            RestartResult::Done,
            RestartResult::Failed(None),
            RestartResult::StillRunning,
            RestartResult::RateLimited {
                retry_after_secs: 1,
            },
            RestartResult::Unavailable(String::new()),
        ] {
            let line = r.reply(t);
            assert!(!line.contains('\n') && line.len() < 128);
            assert_eq!(line.starts_with(REPLY_OK_PREFIX), r == RestartResult::Done);
        }
    }

    #[test]
    fn boottime_clock_reads() {
        assert!(boottime_secs().is_ok());
    }
}
