//! SELECT-only source for restricted readers. Scope precedes list limits;
//! private titles, hidden turns, tool payloads and summaries are not loaded.
use super::{Class, classify};
use crate::session::SessionMessage;
use crate::session_origin::{OriginKind, SessionOrigin};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::Path;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 20_000;
const MAX_SESSIONS: usize = 10_000;
pub struct Metadata {
    pub id: String,
    pub agent: String,
    pub class: Class,
    pub title: Option<String>,
    pub last_active: String,
}
pub struct StoredRow {
    pub id: i64,
    pub message: SessionMessage,
}
/// Admission is acquired before spawn_blocking and held by the closure.
/// Abandoned requests cannot grow an unbounded SQLite worker queue.
pub fn permit() -> Result<tokio::sync::OwnedSemaphorePermit, String> {
    static READERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    READERS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "history busy".into())
}
pub fn open(home: &Path) -> Result<Connection, String> {
    let c = Connection::open_with_flags(
        home.join("sessions.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "history unavailable")?;
    c.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| "history unavailable")?;
    Ok(c)
}
fn time(raw: &str) -> Result<String, String> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&chrono::Utc).to_rfc3339())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
                .map(|t| t.and_utc().to_rfc3339())
        })
        .map_err(|_| "history unavailable".into())
}
fn meta(row: &rusqlite::Row<'_>, user: &str) -> rusqlite::Result<Metadata> {
    let id: String = row.get(0)?;
    let kind: Option<String> = row.get(2)?;
    let owner: Option<String> = row.get(3)?;
    let origin = kind.map(|k| match (OriginKind::parse(&k), owner) {
        (OriginKind::WebchatUser, Some(owner)) => SessionOrigin::webchat_user(&owner),
        (kind, _) => SessionOrigin::of_kind(kind),
    });
    Ok(Metadata {
        id: id.clone(),
        agent: row.get(1)?,
        class: classify(&id, origin.as_ref(), user),
        title: row.get(4)?,
        last_active: row.get(5)?,
    })
}
pub fn metadata(c: &Connection, user: &str, id: &str) -> Result<Option<Metadata>, String> {
    let m=c.query_row("SELECT id,agent_id,origin_kind,owner_user_id,CASE WHEN origin_kind='webchat_user' AND owner_user_id=?2 THEN title ELSE NULL END,last_active FROM sessions WHERE id=?1 AND archived_at IS NULL",params![id,user],|r|meta(r,user)).optional().map_err(|_|"history unavailable")?;
    m.map(|mut m| {
        if m.class != Class::Hidden {
            m.last_active = time(&m.last_active)?;
        }
        Ok(m)
    })
    .transpose()
}
pub fn list(
    c: &Connection,
    user: &str,
    agent: &str,
    limit: usize,
) -> Result<Vec<Metadata>, String> {
    if !(1..=MAX_SESSIONS).contains(&limit) {
        return Err("invalid limit".into());
    }
    let mut s=c.prepare("SELECT id,agent_id,origin_kind,owner_user_id,CASE WHEN origin_kind='webchat_user' AND owner_user_id=?2 THEN title ELSE NULL END,last_active FROM sessions WHERE agent_id=?1 AND archived_at IS NULL AND ((origin_kind='webchat_user' AND owner_user_id=?2) OR origin_kind IN ('channel','webchat_visitor','public_visitor')) ORDER BY last_active DESC,id DESC LIMIT ?3").map_err(|_|"history unavailable")?;
    let entries = s
        .query_map(params![agent, user, (MAX_SESSIONS + 1) as i64], |r| {
            meta(r, user)
        })
        .map_err(|_| "history unavailable")?;
    let mut out = Vec::new();
    for (i, m) in entries.enumerate() {
        if i == MAX_SESSIONS {
            return Err("history unavailable".into());
        }
        let mut m = m.map_err(|_| "history unavailable")?;
        if m.class == Class::Hidden {
            continue;
        }
        m.last_active = time(&m.last_active)?;
        out.push(m);
        if out.len() == limit {
            break;
        }
    }
    Ok(out)
}
pub fn rows(c: &Connection, id: &str) -> Result<Vec<StoredRow>, String> {
    let mut s=c.prepare("SELECT id,role,CASE WHEN length(CAST(content AS BLOB))<=?2 THEN content ELSE NULL END,tokens,timestamp FROM session_messages WHERE session_id=?1 AND hidden=0 AND undone_at IS NULL AND role IN ('user','assistant') ORDER BY id LIMIT ?3").map_err(|_|"history unavailable")?;
    let entries = s
        .query_map(params![id, MAX_BYTES as i64, (MAX_ROWS + 1) as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|_| "history unavailable")?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for (i, row) in entries.enumerate() {
        if i == MAX_ROWS {
            return Err("history unavailable".into());
        }
        let (id, role, body, tokens, stamp) = row.map_err(|_| "history unavailable")?;
        let content = body.ok_or("history unavailable")?;
        bytes = bytes.saturating_add(content.len());
        if bytes > MAX_BYTES {
            return Err("history unavailable".into());
        }
        out.push(StoredRow {
            id,
            message: SessionMessage {
                role,
                content,
                tokens,
                timestamp: time(&stamp)?,
            },
        });
    }
    Ok(out)
}
#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
