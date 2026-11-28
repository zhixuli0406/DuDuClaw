//! Version detection and per-version mapping of sessions, tasks and cron.
//! Every fixture is created from the pinned catalogs themselves.
use super::*;
use crate::{authority::{extract, AuthorityProposal}, import, sqlite::CHANNEL_SETTINGS_SCHEMA, ImportRequest, SourceKind};
use duduclaw_isolation::{ids::{EmployeeId, RequestId}, migration::{ConfirmationCategory as C, ProposalValue}};

fn request(kind: SourceKind, title: &str) -> ImportRequest {
    ImportRequest { version: 1, migration_id: RequestId([6; 16]),
        employee: matches!(kind, SourceKind::SessionsSqlite).then_some(EmployeeId([5; 16])),
        source_id: "old:store".into(), kind, title: title.into(), legacy_agent: Some("sales".into()),
        has_wal: false, blob_handle: None }
}
fn store(schema: &str, rows: &str) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = Connection::open(file.path()).unwrap();
    conn.execute_batch(schema).unwrap();
    conn.execute_batch(rows).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    file
}
fn source(file: &tempfile::NamedTempFile) -> std::fs::File { std::fs::File::open(file.path()).unwrap() }
fn value<'a>(proposal: &'a AuthorityProposal, name: &str) -> &'a str {
    match &proposal.fields.iter().find(|f| f.name == name).unwrap().value { ProposalValue::Text { value } => value, _ => panic!("text") }
}
fn detected(schema: &str, store: Store, extension: Option<&str>) -> Detected {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(schema).unwrap();
    if let Some(extension) = extension { conn.execute_batch(extension).unwrap(); }
    detect(&conn, store, Some(CHANNEL_SETTINGS_SCHEMA).filter(|_| store == Store::Sessions)).unwrap()
}

