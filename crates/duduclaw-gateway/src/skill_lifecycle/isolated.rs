//! Protected lifecycle proposals: E parses bytes; the host binds approval and publication.
use std::path::{Path, PathBuf};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::approval::{ApprovalBroker, operator_cli_gate::{self as gate, *}};

pub const SPEC: KindSpec = KindSpec {
    kind: "isolated_skill_publication", validity: Validity::Fixed(30), consume: Consume::Once,
    reminders: false, max_pending_per_target: 3, max_pending_per_kind: 20,
    push_cap_per_hour: Some(2), legacy_scope_key: "target",
    admin_refusal: "技能發布與維護只能由管理員在儀表板核准。",
    expired_text: "技能提案已逾期，請重新執行以建立提案。",
    channel_refusal: Some("技能提案只能由管理員在儀表板決定。"),
    notice: |_, _, deadline| format!("有技能發布或維護提案，請由管理員在儀表板檢視內容與決定。期限：{deadline}"),
    on_push_suppressed: |_, _, _| {},
};

pub(crate) fn digest(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

pub(crate) fn component(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && s != "." && s != ".."
        && !s.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

/// Walk only validated components from the same pinned home descriptor.
#[cfg(unix)]
fn directory(home: &std::fs::File, parts: &[&str], create: bool) -> Result<std::fs::File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use nix::{fcntl::{openat, OFlag}, sys::stat::{mkdirat, Mode}};
    let mut root = home.try_clone().map_err(|e| e.to_string())?;
    for part in parts {
        if !component(part) { return Err("invalid host directory".into()); }
        if create {
            match mkdirat(Some(root.as_raw_fd()), *part, Mode::from_bits_truncate(0o700)) {
                Ok(()) => root.sync_all().map_err(|e| e.to_string())?,
                Err(nix::errno::Errno::EEXIST) => {},
                Err(e) => return Err(e.to_string()),
            }
        }
        let fd = openat(Some(root.as_raw_fd()), *part,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty()).map_err(|e| e.to_string())?;
        // SAFETY: exactly one owned descriptor from openat.
        root = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(root)
}

/// Both protected and unprotected recovery bind the source to the recorded scope.
/// Windows deliberately refuses restoration: path-based moves cannot enforce the
/// descriptor-bound checks used on Unix. Keep the archive for manual recovery.
pub(crate) fn restore_archived(home: &Path, scope: &str, archived: &Path) -> Result<PathBuf, String> {
    let (archive_scope, parts) = if scope == "global" {
        ("global".to_owned(), vec!["skills"])
    } else {
        let id = scope.strip_prefix("agent:").ok_or("invalid restore scope")?;
        super::curator::owned_scope(id)?;
        (format!("agent-{id}"), vec!["agents", id, "SKILLS"])
    };
    let filename = archived.file_name().and_then(|name| name.to_str())
        .filter(|name| component(name)).ok_or("invalid archive filename")?;
    let expected = home.join("skills-archive").join(&archive_scope).join(filename);
    if archived.as_os_str() != expected.as_os_str() {
        return Err("archive path does not match scope".into());
    }
    #[cfg(unix)]
    {
        restore_unix(home, &archive_scope, &parts, filename)
    }
    #[cfg(not(unix))]
    {
        let _ = parts;
        Err("safe skill restore requires Unix; archive retained for manual recovery".into())
    }
}

#[cfg(unix)]
fn publish_noreplace(dir: &std::fs::File, temp: &str, filename: &str) -> Result<(), String> {
    use std::{ffi::CString, os::fd::AsRawFd};
    let temp = CString::new(temp).map_err(|e| e.to_string())?;
    let name = CString::new(filename).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    {
        duduclaw_core::linux_syscalls::renameat2(dir.as_raw_fd(), &temp,
            dir.as_raw_fd(), &name, libc::RENAME_NOREPLACE).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: live directory descriptor and NUL-terminated names; no ownership transfer.
        if unsafe { libc::renameatx_np(dir.as_raw_fd(), temp.as_ptr(),
            dir.as_raw_fd(), name.as_ptr(), libc::RENAME_EXCL) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (dir, temp, name);
        Err("no safe no-overwrite restore on this platform".into())
    }
}

#[cfg(unix)]
fn restore_unix(home: &Path, archive_scope: &str, parts: &[&str], filename: &str) -> Result<PathBuf, String> {
    use std::{io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::OpenOptionsExt}};
    use nix::{fcntl::{openat, OFlag}, sys::stat::{fstat, Mode, SFlag},
        unistd::{unlinkat, UnlinkatFlags}};
    let root = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home).map_err(|e| e.to_string())?;
    let source = directory(&root, &["skills-archive", archive_scope], false)?;
    let fd = openat(Some(source.as_raw_fd()), filename,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty()).map_err(|e| e.to_string())?;
    // SAFETY: exactly one owned descriptor from openat.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = fstat(file.as_raw_fd()).map_err(|e| e.to_string())?;
    if SFlag::from_bits_truncate(metadata.st_mode) & SFlag::S_IFMT != SFlag::S_IFREG {
        return Err("archive source is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let dest = directory(&root, parts, true)?;
    // Registry::load_skills skips dot-files. Only verified bytes reach the final name.
    let temp = format!(".{filename}.restore-{}", uuid::Uuid::new_v4());
    let fd = openat(Some(dest.as_raw_fd()), temp.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600)).map_err(|e| e.to_string())?;
    // SAFETY: exactly one owned descriptor from openat.
    let mut staged = unsafe { std::fs::File::from_raw_fd(fd) };
    let publication = (|| {
        staged.write_all(&bytes).map_err(|e| e.to_string())?;
        staged.sync_all().map_err(|e| e.to_string())?;
        publish_noreplace(&dest, &temp, filename)?;
        dest.sync_all().map_err(|e| e.to_string())
    })();
    if let Err(error) = publication {
        if let Err(cleanup) = unlinkat(Some(dest.as_raw_fd()), temp.as_str(), UnlinkatFlags::NoRemoveDir) {
            if cleanup != nix::errno::Errno::ENOENT {
                return Err(format!("{error}; restore temp cleanup failed: {cleanup}"));
            }
        }
        return Err(error);
    }
    // Keep the archive copy intentionally; the DB row becomes Active, so this copy is inert.
    drop(file);
    let mut restored = home.to_path_buf();
    for part in parts { restored.push(part); }
    Ok(restored.join(filename))
}

pub(crate) async fn approve(home: &Path, agent: &str, action: &str, target: &str,
    state: &Value, preview: Value) -> Result<(), String> {
    let encoded = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    let hash = digest(&encoded);
    let binding = Binding { action, target, request_digest: hash.clone(), state: hash,
        state_policy: StatePolicy::MustMatch, push_scope: None };
    let broker = ApprovalBroker::open(home)?;
    let filing = Filing { agent_id: agent, summary: "技能提案：請檢視完整內容、影響範圍及摘要後決定。",
        extra: json!({"action":action,"target":target,"preview":preview,"state":state}), ttl_secs: 86400 };
    let (result, _) = gate::gate(&broker, &SPEC, &binding, filing, Some(30), chrono::Utc::now()).await?;
    match result {
        gate::Gate::Proceed(_) => Ok(()),
        _ => Err("技能提案等待管理員在儀表板核准；核准後重新執行同一操作。".into()),
    }
}

/// First generation wins; a later retry reuses those exact model bytes.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Generation { response: String, generated_at: String }
pub(crate) fn cached(home: &Path, key: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("invalid proposal key".into()); }
    let bytes = crate::isolation_worker_plans::read_host_file(home, &["skill-proposals"], key)
        .map_err(|_| "proposal cache unavailable")?;
    bytes.map(|bytes| {
        let generation: Generation = serde_json::from_slice(&bytes).map_err(|_| "invalid staged generation")?;
        chrono::DateTime::parse_from_rfc3339(&generation.generated_at).map_err(|_| "invalid generation time")?;
        Ok((generation.response.into_bytes(), generation.generated_at))
    }).transpose()
}
pub(crate) fn cache(home: &Path, key: &str, bytes: &[u8]) -> Result<String, String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("invalid proposal key".into()); }
    let at = chrono::Utc::now().to_rfc3339();
    let response = std::str::from_utf8(bytes).map_err(|_| "invalid model encoding")?.to_owned();
    let bytes = serde_json::to_vec(&Generation { response, generated_at: at.clone() }).map_err(|e| e.to_string())?;
    crate::isolation_worker_plans::write_host_file(home, &["skill-proposals"], key, &bytes)
        .map_err(|_| "proposal cache publication unconfirmed".to_string())?;
    Ok(at)
}

