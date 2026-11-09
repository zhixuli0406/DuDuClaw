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
//! - [H5] Separate archive, binary and total decompression limits [R2:NC2]
//! - [M1] release_notes truncated to 8 KB
//! - [M3] Pre-release tag tolerance in semver comparison
//! - [M4] Binary replacement runs on the blocking pool (including fsync)
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
/// contains the NEW shared `OFFICIAL_PUBLISHER_KEY` constant.
pub(crate) const UPDATE_PUBKEY: &str = duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY;
#[path = "updater/protected_release.rs"]
mod protected_release;
mod failure;
pub use failure::{failure_kind, failure_payload, failure_response, sanitize_failure_detail};
#[path = "updater/release_identity.rs"]
mod release_identity;
// RM-155: `DuDuClaw Gateway.app` install/update/migration (macOS); the
// strict extractor and swap state machine are tested on every host.
#[path = "updater/macos_bundle.rs"]
mod macos_bundle;
pub use macos_bundle::{FOLDER_ACCESS_NOTICE, take_folder_access_notice};
pub use protected_release::download_protected_release;
pub use release_identity::{VerifiedReleaseIdentity, verify_release_archive_with_pubkey};

/// Current version: prefers build-time `DUDUCLAW_VERSION` env (set by Pro build script),
/// then runtime `DUDUCLAW_VERSION` env, finally falls back to this crate's `CARGO_PKG_VERSION`.
pub fn current_version() -> &'static str {
    release_identity::retain_release_version();
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
    if is_native_protected_install() { return false; }
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
    probe_version(&exe).await
}

/// `<exe> version`, parsed — `None` when it cannot run or prints no version.
async fn probe_version(exe: &std::path::Path) -> Option<String> {
    let exe = exe.to_path_buf();
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

/// Maximum compressed release download (256 MiB).
pub const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum extracted executable (768 MiB).
pub const MAX_BINARY_BYTES: u64 = 768 * 1024 * 1024;
/// Maximum decompressed archive stream, including skipped entries and metadata.
pub const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
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
    /// Root-owned native service; upgrades go through the isolated update service.
    Manual,
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
/// - `"protected"` — the root update service verifies and installs a generation.
pub fn update_channel_label(provider_present: bool) -> &'static str {
    if is_native_protected_install() { return "protected"; }
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
    duduclaw_core::ns_guard::is_containerized()
}

/// This only narrows update permissions; it does not attest credential protection.
pub fn is_native_protected_install() -> bool {
    // SR5 round 3 follow-up: the one protected-mode latch (env flag OR the
    // root marker / confirmed index), never the bare environment variable.
    cfg!(target_os = "linux") && native_protected_install(
        is_containerized(),
        duduclaw_core::protected_mode::active(),
        &duduclaw_core::platform::executable_path(),
    )
}

fn native_protected_install(containerized: bool, protected: bool, exe: &std::path::Path) -> bool {
    !containerized && (protected
        || exe == std::path::Path::new("/opt/duduclaw/bin/duduclaw"))
}

pub const NATIVE_PROTECTED_UPDATE_HINT: &str =
    "此服務由獨立更新服務管理，請從儀表板的系統更新頁操作；無法更新時請管理員檢查更新服務。";

/// Result of applying an update.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub success: bool,
    pub message: String,
    pub needs_restart: bool,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_macos_restart_hint")]
    pub restart_hint: Option<&'static str>,
}

fn serialize_macos_restart_hint<S>(hint: &Option<&'static str>, serializer: S) -> Result<S::Ok, S::Error>
where S: serde::Serializer {
    #[cfg(target_os = "macos")]
    { serde::Serialize::serialize(hint, serializer) }
    #[cfg(not(target_os = "macos"))]
    { let _ = hint; serde::Serialize::serialize(&Option::<&str>::None, serializer) }
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
/// [`fetch_asset_bytes`] for downloads, [`verify_release_archive_with_pubkey`] for
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
        Self {
            class: FailureClass::Transient,
            message: message.into(),
        }
    }
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::Fatal,
            message: message.into(),
        }
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
    if is_native_protected_install() { return InstallMethod::Manual; }
    let exe = duduclaw_core::platform::executable_path();
    if exe.as_os_str().is_empty() {
        return InstallMethod::Unknown;
    }
    // Cached per process; `None` off macOS and for every non-gateway bundle
    // (a `.app` with id ai.duduclaw.desktop — or an unreadable id — stays
    // Desktop through the `.app/Contents/` branch below).
    let gateway_bundle = duduclaw_core::macos_bundle::running_gateway_bundle();
    install_method_for(&exe, gateway_bundle.as_deref(), |path| path.exists())
}

/// Pure half of [`detect_install_method`]: `gateway_bundle` is the root of
/// the `ai.duduclaw.gateway` bundle `exe` runs from, if any.
fn install_method_for(
    exe: &std::path::Path,
    gateway_bundle: Option<&std::path::Path>,
    exists: impl FnMut(&std::path::Path) -> bool,
) -> InstallMethod {
    // Enterprise wrapper: checked BEFORE Desktop, because a `duduclaw-pro`
    // deployed next to a desktop bundle is still a Pro install — the update
    // channel follows the BINARY, not the directory it happens to sit in.
    if is_pro_wrapper_exe(exe) {
        return InstallMethod::Pro;
    }

    // RM-155: `DuDuClaw Gateway.app` (identified by CFBundleIdentifier, not
    // by path) is a self-updating install. Checked BEFORE the `.app/Contents/`
    // Desktop branch; the bundle swap path (`updater/macos_bundle.rs`) keeps
    // its signature seal intact.
    if let Some(root) = gateway_bundle {
        let root = root.to_string_lossy();
        return if root.contains("/node_modules/") { InstallMethod::Npm } else { InstallMethod::Standalone };
    }

    let exe_str = exe.to_string_lossy();

    // Desktop-app sidecar: macOS bundle path, or (Windows/Linux) the Tauri
    // main executable sitting next to us. Windows installs duduclaw-desktop-app.exe
    // as the main executable and duduclaw.exe as the sidecar. Checked FIRST —
    // a sidecar under /Applications would otherwise fall through to Standalone
    // and try to rewrite the signed bundle.
    if exe_str.contains(".app/Contents/") {
        return InstallMethod::Desktop;
    }
    if has_desktop_neighbor(exe, exists) {
        return InstallMethod::Desktop;
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

/// Check desktop main-executable candidates with an injectable existence query.
fn has_desktop_neighbor(
    current_exe: &std::path::Path,
    mut exists: impl FnMut(&std::path::Path) -> bool,
) -> bool {
    const CANDIDATES: &[&str] = &[
        "duduclaw-desktop-app.exe",
        "duduclaw-desktop-app",
        "DuDuClaw.exe",
    ];
    current_exe.parent().is_some_and(|dir| {
        CANDIDATES.iter().any(|name| {
            is_distinct_desktop_neighbor(current_exe, name) && exists(&dir.join(name))
        })
    })
}

/// Whether a desktop main-executable candidate is a different file name from
/// the running updater. Windows file systems normally compare names without
/// case, so a standalone `duduclaw.exe` must not discover itself through the
/// `DuDuClaw.exe` spelling and be misclassified as a Desktop sidecar.
fn is_distinct_desktop_neighbor(current_exe: &std::path::Path, neighbor_name: &str) -> bool {
    current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| !name.eq_ignore_ascii_case(neighbor_name))
}

// ---------------------------------------------------------------------------
// Semver comparison [M3] — strips pre-release suffixes
// ---------------------------------------------------------------------------

fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let s = s.strip_prefix('v').unwrap_or(s);
        let parts: Vec<&str> = s.split('.').collect();
        let parse_component = |p: &str| -> u32 {
            p.split('-')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0)
        };
        let major = parts.first().map(|p| parse_component(p)).unwrap_or(0);
        let minor = parts.get(1).map(|p| parse_component(p)).unwrap_or(0);
        let patch = parts.get(2).map(|p| parse_component(p)).unwrap_or(0);
        (major, minor, patch)
    };
    parse(latest) > parse(current)
}

