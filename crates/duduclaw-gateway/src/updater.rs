//! Self-update module: check GitHub releases, download, verify, and replace the running binary.
//!
//! Security hardening (code review round 1 + round 2):
//! - [C1] SHA-256 checksum verification (hard-fail if unavailable) [R2:NH2]
//! - [C2] AtomicBool concurrency lock — only one update at a time
//! - [C3] Windows .zip extraction with `duduclaw.exe` target
//! - [H2] URL validation inside `apply_update` (both download + checksum URLs) [R2:NC1]
//! - [H3] CSPRNG random temp file name + directory permission check [R2:NH1]
//! - [H4] Cleanup `.bak` after successful update
//! - [H5] 200 MB download + decompression size limit [R2:NC2]
//! - [M1] release_notes truncated to 8 KB
//! - [M3] Pre-release tag tolerance in semver comparison
//! - [M4] All filesystem ops use `tokio::fs` (async)
//! - [R2:NH3] Reject absolute paths, symlinks, hard links in tar/zip
//! - [R2:NH4] Cleanup tmp on set_permissions failure
//! - [R2:NM2] Redirect policy with URL re-validation
//! - [R2:NL1] Binary verification timeout (10s)

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

const GITHUB_REPO_COMMUNITY: &str = "zhixuli0406/DuDuClaw";
const GITHUB_REPO_PRO: &str = "zhixuli0406/duduclaw-pro-releases";
/// Current version: prefers build-time `DUDUCLAW_VERSION` env (set by Pro build script),
/// then runtime `DUDUCLAW_VERSION` env, finally falls back to this crate's `CARGO_PKG_VERSION`.
pub fn current_version() -> &'static str {
    // 1. Build-time override (Pro binary sets this via build.rs / cargo env)
    if let Some(v) = option_env!("DUDUCLAW_VERSION") {
        return v;
    }
    // 2. Runtime override (set by Pro binary's main() before starting gateway)
    static RUNTIME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let rv = RUNTIME.get_or_init(|| std::env::var("DUDUCLAW_VERSION").ok());
    if let Some(v) = rv.as_deref() {
        return v;
    }
    // 3. Fallback: this crate's own version (= CE version)
    env!("CARGO_PKG_VERSION")
}

/// Allow the Pro binary to set the version at runtime before calling `check_update`.
pub fn set_version_override(version: &str) {
    // SAFETY: called once at startup before any concurrent reads.
    unsafe { std::env::set_var("DUDUCLAW_VERSION", version) };
}
/// Maximum download + decompressed binary size: 200 MB. [H5][R2:NC2]
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
/// Maximum release notes length: 8 KB. [M1]
const MAX_RELEASE_NOTES_CHARS: usize = 8192;
/// Binary verification timeout. [R2:NL1]
const VERIFY_TIMEOUT_SECS: u64 = 10;

/// Global concurrency guard — only one update may run at a time. [C2]
static UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Information about an available update.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_notes: String,
    pub published_at: String,
    pub download_url: String,
    pub checksum_url: String,
    pub install_method: InstallMethod,
}

/// How DuDuClaw was installed — determines upgrade path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstallMethod {
    Homebrew,
    Standalone,
    Source,
    Unknown,
}

/// Result of applying an update.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub success: bool,
    pub message: String,
    pub needs_restart: bool,
}

// ---------------------------------------------------------------------------
// Installation method detection (includes linuxbrew [L2])
// ---------------------------------------------------------------------------

/// Detect whether the running binary is the Pro edition.
///
/// Detection order:
/// 1. `DUDUCLAW_EDITION` env var (set by gateway on startup from extension name)
/// 2. Binary filename contains "duduclaw-pro" (release builds)
pub fn is_pro_edition() -> bool {
    // Env var takes priority — set by gateway startup from GatewayExtension::tier()
    if let Ok(edition) = std::env::var("DUDUCLAW_EDITION") {
        return edition.eq_ignore_ascii_case("pro") || edition.eq_ignore_ascii_case("enterprise");
    }
    // Fallback: check binary filename (works for renamed release binaries)
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().contains("duduclaw-pro")))
        .unwrap_or(false)
}

