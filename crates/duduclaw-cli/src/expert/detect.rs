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
}

/// Detect the format of `dir`. Returns `None` when nothing matches (caller
/// rejects + lists contents).
pub fn detect(dir: &Path) -> Option<Format> {
    if dir.join("expert.toml").is_file() {
        return Some(Format::Native);
    }
    if dir.join(".claude-plugin").join("plugin.json").is_file() {
        return Some(Format::ClaudePlugin);
    }
    if dir.join("SKILL.md").is_file() {
        return Some(Format::Skill);
    }
    None
}

/// Some archives wrap everything in a single top-level directory
/// (`slug/expert.toml` rather than `expert.toml`). Resolve to the directory
/// that actually holds the manifest / signature. Returns `root` unchanged when
/// a signature is already present at the top level.
pub fn resolve_root(root: &Path) -> PathBuf {
    if detect(root).is_some() {
        return root.to_path_buf();
    }
    // Exactly one child directory and no signature at the top → descend once.
    let mut child_dirs = Vec::new();
    let mut has_top_files = false;
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                child_dirs.push(p);
            } else if p.is_file() {
                has_top_files = true;
            }
        }
    }
    if !has_top_files && child_dirs.len() == 1 && detect(&child_dirs[0]).is_some() {
        return child_dirs[0].clone();
    }
    root.to_path_buf()
}

/// Shallow listing of a directory (names only, dirs suffixed `/`) for the
/// "unrecognised format" rejection message. Sorted, capped at 40 entries.
pub fn list_contents(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
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
}
