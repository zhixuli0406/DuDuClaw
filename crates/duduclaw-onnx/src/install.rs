//! Verified downloads and the shared ONNX Runtime install.
//!
//! Each transfer uses a fresh, exclusive random temporary file in the destination
//! directory. Hash the open file before publishing with a rename. Old partials
//! are never resumed; cancellation and errors remove this transfer's temporary.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::manifest::{self, ArchiveKind, ORT_VERSION, OrtArchive, OrtSource};
use crate::{OnnxError, OnnxResult};

#[path = "verified_file.rs"]
pub(crate) mod verified_file;

/// Lock sentinel preventing two processes installing into one directory.
const LOCK_FILE: &str = "install.lock";
/// A lock older than this is assumed to belong to a crashed install.
const LOCK_STALE_SECS: u64 = 6 * 60 * 60;

/// Byte-count sink for one transfer. Called often — must be cheap.
pub type BytesFn<'a> = &'a (dyn Fn(u64) + Send + Sync);

/// SHA-256 of a file, lowercase hex. Streams — never loads the file.
pub fn sha256_file(path: &Path) -> OnnxResult<String> {
    let mut f = std::fs::File::open(path)?;
    sha256_reader(&mut f)
}

fn sha256_reader(f: &mut impl Read) -> OnnxResult<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Lowercase hex of `bytes`.
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Result of checking one file against its pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// Present with the right size and hash.
    Verified,
    /// Absent entirely.
    Missing,
    /// Present but the wrong size or hash — must be re-downloaded.
    Corrupt(String),
}

/// Full verification of the file at `path` (hashes the whole thing).
/// `label` names it in the message.
pub fn verify_path(path: &Path, label: &str, size: u64, sha256: &str) -> FileState {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return FileState::Missing,
    };
    if meta.len() != size {
        return FileState::Corrupt(format!(
            "{label}: expected {size} bytes, found {}",
            meta.len()
        ));
    }
    match sha256_file(path) {
        Ok(got) if got == sha256 => FileState::Verified,
        Ok(got) => FileState::Corrupt(format!(
            "{label}: sha256 mismatch (expected {sha256}, got {got})"
        )),
        Err(e) => FileState::Corrupt(format!("{label}: cannot hash: {e}")),
    }
}

/// `<dest>.part` — sibling temp file so the rename stays on one filesystem.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Host portion of a URL, for error text that must not carry query strings.
fn short_url(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

/// Reject unknown redirect destinations and HTTPS downgrades, at most five hops.
fn redirect_allowed(initial: &reqwest::Url, next: &reqwest::Url, hops: usize) -> bool {
    if hops > 5 || initial.scheme() != "https" || next.scheme() != "https"
        || !next.username().is_empty() || next.password().is_some()
        || next.port_or_known_default() != Some(443)
    {
        return false;
    }
    let Some(host) = next.host_str() else { return false; };
    let allowed = match initial.host_str() {
        Some("github.com") => manifest::GITHUB_REDIRECT_HOSTS,
        Some("api.nuget.org") => manifest::NUGET_REDIRECT_HOSTS,
        // The same client downloads the pinned model manifests, too.
        Some("huggingface.co") => manifest::MODEL_REDIRECT_HOSTS,
        _ => return false,
    };
    allowed.contains(&host)
        || (initial.host_str() == Some("huggingface.co")
            && manifest::MODEL_REDIRECT_HOST_SUFFIXES.iter().any(|suffix| host.ends_with(suffix)))
}

/// HTTP client for pinned downloads, with a fail-closed redirect policy.
pub fn http_client(user_agent: &str) -> OnnxResult<reqwest::Client> {
    reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().first().is_some_and(|initial|
                redirect_allowed(initial, attempt.url(), attempt.previous().len()))
            {
                attempt.follow()
            } else {
                attempt.error("download redirect refused")
            }
        }))
        .user_agent(user_agent.to_string())
        .build()
        .map_err(|e| OnnxError::msg(format!("HTTP client: {e}")))
}

