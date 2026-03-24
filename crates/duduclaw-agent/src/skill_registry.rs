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
    /// Load the registry from disk (or create an empty one).
    pub fn load(home_dir: &Path) -> Self {
        let index_path = home_dir.join("skill_index.json");
        let index = match std::fs::read_to_string(&index_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse skill index: {e}");
                SkillIndex::empty()
            }),
            Err(_) => SkillIndex::empty(),
        };

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
