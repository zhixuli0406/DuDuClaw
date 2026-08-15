//! WP2.5 — the settlement queue that connects a committed AEE round to the
//! observation sweep that later judges it.
//!
//! The legacy SOUL path parks an `observing` row in `soul_versions` and
//! `ObservationFinalizer` closes it. AEE writes no `SoulVersion` at all (its
//! artefact is a set of playbook entries), so it needs its own one-row-per-
//! agent queue: "these entry ids went live at T, here are the case scores the
//! champion had *before* they did, come back after the window and compare".
//!
//! Storage reuses `evolution.db` — the same file `VersionStore` and
//! `ChampionStore` already own — so there is no third database to back up,
//! encrypt, or migrate.
//!
//! One row per agent, by primary key: a second commit before the first has
//! settled supersedes it. That is the honest shape, because the second commit
//! changed the very playbook the first was going to be judged against; keeping
//! both would settle the first entry set against a state that no longer
//! matches it. (The AEE round itself refuses to start while a settlement is
//! still pending — see `run::run_aee_round` — so this is a belt-and-braces
//! rule, not the primary guard.)

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tracing::warn;

use crate::gvu::verifier_measure::CaseScore;

/// One agent's outstanding settlement.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingSettlement {
    pub agent_id: String,
    /// When the deltas were committed.
    pub applied_at: DateTime<Utc>,
    /// Earliest instant the settlement sweep may judge it.
    pub settle_after: DateTime<Utc>,
    /// Case scores as they stood BEFORE the commit — the `before` half of
    /// `settle::entry_verdict`.
    pub before: Vec<CaseScore>,
    /// Entry ids the round actually committed (audit; the sweep re-reads the
    /// live playbook rather than trusting this list).
    pub entry_ids: Vec<String>,
    /// The `cases` noise band in force at commit time, so the settlement uses
    /// the same tolerance the commit gate did instead of re-reading a config
    /// that may have changed underneath it.
    pub band_cases: f64,
    /// WP-4E: the held-out band in force at commit time, frozen for the same
    /// reason. `None` on a row written before this column existed — the
    /// accessor then falls back to the shared derivation instead of guessing.
    pub band_holdout: Option<f64>,
}

impl PendingSettlement {
    /// The tolerances the settlement fence must use.
    pub fn case_bands(&self) -> crate::gvu::aee::settle::CaseBands {
        match self.band_holdout {
            Some(h) => crate::gvu::aee::settle::CaseBands { cases: self.band_cases, holdout: h },
            None => crate::gvu::aee::settle::CaseBands::from_cases_only(self.band_cases),
        }
    }
}

/// CRUD over the `aee_pending_settlement` table.
pub struct PendingSettlementStore {
    db_path: std::path::PathBuf,
}

impl PendingSettlementStore {
    pub fn new(db_path: &std::path::Path) -> Self {
        let store = Self { db_path: db_path.to_path_buf() };
        if let Err(e) = store.init() {
            warn!(db = %db_path.display(), "aee_pending_settlement: table init failed: {e}");
        }
        store
    }

    fn conn(&self) -> Result<Connection, String> {
        Connection::open(&self.db_path).map_err(|e| e.to_string())
    }

    fn init(&self) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS aee_pending_settlement (
                 agent_id       TEXT PRIMARY KEY,
                 applied_at     TEXT NOT NULL,
                 settle_after   TEXT NOT NULL,
                 before_json    TEXT NOT NULL,
                 entry_ids_json TEXT NOT NULL,
                 band_cases     REAL NOT NULL DEFAULT 0.05
             );",
        )
        .map_err(|e| e.to_string())?;
        // WP-4E: nullable on purpose — a row written before this column
        // existed reads back as `None`, which the accessor turns into the
        // documented derivation. `ALTER TABLE` on an existing column is an
        // error, not a problem, so it is swallowed rather than surfaced.
        let _ = conn.execute(
            "ALTER TABLE aee_pending_settlement ADD COLUMN band_holdout REAL",
            [],
        );
        Ok(())
    }

    /// Install (or replace) the agent's pending settlement.
    pub fn put(&self, p: &PendingSettlement) -> Result<(), String> {
        let conn = self.conn()?;
        let before = serde_json::to_string(&p.before).map_err(|e| e.to_string())?;
        let ids = serde_json::to_string(&p.entry_ids).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO aee_pending_settlement
                 (agent_id, applied_at, settle_after, before_json, entry_ids_json, band_cases,
                  band_holdout)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id) DO UPDATE SET
                 applied_at     = excluded.applied_at,
                 settle_after   = excluded.settle_after,
                 before_json    = excluded.before_json,
                 entry_ids_json = excluded.entry_ids_json,
                 band_cases     = excluded.band_cases,
                 band_holdout   = excluded.band_holdout",
            params![
                p.agent_id,
                p.applied_at.to_rfc3339(),
                p.settle_after.to_rfc3339(),
                before,
                ids,
                p.band_cases,
                p.band_holdout,
            ],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// The agent's pending settlement, whether or not it is due yet.
    pub fn get(&self, agent_id: &str) -> Option<PendingSettlement> {
        let conn = self.conn().ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, applied_at, settle_after, before_json, entry_ids_json, band_cases,
                        band_holdout
                 FROM aee_pending_settlement WHERE agent_id = ?1",
            )
            .ok()?;
        stmt.query_row(params![agent_id], |r| row_to_pending(r))
            .ok()
            .flatten()
    }

    /// Every settlement whose window has closed by `now`.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<PendingSettlement> {
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT agent_id, applied_at, settle_after, before_json, entry_ids_json, band_cases,
                    band_holdout
             FROM aee_pending_settlement WHERE settle_after <= ?1 ORDER BY settle_after ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map(params![now.to_rfc3339()], |r| row_to_pending(r))
            .map(|rows| rows.filter_map(|r| r.ok().flatten()).collect())
            .unwrap_or_default()
    }

    /// Drop the agent's settlement (it has been judged, or it can never be).
    pub fn remove(&self, agent_id: &str) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM aee_pending_settlement WHERE agent_id = ?1",
            params![agent_id],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

