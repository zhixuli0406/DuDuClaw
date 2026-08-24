//! Self-update module: check GitHub releases, download, verify, and replace the running binary.
//!
//! Security hardening (code review round 1 + round 2):
//! - [S1] minisign Ed25519 signature verification (hard-fail if `.minisig`
//!   asset is missing or invalid) — public key pinned in the binary, so a
//!   compromised GitHub release without a valid signature cannot install
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

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

const GITHUB_REPO: &str = "zhixuli0406/DuDuClaw";

/// minisign Ed25519 public key for release-asset signature verification. [S1]
///
/// Every release asset must ship a `<asset>.minisig` signed by the matching
/// secret key (CI `MINISIGN_SECRET_KEY` secret / `~/.minisign/duduclaw-release.key`).
/// Rotating the key requires shipping a release signed by the OLD key that
/// contains the NEW key here.
pub(crate) const UPDATE_PUBKEY: &str = "RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh";
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
/// Check whether automatic update is enabled.
///
/// Defaults to `false`. Overridable via `[gateway].auto_update` in config.toml
/// or `DUDUCLAW_AUTO_UPDATE` env var (`1`/`true` to enable, `0`/`false` to disable).
pub fn auto_update_enabled(home_dir: &std::path::Path) -> bool {
    // 1. Env override takes priority
    if let Ok(val) = std::env::var("DUDUCLAW_AUTO_UPDATE") {
        return matches!(val.as_str(), "1" | "true" | "yes");
    }

    // 2. config.toml [gateway].auto_update
    let config_path = home_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(gw) = table.get("gateway").and_then(|v| v.as_table()) {
                if let Some(val) = gw.get("auto_update").and_then(|v| v.as_bool()) {
                    return val;
                }
            }
        }
    }

    false
}

/// Extract the version from `duduclaw version` output (which may be preceded
/// by log lines): the first whitespace token that is a plain MAJOR.MINOR.PATCH.
fn parse_cli_version_output(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|t| {
            t.trim_start_matches('v').split('.').count() == 3 && parse_version_tag(t).is_some()
        })
        .map(|t| t.trim_start_matches('v').to_string())
}

/// Version of the binary currently ON DISK at this process's executable path,
/// best-effort (`None` on any failure). After `npm i -g duduclaw` or a manual
/// binary swap replaces the file under a still-running gateway, the dashboard
/// keeps reporting the old in-memory version with no hint that a restart is
/// all that's missing (2026-07-29 field report: CLI said 1.46.2, dashboard
/// said 1.46.0). Comparing this against [`current_version`] lets the update
/// page say "new version installed — restart to apply".
pub async fn on_disk_version() -> Option<String> {
    let exe = duduclaw_core::platform::executable_path();
    if exe.as_os_str().is_empty() {
        return None;
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(VERIFY_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(&exe)
                .arg("version")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
        }),
    )
    .await
    .ok()?
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_cli_version_output(&String::from_utf8_lossy(&output.stdout))
}

/// Maximum download + decompressed binary size: 200 MB. [H5][R2:NC2]
pub const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
/// Back-off between download retries, in seconds. Length + 1 = max attempts
/// (2026-08-04 field report: a v1.50.0 → v1.51.0 install failed with a red
/// error seconds after the release was published, and the very next manual
/// click succeeded — GitHub asset propagation lag / a dropped connection).
/// Applies ONLY to download-class failures; integrity failures never retry.
///
/// Public so an [`UpdateProvider`] outside this crate feeds the SAME policy to
/// [`with_retries`] instead of inventing a second retry doctrine.
pub const DOWNLOAD_RETRY_DELAYS_SECS: [u64; 2] = [5, 15];
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
    /// Installed via `npm i -g duduclaw` — binary lives inside the
    /// platform sub-package (`@duduclaw/<platform>`). Self-update replaces
    /// that binary in place; the npm registry metadata goes stale until
    /// the user runs `npm i -g duduclaw` again (harmless — next npm
    /// install just overwrites with the same or newer version).
    Npm,
    /// Running as the desktop app's bundled sidecar (inside the signed
    /// `.app` bundle / installer directory). Self-replacing that binary
    /// would break the bundle's code signature — updates arrive through
    /// the Tauri updater instead, so `apply_update` refuses (2026-07-29
    /// field report: the generic path failed opaquely here).
    Desktop,
    /// Running as the closed Enterprise wrapper binary (`duduclaw-pro`).
    /// The public release asset is the open CE binary, so this install can
    /// never be updated from the GitHub channel (see the refusal in
    /// [`apply_update_with_progress`]); its updates arrive through an
    /// extension-supplied [`UpdateProvider`], and until one is wired up the
    /// deployment honestly reports "no update channel" rather than offering an
    /// install button that is guaranteed to be refused.
    Pro,
    Standalone,
    Source,
    Unknown,
}

/// Which channel this deployment receives binary updates through — reported to
/// the dashboard so the update page can stop offering an install it cannot do.
///
/// - `"control_plane"` — an extension supplied an [`UpdateProvider`].
/// - `"none"` — Pro wrapper with no provider. New versions are still *visible*
///   (the Pro build follows the OSS release train, so "a newer version exists"
///   is true information), but nothing in this process can install one.
/// - `"github"` — the public CE release channel.
pub fn update_channel_label(provider_present: bool) -> &'static str {
    if provider_present {
        return "control_plane";
    }
    if detect_install_method() == InstallMethod::Pro {
        return "none";
    }
    "github"
}

/// Whether this process runs inside a container image.
///
/// Design hard constraint: an image is immutable, so swapping the binary
/// in-process is undone the moment the container is recreated — version drift
/// that is worse than not updating at all. Container deployments update by
/// pulling a new image, so [`install_verified_binary`] refuses here.
pub fn is_containerized() -> bool {
    if let Ok(val) = std::env::var("DUDUCLAW_IN_CONTAINER") {
        if matches!(val.as_str(), "1" | "true" | "yes") {
            return true;
        }
    }
    std::path::Path::new("/.dockerenv").exists()
}

/// Result of applying an update.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub success: bool,
    pub message: String,
    pub needs_restart: bool,
}

/// Progress signal emitted while an update is being applied, so the dashboard
/// can say "下載中（重試 1/3）" instead of sitting on a silent spinner and then
/// flashing red. Purely informational — never carries a decision.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateProgress {
    /// Machine-readable stage: currently only `"downloading"`.
    pub phase: &'static str,
    /// 1-based attempt number (1 = first try, ≥2 = retry).
    pub attempt: u32,
    /// Total attempts this stage will make before giving up.
    pub max_attempts: u32,
}

