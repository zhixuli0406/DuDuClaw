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

        let config: AgentConfig = toml::from_str(&toml_content).map_err(|e| {
            error!(path = %toml_path.display(), error = %e, "failed to parse agent.toml");
            DuDuClawError::TomlDeser(e)
        })?;

        let soul = Self::load_optional_md(&dir.join("SOUL.md")).await;
        let identity = Self::load_optional_md(&dir.join("IDENTITY.md")).await;
        let memory = Self::load_optional_md(&dir.join("MEMORY.md")).await;
        let skills = Self::load_skills(&dir.join("SKILLS")).await;

        Ok(LoadedAgent {
            config,
            soul,
            identity,
            memory,
            skills,
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

    /// Scan a SKILLS/ directory and load all `.md` files found there.
    async fn load_skills(skills_dir: &Path) -> Vec<SkillFile> {
        let mut skills = Vec::new();

        let mut entries = match fs::read_dir(skills_dir).await {
            Ok(e) => e,
            Err(_) => return skills, // SKILLS/ directory is optional
        };

        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "error reading skills directory entry");
                    continue;
                }
            };

            let path = entry.path();

            // Only load .md files
            let is_md = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("md"))
                .unwrap_or(false);

            if !is_md {
                continue;
            }

            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            match fs::read_to_string(&path).await {
                Ok(content) => {
                    skills.push(SkillFile { name, content });
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to read skill file");
                }
            }
        }

        skills
    }
}
