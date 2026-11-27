//! Knowledge side effects execute under the service's per-approval lock.
use serde_json::{Value, json};
use tracing::warn;
use super::{DecisionEnv, Decider};
#[cfg(test)]
use super::locks::take_forced_decide_failure;
use crate::approval::{ApprovalId, chat_admin::DecisionActor};

pub(crate) const AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED: &str = "knowledge_review_decision_unrecorded";

pub(super) async fn quarantine(
    env: &DecisionEnv, approval_id: &ApprovalId, approve: bool,
    _by: &Decider, actor: &DecisionActor,
) -> Result<Value, String> {
    let broker = &env.broker;
    let fresh = match broker.get(approval_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err("找不到這筆審核".into()),
        Err(e) => return Err(format!("讀取審核失敗：{e}")),
    };
    let rec = &fresh;
    if rec.status != crate::approval::ApprovalStatus::Pending {
        return Err(format!("approval {} already {}", rec.id, rec.status.as_str()));
    }
    if rec.is_stale(chrono::Utc::now()) {
        let _ = broker.poll(approval_id).await;
        return Err("這則知識審核已逾期，已自動捨棄，無法再核准。".into());
    }
    if let Err(error) = crate::approval::check_expected_digest(rec) {
        return Err(error);
    }
    let memory_db = rec
        .payload
        .get("memory_db")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let q_agent = rec
        .payload
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(rec.agent_id.as_str())
        .to_string();
    let ids: Vec<String> = rec
        .payload
        .get("quarantined_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // Trust-held claims are promoted with the approver's authority,
    // bound to the digest of the row the card showed (R-H1).
    let promote = rec
        .payload
        .get("promote_on_approve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let claim_digest = rec
        .payload
        .get("claim_digest")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let origin = match (
        rec.payload.get("origin_channel").and_then(|v| v.as_str()),
        rec.payload.get("origin_chat_id").and_then(|v| v.as_str()),
    ) {
        (Some(ch), Some(chat)) => Some((ch.to_string(), chat.to_string())),
        _ => None,
    };

    let mut side_effect = Value::Null;
    match memory_db {
        Some(db) if !ids.is_empty() => {
            match crate::wiki_ingest::apply_quarantine_decision(
                env.home.clone(),
                db.clone(),
                q_agent.clone(),
                ids,
                approve,
                promote,
                claim_digest,
            )
            .await
            {
                Ok(report) => {
                    side_effect = report.side_effect(approve, promote);
                    if !report.held.is_empty() {
                        crate::wiki_ingest::dispatch_quarantine_side_effects(
                            &q_agent,
                            &env.home,
                            &db,
                            &report.held,
                            origin.as_ref(),
                        )
                        .await;
                    }
                }
                Err(e) => {
                    return Err(format!("沒有套用，這筆審核仍待決定，可以再試一次：{e}"));
                }
            }
        }
        _ => warn!("knowledge_quarantine approval missing memory_db/ids in payload"),
    }

    let decided = {
        #[cfg(test)]
        let forced = take_forced_decide_failure(approval_id.as_str());
        #[cfg(not(test))]
        let forced: Option<String> = None;
        match forced {
            Some(e) => Err(e),
            None => broker.decide_as(approval_id, approve, actor).await,
        }
    };
    if let Err(e) = decided {
        // The side effect is applied but the decision is not recorded
        // (the card expired in between, or a real store error): never
        // report success. Say exactly what was applied, and leave an
        // audit row so the mismatch can be traced.
        let action = if approve { "approve" } else { "deny" };
        warn!(approval_id = %approval_id, "knowledge review decision unrecorded: {e}");
        duduclaw_security::audit::append_audit_event(
            &env.home,
            &duduclaw_security::audit::AuditEvent::new(
                AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED,
                &q_agent,
                duduclaw_security::audit::Severity::Warning,
                json!({
                    "approval_id": rec.id.as_str(),
                    "action": action,
                    "side_effect": side_effect,
                    "error": duduclaw_core::truncate_chars(&e, 300),
                }),
            ),
        );
        return Err(format!(
                "已套用（{action}：{side_effect}），但這筆審核的決定沒有記錄成功（{err}）。\
                 資料以已套用的結果為準，已寫入稽核紀錄 {AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED}。",
                err = duduclaw_core::truncate_chars(&e, 300),
            ));
    }
    Ok(
        json!({
            "id": rec.id.as_str(),
            "decided": if approve { "approved" } else { "denied" },
            "side_effect": side_effect,
            "capability_handle": Value::Null,
        }),
    )
}