/// Shared acceptance path; `extra_*` add rows only the given version can hold.
fn accepts(version: SchemaVersion, sessions: &str, tasks: &str, cron: &str, extra_messages: &str, extra_cron: &str) {
    // Each catalog is detected as exactly its own version, never the other.
    assert_eq!(detected(sessions, Store::Sessions, None), Detected { version, extension: false });
    assert_eq!(detected(sessions, Store::Sessions, Some(CHANNEL_SETTINGS_SCHEMA)), Detected { version, extension: true });
    assert_eq!(detected(tasks, Store::Tasks, None).version, version);
    assert_eq!(detected(cron, Store::Cron, None).version, version);

    let rows = format!("INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now'),('o','other','now','now');
        INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','legacy question','now'),('o','user','OTHER-AGENT','now');
        INSERT INTO session_messages(session_id,role,content,timestamp,hidden) VALUES ('s','assistant','HIDDEN-TEXT','now',1);
        INSERT INTO session_messages(session_id,role,content,timestamp,undone_at) VALUES ('s','assistant','UNDONE-TEXT','now','now');
        {extra_messages}");
    let file = store(sessions, &rows);
    let mut out = Vec::new();
    assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Ok(1));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("legacy question"));
    for hidden in ["HIDDEN-TEXT", "UNDONE-TEXT", "PENDING-TEXT", "OTHER-AGENT"] { assert!(!text.contains(hidden)); }
    assert!(take_schema_refusal().is_none());

    let file = store(&format!("{sessions}\n{CHANNEL_SETTINGS_SCHEMA}"),
        "INSERT INTO channel_settings VALUES('telegram','SECRET-USER','allowed_users','SECRET-VALUE','now');");
    let p = extract(&request(SourceKind::AuthoritySessionsSqlite, "sessions.db"), source(&file), None).unwrap();
    assert_eq!(p.len(), 1); assert_eq!(p[0].category, C::Policy);
    assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));

    let file = store(tasks, "INSERT INTO tasks(id,title,description,assigned_to,created_at,updated_at,kind,goal_mode,acceptance_criteria)
            VALUES ('goal','Quarterly goal','Do work','sales','now','now','goal',1,'shipped');
        INSERT INTO tasks(id,title,assigned_to,created_at,updated_at,status,kind,goal_mode) VALUES ('closed','CLOSED-GOAL','sales','now','now','done','goal',1);
        INSERT INTO tasks(id,title,assigned_to,created_at,updated_at) VALUES ('plain','PLAIN-TASK','sales','now','now');");
    let p = extract(&request(SourceKind::AuthorityTasksSqlite, "tasks.db"), source(&file), None).unwrap();
    assert_eq!(p.len(), 1); assert_eq!(p[0].category, C::Schedule); assert_eq!(value(&p[0], "employee_id"), "sales");
    let encoded = serde_json::to_string(&p).unwrap();
    assert!(encoded.contains("Quarterly goal")); assert!(!encoded.contains("CLOSED-GOAL")); assert!(!encoded.contains("PLAIN-TASK"));

    let file = store(cron, &format!("INSERT INTO cron_tasks(id,name,agent_id,cron,task,created_at,updated_at,cron_timezone,last_error,notify_chat_id)
        VALUES ('c','Daily report','sales','0 9 * * *','Prepare report','now','now','Asia/Taipei','SECRET-ERROR','SECRET-CHAT'); {extra_cron}"));
    let p = extract(&request(SourceKind::AuthorityCronSqlite, "cron_tasks"), source(&file), None).unwrap();
    assert_eq!(p.len(), 1); assert_eq!(p[0].fields.len(), 5);
    assert_eq!(value(&p[0], "employee_id"), "sales"); assert_eq!(value(&p[0], "schedule"), "0 9 * * *");
    assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
}

#[test]
fn import_accepts_v1793_session_tasks_cron() {
    accepts(SchemaVersion::V1793, SESSION_V1793, TASKS_V1793, CRON_V1793, "", "");
}

#[test]
fn import_accepts_v180_session_tasks_cron() {
    // v1.80 rows: an unpublished assistant reply stays out; actor columns are ignored.
    accepts(SchemaVersion::V180, SESSION_V180, TASKS_V180, CRON_V180,
        "INSERT INTO session_messages(session_id,role,content,timestamp,delivery_pending) VALUES ('s','assistant','PENDING-TEXT','now',1);
         UPDATE session_messages SET client_msg_id='client-1' WHERE content='legacy question';",
        "UPDATE cron_tasks SET on_behalf_of='SECRET-PRINCIPAL',created_by_user_id='SECRET-CREATOR',scoped_by_user_id='SECRET-SCOPER';");
}

#[test]
fn import_accepts_v181_tasks_and_never_reads_markers() {
    // A store made by this release (v1.80.x plus task_store_markers) is accepted;
    // the v1.80 catalog is still accepted on its own.
    assert_eq!(detected(TASKS_V181, Store::Tasks, None).version, SchemaVersion::V180);
    assert_eq!(detected(TASKS_V180, Store::Tasks, None).version, SchemaVersion::V180);
    assert!(take_schema_refusal().is_none());
    let goal = "INSERT INTO tasks(id,title,description,assigned_to,created_at,updated_at,kind,goal_mode) VALUES ('goal','Quarterly goal','Do work','sales','now','now','goal',1);";
    for (schema, rows) in [(TASKS_V180, goal.to_string()),
        (TASKS_V181, format!("{goal} INSERT INTO task_store_markers(name,applied_at) VALUES ('MARKER-CANARY','now');"))] {
        let file = store(schema, &rows);
        let p = extract(&request(SourceKind::AuthorityTasksSqlite, "tasks.db"), source(&file), None).unwrap();
        assert_eq!(p.len(), 1);
        let encoded = serde_json::to_string(&p).unwrap();
        assert!(encoded.contains("Quarterly goal") && !encoded.contains("MARKER-CANARY"));
    }
    // The importer's authorizer denies every read of the marker table.
    let file = store(TASKS_V181, "INSERT INTO task_store_markers(name,applied_at) VALUES ('MARKER-CANARY','now');");
    let mut conn = Connection::open(file.path()).unwrap();
    conn.authorizer(Some(|c: rusqlite::hooks::AuthContext<'_>| match c.action {
        rusqlite::hooks::AuthAction::Read { table_name: "task_store_markers", .. } => rusqlite::hooks::Authorization::Deny,
        _ => rusqlite::hooks::Authorization::Allow,
    })).unwrap();
    assert!(conn.prepare("SELECT name FROM task_store_markers").is_err());
}

#[test]
fn rm173_import_accepts_v181_sessions_and_skips_user_cleared_conversations() {
    // A store made by this release is detected as v1.81, with or without the
    // channel settings; the older catalogs still detect as themselves.
    // Both pinned v1.81 shapes (fresh, and upgraded from an older store).
    for schema in [SESSION_V181, SESSION_V181_UPGRADED] {
        assert_eq!(detected(schema, Store::Sessions, None), Detected { version: SchemaVersion::V181, extension: false });
        assert_eq!(detected(schema, Store::Sessions, Some(CHANNEL_SETTINGS_SCHEMA)), Detected { version: SchemaVersion::V181, extension: true });
    }
    assert_eq!(detected(SESSION_V180, Store::Sessions, None).version, SchemaVersion::V180);
    assert_eq!(detected(SESSION_V1793, Store::Sessions, None).version, SchemaVersion::V1793);
    assert!(take_schema_refusal().is_none());

    // 's' live, 'r' archived by retention: imported. A cleared conversation
    // (moved to `#cleared:`) and a restored former clear (user_cleared_at is
    // sticky) are never imported, nor is the migration marker. Archives with
    // no recorded reason (an interrupted migration, or a v1.80 binary after a
    // downgrade) follow the shared legacy rule: 80 hours after the last
    // activity is an inactivity archive ('g80', imported), 1 hour a user clear
    // ('g1'). A `user` reason without the clear mark, an unknown reason, an
    // empty clear mark and an orphan message are never imported.
    let rows = "INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now'),('o','other','now','now');
        INSERT INTO sessions(id,agent_id,last_active,created_at,archived_at,archive_reason) VALUES ('r','sales','now','now','then','retention');
        INSERT INTO sessions(id,agent_id,last_active,created_at,archived_at,archive_reason,user_cleared_at)
            VALUES ('c#cleared:1700000000000','sales','now','now','then','user','then');
        INSERT INTO sessions(id,agent_id,last_active,created_at,user_cleared_at) VALUES ('u','sales','now','now','then');
        INSERT INTO sessions(id,agent_id,last_active,created_at,archived_at) VALUES
            ('g1','sales','2026-01-01T00:00:00Z','now','2026-01-01T01:00:00Z'),
            ('g80','sales','2026-01-01T00:00:00Z','now','2026-01-04T08:00:00Z');
        INSERT INTO sessions(id,agent_id,last_active,created_at,archived_at,archive_reason) VALUES
            ('ru','sales','2026-01-01T00:00:00Z','now','2026-01-04T08:00:00Z','user'),
            ('rb','sales','2026-01-01T00:00:00Z','now','2026-01-04T08:00:00Z','bogus');
        INSERT INTO sessions(id,agent_id,last_active,created_at,user_cleared_at) VALUES ('ue','sales','now','now','');
        INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','legacy question','now'),
            ('r','assistant','retained answer','now'),('o','user','OTHER-AGENT','now'),
            ('c#cleared:1700000000000','user','CLEARED-TEXT','now'),('u','user','RESTORED-CLEAR-TEXT','now'),
            ('g1','user','UNLABELLED-CLEAR-TEXT','now'),('g80','assistant','unlabelled idle answer','now'),
            ('ru','user','USER-REASON-TEXT','now'),('rb','user','BOGUS-REASON-TEXT','now'),('ue','user','EMPTY-MARK-TEXT','now');
        INSERT INTO session_messages(session_id,role,content,timestamp,hidden) VALUES ('s','assistant','HIDDEN-TEXT','now',1);
        INSERT INTO session_messages(session_id,role,content,timestamp,undone_at) VALUES ('s','assistant','UNDONE-TEXT','now','now');
        INSERT INTO session_messages(session_id,role,content,timestamp,delivery_pending) VALUES ('s','assistant','PENDING-TEXT','now',1);
        INSERT INTO session_migrations(name,applied_at) VALUES ('MARKER-CANARY','now');
        PRAGMA foreign_keys=OFF;
        INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('ghost','user','ORPHAN-TEXT','now');";
    for base in [SESSION_V181, SESSION_V181_UPGRADED] {
        for schema in [base.to_owned(), format!("{base}\n{CHANNEL_SETTINGS_SCHEMA}")] {
            let file = store(&schema, rows);
            let mut out = Vec::new();
            assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Ok(3));
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("legacy question") && text.contains("retained answer") && text.contains("unlabelled idle answer"), "{text}");
            for hidden in ["CLEARED-TEXT", "RESTORED-CLEAR-TEXT", "HIDDEN-TEXT", "UNDONE-TEXT", "PENDING-TEXT", "OTHER-AGENT", "MARKER-CANARY", "cleared:",
                "UNLABELLED-CLEAR-TEXT", "USER-REASON-TEXT", "BOGUS-REASON-TEXT", "EMPTY-MARK-TEXT", "ORPHAN-TEXT",
                "2026-01-04T08:00:00Z", "retention", "bogus"] {
                assert!(!text.contains(hidden), "{hidden} leaked: {text}");
            }
            assert!(take_schema_refusal().is_none());
        }
    }

    // The authority reader accepts the v1.81 store too; only channel settings become a proposal.
    let file = store(&format!("{SESSION_V181}\n{CHANNEL_SETTINGS_SCHEMA}"),
        "INSERT INTO channel_settings VALUES('telegram','SECRET-USER','allowed_users','SECRET-VALUE','now');
         INSERT INTO session_migrations(name,applied_at) VALUES ('SECRET-MARKER','now');");
    let p = extract(&request(SourceKind::AuthoritySessionsSqlite, "sessions.db"), source(&file), None).unwrap();
    assert_eq!(p.len(), 1); assert_eq!(p[0].category, C::Policy);
    assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
}

