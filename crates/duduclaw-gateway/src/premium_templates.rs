//! Premium industry template + team playbook resolution.
//!
//! The filesystem side of the premium template gate. Discovery of the
//! gitignored `commercial/templates-premium/` tree used to live in
//! `duduclaw-cli::premium_templates`; it moved here so the dashboard gateway
//! (which the cli depends on, not vice versa) can drive the team staging flow.
//! The cli re-exports these symbols, so `duduclaw wizard` behavior is
//! unchanged.
//!
//! Two layers live here:
//!   1. **Industry packs** — `<slug>-pro/` directories with a root `SOUL.md`
//!      (what the CLI wizard lists).
//!   2. **Team playbooks** — `teams/<industry>-team/team.toml` machine
//!      manifests + shared worker kits under `teams/_departments/` and the
//!      cross-industry CEO kit under `teams/_roles/ceo/`. These power the
//!      dashboard "stage an industry, then let the admin create each agent"
//!      onboarding flow.
//!
//! License checks do NOT live here — callers gate with
//! `license_runtime::global().check_feature("premium_templates")` (gateway)
//! or `duduclaw-cli::premium_templates::premium_unlocked()` (cli). Everything
//! in this module is pure filesystem + parsing and stays fail-closed: any
//! missing file, bad slug, or parse error resolves to "unavailable".

use std::path::{Path, PathBuf};

use serde::Deserialize;

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
pub fn label_for_slug(slug: &str) -> String {
    let pretty = match slug {
        "ecommerce-pro" => "電商客服 (Pro)",
        "clinic-pro" => "醫美/牙醫診所 (Pro)",
        "realestate-pro" => "房仲 (Pro)",
        "education-pro" => "補習班/招生 (Pro)",
        "restaurant-pro" => "餐飲 (Pro)",
        "manufacturing-pro" => "製造業 (Pro)",
        "trading-pro" => "貿易 (Pro)",
        "retail-pro" => "零售 (Pro)",
        "lawfirm-pro" => "法律事務所 (Pro)",
        "accounting-pro" => "會計/記帳事務所 (Pro)",
        "insurance-pro" => "保險業務 (Pro)",
        "hr-pro" => "人資/招募 (Pro)",
        "hospitality-pro" => "旅宿/民宿 (Pro)",
        "fitness-pro" => "健身房 (Pro)",
        "vet-pro" => "寵物醫院 (Pro)",
        "b2bsales-pro" => "B2B 業務/報價 (Pro)",
        "interior-pro" => "室內裝修 (Pro)",
        "funeral-pro" => "殯葬禮儀 (Pro)",
        "usedcar-pro" => "中古車行 (Pro)",
        "autorepair-pro" => "汽車維修 (Pro)",
        "moving-pro" => "搬家公司 (Pro)",
        "wedding-pro" => "婚紗/婚攝 (Pro)",
        "tcm-pro" => "中醫診所 (Pro)",
        "pharmacy-pro" => "藥局 (Pro)",
        "homecare-pro" => "長照居服 (Pro)",
        "childcare-pro" => "托嬰/幼兒園 (Pro)",
        other => return format!("{other} (Pro)"),
    };
    pretty.to_string()
}

