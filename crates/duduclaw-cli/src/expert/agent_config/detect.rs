//! Signature detection for the agent-definition formats the importer reads.
//! First match wins; nothing matching means the caller rejects the source
//! with a listing (fail closed). Order is chosen so the more specific
//! layout always beats a generic one that happens to share a file name
//! (Hermes and OpenClaw both ship `SOUL.md`; every coding-agent repo ships
//! `AGENTS.md`).

use std::path::{Path, PathBuf};

/// A recognised agent-definition format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// 豆包「夥伴安裝包」(`manifest.json` + `agents/<ref>/{agent.yml,persona/}`).
    Doubao,
    /// OpenClaw state dir (`openclaw.json` + `workspace*/`).
    OpenClawState,
    /// Hermes home (`config.yaml` + `SOUL.md` / `memories/`).
    Hermes,
    /// Dify app DSL (`*.yml` with `app:` + `kind: app`).
    Dify,
    /// A persona workspace: `SOUL.md` (+ AGENTS / IDENTITY / USER / memory …).
    Workspace,
    /// Claude Code subagents (`.claude/agents/*.md`) or GitHub Copilot custom
    /// agents (`*.agent.md`, legacy `*.chatmode.md`).
    Subagents,
    /// Plain instruction files (`AGENTS.md` / `CLAUDE.md` / `GEMINI.md` …).
    Instructions,
}

impl ConfigKind {
    /// zh-TW label used in reports and the dashboard.
    pub fn label(self) -> &'static str {
        match self {
            ConfigKind::Doubao => "豆包夥伴安裝包",
            ConfigKind::OpenClawState => "OpenClaw 資料夾",
            ConfigKind::Hermes => "Hermes 資料夾",
            ConfigKind::Dify => "Dify 應用匯出檔",
            ConfigKind::Workspace => "人格工作區（SOUL.md）",
            ConfigKind::Subagents => "Claude Code／Copilot 子代理",
            ConfigKind::Instructions => "操作指令檔（AGENTS.md 等）",
        }
    }

    /// Short ASCII code (fallback ids, archive folder names, JSON).
    pub fn code(self) -> &'static str {
        match self {
            ConfigKind::Doubao => "doubao",
            ConfigKind::OpenClawState => "openclaw",
            ConfigKind::Hermes => "hermes",
            ConfigKind::Dify => "dify",
            ConfigKind::Workspace => "workspace",
            ConfigKind::Subagents => "subagent",
            ConfigKind::Instructions => "instructions",
        }
    }
}

/// Instruction files in precedence order (first present wins; the rest are
/// reported as skipped). AGENTS.md first: it is the open standard most
/// tools read, and Claude Code / Gemini files usually repeat or `@`-import it.
pub const INSTRUCTION_PRECEDENCE: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    "AGENT.md",
    ".github/copilot-instructions.md",
];

pub fn detect(dir: &Path) -> Option<ConfigKind> {
    if is_doubao(dir) {
        return Some(ConfigKind::Doubao);
    }
    if ["openclaw.json", "moltbot.json", "clawdbot.json"]
        .iter()
        .any(|f| dir.join(f).is_file())
    {
        return Some(ConfigKind::OpenClawState);
    }
    if dir.join("config.yaml").is_file()
        && (dir.join("SOUL.md").is_file() || dir.join("memories").is_dir())
    {
        return Some(ConfigKind::Hermes);
    }
    if dify_dsl_file(dir).is_some() {
        return Some(ConfigKind::Dify);
    }
    if dir.join("SOUL.md").is_file() {
        return Some(ConfigKind::Workspace);
    }
    if !subagent_files(dir).is_empty() {
        return Some(ConfigKind::Subagents);
    }
    if INSTRUCTION_PRECEDENCE.iter().any(|f| dir.join(f).is_file()) {
        return Some(ConfigKind::Instructions);
    }
    None
}

/// `manifest.json` with a `units` array, or at least one
/// `agents/<ref>/agent.yml` next to `agents/<ref>/persona/`.
fn is_doubao(dir: &Path) -> bool {
    let manifest_units = std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .map(|v| v.get("units").map(|u| u.is_array()).unwrap_or(false))
        .unwrap_or(false);
    manifest_units || !doubao_agent_dirs(dir).is_empty()
}

