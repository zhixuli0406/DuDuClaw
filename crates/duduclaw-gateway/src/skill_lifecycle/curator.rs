//! Compatibility tools and startup recovery for the removed automatic curator.
use std::path::{Path, PathBuf};
use std::sync::Once;
use chrono::Utc;
use tracing::{info, warn};
use crate::custom_skills::{CurationStatus, CustomSkillStore, SkillCurationRecord};

pub const SCOPE_GLOBAL: &str = "global";

/// Legacy settings remain valid TOML, but no longer control any lifecycle.
pub fn warn_ignored_config(text: &str) {
    static WARN: Once = Once::new();
    if text.parse::<toml::Table>().ok().is_some_and(|table| table.contains_key("skill_curator")) {
        WARN.call_once(|| warn!("[skill_curator] has no effect: automatic skill staleness and archiving were removed"));
    }
}

pub fn owned_scope(agent: &str) -> Result<String, String> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
        || agent.starts_with('_') || !is_safe_component(agent) {
        return Err("技能維護的員工身分無效".into());
    }
    Ok(format!("agent:{agent}"))
}

/// Skills dir for a scope string (`"global"`, `"department:<dept>"`, or
/// `"agent:<id>"`). Returns None for malformed scopes (fail-safe: no file
/// action).
fn scope_dir(home_dir: &Path, scope: &str) -> Option<PathBuf> {
    if scope == SCOPE_GLOBAL {
        return Some(home_dir.join("skills"));
    }
    if let Some(dept) = scope.strip_prefix("department:") {
        // WP7: department layer at shared/skills/departments/<dept>.
        if !duduclaw_core::is_valid_department(dept) {
            return None;
        }
        return Some(duduclaw_agent::skill_loader::department_skills_dir(
            home_dir, dept,
        ));
    }
    let id = scope.strip_prefix("agent:")?;
    if id.is_empty() || !is_safe_component(id) {
        return None;
    }
    Some(home_dir.join("agents").join(id).join("SKILLS"))
}

/// Directory-safe name check (mirrors the MCP-side validator: no traversal).
fn is_safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
        && s != "."
        && s != ".."
}

