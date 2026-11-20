//! spawnd's publication journal `/var/lib/duduclaw-spawnd/publish.json` (plan
//! R6 addendum: `Apply` must be completable after a power loss).
//!
//! Written only by spawnd (root) around the replacement of the eight fixed root
//! components; read by spawnd at start (roll forward / roll back) and by the
//! updater's generation pruning (`linux::service::cleanup_generations`), which
//! must keep every generation the journal names.
//!
//! The format is fixed across versions: a spawnd of either side of an update
//! reads the journal the other wrote. `schema` stays [`JOURNAL_SCHEMA`] for as
//! long as a release that reads only this schema can be on the host; a reader
//! refuses another schema, an unknown or duplicate field, a non-canonical
//! encoding and an oversized file (the caller then stays not ready, it never
//! ignores the journal).
use duduclaw_isolation::ids::Sha256Digest;
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

pub const JOURNAL_DIRECTORY: &str = "/var/lib/duduclaw-spawnd";
pub const JOURNAL_FILE: &str = "publish.json";
pub const JOURNAL_SCHEMA: u16 = 1;
pub const MAX_JOURNAL_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishState {
    /// Destinations may hold a mix of `generation` and earlier bytes.
    Publishing,
    /// Every destination holds `generation`'s bytes, files and directories synced.
    Published,
}

/// Field order is part of the format (the canonical encoding is compared byte
/// for byte); never reorder, rename or add fields without a new schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishJournal {
    pub schema: u16,
    /// The generation being published (its directory name under `generations/`).
    pub generation: Sha256Digest,
    /// SHA-256 of that generation's canonical `manifest.json`.
    pub manifest_sha256: Sha256Digest,
    /// The generation whose components were installed before this publication
    /// (the roll-back target); `None` only when there was none.
    pub previous: Option<Sha256Digest>,
    pub state: PublishState,
}

impl PublishJournal {
    pub fn new(generation: Sha256Digest, manifest_sha256: Sha256Digest, previous: Option<Sha256Digest>,
        state: PublishState) -> Result<Self> {
        let journal = Self { schema: JOURNAL_SCHEMA, generation, manifest_sha256, previous, state };
        journal.check()?;
        Ok(journal)
    }
    fn check(&self) -> Result<()> {
        let zero = Sha256Digest([0; 32]);
        if self.schema != JOURNAL_SCHEMA || self.generation == zero || self.manifest_sha256 == zero
            || self.previous == Some(zero) || self.previous == Some(self.generation) { return Err(Error::Invalid); }
        Ok(())
    }
    /// The canonical bytes: the only encoding [`Self::parse`] accepts.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.check()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_JOURNAL_BYTES { return Err(Error::Invalid); }
        Ok(bytes)
    }
    /// Strict: bounded, known schema and fields only, valid digests, and byte
    /// for byte the canonical encoding (no whitespace, reordering or duplicates).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_JOURNAL_BYTES { return Err(Error::Invalid); }
        let journal: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        if journal.encode()?.as_slice() != bytes { return Err(Error::Invalid); }
        Ok(journal)
    }
    /// The generation directories this journal needs: never pruned while it exists.
    pub fn retained(&self) -> impl Iterator<Item = Sha256Digest> {
        std::iter::once(self.generation).chain(self.previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn digest(n: u8) -> Sha256Digest { Sha256Digest([n; 32]) }

    #[test]
    fn publish_journal_round_trip_and_strict_parse() {
        let journal = PublishJournal::new(digest(2), digest(2), Some(digest(1)), PublishState::Publishing).unwrap();
        let bytes = journal.encode().unwrap();
        assert_eq!(PublishJournal::parse(&bytes).unwrap(), journal);
        assert_eq!(journal.retained().collect::<Vec<_>>(), vec![digest(2), digest(1)]);
        let first = PublishJournal::new(digest(2), digest(3), None, PublishState::Published).unwrap();
        assert_eq!(PublishJournal::parse(&first.encode().unwrap()).unwrap(), first);
        assert_eq!(first.retained().collect::<Vec<_>>(), vec![digest(2)]);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("{\"schema\":1,\"generation\":["), "{text}");
        assert!(text.ends_with(",\"state\":\"publishing\"}"), "{text}");
        let mut trailing = bytes.clone(); trailing.push(b'\n');
        for bad in [
            Vec::new(), trailing, vec![b' '; MAX_JOURNAL_BYTES + 1], b"null".to_vec(),
            text.replace("\"schema\":1", "\"schema\":2").into_bytes(),
            text.replace('{', "{\"extra\":1,").into_bytes(),
            text.replace('{', "{\"schema\":1,").into_bytes(),
            text.replace("publishing", "rolling_back").into_bytes(),
            text.replace(":[", ": [").into_bytes(),
        ] {
            assert!(PublishJournal::parse(&bad).is_err(), "{}", String::from_utf8_lossy(&bad));
        }
        for invalid in [(digest(0), digest(1), None), (digest(1), digest(0), None), (digest(1), digest(1), Some(digest(0))),
            (digest(1), digest(1), Some(digest(1)))] {
            assert!(PublishJournal::new(invalid.0, invalid.1, invalid.2, PublishState::Published).is_err());
        }
    }
}
