//! Source resolution for every pack install: a directory, a local `.zip`,
//! a single `.md` / `.yml` agent definition, an `http(s)` zip, `registry:`
//! and `github:` sources — all through the fenced extractor
//! ([`super::safe_zip`]). Split out of `install.rs` (file-size ceiling).

use std::path::{Path, PathBuf};

use duduclaw_core::error::Result;

use serde_json::json;

use super::import_error::import_err;
use super::{io_err, safe_zip};

/// A resolved source: the directory to read, and what extraction left out
/// when it came from a zip.
pub(crate) struct ResolvedSource {
    pub dir: PathBuf,
    pub zip: Option<safe_zip::ExtractSummary>,
    pub original_dir: Option<PathBuf>,
}

/// Resolve `source` to a directory containing the pack. Handles a plain
/// directory, a local `.zip`, and an `http(s)` URL to a `.zip`.
///
/// `pub(crate)` so the unified `duduclaw pack` front door
/// ([`crate::pack_cmd::install_pack`]) resolves a source through this exact
/// fenced path (zip-slip guard, size cap, registry sha256 + minisign,
/// `github:` side-door warning) instead of growing a second resolver.
pub(crate) async fn resolve_source(source: &str, staging: &Path) -> Result<PathBuf> {
    resolve_source_full(source, staging).await.map(|r| r.dir)
}

/// Single-file agent definitions accepted without a zip: a subagent /
/// instructions `.md` or a Dify `.yml` / `.yaml` export.
const SINGLE_FILE_EXTS: &[&str] = &["md", "yml", "yaml"];