#[test]
fn rm173_import_classifies_archived_conversations_of_v180_and_v1793_sources() {
    // Before v1.81 both a user clear and the 72-hour inactivity task set
    // `sessions.archived_at`. The shared legacy rule tells them apart: an
    // archive at least 72 hours after the last activity is an inactivity
    // archive (imported); a shorter gap or an unparseable time is a user clear
    // (never imported).
    let rows = "INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('live','sales','now','now');
        INSERT INTO sessions(id,agent_id,last_active,created_at,archived_at) VALUES
            ('cleared','sales','2026-01-01T00:00:00Z','now','2026-01-01T01:00:00Z'),
            ('idle','sales','2026-01-01T00:00:00Z','now','2026-01-04T08:00:00.123456789+00:00'),
            ('unparsed','sales','now','now','then');
        INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('live','user','LIVE-QUESTION','now'),
            ('live','assistant','LIVE-ANSWER','now'),('cleared','user','CLEARED-QUESTION','now'),('cleared','assistant','CLEARED-ANSWER','now'),
            ('idle','assistant','IDLE-ANSWER','now'),('unparsed','user','UNPARSED-TEXT','now');";
    for (schema, version) in [(SESSION_V180, SchemaVersion::V180), (SESSION_V1793, SchemaVersion::V1793)] {
        assert_eq!(detected(schema, Store::Sessions, None).version, version);
        for schema in [schema.to_owned(), format!("{schema}\n{CHANNEL_SETTINGS_SCHEMA}")] {
            let file = store(&schema, rows);
            let mut out = Vec::new();
            assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Ok(3), "{version:?}");
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("LIVE-QUESTION") && text.contains("LIVE-ANSWER") && text.contains("IDLE-ANSWER"), "{version:?}: {text}");
            for hidden in ["CLEARED-QUESTION", "CLEARED-ANSWER", "UNPARSED-TEXT", "2026-01-04T08:00:00"] {
                assert!(!text.contains(hidden), "{version:?}: {hidden} leaked: {text}");
            }
            assert!(take_schema_refusal().is_none());
        }
    }
}

