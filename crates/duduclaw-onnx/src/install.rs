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

use crate::manifest::{self, ArchiveKind, ORT_VERSION, OrtArchive};
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

/// HTTP client for pinned downloads: a connect timeout, no overall timeout
/// (a large file on a slow link is not an error).
pub fn http_client(user_agent: &str) -> OnnxResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
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
    on_bytes(expect_size);
    Ok(())
}

/// Pull exactly one member out of the ORT release archive.
pub fn extract_member(archive_path: &Path, archive: &OrtArchive, dest: &Path) -> OnnxResult<()> {
    let tmp = dest.with_extension("part");
    let _ = std::fs::remove_file(&tmp);
    let found = match archive.kind {
        ArchiveKind::TarGz => {
            let f = std::fs::File::open(archive_path)?;
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(f));
            let mut hit = false;
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.to_string_lossy().to_string();
                if path == archive.member {
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
            match zip.by_name(archive.member) {
                Ok(mut entry) => {
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
            archive.member
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
        manifest::ort_lib_pin(target),
    ) else {
        return Err(format!("這個平台（{target}）沒有可用的 ONNX Runtime"));
    };
    verify_lib_at(&lib, pin)
}

fn verify_lib_at(lib: &Path, (size, sha): (u64, &str)) -> Result<(), String> {
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
    match verify_path(lib, "onnxruntime", size, sha) {
        FileState::Verified => {
            cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key);
            Ok(())
        }
        FileState::Missing => Err("ONNX Runtime 執行庫尚未安裝".to_string()),
        FileState::Corrupt(_) => Err(
            "ONNX Runtime 執行庫與這個 DuDuClaw 的版本紀錄不符，拒絕載入；請移除後重新安裝"
                .to_string(),
        ),
    }
}

fn lib_pin(archive: &OrtArchive) -> OnnxResult<(u64, &'static str)> {
    manifest::ort_lib_pin(archive.target).ok_or_else(|| {
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
    let tmp_archive = ort_archive_path(ort_lib_root, archive);
    fetch_verified(
        client,
        archive.url,
        &tmp_archive,
        archive.size,
        archive.sha256,
        cancel,
        on_bytes,
    )
    .await?;
    extract_verified(&tmp_archive, archive, &lib_path, pin)?;
    let _ = std::fs::remove_file(&tmp_archive);
    Ok(())
}

/// Extract the member, then check it against its pin (a mismatch removes it).
fn extract_verified(
    archive_path: &Path,
    archive: &OrtArchive,
    lib_path: &Path,
    pin: (u64, &str),
) -> OnnxResult<()> {
    let _ = std::fs::remove_file(lib_path);
    extract_member(archive_path, archive, lib_path)?;
    if let Err(e) = verify_lib_at(lib_path, pin) {
        let _ = std::fs::remove_file(lib_path);
        return Err(OnnxError::msg(e));
    }
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
    pin: (u64, &str),
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
    extract_verified(&path, archive, &lib_path, pin)?;
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
        };
        let pin = (
            3u64,
            Box::leak(hex_lower(&Sha256::digest(b"ELF")).into_boxed_str()) as &str,
        );
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
        let wrong_pin = (3u64, "0".repeat(64).leak() as &str);
        std::fs::write(ort_archive_path(&root, &archive), &bytes).unwrap();
        assert!(install_ort_from_local_archive_with(&root, &archive, wrong_pin).is_err());
        assert!(!lib.exists(), "a mismatching library is removed");
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
