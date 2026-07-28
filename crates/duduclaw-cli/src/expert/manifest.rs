//! `expert.toml` — the DuDuClaw-native expert-pack manifest (WP2.1).
//!
//! An expert pack bundles a *team* (one or more agents in a `reports_to`
//! hierarchy) with the skills, wiki SOPs, recommended prompts and channel
//! hints that make that team useful out of the box. The manifest is the
//! machine-readable index; the actual persona / settings live beside it:
//!
//! ```text
//! <slug>/
//! ├── expert.toml                    # this manifest
//! ├── agents/<name>/soul.md          # persona (→ SOUL.md)
//! ├── agents/<name>/agent.partial.toml  # settings fragment (deep-merged)
//! ├── skills/<name>/SKILL.md         # Agent Skills spec, verbatim
//! ├── wiki/<ns>/*.md                 # shared-wiki SOP/policy pages
//! └── README.md
//! ```
//!
//! Parsing is total: unknown keys are ignored (forward-compat), missing
//! optional sections default empty. Validation is separate ([`validate`]) so
//! `install` can be lenient (warn) while `pack` is strict (reject).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Root of `expert.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExpertManifest {
    pub expert: ExpertSection,
}

/// The single `[expert]` section (with nested sub-tables).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExpertSection {
    /// Machine-stable slug — also the on-disk pack id. Lowercase
    /// alphanumeric + hyphen (validated against [`crate::is_valid_agent_id`]
    /// shape at install/pack time).
    pub name: String,
    /// Localised display names keyed by locale (`zh-TW` / `en` / …).
    #[serde(default)]
    pub display_name: HashMap<String, String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Catalog category slug (`health` / `professional` / `retail` /
    /// `lifestyle` / `education` / `other`) — dashboard section grouping.
    /// Empty ⇒ the catalog derives one (or files under `other`).
    #[serde(default)]
    pub category: String,
    /// `[expert.prompts]` — recommended prompt strings to surface in the UI.
    #[serde(default)]
    pub prompts: Prompts,
    /// `[expert.channels]` — suggested channels for this team.
    #[serde(default)]
    pub channels: Channels,
    /// `[[expert.agents]]` — the team roster (in-pack relative hierarchy).
    #[serde(default)]
    pub agents: Vec<ExpertAgent>,
    /// `[expert.requires]` — doctor-style environment prerequisites.
    #[serde(default)]
    pub requires: Requires,
}

/// Recommended prompt strings.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Prompts {
    #[serde(default)]
    pub recommended: Vec<String>,
}

/// Suggested channels.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Channels {
    #[serde(default)]
    pub suggested: Vec<String>,
}

/// One roster member.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExpertAgent {
    /// In-pack agent name (also the `agents/<name>/` directory).
    pub name: String,
    /// Canonical DuDuClaw role string (e.g. `front_desk`, `worker`).
    #[serde(default)]
    pub role: String,
    /// Localised display label (falls back to `name`).
    #[serde(default)]
    pub display_name: String,
    /// In-pack supervisor by `name`. Empty = team root.
    #[serde(default)]
    pub reports_to: String,
    /// Functional department this member joins on install (written to
    /// `[agent] department`, org-chart / departments-page visible). Empty ⇒
    /// no department, the pre-WP-ORG behaviour.
    #[serde(default)]
    pub department: String,
    /// Display rank (`executive` / `manager` / `staff`). Informational for
    /// the catalog UI; empty ⇒ derived from `role` via
    /// `duduclaw_core::org::rank_for_role`.
    #[serde(default)]
    pub rank: String,
    /// Trigger keyword (defaults to `@<display_name>` when blank).
    #[serde(default)]
    pub trigger: String,
    /// Skills this member uses. A name present in the pack's `skills/`
    /// directory is installed; anything else is assumed pre-installed and
    /// reported as external.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Doctor-style prerequisites (borrowed from OpenClaw
