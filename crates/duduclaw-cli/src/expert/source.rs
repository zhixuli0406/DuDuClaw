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
    let unpacked = |dir: PathBuf, zip: safe_zip::ExtractSummary| ResolvedSource { dir, zip: Some(zip) };
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
        let (_entry, archive) = super::registry::fetch_verified_archive(slug.trim()).await?;
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        std::fs::write(&zip_path, &archive).map_err(|e| io_err(format!("寫入下載檔失敗: {e}")))?;
        let unpack = staging.join("unpacked");
        let z = safe_zip::extract_to(&zip_path, &unpack)?;
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
        return Ok(ResolvedSource { dir: path, zip: None });
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if SINGLE_FILE_EXTS.contains(&ext.as_str()) {
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
    Ok(ResolvedSource { dir, zip: None })
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