/// A slug is a single path component: lowercase alphanumeric + hyphen, no
/// `.`/`/`/`..`. This blocks path-traversal via a crafted slug.
pub fn is_safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
/// This does NOT check the license — gateway callers gate with
/// `license_runtime`, cli callers with `premium_unlocked()`.
pub fn discover_in(dir: &Path) -> Vec<PremiumIndustry> {
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

// ─────────────────────────────────────────────────────────────────────────────
// Team playbook manifests (teams/<industry>-team/team.toml)
// ─────────────────────────────────────────────────────────────────────────────

/// Machine-readable team manifest — mirrors the machine-relevant facts of the
/// human-facing `TEAM.md` playbook (which stays the SSOT for deployment
/// rationale and compliance narrative).
#[derive(Debug, Clone, Deserialize)]
pub struct TeamManifest {
    pub schema: i64,
    /// Team slug, e.g. `accounting` (directory is `<industry>-team/`).
    pub industry: String,
    /// Front-desk source pack directory, e.g. `accounting-pro`.
    pub pack: String,
    /// zh-TW industry label, e.g. `會計/記帳事務所`.
    pub label: String,
    pub front_desk: FrontDeskSpec,
    #[serde(default)]
    pub workers: Vec<WorkerSpec>,
    #[serde(default)]
    pub humans: Vec<HumanRoleSpec>,
    #[serde(default)]
    pub excluded: Vec<ExcludedKitSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FrontDeskSpec {
    /// Deployed agent name, e.g. `accounting-assistant`.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerSpec {
    /// Shared kit directory under `teams/_departments/`, e.g. `docs-admin`.
    pub kit: String,
    /// Deployed agent name, e.g. `accounting-docs`.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub summary: String,
    /// Compliance overlay entries (verbatim from TEAM.md) appended into the
    /// worker's CONTRACT.toml `must_not` and SOUL.md overlay section.
    #[serde(default)]
    pub overlay: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HumanRoleSpec {
    pub title: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExcludedKitSpec {
    pub kit: String,
    #[serde(default)]
    pub reason: String,
}

/// Compact listing entry for `templates.industries`.
#[derive(Debug, Clone)]
pub struct TeamSummary {
    pub industry: String,
    pub label: String,
    pub pack: String,
    pub worker_count: usize,
}

fn teams_dir(premium_dir: &Path) -> PathBuf {
    premium_dir.join("teams")
}

fn kit_dir(premium_dir: &Path, kit: &str) -> PathBuf {
    teams_dir(premium_dir).join("_departments").join(kit)
}

fn ceo_dir(premium_dir: &Path) -> PathBuf {
    teams_dir(premium_dir).join("_roles").join("ceo")
}

/// Load and sanity-check one team manifest. Fail-closed: unknown industry,
/// unsafe slugs anywhere, unreadable or unparsable file all return `Err`.
pub fn load_team_manifest(premium_dir: &Path, industry: &str) -> Result<TeamManifest, String> {
    if !is_safe_slug(industry) {
        return Err(format!("invalid industry slug: {industry:?}"));
    }
    let path = teams_dir(premium_dir)
        .join(format!("{industry}-team"))
        .join("team.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest: TeamManifest =
        toml::from_str(&raw).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

    if manifest.schema != 1 {
        return Err(format!("unsupported team.toml schema {}", manifest.schema));
    }
    if manifest.industry != industry {
        return Err(format!(
            "team.toml industry mismatch: dir says {industry:?}, file says {:?}",
            manifest.industry
        ));
    }
    if !is_safe_slug(&manifest.pack) {
        return Err(format!("invalid pack slug: {:?}", manifest.pack));
    }
    if !is_safe_slug(&manifest.front_desk.name) {
        return Err(format!(
            "invalid front_desk name: {:?}",
            manifest.front_desk.name
        ));
    }
    for w in &manifest.workers {
        if !is_safe_slug(&w.kit) || !is_safe_slug(&w.name) {
            return Err(format!("invalid worker kit/name: {:?}/{:?}", w.kit, w.name));
        }
        if !kit_dir(premium_dir, &w.kit).join("SOUL.md").is_file() {
            return Err(format!("worker kit not found on disk: {:?}", w.kit));
        }
    }
    if !premium_dir.join(&manifest.pack).join("SOUL.md").is_file() {
        return Err(format!("front-desk pack not found on disk: {:?}", manifest.pack));
    }
    Ok(manifest)
}

/// Enumerate industries that ship a machine-readable team manifest.
/// Silently skips directories whose team.toml is missing or fails validation
/// (a broken manifest must not take the whole listing down).
pub fn list_team_industries(premium_dir: &Path) -> Vec<TeamSummary> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(teams_dir(premium_dir)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(industry) = dir_name.strip_suffix("-team") else {
            continue;
        };
        match load_team_manifest(premium_dir, industry) {
            Ok(m) => out.push(TeamSummary {
                industry: m.industry,
                label: m.label,
                pack: m.pack,
                worker_count: m.workers.len(),
            }),
            Err(e) => {
                tracing::warn!(industry, error = %e, "skipping team with invalid team.toml");
            }
        }
    }
    out.sort_by(|a, b| a.industry.cmp(&b.industry));
    out
}

/// Does the cross-industry CEO kit exist on disk?
pub fn ceo_available(premium_dir: &Path) -> bool {
    ceo_dir(premium_dir).join("SOUL.md").is_file()
}

// ─────────────────────────────────────────────────────────────────────────────
// Role assembly — produce deploy-ready SOUL.md / CONTRACT.toml / agent.toml
// ─────────────────────────────────────────────────────────────────────────────

/// Stable role id of the team lead within a staged roster.
pub const FRONT_DESK_ROLE_ID: &str = "front-desk";
/// Stable role id of the cross-industry CEO kit.
pub const CEO_ROLE_ID: &str = "ceo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    FrontDesk,
    Worker,
    Ceo,
}

impl RoleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoleKind::FrontDesk => "front_desk",
            RoleKind::Worker => "worker",
            RoleKind::Ceo => "ceo",
        }
    }
}