/// `metadata.openclaw.requires` semantics). Missing items warn, never block.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Requires {
    /// Environment variables that should be set (checked, not enforced).
    #[serde(default)]
    pub env: Vec<String>,
    /// Executables that should be on `PATH`.
    #[serde(default)]
    pub bins: Vec<String>,
}

impl ExpertSection {
    /// Display name for `locale`, falling back `locale → zh-TW → en → slug`.
    pub fn display(&self, locale: &str) -> &str {
        self.display_name
            .get(locale)
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                self.display_name
                    .get("zh-TW")
                    .map(|s| s.as_str())
                    .filter(|s| !s.trim().is_empty())
            })
            .or_else(|| {
                self.display_name
                    .get("en")
                    .map(|s| s.as_str())
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or(&self.name)
    }
}

/// Parse `expert.toml` content. Unknown keys are ignored.
pub fn parse(content: &str) -> Result<ExpertManifest, String> {
    toml::from_str::<ExpertManifest>(content).map_err(|e| format!("expert.toml 解析失敗: {e}"))
}

/// Read + parse `<dir>/expert.toml`.
pub fn read(dir: &Path) -> Result<ExpertManifest, String> {
    let path = dir.join("expert.toml");
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("讀取 {} 失敗: {e}", path.display()))?;
    parse(&content)
}

/// A single validation problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub field: String,
    pub message: String,
}

/// Strict, spec-level validation used by `expert pack`. Checks that the
/// manifest is internally consistent and that every referenced on-disk asset
/// exists. Returns all problems (never fabricates a fix).
pub fn validate(manifest: &ExpertManifest, dir: &Path) -> Vec<Problem> {
    let mut problems = Vec::new();
    let e = &manifest.expert;

    let push = |problems: &mut Vec<Problem>, field: &str, message: String| {
        problems.push(Problem {
            field: field.to_string(),
            message,
        });
    };

    // ── [expert] core ──
    if e.name.trim().is_empty() {
        push(
            &mut problems,
            "expert.name",
            "缺少 name（pack slug）".into(),
        );
    } else if !crate::is_valid_agent_id(&e.name) {
        push(
            &mut problems,
            "expert.name",
            format!(
                "'{}' 非合法 slug（小寫英數與連字號、1..=64、首尾非連字號）",
                e.name.escape_debug()
            ),
        );
    }
    if e.version.trim().is_empty() {
        push(&mut problems, "expert.version", "缺少 version".into());
    }
    if e.description.trim().is_empty() {
        push(
            &mut problems,
            "expert.description",
            "缺少 description（一句話說明這個專家包解決什麼問題）".into(),
        );
    }

    // ── agents ──
    if e.agents.is_empty() {
        push(
            &mut problems,
            "expert.agents",
            "至少要有一個 [[expert.agents]]".into(),
        );
    }
    let names: std::collections::BTreeSet<&str> =
        e.agents.iter().map(|a| a.name.as_str()).collect();
    if names.len() != e.agents.len() {
        push(&mut problems, "expert.agents", "agent name 有重複".into());
    }
    for a in &e.agents {
        let field = format!("agents.{}", a.name);
        if a.name.trim().is_empty() {
            push(&mut problems, "expert.agents", "有 agent 缺少 name".into());
            continue;
        }
        // The install sink lowercases/sanitises, but pack asks authors to keep
        // the on-disk dir name already valid so the mapping is 1:1.
        if !crate::is_valid_agent_id(&a.name) {
            push(
                &mut problems,
                &field,
                format!("'{}' 非合法 agent id", a.name.escape_debug()),
            );
        }
        // Org placement: department must be a safe path segment, rank must be
        // a known value (both optional).
        if !a.department.trim().is_empty()
            && !duduclaw_core::is_valid_department(a.department.trim())
        {
            push(
                &mut problems,
                &field,
                format!("department '{}' 非合法部門名", a.department.escape_debug()),
            );
        }
        if !a.rank.trim().is_empty() && duduclaw_core::org::OrgRank::parse(&a.rank).is_none() {
            push(
                &mut problems,
                &field,
                format!(
                    "rank '{}' 非法（executive / manager / staff）",
                    a.rank.escape_debug()
                ),
            );
        }
        // reports_to must reference an in-pack agent (or be empty = root).
        if !a.reports_to.trim().is_empty() && !names.contains(a.reports_to.as_str()) {
            push(
                &mut problems,
                &field,
                format!("reports_to '{}' 不在包內 roster", a.reports_to),
            );
        }
        // On-disk soul.md is required for a persona-carrying pack.
        let soul = dir.join("agents").join(&a.name).join("soul.md");
        if !soul.is_file() {
            push(&mut problems, &field, format!("缺少 {}", soul.display()));
        }
        // Referenced skills present in the pack must have a valid SKILL.md
        // whose frontmatter name equals the directory name (Agent Skills spec).
        for s in &a.skills {
            let sdir = dir.join("skills").join(s);
            if sdir.is_dir() {
                validate_pack_skill(&sdir, s, &mut problems);
            }
        }
    }

    // reports_to cycle detection.
    let nodes: Vec<(String, Option<String>)> = e
        .agents
        .iter()
        .map(|a| {
            let rt = a.reports_to.trim();
            (
                a.name.clone(),
                if rt.is_empty() {
                    None
                } else {
                    Some(rt.to_string())
                },
            )
        })
        .collect();
    if let super::topo::TopoOutcome::Cycle(ids) = super::topo::topo_sort(&nodes) {
        push(
            &mut problems,
            "expert.agents",
            format!("reports_to 有循環：{}", ids.join(", ")),
        );
    }

    // ── any skills/<name> present (even if unreferenced) must be spec-valid ──
    let skills_root = dir.join("skills");
    if let Ok(rd) = std::fs::read_dir(&skills_root) {
        for entry in rd.flatten() {
            if entry.path().is_dir() {
                let dname = entry.file_name().to_string_lossy().into_owned();
                validate_pack_skill(&entry.path(), &dname, &mut problems);
            }
        }
    }

    problems
}

