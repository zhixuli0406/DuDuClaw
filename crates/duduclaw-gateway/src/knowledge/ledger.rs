//! One citation numbering per live turn, keyed `(answering employee, turn
//! id)`.
//!
//! Passages put into the prompt automatically and passages returned by
//! `knowledge_search` during the same turn share one sequence `[1]`, `[2]`, …;
//! the same chunk keeps its number. The ledger lives in gateway memory only
//! for as long as its [`LedgerGuard`] (the channel reply) does; after the
//! reply it is turned into `turn_citations` rows. Outside a live turn (a
//! dispatched run, an unknown turn id) nothing is numbered and tool results
//! use their own list order.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use duduclaw_knowledge::search::Hit;

/// Most passages numbered in one turn.
pub const MAX_ENTRIES: usize = 30;
/// Characters of passage text kept per entry (for the excerpt).
const KEPT_TEXT_CHARS: usize = 1_500;

/// One numbered passage.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerEntry {
    pub n: u32,
    pub chunk_id: i64,
    pub source_id: String,
    pub collection_id: String,
    pub title: String,
    pub locator: String,
    pub text: String,
    /// The query that surfaced it (for the excerpt).
    pub query: String,
}

type Key = (String, String);

fn registry() -> &'static Mutex<HashMap<Key, Vec<LedgerEntry>>> {
    static R: OnceLock<Mutex<HashMap<Key, Vec<LedgerEntry>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<Key, Vec<LedgerEntry>>> {
    registry().lock().unwrap_or_else(|p| p.into_inner())
}

/// Removes the ledger when dropped.
#[derive(Debug)]
pub struct LedgerGuard {
    key: Key,
}

impl Drop for LedgerGuard {
    fn drop(&mut self) {
        lock().remove(&self.key);
    }
}

/// Start the ledger of a live turn. `None` for an empty or malformed key,
/// or when a ledger for this key already exists (the first owner keeps it).
pub fn open(agent_id: &str, turn_id: &str) -> Option<LedgerGuard> {
    if !duduclaw_core::is_valid_agent_id(agent_id)
        || !crate::computer_use_sessions::turns::valid_turn_id(turn_id)
    {
        return None;
    }
    let key = (agent_id.to_string(), turn_id.to_string());
    let mut map = lock();
    if map.contains_key(&key) {
        return None;
    }
    map.insert(key.clone(), Vec::new());
    Some(LedgerGuard { key })
}

/// Number `hits` in the turn's ledger: a chunk seen before keeps its
/// number, a new one gets the next. `None` per hit when the turn has no
/// ledger or the ledger is full.
pub fn number(agent_id: &str, turn_id: &str, hits: &[Hit], query: &str) -> Vec<Option<u32>> {
    let mut map = lock();
    let Some(entries) = map.get_mut(&(agent_id.to_string(), turn_id.to_string())) else {
        return vec![None; hits.len()];
    };
    hits.iter()
        .map(|h| {
            if let Some(e) = entries.iter().find(|e| e.chunk_id == h.chunk_id) {
                return Some(e.n);
            }
            if entries.len() >= MAX_ENTRIES {
                return None;
            }
            let n = entries.len() as u32 + 1;
            entries.push(LedgerEntry {
                n,
                chunk_id: h.chunk_id,
                source_id: h.source_id.clone(),
                collection_id: h.collection_id.clone(),
                title: h.title.clone(),
                locator: h.locator_text.clone(),
                text: duduclaw_core::truncate_chars(&h.text, KEPT_TEXT_CHARS),
                query: duduclaw_core::truncate_chars(query, 500),
            });
            Some(n)
        })
        .collect()
}

/// Every entry of the turn's ledger, in number order.
pub fn entries(agent_id: &str, turn_id: &str) -> Vec<LedgerEntry> {
    lock()
        .get(&(agent_id.to_string(), turn_id.to_string()))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_knowledge::locator::{Locator, LocatorKind};

    fn hit(id: i64) -> Hit {
        Hit {
            chunk_id: id,
            source_id: format!("src_{id}"),
            collection_id: "col".into(),
            title: "t".into(),
            locator: Locator::new(LocatorKind::Page),
            locator_text: "第 1 頁".into(),
            heading_path: String::new(),
            text: "內容".into(),
            score: 1.0,
            fts_rank: Some(1),
            like_rank: None,
            vec_rank: None,
            suspect: false,
        }
    }

    #[test]
    fn shared_numbering_and_cleanup() {
        let g = open("ledger-a", "turn-1").unwrap();
        assert!(open("ledger-a", "turn-1").is_none());
        assert_eq!(
            number("ledger-a", "turn-1", &[hit(5), hit(7)], "q"),
            vec![Some(1), Some(2)]
        );
        // The same chunk from a later tool call keeps its number.
        assert_eq!(
            number("ledger-a", "turn-1", &[hit(7), hit(9)], "q2"),
            vec![Some(2), Some(3)]
        );
        // Another employee with the same turn id has no ledger.
        assert_eq!(number("ledger-b", "turn-1", &[hit(5)], "q"), vec![None]);
        assert_eq!(entries("ledger-a", "turn-1").len(), 3);
        drop(g);
        assert!(entries("ledger-a", "turn-1").is_empty());
        assert!(open("../x", "t").is_none());
    }

    #[test]
    fn full_ledger_stops_numbering() {
        let _g = open("ledger-c", "turn-2").unwrap();
        let hits: Vec<Hit> = (0..(MAX_ENTRIES as i64 + 2)).map(hit).collect();
        let ns = number("ledger-c", "turn-2", &hits, "q");
        assert_eq!(ns[MAX_ENTRIES - 1], Some(MAX_ENTRIES as u32));
        assert_eq!(ns[MAX_ENTRIES], None);
    }
}
