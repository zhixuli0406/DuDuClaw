use std::{fs::OpenOptions, io::{Read, Write}, path::Path, time::{Duration, Instant}};
use rusqlite::{Connection, OpenFlags, config::DbConfig, hooks::{AuthAction, AuthContext, Authorization}, limits::Limit};
use crate::{Emitter, Error, Result, MAX_SOURCE_BYTES, catalog::{self, Detected, SchemaVersion, Store}};
use duduclaw_isolation::migration::ImportedContent;

/// The channel_settings extension is identical in every known sessions.db version.
pub(crate) const CHANNEL_SETTINGS_SCHEMA: &str = include_str!("channel-settings-v1793.sql");
const MAX_SCHEMA_ROWS: usize = 256;
const MAX_QUERY_SECONDS: u64 = 30;
pub(crate) type SchemaRow = (String, String, String, Option<String>);

pub(crate) fn copy_bounded(mut input: impl Read, path: &Path) -> Result<()> {
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    let count = std::io::copy(&mut input.by_ref().take(MAX_SOURCE_BYTES + 1), &mut output)?;
    if count > MAX_SOURCE_BYTES { return Err(Error::Limit); }
    output.sync_all()?;
    Ok(())
}

pub(crate) fn schema(conn: &Connection) -> Result<Vec<SchemaRow>> {
    let mut stmt = conn.prepare("SELECT type, name, tbl_name, sql FROM main.sqlite_schema ORDER BY type, name, tbl_name, sql")?;
    let mut rows = stmt.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        if result.len() >= MAX_SCHEMA_ROWS { return Err(Error::Limit); }
        result.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
    }
    Ok(result)
}

pub(crate) fn harden(conn: &mut Connection) -> Result<()> {
    // SAFETY: importer is a dedicated process; the SQLite process-wide ceiling
    // is only narrowed. -1 queries the current ceiling without changing it.
    unsafe {
        let old = rusqlite::ffi::sqlite3_hard_heap_limit64(-1);
        let limit = if old > 0 { old.min(64 * 1024 * 1024) } else { 64 * 1024 * 1024 };
        rusqlite::ffi::sqlite3_hard_heap_limit64(limit);
        let actual = rusqlite::ffi::sqlite3_hard_heap_limit64(-1);
        if actual <= 0 || actual > limit { return Err(Error::Isolation); }
    }
    for (config, value) in [
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
    ] {
        if conn.set_db_config(config, value)? != value { return Err(Error::Isolation); }
    }
    // SAFETY: live connection handle; SQLite's documented integer configuration.
    let rc = unsafe { rusqlite::ffi::sqlite3_db_config(conn.handle(),
        rusqlite::ffi::SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION, 0i32, std::ptr::null_mut::<i32>()) };
    if rc != rusqlite::ffi::SQLITE_OK { return Err(Error::Isolation); }
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 65536), (Limit::SQLITE_LIMIT_SQL_LENGTH, 65536),
        (Limit::SQLITE_LIMIT_COLUMN, 128), (Limit::SQLITE_LIMIT_EXPR_DEPTH, 32),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 4), (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_VDBE_OP, 100_000),
    ] { conn.set_limit(limit, value)?; }
    conn.execute_batch("PRAGMA cell_size_check=ON; PRAGMA mmap_size=0; PRAGMA query_only=ON;")?;
    let deadline = Instant::now() + Duration::from_secs(MAX_QUERY_SECONDS);
    conn.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    conn.authorizer(Some(|context: AuthContext<'_>| {
        if context.accessor.is_some() { return Authorization::Deny; }
        match context.action {
            AuthAction::Select => Authorization::Allow,
            AuthAction::Pragma { pragma_name: "quick_check" | "user_version" | "data_version", pragma_value: None } => Authorization::Allow,
            AuthAction::Read { table_name: "sqlite_master" | "sqlite_schema", column_name: "type" | "name" | "tbl_name" | "sql" }
                if context.database_name == Some("main") => Authorization::Allow,
            AuthAction::Read { table_name: "memories_fts_config" | "key_facts_fts_config", column_name: "k" | "v" }
                if context.database_name == Some("main") => Authorization::Allow,
            AuthAction::Read { table_name: "memories_fts_content" | "key_facts_fts_content", column_name: "" | "id" | "c0" | "c1" | "c2" }
            | AuthAction::Read { table_name: "memories_fts_data" | "key_facts_fts_data", column_name: "id" | "block" }
            | AuthAction::Read { table_name: "memories_fts_idx" | "key_facts_fts_idx", column_name: "segid" | "term" | "pgno" }
            | AuthAction::Read { table_name: "memories_fts_docsize" | "key_facts_fts_docsize", column_name: "" | "id" | "sz" }
                if context.database_name == Some("main") => Authorization::Allow,
            // SQLite quick_check invokes FTS5's built-in integrity path, which
            // counts its fixed shadow rows. No application query gets this grant.
            AuthAction::Function { function_name: "count" } => Authorization::Allow,
            // SQLite 3.54 also authorizes functions in schema DEFAULT expressions
            // (legacy `DEFAULT (datetime('now'))`). Only the deterministic-by-input
            // built-in date/time functions are granted; nothing else.
            AuthAction::Function { function_name }
                if ["date", "time", "datetime", "julianday", "unixepoch", "strftime", "timediff"]
                    .iter().any(|allowed| function_name.eq_ignore_ascii_case(allowed)) => Authorization::Allow,
            _ => Authorization::Deny,
        }
    }))?;
    Ok(())
}

