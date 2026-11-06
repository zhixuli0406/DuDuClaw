//! Durable deletion intents. No audio, keys, tokens or provider bodies are stored.
//! Stable sharded OS locks survive record replacement and exclude active uploads.
use std::{fs::{self, File}, io::{self, Read, Write}, path::{Path, PathBuf}};
#[cfg(not(windows))]
use std::fs::OpenOptions;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use super::{GoogleSttEngine, TranscriptionError, Duration};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
type DirectoryGuard = File;
#[cfg(not(windows))]
type DirectoryGuard = ();

const DIRECTORY: &str = "google_stt_cleanup";
// Upload HTTP requests have a 600-second total timeout. Unknown generations
// remain journaled beyond that window, even after an early successful DELETE:
// an interrupted upload can still land after its client connection disappears.
const UPLOAD_SETTLE_SECS: u64 = 900;

fn error() -> TranscriptionError { TranscriptionError::new("無法讀寫雲端錄音清理紀錄；請檢查資料目錄。") }
fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    bucket: String,
    object: String,
    generation: Option<String>,
    state: State,
    settle_after: u64,
    attempts: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State { Pending }

pub(super) struct PendingObject {
    path: PathBuf,
    record: Record,
    _lock: duduclaw_core::owner_lock::OwnerLock,
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)] File::open(dir)?.sync_all()?;
    #[cfg(not(unix))] let _ = dir;
    Ok(())
}

// Follow the private-directory convention used by mcp_secret_store. The
// helpers there are private; verify the opened inode as well as the path here.
#[cfg(windows)]
fn private_dir(dir: &Path, create: bool) -> io::Result<DirectoryGuard> { windows::private_dir(dir, create) }

#[cfg(not(windows))]
fn private_dir(dir: &Path, create: bool) -> io::Result<DirectoryGuard> {
    if create {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)] {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(dir) {
            Ok(()) => {},
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e),
        }
    }
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("cleanup directory is not a real directory"));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK).open(dir)?;
        file.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))] duduclaw_core::platform::set_owner_only(dir)?;
    Ok(())
}

fn private_file(path: &Path, create: bool) -> io::Result<File> {
    let _directory = private_dir(path.parent().ok_or_else(|| io::Error::other("missing parent"))?, false)?;
    private_file_in_directory(path, create)
}

#[cfg(windows)]
fn private_file_in_directory(path: &Path, create: bool) -> io::Result<File> { windows::private_file(path, create) }

#[cfg(not(windows))]
fn private_file_in_directory(path: &Path, create: bool) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(io::Error::other("cleanup entry is not a regular file"));
        },
        Ok(_) => {},
        Err(e) if create && e.kind() == io::ErrorKind::NotFound => {},
        Err(e) => return Err(e),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).truncate(false);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() { return Err(io::Error::other("cleanup entry is not a regular file")); }
    #[cfg(unix)] {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.nlink() != 1 { return Err(io::Error::other("cleanup entry has multiple links")); }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))] duduclaw_core::platform::set_owner_only(path)?;
    Ok(file)
}

fn lock_record(path: &Path) -> Result<Option<duduclaw_core::owner_lock::OwnerLock>, TranscriptionError> {
    let name = path.file_stem().and_then(|s| s.to_str()).ok_or_else(error)?;
    let id = uuid::Uuid::parse_str(name).map_err(|_| error())?;
    // At most 256 permanent sidecars; never unlink an OS lock inode.
    let lock_path = path.parent().ok_or_else(error)?.join(format!("{:02x}.lock", id.as_bytes()[0]));
    let file = private_file(&lock_path, true).map_err(|_| error())?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(duduclaw_core::owner_lock::OwnerLock::new(file))),
        // Windows reports ERROR_LOCK_VIOLATION, not ErrorKind::WouldBlock.
        Err(e) if e.kind() == io::ErrorKind::WouldBlock
            || (e.raw_os_error().is_some()
                && e.raw_os_error() == fs2::lock_contended_error().raw_os_error()) => Ok(None),
        Err(_) => Err(error()),
    }
}

