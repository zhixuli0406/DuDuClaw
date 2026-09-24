//! Download, verify and install the NER model + the ONNX Runtime library.
//!
//! Every byte is pinned in [`super::manifest`]. The install is:
//!
//! 1. stream each file to `<dest>.part`, resuming with an HTTP `Range`
//!    header when a partial file is already there;
//! 2. hash the completed `.part` and compare against the manifest;
//! 3. `rename` it into place — so a file at its final path is always a
//!    verified file, never a truncated download;
//! 4. write [`manifest::INSTALL_MARKER`] only after **all** files passed.
//!
//! A mismatch deletes the offending `.part` and fails the whole install. The
//! rule reads the marker, so a partial install is indistinguishable from no
//! install: fail-closed by construction, never "half the model".

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use super::manifest::{
    self, ArchiveKind, INSTALL_MARKER, InstallMarker, MODEL_FILES, MODEL_REVISION, ORT_VERSION,
    OrtArchive, RemoteFile,
};
use crate::error::{RedactionError, Result};

pub use super::InstallDirs;

/// Progress of one file within the install.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Progress {
    /// Which artefact is moving (`onnx/model_q4.onnx_data`, `onnxruntime`).
    pub file: String,
    /// Bytes of the whole install completed so far.
    pub done_bytes: u64,
    /// Bytes the whole install will move.
    pub total_bytes: u64,
}

/// Progress sink. Called often — must be cheap and must not block.
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Lock sentinel preventing two processes installing at once.
const LOCK_FILE: &str = "install.lock";
/// A lock older than this is assumed to belong to a crashed install.
const LOCK_STALE_SECS: u64 = 6 * 60 * 60;

/// SHA-256 of a file, lowercase hex. Streams — never loads the file.
pub fn sha256_file(path: &Path) -> Result<String> {
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

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Result of checking one artefact against the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// Present with the right size and hash.
    Verified,
    /// Absent entirely.
    Missing,
    /// Present but the wrong size or hash — must be re-downloaded.
    Corrupt(String),
}

/// Full verification of one pinned file (hashes the whole thing).
pub fn verify_file(dir: &Path, spec: &RemoteFile) -> FileState {
    let path = spec.dest(dir);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return FileState::Missing,
    };
    if meta.len() != spec.size {
        return FileState::Corrupt(format!(
            "{}: expected {} bytes, found {}",
            spec.rel_path,
            spec.size,
            meta.len()
        ));
    }
    match sha256_file(&path) {
        Ok(got) if got == spec.sha256 => FileState::Verified,
        Ok(got) => FileState::Corrupt(format!(
            "{}: sha256 mismatch (expected {}, got {got})",
            spec.rel_path, spec.sha256
        )),
        Err(e) => FileState::Corrupt(format!("{}: cannot hash: {e}", spec.rel_path)),
    }
}

/// Verify every file in `specs`. `Err` names the first failure.
///
/// Takes the spec list as a parameter (rather than reading [`MODEL_FILES`]
/// directly) so the behaviour is testable without a gigabyte on disk.
pub fn verify_all(dir: &Path, specs: &[RemoteFile]) -> std::result::Result<(), String> {
    for spec in specs {
        match verify_file(dir, spec) {
            FileState::Verified => {}
            FileState::Missing => return Err(format!("{} is missing", spec.rel_path)),
            FileState::Corrupt(why) => return Err(why),
        }
    }
    Ok(())
}

