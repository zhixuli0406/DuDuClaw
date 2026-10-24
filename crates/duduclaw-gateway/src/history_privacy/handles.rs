//! Opaque routing names, not access grants. Every consumer must still apply
//! live authorization and stored-origin classification after resolution.
//! Deterministic HMAC names keep export manifests stable. The cache and
//! bounded read-only fallback require no new database policy or alias rows.
use hmac::{Hmac, Mac};
use rusqlite::{Connection, OpenFlags};
use sha2::Sha256;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
const PREFIX: &str = "view:v1:";
#[derive(Clone, Copy)]
enum Kind {
    Session,
    Run,
    Cursor,
}
impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Self::Session => "s",
            Self::Run => "r",
            Self::Cursor => "c",
        }
    }
    fn max_raw(self) -> usize {
        match self {
            Self::Session => MAX_RAW,
            Self::Run => MAX_RAW + 32,
            Self::Cursor => 4096,
        }
    }
}
fn prefix(kind: Kind) -> String {
    format!("{PREFIX}{}:", kind.tag())
}
const MAX_RAW: usize = 1024;
const MAX_CACHE: usize = 20_000;
const MAX_LOOKUP: usize = 100_000;
const TTL: Duration = Duration::from_secs(1800);
struct Entry {
    raw: String,
    issued: Instant,
}
type Cache = HashMap<(PathBuf, String, String), Entry>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(Default::default)
}
#[cfg(test)]
pub fn is_handle(value: &str) -> bool {
    value
        .strip_prefix(prefix(Kind::Session).as_str())
        .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn with_key(key: &[u8; 32], kind: &str, raw: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("fixed HMAC key");
    mac.update(kind.as_bytes());
    mac.update(&[0]);
    mac.update(raw.as_bytes());
    format!(
        "{PREFIX}{kind}:{}",
        hex::encode(mac.finalize().into_bytes())
    )
}
fn remember(home: &Path, user: &str, handle: &str, raw: &str) {
    if let Ok(mut c) = cache().lock() {
        c.retain(|_, e| e.issued.elapsed() < TTL);
        let k = (home.into(), user.into(), handle.into());
        if c.len() < MAX_CACHE || c.contains_key(&k) {
            c.insert(
                k,
                Entry {
                    raw: raw.into(),
                    issued: Instant::now(),
                },
            );
        }
    }
}
pub fn issue(home: &Path, user: &str, raw: &str) -> Result<String, String> {
    issue_kind(home, user, raw, Kind::Session)
}
pub fn issue_run(home: &Path, user: &str, raw: &str) -> Result<String, String> {
    issue_kind(home, user, raw, Kind::Run)
}
pub fn issue_cursor(home: &Path, user: &str, raw: &str) -> Result<String, String> {
    issue_kind(home, user, raw, Kind::Cursor)
}
/// Non-routing references for search keys and CSV message identifiers.
pub fn reference(home: &Path, user: &str, raw: &str) -> Result<String, String> {
    if raw.len() > 4096 || user.is_empty() {
        return Err("history unavailable".into());
    }
    let key = crate::trigger_route::access::private_view_key(home, user)?;
    Ok(with_key(&key, "ref", raw))
}
fn issue_kind(home: &Path, user: &str, raw: &str, kind: Kind) -> Result<String, String> {
    if user.is_empty()
        || raw.is_empty()
        || raw.len() > kind.max_raw()
        || raw.chars().any(char::is_control)
    {
        return Err("history unavailable".into());
    }
    let key = crate::trigger_route::access::private_view_key(home, user)?;
    let value = with_key(&key, kind.tag(), raw);
    remember(home, user, &value, raw);
    Ok(value)
}
pub async fn resolve(home: &Path, user: &str, value: &str) -> Result<String, String> {
    resolve_kind(home, user, value, Kind::Session).await
}
pub async fn resolve_run(home: &Path, user: &str, value: &str) -> Result<String, String> {
    resolve_kind(home, user, value, Kind::Run).await
}
pub async fn resolve_cursor(home: &Path, user: &str, value: &str) -> Result<String, String> {
    resolve_kind(home, user, value, Kind::Cursor).await
}
async fn resolve_kind(home: &Path, user: &str, value: &str, kind: Kind) -> Result<String, String> {
    if !value.starts_with(PREFIX) {
        return if !value.is_empty()
            && value.len() <= kind.max_raw()
            && !value.chars().any(char::is_control)
        {
            Ok(value.into())
        } else {
            Err("session not found".into())
        };
    }
    if !value
        .strip_prefix(prefix(kind).as_str())
        .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("session not found".into());
    }
    let key = crate::trigger_route::access::private_view_key(home, user)?;
    if let Some(raw) = cache()
        .lock()
        .map_err(|_| "history unavailable")?
        .get(&(home.into(), user.into(), value.into()))
        .filter(|e| e.issued.elapsed() < TTL)
        .map(|e| e.raw.clone())
    {
        return Ok(raw);
    }
    if matches!(kind, Kind::Cursor) {
        return Err("cursor expired".into());
    }
    static READERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = READERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "history unavailable")?;
    let home = home.to_path_buf();
    let user = user.to_string();
    let value = value.to_string();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let conn = Connection::open_with_flags(
            home.join("sessions.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "session not found")?;
        let sql=match kind {
            Kind::Cursor=>return Err("cursor expired".into()),
            Kind::Session=>"SELECT id FROM sessions WHERE archived_at IS NULL ORDER BY rowid DESC LIMIT ?1",
            Kind::Run=>"SELECT s.id || '#' || CAST(m.id AS TEXT) FROM session_messages m JOIN sessions s ON s.id=m.session_id WHERE s.archived_at IS NULL AND m.hidden=0 AND m.undone_at IS NULL AND m.role='user' ORDER BY m.id DESC LIMIT ?1",
        };
        let mut stmt = conn.prepare(sql).map_err(|_| "history unavailable")?;
        let rows = stmt
            .query_map([(MAX_LOOKUP + 1) as i64], |r| r.get::<_, String>(0))
            .map_err(|_| "history unavailable")?;
        for (i, row) in rows.enumerate() {
            if i == MAX_LOOKUP {
                break;
            }
            let raw = row.map_err(|_| "history unavailable")?;
            if raw.len() <= kind.max_raw() && with_key(&key,kind.tag(),&raw) == value {
                remember(&home, &user, &value, &raw);
                return Ok(raw);
            }
        }
        Err("session not found".into())
    })
    .await
    .map_err(|_| "history unavailable".to_string())?
}
