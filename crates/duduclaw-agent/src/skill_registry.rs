//! Local skill registry — cached index for searching and installing skills.
//!
//! [B-2a] Maintains a local JSON index at `~/.duduclaw/skill_index.json`
//! supporting search by name, tag, and description.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// A single skill entry in the registry index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub compatible: Vec<String>,
}

/// The full skill index with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndex {
    pub updated_at: String,
    pub skills: Vec<SkillIndexEntry>,
}

impl SkillIndex {
    /// Create an empty index.
    pub fn empty() -> Self {
        Self {
            updated_at: Utc::now().to_rfc3339(),
            skills: Vec::new(),
        }
    }

    /// Search skills by query (matches name, description, and tags).
    pub fn search(&self, query: &str, limit: usize) -> Vec<&SkillIndexEntry> {
        let lower = query.to_lowercase();
        let terms: Vec<&str> = lower.split_whitespace().collect();

        let mut results: Vec<(&SkillIndexEntry, usize)> = self
            .skills
            .iter()
            .filter_map(|skill| {
                let score = score_match(skill, &terms);
                if score > 0 { Some((skill, score)) } else { None }
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.into_iter().take(limit).map(|(s, _)| s).collect()
    }

    /// Get total skill count.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Check if index is empty.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Score how well a skill matches the search terms.
fn score_match(skill: &SkillIndexEntry, terms: &[&str]) -> usize {
    let mut score = 0;
    let name_lower = skill.name.to_lowercase();
    let desc_lower = skill.description.to_lowercase();

    for term in terms {
        if name_lower.contains(term) {
            score += 10; // Name match is highest weight
        }
        if desc_lower.contains(term) {
            score += 5;
        }
        for tag in &skill.tags {
            if tag.to_lowercase().contains(term) {
                score += 7;
            }
        }
    }
    score
}

/// Local skill registry manager.
pub struct SkillRegistry {
    index_path: PathBuf,
    index: SkillIndex,
}

impl SkillRegistry {
    /// Load the registry from disk. Seeds built-in skills if empty.
    pub fn load(home_dir: &Path) -> Self {
        let index_path = home_dir.join("skill_index.json");
        let mut index = match std::fs::read_to_string(&index_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse skill index: {e}");
                SkillIndex::empty()
            }),
            Err(_) => SkillIndex::empty(),
        };

        // Seed built-in skills if the index is empty
        if index.skills.is_empty() {
            index.skills = builtin_skills();
            index.updated_at = Utc::now().to_rfc3339();
        }

        info!(
            count = index.skills.len(),
            updated = %index.updated_at,
            "Skill registry loaded"
        );

        Self { index_path, index }
    }

    /// Save the current index to disk.
    pub fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.index)
            .map_err(|e| format!("Serialize: {e}"))?;
        std::fs::write(&self.index_path, json)
            .map_err(|e| format!("Write: {e}"))?;
        Ok(())
    }

    /// Search skills in the local index.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&SkillIndexEntry> {
        self.index.search(query, limit)
    }

    /// Add or update a skill entry in the index.
    pub fn upsert(&mut self, entry: SkillIndexEntry) {
        if let Some(existing) = self.index.skills.iter_mut().find(|s| s.name == entry.name) {
            *existing = entry;
        } else {
            self.index.skills.push(entry);
        }
        self.index.updated_at = Utc::now().to_rfc3339();
    }

    /// Remove a skill from the index.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.index.skills.len();
        self.index.skills.retain(|s| s.name != name);
        self.index.updated_at = Utc::now().to_rfc3339();
        self.index.skills.len() < before
    }

    /// Get the total number of indexed skills.
    pub fn count(&self) -> usize {
        self.index.len()
    }

    /// List all skills (optionally filtered by tag).
    pub fn list_by_tag(&self, tag: &str) -> Vec<&SkillIndexEntry> {
        let lower = tag.to_lowercase();
        self.index
            .skills
            .iter()
            .filter(|s| s.tags.iter().any(|t| t.to_lowercase() == lower))
            .collect()
    }

    /// Get the raw index for serialization.
    pub fn index(&self) -> &SkillIndex {
        &self.index
    }
}

