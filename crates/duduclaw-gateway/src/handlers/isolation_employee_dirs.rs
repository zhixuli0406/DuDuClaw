//! SR2 finding 1b: a Personal Employee proposal adopts an existing
//! `agents/<label>` directory only when this proposal flow created it.
//!
//! When `isolation.employee_propose` creates the directory (through
//! `create_agent_files`), it records a creation marker in the private
//! `migration-authority/apply.db`: label, owner and a digest over everything
//! the proposal adopts (SOUL.md, CONTRACT.toml, agent.toml, the SKILLS tree
//! listing and contents, and the label's org.toml entry). A retry for the
//! same owner is accepted only while that marker and digest still match; any
//! other existing directory (hand-made, imported, left by a former owner or
//! an earlier employee) is refused. The tombstone moves the directory into
//! quarantine and deletes the marker (`isolation_lifecycle`), so a directory
//! is never reused across employee ids.
use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

pub(super) const FOREIGN_DIR: &str = "這個員工代號已有一個不是由這次員工配置提案建立的設定資料夾（或內容已被修改），\
    不能用於個人專屬員工。請換一個代號，或請管理員先移除該資料夾。";

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SKILL_ENTRIES: usize = 4096;
const MAX_SKILL_DEPTH: usize = 8;
const ADOPTED_FILES: [&str; 3] = ["SOUL.md", "CONTRACT.toml", "agent.toml"];

/// Marker table in apply.db; created with the request index.
pub(super) const MARKER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_dir_markers (
    label TEXT PRIMARY KEY, owner_user_id TEXT NOT NULL, digest TEXT NOT NULL, created_at TEXT NOT NULL);";

fn put(hash: &mut Sha256, tag: &str, bytes: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// One adopted entry, never followed through a symlink. Directories are
/// walked only under SKILLS; anything else that is not a regular file of
/// bounded size refuses (fail closed).
fn entry(hash: &mut Sha256, path: &Path, relative: &str, depth: usize, count: &mut usize) -> Result<(), &'static str> {
    *count += 1;
    if *count > MAX_SKILL_ENTRIES || depth > MAX_SKILL_DEPTH { return Err(FOREIGN_DIR); }
    let metadata = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            put(hash, &format!("absent:{relative}"), b"");
            return Ok(());
        }
        Err(_) => return Err(FOREIGN_DIR),
        Ok(metadata) => metadata,
    };
    let kind = metadata.file_type();
    if kind.is_file() && metadata.len() <= MAX_FILE_BYTES {
        let bytes = std::fs::read(path).map_err(|_| FOREIGN_DIR)?;
        if bytes.len() as u64 > MAX_FILE_BYTES { return Err(FOREIGN_DIR); }
        put(hash, &format!("file:{relative}"), &bytes);
        return Ok(());
    }
    if kind.is_dir() && (relative == "SKILLS" || relative.starts_with("SKILLS/")) {
        put(hash, &format!("dir:{relative}"), b"");
        let mut names = Vec::new();
        for child in std::fs::read_dir(path).map_err(|_| FOREIGN_DIR)? {
            let name = child.map_err(|_| FOREIGN_DIR)?.file_name().into_string().map_err(|_| FOREIGN_DIR)?;
            if name.contains('/') || name.contains('\0') { return Err(FOREIGN_DIR); }
            names.push(name);
            if names.len() > MAX_SKILL_ENTRIES { return Err(FOREIGN_DIR); }
        }
        names.sort();
        for name in names {
            entry(hash, &path.join(&name), &format!("{relative}/{name}"), depth + 1, count)?;
        }
        return Ok(());
    }
    Err(FOREIGN_DIR)
}

/// Digest over everything a Personal proposal adopts from `agents/<label>`.
pub(super) fn agent_dir_digest(home: &Path, agents_dir: &Path, label: &str) -> Result<String, &'static str> {
    let dir = agents_dir.join(label);
    let metadata = std::fs::symlink_metadata(&dir).map_err(|_| FOREIGN_DIR)?;
    if !metadata.file_type().is_dir() { return Err(FOREIGN_DIR); }
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-employee-dir-v1\0");
    put(&mut hash, "label", label.as_bytes());
    let mut count = 0;
    for name in ADOPTED_FILES {
        entry(&mut hash, &dir.join(name), name, 0, &mut count)?;
    }
    entry(&mut hash, &dir.join("SKILLS"), "SKILLS", 0, &mut count)?;
    match duduclaw_core::org_store::load(home).get(label) {
        Some(org) => {
            put(&mut hash, "org.reports_to", org.reports_to.as_bytes());
            put(&mut hash, "org.department", org.department.as_bytes());
        }
        None => put(&mut hash, "org.absent", b""),
    }
    Ok(hex::encode(hash.finalize()))
}

/// Record (or replace) the marker right after this flow created the
/// directory: a directory that did not exist a moment ago cannot carry an
/// older marker's meaning.
pub(super) fn record_marker(home: &Path, agents_dir: &Path, label: &str, owner: &str) -> Result<(), &'static str> {
    let digest = agent_dir_digest(home, agents_dir, label)?;
    let db = open_requests(home)?;
    db.execute("INSERT INTO employee_dir_markers(label,owner_user_id,digest,created_at) VALUES(?1,?2,?3,?4)
        ON CONFLICT(label) DO UPDATE SET owner_user_id=excluded.owner_user_id,digest=excluded.digest,created_at=excluded.created_at",
        params![label, owner, digest, chrono::Utc::now().to_rfc3339()]).map_err(|_| STORE_UNAVAILABLE)?;
    Ok(())
}

/// An existing directory may be adopted for `Personal(owner)` only with a
/// marker of this flow for the same owner whose digest still matches, and
/// never while a live (not tombstoned) employee holds the label.
pub(super) fn check_marker(db: &Connection, home: &Path, agents_dir: &Path, label: &str, owner: &str)
    -> Result<(), &'static str> {
    let has_confirmed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='confirmed_employees')",
        [], |r| r.get(0)).map_err(|_| STORE_UNAVAILABLE)?;
    if has_confirmed {
        let row: Option<String> = db.query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [label],
            |r| r.get(0)).optional().map_err(|_| STORE_UNAVAILABLE)?;
        if let Some(encoded) = row {
            let receipt: Receipt = serde_json::from_str(&encoded).map_err(|_| STORE_UNAVAILABLE)?;
            // A live employee owns this directory: never re-proposed into it.
            if !receipt.disabled { return Err(LABEL_TAKEN); }
        }
    }
    let marker: Option<(String, String)> = db.query_row(
        "SELECT owner_user_id,digest FROM employee_dir_markers WHERE label=?1", [label],
        |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|_| STORE_UNAVAILABLE)?;
    let Some((marked_owner, digest)) = marker else { return Err(FOREIGN_DIR) };
    if marked_owner != owner { return Err(FOREIGN_DIR); }
    if agent_dir_digest(home, agents_dir, label)? != digest { return Err(FOREIGN_DIR); }
    Ok(())
}

/// [`check_marker`] on its own apply.db connection.
pub(super) fn adopt_existing(home: &Path, agents_dir: &Path, label: &str, owner: &str) -> Result<(), &'static str> {
    let db = open_requests(home)?;
    check_marker(&db, home, agents_dir, label, owner)
}