pub(crate) async fn resolve_source_full(source: &str, staging: &Path) -> Result<ResolvedSource> {
    private_staging(staging)?;
    let unpacked = |dir: PathBuf, zip: safe_zip::ExtractSummary| ResolvedSource { dir, zip: Some(zip), original_dir: None };
    // WP2.5: BRAT-style side door — `github:user/repo[@branch]` installs the
    // repo's default-branch archive directly. Unregistered and unreviewed;
    // say so honestly, then let the normal fences (zip fence, content scan,
    // hook quarantine) do their job.
    if let Some(spec) = source.strip_prefix("github:") {
        let (url, label) = super::registry::github_archive_url(spec.trim())?;
        println!(
            "  ⚠ 側門安裝 {label}：未經 registry 驗證，來源風險自負（掃描與 hooks 隔離照常生效）"
        );
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        download_zip(&url, &zip_path).await?;
        let unpack = staging.join("unpacked");
        let z = safe_zip::extract_to(&zip_path, &unpack)?;
        return Ok(unpacked(unpack, z));
    }
    // WP2.2 R2/R3: `registry:<slug>` — resolve via the pack registry with
    // client-side sha256 (always) + minisign (code lane) verification.
    if let Some(slug) = source.strip_prefix("registry:") {
        let (mut entry, archive, signature) = super::registry::fetch_verified_archive(slug.trim()).await?;
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        std::fs::write(&zip_path, &archive).map_err(|e| io_err(format!("寫入下載檔失敗: {e}")))?;
        let unpack = staging.join("unpacked");
        let z = safe_zip::extract_to(&zip_path, &unpack)?;
        // A compromised index cannot hide code by clearing its contains flags.
        if super::registry::extracted_code_lane(&unpack)? {
            entry.contains.skills = true;
            super::registry::verify_registry_signature(&entry, &archive, signature.as_deref(),
                &duduclaw_core::publishers::TrustStore::load(&crate::duduclaw_home()))?;
        }
        return Ok(unpacked(unpack, z));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        download_zip(source, &zip_path).await?;
        let unpack = staging.join("unpacked");
        let z = safe_zip::extract_to(&zip_path, &unpack)?;
        return Ok(unpacked(unpack, z));
    }

    let path = PathBuf::from(source);
    if !path.exists() {
        return Err(import_err("import.source_missing", json!({}), format!("來源不存在: {source}")));
    }
    if path.is_dir() {
        if std::fs::symlink_metadata(&path).map_err(|e| io_err(e.to_string()))?.file_type().is_symlink() {
            return Err(super::cfg_err("pack source contains a symlink".into()));
        }
        let path = path.canonicalize().map_err(|e| io_err(e.to_string()))?;
        let basename = path.file_name().ok_or_else(|| super::cfg_err("pack source needs a directory name".into()))?;
        let snapshot = staging.join("snapshot").join(basename);
        snapshot_directory(&path, &snapshot)?;
        return Ok(ResolvedSource { dir: snapshot, zip: None, original_dir: Some(path) });
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let native_profile = super::agent_config::detect::has_native_header(&path);
    if SINGLE_FILE_EXTS.contains(&ext.as_str()) || native_profile {
        return stage_single_file(&path, staging);
    }
    // A file → must be a zip.
    if ext != "zip" {
        return Err(import_err(
            "import.bad_extension",
            json!({}),
            format!("來源檔不是資料夾，也不是 .zip / .md / .yml 檔: {source}"),
        ));
    }
    std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
    let unpack = staging.join("unpacked");
    let z = safe_zip::extract_to(&path, &unpack)?;
    Ok(unpacked(unpack, z))
}

/// Copy one `.md` / `.yml` file into a fresh staging dir so detection sees a
/// directory. Same size ceiling as an archive; the original name is kept
/// (it carries the Copilot `.agent.md` suffix).
fn stage_single_file(path: &Path, staging: &Path) -> Result<ResolvedSource> {
    let len = std::fs::metadata(path)
        .map_err(|e| io_err(format!("讀取 {} 失敗: {e}", path.display())))?
        .len();
    if len > safe_zip::MAX_UNPACK_BYTES {
        return Err(safe_zip::too_large(format!("檔案過大（{len} bytes）")));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agent.md".into());
    // Dashboard uploads are staged as `<uuid>-<name>`; drop that prefix.
    let name = match (name.get(..36), name.get(36..37), name.get(37..)) {
        (Some(head), Some("-"), Some(rest)) if !rest.is_empty() && uuid::Uuid::parse_str(head).is_ok() => {
            rest.to_string()
        }
        _ => name,
    };
    let dir = staging.join("unpacked");
    std::fs::create_dir_all(&dir).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
    std::fs::copy(path, dir.join(&name)).map_err(|e| io_err(format!("複製來源檔失敗: {e}")))?;
    Ok(ResolvedSource { dir, zip: None, original_dir: None })
}

/// Download a URL to `dest`, capped at [`safe_zip::MAX_UNPACK_BYTES`].
async fn download_zip(url: &str, dest: &Path) -> Result<()> {
    let failed = |msg: String| import_err("import.download_failed", json!({}), msg);
    let resp = reqwest::get(url)
        .await
        .map_err(|e| failed(format!("下載失敗: {e}")))?;
    if !resp.status().is_success() {
        return Err(failed(format!("下載失敗 HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| failed(format!("讀取回應失敗: {e}")))?;
    if bytes.len() as u64 > safe_zip::MAX_UNPACK_BYTES {
        return Err(safe_zip::too_large(format!(
            "下載內容過大（{} bytes > 上限 {}）",
            bytes.len(),
            safe_zip::MAX_UNPACK_BYTES
        )));
    }
    std::fs::write(dest, &bytes).map_err(|e| io_err(format!("寫入下載檔失敗: {e}")))?;
    Ok(())
}



/// Keep the verified bytes independent of later edits to the caller's source tree.
fn private_staging(staging: &Path) -> Result<()> {
    std::fs::create_dir_all(staging).map_err(|e| io_err(e.to_string()))?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staging, std::fs::Permissions::from_mode(0o700)).map_err(|e| io_err(e.to_string()))?;
    }
    Ok(())
}
fn snapshot_directory(source: &Path, destination: &Path) -> Result<()> {
    use std::io::{Read, Write};
    fn copy(source: &Path, dest: &Path, total: &mut u64, count: &mut usize, depth: usize) -> Result<()> {
        if depth > 64 { return Err(super::cfg_err("pack source nesting exceeds limit".into())); }
        let meta = std::fs::symlink_metadata(source).map_err(|e| io_err(e.to_string()))?;
        if meta.file_type().is_symlink() || !meta.is_dir() { return Err(super::cfg_err("pack source contains a symlink or special directory".into())); }
        std::fs::create_dir_all(dest).map_err(|e| io_err(e.to_string()))?;
        for entry in std::fs::read_dir(source).map_err(|e| io_err(e.to_string()))? {
            let entry = entry.map_err(|e| io_err(e.to_string()))?;
            *count += 1;
            if *count > 10_000 { return Err(super::cfg_err("pack source exceeds file limit".into())); }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| io_err(e.to_string()))?;
            if meta.file_type().is_symlink() { return Err(super::cfg_err("pack source contains a symlink".into())); }
            let target = dest.join(entry.file_name());
            if meta.is_dir() { copy(&path, &target, total, count, depth + 1)?; continue; }
            if !meta.is_file() { return Err(super::cfg_err("pack source contains a special file".into())); }
            let remaining = safe_zip::MAX_UNPACK_BYTES.saturating_sub(*total);
            if meta.len() > remaining { return Err(super::cfg_err("pack source exceeds size limit".into())); }
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)] {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options.open(&path).map_err(|e| io_err(e.to_string()))?;
            if !file.metadata().map_err(|e| io_err(e.to_string()))?.is_file() { return Err(super::cfg_err("pack source changed file type".into())); }
            let mut bytes = Vec::new();
            file.take(remaining + 1).read_to_end(&mut bytes).map_err(|e| io_err(e.to_string()))?;
            if bytes.len() as u64 > remaining { return Err(super::cfg_err("pack source exceeds size limit".into())); }
            *total += bytes.len() as u64;
            let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(target).map_err(|e| io_err(e.to_string()))?;
            output.write_all(&bytes).map_err(|e| io_err(e.to_string()))?;
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                // Preserve ordinary script permissions, never privileged mode bits.
                output.set_permissions(std::fs::Permissions::from_mode(meta.permissions().mode() & 0o777))
                    .map_err(|e| io_err(e.to_string()))?;
            }
        }
        Ok(())
    }
    copy(source, destination, &mut 0, &mut 0, 0)
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[tokio::test]
    async fn local_pack_snapshot_survives_source_changes() {
        let source = tempfile::tempdir().unwrap();
        let staging_parent = tempfile::tempdir().unwrap();
        let staging = staging_parent.path().join("private");
        std::fs::write(source.path().join("pack.toml"), "original").unwrap();
        let resolved = resolve_source_full(source.path().to_str().unwrap(), &staging).await.unwrap();
        std::fs::write(source.path().join("pack.toml"), "changed after verification").unwrap();
        assert_eq!(std::fs::read_to_string(resolved.dir.join("pack.toml")).unwrap(), "original");
        assert_eq!(resolved.original_dir, Some(source.path().canonicalize().unwrap()));
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&staging).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }
    #[cfg(unix)]
    #[test]
    fn snapshot_refuses_symlinks() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", source.path().join("link")).unwrap();
        assert!(snapshot_directory(source.path(), &destination.path().join("copy")).is_err());
    }
}