fn exclusive_temp(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    options.open(path)
}

/// Create a random, exclusive, no-follow temporary beside the destination.
pub fn new_temp(dest: &Path) -> OnnxResult<tempfile::NamedTempFile> {
    let parent = dest.parent().ok_or_else(|| OnnxError::msg("目的地沒有父目錄"))?;
    let prefix = format!(".{}.dudu-", dest.file_name().unwrap_or_default().to_string_lossy());
    // tempfile retries random collisions; the callback never truncates or follows links.
    Ok(tempfile::Builder::new().prefix(&prefix).suffix(".part").rand_bytes(16)
        .make_in(parent, exclusive_temp)?)
}

/// Remove only old, matching regular files whose ownership can be established.
/// On Windows std cannot establish ownership, so leave stale files untouched.
fn cleanup_stale_temps(dest: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Some(parent) = dest.parent() else { return; };
        let prefix = format!(".{}.dudu-", dest.file_name().unwrap_or_default().to_string_lossy());
        let Ok(entries) = std::fs::read_dir(parent) else { return; };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let legacy = entry.path() == part_path(dest) || entry.path() == dest.with_extension("verified")
                || entry.path() == dest.with_extension("part");
            let random = name.strip_prefix(&prefix).and_then(|s| s.strip_suffix(".part"))
                .is_some_and(|s| s.len() == 16 && s.bytes().all(|b| b.is_ascii_alphanumeric()));
            if !legacy && !random { continue; }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue; };
            // SAFETY: geteuid is a side-effect-free query.
            if meta.is_file() && meta.uid() == unsafe { libc::geteuid() }
                && meta.modified().ok().and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > LOCK_STALE_SECS)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dest;
}

/// Download into a fresh exclusive file, verify through its handle, then publish.
pub async fn fetch_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expect_size: u64,
    expect_sha: &str,
    cancel: &Arc<AtomicBool>,
    on_bytes: BytesFn<'_>,
) -> OnnxResult<()> {
    cleanup_stale_temps(dest);
    let part = fetch_partial(client, url, dest, expect_size, cancel, on_bytes).await?;
    finish_download(part, dest, expect_size, expect_sha)?;
    on_bytes(expect_size);
    Ok(())
}

async fn fetch_partial(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expect_size: u64,
    cancel: &Arc<AtomicBool>,
    on_bytes: BytesFn<'_>,
) -> OnnxResult<tempfile::NamedTempFile> {
    let mut part = new_temp(dest)?;
    let resp = client.get(url).send().await
        .map_err(|e| OnnxError::msg(format!("下載失敗：{e}")))?;
    if !resp.status().is_success() {
        return Err(OnnxError::msg(format!("下載失敗（HTTP {}）：{}",
            resp.status().as_u16(), short_url(url))));
    }
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            return Err(OnnxError::msg("下載已取消"));
        }
        let chunk = chunk.map_err(|e| OnnxError::msg(format!("下載中斷：{e}")))?;
        written = written.checked_add(chunk.len() as u64)
            .ok_or_else(|| OnnxError::msg("下載檔大小溢位"))?;
        if written > expect_size {
            return Err(OnnxError::msg(format!("下載檔超過預期大小（{expect_size} 位元組），已刪除")));
        }
        part.write_all(&chunk)?;
        on_bytes(written);
    }
    part.flush()?;
    Ok(part)
}

fn verify_temp(part: &mut tempfile::NamedTempFile, size: u64, sha: &str) -> OnnxResult<()> {
    if part.as_file().metadata()?.len() != size {
        return Err(OnnxError::msg("下載檔大小不符，已刪除重來"));
    }
    part.seek(SeekFrom::Start(0))?;
    if sha256_reader(part.as_file_mut())? != sha {
        return Err(OnnxError::msg("下載檔完整性驗證失敗（sha256 不符），已刪除；請重新下載"));
    }
    Ok(())
}

