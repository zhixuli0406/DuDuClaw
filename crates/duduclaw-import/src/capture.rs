//! Read-only schema inventory. This is a diagnostic, never migration authority.
use std::path::Path;
use rusqlite::{Connection, OpenFlags, hooks::{AuthAction, AuthContext, Authorization}};
use serde::Serialize;
use sha2::{Digest, Sha256};
use crate::{catalog::{self, Store}, expected, sqlite, Error, Result};

#[cfg(target_os = "linux")]
#[path = "capture_memfd.rs"]
mod memfd;

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
fn capture_with_hook(home: &Path, creating_version: &str, after_copy: impl Fn(Store)) -> Result<Report> {
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
        if !metadata.is_file() || metadata.len() > crate::MAX_SOURCE_BYTES {
            entry.status = "refused_file_or_limit"; report.stores.push(entry); continue;
        }
        if metadata.nlink() != 1 {
            entry.status = "refused_hardlink"; report.stores.push(entry); continue;
        }
        let before = match snapshot(home, &directory, store) {
            Ok(state) if state.store == identity(&metadata) => state,
            Ok(_) => { entry.status = "refused_sqlite_or_changed"; report.stores.push(entry); continue; },
            Err(status) => { entry.status = status; report.stores.push(entry); continue; },
        };
        // SQLite never receives a descriptor/path into the customer's home.
        let result = (|| -> Result<StoreReport> {
            let copy = PrivateCopy::new(&file, home, metadata.len())?;
            let inspected = (|| -> Result<StoreReport> {
            after_copy(store);
            if snapshot(home, &directory, store).ok().as_ref() != Some(&before)
                || identity(&file.metadata()?) != before.store { return Err(Error::Io); }
                let mut conn = copy.open()?;
                let inspected = (|| -> Result<StoreReport> {
                    conn.execute_batch("PRAGMA temp_store=MEMORY;")?;
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
                let closed = copy.close(conn);
                closed?;
                inspected
            })();
            let removed = copy.remove();
            removed?;
            let found = inspected?;
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

#[cfg(target_os = "linux")]
const COPY_MECHANISM: &str = "sealed_memfd";
#[cfg(not(target_os = "linux"))]
const COPY_MECHANISM: &str = "private_0700_directory_0600_file";

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

#[cfg(unix)]
struct PrivateCopy {
    #[cfg(target_os = "linux")]
    file: std::fs::File,
    #[cfg(target_os = "linux")]
    copied_bytes: u64,
    #[cfg(not(target_os = "linux"))]
    directory: tempfile::TempDir,
}
#[cfg(unix)]
impl PrivateCopy {
    fn new(input: impl std::io::Read, _home: &Path, expected_bytes: u64) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let (file, copied_bytes) = memfd::copy(input, expected_bytes)?;
            Ok(Self { file, copied_bytes })
        }
        #[cfg(not(target_os = "linux"))]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // Ignore TMPDIR. The shared fixed root must be root-owned and sticky
            // if writable by other users. Request 0700 at directory creation.
            let root = std::fs::canonicalize("/tmp")?;
            let home = std::fs::canonicalize(_home)?;
            let meta = std::fs::metadata(&root)?;
            if meta.uid() != 0 || (meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0)
                || root.starts_with(&home) { return Err(Error::Isolation); }
            let directory = tempfile::Builder::new().prefix("duduclaw-capture-")
                .permissions(std::fs::Permissions::from_mode(0o700)).tempdir_in(&root)?;
            let copied = (|| -> Result<()> {
                let path = directory.path();
                if path.starts_with(&home) || home.starts_with(path)
                    || std::fs::metadata(path)?.mode() & 0o777 != 0o700 { return Err(Error::Isolation); }
                let source = path.join("source.db");
                sqlite::copy_bounded(input, &source)?;
                if std::fs::metadata(source)?.len() != expected_bytes { return Err(Error::Io); }
                Ok(())
            })();
            if let Err(error) = copied {
                directory.close().map_err(|_| Error::Isolation)?;
                return Err(error);
            }
            Ok(Self { directory })
        }
    }
    fn open(&self) -> Result<Connection> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        #[cfg(target_os = "linux")]
        {
            self.verify()?;
            // Omit SQLITE_OPEN_NOFOLLOW only for our own procfs magic link:
            // the owned, sealed anonymous fd cannot be replaced by an attacker.
            // immutable=1 disables locks and all journal/WAL sidecar creation.
            let uri = format!("file:{}?mode=ro&immutable=1", self.path().display());
            Ok(Connection::open_with_flags(uri, flags)?)
        }
        #[cfg(not(target_os = "linux"))]
        { Ok(Connection::open_with_flags(immutable_uri(&self.path()), flags)?) }
    }
    fn verify(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        { memfd::verify(&self.file, self.copied_bytes)?; }
        Ok(())
    }
    fn close(&self, conn: Connection) -> Result<()> {
        // Drop even a connection returned by a failed close before checking the fd.
        let closed = conn.close().map_err(|(_, error)| error);
        self.verify()?;
        closed?;
        Ok(())
    }
    fn remove(self) -> Result<()> {
        #[cfg(not(target_os = "linux"))]
        self.directory.close().map_err(|_| Error::Isolation)?;
        Ok(())
    }
    fn path(&self) -> std::path::PathBuf {
        #[cfg(target_os = "linux")]
        { use std::os::fd::AsRawFd; format!("/proc/self/fd/{}", self.file.as_raw_fd()).into() }
        #[cfg(not(target_os = "linux"))]
        { self.directory.path().join("source.db") }
    }
}

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
