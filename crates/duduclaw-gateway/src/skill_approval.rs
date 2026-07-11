//! WP8 — skill "time-saving" approval flow.
//!
//! When a skill is created (via synthesis or the skill-creation dialogue), the
//! agent asks the operator "roughly how many minutes does this save per use?"
//! and routes a **skill-activation approval** to the manager's Approval Inbox.
//! The skill stays a draft until the manager approves; on approval the estimate
//! is written to the skill's `estimated_minutes_saved` metadata, which the WP10
//! leaderboard aggregates.
//!
//! This module owns the deterministic backend: the action-kind string, the
//! payload shape, and the request helper. The conversational "ask the estimate"
//! step is a prompt template in the skill-creation flow; the approval record is
//! what makes the decision auditable and type-checkable (WP17 later collapses
//! `action_kind` strings into an `ApprovalKind` enum — `skill_activation` is one
//! of its variants).

use serde_json::json;

use crate::approval::{ApprovalBroker, ApprovalId};

/// Stable `action_kind` for a skill-activation approval. Kept as a constant so
/// the WP17 enum migration and the dashboard/channel renderers agree on the
/// exact string.
pub const ACTION_KIND_SKILL_ACTIVATION: &str = "skill_activation";

/// Default TTL for a skill-activation approval: 7 days. A skill sitting in the
/// inbox unactioned for a week reverts to "draft" (deny-on-expiry, fail-closed).
pub const SKILL_ACTIVATION_TTL_SECONDS: i64 = 7 * 24 * 3600;

/// Inputs for a skill-activation approval request.
#[derive(Debug, Clone)]
pub struct SkillActivationRequest {
    /// Machine-stable skill id (registry key).
    pub skill_name: String,
    /// Localised (zh-TW) display name shown to the manager.
    pub display_name: String,
    /// Localised description.
    pub description: String,
    /// Minutes saved per use, as estimated in the creation dialogue. `None`
    /// when the creator declined to answer (treated as 0 by the leaderboard).
    pub estimated_minutes_saved: Option<u32>,
    /// Session that produced the skill, for the manager to trace context.
    pub source_session: Option<String>,
}

/// Build the JSON payload persisted with the approval. Kept pure so it can be
/// unit-tested and reused by the dashboard/channel renderers.
pub fn build_payload(req: &SkillActivationRequest) -> serde_json::Value {
    json!({
        "kind": ACTION_KIND_SKILL_ACTIVATION,
        "skill_name": req.skill_name,
        "display_name": req.display_name,
        "description": req.description,
        "estimated_minutes_saved": req.estimated_minutes_saved,
        "source_session": req.source_session,
    })
}

/// One-line manager-facing summary for the inbox card / channel push.
pub fn build_summary(req: &SkillActivationRequest) -> String {
    match req.estimated_minutes_saved {
        Some(m) => format!("啟用技能「{}」— 預估每次省 {m} 分鐘", req.display_name),
        None => format!("啟用技能「{}」— 未提供省時估計", req.display_name),
    }
}

/// Create the pending skill-activation approval. Returns the approval id the
/// caller can poll / surface. Fail-closed via the broker's TTL (deny-on-expiry).
pub async fn request_skill_activation(
    broker: &ApprovalBroker,
    agent_id: &str,
    req: &SkillActivationRequest,
) -> Result<ApprovalId, String> {
    broker
        .request(
            agent_id,
            ACTION_KIND_SKILL_ACTIVATION,
            &build_summary(req),
            build_payload(req),
            SKILL_ACTIVATION_TTL_SECONDS,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{ApprovalBroker, ApprovalStore};
    use std::sync::Arc;

    fn sample(minutes: Option<u32>) -> SkillActivationRequest {
        SkillActivationRequest {
            skill_name: "compress-context".into(),
            display_name: "壓縮對話".into(),
            description: "把過長的對話壓縮".into(),
            estimated_minutes_saved: minutes,
            source_session: Some("telegram:123".into()),
        }
    }

    #[test]
    fn payload_carries_estimate_and_names() {
        let p = build_payload(&sample(Some(15)));
        assert_eq!(p["skill_name"], "compress-context");
        assert_eq!(p["display_name"], "壓縮對話");
        assert_eq!(p["estimated_minutes_saved"], 15);
        assert_eq!(p["kind"], ACTION_KIND_SKILL_ACTIVATION);
    }

    #[test]
    fn summary_handles_missing_estimate() {
        assert!(build_summary(&sample(None)).contains("未提供"));
        assert!(build_summary(&sample(Some(30))).contains("30 分鐘"));
    }

    #[tokio::test]
    async fn request_persists_pending_approval() {
        let store = Arc::new(ApprovalStore::open_in_memory().unwrap());
        let broker = ApprovalBroker::new(store);
        let id = request_skill_activation(&broker, "agnes", &sample(Some(20)))
            .await
            .unwrap();
        let rec = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.action_kind, ACTION_KIND_SKILL_ACTIVATION);
        assert_eq!(rec.payload["estimated_minutes_saved"], 20);
        assert!(rec.status.as_str() == "pending");
    }
}
