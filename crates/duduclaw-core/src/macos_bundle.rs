//! `DuDuClaw Gateway.app` (RM-155 item 3, v1.81.0) — identity constants and
//! pure path helpers shared by the LaunchAgent writer (`autostart`) and the
//! self-updater (`duduclaw-gateway::updater`).
//!
//! The bundle exists so macOS TCC sees ONE stable program
//! (`ai.duduclaw.gateway`, Developer ID, team 7469HYQ6HH) across updates
//! instead of a new ad-hoc cdhash per release. CE and Pro share the
//! identifier (owner decision D3); the edition is told apart by
//! `CFBundleExecutable` (`duduclaw` / `duduclaw-pro`).
//!
//! Everything here is pure or sits behind [`InfoPlistReader`] so the
//! classification rules are unit-testable on every host. Only
//! [`PlutilReader`] (macOS) spawns a process.

use std::path::{Path, PathBuf};

/// `CFBundleIdentifier` of the gateway bundle (CE and Pro).
pub const BUNDLE_ID: &str = "ai.duduclaw.gateway";
/// `CFBundleIdentifier` of the Tauri desktop app. A sidecar inside it stays
/// `InstallMethod::Desktop` — the Tauri updater owns that bundle.
pub const DESKTOP_BUNDLE_ID: &str = "ai.duduclaw.desktop";
/// Directory name of the bundle (install.sh → `~/Applications/<this>`).
pub const BUNDLE_DIR_NAME: &str = "DuDuClaw Gateway.app";
/// Apple Developer team that signs the bundle.
pub const TEAM_ID: &str = "7469HYQ6HH";
/// Pinned designated requirement: identifier + Developer ID chain + team.
/// The updater verifies every staged bundle against this text with
/// `codesign --verify --strict --deep -R "=<PINNED_DR>"` before it is
/// swapped in; it is compiled in and never read from the environment.
pub const PINNED_DR: &str = concat!(
    "identifier \"ai.duduclaw.gateway\" and anchor apple generic",
    " and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */",
    " and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */",
    " and certificate leaf[subject.OU] = \"7469HYQ6HH\""
);
/// Prefix of the updater's same-volume staging directories
/// (`<bundle parent>/.duduclaw-update-<uuid>/`).
pub const STAGING_PREFIX: &str = ".duduclaw-update-";

/// The `.app` directory containing `exe`, when `exe` sits at
/// `<X>.app/Contents/MacOS/<exe>`. Purely lexical — callers pass a
/// canonical path (see [`crate::platform::executable_path`]).
pub fn bundle_root_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let root = contents.parent()?;
    let name = root.file_name()?.to_str()?;
    (name.len() > 4 && name.ends_with(".app")).then(|| root.to_path_buf())
}

/// `<bundle>/Contents/Info.plist`.
pub fn info_plist_path(bundle: &Path) -> PathBuf {
    bundle.join("Contents").join("Info.plist")
}

/// `<bundle>/Contents/MacOS/<name>`.
pub fn bundle_executable(bundle: &Path, name: &str) -> PathBuf {
    bundle.join("Contents").join("MacOS").join(name)
}

/// Reads one string key from a bundle's `Info.plist`. A trait so detection
/// and the update gate can be tested without `/usr/bin/plutil`.
pub trait InfoPlistReader: Sync {
    /// The raw string value of `key`, or an error when the plist or key is
    /// missing/unreadable. Implementations must not follow the environment.
    fn read_key(&self, bundle: &Path, key: &str) -> Result<String, String>;

    fn bundle_identifier(&self, bundle: &Path) -> Result<String, String> {
        self.read_key(bundle, "CFBundleIdentifier")
    }
}

/// `/usr/bin/plutil -extract <key> raw -o - -- <Info.plist>` (absolute path,
/// no shell, bounded output).
#[cfg(target_os = "macos")]
pub struct PlutilReader;

#[cfg(target_os = "macos")]
impl InfoPlistReader for PlutilReader {
    fn read_key(&self, bundle: &Path, key: &str) -> Result<String, String> {
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("Invalid Info.plist key".into());
        }
        let plist = info_plist_path(bundle);
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-", "--"])
            .arg(&plist)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("/usr/bin/plutil: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Cannot read {key} from {}: {}",
                plist.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if output.stdout.len() > 4096 {
            return Err(format!("{key} in {} is implausibly long", plist.display()));
        }
        let value = String::from_utf8(output.stdout)
            .map_err(|_| format!("{key} in {} is not UTF-8", plist.display()))?;
        Ok(value.trim_end_matches('\n').to_owned())
    }
}

/// What kind of `.app` (if any) an executable lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleKind {
    /// Not inside `<X>.app/Contents/MacOS/`.
    None,
    /// The gateway bundle (`ai.duduclaw.gateway`) rooted at the path.
    Gateway(PathBuf),
    /// Some other bundle — including the desktop app and any bundle whose
    /// identifier cannot be read (fail closed: never self-update it).
    Other,
}

