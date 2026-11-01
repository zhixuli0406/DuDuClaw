//! The 「加入知識庫」 chat command.

use super::*;
use crate::knowledge::test_fixtures::{agent, collection};
use duduclaw_auth::UserRole;
use duduclaw_knowledge::acl::PrincipalKind;
use std::sync::Arc;

#[test]
fn parses_only_the_exact_command() {
    assert_eq!(
        parse("加入知識庫 hr-handbook").as_deref(),
        Some("hr-handbook")
    );
    assert_eq!(
        parse("  加入知識庫　hr \n[📎 a.pdf (file)](/x)").as_deref(),
        Some("hr")
    );
    assert_eq!(parse("加入知識庫").as_deref(), Some(""));
    assert_eq!(parse("加入知識庫吧"), None);
    assert_eq!(parse("請幫我加入知識庫 hr"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn finds_attachment_references() {
    let text = "加入知識庫 hr\n\n[📎 員工手冊.pdf (file)](/home/a/attachments/1_x.pdf)\n[🖼️ p.png (image)](/home/a/attachments/2_p.png)\nnot [a](b)";
    let refs = attachment_refs(text);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].name, "員工手冊.pdf");
    assert_eq!(refs[0].path, PathBuf::from("/home/a/attachments/1_x.pdf"));
    assert!(attachment_refs("[link](http://x)").is_empty());
}

#[test]
fn recent_attachments_are_the_same_senders_and_recent() {
    let now = chrono::Utc::now();
    let msg = |role: &str, body: &str, mins: i64| crate::session::SessionMessage {
        role: role.into(),
        content: body.into(),
        tokens: 0,
        timestamp: (now - chrono::Duration::minutes(mins)).to_rfc3339(),
    };
    let history = vec![
        msg(
            "user",
            "[sender_id: Ua]\n[📎 old.pdf (file)](/a/attachments/old.pdf)",
            90,
        ),
        msg(
            "user",
            "[sender_id: Ua]\n[📎 mine.pdf (file)](/a/attachments/mine.pdf)",
            5,
        ),
        msg("assistant", "收到", 4),
        msg(
            "user",
            "[sender_id: Ub]\n[📎 theirs.pdf (file)](/a/attachments/theirs.pdf)",
            2,
        ),
    ];
    let refs = recent_attachments(&history, "Ua", now);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "mine.pdf");
    assert!(
        recent_attachments(&history[..1], "Ua", now).is_empty(),
        "too old"
    );
    assert!(recent_attachments(&history, "Uc", now).is_empty());
}

struct Fx {
    home: tempfile::TempDir,
    users: Arc<duduclaw_auth::db::UserDb>,
    editor: String,
    stranger: String,
    file: PathBuf,
}