/// Reviewed catalogs per known version: SessionManager alone, or the same
/// database with the channel settings initialized by ChannelReplyEngine. The
/// whole catalog must match one of them exactly. No arbitrary extra table.
pub(crate) fn session_schema_variant(conn:&Connection)->Result<Detected> {
    catalog::detect(conn, Store::Sessions, Some(CHANNEL_SETTINGS_SCHEMA))
}

/// Column mapping into conversation content, per source version. Both keep
/// hidden and undone rows out. v1.80 also stores assistant rows that were
/// never published (`delivery_pending=1`); v1.79.3 has no such column and
/// removed undelivered rows instead.
fn session_message_query(version: SchemaVersion) -> &'static str {
    match version {
        SchemaVersion::V1793 => "SELECT m.id, m.role, m.content, m.timestamp FROM main.session_messages AS m JOIN main.sessions AS s ON m.session_id = s.id WHERE s.agent_id = ?1 AND m.hidden = 0 AND m.undone_at IS NULL ORDER BY m.id",
        SchemaVersion::V180 => "SELECT m.id, m.role, m.content, m.timestamp FROM main.session_messages AS m JOIN main.sessions AS s ON m.session_id = s.id WHERE s.agent_id = ?1 AND m.hidden = 0 AND m.undone_at IS NULL AND m.delivery_pending = 0 ORDER BY m.id",
    }
}

