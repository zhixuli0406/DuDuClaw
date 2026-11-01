//! One budget for the whole refetch, including all employee scopes and extras.
use crate::{Error, Result};
use serde_json::Value;
use std::io::{self, Write};

pub const MAX_SNAPSHOT_ITEMS: usize = 2_000;
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct SnapshotBudget {
    items: usize,
    bytes: usize,
}
impl SnapshotBudget {
    pub fn ensure_items(&self, count: u64) -> Result<()> {
        if count > MAX_SNAPSHOT_ITEMS.saturating_sub(self.items) as u64 {
            return Err(Error::SnapshotBudget);
        }
        Ok(())
    }
    pub fn charge(&mut self, response: &Value, field: &str) -> Result<()> {
        let rows = response
            .get(field)
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        self.ensure_items(rows.len() as u64)?;
        // Count the entire encoded response (including fields we do not use)
        // without allocating a second JSON buffer. Wire size bounds content;
        // decoded nodes/typed rows/detail copies have additional bounded overhead.
        serde_json::to_writer(&mut *self, response).map_err(|_| Error::SnapshotBudget)?;
        self.items += rows.len();
        Ok(())
    }
}
impl Write for SnapshotBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_SNAPSHOT_BYTES.saturating_sub(self.bytes) {
            return Err(io::Error::other("snapshot budget exceeded"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn item_and_byte_budgets_are_global_and_accept_exact_limit() {
        let mut budget = SnapshotBudget::default();
        budget
            .charge(&json!({"agents":vec![json!({}); 1000]}), "agents")
            .unwrap();
        budget
            .charge(&json!({"tasks":vec![json!({}); 1000]}), "tasks")
            .unwrap();
        assert_eq!(budget.ensure_items(1), Err(Error::SnapshotBudget));
        let mut budget = SnapshotBudget::default();
        budget.write_all(&vec![b'x'; MAX_SNAPSHOT_BYTES]).unwrap();
        assert!(budget.write_all(b"x").is_err());
        let mut budget = SnapshotBudget::default();
        for _ in 0..4 {
            budget
                .charge(&json!({"tasks":[], "extra":"x".repeat(900_000)}), "tasks")
                .unwrap();
        }
        assert_eq!(
            budget.charge(&json!({"tasks":[], "extra":"x".repeat(900_000)}), "tasks"),
            Err(Error::SnapshotBudget)
        );
    }
}
