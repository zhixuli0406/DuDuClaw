//! Verified downloads and the shared ONNX Runtime install.
//!
//! A download is:
//!
//! 1. stream the file to `<dest>.part`, resuming with an HTTP `Range`
//!    header when a partial file is already there;
//! 2. hash the completed `.part` and compare against the pinned SHA-256;
//! 3. `rename` it into place — so a file at its final path is always a
//!    verified file, never a truncated download.
//!
//! A mismatch deletes the offending `.part` and fails. Callers write their
//! own install marker only after every file passed, so a partial install is
//! indistinguishable from no install.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::manifest::{self, ArchiveKind, ORT_VERSION, OrtArchive, OrtSource};
use crate::{OnnxError, OnnxResult};

/// Lock sentinel preventing two processes installing into one directory.
const LOCK_FILE: &str = "install.lock";
/// A lock older than this is assumed to belong to a crashed install.
const LOCK_STALE_SECS: u64 = 6 * 60 * 60;

/// Byte-count sink for one transfer. Called often — must be cheap.
pub type BytesFn<'a> = &'a (dyn Fn(u64) + Send + Sync);

/// SHA-256 of a file, lowercase hex. Streams — never loads the file.
pub fn sha256_file(path: &Path) -> OnnxResult<String> {
    let mut f = std::fs::File::open(path)?;
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

/// HTTP client for pinned downloads: connect/read timeouts, no overall timeout
/// (a large file on a slow but active link is not an error).
pub fn http_client(user_agent: &str) -> OnnxResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .user_agent(user_agent.to_string())
        .build()
        .map_err(|e| OnnxError::msg(format!("HTTP client: {e}")))
}

/// Stream `url` into `<dest>.part`, resuming from whatever is already there,
/// verify the hash, then rename into place.
///
/// `cancel` is polled between chunks; cancelling leaves the `.part` file in
/// place so a later call resumes rather than restarts.
pub async fn fetch_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expect_size: u64,
    expect_sha: &str,
    cancel: &Arc<AtomicBool>,
    on_bytes: BytesFn<'_>,
) -> OnnxResult<()> {
    fetch_partial(client, url, dest, expect_size, cancel, on_bytes).await?;
    finish_download(dest, expect_size, expect_sha)?;
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
) -> OnnxResult<()> {
    let part = part_path(dest);
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > expect_size {
        // A stale `.part` from a different revision — start over rather than
        // resume into a file that can never hash correctly.
        let _ = std::fs::remove_file(&part);
        have = 0;
    }

    if have < expect_size {
        let mut req = client.get(url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| OnnxError::msg(format!("下載失敗：{e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(OnnxError::msg(format!(
                "下載失敗（HTTP {}）：{}",
                status.as_u16(),
                short_url(url)
            )));
        }
        // A server that ignored our Range header restarts the file; honour
        // that rather than appending a second copy onto the first.
        let resuming = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut file = if resuming {
            let mut f = std::fs::OpenOptions::new().write(true).open(&part)?;
            f.seek(SeekFrom::Start(have))?;
            f
        } else {
            have = 0;
            std::fs::File::create(&part)?
        };

        let mut written = have;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                file.flush()?;
                return Err(OnnxError::msg("下載已取消"));
            }
            let chunk = chunk.map_err(|e| OnnxError::msg(format!("下載中斷：{e}")))?;
            written += chunk.len() as u64;
            if written > expect_size {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(OnnxError::msg(format!(
                    "下載檔超過預期大小（{expect_size} 位元組），已刪除"
                )));
            }
            file.write_all(&chunk)?;
            on_bytes(written);
        }
        file.flush()?;
        drop(file);
    }

    Ok(())
}

