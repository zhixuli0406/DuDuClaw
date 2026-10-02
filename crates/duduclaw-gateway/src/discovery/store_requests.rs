//! Private frozen requests. Public tasks and approval payloads carry only a digest.
use super::{DiscoveryStore, StoreError};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

impl DiscoveryStore {
    pub fn save_frozen_request(&self, task_id: &str, run_id: &str, json: &str) -> Result<String, StoreError> {
        if !valid_id(task_id) || !valid_id(run_id) || json.len() > 2 * 1024 * 1024 {
            return Err(StoreError::Corrupt("invalid private request identity or size".into()));
        }
        let _: serde_json::Value = serde_json::from_str(json)?;
        let digest = format!("{:x}", Sha256::digest(json.as_bytes()));
        self.conn.execute("INSERT INTO discovery_requests(task_id,run_id,sha256,frozen_json) VALUES(?1,?2,?3,?4)",
            params![task_id, run_id, digest, json])?;
        Ok(digest)
    }
    pub fn load_frozen_request(&self, task_id: &str, run_id: &str, expected_sha256: &str) -> Result<String, StoreError> {
        if !valid_id(task_id) || !valid_id(run_id) || expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(StoreError::Corrupt("invalid private request identity or digest".into()));
        }
        let row: Option<(String, String)> = self.conn.query_row(
            "SELECT frozen_json,sha256 FROM discovery_requests WHERE task_id=?1 AND run_id=?2",
            params![task_id, run_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let (json, stored) = row.ok_or_else(|| StoreError::Corrupt("private request is missing".into()))?;
        if json.len() > 2 * 1024 * 1024 || stored != expected_sha256
            || format!("{:x}", Sha256::digest(json.as_bytes())) != expected_sha256 {
            return Err(StoreError::Corrupt("private request digest mismatch".into()));
        }
        let _: serde_json::Value = serde_json::from_str(&json)?;
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_request_survives_reopen_and_cannot_be_rebound_or_loaded_with_a_foreign_digest() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let source = r#"{"source":"private-policy-canary","budget":{"calls":1}}"#;
        let digest = store.save_frozen_request("task-1", "run-1", source).unwrap();
        drop(store);
        let store = DiscoveryStore::open(home.path()).unwrap();
        assert_eq!(store.load_frozen_request("task-1", "run-1", &digest).unwrap(), source);
        assert!(store.save_frozen_request("task-1", "foreign-run", source).is_err());
        assert!(store.save_frozen_request("foreign-task", "run-1", source).is_err());
        assert!(store.load_frozen_request("task-1", "foreign-run", &digest).is_err());
        assert!(store.load_frozen_request("task-1", "run-1", &"f".repeat(64)).is_err());
        store.conn.execute("UPDATE discovery_requests SET frozen_json='changed' WHERE task_id='task-1'", []).unwrap();
        assert!(store.load_frozen_request("task-1", "run-1", &digest).is_err());
    }
}