/// Replays `catalog` statement by statement on `conn`, ignoring each failure:
/// what `SessionManager::init_tables` does on an existing store (its ALTERs
/// ignore "duplicate column", its CREATEs are IF NOT EXISTS). The catalogs'
/// comments contain no `;`. The gateway test
/// `migration_import_schema_catalog_matches_upgraded_session_stores` checks
/// the same paths with the real `SessionManager`.
fn upgrade(conn: &Connection, catalog: &str) {
    for statement in catalog.split(';') { let _ = conn.execute_batch(statement); }
}

#[test]
fn rm173_upgraded_session_stores_match_a_pinned_catalog() {
    // v1.79.3 -> v1.80.x yields exactly the fresh v1.80.x catalog.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SESSION_V1793).unwrap();
    upgrade(&conn, SESSION_V180);
    assert_eq!(detect(&conn, Store::Sessions, None).unwrap(), Detected { version: SchemaVersion::V180, extension: false });
    // Every path into v1.81 from an older store yields the upgraded shape,
    // never the fresh one.
    let (upgraded, fresh) = (Connection::open_in_memory().unwrap(), Connection::open_in_memory().unwrap());
    upgraded.execute_batch(SESSION_V181_UPGRADED).unwrap();
    fresh.execute_batch(SESSION_V181).unwrap();
    for path in [&[SESSION_V180, SESSION_V181][..], &[SESSION_V1793, SESSION_V181], &[SESSION_V1793, SESSION_V180, SESSION_V181]] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(path[0]).unwrap();
        for next in &path[1..] { upgrade(&conn, next); }
        let actual = schema(&conn).unwrap();
        assert_eq!(actual, schema(&upgraded).unwrap());
        assert_ne!(actual, schema(&fresh).unwrap());
        assert_eq!(detect(&conn, Store::Sessions, Some(CHANNEL_SETTINGS_SCHEMA)).unwrap(), Detected { version: SchemaVersion::V181, extension: false });
    }
    assert!(take_schema_refusal().is_none());
}

#[test]
fn import_accepts_v185_sessions_without_importing_tool_references() {
    for catalog in [SESSION_V185, SESSION_V185_UPGRADED, SESSION_V185_FROM_V181, SESSION_V185_FROM_V181_UPGRADED] {
        assert_eq!(detected(catalog, Store::Sessions, None), Detected { version: SchemaVersion::V181, extension: false });
        let file = store(catalog, "INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now');
            INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','legacy question','now');
            INSERT INTO session_tool_refs(session_id,session_generation,created_at,tool_name,ref_kind,ref_id,label)
            VALUES ('s','now',0,'docs_create','google_doc','SECRET-ID','SECRET-LABEL');");
        let mut out = Vec::new();
        assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Ok(1));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("legacy question") && !text.contains("SECRET"), "{text}");
    }
}

