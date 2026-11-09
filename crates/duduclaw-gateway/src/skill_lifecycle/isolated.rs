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

#[derive(Clone, Debug)]
pub(crate) struct Snapshot { pub directories: Vec<String>, pub filename: String, pub bytes: Vec<u8> }
impl Snapshot {
    pub fn read(&self, home: &Path) -> Result<Vec<u8>, String> {
        crate::isolation_worker_plans::read_host_file(home,
            &self.directories.iter().map(String::as_str).collect::<Vec<_>>(), &self.filename)
            .map_err(|_| "技能快照無法讀取".to_string())?.ok_or_else(|| "技能快照已消失".into())
    }
    pub fn verify(&self, home: &Path) -> Result<(), String> {
        if self.read(home)? != self.bytes { return Err("技能內容已變更，請重新建立提案".into()); }
        Ok(())
    }
}
fn component(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && s != "." && s != ".."
        && !s.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

/// All ancestor descriptors are retained; readdir uses only host-selected fds.
#[cfg(target_os = "linux")]
fn directory(home: &Path, parts: &[&str], create: bool) -> Result<Option<std::fs::File>, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use nix::{fcntl::{openat, OFlag}, sys::stat::{mkdirat, Mode}};
    let mut root = std::fs::File::open(home).map_err(|e| e.to_string())?;
    for part in parts {
        if !component(part) { return Err("invalid host directory".into()); }
        if create {
            match mkdirat(Some(root.as_raw_fd()), *part, Mode::from_bits_truncate(0o700)) {
                Ok(()) | Err(nix::errno::Errno::EEXIST) => {}, Err(e) => return Err(e.to_string()),
            }
        }
        match openat(Some(root.as_raw_fd()), *part,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW, Mode::empty()) {
            Ok(fd) => {
                // SAFETY: exactly one owned descriptor from openat.
                root = unsafe { std::fs::File::from_raw_fd(fd) };
            }
            Err(nix::errno::Errno::ENOENT) if !create => return Ok(None),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(Some(root))
}

#[cfg(target_os = "linux")]
pub(crate) fn snapshots(home: &Path, parts: &[&str]) -> Result<Vec<Snapshot>, String> {
    use std::os::fd::AsRawFd;
    fn walk(home: &Path, parts: Vec<String>, depth: usize, count: &mut usize, out: &mut Vec<Snapshot>) -> Result<(), String> {
        if depth > 8 { return Err("skill directory depth limit".into()); }
        let refs = parts.iter().map(String::as_str).collect::<Vec<_>>();
        let Some(root) = directory(home, &refs, false)? else { return Ok(()); };
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", root.as_raw_fd())).map_err(|e| e.to_string())? {
            *count += 1;
            if *count > 2000 { return Err("skill inventory limit".into()); }
            entries.push(entry.map_err(|e| e.to_string())?);
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().into_string().map_err(|_| "invalid skill filename")?;
            if !component(&name) { return Err("invalid skill filename".into()); }
            if name.starts_with('.') { continue; }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() { return Err("unsafe skill artifact".into()); }
            if kind.is_dir() {
                let mut nested = parts.clone(); nested.push(name); walk(home, nested, depth + 1, count, out)?;
            } else if (depth == 0 && name.to_ascii_lowercase().ends_with(".md")) || name.eq_ignore_ascii_case("SKILL.md") {
                if !kind.is_file() { return Err("unsafe skill artifact".into()); }
                let mut snapshot = Snapshot { directories: parts.clone(), filename: name, bytes: vec![] };
                snapshot.bytes = snapshot.read(home)?;
                if out.len() >= 1000 { return Err("skill inventory limit".into()); }
                out.push(snapshot);
            }
        }
        Ok(())
    }
    let mut out = vec![]; walk(home, parts.iter().map(|s| s.to_string()).collect(), 0, &mut 0, &mut out)?; Ok(out)
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn snapshots(_: &Path, _: &[&str]) -> Result<Vec<Snapshot>, String> { Err("isolated skills require Linux".into()) }

#[cfg(target_os = "linux")]
pub(crate) fn archive(home: &Path, snapshot: &Snapshot, scope: &str, skill: &str) -> Result<PathBuf, String> {
    use std::{ffi::CString, os::fd::AsRawFd};
    if !component(scope) || !component(&snapshot.filename) { return Err("invalid archive binding".into()); }
    let source = directory(home, &snapshot.directories.iter().map(String::as_str).collect::<Vec<_>>(), false)?
        .ok_or("source disappeared")?;
    let dest = directory(home, &["skills-archive", scope], true)?.ok_or("archive unavailable")?;
    snapshot.verify(home)?;
    let intent = serde_json::to_vec(&json!({"source":snapshot.directories,"filename":snapshot.filename,
        "sha256":digest(&snapshot.bytes),"archive_scope":scope,"skill":skill})).map_err(|e| e.to_string())?;
    let key = archive_key(scope,skill);
    match crate::isolation_worker_plans::read_host_file(home, &["skill-proposals"], &key).map_err(|_| "archive intent unavailable")? {
        Some(existing) if existing != intent => return Err("archive intent changed; manual reconciliation required".into()),
        Some(_) => {},
        None => crate::isolation_worker_plans::write_host_file(home, &["skill-proposals"], &key, &intent)
            .map_err(|_| "archive intent publication unconfirmed")?,
    }
    let name = CString::new(snapshot.filename.as_str()).map_err(|e| e.to_string())?;
    // Both directory fds stay alive; the single component is NUL-free. Raw
    // syscall: glibc's `renameat2` wrapper is newer than the release baseline.
    duduclaw_core::linux_syscalls::renameat2(source.as_raw_fd(), &name, dest.as_raw_fd(), &name, libc::RENAME_NOREPLACE)
        .map_err(|error| error.to_string())?;
    source.sync_all().map_err(|e| e.to_string())?; dest.sync_all().map_err(|e| e.to_string())?;
    let archived = Snapshot { directories: vec!["skills-archive".into(), scope.into()], filename: snapshot.filename.clone(), bytes: snapshot.bytes.clone() };
    archived.verify(home)?;
    Ok(home.join("skills-archive").join(scope).join(&snapshot.filename))
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn archive(_: &Path, _: &Snapshot, _: &str, _: &str) -> Result<PathBuf, String> { Err("isolated archive requires Linux".into()) }

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

#[cfg(target_os = "linux")]
pub(crate) fn agents(home: &Path) -> Result<Vec<String>, String> {
    use std::os::fd::AsRawFd;
    let Some(root) = directory(home, &["agents"], false)? else { return Ok(vec![]); };
    let mut ids = vec![];
    for entry in std::fs::read_dir(format!("/proc/self/fd/{}", root.as_raw_fd())).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry.file_name().into_string().map_err(|_| "invalid employee name")?;
        if !duduclaw_core::is_valid_agent_id(&id) || id.starts_with('_') { continue; }
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() { return Err("unsafe employee directory".into()); }
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() { ids.push(id); }
        if ids.len() > 1000 { return Err("employee inventory limit".into()); }
    }
    ids.sort(); Ok(ids)
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn agents(_: &Path) -> Result<Vec<String>, String> { Err("isolated inventory requires Linux".into()) }

fn archive_key(scope: &str, skill: &str) -> String {
    format!("{}.archive-intent", digest(&serde_json::to_vec(&(scope,skill)).expect("string pair")))
}
pub(crate) fn archive_pending(home: &Path, scope: &str, skill: &str) -> Result<bool, String> {
    crate::isolation_worker_plans::read_host_file(home, &["skill-proposals"], &archive_key(scope,skill))
        .map(|v| v.is_some()).map_err(|_| "archive intent unavailable".into())
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
    fn i1c_curator_inventory_matches_loader_and_refuses_unsafe_components() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("skills/notes/references")).unwrap();
        std::fs::write(home.path().join("skills/flat.MD"), "flat").unwrap();
        std::fs::write(home.path().join("skills/notes/SKILL.md"), "nested").unwrap();
        std::fs::write(home.path().join("skills/notes/references/api.md"), "reference").unwrap();
        std::fs::write(home.path().join("skills/.hidden.md"), "hidden").unwrap();
        let files = snapshots(home.path(), &["skills"]).unwrap();
        assert_eq!(files.len(), 2);
        symlink("notes", home.path().join("skills/link")).unwrap();
        assert!(snapshots(home.path(), &["skills"]).is_err());
    }
    #[test]
    fn i1c_curator_archive_preserves_bytes_refuses_clobber_and_keeps_recovery_intent() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("skills")).unwrap();
        std::fs::write(home.path().join("skills/notes.md"), "original").unwrap();
        let snapshot = snapshots(home.path(), &["skills"]).unwrap().pop().unwrap();
        std::fs::write(home.path().join("skills/notes.md"), "changed").unwrap();
        assert!(archive(home.path(), &snapshot, "global", "notes").is_err());
        std::fs::write(home.path().join("skills/notes.md"), "original").unwrap();
        let dest = archive(home.path(), &snapshot, "global", "notes").unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"original");
        assert!(!home.path().join("skills/notes.md").exists());
        assert!(archive_pending(home.path(), "global", "notes").unwrap());
        std::fs::write(home.path().join("skills/notes.md"), "original").unwrap();
        assert!(archive(home.path(), &snapshot, "global", "notes").is_err());
        assert_eq!(std::fs::read(home.path().join("skills/notes.md")).unwrap(), b"original");
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
