//! SR2 finding 1b: a Personal Employee proposal adopts an existing
//! `agents/<label>` directory only when this proposal flow created it.
//!
//! When `isolation.employee_propose` creates the directory (through
//! `create_agent_files`), it records a creation marker in the private
//! `migration-authority/apply.db`: label, owner and a digest over everything
//! the proposal adopts (SOUL.md, CONTRACT.toml, agent.toml, the hook
//! settings `.claude/settings.json`, the SKILLS tree listing and contents,
//! and the label's org.toml entry). A retry for the same owner is accepted
//! only while that marker and digest still match; any other existing
//! directory (hand-made, imported, left by a former owner or an earlier
//! employee) is refused. The tombstone moves the directory into quarantine
//! and deletes the marker (`isolation_lifecycle`), so a directory is never
//! reused across employee ids.
//!
//! SR2 round 3 (1b-residual):
//! - The directory may hold nothing outside [`ALLOWED_TOP`] (and `.claude/`
//!   nothing but `settings.json`): anything else the registry or runtime
//!   could load (IDENTITY.md, MEMORY.md, instructions, MCP config, …) is not
//!   covered by the digest, so it refuses (fail closed).
//! - The proposal's `settings` are parsed from exactly the agent.toml bytes
//!   inside the digest ([`Snapshot::settings`]), never from the in-memory
//!   registry entry looked up by name.
use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

pub(super) const FOREIGN_DIR: &str = "這個員工代號已有一個不是由這次員工配置提案建立的設定資料夾（或內容已被修改），\
    不能用於個人專屬員工。請換一個代號，或請管理員先移除該資料夾。";

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SKILL_ENTRIES: usize = 4096;
const MAX_SKILL_DEPTH: usize = 8;
const ADOPTED_FILES: [&str; 2] = ["SOUL.md", "CONTRACT.toml"];
const AGENT_TOML: &str = "agent.toml";
const HOOK_DIR: &str = ".claude";
const HOOK_SETTINGS: &str = "settings.json";
/// Every top-level entry an adopted directory may hold: what
/// `create_agent_files` writes (agent.toml, SOUL.md, SKILLS/,
/// .claude/settings.json) plus CONTRACT.toml. Anything else refuses.
pub(super) const ALLOWED_TOP: [&str; 5] = ["SOUL.md", "CONTRACT.toml", AGENT_TOML, "SKILLS", HOOK_DIR];

/// Marker table in apply.db; created with the request index. F6g (R8):
/// `origin ∈ {created, adopted}` and `quarantine_ref` are added by
/// [`crate::isolation_adopt::ensure_marker_schema`]; only the adoption apply
/// step writes an `adopted` marker and [`check_marker`] refuses one.
pub(crate) const MARKER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_dir_markers (
    label TEXT PRIMARY KEY, owner_user_id TEXT NOT NULL, digest TEXT NOT NULL, created_at TEXT NOT NULL);";

/// One read of what a Personal proposal adopts: the digest and the exact
/// agent.toml bytes that went into it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) digest: String,
    agent_toml: Vec<u8>,
}

impl Snapshot {
    /// The proposal's `settings`, parsed from exactly the digested agent.toml
    /// bytes the way the registry loads an unbound employee (parse, then the
    /// same post-deserialize clamps), and encoded as the approval validator
    /// compares them.
    pub(crate) fn settings(&self) -> Result<String, &'static str> {
        let text = std::str::from_utf8(&self.agent_toml).map_err(|_| SETTINGS_INVALID)?;
        let mut config: duduclaw_core::types::AgentConfig = toml::from_str(text).map_err(|_| SETTINGS_INVALID)?;
        config.proactive.sanitize();
        config.sticker.sanitize();
        settings_for(&config)
    }
}

fn put(hash: &mut Sha256, tag: &str, bytes: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// Sorted entry names of a directory (never followed), bounded.
fn names(path: &Path) -> Result<Vec<String>, &'static str> {
    let mut names = Vec::new();
    for child in std::fs::read_dir(path).map_err(|_| FOREIGN_DIR)? {
        let name = child.map_err(|_| FOREIGN_DIR)?.file_name().into_string().map_err(|_| FOREIGN_DIR)?;
        if name.contains('/') || name.contains('\0') { return Err(FOREIGN_DIR); }
        names.push(name);
        if names.len() > MAX_SKILL_ENTRIES { return Err(FOREIGN_DIR); }
    }
    names.sort();
    Ok(names)
}

/// A regular file of bounded size, never followed through a symlink.
/// `Ok(None)` when absent.
fn regular_file(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    let metadata = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(FOREIGN_DIR),
        Ok(metadata) => metadata,
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES { return Err(FOREIGN_DIR); }
    let bytes = std::fs::read(path).map_err(|_| FOREIGN_DIR)?;
    if bytes.len() as u64 > MAX_FILE_BYTES { return Err(FOREIGN_DIR); }
    Ok(Some(bytes))
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
    if kind.is_file() {
        let bytes = regular_file(path)?.ok_or(FOREIGN_DIR)?;
        put(hash, &format!("file:{relative}"), &bytes);
        return Ok(());
    }
    if kind.is_dir() && (relative == "SKILLS" || relative.starts_with("SKILLS/")) {
        put(hash, &format!("dir:{relative}"), b"");
        for name in names(path)? {
            entry(hash, &path.join(&name), &format!("{relative}/{name}"), depth + 1, count)?;
        }
        return Ok(());
    }
    Err(FOREIGN_DIR)
}