/// Callback type for [`apply_update_with_progress`].
pub type ProgressSink<'a> = &'a (dyn Fn(UpdateProgress) + Send + Sync);

/// A pluggable source of binary updates.
///
/// The open-source gateway ships exactly one path: the public GitHub release
/// channel, wired directly into [`check_update`] /
/// [`apply_update_with_progress`]. A closed-source wrapper injects its own
/// provider through `GatewayExtension::update_provider`, swapping BOTH the
/// update source and the signature-verification key without touching — or
/// weakening — the CE path. No provider ⇒ every code path below behaves
/// exactly as it did before this seam existed.
///
/// Implementors are expected to reuse the machinery this module already
/// exposes rather than re-implement it: [`with_retries`] +
/// [`fetch_asset_bytes`] for downloads, [`verify_archive_with_pubkey`] for
/// integrity (with their OWN pinned public key — never the CE
/// `UPDATE_PUBKEY`), and [`install_verified_binary`] for the
/// extract → verify-run → swap → cleanup sequence.
#[async_trait]
pub trait UpdateProvider: Send + Sync {
    /// Whether a newer version is available, and what it is.
    ///
    /// `UpdateInfo::download_url` / `checksum_url` are OPAQUE to the gateway on
    /// this path — a provider that resolves its asset at apply time (signed,
    /// short-TTL URLs) may leave them empty, and the gateway never validates
    /// them against the GitHub allowlist. Only `available` / `latest_version` /
    /// `release_notes` / `published_at` / `install_method` are consumed.
    async fn check(&self) -> Result<UpdateInfo, String>;

    /// Download, verify, and install the update described by `info`.
    ///
    /// `on_progress` is the same sink the dashboard renders as
    /// 「下載中（重試 N/M）」. Integrity verification is the implementor's
    /// responsibility and MUST fail closed.
    async fn apply(
        &self,
        info: &UpdateInfo,
        on_progress: ProgressSink<'_>,
    ) -> Result<ApplyResult, String>;
}

/// Whether a failed update stage may be retried.
///
/// The split is the whole point of the retry feature: a flaky download is worth
/// another attempt, a failed *integrity* check never is — retrying past a bad
/// checksum or a bad Ed25519 signature would turn a fail-closed security gate
/// into "try until it slips through". [S1][C1]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Network / HTTP / release-propagation failure — safe to retry.
    Transient,
    /// Integrity or permanent failure — must surface immediately.
    Fatal,
}

/// A stage failure carrying its retry classification.
#[derive(Debug, Clone)]
pub struct StageError {
    pub class: FailureClass,
    pub message: String,
}

impl StageError {
    pub fn transient(message: impl Into<String>) -> Self {
        Self { class: FailureClass::Transient, message: message.into() }
    }
    pub fn fatal(message: impl Into<String>) -> Self {
        Self { class: FailureClass::Fatal, message: message.into() }
    }
}

/// Classify an unsuccessful HTTP status from a release-asset fetch.
///
/// 404 is deliberately transient here: the dashboard only ever downloads a URL
/// the gateway itself resolved from the release, so "asset not there yet"
/// means propagation lag, not a wrong URL. 403 covers expired signed CDN URLs
/// and secondary rate limits; 5xx / 408 / 425 / 429 are the usual suspects.
fn classify_http_status(status: reqwest::StatusCode) -> FailureClass {
    if status.is_server_error() {
        return FailureClass::Transient;
    }
    match status.as_u16() {
        403 | 404 | 408 | 425 | 429 => FailureClass::Transient,
        _ => FailureClass::Fatal,
    }
}

