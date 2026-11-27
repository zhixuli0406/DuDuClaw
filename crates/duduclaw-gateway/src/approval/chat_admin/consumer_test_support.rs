//! Deliberately forged persisted decisions for downstream authority tests.

use rusqlite::{Connection, params};

pub(crate) const USER: &str = "3f2afc12-4f13-4a28-8b7f-df31eadbb013";

#[derive(Clone, Copy, Debug)]
pub(crate) enum Evidence {
    Missing,
    Matching,
    DifferentUser,
    DifferentChannel,
    DifferentStep,
    Denied,
    TapRequiresStep,
    WebRequiresStep,
    TotpRequiresStep,
    Unreadable,
}

pub(crate) const CASES: &[(Evidence, bool)] = &[
    (Evidence::Matching, true),
    (Evidence::DifferentUser, false),
    (Evidence::DifferentChannel, false),
    (Evidence::DifferentStep, false),
    (Evidence::Denied, false),
    (Evidence::TapRequiresStep, false),
    (Evidence::WebRequiresStep, true),
    (Evidence::TotpRequiresStep, true),
    (Evidence::Unreadable, false),
];

/// Bypass decision ingress intentionally: consumers must check durable evidence.
pub(crate) fn install(conn: &Connection, id: &str, evidence: Evidence) {
    let decision_step = match evidence {
        Evidence::WebRequiresStep => "web",
        Evidence::TotpRequiresStep => "totp",
        _ => "tap",
    };
    conn.execute(
        "UPDATE approvals SET status='approved', decided_by=?2, decided_at=?3 WHERE id=?1",
        params![id, format!("chat-admin:{USER}:telegram:{decision_step}"), chrono::Utc::now().to_rfc3339()],
    ).unwrap();
    if matches!(evidence, Evidence::Missing) { return; }
    if matches!(evidence, Evidence::Unreadable) {
        conn.execute_batch("DROP TABLE chat_admin_decisions").unwrap();
        return;
    }
    let user = if matches!(evidence, Evidence::DifferentUser) {
        "5d161ec0-c99d-4f32-83fd-8acbde73de58"
    } else { USER };
    let channel = if matches!(evidence, Evidence::DifferentChannel) { "googlechat" } else { "telegram" };
    let step = if matches!(evidence, Evidence::DifferentStep) { "web" } else { decision_step };
    let required = matches!(evidence, Evidence::TapRequiresStep | Evidence::WebRequiresStep | Evidence::TotpRequiresStep);
    conn.execute(
        "INSERT INTO chat_admin_decisions
         (approval_id,card_id,user_id,channel,step,step_required,digest,approve,decided_at)
         VALUES (?1,?2,?3,?4,?5,?6,'test-digest',?7,?8)",
        params![id, uuid::Uuid::new_v4().to_string(), user, channel, step, required,
            !matches!(evidence, Evidence::Denied), chrono::Utc::now().to_rfc3339()],
    ).unwrap();
}
