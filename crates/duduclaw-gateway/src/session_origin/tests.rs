use super::backfill::{BACKFILL_BATCH, OwnerTagIndex, legacy_origin};
use super::*;
use crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL;

const CHANNELS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "line",
    "whatsapp",
    "feishu",
    "googlechat",
    "teams",
    "wecom",
    "dingtalk",
];
const INTERNAL: &[&str] = &[
    "cron",
    "dispatch",
    "heartbeat",
    "reminder",
    "goal",
    "autopilot",
    "responsibility",
];

fn tag(user: &str) -> String {
    crate::webchat::webchat_owner_tag(user)
}

fn webchat_sid(user: &str, conn: &str) -> String {
    format!("webchat:webchat:{}:{conn}", tag(user))
}

fn manager() -> (tempfile::NamedTempFile, SessionManager) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mgr = SessionManager::new(tmp.path()).unwrap();
    (tmp, mgr)
}

fn raw(path: &std::path::Path, sid: &str) -> (Option<String>, Option<String>, Option<String>) {
    let c = rusqlite::Connection::open(path).unwrap();
    c.query_row(
        "SELECT origin_kind, owner_user_id, origin_at FROM sessions WHERE id = ?1",
        [sid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

// ── Pure mapping ────────────────────────────────────────────────

#[test]
fn origin_kind_round_trips_and_unrecognised_values_are_unknown() {
    for k in OriginKind::ALL {
        assert_eq!(OriginKind::parse(k.as_str()), k);
    }
    for bad in ["", "WEBCHAT_USER", "webchat_user ", "admin", "channel\n"] {
        assert_eq!(OriginKind::parse(bad), OriginKind::Unknown, "{bad:?}");
    }
    assert_eq!(
        SessionOrigin::of_kind(OriginKind::WebchatUser),
        SessionOrigin::unknown(),
        "an ownerless webchat_user is not an origin"
    );
}

#[test]
fn webchat_account_and_widget_visitor_are_told_apart_by_the_principal() {
    // Same id shape for both — only the principal differs.
    let sid = "webchat:webchat:0123456789ab:c0ffee00";
    let user = origin_of(sid, Some("u-emp1"));
    assert_eq!(user.kind(), OriginKind::WebchatUser);
    assert_eq!(user.owner_user_id(), Some("u-emp1"));
    let visitor = origin_of(sid, Some("widget-visitor:7d1c"));
    assert_eq!(visitor.kind(), OriginKind::WebchatVisitor);
    assert_eq!(visitor.owner_user_id(), None);
    // No principal: a WebChat id alone proves nothing.
    assert_eq!(origin_of(sid, None), SessionOrigin::unknown());
    // A blank or control-character principal names nobody.
    assert_eq!(origin_of(sid, Some("  ")), SessionOrigin::unknown());
    assert_eq!(origin_of(sid, Some("u\n1")), SessionOrigin::unknown());
}

#[test]
fn webchat_buckets_follow_the_conversation_owner() {
    for sid in [
        "webchat:webchat:0123456789ab:c1#agent:sales",
        "webchat:webchat:0123456789ab:c1#agent:sales#conv:A1",
        "webchat:webchat:0123456789ab:c1#conv:A1",
    ] {
        let o = origin_of(sid, Some("u-emp1"));
        assert_eq!(o.kind(), OriginKind::WebchatUser, "{sid}");
        assert_eq!(o.owner_user_id(), Some("u-emp1"));
    }
}

#[test]
fn a_webchat_principal_never_owns_a_non_webchat_session() {
    for sid in [
        "line:U1",
        "telegram:1#agent:hr",
        "cron:bot",
        "default",
        "webchatx:1",
    ] {
        assert_ne!(
            origin_of(sid, Some("u-emp1")).kind(),
            OriginKind::WebchatUser,
            "{sid}"
        );
    }
    assert_eq!(
        origin_of("line:U1", Some("u-emp1")).kind(),
        OriginKind::Channel
    );
}

#[test]
fn every_messaging_channel_and_its_buckets_are_channel() {
    for ch in CHANNELS {
        for sid in [
            format!("{ch}:chat-1"),
            format!("{ch}:chat-1:thread-9"),
            // A called employee's bucket and a per-employee bot bucket
            // (`telegram:555#agent:sales`).
            format!("{ch}:chat-1#agent:sales"),
            format!("{ch}:chat-1#agent:sales#conv:2"),
        ] {
            assert_eq!(
                origin_of(&sid, None),
                SessionOrigin::of_kind(OriginKind::Channel),
                "{sid}"
            );
        }
    }
    assert_eq!(
        origin_of("discord:thread:42", None).kind(),
        OriginKind::Channel
    );
    assert_eq!(
        origin_of("slack:group:C1", None).kind(),
        OriginKind::Channel
    );
}

#[test]
fn odoo_live_chat_is_a_public_visitor() {
    assert_eq!(
        origin_of("odoolivechat:1:42", None),
        SessionOrigin::of_kind(OriginKind::PublicVisitor)
    );
    assert_eq!(
        origin_of("odoolivechat:1:42#agent:sales", None).kind(),
        OriginKind::PublicVisitor
    );
    assert_eq!(origin_of("odoolivechat:", None).kind(), OriginKind::Unknown);
}

#[test]
fn internal_prefixes_are_internal_and_the_rest_unknown() {
    for p in INTERNAL {
        assert_eq!(
            origin_of(&format!("{p}:x"), None),
            SessionOrigin::of_kind(OriginKind::Internal),
            "{p}"
        );
    }
    for sid in [
        "",
        "default",
        "anonymous",
        "acp:abc",
        "lineX:1",
        "line",
        "cronx:bot",
        "newchat:1",
    ] {
        assert_eq!(origin_of(sid, None), SessionOrigin::unknown(), "{sid:?}");
    }
}

// ── Store ───────────────────────────────────────────────────────

#[tokio::test]
async fn the_first_origin_wins() {
    let (tmp, mgr) = manager();
    let sid = webchat_sid("u-emp1", "c1");
    mgr.get_or_create_with_origin(&sid, "sales", &SessionOrigin::webchat_user("u-emp1"))
        .await
        .unwrap();
    // A second, different stamp never overwrites.
    mgr.get_or_create_with_origin(&sid, "sales", &SessionOrigin::webchat_user("u-other"))
        .await
        .unwrap();
    mgr.get_or_create_with_origin(&sid, "sales", &SessionOrigin::of_kind(OriginKind::Channel))
        .await
        .unwrap();
    let o = mgr.session_origin(&sid).await.unwrap().unwrap();
    assert_eq!(o, SessionOrigin::webchat_user("u-emp1"));
    let (_, _, at) = raw(tmp.path(), &sid);
    assert!(chrono::DateTime::parse_from_rfc3339(&at.unwrap()).is_ok());
}

#[tokio::test]
async fn legacy_get_or_create_writes_unknown_and_never_stamps_old_rows() {
    let (tmp, mgr) = manager();
    mgr.get_or_create("fresh", "a").await.unwrap();
    assert_eq!(
        mgr.session_origin("fresh").await.unwrap(),
        Some(SessionOrigin::unknown())
    );
    // A row from before the migration (NULL origin) stays NULL for the
    // backfill to classify.
    let c = rusqlite::Connection::open(tmp.path()).unwrap();
    c.execute(
        "INSERT INTO sessions (id, agent_id, last_active, created_at) VALUES ('old','a','t','t')",
        [],
    )
    .unwrap();
    mgr.get_or_create("old", "a").await.unwrap();
    assert_eq!(raw(tmp.path(), "old").0, None);
    assert_eq!(mgr.session_origin("old").await.unwrap(), None);
}

#[tokio::test]
async fn a_resumed_legacy_row_takes_the_live_origin_once() {
    let (tmp, mgr) = manager();
    let sid = webchat_sid("u-emp1", "old-conn");
    let c = rusqlite::Connection::open(tmp.path()).unwrap();
    c.execute(
        "INSERT INTO sessions (id, agent_id, last_active, created_at) VALUES (?1,'sales','t','t')",
        [&sid],
    )
    .unwrap();
    AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope("u-emp1".into(), ensure_turn_session(&mgr, &sid, "sales"))
        .await
        .unwrap();
    assert_eq!(
        mgr.session_origin(&sid).await.unwrap(),
        Some(SessionOrigin::webchat_user("u-emp1"))
    );
    // Another principal on the same row later changes nothing.
    AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope("u-other".into(), ensure_turn_session(&mgr, &sid, "sales"))
        .await
        .unwrap();
    assert_eq!(
        mgr.session_origin(&sid).await.unwrap(),
        Some(SessionOrigin::webchat_user("u-emp1"))
    );
}

#[tokio::test]
async fn another_agents_row_is_refused_and_not_stamped() {
    let (tmp, mgr) = manager();
    let c = rusqlite::Connection::open(tmp.path()).unwrap();
    c.execute(
        "INSERT INTO sessions (id, agent_id, last_active, created_at) VALUES ('line:U1','hr','t','t')",
        [],
    )
    .unwrap();
    assert!(ensure_turn_session(&mgr, "line:U1", "sales").await.is_err());
    assert_eq!(raw(tmp.path(), "line:U1").0, None);
}

#[tokio::test]
async fn a_webchat_user_row_without_an_owner_reads_as_unknown() {
    let (tmp, mgr) = manager();
    let c = rusqlite::Connection::open(tmp.path()).unwrap();
    c.execute(
        "INSERT INTO sessions (id, agent_id, last_active, created_at, origin_kind)
         VALUES ('w','a','t','t','webchat_user')",
        [],
    )
    .unwrap();
    assert_eq!(
        mgr.session_origin("w").await.unwrap(),
        Some(SessionOrigin::unknown())
    );
    assert_eq!(mgr.session_origin("missing").await.unwrap(), None);
}

// ── Creation paths (task-local scopes as production sets them) ─

#[tokio::test]
async fn ensure_turn_session_stamps_each_kind_of_turn() {
    let (_tmp, mgr) = manager();
    let account = webchat_sid("u-emp1", "c1");
    let visitor = webchat_sid("widget-visitor:1234", "c2");
    // WebChat turns run inside the socket's principal scope.
    AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope(
            "u-emp1".into(),
            ensure_turn_session(&mgr, &account, "sales"),
        )
        .await
        .unwrap();
    AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope(
            "widget-visitor:1234".into(),
            ensure_turn_session(&mgr, &visitor, "sales"),
        )
        .await
        .unwrap();
    // `@`-called employee bucket inside the account's WebChat.
    let bucket = format!("{account}#agent:hr");
    AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope("u-emp1".into(), ensure_turn_session(&mgr, &bucket, "hr"))
        .await
        .unwrap();
    for sid in [
        "line:Cgroup1",
        "telegram:555#agent:sales",
        "odoolivechat:1:42",
        "cron:daily",
        "default",
    ] {
        ensure_turn_session(&mgr, sid, "sales").await.unwrap();
    }
    let got = |sid: String| {
        let mgr = &mgr;
        async move { mgr.session_origin(&sid).await.unwrap().unwrap() }
    };
    assert_eq!(got(account).await, SessionOrigin::webchat_user("u-emp1"));
    assert_eq!(got(bucket).await, SessionOrigin::webchat_user("u-emp1"));
    assert_eq!(got(visitor).await.kind(), OriginKind::WebchatVisitor);
    assert_eq!(got("line:Cgroup1".into()).await.kind(), OriginKind::Channel);
    assert_eq!(
        got("telegram:555#agent:sales".into()).await.kind(),
        OriginKind::Channel
    );
    assert_eq!(
        got("odoolivechat:1:42".into()).await.kind(),
        OriginKind::PublicVisitor
    );
    assert_eq!(got("cron:daily".into()).await.kind(), OriginKind::Internal);
    assert_eq!(got("default".into()).await.kind(), OriginKind::Unknown);
}

#[tokio::test]
async fn a_webchat_status_command_records_the_owner() {
    // `/status` as the first message creates the session from the command
    // path, which the WebChat socket now runs inside the principal scope.
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx(dir.path());
    let sid = webchat_sid("u-emp1", "c1");
    let reply = AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope(
            "u-emp1".into(),
            crate::chat_commands::handle_command(
                &crate::chat_commands::ChatCommand::Status,
                &ctx,
                &sid,
                "main",
                false,
                "",
            ),
        )
        .await;
    assert!(reply.contains("Session"), "{reply}");
    assert_eq!(
        ctx.session_manager.session_origin(&sid).await.unwrap(),
        Some(SessionOrigin::webchat_user("u-emp1"))
    );
    // The same command from a messaging app.
    crate::chat_commands::handle_command(
        &crate::chat_commands::ChatCommand::Status,
        &ctx,
        "telegram:42",
        "main",
        false,
        "42",
    )
    .await;
    assert_eq!(
        ctx.session_manager
            .session_origin("telegram:42")
            .await
            .unwrap()
            .map(|o| o.kind()),
        Some(OriginKind::Channel)
    );
}

#[tokio::test]
async fn a_takeover_recorded_turn_through_the_reply_pipeline_is_channel() {
    // Full guarded → inner pipeline: a held conversation records the turn
    // without any model call.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::write(home.join("config.toml"), "[takeover]\nenabled = true\n").unwrap();
    seed_verified_manager(home, "555");
    let ctx = test_ctx(home);
    let reply = crate::channel_reply::build_guarded_reply_with_session(
        "我來處理",
        &ctx,
        "telegram:1",
        "555",
        None,
    )
    .await
    .text
    .clone();
    assert!(!reply.is_empty(), "takeover announcement expected");
    assert_eq!(
        ctx.session_manager
            .session_origin("telegram:1")
            .await
            .unwrap()
            .map(|o| o.kind()),
        Some(OriginKind::Channel)
    );
}

fn test_ctx(home: &std::path::Path) -> crate::channel_reply::ReplyContext {
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.join("agents")),
    ));
    let sessions = std::sync::Arc::new(SessionManager::new(&home.join("sessions.db")).unwrap());
    let status: crate::channel_reply::ChannelStatusMap =
        std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    crate::channel_reply::ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
}