/// Return the GitHub repo slug for update checking based on edition.
fn github_repo() -> &'static str {
    if is_pro_edition() { GITHUB_REPO_PRO } else { GITHUB_REPO_COMMUNITY }
}

/// Return the Homebrew formula name based on edition.
pub fn brew_formula_name() -> &'static str {
    if is_pro_edition() { "duduclaw-pro" } else { "duduclaw" }
}

pub fn detect_install_method() -> InstallMethod {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return InstallMethod::Unknown,
    };
    let exe_str = exe.to_string_lossy();

    if exe_str.contains("/Cellar/duduclaw-pro/")
        || exe_str.contains("/Cellar/duduclaw/")
        || exe_str.contains("/homebrew/")
        || exe_str.contains("/linuxbrew/")
    {
        return InstallMethod::Homebrew;
    }
    if exe_str.contains("/.cargo/bin/") {
        return InstallMethod::Source;
    }
    if exe_str.contains("/.duduclaw/bin/") {
        return InstallMethod::Standalone;
    }
    if exe_str.contains("/target/release/") || exe_str.contains("/target/debug/") {
        return InstallMethod::Source;
    }
    InstallMethod::Standalone
}

// ---------------------------------------------------------------------------
// Semver comparison [M3] — strips pre-release suffixes
// ---------------------------------------------------------------------------

fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let s = s.strip_prefix('v').unwrap_or(s);
        let parts: Vec<&str> = s.split('.').collect();
        let parse_component = |p: &str| -> u32 {
            p.split('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
        };
        let major = parts.first().map(|p| parse_component(p)).unwrap_or(0);
        let minor = parts.get(1).map(|p| parse_component(p)).unwrap_or(0);
        let patch = parts.get(2).map(|p| parse_component(p)).unwrap_or(0);
        (major, minor, patch)
    };
    parse(latest) > parse(current)
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

fn platform_asset_suffix() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "arm64-apple-darwin.tar.gz" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "x64-apple-darwin.tar.gz" }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "x64-unknown-linux-gnu.tar.gz" }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { "arm64-unknown-linux-gnu.tar.gz" }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    { "x64-pc-windows-msvc.zip" }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    { "unknown" }
}

fn binary_name() -> &'static str {
    if is_pro_edition() {
        #[cfg(target_os = "windows")]
        { return "duduclaw-pro.exe"; }
        #[cfg(not(target_os = "windows"))]
        { return "duduclaw-pro"; }
    }
    #[cfg(target_os = "windows")]
    { "duduclaw.exe" }
    #[cfg(not(target_os = "windows"))]
    { "duduclaw" }
}

// ---------------------------------------------------------------------------
// URL validation [H2][R2:NC1] — validates both download and checksum URLs
// ---------------------------------------------------------------------------

/// Validate that a URL points to the official GitHub release assets.
pub fn is_valid_download_url(url: &str) -> bool {
    let repo = github_repo();
    let prefix = format!("https://github.com/{repo}/releases/download/");
    url.starts_with(&prefix)
        && !url.contains("..")
        && url.len() < 512
}

// ---------------------------------------------------------------------------
// Check for update
// ---------------------------------------------------------------------------