/// Move without replacing even if a destination appears after the precheck.
fn restore_move(source: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = CString::new(source.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let dest = CString::new(dest.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        #[cfg(target_os = "linux")]
        return duduclaw_core::linux_syscalls::renameat2(libc::AT_FDCWD, &source,
            libc::AT_FDCWD, &dest, libc::RENAME_NOREPLACE).map_err(|e| e.to_string());
        #[cfg(target_os = "macos")]
        {
            // SAFETY: both paths are live NUL-terminated C strings.
            if unsafe { libc::renamex_np(source.as_ptr(), dest.as_ptr(), libc::RENAME_EXCL) } == 0 {
                return Ok(());
            }
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    // Windows rename refuses an existing destination.
    #[cfg(target_os = "windows")]
    { std::fs::rename(source, dest).map_err(|e| e.to_string()) }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { let _ = (source, dest); Err("no safe no-overwrite restore on this platform".into()) }
}

async fn restore_record(home: &Path, store: &CustomSkillStore, rec: &SkillCurationRecord) -> Result<PathBuf, String> {
    let archived = PathBuf::from(rec.archived_path.as_deref().ok_or("archived skill has no archive path")?);
    let dest = if duduclaw_parse_worker::client::required() {
        super::isolated::restore_archived(home, &rec.scope, &archived)?
    } else {
        restore_unprotected(home, &rec.scope, &archived)?
    };
    store.curation_set_status(&rec.skill_name, &rec.scope, CurationStatus::Active, None,
        &Utc::now().to_rfc3339()).await?;
    Ok(dest)
}

fn restore_unprotected(home: &Path, scope: &str, archived: &Path) -> Result<PathBuf, String> {
    let filename = archived.file_name().and_then(|name| name.to_str())
        .filter(|name| super::isolated::component(name)).ok_or("invalid archive filename")?;
    let archive_root = home.join("skills-archive").canonicalize().map_err(|e| format!("archive root: {e}"))?;
    let parent = archived.parent().ok_or("archive path has no parent")?
        .canonicalize().map_err(|e| format!("archive parent: {e}"))?;
    if !parent.starts_with(&archive_root) {
        return Err("archive path is outside skills-archive".into());
    }
    let source = parent.join(filename);
    if !source.symlink_metadata().map_err(|e| format!("archive source: {e}"))?.is_file() {
        return Err("archive source is not a regular file".into());
    }
    let dir = scope_dir(home, scope).ok_or("malformed scope")?;
    let dest = dir.join(filename);
    if dest.symlink_metadata().is_ok() {
        return Err(format!("restore destination already exists: {}", dest.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("restore dir: {e}"))?;
    restore_move(&source, &dest).map_err(|e| format!("restore move: {e}"))?;
    Ok(dest)
}

/// One startup pass, exclusively in the process holding the gateway lock.
/// Successful rows become Active; conflicts remain available for manual recovery.
pub async fn restore_on_startup(home: &Path) {
    if !duduclaw_core::gateway_instance::held(home) { return; }
    if let Ok(text) = std::fs::read_to_string(home.join("config.toml")) { warn_ignored_config(&text); }
    let result = async {
        let store = CustomSkillStore::open(home)?;
        for rec in store.curation_list().await? {
            if !duduclaw_core::gateway_instance::held(home) { break; }
            if rec.status != CurationStatus::Archived { continue; }
            match restore_record(home, &store, &rec).await {
                Ok(dest) => info!(skill = %rec.skill_name, scope = %rec.scope, path = %dest.display(), "archived skill restored"),
                Err(error) => warn!(skill = %rec.skill_name, scope = %rec.scope, %error, "skill archive restore skipped"),
            }
        }
        Ok::<(), String>(())
    }.await;
    if let Err(error) = result { warn!(%error, "skill archive startup restore failed"); }
}

/// Deprecated compatibility call: only an explicit archived-skill restore acts.
pub async fn set_pin(home: &Path, store: &CustomSkillStore, skill_name: &str, scope: &str, pinned: bool) -> Result<String, String> {
    if pinned {
        if let Some(rec) = store.curation_get(skill_name, scope).await? {
            if rec.status == CurationStatus::Archived {
                let dest = restore_record(home, store, &rec).await?;
                return Ok(format!("Skill '{skill_name}' [{scope}] restored to {}. skill_pin is deprecated (removal v1.86.0); pinning has no effect.", dest.display()));
            }
        }
    }
    Ok("skill_pin is deprecated (removal v1.86.0); pinning has no effect because automatic skill archiving was removed.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed(home: &Path, archived: bool) -> CustomSkillStore {
        let store = CustomSkillStore::open(home).unwrap();
        let old = (Utc::now() - chrono::Duration::days(400)).to_rfc3339();
        let path = home.join(if archived { "skills-archive/global/notes.md" } else { "skills/notes.md" });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "---\nname: notes\ndescription: Test skill\n---\nOriginal skill").unwrap();
        store.curation_put(&SkillCurationRecord {
            skill_name: "notes".into(), scope: SCOPE_GLOBAL.into(), first_seen: old.clone(),
            last_used_at: Some(old.clone()), pinned: false,
            status: if archived { CurationStatus::Archived } else { CurationStatus::Active },
            archived_path: archived.then(|| path.to_string_lossy().into_owned()), updated_at: old,
        }).await.unwrap();
        store
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unused_400_days_stays_active_and_loadable_despite_legacy_config() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), false).await;
        std::fs::write(home.path().join("config.toml"), "[skill_curator]\nenabled=true\narchive_days=90\n").unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        restore_on_startup(home.path()).await;
        assert_eq!(store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap().status, CurationStatus::Active);
        assert!(duduclaw_agent::skill_loader::parse_skill_file(&home.path().join("skills/notes.md")).is_ok());
        assert!(!home.path().join("skills-archive").exists());
        assert!(!home.path().join("shared/wiki/reports").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_restore_requires_lock_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        restore_on_startup(home.path()).await;
        assert!(home.path().join("skills-archive/global/notes.md").exists());
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        restore_on_startup(home.path()).await;
        restore_on_startup(home.path()).await;
        assert!(home.path().join("skills/notes.md").is_file());
        assert!(!home.path().join("skills-archive/global/notes.md").exists());
        let rec = store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap();
        assert_eq!(rec.status, CurationStatus::Active);
        assert!(rec.archived_path.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_conflict_preserves_both_files_and_archived_status() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        let archived = home.path().join("skills-archive/global/notes.md");
        let original = std::fs::read(&archived).unwrap();
        std::fs::create_dir_all(home.path().join("skills")).unwrap();
        std::fs::write(home.path().join("skills/notes.md"), "Existing skill").unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        restore_on_startup(home.path()).await;
        assert_eq!(std::fs::read(&archived).unwrap(), original);
        assert_eq!(std::fs::read_to_string(home.path().join("skills/notes.md")).unwrap(), "Existing skill");
        assert_eq!(store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap().status, CurationStatus::Archived);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn archive_path_outside_archive_root_is_skipped_and_preserved() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        let outside = home.path().join("skills-archive-other/notes.md");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, b"outside archive").unwrap();
        let mut rec = store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap();
        rec.archived_path = Some(outside.to_string_lossy().into_owned());
        store.curation_put(&rec).await.unwrap();
        // Exercise the unprotected validation regardless of the process mode.
        assert!(restore_unprotected(home.path(), SCOPE_GLOBAL, &outside).is_err());
        assert!(restore_record(home.path(), &store, &rec).await.is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside archive");
        assert!(!home.path().join("skills/notes.md").exists());
        assert_eq!(store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap().status, CurationStatus::Archived);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pin_only_restores_archived_skills() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        assert!(set_pin(home.path(), &store, "notes", SCOPE_GLOBAL, false).await.unwrap().contains("no effect"));
        assert!(home.path().join("skills-archive/global/notes.md").exists());
        assert!(set_pin(home.path(), &store, "notes", SCOPE_GLOBAL, true).await.unwrap().contains("restored"));
        assert!(!store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap().pinned);
    }
}
