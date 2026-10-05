//! Cost source for responsibility budgets.
//!
//! Only what the telemetry measured counts (`token_usage.episode_id = task
//! id`); an occurrence with no measured row is charged its full reservation
//! (`reserved_unknown`). A usage row with no token counts (a runtime that
//! reported no usage, e.g. Antigravity without a `usage` block, or the Claude
//! runtime reached through the multi-runtime path, which reports zeros) is
//! not a measurement: an occurrence with such a row is charged at least its
//! reservation. A source that cannot be read is an error, and every
//! caller treats an error as "do not wake / do not dispatch" (fail closed).
//!
//! Unit: cents — the same unit as `monthly_budget_cents`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Measured spend of one episode (= one goal task).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EpisodeCost {
    pub cost: i64,
    /// Number of usage rows that carried token counts; `0` ⇒ nothing was
    /// measured.
    pub rows: i64,
    /// Usage rows with no token counts at all: spend happened but its size is
    /// unknown.
    pub unmeasured: i64,
}

/// Reads measured spend per episode. Synchronous: one indexed query.
pub trait CostSource: Send + Sync {
    fn spent_by_episode(&self, episodes: &[String])
    -> Result<HashMap<String, EpisodeCost>, String>;
}

/// Production source: `<home>/cost_telemetry.db`, opened read-only. A missing
/// database or table is an error (never "zero spent").
pub struct TelemetryCostSource {
    db_path: PathBuf,
}

impl TelemetryCostSource {
    pub fn new(home: &Path) -> Self {
        Self {
            db_path: home.join("cost_telemetry.db"),
        }
    }
}

impl CostSource for TelemetryCostSource {
    fn spent_by_episode(
        &self,
        episodes: &[String],
    ) -> Result<HashMap<String, EpisodeCost>, String> {
        if episodes.is_empty() {
            return Ok(HashMap::new());
        }
        if !self.db_path.exists() {
            return Err("cost telemetry database missing".into());
        }
        let conn = rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open cost telemetry: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("cost telemetry busy timeout: {e}"))?;
        let ids = serde_json::to_string(episodes).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT episode_id, COALESCE(SUM(cost_millicents),0),
                        SUM(CASE WHEN input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens > 0 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens > 0 THEN 0 ELSE 1 END)
                   FROM token_usage
                  WHERE episode_id IN (SELECT value FROM json_each(?1)) GROUP BY episode_id",
            )
            .map_err(|e| format!("cost telemetry query: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![ids], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    EpisodeCost {
                        cost: r.get(1)?,
                        rows: r.get(2)?,
                        unmeasured: r.get(3)?,
                    },
                ))
            })
            .map_err(|e| format!("cost telemetry query: {e}"))?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| format!("cost telemetry query: {e}"))?;
        Ok(rows)
    }
}

/// E-H4: spend of each occurrence **and everything under it** (sub-tasks,
/// their sub-tasks, goal sub-tasks looping on their own episode ids), keyed
/// by the occurrence id. A tree too large to walk is an error (fail closed:
/// the caller treats the spend as unknown), never a partial sum. Work handed
/// to another employee with no task in the tree is not seen (listed in
/// `summary::COST_NOT_COUNTED`).
pub async fn tree_spent(
    store: &crate::task_store::TaskStore,
    cost: &dyn CostSource,
    occurrences: &[String],
) -> Result<HashMap<String, EpisodeCost>, String> {
    let limit = crate::task_store::STOP_TREE_SCAN_LIMIT;
    let mut members: Vec<(String, Vec<String>)> = Vec::with_capacity(occurrences.len());
    for occ in occurrences {
        let ids = store.stop_tree_ids(occ, limit + 1).await?;
        if ids.len() > limit {
            return Err(format!("task tree under {occ} is too large to measure"));
        }
        members.push((occ.clone(), ids));
    }
    let all: Vec<String> = members
        .iter()
        .flat_map(|(_, ids)| ids.iter().cloned())
        .collect();
    let measured = cost.spent_by_episode(&all)?;
    Ok(members
        .into_iter()
        .filter_map(|(occ, ids)| {
            let total = ids.iter().filter_map(|id| measured.get(id)).fold(
                EpisodeCost::default(),
                |acc, m| EpisodeCost {
                    cost: acc.cost + m.cost,
                    rows: acc.rows + m.rows,
                    unmeasured: acc.unmeasured + m.unmeasured,
                },
            );
            (total.rows > 0 || total.unmeasured > 0).then_some((occ, total))
        })
        .collect())
}

