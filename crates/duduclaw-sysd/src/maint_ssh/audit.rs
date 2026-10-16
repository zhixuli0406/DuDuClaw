//! The root audit file `/data/system/maint-ssh-audit.log` (design §6.2): one
//! JSON object per line, root 0600, opened `O_APPEND|O_NOFOLLOW`, written only
//! by sysd and by the OS's PID 1 fallback script (`maint-ssh-close`). Both
//! hold an advisory lock on `<file>.lock` (`duduclaw_core::with_file_lock`;
//! the script uses `flock` on the same path), so lines never interleave.
//!
//! Rotation (R2-L8): once the file reaches [`AUDIT_MAX_BYTES`] it is renamed
//! to `<file>.1` (older ones shift up to `<file>.5`) and the current window's open
//! record is written again at the top of the new file, so repeated logins
//! cannot push the record of who opened the window out of both files.

use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Rotation threshold.
pub const AUDIT_MAX_BYTES: u64 = 1024 * 1024;
/// Rotated files kept (`<file>.1` … `<file>.5`), so a caller that opens and
/// closes windows in a loop needs about 5 MiB of records to push old
/// evidence out.
pub const AUDIT_GENERATIONS: u32 = 5;

#[derive(Debug, Clone)]
pub struct AuditLog {
    pub path: PathBuf,
    /// Required owner of the file (root in production).
    pub owner_uid: u32,
    pub max_bytes: u64,
}

impl AuditLog {
    pub fn new(path: impl Into<PathBuf>, owner_uid: u32) -> Self {
        Self { path: path.into(), owner_uid, max_bytes: AUDIT_MAX_BYTES }
    }

    fn generation(&self, n: u32) -> PathBuf {
        let mut s = self.path.as_os_str().to_os_string();
        s.push(format!(".{n}"));
        PathBuf::from(s)
    }

    #[cfg(test)]
    fn rotated_path(&self) -> PathBuf {
        self.generation(1)
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

    /// Append `records`, rotating first whenever the file has reached the
    /// limit; after a rotation `reopen` (the open record of the window in
    /// progress) is written again before anything else.
    pub fn append(&self, records: &[Value], reopen: Option<&Value>) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        duduclaw_core::with_file_lock(&self.path, || {
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
            file.sync_data()?;
            Ok(())
        })
        .map_err(|e| format!("writing {}: {e}", self.path.display()))
    }
}

fn write_line(file: &mut std::fs::File, record: &Value) -> std::io::Result<()> {
    // serde_json escapes every control character, so one record is one line.
    let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes())
}

fn open_append(path: &Path, owner_uid: u32) -> std::io::Result<std::fs::File> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: valid NUL-terminated path; a new file starts at 0600.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
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
mod tests {
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

    #[test]
    fn appends_one_json_line_per_record_with_0600() {
        let t = tempfile::tempdir().unwrap();
        let log = AuditLog::new(t.path().join("audit.log"), me());
        log.append(&[serde_json::json!({"event":"open","m":"a\nb"})], None).unwrap();
        log.append(&[serde_json::json!({"event":"close"})], None).unwrap();
        let got = lines(&log.path);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["m"], "a\nb");
        assert_eq!(std::fs::metadata(&log.path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn rotation_rewrites_the_open_record_at_the_top_of_the_new_file() {
        let t = tempfile::tempdir().unwrap();
        let mut log = AuditLog::new(t.path().join("audit.log"), me());
        log.max_bytes = 400;
        let open = serde_json::json!({"event":"open","window_id":"w1"});
        log.append(std::slice::from_ref(&open), Some(&open)).unwrap();
        let logins: Vec<Value> = (0..40).map(|i| serde_json::json!({"event":"login","n":i})).collect();
        log.append(&logins, Some(&open)).unwrap();
        let current = lines(&log.path);
        assert_eq!(current[0]["event"], "open");
        assert_eq!(current[0]["window_id"], "w1");
        assert_eq!(current[0]["rewritten_after_rotation"], true);
        assert!(log.rotated_path().exists());
        assert_eq!(current.last().unwrap()["n"], 39);
        // Older generations are kept, the oldest dropped beyond five.
        assert!(log.generation(2).exists());
        let many: Vec<Value> = (0..400).map(|i| serde_json::json!({"event":"login","n":i})).collect();
        log.append(&many, Some(&open)).unwrap();
        assert!(log.generation(AUDIT_GENERATIONS).exists());
        assert!(!log.generation(AUDIT_GENERATIONS + 1).exists());
    }

    #[test]
    fn untrusted_files_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let log = AuditLog::new(t.path().join("audit.log"), me());
        std::fs::write(&log.path, b"").unwrap();
        std::fs::set_permissions(&log.path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        assert!(log.append(&[serde_json::json!({})], None).is_err());
        std::fs::remove_file(&log.path).unwrap();
        let target = t.path().join("elsewhere");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, &log.path).unwrap();
        assert!(log.append(&[serde_json::json!({})], None).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"");
        let wrong_owner = AuditLog::new(t.path().join("other.log"), me().wrapping_add(1));
        assert!(wrong_owner.append(&[serde_json::json!({})], None).is_err());
    }
}
