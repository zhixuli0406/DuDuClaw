//! Atomic decisions and observation of external decisions for one AG-UI turn.

use super::*;

/// Compare the request that was interrupted, ignoring delivery bookkeeping
/// and its later decision. No client-supplied payload supplies this snapshot.
pub(crate) fn same_ag_ui_request(a: &ApprovalRecord, b: &ApprovalRecord) -> bool {
    a.id == b.id && a.agent_id == b.agent_id && a.action_kind == b.action_kind
        && a.summary == b.summary && a.payload == b.payload
        && a.created_at == b.created_at && a.ttl_seconds == b.ttl_seconds
        && a.request_kind == b.request_kind && a.binding == b.binding
        && a.simulation == b.simulation && a.on_behalf_of == b.on_behalf_of
}

impl ApprovalBroker {
    /// The caller checks live user authority before entering this transaction.
    /// Pending snapshots request decisions; terminal snapshots only acknowledge
    /// existing decisions, including cards that AG-UI cannot itself decide.
    pub(crate) async fn decide_ag_ui_batch(
        &self, session: &str, turn: &str, decisions: &[(ApprovalRecord, bool)], decided_by: &str,
    ) -> Result<(), String> {
        if session.is_empty() || turn.is_empty() || decisions.is_empty() || decisions.len() > 256 {
            return Err("invalid AG-UI decision batch".into());
        }
        let mut seen = HashSet::new();
        if decisions.iter().any(|(rec, _)| !seen.insert(rec.id.as_str())) {
            return Err("duplicate AG-UI decision".into());
        }
        let changed = {
            let mut conn = self.store.conn.lock().await;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("begin AG-UI decisions: {e}"))?;
            let now = Utc::now();
            let mut changed = Vec::new();
            for (expected, approve) in decisions {
                let current = super::store::record_in_connection(&tx, &expected.id)?
                    .ok_or("approval disappeared")?;
                if !same_ag_ui_request(expected, &current) || current.status != expected.status
                    || current.invalidated_reason.is_some() || current.answer != expected.answer
                    || current.decided_at != expected.decided_at || current.decided_by != expected.decided_by {
                    return Err("approval changed before resume".into());
                }
                let live: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM approvals a JOIN approval_turn_origins o ON o.approval_id=a.id
                     WHERE a.id=?1 AND a.origin_session_id IS NOT NULL AND a.origin_turn_id IS NOT NULL
                     AND o.session_id=?2 AND o.turn_id=?3
                     AND NOT EXISTS (SELECT 1 FROM cancelled_approval_turns c
                         WHERE c.session_id=o.session_id AND c.turn_id=o.turn_id))",
                    params![current.id.as_str(), session, turn], |r| r.get(0),
                ).map_err(|e| format!("check AG-UI approval origin: {e}"))?;
                if !live { return Err("approval turn is not live".into()); }
                check_expected_digest(&current)?;
                let status = if *approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied };
                if current.status == ApprovalStatus::Pending {
                    if current.is_stale(now) || current.request_kind != RequestKind::Approval
                        || current.binding.is_some()
                        || !matches!(current.action_kind.as_str(), "mcp_call" | "mcp_install")
                        || crate::approval_notify::is_dashboard_only_record(&current)
                        || current.payload.get("scope_epoch").is_some()
                        || current.payload.get("egress_guard").is_some() {
                        return Err("approval cannot be decided by AG-UI".into());
                    }
                    let n = tx.execute(
                        "UPDATE approvals SET status=?1, decided_by=?2, decided_at=?3
                         WHERE id=?4 AND status='pending'",
                        params![status.as_str(), decided_by, now.to_rfc3339(), current.id.as_str()],
                    ).map_err(|e| format!("write AG-UI decision: {e}"))?;
                    if n != 1 { return Err("approval decision conflict".into()); }
                    changed.push((current, status));
                } else {
                    let decided = current.decided_at.as_deref()
                        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| value.with_timezone(&Utc));
                    let created = DateTime::parse_from_rfc3339(&current.created_at).ok()
                        .map(|value| value.with_timezone(&Utc));
                    if current.status != status || current.decided_by.as_deref().is_none_or(str::is_empty)
                        || !matches!((created, decided, current.expires_at()),
                            (Some(created), Some(decided), Some(expires)) if created <= decided && decided < expires) {
                        return Err("approval has no matching legal decision".into());
                    }
                }
            }
            // An approval raised just before suspension must not escape coverage.
            let pending = {
                let mut stmt = tx.prepare(
                    "SELECT a.id FROM approvals a JOIN approval_turn_origins o ON o.approval_id=a.id
                     WHERE o.session_id=?1 AND o.turn_id=?2 AND a.status='pending' LIMIT 257",
                ).map_err(|e| format!("prepare AG-UI coverage: {e}"))?;
                stmt.query_map(params![session, turn], |r| r.get::<_, String>(0))
                    .map_err(|e| format!("query AG-UI coverage: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("read AG-UI coverage: {e}"))?
            };
            if !pending.is_empty() { return Err("incomplete AG-UI interrupt coverage".into()); }
            tx.commit().map_err(|e| format!("commit AG-UI decisions: {e}"))?;
            changed
        };
        // Neither lifecycle observers nor polling runtimes see partial decisions.
        for (rec, status) in changed {
            self.store.emit_lifecycle("approval.decided", &rec, status).await;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "ag_ui_tests.rs"]
mod tests;