/// A row whose JSON columns are unreadable yields `Ok(None)` rather than an
/// error: a corrupt row must not stop the sweep judging every OTHER agent's
/// settlement.
fn row_to_pending(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<PendingSettlement>> {
    let agent_id: String = r.get(0)?;
    let applied_at: String = r.get(1)?;
    let settle_after: String = r.get(2)?;
    let before_json: String = r.get(3)?;
    let ids_json: String = r.get(4)?;
    let band_cases: f64 = r.get(5)?;
    // Nullable, and a non-REAL value is treated as absent rather than as an
    // error that would drop the whole settlement.
    let band_holdout: Option<f64> = r.get(6).unwrap_or(None);

    let (Ok(before), Ok(entry_ids)) = (
        serde_json::from_str::<Vec<CaseScore>>(&before_json),
        serde_json::from_str::<Vec<String>>(&ids_json),
    ) else {
        warn!(agent = %agent_id, "aee_pending_settlement: unreadable row — skipped");
        return Ok(None);
    };
    let parse = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now())
    };
    Ok(Some(PendingSettlement {
        agent_id,
        applied_at: parse(&applied_at),
        settle_after: parse(&settle_after),
        before,
        entry_ids,
        band_cases,
        band_holdout,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(case: &str, score: f64) -> CaseScore {
        CaseScore { case: case.into(), score, held_out: false }
    }

    fn sample(agent: &str, settle_after: DateTime<Utc>) -> PendingSettlement {
        PendingSettlement {
            agent_id: agent.to_string(),
            applied_at: Utc::now(),
            settle_after,
            before: vec![cs("s/a", 1.0), cs("s/b", 0.0)],
            entry_ids: vec!["e1".into(), "e2".into()],
            band_cases: 0.05,
            band_holdout: Some(0.025),
        }
    }

    #[test]
    fn put_get_round_trips_and_due_respects_the_window() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let store = PendingSettlementStore::new(f.path());

        let future = Utc::now() + chrono::Duration::hours(24);
        store.put(&sample("a1", future)).unwrap();
        let got = store.get("a1").unwrap();
        assert_eq!(got.before.len(), 2);
        assert_eq!(got.entry_ids, vec!["e1".to_string(), "e2".to_string()]);

        assert!(store.due(Utc::now()).is_empty(), "not due yet");
        assert_eq!(store.due(future + chrono::Duration::seconds(1)).len(), 1);

        store.remove("a1").unwrap();
        assert!(store.get("a1").is_none());
    }

    #[test]
    fn a_second_commit_supersedes_the_first() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let store = PendingSettlementStore::new(f.path());
        let past = Utc::now() - chrono::Duration::hours(1);
        store.put(&sample("a2", past)).unwrap();
        let mut second = sample("a2", past);
        second.entry_ids = vec!["e9".into()];
        store.put(&second).unwrap();

        let due = store.due(Utc::now());
        assert_eq!(due.len(), 1, "one row per agent");
        assert_eq!(due[0].entry_ids, vec!["e9".to_string()]);
    }

    /// WP-4E: the held-out band is frozen at commit time like `band_cases`,
    /// and a row written before the column existed settles under the shared
    /// derivation rather than under an invented number.
    #[test]
    fn holdout_band_round_trips_and_a_legacy_row_derives_it() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let store = PendingSettlementStore::new(f.path());
        let past = Utc::now() - chrono::Duration::hours(1);

        let mut p = sample("a3", past);
        p.band_cases = 0.08;
        p.band_holdout = Some(0.01);
        store.put(&p).unwrap();
        let got = store.get("a3").unwrap();
        assert_eq!(got.band_holdout, Some(0.01));
        assert_eq!(got.case_bands().holdout, 0.01, "an override is not re-derived");

        // Simulate a pre-migration row: the column is NULL.
        {
            let conn = Connection::open(f.path()).unwrap();
            conn.execute(
                "UPDATE aee_pending_settlement SET band_holdout = NULL WHERE agent_id = 'a3'",
                [],
            )
            .unwrap();
        }
        let legacy = store.get("a3").unwrap();
        assert_eq!(legacy.band_holdout, None);
        assert_eq!(legacy.case_bands().cases, 0.08);
        assert_eq!(legacy.case_bands().holdout, 0.04, "derived, not defaulted to zero or to cases");
    }

    /// The `ALTER TABLE` migration must be idempotent — `new()` runs on every
    /// gateway boot and on every store construction.
    #[test]
    fn reopening_the_store_does_not_break_on_the_added_column() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let past = Utc::now() - chrono::Duration::hours(1);
        {
            let s = PendingSettlementStore::new(f.path());
            s.put(&sample("a4", past)).unwrap();
        }
        let s2 = PendingSettlementStore::new(f.path());
        let s3 = PendingSettlementStore::new(f.path());
        assert_eq!(s2.get("a4").unwrap().band_holdout, Some(0.025));
        assert_eq!(s3.due(Utc::now()).len(), 1);
    }

    #[test]
    fn absent_settlement_is_none_not_an_error() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let store = PendingSettlementStore::new(f.path());
        assert!(store.get("nobody").is_none());
        assert!(store.due(Utc::now()).is_empty());
    }
}