/// A fully-assembled, deploy-ready role: what the dashboard shows in the
/// editor and what `templates.create_agent` writes to disk when the admin
/// keeps the defaults.
#[derive(Debug, Clone)]
pub struct AssembledRole {
    pub role_id: String,
    pub kind: RoleKind,
    pub name: String,
    pub display_name: String,
    pub trigger: String,
    pub reports_to: String,
    pub summary: String,
    pub soul_md: String,
    pub contract_toml: String,
    pub agent_toml: String,
    /// Source directory the non-editable extras (FAQ.json, wiki/) are copied
    /// from at create time. Only set for the front desk (industry pack).
    pub extras_dir: Option<PathBuf>,
}

fn read_kit_file(dir: &Path, file: &str) -> Result<String, String> {
    let path = dir.join(file);
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Patch identity fields into a template agent.toml, preserving comments and
/// layout (toml_edit round-trip). Empty `display_name`/`trigger` keep the
/// template default.
fn patch_agent_toml(
    src: &str,
    name: &str,
    display_name: &str,
    trigger: &str,
    reports_to: Option<&str>,
    department: Option<&str>,
    enable_cross_agent: bool,
) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut =
        src.parse().map_err(|e| format!("template agent.toml invalid: {e}"))?;
    let agent = doc
        .get_mut("agent")
        .and_then(|v| v.as_table_mut())
        .ok_or_else(|| "template agent.toml missing [agent]".to_string())?;
    agent["name"] = toml_edit::value(name);
    if !display_name.is_empty() {
        agent["display_name"] = toml_edit::value(display_name);
    }
    if !trigger.is_empty() {
        agent["trigger"] = toml_edit::value(trigger);
    }
    if let Some(rt) = reports_to {
        agent["reports_to"] = toml_edit::value(rt);
    }
    if let Some(dept) = department {
        if !duduclaw_core::is_valid_department(dept) {
            return Err(format!(
                "department '{}' is not a valid name (ASCII alphanumeric, '-', '_'; 1..=64 chars)",
                dept.escape_debug()
            ));
        }
        agent["department"] = toml_edit::value(dept);
    }
    if enable_cross_agent {
        // Team lead must be able to delegate to its workers over the bus.
        let perms = doc
            .entry("permissions")
            .or_insert(toml_edit::table())
            .as_table_mut()
            .ok_or_else(|| "template agent.toml [permissions] is not a table".to_string())?;
        perms["can_send_cross_agent"] = toml_edit::value(true);
    }
    Ok(doc.to_string())
}

/// Append compliance-overlay entries into the CONTRACT.toml `must_not` array,
/// preserving the file's comments and the array's one-entry-per-line layout.
fn append_overlay_to_contract(src: &str, overlay: &[String]) -> Result<String, String> {
    if overlay.is_empty() {
        return Ok(src.to_string());
    }
    let mut doc: toml_edit::DocumentMut =
        src.parse().map_err(|e| format!("template CONTRACT.toml invalid: {e}"))?;
    let arr = doc
        .get_mut("boundaries")
        .and_then(|v| v.as_table_mut())
        .and_then(|t| t.get_mut("must_not"))
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "template CONTRACT.toml missing [boundaries] must_not array".to_string())?;
    for entry in overlay {
        arr.push(entry.as_str());
    }
    // Re-apply the one-entry-per-line layout the kits use, so the appended
    // overlay reads like the hand-written entries above it.
    for item in arr.iter_mut() {
        item.decor_mut().set_prefix("\n    ");
    }
    arr.set_trailing("\n");
    arr.set_trailing_comma(true);
    Ok(doc.to_string())
}

