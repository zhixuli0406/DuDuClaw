// WP-C-M3 — update manifest: fetch + parse + version compare.
//
// Deliberately NOT the Tauri shell's `latest.json` shape/endpoint
// (`src-tauri/tauri.conf.json`'s `plugins.updater.endpoints`): that file is
// signed with the Tauri-specific keypair (`~/.tauri/duduclaw.key`,
// `TAURI_SIGNING_PRIVATE_KEY` secret — see `docs/guides/desktop-release.md`
// §1) and consumed by `tauri-plugin-updater`'s OWN client, which this crate
// has no code path to reuse (gpui hosts no webview/JS runtime). Publishing
// THIS crate's manifest under a different filename on the SAME fixed
// `desktop-updater` release tag — `native-gui-latest.json` vs. Tauri's
// `latest.json` — reuses the one proven "fixed pointer, republished on every
// release" mechanism (`docs/guides/desktop-release.md` §5's reasoning for
// why `releases/latest` can't be the endpoint: core `v*` releases ship far
// more often and would 404 the check) without the two shells' release
// assets ever colliding on the same GitHub release object.
//
// Manifest shape (produced by `.github/workflows/native-gui-desktop-
// release.yml`'s `updater-manifest` job — see that file):
//   {
//     "version": "1.62.1",
//     "notes": "...",
//     "pub_date": "2026-08-22T00:00:00Z",
//     "platforms": { "darwin-arm64": { "url": "https://github.com/.../
//       DuDuClaw-native-gui-native-gui-v1.62.1-aarch64-apple-darwin.tar.gz" } }
//   }
// `checksum`/`signature` are deliberately NOT embedded fields — same
// derive-don't-trust-the-payload approach `duduclaw-gateway/src/updater.rs`
// documents at its `[S1]` signature-url comment ("derived, not client-
// supplied — validate it before any network work"): the sha256 sidecar is
// always `<url>.sha256` and the minisign signature is always `<url>.minisig`,
// so a tampered manifest can redirect the download but can never redirect
// which files get treated as its integrity proof.

use serde::Deserialize;
use std::collections::HashMap;

/// GitHub repo slug — matches `crates/duduclaw-gateway/src/updater.rs`'s own
/// `GITHUB_REPO` constant and `src-tauri/tauri.conf.json`'s updater
/// endpoint host. One publisher, one repo; not worth threading through a
/// config value this crate has nowhere else to source it from.
const GITHUB_REPO: &str = "zhixuli0406/DuDuClaw";

/// Fixed manifest URL — the `desktop-updater` release tag never changes, so
/// (unlike the gateway's CLI-channel `check_update`, which has to resolve
/// `releases/latest` first) this is a constant, not a runtime lookup.
pub const MANIFEST_URL: &str =
    "https://github.com/zhixuli0406/DuDuClaw/releases/download/desktop-updater/native-gui-latest.json";

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformEntry {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    /// Parsed for schema completeness (the manifest CI publishes always
    /// carries it) but not rendered anywhere yet — `screens/about.rs`'s
    /// update card shows `notes`/`version`/a relative "有新版" line, not a
    /// publish timestamp. Kept `pub` as forward-looking surface for
    /// whichever page adds that nuance next, same `#[allow(dead_code)]`-
    /// with-rationale convention this crate already uses elsewhere (e.g.
    /// `main.rs`'s `refresh_token` field).
    #[allow(dead_code)]
    #[serde(default)]
    pub pub_date: String,
    pub platforms: HashMap<String, PlatformEntry>,
}

/// This build's own version — `CARGO_PKG_VERSION` is the single source of
/// truth for this crate (no `DUDUCLAW_VERSION`-style runtime override chain
/// like the gateway's `current_version()` needs; there is no Pro/Enterprise
/// wrapper around the native-gui binary).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The manifest key for the platform THIS build runs on, matching
/// `crates/duduclaw-gateway/src/updater.rs::platform_asset_suffix()`'s
/// `darwin-arm64`/`darwin-x64` naming (reusing an existing convention in
/// this repo rather than inventing Tauri's `darwin-aarch64`/`darwin-x86_64`
/// spelling). `None` on every non-Darwin target — this crate is gpui-based
/// and macOS-only today (see `Cargo.toml`'s header comment and
/// `.github/workflows/native-gui-desktop-release.yml`'s placeholder
/// Windows/Linux matrix legs); the updater has nothing to offer there yet.
pub fn platform_key() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("darwin-arm64")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("darwin-x64")
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
    )))]
    {
        None
    }
}

/// Validate a manifest asset URL points at the official GitHub release
/// assets for THIS repo — same allowlist shape as `duduclaw-gateway/src/
/// updater.rs::is_valid_download_url`, hand-duplicated rather than shared
/// (this crate cannot depend on `duduclaw-gateway`, see this module's header
/// comment; the check itself is three lines, not worth a shared crate).
pub fn is_valid_release_url(url: &str) -> bool {
    let prefix = format!("https://github.com/{GITHUB_REPO}/releases/download/");
    url.starts_with(&prefix) && !url.contains("..") && url.len() < 512
}