/// Read the install marker, if a complete install is recorded.
pub fn read_marker(model_dir: &Path) -> Option<InstallMarker> {
    let raw = std::fs::read_to_string(model_dir.join(INSTALL_MARKER)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Cheap installed-check used on every rule compile: the marker must name
/// this build's pinned revision, every model file must exist at the pinned
/// size, and the ONNX Runtime library must be present.
///
/// Deliberately does NOT re-hash a gigabyte — hashing happens at install
/// time and is recorded in the marker. A file swapped underneath us by
/// something with write access to the model directory is out of scope; that
/// actor can equally edit `config.toml`.
pub fn is_installed(dirs: &InstallDirs) -> bool {
    installed_problem(dirs).is_none()
}

/// The reason [`is_installed`] would say no, or `None` when it is installed.
pub fn installed_problem(dirs: &InstallDirs) -> Option<String> {
    let Some(marker) = read_marker(&dirs.model_dir) else {
        return Some("模型尚未安裝".to_string());
    };
    if marker.model_revision != MODEL_REVISION {
        return Some(format!(
            "已安裝的模型版本是 {}，這個版本的 DuDuClaw 需要 {MODEL_REVISION}",
            marker.model_revision
        ));
    }
    for spec in MODEL_FILES {
        let path = spec.dest(&dirs.model_dir);
        match std::fs::metadata(&path) {
            Ok(m) if m.len() == spec.size => {}
            Ok(m) => {
                return Some(format!(
                    "模型檔 {} 大小不符（預期 {} 位元組，實際 {}）",
                    spec.rel_path,
                    spec.size,
                    m.len()
                ));
            }
            Err(_) => return Some(format!("模型檔 {} 不存在", spec.rel_path)),
        }
    }
    let Some(lib) = manifest::ort_lib_path(&dirs.ort_lib_root) else {
        return Some(format!(
            "這個平台（{}）沒有對應的 ONNX Runtime 版本，AI 智慧偵測無法使用",
            manifest::current_target()
        ));
    };
    if !lib.exists() {
        return Some("ONNX Runtime 執行庫尚未安裝".to_string());
    }
    None
}

/// Total bytes an install will move on this platform.
pub fn total_install_bytes() -> u64 {
    manifest::model_total_bytes()
        + manifest::ort_archive_for_current_platform()
            .map(|a| a.size)
            .unwrap_or(0)
}

/// Install everything. Idempotent: an already-verified file is skipped, a
/// partially downloaded one resumes.
///
/// `cancel` is polled between chunks; cancelling leaves the `.part` files in
/// place so a later install resumes rather than restarts.
pub async fn install(
    dirs: &InstallDirs,
    progress: ProgressFn,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let Some(archive) = manifest::ort_archive_for_current_platform() else {
        return Err(RedactionError::config(format!(
            "AI 智慧偵測在這個平台（{}）沒有可用的 ONNX Runtime 版本",
            manifest::current_target()
        )));
    };

    std::fs::create_dir_all(&dirs.model_dir)?;
    let _lock = InstallLock::acquire(&dirs.model_dir)?;

    let total = total_install_bytes();
    let mut done: u64 = 0;
    let client = http_client()?;
    let mut hashes = std::collections::BTreeMap::new();

    for spec in MODEL_FILES {
        let dest = spec.dest(&dirs.model_dir);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Already verified? Skip the bytes but still count them so the bar is
        // honest about overall position.
        if std::fs::metadata(&dest).map(|m| m.len()).ok() == Some(spec.size) {
            hashes.insert(spec.rel_path.to_string(), spec.sha256.to_string());
            done += spec.size;
            progress(Progress {
                file: spec.rel_path.to_string(),
                done_bytes: done,
                total_bytes: total,
            });
            continue;
        }

        let base = done;
        fetch_verified(
            &client,
            &spec.url(),
            &dest,
            spec.size,
            spec.sha256,
            &cancel,
            &|got| {
                progress(Progress {
                    file: spec.rel_path.to_string(),
                    done_bytes: base + got,
                    total_bytes: total,
                });
            },
        )
        .await?;
        hashes.insert(spec.rel_path.to_string(), spec.sha256.to_string());
        done += spec.size;
    }

    install_ort(&client, dirs, archive, &cancel, total, &mut done, &progress).await?;
    hashes.insert("onnxruntime".to_string(), archive.sha256.to_string());

    write_marker(&dirs.model_dir, hashes)?;
    progress(Progress {
        file: "done".to_string(),
        done_bytes: total,
        total_bytes: total,
    });
    Ok(())
}

fn write_marker(
    model_dir: &Path,
    files: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let marker = InstallMarker {
        model_revision: MODEL_REVISION.to_string(),
        ort_version: ORT_VERSION.to_string(),
        verified_at: chrono::Utc::now().to_rfc3339(),
        files,
    };
    let body = serde_json::to_string_pretty(&marker)?;
    let path = model_dir.join(INSTALL_MARKER);
    // Cross-process advisory lock: the marker is the one file another
    // DuDuClaw process reads to decide whether the model is usable.
    duduclaw_core::with_file_lock(&path, || {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body.as_bytes())?;
        std::fs::rename(&tmp, &path)
    })?;
    Ok(())
}

async fn install_ort(
    client: &reqwest::Client,
    dirs: &InstallDirs,
    archive: &OrtArchive,
    cancel: &Arc<AtomicBool>,
    total: u64,
    done: &mut u64,
    progress: &ProgressFn,
) -> Result<()> {
    let lib_dir = manifest::ort_lib_dir(&dirs.ort_lib_root);
    let lib_path = lib_dir.join(archive.lib_file);
    if lib_path.exists() {
        *done += archive.size;
        progress(Progress {
            file: "onnxruntime".to_string(),
            done_bytes: *done,
            total_bytes: total,
        });
        return Ok(());
    }
    std::fs::create_dir_all(&lib_dir)?;

    let tmp_archive = lib_dir.join(format!(
        "onnxruntime-{ORT_VERSION}.{}",
        match archive.kind {
            ArchiveKind::TarGz => "tgz",
            ArchiveKind::Zip => "zip",
        }
    ));
    let base = *done;
    fetch_verified(
        client,
        archive.url,
        &tmp_archive,
        archive.size,
        archive.sha256,
        cancel,
        &|got| {
            progress(Progress {
                file: "onnxruntime".to_string(),
                done_bytes: base + got,
                total_bytes: total,
            });
        },
    )
    .await?;

    extract_member(&tmp_archive, archive, &lib_path)?;
    let _ = std::fs::remove_file(&tmp_archive);
    *done += archive.size;
    Ok(())
}

/// Pull exactly one member out of the ORT release archive.
pub fn extract_member(archive_path: &Path, archive: &OrtArchive, dest: &Path) -> Result<()> {
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
                .map_err(|e| RedactionError::config(format!("ONNX Runtime zip 無法讀取：{e}")))?;
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
        return Err(RedactionError::config(format!(
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

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        // No global timeout: a 917 MB file on a slow link is not an error.
        .user_agent(concat!("duduclaw-redaction/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| RedactionError::config(format!("HTTP client: {e}")))
}

/// Stream `url` into `<dest>.part`, resuming from whatever is already there,
/// verify the hash, then rename into place.
#[allow(clippy::too_many_arguments)]
async fn fetch_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expect_size: u64,
    expect_sha: &str,
    cancel: &Arc<AtomicBool>,
    on_bytes: &(dyn Fn(u64) + Send + Sync),
) -> Result<()> {
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
            .map_err(|e| RedactionError::config(format!("下載失敗：{e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RedactionError::config(format!(
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
                return Err(RedactionError::config("下載已取消".to_string()));
            }
            let chunk = chunk.map_err(|e| RedactionError::config(format!("下載中斷：{e}")))?;
            file.write_all(&chunk)?;
            written += chunk.len() as u64;
            on_bytes(written);
        }
        file.flush()?;
        drop(file);
    }

    let got_size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if got_size != expect_size {
        let _ = std::fs::remove_file(&part);
        return Err(RedactionError::config(format!(
            "下載檔大小不符（預期 {expect_size} 位元組，實際 {got_size}），已刪除重來"
        )));
    }
    let got = sha256_file(&part)?;
    if got != expect_sha {
        let _ = std::fs::remove_file(&part);
        return Err(RedactionError::config(
            "下載檔完整性驗證失敗（sha256 不符），已刪除；請重新下載".to_string(),
        ));
    }
    std::fs::rename(&part, dest)?;
    on_bytes(expect_size);
    Ok(())
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

/// Delete the model files (and the marker). The ONNX Runtime library is
/// deliberately left alone — it is version-shared, tens of MB, and not the
/// thing an operator is reclaiming space for.
pub fn remove_model(model_dir: &Path) -> Result<()> {
    let marker = model_dir.join(INSTALL_MARKER);
    if marker.exists() {
        // Remove the marker FIRST: if anything below fails, the install is
        // already "not installed" rather than "half present but believed OK".
        std::fs::remove_file(&marker)?;
    }
    for spec in MODEL_FILES {
        let p = spec.dest(model_dir);
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
        let part = part_path(&p);
        if part.exists() {
            std::fs::remove_file(&part)?;
        }
    }
    // Clean the now-empty `onnx/` subdirectory; ignore failure (it may hold
    // a variant the operator downloaded by hand).
    let _ = std::fs::remove_dir(model_dir.join("onnx"));
    Ok(())
}

/// Cross-process install lock, released on drop.
struct InstallLock {
    path: PathBuf,
}

impl InstallLock {
    fn acquire(model_dir: &Path) -> Result<Self> {
        let path = model_dir.join(LOCK_FILE);
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
                return Err(RedactionError::config(
                    "另一個安裝程序正在下載模型，請稍候".to_string(),
                ));
            }
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                RedactionError::config(format!("無法建立安裝鎖 {}：{e}", path.display()))
            })?;
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

    fn spec_for(rel: &str, body: &[u8]) -> RemoteFile {
        // Leaked so the 'static manifest shape holds in a test; one tiny
        // allocation per test case.
        let sha: &'static str = Box::leak(hex_lower(&Sha256::digest(body)).into_boxed_str());
        let rel: &'static str = Box::leak(rel.to_string().into_boxed_str());
        RemoteFile {
            rel_path: rel,
            size: body.len() as u64,
            sha256: sha,
        }
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
    fn verify_reports_missing_wrong_size_and_wrong_hash_distinctly() {
        let tmp = TempDir::new().unwrap();
        let spec = spec_for("a.bin", b"hello world");

        assert_eq!(verify_file(tmp.path(), &spec), FileState::Missing);

        std::fs::write(spec.dest(tmp.path()), b"hello").unwrap();
        match verify_file(tmp.path(), &spec) {
            FileState::Corrupt(m) => assert!(m.contains("bytes"), "{m}"),
            other => panic!("expected size complaint, got {other:?}"),
        }

        std::fs::write(spec.dest(tmp.path()), b"HELLO WORLD").unwrap();
        match verify_file(tmp.path(), &spec) {
            FileState::Corrupt(m) => assert!(m.contains("sha256"), "{m}"),
            other => panic!("expected hash complaint, got {other:?}"),
        }

        std::fs::write(spec.dest(tmp.path()), b"hello world").unwrap();
        assert_eq!(verify_file(tmp.path(), &spec), FileState::Verified);
    }

    #[test]
    fn verify_all_fails_the_whole_set_on_one_bad_file() {
        let tmp = TempDir::new().unwrap();
        let a = spec_for("a.bin", b"aaa");
        let b = spec_for("b.bin", b"bbb");
        std::fs::write(a.dest(tmp.path()), b"aaa").unwrap();
        std::fs::write(b.dest(tmp.path()), b"xxx").unwrap();
        let err = verify_all(tmp.path(), &[a, b]).unwrap_err();
        assert!(err.contains("b.bin"), "{err}");
    }

    #[test]
    fn verify_all_passes_when_every_file_matches() {
        let tmp = TempDir::new().unwrap();
        let a = spec_for("a.bin", b"aaa");
        let b = spec_for("nested/b.bin", b"bbb");
        std::fs::create_dir_all(tmp.path().join("nested")).unwrap();
        std::fs::write(a.dest(tmp.path()), b"aaa").unwrap();
        std::fs::write(b.dest(tmp.path()), b"bbb").unwrap();
        assert!(verify_all(tmp.path(), &[a, b]).is_ok());
    }

    #[test]
    fn not_installed_without_a_marker() {
        let tmp = TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        assert!(!is_installed(&dirs));
        assert!(installed_problem(&dirs).unwrap().contains("尚未安裝"));
    }

    #[test]
    fn a_marker_from_another_revision_is_not_installed() {
        let tmp = TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        std::fs::create_dir_all(&dirs.model_dir).unwrap();
        std::fs::write(
            dirs.model_dir.join(INSTALL_MARKER),
            serde_json::to_string(&InstallMarker {
                model_revision: "deadbeef".into(),
                ort_version: ORT_VERSION.into(),
                verified_at: "2026-01-01T00:00:00Z".into(),
                files: Default::default(),
            })
            .unwrap(),
        )
        .unwrap();
        let problem = installed_problem(&dirs).unwrap();
        assert!(problem.contains("deadbeef"), "{problem}");
    }

    #[test]
    fn a_correct_marker_with_missing_files_is_still_not_installed() {
        let tmp = TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        std::fs::create_dir_all(&dirs.model_dir).unwrap();
        std::fs::write(
            dirs.model_dir.join(INSTALL_MARKER),
            serde_json::to_string(&InstallMarker {
                model_revision: MODEL_REVISION.into(),
                ort_version: ORT_VERSION.into(),
                verified_at: "2026-01-01T00:00:00Z".into(),
                files: Default::default(),
            })
            .unwrap(),
        )
        .unwrap();
        let problem = installed_problem(&dirs).unwrap();
        assert!(problem.contains("不存在") || problem.contains("大小不符"), "{problem}");
    }

    #[test]
    fn part_path_is_a_sibling() {
        let p = part_path(Path::new("/a/b/model.onnx"));
        assert_eq!(p, PathBuf::from("/a/b/model.onnx.part"));
    }

    #[test]
    fn install_lock_is_exclusive_and_released_on_drop() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let first = InstallLock::acquire(dir).unwrap();
        assert!(InstallLock::acquire(dir).is_err(), "second lock must fail");
        drop(first);
        assert!(InstallLock::acquire(dir).is_ok(), "lock must free on drop");
    }

    #[test]
    fn remove_model_drops_the_marker_first() {
        let tmp = TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        std::fs::create_dir_all(dirs.model_dir.join("onnx")).unwrap();
        std::fs::write(dirs.model_dir.join(INSTALL_MARKER), "{}").unwrap();
        std::fs::write(dirs.model_dir.join("config.json"), "{}").unwrap();
        remove_model(&dirs.model_dir).unwrap();
        assert!(!dirs.model_dir.join(INSTALL_MARKER).exists());
        assert!(!dirs.model_dir.join("config.json").exists());
    }

    #[test]
    fn remove_model_on_a_clean_dir_is_not_an_error() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        assert!(remove_model(tmp.path()).is_ok());
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

    fn zip_with(member: &str, body: &[u8], at: &Path) {
        use std::io::Write as _;
        let f = std::fs::File::create(at).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file(member, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(body).unwrap();
        z.finish().unwrap();
    }

    fn archive_spec(kind: ArchiveKind, member: &'static str) -> OrtArchive {
        OrtArchive {
            target: "test",
            url: "https://example.invalid/x",
            size: 1,
            sha256: "00",
            kind,
            member,
            lib_file: "lib.out",
        }
    }

    #[test]
    fn extract_member_pulls_exactly_one_file_out_of_a_tgz() {
        let tmp = TempDir::new().unwrap();
        let arch = tmp.path().join("a.tgz");
        tgz_with("root/lib/libonnxruntime.so.1.24.2", b"ELF-ish", &arch);
        let spec = archive_spec(ArchiveKind::TarGz, "root/lib/libonnxruntime.so.1.24.2");
        let dest = tmp.path().join("libonnxruntime.so");
        extract_member(&arch, &spec, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ELF-ish");
        assert!(!part_path(&dest).exists(), "temp file must be renamed away");
    }

    #[test]
    fn extract_member_pulls_exactly_one_file_out_of_a_zip() {
        let tmp = TempDir::new().unwrap();
        let arch = tmp.path().join("a.zip");
        zip_with("root/lib/onnxruntime.dll", b"MZ-ish", &arch);
        let spec = archive_spec(ArchiveKind::Zip, "root/lib/onnxruntime.dll");
        let dest = tmp.path().join("onnxruntime.dll");
        extract_member(&arch, &spec, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ-ish");
    }

    #[test]
    fn extract_member_fails_loudly_when_the_pinned_path_moved() {
        let tmp = TempDir::new().unwrap();
        let arch = tmp.path().join("a.tgz");
        tgz_with("root/lib/somewhere-else.so", b"x", &arch);
        let spec = archive_spec(ArchiveKind::TarGz, "root/lib/libonnxruntime.so.1.24.2");
        let dest = tmp.path().join("out.so");
        let err = extract_member(&arch, &spec, &dest).unwrap_err();
        assert!(format!("{err}").contains("libonnxruntime.so.1.24.2"), "{err}");
        assert!(!dest.exists(), "a failed extraction must leave nothing behind");
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn total_install_bytes_covers_model_plus_runtime() {
        let total = total_install_bytes();
        assert!(total >= manifest::model_total_bytes());
    }
}