/// Built-in skill catalog — seeded when the index is empty.
fn builtin_skills() -> Vec<SkillIndexEntry> {
    vec![
        SkillIndexEntry { name: "code-review".into(), description: "Automated code review with severity ratings, security checks, and actionable suggestions".into(), tags: vec!["code".into(), "review".into(), "quality".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "git-commit".into(), description: "Generate conventional commit messages from staged changes".into(), tags: vec!["git".into(), "automation".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "test-generator".into(), description: "Generate unit tests for functions and modules with TDD methodology".into(), tags: vec!["testing".into(), "tdd".into(), "code".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "api-designer".into(), description: "Design REST API endpoints with OpenAPI spec generation".into(), tags: vec!["api".into(), "design".into(), "openapi".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "sql-optimizer".into(), description: "Analyze and optimize SQL queries for PostgreSQL and MySQL".into(), tags: vec!["database".into(), "sql".into(), "performance".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "security-scanner".into(), description: "Scan code for OWASP Top 10 vulnerabilities, secrets, and injection risks".into(), tags: vec!["security".into(), "owasp".into(), "scanning".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "dockerfile-builder".into(), description: "Generate optimized Dockerfiles with multi-stage builds and security best practices".into(), tags: vec!["docker".into(), "devops".into(), "container".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "i18n-translator".into(), description: "Extract and translate UI strings for internationalization (zh-TW, en, ja)".into(), tags: vec!["i18n".into(), "translation".into(), "ui".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "changelog-writer".into(), description: "Generate changelogs from git history with semantic grouping".into(), tags: vec!["git".into(), "documentation".into(), "release".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "data-visualizer".into(), description: "Create charts and dashboards from CSV/JSON data using D3.js or Recharts".into(), tags: vec!["data".into(), "visualization".into(), "charts".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "email-composer".into(), description: "Draft professional emails with tone adjustment and multilingual support".into(), tags: vec!["communication".into(), "email".into(), "writing".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "meeting-summarizer".into(), description: "Summarize meeting notes into action items, decisions, and follow-ups".into(), tags: vec!["communication".into(), "meetings".into(), "productivity".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "odoo-assistant".into(), description: "Query and manage Odoo ERP data — CRM leads, sales orders, inventory, invoices".into(), tags: vec!["erp".into(), "odoo".into(), "business".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "prompt-engineer".into(), description: "Optimize and refine AI prompts for better outputs with evaluation metrics".into(), tags: vec!["ai".into(), "prompts".into(), "optimization".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "regex-builder".into(), description: "Build and test regular expressions with explanations and test cases".into(), tags: vec!["utility".into(), "regex".into(), "text".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "dependency-auditor".into(), description: "Audit npm/cargo/pip dependencies for vulnerabilities and license issues".into(), tags: vec!["security".into(), "dependencies".into(), "audit".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "performance-profiler".into(), description: "Profile application performance and suggest optimizations for web and API".into(), tags: vec!["performance".into(), "optimization".into(), "profiling".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "markdown-formatter".into(), description: "Format and beautify Markdown documents with table alignment and TOC generation".into(), tags: vec!["documentation".into(), "markdown".into(), "formatting".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
        SkillIndexEntry { name: "crm-pipeline".into(), description: "Manage CRM pipeline — create leads, update stages, send follow-up notifications".into(), tags: vec!["crm".into(), "sales".into(), "business".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into()] },
        SkillIndexEntry { name: "image-describer".into(), description: "Describe images for accessibility, generate alt text, and extract visual content".into(), tags: vec!["media".into(), "accessibility".into(), "ai".into()], author: "duduclaw".into(), url: String::new(), compatible: vec!["duduclaw".into(), "openclaw".into()] },
    ]
}
