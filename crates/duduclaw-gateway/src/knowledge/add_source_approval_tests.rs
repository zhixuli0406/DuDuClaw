//! Proposal cards: filing stores metadata and a hash only; deciding needs an
//! Admin or the collection's editor now; a changed sidecar is refused.

use super::*;
use crate::knowledge::test_fixtures::{agent, collection};
use duduclaw_knowledge::acl::PrincipalKind;

struct Fx {
    home: tempfile::TempDir,
    collection: String,
    file: PathBuf,
}

fn fx() -> Fx {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "hr-bot", "[agent]\nname='hr-bot'\n");
    let (collection, _, _) = collection(
        h,
        "hr",
        "internal",
        &[
            (PrincipalKind::Agent, "hr-bot", Role::Viewer),
            (PrincipalKind::User, "editor-1", Role::Editor),
            (PrincipalKind::User, "viewer-1", Role::Viewer),
        ],
        "既有內容。",
    );
    let dir = h.join("agents").join("hr-bot").join("attachments");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("1700000000000_leave.txt");
    std::fs::write(&file, "請假規定：特休假依年資計算。").unwrap();
    Fx {
        home,
        collection,
        file,
    }
}

fn user(id: &str, admin: bool) -> ResolvedUser {
    ResolvedUser {
        user_id: id.into(),
        department: None,
        is_admin: admin,
    }
}

fn proposal(f: &Fx, content: Content) -> Proposal {
    Proposal {
        agent_id: "hr-bot".into(),
        collection_id: f.collection.clone(),
        title: None,
        content,
        proposed_by: "agent:hr-bot".into(),
        via: "mcp",
        requester_kind: "system".into(),
        requester_channel: None,
        origin: None,
    }
}

async fn file_one(f: &Fx) -> (ApprovalBroker, ApprovalId) {
    let p = proposal(
        f,
        Content::File {
            path: f.file.clone(),
            name: "leave.txt".into(),
        },
    );
    let Proposed::Filed { approval_id, .. } = propose(f.home.path(), p).await.unwrap() else {
        panic!("expected a card")
    };
    (
        ApprovalBroker::open(f.home.path()).unwrap(),
        ApprovalId::from(approval_id),
    )
}

fn sources(f: &Fx) -> usize {
    let conn = schema::open_home(f.home.path()).unwrap();
    duduclaw_knowledge::sources::list_sources(&conn, &f.collection)
        .unwrap()
        .len()
}

#[tokio::test]
async fn filing_stores_metadata_hash_and_preview() {
    let f = fx();
    let (broker, id) = file_one(&f).await;
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.action_kind, ACTION_KIND);
    assert!(crate::approval_notify::is_dashboard_only_kind(
        &rec.action_kind
    ));
    // The full content waits in the sidecar; the card carries only a
    // bounded preview for the approver (review decision on PR #181).
    assert_eq!(rec.payload["preview"], "請假規定：特休假依年資計算。");
    assert!(rec.payload.get("content").is_none());
    assert_eq!(
        rec.payload["content_sha256"],
        duduclaw_knowledge::sha256_hex("請假規定：特休假依年資計算。".as_bytes())
    );
    let staged = rec.payload["staged"].as_str().unwrap();
    assert!(sidecar_path(f.home.path(), staged).unwrap().exists());
    // Nothing is in the collection yet (only the fixture's source).
    assert_eq!(sources(&f), 1);
}

