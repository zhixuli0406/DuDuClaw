//! v1.82 — single-use consumption of an approved `autopilot_action` card.
//!
//! Same idea as [`super::action_grant::verify_and_consume`]: the approval is
//! bound to the exact row the executor verified (kind, requester, payload
//! text, creation time, TTL) and moved `approved → invalidated` exactly once,
//! inside one IMMEDIATE transaction, BEFORE the frozen action runs. A second
//! consumer, a changed row or a lost race gets `Ok(false)` and must not run
//! anything. The caller (`autopilot_approval`) checks the payload digest
//! against its own ledger row before calling this.
//!
//! Unlike an `ActionGrant`, the approval TTL is not re-judged here: the TTL
//! bounds the *decision* (`decide` refuses a stale card), and the executor
//! bounds how long after the decision an approved action may still run
//! (`autopilot_approval::APPROVED_EXECUTION_WINDOW_SECS`). That keeps an
//! approval granted just before a gateway restart executable after it.

use super::*;

impl ApprovalBroker {
    /// Move the approved autopilot card `rec` to `invalidated` with `reason`.
    ///
    /// `Ok(true)`: this caller consumed it and may proceed. `Ok(false)`: the
    /// row is not approved any more or differs from `rec` (already consumed,
    /// withdrawn, edited) — do nothing. `Err`: store failure, or the card's
    /// employee incarnation no longer matches (the card is invalidated).
    pub async fn consume_autopilot_approval(&self, rec: &ApprovalRecord, reason: &str) -> Result<bool, String> {
        if rec.action_kind != crate::autopilot_approval::ACTION_KIND
            || rec.request_kind != RequestKind::Approval
            || rec.binding.is_some()
        {
            return Ok(false);
        }
        let reason = duduclaw_core::truncate_chars(reason, 200);
        let mut conn = self.store.conn.lock().await;
        let tx = begin_live_write(&mut conn)?;
        // SR5: the same consumption-time incarnation rule every other consumer
        // of an approved card applies (the requester here is the system name
        // `autopilot`, which has no confirmed employee row).
        let stored: Option<Option<i64>> = tx
            .query_row(
                "SELECT employee_incarnation FROM approvals WHERE id=?1 AND status='approved'",
                params![rec.id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(stored) = stored else { return Ok(false) };
        let home = self.store.db_path.as_deref().and_then(Path::parent);
        if let Err((why, message)) = super::store::card_consumable(home, &rec.agent_id, stored) {
            tx.execute(
                "UPDATE approvals SET status='invalidated', invalidated_reason=?1 WHERE id=?2 AND status='approved'",
                params![why, rec.id.as_str()],
            )
            .map_err(|e| e.to_string())?;
            commit_live_write(tx)?;
            return Err(message);
        }
        let n = tx
            .execute(
                "UPDATE approvals SET status='invalidated', invalidated_reason=?1
                 WHERE id=?2 AND status='approved' AND action_kind=?3 AND agent_id=?4
                   AND payload=?5 AND created_at=?6 AND ttl_seconds=?7 AND binding_json IS NULL",
                params![
                    reason,
                    rec.id.as_str(),
                    rec.action_kind,
                    rec.agent_id,
                    rec.payload.to_string(),
                    rec.created_at,
                    rec.ttl_seconds,
                ],
            )
            .map_err(|e| e.to_string())?;
        commit_live_write(tx)?;
        Ok(n == 1)
    }

    /// Every `autopilot_action` card (any status) whose payload names
    /// `dispatch_id`. Used only by boot recovery for a ledger row that crashed
    /// between "row written" and "approval id recorded".
    pub async fn autopilot_cards_for_dispatch(&self, dispatch_id: &str) -> Result<Vec<ApprovalRecord>, String> {
        Ok(self
            .list_by_kind(crate::autopilot_approval::ACTION_KIND)
            .await?
            .into_iter()
            .filter(|rec| {
                rec.payload
                    .get(crate::autopilot_approval::PAYLOAD_KEY)
                    .and_then(|d| d.get("dispatch_id"))
                    .and_then(Value::as_str)
                    == Some(dispatch_id)
            })
            .collect())
    }
}