/// Run `op` until it succeeds, a [`FailureClass::Fatal`] error surfaces, or the
/// attempts run out. `delays` holds the back-off before each RETRY, so
/// `max_attempts = delays.len() + 1`. `on_attempt(attempt, max_attempts)` fires
/// before every attempt (including the first) for UI progress.
pub async fn with_retries<T, F, Fut>(
    delays: &[u64],
    on_attempt: &(dyn Fn(u32, u32) + Send + Sync),
    mut op: F,
) -> Result<T, StageError>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, StageError>>,
{
    let max_attempts = delays.len() as u32 + 1;
    let mut attempt = 1u32;
    loop {
        on_attempt(attempt, max_attempts);
        match op(attempt).await {
            Ok(v) => return Ok(v),
            Err(e) if e.class == FailureClass::Transient && attempt < max_attempts => {
                let delay = delays.get((attempt - 1) as usize).copied().unwrap_or(15);
                warn!(
                    attempt,
                    max_attempts,
                    retry_in_secs = delay,
                    error = %e.message,
                    "Update download failed — retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Installation method detection (includes linuxbrew [L2])
// ---------------------------------------------------------------------------

/// Return the GitHub repo slug for update checking.
fn github_repo() -> &'static str {
    GITHUB_REPO
}

/// Return the Homebrew formula name.
pub fn brew_formula_name() -> &'static str {
    "duduclaw"
}

/// Whether `exe` is the closed Enterprise wrapper binary (`duduclaw-pro`),
/// which must never be overwritten by the public CE release asset — see the
/// refusal in [`apply_update_with_progress`]. Matches the bare stem so
/// `duduclaw-pro.exe` (Windows) is covered too.
fn is_pro_wrapper_exe(exe: &std::path::Path) -> bool {
    exe.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s == "duduclaw-pro")
}

pub fn detect_install_method() -> InstallMethod {
    let exe = duduclaw_core::platform::executable_path();
    if exe.as_os_str().is_empty() {
        return InstallMethod::Unknown;
    }

    // Enterprise wrapper: checked BEFORE Desktop, because a `duduclaw-pro`
    // deployed next to a desktop bundle is still a Pro install — the update
    // channel follows the BINARY, not the directory it happens to sit in.
    if is_pro_wrapper_exe(&exe) {
        return InstallMethod::Pro;
    }

    let exe_str = exe.to_string_lossy();

    // Desktop-app sidecar: macOS bundle path, or (Windows/Linux) the Tauri
    // main executable sitting next to us. Checked FIRST — a sidecar under
    // /Applications would otherwise fall through to Standalone and try to
    // rewrite the signed bundle.
    if exe_str.contains(".app/Contents/") {
        return InstallMethod::Desktop;
    }
    if let Some(dir) = exe.parent() {
        if dir.join("DuDuClaw.exe").exists() || dir.join("duduclaw-desktop-app").exists() {
            return InstallMethod::Desktop;
        }
    }
    if exe_str.contains("/Cellar/duduclaw/")
        || exe_str.contains("/homebrew/")
        || exe_str.contains("/linuxbrew/")
    {
        return InstallMethod::Homebrew;
    }
    if exe_str.contains("/node_modules/") || exe_str.contains("\\node_modules\\") {
        return InstallMethod::Npm;
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

/// Strict parse of a CLI version tag (`v1.46.2`, `1.46.2`, `v2.0.0-rc.1`).
/// Returns `None` for anything that is not a plain version — notably the
/// desktop installer tags (`desktop-v1.46.2`) and the fixed `desktop-updater`
/// pointer — so callers can tell "older version" apart from "not a CLI
/// version tag at all" (which `is_newer`'s tolerant parse silently maps to 0).
fn parse_version_tag(tag: &str) -> Option<(u32, u32, u32)> {
    let s = tag.strip_prefix('v').unwrap_or(tag);
    if s.is_empty() {
        return None;
    }
    let mut nums = [0u32; 3];
    for (i, part) in s.split('.').enumerate() {
        if i >= 3 {
            break; // pre-release dots ("1.0.0-rc.1") beyond patch
        }
        let numeric = part.split('-').next()?;
        nums[i] = numeric.parse().ok()?;
    }
    Some((nums[0], nums[1], nums[2]))
}

/// Map a GitHub "latest" tag to the CLI release tag it corresponds to.
/// Plain version tags map to themselves; a desktop installer tag
/// (`desktop-v1.46.2`, published minutes after `v1.46.2` and thus able to
/// claim the repo's "Latest" marker) maps to its paired CLI tag. Anything
/// else → `None`.
///
/// 2026-07-29 field report: `releases/latest` returned `desktop-v1.46.2`, the
/// update page showed "vdesktop-v1.46.2", and the tolerant semver compare
/// parsed it as (0,46,2) — wrongly reporting "already up to date" on v1.46.0.
fn paired_cli_tag(tag: &str) -> Option<&str> {
    if parse_version_tag(tag).is_some() {
        return Some(tag);
    }
    tag.strip_prefix("desktop-")
        .filter(|t| parse_version_tag(t).is_some())
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Asset name suffix matching `.github/workflows/release.yml` artifacts
/// (e.g. `duduclaw-darwin-arm64.tar.gz`). Must stay in sync with the CI
/// matrix `artifact:` names.
fn platform_asset_suffix() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "darwin-arm64.tar.gz" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "darwin-x64.tar.gz" }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "linux-x64.tar.gz" }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { "linux-arm64.tar.gz" }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    { "windows-x64.zip" }
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
// Ed25519 signature verification [S1] — minisign format, pinned public key
// ---------------------------------------------------------------------------

/// Verify a minisign signature (`.minisig` file content) over `data` against
/// `pubkey_b64`. Fails closed: any parse or verification error rejects the
/// update. Legacy (non-prehashed) signatures are rejected — CI signs with
/// modern minisign (`ED` alg).
///
/// `pub` since H3d because the OS-image update chain
/// ([`crate::os_update`]) signs a *manifest* covering several payload files
/// rather than one archive, so it needs detached-signature verification on
/// its own, and must not grow a second minisign dialect to get it. The key
/// stays a parameter for the same reason
/// [`verify_archive_with_pubkey`] takes one: the OS channel pins a
/// different keypair than the app channel, so a compromise of one cannot
/// install over the other's users.
pub fn verify_minisign_signature_with_pubkey(
    data: &[u8],
    sig_text: &str,
    pubkey_b64: &str,
) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("Embedded update public key is invalid: {e}"))?;
    let sig = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| format!("Signature file has invalid format: {e}"))?;
    pk.verify(data, &sig, false)
        .map_err(|e| format!("Ed25519 signature verification FAILED — refusing update: {e}"))
}

/// CE-channel signature verification: [`verify_minisign_signature_with_pubkey`]
/// against the pinned [`UPDATE_PUBKEY`].
#[cfg(test)]
fn verify_minisign_signature(data: &[u8], sig_text: &str) -> Result<(), String> {
    verify_minisign_signature_with_pubkey(data, sig_text, UPDATE_PUBKEY)
}

/// Extract the first 64-char hex token from a checksum sidecar file.
/// Tolerates `shasum -a 256` ("<hash>  <file>"), BSD tag format, and
/// PowerShell `Format-List` ("Hash : <HASH>") layouts.
fn extract_sha256_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_hexdigit()))
        .find(|t| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|t| t.to_lowercase())
}

// ---------------------------------------------------------------------------
// Check for update
// ---------------------------------------------------------------------------

pub async fn check_update() -> Result<UpdateInfo, String> {
    // [R3:M2] Redirect policy for check_update — only allow GitHub API/CDN
    let ver = current_version();
    let ua = format!("{}/{ver}", "duduclaw");
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
    let api_result: Result<GitHubRelease, String> = async {
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch release info: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GitHub API returned {}", resp.status()));
        }
        resp.json::<GitHubRelease>()
            .await
            .map_err(|e| format!("Failed to parse release JSON: {e}"))
    }
    .await;

    let release: GitHubRelease = match api_result {
        Ok(r) => r,
        Err(api_err) => {
            // Fallback (2026-07-29 field report: 更新失敗 on an office NAT):
            // the anonymous GitHub API allows only 60 req/hr per IP, so a
            // shared egress trivially exhausts it. The releases/latest WEB
            // endpoint is not rate-limited that way — its redirect Location
            // carries the tag, and asset URLs are deterministic from it.
            match check_latest_via_redirect(&ua, repo).await {
                Ok(r) => r,
                Err(fb_err) => {
                    return Err(format!("{api_err}（fallback 亦失敗：{fb_err}）"));
                }
            }
        }
    };

    // "Latest" may be claimed by a desktop installer release (desktop-v*) —
    // resolve the paired CLI release before comparing versions.
    let release = resolve_cli_release(&client, repo, release).await?;

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

/// Apply an update without progress reporting (CLI / auto-update loop).
pub async fn apply_update(download_url: &str, checksum_url: &str) -> Result<ApplyResult, String> {
    apply_update_with_progress(download_url, checksum_url, &|_| {}).await
}

