//! `config.toml [secaudit]` — scheduled-scan tuning (S4, OS security line P0
//! WS-1 — `commercial/docs/DESIGN-os-security-line-2026-09.md` §2 D3'/D4).
//!
//! Parsed in isolation from a generic `toml::Table`, the same pattern
//! `spawn_admission::AdmissionConfig::from_home` established for `[dispatch]`
//! — an absent or malformed `[secaudit]` section falls back to built-in
//! defaults rather than failing the whole `config.toml` parse, and this
//! struct only reads the keys it knows, ignoring anything else in the same
//! table (so adding fields here never conflicts with an unrelated section).
//!
//! This module only PARSES the section. The OS-side scheduled-scan timer
//! that reads `scheduled_scan`/`scan_paths` and the operator rule-directory
//! consumer that reads `rules_dir` are later waves (拍板 D2'/D3') — this
//! crate has no scheduling primitive or `duduclaw secaudit` invocation of
//! its own, by design (`duduclaw-core` has no CLI/process-spawn surface).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// `[secaudit]` tuning, read from `<home>/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecauditConfig {
    /// Whether an OS-level timer should run a scheduled scan at all. 拍板
    /// D4: default **ON** ("預設開、每日 quick") — the opposite default from
    /// most opt-in features in this codebase, because an unscanned appliance
    /// is a worse default posture than an occasional quick scan's CPU cost.
    pub scheduled_scan: bool,
    /// Paths to scan on a scheduled run. Empty by default — per 拍板 D4 the
    /// OS timer itself supplies the scan scope (`/data` writable-execution
    /// surface: skills, `compat.d`, agent workdirs) rather than this config
    /// section hardcoding a path list that would drift from the image
    /// layout across releases. An operator MAY override with explicit paths.
    pub scan_paths: Vec<String>,
    /// `"quick"` (default — scanners only) or `"deep"` (+ intake/AI steps,
    /// see `duduclaw secaudit --profile`). Deliberately NOT validated here —
    /// `secaudit::schema::ProfileMode::from_str` (in `duduclaw-cli`, which
    /// this crate cannot depend on) is the single authority on valid values,
    /// so a typo'd profile fails loudly at scan time with a clear message
    /// instead of silently here.
    pub profile: String,
    /// Optional additional semgrep rule directory (拍板 D3' 第 6 環 —
    /// operator-authored rules layered on top of the built-in read-only set
    /// at `/usr/share/duduclaw/secaudit-rules/`). `None` ⇒ built-in rules
    /// only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_dir: Option<String>,
}

impl Default for SecauditConfig {
    fn default() -> Self {
        Self {
            scheduled_scan: true,
            scan_paths: Vec::new(),
            profile: "quick".to_string(),
            rules_dir: None,
        }
    }
}

impl SecauditConfig {
    /// Load `[secaudit]` from `<home>/config.toml`. Absent file, absent
    /// section, or a section that fails to deserialize into this shape ⇒
    /// [`SecauditConfig::default`] — never a hard error (same discipline as
    /// `spawn_admission::AdmissionConfig::from_home`).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("secaudit") {
            Some(section) => section.clone().try_into::<SecauditConfig>().unwrap_or_default(),
            None => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SecauditConfig::from_home(dir.path());
        assert!(cfg.scheduled_scan, "D4: scheduled scan defaults ON");
        assert_eq!(cfg.profile, "quick");
        assert!(cfg.scan_paths.is_empty());
        assert_eq!(cfg.rules_dir, None);
    }

    #[test]
    fn defaults_when_section_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[general]\nlog_level = \"info\"\n").unwrap();
        let cfg = SecauditConfig::from_home(dir.path());
        assert_eq!(cfg, SecauditConfig::default());
    }

    #[test]
    fn parses_explicit_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[secaudit]\n\
             scheduled_scan = false\n\
             scan_paths = [\"/data/duduclaw/skills\"]\n\
             profile = \"deep\"\n\
             rules_dir = \"/data/duduclaw/secaudit/rules.d\"\n",
        )
        .unwrap();
        let cfg = SecauditConfig::from_home(dir.path());
        assert!(!cfg.scheduled_scan);
        assert_eq!(cfg.scan_paths, vec!["/data/duduclaw/skills".to_string()]);
        assert_eq!(cfg.profile, "deep");
        assert_eq!(cfg.rules_dir.as_deref(), Some("/data/duduclaw/secaudit/rules.d"));
    }

    #[test]
    fn malformed_section_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        // `scheduled_scan` must be a bool; a string here fails to deserialize
        // into this shape — fail-open to defaults, never a panic/hard error.
        let table = "[secaudit]\nscheduled_scan = \"yes\"\n";
        std::fs::write(dir.path().join("config.toml"), table).unwrap();
        let cfg = SecauditConfig::from_home(dir.path());
        assert_eq!(cfg, SecauditConfig::default());
    }

    #[test]
    fn unrelated_sibling_sections_do_not_interfere() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\nadmission = \"fail\"\n\n[secaudit]\nprofile = \"deep\"\n",
        )
        .unwrap();
        let cfg = SecauditConfig::from_home(dir.path());
        assert_eq!(cfg.profile, "deep");
        assert!(
            cfg.scheduled_scan,
            "an unrelated sibling section's own parse must not affect this one"
        );
    }

    #[test]
    fn unknown_keys_in_section_are_ignored_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[secaudit]\nprofile = \"quick\"\nsome_future_key = 42\n",
        )
        .unwrap();
        let cfg = SecauditConfig::from_home(dir.path());
        assert_eq!(cfg.profile, "quick");
    }
}
