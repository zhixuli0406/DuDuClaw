//! Gateway-owned employee nonces. Never derive authority from filesystem times.
use std::{io, path::Path};
use rusqlite::{Connection, OptionalExtension, params};
use crate::private_sqlite::{self, Access};

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS google_employee_incarnations (
        agent_id TEXT PRIMARY KEY NOT NULL, nonce TEXT NOT NULL);")
}

pub fn read(conn: &Connection, agent: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT nonce FROM google_employee_incarnations WHERE agent_id=?1",
        [agent], |r| r.get(0)).optional()
}

/// Only employee creation or an authorized first attestation may call this.
pub fn ensure(conn: &Connection, agent: &str) -> rusqlite::Result<String> {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    conn.execute("INSERT OR IGNORE INTO google_employee_incarnations(agent_id,nonce) VALUES (?1,?2)",
        params![agent, nonce])?;
    read(conn, agent)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub fn retire(conn: &Connection, agent: &str) -> rusqlite::Result<()> {
    migrate(conn)?;
    conn.execute("DELETE FROM google_employee_incarnations WHERE agent_id=?1", [agent])?;
    Ok(())
}

/// Rotate when publishing a new directory, including import-as-new. Failed
/// publication must never restore an old grant.
pub fn created(home: &Path, agent: &str) -> io::Result<()> {
    let mut conn = private_sqlite::open(home, "users.db", Access::Create)
        .map_err(io::Error::other)?.ok_or_else(|| io::Error::other("users.db unavailable"))?;
    let tx = conn.transaction().map_err(io::Error::other)?;
    retire(&tx, agent).map_err(io::Error::other)?;
    ensure(&tx, agent).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)
}

pub fn deleted(home: &Path, agent: &str) -> io::Result<()> {
    if let Some(conn) = private_sqlite::open(home, "users.db", Access::ReadWrite).map_err(io::Error::other)? {
        retire(&conn, agent).map_err(io::Error::other)?;
    }
    Ok(())
}

/// Scan persisted labels, including ones missing before gateway restart.
/// An unreadable directory fails the scan; it never creates a replacement nonce.
pub fn reconcile(home: &Path) -> io::Result<()> {
    let Some(mut conn) = private_sqlite::open(home, "users.db", Access::ReadWrite).map_err(io::Error::other)? else { return Ok(()); };
    migrate(&conn).map_err(io::Error::other)?;
    let tx = conn.transaction().map_err(io::Error::other)?;
    let labels = {
        let mut stmt = tx.prepare("SELECT agent_id FROM google_employee_incarnations").map_err(io::Error::other)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(io::Error::other)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(io::Error::other)?
    };
    for agent in labels {
        if agent.is_empty() || agent.contains(['/', '\\']) || agent == "." || agent == ".." {
            retire(&tx, &agent).map_err(io::Error::other)?;
            continue;
        }
        match std::fs::symlink_metadata(home.join("agents").join(&agent)) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {},
            Ok(_) => retire(&tx, &agent).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => retire(&tx, &agent).map_err(io::Error::other)?,
            Err(e) => return Err(e),
        }
    }
    tx.commit().map_err(io::Error::other)
}