/// `agents/<ref>/` directories that carry `agent.yml` and `persona/`.
pub fn doubao_agent_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir.join("agents"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.join("agent.yml").is_file() && p.join("persona").is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The single Dify DSL file at the root, if there is exactly one.
pub fn dify_dsl_file(dir: &Path) -> Option<PathBuf> {
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        let is_yaml = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
            .unwrap_or(false);
        if !is_yaml || !p.is_file() {
            continue;
        }
        let Some(text) = super::md::read_text(&p) else {
            continue;
        };
        let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let has_app = v.get("app").map(|a| a.is_mapping()).unwrap_or(false);
        let kind_app = v.get("kind").and_then(|k| k.as_str()) == Some("app");
        let has_version = v.get("version").is_some();
        if has_app && (kind_app || has_version) {
            hits.push(p);
        }
    }
    if hits.len() == 1 { hits.pop() } else { None }
}

/// Subagent definition files: `.claude/agents/*.md`, `.github/agents/*.md`,
/// `agents/*.md` with `name` + `description` frontmatter, and root-level
/// `*.agent.md` / `*.chatmode.md`. Sorted, de-duplicated.
pub fn subagent_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in [".claude/agents", ".github/agents", "agents"] {
        for p in md_files(&dir.join(sub)) {
            if has_agent_frontmatter(&p, sub == ".github/agents") {
                out.push(p);
            }
        }
    }
    for p in md_files(dir) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let lower = name.to_ascii_lowercase();
        let copilot = lower.ends_with(".agent.md") || lower.ends_with(".chatmode.md");
        if copilot && has_agent_frontmatter(&p, true) {
            out.push(p);
        } else if !copilot
            && !lower.eq("skill.md")
            && !INSTRUCTION_PRECEDENCE
                .iter()
                .any(|f| f.eq_ignore_ascii_case(name))
            && lower != "readme.md"
            && has_agent_frontmatter(&p, false)
        {
            // A lone `reviewer.md` uploaded on its own (single-file source).
            out.push(p);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("md"))
                            .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Claude Code requires `name` + `description`; Copilot only `description`.
fn has_agent_frontmatter(path: &Path, description_only: bool) -> bool {
    let Some(text) = super::md::read_text(path) else {
        return false;
    };
    let (Some(fm), _) = super::md::split_frontmatter(&text) else {
        return false;
    };
    fm.get("description").is_some() && (description_only || fm.get("name").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn doubao_beats_everything() {
        let d = tmp();
        write(&d.path().join("manifest.json"), r#"{"units":[]}"#);
        write(&d.path().join("SOUL.md"), "x");
        assert_eq!(detect(d.path()), Some(ConfigKind::Doubao));
        let e = tmp();
        write(&e.path().join("agents/a1/agent.yml"), "name: x");
        std::fs::create_dir_all(e.path().join("agents/a1/persona")).unwrap();
        assert_eq!(detect(e.path()), Some(ConfigKind::Doubao));
    }

    #[test]
    fn hermes_beats_workspace_and_workspace_beats_instructions() {
        let d = tmp();
        write(&d.path().join("SOUL.md"), "x");
        write(&d.path().join("AGENTS.md"), "x");
        assert_eq!(detect(d.path()), Some(ConfigKind::Workspace));
        write(&d.path().join("config.yaml"), "model: x");
        assert_eq!(detect(d.path()), Some(ConfigKind::Hermes));
        let i = tmp();
        write(&i.path().join("CLAUDE.md"), "x");
        assert_eq!(detect(i.path()), Some(ConfigKind::Instructions));
    }

    #[test]
    fn dify_dsl_needs_app_and_kind_or_version() {
        let d = tmp();
        write(
            &d.path().join("bot.yml"),
            "app:\n  name: 客服\nkind: app\nversion: 0.3.0\n",
        );
        assert_eq!(detect(d.path()), Some(ConfigKind::Dify));
        let n = tmp();
        write(&n.path().join("ci.yml"), "on: push\njobs: {}\n");
        assert_eq!(detect(n.path()), None);
    }

    #[test]
    fn subagents_claude_and_copilot() {
        let d = tmp();
        write(
            &d.path().join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews code\n---\nBody",
        );
        assert_eq!(detect(d.path()), Some(ConfigKind::Subagents));
        let c = tmp();
        write(
            &c.path().join("planner.agent.md"),
            "---\ndescription: Plans\n---\nBody",
        );
        assert_eq!(subagent_files(c.path()).len(), 1);
        // A plain README with frontmatter is not an agent.
        let r = tmp();
        write(
            &r.path().join("README.md"),
            "---\nname: x\ndescription: y\n---\n",
        );
        assert!(subagent_files(r.path()).is_empty());
    }
}