/// Verify size and digest through the held handle, then atomically rename.
pub fn finish_download(mut part: tempfile::NamedTempFile, dest: &Path, size: u64, sha: &str) -> OnnxResult<()> {
    verify_temp(&mut part, size, sha)?;
    part.persist(dest).map_err(|e| OnnxError::Io(e.error))?;
    Ok(())
}

/// Pull exactly one member out of the ORT release archive.
pub fn extract_member(archive_path: &Path, archive: &OrtArchive, dest: &Path) -> OnnxResult<()> {
    let max_size = max_library_size(lib_pin(archive)?)?;
    let part = extract_source_member(archive_path, &archive.sources().next().unwrap(), dest, max_size)?;
    part.persist(dest).map_err(|e| OnnxError::Io(e.error))?;
    Ok(())
}

fn max_library_size(pins: &[(u64, &str)]) -> OnnxResult<u64> {
    pins.iter().map(|p| p.0).max().ok_or_else(|| OnnxError::msg("ONNX Runtime 缺少版本紀錄"))
}

fn safe_member(member: &str) -> bool {
    !member.is_empty() && !member.contains('\\') && !member.contains(':')
        && member.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn copy_member(reader: impl Read, out: &mut impl Write, max_size: u64) -> OnnxResult<()> {
    let limit = max_size.checked_add(1).ok_or_else(|| OnnxError::msg("ONNX Runtime 大小紀錄錯誤"))?;
    if std::io::copy(&mut reader.take(limit), out)? > max_size {
        return Err(OnnxError::msg("ONNX Runtime 壓縮檔成員超過大小上限"));
    }
    Ok(())
}

fn extract_source_member(
    archive_path: &Path, source: &OrtSource, dest: &Path, max_size: u64,
) -> OnnxResult<tempfile::NamedTempFile> {
    if !safe_member(source.member) {
        return Err(OnnxError::msg("ONNX Runtime 壓縮檔成員路徑不安全"));
    }
    let mut out = new_temp(dest)?;
    let mut hits = 0;
    match source.kind {
        ArchiveKind::TarGz => {
            let f = std::fs::File::open(archive_path)?;
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(f));
            for entry in tar.entries()? {
                let mut entry = entry?;
                if entry.path_bytes().as_ref() == source.member.as_bytes() {
                    hits += 1;
                    if hits != 1 || !entry.header().entry_type().is_file() {
                        return Err(OnnxError::msg("ONNX Runtime 成員重複或不是一般檔案"));
                    }
                    copy_member(&mut entry, &mut out, max_size)?;
                }
            }
        }
        ArchiveKind::Zip => {
            let f = std::fs::File::open(archive_path)?;
            let mut zip = zip::ZipArchive::new(f)
                .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 無法讀取：{e}")))?;
            // ZipArchive indexes names and would otherwise hide duplicate members.
            zip.validate_header_consistency()
                .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 標頭不一致：{e}")))?;
            for index in 0..zip.len() {
                let mut entry = zip.by_index(index)
                    .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 無法讀取：{e}")))?;
                let name = entry.name()
                    .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 成員名稱無法讀取：{e}")))?;
                if !safe_member(name.trim_end_matches('/')) {
                    return Err(OnnxError::msg("ONNX Runtime zip 成員路徑不安全"));
                }
                if name == source.member {
                    hits += 1;
                    let kind = entry.unix_mode().map(|mode| mode & 0o170000);
                    if hits != 1 || !entry.is_file() || kind.is_some_and(|k| k != 0 && k != 0o100000) {
                        return Err(OnnxError::msg("ONNX Runtime zip 成員重複或不是一般檔案"));
                    }
                    copy_member(&mut entry, &mut out, max_size)?;
                }
            }
        }
    }
    if hits != 1 {
        return Err(OnnxError::msg(format!("ONNX Runtime 壓縮檔裡找不到 {}（版本可能已變更）", source.member)));
    }
    out.flush()?;
    Ok(out)
}

