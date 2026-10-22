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

fn cmd<'a>(
    f: &'a Fx,
    agent_dir: &'a Path,
    requester: RequesterIdentity,
    slug: &str,
) -> Command<'a> {
    Command {
        home: f.home.path(),
        users: Some(f.users.clone()),
        agent_id: "hr-bot",
        agent_dir,
        requester,
        slug: slug.into(),
        attachments: vec![AttachmentRef {
            name: "rules.md".into(),
            path: f.file.clone(),
        }],
    }
}

#[tokio::test]
async fn bound_sender_files_a_card_and_others_are_refused() {
    let f = fx();
    let agent_dir = f.home.path().join("agents").join("hr-bot");
    let bound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Ue".into(),
        sender: "Ue".into(),
        user_id: Some(f.editor.clone()),
    };
    let reply = handle(cmd(&f, &agent_dir, bound.clone(), "hr")).await;
    assert!(reply.contains("已送出審核"), "{reply}");
    let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
    let cards = broker.list_by_kind(approval::ACTION_KIND).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].payload["via"], "chat");
    assert_eq!(cards[0].payload["kind"], "attachment");
    assert_eq!(cards[0].payload["origin_channel"], "line");
    // Unbound sender.
    let unbound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Ux".into(),
        sender: "Ux".into(),
        user_id: None,
    };
    assert_eq!(
        handle(cmd(&f, &agent_dir, unbound, "hr")).await,
        NEED_BINDING
    );
    // A bound person who cannot read the collection, and a missing one,
    // get the same answer.
    let other = RequesterIdentity::DashboardUser {
        user_id: f.stranger.clone(),
    };
    assert_eq!(
        handle(cmd(&f, &agent_dir, other, "hr")).await,
        NO_COLLECTION
    );
    assert_eq!(
        handle(cmd(&f, &agent_dir, bound.clone(), "nope")).await,
        NO_COLLECTION
    );
    assert_eq!(handle(cmd(&f, &agent_dir, bound.clone(), "")).await, USAGE);
    let mut none = cmd(&f, &agent_dir, bound, "hr");
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
