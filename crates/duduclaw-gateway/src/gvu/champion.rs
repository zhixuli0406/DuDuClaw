//! WP2.4 §2.4.1 — the reigning champion a candidate must match or improve on.
//!
//! A champion is **the agent's whole playbook snapshot**, not a single entry:
//! comparing entry-by-entry would let a candidate that improves one rule while
//! quietly wrecking three others look like an improvement. The snapshot
//! identity is `sha256` over the sorted `dedup_key`s of every active/probation
//! entry, so it is stable under reordering and changes the moment the set of
//! live rules changes.
//!
//! Storage reuses `evolution.db` (the same file
//! [`crate::gvu::version_store::VersionStore`] owns) via its own table, so
//! there is no second database to back up, encrypt, or migrate. Every method
//! is best-effort: a champion read failure degrades to "no champion"
//! (bootstrap), never to a crash — but it `warn!`s, it does not go quiet.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tracing::warn;

use super::verifier_measure::{AntiDriftState, MeasureVector};

/// One agent's reigning champion.
#[derive(Debug, Clone, PartialEq)]
pub struct Champion {
    pub agent_id: String,
    /// sha256 of the sorted `dedup_key` list — see module docs.
    pub snapshot_hash: String,
    pub measure: MeasureVector,
    pub established_at: DateTime<Utc>,
    /// §2.4.3 companion 1 counters.
    pub anti_drift: AntiDriftState,
    /// Monotonically increasing AEE round counter. Drives the deterministic
    /// strategy mix (§3.2.2) and the deterministic case sampling (§3.7.1), so
    /// it must persist across gateway restarts — an in-memory counter would
    /// silently reset the mix to "always Repair" on every restart.
    pub round_seq: u64,
    /// §2.4.3 companion 3: raised on a tie-commit, cleared by the operator
    /// `rotate-holdout` CLI. AEE never rotates the held-out subset itself.
    pub holdout_rotation_due: bool,
}

