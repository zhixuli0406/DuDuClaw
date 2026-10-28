//! Strict, re-read policy and canonical dedicated-profile validation.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalBrowserConfig {
    pub enabled: bool,
    pub idle_seconds: u64,
}

impl LocalBrowserConfig {
    pub(crate) fn parse(table: &toml::Table) -> Result<Self, String> {
        let off = Self {
            enabled: false,
            idle_seconds: 600,
        };
        let Some(computer) = table.get("computer_use") else {
            return Ok(off);
        };
        let computer = computer.as_table().ok_or("invalid computer_use section")?;
        let Some(local) = computer.get("local_browser") else {
            return Ok(off);
        };
        let local = local.as_table().ok_or("invalid local_browser section")?;
        if local
            .keys()
            .any(|k| !matches!(k.as_str(), "enabled" | "idle_seconds"))
        {
            return Err("unknown local_browser setting".into());
        }
        let enabled = match local.get("enabled") {
            None => false,
            Some(v) => v.as_bool().ok_or("local_browser.enabled must be boolean")?,
        };
        let idle_seconds = match local.get("idle_seconds") {
            None => 600,
            Some(v) => v
                .as_integer()
                .filter(|n| (30..=600).contains(n))
                .ok_or("local_browser.idle_seconds must be 30..600")? as u64,
        };
        Ok(Self {
            enabled,
            idle_seconds,
        })
    }

    pub(crate) fn load(home: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => return Err("local browser configuration unreadable".into()),
        };
        let table = text
            .parse::<toml::Table>()
            .map_err(|_| "local browser configuration invalid")?;
        Self::parse(&table)
    }
}

/// Only macOS ships this backend. Windows must never fall back to TCP CDP or
/// the removed desktop-native backend.
pub(crate) fn platform_supported() -> bool {
    cfg!(target_os = "macos")
}

/// Derive the path internally and reject links in every existing component.
/// Canonicalization alone would accept a symlink into a real browser profile.
pub(crate) fn dedicated_profile(home: &Path, agent: &str) -> Result<PathBuf, String> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return Err("invalid employee".into());
    }
    let home = home
        .canonicalize()
        .map_err(|_| "application home unavailable")?;
    let profile = home.join("local_browser").join(agent).join("profile");
    let mut cursor = PathBuf::new();
    for part in profile.components() {
        if matches!(part, Component::ParentDir) {
            return Err("invalid profile path".into());
        }
        cursor.push(part.as_os_str());
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err("linked or invalid profile path".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("unreadable profile path".into()),
        }
    }
    // Reject even a deliberately chosen DUDUCLAW_HOME nested in a normal
    // Chrome/Edge/Chromium data tree. macOS paths can be case-insensitive.
    let normalized = profile
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    for marker in [
        "/library/application support/google/chrome",
        "/library/application support/microsoft edge",
        "/library/application support/chromium",
        "/google/chrome/user data",
        "/microsoft/edge/user data",
        "/.config/google-chrome",
        "/.config/chromium",
    ] {
        if normalized.contains(marker) {
            return Err("normal browser profile refused".into());
        }
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_absence_and_bad_types_never_enable_local_browser() {
        assert!(
            !LocalBrowserConfig::parse(&toml::Table::new())
                .unwrap()
                .enabled
        );
        for source in [
            "computer_use = true",
            "[computer_use]\nlocal_browser = true",
            "[computer_use.local_browser]\nenabled = 'yes'",
            "[computer_use.local_browser]\nenabled = true\nidle_seconds = 601",
            "[computer_use.local_browser]\nenabled = true\nunknown = false",
        ] {
            assert!(
                LocalBrowserConfig::parse(&source.parse().unwrap()).is_err(),
                "{source}"
            );
        }
        let config = LocalBrowserConfig::parse(
            &"[computer_use.local_browser]\nenabled = true"
                .parse()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            config,
            LocalBrowserConfig {
                enabled: true,
                idle_seconds: 600
            }
        );
    }

    #[test]
    fn profile_is_derived_and_normal_user_profile_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let normal = root
            .path()
            .join("Library/Application Support/Google/Chrome");
        std::fs::create_dir_all(&normal).unwrap();
        assert!(dedicated_profile(&normal, "alice").is_err());
        assert!(dedicated_profile(root.path(), "../alice").is_err());
        assert_eq!(
            dedicated_profile(root.path(), "alice").unwrap(),
            root.path()
                .canonicalize()
                .unwrap()
                .join("local_browser/alice/profile")
        );
    }

    #[test]
    #[cfg(unix)]
    fn profile_symlink_is_refused_even_when_target_is_within_home() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("other")).unwrap();
        std::os::unix::fs::symlink(root.path().join("other"), root.path().join("local_browser"))
            .unwrap();
        assert!(dedicated_profile(root.path(), "alice").is_err());
    }
}