#[test]
fn import_accepts_v182_sessions_without_reading_origin_proofs() {
    // v1.82 = either v1.81 shape plus the gateway-owned proof table; both map
    // as V181 and the reader never emits a proof record.
    for (catalog, base) in [(SESSION_V182, SESSION_V181), (SESSION_V182_UPGRADED, SESSION_V181_UPGRADED)] {
        assert_eq!(detected(catalog, Store::Sessions, None), Detected { version: SchemaVersion::V181, extension: false });
        assert_eq!(detected(catalog, Store::Sessions, Some(CHANNEL_SETTINGS_SCHEMA)), Detected { version: SchemaVersion::V181, extension: true });
        // Opening a v1.81 store with this release adds exactly that table.
        let (upgraded, expected) = (Connection::open_in_memory().unwrap(), Connection::open_in_memory().unwrap());
        upgraded.execute_batch(base).unwrap();
        upgrade(&upgraded, SESSION_ORIGIN_PROOFS_V182);
        expected.execute_batch(catalog).unwrap();
        assert_eq!(schema(&upgraded).unwrap(), schema(&expected).unwrap());
        let file = store(catalog, "INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now');
            INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','legacy question','now');
            INSERT INTO schedule_origin_proofs VALUES ('SECRET-ID','sales','SECRET-RECIPIENT','SECRET-USER','googlechat','SECRET-SENDER','s','now',0);");
        let mut out = Vec::new();
        assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Ok(1));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("legacy question") && !text.contains("SECRET"), "{text}");
        assert!(take_schema_refusal().is_none());
    }
}

#[test]
fn rm173_import_refuses_v181_sessions_with_planted_objects() {
    for sql in ["CREATE INDEX planted_idx ON sessions(user_cleared_at);", "CREATE INDEX planted_idx ON session_migrations(applied_at);",
        "CREATE TABLE planted_markers(name TEXT);", "CREATE VIEW planted AS SELECT * FROM session_migrations;",
        "CREATE TRIGGER planted AFTER INSERT ON session_migrations BEGIN DELETE FROM session_messages; END;",
        "ALTER TABLE session_migrations ADD COLUMN planted TEXT;", "ALTER TABLE sessions ADD COLUMN planted TEXT;"] {
        let file = store(SESSION_V181, &format!("{sql}
            INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now');
            INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','NEVER-EMITTED','now');"));
        let mut out = Vec::new();
        assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Err(Error::Schema), "{sql}");
        assert!(out.is_empty());
        let refusal = take_schema_refusal().expect("refusal report");
        assert_eq!((refusal.store, refusal.closest, refusal.closest_extension), (Store::Sessions, SchemaVersion::V181, false), "{sql}");
        assert_eq!(refusal.differences.len(), 1, "{sql}: {:?}", refusal.differences);
        let message = refusal.to_string();
        assert!(message.contains("known catalogs (v1.79.3, v1.80.x, v1.81, v1.82, v1.85)") && message.contains("closest (v1.81)"), "{message}");
    }
    let file = store(SESSION_V181, "CREATE INDEX planted_idx ON sessions(user_cleared_at);");
    assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, Vec::new()), Err(Error::Schema));
    assert_eq!(take_schema_refusal().expect("refusal report").differences,
        vec![Difference::Unexpected { object: "index".into(), name: "planted_idx".into() }]);
    // Half-upgraded: v1.80.x plus only one RM-173 column (and no marker table) is neither catalog.
    let file = store(SESSION_V180, "ALTER TABLE sessions ADD COLUMN user_cleared_at TEXT;");
    assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, Vec::new()), Err(Error::Schema));
    let _ = take_schema_refusal();
}

#[test]
fn import_refuses_tasks_store_with_extra_unknown_table() {
    for schema in [TASKS_V180, TASKS_V181, TASKS_V182, TASKS_V186] {
        let file = store(schema, "CREATE TABLE planted_markers(name TEXT);");
        assert_eq!(extract(&request(SourceKind::AuthorityTasksSqlite, "tasks.db"), source(&file), None).err(), Some(Error::Schema));
        let message = take_schema_refusal().expect("refusal report").to_string();
        assert!(message.contains("known catalogs (v1.79.3, v1.80.x, v1.81, v1.82, v1.86)"), "{message}");
        assert!(message.contains("unexpected table planted_markers"), "{message}");
    }
}

