//! Shared fixtures for the F4 decision-routing tests (adapter test modules
//! under `adapter_tests/` and `tests.rs`).

use std::path::Path;

use duduclaw_auth::UserDb;
use duduclaw_auth::models::UserRole;

/// Create `users.db` with one Active **Employee** and one Active **Manager**,
/// each with a *verified* identity on `channel`. With this file present the
/// deployment is no longer in solo mode: only a verified Admin/Manager who is
/// also the person the request is bound to may approve from a channel.
pub(crate) fn seed_decision_users(home: &Path, channel: &str, employee: &str, manager: &str) {
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let e = db
        .create_user(
            &format!("employee-{channel}@example.test"),
            "Employee",
            "fixture-password-employee",
            UserRole::Employee,
        )
        .unwrap();
    db.bind_channel_identity(&e.id, channel, employee, true)
        .unwrap();
    let m = db
        .create_user(
            &format!("manager-{channel}@example.test"),
            "Manager",
            "fixture-password-manager",
            UserRole::Manager,
        )
        .unwrap();
    db.bind_channel_identity(&m.id, channel, manager, true)
        .unwrap();
}

/// Current status of an approval row.
pub(crate) async fn status_of(
    home: &Path,
    id: &crate::approval::ApprovalId,
) -> crate::approval::ApprovalStatus {
    crate::approval::ApprovalBroker::open(home)
        .unwrap()
        .get(id)
        .await
        .unwrap()
        .unwrap()
        .status
}

/// A legacy (unbound) approval whose card was delivered as `message_id` in
/// `chat_id` on `channel` — what a shipped WP1.6 reply-to-card targets.
pub(crate) async fn legacy_card(
    home: &Path,
    channel: &str,
    chat_id: &str,
    message_id: &str,
) -> crate::approval::ApprovalId {
    let broker = crate::approval::ApprovalBroker::open(home).unwrap();
    let id = broker
        .request(
            "alice",
            "fixture_legacy_action",
            "legacy card",
            serde_json::json!({"fixture": true}),
            600,
        )
        .await
        .unwrap();
    crate::decision_message_store::record_card_message(
        home,
        crate::decision_action::DecisionSource::Approval.namespace(),
        id.as_str(),
        channel,
        chat_id,
        &crate::decision_card::PushedMessage {
            edit_chat_id: chat_id.into(),
            message_id: message_id.into(),
        },
    );
    id
}

/// True when any captured provider request carries `needle` in a text body.
pub(crate) fn provider_sent_text(
    provider: &crate::test_channel_provider::TestChannelProvider,
    needle: &str,
) -> bool {
    provider.requests().iter().any(|r| {
        ["text", "content"].iter().any(|k| {
            r.body[*k]
                .as_str()
                .is_some_and(|text| text.contains(needle))
        }) || r.body["messages"].as_array().is_some_and(|m| {
            m.iter()
                .any(|m| m["text"].as_str().is_some_and(|t| t.contains(needle)))
        })
    })
}
