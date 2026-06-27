//! Premium (licensed) industry template resolution.
//!
//! Free starter templates live in `templates/` and are copied by the wizard
//! unconditionally (they ship in the public, Apache-2.0 repo). The *premium*
//! industry templates — battle-tested SOUL.md / CONTRACT.toml / wiki knowledge
//! tuned per vertical — live in the gitignored `commercial/templates-premium/`
//! tree and are only unlocked for tiers whose license grants the
//! `premium_templates` feature (Studio / Business / SelfHostPro /
//! PersonalProSelfHost / OEM — see `crates/duduclaw-license/features.toml`).
//!
//! This module is the single gate between "has a license that unlocks premium
//! templates" and "can actually instantiate a premium template". It is
//! **fail-closed** per the project security convention: any error reading the
//! license, a missing/expired license, a missing directory, or an
//! unrecognised slug all resolve to *locked / unavailable*, never to an
//! accidental unlock.

use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_license::{
    load_default, FeatureGate, LicenseError, LicenseTier, EMBEDDED_FEATURES_TOML,
};

/// The feature flag (in `features.toml`) that unlocks premium templates.
const PREMIUM_FEATURE: &str = "premium_templates";

/// A discovered premium industry template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumIndustry {
    /// Directory slug, e.g. `ecommerce-pro`. Validated to be a safe path
    /// component (lowercase alphanumeric + hyphen) before use.
    pub slug: String,
    /// Human-facing label shown in the wizard, e.g. `電商客服 (Pro)`.
    pub label: String,
    /// Absolute path to the template directory.
    pub dir: PathBuf,
}

/// Pretty label for a known premium slug; falls back to the slug itself so a
/// newly-added premium template still shows *something* sensible without a
/// code change.
fn label_for_slug(slug: &str) -> String {
    let pretty = match slug {
        "ecommerce-pro" => "電商客服 (Pro)",
        "clinic-pro" => "醫美/牙醫診所 (Pro)",
        "realestate-pro" => "房仲 (Pro)",
        "education-pro" => "補習班/招生 (Pro)",
        "restaurant-pro" => "餐飲 (Pro)",
        "manufacturing-pro" => "製造業 (Pro)",
        "trading-pro" => "貿易 (Pro)",
        "retail-pro" => "零售 (Pro)",
        other => return format!("{other} (Pro)"),
    };
    pretty.to_string()
}

/// A slug is a single path component: lowercase alphanumeric + hyphen, no
/// `.`/`/`/`..`. This blocks path-traversal via a crafted slug.
fn is_safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Pure gate check over a tier — does this tier's license grant
/// `premium_templates`? Fail-closed: a broken embedded features.toml denies.
///
/// Separated from filesystem/license-IO so it can be unit-tested directly.
pub fn premium_gate_open(tier: LicenseTier) -> bool {
    match FeatureGate::from_str(EMBEDDED_FEATURES_TOML) {
        Ok(gate) => gate.check(tier, PREMIUM_FEATURE),
        // If the embedded table can't parse we deny rather than guess.
        Err(_) => false,
    }
}

/// Is the premium-templates feature unlocked on *this* machine right now?
///
/// Fail-closed: no license file → OpenSource → locked; expired license →
/// locked; any read error → locked.
pub fn premium_unlocked() -> bool {
    match load_default() {
        Ok(license) => {
            if license.is_expired() {
                return false;
            }
            premium_gate_open(license.tier)
        }
        // FileNotFound == OpenSource == locked. Any other error also denies.
        Err(LicenseError::FileNotFound(_)) => false,
        Err(_) => false,
    }
}

/// Locate the premium templates directory, if present on disk.
///
/// Resolution order (first existing directory wins):
///   1. `DUDUCLAW_PREMIUM_TEMPLATES` env var (explicit override)
///   2. `templates-premium/` next to the executable (installed layout)
///   3. `../../templates-premium` relative to the exe (dev: target/<profile>)
///   4. `commercial/templates-premium/` under the CWD (dev checkout)
///   5. `templates-premium/` under the CWD
///
/// Returns `None` when no premium tree is installed (e.g. the public OSS
/// binary that never shipped the closed templates).
pub fn find_premium_templates_dir() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("DUDUCLAW_PREMIUM_TEMPLATES") {
        let p = PathBuf::from(custom);
        if p.is_dir() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("templates-premium");
            if candidate.is_dir() {
                return Some(candidate);
            }
            // Dev layout: exe in target/<profile>/, premium tree two levels up
            // under commercial/.
            if let Some(root) = parent.parent().and_then(|p| p.parent()) {
                let candidate = root.join("commercial").join("templates-premium");
                if candidate.is_dir() {
                    return Some(candidate);
                }
                let candidate = root.join("templates-premium");
                if candidate.is_dir() {
                    return Some(candidate);
                }
            }
        }
    }

    let cwd_commercial = PathBuf::from("commercial").join("templates-premium");
    if cwd_commercial.is_dir() {
        return Some(cwd_commercial);
    }
    let cwd = PathBuf::from("templates-premium");
    if cwd.is_dir() {
        return Some(cwd);
    }

    None
}