pub async fn check_update() -> Result<UpdateInfo, String> {
    // [R3:M2] Redirect policy for check_update — only allow GitHub API/CDN
    let ver = current_version();
    let ua = format!("{}/{ver}", if is_pro_edition() { "duduclaw-pro" } else { "duduclaw" });
    let client = reqwest::Client::builder()
        .user_agent(&ua)
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().clone();
            let target = url.as_str();
            if target.starts_with("https://api.github.com/")
                || target.starts_with("https://github.com/")
            {
                attempt.follow()
            } else {
                attempt.error(format!("Redirect blocked in check_update: {target}"))
            }
        }))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let repo = github_repo();
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch release info: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }

    let release: GitHubRelease = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse release JSON: {e}"))?;

    let latest = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let available = is_newer(current_version(), latest);
    let install_method = detect_install_method();

    let suffix = platform_asset_suffix();
    let download_url = release
        .assets
        .iter()
        .find(|a| a.name.ends_with(suffix))
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    let checksum_url = if download_url.is_empty() {
        String::new()
    } else {
        format!("{download_url}.sha256")
    };

    let release_notes: String = release
        .body
        .unwrap_or_default()
        .chars()
        .take(MAX_RELEASE_NOTES_CHARS)
        .collect();

    Ok(UpdateInfo {
        available,
        current_version: current_version().to_string(),
        latest_version: latest.to_string(),
        release_notes,
        published_at: release.published_at.unwrap_or_default(),
        download_url,
        checksum_url,
        install_method,
    })
}

// ---------------------------------------------------------------------------
// Apply update
// ---------------------------------------------------------------------------

pub async fn apply_update(download_url: &str, checksum_url: &str) -> Result<ApplyResult, String> {
    // [C2] Concurrency guard
    if UPDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Another update is already in progress".into());
    }
    let _guard = UpdateGuard;

    // Homebrew check
    if detect_install_method() == InstallMethod::Homebrew {
        return Ok(ApplyResult {
            success: false,
            message: format!("Homebrew installation detected. Please run: brew upgrade {}", brew_formula_name()),
            needs_restart: false,
        });
    }

    // [H2][R2:NC1][R3:H1] Validate BOTH URLs — before any download
    if download_url.is_empty() {
        return Err("No download URL available for this platform".into());
    }
    if !is_valid_download_url(download_url) {
        return Err(format!("Rejected unsafe download URL: {download_url}"));
    }
    // [R3:H1] Reject empty checksum_url BEFORE downloading
    if checksum_url.is_empty() {
        return Err("No checksum URL provided — refusing update without integrity verification".into());
    }
    if !is_valid_download_url(checksum_url) {
        return Err(format!("Rejected unsafe checksum URL: {checksum_url}"));
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Cannot determine current binary path: {e}"))?;
    let exe_dir = current_exe
        .parent()
        .ok_or("Cannot determine binary directory")?;

    // [H3] Verify directory permissions
    if duduclaw_core::platform::is_world_writable(exe_dir) {
        return Err("Binary directory is world/group writable — refusing update for safety".into());
    }

    info!(url = download_url, "Downloading update...");

    // [R2:NM2] Custom redirect policy — re-validate redirect targets
    let ver = current_version();
    let ua = format!("{}/{ver}", if is_pro_edition() { "duduclaw-pro" } else { "duduclaw" });
    let client = reqwest::Client::builder()
        .user_agent(&ua)
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().clone();
            let target = url.as_str();
            // Allow GitHub CDN redirects (objects.githubusercontent.com)
            let repo = github_repo();
            let repo_prefix = format!("https://github.com/{repo}/");
            if target.starts_with("https://objects.githubusercontent.com/")
                || target.starts_with(&repo_prefix)
            {
                attempt.follow()
            } else {
                attempt.error(format!("Redirect to non-whitelisted URL blocked: {target}"))
            }
        }))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Download archive
    let resp = client
        .get(download_url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Download returned HTTP {}", resp.status()));
    }

    // [H5] Size limit
    if let Some(len) = resp.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(format!("Download too large: {len} bytes (limit: {MAX_DOWNLOAD_BYTES})"));
        }
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download: {e}"))?;

    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(format!("Download exceeded size limit: {} bytes", bytes.len()));
    }

    info!(size = bytes.len(), "Download complete");

    // [C1][R2:NH2] SHA-256 checksum verification — HARD FAIL
    // (empty checksum_url already rejected at function entry [R3:H1])
    info!("Verifying SHA-256 checksum...");
    let checksum_resp = client
        .get(checksum_url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch checksum file: {e}"))?;

    if !checksum_resp.status().is_success() {
        return Err(format!(
            "Checksum file unavailable (HTTP {}) — refusing update without integrity verification",
            checksum_resp.status()
        ));
    }

    let checksum_text = checksum_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read checksum: {e}"))?;

    let expected = checksum_text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let computed = format!("{:x}", Sha256::digest(&bytes));

    if expected.len() < 64 {
        return Err(format!("Checksum file has invalid format (got {} chars)", expected.len()));
    }
    if computed != expected {
        return Err(format!(
            "SHA-256 checksum mismatch!\n  Expected: {expected}\n  Computed: {computed}"
        ));
    }
    info!("SHA-256 checksum verified");

    // Extract binary
    info!("Extracting binary...");
    let new_binary_bytes = extract_binary_from_archive(&bytes)?;

    // [R2:NC2] Verify decompressed size
    if new_binary_bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "Extracted binary too large: {} bytes (limit: {MAX_DOWNLOAD_BYTES})",
            new_binary_bytes.len()
        ));
    }

    // [H3][R2:NH1] CSPRNG random temp file name
    let random_suffix = uuid::Uuid::new_v4();
    let tmp_path = exe_dir.join(format!(".duduclaw-update-{random_suffix}.tmp"));

    // Write temp file — all cleanup below uses helper to ensure tmp is removed on error
    tokio::fs::write(&tmp_path, &new_binary_bytes)
        .await
        .map_err(|e| format!("Failed to write temp binary: {e}"))?;

    // [R2:NH4] From here on, any error must clean up tmp_path
    let result = apply_update_inner(&tmp_path, &current_exe, exe_dir).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }
    result
}