/// Charge for a settled occurrence: measured if everything was measured,
/// otherwise at least the full reservation. The charge never falls below what
/// was measured. Returns `(charged, cost_basis)`.
pub fn settle_charge(measured: Option<EpisodeCost>, reserved: i64) -> (i64, &'static str) {
    match measured {
        Some(m) if m.rows > 0 && m.unmeasured == 0 => (m.cost.max(0), "measured"),
        Some(m) => (m.cost.max(reserved), "reserved_unknown"),
        None => (reserved, "reserved_unknown"),
    }
}

/// Spend used against a running occurrence's cap: what was measured, or at
/// least the reservation when some of it is unknown (see [`settle_charge`]).
pub fn running_spend(c: EpisodeCost, reserved: i64) -> i64 {
    if c.unmeasured > 0 {
        c.cost.max(reserved)
    } else {
        c.cost
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Mutex;

    /// In-memory source for tests. `fail = true` simulates an unreadable
    /// telemetry database.
    #[derive(Default)]
    pub struct FixedCost {
        pub by_episode: Mutex<HashMap<String, EpisodeCost>>,
        pub fail: Mutex<bool>,
    }

    impl FixedCost {
        pub fn set(&self, episode: &str, cost: i64) {
            self.by_episode.lock().unwrap().insert(
                episode.to_string(),
                EpisodeCost {
                    cost,
                    rows: 1,
                    unmeasured: 0,
                },
            );
        }
        /// An episode whose only usage row carried no token counts.
        pub fn set_unmeasured(&self, episode: &str) {
            self.by_episode.lock().unwrap().insert(
                episode.to_string(),
                EpisodeCost {
                    cost: 0,
                    rows: 0,
                    unmeasured: 1,
                },
            );
        }
        pub fn set_fail(&self, fail: bool) {
            *self.fail.lock().unwrap() = fail;
        }
    }

    impl CostSource for FixedCost {
        fn spent_by_episode(
            &self,
            episodes: &[String],
        ) -> Result<HashMap<String, EpisodeCost>, String> {
            if *self.fail.lock().unwrap() {
                return Err("cost telemetry unavailable (test)".into());
            }
            let map = self.by_episode.lock().unwrap();
            Ok(episodes
                .iter()
                .filter_map(|e| map.get(e).map(|c| (e.clone(), *c)))
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmeasured_occurrences_are_charged_their_reservation() {
        assert_eq!(settle_charge(None, 500), (500, "reserved_unknown"));
        assert_eq!(
            settle_charge(Some(EpisodeCost::default()), 500),
            (500, "reserved_unknown")
        );
        let measured = EpisodeCost {
            cost: 120,
            rows: 3,
            unmeasured: 0,
        };
        assert_eq!(settle_charge(Some(measured), 500), (120, "measured"));
        // Round 4: a row with no token counts makes the spend unknown.
        let partly = EpisodeCost {
            cost: 120,
            rows: 3,
            unmeasured: 1,
        };
        assert_eq!(settle_charge(Some(partly), 500), (500, "reserved_unknown"));
        let over = EpisodeCost {
            cost: 900,
            ..partly
        };
        assert_eq!(settle_charge(Some(over), 500), (900, "reserved_unknown"));
        assert_eq!(running_spend(partly, 500), 500);
        assert_eq!(running_spend(measured, 500), 120);
    }

    #[test]
    fn missing_telemetry_database_is_an_error_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let src = TelemetryCostSource::new(dir.path());
        assert!(src.spent_by_episode(&["t1".into()]).is_err());
        // An empty request never needs the database.
        assert!(src.spent_by_episode(&[]).unwrap().is_empty());
    }

    #[test]
    fn telemetry_rows_are_summed_per_episode() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("cost_telemetry.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE token_usage (episode_id TEXT, cost_millicents INTEGER,
                input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_creation_tokens INTEGER);
             INSERT INTO token_usage VALUES ('t1', 10, 5, 5, 0, 0), ('t1', 15, 5, 5, 0, 0),
               ('t2', 7, 1, 0, 0, 0), (NULL, 99, 1, 1, 0, 0), ('t4', 0, 0, 0, 0, 0);",
        )
        .unwrap();
        drop(conn);
        let src = TelemetryCostSource::new(dir.path());
        let got = src
            .spent_by_episode(&["t1".into(), "t2".into(), "t3".into(), "t4".into()])
            .unwrap();
        let m = |cost, rows, unmeasured| EpisodeCost {
            cost,
            rows,
            unmeasured,
        };
        assert_eq!(got["t1"], m(25, 2, 0));
        assert_eq!(got["t2"], m(7, 1, 0));
        assert!(!got.contains_key("t3"));
        // A zero-token row (runtime reported no usage) is not a measurement.
        assert_eq!(got["t4"], m(0, 0, 1));
    }
}