/// Where a downloaded (or bundled) ORT archive is kept while it is
/// verified and extracted: `<ort_lib_root>/<ver>/onnxruntime-<ver>.<ext>`.
pub fn ort_archive_path(ort_lib_root: &Path, archive: &OrtArchive) -> PathBuf {
    manifest::ort_lib_dir(ort_lib_root).join(format!(
        "onnxruntime-{ORT_VERSION}.{}",
        archive.kind.extension()
    ))
}

/// Whether the ORT shared library for this platform is present (not
/// verified; see [`verify_ort_lib`]).
pub fn ort_installed(ort_lib_root: &Path) -> bool {
    manifest::ort_lib_path(ort_lib_root).is_some_and(|p| p.exists())
}

/// Check installed-library readiness. No successful hash is cached.
pub fn verify_ort_lib(ort_lib_root: &Path) -> Result<(), String> {
    let lib = manifest::ort_lib_path(ort_lib_root)
        .ok_or_else(|| "這個平台沒有可用的 ONNX Runtime".to_string())?;
    verify_ort_lib_path(&lib)
}

/// Readiness check only; the loader must retain the handle returned by verify_for_load.
pub fn verify_ort_lib_path(lib: &Path) -> Result<(), String> {
    let pins = manifest::ort_lib_pins(manifest::current_target())
        .ok_or_else(|| "這個平台沒有可用的 ONNX Runtime".to_string())?;
    verify_lib_at(lib, pins)
}

fn verify_lib_at(lib: &Path, pins: &[(u64, &str)]) -> Result<(), String> {
    verified_file::verify_for_load(lib, pins).map(|_| ())
}

#[cfg(unix)]
const MANAGED_DIRECTORY: &str = "ONNX Runtime 目錄由其他使用者管理；請由該使用者更新元件";

#[cfg(unix)]
fn managed_directory_result(
    owner: u32, current_uid: u32, verify: impl FnOnce() -> Result<(), String>,
) -> OnnxResult<bool> {
    if owner == current_uid { return Ok(false); }
    verify().map_err(|_| OnnxError::msg(MANAGED_DIRECTORY))?;
    Ok(true)
}

/// Never repair, lock, or clean up a version directory managed by another uid.
fn use_managed_library(dir: &Path, lib: &Path, pins: &[(u64, &str)]) -> OnnxResult<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = match std::fs::symlink_metadata(dir) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        // SAFETY: geteuid has no memory preconditions.
        managed_directory_result(meta.uid(), unsafe { libc::geteuid() }, || verify_lib_at(lib, pins))
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, lib, pins);
        Ok(false)
    }
}

fn prepare_lib_dir(dir: &Path) -> OnnxResult<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY).open(dir)?;
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no memory preconditions.
        if file.metadata()?.uid() != unsafe { libc::geteuid() } {
            return Err(OnnxError::msg(MANAGED_DIRECTORY));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn lib_pin(archive: &OrtArchive) -> OnnxResult<&'static [(u64, &'static str)]> {
    manifest::ort_lib_pins(archive.target).ok_or_else(|| {
        OnnxError::msg(format!(
            "{} 沒有 ONNX Runtime 執行庫的版本紀錄",
            archive.target
        ))
    })
}