/// Why an archive is being installed — decides whether the version that is
/// already running may be installed again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallIntent {
    /// Regular update: strictly newer than the running AND the on-disk version.
    Update,
    /// Explicit bare → `DuDuClaw Gateway.app` migration: the same version as
    /// the running bare binary is allowed (it is what migration installs).
    MigrateToBundle,
}

/// Replay/downgrade gate: a validly signed OLD release must never be
/// installable. `target` must be newer than the running version and the
/// version on disk (when known); equal only when `allow_equal` (migration).
/// A running pre-release (`1.81.0-rc.1`) is older than its final release.
fn check_version_floor(target: &str, running: &str, on_disk: Option<&str>, allow_equal: bool) -> Result<(), String> {
    let key = |v: &str, what: &str| -> Result<(u32, u32, u32, bool), String> {
        let (a, b, c) = parse_version_tag(v)
            .ok_or_else(|| format!("Update verification failed: cannot parse the {what} version {v:?}"))?;
        Ok((a, b, c, !v.contains('-')))
    };
    let wanted = key(target, "release")?;
    for (what, version) in [("running", Some(running)), ("installed", on_disk)] {
        let Some(version) = version else { continue };
        let have = key(version, what)?;
        if wanted < have {
            return Err(format!(
                "Update verification failed: refusing downgrade — release v{target} is older than the {what} v{version}"
            ));
        }
        if wanted == have && !allow_equal {
            return Err(format!(
                "Update verification failed: refusing to reinstall v{target} — it is already the {what} version"
            ));
        }
    }
    Ok(())
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
    {
        "darwin-arm64.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x64.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x64.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-arm64.tar.gz"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x64.zip"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        "unknown"
    }
}

/// RM-155: suffix of the Developer ID-signed `DuDuClaw Gateway.app` asset
/// (`duduclaw[-pro]-darwin-<arch>.app.zip`). macOS only — no other platform
/// ever selects or accepts a bundle asset.
fn bundle_asset_suffix() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("darwin-arm64.app.zip")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("darwin-x64.app.zip")
    }
    #[cfg(not(all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64"))))]
    {
        None
    }
}

/// Which release asset this install updates from.
///
/// - Running from `DuDuClaw Gateway.app`: the `.app.zip` only. A bare tarball
///   would rewrite `Contents/MacOS/*` and break the bundle's seal, so a
///   release without the bundle asset offers no URL (fail closed).
/// - Migrating a bare install (`want_bundle`): the `.app.zip`, falling back to
///   nothing — migration never installs a bare binary.
/// - Bare macOS / every other platform: the platform tarball/zip, as before
///   (old updaters and npm keep consuming it).
fn select_update_asset<'a>(
    assets: &'a [GitHubAsset],
    archive_suffix: &str,
    bundle_suffix: Option<&str>,
    want_bundle: bool,
) -> Option<&'a GitHubAsset> {
    if want_bundle {
        let suffix = bundle_suffix?;
        return assets.iter().find(|a| a.name.ends_with(suffix));
    }
    assets.iter().find(|a| a.name.ends_with(archive_suffix))
}

fn binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "duduclaw.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "duduclaw"
    }
}

/// The archive entry to install over `current_exe`. The Pro control-plane
/// asset (`duduclaw-pro-<plat>.tar.gz`, packaged by `scripts/release.sh`)
/// holds `duduclaw-pro`, not the CE `duduclaw`; looking for the CE name made
/// every Pro self-update fail with "Binary 'duduclaw' not found in archive"
/// (2026-10-27 field report, v1.70.1 → v1.76.0).
fn archive_binary_name(current_exe: &std::path::Path) -> &'static str {
    if !is_pro_wrapper_exe(current_exe) {
        return binary_name();
    }
    #[cfg(target_os = "windows")]
    {
        "duduclaw-pro.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "duduclaw-pro"
    }
}

// ---------------------------------------------------------------------------
// URL validation [H2][R2:NC1] — validates both download and checksum URLs
// ---------------------------------------------------------------------------

/// Validate that a URL points to the official GitHub release assets.
pub fn is_valid_download_url(url: &str) -> bool {
    let repo = github_repo();
    let prefix = format!("https://github.com/{repo}/releases/download/");
    url.starts_with(&prefix) && !url.contains("..") && url.len() < 512
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
    let release = fetch_latest_cli_release().await?;
    let latest = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let available = is_newer(current_version(), latest);
    let install_method = detect_install_method();

    let download_url = select_update_asset(
        &release.assets,
        platform_asset_suffix(),
        bundle_asset_suffix(),
        duduclaw_core::macos_bundle::running_gateway_bundle().is_some(),
    )
    .map(|a| a.browser_download_url.clone())
    .unwrap_or_default();

    let checksum_url = if download_url.is_empty() {
        String::new()
    } else {
        format!("{download_url}.sha256")
    };

    let release_notes: String = release
        .body
        .clone()
        .unwrap_or_default()
        .chars()
        .take(MAX_RELEASE_NOTES_CHARS)
        .collect();

    Ok(UpdateInfo {
        available,
        current_version: current_version().to_string(),
        latest_version: latest.to_string(),
        release_notes,
        published_at: release.published_at.clone().unwrap_or_default(),
        download_url,
        checksum_url,
        install_method,
    })
}

/// The latest CLI release (API, rate-limit-immune redirect fallback, and the
/// desktop-tag pairing) — shared by [`check_update`] and [`migrate_to_bundle`].
async fn fetch_latest_cli_release() -> Result<GitHubRelease, String> {
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
    resolve_cli_release(&client, repo, release).await
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
    if is_native_protected_install() { return Err(NATIVE_PROTECTED_UPDATE_HINT.into()); }
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
            "桌面版由 DuDuClaw App 自動更新（重新啟動 App 即會套用新版本），此處不執行覆寫".into(),
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
            restart_hint: None,
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
        return Err(
            "No checksum URL provided — refusing update without integrity verification".into(),
        );
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
    let client = download_client()?;
    download_verify_install(&client, download_url, checksum_url, &current_exe, exe_dir,
        InstallIntent::Update, on_progress).await
}

/// [S1][C1] download (retrying only download-class failures) → minisign +
/// SHA-256 + release identity → install. URLs were validated by the caller.
async fn download_verify_install(
    client: &reqwest::Client,
    download_url: &str,
    checksum_url: &str,
    current_exe: &std::path::Path,
    exe_dir: &std::path::Path,
    intent: InstallIntent,
    on_progress: ProgressSink<'_>,
) -> Result<ApplyResult, String> {
    // [S1] minisign signature URL is derived, not client-supplied — validate
    // it before any network work so a bad shape fails fast, not mid-retry.
    let signature_url = format!("{download_url}.minisig");
    if !is_valid_download_url(&signature_url) {
        return Err(format!("Rejected unsafe signature URL: {signature_url}"));
    }

    // Download + verify, retrying ONLY download-class failures. A checksum or
    // signature failure aborts on the first attempt (fail-closed [S1][C1]).
    let (bytes, identity) = with_retries(
        &DOWNLOAD_RETRY_DELAYS_SECS,
        &|attempt, max_attempts| {
            on_progress(UpdateProgress {
                phase: "downloading",
                attempt,
                max_attempts,
            })
        },
        |_attempt| download_and_verify_once(client, download_url, checksum_url, &signature_url),
    )
    .await
    .map_err(|e| e.message)?;

    install_archive_bytes(&bytes, current_exe, exe_dir, &identity, intent).await
}

