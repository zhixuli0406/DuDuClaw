use super::*;
use super::super::{add_source_approval as approval, add_source_chat as chat};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_knowledge::{schema, store, limits::KnowledgeConfig, acl::{PrincipalKind, Role}};
use duduclaw_knowledge::requester::ResolvedUser;
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus, DecisionContext};
use super::super::turn_requesters::{self, InboundContent};

struct Fixture {
    home: tempfile::TempDir,
    db: UserDb,
    owner: String,
    editor: String,
    cid: String,
    turn: Turn,
    _guard: super::super::turn_requesters::RequesterGuard,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        std::fs::write(h.join("config.toml"), "[knowledge]\nowner_self_add=true\n").unwrap();
        super::super::test_fixtures::agent(h, "kb-avatar", "[agent]\nname='kb-avatar'\n");
        let db = UserDb::new(&h.join("users.db")).unwrap();
        let admin = db.create_user("admin@example.com", "Admin", "test-password", UserRole::Admin).unwrap();
        let owner = db.create_user("owner@example.com", "Owner", "test-password", UserRole::Employee).unwrap().id;
        let editor = db.create_user("editor@example.com", "Editor", "test-password", UserRole::Employee).unwrap().id;
        db.bind_agent(&owner, "kb-avatar", AccessLevel::Operator).unwrap();
        db.set_avatar_as(&admin.id, &owner, Some("kb-avatar")).unwrap();
        db.bind_channel_identity(&owner, "telegram", "101", true).unwrap();
        db.bind_channel_identity(&editor, "telegram", "102", true).unwrap();
        let mut conn = schema::open_home(h).unwrap();
        let c = store::create_collection(&mut conn, &store::NewCollection { name: "Private".into(), ..Default::default() }, &owner, &KnowledgeConfig::default()).unwrap();
        duduclaw_knowledge::acl::set_acl(&mut conn, &c.id, &[
            (PrincipalKind::User, owner.clone(), Role::Owner),
            (PrincipalKind::Agent, "kb-avatar".into(), Role::Viewer),
        ], 0, &owner).unwrap();
        conn.execute("UPDATE collections SET owner_self_add=1 WHERE id=?1", [&c.id]).unwrap();
        let turn = Turn { agent: "kb-avatar".into(), turn_id: Some(uuid::Uuid::new_v4().to_string()), run_id: None };
        let guard = turn_requesters::register_inbound(&turn.agent, turn.turn_id.as_deref().unwrap(),
            RequesterIdentity::DashboardUser { user_id: owner.clone() },
            Some(InboundContent { text: "source data".into(), attachment_hashes: vec![] })).unwrap();
        Self { home, db, owner, editor, cid: c.id, turn, _guard: guard }
    }
    fn proposal(&self, text: &str) -> approval::Proposal {
        approval::Proposal {
            agent_id: self.turn.agent.clone(), collection_id: self.cid.clone(), title: Some("中文標題".into()),
            content: approval::Content::Text(text.into()), proposed_by: "agent:kb-avatar".into(), via: "mcp",
            requester_kind: "dashboard_user".into(), requester_channel: None, origin: None,
        }
    }
    async fn submit(&self, text: &str) -> approval::Proposed {
        self.submit_inbound(self.proposal(text), Some(InboundContent {
            text: text.into(), attachment_hashes: vec![],
        })).await
    }
    async fn submit_inbound(&self, proposal: approval::Proposal, inbound: Option<InboundContent>) -> approval::Proposed {
        let _guard = turn_requesters::register_inbound(&self.turn.agent, self.turn.turn_id.as_deref().unwrap(),
            RequesterIdentity::DashboardUser { user_id: self.owner.clone() }, inbound).unwrap();
        approval::propose_in_turn(self.home.path(), proposal, self.turn.turn_id.clone(), None).await.unwrap()
    }
    async fn card(&self) -> (ApprovalBroker, crate::approval::ApprovalRecord) {
        let approval::Proposed::Filed { approval_id, .. } = approval::propose_as(self.home.path(), self.proposal("<secret>私密內容🦀</secret>"),
            RequesterIdentity::DashboardUser { user_id: self.owner.clone() }).await.unwrap() else { panic!("card required") };
        let broker = ApprovalBroker::open(self.home.path()).unwrap();
        let rec = broker.get(&ApprovalId::from(approval_id)).await.unwrap().unwrap();
        (broker, rec)
    }
    fn context(&self, principal: &str) -> DecisionContext {
        DecisionContext { channel: "telegram".into(), account_id: "bot".into(), conversation_id: principal.into(), principal_id: principal.into() }
    }
    fn add_grant(&self, kind: &str, id: &str, role: &str) {
        let conn = schema::open_home(self.home.path()).unwrap();
        conn.execute("INSERT INTO collection_acl VALUES(?1, ?2, ?3, ?4, 'test', 'now')", params![self.cid, kind, id, role]).unwrap();
    }
    fn count(&self) -> i64 {
        schema::open_home(self.home.path()).unwrap().query_row("SELECT COUNT(*) FROM sources WHERE collection_id=?1", [&self.cid], |r| r.get(0)).unwrap()
    }
}