/// A dashboard Admin with a verified Telegram binding (may take over).
fn seed_verified_manager(home: &std::path::Path, telegram_id: &str) {
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let u = db
        .create_user(
            "manager@example.com",
            "王小明",
            "correct-horse-battery-staple",
            duduclaw_auth::UserRole::Admin,
        )
        .unwrap();
    db.bind_channel_identity(&u.id, "telegram", telegram_id, true)
        .unwrap();
}

// ── Migration and backfill ─────────────────────────────────────

/// The `sessions` table exactly as the first schema created it.
fn old_schema_db(path: &std::path::Path, ids: &[&str]) {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, summary TEXT DEFAULT '',
            total_tokens INTEGER DEFAULT 0, last_active TEXT NOT NULL,
            model TEXT DEFAULT 'auto', created_at TEXT NOT NULL);
         CREATE TABLE session_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id), role TEXT NOT NULL,
            content TEXT NOT NULL, tokens INTEGER DEFAULT 0, timestamp TEXT NOT NULL);",
    )
    .unwrap();
    for id in ids {
        c.execute(
            "INSERT INTO sessions (id, agent_id, last_active, created_at)
             VALUES (?1, 'sales', '2026-07-01T00:00:00Z', '2026-07-01T00:00:00Z')",
            [id],
        )
        .unwrap();
        c.execute(
            "INSERT INTO session_messages (session_id, role, content, timestamp)
             VALUES (?1, 'user', 'hello', '2026-07-01T00:00:00Z')",
            [id],
        )
        .unwrap();
    }
}

