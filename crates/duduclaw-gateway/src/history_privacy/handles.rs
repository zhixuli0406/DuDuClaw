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
    Attachment,
}
impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Self::Session => "s",
            Self::Run => "r",
            Self::Cursor => "c",
            Self::Attachment => "a",
        }
    }
    fn max_raw(self) -> usize {
        match self {
            Self::Session => MAX_RAW,
            Self::Run => MAX_RAW + 32,
            Self::Cursor | Self::Attachment => 4096,
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
    accessed: Instant,
}
type Cache = HashMap<(PathBuf, String, String), Entry>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static ATTACHMENTS: OnceLock<Mutex<Cache>> = OnceLock::new();
const ATTACHMENTS_PER_USER: usize = 512;
fn attachment_cache() -> &'static Mutex<Cache> { ATTACHMENTS.get_or_init(Default::default) }
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
                    accessed: Instant::now(),
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
/// Attachment routes are cached server-side and never accept raw client paths.
pub fn issue_attachment(home: &Path, user: &str, raw: &str) -> Result<String, String> {
    issue_kind(home, user, raw, Kind::Attachment)
}
pub async fn resolve_attachment(home: &Path, user: &str, value: &str) -> Result<String, String> {
    if !value.strip_prefix(prefix(Kind::Attachment).as_str())
        .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("attachment unavailable".into());
    }
    resolve_kind(home, user, value, Kind::Attachment).await
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
    if matches!(kind, Kind::Attachment) {
        let mut c = attachment_cache().lock().map_err(|_| "attachment unavailable")?;
        remember_attachment(&mut c, home, user, &value, raw)?;
    } else {
        remember(home, user, &value, raw);
    }
    Ok(value)
}
fn remember_attachment(c: &mut Cache, home: &Path, user: &str, value: &str, raw: &str) -> Result<(), String> {
    c.retain(|_, e| e.issued.elapsed() < TTL);
    let key = (home.to_path_buf(), user.to_owned(), value.to_owned());
    if !c.contains_key(&key) {
        let own: Vec<_> = c.iter().filter(|((h,u,_),_)| h == home && u == user)
            .map(|(k,e)| (k.clone(), e.accessed)).collect();
        if own.len() >= ATTACHMENTS_PER_USER {
            if let Some((oldest, _)) = own.into_iter().min_by_key(|(_, t)| *t) { c.remove(&oldest); }
        }
        if c.len() >= MAX_CACHE { return Err("attachment capacity reached".into()); }
    }
    c.insert(key, Entry { raw: raw.into(), issued: Instant::now(), accessed: Instant::now() });
    Ok(())
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
    let selected_cache = if matches!(kind, Kind::Attachment) { attachment_cache() } else { cache() };
    if let Some(raw) = selected_cache
        .lock()
        .map_err(|_| "history unavailable")?
        .get_mut(&(home.into(), user.into(), value.into()))
        .filter(|e| e.issued.elapsed() < TTL)
        .map(|e| { e.accessed = Instant::now(); e.raw.clone() })
    {
        return Ok(raw);
    }
    if matches!(kind, Kind::Cursor | Kind::Attachment) {
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
            Kind::Cursor | Kind::Attachment=>return Err("cursor expired".into()),
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

#[cfg(test)]
mod attachment_cache_tests {
    use super::*;
    #[test]
    fn chat_attachment_cache_per_user_lru_and_capacity_never_issue_dead_handles() {
        let mut c = Cache::new();
        let home = Path::new("/cache-test");
        remember_attachment(&mut c, home, "bob", "b", "bob-reference").unwrap();
        for i in 0..ATTACHMENTS_PER_USER + 5 {
            let id = format!("a-{i}");
            remember_attachment(&mut c, home, "alice", &id, "alice-reference").unwrap();
            assert!(c.contains_key(&(home.into(), "alice".into(), id)));
            if i == ATTACHMENTS_PER_USER - 1 {
                c.get_mut(&(home.into(), "alice".into(), "a-0".into())).unwrap().accessed = Instant::now();
            }
        }
        assert!(c.contains_key(&(home.into(), "bob".into(), "b".into())));
        assert_eq!(c.len(), ATTACHMENTS_PER_USER + 1);
        assert!(c.contains_key(&(home.into(), "alice".into(), "a-0".into())));
        assert!(!c.contains_key(&(home.into(), "alice".into(), "a-1".into())));
        while c.len() < MAX_CACHE {
            let user = format!("other-{}", c.len());
            c.insert((home.into(), user, "id".into()), Entry { raw: "r".into(), issued: Instant::now(), accessed: Instant::now() });
        }
        assert!(remember_attachment(&mut c, home, "carol", "new", "ref").is_err());
        assert!(!c.contains_key(&(home.into(), "carol".into(), "new".into())));
        assert!(remember_attachment(&mut c, home, "bob", "b", "updated").is_ok());
    }
}
