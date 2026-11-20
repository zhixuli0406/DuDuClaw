//! Read-only schema inventory. This is a diagnostic, never migration authority.
use std::path::Path;
use rusqlite::{Connection, OpenFlags, hooks::{AuthAction, AuthContext, Authorization}};
use serde::Serialize;
use sha2::{Digest, Sha256};
use crate::{catalog::{self, Store}, expected, sqlite, Error, Result};

const MAX_REPORT_BYTES: usize = 128 * 1024;
const MAX_DIFFERENCES: usize = 16;
use crate::expected::STORES;

#[derive(Debug, Serialize)]
pub struct Report {
    pub format_version: u16,
    pub source_version: &'static str,
    pub migration_target: &'static str,
    pub creating_version: String,
    /// Never becomes true from a code-derived candidate or a closest match.
    pub release_matrix_verified: bool,
    pub stores: Vec<StoreReport>,
    pub pending_captures: Vec<String>,
    pub private_copy: &'static str,
}
#[derive(Debug, Serialize)]
pub struct StoreReport {
    pub path: &'static str,
    pub status: &'static str,
    pub user_version: Option<i64>,
    pub application_id: Option<i64>,
    /// SHA-256 of serde_json's compact ordered schema-row tuples; no SQL emitted.
    pub schema_sha256: Option<String>,
    pub catalog: Option<String>,
    pub exact_catalog: bool,
    pub expected_candidate: Option<&'static str>,
    pub differences: Vec<String>,
    pub omitted_differences: usize,
}
impl StoreReport {
    fn absent(store: Store) -> Self {
        Self { path: store.file_name(), status: "absent", user_version: None, application_id: None,
            schema_sha256: None, catalog: None, exact_catalog: false, expected_candidate: None,
            differences: Vec::new(), omitted_differences: 0 }
    }
}
impl Report {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|_| Error::Content)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_REPORT_BYTES { return Err(Error::Limit); }
        Ok(bytes)
    }
    pub fn has_refusals(&self) -> bool {
        self.stores.iter().any(|s| !matches!(s.status, "absent" | "code_candidate"))
    }
}

/// Inventory only fixed migration-store paths. No config, identities, rows,
/// host path, timestamp, file size or SQL text is included in the report.
/// The caller must stop all home writers first; nonempty WAL/journals refuse.
#[cfg(unix)]
pub fn capture(home: &Path, creating_version: &str) -> Result<Report> {
    capture_with_hook(home, creating_version, |_| {})
}

#[cfg(unix)]
fn capture_with_hook(home: &Path, creating_version: &str, after_read: impl Fn(Store)) -> Result<Report> {
    capture_with_bound(home, creating_version, crate::MAX_SOURCE_BYTES, after_read)
}

#[cfg(unix)]
fn capture_with_bound(home: &Path, creating_version: &str, max_bytes: u64, after_read: impl Fn(Store)) -> Result<Report> {
    use std::{fs::File, path::Component};
    use rustix::fs::{open, openat, Mode, OFlags};
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    if home.components().any(|c| !matches!(c, Component::RootDir | Component::CurDir | Component::Normal(_))) {
        return Err(Error::InvalidRequest);
    }
    // Normalize trailing separators/dots so NOFOLLOW applies to home itself.
    // Ancestors are operator-selected (e.g. macOS /var -> /private/var);
    // only home and store/sidecar components below it must not be symlinks.
    let normalized: std::path::PathBuf = home.components().collect();
    let home = if normalized.as_os_str().is_empty() { Path::new(".") } else { normalized.as_path() };
    let home_metadata = std::fs::symlink_metadata(home)?;
    if !home_metadata.is_dir() { return Err(Error::Io); }
    let directory = open(home, directory_flags, Mode::empty()).map_err(|_| Error::Io)?;
    if !expected::CREATING_VERSIONS.contains(&creating_version) { return Err(Error::InvalidRequest); }
    let mut report = Report { format_version: 1, source_version: expected::SOURCE,
        migration_target: expected::TARGET, creating_version: creating_version.to_owned(),
        release_matrix_verified: false, stores: Vec::new(), pending_captures: Vec::new(), private_copy: COPY_MECHANISM };
    // Each creating-version/store pair is deliberately unresolved until a
    // signed-release VM capture has been reviewed, even if code SQL matches.
    report.pending_captures = expected::release_captures().into_iter().map(|c| c.id).collect();
    for set in expected::SETS.iter().filter(|s| s.sql.is_none()) { report.pending_captures.push(set.id.to_owned()); }
    report.pending_captures.sort();
    for store in STORES {
        let mut entry = StoreReport::absent(store);
        let flags = source_flags();
        let fd = match openat(&directory, store.file_name(), flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => { report.stores.push(entry); continue; },
            Err(error) => { entry.status = open_refusal(error); report.stores.push(entry); continue; },
        };
        let file = File::from(fd);
        let metadata = file.metadata()?;
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() {
            entry.status = "refused_file_or_limit"; report.stores.push(entry); continue;
        }
        if metadata.len() > max_bytes {
            entry.status = "refused_too_large"; report.stores.push(entry); continue;
        }
        if metadata.nlink() != 1 {
            entry.status = "refused_hardlink"; report.stores.push(entry); continue;
        }
        let before = match snapshot(home, &directory, store) {
            Ok(state) if state.store == identity(&metadata) => state,
            Ok(_) => { entry.status = "refused_sqlite_or_changed"; report.stores.push(entry); continue; },
            Err(status) => { entry.status = status; report.stores.push(entry); continue; },
        };
        // Only the validated descriptor is read; SQLite never opens a source path.
        let result = (|| -> Result<StoreReport> {
            let conn = read_image(&file, metadata.len(), max_bytes, store)?;
            after_read(store);
            if snapshot(home, &directory, store).ok().as_ref() != Some(&before)
                || identity(&file.metadata()?) != before.store { return Err(Error::Io); }
            let found = inspect(&conn, store)?;
            conn.close().map_err(|(_, error)| error)?;
            if snapshot(home, &directory, store).ok().as_ref() != Some(&before)
                || identity(&file.metadata()?) != before.store { return Err(Error::Io); }
            Ok(found)
        })();
        entry = match result {
            Ok(found) => found,
            Err(Error::Limit) => { entry.status = "refused_limit"; entry },
            Err(_) => { entry.status = "refused_sqlite_or_changed"; entry },
        };
        report.stores.push(entry);
    }
    report.stores.sort_by_key(|s| s.path);
    Ok(report)
}
#[cfg(not(unix))]
pub fn capture(_home: &Path, _creating_version: &str) -> Result<Report> { Err(Error::Isolation) }

