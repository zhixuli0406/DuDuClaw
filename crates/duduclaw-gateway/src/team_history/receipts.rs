//! Bounded self-only audit projections. Cursors never contain file offsets,
//! identities or raw details; they are short-lived process-local capabilities.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const SCAN_BYTES: usize = 4 * 1024 * 1024;
const SCAN_LINES: usize = 10_000;
const MAX_LINE: usize = 256 * 1024;
const MAX_CURSORS: usize = 4096;
const CURSOR_TTL: Duration = Duration::from_secs(600);

pub struct Receipt {
    pub viewer: String,
    pub agent: String,
    pub timestamp: String,
}
pub struct Page {
    pub views: Vec<Receipt>,
    pub next_before: Option<String>,
}
#[derive(Clone)]
struct Cursor {
    home: PathBuf,
    user: String,
    end: u64,
    offset: u64,
    anchor: [u8; 32],
    issued: Instant,
}
static CURSORS: OnceLock<Mutex<HashMap<String, Cursor>>> = OnceLock::new();
fn cursors() -> &'static Mutex<HashMap<String, Cursor>> {
    CURSORS.get_or_init(Default::default)
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn hash_hex(bytes: &[u8]) -> String {
    hash(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
fn clean_line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}
fn anchor(file: &mut File, end: u64) -> Result<[u8; 32], String> {
    let size = end.min(256) as usize;
    file.seek(SeekFrom::Start(end - size as u64))
        .map_err(|_| "read_error")?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes).map_err(|_| "stale_cursor")?;
    Ok(hash(&bytes))
}
fn remember(mut cursor: Cursor, offset: u64) -> Result<String, String> {
    let mut all = cursors().lock().map_err(|_| "read_error")?;
    all.retain(|_, c| c.issued.elapsed() < CURSOR_TTL);
    if all.len() >= MAX_CURSORS {
        return Err("rate_limited".into());
    }
    cursor.offset = offset;
    cursor.issued = Instant::now();
    let id = uuid::Uuid::new_v4().to_string();
    all.insert(id.clone(), cursor);
    Ok(id)
}
/// A missing, truncated, replaced or expired snapshot is never silently
/// restarted from a different position. Refresh starts a new snapshot.
pub fn read(home: &Path, user: &str, before: Option<&str>, limit: usize) -> Result<Page, String> {
    read_with_budget(home, user, before, limit, SCAN_BYTES)
}
fn read_with_budget(
    home: &Path,
    user: &str,
    before: Option<&str>,
    limit: usize,
    budget: usize,
) -> Result<Page, String> {
    if !(1..=100).contains(&limit) {
        return Err("invalid_params".into());
    }
    let cursor = if let Some(id) = before {
        let all = cursors().lock().map_err(|_| "read_error")?;
        Some(
            all.get(id)
                .filter(|c| c.user == user && c.home == home && c.issued.elapsed() < CURSOR_TTL)
                .cloned()
                .ok_or("stale_cursor")?,
        )
    } else {
        None
    };
    let path = home.join("security_audit.jsonl");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && cursor.is_none() => {
            return Ok(Page {
                views: vec![],
                next_before: None,
            });
        }
        Ok(m) if m.file_type().is_file() => {}
        _ => {
            return Err(if cursor.is_some() {
                "stale_cursor"
            } else {
                "read_error"
            }
            .into());
        }
    }
    // Nonblocking shared locks match both writer lanes. A busy writer is an
    // explicit retry, never an unbounded blocking-task queue.
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(PathBuf::from(lock_name))
        .map_err(|_| "read_error")?;
    fs2::FileExt::try_lock_shared(&lock).map_err(|_| "read_error")?;
    let mut file = File::open(&path).map_err(|_| "read_error")?;
    #[cfg(unix)]
    fs2::FileExt::try_lock_shared(&file).map_err(|_| "read_error")?;
    read_locked(&mut file, home, user, cursor, limit, budget)
}
fn read_locked(
    file: &mut File,
    home: &Path,
    user: &str,
    cursor: Option<Cursor>,
    limit: usize,
    budget: usize,
) -> Result<Page, String> {
    let len = file.metadata().map_err(|_| "read_error")?.len();
    let snapshot = if let Some(c) = cursor {
        if len < c.end || anchor(file, c.end)? != c.anchor {
            return Err("stale_cursor".into());
        }
        c
    } else {
        Cursor {
            home: home.into(),
            user: user.into(),
            end: len,
            offset: len,
            anchor: anchor(file, len)?,
            issued: Instant::now(),
        }
    };
    if snapshot.offset == 0 {
        return Ok(Page {
            views: vec![],
            next_before: None,
        });
    }
    let mut start = snapshot.offset.saturating_sub(budget as u64);
    let boundary = if start > 0 {
        file.seek(SeekFrom::Start(start - 1))
            .map_err(|_| "read_error")?;
        let mut byte = [0];
        file.read_exact(&mut byte).map_err(|_| "read_error")?;
        byte[0] == b'\n'
    } else {
        true
    };
    file.seek(SeekFrom::Start(start))
        .map_err(|_| "read_error")?;
    let mut bytes = vec![0; (snapshot.offset - start) as usize];
    file.read_exact(&mut bytes).map_err(|_| "read_error")?;
    if bytes.last() != Some(&b'\n') {
        return Err("read_error".into());
    }
    let skip = if !boundary {
        bytes.iter().position(|b| *b == b'\n').ok_or("read_error")? + 1
    } else {
        0
    };
    start += skip as u64;
    let bytes = &bytes[skip..];
    if bytes.is_empty() {
        return Err("read_error".into());
    }
    let mut lines = bytes[..bytes.len() - 1].rsplit(|b| *b == b'\n').peekable();
    let mut offset = snapshot.offset;
    let mut views = Vec::new();
    for _ in 0..SCAN_LINES {
        let Some(raw) = lines.next() else { break };
        offset -= raw.len() as u64 + 1;
        if raw.len() > MAX_LINE {
            return Err("read_error".into());
        }
        let row: Value = serde_json::from_slice(clean_line(raw)).map_err(|_| "read_error")?;
        if !row.is_object() {
            return Err("read_error".into());
        }
        if let Some(expected) = row.get("_prev_hash") {
            let expected = expected
                .as_str()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("read_error")?;
            if let Some(previous) = lines.peek() {
                if expected != hash_hex(clean_line(previous)) {
                    return Err("read_error".into());
                }
            } else if offset == 0 && expected != "0".repeat(64) {
                return Err("read_error".into());
            }
        }
        if let Some(receipt) = project(&row, user)? {
            views.push(receipt);
        }
        if views.len() == limit {
            break;
        }
    }
    debug_assert!(offset >= start);
    let next_before = if offset > 0 {
        Some(remember(snapshot, offset)?)
    } else {
        None
    };
    Ok(Page { views, next_before })
}
fn project(row: &Value, user: &str) -> Result<Option<Receipt>, String> {
    // Neither arbitrary event names nor arbitrary class strings grant access.
    if row.get("event_type").and_then(Value::as_str) != Some("team_history_viewed") {
        return Ok(None);
    }
    let d = row
        .get("details")
        .and_then(Value::as_object)
        .ok_or("read_error")?;
    let class = d.get("class").and_then(Value::as_str).ok_or("read_error")?;
    if class != "report_webchat" {
        return Ok(None);
    }
    let report = d
        .get("report")
        .and_then(Value::as_str)
        .ok_or("read_error")?;
    if report != user {
        return Ok(None);
    }
    // B5 reads have always used the mandatory chained writer. A malformed
    // known record must not turn into an apparently empty receipt page.
    if row.get("_prev_hash").and_then(Value::as_str).is_none() {
        return Err("read_error".into());
    }
    let text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or_else(|| "read_error".to_string())
    };
    let viewer = text(d.get("viewer"))?;
    let agent = text(d.get("agent"))?;
    if viewer == user
        || !matches!(
            d.get("viewer_role").and_then(Value::as_str),
            Some("manager" | "admin")
        )
        || row.get("agent_id").and_then(Value::as_str) != Some(&agent)
        || !duduclaw_core::is_valid_agent_id(&agent)
    {
        return Err("read_error".into());
    }
    let timestamp = text(row.get("timestamp"))?;
    chrono::DateTime::parse_from_rfc3339(&timestamp).map_err(|_| "read_error")?;
    Ok(Some(Receipt {
        viewer,
        agent,
        timestamp,
    }))
}
#[cfg(test)]
#[path = "receipts_tests.rs"]
mod tests;
