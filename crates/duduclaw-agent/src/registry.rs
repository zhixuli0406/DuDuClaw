use std::collections::HashMap;
use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::types::{AgentConfig, AgentRole};
use tokio::fs;
use tracing::{error, info, warn};

/// A single skill file loaded from the SKILLS/ directory.
#[derive(Debug, Clone)]
pub struct SkillFile {
    pub name: String,
    pub content: String,
}

/// A fully loaded agent with its configuration and associated markdown files.
#[derive(Debug, Clone)]
pub struct LoadedAgent {
    pub config: AgentConfig,
    /// Content of SOUL.md (optional).
    pub soul: Option<String>,
    /// Content of IDENTITY.md (optional).
    pub identity: Option<String>,
    /// Content of MEMORY.md (optional).
    pub memory: Option<String>,
    /// Skill files loaded from SKILLS/*.md.
    pub skills: Vec<SkillFile>,
    /// Behavioral contract loaded from CONTRACT.toml.
    pub contract: crate::contract::Contract,
    /// Directory this agent was loaded from.
    pub dir: PathBuf,
}

/// Registry that scans and holds all agents from the agents directory.
pub struct AgentRegistry {
    agents_dir: PathBuf,
    agents: HashMap<String, LoadedAgent>,
    /// Global skills loaded from `~/.duduclaw/skills/` — shared by all agents.
    global_skills: Vec<SkillFile>,
}