/// Apply an update, reporting download attempts through `on_progress` so the
/// dashboard can show "下載中（重試 N/M）" while a transient failure is retried.
pub async fn apply_update_with_progress(
    download_url: &str,
    checksum_url: &str,
    on_progress: ProgressSink<'_>,
) -> Result<ApplyResult, String> {
    // [C2] Concurrency guard
    if UPDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Another update is already in progress".into());
    }
    let _guard = UpdateGuard;

    // Desktop sidecar: replacing the binary inside the signed app bundle
    // breaks the code-signature seal — the Tauri updater owns this install.
    if matches!(detect_install_method(), InstallMethod::Desktop) {
        return Err(
            "桌面版由 DuDuClaw App 自動更新（重新啟動 App 即會套用新版本），此處不執行覆寫"
                .into(),
        );
    }

    // Enterprise wrapper (`duduclaw-pro`): the public release asset is the
    // open CE binary. Installing it over the wrapper would silently drop the
    // Enterprise extension AND — because the wrapper ignores argv while the
    // CE binary requires a subcommand — crash-loop any service unit that
    // launches the binary with no arguments (the wrapper's documented usage).
    // Refuse with the real upgrade path instead of bricking the deployment.
    if is_pro_wrapper_exe(&duduclaw_core::platform::executable_path()) {
        return Err(
            "企業版 (duduclaw-pro) 不透過公開發行通道自我更新——請以企業散發包或重建流程更新 binary"
                .into(),
        );
    }

    // Homebrew check
    if detect_install_method() == InstallMethod::Homebrew {
        return Ok(ApplyResult {
            success: false,
            message: "Homebrew installation detected. The Homebrew tap has been discontinued — \
                      please reinstall via npm (npm install -g duduclaw) or the desktop app to \
                      keep receiving updates."
                .to_string(),
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

    // Pinned at first access — stays valid for the post-shutdown re-exec
    // even after the on-disk binary is replaced (Linux /proc/self/exe
    // would otherwise read "… (deleted)").
    let current_exe = duduclaw_core::platform::executable_path();
    if current_exe.as_os_str().is_empty() {
        return Err("Cannot determine current binary path".into());
    }
    let exe_dir = current_exe
        .parent()
        .ok_or("Cannot determine binary directory")?;

    // [H3] Verify directory permissions. Group-writable by an admin-class
    // group (macOS `/usr/local/bin` ships `drwxrwxr-x …:admin`) is tolerated —
    // the blanket group-writable refusal made self-update fail on every stock
    // standalone macOS install (2026-08-17 field report: dashboard 更新失敗,
    // audit log "Binary directory is world/group writable").
    if duduclaw_core::platform::is_unsafe_update_dir(exe_dir) {
        return Err(format!(
            "Binary directory {} is writable by non-admin users — refusing update for safety \
             (fix: chmod o-w,g-w on that directory, or reinstall to a user-owned path)",
            exe_dir.display()
        ));
    }

    info!(url = download_url, "Downloading update...");

    // [R2:NM2] Custom redirect policy — re-validate redirect targets
    let ver = current_version();
    let ua = format!("{}/{ver}", "duduclaw");
    let client = reqwest::Client::builder()
        .user_agent(&ua)
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().clone();
            let target = url.as_str();
            // Allow GitHub CDN redirects. Release assets historically
            // redirected to objects.githubusercontent.com; GitHub now uses
            // release-assets.githubusercontent.com (Azure-blob-signed URLs).
            let repo = github_repo();
            let repo_prefix = format!("https://github.com/{repo}/");
            if target.starts_with("https://objects.githubusercontent.com/")
                || target.starts_with("https://release-assets.githubusercontent.com/")
                || target.starts_with(&repo_prefix)
            {
                attempt.follow()
            } else {
                attempt.error(format!("Redirect to non-whitelisted URL blocked: {target}"))
            }
        }))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // [S1] minisign signature URL is derived, not client-supplied — validate
    // it before any network work so a bad shape fails fast, not mid-retry.
    let signature_url = format!("{download_url}.minisig");
    if !is_valid_download_url(&signature_url) {
        return Err(format!("Rejected unsafe signature URL: {signature_url}"));
    }

    // Download + verify, retrying ONLY download-class failures. A checksum or
    // signature failure aborts on the first attempt (fail-closed [S1][C1]).
    let bytes = with_retries(
        &DOWNLOAD_RETRY_DELAYS_SECS,
        &|attempt, max_attempts| {
            on_progress(UpdateProgress { phase: "downloading", attempt, max_attempts })
        },
        |_attempt| download_and_verify_once(&client, download_url, checksum_url, &signature_url),
    )
    .await
    .map_err(|e| e.message)?;

    install_archive_bytes(&bytes, &current_exe, exe_dir).await
}

/// Install an already-verified archive over the running binary: extract →
/// size-check → temp file → verify-run → atomic swap → cleanup.
///
/// Callers own integrity verification and the environment gates (container,
/// directory permissions); this is only the swap machine. Shared by the CE
/// GitHub path and by [`install_verified_binary`] so a provider cannot end up
/// with a subtly different install sequence.
async fn install_archive_bytes(
    archive_bytes: &[u8],
    current_exe: &std::path::Path,
    exe_dir: &std::path::Path,
) -> Result<ApplyResult, String> {
    // Extract binary
    info!("Extracting binary...");
    let new_binary_bytes = extract_binary_from_archive(archive_bytes)?;

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
    let result = apply_update_inner(&tmp_path, current_exe, exe_dir).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }
    result
}