impl PendingObject {
    pub(super) fn create(home: &Path, bucket: &str, object: &str) -> Result<Self, TranscriptionError> {
        let dir = home.join(DIRECTORY);
        let _directory = private_dir(&dir, true).map_err(|_| error())?;
        // Persist the new directory itself before any upload can start.
        sync_dir(home).map_err(|_| error())?;
        for _ in 0..32 {
            let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
            let Some(lock) = lock_record(&path)? else { continue; };
            let pending = Self { path, record: Record { bucket: bucket.into(), object: object.into(),
                generation: None, state: State::Pending, settle_after: now().saturating_add(UPLOAD_SETTLE_SECS), attempts: 0 }, _lock: lock };
            pending.validate()?;
            pending.persist()?;
            return Ok(pending);
        }
        Err(error())
    }
    fn validate(&self) -> Result<(), TranscriptionError> {
        // Persisted data never controls the hostname or unrestricted object paths.
        let bucket = &self.record.bucket;
        if !(3..=63).contains(&bucket.len()) || !bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !bucket.as_bytes()[0].is_ascii_alphanumeric() || !bucket.as_bytes()[bucket.len() - 1].is_ascii_alphanumeric() {
            return Err(error());
        }
        let object = self.record.object.strip_prefix("duduclaw-meetings/").and_then(|s| s.strip_suffix(".m4a")).ok_or_else(error)?;
        #[cfg(test)] let valid_object = object == "test" || uuid::Uuid::parse_str(object).is_ok();
        #[cfg(not(test))] let valid_object = uuid::Uuid::parse_str(object).is_ok();
        if !valid_object || self.record.generation.as_ref().is_some_and(|s| s.is_empty() || s.len() > 32 || !s.bytes().all(|b| b.is_ascii_digit())) { return Err(error()); }
        Ok(())
    }
    fn persist(&self) -> Result<(), TranscriptionError> {
        let dir = self.path.parent().ok_or_else(error)?;
        let _directory = private_dir(dir, false).map_err(|_| error())?;
        // Refuse existing links/special files rather than silently replacing them.
        match private_file(&self.path, false) {
            Ok(_) => {},
            Err(e) if e.kind() == io::ErrorKind::NotFound => {},
            Err(_) => return Err(error()),
        }
        let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|_| error())?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file().set_permissions(fs::Permissions::from_mode(0o600)).map_err(|_| error())?;
        }
        #[cfg(windows)] drop(windows::private_file(temp.path(), false).map_err(|_| error())?);
        #[cfg(not(any(unix, windows)))] duduclaw_core::platform::set_owner_only(temp.path()).map_err(|_| error())?;
        serde_json::to_writer(&mut temp, &self.record).map_err(|_| error())?;
        temp.flush().and_then(|_| temp.as_file().sync_all()).map_err(|_| error())?;
        temp.persist(&self.path).map_err(|_| error())?;
        sync_dir(dir).map_err(|_| error())
    }
    pub(super) fn set_generation(&mut self, generation: Option<&str>) -> Result<(), TranscriptionError> {
        if let Some(generation) = generation {
            if generation.is_empty() || generation.len() > 32 || !generation.bytes().all(|b| b.is_ascii_digit()) { return Err(error()); }
            self.record.generation = Some(generation.to_owned());
            self.persist()?;
        }
        Ok(())
    }
    fn finish(&self) -> Result<(), TranscriptionError> {
        // A missing upload response leaves its generation unknown. Keep the
        // intent until the grace window closes to catch late upload completion.
        if self.record.generation.is_none() && now() < self.record.settle_after { return Ok(()); }
        fs::remove_file(&self.path).map_err(|_| error())?;
        sync_dir(self.path.parent().ok_or_else(error)?).map_err(|_| error())
    }
}

