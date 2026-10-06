//! Scripted human decisions for fixture runs (F1b).
//!
//! A fixture run exercises approval, question and human-gated effect steps
//! without a person: the draft's fixture names the decision per step, and
//! this writes the card already decided, with `decided_by = fixture:<run>`.
//! Nothing is pushed to any channel. Only a fixture run (trigger `fixture`,
//! no activation) can get one; the claim path refuses a fixture decision
//! for an activated run, so a formal run always needs a real person.
use super::*;
use crate::workflow_drafts::FixtureDecision;

/// `action_kind` of a card decided by a fixture script.
pub const FIXTURE_DECISION_KIND: &str = "bound_fixture_decision";

impl ApprovalBroker {
    /// The decided card for one fixture step. Idempotent per key: a retry
    /// returns the stored card, a different contract is refused.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_fixture_decision(
        &self,
        key: &str,
        kind: RequestKind,
        agent_id: &str,
        summary: &str,
        payload: Value,
        binding: ExecutionBinding,
        decision: &FixtureDecision,
    ) -> Result<ApprovalId, String> {
        self.require_fixture_run(&binding).await?;
        let (status, answer) = match (kind, decision) {
            (RequestKind::Approval, FixtureDecision::Approve) => (ApprovalStatus::Approved, None),
            (_, FixtureDecision::Deny) => (ApprovalStatus::Denied, None),
            (RequestKind::Question, FixtureDecision::Answer { text }) => {
                if text.trim().is_empty() || text.len() > 8192 {
                    return Err("fixture answer empty or too long".into());
                }
                (ApprovalStatus::Answered, Some(Value::String(text.clone())))
            }
            _ => return Err("fixture_decision_mismatch".into()),
        };
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!("duduclaw-fixture-decision:{key}").as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        let id = ApprovalId::from(
            uuid::Builder::from_random_bytes(bytes)
                .into_uuid()
                .to_string(),
        );
        let mut rec = self
            .bound_record(id, kind, agent_id, summary, payload, binding)
            .await?;
        rec.action_kind = FIXTURE_DECISION_KIND.into();
        rec.status = status;
        rec.decided_at = Some(Utc::now().to_rfc3339());
        rec.decided_by = Some(format!(
            "fixture:{}",
            rec.binding
                .as_ref()
                .map(|b| b.run_id.as_str())
                .unwrap_or("")
        ));
        rec.answer = answer;
        // No channel destination: a fixture decision is never pushed.
        rec.notify_channel = None;
        rec.notify_chat_id = None;
        self.store.insert_if_absent(&rec).await?;
        let stored = self.get(&rec.id).await?.ok_or("fixture decision missing")?;
        if stored.action_kind != FIXTURE_DECISION_KIND
            || stored.binding != rec.binding
            || stored.status != rec.status
            || payload_hash(&stored.payload) != payload_hash(&rec.payload)
        {
            return Err("fixture decision already exists with a different contract".into());
        }
        Ok(stored.id)
    }

    async fn require_fixture_run(&self, binding: &ExecutionBinding) -> Result<(), String> {
        if binding.run_origin_kind != "workflow" || binding.resume_handler != "workflow_v1" {
            return Err("fixture decisions apply to workflow runs only".into());
        }
        // Read through the broker's own attached reader: opening and closing
        // another handle on workflow.db would drop this process's locks.
        let conn = self.store.conn.lock().await;
        self.attach_workflow_reader(&conn)?;
        let raw: String = conn
            .query_row(
                "SELECT record_json FROM approval_workflow.workflow_runs WHERE run_id=?1",
                params![binding.run_id],
                |r| r.get(0),
            )
            .map_err(|_| "fixture run missing")?;
        drop(conn);
        let run: crate::workflow::WorkflowRun =
            serde_json::from_str(&raw).map_err(|_| "fixture run corrupt")?;
        if run.activation_id.is_some()
            || run.grant.is_some()
            || !matches!(run.trigger, crate::workflow::Trigger::Fixture { .. })
        {
            return Err("fixture decisions are refused for a formal run".into());
        }
        Ok(())
    }
}