/// RM-155: migrate a bare macOS install (standalone / npm) to
/// `DuDuClaw Gateway.app` — the dashboard "migrate" action
/// (`system.migrate_bundle`), `duduclaw service migrate-bundle`, and the
/// one-time automatic migration on the first v1.81 boot under launchd with
/// auto-update enabled. Installs the latest release's signed `.app.zip`
/// through exactly the same verification as an update (minisign + SHA-256 +
/// release identity + pinned code signature), then points the old binary
/// path and the LaunchAgent at the bundle. The caller restarts.
pub async fn migrate_to_bundle() -> Result<ApplyResult, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Err("DuDuClaw Gateway.app migration is macOS-only".into())
    }
    #[cfg(target_os = "macos")]
    {
        if is_native_protected_install() || is_containerized() {
            return Err("This install cannot migrate to DuDuClaw Gateway.app".into());
        }
        if duduclaw_core::macos_bundle::running_gateway_bundle().is_some() {
            return Err("Already running from DuDuClaw Gateway.app".into());
        }
        let current_exe = duduclaw_core::platform::executable_path();
        if current_exe.as_os_str().is_empty() {
            return Err("Cannot determine current binary path".into());
        }
        // The public channel only ships CE; Enterprise migrates through its
        // own update provider (`kind: "app-bundle"`).
        if is_pro_wrapper_exe(&current_exe) {
            return Err("企業版 (duduclaw-pro) 透過企業更新通道遷移到 DuDuClaw Gateway.app".into());
        }
        match detect_install_method() {
            InstallMethod::Standalone | InstallMethod::Npm => {}
            other => {
                return Err(format!(
                    "This install ({other:?}) cannot migrate to DuDuClaw Gateway.app — reinstall with install.sh"
                ));
            }
        }
        if UPDATE_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(UPDATE_BUSY.into());
        }
        let _guard = UpdateGuard;
        let exe_dir = current_exe.parent().ok_or("Cannot determine binary directory")?.to_path_buf();
        let release = fetch_latest_cli_release().await?;
        let latest = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
        if is_newer(latest, current_version()) {
            return Err(format!(
                "Running v{} is newer than the latest release v{latest}; migrate after the next release",
                current_version()
            ));
        }
        let download_url = select_update_asset(&release.assets, platform_asset_suffix(), bundle_asset_suffix(), true)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| format!("Release v{latest} has no DuDuClaw Gateway.app asset"))?;
        if !is_valid_download_url(&download_url) {
            return Err(format!("Rejected unsafe download URL: {download_url}"));
        }
        let checksum_url = format!("{download_url}.sha256");
        if !is_valid_download_url(&checksum_url) {
            return Err(format!("Rejected unsafe checksum URL: {checksum_url}"));
        }
        info!(url = %download_url, "Migrating to DuDuClaw Gateway.app...");
        let client = download_client()?;
        download_verify_install(&client, &download_url, &checksum_url, &current_exe, &exe_dir,
            InstallIntent::MigrateToBundle, &|_| {}).await
    }
}

/// Whether the dashboard should offer `system.migrate_bundle`: a bare
/// standalone/npm CE install on macOS. Always false elsewhere.
pub fn bundle_migration_offered() -> bool {
    cfg!(target_os = "macos")
        && !is_containerized()
        && duduclaw_core::macos_bundle::running_gateway_bundle().is_none()
        && !is_pro_wrapper_exe(&duduclaw_core::platform::executable_path())
        && matches!(detect_install_method(), InstallMethod::Standalone | InstallMethod::Npm)
}

/// Error text when another install holds the update slot.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const UPDATE_BUSY: &str = "Another update is already in progress";

/// RM-155 boot maintenance (macOS; no-op elsewhere): rewrite LaunchAgent
/// plists that reach this executable through another spelling, remove stale
/// `.duduclaw-update-*` staging directories, and — once, on the first boot
/// under launchd with auto-update enabled (owner decision D5=A) — migrate a
/// bare install to `DuDuClaw Gateway.app` and relaunch from it.
pub async fn macos_boot_maintenance(home_dir: PathBuf, auto_update: bool) {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home_dir, auto_update);
    }
    #[cfg(target_os = "macos")]
    {
        let maintenance = tokio::task::spawn_blocking(|| {
            let _ = duduclaw_core::autostart::migrate_launchd_plists();
            let exe = duduclaw_core::platform::executable_path();
            let parent = match duduclaw_core::macos_bundle::running_gateway_bundle() {
                Some(bundle) => bundle.parent().map(std::path::Path::to_path_buf),
                None => dirs::home_dir().and_then(|home| {
                    duduclaw_core::macos_bundle::migration_target_root(&exe, &home)
                        .parent().map(std::path::Path::to_path_buf)
                }),
            };
            let has_staging = |dir: &std::path::Path| std::fs::read_dir(dir).is_ok_and(|entries| {
                entries.flatten().any(|e| e.file_name().to_str()
                    .is_some_and(|n| n.starts_with(duduclaw_core::macos_bundle::STAGING_PREFIX)))
            });
            if let Some(parent) = parent.filter(|p| has_staging(p.as_path())) {
                // Under the cross-process bundle lock: restores a bundle an
                // interrupted swap left only in staging, then removes stale
                // staging. Another process mid-update → try next boot.
                match macos_bundle::lock_bundle_parent(&parent) {
                    Ok(_lock) => {
                        let removed = macos_bundle::cleanup_stale_staging(&parent, &exe,
                            std::time::Duration::from_secs(3600));
                        if removed > 0 {
                            info!(removed, dir = %parent.display(), "Removed stale DuDuClaw Gateway.app staging directories");
                        }
                    }
                    Err(e) => info!(error = %e, dir = %parent.display(), "Skipping DuDuClaw Gateway.app staging recovery"),
                }
            }
        });
        let _ = maintenance.await;

        let launchd_job = std::env::var("XPC_SERVICE_NAME").is_ok_and(|name| {
            name == duduclaw_core::autostart::LAUNCHD_LABEL || name == duduclaw_core::autostart::LEGACY_LAUNCHD_LABEL
        });
        let eligible = auto_update
            && launchd_job
            && duduclaw_core::macos_bundle::running_gateway_bundle().is_none()
            && !is_pro_wrapper_exe(&duduclaw_core::platform::executable_path())
            && matches!(detect_install_method(), InstallMethod::Standalone | InstallMethod::Npm);
        let marker = home_dir.join("state").join("macos_bundle_migration.json");
        if !eligible || marker.exists() {
            return;
        }
        // Before the auto-update loop's first check (30 s) so the two do not
        // race for the update slot.
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let outcome = migrate_to_bundle().await;
        if matches!(&outcome, Err(e) if e == UPDATE_BUSY) {
            return; // try again next boot
        }
        // One automatic attempt per install, success or not; afterwards the
        // dashboard action / `duduclaw service migrate-bundle` remain.
        let record = serde_json::json!({
            "version": current_version(),
            "ok": outcome.as_ref().is_ok_and(|r| r.success),
            "error": outcome.as_ref().err().map(|e| sanitize_failure_detail(e)),
        });
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, record.to_string());
        match outcome {
            Ok(result) if result.success && result.needs_restart => {
                info!("Migrated to DuDuClaw Gateway.app — relaunching from the bundle");
                restart_after_update().await;
            }
            Ok(result) => warn!(msg = %sanitize_failure_detail(&result.message), "DuDuClaw Gateway.app migration skipped"),
            Err(e) => warn!(error = %sanitize_failure_detail(&e), "DuDuClaw Gateway.app migration failed; use the dashboard or `duduclaw service migrate-bundle`"),
        }
    }
}

/// [R2:NM2] Release-download client: redirects re-validated against GitHub's
/// release CDNs and this repository only.
fn download_client() -> Result<reqwest::Client, String> {
    // [R2:NM2] Custom redirect policy — re-validate redirect targets
    let ver = current_version();
    let ua = format!("{}/{ver}", "duduclaw");
    reqwest::Client::builder()
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
        .map_err(|e| format!("HTTP client error: {e}"))
}

