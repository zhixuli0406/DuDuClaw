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
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn read_regular(dir: &std::fs::File, filename: &str) -> Result<Vec<u8>, String> {
    use std::{io::Read, os::fd::{AsRawFd, FromRawFd}};
    use nix::{fcntl::{openat, OFlag}, sys::stat::Mode};
    if !component(filename) { return Err("invalid skill filename".into()); }
    let fd = openat(Some(dir.as_raw_fd()), filename,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty()).map_err(|e| e.to_string())?;
    // SAFETY: exactly one owned descriptor from openat.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("archive source is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Reverse a flat protected archive without following symlinks or overwriting.
/// Existing archive-intent files are deliberately left untouched.
#[cfg(target_os = "linux")]
pub(crate) fn restore_archived(home: &Path, scope: &str, archived: &Path) -> Result<PathBuf, String> {
    use std::{ffi::CString, os::{fd::AsRawFd, unix::fs::OpenOptionsExt}};
    let (archive_scope, parts) = if scope == "global" {
        ("global".to_owned(), vec!["skills"])
    } else {
        let id = scope.strip_prefix("agent:").filter(|id| component(id)).ok_or("invalid restore scope")?;
        (format!("agent-{id}"), vec!["agents", id, "SKILLS"])
    };
    if !component(&archive_scope) { return Err("invalid archive scope".into()); }
    let filename = archived.file_name().and_then(|name| name.to_str())
        .filter(|name| component(name)).ok_or("invalid archive filename")?;
    if archived != home.join("skills-archive").join(&archive_scope).join(filename) {
        return Err("archive path does not match scope".into());
    }
    let root = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home).map_err(|e| e.to_string())?;
    let source = directory(&root, &["skills-archive", &archive_scope], false)?;
    let bytes = read_regular(&source, filename)?;
    let dest = directory(&root, &parts, true)?;
    let name = CString::new(filename).map_err(|e| e.to_string())?;
    duduclaw_core::linux_syscalls::renameat2(source.as_raw_fd(), &name,
        dest.as_raw_fd(), &name, libc::RENAME_NOREPLACE).map_err(|e| e.to_string())?;
    source.sync_all().map_err(|e| e.to_string())?;
    dest.sync_all().map_err(|e| e.to_string())?;
    if read_regular(&dest, filename)? != bytes {
        return Err("restored skill bytes changed; recovery unconfirmed".into());
    }
    let mut restored = home.to_path_buf();
    for part in parts { restored.push(part); }
    Ok(restored.join(filename))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn restore_archived(_: &Path, _: &str, _: &Path) -> Result<PathBuf, String> {
    Err("protected skill restore requires Linux".into())
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

#[cfg(all(test, target_os = "linux"))]
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
        assert!(!archived.exists());
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