/// Fill the worker SOUL.md `## Industry Overlay` placeholder section by
/// appending the overlay entries at the end of the file (the placeholder
/// section is the last section in every shared kit).
fn append_overlay_to_soul(src: &str, overlay: &[String]) -> String {
    if overlay.is_empty() {
        return src.to_string();
    }
    let mut out = src.trim_end().to_string();
    out.push_str("\n\n以下條目為所屬產業 TEAM.md 的合規 overlay（備妥時自動填入，與 CONTRACT.toml 同步）：\n\n");
    for entry in overlay {
        out.push_str("- ");
        out.push_str(entry);
        out.push('\n');
    }
    out
}

/// Assemble the front-desk (team lead) role from the industry pack.
fn assemble_front_desk(premium_dir: &Path, manifest: &TeamManifest) -> Result<AssembledRole, String> {
    let pack_dir = premium_dir.join(&manifest.pack);
    let soul_md = read_kit_file(&pack_dir, "SOUL.md")?;
    let contract_toml = read_kit_file(&pack_dir, "CONTRACT.toml")?;
    let agent_src = read_kit_file(&pack_dir, "agent.toml")?;
    let agent_toml = patch_agent_toml(
        &agent_src,
        &manifest.front_desk.name,
        &manifest.front_desk.display_name,
        "",
        None,
        None,
        true, // the team lead delegates to workers
    )?;
    // Surface trigger/display defaults from the patched doc for UI prefill.
    let (display_name, trigger) = agent_identity_fields(&agent_toml);
    Ok(AssembledRole {
        role_id: FRONT_DESK_ROLE_ID.to_string(),
        kind: RoleKind::FrontDesk,
        name: manifest.front_desk.name.clone(),
        display_name,
        trigger,
        reports_to: String::new(),
        summary: manifest.front_desk.summary.clone(),
        soul_md,
        contract_toml,
        agent_toml,
        extras_dir: Some(pack_dir),
    })
}

/// Assemble one department worker from its shared kit + overlay.
fn assemble_worker(
    premium_dir: &Path,
    manifest: &TeamManifest,
    spec: &WorkerSpec,
) -> Result<AssembledRole, String> {
    let dir = kit_dir(premium_dir, &spec.kit);
    let soul_src = read_kit_file(&dir, "SOUL.md")?;
    let contract_src = read_kit_file(&dir, "CONTRACT.toml")?;
    let agent_src = read_kit_file(&dir, "agent.toml")?;

    let soul_md = append_overlay_to_soul(&soul_src, &spec.overlay);
    let contract_toml = append_overlay_to_contract(&contract_src, &spec.overlay)?;
    let trigger = if spec.trigger.is_empty() { spec.name.clone() } else { spec.trigger.clone() };
    let agent_toml = patch_agent_toml(
        &agent_src,
        &spec.name,
        &spec.display_name,
        &trigger,
        Some(manifest.front_desk.name.as_str()),
        None,
        false,
    )?;
    let (display_name, trigger) = agent_identity_fields(&agent_toml);
    Ok(AssembledRole {
        role_id: spec.name.clone(),
        kind: RoleKind::Worker,
        name: spec.name.clone(),
        display_name,
        trigger,
        reports_to: manifest.front_desk.name.clone(),
        summary: spec.summary.clone(),
        soul_md,
        contract_toml,
        agent_toml,
        extras_dir: None,
    })
}

/// Assemble the cross-industry CEO kit. Available without staging an
/// industry — it's the suggested template for the very first agent.
pub fn assemble_ceo(premium_dir: &Path) -> Result<AssembledRole, String> {
    let dir = ceo_dir(premium_dir);
    let soul_md = read_kit_file(&dir, "SOUL.md")?;
    let contract_toml = read_kit_file(&dir, "CONTRACT.toml")?;
    let agent_toml = read_kit_file(&dir, "agent.toml")?;
    let (display_name, trigger) = agent_identity_fields(&agent_toml);
    let name = agent_name_field(&agent_toml).unwrap_or_else(|| "ceo-assistant".to_string());
    Ok(AssembledRole {
        role_id: CEO_ROLE_ID.to_string(),
        kind: RoleKind::Ceo,
        name,
        display_name,
        trigger,
        reports_to: String::new(),
        summary: "老闆的營運總管：拆解任務、派工團隊、匯整回報，不可逆決定留給老闆".to_string(),
        soul_md,
        contract_toml,
        agent_toml,
        extras_dir: None,
    })
}