/// Inner apply logic after tmp file is written. Caller cleans up tmp on error.
async fn apply_update_inner(
    tmp_path: &std::path::Path,
    current_exe: &std::path::Path,
    _exe_dir: &std::path::Path,
) -> Result<ApplyResult, String> {
    // Set executable permissions [M4]
    duduclaw_core::platform::set_executable(tmp_path)
        .map_err(|e| format!("Failed to set permissions: {e}"))?;

    // [R2:NL1] Verify with timeout
    let tmp_for_verify = tmp_path.to_path_buf();
    let verify = tokio::time::timeout(
        std::time::Duration::from_secs(VERIFY_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(&tmp_for_verify)
                .arg("version")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
        }),
    )
    .await
    .map_err(|_| format!("Binary verification timed out after {VERIFY_TIMEOUT_SECS}s"))?
    .map_err(|e| format!("Verification task panicked: {e}"))?;

    match verify {
        Ok(output) if output.status.success() => {
            let version_out = String::from_utf8_lossy(&output.stdout);
            info!(version = %version_out.trim(), "New binary verified");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("New binary verification failed: {stderr}"));
        }
        Err(e) => {
            return Err(format!("Cannot execute new binary: {e}"));
        }
    }

    // [H4] Remove old backup if it exists
    // [R3:M3] Use OsString to avoid lossy conversion on non-UTF-8 paths
    let mut backup_os = current_exe.as_os_str().to_owned();
    backup_os.push(".bak");
    let backup_path = PathBuf::from(backup_os);
    if backup_path.exists() {
        let _ = tokio::fs::remove_file(&backup_path).await;
    }

    // Backup current binary
    if let Err(e) = tokio::fs::rename(current_exe, &backup_path).await {
        return Err(format!("Failed to backup current binary: {e}"));
    }

    // Move new binary into place
    if let Err(e) = tokio::fs::rename(tmp_path, current_exe).await {
        warn!("Failed to install new binary, rolling back: {e}");
        // [R3:H2] Rollback — report failure explicitly if rollback also fails
        if let Err(rb_err) = tokio::fs::rename(&backup_path, current_exe).await {
            tracing::error!(
                backup = %backup_path.display(),
                target = %current_exe.display(),
                rollback_error = %rb_err,
                "CRITICAL: rollback failed — binary is at backup path, manual recovery needed"
            );
            return Err(format!(
                "Failed to install AND rollback failed: {e}. Binary is at: {}. Manual recovery required: {rb_err}",
                backup_path.display()
            ));
        }
        return Err(format!("Failed to install new binary (rolled back successfully): {e}"));
    }

    // [H4] Clean up backup
    let _ = tokio::fs::remove_file(&backup_path).await;

    info!("Update installed successfully, restart required");

    Ok(ApplyResult {
        success: true,
        message: "Update installed. The service will restart automatically.".into(),
        needs_restart: true,
    })
}

