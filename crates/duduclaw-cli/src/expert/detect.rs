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

/// How many single wrapper folders [`resolve_root`] steps through. Two
/// covers the common "zip of a folder that holds the exported folder"
/// (`下載/報告顧問/manifest.json`) and GitHub's `<repo>-<branch>/` archive
/// root around a pack folder.
pub const MAX_WRAPPER_DEPTH: usize = 2;

/// Some archives wrap everything in one or two top-level directories
/// (`slug/expert.toml` rather than `expert.toml`). Resolve to the directory
/// that actually holds the manifest / signature, stepping through at most
/// [`MAX_WRAPPER_DEPTH`] wrappers. Returns `root` unchanged when a signature
/// is already present at the top level.
///
/// OS clutter (`__MACOSX/`, `.DS_Store`, …) does not count, so a macOS zip of
/// one folder still descends. When nothing is recognised either way, a lone
/// non-hidden wrapper directory is still entered, so the rejection listing
/// shows what the archive actually contains.
///
/// Safe with the extraction fences: this only picks a subdirectory of a tree
/// that is already on disk. Symbolic links are never followed (a zip never
/// yields one; a plain directory source might), so the result always stays
/// inside `root`.
pub fn resolve_root(root: &Path) -> PathBuf {
    let mut current = root.to_path_buf();
    for _ in 0..MAX_WRAPPER_DEPTH {
        match step_into_wrapper(&current) {
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

/// The single wrapper directory below `dir`, when `dir` itself is not a
/// recognised pack and holds nothing but that one real (non-link) folder.
fn step_into_wrapper(dir: &Path) -> Option<PathBuf> {
    if detect(dir).is_some() {
        return None;
    }
    let mut child_dirs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_clutter(&name) {
            continue;
        }
        let ft = entry.file_type().ok()?;
        if ft.is_dir() {
            child_dirs.push((name, entry.path()));
        } else {
            // A file (or a link of any kind) next to the folder: not a wrapper.
            return None;
        }
    }
    match child_dirs.as_slice() {
        [(name, child)] if detect(child).is_some() || !name.starts_with('.') => Some(child.clone()),
        _ => None,
    }
}

/// A recognised layout found deeper than [`resolve_root`] looks: its format
/// code and its folder relative to the root (`/`-separated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedMatch {
    pub format: &'static str,
    pub rel: String,
}

/// Directory budget for [`find_nested`]: a hostile archive with thousands of
/// folders must not turn a rejection into a slow scan.
const NESTED_DIR_BUDGET: usize = 400;
const NESTED_MAX_DEPTH: usize = 5;
const NESTED_MAX_HITS: usize = 5;

/// Breadth-first search below `root` (not `root` itself) for folders that
/// would be recognised, so an "unrecognised" rejection can say "the pack is
/// in `a/b/c/` — zip that folder instead". Bounded in depth, folders visited
/// and hits; links and hidden or clutter folders are skipped.
pub fn find_nested(root: &Path) -> Vec<NestedMatch> {
    let mut hits = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, usize)> = std::collections::VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));
    let mut visited = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        if visited >= NESTED_DIR_BUDGET || hits.len() >= NESTED_MAX_HITS {
            break;
        }
        visited += 1;
        if depth > 0
            && let Some(f) = detect(&dir)
        {
            let rel = dir
                .strip_prefix(root)
                .map(|r| r.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
                .unwrap_or_default();
            hits.push(NestedMatch { format: format_code(f), rel: format!("{rel}/") });
            continue; // the folder itself is the answer; do not list its insides
        }
        if depth >= NESTED_MAX_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut children: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                !is_clutter(&n) && !n.starts_with('.')
            })
            .map(|e| e.path())
            .collect();
        children.sort();
        queue.extend(children.into_iter().map(|c| (c, depth + 1)));
    }
    hits
}