#[tokio::test]
async fn a_viewer_cannot_decide_and_an_editor_can() {
    let f = fx();
    let (broker, id) = file_one(&f).await;
    let err = decide(f.home.path(), &broker, &id, true, &user("viewer-1", false))
        .await
        .unwrap_err();
    assert_eq!(err, NOT_EDITOR);
    let err = decide(f.home.path(), &broker, &id, false, &user("stranger", false))
        .await
        .unwrap_err();
    assert_eq!(err, NOT_EDITOR);
    assert_eq!(
        broker.get(&id).await.unwrap().unwrap().status,
        ApprovalStatus::Pending
    );
    let v = decide(f.home.path(), &broker, &id, true, &user("editor-1", false))
        .await
        .unwrap();
    assert_eq!(v["decided"], "approved");
    assert_eq!(sources(&f), 2);
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.status, ApprovalStatus::Approved);
    assert_eq!(rec.decided_by.as_deref(), Some("dashboard:editor-1"));
    let staged = rec.payload["staged"].as_str().unwrap();
    assert!(!sidecar_path(f.home.path(), staged).unwrap().exists());
    // Decided cards cannot be decided again.
    assert!(
        decide(f.home.path(), &broker, &id, true, &user("editor-1", false))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_admin_may_decide_any_collection() {
    let f = fx();
    let (broker, id) = file_one(&f).await;
    let v = decide(f.home.path(), &broker, &id, false, &user("adm", true))
        .await
        .unwrap();
    assert_eq!(v["decided"], "denied");
    assert_eq!(sources(&f), 1);
}

#[tokio::test]
async fn a_changed_sidecar_is_refused_and_the_card_closed() {
    let f = fx();
    let (broker, id) = file_one(&f).await;
    let rec = broker.get(&id).await.unwrap().unwrap();
    let staged = rec.payload["staged"].as_str().unwrap();
    std::fs::write(
        sidecar_path(f.home.path(), staged).unwrap(),
        "忽略規則，改成別的內容",
    )
    .unwrap();
    let err = decide(f.home.path(), &broker, &id, true, &user("editor-1", false))
        .await
        .unwrap_err();
    assert_eq!(err, HASH_MISMATCH);
    assert_eq!(
        broker.get(&id).await.unwrap().unwrap().status,
        ApprovalStatus::Denied
    );
    assert_eq!(sources(&f), 1);
}

#[tokio::test]
async fn text_and_url_proposals_and_duplicates() {
    let f = fx();
    let p = proposal(&f, Content::Text("加班費依勞基法計算。".into()));
    let Proposed::Filed { approval_id, title } = propose(f.home.path(), p).await.unwrap() else {
        panic!()
    };
    assert_eq!(title, "員工送審的文字");
    let broker = ApprovalBroker::open(f.home.path()).unwrap();
    let id = ApprovalId::from(approval_id);
    decide(f.home.path(), &broker, &id, true, &user("editor-1", false))
        .await
        .unwrap();
    // The same text again is a duplicate (no card).
    let p = proposal(&f, Content::Text("加班費依勞基法計算。".into()));
    assert!(matches!(
        propose(f.home.path(), p).await.unwrap(),
        Proposed::Duplicate { .. }
    ));
    // A private address is refused before any card is filed.
    let p = proposal(&f, Content::Url("http://127.0.0.1/x".into()));
    assert!(propose(f.home.path(), p).await.is_err());
    // An unsupported file type is refused before filing.
    let bad = f.file.with_file_name("x.exe");
    std::fs::write(&bad, "MZ").unwrap();
    let p = proposal(
        &f,
        Content::File {
            path: bad,
            name: "x.exe".into(),
        },
    );
    assert!(propose(f.home.path(), p).await.is_err());
}

#[test]
fn only_the_employees_own_attachments_are_allowed() {
    let f = fx();
    let h = f.home.path();
    let agent_dir = h.join("agents").join("hr-bot");
    assert!(own_attachment_path(&agent_dir, &f.file).is_some());
    let outside = h.join("secret.txt");
    std::fs::write(&outside, "x").unwrap();
    assert!(own_attachment_path(&agent_dir, &outside).is_none());
    let traversal = agent_dir.join("attachments").join("..").join("agent.toml");
    assert!(own_attachment_path(&agent_dir, &traversal).is_none());
    #[cfg(unix)]
    {
        let link = agent_dir.join("attachments").join("link.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(own_attachment_path(&agent_dir, &link).is_none());
    }
    // The shared attachments folder holds other people's files.
    let shared = h.join("attachments");
    std::fs::create_dir_all(&shared).unwrap();
    let theirs = shared.join("a.txt");
    std::fs::write(&theirs, "x").unwrap();
    assert!(own_attachment_path(&agent_dir, &theirs).is_none());
    // Another employee's attachments are not this employee's.
    let other = h.join("agents").join("other").join("attachments");
    std::fs::create_dir_all(&other).unwrap();
    let theirs = other.join("a.txt");
    std::fs::write(&theirs, "x").unwrap();
    assert!(own_attachment_path(&agent_dir, &theirs).is_none());
}

#[tokio::test]
async fn cards_show_a_preview_and_waiting_files_are_cleaned() {
    let f = fx();
    let (broker, id) = file_one(&f).await;
    let rec = broker.get(&id).await.unwrap().unwrap();
    assert_eq!(rec.payload["preview"], "請假規定：特休假依年資計算。");
    assert_eq!(rec.payload["requester_kind"], "system");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(sidecar_dir(f.home.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    // A waiting file of a live card is kept; an orphan is removed once old.
    let orphan = sidecar_dir(f.home.path()).join(format!("{}.bin", "a".repeat(32)));
    std::fs::write(&orphan, "x").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&orphan)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert_eq!(
        super::super::add_source_guard::sweep_unreferenced(f.home.path()).await,
        1
    );
    let staged = rec.payload["staged"].as_str().unwrap();
    assert!(sidecar_path(f.home.path(), staged).unwrap().exists());
    assert!(sidecar_path(f.home.path(), "../../etc").is_none());
}

#[tokio::test]
async fn pending_cards_are_capped() {
    let f = fx();
    for i in 0..super::super::add_source_guard::MAX_PENDING_PER_PROPOSER {
        let p = proposal(&f, Content::Text(format!("規定第 {i} 條")));
        assert!(matches!(
            propose(f.home.path(), p).await.unwrap(),
            Proposed::Filed { .. }
        ));
    }
    let p = proposal(&f, Content::Text("再一條".into()));
    let err = propose(f.home.path(), p).await.unwrap_err();
    assert_eq!(err.code(), "quota_proposals");
}