/// Install a verified update archive over the running binary — the install half
/// of an [`UpdateProvider`], shared with the CE path so both go through exactly
/// one swap implementation.
///
/// `archive_bytes` MUST already have passed integrity verification (see
/// [`verify_archive_with_pubkey`]); this function does not and cannot know which
/// key the caller's channel is pinned to.
///
/// Two environment gates fire first, both fail-closed:
/// 1. **Container** — an image is immutable, so an in-process swap is silently
///    reverted the next time the container is recreated. Refused outright; those
///    deployments update by pulling a new image.
/// 2. **Directory permissions** — the same [`duduclaw_core::platform::is_unsafe_update_dir`]
///    gate the CE path applies, so a world-writable install directory cannot be
///    used to land a binary.
pub async fn install_verified_binary(archive_bytes: &[u8]) -> Result<ApplyResult, String> {
    if is_containerized() {
        return Err(
            "容器內不執行 binary 換裝（image 不可變，換裝會在重建容器時還原）——請改用 image 更新流程"
                .into(),
        );
    }

    let current_exe = duduclaw_core::platform::executable_path();
    if current_exe.as_os_str().is_empty() {
        return Err("Cannot determine current binary path".into());
    }
    let exe_dir = current_exe
        .parent()
        .ok_or("Cannot determine binary directory")?;

    if duduclaw_core::platform::is_unsafe_update_dir(exe_dir) {
        return Err(format!(
            "Binary directory {} is writable by non-admin users — refusing update for safety \
             (fix: chmod o-w,g-w on that directory, or reinstall to a user-owned path)",
            exe_dir.display()
        ));
    }

    install_archive_bytes(archive_bytes, &current_exe, exe_dir).await
}

/// Fetch a release asset body once, classifying every failure. Network errors
/// and retryable HTTP statuses come back [`FailureClass::Transient`]; anything
/// else is fatal. `what` names the asset in error messages ("Update archive").
///
/// Public so an out-of-crate [`UpdateProvider`] inherits the same size limit
/// ([`MAX_DOWNLOAD_BYTES`]) and retry classification as the CE path.
pub async fn fetch_asset_bytes(
    client: &reqwest::Client,
    url: &str,
    what: &str,
) -> Result<Vec<u8>, StageError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| StageError::transient(format!("{what} download failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let msg = format!("{what} download returned HTTP {status}");
        return Err(match classify_http_status(status) {
            FailureClass::Transient => StageError::transient(msg),
            FailureClass::Fatal => StageError::fatal(msg),
        });
    }

    // [H5] Size limit — a declared over-limit body is not worth retrying.
    if let Some(len) = resp.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(StageError::fatal(format!(
                "{what} too large: {len} bytes (limit: {MAX_DOWNLOAD_BYTES})"
            )));
        }
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| StageError::transient(format!("Failed to read {what}: {e}")))?;

    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(StageError::fatal(format!(
            "{what} exceeded size limit: {} bytes",
            bytes.len()
        )));
    }
    Ok(bytes.to_vec())
}

/// Verify a downloaded archive against its checksum sidecar and its minisign
/// signature, using a caller-supplied public key.
///
/// The key is a PARAMETER precisely so the Pro update channel can pin its own
/// keypair: a compromise of one signing key must not become the ability to
/// install over the other channel's users. Callers pass a key that is pinned in
/// their own binary — never one read from the network.
///
/// Fails closed on every branch (unparseable sidecar, digest mismatch, bad or
/// absent signature). Never retry a failure from here: an integrity gate a
/// retry loop can wear down is not a gate. [S1][C1][R2:NH2]
pub fn verify_archive_with_pubkey(
    bytes: &[u8],
    checksum_text: &str,
    sig_text: &str,
    pubkey_b64: &str,
) -> Result<(), String> {
    // Scan tokens for the first 64-char hex digest so both `shasum`
    // ("<hash>  <file>") and PowerShell `Format-List` ("Hash : <HASH>")
    // sidecar formats parse.
    let expected = extract_sha256_token(checksum_text)
        .ok_or("Checksum file has invalid format (no SHA-256 digest found)")?;
    let computed = format!("{:x}", Sha256::digest(bytes));
    if computed != expected {
        return Err(format!(
            "SHA-256 checksum mismatch!\n  Expected: {expected}\n  Computed: {computed}"
        ));
    }
    info!("SHA-256 checksum verified");

    // The public key is pinned in the binary, so this holds even if the
    // release (and its .sha256 sidecar) is attacker-controlled.
    verify_minisign_signature_with_pubkey(bytes, sig_text, pubkey_b64)?;
    info!("Ed25519 signature verified");
    Ok(())
}

/// CE-channel integrity check: [`verify_archive_with_pubkey`] against the
/// pinned [`UPDATE_PUBKEY`], with every failure classified
/// [`FailureClass::Fatal`] for the retry loop.
fn verify_archive_integrity(
    bytes: &[u8],
    checksum_text: &str,
    sig_text: &str,
) -> Result<(), StageError> {
    verify_archive_with_pubkey(bytes, checksum_text, sig_text, UPDATE_PUBKEY)
        .map_err(StageError::fatal)
}

/// One download + verify attempt: archive, checksum sidecar, signature sidecar,
/// then the two integrity checks.
async fn download_and_verify_once(
    client: &reqwest::Client,
    download_url: &str,
    checksum_url: &str,
    signature_url: &str,
) -> Result<Vec<u8>, StageError> {
    let bytes = fetch_asset_bytes(client, download_url, "Update archive").await?;
    info!(size = bytes.len(), "Download complete");

    // (empty checksum_url already rejected at apply_update entry [R3:H1])
    info!("Verifying SHA-256 checksum...");
    let checksum_bytes = fetch_asset_bytes(client, checksum_url, "Checksum file").await?;
    let checksum_text = String::from_utf8_lossy(&checksum_bytes).into_owned();

    info!("Verifying Ed25519 signature...");
    let sig_bytes = fetch_asset_bytes(client, signature_url, "Signature file").await?;
    let sig_text = String::from_utf8_lossy(&sig_bytes).into_owned();

    verify_archive_integrity(&bytes, &checksum_text, &sig_text)?;
    Ok(bytes)
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

/// Extract the release tag from a `releases/latest` redirect Location
/// (`…/releases/tag/v1.46.1` → `v1.46.1`). Pure for testability.
fn tag_from_latest_location(location: &str) -> Option<String> {
    let (_, tag) = location.rsplit_once("/releases/tag/")?;
    let tag = tag.trim_end_matches('/');
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | 'v')) {
        return None;
    }
    Some(tag.to_string())
}

/// Synthesize a release record with deterministic asset URLs
/// (`releases/download/<tag>/duduclaw-<suffix>`) for paths where the real
/// release object is unavailable (rate-limited API). Release notes / publish
/// date are unavailable here — empty, never fabricated.
fn synthesize_release(repo: &str, tag: &str) -> GitHubRelease {
    let asset_name = format!("duduclaw-{}", platform_asset_suffix());
    let download_url = format!("https://github.com/{repo}/releases/download/{tag}/{asset_name}");
    GitHubRelease {
        tag_name: tag.to_string(),
        body: None,
        published_at: None,
        assets: vec![GitHubAsset {
            name: asset_name,
            browser_download_url: download_url,
        }],
    }
}

