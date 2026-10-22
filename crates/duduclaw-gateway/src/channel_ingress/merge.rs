//! Messages that waited behind a running turn, answered in one turn (B10).
//!
//! One conversation is processed in order, and a LINE reply token is valid
//! for about a minute from receipt. When a person sends several messages
//! while an earlier one is still being answered, each later message used to
//! wait for its own full turn, so from the third or fourth one on the token
//! had expired and the answer depended on the Push API (which uses the
//! official account's monthly quota; on the free plan it runs out and the
//! answer is lost). Now, when a turn starts, the `ready` messages directly
//! behind it that the adapter accepts are folded into it:
//!
//! - they become `merged` (terminal, does not hold the conversation, no
//!   operator action), with their payload moved into the head event's
//!   payload under `"merged"` — so a retry or rerun of the head still covers
//!   every message;
//! - the turn answers all of them at once, with the newest message's reply
//!   token.
//!
//! Only rows with the head's exact route and authority snapshot are taken,
//! and taking stops at the first row that does not qualify, so order is kept.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{IngressRow, IngressStore, PENDING_REVISION, ROW_SELECT, row_from_sql};

/// Status of a message answered as part of an earlier message's turn.
pub(crate) const MERGED_STATUS: &str = "merged";

/// Most messages answered in one turn, the head included.
pub(crate) const MAX_MERGED: usize = 5;

/// One message folded into the head's turn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergedEvent {
    pub id: String,
    pub received_at: i64,
    /// The provider event (`payload.event` of the folded row).
    pub event: serde_json::Value,
}

/// The messages already folded into a head payload.
pub(crate) fn merged_from_payload(payload: &serde_json::Value) -> Vec<MergedEvent> {
    payload["merged"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(MergedEvent {
                id: m["id"].as_str()?.to_string(),
                received_at: m["received_at"].as_i64()?,
                event: m.get("event")?.clone(),
            })
        })
        .collect()
}

pub(crate) fn merged_json(merged: &[MergedEvent]) -> serde_json::Value {
    merged
        .iter()
        .map(|m| serde_json::json!({"id": m.id, "received_at": m.received_at, "event": m.event}))
        .collect()
}

/// A stored route: `(agent, one_shot)`.
type Route = Option<(String, bool)>;

fn route_of(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<Route, String> {
    tx.query_row(
        "SELECT agent,one_shot FROM ingress_routes WHERE id=?1",
        [id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)),
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Whether `row` may be folded into `head` (store-level checks; the
/// adapter's `accept` decides about the message itself).
fn same_run_shape(head: &IngressRow, head_route: &Route, row: &IngressRow, route: &Route) -> bool {
    row.status == "ready"
        && !row.decision_fastlane
        && row.revision != PENDING_REVISION
        && row.revision == head.revision
        && row.authorization_revision == head.authorization_revision
        && row.run_authorization_id.is_none()
        && row.retry_at.is_none()
        && route == head_route
}

impl IngressStore {
    /// Fold the waiting messages directly behind the claimed `head` into it
    /// (see the module docs). `accept` sees each candidate's provider event
    /// and must also have accepted the head's (the caller checks). Returns
    /// every message folded into the head so far, oldest first; an operator
    /// rerun (`run_authorization_id`) folds nothing new.
    pub(crate) async fn absorb_followers(
        &self,
        head: &IngressRow,
        accept: &(dyn Fn(&serde_json::Value) -> bool + Sync),
    ) -> Result<Vec<MergedEvent>, String> {
        let mut conn = self.connection().lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current = tx
            .query_row(&format!("{ROW_SELECT} WHERE i.id=?1"), [&head.id], row_from_sql)
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(current) = current.filter(|c| c.status == "claimed" && c.lease_id == head.lease_id)
        else {
            return Ok(Vec::new());
        };
        let Some(mut payload) = current
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        else {
            return Ok(Vec::new());
        };
        let mut merged = merged_from_payload(&payload);
        let head_route = route_of(&tx, &head.id)?;
        let room = MAX_MERGED.saturating_sub(1 + merged.len());
        let one_shot = head_route.as_ref().is_some_and(|(_, one_shot)| *one_shot);
        if head.run_authorization_id.is_some() || room == 0 || one_shot {
            return Ok(merged);
        }
        let followers = {
            let mut stmt = tx
                .prepare(&format!(
                    "{ROW_SELECT} WHERE i.channel=?1 AND i.account=?2 AND i.conversation=?3 AND i.seq>?4
                        AND i.decision_fastlane=0 ORDER BY i.seq LIMIT ?5"
                ))
                .map_err(|e| e.to_string())?;
            stmt.query_map(
                params![head.channel, head.account, head.conversation, head.seq, room as i64],
                row_from_sql,
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
        };
        let before = merged.len();
        for row in followers {
            let event = row
                .payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .map(|p| p["event"].clone());
            let Some(event) = event.filter(|e| accept(e)) else {
                break;
            };
            if !same_run_shape(head, &head_route, &row, &route_of(&tx, &row.id)?) {
                break;
            }
            let n = tx
                .execute(
                    "UPDATE ingress SET status=?2,reason=?2,lease_id=NULL,lease_until=NULL
                        WHERE id=?1 AND status='ready'",
                    params![row.id, MERGED_STATUS],
                )
                .map_err(|e| e.to_string())?;
            if n != 1 {
                break;
            }
            tx.execute("DELETE FROM ingress_payload WHERE id=?1", [&row.id])
                .map_err(|e| e.to_string())?;
            merged.push(MergedEvent {
                id: row.id,
                received_at: row.received_at,
                event,
            });
        }
        if merged.len() == before {
            return Ok(merged);
        }
        payload["merged"] = merged_json(&merged);
        tx.execute(
            "UPDATE ingress_payload SET payload=?2 WHERE id=?1",
            params![head.id, payload.to_string()],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(merged)
    }
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