/// Install an already-verified archive over the running binary: extract →
/// size-check → temp file → verify-run → atomic swap → cleanup.
///
/// Callers own integrity verification and the environment gates (container,
/// directory permissions); this is only the swap machine. Shared by the CE
/// GitHub path and by [`install_verified_binary`] so a provider cannot end up
/// with a subtly different install sequence.
///
/// This is the single choke point every install goes through, so the
/// replay/downgrade gate lives here: the signed release version must be newer
/// than both the running version and the version of the binary on disk
/// ([`check_version_floor`]); the same version only for an explicit
/// [`InstallIntent::MigrateToBundle`] of a bare install to the `.app.zip`.
async fn install_archive_bytes(
    archive_bytes: &[u8],
    current_exe: &std::path::Path,
    exe_dir: &std::path::Path,
    identity: &VerifiedReleaseIdentity,
    intent: InstallIntent,
) -> Result<ApplyResult, String> {
    identity.check_archive(archive_bytes)
        .map_err(|e| format!("Update verification failed: {e}"))?;
    let migrating = intent == InstallIntent::MigrateToBundle;
    if migrating && !(release_identity::is_bundle_file(identity.file())
        && duduclaw_core::macos_bundle::running_gateway_bundle().is_none())
    {
        return Err("Update verification failed: bundle migration requires the .app.zip asset and a bare install".into());
    }
    let on_disk = probe_version(current_exe).await;
    check_version_floor(identity.version(), current_version(), on_disk.as_deref(), migrating)?;
    install_floor_checked(archive_bytes, current_exe, exe_dir, identity, migrating).await
}

/// [`install_archive_bytes`] after the version floor passed (tests call this
/// directly to exercise later stages with older signed fixtures).
async fn install_floor_checked(
    archive_bytes: &[u8],
    current_exe: &std::path::Path,
    exe_dir: &std::path::Path,
    identity: &VerifiedReleaseIdentity,
    allow_equal: bool,
) -> Result<ApplyResult, String> {
    identity.check_archive(archive_bytes)
        .map_err(|e| format!("Update verification failed: {e}"))?;
    // RM-155: a verified `.app.zip` goes through the bundle pipeline
    // (strict unzip → codesign gate → atomic directory swap).
    if release_identity::is_bundle_file(identity.file()) {
        #[cfg(target_os = "macos")]
        {
            let (result, installed) =
                macos_bundle::install_bundle_archive(archive_bytes, current_exe, identity, allow_equal).await?;
            set_installed_exe(installed);
            return Ok(result);
        }
        #[cfg(not(target_os = "macos"))]
        return Err("App bundle updates are macOS-only".into());
    }
    let _ = allow_equal; // the bundle pipeline re-checks an adopted bundle's version
    // Writing a bare binary into `Contents/MacOS/` would break the bundle's
    // signature seal (and TCC's identity for it).
    if duduclaw_core::macos_bundle::running_gateway_bundle().is_some() {
        return Err("DuDuClaw Gateway.app only updates from the signed .app.zip release asset".into());
    }
    // Stream into a same-directory, exclusively created staging file; never buffer the binary.
    let mut builder = tempfile::Builder::new();
    builder.prefix(".duduclaw-update-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o755));
    }
    let mut staged = builder.tempfile_in(exe_dir)
        .map_err(|e| format!("Failed to stage binary: {e}"))?;
    extract_binary_to(archive_bytes, archive_binary_name(current_exe), staged.as_file_mut())
        .map_err(|e| format!("Update extraction failed: {e}"))?;
    staged.as_file().sync_all().map_err(|e| format!("Failed to sync binary: {e}"))?;
    let tmp_path = staged.into_temp_path();

    // [R2:NH4] From here on, any error must clean up tmp_path
    let result = apply_update_inner(&tmp_path, current_exe, exe_dir, identity.version()).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }
    result
}

/// Install a verified update archive over the running binary — the install half
/// of an [`UpdateProvider`], shared with the CE path so both go through exactly
/// one swap implementation.
///
/// `identity` must come from [`verify_release_archive_with_pubkey`] using the
/// provider's pinned key and expected release version/file. It is bound to the
/// archive digest; another archive cannot reuse it. Binary version verification
/// is mandatory before backup or replacement.
///
/// Two environment gates fire first, both fail-closed:
/// 1. **Container** — an image is immutable, so an in-process swap is silently
///    reverted the next time the container is recreated. Refused outright; those
///    deployments update by pulling a new image.
/// 2. **Directory permissions** — the same [`duduclaw_core::platform::is_unsafe_update_dir`]
///    gate the CE path applies, so a world-writable install directory cannot be
///    used to land a binary.
///
/// The signed version must be newer than the running and the on-disk version
/// (downgrade / replay gate in [`install_archive_bytes`]).
pub async fn install_verified_binary(archive_bytes: &[u8], identity: &VerifiedReleaseIdentity) -> Result<ApplyResult, String> {
    install_verified_with_intent(archive_bytes, identity, InstallIntent::Update).await
}

/// [`install_verified_binary`] for the explicit bare → `DuDuClaw Gateway.app`
/// migration (a provider's `.app.zip` offer to a bare install). The only
/// provider path that may install the version that is already running;
/// anything older is still refused, and the archive must be the bundle asset.
pub async fn install_verified_bundle_migration(archive_bytes: &[u8], identity: &VerifiedReleaseIdentity) -> Result<ApplyResult, String> {
    install_verified_with_intent(archive_bytes, identity, InstallIntent::MigrateToBundle).await
}

async fn install_verified_with_intent(
    archive_bytes: &[u8],
    identity: &VerifiedReleaseIdentity,
    intent: InstallIntent,
) -> Result<ApplyResult, String> {
    if is_native_protected_install() { return Err(NATIVE_PROTECTED_UPDATE_HINT.into()); }
    if is_containerized() {
        return Err(
            "容器內不執行 binary 換裝（image 不可變，換裝會在重建容器時還原）——請改用 image 更新流程"
                .into(),
        );
    }

    // [C2] Same concurrency guard as the CE path: a provider install must not
    // overlap another install or a `system.restart_pending_update` restart.
    if UPDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Another update is already in progress".into());
    }
    let _guard = UpdateGuard;

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

    install_archive_bytes(archive_bytes, &current_exe, exe_dir, identity, intent).await
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
        check_size(len, MAX_DOWNLOAD_BYTES, what).map_err(StageError::fatal)?;
    }

    let mut resp = resp;
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await
        .map_err(|e| StageError::transient(format!("Failed to read {what}: {e}")))? {
        if chunk.len() as u64 > MAX_DOWNLOAD_BYTES.saturating_sub(bytes.len() as u64) {
            return Err(StageError::fatal(format!("{what} exceeded size limit")));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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
#[cfg(test)]
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
) -> Result<(Vec<u8>, VerifiedReleaseIdentity), StageError> {
    let (version, file) = release_identity::release_identity(download_url)
        .map_err(|e| StageError::fatal(format!("Update verification failed: {e}")))?;
    if checksum_url != format!("{download_url}.sha256") || signature_url != format!("{download_url}.minisig") {
        return Err(StageError::fatal("Update verification failed: Release sidecars must match the archive URL"));
    }
    let bytes = fetch_asset_bytes(client, download_url, "Update archive").await?;
    info!(size = bytes.len(), "Download complete");

    // (empty checksum_url already rejected at apply_update entry [R3:H1])
    info!("Verifying SHA-256 checksum...");
    let checksum_bytes = fetch_asset_bytes(client, checksum_url, "Checksum file").await?;
    let checksum_text = String::from_utf8_lossy(&checksum_bytes).into_owned();

    info!("Verifying Ed25519 signature...");
    let sig_bytes = fetch_asset_bytes(client, signature_url, "Signature file").await?;
    let sig_text = String::from_utf8_lossy(&sig_bytes).into_owned();

    let identity = verify_release_archive_with_pubkey(&bytes, &checksum_text, &sig_text,
        UPDATE_PUBKEY, &version, &file)
        .map_err(|e| StageError::fatal(format!("Update verification failed: {e}")))?;
    Ok((bytes, identity))
}

/// Inner apply logic after tmp file is written. Caller cleans up tmp on error.
async fn apply_update_inner(
    tmp_path: &std::path::Path,
    current_exe: &std::path::Path,
    _exe_dir: &std::path::Path,
    expected_version: &str,
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
            release_identity::verify_binary_version_output(&version_out, expected_version)
                .map_err(|e| format!("Update verification failed: {e}"))?;
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

    // Flush permissions as well as contents, preserve the old binary, then
    // replace the install path without first removing it on Unix.
    let staged = tmp_path.to_path_buf();
    let target = current_exe.to_path_buf();
    tokio::task::spawn_blocking(move || {
        duduclaw_core::binary_replace::replace_verified_binary(&staged, &target)
    })
    .await
    .map_err(|e| format!("Binary replacement task panicked: {e}"))??;

    info!("Update installed successfully, restart required");

    Ok(ApplyResult {
        success: true,
        message: "Update installed. The service will restart automatically.".into(),
        needs_restart: true,
        restart_hint: macos_launchd_restart_hint(),
    })
}

/// Executable the last successful install put in place, when it differs from
/// this process's own path (bare → `DuDuClaw Gateway.app` migration).
static INSTALLED_EXE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn set_installed_exe(path: PathBuf) {
    if let Ok(mut slot) = INSTALLED_EXE.lock() {
        *slot = Some(path);
    }
}

/// The executable the relaunch must start: the freshly installed one, or
/// (no install recorded) this process's own canonical path.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn installed_exe() -> PathBuf {
    INSTALLED_EXE.lock().ok().and_then(|slot| slot.clone())
        .unwrap_or_else(duduclaw_core::platform::executable_path)
}