/// Stable format code (same vocabulary as the import report's `format`).
pub fn format_code(f: Format) -> &'static str {
    match f {
        Format::Native => "expert",
        Format::ClaudePlugin => "claude-plugin",
        Format::Skill => "skill",
        Format::AgentConfig(k) => k.code(),
    }
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

/// The fail-closed rejection of an unrecognised layout: printed for the
/// terminal, and coded `import.unrecognized` with the top-level entries
/// and any recognisable folder found deeper (`found`), relative to the
/// extraction root, so the dashboard can say "zip that folder instead".
pub fn unrecognized_error(
    extract_root: &Path,
    resolved_root: &Path,
    contents: Vec<String>,
) -> duduclaw_core::error::DuDuClawError {
    let nested = find_nested(extract_root);
    let unwrapped = resolved_root
        .strip_prefix(extract_root)
        .map(|r| r.components().count())
        .unwrap_or(0);
    eprintln!(
        "\n  ✗ 無法識別的格式。支援：DuDuClaw 專家包（expert.toml）、Claude Code 外掛、單一技能（SKILL.md）、\
         豆包夥伴安裝包、OpenClaw、Hermes、AGENTS.md／CLAUDE.md／GEMINI.md、Claude Code／Copilot 子代理、Dify 應用匯出檔。\n"
    );
    eprintln!("  目錄內容:");
    for c in &contents {
        // Names come from the upload: escaped, so a name holding a newline
        // cannot print a line of its own.
        eprintln!("    - {}", c.escape_debug());
    }
    for n in &nested {
        eprintln!(
            "  在更深的資料夾 {} 找到可以匯入的內容（{}）：請直接壓縮那個資料夾再匯入",
            n.rel.escape_debug(),
            n.format
        );
    }
    eprintln!();
    let found: Vec<serde_json::Value> = nested
        .iter()
        .map(|n| serde_json::json!({ "format": n.format, "path": n.rel }))
        .collect();
    super::import_error::import_err(
        "import.unrecognized",
        serde_json::json!({
            "entries": contents,
            "found": found,
            "unwrapped": unwrapped,
            "max_unwrap": MAX_WRAPPER_DEPTH,
        }),
        "未識別格式，拒絕安裝",
    )
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
    fn resolve_root_descends_two_wrappers_but_not_three() {
        let d = tmp();
        let two = d.join("下載").join("報告顧問");
        std::fs::create_dir_all(&two).unwrap();
        std::fs::write(two.join("expert.toml"), "x").unwrap();
        std::fs::create_dir_all(d.join("__MACOSX/下載")).unwrap();
        assert_eq!(resolve_root(&d), two);

        let e = tmp();
        let three = e.join("a").join("b").join("c");
        std::fs::create_dir_all(&three).unwrap();
        std::fs::write(three.join("expert.toml"), "x").unwrap();
        let resolved = resolve_root(&e);
        assert_eq!(resolved, e.join("a").join("b"));
        assert_eq!(detect(&resolved), None);
        // The rejection can still point at where the pack really is.
        let nested = find_nested(&e);
        assert_eq!(nested, vec![NestedMatch { format: "expert", rel: "a/b/c/".into() }]);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&e);
    }

    #[test]
    fn resolve_root_stops_at_a_folder_with_files_next_to_it() {
        let d = tmp();
        std::fs::create_dir_all(d.join("pack")).unwrap();
        std::fs::write(d.join("pack/expert.toml"), "x").unwrap();
        std::fs::write(d.join("notes.txt"), "x").unwrap();
        assert_eq!(resolve_root(&d), d);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_root_never_follows_a_symlinked_wrapper() {
        let outside = tmp();
        std::fs::write(outside.join("expert.toml"), "x").unwrap();
        let d = tmp();
        std::os::unix::fs::symlink(&outside, d.join("wrapper")).unwrap();
        assert_eq!(resolve_root(&d), d);
        assert!(find_nested(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
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