#[tokio::test]
async fn an_old_schema_database_migrates_and_still_reads() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    old_schema_db(tmp.path(), &["line:U1", "webchat:webchat:abc:c1"]);
    let mgr = SessionManager::new(tmp.path()).unwrap();
    let c = rusqlite::Connection::open(tmp.path()).unwrap();
    let cols: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info('sessions')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for col in ["origin_kind", "owner_user_id", "origin_at"] {
        assert!(cols.iter().any(|c| c == col), "{col} missing: {cols:?}");
    }
    let index: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_sessions_agent_origin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
    // Old rows are not stamped by the migration itself.
    assert_eq!(mgr.session_origin("line:U1").await.unwrap(), None);
    // Existing readers are unaffected.
    assert_eq!(mgr.get_messages("line:U1").await.unwrap().len(), 1);
    assert_eq!(mgr.list_sessions(Some("sales"), 10).await.unwrap().len(), 2);
    // Re-opening is idempotent.
    drop(mgr);
    let mgr = SessionManager::new(tmp.path()).unwrap();
    assert_eq!(mgr.session_origin("line:U1").await.unwrap(), None);
}

#[test]
fn legacy_ids_map_only_where_reliable() {
    let index = OwnerTagIndex::from_user_ids(["u-emp1", "u-mgr"]);
    let emp = legacy_origin(&webchat_sid("u-emp1", "c1"), &index);
    assert_eq!(emp, SessionOrigin::webchat_user("u-emp1"));
    assert_eq!(
        legacy_origin(
            &format!("{}#agent:hr#conv:2", webchat_sid("u-mgr", "c1")),
            &index
        ),
        SessionOrigin::webchat_user("u-mgr")
    );
    for sid in [
        // A widget visitor's tag matches no account.
        webchat_sid("widget-visitor:1234", "c1"),
        // Connection-keyed ids from before 2026-08-03.
        "webchat:webchat:127.0.0.1:00c7766f".to_string(),
        "webchat:webchat:::1:00c7766f".to_string(),
        // Tag without a connection part, upper-case tag, other WebChat shapes.
        format!("webchat:webchat:{}", tag("u-emp1")),
        format!("webchat:webchat:{}:c1", tag("u-emp1").to_uppercase()),
        "webchat:s1".to_string(),
        "webchat:".to_string(),
        "default".to_string(),
        "acp:abc".to_string(),
    ] {
        assert_eq!(
            legacy_origin(&sid, &index),
            SessionOrigin::unknown(),
            "{sid}"
        );
    }
    assert_eq!(legacy_origin("line:U1", &index).kind(), OriginKind::Channel);
    assert_eq!(
        legacy_origin("telegram:9#agent:hr", &index).kind(),
        OriginKind::Channel
    );
    assert_eq!(
        legacy_origin("odoolivechat:1:2", &index).kind(),
        OriginKind::PublicVisitor
    );
    assert_eq!(
        legacy_origin("dispatch:r1", &index).kind(),
        OriginKind::Internal
    );
}

