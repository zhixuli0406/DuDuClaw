//! Operator resolution of an event that needs a human (review I-HIGH-5).
//!
//! Three actions, each a CAS on (revision, attempt, status):
//! - `close`: stop here. Allowed for every waiting state.
//! - `retry`: only for events proven not executed (`failed_before_dispatch`,
//!   or `quarantined` because the route/authority could not be read).
//! - `rerun`: only for `uncertain` / `undelivered` (the turn may have run or
//!   did run). Needs `confirm_duplicate_risk`; the confirmation, the
//!   operator's reason and an optional provider receipt are stored with the
//!   new run's authorization.
//!
//! `retry` and `rerun` start a new run id; the next claim of the event runs
//! that run. The LINE reply token keeps its original window: once it has
//! passed, the answer of a new run can only go out by Push. So with
//! `line_late_reply = "fail"` both are refused for an event past its reply
//! window ([`LATE_FAIL_REFUSAL`]): the turn would run (side effects, cost)
//! with no way to deliver its answer.

use super::config::LateReply;
use super::{IngressStore, REPLY_TOKEN_SECONDS, ROW_SELECT_BY_ID, row_from_sql};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

/// Longest operator note accepted (bytes).
pub(crate) const NOTE_MAX_BYTES: usize = 2000;
/// Longest provider receipt accepted (bytes).
pub(crate) const RECEIPT_MAX_BYTES: usize = 200;
/// Why `retry` / `rerun` is refused under `line_late_reply = "fail"`.
pub(crate) const LATE_FAIL_REFUSAL: &str = "回覆期限已過，目前設定為不改用 Push（line_late_reply = \"fail\"），重新執行不會有回覆；要送達請把 line_late_reply 設為 \"push\"，或直接結案。";

/// Whether a new run of an event received at `received_at` could deliver
/// anything: the reply token is still within its window, or Push is allowed.
pub(crate) fn rerun_can_deliver(received_at: i64, now: i64, late: LateReply) -> bool {
    late == LateReply::Push || now <= received_at + REPLY_TOKEN_SECONDS
}

/// One operator resolution.
pub(crate) struct ResolveRequest<'a> {
    pub id: &'a str,
    pub expected_revision: &'a str,
    pub expected_attempt: i64,
    pub action: &'a str,
    pub confirm_duplicate_risk: bool,
    pub actor: &'a str,
    pub note: &'a str,
    pub provider_receipt: Option<&'a str>,
    pub now: i64,
    /// The current `[channel_ingress] line_late_reply`.
    pub late_reply: LateReply,
}

/// Statuses an operator may act on.
const RESOLVABLE: &[&str] = &[
    "uncertain",
    "quarantined",
    "undelivered",
    "failed_before_dispatch",
];

/// Whether `retry` (no duplicate risk) applies to a row in this state.
pub(crate) fn retry_allowed(status: &str, reason: Option<&str>) -> bool {
    status == "failed_before_dispatch"
        || (status == "quarantined"
            && matches!(
                reason,
                Some("revalidation_unavailable") | Some(super::snapshot::SNAPSHOT_UNAVAILABLE)
            ))
}

/// Whether `rerun` (explicit duplicate risk) applies to a row in this state.
/// An event held after a device restore never ran here, but the device the
/// backup came from may have handled it, so it is a rerun, not a retry.
pub(crate) fn rerun_allowed(status: &str, reason: Option<&str>) -> bool {
    matches!(status, "uncertain" | "undelivered")
        || (status == "quarantined" && reason == Some(super::RESTORED_REASON))
}

fn validate(req: &ResolveRequest<'_>) -> Result<(), String> {
    if req.actor.is_empty() || req.note.trim().is_empty() {
        return Err("operator and note required".into());
    }
    if req.note.len() > NOTE_MAX_BYTES {
        return Err("note too long".into());
    }
    if let Some(r) = req.provider_receipt {
        if r.trim().is_empty() || r.len() > RECEIPT_MAX_BYTES || r.chars().any(char::is_control) {
            return Err("provider_receipt must be 1-200 bytes without control characters".into());
        }
    }
    match req.action {
        "close" | "retry" => Ok(()),
        "rerun" if req.confirm_duplicate_risk => Ok(()),
        "rerun" => Err("confirm_duplicate_risk required".into()),
        _ => Err("action must be close, retry or rerun".into()),
    }
}

