//! Creation-only support for RM-194. Decision and execution hooks live elsewhere.
use super::*;

pub(super) const PENDING_LIMIT: &str = "admin_ai_pending_limit";
pub(super) const HOURLY_LIMIT: &str = "admin_ai_hourly_limit";

impl ApprovalBroker {
    /// Keep the normal credential, incarnation, origin and lifecycle-event checks.
    pub(crate) async fn request_admin_ai(
        &self, id: ApprovalId, agent: &str, payload: Value, binding: ExecutionBinding,
    ) -> Result<ApprovalId, String> {
        let admin = payload["admin_user_id"].as_str().ok_or("admin_ai_missing_admin")?;
        let principal = duduclaw_core::HumanPrincipal::user(admin).ok_or("admin_ai_invalid_admin")?.to_string();
        if raising_principal().as_deref() != Some(principal.as_str())
            || binding.run_origin_kind != "ingress" || binding.resume_handler != "live_only_v1"
            || binding.conversation_audience != ConversationAudience::Private {
            return Err("admin_ai_invalid_binding".into());
        }
        let mut rec = self.bound_record(id, RequestKind::Approval, agent,
            "管理員 AI 設定變更確認", payload, binding).await?;
        rec.action_kind = crate::admin_ai::card::ACTION_KIND.into();
        rec.ttl_seconds = 600;
        self.store.insert(&rec).await?;
        Ok(rec.id)
    }
}

/// Runs inside the same IMMEDIATE transaction as insertion; durable across restarts.
pub(super) fn check_rate(tx: &rusqlite::Transaction<'_>, rec: &ApprovalRecord) -> Result<(), String> {
    let user = rec.payload["admin_user_id"].as_str().ok_or("admin_ai_missing_admin")?;
    let (pending, hourly): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(CASE WHEN status='pending'
                AND julianday(created_at)+ttl_seconds/86400.0 > julianday(?2) THEN 1 ELSE 0 END),0),
            COALESCE(SUM(CASE WHEN julianday(created_at) > julianday(?2)-1.0/24.0 THEN 1 ELSE 0 END),0)
         FROM approvals WHERE action_kind='admin_ai_settings' AND json_extract(payload,'$.admin_user_id')=?1",
        params![user, rec.created_at], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|_| "admin_ai_rate_unavailable".to_owned())?;
    if pending >= 3 { return Err(PENDING_LIMIT.into()); }
    if hourly >= 20 { return Err(HOURLY_LIMIT.into()); }
    Ok(())
}