#[test]
fn a_shared_owner_tag_attributes_to_nobody() {
    let target = tag("u-a");
    // The same id listed twice is not a collision.
    let index = OwnerTagIndex::from_user_ids(["u-a", "u-a", "u-b"]);
    assert_eq!(
        legacy_origin(&webchat_sid("u-a", "c1"), &index),
        SessionOrigin::webchat_user("u-a")
    );
    // Two accounts on one tag: neither gets the conversation.
    let index = OwnerTagIndex::colliding_for_test(&target, &["u-a", "u-twin"]);
    assert_eq!(
        legacy_origin(&webchat_sid("u-a", "c1"), &index),
        SessionOrigin::unknown()
    );
}

#[tokio::test]
async fn backfill_classifies_old_rows_and_is_idempotent() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let emp = webchat_sid("u-emp1", "c1");
    let visitor = webchat_sid("widget-visitor:9", "c2");
    let ids = [
        emp.as_str(),
        visitor.as_str(),
        "webchat:webchat:10.0.0.1:conn",
        "line:Cgroup",
        "telegram:5#agent:hr",
        "odoolivechat:1:42",
        "goal:t1:1",
        "default",
    ];
    old_schema_db(tmp.path(), &ids);
    let mgr = SessionManager::new(tmp.path()).unwrap();
    let index = OwnerTagIndex::from_user_ids(["u-emp1"]);
    let counts = mgr.backfill_origins(&index).await.unwrap();
    assert_eq!(counts.total(), ids.len() as u64);
    let by = |k| counts.by_kind.get(&k).copied().unwrap_or(0);
    assert_eq!(by(OriginKind::WebchatUser), 1);
    assert_eq!(by(OriginKind::Unknown), 3);
    assert_eq!(by(OriginKind::Channel), 2);
    assert_eq!(by(OriginKind::PublicVisitor), 1);
    assert_eq!(by(OriginKind::Internal), 1);
    assert_eq!(
        mgr.session_origin(&emp).await.unwrap(),
        Some(SessionOrigin::webchat_user("u-emp1"))
    );
    // The widget visitor cannot be told from an employee by id: unknown.
    assert_eq!(
        mgr.session_origin(&visitor).await.unwrap(),
        Some(SessionOrigin::unknown())
    );
    // Rerun: nothing left to change.
    assert_eq!(mgr.backfill_origins(&index).await.unwrap().total(), 0);
}

