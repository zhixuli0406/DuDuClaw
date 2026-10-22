//! Version history of every profile file, kept OUTSIDE the employee's
//! directory: `<home>/agent_file_history/<agent>/<file>/<unix_ms>-<sha12>.<ext>`
//! plus an `index.jsonl` appended under the cross-process lock. Outside the
//! agent directory so the employee's CLI working directory, skill scans and
//! fork copies never see (or rewrite) it. Owner-only permissions.
//!
//! The first change of a file stores the content it replaced as an
//! `initial` version, so the very first edit can be undone too. The newest
//! `history_keep` versions are kept per file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::FileKey;
use super::verdict::Verdict;

/// One index row (newest last on disk).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VersionEntry {
    pub version_id: String,
    pub file: String,
    pub at: String,
    pub user_id: String,
    pub source: String,
    pub sha256: String,
    pub size: usize,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub reason_categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
}

/// Who and what produced a version.
#[derive(Debug, Clone, Default)]
pub(crate) struct VersionMeta {
    pub user_id: String,
    pub source: String,
    pub verdict: Option<Verdict>,
    pub reason_categories: Vec<String>,
    pub confirm_reason: Option<String>,
    pub approval_id: Option<String>,
    pub decided_by: Option<String>,
}

fn file_dir(home: &Path, agent_id: &str, key: FileKey) -> PathBuf {
    home.join("agent_file_history").join(agent_id).join(key.as_str())
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create the version folder: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let root = dir.ancestors().find(|p| p.file_name().is_some_and(|n| n == "agent_file_history"));
        for p in dir.ancestors().take_while(|p| Some(*p) != root.and_then(Path::parent)) {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
        }
    }
    Ok(())
}

/// A version id is `<unix_ms>-<12 hex>`; anything else is refused before it
/// can become a path.
pub(crate) fn is_valid_version_id(id: &str) -> bool {
    let Some((ms, sha)) = id.split_once('-') else { return false };
    !ms.is_empty()
        && ms.len() <= 20
        && ms.bytes().all(|b| b.is_ascii_digit())
        && sha.len() == 12
        && sha.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn read_index(dir: &Path) -> Vec<VersionEntry> {
    std::fs::read_to_string(dir.join("index.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<VersionEntry>(l).ok())
        .collect()
}

/// Store `content` as a new version. Returns its id.
pub(crate) fn record(
    home: &Path,
    agent_id: &str,
    key: FileKey,
    content: &str,
    meta: &VersionMeta,
    keep: usize,
) -> Result<String, String> {
    let dir = file_dir(home, agent_id, key);
    ensure_private_dir(&dir)?;
    let sha = super::hash_of(content);
    let now = chrono::Utc::now();
    let id = format!("{}-{}", now.timestamp_millis(), duduclaw_core::truncate_bytes(&sha, 12));
    let body = dir.join(format!("{id}.{}", key.ext()));
    if !body.exists() {
        crate::handlers::config_commit::write_owner_only(&body, content)
            .map_err(|e| format!("cannot store the version: {e}"))?;
    }
    let entry = VersionEntry {
        version_id: id.clone(),
        file: key.as_str().to_string(),
        at: now.to_rfc3339(),
        user_id: meta.user_id.clone(),
        source: meta.source.clone(),
        sha256: sha,
        size: content.len(),
        verdict: meta.verdict.map(|v| v.as_str().to_string()),
        reason_categories: meta.reason_categories.clone(),
        confirm_reason: meta.confirm_reason.clone(),
        approval_id: meta.approval_id.clone(),
        decided_by: meta.decided_by.clone(),
    };
    let index = dir.join("index.jsonl");
    let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
    duduclaw_core::with_file_lock(&index, || {
        let mut rows = read_index(&dir);
        rows.push(entry.clone());
        let keep = keep.max(1);
        let dropped: Vec<VersionEntry> = if rows.len() > keep { rows.drain(..rows.len() - keep).collect() } else { Vec::new() };
        if dropped.is_empty() {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&index)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o600));
            }
            return writeln!(f, "{line}");
        }
        let text: String = rows.iter().filter_map(|r| serde_json::to_string(r).ok()).map(|l| l + "\n").collect();
        crate::handlers::config_commit::replace_owner_only(&index, &text)?;
        for old in dropped {
            if !rows.iter().any(|r| r.version_id == old.version_id) {
                let _ = std::fs::remove_file(dir.join(format!("{}.{}", old.version_id, key.ext())));
            }
        }
        Ok(())
    })
    .map_err(|e| format!("cannot update the version index: {e}"))?;
    Ok(id)
}

/// Before the first recorded change, keep what the file contained as an
/// `initial` version (nothing when the file was empty or absent).
pub(crate) fn ensure_initial(home: &Path, agent_id: &str, key: FileKey, before: &str, keep: usize) -> Result<(), String> {
    if before.is_empty() || !read_index(&file_dir(home, agent_id, key)).is_empty() {
        return Ok(());
    }
    let meta = VersionMeta { user_id: "system".into(), source: "initial".into(), ..Default::default() };
    record(home, agent_id, key, before, &meta, keep).map(|_| ())
}