pub(crate) fn sessions<R: Read, W: Write>(input: R, wal: Option<&mut dyn Read>, emitter: &mut Emitter<'_, W>) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let main = temp.path().join("source.db");
    copy_bounded(input, &main)?;
    if let Some(wal) = wal { copy_bounded(wal, &temp.path().join("source.db-wal"))?; }
    // Opening only the worker-private copy permits WAL recovery without touching legacy data.
    let mut conn = Connection::open_with_flags(&main, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    harden(&mut conn)?;
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 0 { return Err(Error::Schema); }
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" { return Err(Error::Content); }
    let version = session_schema_variant(&conn)?.version;
    conn.authorizer(Some(move |context: AuthContext<'_>| {
        if context.accessor.is_some() { return Authorization::Deny; }
        match context.action {
            AuthAction::Select => Authorization::Allow,
            AuthAction::Read { table_name: "sessions", column_name: "id" | "agent_id" }
            | AuthAction::Read { table_name: "session_messages", column_name: "id" | "session_id" | "role" | "content" | "timestamp" | "hidden" | "undone_at" }
                if context.database_name == Some("main") => Authorization::Allow,
            AuthAction::Read { table_name: "session_messages", column_name: "delivery_pending" }
                if version == SchemaVersion::V180 && context.database_name == Some("main") => Authorization::Allow,
            _ => Authorization::Deny,
        }
    }))?;
    let mut stmt = conn.prepare(session_message_query(version))?;
    let mut rows = stmt.query([emitter.request.legacy_agent.as_deref().ok_or(Error::InvalidRequest)?])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let role: String = row.get(1)?;
        if !matches!(role.as_str(), "user" | "assistant" | "system" | "tool") { return Err(Error::Content); }
        // Role and timestamps remain quoted historical content, never a live system message.
        let text = serde_json::to_string(&serde_json::json!({
            "role": role, "text": row.get::<_, String>(2)?, "timestamp": row.get::<_, String>(3)?
        })).map_err(|_| Error::Content)?;
        emitter.emit(&format!("message:{id}"), ImportedContent::Conversation { text, recorded_at_unix: None })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use crate::{import, ImportRequest, SourceKind};
    use duduclaw_isolation::ids::{EmployeeId, RequestId};
    fn request() -> ImportRequest {
        ImportRequest { version: 1, migration_id: RequestId([1; 16]), employee: Some(EmployeeId([2;16])),
            source_id: "sessions".into(), kind: SourceKind::SessionsSqlite, title: String::new(),
            legacy_agent: Some("sales".into()), has_wal: false, blob_handle: None }
    }
    fn db(extra: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let conn = Connection::open(file.path()).unwrap();
        conn.execute_batch(catalog::SESSION_V180).unwrap();
        conn.execute_batch("INSERT INTO sessions(id, agent_id, last_active, created_at) VALUES ('s','sales','now','now'); INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','assistant','hello','now');").unwrap();
        conn.execute_batch(extra).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        file
    }
    #[test]
    fn sessions_imports_only_visible_content() {
        let file = db("INSERT INTO session_messages(session_id,role,content,timestamp,hidden) VALUES ('s','user','forgotten','now',1)");
        let mut out = Vec::new();
        assert_eq!(import(&request(), File::open(file.path()).unwrap(), None, &mut out), Ok(1));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("hello"));
        assert!(!text.contains("forgotten"));
        assert!(!text.contains("pinned_instructions"));
    }
    #[test]
    fn sessions_rejects_planted_admin_mcp_trigger_view_and_column() {
        for sql in ["CREATE TABLE users(role TEXT); INSERT INTO users VALUES ('admin')",
            "CREATE TABLE mcp(command TEXT)", "CREATE VIEW evil AS SELECT * FROM sessions",
            "CREATE TRIGGER evil AFTER INSERT ON sessions BEGIN DELETE FROM session_messages; END",
            "ALTER TABLE sessions ADD COLUMN planted TEXT", "PRAGMA user_version=1"] {
            let file = db(sql);
            let mut out = Vec::new();
            assert!(import(&request(), File::open(file.path()).unwrap(), None, &mut out).is_err());
            assert!(out.is_empty());
        }
    }
    #[test]
    fn wal_replay_only_changes_private_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(catalog::SESSION_V1793).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now'); INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','user','in WAL','now');").unwrap();
        let wal_path = dir.path().join("legacy.db-wal");
        let before = (std::fs::read(&path).unwrap(), std::fs::read(&wal_path).unwrap());
        let mut req = request();
        req.has_wal = true;
        let mut wal = File::open(&wal_path).unwrap();
        let mut out = Vec::new();
        assert_eq!(import(&req, File::open(&path).unwrap(), Some(&mut wal), &mut out), Ok(1));
        assert!(String::from_utf8(out).unwrap().contains("in WAL"));
        assert_eq!(before, (std::fs::read(&path).unwrap(), std::fs::read(&wal_path).unwrap()));
    }
    #[test]
    fn corrupt_sqlite_fails_without_echoing_content() {
        let err = import(&request(), &b"secret-malicious-db"[..], None, Vec::new()).unwrap_err();
        assert!(!err.to_string().contains("secret"));
    }
}