/// Tolerant `MAJOR.MINOR.PATCH` compare — pre-release/build suffixes after a
/// `-` are ignored, unparseable components read as 0. Mirrors `duduclaw-
/// gateway/src/updater.rs::is_newer` (same reasoning: a version-tag mismatch
/// should never be a hard error, just conservatively "not newer").
pub fn is_newer(current: &str, latest: &str) -> bool {
    parse_version(latest) > parse_version(current)
}

fn parse_version(s: &str) -> (u32, u32, u32) {
    let s = s.strip_prefix('v').unwrap_or(s);
    let parts: Vec<&str> = s.split('.').collect();
    let component = |p: &str| -> u32 { p.split('-').next().and_then(|n| n.parse().ok()).unwrap_or(0) };
    (
        parts.first().map(|p| component(p)).unwrap_or(0),
        parts.get(1).map(|p| component(p)).unwrap_or(0),
        parts.get(2).map(|p| component(p)).unwrap_or(0),
    )
}

/// Parse a manifest response body. Kept separate from the network fetch so
/// it's testable with fixed strings, no HTTP involved.
pub fn parse_manifest(body: &str) -> Result<UpdateManifest, String> {
    serde_json::from_str(body).map_err(|e| format!("無法解析更新資訊: {e}"))
}

/// Resolve THIS platform's entry out of a parsed manifest, validating the
/// URL before returning it. Folds three distinct "nothing to offer" cases
/// (unsupported platform / manifest has no entry for it / entry URL fails
/// the allowlist) into one `Result` so callers don't need to distinguish
/// them — all three mean the same thing to the user: no update available
/// for this machine.
pub fn resolve_platform_entry(manifest: &UpdateManifest) -> Result<&PlatformEntry, String> {
    let key = platform_key().ok_or_else(|| "此平台尚未支援自動更新".to_string())?;
    let entry = manifest
        .platforms
        .get(key)
        .ok_or_else(|| format!("更新資訊未包含此平台（{key}）的下載項目"))?;
    if !is_valid_release_url(&entry.url) {
        return Err(format!("拒絕不安全的下載位址: {}", entry.url));
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_recognizes_a_real_bump() {
        assert!(is_newer("1.62.0", "1.62.1"));
        assert!(is_newer("1.62.0", "1.63.0"));
        assert!(is_newer("1.62.0", "2.0.0"));
        assert!(!is_newer("1.62.1", "1.62.0"));
        assert!(!is_newer("1.62.0", "1.62.0"));
    }

    #[test]
    fn is_newer_tolerates_v_prefix_and_prerelease_suffix() {
        assert!(is_newer("v1.62.0", "v1.62.1"));
        assert!(is_newer("1.62.0", "1.62.1-rc.1"));
    }

    #[test]
    fn is_newer_unparseable_reads_as_zero_not_a_panic() {
        // "garbage" parses to (0,0,0) — never a panic, and always "not newer"
        // than any real version on either side of the comparison.
        assert!(is_newer("garbage", "1.0.0"));
        assert!(!is_newer("1.0.0", "garbage"));
    }

    #[test]
    fn parse_manifest_accepts_the_documented_shape() {
        let body = r#"{
            "version": "1.62.1",
            "notes": "fix things",
            "pub_date": "2026-08-22T00:00:00Z",
            "platforms": {
                "darwin-arm64": { "url": "https://github.com/zhixuli0406/DuDuClaw/releases/download/native-gui-v1.62.1/x.tar.gz" }
            }
        }"#;
        let m = parse_manifest(body).unwrap();
        assert_eq!(m.version, "1.62.1");
        assert_eq!(m.notes, "fix things");
        assert!(m.platforms.contains_key("darwin-arm64"));
    }

    #[test]
    fn parse_manifest_rejects_garbage() {
        assert!(parse_manifest("not json").is_err());
        assert!(parse_manifest("{}").is_err(), "platforms is a required field");
    }

    #[test]
    fn is_valid_release_url_accepts_only_this_repos_release_assets() {
        assert!(is_valid_release_url(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/native-gui-v1.62.1/x.tar.gz"
        ));
        assert!(!is_valid_release_url("https://evil.example.com/x.tar.gz"));
        assert!(!is_valid_release_url(
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/../../etc/passwd"
        ));
        assert!(!is_valid_release_url(
            "https://github.com.evil.com/zhixuli0406/DuDuClaw/releases/download/x/y"
        ));
    }

    #[test]
    fn resolve_platform_entry_missing_key_is_a_clean_error_not_a_panic() {
        let manifest = UpdateManifest {
            version: "1.0.0".into(),
            notes: String::new(),
            pub_date: String::new(),
            platforms: HashMap::new(),
        };
        assert!(resolve_platform_entry(&manifest).is_err());
    }

    #[test]
    fn resolve_platform_entry_rejects_an_untrusted_url_even_if_present() {
        let Some(key) = platform_key() else {
            return; // this test target has no platform_key — nothing to resolve
        };
        let mut platforms = HashMap::new();
        platforms.insert(key.to_string(), PlatformEntry { url: "https://evil.example.com/x.tar.gz".into() });
        let manifest =
            UpdateManifest { version: "9.9.9".into(), notes: String::new(), pub_date: String::new(), platforms };
        assert!(resolve_platform_entry(&manifest).is_err());
    }
}