/// SHA-256 hex over a snapshot's sorted dedup keys.
pub fn snapshot_hash(dedup_keys: &[String]) -> String {
    let mut sorted: Vec<&str> = dedup_keys.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let joined = sorted.join("\u{1f}");
    let d = ring::digest::digest(&ring::digest::SHA256, joined.as_bytes());
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// CRUD over the `gvu_champion` table.
pub struct ChampionStore {
    db_path: PathBuf,
}

impl ChampionStore {
    /// Open (and lazily create) the table. Mirrors `VersionStore`'s
    /// connection convention: one short-lived connection per call, no pool.
    pub fn new(db_path: &Path) -> Self {
        let store = Self { db_path: db_path.to_path_buf() };
        if let Err(e) = store.init() {
            warn!(db = %db_path.display(), "gvu_champion: table init failed: {e}");
        }
        store
    }

    fn conn(&self) -> Result<Connection, String> {
        Connection::open(&self.db_path).map_err(|e| e.to_string())
    }

    fn init(&self) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS gvu_champion (
                 agent_id             TEXT PRIMARY KEY,
                 snapshot_hash        TEXT NOT NULL,
                 measure_vector_json  TEXT NOT NULL,
                 established_at       TEXT NOT NULL,
                 consecutive_matches  INTEGER NOT NULL DEFAULT 0,
                 round_seq            INTEGER NOT NULL DEFAULT 0,
                 holdout_rotation_due INTEGER NOT NULL DEFAULT 0
             );",
        )
        .map_err(|e| e.to_string())
    }

    /// The agent's current champion, or `None` when it has never had one
    /// (bootstrap: the first non-`Invalid` candidate takes the crown).
    pub fn get(&self, agent_id: &str) -> Option<Champion> {
        let conn = self.conn().ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT snapshot_hash, measure_vector_json, established_at,
                        consecutive_matches, round_seq, holdout_rotation_due
                 FROM gvu_champion WHERE agent_id = ?1",
            )
            .ok()?;
        let row = stmt
            .query_row(params![agent_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .ok()?;
        let measure: MeasureVector = match serde_json::from_str(&row.1) {
            Ok(m) => m,
            Err(e) => {
                // Corrupt vector: degrade to "no champion" rather than
                // comparing against garbage, and say so.
                warn!(agent = %agent_id, "gvu_champion: measure vector unreadable ({e}) — treating as no champion");
                return None;
            }
        };
        Some(Champion {
            agent_id: agent_id.to_string(),
            snapshot_hash: row.0,
            measure,
            established_at: DateTime::parse_from_rfc3339(&row.2)
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            anti_drift: AntiDriftState { consecutive_matches: row.3.max(0) as u32 },
            round_seq: row.4.max(0) as u64,
            holdout_rotation_due: row.5 != 0,
        })
    }

    /// Install a new champion (or update the reigning one in place).
    pub fn put(&self, champion: &Champion) -> Result<(), String> {
        let conn = self.conn()?;
        let json = serde_json::to_string(&champion.measure).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO gvu_champion
                 (agent_id, snapshot_hash, measure_vector_json, established_at,
                  consecutive_matches, round_seq, holdout_rotation_due)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id) DO UPDATE SET
                 snapshot_hash        = excluded.snapshot_hash,
                 measure_vector_json  = excluded.measure_vector_json,
                 established_at       = excluded.established_at,
                 consecutive_matches  = excluded.consecutive_matches,
                 round_seq            = excluded.round_seq,
                 holdout_rotation_due = excluded.holdout_rotation_due",
            params![
                champion.agent_id,
                champion.snapshot_hash,
                json,
                champion.established_at.to_rfc3339(),
                champion.anti_drift.consecutive_matches as i64,
                champion.round_seq as i64,
                i64::from(champion.holdout_rotation_due),
            ],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Read the agent's round counter without materialising the champion.
    /// Absent champion ⇒ round 0.
    pub fn round_seq(&self, agent_id: &str) -> u64 {
        self.get(agent_id).map(|c| c.round_seq).unwrap_or(0)
    }

    /// Advance the round counter by one, creating a counter-only row when the
    /// agent has no champion yet.
    ///
    /// Called at the START of an AEE round, so a round that ends in `Skipped`
    /// still advances the mix — otherwise `RepairOnly`-style skips would pin
    /// `round_seq % 10` and the configured ratio could never be honoured.
    pub fn bump_round(&self, agent_id: &str) -> Result<u64, String> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO gvu_champion
                 (agent_id, snapshot_hash, measure_vector_json, established_at, round_seq)
             VALUES (?1, '', '{}', ?2, 1)
             ON CONFLICT(agent_id) DO UPDATE SET round_seq = round_seq + 1",
            params![agent_id, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(self.round_seq(agent_id))
    }

    /// Clear the held-out rotation flag — the operator CLI's job, never the
    /// agent's (§3.5: letting the examinee reshuffle its own exam is exactly
    /// the reward-hacking entry point this design closes).
    pub fn clear_holdout_rotation(&self, agent_id: &str) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE gvu_champion SET holdout_rotation_due = 0 WHERE agent_id = ?1",
            params![agent_id],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::NamedTempFile, ChampionStore) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let s = ChampionStore::new(f.path());
        (f, s)
    }

    #[test]
    fn snapshot_hash_is_order_independent() {
        let a = snapshot_hash(&["b".into(), "a".into(), "c".into()]);
        let b = snapshot_hash(&["c".into(), "b".into(), "a".into()]);
        assert_eq!(a, b);
        assert_ne!(a, snapshot_hash(&["a".into(), "b".into()]));
    }

    #[test]
    fn absent_champion_is_none_not_an_error() {
        let (_f, s) = store();
        assert!(s.get("nobody").is_none());
        assert_eq!(s.round_seq("nobody"), 0);
    }

    #[test]
    fn put_then_get_round_trips() {
        let (_f, s) = store();
        let c = Champion {
            agent_id: "a1".into(),
            snapshot_hash: snapshot_hash(&["k1".into()]),
            measure: MeasureVector { judge: Some(0.8), anti_sycophancy: 1.0, ..Default::default() },
            established_at: Utc::now(),
            anti_drift: AntiDriftState { consecutive_matches: 2 },
            round_seq: 41,
            holdout_rotation_due: true,
        };
        s.put(&c).unwrap();
        let got = s.get("a1").unwrap();
        assert_eq!(got.snapshot_hash, c.snapshot_hash);
        assert_eq!(got.measure.judge, Some(0.8));
        assert_eq!(got.anti_drift.consecutive_matches, 2);
        assert_eq!(got.round_seq, 41);
        assert!(got.holdout_rotation_due);
        s.clear_holdout_rotation("a1").unwrap();
        assert!(!s.get("a1").unwrap().holdout_rotation_due);
    }

    #[test]
    fn round_seq_survives_reopen_and_advances_on_skipped_rounds() {
        let f = tempfile::NamedTempFile::new().unwrap();
        {
            let s = ChampionStore::new(f.path());
            assert_eq!(s.bump_round("a2").unwrap(), 1);
            assert_eq!(s.bump_round("a2").unwrap(), 2);
        }
        // Fresh store over the same file — the mix must not reset.
        let s2 = ChampionStore::new(f.path());
        assert_eq!(s2.round_seq("a2"), 2);
        assert_eq!(s2.bump_round("a2").unwrap(), 3);
    }
}