/// If the "latest" release carries a non-CLI tag (desktop installer), swap in
/// the paired CLI release: prefer the real release object (correct notes,
/// publish date, asset list); degrade to deterministic asset URLs when the
/// API is unavailable (e.g. this check already came through the redirect
/// fallback because of rate limiting).
async fn resolve_cli_release(
    client: &reqwest::Client,
    repo: &str,
    release: GitHubRelease,
) -> Result<GitHubRelease, String> {
    if parse_version_tag(&release.tag_name).is_some() {
        return Ok(release);
    }
    let Some(cli_tag) = paired_cli_tag(&release.tag_name).map(str::to_string) else {
        return Err(format!(
            "最新 release tag「{}」不是 CLI 版本，無法判斷可更新版本",
            release.tag_name
        ));
    };
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/{cli_tag}");
    let by_tag: Result<GitHubRelease, String> = async {
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch release by tag: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GitHub API returned {}", resp.status()));
        }
        resp.json::<GitHubRelease>()
            .await
            .map_err(|e| format!("Failed to parse release JSON: {e}"))
    }
    .await;
    match by_tag {
        Ok(r) => Ok(r),
        Err(e) => {
            warn!(tag = %cli_tag, error = %e, "release-by-tag lookup failed; using deterministic asset URLs");
            Ok(synthesize_release(repo, &cli_tag))
        }
    }
}

/// Rate-limit-immune fallback: resolve the latest tag from the WEB
/// `releases/latest` redirect (no API quota) and synthesize the release with
/// deterministic asset URLs (`releases/download/<tag>/duduclaw-<suffix>`).
/// Release notes / publish date are unavailable on this path — empty, never
/// fabricated.
async fn check_latest_via_redirect(ua: &str, repo: &str) -> Result<GitHubRelease, String> {
    let client = reqwest::Client::builder()
        .user_agent(ua)
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;
    let resp = client
        .get(format!("https://github.com/{repo}/releases/latest"))
        .send()
        .await
        .map_err(|e| format!("releases/latest fetch failed: {e}"))?;
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("releases/latest returned {} without a redirect", resp.status()))?;
    let tag = tag_from_latest_location(location)
        .ok_or_else(|| format!("unrecognized releases/latest redirect: {location}"))?;
    Ok(synthesize_release(repo, &tag))
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
    fn test_pro_wrapper_exe_detection() {
        use std::path::Path;
        assert!(is_pro_wrapper_exe(Path::new("/usr/local/bin/duduclaw-pro")));
        // Windows form (`.exe` stripped by file_stem); backslash separators
        // only parse as separators on Windows, so keep this relative.
        assert!(is_pro_wrapper_exe(Path::new("duduclaw-pro.exe")));
        assert!(!is_pro_wrapper_exe(Path::new("/usr/local/bin/duduclaw")));
        assert!(!is_pro_wrapper_exe(Path::new(
            "/home/x/.duduclaw/bin/duduclaw-pro-gateway"
        )));
        assert!(!is_pro_wrapper_exe(Path::new("")));
    }

    // Real minisign signature over the exact bytes b"FAKE_BINARY", signed
    // with the DuDuClaw release key that matches UPDATE_PUBKEY.
    const TEST_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUTh5pOpk0YmdButYZG9tlcekzhpZGpz83+E62dF1rtB5NtAoxXBenf0jZrluKejtL1R5cHtL60+75EzvH/EorwGHcc2aZlwkwQ=\n\