const COPY_MECHANISM: &str = "in_memory";

#[cfg(unix)]
fn source_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    #[cfg(target_os = "linux")]
    let flags = flags | OFlags::NOATIME;
    flags
}
#[cfg(unix)]
fn open_refusal(error: rustix::io::Errno) -> &'static str {
    #[cfg(target_os = "linux")]
    if error == rustix::io::Errno::PERM { return "refused_noatime"; }
    let _ = error;
    "refused_open"
}
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Identity { dev: u64, ino: u64, size: u64, mtime: i64, mtime_ns: i64, ctime: i64, ctime_ns: i64, nlink: u64 }
#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> Identity {
    use std::os::unix::fs::MetadataExt;
    Identity { dev: meta.dev(), ino: meta.ino(), size: meta.len(), mtime: meta.mtime(),
        mtime_ns: meta.mtime_nsec(), ctime: meta.ctime(), ctime_ns: meta.ctime_nsec(), nlink: meta.nlink() }
}
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Snapshot { parent: (u64, u64), store: Identity, sidecars: Vec<Option<Identity>> }
#[cfg(unix)]
fn snapshot(home: &Path, directory: &std::os::fd::OwnedFd, store: Store) -> std::result::Result<Snapshot, &'static str> {
    use std::{fs::File, os::unix::fs::MetadataExt};
    use rustix::fs::{open, openat, Mode, OFlags};
    let parent = File::from(open(home, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty()).map_err(|_| "refused_sqlite_or_changed")?).metadata().map_err(|_| "refused_sqlite_or_changed")?;
    let held = File::from(directory.try_clone().map_err(|_| "refused_sqlite_or_changed")?)
        .metadata().map_err(|_| "refused_sqlite_or_changed")?;
    if (parent.dev(), parent.ino()) != (held.dev(), held.ino()) { return Err("refused_sqlite_or_changed"); }
    let read_meta = |name: &str| -> std::result::Result<Option<std::fs::Metadata>, &'static str> {
        match openat(directory, name, source_flags(), Mode::empty()) {
            Ok(fd) => Ok(Some(File::from(fd).metadata().map_err(|_| "refused_sidecar")?)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(if open_refusal(error) == "refused_noatime" { "refused_noatime" } else { "refused_sidecar" }),
        }
    };
    let main = read_meta(store.file_name())?.ok_or("refused_sqlite_or_changed")?;
    let mut sidecars = Vec::new();
    for suffix in ["-wal", "-shm", "-journal"] {
        let meta = read_meta(&format!("{}{suffix}", store.file_name()))?;
        if let Some(meta) = &meta {
            if meta.nlink() != 1 { return Err("refused_hardlink"); }
            if !meta.is_file() || meta.len() > crate::MAX_SOURCE_BYTES
                || (suffix != "-shm" && meta.len() != 0) { return Err("refused_sidecar"); }
        }
        sidecars.push(meta.as_ref().map(identity));
    }
    Ok(Snapshot { parent: (parent.dev(), parent.ino()), store: identity(&main), sidecars })
}