/// Digest over everything a Personal proposal adopts from `agents/<label>`,
/// plus the agent.toml bytes it covers. An entry outside [`ALLOWED_TOP`], a
/// missing agent.toml or any link/special file refuses.
pub(crate) fn snapshot(home: &Path, agents_dir: &Path, label: &str) -> Result<Snapshot, &'static str> {
    let dir = agents_dir.join(label);
    let metadata = std::fs::symlink_metadata(&dir).map_err(|_| FOREIGN_DIR)?;
    if !metadata.file_type().is_dir() { return Err(FOREIGN_DIR); }
    if names(&dir)?.iter().any(|name| !ALLOWED_TOP.contains(&name.as_str())) { return Err(FOREIGN_DIR); }
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-employee-dir-v2\0");
    put(&mut hash, "label", label.as_bytes());
    // Read once: the digested bytes are the bytes `settings` is parsed from.
    let agent_toml = regular_file(&dir.join(AGENT_TOML))?.ok_or(FOREIGN_DIR)?;
    put(&mut hash, &format!("file:{AGENT_TOML}"), &agent_toml);
    let mut count = 0;
    for name in ADOPTED_FILES {
        entry(&mut hash, &dir.join(name), name, 0, &mut count)?;
    }
    entry(&mut hash, &dir.join("SKILLS"), "SKILLS", 0, &mut count)?;
    // The hook settings `create_agent_files` installs, and nothing else.
    let hooks = dir.join(HOOK_DIR);
    match std::fs::symlink_metadata(&hooks) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => put(&mut hash, &format!("absent:{HOOK_DIR}"), b""),
        Ok(metadata) if metadata.file_type().is_dir() => {
            if names(&hooks)?.iter().any(|name| name != HOOK_SETTINGS) { return Err(FOREIGN_DIR); }
            put(&mut hash, &format!("dir:{HOOK_DIR}"), b"");
            entry(&mut hash, &hooks.join(HOOK_SETTINGS), &format!("{HOOK_DIR}/{HOOK_SETTINGS}"), 1, &mut count)?;
        }
        _ => return Err(FOREIGN_DIR),
    }
    match duduclaw_core::org_store::load(home).get(label) {
        Some(org) => {
            put(&mut hash, "org.reports_to", org.reports_to.as_bytes());
            put(&mut hash, "org.department", org.department.as_bytes());
        }
        None => put(&mut hash, "org.absent", b""),
    }
    Ok(Snapshot { digest: hex::encode(hash.finalize()), agent_toml })
}

/// The digest alone (see [`snapshot`]).
pub(super) fn agent_dir_digest(home: &Path, agents_dir: &Path, label: &str) -> Result<String, &'static str> {
    snapshot(home, agents_dir, label).map(|snapshot| snapshot.digest)
}

/// Record (or replace) the marker right after this flow created the
/// directory: a directory that did not exist a moment ago cannot carry an
/// older marker's meaning.
pub(super) fn record_marker(home: &Path, agents_dir: &Path, label: &str, owner: &str) -> Result<(), &'static str> {
    let digest = agent_dir_digest(home, agents_dir, label)?;
    let db = open_requests(home)?;
    // F6g (R8): an `adopted` marker is never replaced by this flow.
    db.execute("INSERT INTO employee_dir_markers(label,owner_user_id,digest,created_at,origin) VALUES(?1,?2,?3,?4,'created')
        ON CONFLICT(label) DO UPDATE SET owner_user_id=excluded.owner_user_id,digest=excluded.digest,created_at=excluded.created_at
        WHERE employee_dir_markers.origin='created'",
        params![label, owner, digest, chrono::Utc::now().to_rfc3339()]).map_err(|_| STORE_UNAVAILABLE)?;
    Ok(())
}

/// An existing directory may be adopted for `Personal(owner)` only with a
/// marker of this flow for the same owner whose digest still matches, and
/// never while a live (not tombstoned) employee holds the label. Returns the
/// matching snapshot (its agent.toml bytes are the proposal's settings).
pub(crate) fn check_marker(db: &Connection, home: &Path, agents_dir: &Path, label: &str, owner: &str)
    -> Result<Snapshot, &'static str> {
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
    let marker: Option<(String, String, String)> = db.query_row(
        "SELECT owner_user_id,digest,origin FROM employee_dir_markers WHERE label=?1", [label],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(|_| STORE_UNAVAILABLE)?;
    let Some((marked_owner, digest, origin)) = marker else { return Err(FOREIGN_DIR) };
    // F6g (R8): the normal proposal flow never adopts an `adopted` marker;
    // only the adoption apply step creates one, from its own quarantine row.
    if origin != "created" { return Err(FOREIGN_DIR); }
    if marked_owner != owner { return Err(FOREIGN_DIR); }
    let current = snapshot(home, agents_dir, label)?;
    if current.digest != digest { return Err(FOREIGN_DIR); }
    Ok(current)
}

/// [`check_marker`] on its own apply.db connection.
pub(super) fn adopt_existing(home: &Path, agents_dir: &Path, label: &str, owner: &str) -> Result<Snapshot, &'static str> {
    let db = open_requests(home)?;
    check_marker(&db, home, agents_dir, label, owner)
}
