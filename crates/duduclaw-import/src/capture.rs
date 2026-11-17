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
    use std::{fs::File, io::Read, path::Component};
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
        release_matrix_verified: false, stores: Vec::new(), pending_captures: Vec::new() };
    // Each creating-version/store pair is deliberately unresolved until a
    // signed-release VM capture has been reviewed, even if code SQL matches.
    report.pending_captures = expected::release_captures().into_iter().map(|c| c.id).collect();
    for set in expected::SETS.iter().filter(|s| s.sql.is_none()) { report.pending_captures.push(set.id.to_owned()); }
    report.pending_captures.sort();
    for store in STORES {
        let mut entry = StoreReport::absent(store);
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = match openat(&directory, store.file_name(), flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => { report.stores.push(entry); continue; },
            Err(_) => { entry.status = "refused_open"; report.stores.push(entry); continue; },
        };
        let mut file = File::from(fd);
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > crate::MAX_SOURCE_BYTES {
            entry.status = "refused_file_or_limit"; report.stores.push(entry); continue;
        }
        let sidecars_clean = || -> Result<bool> {
            for suffix in ["-wal", "-shm", "-journal"] {
                let name = format!("{}{suffix}", store.file_name());
                match openat(&directory, name.as_str(), flags, Mode::empty()) {
                    Ok(fd) => {
                        let meta = File::from(fd).metadata()?;
                        // SHM is not read, but must be a bounded regular file.
                        if !meta.is_file() || meta.len() > crate::MAX_SOURCE_BYTES
                            || (suffix != "-shm" && meta.len() != 0) { return Ok(false); }
                    },
                    Err(rustix::io::Errno::NOENT) => {},
                    Err(_) => return Ok(false),
                }
            }
            Ok(true)
        };
        if !sidecars_clean()? {
            entry.status = "refused_sidecar"; report.stores.push(entry); continue;
        }
        // Source is only ever a read-only descriptor. SQLite opens an immutable
        // private copy, avoiding path re-open races and any home WAL/SHM writes.
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("source.db");
        let result = (|| -> Result<StoreReport> {
            sqlite::copy_bounded(file.by_ref(), &path)?;
            if !sidecars_clean()? { return Err(Error::Io); }
            use std::os::unix::fs::MetadataExt;
            let after = file.metadata()?;
            // Compare the same descriptor's identity and content timestamps at
            // full filesystem precision, excluding access/metadata-only changes.
            if metadata.dev() != after.dev() || metadata.ino() != after.ino()
                || metadata.len() != after.len() || metadata.mtime() != after.mtime()
                || metadata.mtime_nsec() != after.mtime_nsec() { return Err(Error::Io); }
            let mut conn = Connection::open_with_flags(immutable_uri(&path),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            sqlite::harden(&mut conn)?;
            // Match the existing authority readers: the task authority trigger
            // covers more than 32 fields; workflow also needs this fixed bound.
            if matches!(store, Store::Tasks | Store::Workflow) {
                conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH, 128)?;
            }
            conn.authorizer(Some(|c: AuthContext<'_>| {
                if c.accessor.is_some() { return Authorization::Deny; }
                match c.action {
                    AuthAction::Select => Authorization::Allow,
                    AuthAction::Pragma { pragma_name: "user_version" | "application_id", pragma_value: None } => Authorization::Allow,
                    AuthAction::Read { table_name: "sqlite_schema" | "sqlite_master", column_name: "type" | "name" | "tbl_name" | "sql" }
                        if c.database_name == Some("main") => Authorization::Allow,
                    AuthAction::Function { function_name }
                        if ["date", "time", "datetime", "julianday", "unixepoch", "strftime", "timediff"].iter().any(|n| function_name.eq_ignore_ascii_case(n)) => Authorization::Allow,
                    _ => Authorization::Deny,
                }
            }))?;
            inspect(&conn, store)
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

#[cfg(unix)]
fn immutable_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_bytes() { uri.push_str(&format!("%{byte:02X}")); }
    uri.push_str("?mode=ro&immutable=1");
    uri
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