fn finish_download(dest: &Path, expect_size: u64, expect_sha: &str) -> OnnxResult<()> {
    let part = part_path(dest);
    let got_size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if got_size != expect_size {
        let _ = std::fs::remove_file(&part);
        return Err(OnnxError::msg(format!(
            "下載檔大小不符（預期 {expect_size} 位元組，實際 {got_size}），已刪除重來"
        )));
    }
    let got = sha256_file(&part)?;
    if got != expect_sha {
        let _ = std::fs::remove_file(&part);
        return Err(OnnxError::msg(
            "下載檔完整性驗證失敗（sha256 不符），已刪除；請重新下載",
        ));
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// Pull exactly one member out of the ORT release archive.
pub fn extract_member(archive_path: &Path, archive: &OrtArchive, dest: &Path) -> OnnxResult<()> {
    extract_source_member(archive_path, &archive.sources().next().unwrap(), dest)
}

fn safe_member(member: &str) -> bool {
    !member.is_empty() && !member.contains('\\') && !member.contains(':')
        && member.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn extract_source_member(archive_path: &Path, source: &OrtSource, dest: &Path) -> OnnxResult<()> {
    if !safe_member(source.member) {
        return Err(OnnxError::msg("ONNX Runtime 壓縮檔成員路徑不安全"));
    }
    let tmp = dest.with_extension("part");
    let _ = std::fs::remove_file(&tmp);
    let found = match source.kind {
        ArchiveKind::TarGz => {
            let f = std::fs::File::open(archive_path)?;
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(f));
            let mut hit = false;
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.to_string_lossy().to_string();
                if path == source.member {
                    if !entry.header().entry_type().is_file() {
                        return Err(OnnxError::msg("ONNX Runtime 成員不是一般檔案"));
                    }
                    let mut out = std::fs::File::create(&tmp)?;
                    std::io::copy(&mut entry, &mut out)?;
                    hit = true;
                    break;
                }
            }
            hit
        }
        ArchiveKind::Zip => {
            let f = std::fs::File::open(archive_path)?;
            let mut zip = zip::ZipArchive::new(f)
                .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 無法讀取：{e}")))?;
            // Check every entry name before writing anything. Do not normalize
            // traversal components into an otherwise matching pinned name.
            for index in 0..zip.len() {
                let entry = zip.by_index(index)
                    .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 無法讀取：{e}")))?;
                let name = entry.name()
                    .map_err(|e| OnnxError::msg(format!("ONNX Runtime zip 成員名稱無法讀取：{e}")))?;
                if !safe_member(name.trim_end_matches('/')) {
                    return Err(OnnxError::msg("ONNX Runtime zip 成員路徑不安全"));
                }
            }
            match zip.by_name(source.member) {
                Ok(mut entry) => {
                    if !entry.is_file() || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
                        return Err(OnnxError::msg("ONNX Runtime zip 成員不是一般檔案"));
                    }
                    let mut out = std::fs::File::create(&tmp)?;
                    std::io::copy(&mut entry, &mut out)?;
                    true
                }
                Err(_) => false,
            }
        }
    };
    if !found {
        let _ = std::fs::remove_file(&tmp);
        return Err(OnnxError::msg(format!(
            "ONNX Runtime 壓縮檔裡找不到 {}（版本可能已變更）",
            source.member
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
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

/// Check the installed library against its pin before anything loads it.
/// The full hash runs once per (path, size, mtime) per process.
pub fn verify_ort_lib(ort_lib_root: &Path) -> Result<(), String> {
    let target = manifest::current_target();
    let (Some(lib), Some(pin)) = (
        manifest::ort_lib_path(ort_lib_root),
        manifest::ort_lib_pins(target),
    ) else {
        return Err(format!("這個平台（{target}）沒有可用的 ONNX Runtime"));
    };
    verify_lib_at(&lib, pin)
}

/// [`verify_ort_lib`] for an explicit library path (what the loader is
/// about to open), against this platform's pin.
pub fn verify_ort_lib_path(lib: &Path) -> Result<(), String> {
    let target = manifest::current_target();
    let pin = manifest::ort_lib_pins(target)
        .ok_or_else(|| format!("這個平台（{target}）沒有可用的 ONNX Runtime"))?;
    verify_lib_at(lib, pin)
}

fn verify_lib_at(lib: &Path, pins: &[(u64, &str)]) -> Result<(), String> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static OK: OnceLock<Mutex<HashSet<(PathBuf, u64, std::time::SystemTime)>>> = OnceLock::new();
    let meta =
        std::fs::symlink_metadata(lib).map_err(|_| "ONNX Runtime 執行庫尚未安裝".to_string())?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("ONNX Runtime 執行庫不是一般檔案，拒絕載入".to_string());
    }
    let key = (
        lib.to_path_buf(),
        meta.len(),
        meta.modified().unwrap_or(std::time::UNIX_EPOCH),
    );
    let cache = OK.get_or_init(Default::default);
    if cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&key)
    {
        return Ok(());
    }
    if !pins.iter().any(|&(size, _)| size == meta.len()) {
        return Err("ONNX Runtime 執行庫大小與版本紀錄不符，拒絕載入".into());
    }
    let sha = sha256_file(lib).map_err(|e| e.to_string())?;
    if !manifest::matches_lib_pin(pins, meta.len(), &sha) {
        return Err("ONNX Runtime 執行庫雜湊與版本紀錄不符，拒絕載入；請移除後重新安裝".into());
    }
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key);
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
    if verify_lib_at(&lib_path, pin).is_ok() {
        on_bytes(archive.size);
        return Ok(());
    }
    std::fs::create_dir_all(&lib_dir)?;
    let _lock = InstallLock::acquire(&lib_dir)?;
    install_from_sources(ort_lib_root, archive, pin, cancel, |source, dest| async move {
        fetch_partial(client, source.url, &dest, source.size, cancel, on_bytes).await
    }).await
}

// The fetcher writes only <dest>.part. Verification and extraction always stay
// here, including for the offline stub fetcher used by tests.
async fn install_from_sources<F, Fut>(
    ort_lib_root: &Path,
    archive: &OrtArchive,
    pins: &[(u64, &str)],
    cancel: &Arc<AtomicBool>,
    mut fetch: F,
) -> OnnxResult<()>
where
    F: FnMut(OrtSource, PathBuf) -> Fut,
    Fut: std::future::Future<Output = OnnxResult<()>>,
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
            fetch(source, path.clone()).await?;
            finish_download(&path, source.size, source.sha256)?;
            extract_verified(&path, &source, &lib_path, pins)
        }.await;
        let _ = std::fs::remove_file(&path);
        match result {
            Ok(()) => return Ok(()),
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
    let staging = lib_path.with_extension("verified");
    let result = (|| {
        extract_source_member(archive_path, source, &staging)?;
        // Always hash this newly extracted artifact, without the installed-file cache.
        let size = std::fs::metadata(&staging)?.len();
        if !manifest::matches_lib_pin(pins, size, &sha256_file(&staging)?) {
            return Err(OnnxError::msg("ONNX Runtime 執行庫大小或雜湊與版本紀錄不符，拒絕安裝"));
        }
        std::fs::rename(&staging, lib_path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&staging);
    let _ = std::fs::remove_file(staging.with_extension("part"));
    result
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
    std::fs::create_dir_all(&lib_dir)?;
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

    #[tokio::test]
    async fn fetch_verified_refuses_a_wrong_hash_and_keeps_nothing() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                    )
                    .await;
            }
        });
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("f.bin");
        let client = http_client("test").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let url = format!("http://{addr}/f");
        // More bytes than announced are refused while streaming.
        let err = fetch_verified(&client, &url, &dest, 3, &"0".repeat(64), &cancel, &|_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("超過"), "{err}");
        let bad = "0".repeat(64);
        let err = fetch_verified(&client, &url, &dest, 5, &bad, &cancel, &|_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("sha256"), "{err}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        let good = hex_lower(&Sha256::digest(b"hello"));
        fetch_verified(&client, &url, &dest, 5, &good, &cancel, &|_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }
}

#[cfg(test)]
#[path = "install_source_tests.rs"]
mod source_tests;