/// A packaged skill must (spec) have a `SKILL.md` whose frontmatter `name`
/// equals its directory name, and that name must be a safe path component.
fn validate_pack_skill(sdir: &Path, dir_name: &str, problems: &mut Vec<Problem>) {
    let field = format!("skills.{dir_name}");
    let skill_md = sdir.join("SKILL.md");
    if !skill_md.is_file() {
        problems.push(Problem {
            field,
            message: format!("缺少 {}", skill_md.display()),
        });
        return;
    }
    if !duduclaw_agent::skill_loader::is_safe_skill_name(dir_name) {
        problems.push(Problem {
            field: field.clone(),
            message: format!("skill 目錄名 '{}' 非安全路徑元件", dir_name.escape_debug()),
        });
    }
    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
    let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(&content, dir_name);
    if meta.name != dir_name {
        problems.push(Problem {
            field,
            message: format!(
                "SKILL.md frontmatter name '{}' 與目錄名 '{}' 不一致（Agent Skills spec 要求相同）",
                meta.name, dir_name
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_and_display_fallback() {
        let src = r#"
[expert]
name = "pharmacy-pro"
description = "社區藥局團隊"
version = "1.0.0"

[expert.display_name]
"zh-TW" = "社區藥局"

[[expert.agents]]
name = "front"
role = "front_desk"
"#;
        let m = parse(src).unwrap();
        assert_eq!(m.expert.name, "pharmacy-pro");
        assert_eq!(m.expert.display("en"), "社區藥局"); // en absent → zh-TW
        assert_eq!(m.expert.agents.len(), 1);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let src = r#"
[expert]
name = "x"
version = "1"
description = "d"
future_field = "ok"

[[expert.agents]]
name = "a"
"#;
        assert!(parse(src).is_ok());
    }
}