/// Move an employee's whole history to `agent_file_history/_trash/<id>-<unix
/// ms>` (soft delete keeps the data, as the rest of the employee's folder is
/// kept; a new employee that later gets the same id starts with no history).
/// `Ok(None)` when there was nothing to move.
pub(crate) fn retire(home: &Path, agent_id: &str) -> Result<Option<PathBuf>, String> {
    let root = home.join("agent_file_history");
    let dir = root.join(agent_id);
    if !dir.is_dir() {
        return Ok(None);
    }
    let trash = root.join("_trash");
    ensure_private_dir(&trash)?;
    let dest = trash.join(format!("{agent_id}-{}", chrono::Utc::now().timestamp_millis()));
    std::fs::rename(&dir, &dest).map_err(|e| format!("cannot move the version history: {e}"))?;
    Ok(Some(dest))
}

/// Every kept version, newest first.
pub(crate) fn list(home: &Path, agent_id: &str, key: FileKey) -> Vec<VersionEntry> {
    let mut rows = read_index(&file_dir(home, agent_id, key));
    rows.reverse();
    rows
}

/// The newest version, if any.
pub(crate) fn last(home: &Path, agent_id: &str, key: FileKey) -> Option<VersionEntry> {
    read_index(&file_dir(home, agent_id, key)).pop()
}

/// The newest version whose content hashes to `sha256`.
pub(crate) fn find_by_sha(home: &Path, agent_id: &str, key: FileKey, sha256: &str) -> Option<String> {
    list(home, agent_id, key).into_iter().find(|v| v.sha256 == sha256).map(|v| v.version_id)
}

/// The content of one kept version.
pub(crate) fn read_version(home: &Path, agent_id: &str, key: FileKey, version_id: &str) -> Result<String, String> {
    if !is_valid_version_id(version_id) {
        return Err("invalid version id".into());
    }
    let dir = file_dir(home, agent_id, key);
    if !read_index(&dir).iter().any(|v| v.version_id == version_id) {
        return Err("this version is no longer kept".into());
    }
    std::fs::read_to_string(dir.join(format!("{version_id}.{}", key.ext())))
        .map_err(|_| "this version is no longer kept".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(source: &str) -> VersionMeta {
        VersionMeta { user_id: "u1".into(), source: source.into(), ..Default::default() }
    }

    #[test]
    fn records_lists_reads_and_prunes() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        ensure_initial(h, "a1", FileKey::Soul, "", 5).unwrap();
        assert!(list(h, "a1", FileKey::Soul).is_empty(), "nothing to keep for an empty file");
        ensure_initial(h, "a1", FileKey::Soul, "v0", 5).unwrap();
        let mut ids = Vec::new();
        for i in 1..=6 {
            ids.push(record(h, "a1", FileKey::Soul, &format!("v{i}"), &meta("dashboard"), 5).unwrap());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        ensure_initial(h, "a1", FileKey::Soul, "again", 5).unwrap();
        let rows = list(h, "a1", FileKey::Soul);
        assert_eq!(rows.len(), 5, "keeps the newest five");
        assert_eq!(rows[0].version_id, ids[5]);
        assert!(rows.iter().all(|r| r.source == "dashboard"), "initial pruned, not re-added");
        assert_eq!(read_version(h, "a1", FileKey::Soul, &ids[5]).unwrap(), "v6");
        assert!(read_version(h, "a1", FileKey::Soul, &ids[0]).is_err(), "pruned");
        assert!(read_version(h, "a1", FileKey::Soul, "../../etc").is_err());
        assert_eq!(find_by_sha(h, "a1", FileKey::Soul, &super::super::hash_of("v4")), Some(ids[3].clone()));
        let dir = file_dir(h, "a1", FileKey::Soul);
        let bodies = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "md"))
            .count();
        assert_eq!(bodies, 5, "pruned version files are deleted");
        assert!(!h.join("agents").exists(), "nothing inside the agent directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let body = dir.join(format!("{}.md", ids[5]));
            assert_eq!(std::fs::metadata(body).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn retired_history_is_not_seen_by_a_new_employee_with_the_same_id() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        record(h, "a1", FileKey::Soul, "old employee", &meta("dashboard"), 5).unwrap();
        let moved = retire(h, "a1").unwrap().unwrap();
        assert!(moved.starts_with(h.join("agent_file_history/_trash")));
        assert!(list(h, "a1", FileKey::Soul).is_empty());
        assert!(retire(h, "a1").unwrap().is_none());
    }

    #[test]
    fn version_ids_are_strict() {
        assert!(is_valid_version_id("1730000000000-0123456789ab"));
        for bad in ["", "1-0123", "x-0123456789ab", "1-0123456789AB", "1-0123456789ab/..", "-0123456789ab"] {
            assert!(!is_valid_version_id(bad), "{bad}");
        }
    }
}