#[test]
fn sr5r3_import_accepts_v181_approvals_and_never_reads_incarnation() {
    // Every known approvals catalog is accepted exactly; v1.81 maps as V180.
    assert_eq!(detected(APPROVALS_V1793, Store::Approvals, None).version, SchemaVersion::V1793);
    assert_eq!(detected(APPROVALS_V180, Store::Approvals, None).version, SchemaVersion::V180);
    assert_eq!(detected(APPROVALS_V181, Store::Approvals, None).version, SchemaVersion::V180);
    assert!(take_schema_refusal().is_none());
    let rows = "INSERT INTO approvals(id,agent_id,action_kind,summary,payload,status,created_at,employee_incarnation)
        VALUES ('a','sales','SECRET-KIND','SECRET-SUMMARY','{}','approved','now',7);";
    let file = store(APPROVALS_V181, rows);
    let p = extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].category, C::LegacyApproval);
    assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
}

#[test]
fn sr5r3_import_refuses_approvals_with_an_extra_column() {
    for schema in [APPROVALS_V180, APPROVALS_V181, APPROVALS_V186] {
        let file = store(schema, "ALTER TABLE approvals ADD COLUMN planted_column TEXT;");
        assert_eq!(extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).err(), Some(Error::Schema));
        let message = take_schema_refusal().expect("refusal report").to_string();
        assert!(message.contains("known catalogs (v1.79.3, v1.80.x, v1.81, v1.86)"), "{message}");
        assert!(message.contains("changed table approvals"), "{message}");
    }
    // Half-upgraded: v1.80.x plus the incarnation column with a different type.
    let file = store(APPROVALS_V180, "ALTER TABLE approvals ADD COLUMN employee_incarnation TEXT;");
    assert_eq!(extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).err(), Some(Error::Schema));
    let _ = take_schema_refusal();
}