#[tokio::test]
async fn backfill_keeps_rows_a_live_turn_stamped_first() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let sid = webchat_sid("widget-visitor:9", "c2");
    old_schema_db(tmp.path(), &[&sid]);
    let mgr = SessionManager::new(tmp.path()).unwrap();
    // The visitor resumes before the backfill runs.
    AUTHENTICATED_WEBCHAT_PRINCIPAL
        .scope(
            "widget-visitor:9".into(),
            ensure_turn_session(&mgr, &sid, "sales"),
        )
        .await
        .unwrap();
    let counts = mgr
        .backfill_origins(&OwnerTagIndex::from_user_ids(["widget-visitor:9"]))
        .await
        .unwrap();
    assert_eq!(counts.total(), 0);
    assert_eq!(
        mgr.session_origin(&sid).await.unwrap().map(|o| o.kind()),
        Some(OriginKind::WebchatVisitor)
    );
}

#[tokio::test]
async fn backfill_runs_in_batches() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let ids: Vec<String> = (0..BACKFILL_BATCH * 2 + 7)
        .map(|i| format!("line:U{i}"))
        .collect();
    old_schema_db(
        tmp.path(),
        &ids.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let mgr = SessionManager::new(tmp.path()).unwrap();
    let counts = mgr
        .backfill_origins(&OwnerTagIndex::default())
        .await
        .unwrap();
    assert_eq!(counts.total(), ids.len() as u64);
    let c = rusqlite::Connection::open(tmp.path()).unwrap();
    let left: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE origin_kind IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn backfill_run_reads_every_account_and_audits_once() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let users = std::sync::Arc::new(duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap());
    let active = users
        .create_user(
            "a@example.com",
            "Active",
            "pw-123456789",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap();
    let gone = users
        .create_user(
            "g@example.com",
            "Gone",
            "pw-123456789",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap();
    users
        .set_user_status(&gone.id, duduclaw_auth::UserStatus::Offboarded)
        .unwrap();
    let db = home.join("sessions.db");
    let (a, g) = (webchat_sid(&active.id, "c1"), webchat_sid(&gone.id, "c1"));
    old_schema_db(&db, &[&a, &g]);
    let mgr = SessionManager::new(&db).unwrap();
    let counts = backfill::run(home, &mgr, users.clone()).await.unwrap();
    assert_eq!(counts.by_kind.get(&OriginKind::WebchatUser), Some(&2));
    assert_eq!(
        mgr.session_origin(&g).await.unwrap(),
        Some(SessionOrigin::webchat_user(&gone.id))
    );
    let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|v: &serde_json::Value| v["event_type"] == backfill::BACKFILL_AUDIT_EVENT)
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["details"]["updated"], 2);
    assert_eq!(events[0]["details"]["by_kind"]["webchat_user"], 2);
    // Nothing to do on the second boot: no second event.
    backfill::run(home, &mgr, users).await.unwrap();
    let again = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    assert_eq!(
        again
            .lines()
            .filter(|l| l.contains(backfill::BACKFILL_AUDIT_EVENT))
            .count(),
        1
    );
}
