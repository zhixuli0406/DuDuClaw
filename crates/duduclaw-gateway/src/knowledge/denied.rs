//! `knowledge.access_log.denied` — every refused read across all
//! collections, newest first. Admin only: a refused read can name a
//! collection the caller of a per-collection log may not see.

use std::path::Path;

use duduclaw_knowledge::{KbResult, schema};
use rusqlite::{Connection, params};
use serde::Serialize;

/// Largest page.
pub const MAX_PAGE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeniedRow {
    pub id: i64,
    pub at: String,
    pub op: String,
    pub collection_id: Option<String>,
    /// Name of that collection when it still exists.
    pub collection_name: Option<String>,
    pub agent_id: Option<String>,
    pub requester_kind: String,
    pub requester_id: Option<String>,
    pub channel: Option<String>,
    pub query: Option<String>,
}

fn query(conn: &Connection, before_id: Option<i64>, limit: usize) -> KbResult<Vec<DeniedRow>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.at, a.op, a.collection_id, c.name, a.agent_id, a.requester_kind, a.requester_id,
                a.channel, a.query
           FROM access_log a LEFT JOIN collections c ON c.id = a.collection_id
          WHERE a.outcome = 'denied' AND (?1 IS NULL OR a.id < ?1)
          ORDER BY a.id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![before_id, limit.clamp(1, MAX_PAGE) as i64], |r| {
        Ok(DeniedRow {
            id: r.get(0)?,
            at: r.get(1)?,
            op: r.get(2)?,
            collection_id: r.get(3)?,
            collection_name: r.get(4)?,
            agent_id: r.get(5)?,
            requester_kind: r.get(6)?,
            requester_id: r.get(7)?,
            channel: r.get(8)?,
            query: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One page of refused reads.
pub fn list(home: &Path, before_id: Option<i64>, limit: usize) -> KbResult<Vec<DeniedRow>> {
    let conn = schema::open_home(home)?;
    query(&conn, before_id, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_knowledge::audit::{AccessEntry, log_access};

    #[test]
    fn lists_only_denied_rows_newest_first() {
        let conn = schema::open_in_memory().unwrap();
        for (op, outcome) in [("search", "ok"), ("get_passage", "denied"), ("search", "no_match"), ("get_passage", "denied")] {
            let e = AccessEntry {
                op: op.into(),
                requester_kind: "channel_unbound".into(),
                outcome: outcome.into(),
                collection_id: Some("col_gone".into()),
                ..Default::default()
            };
            log_access(&conn, &e).unwrap();
        }
        let rows = query(&conn, None, 50).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].id > rows[1].id);
        assert!(rows.iter().all(|r| r.op == "get_passage"));
        assert_eq!(rows[0].collection_name, None);
        let older = query(&conn, Some(rows[0].id), 50).unwrap();
        assert_eq!(older.len(), 1);
    }
}