/// Load exactly one bounded image into SQLite-owned memory. No VFS file is opened.
fn read_image(mut input: impl std::io::Read, expected_bytes: u64, max_bytes: u64, store: Store) -> Result<Connection> {
    use std::io::Read;
    if expected_bytes > max_bytes || expected_bytes > crate::MAX_SOURCE_BYTES { return Err(Error::Limit); }
    let size = usize::try_from(expected_bytes).map_err(|_| Error::Limit)?;
    let mut conn = Connection::open_in_memory_with_flags(
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.execute_batch("PRAGMA temp_store=MEMORY;")?;
    sqlite::harden_capture(&mut conn)?;
    // The reviewed task/workflow schemas require this exception to the depth-32 limit.
    if matches!(store, Store::Tasks | Store::Workflow) {
        conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH, 128)?;
    }
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let reopening = Arc::new(AtomicBool::new(false));
    let allow_reopen = Arc::clone(&reopening);
    conn.authorizer(Some(move |c: AuthContext<'_>| {
        if c.accessor.is_some() { return Authorization::Deny; }
        match c.action {
            // sqlite3_deserialize prepares this fixed internal ATTACH before
            // reopening main with memdb. Permit it once, only during that API call.
            AuthAction::Attach { filename: "x" } if allow_reopen.swap(false, Ordering::SeqCst) => Authorization::Allow,
            AuthAction::Select => Authorization::Allow,
            AuthAction::Pragma { pragma_name: "user_version" | "application_id", pragma_value: None } => Authorization::Allow,
            AuthAction::Read { table_name: "sqlite_schema" | "sqlite_master", column_name: "type" | "name" | "tbl_name" | "sql" }
                if c.database_name == Some("main") => Authorization::Allow,
            AuthAction::Function { function_name }
                if ["date", "time", "datetime", "julianday", "unixepoch", "strftime", "timediff"].iter().any(|n| function_name.eq_ignore_ascii_case(n)) => Authorization::Allow,
            _ => Authorization::Deny,
        }
    }))?;
    // A checkpointed WAL-mode header cannot be deserialized as-is (SQLITE_CANTOPEN).
    // Normalize only the in-memory journal-version bytes, never the source. The
    // caller has already refused nonempty WAL/journals and checks identities again.
    let mut reader = ImageReader { input: &mut input, offset: 0 };
    reopening.store(true, Ordering::SeqCst);
    let loaded = conn.deserialize_read_exact("main", &mut reader, size, true);
    reopening.store(false, Ordering::SeqCst);
    loaded?;
    let mut extra = [0];
    if reader.read(&mut extra)? != 0 { return Err(Error::Io); }
    Ok(conn)
}

struct ImageReader<R> { input: R, offset: u64 }
impl<R: std::io::Read> std::io::Read for ImageReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(buffer)?;
        for (index, byte) in buffer.iter_mut().take(count).take(20u64.saturating_sub(self.offset) as usize).enumerate() {
            if matches!(self.offset + index as u64, 18 | 19) && *byte == 2 { *byte = 1; }
        }
        self.offset += count as u64;
        Ok(count)
    }
}

fn inspect(conn: &Connection, store: Store) -> Result<StoreReport> {
    let actual = sqlite::schema(conn)?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let bytes = serde_json::to_vec(&actual).map_err(|_| Error::Content)?;
    let mut result = StoreReport::absent(store);
    result.status = "unrecognised";
    result.user_version = Some(user_version);
    result.application_id = Some(application_id);
    result.schema_sha256 = Some(format!("{:x}", Sha256::digest(bytes)));
    let mut best: Option<(String, Vec<String>)> = None;
    // Catalog IDs are stable store + release label + ordinal + extension.
    // The ordinal distinguishes exact fresh/upgraded SQL with the same mapping.
    for (index, catalog) in catalog::catalogs(store).iter().enumerate() {
        for extension in [false, true] {
            if extension && store != Store::Sessions { continue; }
            let expected = Connection::open_in_memory()?;
            expected.execute_batch("PRAGMA temp_store=MEMORY;")?;
            expected.execute_batch(catalog.sql)?;
            if extension { expected.execute_batch(sqlite::CHANNEL_SETTINGS_SCHEMA)?; }
            let expected_rows = sqlite::schema(&expected)?;
            let mut differences = catalog::differences(&actual, &expected_rows, &expected)?.iter().map(ToString::to_string).collect::<Vec<_>>();
            let expected_user: i64 = expected.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if user_version != expected_user { differences.push("changed user_version".into()); }
            if application_id != 0 { differences.push("changed application_id".into()); }
            let id = format!("{}:{}:{index}{}", store.file_name(), catalog.label, if extension { ":channel_settings" } else { "" });
            if differences.is_empty() {
                result.catalog = Some(id); result.exact_catalog = true;
                break;
            }
            if best.as_ref().is_none_or(|(_, d)| differences.len() < d.len()) { best = Some((id, differences)); }
        }
        if result.exact_catalog { break; }
    }
    if !result.exact_catalog {
        if let Some((id, mut differences)) = best {
            result.catalog = Some(id);
            result.omitted_differences = differences.len().saturating_sub(MAX_DIFFERENCES);
            differences.truncate(MAX_DIFFERENCES);
            result.differences = differences;
        }
    }
    for set in expected::SETS.iter().filter(|s| s.store == store) {
        if set.matches(conn)? { result.expected_candidate = Some(set.id); break; }
    }
    if result.expected_candidate.is_some() { result.status = "code_candidate"; }
    else if result.exact_catalog { result.status = "catalog_only_not_v182_expected"; }
    Ok(result)
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