/// Called only after a successful verified install (or the explicit on-disk
/// update restart gate). A launchd helper owns shutdown; never exec on failure.
pub async fn restart_after_update() {
    #[cfg(target_os = "macos")]
    match duduclaw_core::autostart::update_restart::schedule_if_managed_for(
        detect_install_method() == InstallMethod::Desktop,
        &installed_exe(),
    ).await {
        Ok(true) => {
            info!("Update installed — independent launchd helper scheduled to re-register and restart gateway");
            // If launchd accepts the plist but cannot execute the new binary,
            // no helper can write its own failure log. This old process remains
            // alive and supplies the manual recovery hint after the deadline.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(100)).await;
                warn!("Update installed successfully, restart required; launchd relaunch did not stop this gateway; run `duduclaw service restart`");
            });
            return;
        }
        Err(error) => {
            warn!(%error, "Update installed successfully, restart required; run `duduclaw service restart`");
            return;
        }
        Ok(false) => {}
    }
    duduclaw_core::platform::request_restart_after_shutdown();
    duduclaw_core::platform::self_interrupt();
}

pub fn macos_launchd_restart_hint() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().unwrap_or_default();
        let registered = std::env::var_os("XPC_SERVICE_NAME").is_some()
            || [duduclaw_core::autostart::LAUNCHD_LABEL, duduclaw_core::autostart::LEGACY_LAUNCHD_LABEL]
                .iter().any(|label| duduclaw_core::autostart::launchd_plist_path_in(&home, label).exists());
        return registered.then_some("duduclaw service restart");
    }
    #[cfg(not(target_os = "macos"))]
    { None }
}

#[cfg(all(test, unix))]
mod atomic_install_tests {
    use super::*;

    #[tokio::test]
    async fn atomic_install_replaces_verified_binary() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("duduclaw");
        let staged = dir.path().join("new");
        std::fs::write(&target, b"old binary").unwrap();
        let script = b"#!/bin/sh\necho duduclaw 1.80.0\n";
        std::fs::write(&staged, script).unwrap();
        let result = apply_update_inner(&staged, &target, dir.path(), "1.80.0").await.unwrap();
        assert!(result.success && result.needs_restart);
        assert_eq!(std::fs::read(target).unwrap(), script);
        assert!(!staged.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn atomic_install_version_mismatch_reaches_dashboard_without_swap() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("duduclaw");
        let staged = dir.path().join("new");
        std::fs::write(&target, b"old binary").unwrap();
        std::fs::write(&staged, b"#!/bin/sh\necho duduclaw 1.79.3\n").unwrap();
        let error = apply_update_inner(&staged, &target, dir.path(), "1.80.0").await.unwrap_err();
        assert!(error.contains("version mismatch"));
        let payload = failure_payload(&error);
        assert_eq!(payload["kind"], "verify");
        let wire = serde_json::to_value(failure_response(&error)).unwrap();
        assert_eq!(wire["error"], payload);
        let event = crate::protocol::WsFrame::event("system.update_failed", payload.clone());
        assert_eq!(serde_json::to_value(event).unwrap()["payload"], payload);
        assert_eq!(std::fs::read(&target).unwrap(), b"old binary");
        assert!(!target.with_extension("bak").exists());
        assert!(staged.exists());
    }

    #[tokio::test]
    async fn atomic_install_failed_verification_keeps_old_binary() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("duduclaw");
        let staged = dir.path().join("new");
        std::fs::write(&target, b"old binary").unwrap();
        std::fs::write(&staged, b"#!/bin/sh\nexit 1\n").unwrap();
        let error = apply_update_inner(&staged, &target, dir.path(), "1.80.0").await.unwrap_err();
        assert!(error.contains("verification failed"));
        assert_eq!(failure_payload(&error)["kind"], "verify");
        assert_eq!(std::fs::read(target).unwrap(), b"old binary");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

#[cfg(test)]
mod restart_hint_tests {
    #[test]
    fn restart_hint_is_absent_off_macos() {
        #[cfg(not(target_os = "macos"))]
        assert_eq!(super::macos_launchd_restart_hint(), None);
    }
}

/// Claim the update slot for a restart onto the on-disk binary. Never
/// released: the process is about to exit, and no install may start (or be
/// mid-write) underneath it. False when an install or restart already holds it.
pub fn claim_update_slot_for_restart() -> bool {
    UPDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
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

#[cfg(test)]
fn extract_binary_from_archive(archive_bytes: &[u8], target: &str) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    extract_binary_to(archive_bytes, target, &mut output)?;
    Ok(output)
}

fn extract_binary_to(archive_bytes: &[u8], target: &str, output: &mut impl std::io::Write) -> Result<u64, String> {
    check_size(archive_bytes.len() as u64, MAX_DOWNLOAD_BYTES, "Compressed archive")?;
    #[cfg(target_os = "windows")]
    if archive_bytes.starts_with(b"PK") {
        return extract_from_zip_to(archive_bytes, target, output);
    }
    extract_tar_to(archive_bytes, target, output, MAX_BINARY_BYTES, MAX_ARCHIVE_BYTES)
}

#[cfg(test)]
fn extract_from_tar_gz(archive_bytes: &[u8], target: &str) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    extract_tar_to(archive_bytes, target, &mut output, MAX_BINARY_BYTES, MAX_ARCHIVE_BYTES)?;
    Ok(output)
}

fn check_size(size: u64, limit: u64, what: &str) -> Result<(), String> {
    if size > limit { Err(format!("{what} too large: {size} bytes (limit: {limit})")) }
    else { Ok(()) }
}

// Count actual decompression, including tar metadata and skipped entries.
// One extra byte distinguishes an exact boundary from overflow.
struct ArchiveBudget<R> { reader: R, remaining: u64 }
impl<R: std::io::Read> std::io::Read for ArchiveBudget<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() { return Ok(0); }
        let len = buf.len().min(self.remaining.saturating_add(1).min(usize::MAX as u64) as usize);
        let n = self.reader.read(&mut buf[..len])?;
        if n as u64 > self.remaining {
            return Err(std::io::Error::other("Total decompressed archive exceeds size limit"));
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

fn copy_binary(reader: &mut impl std::io::Read, output: &mut impl std::io::Write, limit: u64) -> Result<u64, String> {
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| format!("Archive read failed: {e}"))?;
        if n == 0 { break; }
        total = total.checked_add(n as u64).ok_or("Binary size overflow")?;
        if total > limit { return Err("Extracted binary exceeds size limit".into()); }
        hash.update(&buffer[..n]);
        output.write_all(&buffer[..n]).map_err(|e| format!("Staging write failed: {e}"))?;
    }
    info!(bytes = total, sha256 = %hex::encode(hash.finalize()), "Extracted release binary");
    Ok(total)
}