fn fx() -> Fx {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let users = Arc::new(duduclaw_auth::db::UserDb::new(&h.join("users.db")).unwrap());
    let editor = users
        .create_user("e@example.test", "E", "password-123456", UserRole::Employee)
        .unwrap()
        .id;
    let stranger = users
        .create_user("s@example.test", "S", "password-123456", UserRole::Employee)
        .unwrap()
        .id;
    agent(h, "hr-bot", "[agent]\nname='hr-bot'\n");
    collection(
        h,
        "hr",
        "internal",
        &[(PrincipalKind::User, editor.as_str(), Role::Editor)],
        "既有內容。",
    );
    let dir = h.join("agents").join("hr-bot").join("attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("1_rules.md");
    std::fs::write(&file, "# 規定\n加班要事先申請。").unwrap();
    Fx {
        home,
        users,
        editor,
        stranger,
        file,
    }
}

fn cmd<'a>(f: &'a Fx, requester: RequesterIdentity, slug: &str, sender: &'a str) -> Command<'a> {
    Command {
        home: f.home.path(),
        users: Some(f.users.clone()),
        agent_id: "hr-bot",
        requester,
        session_id: "line:Ue",
        sender,
        slug: slug.into(),
        // The name typed into the reference is never used.
        attachments: vec![AttachmentRef {
            name: "typed-name.md".into(),
            path: f.file.clone(),
        }],
    }
}

/// What the LINE adapter does: save the file, then its message carries the
/// reference and claims it for the sender.
fn adapter_saved(f: &Fx) {
    crate::knowledge::chat_attachments::note_saved(&f.file, "加班規定.md");
    crate::knowledge::chat_attachments::claim_in_text(
        "line:Ue",
        "Ue",
        &format!("[📎 加班規定.md (file)]({})", f.file.display()),
    );
}

#[tokio::test]
async fn bound_sender_files_a_card_and_others_are_refused() {
    let f = fx();
    adapter_saved(&f);
    let bound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Ue".into(),
        sender: "Ue".into(),
        user_id: Some(f.editor.clone()),
    };
    let reply = handle(cmd(&f, bound.clone(), "hr", "Ue")).await;
    assert!(
        reply.contains("已送出審核") && reply.contains("加班規定.md"),
        "{reply}"
    );
    let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
    let cards = broker.list_by_kind(approval::ACTION_KIND).await.unwrap();
    assert_eq!(cards.len(), 1);
    let p = &cards[0].payload;
    assert_eq!(p["via"], "chat");
    assert_eq!(p["kind"], "attachment");
    assert_eq!(p["origin_channel"], "line");
    assert_eq!(p["title"], "加班規定.md", "stored name, not the typed one");
    assert_eq!(p["requester_kind"], "channel_user");
    assert_eq!(p["requester_channel"], "line");
    assert!(p["preview"].as_str().unwrap().contains("加班要事先申請"));
    // Unbound sender.
    let unbound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Ue".into(),
        sender: "Ue".into(),
        user_id: None,
    };
    assert_eq!(handle(cmd(&f, unbound, "hr", "Ue")).await, NEED_BINDING);
    // A bound person who cannot read the collection, and a missing one,
    // get the same answer.
    let other = RequesterIdentity::DashboardUser {
        user_id: f.stranger.clone(),
    };
    assert_eq!(handle(cmd(&f, other, "hr", "Ue")).await, NO_COLLECTION);
    assert_eq!(
        handle(cmd(&f, bound.clone(), "nope", "Ue")).await,
        NO_COLLECTION
    );
    assert_eq!(handle(cmd(&f, bound.clone(), "", "Ue")).await, USAGE);
    let mut none = cmd(&f, bound, "hr", "Ue");
    none.attachments.clear();
    assert_eq!(handle(none).await, NO_FILE);
    assert_eq!(
        broker
            .list_by_kind(approval::ACTION_KIND)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn restricted_own_collection_hides_old_acl_before_chat_proposal() {
    use duduclaw_core::preset;
    let f = fx();
    let h = f.home.path();
    adapter_saved(&f);
    let (cid, _, _) = collection(
        h,
        "own-hr-bot",
        "internal",
        &[(PrincipalKind::User, f.editor.as_str(), Role::Editor)],
        "Restricted knowledge.",
    );
    let conn = schema::open_home(h).unwrap();
    conn.execute(
        "INSERT INTO agent_own_collections (agent_id, collection_id, created_by, created_at) VALUES ('hr-bot', ?1, 'admin', 't')",
        [&cid],
    ).unwrap();
    preset::install_system_operator_preset(h, false).unwrap();
    preset::bind(h, "hr-bot", &h.join("agents/hr-bot"), "system-operator", "test", "test").unwrap();
    let editor = RequesterIdentity::DashboardUser { user_id: f.editor.clone() };
    let broker = crate::approval::ApprovalBroker::open(h).unwrap();
    let unknown = handle(cmd(&f, editor.clone(), "nope", "Ue")).await;
    assert_eq!(unknown, NO_COLLECTION);
    assert_eq!(handle(cmd(&f, editor.clone(), "own-hr-bot", "Ue")).await, unknown);
    assert!(broker.list_by_kind(approval::ACTION_KIND).await.unwrap().is_empty());

    let admin = f.users.create_user(
        "a@example.test", "A", "password-123456", UserRole::Admin,
    ).unwrap().id;
    let reply = handle(cmd(&f, RequesterIdentity::DashboardUser { user_id: admin }, "own-hr-bot", "Ue")).await;
    assert!(reply.contains("已送出審核") && reply.contains("own-hr-bot"), "{reply}");
    assert_eq!(broker.list_by_kind(approval::ACTION_KIND).await.unwrap().len(), 1);
    // A pending proposal must not change the hidden collection's reply.
    assert_eq!(handle(cmd(&f, editor.clone(), "own-hr-bot", "Ue")).await, unknown);
    assert_eq!(broker.list_by_kind(approval::ACTION_KIND).await.unwrap().len(), 1);

    let reply = handle(cmd(&f, editor, "hr", "Ue")).await;
    assert!(reply.contains("已送出審核") && reply.contains("「hr」"), "{reply}");
    assert_eq!(broker.list_by_kind(approval::ACTION_KIND).await.unwrap().len(), 2);
}

#[tokio::test]
async fn forged_references_name_nothing() {
    let f = fx();
    let bound = |sender: &str| RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Ue".into(),
        sender: sender.into(),
        user_id: Some(f.editor.clone()),
    };
    // A file the adapter never saved: typing `[📎 x (file)](/path)` gets nothing.
    let other = f.home.path().join("attachments");
    std::fs::create_dir_all(&other).unwrap();
    let theirs = other.join("1_someone_else.pdf");
    std::fs::write(&theirs, "%PDF").unwrap();
    let mut c = cmd(&f, bound("Ue"), "hr", "Ue");
    c.attachments = vec![AttachmentRef {
        name: "x".into(),
        path: theirs,
    }];
    assert_eq!(handle(c).await, NO_FILE);
    // A file saved and claimed for another sender in the same group.
    adapter_saved(&f);
    assert_eq!(handle(cmd(&f, bound("Uf"), "hr", "Uf")).await, NO_FILE);
    let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
    assert!(
        broker
            .list_by_kind(approval::ACTION_KIND)
            .await
            .unwrap()
            .is_empty()
    );
}