fn paths(home: &Path) -> Result<Vec<PathBuf>, TranscriptionError> {
    // Windows can report NotFound for `regular-file/child`. Only a genuinely
    // absent home or journal means there is no pending cleanup.
    match fs::metadata(home) {
        Ok(metadata) if metadata.is_dir() => {},
        Ok(_) => return Err(error()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(error()),
    }
    let dir = home.join(DIRECTORY);
    let _directory = match private_dir(&dir, false) {
        Ok(directory) => directory,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(error()),
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(error()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| error())?;
        if entry.path().extension().and_then(|s| s.to_str()) == Some("json") { paths.push(entry.path()); }
    }
    paths.sort();
    Ok(paths)
}

impl GoogleSttEngine {
    pub fn has_pending_cleanup(home: &Path) -> Result<bool, TranscriptionError> { Ok(!paths(home)?.is_empty()) }

    /// Maintenance is independent of the selected engine, but pinned to the current bucket.
    /// A missing/rotated credential leaves records intact for later retries.
    pub async fn reconcile_pending(credentials: String, home: &Path) -> Result<usize, TranscriptionError> {
        let credentials = zeroize::Zeroizing::new(credentials);
        super::validate_service_account_json(&credentials)?;
        let config = crate::transcription::config::TranscriptionConfig::load(home)
            .map_err(|_| error())?.google_stt;
        let engine = Self { credentials, config, client: super::http_client()?,
            speech_base: String::new(), storage_base: "https://storage.googleapis.com".into(), cleanup_home: home.into(),
            #[cfg(test)] test_token: None, #[cfg(test)] _test_home: None };
        engine.reconcile_cleanup().await
    }

    pub async fn reconcile_cleanup(&self) -> Result<usize, TranscriptionError> {
        let mut token = None;
        let mut failed = false;
        let mut bucket_mismatches = 0usize;
        for path in paths(&self.cleanup_home)? {
            let lock = match lock_record(&path) {
                Ok(Some(lock)) => lock,
                Ok(None) => continue,
                Err(_) => { failed = true; continue; }
            };
            let file = match private_file(&path, false) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => { failed = true; continue; }
            };
            let mut bytes = Vec::new();
            if file.take(4097).read_to_end(&mut bytes).is_err() || bytes.len() > 4096 {
                failed = true; continue;
            }
            let record: Record = match serde_json::from_slice(&bytes) { Ok(record) => record, Err(_) => { failed = true; continue; } };
            let mut pending = PendingObject { path, record, _lock: lock };
            if pending.validate().is_err() { failed = true; continue; }
            if pending.record.bucket != self.config.bucket {
                bucket_mismatches += 1;
                continue;
            }
            if pending.record.generation.is_none() && now() < pending.record.settle_after { continue; }
            if token.is_none() {
                match self.access_token().await {
                    Ok(value) => token = Some(value),
                    Err(_) => { failed = true; break; },
                }
            }
            if self.cleanup_object(&mut pending, token.as_ref().unwrap()).await.is_err() { failed = true; }
        }
        if bucket_mismatches > 0 {
            tracing::warn!(count = bucket_mismatches, "Google STT cleanup bucket mismatch; records retained");
        }
        if failed { return Err(error()); }
        Ok(paths(&self.cleanup_home)?.len())
    }

    pub(super) async fn cleanup_object(&self, pending: &mut PendingObject, token: &str) -> Result<bool, TranscriptionError> {
        pending.validate()?;
        if pending.record.bucket != self.config.bucket { return Err(error()); }
        for attempt in 0..3 {
            pending.record.attempts = pending.record.attempts.saturating_add(1);
            pending.persist()?;
            let mut request = self.client.delete(format!("{}/storage/v1/b/{}/o/{}", self.storage_base,
                pending.record.bucket, urlencoding::encode(&pending.record.object)))
                .bearer_auth(token).timeout(Duration::from_secs(30));
            if let Some(generation) = &pending.record.generation { request = request.query(&[("generation", generation)]); }
            let response = request.send().await;
            let retry = match response {
                Ok(response) if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND => { pending.finish()?; return Ok(true); },
                Ok(response) => response.status().is_server_error() || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS,
                Err(_) => true,
            };
            if !retry { break; }
            if attempt < 2 {
                #[cfg(not(test))] tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                #[cfg(test)] tokio::task::yield_now().await;
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