fn extract_tar_to(archive_bytes: &[u8], target: &str, output: &mut impl std::io::Write,
    binary_limit: u64, archive_limit: u64) -> Result<u64, String> {
    let gz = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(ArchiveBudget { reader: gz, remaining: archive_limit });
    let mut found = None;
    for entry in archive.entries().map_err(|e| format!("Invalid tar.gz: {e}"))? {
        let mut entry = entry.map_err(|e| format!("Archive entry error: {e}"))?;
        let path = entry.path().map_err(|e| format!("Invalid archive path: {e}"))?;
        let safe = !path.is_absolute() && !path.components().any(|c| matches!(c,
            std::path::Component::ParentDir | std::path::Component::RootDir));
        if safe && path.file_name().and_then(|n| n.to_str()) == Some(target)
            && entry.header().entry_type().is_file() {
            if found.is_some() { return Err("Duplicate release binary".into()); }
            if entry.size() > binary_limit { return Err("Extracted binary exceeds size limit".into()); }
            found = Some(copy_binary(&mut entry, output, binary_limit)?);
        }
    }
    // tar stops at its end marker. Drain the decoder as well so trailing data
    // cannot escape the global budget or gzip integrity checks.
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink())
        .map_err(|e| format!("Archive read failed: {e}"))?;
    found.ok_or_else(|| format!("'{target}' not found in tar.gz"))
}

#[cfg(any(target_os = "windows", test))]
fn extract_from_zip_to(archive_bytes: &[u8], target: &str, output: &mut impl std::io::Write) -> Result<u64, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(archive_bytes))
        .map_err(|e| format!("Invalid zip: {e}"))?;
    let mut remaining = MAX_ARCHIVE_BYTES;
    let mut found = None;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("Zip entry error: {e}"))?;
        let name = file.name().map_err(|e| format!("Zip name error: {e}"))?.into_owned();
        let selected = !name.contains("..") && !name.starts_with('/') && !name.starts_with('\\')
            && std::path::Path::new(&name).file_name().and_then(|n| n.to_str()) == Some(target);
        if selected && !file.is_file() { return Err("Release binary is not a regular file".into()); }
        if file.size() > remaining { return Err("Total decompressed archive exceeds size limit".into()); }
        let mut reader = ArchiveBudget { reader: &mut file, remaining };
        if selected {
            if found.is_some() { return Err("Duplicate release binary".into()); }
            found = Some(copy_binary(&mut reader, output, MAX_BINARY_BYTES)?);
        } else {
            std::io::copy(&mut reader, &mut std::io::sink()).map_err(|e| format!("Zip read failed: {e}"))?;
        }
        remaining = reader.remaining;
    }
    found.ok_or_else(|| format!("'{target}' not found in zip"))
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
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | 'v'))
    {
        return None;
    }
    Some(tag.to_string())
}