trusted comment: duduclaw test\n\
mRGk2RUiXNVr9zzLXu6BI9+0URr0xlBwS3rMMXD3smkK7rcMrajd/tMz7jhWxQkiPzmWe4pdxFiIG1Vx2JdFAQ==\n";

    #[test]
    fn test_minisign_verify_valid() {
        assert!(verify_minisign_signature(b"FAKE_BINARY", TEST_SIG).is_ok());
    }

    #[test]
    fn test_minisign_verify_tampered_data() {
        let err = verify_minisign_signature(b"FAKE_BINARY_TAMPERED", TEST_SIG).unwrap_err();
        assert!(err.contains("FAILED"));
    }

    #[test]
    fn test_minisign_verify_garbage_signature() {
        assert!(verify_minisign_signature(b"FAKE_BINARY", "not a signature").is_err());
    }

    #[test]
    fn test_extract_sha256_token_shasum_format() {
        let digest = "a".repeat(64);
        let text = format!("{digest}  duduclaw-darwin-arm64.tar.gz\n");
        assert_eq!(extract_sha256_token(&text), Some(digest));
    }

    #[test]
    fn test_extract_sha256_token_powershell_format() {
        // `Get-FileHash | Format-List Hash` output shape
        let text = format!("\nHash : {}\n\n", "AB".repeat(32));
        assert_eq!(extract_sha256_token(&text), Some("ab".repeat(32)));
    }

    #[test]
    fn test_extract_sha256_token_rejects_garbage() {
        assert_eq!(extract_sha256_token("no digest here"), None);
        assert_eq!(extract_sha256_token(&"z".repeat(64)), None);
        // Too short
        assert_eq!(extract_sha256_token(&"a".repeat(63)), None);
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
    fn test_auto_update_defaults() {
        // With no env var and no config.toml, CE defaults to false
        let tmp = std::env::temp_dir().join("duduclaw-test-auto-update");
        let _ = std::fs::create_dir_all(&tmp);
        // No config.toml → defaults to false
        let result = auto_update_enabled(&tmp);
        assert!(!result);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_auto_update_config_override() {
        let tmp = std::env::temp_dir().join("duduclaw-test-auto-update-cfg");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(
            tmp.join("config.toml"),
            "[gateway]\nauto_update = true\n",
        ).unwrap();
        assert!(auto_update_enabled(&tmp));

        std::fs::write(
            tmp.join("config.toml"),
            "[gateway]\nauto_update = false\n",
        ).unwrap();
        assert!(!auto_update_enabled(&tmp));

        let _ = std::fs::remove_dir_all(&tmp);
    }

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

    // ── releases/latest redirect fallback (rate-limit immunity) ──

    #[test]
    fn tag_from_latest_location_parses_and_fences() {
        assert_eq!(
            tag_from_latest_location("https://github.com/zhixuli0406/DuDuClaw/releases/tag/v1.46.1"),
            Some("v1.46.1".to_string())
        );
        assert_eq!(
            tag_from_latest_location("/zhixuli0406/DuDuClaw/releases/tag/v2.0.0-rc.1/"),
            Some("v2.0.0-rc.1".to_string())
        );
        // No tag segment / empty / hostile characters → None.
        assert_eq!(tag_from_latest_location("https://github.com/x/y/releases"), None);
        assert_eq!(tag_from_latest_location(".../releases/tag/"), None);
        assert_eq!(tag_from_latest_location(".../releases/tag/v1.0.0?x=../../evil"), None);
    }

    // ── desktop-v* claiming "Latest" (2026-07-29 field report) ──

    #[test]
    fn parse_version_tag_accepts_cli_tags() {
        assert_eq!(parse_version_tag("v1.46.2"), Some((1, 46, 2)));
        assert_eq!(parse_version_tag("1.46.2"), Some((1, 46, 2)));
        assert_eq!(parse_version_tag("v2.0.0-rc.1"), Some((2, 0, 0)));
        assert_eq!(parse_version_tag("1.0"), Some((1, 0, 0)));
    }

    #[test]
    fn parse_version_tag_rejects_non_cli_tags() {
        assert_eq!(parse_version_tag("desktop-v1.46.2"), None);
        assert_eq!(parse_version_tag("desktop-updater"), None);
        assert_eq!(parse_version_tag(""), None);
        assert_eq!(parse_version_tag("v"), None);
        assert_eq!(parse_version_tag("1.46a.2"), None);
    }

    #[test]
    fn paired_cli_tag_resolves_desktop_tags() {
        assert_eq!(paired_cli_tag("v1.46.2"), Some("v1.46.2"));
        assert_eq!(paired_cli_tag("desktop-v1.46.2"), Some("v1.46.2"));
        assert_eq!(paired_cli_tag("desktop-updater"), None);
        assert_eq!(paired_cli_tag("nightly"), None);
    }

    #[test]
    fn parse_cli_version_output_skips_log_lines() {
        // Real-world shape: a log line precedes the version line.
        let out = "[duduclaw] effective log level: config.toml [general] log_level=info\nduduclaw 1.46.2\n";
        assert_eq!(parse_cli_version_output(out), Some("1.46.2".to_string()));
        assert_eq!(parse_cli_version_output("duduclaw v1.46.2"), Some("1.46.2".to_string()));
        assert_eq!(parse_cli_version_output("no version here"), None);
        // Two-dot requirement rejects filenames like config.toml or bare "1.46".
        assert_eq!(parse_cli_version_output("config.toml 1.46"), None);
    }

    #[test]
    fn synthesize_release_builds_deterministic_urls() {
        let r = synthesize_release("zhixuli0406/DuDuClaw", "v1.46.2");
        assert_eq!(r.tag_name, "v1.46.2");
        assert_eq!(r.assets.len(), 1);
        assert!(r.assets[0]
            .browser_download_url
            .starts_with("https://github.com/zhixuli0406/DuDuClaw/releases/download/v1.46.2/duduclaw-"));
        assert!(is_valid_download_url(&r.assets[0].browser_download_url));
    }

    // ── download retry vs. integrity fail-closed (2026-08-04 field report) ──

    #[test]
    fn classify_http_status_splits_transient_from_fatal() {
        use reqwest::StatusCode;
        // Release-asset propagation lag / CDN hiccups / throttling → retry.
        for s in [403u16, 404, 408, 425, 429, 500, 502, 503, 504] {
            assert_eq!(
                classify_http_status(StatusCode::from_u16(s).unwrap()),
                FailureClass::Transient,
                "HTTP {s} should be retryable"
            );
        }
        // Anything else is a real "this will never work" answer.
        for s in [400u16, 401, 410, 451] {
            assert_eq!(
                classify_http_status(StatusCode::from_u16(s).unwrap()),
                FailureClass::Fatal,
                "HTTP {s} must not be retried"
            );
        }
    }

    #[test]
    fn integrity_failures_are_always_fatal() {
        // Signature is valid for exactly b"FAKE_BINARY".
        let good = b"FAKE_BINARY";
        let good_sum = format!("{:x}", Sha256::digest(good));

        // Happy path: matching digest + valid signature.
        assert!(verify_archive_integrity(good, &format!("{good_sum}  duduclaw.tar.gz\n"), TEST_SIG).is_ok());

        // Checksum mismatch → fatal, never retried.
        let wrong_sum = "a".repeat(64);
        let err = verify_archive_integrity(good, &wrong_sum, TEST_SIG).unwrap_err();
        assert_eq!(err.class, FailureClass::Fatal);
        assert!(err.message.contains("checksum mismatch"));

        // Unparseable checksum sidecar → fatal.
        assert_eq!(
            verify_archive_integrity(good, "no digest here", TEST_SIG).unwrap_err().class,
            FailureClass::Fatal
        );

        // Signature over different bytes → fatal (checksum sidecar can be
        // rewritten by whoever tampered with the archive; the pinned key can't).
        let tampered = b"FAKE_BINARY_TAMPERED";
        let tampered_sum = format!("{:x}", Sha256::digest(tampered));
        let err = verify_archive_integrity(tampered, &tampered_sum, TEST_SIG).unwrap_err();
        assert_eq!(err.class, FailureClass::Fatal);
        assert!(err.message.contains("FAILED"));
    }

    #[tokio::test]
    async fn with_retries_retries_transient_then_succeeds() {
        use std::sync::atomic::AtomicU32;
        let calls = AtomicU32::new(0);
        let seen: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());
        // Zero delays keep the test instant; production uses [5, 15].
        let out: Result<&str, StageError> = with_retries(
            &[0, 0],
            &|a, m| seen.lock().unwrap().push((a, m)),
            |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(StageError::transient(format!("boom {n}")))
                    } else {
                        Ok("installed")
                    }
                }
            },
        )
        .await;
        assert_eq!(out.unwrap(), "installed");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // Progress reported for every attempt, including the first.
        assert_eq!(*seen.lock().unwrap(), vec![(1, 3), (2, 3), (3, 3)]);
    }

    #[tokio::test]
    async fn with_retries_gives_up_after_max_attempts() {
        use std::sync::atomic::AtomicU32;
        let calls = AtomicU32::new(0);
        let out: Result<(), StageError> = with_retries(&[0, 0], &|_, _| {}, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(StageError::transient("always down")) }
        })
        .await;
        assert_eq!(out.unwrap_err().message, "always down");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "1 initial + 2 retries");
    }

    #[tokio::test]
    async fn with_retries_never_retries_fatal() {
        use std::sync::atomic::AtomicU32;
        let calls = AtomicU32::new(0);
        let out: Result<(), StageError> = with_retries(&[0, 0], &|_, _| {}, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(StageError::fatal("SHA-256 checksum mismatch!")) }
        })
        .await;
        assert!(out.unwrap_err().message.contains("checksum mismatch"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an integrity failure must abort on the first attempt"
        );
    }

    #[test]
    fn retry_delays_match_the_documented_policy() {
        // 2 retries at 5s / 15s — the UI's "重試 N/3" copy is derived from this.
        assert_eq!(DOWNLOAD_RETRY_DELAYS_SECS, [5, 15]);
    }

    // ── Pro update channel seam (P0a/P1a) ──

    #[test]
    fn install_method_pro_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&InstallMethod::Pro).unwrap(),
            "\"pro\"",
            "the dashboard branches on this exact literal"
        );
        // Round-trips, so a persisted value stays readable.
        assert_eq!(
            serde_json::from_str::<InstallMethod>("\"pro\"").unwrap(),
            InstallMethod::Pro
        );
        // Pre-existing variants are untouched.
        assert_eq!(serde_json::to_string(&InstallMethod::Desktop).unwrap(), "\"desktop\"");
        assert_eq!(serde_json::to_string(&InstallMethod::Homebrew).unwrap(), "\"homebrew\"");
    }

    #[test]
    fn update_channel_label_rules() {
        // A provider wins regardless of how the binary was installed.
        assert_eq!(update_channel_label(true), "control_plane");
        // Without a provider the label follows the install method. This test
        // process is not `duduclaw-pro`, so it must read "github" — the CE
        // answer, which is what byte-identical CE behavior depends on.
        assert_ne!(detect_install_method(), InstallMethod::Pro);
        assert_eq!(update_channel_label(false), "github");
    }

    #[test]
    fn containerized_detection_honors_env_override() {
        // Serialized against the other env-mutating tests in this module by
        // being the only one touching DUDUCLAW_IN_CONTAINER.
        let prev = std::env::var("DUDUCLAW_IN_CONTAINER").ok();
        for truthy in ["1", "true", "yes"] {
            unsafe { std::env::set_var("DUDUCLAW_IN_CONTAINER", truthy) };
            assert!(is_containerized(), "{truthy} must read as containerized");
        }
        // A non-truthy value falls through to the /.dockerenv probe rather than
        // asserting "not a container" — on a real container host the file is
        // the authority, so only assert the negative when it is absent.
        unsafe { std::env::set_var("DUDUCLAW_IN_CONTAINER", "0") };
        if !std::path::Path::new("/.dockerenv").exists() {
            assert!(!is_containerized());
        }
        match prev {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_IN_CONTAINER", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_IN_CONTAINER") },
        }
    }

    #[tokio::test]
    async fn install_verified_binary_refuses_inside_container() {
        let prev = std::env::var("DUDUCLAW_IN_CONTAINER").ok();
        unsafe { std::env::set_var("DUDUCLAW_IN_CONTAINER", "1") };
        // Garbage bytes: the container gate must fire BEFORE any extraction, so
        // the error is the container refusal, never "binary not found in archive".
        let err = install_verified_binary(b"not an archive").await.unwrap_err();
        match prev {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_IN_CONTAINER", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_IN_CONTAINER") },
        }
        assert!(err.contains("容器"), "unexpected error: {err}");
        assert!(err.contains("image"), "should point at the image update path: {err}");
    }

    #[test]
    fn verify_archive_with_pubkey_is_unchanged_for_the_ce_key() {
        // Signature is valid for exactly b"FAKE_BINARY" under UPDATE_PUBKEY.
        let good = b"FAKE_BINARY";
        let good_sum = format!("{:x}", Sha256::digest(good));
        let sidecar = format!("{good_sum}  duduclaw.tar.gz\n");
        assert!(verify_archive_with_pubkey(good, &sidecar, TEST_SIG, UPDATE_PUBKEY).is_ok());

        // Same three failure shapes as the classified CE wrapper, fail-closed.
        assert!(
            verify_archive_with_pubkey(good, &"a".repeat(64), TEST_SIG, UPDATE_PUBKEY)
                .unwrap_err()
                .contains("checksum mismatch")
        );
        assert!(
            verify_archive_with_pubkey(good, "no digest here", TEST_SIG, UPDATE_PUBKEY).is_err()
        );
        let tampered = b"FAKE_BINARY_TAMPERED";
        let tampered_sum = format!("{:x}", Sha256::digest(tampered));
        assert!(
            verify_archive_with_pubkey(tampered, &tampered_sum, TEST_SIG, UPDATE_PUBKEY)
                .unwrap_err()
                .contains("FAILED")
        );
    }

    #[test]
    fn verify_archive_with_pubkey_rejects_a_foreign_key() {
        // A signature that verifies under the CE key must NOT verify under a
        // different (well-formed) key — the whole point of parameterizing it is
        // that the two channels' trust roots stay separate.
        let good = b"FAKE_BINARY";
        let good_sum = format!("{:x}", Sha256::digest(good));
        let sidecar = format!("{good_sum}  duduclaw.tar.gz\n");
        // Same key material with one flipped base64 char → valid shape, wrong key.
        let mut other = UPDATE_PUBKEY.to_string();
        other.replace_range(10..11, if &UPDATE_PUBKEY[10..11] == "A" { "B" } else { "A" });
        assert!(verify_archive_with_pubkey(good, &sidecar, TEST_SIG, &other).is_err());
        // A malformed key is an error, never a pass.
        assert!(verify_archive_with_pubkey(good, &sidecar, TEST_SIG, "not-a-key").is_err());
    }

    #[test]
    fn desktop_bundle_paths_detected() {
        // Pure-path check mirrors detect_install_method's first rule.
        assert!("/Applications/DuDuClaw.app/Contents/MacOS/duduclaw".contains(".app/Contents/"));
        assert!(!"/Users/x/.nvm/versions/node/v24/lib/node_modules/duduclaw/bin/duduclaw"
            .contains(".app/Contents/"));
    }
}