/// Force identity fields into a (possibly admin-edited) agent.toml before it
/// is written to disk: the directory name and `[agent].name` must agree, and
/// form-level display_name/trigger overrides win over in-file edits. Empty
/// `display_name`/`trigger` keep whatever the document says. Fail-closed:
/// an unparsable document is rejected, never written as-is.
pub fn override_agent_identity(
    src: &str,
    name: &str,
    display_name: &str,
    trigger: &str,
    reports_to: Option<&str>,
    department: Option<&str>,
) -> Result<String, String> {
    patch_agent_toml(src, name, display_name, trigger, reports_to, department, false)
}

/// Assemble a role by its stable id within a staged team.
pub fn assemble_role(
    premium_dir: &Path,
    manifest: &TeamManifest,
    role_id: &str,
) -> Result<AssembledRole, String> {
    if role_id == FRONT_DESK_ROLE_ID {
        return assemble_front_desk(premium_dir, manifest);
    }
    if role_id == CEO_ROLE_ID {
        return assemble_ceo(premium_dir);
    }
    let spec = manifest
        .workers
        .iter()
        .find(|w| w.name == role_id)
        .ok_or_else(|| format!("unknown role_id {role_id:?} for team {:?}", manifest.industry))?;
    assemble_worker(premium_dir, manifest, spec)
}