pub(crate) fn success_record(source: OrtSource) -> String {
    let host = reqwest::Url::parse(source.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or("unknown".to_string());
    format!("ONNX Runtime 來源成功：kind={:?} host={}", source.kind, host)
}

/// Download, verify and extract the ORT library for `archive` into
/// `<ort_lib_root>/<ver>/`. A library already there that matches its pin is
/// kept; one that does not is replaced. `on_bytes` gets bytes of the archive
/// moved so far. Holds the install lock of the library directory.
pub async fn install_ort(
    client: &reqwest::Client,
    ort_lib_root: &Path,
    archive: &OrtArchive,
    cancel: &Arc<AtomicBool>,
    on_bytes: BytesFn<'_>,
) -> OnnxResult<()> {
    let pin = lib_pin(archive)?;
    let lib_dir = manifest::ort_lib_dir(ort_lib_root);
    let lib_path = lib_dir.join(archive.lib_file);
    if use_managed_library(&lib_dir, &lib_path, pin)? || verify_lib_at(&lib_path, pin).is_ok() {
        on_bytes(max_source_size(archive));
        return Ok(());
    }
    prepare_lib_dir(&lib_dir)?;
    let _lock = InstallLock::acquire(&lib_dir)?;
    let reserved = max_source_size(archive);
    install_from_sources(ort_lib_root, archive, pin, cancel, |source, dest| async move {
        // Each source reports an independent byte count; a failed source's
        // partial bytes are discarded before the next source starts.
        on_bytes(0);
        fetch_partial(client, source.url, &dest, source.size, cancel, &|bytes| {
            let scaled = if source.size == 0 {
                0
            } else {
                ((bytes.min(source.size) as u128 * reserved as u128) / source.size as u128)
                    .min(reserved as u128) as u64
            };
            on_bytes(scaled);
        }).await
    }).await?;
    on_bytes(reserved);
    Ok(())
}

/// Maximum transfer size among the ordered sources, for aggregate progress
/// callers that need to reserve the runtime portion before selecting a source.
pub fn max_source_size(archive: &OrtArchive) -> u64 {
    archive.sources().map(|source| source.size).max().unwrap_or(0)
}

// The fetcher returns an exclusively owned temporary; verification stays here.
async fn install_from_sources<F, Fut>(
    ort_lib_root: &Path,
    archive: &OrtArchive,
    pins: &[(u64, &str)],
    cancel: &Arc<AtomicBool>,
    mut fetch: F,
) -> OnnxResult<()>
where
    F: FnMut(OrtSource, PathBuf) -> Fut,
    Fut: std::future::Future<Output = OnnxResult<tempfile::NamedTempFile>>,
{
    let lib_dir = manifest::ort_lib_dir(ort_lib_root);
    let lib_path = lib_dir.join(archive.lib_file);
    let mut failures = Vec::new();
    for (index, source) in archive.sources().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(OnnxError::msg("下載已取消"));
        }
        let path = if index == 0 {
            ort_archive_path(ort_lib_root, archive)
        } else {
            lib_dir.join(format!("onnxruntime-{ORT_VERSION}-source-{index}.{}", source.kind.extension()))
        };
        let result = async {
            cleanup_stale_temps(&path);
            let part = fetch(source, path.clone()).await?;
            finish_download(part, &path, source.size, source.sha256)?;
            extract_verified(&path, &source, &lib_path, pins)
        }.await;
        let _ = std::fs::remove_file(&path);
        match result {
            Ok(()) => {
                eprintln!("{}", success_record(source));
                return Ok(());
            }
            Err(error) => {
                let why = format!("{}: {error}", source.url).replace(['\n', '\r'], " ");
                eprintln!("ONNX Runtime 來源失敗：{why}");
                failures.push(why);
            }
        }
    }
    Err(OnnxError::msg(format!("ONNX Runtime 所有下載來源失敗：{}", failures.join("; "))))
}

/// Verify the extracted file before the atomic rename to the installed name.
fn extract_verified(
    archive_path: &Path,
    source: &OrtSource,
    lib_path: &Path,
    pins: &[(u64, &str)],
) -> OnnxResult<()> {
    cleanup_stale_temps(lib_path);
    let mut staging = extract_source_member(archive_path, source, lib_path, max_library_size(pins)?)?;
    let size = staging.as_file().metadata()?.len();
    staging.seek(SeekFrom::Start(0))?;
    if !manifest::matches_lib_pin(pins, size, &sha256_reader(staging.as_file_mut())?) {
        return Err(OnnxError::msg("ONNX Runtime 執行庫大小或雜湊與版本紀錄不符，拒絕安裝"));
    }
    staging.persist(lib_path).map_err(|e| OnnxError::Io(e.error))?;
    Ok(())
}