impl IngressStore {
    /// Pre-F2 call shape: `close` / `retry` / `rerun` without a receipt.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resolve(
        &self,
        id: &str,
        revision: &str,
        action: &str,
        confirm: bool,
        actor: &str,
        note: &str,
        now: i64,
        expected_attempt: i64,
    ) -> Result<(), String> {
        self.resolve_request(&ResolveRequest {
            id,
            expected_revision: revision,
            expected_attempt,
            action,
            confirm_duplicate_risk: confirm,
            actor,
            note,
            provider_receipt: None,
            now,
            late_reply: LateReply::Push,
        })
        .await
    }

    pub(crate) async fn resolve_request(&self, req: &ResolveRequest<'_>) -> Result<(), String> {
        validate(req)?;
        let ready = {
            let mut conn = self.connection().lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            let row = tx
                .query_row(ROW_SELECT_BY_ID, [req.id], row_from_sql)
                .optional()
                .map_err(|e| e.to_string())?
                .ok_or("ingress not found")?;
            if row.revision != req.expected_revision
                || row.attempt != req.expected_attempt
                || !RESOLVABLE.contains(&row.status.as_str())
            {
                return Err("ingress revision/status conflict".into());
            }
            match req.action {
                "retry" if !retry_allowed(&row.status, row.reason.as_deref()) => {
                    return Err(if rerun_allowed(&row.status, row.reason.as_deref()) {
                        "this event may already have run; use action \"rerun\" with confirm_duplicate_risk"
                            .into()
                    } else {
                        "quarantined events can only be closed".into()
                    });
                }
                "rerun" if !rerun_allowed(&row.status, row.reason.as_deref()) => {
                    return Err(
                        "rerun is only for uncertain, undelivered or restored events".into(),
                    );
                }
                "retry" | "rerun" if row.payload.is_none() => {
                    return Err("quarantined or expired payload cannot retry".into());
                }
                "retry" | "rerun"
                    if !rerun_can_deliver(row.received_at, req.now, req.late_reply) =>
                {
                    return Err(LATE_FAIL_REFUSAL.into());
                }
                _ => {}
            }
            let mut next_run = row.run_id.clone();
            let mut next_authorization = row.run_authorization_id.clone();
            if req.action != "close" {
                let predecessor: String = if req.action == "rerun" {
                    let last: Option<String> = tx
                        .query_row(
                            "SELECT operation_id FROM ingress_attempts WHERE ingress_id=?1
                                ORDER BY ordinal DESC LIMIT 1",
                            [req.id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| e.to_string())?;
                    match last {
                        Some(op) => op,
                        // Held after a restore: no attempt on this device.
                        None if row.reason.as_deref() == Some(super::RESTORED_REASON) => {
                            format!("{}:{}", super::RESTORED_REASON, row.attempt)
                        }
                        None => {
                            return Err("immutable predecessor receipt required for rerun".into());
                        }
                    }
                } else {
                    // Proven not executed: there is no dispatch attempt.
                    format!("not_dispatched:{}", row.attempt)
                };
                let authorization_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO ingress_retry_authorizations VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        authorization_id,
                        req.id,
                        predecessor,
                        req.actor,
                        req.note,
                        req.now
                    ],
                )
                .map_err(|e| e.to_string())?;
                tx.execute(
                    "INSERT INTO ingress_run_confirmations VALUES (?1,?2,?3,?4)",
                    params![
                        authorization_id,
                        req.action,
                        req.confirm_duplicate_risk,
                        req.provider_receipt
                    ],
                )
                .map_err(|e| e.to_string())?;
                next_run = authorization_id.clone();
                next_authorization = Some(authorization_id);
            }
            let ready = req.action != "close";
            tx.execute(
                "UPDATE ingress SET status=?2,reason='operator_resolution',lease_id=NULL,lease_until=NULL,run_id=?3,
                    run_authorization_id=?4,retry_at=NULL,unavailable_count=0 WHERE id=?1",
                params![
                    req.id,
                    if ready { "ready" } else { "closed" },
                    next_run,
                    next_authorization
                ],
            )
            .map_err(|e| e.to_string())?;
            if !ready {
                // Nothing will run again: the message text and reply token go now.
                tx.execute("DELETE FROM ingress_payload WHERE id=?1", [req.id])
                    .map_err(|e| e.to_string())?;
            }
            tx.execute(
                "INSERT INTO ingress_resolutions(ingress_id,actor,action,note,at,provider_receipt,
                    confirmed_duplicate_risk) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    req.id,
                    req.actor,
                    req.action,
                    req.note,
                    req.now,
                    req.provider_receipt,
                    req.confirm_duplicate_risk
                ],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            ready
        };
        if ready {
            self.work_signal().notify_waiters();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_ingress::AcceptedEvent;

    fn event(id: &str) -> AcceptedEvent {
        AcceptedEvent {
            decision_fastlane: false,
            decision_binding: None,
            event_id: id.into(),
            account: "account".into(),
            revision: "r1".into(),
            authorization_revision: "a1".into(),
            conversation: "chat".into(),
            payload: "{}".into(),
        }
    }

    fn req<'a>(id: &'a str, attempt: i64, action: &'a str, confirm: bool) -> ResolveRequest<'a> {
        ResolveRequest {
            id,
            expected_revision: "r1",
            expected_attempt: attempt,
            action,
            confirm_duplicate_risk: confirm,
            actor: "operator",
            note: "checked with the customer",
            provider_receipt: Some("line-request-1"),
            now: 50,
            late_reply: LateReply::Push,
        }
    }

    #[tokio::test]
    async fn executed_events_need_rerun_and_persist_confirmation_and_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(&[event("a")], 10).await.unwrap();
        let row = s.claim(10).await.unwrap().unwrap();
        s.transition(&row, "claimed", "dispatching", None)
            .await
            .unwrap();
        s.transition(&row, "dispatching", "undelivered", Some("reply_rejected"))
            .await
            .unwrap();
        // A plain retry cannot run an executed turn again.
        assert!(
            s.resolve_request(&req(&row.id, row.attempt, "retry", true))
                .await
                .unwrap_err()
                .contains("rerun")
        );
        assert!(
            s.resolve_request(&req(&row.id, row.attempt, "rerun", false))
                .await
                .is_err()
        );
        s.resolve_request(&req(&row.id, row.attempt, "rerun", true))
            .await
            .unwrap();
        let inspected = s.inspect(&row.id, None, None).await.unwrap().unwrap();
        let auth = &inspected["retry_authorizations"][0];
        assert_eq!(auth["action"], "rerun");
        assert_eq!(auth["confirmed_duplicate_risk"], true);
        assert_eq!(auth["provider_receipt"], "line-request-1");
        assert_eq!(inspected["event"]["status"], "ready");
    }

    #[tokio::test]
    async fn proven_not_executed_events_retry_without_duplicate_risk() {
        let dir = tempfile::tempdir().unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(&[event("a")], 10).await.unwrap();
        let row = s.claim(10).await.unwrap().unwrap();
        s.transition(
            &row,
            "claimed",
            "failed_before_dispatch",
            Some("late_reply_expired"),
        )
        .await
        .unwrap();
        assert!(
            s.resolve_request(&req(&row.id, row.attempt, "rerun", true))
                .await
                .is_err()
        );
        s.resolve_request(&req(&row.id, row.attempt, "retry", false))
            .await
            .unwrap();
        let again = s.claim(11).await.unwrap().unwrap();
        assert_ne!(again.run_id, row.run_id);
        // A changed route stays close-only.
        s.transition(&again, "claimed", "quarantined", Some("changed"))
            .await
            .unwrap();
        assert!(
            s.resolve_request(&req(&row.id, again.attempt, "retry", false))
                .await
                .is_err()
        );
        s.resolve_request(&req(&row.id, again.attempt, "close", false))
            .await
            .unwrap();
        let closed = s.list().await.unwrap().remove(0);
        assert_eq!(closed.status, "closed");
        assert!(closed.payload.is_none(), "closing drops the payload");
    }

    #[tokio::test]
    async fn fail_policy_refuses_new_runs_past_the_reply_window() {
        let dir = tempfile::tempdir().unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(&[event("a")], 10).await.unwrap();
        let row = s.claim(10).await.unwrap().unwrap();
        s.transition(&row, "claimed", "dispatching", None)
            .await
            .unwrap();
        s.transition(&row, "dispatching", "undelivered", Some("reply_rejected"))
            .await
            .unwrap();
        let strict = |now: i64| ResolveRequest {
            now,
            late_reply: LateReply::Fail,
            ..req(&row.id, row.attempt, "rerun", true)
        };
        // Past the window (received 10, now 71): refused with the explanation.
        let err = s.resolve_request(&strict(71)).await.unwrap_err();
        assert!(err.contains("line_late_reply"), "{err}");
        // Still inside the window, or Push allowed: accepted.
        assert!(rerun_can_deliver(10, 70, LateReply::Fail));
        assert!(rerun_can_deliver(10, 10_000, LateReply::Push));
        // Closing stays possible.
        s.resolve_request(&ResolveRequest {
            action: "close",
            ..strict(71)
        })
        .await
        .unwrap();

        s.append(&[event("b")], 10).await.unwrap();
        let b = s.claim(10).await.unwrap().unwrap();
        s.transition(&b, "claimed", "failed_before_dispatch", Some("late_reply_expired"))
            .await
            .unwrap();
        let retry = ResolveRequest {
            now: 500,
            late_reply: LateReply::Fail,
            ..req(&b.id, b.attempt, "retry", false)
        };
        assert!(s.resolve_request(&retry).await.unwrap_err().contains("line_late_reply"));
        s.resolve_request(&ResolveRequest {
            late_reply: LateReply::Push,
            ..retry
        })
        .await
        .unwrap();
    }
}