/// Exact fresh memory engine schema of a known version (v1.79.3 or v1.80.x).
/// Other historical variants are refused.
pub(crate) fn memories<R: Read,W: Write>(input:R,wal:Option<&mut dyn Read>,emitter:&mut Emitter<'_,W>)->Result<()> {
    let temp=tempfile::tempdir()?;let main=temp.path().join("source.db");copy_bounded(input,&main)?;
    if let Some(wal)=wal {copy_bounded(wal,&temp.path().join("source.db-wal"))?;}
    let mut conn=Connection::open_with_flags(&main,OpenFlags::SQLITE_OPEN_READ_WRITE|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    harden(&mut conn)?;
    if conn.query_row("PRAGMA user_version",[],|r|r.get::<_,u32>(0))? != 0 {return Err(Error::Schema);}
    if conn.query_row("PRAGMA quick_check",[],|r|r.get::<_,String>(0))? != "ok" {return Err(Error::Content);}
    // Both versions share the column mapping below: v1.80 only adds an index.
    match catalog::detect(&conn,Store::Memory,None)?.version {SchemaVersion::V1793|SchemaVersion::V180=>{}}
    conn.authorizer(Some(|context:AuthContext<'_>| {
        if context.accessor.is_some() {return Authorization::Deny;}
        match context.action {
            AuthAction::Select=>Authorization::Allow,
            AuthAction::Read {table_name:"memories",column_name:"id"|"agent_id"|"content"|"valid_until"|"quarantined"}
            | AuthAction::Read {table_name:"key_facts",column_name:"id"|"agent_id"|"fact"}
                if context.database_name==Some("main")=>Authorization::Allow,
            _=>Authorization::Deny,
        }
    }))?;
    let agent=emitter.request.legacy_agent.as_deref().ok_or(Error::InvalidRequest)?.to_owned();
    // Tags, roles, trust, provenance, embeddings and executable metadata are never imported.
    for (query,label) in [("SELECT id,content FROM main.memories WHERE agent_id=?1 AND valid_until IS NULL AND quarantined=0 ORDER BY id","memory"),
        ("SELECT id,fact FROM main.key_facts WHERE agent_id=?1 ORDER BY id","fact")] {
        let mut statement=conn.prepare(query)?;let mut rows=statement.query([&agent])?;
        while let Some(row)=rows.next()? {
            let id:String=row.get(0)?;if id.is_empty() || id.len()>128 {return Err(Error::Content);}
            emitter.emit(&format!("{label}:{id}"),ImportedContent::Memory {text:row.get(1)?})?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::{import,ImportRequest,SourceKind};
    use duduclaw_isolation::ids::{RequestId,EmployeeId};
    fn request()->ImportRequest {ImportRequest {version:1,migration_id:RequestId([1;16]),employee:Some(EmployeeId([2;16])),source_id:"memory".into(),
        kind:SourceKind::MemorySqlite,title:String::new(),legacy_agent:Some("sales".into()),has_wal:false,blob_handle:None}}
    fn database(extra:&str)->tempfile::NamedTempFile {
        database_with(catalog::MEMORY_V1793,extra)
    }
    fn database_with(catalog_sql:&str,extra:&str)->tempfile::NamedTempFile {
        let file=tempfile::NamedTempFile::new().unwrap();let conn=Connection::open(file.path()).unwrap();
        conn.execute_batch(catalog_sql).unwrap();
        conn.execute_batch("INSERT INTO memories(id,agent_id,content,timestamp,origin_trust) VALUES ('m','sales','legacy text','now',1.0);
            INSERT INTO key_facts(id,agent_id,fact,timestamp) VALUES ('f','sales','legacy fact','now');
            INSERT INTO memories(id,agent_id,content,timestamp,quarantined) VALUES ('q','sales','quarantined','now',1);").unwrap();
        conn.execute_batch(extra).unwrap();file
    }
    #[test]
    fn memory_import_only_content_never_authority_or_old_trust() {
        for catalog_sql in [catalog::MEMORY_V1793,catalog::MEMORY_V180] {
            let file=database_with(catalog_sql,"");let mut out=Vec::new();
            assert_eq!(import(&request(),std::fs::File::open(file.path()).unwrap(),None,&mut out),Ok(2));
            let text=String::from_utf8(out).unwrap();assert!(text.contains("legacy text"));assert!(text.contains("legacy fact"));
            assert!(!text.contains("origin_trust"));assert!(!text.contains("quarantined"));
        }
    }
    #[test]
    fn memory_import_detects_each_version_exactly() {
        for (catalog_sql,version) in [(catalog::MEMORY_V1793,SchemaVersion::V1793),(catalog::MEMORY_V180,SchemaVersion::V180)] {
            let conn=Connection::open_in_memory().unwrap();conn.execute_batch(catalog_sql).unwrap();
            assert_eq!(catalog::detect(&conn,Store::Memory,None).unwrap().version,version);
        }
        // A v1.80 store with one planted index matches neither catalog and emits nothing.
        let file=database_with(catalog::MEMORY_V180,"CREATE INDEX planted_idx ON memories(content);");let mut out=Vec::new();
        assert_eq!(import(&request(),std::fs::File::open(file.path()).unwrap(),None,&mut out),Err(Error::Schema));
        assert!(out.is_empty());
        let refusal=catalog::take_schema_refusal().expect("refusal report");
        assert_eq!((refusal.store,refusal.closest),(Store::Memory,SchemaVersion::V180));
        assert_eq!(refusal.differences,vec![catalog::Difference::Unexpected{object:"index".into(),name:"planted_idx".into()}]);
    }
    #[test]
    fn memory_import_rejects_planted_table_view_and_trigger() {
        for sql in ["CREATE TABLE users(role TEXT)","CREATE VIEW evil AS SELECT * FROM memories",
            "CREATE TRIGGER evil AFTER INSERT ON memories BEGIN DELETE FROM key_facts; END"] {
            let file=database(sql);let mut out=Vec::new();
            let error=import(&request(),std::fs::File::open(file.path()).unwrap(),None,&mut out).unwrap_err();
            assert_eq!(error,Error::Schema);assert!(!error.to_string().contains(sql));assert!(out.is_empty());
        }
    }
}