#[test]
fn import_refuses_unknown_schema_with_diff() {
    // Neither version: a v1.80 cron store missing one scoped column and its index.
    let file = store(CRON_V180, "DROP INDEX idx_cron_scoped_actor; ALTER TABLE cron_tasks DROP COLUMN scoped_by_user_id;");
    let error = extract(&request(SourceKind::AuthorityCronSqlite, "cron_tasks"), source(&file), None).err().unwrap();
    assert_eq!(error, Error::Schema);
    assert_eq!(error.to_string(), "unsupported legacy schema");
    let refusal = take_schema_refusal().expect("refusal report");
    assert_eq!((refusal.store, refusal.closest, refusal.closest_extension, refusal.omitted), (Store::Cron, SchemaVersion::V180, false, 0));
    assert_eq!(refusal.differences.len(), 2);
    assert!(refusal.differences.contains(&Difference::Missing { object: "index".into(), name: "idx_cron_scoped_actor".into() }));
    assert!(refusal.differences.contains(&Difference::Changed { object: "table".into(), name: "cron_tasks".into(), missing_columns: vec!["scoped_by_user_id".into()] }));
    let message = refusal.to_string();
    for expected in ["cron_tasks.db", "known catalogs (v1.79.3, v1.80.x, v1.82)", "closest (v1.80.x)",
        "missing index idx_cron_scoped_actor", "changed table cron_tasks (missing column scoped_by_user_id)"] {
        assert!(message.contains(expected), "{message}");
    }
    assert!(take_schema_refusal().is_none(), "taking the report clears it");

    // Half-upgraded tasks store plus planted objects: closest is v1.79.3. The
    // report names objects but never their SQL, and hides non-identifier names.
    let file = store(TASKS_V1793, "ALTER TABLE tasks ADD COLUMN a2a_owner TEXT;
        CREATE TABLE \"SECRET-CANARY\"(token TEXT);
        CREATE TRIGGER planted AFTER INSERT ON tasks BEGIN SELECT 'SECRET-BODY'; END;");
    assert_eq!(extract(&request(SourceKind::AuthorityTasksSqlite, "tasks.db"), source(&file), None).err(), Some(Error::Schema));
    let message = take_schema_refusal().expect("refusal report").to_string();
    for expected in ["tasks.db", "closest (v1.79.3)", "changed table tasks", "unexpected trigger planted", "unexpected table <unprintable name>"] {
        assert!(message.contains(expected), "{message}");
    }
    assert!(!message.contains("SECRET"), "{message}");

    // Content importer: a v1.80 sessions store with a planted index emits nothing.
    let file = store(SESSION_V180, "CREATE INDEX planted_idx ON sessions(title);
        INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now');
        INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','NEVER-EMITTED','now');");
    let mut out = Vec::new();
    assert_eq!(import(&request(SourceKind::SessionsSqlite, "sessions.db"), source(&file), None, &mut out), Err(Error::Schema));
    assert!(out.is_empty());
    let refusal = take_schema_refusal().expect("refusal report");
    assert_eq!((refusal.store, refusal.closest), (Store::Sessions, SchemaVersion::V180));
    assert_eq!(refusal.differences, vec![Difference::Unexpected { object: "index".into(), name: "planted_idx".into() }]);
    assert!(refusal.to_string().contains("sessions.db"));
}

#[test]
fn import_accepts_v182_and_v186_tasks_without_importing_origin_proofs() {
    for schema in [TASKS_V182, TASKS_V186] {
    assert_eq!(detected(schema, Store::Tasks, None).version, SchemaVersion::V180);
    assert!(take_schema_refusal().is_none());
    let file = store(schema, "INSERT INTO tasks(id,title,description,assigned_to,created_at,updated_at,kind,goal_mode,source_channel,source_chat_id,source_origin_proof)
        VALUES ('goal','Quarterly goal','Do work','sales','now','now','goal',1,'googlechat','spaces/SECRET-CHAT','SECRET-PROOF');");
    let p = extract(&request(SourceKind::AuthorityTasksSqlite, "tasks.db"), source(&file), None).unwrap();
    assert_eq!(p.len(), 1);
    let encoded = serde_json::to_string(&p).unwrap();
    assert!(encoded.contains("Quarterly goal") && !encoded.contains("SECRET-PROOF"));
    }
}

#[test]
fn import_accepts_v182_cron_without_importing_delivery_authority() {
    assert_eq!(detected(CRON_V182, Store::Cron, None).version, SchemaVersion::V180);
    let file = store(CRON_V182, "INSERT INTO cron_tasks(id,name,agent_id,cron,task,created_at,updated_at,notify_sender,notify_origin,last_delivery_status,last_delivery_reason,notify_origin_proof)
        VALUES('c','Daily','sales','0 9 * * *','report','then','then','SECRET-SENDER','googlechat:SECRET-ORIGIN','SECRET-STATUS','SECRET-REASON','SECRET-PROOF');");
    let proposals = extract(&request(SourceKind::AuthorityCronSqlite, "cron_tasks"), source(&file), None).unwrap();
    assert_eq!(proposals.len(), 1);
    assert!(!serde_json::to_string(&proposals).unwrap().contains("SECRET"));
}

#[test]
fn googlechat_recovery_catalogs_match_initializer_and_refuse_unknown_objects() {
    for sql in [GOOGLECHAT_INBOX_V182, GOOGLECHAT_INBOX_V183_V1, GOOGLECHAT_INBOX_V183] {
        let conn = catalog_connection(sql, None).unwrap();
        assert!(detect(&conn, Store::GoogleChatInbox, None).is_ok());
        conn.execute_batch("CREATE TABLE unknown_state (id TEXT)").unwrap();
        assert!(detect(&conn, Store::GoogleChatInbox, None).is_err());
    }
    let source = include_str!("../../duduclaw-gateway/src/googlechat_inbox.rs").replace("\r\n", "\n");
    assert!(source.contains(&GOOGLECHAT_INBOX_V182.replace("\r\n", "\n")));
    assert!(source.contains(&include_str!("googlechat-placeholders-v183.sql").replace("\r\n", "\n")));
    assert!(source.contains(include_str!("googlechat-placeholder-state-v183.sql").replace("\r\n", "\n").trim()));
    let legacy = catalog_connection(GOOGLECHAT_INBOX_V183_V1, None).unwrap();
    legacy.execute_batch(include_str!("googlechat-placeholder-state-v183.sql")).unwrap();
    assert_eq!(detect(&legacy, Store::GoogleChatInbox, None).unwrap().version, SchemaVersion::V181);
}

#[test]
fn v186_import_approvals_discards_source_execution_result() {
    assert_eq!(detected(APPROVALS_V186_EXECUTION_RESULT, Store::Approvals, None).version, SchemaVersion::V180);
    let file = store(APPROVALS_V186_EXECUTION_RESULT,
        "INSERT INTO approvals(id,agent_id,action_kind,summary,status,created_at,execution_result)
        VALUES ('SOURCE-APPROVAL','sales','admin_ai_settings','old','approved','now',
            '{\"outcome\":\"complete\",\"message\":\"SOURCE-EXECUTION-RESULT\"}');");
    let proposals = extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].category, C::LegacyApproval);
    assert_eq!(proposals[0].fields.len(), 1);
    assert_eq!(proposals[0].fields[0].name, "reference");
    let encoded = serde_json::to_string(&proposals).unwrap();
    for forbidden in ["SOURCE-APPROVAL", "execution_result", "SOURCE-EXECUTION-RESULT"] {
        assert!(!encoded.contains(forbidden));
    }
    for extra in ["ALTER TABLE approvals ADD COLUMN planted TEXT;",
        "CREATE TRIGGER planted AFTER INSERT ON approvals BEGIN SELECT 1; END;"] {
        let file = store(APPROVALS_V186_EXECUTION_RESULT, extra);
        assert_eq!(extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).err(), Some(Error::Schema));
    }
}

#[test]
fn rm143_import_v186_approvals_discards_chat_admin_evidence() {
    assert_eq!(detected(APPROVALS_V186, Store::Approvals, None).version, SchemaVersion::V180);
    let rows = "INSERT INTO approvals(id,agent_id,action_kind,summary,status,created_at,decided_by)
        VALUES ('SECRET-APPROVAL','sales','expert_skills_enable','old','approved','now',
            'chat-admin:3f2afc12-4f13-4a28-8b7f-df31eadbb013:telegram:totp');
        INSERT INTO chat_admin_cards VALUES ('SECRET-CARD','SECRET-APPROVAL','rm143','SECRET-USER',
            'telegram','SECRET-BOT','SECRET-PRINCIPAL','SECRET-DM','42','SECRET-DIGEST','T2','later','consumed','now');
        INSERT INTO chat_admin_decisions VALUES ('SECRET-APPROVAL','SECRET-CARD','SECRET-USER',
            'telegram','totp',1,'SECRET-DIGEST',1,'now');";
    let file = store(APPROVALS_V186, rows);
    let proposals = extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].category, C::LegacyApproval);
    assert_eq!(proposals[0].fields.len(), 1);
    assert_eq!(proposals[0].fields[0].name, "reference");
    let encoded = serde_json::to_string(&proposals).unwrap();
    for forbidden in ["SECRET", "chat-admin:", "chat_admin_cards", "chat_admin_decisions"] {
        assert!(!encoded.contains(forbidden));
    }
    // Unknown additions to the evidence tables still reject the entire source.
    for extra in ["ALTER TABLE chat_admin_cards ADD COLUMN planted TEXT;",
        "CREATE TRIGGER planted AFTER INSERT ON chat_admin_decisions BEGIN SELECT 1; END;"] {
        let file = store(APPROVALS_V186, extra);
        assert_eq!(extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).err(), Some(Error::Schema));
    }
}