/// RAII guard to reset UPDATE_IN_PROGRESS on drop. [C2]
struct UpdateGuard;
impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Archive extraction [C3][R2:NH3] — rejects absolute paths, symlinks, hard links
// ---------------------------------------------------------------------------

fn extract_binary_from_archive(archive_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let target = binary_name();

    if let Ok(result) = extract_from_tar_gz(archive_bytes, target) {
        return Ok(result);
    }

    #[cfg(target_os = "windows")]
    if let Ok(result) = extract_from_zip(archive_bytes, target) {
        return Ok(result);
    }

    Err(format!("Binary '{target}' not found in archive"))
}

fn extract_from_tar_gz(archive_bytes: &[u8], target: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let gz = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries().map_err(|e| format!("Invalid tar.gz: {e}"))? {
        let mut entry = entry.map_err(|e| format!("Archive entry error: {e}"))?;

        // [R2:NH3] Reject symlinks and hard links
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            warn!("Skipping symlink/hardlink entry in archive");
            continue;
        }

        let path = entry
            .path()
            .map_err(|e| format!("Invalid path in archive: {e}"))?;

        // [R2:NH3] Reject absolute paths and path traversal
        if path.is_absolute()
            || path.components().any(|c| matches!(c,
                std::path::Component::ParentDir | std::path::Component::RootDir))
        {
            warn!(?path, "Skipping unsafe archive entry");
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == target {
            // [R2:NC2] Size-limited read
            let mut buf = Vec::new();
            entry
                .take(MAX_DOWNLOAD_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read binary from archive: {e}"))?;
            if buf.len() as u64 >= MAX_DOWNLOAD_BYTES {
                return Err("Extracted binary exceeds size limit (possible zip bomb)".into());
            }
            return Ok(buf);
        }
    }

    Err(format!("'{target}' not found in tar.gz"))
}

#[cfg(target_os = "windows")]
fn extract_from_zip(archive_bytes: &[u8], target: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let cursor = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| format!("Invalid zip: {e}"))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Zip entry error: {e}"))?;

        let name = file.name().to_string();

        // [R2:NH3] Reject path traversal and absolute paths
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            warn!(name, "Skipping unsafe zip entry");
            continue;
        }

        let file_name = std::path::Path::new(&name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == target {
            // [R2:NC2] Size-limited read
            let mut buf = Vec::new();
            file.take(MAX_DOWNLOAD_BYTES)
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read binary from zip: {e}"))?;
            if buf.len() as u64 >= MAX_DOWNLOAD_BYTES {
                return Err("Extracted binary exceeds size limit (possible zip bomb)".into());
            }
            return Ok(buf);
        }
    }

    Err(format!("'{target}' not found in zip"))
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    published_at: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_basic() {
        assert!(is_newer("0.12.0", "0.13.0"));
        assert!(is_newer("0.12.0", "1.0.0"));
        assert!(is_newer("0.12.0", "0.12.1"));
        assert!(!is_newer("0.12.0", "0.12.0"));
        assert!(!is_newer("0.12.0", "0.11.0"));
        assert!(is_newer("0.9.7", "0.12.0"));
        assert!(!is_newer("0.0.0", "0.0.0")); // equal edge case
    }

    #[test]
    fn test_is_newer_with_v_prefix() {
        assert!(is_newer("v0.12.0", "v0.13.0"));
        assert!(is_newer("0.12.0", "v0.13.0"));
    }

    #[test]
    fn test_is_newer_prerelease() {
        assert!(!is_newer("0.13.0", "0.13.0-beta.1"));
        assert!(is_newer("0.12.0", "0.13.0-beta.1"));
        assert!(!is_newer("0.13.0", "0.13.0-rc.1"));
    }

    #[test]
    fn test_is_newer_rc_to_release() {
        // [R3:L3] RC user should be prompted to upgrade to release
        assert!(is_newer("0.12.0-rc.1", "0.13.0"));
        // Same numeric version: rc is NOT newer than release (both parse to same)
        assert!(!is_newer("0.13.0-rc.1", "0.13.0"));
    }

    #[test]
    fn test_is_newer_partial_version() {
        // "1.0" has no patch — should default to 0
        assert!(is_newer("0.9.0", "1.0"));
        assert!(!is_newer("1.0", "1.0"));
    }

    #[test]
    fn test_valid_download_url() {
        assert!(is_valid_download_url(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/v0.13.0/duduclaw-arm64-apple-darwin.tar.gz"
        ));
        // Checksum URL must also pass [R2:NC1]
        assert!(is_valid_download_url(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/v0.13.0/duduclaw-arm64-apple-darwin.tar.gz.sha256"
        ));
    }

    #[test]
    fn test_reject_foreign_url() {
        assert!(!is_valid_download_url("https://evil.com/duduclaw.tar.gz"));
        assert!(!is_valid_download_url("https://github.com/attacker/repo/releases/download/v1/x.tar.gz"));
    }

    #[test]
    fn test_reject_path_traversal_url() {
        assert!(!is_valid_download_url(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/../../evil"
        ));
    }

    #[test]
    fn test_reject_overlong_url() {
        let long_url = format!(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/v1/{}",
            "a".repeat(600)
        );
        assert!(!is_valid_download_url(&long_url));
    }

    #[test]
    fn test_detect_install_method() {
        let method = detect_install_method();
        let _serialized = serde_json::to_string(&method).unwrap();
    }

    #[test]
    fn test_platform_asset_suffix() {
        assert!(!platform_asset_suffix().is_empty());
    }

    #[test]
    fn test_binary_name() {
        #[cfg(target_os = "windows")]
        assert_eq!(binary_name(), "duduclaw.exe");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(binary_name(), "duduclaw");
    }

    #[test]
    fn test_extract_from_tar_gz_missing_binary() {
        let empty_gz = {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(&[0u8; 1024]).unwrap();
            enc.finish().unwrap()
        };
        let result = extract_from_tar_gz(&empty_gz, "duduclaw");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_extract_from_tar_gz_with_binary() {
        use std::io::Write;

        let mut tar_builder = tar::Builder::new(Vec::new());
        let content = b"FAKE_BINARY";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar_builder.append_data(&mut header, "duduclaw", &content[..]).unwrap();
        let tar_bytes = tar_builder.into_inner().unwrap();

        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&tar_bytes).unwrap();
        let gz_bytes = enc.finish().unwrap();

        let result = extract_from_tar_gz(&gz_bytes, "duduclaw");
        assert_eq!(result.unwrap(), b"FAKE_BINARY");
    }

    #[test]
    fn test_extract_rejects_symlink_entry() {
        use std::io::Write;

        // Build tar with a symlink named "duduclaw" → should be skipped
        let mut tar_builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_cksum();
        tar_builder.append_link(&mut header, "duduclaw", "/etc/evil").unwrap();
        let tar_bytes = tar_builder.into_inner().unwrap();

        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&tar_bytes).unwrap();
        let gz_bytes = enc.finish().unwrap();

        let result = extract_from_tar_gz(&gz_bytes, "duduclaw");
        assert!(result.is_err()); // symlink should be skipped, binary not found
    }

    // [R2:NL2] Test isolation: use compare_exchange to avoid polluting parallel tests
    #[test]
    fn test_update_guard_resets_flag() {
        let was_false = UPDATE_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if !was_false {
            // Another test holds the flag — skip gracefully
            return;
        }
        {
            let _g = UpdateGuard;
        }
        assert!(!UPDATE_IN_PROGRESS.load(Ordering::SeqCst));
    }
}