#[tokio::test]
async fn exact_owner_avatar_turn_imports_and_audits_without_content() {
    let f = Fixture::new();
    assert!(matches!(f.submit("secret source body").await, approval::Proposed::Imported { .. }));
    assert_eq!(f.count(), 1);
    let conn = schema::open_home(f.home.path()).unwrap();
    let row: (String, String, String) = conn.query_row("SELECT actor_ai, owner_id, source_summary FROM owner_self_add_log", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(row, ("kb-avatar".into(), f.owner.clone(), "中文標題".into()));
    assert!(matches!(f.submit("secret source body").await, approval::Proposed::Duplicate { .. }));
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn dispatch_run_cannot_borrow_a_live_owner_turn_for_auto_import() {
    let f = Fixture::new();
    let turn_id = f.turn.turn_id.clone().unwrap();
    let run_id = uuid::Uuid::new_v4().to_string();
    turn_requesters::remember_turn_origin(&turn_id,
        RequesterIdentity::DashboardUser { user_id: f.owner.clone() });
    let _run = duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn_id.clone()), async {
        turn_requesters::register_run(&f.turn.agent, &run_id, "dispatch")
    }).await.unwrap();
    assert_eq!(turn_requesters::lookup(&f.turn.agent, &run_id), Some(RequesterIdentity::System {
        kind: "dispatch".into(), on_behalf_of: Some(f.owner.clone()),
    }));
    let result = approval::propose_in_turn(f.home.path(), f.proposal("delegated source"),
        Some(turn_id), Some(run_id)).await.unwrap();
    assert!(matches!(result, approval::Proposed::Filed { .. }));
    assert_eq!(f.count(), 0);
    let conn = schema::open_home(f.home.path()).unwrap();
    for table in ["owner_self_add_log", "owner_self_add_notices", "ingest_jobs"] {
        assert_eq!(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 0, "{table}");
    }
    // The same registered Owner turn remains eligible when called directly.
    assert!(matches!(f.submit("direct owner source").await, approval::Proposed::Imported { .. }));
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn background_runs_with_a_live_owner_turn_always_require_a_card() {
    let f = Fixture::new();
    for kind in ["cron", "heartbeat", "sub-agent"] {
        let run_id = uuid::Uuid::new_v4().to_string();
        let _run = turn_requesters::register_run(&f.turn.agent, &run_id, kind).unwrap();
        let result = approval::propose_in_turn(f.home.path(), f.proposal(kind),
            f.turn.turn_id.clone(), Some(run_id.clone())).await.unwrap();
        assert!(matches!(result, approval::Proposed::Filed { .. }), "{kind}");
        // A System registration cannot become a direct Owner turn even if
        // supplied as the turn id without a separate run id.
        let result = approval::propose_in_turn(f.home.path(), f.proposal(kind),
            Some(run_id), None).await.unwrap();
        assert!(matches!(result, approval::Proposed::Filed { .. }), "{kind}");
    }
    for run_id in ["unregistered-run", ""] {
        let result = approval::propose_in_turn(f.home.path(), f.proposal("unknown run"),
            f.turn.turn_id.clone(), Some(run_id.into())).await.unwrap();
        assert!(matches!(result, approval::Proposed::Filed { .. }));
    }
    assert_eq!(f.count(), 0);
}

#[tokio::test]
async fn short_nonempty_paste_auto_imports() {
    for text in ["的", "a", "1234567"] {
        let f = Fixture::new();
        assert!(matches!(f.submit(text).await, approval::Proposed::Imported { .. }), "{text:?}");
        assert_eq!(f.count(), 1, "{text:?}");
    }
}

#[tokio::test]
async fn empty_verbatim_paste_is_rejected_without_import() {
    let f = Fixture::new();
    for text in ["", "   \n\t"] {
        assert!(approval::propose_in_turn(f.home.path(), f.proposal(text),
            f.turn.turn_id.clone(), None).await.is_err());
    }
    assert_eq!(f.count(), 0);
}

#[tokio::test]
async fn eight_character_verbatim_paste_auto_imports() {
    let f = Fixture::new();
    assert!(matches!(f.submit("abcdefgh").await, approval::Proposed::Imported { .. }));
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn extra_acl_principal_always_requires_approval() {
    for (kind, id, role) in [("department", "hr", "viewer"), ("channel", "telegram", "viewer"),
        ("user", "other", "viewer"), ("user", "co-owner", "owner"), ("agent", "other-bot", "viewer")] {
        let f = Fixture::new();
        f.add_grant(kind, id, role);
        assert!(matches!(f.submit("new source").await, approval::Proposed::Filed { .. }), "{kind}");
        assert_eq!(f.count(), 0);
    }
}

#[tokio::test]
async fn master_off_missing_malformed_and_collection_off_require_approval() {
    for config in ["", "[knowledge]\nenabled=false\nowner_self_add=true", "[knowledge]\nowner_self_add=false", "[knowledge]\nowner_self_add='true'", "[broken"] {
        let f = Fixture::new();
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        assert!(matches!(f.submit("new source").await, approval::Proposed::Filed { .. }));
    }
    let f = Fixture::new();
    schema::open_home(f.home.path()).unwrap().execute("UPDATE collections SET owner_self_add=0", []).unwrap();
    assert!(matches!(f.submit("new source").await, approval::Proposed::Filed { .. }));
}

#[tokio::test]
async fn foreign_requester_missing_avatar_and_inactive_owner_require_approval() {
    let f = Fixture::new();
    let other_turn = uuid::Uuid::new_v4().to_string();
    let _guard = super::super::turn_requesters::register("kb-avatar", &other_turn,
        RequesterIdentity::DashboardUser { user_id: f.editor.clone() }).unwrap();
    let result = approval::propose_in_turn(f.home.path(), f.proposal("foreign"), Some(other_turn), None).await.unwrap();
    assert!(matches!(result, approval::Proposed::Filed { .. }));
    let users = Connection::open(f.home.path().join("users.db")).unwrap();
    users.execute("UPDATE users SET avatar_agent_id=NULL WHERE id=?1", [&f.owner]).unwrap();
    assert!(matches!(f.submit("unassigned").await, approval::Proposed::Filed { .. }));
    users.execute("UPDATE users SET avatar_agent_id='kb-avatar', status='suspended' WHERE id=?1", [&f.owner]).unwrap();
    assert!(matches!(f.submit("inactive").await, approval::Proposed::Filed { .. }));
    assert_eq!(f.count(), 0);
}

#[tokio::test]
async fn rolling_hour_cap_falls_back_to_card() {
    let f = Fixture::new();
    for n in 0..HOURLY_CAP {
        assert!(matches!(f.submit(&format!("source {n}")).await, approval::Proposed::Imported { .. }));
    }
    assert!(matches!(f.submit("over cap").await, approval::Proposed::Filed { .. }));
    assert_eq!(f.count(), HOURLY_CAP);
    schema::open_home(f.home.path()).unwrap().execute("UPDATE owner_self_add_log SET imported_at=0", []).unwrap();
    assert!(matches!(f.submit("after hour").await, approval::Proposed::Imported { .. }));
}

#[test]
fn sharing_after_initial_check_is_rechecked_inside_import_transaction() {
    let f = Fixture::new();
    let mut conn = schema::open_home(f.home.path()).unwrap();
    attach_users(&conn, f.home.path()).unwrap();
    let new = duduclaw_knowledge::sources::NewSource {
        collection_id: f.cid.clone(), kind: duduclaw_knowledge::sources::SourceKind::Paste,
        title: "metadata".into(), original_name: None, url: None,
        format: duduclaw_knowledge::extract::Format::Txt,
        blob_sha256: duduclaw_knowledge::blob::BlobStore::for_home(f.home.path()).put(b"source data").unwrap(), bytes: 11,
        origin: serde_json::json!({}), created_by: "kb-avatar".into(),
    };
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    authorize(&tx, f.home.path(), &f.turn, &new).unwrap();
    tx.rollback().unwrap();
    f.add_grant("department", "new-share", "viewer");
    assert!(duduclaw_knowledge::sources::add_source_checked(&mut conn, &new, &KnowledgeConfig::default(),
        |tx| authorize(tx, f.home.path(), &f.turn, &new)).is_err());
    assert_eq!(f.count(), 0);
    let audits: i64 = conn.query_row("SELECT COUNT(*) FROM owner_self_add_log", [], |r| r.get(0)).unwrap();
    assert_eq!(audits, 0, "failed import must not consume rate slots or claim an import");
}

#[tokio::test]
async fn self_add_still_rejects_ssrf_urls() {
    let f = Fixture::new();
    for url in ["http://127.0.0.1/", "http://169.254.169.254/", "http://localhost.evil.com@localhost/", "file:///etc/passwd"] {
        let mut p = f.proposal("unused");
        p.content = approval::Content::Url(url.into());
        assert!(approval::propose_in_turn(f.home.path(), p, f.turn.turn_id.clone(), None).await.is_err());
    }
    assert_eq!(f.count(), 0);
}

#[tokio::test]
async fn chat_owner_imports_exactly_once_and_generic_paths_refuse() {
    let f = Fixture::new();
    let (_, rec) = f.card().await;
    assert!(crate::approval_notify::apply_decision(f.home.path(), "telegram", "101", rec.id.as_str(), true).await.is_err());
    let context = f.context("101");
    let command = format!("確認 {}", rec.id.as_str());
    let (a, b) = tokio::join!(
        crate::decision_notify::route_bound_text(f.home.path(), &context, &command),
        crate::decision_notify::route_bound_text(f.home.path(), &context, &command));
    assert!(a.unwrap().is_ok() ^ b.unwrap().is_ok());
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn editor_nonowner_inactive_and_removed_owner_cannot_decide_in_chat() {
    let f = Fixture::new();
    f.add_grant("user", &f.editor, "editor");
    let (broker, rec) = f.card().await;
    for principal in ["102", "unknown"] {
        assert!(chat::decide(f.home.path(), &rec, &f.context(principal), true).await.is_err());
    }
    let users = Connection::open(f.home.path().join("users.db")).unwrap();
    users.execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.owner]).unwrap();
    assert!(chat::decide(f.home.path(), &rec, &f.context("101"), true).await.is_err());
    users.execute("UPDATE users SET status='active' WHERE id=?1", [&f.owner]).unwrap();
    schema::open_home(f.home.path()).unwrap().execute("UPDATE collection_acl SET role='editor' WHERE principal_id=?1", [&f.owner]).unwrap();
    assert!(chat::decide(f.home.path(), &rec, &f.context("101"), true).await.is_err());
    assert_eq!(broker.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    assert_eq!(f.count(), 0);
}

#[tokio::test]
async fn chat_needs_verb_full_uuid_and_same_self_approval_policy_as_dashboard() {
    let f = Fixture::new();
    let (broker, rec) = f.card().await;
    for text in ["確認".to_string(), rec.id.to_string(), format!("確認 {}", duduclaw_core::truncate_chars(rec.id.as_str(), 8))] {
        assert!(crate::decision_notify::route_bound_text(f.home.path(), &f.context("101"), &text).await.is_none());
    }
    std::fs::write(f.home.path().join("config.toml"), "[approval]\nforbid_self_approval=true").unwrap();
    let owner = ResolvedUser { user_id: f.owner.clone(), department: None, is_admin: false };
    assert_eq!(approval::decide(f.home.path(), &broker, &rec.id, true, &owner).await.unwrap_err(), crate::on_behalf_of::self_approval::REFUSAL);
    assert_eq!(chat::decide(f.home.path(), &rec, &f.context("101"), true).await.unwrap_err(), crate::on_behalf_of::self_approval::REFUSAL);
    assert!(chat::decide(f.home.path(), &rec, &f.context("101"), false).await.is_ok());
}

#[tokio::test]
async fn owners_editors_are_notified_and_only_owner_dm_has_escaped_preview() {
    let f = Fixture::new();
    f.add_grant("user", &f.editor, "editor");
    // The same person appears in both collection deciders and admin fallback.
    Connection::open(f.home.path().join("users.db")).unwrap()
        .execute("UPDATE users SET role='admin' WHERE id=?1", [&f.owner]).unwrap();
    // Even a manually verified group-shaped identity cannot receive previews.
    f.db.bind_channel_identity(&f.owner, "telegram", "-100123", true).unwrap();
    let (_, mut rec) = f.card().await;
    rec.payload["origin_channel"] = serde_json::json!("telegram");
    rec.payload["origin_chat_id"] = serde_json::json!("101");
    let targets = crate::approval_notify::dashboard_only_targets_for_test(f.home.path(), &rec);
    assert!(targets.contains(&("telegram".into(), "101".into())));
    assert!(targets.contains(&("telegram".into(), "102".into())));
    assert_eq!(targets.iter().filter(|(_, id)| id == "101").count(), 1);
    let notice = approval::notice_body(&rec, false, "soon");
    assert!(!notice.contains("私密內容"));
    for dest in ["-100123", "102"] {
        assert_eq!(chat::body_for_target(f.home.path(), &rec, &("telegram".into(), dest.into()), &notice), notice);
    }
    let body = chat::body_for_target(f.home.path(), &rec, &("telegram".into(), "101".into()), &notice);
    assert!(body.contains("＜secret＞"));
    rec.payload["preview"] = serde_json::json!("🦀".repeat(301));
    assert_eq!(chat::card_body(&rec, "telegram").chars().filter(|c| *c == '🦀').count(), 300);
    rec.payload["verified_requester"] = serde_json::json!(f.editor);
    assert!(!crate::approval_notify::dashboard_only_targets_for_test(f.home.path(), &rec).contains(&("telegram".into(), "101".into())));
}

#[test]
fn only_active_direct_owner_can_set_opt_in_while_master_is_on() {
    let f = Fixture::new();
    let admin = f.db.list_users().unwrap().into_iter().find(|u| u.role == UserRole::Admin).unwrap();
    let mut conn = schema::open_home(f.home.path()).unwrap();
    attach_users(&conn, f.home.path()).unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    assert!(may_set_opt_in(&tx, f.home.path(), &f.cid, &f.owner).is_ok());
    assert!(may_set_opt_in(&tx, f.home.path(), &f.cid, &f.editor).is_err());
    assert!(may_set_opt_in(&tx, f.home.path(), &f.cid, &admin.id).is_err());
    std::fs::write(f.home.path().join("config.toml"), "[knowledge]\nowner_self_add=false").unwrap();
    assert!(may_set_opt_in(&tx, f.home.path(), &f.cid, &f.owner).is_err());
    tx.rollback().unwrap();
}

#[tokio::test]
async fn bound_buttons_use_owner_import_path_and_a_foreign_group_cannot_decide() {
    let f = Fixture::new();
    let (_, rec) = f.card().await;
    let data = format!("duduclaw:decide:apv:ok:{}", rec.id.as_str());
    let mut group = f.context("101");
    group.conversation_id = "-100999".into();
    assert!(crate::decision_notify::route_bound_press(f.home.path(), &group, &data).await.unwrap().is_err());
    assert!(crate::decision_notify::route_bound_press(f.home.path(), &f.context("101"), &data).await.unwrap().is_ok());
    assert_eq!(f.count(), 1);
}


#[tokio::test]
async fn urls_auto_import_regardless_of_current_owner_message() {
    for message in [
        "請加入 HTTPS://Example.COM:443/guide/#section",
        "請閱讀網頁，依工具結果加入相關資料",
        "https://example.com/guide/other",
        "https://evil.example/?next=https://example.com/guide",
    ] {
        let f = Fixture::new();
        let mut proposal = f.proposal("unused");
        proposal.content = approval::Content::Url("https://example.com/guide".into());
        // The title and current message do not authorize the source; the live Owner does.
        proposal.title = Some("https://example.com/guide".into());
        let result = f.submit_inbound(proposal, Some(InboundContent {
            text: message.into(), attachment_hashes: vec![],
        })).await;
        assert!(matches!(result, approval::Proposed::Imported { .. }), "{message}");
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn literal_summary_and_tool_derived_text_auto_import_in_owner_turn() {
    for text in ["Café 文件 內容", "咖啡文件的內容摘要", "工具查詢得到的資料"] {
        let f = Fixture::new();
        let result = f.submit_inbound(f.proposal(text), Some(InboundContent {
            text: "請加入：  Cafe\u{301}\n文件\t內容  ".into(), attachment_hashes: vec![],
        })).await;
        assert!(matches!(result, approval::Proposed::Imported { .. }));
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn current_and_prior_turn_attachments_auto_import_in_owner_turn() {
    for earlier in [false, true] {
        let f = Fixture::new();
        let path = f.home.path().join("uploaded.txt");
        let bytes = b"owner's uploaded document";
        std::fs::write(&path, bytes).unwrap();
        super::super::chat_attachments::note_saved_bytes(&path, "document.txt", bytes);
        let message = format!("[📎 document.txt (file)]({})", path.display());
        let session = format!("webchat:{}", f.owner);
        if earlier { super::super::chat_attachments::claim_in_text(&session, &f.owner, &message); }
        let hashes = super::super::chat_attachments::claim_current(&session, &f.owner, &message);
        let mut proposal = f.proposal("unused");
        // A prior attachment need not be present in this turn's ingress snapshot.
        proposal.content = approval::Content::File { path, name: "different-name.txt".into() };
        let result = f.submit_inbound(proposal, Some(InboundContent {
            text: message, attachment_hashes: hashes,
        })).await;
        assert!(matches!(result, approval::Proposed::Imported { .. }));
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn verified_owner_without_inbound_snapshot_auto_imports() {
    let f = Fixture::new();
    // Identity registration remains live; the optional content snapshot is
    // no longer an authorization requirement.
    assert!(matches!(f.submit_inbound(f.proposal("source data"), None).await, approval::Proposed::Imported { .. }));
    assert_eq!(f.count(), 1);
}

#[test]
fn lost_turn_record_is_rechecked_inside_source_transaction() {
    let f = Fixture::new();
    let mut conn = schema::open_home(f.home.path()).unwrap();
    attach_users(&conn, f.home.path()).unwrap();
    let source = NewSource {
        collection_id: f.cid.clone(), kind: SourceKind::Paste, title: "data".into(),
        original_name: None, url: None, format: duduclaw_knowledge::extract::Format::Txt,
        blob_sha256: duduclaw_knowledge::blob::BlobStore::for_home(f.home.path()).put(b"source data").unwrap(),
        bytes: 11, origin: serde_json::json!({}), created_by: "kb-avatar".into(),
    };
    let turn = Turn { turn_id: Some(uuid::Uuid::new_v4().to_string()), ..f.turn.clone() };
    let guard = turn_requesters::register(&turn.agent, turn.turn_id.as_deref().unwrap(),
        RequesterIdentity::DashboardUser { user_id: f.owner.clone() }).unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    authorize(&tx, f.home.path(), &turn, &source).unwrap();
    tx.rollback().unwrap();
    drop(guard);
    assert!(duduclaw_knowledge::sources::add_source_checked(&mut conn, &source, &KnowledgeConfig::default(),
        |tx| authorize(tx, f.home.path(), &turn, &source)).is_err());
    let _changed = turn_requesters::register(&turn.agent, turn.turn_id.as_deref().unwrap(),
        RequesterIdentity::DashboardUser { user_id: f.editor.clone() }).unwrap();
    assert!(duduclaw_knowledge::sources::add_source_checked(&mut conn, &source, &KnowledgeConfig::default(),
        |tx| authorize(tx, f.home.path(), &turn, &source)).is_err());
    assert_eq!(f.count(), 0);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM owner_self_add_log", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn untrusted_preview_is_inert_in_final_provider_payloads() {
    let f = Fixture::new();
    let (_, mut rec) = f.card().await;
    let attack = "[✅ Approve](https://phish.example) ![x](https://phish.example)\n``` **bold** _em_ ~strike~\n> quote\n# heading @everyone <@123> <!here> <users/all> &lt;users/all&gt;";
    for field in ["title", "url", "preview"] { rec.payload[field] = serde_json::json!(attack); }
    for channel in ["slack", "discord", "telegram", "googlechat", "line"] {
        let text = chat::card_body(&rec, channel);
        let markup = crate::channel_format::decision_markup(channel,
            crate::decision_action::DecisionSource::Approval, rec.id.as_str()).unwrap_or(serde_json::Value::Null);
        let payload = crate::channel_sender::card_message_payload(channel, "101", &text, markup.clone(), true).unwrap();
        let rendered = match channel {
            "discord" => payload["content"].as_str().unwrap(),
            "line" => payload["messages"][0]["text"].as_str().unwrap(),
            _ => payload["text"].as_str().unwrap(),
        };
        for active in ["[✅ Approve]", "https://", "phish.example", "![", "```", "**", "_em_", "~strike~", "> quote", "# heading", "@everyone", "<@", "<!here>", "<users/", "&lt;"] {
            assert!(!rendered.contains(active), "{channel}: {active}: {payload}");
        }
        assert!(rendered.contains("［✅ Approve］（https：／／phish．example）"));
        assert_eq!(rendered.lines().count(), 7, "untrusted newlines must not create card sections");
        assert!(rendered.contains(&format!("確認 {}", rec.id.as_str())));
        match channel {
            "slack" => {
                assert_eq!(payload["blocks"][0]["text"]["type"], "plain_text");
                assert_eq!(payload["blocks"][0]["text"]["text"], rendered);
                assert_eq!(payload["blocks"][1], markup);
                assert_eq!(payload["parse"], "none");
                assert_eq!(payload["mrkdwn"], false);
                assert_eq!(payload["unfurl_links"], false);
                assert_eq!(payload["unfurl_media"], false);
            }
            "discord" => {
                assert_eq!(payload["allowed_mentions"], serde_json::json!({ "parse": [], "users": [], "roles": [], "replied_user": false }));
                assert_eq!(payload["flags"], 4);
                assert_eq!(payload["components"], if markup.is_array() { markup } else { serde_json::json!([markup]) });
            }
            "telegram" => {
                assert!(payload.get("parse_mode").is_none());
                assert!(payload.get("entities").is_none());
                assert_eq!(payload["link_preview_options"]["is_disabled"], true);
                assert_eq!(payload["reply_markup"], markup);
            }
            "line" => assert_eq!(payload["messages"][0]["quickReply"], markup),
            "googlechat" => assert_eq!(payload, serde_json::json!({ "text": rendered })),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn channel_owner_is_reverified_and_delegated_or_unverified_requesters_never_auto_import() {
    let f = Fixture::new();
    let channel = |sender: &str, user_id: Option<String>| RequesterIdentity::ChannelUser {
        channel: "telegram".into(), chat: "101".into(), sender: sender.into(), user_id,
        attested_private: true,
    };
    for (who, auto) in [
        (channel("101", Some(f.owner.clone())), true),
        (channel("unknown", Some(f.owner.clone())), false),
        (channel("101", None), false),
        (channel("102", Some(f.editor.clone())), false),
        (turn_requesters::delegated(RequesterIdentity::DashboardUser { user_id: f.owner.clone() }, "dispatch"), false),
        (RequesterIdentity::Unknown, false),
    ] {
        let id = uuid::Uuid::new_v4().to_string();
        let _guard = turn_requesters::register(&f.turn.agent, &id, who).unwrap();
        let result = approval::propose_in_turn(f.home.path(), f.proposal(&id), Some(id), None).await;
        if auto {
            assert!(matches!(result.unwrap(), approval::Proposed::Imported { .. }));
        } else {
            assert!(!matches!(result, Ok(approval::Proposed::Imported { .. })));
        }
    }
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn another_ais_collection_or_changed_owner_never_auto_imports() {
    for (kind, replacement) in [("agent", "other-ai"), ("user", "other-owner")] {
        let f = Fixture::new();
        schema::open_home(f.home.path()).unwrap().execute(
            "UPDATE collection_acl SET principal_id=?1 WHERE collection_id=?2 AND principal_kind=?3",
            params![replacement, f.cid, kind]).unwrap();
        assert!(matches!(f.submit("tool-derived text").await, approval::Proposed::Filed { .. }));
        assert_eq!(f.count(), 0);
    }
}

#[test]
fn tampered_blob_empty_content_and_url_identity_fail_inside_transaction() {
    let f = Fixture::new();
    let mut conn = schema::open_home(f.home.path()).unwrap();
    attach_users(&conn, f.home.path()).unwrap();
    let blobs = duduclaw_knowledge::blob::BlobStore::for_home(f.home.path());
    let mut source = NewSource {
        collection_id: f.cid.clone(), kind: SourceKind::Paste, title: "metadata".into(),
        original_name: None, url: None, format: duduclaw_knowledge::extract::Format::Txt,
        blob_sha256: blobs.put(b"original").unwrap(), bytes: 8,
        origin: serde_json::json!({}), created_by: "kb-avatar".into(),
    };
    std::fs::write(blobs.path_for(&source.blob_sha256).unwrap(), b"modified").unwrap();
    for case in 0..4 {
        match case {
            1 => { source.blob_sha256 = blobs.put(b"").unwrap(); source.bytes = 0; }
            2 => { source.blob_sha256 = blobs.put(b" \n\t").unwrap(); source.bytes = 3; }
            3 => { source.kind = SourceKind::Url; source.url = Some("https://example.com/".into()); }
            _ => {}
        }
        assert!(duduclaw_knowledge::sources::add_source_checked(&mut conn, &source, &KnowledgeConfig::default(),
            |tx| authorize(tx, f.home.path(), &f.turn, &source)).is_err());
    }
    assert_eq!(f.count(), 0);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM owner_self_add_log", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn empty_file_is_rejected_without_import_or_notification() {
    let f = Fixture::new();
    let path = f.home.path().join("empty.txt");
    std::fs::write(&path, b"").unwrap();
    let mut proposal = f.proposal("unused");
    proposal.content = approval::Content::File { path, name: "empty.txt".into() };
    assert!(approval::propose_in_turn(f.home.path(), proposal, f.turn.turn_id.clone(), None).await.is_err());
    assert_eq!(f.count(), 0);
    let conn = schema::open_home(f.home.path()).unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM owner_self_add_notices", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn auto_import_notification_is_durable_retryable_and_deduplicated_per_source() {
    use super::super::owner_self_add_notify as notify;
    let f = Fixture::new();
    let secret = "private body must never appear in a notification";
    let (a, b) = tokio::join!(f.submit(secret), f.submit(secret));
    assert!(matches!((&a, &b), (approval::Proposed::Imported { .. }, approval::Proposed::Duplicate { .. })
        | (approval::Proposed::Duplicate { .. }, approval::Proposed::Imported { .. })));
    // Reopen as after restart. One committed source means one owed notice.
    let conn = schema::open_home(f.home.path()).unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM owner_self_add_notices", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let first = notify::claim(&conn, 1000).unwrap().unwrap();
    assert_eq!(first.owner_id, f.owner);
    assert_eq!(first.title, "中文標題");
    for channel in ["telegram", "slack", "discord", "googlechat", "line"] {
        let body = notify::body(&first, channel);
        assert!(body.contains(&first.source_id));
        assert!(!body.contains(secret));
        assert!(!body.contains("確認"));
    }
    assert!(notify::claim(&conn, 1001).unwrap().is_none(), "concurrent drainer must respect lease");
    drop(conn); // Delivery failed, or gateway stopped before acknowledgement.
    let conn = schema::open_home(f.home.path()).unwrap();
    let retry = notify::claim(&conn, 1301).unwrap().unwrap();
    assert_eq!(retry.source_id, first.source_id);
    assert_eq!(retry.attempt, 2);
    notify::finish(&conn, &first).unwrap(); // Stale worker cannot acknowledge a new lease.
    assert_eq!(conn.query_row("SELECT delivered FROM owner_self_add_notices", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    notify::finish(&conn, &retry).unwrap();
    assert!(notify::claim(&conn, 2000).unwrap().is_none());
    assert!(matches!(f.submit(secret).await, approval::Proposed::Duplicate { .. }));
    assert!(notify::claim(&conn, 3000).unwrap().is_none());
}

#[tokio::test]
async fn broken_notification_outbox_rolls_back_import_job_and_rate_slot() {
    let f = Fixture::new();
    let conn = schema::open_home(f.home.path()).unwrap();
    conn.execute_batch("CREATE TRIGGER broken_self_add_notice BEFORE INSERT ON owner_self_add_notices
        BEGIN SELECT RAISE(ABORT, 'outbox unavailable'); END;").unwrap();
    assert!(matches!(f.submit("source body").await, approval::Proposed::Filed { .. }));
    assert_eq!(f.count(), 0);
    for table in ["owner_self_add_log", "owner_self_add_notices", "ingest_jobs"] {
        assert_eq!(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 0, "{table}");
    }
}

#[tokio::test]
async fn auto_import_notice_targets_only_active_owners_verified_private_destinations() {
    use super::super::owner_self_add_notify as notify;
    let f = Fixture::new();
    f.db.bind_channel_identity(&f.owner, "telegram", "-100123", true).unwrap();
    f.db.bind_channel_identity(&f.owner, "telegram", "103", false).unwrap();
    assert_eq!(notify::targets(f.home.path(), &f.owner), vec![("telegram".into(), "101".into())]);
    assert!(matches!(f.submit("private tool result").await, approval::Proposed::Imported { .. }));
    Connection::open(f.home.path().join("users.db")).unwrap()
        .execute("UPDATE users SET status='suspended' WHERE id=?1", [&f.owner]).unwrap();
    assert!(notify::targets(f.home.path(), &f.owner).is_empty());
    notify::drain(f.home.path()).await;
    let conn = schema::open_home(f.home.path()).unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM owner_self_add_notices WHERE delivered=0", [], |r| r.get::<_, i64>(0)).unwrap(), 1,
        "inactive Owner/no destination must retain the notice, never send it to Editors or Admins");
}

#[tokio::test]
async fn auto_import_notice_is_terminal_when_owner_is_revoked_or_replaced() {
    use super::super::owner_self_add_notify as notify;
    for replace in [false, true] {
        let f = Fixture::new();
        assert!(matches!(f.submit("private source").await, approval::Proposed::Imported { .. }));
        let conn = schema::open_home(f.home.path()).unwrap();
        conn.execute("DELETE FROM collection_acl WHERE collection_id=?1 AND principal_kind='user' AND principal_id=?2",
            params![f.cid, f.owner]).unwrap();
        if replace { f.add_grant("user", &f.editor, "owner"); }
        // The old Owner is still active and has a verified DM destination.
        assert_eq!(notify::targets(f.home.path(), &f.owner), vec![("telegram".into(), "101".into())]);
        notify::drain(f.home.path()).await;
        let row: (i64, i64, String) = conn.query_row(
            "SELECT delivered, attempts, terminal_reason FROM owner_self_add_notices", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(row, (1, 1, "recipient_not_owner".into()));
        // Restoring ownership must not revive a discarded notice or send it
        // to either the old or replacement Owner on a later drain.
        f.add_grant("user", &f.owner, "owner");
        notify::drain(f.home.path()).await;
        assert!(notify::claim(&conn, i64::MAX - 300).unwrap().is_none());
        assert_eq!(conn.query_row("SELECT attempts FROM owner_self_add_notices", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
}

#[tokio::test]
async fn auto_import_notice_for_a_deleted_source_is_terminal() {
    use super::super::owner_self_add_notify as notify;
    let f = Fixture::new();
    let approval::Proposed::Imported { source_id, .. } = f.submit("deleted private source").await
        else { panic!("auto-import required") };
    let conn = schema::open_home(f.home.path()).unwrap();
    conn.execute("DELETE FROM sources WHERE id=?1", [&source_id]).unwrap();
    notify::drain(f.home.path()).await;
    let row: (i64, String) = conn.query_row("SELECT delivered, terminal_reason FROM owner_self_add_notices", [],
        |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(row, (1, "source_missing".into()));
    assert!(notify::claim(&conn, i64::MAX - 300).unwrap().is_none());
}

#[tokio::test]
async fn auto_import_notice_rechecks_the_sources_current_collection() {
    use super::super::owner_self_add_notify as notify;
    let f = Fixture::new();
    let approval::Proposed::Imported { source_id, .. } = f.submit("moved private source").await
        else { panic!("auto-import required") };
    let mut conn = schema::open_home(f.home.path()).unwrap();
    let other = store::create_collection(&mut conn, &store::NewCollection {
        name: "Other Owner".into(), ..Default::default()
    }, &f.editor, &KnowledgeConfig::default()).unwrap();
    conn.execute("UPDATE sources SET collection_id=?1 WHERE id=?2", params![other.id, source_id]).unwrap();
    assert!(is_direct_owner(&conn, &f.cid, &f.owner).unwrap());
    notify::drain(f.home.path()).await;
    let row: (i64, String) = conn.query_row("SELECT delivered, terminal_reason FROM owner_self_add_notices", [],
        |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(row, (1, "recipient_not_owner".into()));
}

#[tokio::test]
async fn auto_import_notice_acl_read_failure_is_terminal() {
    use super::super::owner_self_add_notify as notify;
    let f = Fixture::new();
    assert!(matches!(f.submit("private source").await, approval::Proposed::Imported { .. }));
    let conn = schema::open_home(f.home.path()).unwrap();
    conn.execute("ALTER TABLE collection_acl RENAME COLUMN role TO unreadable_role", []).unwrap();
    notify::drain(f.home.path()).await;
    let row: (i64, i64, String) = conn.query_row("SELECT delivered, attempts, terminal_reason FROM owner_self_add_notices", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(row, (1, 1, "ownership_check_failed".into()));
    assert!(notify::claim(&conn, i64::MAX - 300).unwrap().is_none());
}