impl AgentRegistry {
    /// Create a new registry targeting the given agents directory.
    pub fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            agents: HashMap::new(),
            global_skills: Vec::new(),
        }
    }

    /// Return the agents directory path.
    pub fn agents_dir(&self) -> &Path {
        &self.agents_dir
    }

    /// Scan the agents directory and load all valid agent configurations.
    ///
    /// Also loads global skills from `~/.duduclaw/skills/` and merges them
    /// into each agent (global skills appear before agent-local skills).
    ///
    /// Directories whose name starts with `_` (e.g. `_defaults`) are skipped.
    pub async fn scan(&mut self) -> Result<()> {
        // Load global skills from sibling `skills/` directory
        let global_skills_dir = self.agents_dir.parent()
            .map(|home| home.join("skills"))
            .unwrap_or_else(|| self.agents_dir.join("../skills"));
        self.global_skills = Self::load_skills(&global_skills_dir).await;
        if !self.global_skills.is_empty() {
            info!(count = self.global_skills.len(), dir = %global_skills_dir.display(), "loaded global skills");
        }

        let mut entries = fs::read_dir(&self.agents_dir).await.map_err(|e| {
            DuDuClawError::Agent(format!(
                "failed to read agents directory {}: {e}",
                self.agents_dir.display()
            ))
        })?;

        let mut loaded: HashMap<String, LoadedAgent> = HashMap::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            // Only process directories
            if !path.is_dir() {
                continue;
            }

            // Skip directories starting with _
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };
            if dir_name.starts_with('_') {
                info!(dir = %dir_name, "skipping underscore-prefixed directory");
                continue;
            }

            // Skip directories without agent.toml — these are not agent dirs
            // (e.g. legacy wiki-only directories). Without this guard the load
            // attempt below would log a WARN on every scan tick.
            if !fs::try_exists(path.join("agent.toml")).await.unwrap_or(false) {
                info!(dir = %dir_name, "skipping non-agent directory (no agent.toml)");
                continue;
            }

            // Attempt to load the agent
            match Self::load_agent(&path).await {
                Ok(mut agent) => {
                    // Prepend global skills (agent-local skills override globals with same name)
                    let local_names: std::collections::HashSet<&str> =
                        agent.skills.iter().map(|s| s.name.as_str()).collect();
                    let mut merged: Vec<SkillFile> = self.global_skills.iter()
                        .filter(|gs| !local_names.contains(gs.name.as_str()))
                        .cloned()
                        .collect();
                    merged.append(&mut agent.skills);
                    agent.skills = merged;

                    let name = agent.config.agent.name.clone();
                    info!(agent = %name, dir = %dir_name, "loaded agent");
                    loaded.insert(name, agent);
                }
                Err(e) => {
                    warn!(dir = %dir_name, error = %e, "failed to load agent, skipping");
                }
            }
        }

        self.agents = loaded;
        let names: Vec<&str> = self.agents.keys().map(|s| s.as_str()).collect();
        info!(count = self.agents.len(), agents = ?names, "agent registry scan complete");
        Ok(())
    }

    /// Load a single agent from the given directory.
    ///
    /// Expects an `agent.toml` file at the root of `dir`.
    pub async fn load_agent(dir: &Path) -> Result<LoadedAgent> {
        let toml_path = dir.join("agent.toml");
        let toml_content = fs::read_to_string(&toml_path).await.map_err(|e| {
            DuDuClawError::Agent(format!(
                "failed to read {}: {e}",
                toml_path.display()
            ))
        })?;

        let mut config: AgentConfig = toml::from_str(&toml_content).map_err(|e| {
            error!(path = %toml_path.display(), error = %e, "failed to parse agent.toml");
            DuDuClawError::TomlDeser(e)
        })?;
        config.proactive.sanitize();
        config.sticker.sanitize();

        let soul = Self::load_optional_md(&dir.join("SOUL.md")).await;
        let identity = Self::load_optional_md(&dir.join("IDENTITY.md")).await;
        let memory = Self::load_optional_md(&dir.join("MEMORY.md")).await;
        let skills = Self::load_skills(&dir.join("SKILLS")).await;
        let contract = crate::contract::load_contract(dir);

        Ok(LoadedAgent {
            config,
            soul,
            identity,
            memory,
            skills,
            contract,
            dir: dir.to_path_buf(),
        })
    }

    /// Look up an agent by name.
    pub fn get(&self, name: &str) -> Option<&LoadedAgent> {
        self.agents.get(name)
    }

    /// Return all loaded agents as a list.
    pub fn list(&self) -> Vec<&LoadedAgent> {
        self.agents.values().collect()
    }

    /// Return the global skills (loaded from `~/.duduclaw/skills/`).
    pub fn global_skills(&self) -> &[SkillFile] {
        &self.global_skills
    }

    /// Find the agent whose role is `Main`, if any.
    pub fn main_agent(&self) -> Option<&LoadedAgent> {
        self.agents
            .values()
            .find(|a| a.config.agent.role == AgentRole::Main)
    }

    /// Read an optional markdown file; returns `None` if the file does not exist
    /// or cannot be read.
    async fn load_optional_md(path: &Path) -> Option<String> {
        match fs::read_to_string(path).await {
            Ok(content) => {
                if content.is_empty() {
                    None
                } else {
                    Some(content)
                }
            }
            Err(_) => None,
        }
    }

    /// Scan a skills directory and load all skills found there, recursively.
    ///
    /// Supports two co-existing layouts:
    ///
    /// 1. **Anthropic Skills spec** (canonical, see <https://code.claude.com/docs/en/skills>):
    ///    Each skill lives in its own directory containing `SKILL.md` plus
    ///    optional `scripts/`, `references/`, `assets/` sub-trees. The skill
    ///    name comes from the *parent directory name*, and only `SKILL.md`
    ///    is treated as the skill body — sibling `.md` files in
    ///    `references/` etc. are reference material and **not** loaded as
    ///    separate skills. Example:
    ///
    ///    ```text
    ///    skills/
    ///    └── pdf-extractor/
    ///        ├── SKILL.md            ← loaded, skill name = "pdf-extractor"
    ///        ├── scripts/run.py      ← ignored (not .md)
    ///        └── references/api.md   ← ignored (not a SKILL.md)
    ///    ```
    ///
    /// 2. **Legacy DuDuClaw flat layout** (back-compat):
    ///    A loose `<name>.md` file directly under the scanned root. Skill
    ///    name comes from the file stem. The flat form is only honoured
    ///    at the top level — we do not promote arbitrary nested `.md`
    ///    files to skills, otherwise `references/api.md` etc. would
    ///    pollute the skill list.
    ///
    /// Implementation notes:
    /// - **Recursion depth capped at `MAX_DEPTH`** (currently 8) so a
    ///   misplaced symlink loop can't hang startup.
    /// - **Hidden entries skipped** (`.` prefix) — `.git`, `.DS_Store`, etc.
    /// - **Symlink-safe**: we resolve via `tokio::fs::metadata` (follows
    ///   symlinks once) but don't recurse into a symlinked directory whose
    ///   target is outside `root`.
    /// - **Errors are warned but never fatal** — a single broken file must
    ///   not stop the agent from starting.
    pub async fn load_skills(skills_dir: &Path) -> Vec<SkillFile> {
        // Bounded BFS so we don't risk stack overflow on adversarial trees.
        // Tuple is `(path, depth, is_top_level)`.
        const MAX_DEPTH: usize = 8;
        let mut skills: Vec<SkillFile> = Vec::new();
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Resolve once so symlinked-root cases are stable. If canonicalize
        // fails (root doesn't exist), bail with empty Vec — caller treats
        // missing skills/ as optional.
        let root_canonical = match tokio::fs::canonicalize(skills_dir).await {
            Ok(p) => p,
            Err(_) => return skills,
        };

        let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
            std::collections::VecDeque::new();
        queue.push_back((root_canonical.clone(), 0));

        while let Some((dir, depth)) = queue.pop_front() {
            if depth > MAX_DEPTH {
                warn!(
                    dir = %dir.display(),
                    depth,
                    "skill scan: max depth reached, pruning subtree"
                );
                continue;
            }

            let mut entries = match fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "skill scan: read_dir failed");
                    continue;
                }
            };

            loop {
                let entry = match entries.next_entry().await {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(e) => {
                        warn!(error = %e, "skill scan: next_entry failed, skipping");
                        continue;
                    }
                };

                let path = entry.path();
                let file_name_os = entry.file_name();
                let file_name = match file_name_os.to_str() {
                    Some(s) => s,
                    None => {
                        // Non-UTF8 filename — skip rather than panic.
                        warn!(path = %path.display(), "skill scan: non-UTF8 file name, skipping");
                        continue;
                    }
                };

                // Skip hidden entries (`.git`, `.DS_Store`, dotfiles).
                if file_name.starts_with('.') {
                    continue;
                }

                let metadata = match tokio::fs::metadata(&path).await {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "skill scan: metadata failed");
                        continue;
                    }
                };

                if metadata.is_dir() {
                    // Symlink containment: only recurse if the resolved
                    // path is still under the original root, so a
                    // `skills/external -> /etc` doesn't expose unrelated
                    // dirs. If canonicalize fails, log and skip.
                    let resolved = match tokio::fs::canonicalize(&path).await {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "skill scan: canonicalize subdir failed");
                            continue;
                        }
                    };
                    if !resolved.starts_with(&root_canonical) {
                        warn!(
                            path = %path.display(),
                            resolved = %resolved.display(),
                            "skill scan: refusing to follow symlink outside skill root"
                        );
                        continue;
                    }
                    queue.push_back((path, depth + 1));
                    continue;
                }

                if !metadata.is_file() {
                    continue;
                }

                let is_md = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("md"))
                    .unwrap_or(false);
                if !is_md {
                    continue;
                }

                // Decide whether this `.md` file is a skill body:
                //   - `SKILL.md` (case-insensitive) anywhere → Anthropic
                //     spec, skill name = parent directory name.
                //   - Top-level `*.md` (depth == 0) → legacy flat skill,
                //     skill name = file stem.
                //   - Otherwise (e.g. `references/api.md`) → reference
                //     material, NOT a separately loadable skill.
                let is_skill_md = file_name.eq_ignore_ascii_case("SKILL.md");
                let is_top_level_flat = depth == 0 && !is_skill_md;

                let skill_name = if is_skill_md {
                    // Use the parent directory name. Falls back to
                    // file_stem() if the parent is somehow unreadable
                    // (root-mounted SKILL.md — unusual but valid).
                    path.parent()
                        .and_then(|p| p.file_name())
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "SKILL".to_string())
                } else if is_top_level_flat {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string()
                } else {
                    // Nested non-SKILL.md file — reference material.
                    continue;
                };

                // De-duplicate when the same skill name shows up via
                // both layouts (e.g. legacy `foo.md` AND `foo/SKILL.md`).
                // Anthropic spec wins because it carries metadata.
                let dedup_key = skill_name.to_ascii_lowercase();
                if !is_skill_md && seen.contains(&dedup_key) {
                    // SKILL.md form already won; skip the legacy file.
                    continue;
                }

                match fs::read_to_string(&path).await {
                    Ok(content) => {
                        // If we previously inserted a flat-form skill
                        // with the same name and now found the SKILL.md
                        // form, replace it.
                        if is_skill_md
                            && let Some(existing) = skills
                                .iter_mut()
                                .find(|s| s.name.eq_ignore_ascii_case(&skill_name))
                        {
                            existing.content = content;
                            seen.insert(dedup_key);
                            continue;
                        }
                        seen.insert(dedup_key);
                        skills.push(SkillFile {
                            name: skill_name,
                            content,
                        });
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "failed to read skill file");
                    }
                }
            }
        }

        // Stable order so prompt-cache hits stay consistent across reboots.
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }
}

