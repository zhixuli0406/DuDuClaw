//! Recoverable global publication. Only durably logged bytes enter skills/.
use super::{read_host_file_limit, VerificationError};
use std::{fs::File, io::Write, os::fd::{AsRawFd, FromRawFd}, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use nix::{fcntl::{openat, OFlag}, sys::stat::{mkdirat, Mode}};

const JOURNAL: &str = ".isolation-skill-publications";
const LOG: &str = "graduation_log.jsonl";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    id: String,
    name: String,
    agent: String,
    digest: String,
    committed: bool,
    staged: Option<FileIdentity>,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileIdentity { device: u64, inode: u64 }
fn snapshot(dir: &File, name: &str) -> Result<Option<(FileIdentity, Vec<u8>)>, VerificationError> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let fd = match openat(Some(dir.as_raw_fd()), name, OFlag::O_RDONLY | OFlag::O_CLOEXEC
        | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK, Mode::empty()) {
        Ok(fd) => fd,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(error) => return Err(unavailable(error)),
    };
    // SAFETY: openat returned one owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(unavailable)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > 64 * 1024 {
        return Err(VerificationError::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes).map_err(unavailable)?;
    if bytes.len() as u64 != metadata.len() { return Err(VerificationError::Invalid); }
    Ok(Some((FileIdentity { device:metadata.dev(), inode:metadata.ino() }, bytes)))
}
fn unavailable(_: impl std::fmt::Debug) -> VerificationError { VerificationError::Unavailable }
fn digest(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn dir(home: &Path, name: &str) -> Result<File, VerificationError> {
    let root = File::open(home).map_err(unavailable)?;
    match mkdirat(Some(root.as_raw_fd()), name, Mode::from_bits_truncate(0o700)) {
        Ok(()) | Err(nix::errno::Errno::EEXIST) => {},
        Err(error) => return Err(unavailable(error)),
    }
    root.sync_all().map_err(unavailable)?;
    let fd = openat(Some(root.as_raw_fd()), name, OFlag::O_RDONLY | OFlag::O_CLOEXEC
        | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW, Mode::empty()).map_err(unavailable)?;
    // SAFETY: openat returned one owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn path(dir: &File, name: &str) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(name)
}
fn atomic_write(dir: &File, name: &str, bytes: &[u8]) -> Result<(), VerificationError> {
    let mut staged = tempfile::NamedTempFile::new_in(path(dir, "")).map_err(unavailable)?;
    staged.write_all(bytes).map_err(unavailable)?;
    staged.as_file().sync_all().map_err(unavailable)?;
    staged.persist(path(dir, name)).map_err(unavailable)?;
    dir.sync_all().map_err(unavailable)
}
fn save(journal: &File, intent: &Intent) -> Result<(), VerificationError> {
    atomic_write(journal, &format!("{}.json", intent.id), &serde_json::to_vec(intent).map_err(unavailable)?)
}
fn log_update(home: &Path, intent: &Intent, add: bool) -> Result<(), VerificationError> {
    duduclaw_core::with_file_lock(&home.join(LOG), || {
        let result = (|| {
            let previous = read_host_file_limit(home, &[], LOG, 16 * 1024 * 1024)?.unwrap_or_default();
            let text = std::str::from_utf8(&previous).map_err(unavailable)?;
            let mut output = Vec::new();
            for line in text.lines().filter(|line| !line.is_empty()) {
                let entry: serde_json::Value = serde_json::from_str(line).map_err(unavailable)?;
                if entry["publication_id"].as_str() != Some(&intent.id) {
                    output.extend_from_slice(line.as_bytes()); output.push(b'\n');
                }
            }
            if add {
                duduclaw_mcp_boundary::io_authority::verify_before_io()?;
                let record = serde_json::json!({"skill_name":intent.name,"source_agent":intent.agent,
                    "graduated_at":chrono::Utc::now(),"lift":0.0,"load_count":0,"publication_id":intent.id});
                serde_json::to_writer(&mut output, &record).map_err(unavailable)?;
                output.push(b'\n');
            }
            atomic_write(&File::open(home).map_err(unavailable)?, LOG, &output)
        })();
        result.map_err(std::io::Error::other)
    }).map_err(unavailable)
}
fn remove_if_present(dir: &File, name: &str) -> Result<(), VerificationError> {
    match std::fs::remove_file(path(dir, name)) {
        Ok(()) => dir.sync_all().map_err(unavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(unavailable(error)),
    }
}
// Compensation is host-owned housekeeping: revocation must not prevent rollback.
// A mismatched destination is never deleted and keeps the intent for operator repair.
fn compensate(home: &Path, journal: &File, intent: &Intent) -> Result<(), VerificationError> {
    let filename = format!("{}.md", intent.name);
    let destination = dir(home, "skills")?;
    if let Some((identity, bytes)) = snapshot(&destination, &filename)? {
        // Equal bytes alone do not prove this transaction installed the file.
        if Some(identity) != intent.staged || digest(&bytes) != intent.digest {
            return Err(VerificationError::BindingMismatch);
        }
        remove_if_present(&destination, &filename)?;
    }
    log_update(home, intent, false)?;
    remove_if_present(journal, &format!("{}.stage", intent.id))?;
    remove_if_present(journal, &format!("{}.json", intent.id))
}
fn parse(bytes: &[u8], filename: &str) -> Result<Intent, VerificationError> {
    let intent: Intent = serde_json::from_slice(bytes).map_err(unavailable)?;
    if !valid_name(&intent.name) || !valid_name(&intent.agent) || intent.id.len() != 64
        || !intent.id.bytes().all(|b| b.is_ascii_hexdigit()) || intent.digest.len() != 64
        || !intent.digest.bytes().all(|b| b.is_ascii_hexdigit()) || filename != format!("{}.json", intent.id) {
        return Err(VerificationError::Invalid);
    }
    Ok(intent)
}
/// Called at producer construction, before protected operations are admitted.
/// Recovery only rolls back unfinished work; it never resurrects revoked authority.
pub(super) fn recover(home: &Path) -> Result<(), VerificationError> {
    if !home.join(JOURNAL).try_exists().map_err(unavailable)? { return Ok(()); }
    duduclaw_core::with_file_lock(&home.join("skills-publication.lock"), || {
        let result = (|| {
            let journal = dir(home, JOURNAL)?;
            for entry in std::fs::read_dir(path(&journal, "")).map_err(unavailable)? {
                let name = entry.map_err(unavailable)?.file_name().into_string().map_err(unavailable)?;
                if !name.ends_with(".json") { continue; }
                let bytes = read_host_file_limit(home, &[JOURNAL], &name, 4096)?.ok_or(VerificationError::Unavailable)?;
                let intent = parse(&bytes, &name)?;
                if !intent.committed { compensate(home, &journal, &intent)?; }
            }
            Ok::<_, VerificationError>(())
        })();
        result.map_err(std::io::Error::other)
    }).map_err(unavailable)
}

/// Caller holds skills-publication.lock. Fault callback is a deterministic test seam.
pub(super) fn publish(home: &Path, agent: &str, name: &str, source_digest: &str, bytes: &[u8])
    -> Result<(), VerificationError> {
    publish_checked(home, agent, name, source_digest, bytes, |_| Ok(()))
}
pub(super) fn publish_checked(home: &Path, agent: &str, name: &str, source_digest: &str, bytes: &[u8],
    mut checkpoint: impl FnMut(&str) -> Result<(), VerificationError>) -> Result<(), VerificationError> {
    if !valid_name(agent) || !valid_name(name) { return Err(VerificationError::Invalid); }
    let content_digest = digest(bytes);
    let id = digest(format!("{agent}\0{name}\0{source_digest}\0{content_digest}").as_bytes());
    let journal = dir(home, JOURNAL)?;
    let filename = format!("{name}.md");
    if let Some(previous) = read_host_file_limit(home, &[JOURNAL], &format!("{id}.json"), 4096)? {
        let old = parse(&previous, &format!("{id}.json"))?;
        if old.committed {
            duduclaw_mcp_boundary::io_authority::verify_before_io()?;
            let actual = snapshot(&dir(home, "skills")?, &filename)?;
            return if actual.is_some_and(|(identity, bytes)| Some(identity) == old.staged && digest(&bytes) == content_digest) {
                Ok(())
            } else { Err(VerificationError::BindingMismatch) };
        }
        compensate(home, &journal, &old)?;
    }
    // Reject pre-existing content before an intent could claim ownership of it.
    if read_host_file_limit(home, &["skills"], &filename, 64 * 1024)?.is_some() {
        return Err(VerificationError::BindingMismatch);
    }
    let mut intent = Intent { id, name:name.into(), agent:agent.into(), digest:content_digest, committed:false, staged:None };
    let stage = format!("{}.stage", intent.id);
    let mut published = false;
    let result = (|| {
        duduclaw_mcp_boundary::io_authority::verify_before_io()?;
        save(&journal, &intent)?;
        checkpoint("intent")?;
        atomic_write(&journal, &stage, bytes)?;
        intent.staged = Some(snapshot(&journal, &stage)?.ok_or(VerificationError::Unavailable)?.0);
        save(&journal, &intent)?;
        checkpoint("stage")?;
        log_update(home, &intent, true)?;
        checkpoint("log")?;
        let destination = dir(home, "skills")?;
        duduclaw_mcp_boundary::io_authority::verify_before_io()?;
        let from = std::ffi::CString::new(stage.as_str()).map_err(unavailable)?;
        let to = std::ffi::CString::new(filename.as_str()).map_err(unavailable)?;
        // Retained directory FDs and validated single-component names; raw
        // syscall because glibc's `renameat2` wrapper is above the baseline.
        duduclaw_core::linux_syscalls::renameat2(journal.as_raw_fd(), &from, destination.as_raw_fd(),
            &to, libc::RENAME_NOREPLACE).map_err(|_| VerificationError::Unavailable)?;
        published = true;
        checkpoint("rename")?;
        destination.sync_all().map_err(unavailable)?;
        journal.sync_all().map_err(unavailable)?;
        checkpoint("directory_sync")?;
        duduclaw_mcp_boundary::io_authority::verify_before_io()?;
        checkpoint("commit")?;
        Ok(())
    })();
    if result.is_err() {
        // Before rename we own no destination, including a racing external writer.
        if !published && read_host_file_limit(home, &["skills"], &filename, 64 * 1024)?.is_some() {
            return result;
        }
        compensate(home, &journal, &intent)?;
    }
    result?;
    // All effect bytes and the log are durable now. If receipt persistence fails,
    // leave recovery to distinguish a pending rollback from a committed retry.
    checkpoint("receipt")?;
    if let Err(error) = duduclaw_mcp_boundary::io_authority::verify_before_io() {
        compensate(home, &journal, &intent)?;
        return Err(error);
    }
    intent.committed = true;
    save(&journal, &intent)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn i1c_graduation_compensates_every_failure_and_retry_is_idempotent() {
        for failure in ["intent", "stage", "log", "rename", "directory_sync", "commit"] {
            let home = tempfile::tempdir().unwrap();
            assert!(publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
                if point == failure { Err(VerificationError::Unavailable) } else { Ok(()) }
            }).is_err(), "{failure}");
            assert!(!home.path().join("skills/notes.md").exists(), "{failure}");
            let log = std::fs::read_to_string(home.path().join(LOG)).unwrap_or_default();
            assert!(log.is_empty(), "{failure}");
            assert_eq!(std::fs::read_dir(home.path().join(JOURNAL)).unwrap().count(), 0, "{failure}");
            publish(home.path(), "alice", "notes", "source", b"scanned").unwrap();
            publish(home.path(), "alice", "notes", "source", b"scanned").unwrap();
            assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"scanned");
            assert_eq!(std::fs::read_to_string(home.path().join(LOG)).unwrap().lines().count(), 1);
        }
    }
    #[test]
    fn i1c_graduation_boot_recovers_crashes_and_preserves_committed_files() {
        for crash in ["intent", "stage", "log", "rename", "directory_sync", "commit"] {
            let home = tempfile::tempdir().unwrap();
            let result = std::panic::catch_unwind(|| publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
                if point == crash { panic!("simulated process death"); } Ok(())
            }));
            assert!(result.is_err());
            recover(home.path()).unwrap();
            assert!(!home.path().join("skills/notes.md").exists(), "{crash}");
            publish(home.path(), "alice", "notes", "source", b"scanned").unwrap();
            recover(home.path()).unwrap();
            assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"scanned");
        }
    }
    #[test]
    fn i1c_graduation_rollback_preserves_other_records_and_existing_destinations() {
        let home = tempfile::tempdir().unwrap();
        let other = b"{\"skill_name\":\"other\",\"source_agent\":\"bob\"}\n";
        std::fs::write(home.path().join(LOG), other).unwrap();
        assert!(publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
            if point == "log" { Err(VerificationError::Unavailable) } else { Ok(()) }
        }).is_err());
        assert_eq!(std::fs::read(home.path().join(LOG)).unwrap(), other);
        std::fs::create_dir_all(home.path().join("skills")).unwrap();
        std::fs::write(home.path().join("skills/notes.md"), b"existing").unwrap();
        assert!(publish(home.path(), "alice", "notes", "source", b"scanned").is_err());
        assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"existing");
    }
    #[test]
    fn i1c_graduation_receipt_failure_keeps_logged_bytes_until_boot_rollback() {
        let home = tempfile::tempdir().unwrap();
        assert!(publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
            if point == "receipt" { Err(VerificationError::Unavailable) } else { Ok(()) }
        }).is_err());
        assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"scanned");
        assert_eq!(std::fs::read_to_string(home.path().join(LOG)).unwrap().lines().count(), 1);
        recover(home.path()).unwrap();
        assert!(!home.path().join("skills/notes.md").exists());
        publish(home.path(), "alice", "notes", "source", b"scanned").unwrap();
    }
    #[test]
    fn i1c_graduation_log_io_failure_stays_unpublished_and_recovers() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(LOG)).unwrap();
        assert!(publish(home.path(), "alice", "notes", "source", b"scanned").is_err());
        assert!(!home.path().join("skills/notes.md").exists());
        std::fs::remove_dir(home.path().join(LOG)).unwrap();
        recover(home.path()).unwrap();
        publish(home.path(), "alice", "notes", "source", b"scanned").unwrap();
    }
    #[test]
    fn i1c_graduation_recovery_preserves_foreign_same_digest_file_before_rename() {
        for crash in ["intent", "stage", "log"] {
            let home = tempfile::tempdir().unwrap();
            let _ = std::panic::catch_unwind(|| publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
                if point == crash { panic!("crash"); } Ok(())
            }));
            std::fs::create_dir_all(home.path().join("skills")).unwrap();
            std::fs::write(home.path().join("skills/notes.md"), b"scanned").unwrap();
            assert!(recover(home.path()).is_err());
            assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"scanned");
        }
    }
    #[test]
    fn i1c_graduation_rename_conflict_never_claims_foreign_same_digest_file() {
        let home = tempfile::tempdir().unwrap();
        assert!(publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
            if point == "log" {
                std::fs::create_dir_all(home.path().join("skills")).unwrap();
                std::fs::write(home.path().join("skills/notes.md"), b"scanned").unwrap();
            }
            Ok(())
        }).is_err());
        assert!(recover(home.path()).is_err());
        assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"scanned");
    }
    #[test]
    fn i1c_graduation_recovery_never_deletes_different_bytes() {
        let home = tempfile::tempdir().unwrap();
        let _ = std::panic::catch_unwind(|| publish_checked(home.path(), "alice", "notes", "source", b"scanned", |point| {
            if point == "rename" { panic!("crash"); } Ok(())
        }));
        std::fs::write(home.path().join("skills/notes.md"), b"foreign replacement").unwrap();
        assert!(recover(home.path()).is_err());
        assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"foreign replacement");
    }
}
