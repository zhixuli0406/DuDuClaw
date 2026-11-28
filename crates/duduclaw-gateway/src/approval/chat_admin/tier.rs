use std::path::Path;

use crate::approval::{operator_cli_gate, ApprovalRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    T1,
    T2,
    T3,
}

/// Stable configuration/UI keys: selectable action kinds and the record-level guard.
pub(crate) const STEP_UP_KIND_KEYS: &[&str] = &[
    crate::expert_skills::SKILLS_ACTION_KIND,
    crate::approval::WORKFLOW_ACTIVATION_KIND,
    crate::agent_files::approval::ACTION_KIND,
    crate::org_change_approval::ACTION_KIND,
    crate::autopilot_approval::ACTION_KIND,
    crate::approval::cli_egress::KIND,
    crate::admin_ai::card::ACTION_KIND,
    "egress_guard",
];

pub(crate) fn step_up_selectable(kind_key: &str) -> bool {
    STEP_UP_KIND_KEYS.contains(&kind_key)
}

fn dashboard_only(kind: &str) -> bool {
    kind == crate::isolation_background::PERSONAL_DELEGATION_KIND
        || operator_cli_gate::spec_for(kind).is_some()
}

pub(crate) fn tier(rec: &ApprovalRecord) -> Tier {
    let kind = rec.action_kind.as_str();
    if dashboard_only(kind) {
        Tier::T3
    } else if crate::approval::is_outbound_data_card(rec) {
        Tier::T2
    } else if kind == crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE
        || kind == crate::knowledge::add_source_approval::ACTION_KIND {
        Tier::T1
    } else if STEP_UP_KIND_KEYS.iter().any(|key| *key != "egress_guard" && *key == kind) {
        Tier::T2
    } else {
        Tier::T3
    }
}

/// Keep the existing requester-confirms-own-mail route outside chat-admin.
/// Without a home we cannot establish mail eligibility: use the fixed tier
/// table, not a guessed exception. This is classification, not authorization;
/// callers must still validate live identity, configuration and receipts.
pub(crate) fn chat_tier(home: Option<&Path>, rec: &ApprovalRecord) -> Tier {
    if home.is_some_and(|home| crate::approval::mail_requester::eligible(home, rec)) {
        Tier::T3
    } else {
        tier(rec)
    }
}

pub(crate) fn kind_key(rec: &ApprovalRecord) -> Option<&'static str> {
    if tier(rec) == Tier::T3 {
        return None;
    }
    let kind = rec.action_kind.as_str();
    if kind != crate::approval::cli_egress::KIND
        && crate::approval::is_outbound_data_card(rec) {
        return Some("egress_guard");
    }
    [crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE,
        crate::knowledge::add_source_approval::ACTION_KIND]
        .into_iter().chain(STEP_UP_KIND_KEYS.iter().copied()
            .filter(|key| *key != "egress_guard"))
        .find(|key| *key == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: &str, payload: serde_json::Value) -> ApprovalRecord {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(), "agent_id": "test",
            "action_kind": kind, "summary": "test", "payload": payload,
            "status": "pending", "created_at": "2026-11-27T00:00:00Z",
            "decided_at": null, "decided_by": null, "ttl_seconds": 3600
        })).unwrap()
    }

    #[test]
    fn fixed_tier_table() {
        let mut rows = vec![
            (crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE, Tier::T1),
            (crate::knowledge::add_source_approval::ACTION_KIND, Tier::T1),
            (crate::expert_skills::SKILLS_ACTION_KIND, Tier::T2),
            (crate::approval::WORKFLOW_ACTIVATION_KIND, Tier::T2),
            (crate::agent_files::approval::ACTION_KIND, Tier::T2),
            (crate::org_change_approval::ACTION_KIND, Tier::T2),
            (crate::autopilot_approval::ACTION_KIND, Tier::T2),
            (crate::admin_ai::card::ACTION_KIND, Tier::T2),
            (crate::approval::cli_egress::KIND, Tier::T2),
            (crate::isolation_background::PERSONAL_DELEGATION_KIND, Tier::T3),
            ("unknown", Tier::T3),
            ("expert_skills_enable_extra", Tier::T3),
            ("egress_guard", Tier::T3),
        ];
        rows.extend(operator_cli_gate::OPERATOR_CLI_KINDS.iter()
            .map(|spec| (spec.kind, Tier::T3)));
        for (kind, expected) in rows {
            let rec = record(kind, serde_json::json!({}));
            assert_eq!(tier(&rec), expected, "{kind}");
            assert_eq!(chat_tier(None, &rec), expected, "{kind}");
            if expected == Tier::T3 {
                assert_eq!(kind_key(&rec), None, "{kind}");
            } else {
                assert_eq!(kind_key(&rec), Some(kind), "{kind}");
            }
        }
    }

    #[test]
    fn guard_presence_cannot_override_t3_exclusions() {
        for guard in [serde_json::json!({}), serde_json::Value::Null, serde_json::json!(false)] {
            let payload = serde_json::json!({"egress_guard": guard});
            let rec = record("ordinary", payload.clone());
            assert_eq!(tier(&rec), Tier::T2);
            assert_eq!(chat_tier(None, &rec), Tier::T2);
            assert_eq!(kind_key(&rec), Some("egress_guard"));
            let cli = record(crate::approval::cli_egress::KIND, payload.clone());
            assert_eq!(kind_key(&cli), Some(crate::approval::cli_egress::KIND));
            for kind in [crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE,
                crate::knowledge::add_source_approval::ACTION_KIND] {
                let rec = record(kind, payload.clone());
                assert_eq!(tier(&rec), Tier::T2, "{kind}");
                assert_eq!(kind_key(&rec), Some("egress_guard"));
            }
            for kind in operator_cli_gate::OPERATOR_CLI_KINDS.iter().map(|spec| spec.kind)
                .chain(std::iter::once(crate::isolation_background::PERSONAL_DELEGATION_KIND)) {
                let rec = record(kind, payload.clone());
                assert_eq!(tier(&rec), Tier::T3, "{kind}");
                assert_eq!(kind_key(&rec), None, "{kind}");
            }
        }
    }

    #[test]
    fn only_t2_keys_are_selectable() {
        for key in STEP_UP_KIND_KEYS { assert!(step_up_selectable(key)); }
        for key in [crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE,
            crate::knowledge::add_source_approval::ACTION_KIND, "unknown", "CLI_EGRESS"] {
            assert!(!step_up_selectable(key), "{key}");
        }
    }
}