pub(crate) fn published(home: &Path, key: &str) -> Result<bool, String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("invalid proposal key".into()); }
    crate::isolation_worker_plans::read_host_file(home, &["skill-proposals"], &format!("{key}.published"))
        .map(|v| v.is_some()).map_err(|_| "publication receipt unavailable".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn restore_archived_creates_missing_skills_dir_and_preserves_bytes() {
        let home = tempfile::tempdir().unwrap();
        let archived = home.path().join("skills-archive/agent-writer/notes.md");
        let bytes = "技能原文\n🦀\0".as_bytes();
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::create_dir_all(home.path().join("agents/writer")).unwrap();
        std::fs::create_dir_all(home.path().join("skill-proposals")).unwrap();
        let intent = home.path().join("skill-proposals/existing.archive-intent");
        std::fs::write(&intent, b"keep intent").unwrap();
        std::fs::write(&archived, bytes).unwrap();
        assert!(!home.path().join("agents/writer/SKILLS").exists());
        let restored = restore_archived(home.path(), "agent:writer", &archived).unwrap();
        assert_eq!(restored, home.path().join("agents/writer/SKILLS/notes.md"));
        assert_eq!(std::fs::read(restored).unwrap(), bytes);
        assert_eq!(std::fs::read(&archived).unwrap(), bytes);
        assert_eq!(std::fs::read(intent).unwrap(), b"keep intent");
    }

    #[test]
    fn restore_archived_refuses_existing_destination_without_changing_files() {
        let home = tempfile::tempdir().unwrap();
        let archived = home.path().join("skills-archive/global/notes.md");
        let dest = home.path().join("skills/notes.md");
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&archived, b"archived").unwrap();
        std::fs::write(&dest, b"existing").unwrap();
        assert!(restore_archived(home.path(), "global", &archived).is_err());
        assert_eq!(std::fs::read(archived).unwrap(), b"archived");
        assert_eq!(std::fs::read(dest).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(home.path().join("skills")).unwrap().count(), 1);
    }

    #[test]
    fn restore_archived_refuses_symlinked_archive_directory() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("notes.md");
        std::fs::write(&source, b"untouched").unwrap();
        std::fs::create_dir_all(home.path().join("skills-archive")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("skills-archive/global")).unwrap();
        let archived = home.path().join("skills-archive/global/notes.md");
        assert!(restore_archived(home.path(), "global", &archived).is_err());
        assert_eq!(std::fs::read(source).unwrap(), b"untouched");
        assert!(home.path().join("skills-archive/global").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!home.path().join("skills/notes.md").exists());
    }

    #[test]
    fn restore_refuses_other_scope_and_invalid_agents() {
        let home = tempfile::tempdir().unwrap();
        let alice = home.path().join("skills-archive/agent-alice/notes.md");
        let bob = home.path().join("skills-archive/agent-bob/notes.md");
        for (path, bytes) in [(&alice, b"alice".as_slice()), (&bob, b"bob".as_slice())] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        assert!(restore_archived(home.path(), "agent:bob", &alice).is_err());
        for scope in ["department:sales", "agent:_hidden", "agent:dashboard", "agent:..", "agent:", "other"] {
            assert!(restore_archived(home.path(), scope, &alice).is_err(), "{scope}");
        }
        assert_eq!(std::fs::read(alice).unwrap(), b"alice");
        assert_eq!(std::fs::read(bob).unwrap(), b"bob");
        assert!(!home.path().join("agents").exists());
    }

    #[test]
    fn restore_refuses_symlinked_archive_root_or_destination_components() {
        for linked in ["skills-archive", "agents", "agents/writer", "agents/writer/SKILLS"] {
            let home = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let link = home.path().join(linked);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(outside.path(), &link).unwrap();
            let archived = home.path().join("skills-archive/agent-writer/notes.md");
            std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
            std::fs::write(&archived, b"untouched").unwrap();
            assert!(restore_archived(home.path(), "agent:writer", &archived).is_err(), "{linked}");
            assert_eq!(std::fs::read(archived).unwrap(), b"untouched");
            assert!(!home.path().join("agents/writer/SKILLS/notes.md").exists());
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        }
    }

    #[test]
    fn restore_publishes_opened_bytes_and_keeps_archive_copy() {
        let home = tempfile::tempdir().unwrap();
        let archived = home.path().join("skills-archive/global/notes.md");
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::write(&archived, b"verified original").unwrap();
        let restored = restore_archived(home.path(), "global", &archived).unwrap();
        assert_eq!(std::fs::read(restored).unwrap(), b"verified original");
        assert_eq!(std::fs::read(archived).unwrap(), b"verified original");
        assert_eq!(std::fs::read_dir(home.path().join("skills")).unwrap().count(), 1);
    }

    #[test]
    fn restore_refuses_symlink_and_nonregular_source() {
        let home = tempfile::tempdir().unwrap();
        let archived = home.path().join("skills-archive/global/notes.md");
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        let outside = home.path().join("outside.md");
        std::fs::write(&outside, b"untouched").unwrap();
        std::os::unix::fs::symlink(&outside, &archived).unwrap();
        assert!(restore_archived(home.path(), "global", &archived).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"untouched");
        std::fs::remove_file(&archived).unwrap();
        std::fs::create_dir(&archived).unwrap();
        assert!(restore_archived(home.path(), "global", &archived).is_err());
        assert!(!home.path().join("skills").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn i1c_synthesis_staging_retains_generation_time_and_refuses_overwrite() {
        let home = tempfile::tempdir().unwrap(); let key = digest(b"exact evidence");
        assert!(cached(home.path(), &key).unwrap().is_none());
        let at = cache(home.path(), &key, b"actual model bytes").unwrap();
        let (bytes, replay_at) = cached(home.path(), &key).unwrap().unwrap();
        assert_eq!(bytes, b"actual model bytes"); assert_eq!(at, replay_at);
        assert!(cache(home.path(), &key, b"replacement").is_err());
        assert!(!published(home.path(), &key).unwrap());
        assert!(cached(home.path(), "../other").is_err());
    }
}
