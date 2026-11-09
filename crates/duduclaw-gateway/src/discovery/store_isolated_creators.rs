//! FD-SEL: protected creator bindings. A discovery request filed by a protected
//! employee (`TrustedCaller::from_isolated_agent`) records the exact employee
//! id and incarnation it was filed as; protected reads and cancellation accept
//! only that very identity. The label in `tasks.created_by` alone is not
//! authority in protected mode: a recreated or restored label is another
//! creator.
use super::{DiscoveryStore, StoreError};
use rusqlite::{params, OptionalExtension};

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn employee_hex(employee: &[u8; 16]) -> String { employee.iter().map(|byte| format!("{byte:02x}")).collect() }

impl DiscoveryStore {
    /// Record the creator of `task_id`/`run_id` once. A second binding for the
    /// same task or run (any identity) is refused, never replaced.
    pub fn save_isolated_creator(&self, task_id: &str, run_id: &str, employee: &[u8; 16], incarnation: u32)
        -> Result<(), StoreError> {
        if !valid_id(task_id) || !valid_id(run_id) {
            return Err(StoreError::Corrupt("invalid protected creator identity".into()));
        }
        self.conn.execute("INSERT INTO discovery_isolated_creators(task_id,run_id,employee,incarnation) VALUES(?1,?2,?3,?4)",
            params![task_id, run_id, employee_hex(employee), i64::from(incarnation)])?;
        Ok(())
    }

    /// Whether `task_id`/`run_id` was filed by exactly this protected creator.
    /// No row (an ordinary or pre-FD-SEL request), another employee id or
    /// another incarnation are all `false`.
    pub fn isolated_creator_matches(&self, task_id: &str, run_id: &str, employee: &[u8; 16], incarnation: u32)
        -> Result<bool, StoreError> {
        if !valid_id(task_id) || !valid_id(run_id) { return Ok(false); }
        let row: Option<(String, i64)> = self.conn.query_row(
            "SELECT employee,incarnation FROM discovery_isolated_creators WHERE task_id=?1 AND run_id=?2",
            params![task_id, run_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        Ok(row.is_some_and(|(stored, stored_incarnation)|
            stored == employee_hex(employee) && stored_incarnation == i64::from(incarnation)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn fdsel_creator_binding_is_exact_and_never_rebound() {
        let home = crate::discovery::private_test_home();
        let store = DiscoveryStore::open(home.path()).unwrap();
        store.save_isolated_creator("task-1", "run-1", &[2; 16], 0).unwrap();
        assert!(store.isolated_creator_matches("task-1", "run-1", &[2; 16], 0).unwrap());
        // Another employee id, another incarnation, another task or run.
        assert!(!store.isolated_creator_matches("task-1", "run-1", &[9; 16], 0).unwrap());
        assert!(!store.isolated_creator_matches("task-1", "run-1", &[2; 16], 1).unwrap());
        assert!(!store.isolated_creator_matches("task-1", "run-2", &[2; 16], 0).unwrap());
        assert!(!store.isolated_creator_matches("task-2", "run-1", &[2; 16], 0).unwrap());
        assert!(!store.isolated_creator_matches("../task-1", "run-1", &[2; 16], 0).unwrap());
        // Never rebound to another creator, for the same task or the same run.
        assert!(store.save_isolated_creator("task-1", "run-1", &[9; 16], 0).is_err());
        assert!(store.save_isolated_creator("task-1", "run-9", &[2; 16], 1).is_err());
        assert!(store.save_isolated_creator("task-9", "run-1", &[2; 16], 1).is_err());
        assert!(store.save_isolated_creator("bad id", "run-3", &[2; 16], 0).is_err());
        drop(store);
        let store = DiscoveryStore::open(home.path()).unwrap();
        assert!(store.isolated_creator_matches("task-1", "run-1", &[2; 16], 0).unwrap());
    }
}
