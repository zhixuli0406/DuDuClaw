//! Owner credential (review M-5): a workspace belongs to the employee
//! *instance* that created it, not just to a name.
//!
//! At creation a random credential is written twice: into the registry row
//! (`owner_credential`) and into the owner's own state file
//! `<home>/agents/<owner>/state/computer_workspaces.json` (`{id: credential}`).
//! Every attach, read, write and list compares the two. A missing or
//! different value means the employee of that name is not the one that
//! created the workspace (removed and re-created, or its directory replaced),
//! so the workspace is treated as ownerless (fail closed) and needs an
//! operator. A row without a credential (created before this check) also
//! fails closed.
//!
//! Known limit: the state file lives in the employee's own directory, which
//! that employee can read and write; an employee with `Read` can also read a
//! removed predecessor's file under `agents/_trash/`. The check stops a
//! re-created employee that only uses the product tools, not one with
//! unrestricted file access.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use super::store::WorkspaceRow;

/// File name under the owner's `state/` directory.
pub const STATE_FILE: &str = "computer_workspaces.json";

/// A fresh random credential (64 hex digits).
pub fn new_credential() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().as_simple(),
        uuid::Uuid::new_v4().as_simple()
    )
}

fn state_path(home: &Path, owner: &str) -> Option<PathBuf> {
    duduclaw_core::is_valid_agent_id(owner).then(|| {
        home.join("agents")
            .join(owner)
            .join("state")
            .join(STATE_FILE)
    })
}

fn read_map(path: &Path) -> io::Result<BTreeMap<String, String>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
        Ok(m) if !m.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "owner state is not a regular file",
            ));
        }
        Ok(m) if m.len() > 256 * 1024 => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "owner state too large",
            ));
        }
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_map(path: &Path, map: &BTreeMap<String, String>) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{STATE_FILE}.tmp-{}",
        uuid::Uuid::new_v4().as_simple()
    ));
    let body = serde_json::to_vec_pretty(map).map_err(io::Error::other)?;
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Record `credential` for workspace `id` in `owner`'s state file.
pub fn record(home: &Path, owner: &str, id: &str, credential: &str) -> io::Result<()> {
    let path = state_path(home, owner).ok_or_else(|| io::Error::other("invalid owner"))?;
    duduclaw_core::with_file_lock(&path, || {
        let mut map = read_map(&path)?;
        map.insert(id.to_string(), credential.to_string());
        write_map(&path, &map)
    })
}

/// Drop workspace `id` from `owner`'s state file (best effort, on delete).
pub fn forget(home: &Path, owner: &str, id: &str) {
    let Some(path) = state_path(home, owner) else {
        return;
    };
    let _ = duduclaw_core::with_file_lock(&path, || {
        let mut map = read_map(&path)?;
        if map.remove(id).is_some() {
            write_map(&path, &map)?;
        }
        Ok(())
    });
}

/// Whether the employee now named `row.owner_agent_id` holds this
/// workspace's credential. Anything else (no credential on the row, no or
/// an unreadable state file, a different value) is `false`.
pub fn matches(home: &Path, row: &WorkspaceRow) -> bool {
    let Some(expected) = row.owner_credential.as_deref().filter(|c| !c.is_empty()) else {
        return false;
    };
    let Some(path) = state_path(home, &row.owner_agent_id) else {
        return false;
    };
    match read_map(&path) {
        Ok(map) => map
            .get(&row.workspace_id)
            .is_some_and(|got| constant_time_eq(got.as_bytes(), expected.as_bytes())),
        Err(_) => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