#[test]
fn rm143_import_users_requires_reentry_and_refuses_unpinned_proofs() {
    let rows = "INSERT INTO users(id,email,display_name,password_hash,role,created_at,updated_at)
        VALUES ('u','admin@example.com','Admin','SECRET-PASSWORD','admin','now','now');
        INSERT INTO channel_identities(user_id,channel,channel_user_id,verified,created_at)
        VALUES ('u','telegram','SECRET-PRINCIPAL',1,'now');";
    let file = store(include_str!("users-v1793.sql"), rows);
    let proposals = extract(&request(SourceKind::AuthorityUsersSqlite, "users.db"), source(&file), None).unwrap();
    let binding = proposals.iter().find(|p| p.category == C::ChannelBinding).unwrap();
    assert_eq!(value(binding, "requires_reentry"), "true");
    assert_eq!(value(binding, "binding"), r#"{"owner":"admin@example.com","requires_new_login":true}"#);
    let encoded = serde_json::to_string(&proposals).unwrap();
    for forbidden in ["SECRET", "proof_method", "proof_user_id", "proven_at", "proof_account", "proof_dm", "chat_link_"] {
        assert!(!encoded.contains(forbidden));
    }
    // Users is intentionally still an exact v1.79.3-only reader. Even one
    // RM-143 column or table refuses the whole source, never a partial copy.
    let chat_link = include_str!("../../duduclaw-auth/src/chat_link.rs");
    let ddl = chat_link.split("conn.execute_batch(\"").nth(1).unwrap().split("\")").next().unwrap();
    for extra in ["ALTER TABLE channel_identities ADD COLUMN proof_method TEXT;",
        "ALTER TABLE channel_identities ADD COLUMN proof_user_id TEXT;",
        "ALTER TABLE channel_identities ADD COLUMN proven_at INTEGER;",
        "ALTER TABLE channel_identities ADD COLUMN proof_account TEXT;",
        "ALTER TABLE channel_identities ADD COLUMN proof_dm TEXT;", ddl] {
        let file = store(include_str!("users-v1793.sql"), &format!("{rows} {extra}"));
        assert_eq!(extract(&request(SourceKind::AuthorityUsersSqlite, "users.db"), source(&file), None).err(), Some(Error::Schema));
    }
}
