//! Durable content bindings. JSONL remains a bounded, best-effort audit view.
use super::{ArtifactRecord, ARTIFACTS_FILE};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub(super) const DB_FILE: &str = "artifact_provenance.db";
const MIGRATION_MAX_BYTES: u64 = 32 * 1024 * 1024;
const ROW_MAX_BYTES: u64 = 64 * 1024;

pub(super) fn open(home: &Path) -> Result<Connection, String> {
    let mut connection = crate::fs_safe::sqlite::open(home, DB_FILE)
        .map_err(|_| "artifact authority unavailable")?;
    connection.busy_timeout(std::time::Duration::from_secs(1)).map_err(|_| "artifact authority unavailable")?;
    connection.execute_batch("PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS artifact_provenance (
            agent TEXT NOT NULL, name TEXT NOT NULL, source TEXT, outbound INTEGER NOT NULL,
            record TEXT NOT NULL, PRIMARY KEY(agent,name));
        CREATE INDEX IF NOT EXISTS artifact_provenance_source ON artifact_provenance(agent,source,outbound);
        CREATE TABLE IF NOT EXISTS artifact_provenance_meta (key TEXT PRIMARY KEY);")
        .map_err(|_| "artifact authority unavailable")?;
    let migrated: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM artifact_provenance_meta WHERE key='jsonl_imported')", [], |r| r.get(0),
    ).map_err(|_| "artifact authority unavailable")?;
    if !migrated {
        duduclaw_core::with_file_lock(&home.join(ARTIFACTS_FILE), || {
            migrate(&mut connection, home).map_err(std::io::Error::other)
        }).map_err(|_| "artifact authority migration unavailable")?;
    }
    Ok(connection)
}

fn put(connection: &Connection, record: &ArtifactRecord, replace: bool) -> Result<(), String> {
    let body = serde_json::to_string(&record.to_json()).map_err(|_| "artifact authority serialization failed")?;
    if body.len() as u64 > ROW_MAX_BYTES { return Err("artifact authority row too large".into()); }
    connection.execute("INSERT INTO artifact_provenance(agent,name,source,outbound,record) VALUES (?1,?2,?3,?4,?5)
        ON CONFLICT(agent,name) DO UPDATE SET source=excluded.source,outbound=excluded.outbound,record=excluded.record WHERE ?6",
        params![record.agent_id, record.archived_name, record.source_path,
            matches!(record.origin, super::ArtifactOrigin::Declared | super::ArtifactOrigin::Swept), body, replace])
        .map_err(|_| "artifact authority write failed")?;
    Ok(())
}

fn migrate(connection: &mut Connection, home: &Path) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Read};
    let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "artifact authority migration unavailable")?;
    let migrated: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM artifact_provenance_meta WHERE key='jsonl_imported')", [], |r| r.get(0),
    ).map_err(|_| "artifact authority migration unavailable")?;
    if !migrated {
        // Oldest first, with the same later-row-wins semantics as the old index.
        let mut total = 0;
        for name in ["artifacts.jsonl.1", ARTIFACTS_FILE] {
            let file = match std::fs::File::open(home.join(name)) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("artifact authority migration unreadable".into()),
            };
            let metadata = file.metadata().map_err(|_| "artifact authority migration unreadable")?;
            if !metadata.is_file() || metadata.len() > MIGRATION_MAX_BYTES.saturating_sub(total) {
                return Err("artifact authority migration limit exceeded".into());
            }
            let mut reader = BufReader::new(file);
            loop {
                let mut bytes = Vec::new();
                let count = reader.by_ref().take(ROW_MAX_BYTES + 1).read_until(b'\n', &mut bytes)
                    .map_err(|_| "artifact authority migration unreadable")?;
                if count == 0 { break; }
                total += count as u64;
                if count as u64 > ROW_MAX_BYTES || total > MIGRATION_MAX_BYTES {
                    return Err("artifact authority migration limit exceeded".into());
                }
                let line = std::str::from_utf8(&bytes).map_err(|_| "artifact authority migration malformed")?;
                if let Some(record) = ArtifactRecord::from_line(line.trim()) {
                    put(&transaction, &record, true)?;
                } else if !line.trim().is_empty() {
                    return Err("artifact authority migration malformed".into());
                }
            }
        }
        transaction.execute("INSERT INTO artifact_provenance_meta(key) VALUES ('jsonl_imported')", [])
            .map_err(|_| "artifact authority migration unavailable")?;
    }
    transaction.commit().map_err(|_| "artifact authority migration unavailable".into())
}

pub(super) fn write(home: &Path, rows: &[serde_json::Value], replace: bool) -> Result<(), String> {
    let mut connection = open(home)?;
    let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "artifact authority write failed")?;
    for row in rows {
        let record = ArtifactRecord::from_line(&row.to_string()).ok_or("artifact authority invalid row")?;
        put(&transaction, &record, replace)?;
    }
    transaction.commit().map_err(|_| "artifact authority write failed".into())
}

pub(super) fn get(connection: &Connection, agent: &str, name: &str) -> Result<Option<ArtifactRecord>, String> {
    let body: Option<String> = connection.query_row(
        "SELECT record FROM artifact_provenance WHERE agent=?1 AND name=?2",
        params![agent, name], |r| r.get(0),
    ).optional().map_err(|_| "artifact authority read failed")?;
    body.map(|body| ArtifactRecord::from_line(&body).ok_or_else(|| "artifact authority row malformed".into())).transpose()
}

pub(super) fn source(connection: &Connection, agent: &str, source: &Path) -> Option<String> {
    let mut statement = connection.prepare(
        "SELECT name FROM artifact_provenance WHERE agent=?1 AND source=?2 AND outbound=1 LIMIT 2",
    ).ok()?;
    let mut rows = statement.query(params![agent, source.to_str()?]).ok()?;
    let first: String = rows.next().ok()??.get(0).ok()?;
    if rows.next().ok()?.is_some() || !crate::files_api::is_safe_filename(&first) { return None; }
    Some(first)
}

#[cfg(all(test, unix))]
#[path = "authority/tests.rs"]
mod tests;
