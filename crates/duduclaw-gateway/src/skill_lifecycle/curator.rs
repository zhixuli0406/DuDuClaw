//! Startup recovery for the removed automatic curator.
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

async fn restore_record(home: &Path, store: &CustomSkillStore, rec: &SkillCurationRecord) -> Result<PathBuf, String> {
    let archived = PathBuf::from(rec.archived_path.as_deref().ok_or("archived skill has no archive path")?);
    let dest = super::isolated::restore_archived(home, &rec.scope, &archived)?;
    store.curation_set_status(&rec.skill_name, &rec.scope, CurationStatus::Active, None,
        &Utc::now().to_rfc3339()).await?;
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

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn startup_restore_requires_lock_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        restore_on_startup(home.path()).await;
        assert!(home.path().join("skills-archive/global/notes.md").exists());
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let archive_bytes = std::fs::read(home.path().join("skills-archive/global/notes.md")).unwrap();
        restore_on_startup(home.path()).await;
        restore_on_startup(home.path()).await;
        assert!(home.path().join("skills/notes.md").is_file());
        assert_eq!(std::fs::read(home.path().join("skills-archive/global/notes.md")).unwrap(), archive_bytes);
        assert_eq!(std::fs::read_dir(home.path().join("skills")).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(home.path().join("skills-archive/global")).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(home.path().join("skills")).unwrap().filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".restore-")).count(), 0);
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
        // Both modes use the same exact-path validation.
        assert!(super::super::isolated::restore_archived(home.path(), SCOPE_GLOBAL, &outside).is_err());
        assert!(restore_record(home.path(), &store, &rec).await.is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside archive");
        assert!(!home.path().join("skills/notes.md").exists());
        assert_eq!(store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap().status, CurationStatus::Archived);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scope_path_mismatch_preserves_files_and_record() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        let mut rec = store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap();
        let alice = home.path().join("skills-archive/agent-alice/notes.md");
        let bob = home.path().join("skills-archive/agent-bob/notes.md");
        for path in [&alice, &bob] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, path.as_os_str().as_encoded_bytes()).unwrap();
        }
        rec.scope = "agent:bob".into();
        rec.archived_path = Some(alice.to_string_lossy().into_owned());
        store.curation_put(&rec).await.unwrap();
        assert!(restore_record(home.path(), &store, &rec).await.is_err());
        for path in [&alice, &bob] {
            assert_eq!(std::fs::read(path).unwrap(), path.as_os_str().as_encoded_bytes());
        }
        assert_eq!(store.curation_get("notes", "agent:bob").await.unwrap().unwrap().status, CurationStatus::Archived);
        assert!(!home.path().join("agents/bob/SKILLS/notes.md").exists());
    }

    #[cfg(not(unix))]
    #[tokio::test(flavor = "current_thread")]
    async fn unsupported_restore_preserves_archive_and_record() {
        let home = tempfile::tempdir().unwrap();
        let store = seed(home.path(), true).await;
        let archived = home.path().join("skills-archive/global/notes.md");
        let bytes = std::fs::read(&archived).unwrap();
        let rec = store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap();
        assert!(restore_record(home.path(), &store, &rec).await.is_err());
        assert_eq!(std::fs::read(archived).unwrap(), bytes);
        assert!(!home.path().join("skills/notes.md").exists());
        assert_eq!(store.curation_get("notes", SCOPE_GLOBAL).await.unwrap().unwrap().status, CurationStatus::Archived);
    }

}
