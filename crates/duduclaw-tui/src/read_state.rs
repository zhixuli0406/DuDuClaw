//! Client-local read receipts. Only opaque IDs are persisted, never text or tokens.
use crate::{Error, Result, strings};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, BTreeMap},
    io::{Read, Write},
    path::Path,
};

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadState {
    ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    snoozes: BTreeMap<String, crate::snooze::Snooze>,
}
impl ReadState {
    pub fn snooze(&mut self, item: &crate::state::InboxItem, now: i64) -> bool {
        if let Some(snooze) = crate::snooze::Snooze::new(item, now, 600) {
            self.snoozes.insert(item.key(), snooze);
            true
        } else { false }
    }
    pub fn hidden(&self, item: &crate::state::InboxItem, now: i64) -> bool {
        self.snoozes.get(&item.key()).is_some_and(|s| s.hidden(item, now))
    }
    pub fn resurfaced(&self, item: &crate::state::InboxItem, now: i64) -> bool {
        self.snoozes.contains_key(&item.key()) && !self.hidden(item, now)
    }
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }
    pub fn mark(&mut self, id: String) {
        self.ids.insert(id);
    }
    pub fn load(home: &Path) -> (Self, Option<&'static str>) {
        let file = match crate::local_file::open_regular(&home.join("tui-state.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(_) => return (Self::default(), Some(strings::READ_STATE_NOTICE)),
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.len() as u64 <= MAX_FILE_BYTES
            && let Ok(state) = serde_json::from_slice(&bytes)
        {
            return (state, None);
        }
        (Self::default(), Some(strings::READ_STATE_NOTICE))
    }
    pub fn save(&self, home: &Path) -> Result<()> {
        // Same-directory tempfile + fsync + atomic replacement (also on Windows).
        std::fs::create_dir_all(home).map_err(|_| Error::LocalState)?;
        let mut temp = tempfile::NamedTempFile::new_in(home).map_err(|_| Error::LocalState)?;
        let bytes = serde_json::to_vec(self).map_err(|_| Error::LocalState)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::LocalState);
        }
        temp.write_all(&bytes).map_err(|_| Error::LocalState)?;
        temp.as_file().sync_all().map_err(|_| Error::LocalState)?;
        temp.persist(home.join("tui-state.json"))
            .map_err(|_| Error::LocalState)?;
        #[cfg(unix)]
        std::fs::File::open(home)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::LocalState)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_read_state_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let mut bytes = b"{\"ids\":[\"secret\"]}".to_vec();
        bytes.resize(MAX_FILE_BYTES as usize + 1, b' ');
        std::fs::write(home.path().join("tui-state.json"), bytes).unwrap();
        let (state, notice) = ReadState::load(home.path());
        assert!(!state.contains("secret"));
        assert!(notice.is_some());
    }
    #[test]
    fn corrupt_atomic_roundtrip_ids_only() {
        let home = tempfile::tempdir().unwrap();
        assert!(ReadState::load(home.path()).1.is_none());
        std::fs::write(home.path().join("tui-state.json"), "{broken").unwrap();
        assert!(ReadState::load(home.path()).1.is_some());
        let mut state = ReadState::default();
        state.mark("run:123".into());
        state.save(home.path()).unwrap();
        state.mark("approval:abc".into());
        state.save(home.path()).unwrap();
        let (loaded, notice) = ReadState::load(home.path());
        assert!(notice.is_none());
        assert!(loaded.contains("run:123"));
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join("tui-state.json")).unwrap())
                .unwrap();
        assert_eq!(value, serde_json::json!({"ids":["approval:abc","run:123"]}));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    }
}