/// Classify `exe` (canonical) by its bundle's `CFBundleIdentifier`.
pub fn classify(exe: &Path, reader: &dyn InfoPlistReader) -> BundleKind {
    let Some(root) = bundle_root_of(exe) else { return BundleKind::None };
    match reader.bundle_identifier(&root) {
        Ok(id) if id == BUNDLE_ID => BundleKind::Gateway(root),
        _ => BundleKind::Other,
    }
}

/// The gateway bundle this process runs from, if any (cached per process).
/// Always `None` off macOS.
pub fn running_gateway_bundle() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        static BUNDLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        BUNDLE
            .get_or_init(|| match classify(&crate::platform::executable_path(), &PlutilReader) {
                BundleKind::Gateway(root) => Some(root),
                _ => None,
            })
            .clone()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The platform npm package directory (`…/node_modules/@duduclaw/<plat>/`)
/// containing `exe`, if any — the bare→bundle migration installs the bundle
/// there so `npm i -g duduclaw` keeps owning the files.
pub fn npm_package_dir_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).find_map(|dir| {
        let scope = dir.parent()?;
        let node_modules = scope.parent()?;
        (scope.file_name()? == "@duduclaw" && node_modules.file_name()? == "node_modules")
            .then(|| dir.to_path_buf())
    })
}

/// Where a bare install migrates its bundle to: inside the npm platform
/// package for npm installs, else `<home>/Applications/`.
pub fn migration_target_root(exe: &Path, home: &Path) -> PathBuf {
    npm_package_dir_of(exe)
        .unwrap_or_else(|| home.join("Applications"))
        .join(BUNDLE_DIR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Stub(HashMap<PathBuf, &'static str>);
    impl InfoPlistReader for Stub {
        fn read_key(&self, bundle: &Path, key: &str) -> Result<String, String> {
            assert_eq!(key, "CFBundleIdentifier");
            self.0.get(bundle).map(|s| s.to_string()).ok_or_else(|| "missing".into())
        }
    }

    #[test]
    fn bundle_root_requires_contents_macos_layout() {
        let exe = Path::new("/Users/k/Applications/DuDuClaw Gateway.app/Contents/MacOS/duduclaw");
        assert_eq!(
            bundle_root_of(exe),
            Some(PathBuf::from("/Users/k/Applications/DuDuClaw Gateway.app"))
        );
        for bad in [
            "/usr/local/bin/duduclaw",
            "/x/DuDuClaw Gateway.app/Contents/Resources/duduclaw",
            "/x/DuDuClaw Gateway/Contents/MacOS/duduclaw",
            "/x/.app/Contents/MacOS/duduclaw",
            "/x/Foo.app/MacOS/duduclaw",
            "duduclaw",
        ] {
            assert_eq!(bundle_root_of(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn classify_by_bundle_identifier() {
        let gw = PathBuf::from("/A/DuDuClaw Gateway.app");
        let desk = PathBuf::from("/Applications/DuDuClaw.app");
        let stub = Stub(HashMap::from([(gw.clone(), BUNDLE_ID), (desk.clone(), DESKTOP_BUNDLE_ID)]));
        assert_eq!(classify(&gw.join("Contents/MacOS/duduclaw"), &stub), BundleKind::Gateway(gw));
        assert_eq!(classify(&desk.join("Contents/MacOS/duduclaw"), &stub), BundleKind::Other);
        // Unreadable identifier fails closed to Other (never self-updated).
        assert_eq!(classify(Path::new("/B/X.app/Contents/MacOS/duduclaw"), &stub), BundleKind::Other);
        assert_eq!(classify(Path::new("/usr/local/bin/duduclaw"), &stub), BundleKind::None);
    }

    #[test]
    fn pinned_dr_names_identifier_and_team() {
        assert!(PINNED_DR.starts_with("identifier \"ai.duduclaw.gateway\" and anchor apple generic"));
        assert!(PINNED_DR.contains("certificate leaf[subject.OU] = \"7469HYQ6HH\""));
        assert!(PINNED_DR.contains(TEAM_ID));
    }

    #[test]
    fn migration_target_prefers_npm_platform_package() {
        let npm = Path::new("/opt/homebrew/lib/node_modules/@duduclaw/darwin-arm64/duduclaw");
        assert_eq!(
            migration_target_root(npm, Path::new("/Users/k")),
            PathBuf::from("/opt/homebrew/lib/node_modules/@duduclaw/darwin-arm64/DuDuClaw Gateway.app")
        );
        let nested = Path::new("/n/node_modules/@duduclaw/darwin-x64/bin/duduclaw");
        assert_eq!(
            npm_package_dir_of(nested),
            Some(PathBuf::from("/n/node_modules/@duduclaw/darwin-x64"))
        );
        assert_eq!(
            migration_target_root(Path::new("/Users/k/.duduclaw/bin/duduclaw"), Path::new("/Users/k")),
            PathBuf::from("/Users/k/Applications/DuDuClaw Gateway.app")
        );
    }
}
