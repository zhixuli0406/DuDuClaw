//! Operator policy commands may run before the gateway has migrated users.db.
//! Use the normal auth initialization, never teach the policy fence to repair
//! missing metadata. Read-only commands and homes without a database stay so.
use std::path::Path;
use duduclaw_core::error::{DuDuClawError, Result};

/// Call only after the command's operator/session gate and input validation.
pub(crate) fn prepare_existing_authority(home: &Path) -> Result<()> {
    let path = home.join("users.db");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() || metadata.permissions().readonly() => {
            return Err(DuDuClawError::Agent("使用者權限資料無法安全更新。".into()));
        }
        Ok(_) => {}
    }
    duduclaw_auth::UserDb::new(&path)
        .map_err(|_| DuDuClawError::Agent("使用者權限資料初始化失敗，尚未變更政策。".into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_authority_is_not_created_by_operator_preparation() {
        let home = tempfile::tempdir().unwrap();
        prepare_existing_authority(home.path()).unwrap();
        assert!(!home.path().join("users.db").exists());
    }
    #[test]
    fn legacy_authority_initializes_before_a_standalone_policy_write() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        drop(duduclaw_auth::UserDb::new(&path).unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("DELETE FROM auth_meta WHERE key IN ('isolation_tenant_id','isolation_policy_epoch')", []).unwrap();
        assert!(duduclaw_core::isolation_policy::fence_change(home.path()).is_err());
        prepare_existing_authority(home.path()).unwrap();
        duduclaw_core::org_store::upsert(home.path(), "alice", duduclaw_core::OrgEntry::new("boss", "sales")).unwrap();
        let epoch: i64 = db.query_row("SELECT value FROM auth_meta WHERE key='isolation_policy_epoch'", [], |row| row.get(0)).unwrap();
        assert_eq!(epoch, 2);
    }
    #[test]
    fn partial_authority_is_not_repaired_by_operator_preparation() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        drop(duduclaw_auth::UserDb::new(&path).unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("DELETE FROM auth_meta WHERE key='isolation_policy_epoch'", []).unwrap();
        assert!(prepare_existing_authority(home.path()).is_err());
        assert_eq!(db.query_row("SELECT count(*) FROM auth_meta WHERE key='isolation_policy_epoch'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }
}