fn agent_identity_fields(agent_toml: &str) -> (String, String) {
    let parsed: toml::Table = agent_toml.parse().unwrap_or_default();
    let agent = parsed.get("agent").and_then(|v| v.as_table());
    let get = |key: &str| {
        agent
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    (get("display_name"), get("trigger"))
}

fn agent_name_field(agent_toml: &str) -> Option<String> {
    let parsed: toml::Table = agent_toml.parse().ok()?;
    parsed
        .get("agent")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_slug_accepts_known_premium_dirs() {
        for s in ["ecommerce-pro", "clinic-pro", "realestate-pro", "education-pro"] {
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
            "_departments",
            "_roles",
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
        let tmp = std::env::temp_dir().join(format!("dudu-premium-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("foo-pro")).unwrap();
        std::fs::write(tmp.join("foo-pro").join("SOUL.md"), "# x").unwrap();
        std::fs::create_dir_all(tmp.join("bar-pro")).unwrap();
        std::fs::create_dir_all(tmp.join(".sneaky")).unwrap();
        std::fs::write(tmp.join(".sneaky").join("SOUL.md"), "# x").unwrap();
        // teams/ must never surface as an industry pack (no root SOUL.md, and
        // even with one it would need to be a direct safe-slug dir).
        std::fs::create_dir_all(tmp.join("teams").join("x-team")).unwrap();

        let found = discover_in(&tmp);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].slug, "foo-pro");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn fixture_premium_dir(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "dudu-team-fixture-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);

        // Industry pack (front desk source).
        let pack = tmp.join("foo-pro");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("SOUL.md"), "# Foo 總機\n").unwrap();
        std::fs::write(
            pack.join("CONTRACT.toml"),
            "[boundaries]\nmust_not = [\n    \"a\",\n]\nmust_always = []\n",
        )
        .unwrap();
        std::fs::write(
            pack.join("agent.toml"),
            "# pack comment\n[agent]\nname = \"foo-pro\"\ndisplay_name = \"Foo 前台\"\nrole = \"main\"\ntrigger = \"@foo\"\n\n[permissions]\ncan_send_cross_agent = false\n",
        )
        .unwrap();

        // Shared worker kit.
        let kit = tmp.join("teams").join("_departments").join("docs-admin");
        std::fs::create_dir_all(&kit).unwrap();
        std::fs::write(kit.join("SOUL.md"), "# 文件行政\n\n## Industry Overlay（部署時由 TEAM.md 填入）\n").unwrap();
        std::fs::write(
            kit.join("CONTRACT.toml"),
            "# kit comment\n[boundaries]\nmust_not = [\n    \"base rule（底線）\",\n]\nmust_always = [\n    \"x\",\n]\nmax_tool_calls_per_turn = 5\n",
        )
        .unwrap();
        std::fs::write(
            kit.join("agent.toml"),
            "# kit header comment\n[agent]\nname = \"dept-docs\"\ndisplay_name = \"文件行政助理\"\nrole = \"worker\"\ntrigger = \"team-docs\"\nreports_to = \"CHANGE-ME\"\n",
        )
        .unwrap();

        // CEO kit.
        let ceo = tmp.join("teams").join("_roles").join("ceo");
        std::fs::create_dir_all(&ceo).unwrap();
        std::fs::write(ceo.join("SOUL.md"), "# 營運總管\n").unwrap();
        std::fs::write(ceo.join("CONTRACT.toml"), "[boundaries]\nmust_not = []\nmust_always = []\n").unwrap();
        std::fs::write(
            ceo.join("agent.toml"),
            "[agent]\nname = \"ceo-assistant\"\ndisplay_name = \"營運總管\"\nrole = \"main\"\ntrigger = \"@營運總管\"\n",
        )
        .unwrap();

        // Team manifest.
        let team = tmp.join("teams").join("foo-team");
        std::fs::create_dir_all(&team).unwrap();
        std::fs::write(
            team.join("team.toml"),
            r#"schema = 1
industry = "foo"
pack = "foo-pro"
label = "Foo 產業"

[front_desk]
name = "foo-assistant"
display_name = "Foo 前台"
summary = "總機"

[[workers]]
kit = "docs-admin"
name = "foo-docs"
display_name = "文件行政助理"
trigger = "foo-docs"
summary = "補件催收"
overlay = [
  "overlay rule one（中文說明）",
  "overlay rule two",
]

[[humans]]
title = "持照專業人員"
summary = "留真人"
"#,
        )
        .unwrap();
        tmp
    }

    #[test]
    fn manifest_loads_and_lists() {
        let tmp = fixture_premium_dir("list");
        let teams = list_team_industries(&tmp);
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].industry, "foo");
        assert_eq!(teams[0].worker_count, 1);
        let m = load_team_manifest(&tmp, "foo").unwrap();
        assert_eq!(m.pack, "foo-pro");
        assert!(ceo_available(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn manifest_rejects_traversal_and_mismatch() {
        let tmp = fixture_premium_dir("reject");
        assert!(load_team_manifest(&tmp, "../foo").is_err());
        assert!(load_team_manifest(&tmp, "nope").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn worker_assembly_wires_identity_and_overlay() {
        let tmp = fixture_premium_dir("worker");
        let m = load_team_manifest(&tmp, "foo").unwrap();
        let role = assemble_role(&tmp, &m, "foo-docs").unwrap();
        assert_eq!(role.kind, RoleKind::Worker);
        assert_eq!(role.reports_to, "foo-assistant");
        // agent.toml patched, comment preserved.
        assert!(role.agent_toml.contains("# kit header comment"));
        assert!(role.agent_toml.contains("name = \"foo-docs\""));
        assert!(role.agent_toml.contains("reports_to = \"foo-assistant\""));
        assert!(!role.agent_toml.contains("CHANGE-ME"));
        // CONTRACT gains overlay entries and still parses; base rule intact.
        let parsed: toml::Table = role.contract_toml.parse().unwrap();
        let must_not = parsed["boundaries"]["must_not"].as_array().unwrap();
        assert_eq!(must_not.len(), 3);
        assert!(role.contract_toml.contains("# kit comment"));
        assert!(role.contract_toml.contains("overlay rule one（中文說明）"));
        // SOUL got the overlay block appended.
        assert!(role.soul_md.contains("- overlay rule two"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn front_desk_assembly_enables_delegation() {
        let tmp = fixture_premium_dir("frontdesk");
        let m = load_team_manifest(&tmp, "foo").unwrap();
        let role = assemble_role(&tmp, &m, FRONT_DESK_ROLE_ID).unwrap();
        assert_eq!(role.kind, RoleKind::FrontDesk);
        assert_eq!(role.name, "foo-assistant");
        assert!(role.agent_toml.contains("# pack comment"));
        assert!(role.agent_toml.contains("can_send_cross_agent = true"));
        assert!(role.extras_dir.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ceo_assembly_available_without_team() {
        let tmp = fixture_premium_dir("ceo");
        let role = assemble_ceo(&tmp).unwrap();
        assert_eq!(role.role_id, CEO_ROLE_ID);
        assert_eq!(role.name, "ceo-assistant");
        assert_eq!(role.display_name, "營運總管");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unknown_role_id_fails_closed() {
        let tmp = fixture_premium_dir("unknown");
        let m = load_team_manifest(&tmp, "foo").unwrap();
        assert!(assemble_role(&tmp, &m, "not-a-role").is_err());
        assert!(assemble_role(&tmp, &m, "../../etc/passwd").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
