//! Feishu commands treat every chat as shared unless Feishu marks it an
//! explicit 1:1 (`p2p`) chat (security review H4).

use super::feishu_command_origin;
use crate::channel_reply::access_gate_tests::reply_ctx;

fn bind(home: &std::path::Path, open_id: &str, role: duduclaw_auth::UserRole) {
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let user = db
        .create_user(&format!("{open_id}@test.local"), open_id, "StaffPass-12345", role)
        .unwrap();
    db.bind_channel_identity(&user.id, "feishu", open_id, true).unwrap();
}

fn message(chat_type: Option<&str>) -> serde_json::Value {
    match chat_type {
        Some(kind) => serde_json::json!({ "chat_id": "oc_1", "chat_type": kind }),
        None => serde_json::json!({ "chat_id": "oc_1" }),
    }
}

#[test]
fn only_an_explicit_p2p_chat_is_one_to_one() {
    for kind in [Some("group"), Some("topic_group"), None, Some("future_kind"), Some("P2P"), Some("")] {
        let msg = message(kind);
        assert!(feishu_command_origin(&msg, "feishu:oc_1", "ou_a", "/new").shared, "{kind:?}");
    }
    let msg = message(Some("p2p"));
    assert!(!feishu_command_origin(&msg, "feishu:oc_1", "ou_a", "/new").shared);
}

#[tokio::test]
async fn non_admin_cannot_clear_a_topic_group_missing_or_unknown_chat() {
    let home = tempfile::tempdir().unwrap();
    bind(home.path(), "ou_admin", duduclaw_auth::UserRole::Admin);
    bind(home.path(), "ou_emp", duduclaw_auth::UserRole::Employee);
    let ctx = reply_ctx(home.path());
    for text in ["/new", "/compact"] {
        let cmd = crate::chat_commands::parse_command(text, None).unwrap();
        for kind in [Some("topic_group"), None, Some("future_kind"), Some("group")] {
            let msg = message(kind);
            let origin = feishu_command_origin(&msg, "feishu:oc_1", "ou_emp", text);
            assert_eq!(
                crate::chat_commands::handle_gated_command(&cmd, &ctx, &origin, "agent-a").await,
                crate::chat_commands::GROUP_RESET_REFUSED,
                "{text} {kind:?}"
            );
        }
    }
    let new = crate::chat_commands::parse_command("/new", None).unwrap();
    let p2p = message(Some("p2p"));
    let own = feishu_command_origin(&p2p, "feishu:ou_emp", "ou_emp", "/new");
    let reply = crate::chat_commands::handle_gated_command(&new, &ctx, &own, "agent-a").await;
    assert!(reply.starts_with('✅'), "{reply}");
    let topic = message(Some("topic_group"));
    let admin = feishu_command_origin(&topic, "feishu:oc_1", "ou_admin", "/new");
    let reply = crate::chat_commands::handle_gated_command(&new, &ctx, &admin, "agent-a").await;
    assert!(reply.starts_with('✅'), "{reply}");
}