/// Offline install: an ORT archive placed at [`ort_archive_path`] (from an
/// offline bundle) is verified against its pin and the library extracted
/// and verified. A library already there must match its own pin, otherwise
/// the bundled archive replaces it (and without one the install is
/// refused): a library dropped into place by hand is never trusted.
pub fn install_ort_from_local_archive(ort_lib_root: &Path, archive: &OrtArchive) -> OnnxResult<()> {
    install_ort_from_local_archive_with(ort_lib_root, archive, lib_pin(archive)?)
}

fn install_ort_from_local_archive_with(
    ort_lib_root: &Path,
    archive: &OrtArchive,
    pin: &[(u64, &str)],
) -> OnnxResult<()> {
    let lib_dir = manifest::ort_lib_dir(ort_lib_root);
    let lib_path = lib_dir.join(archive.lib_file);
    if use_managed_library(&lib_dir, &lib_path, pin)? {
        return Ok(());
    }
    let path = ort_archive_path(ort_lib_root, archive);
    let archive_present = path.exists();
    if verify_lib_at(&lib_path, pin).is_ok() && !archive_present {
        return Ok(());
    }
    match verify_path(&path, "onnxruntime", archive.size, archive.sha256) {
        FileState::Verified => {}
        FileState::Missing => {
            return Err(OnnxError::msg(format!(
                "找不到 ONNX Runtime 壓縮檔 {}（現有的執行庫無法驗證）",
                path.display()
            )));
        }
        FileState::Corrupt(why) => {
            return Err(OnnxError::msg(format!(
                "ONNX Runtime 壓縮檔驗證失敗：{why}"
            )));
        }
    }
    prepare_lib_dir(&lib_dir)?;
    let _lock = InstallLock::acquire(&lib_dir)?;
    extract_verified(&path, &archive.sources().next().unwrap(), &lib_path, pin)?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// Cross-process install lock on a directory, released on drop.
#[derive(Debug)]
pub struct InstallLock {
    path: PathBuf,
}

impl InstallLock {
    /// Take the lock in `dir` (which must exist). A lock older than six
    /// hours is treated as left behind by a crashed install.
    pub fn acquire(dir: &Path) -> OnnxResult<Self> {
        let path = dir.join(LOCK_FILE);
        if let Ok(meta) = std::fs::metadata(&path) {
            let stale = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs() > LOCK_STALE_SECS)
                .unwrap_or(true);
            if stale {
                let _ = std::fs::remove_file(&path);
            } else {
                return Err(OnnxError::msg("另一個安裝程序正在下載模型，請稍候"));
            }
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| OnnxError::msg(format!("無法建立安裝鎖 {}：{e}", path.display())))?;
        Ok(Self { path })
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn managed_directory_uses_verified_library_or_requires_its_owner() {
        assert!(managed_directory_result(0, 1000, || Ok(())).unwrap());
        for owner in [0, 1234] {
            let error = managed_directory_result(owner, 1000, || Err("unverified".into()))
                .unwrap_err().to_string();
            assert!(error.contains("由其他使用者管理"));
            assert!(error.contains("由該使用者更新"));
        }
        assert!(!managed_directory_result(1000, 1000, || panic!("self-owned directory can be repaired"))
            .unwrap());
    }

    #[test]
    fn sha256_file_matches_a_known_digest() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("x");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_path_distinguishes_missing_size_and_hash() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("a.bin");
        let sha = hex_lower(&Sha256::digest(b"hello world"));
        assert_eq!(verify_path(&p, "a", 11, &sha), FileState::Missing);
        std::fs::write(&p, b"hello").unwrap();
        assert!(
            matches!(verify_path(&p, "a", 11, &sha), FileState::Corrupt(m) if m.contains("bytes"))
        );
        std::fs::write(&p, b"HELLO WORLD").unwrap();
        assert!(
            matches!(verify_path(&p, "a", 11, &sha), FileState::Corrupt(m) if m.contains("sha256"))
        );
        std::fs::write(&p, b"hello world").unwrap();
        assert_eq!(verify_path(&p, "a", 11, &sha), FileState::Verified);
    }

    #[test]
    fn install_lock_is_exclusive_and_released_on_drop() {
        let tmp = TempDir::new().unwrap();
        let first = InstallLock::acquire(tmp.path()).unwrap();
        assert!(InstallLock::acquire(tmp.path()).is_err());
        drop(first);
        assert!(InstallLock::acquire(tmp.path()).is_ok());
    }

    fn tgz_with(member: &str, body: &[u8], at: &Path) {
        let f = std::fs::File::create(at).unwrap();
        let enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let mut b = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        b.append_data(&mut header, member, body).unwrap();
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn local_archive_install_verifies_then_extracts() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ort");
        std::fs::create_dir_all(manifest::ort_lib_dir(&root)).unwrap();
        let src = tmp.path().join("src.tgz");
        tgz_with("pkg/lib/libx.so", b"ELF", &src);
        let bytes = std::fs::read(&src).unwrap();
        let sha: &'static str = Box::leak(hex_lower(&Sha256::digest(&bytes)).into_boxed_str());
        let archive = OrtArchive {
            target: "test",
            url: "https://example.invalid/x",
            size: bytes.len() as u64,
            sha256: sha,
            kind: ArchiveKind::TarGz,
            member: "pkg/lib/libx.so",
            lib_file: "libx.so",
            fallbacks: &[],
        };
        let pin = &[(
            3u64,
            Box::leak(hex_lower(&Sha256::digest(b"ELF")).into_boxed_str()) as &str,
        )];
        let lib = manifest::ort_lib_dir(&root).join("libx.so");
        // Missing archive: refused.
        assert!(install_ort_from_local_archive_with(&root, &archive, pin).is_err());
        // Tampered archive: refused, nothing extracted.
        std::fs::write(ort_archive_path(&root, &archive), b"tampered").unwrap();
        assert!(install_ort_from_local_archive_with(&root, &archive, pin).is_err());
        assert!(!lib.exists());
        // A library dropped in by hand that does not match its pin, and no
        // archive: refused.
        std::fs::remove_file(ort_archive_path(&root, &archive)).unwrap();
        std::fs::write(&lib, b"EVIL").unwrap();
        assert!(install_ort_from_local_archive_with(&root, &archive, pin).is_err());
        // The real archive replaces it: extracted, verified, archive removed.
        std::fs::write(ort_archive_path(&root, &archive), &bytes).unwrap();
        install_ort_from_local_archive_with(&root, &archive, pin).unwrap();
        assert_eq!(std::fs::read(&lib).unwrap(), b"ELF");
        assert!(!ort_archive_path(&root, &archive).exists());
        assert!(verify_lib_at(&lib, pin).is_ok());
        // An archive whose member does not match the library pin is refused.
        let wrong_pin = &[(3u64, "0".repeat(64).leak() as &str)];
        std::fs::write(ort_archive_path(&root, &archive), &bytes).unwrap();
        assert!(install_ort_from_local_archive_with(&root, &archive, wrong_pin).is_err());
        assert_eq!(std::fs::read(&lib).unwrap(), b"ELF", "failed replacement preserves the installed library");
    }

    #[test]
    fn download_verification_refuses_wrong_size_and_hash_and_cleans_temps() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("f.bin");
        for (size, sha) in [(3, "0".repeat(64)), (5, "0".repeat(64))] {
            let mut part = new_temp(&dest).unwrap();
            part.write_all(b"hello").unwrap();
            let path = part.path().to_path_buf();
            assert!(finish_download(part, &dest, size, &sha).is_err());
            assert!(!path.exists() && !dest.exists());
        }
        let mut part = new_temp(&dest).unwrap();
        part.write_all(b"hello").unwrap();
        finish_download(part, &dest, 5, &hex_lower(&Sha256::digest(b"hello"))).unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"hello");
    }
}

#[cfg(test)]
#[path = "install_source_tests.rs"]
mod source_tests;
