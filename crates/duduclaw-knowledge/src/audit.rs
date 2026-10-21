//! `access_log`: one row per read (search, passage, listing, download) and
//! per management action, with the requester, the allowed set, the query
//! (truncated) and the result ids — never the passage text.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::KbResult;

/// Longest query stored.
pub const MAX_QUERY_CHARS: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessEntry {
    pub op: String,
    pub collection_id: Option<String>,
    pub agent_id: Option<String>,
    /// `dashboard_user` | `channel_user` | `channel_unbound` | `system`.
    pub requester_kind: String,
    pub requester_id: Option<String>,
    pub channel: Option<String>,
    pub session_digest: Option<String>,
    pub turn_id: Option<String>,
    pub query: Option<String>,
    pub allowed: Vec<String>,
    /// `[{chunk_id, source_id, n, rank}]`.
    pub results: Vec<Value>,
    pub mode: Option<String>,
    /// `ok` | `denied` | `error` | `no_match`.
    pub outcome: String,
}

/// Append one row.
pub fn log_access(conn: &Connection, e: &AccessEntry) -> KbResult<()> {
    conn.execute(
        "INSERT INTO access_log(at, op, collection_id, agent_id, requester_kind, requester_id, channel, session_digest,
                                turn_id, query, allowed_json, results_json, result_count, mode, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            crate::now_rfc3339(),
            e.op,
            e.collection_id,
            e.agent_id,
            e.requester_kind,
            e.requester_id,
            e.channel,
            e.session_digest,
            e.turn_id,
            e.query
                .as_deref()
                .map(|q| duduclaw_core::truncate_chars(q, MAX_QUERY_CHARS)),
            serde_json::to_string(&e.allowed).unwrap_or_else(|_| "[]".into()),
            Value::Array(e.results.clone()).to_string(),
            e.results.len() as i64,
            e.mode,
            e.outcome,
        ],
    )?;
    Ok(())
}

/// Delete rows older than `days`.
pub fn prune(conn: &Connection, days: u32) -> KbResult<usize> {
    let cutoff =
        (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Ok(conn.execute("DELETE FROM access_log WHERE at < ?1", params![cutoff])?)
}

/// A row as listed for a collection owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessRow {
    pub id: i64,
    pub at: String,
    pub op: String,
    pub agent_id: Option<String>,
    pub requester_kind: String,
    pub requester_id: Option<String>,
    pub channel: Option<String>,
    pub query: Option<String>,
    /// Results that came from the listed collection.
    pub result_count: i64,
    pub mode: Option<String>,
    pub outcome: String,
}

/// Rows touching `collection_id` (named directly, or in the allowed set of a
/// search), newest first, paged by `before_id`.
pub fn list_for_collection(
    conn: &Connection,
    collection_id: &str,
    source_ids: &[String],
    before_id: Option<i64>,
    limit: usize,
) -> KbResult<Vec<AccessRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, at, op, agent_id, requester_kind, requester_id, channel, query, results_json, mode, outcome
           FROM access_log
          WHERE (collection_id = ?1 OR EXISTS (SELECT 1 FROM json_each(allowed_json) WHERE value = ?1))
            AND (?2 IS NULL OR id < ?2)
          ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![collection_id, before_id, limit.clamp(1, 200) as i64], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, String>(10)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, at, op, agent_id, requester_kind, requester_id, channel, query, results, mode, outcome) = row?;
        let results: Vec<Value> = serde_json::from_str(&results).unwrap_or_default();
        // Count only this collection's results; other collections' ids stay hidden.
        let result_count = results
            .iter()
            .filter(|r| {
                r.get("source_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| source_ids.iter().any(|x| x == s))
            })
            .count() as i64;
        out.push(AccessRow {
            id,
            at,
            op,
            agent_id,
            requester_kind,
            requester_id,
            channel,
            query,
            result_count,
            mode,
            outcome,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_list_and_prune() {
        let conn = crate::schema::open_in_memory().unwrap();
        let entry = AccessEntry {
            op: "search".into(),
            requester_kind: "dashboard_user".into(),
            requester_id: Some("u".into()),
            query: Some("特".repeat(800)),
            allowed: vec!["col_a".into(), "col_b".into()],
            results: vec![
                serde_json::json!({"chunk_id": 1, "source_id": "src_a", "n": 1, "rank": 1}),
                serde_json::json!({"chunk_id": 2, "source_id": "src_b", "n": 2, "rank": 2}),
            ],
            mode: Some("keyword_only".into()),
            outcome: "ok".into(),
            ..Default::default()
        };
        log_access(&conn, &entry).unwrap();
        let rows = list_for_collection(&conn, "col_a", &["src_a".into()], None, 50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result_count, 1);
        assert_eq!(rows[0].query.as_ref().unwrap().chars().count(), MAX_QUERY_CHARS);
        assert!(list_for_collection(&conn, "col_c", &[], None, 50).unwrap().is_empty());
        conn.execute("UPDATE access_log SET at = '2000-01-01T00:00:00.000Z'", [])
            .unwrap();
        assert_eq!(prune(&conn, 180).unwrap(), 1);
    }
}