/// Synthesize a release record with deterministic asset URLs
/// (`releases/download/<tag>/duduclaw-<suffix>`) for paths where the real
/// release object is unavailable (rate-limited API). Release notes / publish
/// date are unavailable here — empty, never fabricated.
fn synthesize_release(repo: &str, tag: &str) -> GitHubRelease {
    let assets = std::iter::once(platform_asset_suffix())
        .chain(bundle_asset_suffix())
        .map(|suffix| {
            let name = format!("duduclaw-{suffix}");
            let browser_download_url = format!("https://github.com/{repo}/releases/download/{tag}/{name}");
            GitHubAsset { name, browser_download_url }
        })
        .collect();
    GitHubRelease {
        tag_name: tag.to_string(),
        body: None,
        published_at: None,
        assets,
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
        .ok_or_else(|| {
            format!(
                "releases/latest returned {} without a redirect",
                resp.status()
            )
        })?;
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
        assert!(!is_valid_download_url(
            "https://github.com/attacker/repo/releases/download/v1/x.tar.gz"
        ));
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
    fn native_protected_update_is_manual() {
        let native = std::path::Path::new("/opt/duduclaw/bin/duduclaw");
        let other = std::path::Path::new("/usr/bin/duduclaw");
        // SR5 round 3 follow-up: `protected` is `protected_mode::active()`
        // (env flag OR durable fact); the executable path stays a fallback.
        assert!(native_protected_install(false, false, native));
        assert!(native_protected_install(false, true, other));
        assert!(!native_protected_install(true, true, native));
        assert!(!native_protected_install(false, false, other));
        assert_eq!(serde_json::to_string(&InstallMethod::Manual).unwrap(), "\"manual\"");
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
        assert_eq!(bundle_asset_suffix().is_some(), cfg!(target_os = "macos"));
    }

    #[test]
    fn gateway_bundle_is_detected_by_id_before_the_desktop_branch() {
        use std::path::Path;
        let none = |_: &Path| false;
        let app = Path::new("/Users/k/Applications/DuDuClaw Gateway.app");
        let exe = Path::new("/Users/k/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw");
        assert_eq!(install_method_for(exe, Some(app), none), InstallMethod::Standalone);
        let npm = Path::new("/opt/homebrew/lib/node_modules/@duduclaw/darwin-arm64/DuDuClaw Gateway.app");
        assert_eq!(install_method_for(&npm.join("Contents/MacOS/duduclaw"), Some(npm), none), InstallMethod::Npm);
        // Same layout without the gateway id (desktop app, unreadable id) stays Desktop.
        assert_eq!(install_method_for(exe, None, none), InstallMethod::Desktop);
        assert_eq!(install_method_for(Path::new("/Applications/DuDuClaw.app/Contents/MacOS/duduclaw"), None, none),
            InstallMethod::Desktop);
        // Pro is still Pro inside the bundle (updates via its provider).
        assert_eq!(install_method_for(&app.join("Contents/MacOS/duduclaw-pro"), Some(app), none), InstallMethod::Pro);
        assert_eq!(install_method_for(Path::new("/Users/k/.duduclaw/bin/duduclaw"), None, none), InstallMethod::Standalone);
    }

    #[test]
    fn bundle_installs_only_take_the_app_zip_asset() {
        let asset = |name: &str| GitHubAsset { name: name.into(), browser_download_url: format!("https://x/{name}") };
        let both = [asset("duduclaw-darwin-arm64.tar.gz"), asset("duduclaw-darwin-arm64.app.zip")];
        let tar_only = [asset("duduclaw-darwin-arm64.tar.gz")];
        let pick = |assets: &[GitHubAsset], bundle: bool| {
            select_update_asset(assets, "darwin-arm64.tar.gz", Some("darwin-arm64.app.zip"), bundle).map(|a| a.name.clone())
        };
        assert_eq!(pick(&both, true).as_deref(), Some("duduclaw-darwin-arm64.app.zip"));
        assert_eq!(pick(&both, false).as_deref(), Some("duduclaw-darwin-arm64.tar.gz"));
        assert_eq!(pick(&tar_only, false).as_deref(), Some("duduclaw-darwin-arm64.tar.gz"));
        // A bundle never falls back to the bare tarball (it would break the seal).
        assert_eq!(pick(&tar_only, true), None);
        // No bundle suffix (non-macOS) never yields a bundle.
        assert!(select_update_asset(&both, "linux-x64.tar.gz", None, true).is_none());
    }

    #[test]
    fn windows_standalone_name_is_not_its_own_desktop_neighbor() {
        assert!(!is_distinct_desktop_neighbor(
            std::path::Path::new("C:/tools/duduclaw.exe"),
            "DuDuClaw.exe"
        ));
        assert!(is_distinct_desktop_neighbor(
            std::path::Path::new("C:/Program Files/DuDuClaw/duduclaw-sidecar.exe"),
            "DuDuClaw.exe"
        ));
    }

    #[test]
    fn windows_desktop_sidecar_has_desktop_neighbor() {
        use std::path::Path;
        let existing = [
            Path::new("C:/Program Files/DuDuClaw/duduclaw-desktop-app.exe"),
            Path::new("C:/Program Files/DuDuClaw/duduclaw.exe"),
        ];
        assert!(has_desktop_neighbor(
            Path::new("C:/Program Files/DuDuClaw/duduclaw.exe"),
            |path| existing.contains(&path)
        ));
    }

    #[test]
    fn windows_standalone_has_no_desktop_neighbor_on_case_insensitive_filesystem() {
        use std::ffi::OsStr;
        use std::path::Path;
        let exe = Path::new("C:/Users/x/.duduclaw/bin/duduclaw.exe");
        // Simulate NTFS reporting DuDuClaw.exe as existing through the sidecar itself.
        let exists = |path: &Path| {
            path.parent() == exe.parent()
                && matches!(
                    (
                        path.file_name().and_then(OsStr::to_str),
                        exe.file_name().and_then(OsStr::to_str)
                    ),
                    (Some(candidate), Some(exe_name))
                        if candidate.eq_ignore_ascii_case(exe_name)
                )
        };
        assert!(exists(&exe.parent().unwrap().join("DuDuClaw.exe")));
        assert!(!has_desktop_neighbor(exe, exists));
    }

    #[test]
    fn linux_desktop_sidecar_has_desktop_neighbor() {
        use std::path::Path;
        assert!(has_desktop_neighbor(
            Path::new("/opt/duduclaw/duduclaw"),
            |path| path == Path::new("/opt/duduclaw/duduclaw-desktop-app")
        ));
    }

    #[test]
    fn test_binary_name() {
        #[cfg(target_os = "windows")]
        assert_eq!(binary_name(), "duduclaw.exe");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(binary_name(), "duduclaw");
    }

    #[test]
    fn test_archive_binary_name_follows_running_exe() {
        use std::path::Path;
        assert_eq!(
            archive_binary_name(Path::new("/usr/local/bin/duduclaw")),
            binary_name()
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            archive_binary_name(Path::new("/usr/local/bin/duduclaw-pro")),
            "duduclaw-pro"
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            archive_binary_name(Path::new(r"C:\DuDuClaw\duduclaw-pro.exe")),
            "duduclaw-pro.exe"
        );
    }

    /// The Pro asset layout from `scripts/release.sh`
    /// (`tar czf … duduclaw-pro`) must extract when running as the wrapper.
    #[test]
    fn test_extract_pro_archive_layout() {
        use std::io::Write;

        let mut tar_builder = tar::Builder::new(Vec::new());
        let content = b"FAKE_PRO_BINARY";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "duduclaw-pro", &content[..])
            .unwrap();
        let tar_bytes = tar_builder.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&tar_bytes).unwrap();
        let gz_bytes = enc.finish().unwrap();

        let pro_exe = std::path::Path::new("/usr/local/bin/duduclaw-pro");
        let target = archive_binary_name(pro_exe);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            extract_binary_from_archive(&gz_bytes, target).unwrap(),
            b"FAKE_PRO_BINARY"
        );
        // The CE name is not in a Pro archive — the pre-fix failure.
        assert!(extract_binary_from_archive(&gz_bytes, binary_name()).is_err());
    }

    #[test]
    fn updater_size_boundaries() {
        use std::io::Read;
        assert_eq!(MAX_DOWNLOAD_BYTES, 256 * 1024 * 1024);
        assert_eq!(MAX_BINARY_BYTES, 768 * 1024 * 1024);
        assert_eq!(MAX_ARCHIVE_BYTES, 1024 * 1024 * 1024);
        for limit in [MAX_DOWNLOAD_BYTES, MAX_BINARY_BYTES, MAX_ARCHIVE_BYTES] {
            for size in [limit - 1, limit, limit + 1] {
                assert_eq!(check_size(size, limit, "test").is_ok(), size <= limit);
            }
        }
        for len in [7, 8, 9] {
            let bytes = vec![42; len];
            let mut output = Vec::new();
            assert_eq!(copy_binary(&mut bytes.as_slice(), &mut output, 8).is_ok(), len <= 8);
            let mut budget = ArchiveBudget { reader: bytes.as_slice(), remaining: 8 };
            assert_eq!(budget.read_to_end(&mut Vec::new()).is_ok(), len <= 8);
        }
    }

    #[tokio::test]
    async fn updater_install_failure_reaches_dashboard_payload() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("duduclaw");
        let (archive, identity) = release_identity::tests::verified_fixture();
        // A mismatched proof must fail before even trying to create a staging file.
        let missing_dir = dir.path().join("missing");
        let error = install_archive_bytes(b"not a gzip archive", &target, &missing_dir, &identity,
            InstallIntent::Update).await.unwrap_err();
        assert_eq!(failure_payload(&error)["kind"], "verify");
        assert!(error.contains("different archive"));
        let payload = failure_payload(&error);
        assert_eq!(serde_json::to_value(failure_response(&error)).unwrap()["error"], payload);
        let event = crate::protocol::WsFrame::event("system.update_failed", payload.clone());
        assert_eq!(serde_json::to_value(event).unwrap()["payload"], payload);
        assert!(!missing_dir.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        let corrupt = extract_binary_to(b"not a gzip archive", binary_name(), &mut Vec::new())
            .unwrap_err();
        assert_eq!(failure_payload(&format!("Update extraction failed: {corrupt}"))["kind"], "extract");
        // The verified CE archive has no Pro entry: retain the install-path
        // extraction failure and staging cleanup coverage behind the identity gate.
        let pro_target = dir.path().join("duduclaw-pro");
        // The fixture is an OLD signed release: past the version floor only
        // through the inner stage (the floor itself is tested separately).
        let error = install_floor_checked(archive, &pro_target, dir.path(), &identity, false)
            .await.unwrap_err();
        let payload = failure_payload(&error);
        assert_eq!(payload["kind"], "extract");
        assert_eq!(payload["detail"], error);
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        let missing_dir = dir.path().join("missing");
        let error = install_floor_checked(archive, &target, &missing_dir, &identity, false).await.unwrap_err();
        assert_eq!(failure_payload(&error)["kind"], "write");
        assert!(error.starts_with("Failed to stage binary:"));
    }

    #[test]
    fn version_floor_refuses_downgrades_and_same_version_reinstalls() {
        // Newer than both: allowed.
        assert!(check_version_floor("1.82.0", "1.81.0", Some("1.81.0"), false).is_ok());
        assert!(check_version_floor("1.82.0", "1.81.0", None, false).is_ok());
        // Older than the running version (signed-archive replay): refused,
        // even for migration.
        for allow_equal in [false, true] {
            let e = check_version_floor("1.80.0", "1.81.0", None, allow_equal).unwrap_err();
            assert!(e.contains("refusing downgrade") && e.contains("running"), "{e}");
        }
        // Older than what is already on disk (restart pending): refused.
        let e = check_version_floor("1.81.5", "1.81.0", Some("1.82.0"), false).unwrap_err();
        assert!(e.contains("refusing downgrade") && e.contains("installed"), "{e}");
        assert!(check_version_floor("1.81.5", "1.81.0", Some("1.82.0"), true).is_err());
        // Same version: only the explicit migration may reinstall it.
        assert!(check_version_floor("1.81.0", "1.81.0", Some("1.81.0"), false).unwrap_err().contains("reinstall"));
        assert!(check_version_floor("1.81.0", "1.81.0", Some("1.81.0"), true).is_ok());
        assert!(check_version_floor("1.81.0", "1.80.0", Some("1.81.0"), false).is_err());
        // A running pre-release is older than its final release.
        assert!(check_version_floor("1.81.0", "1.81.0-rc.1", None, false).is_ok());
        assert!(check_version_floor("1.80.9", "1.81.0-rc.1", None, false).is_err());
        // Numeric, not lexical.
        assert!(check_version_floor("1.10.0", "1.9.9", None, false).is_ok());
        // Unparseable versions fail closed.
        assert!(check_version_floor("1.81.0", "dev", None, false).is_err());
        assert!(check_version_floor("1.81.0", "1.80.0", Some("garbage"), false).is_err());
    }

    #[tokio::test]
    async fn install_refuses_a_validly_signed_old_release_before_staging() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("duduclaw");
        std::fs::write(&target, b"installed binary").unwrap();
        // Signed (test key) for v1.80.0 — older than this crate's running version.
        let (archive, identity) = release_identity::tests::verified_fixture();
        assert!(is_newer(identity.version(), current_version()), "fixture must predate the running version");
        for intent in [InstallIntent::Update, InstallIntent::MigrateToBundle] {
            let error = install_archive_bytes(archive, &target, dir.path(), &identity, intent)
                .await.unwrap_err();
            assert!(error.starts_with("Update verification failed:"), "{error}");
            assert_eq!(failure_payload(&error)["kind"], "verify");
            assert_eq!(std::fs::read(&target).unwrap(), b"installed binary");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "nothing staged");
        }
        // Migration intent is only valid for the bundle asset.
        let error = install_archive_bytes(archive, &target, dir.path(), &identity,
            InstallIntent::MigrateToBundle).await.unwrap_err();
        assert!(error.contains("bundle migration requires"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_version_reads_the_on_disk_binary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("duduclaw");
        std::fs::write(&exe, b"#!/bin/sh\necho 'starting'\necho 'duduclaw 9.9.9'\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(probe_version(&exe).await.as_deref(), Some("9.9.9"));
        assert_eq!(probe_version(&dir.path().join("missing")).await, None);
    }

    #[test]
    fn updater_write_failure_reaches_dashboard_payload() {
        struct DiskFull;
        impl std::io::Write for DiskFull {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let error = copy_binary(&mut b"binary".as_slice(), &mut DiskFull, 100).unwrap_err();
        assert_eq!(failure_payload(&error)["kind"], "write");
        assert!(failure_payload(&error)["detail"].as_str().unwrap().contains("disk full"));
        let size_error = copy_binary(&mut b"binary".as_slice(), &mut Vec::new(), 1).unwrap_err();
        assert_eq!(failure_payload(&size_error)["kind"], "size");
    }

    #[test]
    fn updater_tar_budget_walks_entries_after_binary() {
        use std::io::Write;
        let mut tar = tar::Builder::new(Vec::new());
        for (name, bytes) in [("duduclaw", b"binary!".as_slice()), ("python/large.py", &[42u8; 2048][..])] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, bytes).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let size = raw.len() as u64;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        for binary_limit in [6, 7, 8] {
            assert_eq!(extract_tar_to(&compressed, "duduclaw", &mut Vec::new(), binary_limit, size).is_ok(), binary_limit >= 7);
        }
        for budget in [size - 1, size, size + 1] {
            assert_eq!(extract_tar_to(&compressed, "duduclaw", &mut Vec::new(), 8, budget).is_ok(), budget >= size);
        }
        assert!(extract_tar_to(&compressed, "duduclaw", &mut Vec::new(), 8, 1024)
            .unwrap_err().contains("size limit"));
        let mut trailing = raw;
        trailing.extend_from_slice(&[42; 1024]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&trailing).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(extract_tar_to(&compressed, "duduclaw", &mut Vec::new(), 8, size)
            .unwrap_err().contains("size limit"));
    }

    #[test]
    fn test_zip_binary_requires_regular_file() {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;
        for kind in ["file", "directory", "symlink"] {
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = SimpleFileOptions::default();
            match kind {
                "directory" => archive.add_directory("duduclaw.exe/", options).unwrap(),
                "symlink" => archive.add_symlink("duduclaw.exe", "elsewhere", options).unwrap(),
                _ => {
                    archive.start_file("duduclaw.exe", options).unwrap();
                    archive.write_all(b"binary").unwrap();
                }
            }
            let bytes = archive.finish().unwrap().into_inner();
            let mut output = Vec::new();
            let result = extract_from_zip_to(&bytes, "duduclaw.exe", &mut output);
            if kind == "file" {
                assert_eq!(result.unwrap(), 6);
                assert_eq!(output, b"binary");
            } else {
                assert!(result.is_err(), "{kind} must be refused");
                assert!(output.is_empty());
            }
        }
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
        tar_builder
            .append_data(&mut header, "duduclaw", &content[..])
            .unwrap();
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
        tar_builder
            .append_link(&mut header, "duduclaw", "/etc/evil")
            .unwrap();
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
        std::fs::write(tmp.join("config.toml"), "[gateway]\nauto_update = true\n").unwrap();
        assert!(auto_update_enabled(&tmp));

        std::fs::write(tmp.join("config.toml"), "[gateway]\nauto_update = false\n").unwrap();
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
            tag_from_latest_location(
                "https://github.com/zhixuli0406/DuDuClaw/releases/tag/v1.46.1"
            ),
            Some("v1.46.1".to_string())
        );
        assert_eq!(
            tag_from_latest_location("/zhixuli0406/DuDuClaw/releases/tag/v2.0.0-rc.1/"),
            Some("v2.0.0-rc.1".to_string())
        );
        // No tag segment / empty / hostile characters → None.
        assert_eq!(
            tag_from_latest_location("https://github.com/x/y/releases"),
            None
        );
        assert_eq!(tag_from_latest_location(".../releases/tag/"), None);
        assert_eq!(
            tag_from_latest_location(".../releases/tag/v1.0.0?x=../../evil"),
            None
        );
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
        assert_eq!(
            parse_cli_version_output("duduclaw v1.46.2"),
            Some("1.46.2".to_string())
        );
        assert_eq!(parse_cli_version_output("no version here"), None);
        // Two-dot requirement rejects filenames like config.toml or bare "1.46".
        assert_eq!(parse_cli_version_output("config.toml 1.46"), None);
    }

    #[test]
    fn synthesize_release_builds_deterministic_urls() {
        let r = synthesize_release("zhixuli0406/DuDuClaw", "v1.46.2");
        assert_eq!(r.tag_name, "v1.46.2");
        // macOS also synthesizes the `.app.zip` bundle asset (RM-155).
        assert_eq!(r.assets.len(), 1 + usize::from(bundle_asset_suffix().is_some()));
        assert!(r.assets[0].browser_download_url.starts_with(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/v1.46.2/duduclaw-"
        ));
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
        assert!(
            verify_archive_integrity(good, &format!("{good_sum}  duduclaw.tar.gz\n"), TEST_SIG)
                .is_ok()
        );

        // Checksum mismatch → fatal, never retried.
        let wrong_sum = "a".repeat(64);
        let err = verify_archive_integrity(good, &wrong_sum, TEST_SIG).unwrap_err();
        assert_eq!(err.class, FailureClass::Fatal);
        assert!(err.message.contains("checksum mismatch"));

        // Unparseable checksum sidecar → fatal.
        assert_eq!(
            verify_archive_integrity(good, "no digest here", TEST_SIG)
                .unwrap_err()
                .class,
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
        let out: Result<&str, StageError> =
            with_retries(&[0, 0], &|a, m| seen.lock().unwrap().push((a, m)), |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(StageError::transient(format!("boom {n}")))
                    } else {
                        Ok("installed")
                    }
                }
            })
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
        assert_eq!(
            serde_json::to_string(&InstallMethod::Desktop).unwrap(),
            "\"desktop\""
        );
        assert_eq!(
            serde_json::to_string(&InstallMethod::Homebrew).unwrap(),
            "\"homebrew\""
        );
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
        // The container gate must fire before extraction, even with an authentic
        // release identity.
        let (bytes, identity) = release_identity::tests::verified_fixture();
        let err = install_verified_binary(bytes, &identity)
            .await
            .unwrap_err();
        match prev {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_IN_CONTAINER", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_IN_CONTAINER") },
        }
        assert!(err.contains("容器"), "unexpected error: {err}");
        assert!(
            err.contains("image"),
            "should point at the image update path: {err}"
        );
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
        other.replace_range(
            10..11,
            if &UPDATE_PUBKEY[10..11] == "A" {
                "B"
            } else {
                "A"
            },
        );
        assert!(verify_archive_with_pubkey(good, &sidecar, TEST_SIG, &other).is_err());
        // A malformed key is an error, never a pass.
        assert!(verify_archive_with_pubkey(good, &sidecar, TEST_SIG, "not-a-key").is_err());
    }

    #[test]
    fn desktop_bundle_paths_detected() {
        // Pure-path check mirrors detect_install_method's first rule.
        assert!("/Applications/DuDuClaw.app/Contents/MacOS/duduclaw".contains(".app/Contents/"));
        assert!(
            !"/Users/x/.nvm/versions/node/v24/lib/node_modules/duduclaw/bin/duduclaw"
                .contains(".app/Contents/")
        );
    }
}
