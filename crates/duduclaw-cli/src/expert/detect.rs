//! Fail-closed format detection for an expanded pack directory.
//!
//! Order matters — the first matching signature wins. An unrecognised layout
//! is **rejected** (never guessed), and the caller lists the directory contents
//! so the operator can see what was actually shipped.

use std::path::{Path, PathBuf};

/// A recognised pack format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// DuDuClaw-native `expert.toml`.
    Native,
    /// Claude Code plugin (`.claude-plugin/plugin.json`).
    ClaudePlugin,
    /// A single Agent Skill (`SKILL.md` at the root, or a lone skill dir).
    Skill,
    /// Another platform's agent definition (豆包, OpenClaw, Hermes, Dify,
    /// AGENTS.md family, Claude/Copilot subagents) — see
    /// [`super::agent_config`].
    AgentConfig(super::agent_config::ConfigKind),
}

/// Detect the format of `dir`. Returns `None` when nothing matches (caller
/// rejects + lists contents).
///
/// Order: native and plugin manifests first (they are explicit), then the
/// agent-definition formats whose signature is a manifest or config file
/// (豆包 / OpenClaw / Hermes / Dify), then a root `SKILL.md`, and last the
/// formats recognised by well-known file names only (persona workspace,
/// subagents, instruction files) — a skill repo that also carries an
/// AGENTS.md stays a skill, as before.
pub fn detect(dir: &Path) -> Option<Format> {
    use super::agent_config::ConfigKind as K;
    if dir.join("expert.toml").is_file() {
        return Some(Format::Native);
    }
    if dir.join(".claude-plugin").join("plugin.json").is_file() {
        return Some(Format::ClaudePlugin);
    }
    let config = super::agent_config::detect::detect(dir);
    if let Some(k @ (K::Doubao | K::OpenClawState | K::Hermes | K::Dify)) = config {
        return Some(Format::AgentConfig(k));
    }
    if dir.join("SKILL.md").is_file() {
        return Some(Format::Skill);
    }
    config.map(Format::AgentConfig)
}

/// OS clutter that never counts as content (also filtered at extraction;
/// a plain directory source can still carry it).
fn is_clutter(name: &str) -> bool {
    super::zip_names::is_junk_basename(name.as_bytes())
}

/// Some archives wrap everything in a single top-level directory
/// (`slug/expert.toml` rather than `expert.toml`). Resolve to the directory
/// that actually holds the manifest / signature. Returns `root` unchanged when
/// a signature is already present at the top level.
///
/// OS clutter (`__MACOSX/`, `.DS_Store`, …) does not count, so a macOS zip of
/// one folder still descends. When nothing is recognised either way, a lone
/// non-hidden wrapper directory is still entered, so the rejection listing
/// shows what the archive actually contains.
pub fn resolve_root(root: &Path) -> PathBuf {
    if detect(root).is_some() {
        return root.to_path_buf();
    }
    let mut child_dirs = Vec::new();
    let mut has_top_files = false;
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_clutter(&name) {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                child_dirs.push((name, p));
            } else if p.is_file() {
                has_top_files = true;
            }
        }
    }
    if !has_top_files && child_dirs.len() == 1 {
        let (name, child) = &child_dirs[0];
        if detect(child).is_some() || !name.starts_with('.') {
            return child.clone();
        }
    }
    root.to_path_buf()
}

/// Shallow listing of a directory (names only, dirs suffixed `/`) for the
/// "unrecognised format" rejection message. Sorted, capped at 40 entries,
/// OS clutter left out.
pub fn list_contents(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_clutter(&name) {
                continue;
            }
            let suffix = if entry.path().is_dir() { "/" } else { "" };
            out.push(format!("{name}{suffix}"));
        }
    }
    out.sort();
    out.truncate(40);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("dc-detect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detects_each_format_in_order() {
        let d = tmp();
        assert_eq!(detect(&d), None);

        std::fs::write(d.join("SKILL.md"), "x").unwrap();
        assert_eq!(detect(&d), Some(Format::Skill));

        std::fs::create_dir_all(d.join(".claude-plugin")).unwrap();
        std::fs::write(d.join(".claude-plugin/plugin.json"), "{}").unwrap();
        assert_eq!(detect(&d), Some(Format::ClaudePlugin)); // plugin beats skill

        std::fs::write(d.join("expert.toml"), "x").unwrap();
        assert_eq!(detect(&d), Some(Format::Native)); // native wins

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_root_descends_single_wrapper_dir() {
        let d = tmp();
        let inner = d.join("pharmacy-pro");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("expert.toml"), "x").unwrap();
        assert_eq!(resolve_root(&d), inner);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_root_ignores_macos_clutter_when_descending() {
        let d = tmp();
        let inner = d.join("報告顧問");
        std::fs::create_dir_all(inner.join("agents/a1/persona")).unwrap();
        std::fs::write(inner.join("agents/a1/agent.yml"), "name: x").unwrap();
        std::fs::create_dir_all(d.join("__MACOSX")).unwrap();
        std::fs::write(d.join(".DS_Store"), "x").unwrap();
        assert_eq!(resolve_root(&d), inner);
        assert_eq!(
            detect(&inner),
            Some(Format::AgentConfig(super::super::agent_config::ConfigKind::Doubao))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn skill_still_beats_name_only_agent_formats() {
        let d = tmp();
        std::fs::write(d.join("SKILL.md"), "x").unwrap();
        std::fs::write(d.join("AGENTS.md"), "x").unwrap();
        assert_eq!(detect(&d), Some(Format::Skill));
        std::fs::write(d.join("manifest.json"), r#"{"units":[]}"#).unwrap();
        assert_eq!(
            detect(&d),
            Some(Format::AgentConfig(super::super::agent_config::ConfigKind::Doubao))
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