#[cfg(test)]
mod load_skills_tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper to create a single skill file under `root` at the given
    /// relative path, creating parent directories as needed.
    fn write_skill(root: &Path, rel: &str, body: &str) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }

    /// Returns the loaded skills sorted by name for stable assertions.
    async fn load(root: &Path) -> Vec<SkillFile> {
        AgentRegistry::load_skills(root).await
    }

    /// Names-only convenience for assertions.
    fn names(skills: &[SkillFile]) -> Vec<String> {
        skills.iter().map(|s| s.name.clone()).collect()
    }

    // ── Layout 1: legacy flat ────────────────────────────────────────────

    #[tokio::test]
    async fn flat_layout_loads_top_level_md_files() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "alpha.md", "alpha body");
        write_skill(tmp.path(), "beta.md", "beta body");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["alpha", "beta"]);
        assert_eq!(skills[0].content, "alpha body");
    }

    #[tokio::test]
    async fn missing_directory_returns_empty_not_panic() {
        let tmp = TempDir::new().unwrap();
        // Don't create the dir — load_skills must tolerate ENOENT.
        let skills = load(&tmp.path().join("nonexistent")).await;
        assert!(skills.is_empty());
    }

    #[tokio::test]
    async fn empty_directory_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let skills = load(tmp.path()).await;
        assert!(skills.is_empty());
    }

    // ── Layout 2: Anthropic SKILL.md spec ────────────────────────────────

    #[tokio::test]
    async fn skill_md_in_subdirectory_uses_parent_dir_as_name() {
        // Per Anthropic spec: <skill-name>/SKILL.md.
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "pdf-extractor/SKILL.md",
            "---\nname: pdf-extractor\ndescription: extracts text\n---\n\n# Body",
        );
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["pdf-extractor"]);
        assert!(skills[0].content.contains("# Body"));
    }

    #[tokio::test]
    async fn skill_md_case_insensitive() {
        // Some tooling produces `Skill.md` / `skill.md` — accept both.
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "case-a/skill.md", "lower body");
        write_skill(tmp.path(), "case-b/Skill.md", "title body");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["case-a", "case-b"]);
    }

    #[tokio::test]
    async fn nested_non_skill_md_files_are_ignored_as_references() {
        // `references/api.md` is reference material per spec, NOT a skill.
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "tool/SKILL.md", "skill body");
        write_skill(tmp.path(), "tool/references/api.md", "reference doc");
        write_skill(tmp.path(), "tool/scripts/notes.md", "script notes");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["tool"], "only SKILL.md should load");
    }

    #[tokio::test]
    async fn deep_nested_skill_md_still_uses_immediate_parent_name() {
        // Even at depth 3, the parent dir name wins over the path.
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "category/subcategory/my-skill/SKILL.md",
            "deep body",
        );
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["my-skill"]);
    }

    // ── Layout co-existence ──────────────────────────────────────────────

    #[tokio::test]
    async fn flat_and_skill_md_coexist() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "legacy.md", "legacy body");
        write_skill(tmp.path(), "modern/SKILL.md", "modern body");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["legacy", "modern"]);
    }

    #[tokio::test]
    async fn skill_md_form_wins_when_same_name_present_in_both_layouts() {
        // If both `foo.md` and `foo/SKILL.md` exist, Anthropic spec wins
        // (it carries frontmatter metadata).
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "shared.md", "FLAT VERSION");
        write_skill(tmp.path(), "shared/SKILL.md", "SKILL.md VERSION");
        let skills = load(tmp.path()).await;
        assert_eq!(skills.len(), 1, "duplicate skill names should de-dupe");
        assert_eq!(skills[0].name, "shared");
        assert_eq!(skills[0].content, "SKILL.md VERSION");
    }

    // ── Hidden / special ────────────────────────────────────────────────

    #[tokio::test]
    async fn hidden_dirs_and_files_are_skipped() {
        let tmp = TempDir::new().unwrap();
        // Hidden dir (e.g. `.git`) — entire subtree skipped.
        write_skill(tmp.path(), ".git/SKILL.md", "should not load");
        // Hidden file at root.
        write_skill(tmp.path(), ".hidden.md", "should not load");
        // Normal skill so the test isn't trivially passing on emptiness.
        write_skill(tmp.path(), "real.md", "loaded");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["real"]);
    }

    #[tokio::test]
    async fn non_md_files_are_ignored() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "foo/SKILL.md", "skill");
        write_skill(tmp.path(), "foo/scripts/run.py", "print('x')");
        write_skill(tmp.path(), "foo/scripts/run.js", "console.log('x')");
        write_skill(tmp.path(), "stray.txt", "noise");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["foo"]);
    }

    #[tokio::test]
    async fn results_are_sorted_for_stable_cache() {
        // Prompt cache hits depend on stable section ordering. The
        // returned Vec is sorted by name regardless of filesystem order.
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "zebra.md", "z");
        write_skill(tmp.path(), "alpha.md", "a");
        write_skill(tmp.path(), "mike.md", "m");
        let skills = load(tmp.path()).await;
        assert_eq!(names(&skills), vec!["alpha", "mike", "zebra"]);
    }

    // ── Safety: max-depth + symlink containment ─────────────────────────

    #[tokio::test]
    async fn deeper_than_max_depth_subtree_is_pruned_not_panicked() {
        // Build a chain depth 12 so max=8 trims it. We only need to
        // prove the loader doesn't hang or panic — exact contents
        // beyond MAX_DEPTH are an explicit non-feature.
        let tmp = TempDir::new().unwrap();
        let mut path = String::new();
        for i in 0..12 {
            path.push_str(&format!("d{i}/"));
        }
        path.push_str("SKILL.md");
        write_skill(tmp.path(), &path, "deep body");
        // Loader should complete (within reasonable time) and not panic.
        let _skills = load(tmp.path()).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn external_symlink_is_not_followed() {
        // Create a symlink inside skill dir pointing OUTSIDE the root.
        // The target dir contains a SKILL.md that we must NOT load.
        let outside = TempDir::new().unwrap();
        write_skill(outside.path(), "evil/SKILL.md", "should not load");

        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "honest.md", "honest body");
        std::os::unix::fs::symlink(
            outside.path().join("evil"),
            tmp.path().join("trojan"),
        )
        .unwrap();

        let skills = load(tmp.path()).await;
        assert_eq!(
            names(&skills),
            vec!["honest"],
            "symlink to outside the skill root must not be followed"
        );
    }
}
