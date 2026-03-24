//! Skill registry — fetches from remote skill markets and caches locally.
//!
//! [B-2a] Fetches skill index from GitHub (awesome-openclaw-skills or similar),
//! caches to `~/.duduclaw/skill_index.json`, and supports search.
//! Falls back to built-in seeds only when offline and no cache exists.

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
    #[serde(default)]
    pub source: String,
    pub skills: Vec<SkillIndexEntry>,
}

impl SkillIndex {
    pub fn empty() -> Self {
        Self {
            updated_at: Utc::now().to_rfc3339(),
            source: String::new(),
            skills: Vec::new(),
        }
    }

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

        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.into_iter().take(limit).map(|(s, _)| s).collect()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

fn score_match(skill: &SkillIndexEntry, terms: &[&str]) -> usize {
    let mut score = 0;
    let name_lower = skill.name.to_lowercase();
    let desc_lower = skill.description.to_lowercase();

    for term in terms {
        if name_lower.contains(term) {
            score += 10;
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

// ── Remote sources ──────────────────────────────────────────

/// Known remote skill index sources.
const REMOTE_SOURCES: &[(&str, &str)] = &[
    (
        "awesome-openclaw-skills",
        "https://raw.githubusercontent.com/VoltAgent/awesome-openclaw-skills/main/index.json",
    ),
    (
        "duduclaw-skills",
        "https://raw.githubusercontent.com/zhixuli0406/duduclaw-skills/main/index.json",
    ),
];

/// Maximum age of cached index before re-fetching (24 hours).
const CACHE_MAX_AGE_SECS: i64 = 86400;

// ── SkillRegistry ───────────────────────────────────────────

/// Local skill registry manager with remote fetching.
pub struct SkillRegistry {
    index_path: PathBuf,
    index: SkillIndex,
}

impl SkillRegistry {
    /// Load from disk cache. Does NOT fetch remotely (use `refresh()` for that).
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
            source = %index.source,
            "Skill registry loaded from cache"
        );

        Self { index_path, index }
    }

    /// Check if the cache is stale (older than 24 hours) or empty.
    pub fn needs_refresh(&self) -> bool {
        if self.index.skills.is_empty() {
            return true;
        }
        let updated = chrono::DateTime::parse_from_rfc3339(&self.index.updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now() - chrono::Duration::days(2));
        let age = Utc::now().signed_duration_since(updated);
        age.num_seconds() > CACHE_MAX_AGE_SECS
    }

    /// Fetch skill index from remote sources and update local cache.
    /// Falls back to built-in seeds if all remotes fail.
    pub async fn refresh(&mut self) -> Result<usize, String> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;

        for (source_name, url) in REMOTE_SOURCES {
            info!(source = source_name, url, "Fetching skill index");

            match http.get(*url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<RemoteIndex>().await {
                        Ok(remote) => {
                            let skills: Vec<SkillIndexEntry> = remote.skills
                                .into_iter()
                                .map(|s| SkillIndexEntry {
                                    name: s.name,
                                    description: s.description.unwrap_or_default(),
                                    tags: s.tags.unwrap_or_default(),
                                    author: s.author.unwrap_or_default(),
                                    url: s.url.unwrap_or_default(),
                                    compatible: s.compatible.unwrap_or_default(),
                                })
                                .collect();

                            let count = skills.len();
                            self.index = SkillIndex {
                                updated_at: Utc::now().to_rfc3339(),
                                source: source_name.to_string(),
                                skills,
                            };

                            if let Err(e) = self.save() {
                                warn!("Failed to cache skill index: {e}");
                            }

                            info!(source = source_name, count, "Skill index fetched from remote");
                            return Ok(count);
                        }
                        Err(e) => {
                            warn!(source = source_name, "Failed to parse remote index: {e}");
                        }
                    }
                }
                Ok(resp) => {
                    warn!(source = source_name, status = %resp.status(), "Remote index returned error");
                }
                Err(e) => {
                    warn!(source = source_name, "Failed to fetch remote index: {e}");
                }
            }
        }

        // All remotes failed — keep stale cache if available
        if self.index.skills.is_empty() {
            warn!("All remote sources failed and no cached skills available");
            Ok(0)
        } else {
            warn!("All remote sources failed, using stale cache ({} skills)", self.index.skills.len());
            Ok(self.index.skills.len())
        }
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

    /// Get the source name of the current index.
    pub fn source(&self) -> &str {
        &self.index.source
    }
}

// ── Remote index format (flexible parsing) ──────────────────

#[derive(Deserialize)]
struct RemoteIndex {
    #[serde(default)]
    skills: Vec<RemoteSkill>,
}

#[derive(Deserialize)]
struct RemoteSkill {
    name: String,
    description: Option<String>,
    tags: Option<Vec<String>>,
    author: Option<String>,
    url: Option<String>,
    compatible: Option<Vec<String>>,
}

