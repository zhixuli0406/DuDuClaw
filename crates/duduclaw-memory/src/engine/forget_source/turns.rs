//! The `mcp_turn` keys that belong to what a plan forgets (P2-B live
//! verification, issue 1).
//!
//! An employee's MCP write during a channel turn carries two sources: the
//! turn (`mcp_turn`, `turn:<id>`) and the user message that triggered it
//! (`channel_message`, `m:<seq>`). A forget of that message must also fence
//! a later write that names only the turn. The link comes from recorded
//! lineage only, never from timing:
//!
//! * message scope: a turn key that one write recorded as a direct source
//!   together with a forgotten `m:<seq>` key — from `turn_message_links`
//!   (kept across namespaces and forgets; reaffirming writes record it too)
//!   or from live lineage of any namespace;
//! * session scope: every turn key of the session that the session
//!   watermark itself reaches.
//!
//! Each such turn key gets a message-scope tombstone. It is part of the plan
//! body (and its hash), so plan and apply agree.

use std::collections::BTreeSet;

use super::body::TombstoneSpec;
use super::*;
use crate::lineage::db::tombstone_match_sql;

fn e(x: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(x.to_string())
}

/// Message-scope tombstones for the turn keys linked to `sel` (sorted, not
/// already named by the selector). Expects `temp.forget_candidates` to hold
/// the selector's own tombstones.
pub(super) fn linked_turn_specs(
    conn: &Connection,
    agent_id: &str,
    sel: &FrozenSelector,
) -> Result<Vec<TombstoneSpec>> {
    let kind = crate::lineage::SourceKind::McpTurn.as_str();
    let mut keys: BTreeSet<String> = BTreeSet::new();
    if sel.messages.is_empty() {
        let sql = format!(
            "SELECT DISTINCT o.source_message FROM memory_origins o
             WHERE o.agent_id = ?1 AND o.source_session = ?2 AND o.source_kind = ?3 AND {}",
            tombstone_match_sql(
                "temp.forget_candidates",
                "o.agent_id",
                "o.source_session",
                "o.source_message",
                "o.source_seq",
                "o.source_observed_at"
            )
        );
        let mut stmt = conn.prepare(&sql).map_err(e)?;
        let rows = stmt
            .query_map(params![agent_id, sel.session, kind], |r| {
                r.get::<_, String>(0)
            })
            .map_err(e)?;
        for k in rows {
            keys.insert(k.map_err(e)?);
        }
    } else {
        // The link is a fact about the conversation, recorded where the
        // turn's first write landed — possibly another namespace (F1: the
        // employee dispatched from that turn records only the turn). Read it
        // from the link table and from live lineage of any namespace; only
        // keys are read, and tombstones are still written for `agent_id`.
        let mut stmt = conn
            .prepare_cached(
                "SELECT turn_message FROM turn_message_links
                 WHERE source_session = ?1 AND user_message = ?2
                 UNION
                 SELECT t.source_message FROM memory_origins t
                 JOIN memory_origins c
                   ON c.memory_store = t.memory_store AND c.memory_id = t.memory_id
                  AND c.agent_id = t.agent_id
                 WHERE t.source_session = ?1 AND t.source_kind = ?3 AND t.role = 'direct'
                   AND c.source_session = ?1 AND c.source_kind = 'channel_message'
                   AND c.role = 'direct' AND c.source_message = ?2",
            )
            .map_err(e)?;
        for m in sel.messages.iter().filter(|m| m.starts_with("m:")) {
            let rows = stmt
                .query_map(params![sel.session, m, kind], |r| r.get::<_, String>(0))
                .map_err(e)?;
            for k in rows {
                keys.insert(k.map_err(e)?);
            }
        }
    }
    Ok(keys
        .into_iter()
        .filter(|k| !sel.messages.contains(k))
        .map(|k| TombstoneSpec {
            scope: "message",
            session: sel.session.clone(),
            message: k,
            upto_seq: None,
            upto_time: None,
        })
        .collect())
}
