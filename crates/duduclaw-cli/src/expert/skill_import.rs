//! Agent Skills single-skill importer (root `SKILL.md`) — P0.
//!
//! Adopts the Agent Skills spec verbatim (no bespoke schema). The whole pack
//! directory *is* the skill package (SKILL.md + any bundled resources); it is
//! installed as a global skill (`<home>/skills/<name>/`) shared by all agents.
//!
//! OpenClaw skills (which carry a `metadata.openclaw` block with `requires` /
//! `install` directives) are detected and imported as a standard skill, with
//! their OpenClaw-specific automation reported as **P1 / not executed** — we
//! never run a foreign install script during import.

use std::path::Path;

use super::install::{InstallCtx, install_skill_global};
use super::{InstallRecord, PackKind, Report, cfg_err, now_iso};

use duduclaw_core::error::Result;

pub(super) async fn install(
    ctx: &InstallCtx,
    dir: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    let skill_md = dir.join("SKILL.md");
    let content = std::fs::read_to_string(&skill_md)
        .map_err(|e| cfg_err(format!("讀取 SKILL.md 失敗: {e}")))?;

    // Frontmatter name (Agent Skills spec identity); fallback to dir name.
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("skill")
        .to_string();
    let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(&content, &dir_name);
    let name = crate::sanitize_agent_id(&meta.name);
    if !duduclaw_agent::skill_loader::is_safe_skill_name(&name) {
        return Err(cfg_err(format!(
            "SKILL.md name '{}' 非安全技能名稱",
            meta.name.escape_debug()
        )));
    }

    if super::read_record(&ctx.home, &name).is_some() {
        return Err(cfg_err(format!(
            "專家包 '{name}' 已安裝（先 `expert remove {name}` 再重裝）"
        )));
    }

    // OpenClaw provenance → import as standard skill, flag automation as P1.
    if detect_openclaw(&content) {
        report.warning(
            "openclaw",
            &name,
            "偵測到 metadata.openclaw；requires/install 自動化為 P1，未執行任何安裝腳本",
        );
    }
    if !meta.tools.is_empty() {
        report.ignored(
            "skill-tools",
            &name,
            format!("宣告工具: {}", meta.tools.join(", ")),
        );
    }

    let mut record = InstallRecord {
        slug: name.clone(),
        kind: PackKind::Skill,
        display_name: if meta.description.is_empty() {
            name.clone()
        } else {
            duduclaw_core::truncate_chars(&meta.description, 60)
        },
        version: meta.version.clone(),
        description: meta.description.clone(),
        agents: Vec::new(),
        global_skills: Vec::new(),
        wiki_files: Vec::new(),
        installed_at: now_iso(),
    };

    // The scan + copy + record all happen in the shared primitive (which uses
    // the directory name for the SKILL.md lookup, so pass the resolved `name`).
    install_skill_global(ctx, dir, &name, report, &mut record);

    Ok(record)
}

/// Heuristic: does the frontmatter carry a `metadata.openclaw` block?
fn detect_openclaw(content: &str) -> bool {
    // Parse only the frontmatter region.
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = normalized.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    let mut yaml_lines = Vec::new();
    for line in lines {
        if line.trim_end() == "---" {
            break;
        }
        yaml_lines.push(line);
    }
    let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&yaml_lines.join("\n")) else {
        return false;
    };
    val.get("metadata")
        .and_then(|m| m.get("openclaw"))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_openclaw_block() {
        let with = "---\nname: x\nmetadata:\n  openclaw:\n    requires: [git]\n---\nbody";
        assert!(detect_openclaw(with));
        let without = "---\nname: x\ndescription: d\n---\nbody";
        assert!(!detect_openclaw(without));
        assert!(!detect_openclaw("no frontmatter"));
    }
}