/// Enumerate premium templates physically present under `dir`.
///
/// A premium template is a direct sub-directory that contains a `SOUL.md`
/// (the minimum marker of a usable template) and whose name is a safe slug.
/// This does NOT check the license — callers gate with [`premium_unlocked`].
fn discover_in(dir: &Path) -> Vec<PremiumIndustry> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(slug) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_safe_slug(slug) {
            continue;
        }
        if !path.join("SOUL.md").is_file() {
            continue;
        }
        out.push(PremiumIndustry {
            slug: slug.to_string(),
            label: label_for_slug(slug),
            dir: path,
        });
    }
    // Deterministic ordering for stable wizard menus.
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// The premium industries available to the user *right now*: present on disk
/// **and** unlocked by the active license. Empty when locked or absent.
pub fn available_premium_industries() -> Vec<PremiumIndustry> {
    if !premium_unlocked() {
        return Vec::new();
    }
    match find_premium_templates_dir() {
        Some(dir) => discover_in(&dir),
        None => Vec::new(),
    }
}

/// Premium templates exist on disk but are NOT unlocked — used to render a
/// gentle upsell hint in the wizard. Returns `false` when no premium tree is
/// installed at all (nothing to upsell).
pub fn premium_present_but_locked() -> bool {
    if premium_unlocked() {
        return false;
    }
    find_premium_templates_dir()
        .map(|d| !discover_in(&d).is_empty())
        .unwrap_or(false)
}

/// Resolve a premium template directory by slug, enforcing the license gate.
///
/// Fail-closed: returns an actionable error (not a silent fallback) when the
/// feature is locked, the tree is absent, the slug is unsafe, or the named
/// template does not exist.
pub fn resolve_premium_template(slug: &str) -> Result<PathBuf> {
    if !is_safe_slug(slug) {
        return Err(DuDuClawError::Agent(format!(
            "invalid premium template name '{slug}'"
        )));
    }
    if !premium_unlocked() {
        return Err(DuDuClawError::License(format!(
            "premium template '{slug}' requires a Pro license. \
             Activate one with `duduclaw license activate <key>` \
             (see https://duduclaw.tw/pricing)."
        )));
    }
    let base = find_premium_templates_dir().ok_or_else(|| {
        DuDuClawError::Agent(
            "premium templates are not installed on this machine".into(),
        )
    })?;
    let dir = base.join(slug);
    if !dir.join("SOUL.md").is_file() {
        return Err(DuDuClawError::Agent(format!(
            "premium template '{slug}' not found under {}",
            base.display()
        )));
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opensource_tier_is_gated() {
        assert!(!premium_gate_open(LicenseTier::OpenSource));
    }

    #[test]
    fn paid_tiers_unlock_premium_templates() {
        // These tiers grant premium_templates per features.toml.
        assert!(premium_gate_open(LicenseTier::Studio));
        assert!(premium_gate_open(LicenseTier::Business));
        assert!(premium_gate_open(LicenseTier::SelfHostPro));
        assert!(premium_gate_open(LicenseTier::PersonalProSelfHost));
        assert!(premium_gate_open(LicenseTier::Oem));
    }

    #[test]
    fn safe_slug_accepts_known_premium_dirs() {
        for s in [
            "ecommerce-pro",
            "clinic-pro",
            "realestate-pro",
            "education-pro",
        ] {
            assert!(is_safe_slug(s), "{s} should be a safe slug");
        }
    }

    #[test]
    fn safe_slug_rejects_traversal_and_junk() {
        for s in [
            "",
            "../etc",
            "a/b",
            "..",
            ".hidden",
            "-leading",
            "trailing-",
            "Upper",
            "white space",
        ] {
            assert!(!is_safe_slug(s), "{s:?} must be rejected");
        }
    }

    #[test]
    fn label_falls_back_to_slug() {
        assert_eq!(label_for_slug("ecommerce-pro"), "電商客服 (Pro)");
        assert_eq!(label_for_slug("logistics-pro"), "logistics-pro (Pro)");
    }

    #[test]
    fn discover_in_finds_only_dirs_with_soul() {
        let tmp = std::env::temp_dir().join(format!(
            "dudu-premium-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        // valid template
        std::fs::create_dir_all(tmp.join("foo-pro")).unwrap();
        std::fs::write(tmp.join("foo-pro").join("SOUL.md"), "# x").unwrap();
        // dir without SOUL.md → ignored
        std::fs::create_dir_all(tmp.join("bar-pro")).unwrap();
        // unsafe slug dir (even with SOUL.md) → ignored
        std::fs::create_dir_all(tmp.join(".sneaky")).unwrap();
        std::fs::write(tmp.join(".sneaky").join("SOUL.md"), "# x").unwrap();

        let found = discover_in(&tmp);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].slug, "foo-pro");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_rejects_unsafe_slug_before_touching_fs() {
        let err = resolve_premium_template("../secrets").unwrap_err();
        assert!(matches!(err, DuDuClawError::Agent(_)));
    }
}
