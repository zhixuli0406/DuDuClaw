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
    let conn = Connection::open(file.path()).unwrap();
    conn.authorizer(Some(|c: rusqlite::hooks::AuthContext<'_>| match c.action {
        rusqlite::hooks::AuthAction::Read { table_name: "task_store_markers", .. } => rusqlite::hooks::Authorization::Deny,
        _ => rusqlite::hooks::Authorization::Allow,
    }));
    assert!(conn.prepare("SELECT name FROM task_store_markers").is_err());
}

#[test]
fn import_refuses_tasks_store_with_extra_unknown_table() {
    for schema in [TASKS_V180, TASKS_V181] {
        let file = store(schema, "CREATE TABLE planted_markers(name TEXT);");
        assert_eq!(extract(&request(SourceKind::AuthorityTasksSqlite, "tasks.db"), source(&file), None).err(), Some(Error::Schema));
        let message = take_schema_refusal().expect("refusal report").to_string();
        assert!(message.contains("known catalogs (v1.79.3, v1.80.x, v1.81)"), "{message}");
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
    for schema in [APPROVALS_V180, APPROVALS_V181] {
        let file = store(schema, "ALTER TABLE approvals ADD COLUMN planted_column TEXT;");
        assert_eq!(extract(&request(SourceKind::AuthorityApprovalsSqlite, "approvals.db"), source(&file), None).err(), Some(Error::Schema));
        let message = take_schema_refusal().expect("refusal report").to_string();
        assert!(message.contains("known catalogs (v1.79.3, v1.80.x, v1.81)"), "{message}");
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
    for expected in ["cron_tasks.db", "known catalogs (v1.79.3, v1.80.x)", "closest (v1.80.x)",
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
