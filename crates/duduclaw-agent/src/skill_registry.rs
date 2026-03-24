//! Skill registry — indexes real GitHub skill repos and caches locally.
//!
//! Searches GitHub for skill repositories (SKILL.md, claude-code skills, openclaw skills),
//! indexes them locally at `~/.duduclaw/skill_index.json`, and supports search.

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
        if name_lower.contains(term) { score += 10; }
        if desc_lower.contains(term) { score += 5; }
        for tag in &skill.tags {
            if tag.to_lowercase().contains(term) { score += 7; }
        }
    }
    score
}

// ── GitHub search queries ───────────────────────────────────

/// GitHub Search API queries to discover real skill repos.
const GITHUB_SEARCH_QUERIES: &[&str] = &[
    "claude-code skill SKILL.md",
    "openclaw skill",
    "claude agent skill",
    "MCP tool skill claude",
];

/// GitHub Search API endpoint.
const GITHUB_SEARCH_URL: &str = "https://api.github.com/search/repositories";

/// Maximum cache age (24 hours).
const CACHE_MAX_AGE_SECS: i64 = 86400;

// ── SkillRegistry ───────────────────────────────────────────

pub struct SkillRegistry {
    index_path: PathBuf,
    index: SkillIndex,
}

impl SkillRegistry {
    pub fn load(home_dir: &Path) -> Self {
        let index_path = home_dir.join("skill_index.json");
        let index = match std::fs::read_to_string(&index_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse skill index: {e}");
                SkillIndex::empty()
            }),
            Err(_) => SkillIndex::empty(),
        };

        info!(count = index.skills.len(), source = %index.source, "Skill registry loaded");
        Self { index_path, index }
    }

    pub fn needs_refresh(&self) -> bool {
        if self.index.skills.is_empty() {
            return true;
        }
        let updated = chrono::DateTime::parse_from_rfc3339(&self.index.updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now() - chrono::Duration::days(2));
        Utc::now().signed_duration_since(updated).num_seconds() > CACHE_MAX_AGE_SECS
    }

    /// Fetch skill index by searching GitHub for real skill repos,
    /// then cache the results locally.
    pub async fn refresh(&mut self) -> Result<usize, String> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("DuDuClaw-SkillRegistry/0.6")
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;

        let mut all_skills: Vec<SkillIndexEntry> = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        for query in GITHUB_SEARCH_QUERIES {
            match search_github_repos(&http, query).await {
                Ok(entries) => {
                    for entry in entries {
                        if seen_names.insert(entry.name.clone()) {
                            all_skills.push(entry);
                        }
                    }
                }
                Err(e) => {
                    warn!(query, "GitHub search failed: {e}");
                }
            }
        }

        if all_skills.is_empty() {
            if self.index.skills.is_empty() {
                warn!("GitHub search returned no results and no cache available");
                return Ok(0);
            }
            warn!("GitHub search returned no results, keeping stale cache ({} skills)", self.index.skills.len());
            return Ok(self.index.skills.len());
        }

        let count = all_skills.len();
        self.index = SkillIndex {
            updated_at: Utc::now().to_rfc3339(),
            source: "github-search".to_string(),
            skills: all_skills,
        };

        if let Err(e) = self.save() {
            warn!("Failed to cache skill index: {e}");
        }

        info!(count, "Skill index refreshed from GitHub");
        Ok(count)
    }

    pub fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.index)
            .map_err(|e| format!("Serialize: {e}"))?;
        std::fs::write(&self.index_path, json)
            .map_err(|e| format!("Write: {e}"))?;
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&SkillIndexEntry> {
        self.index.search(query, limit)
    }

    pub fn upsert(&mut self, entry: SkillIndexEntry) {
        if let Some(existing) = self.index.skills.iter_mut().find(|s| s.name == entry.name) {
            *existing = entry;
        } else {
            self.index.skills.push(entry);
        }
        self.index.updated_at = Utc::now().to_rfc3339();
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.index.skills.len();
        self.index.skills.retain(|s| s.name != name);
        self.index.updated_at = Utc::now().to_rfc3339();
        self.index.skills.len() < before
    }

    pub fn count(&self) -> usize { self.index.len() }

    pub fn list_by_tag(&self, tag: &str) -> Vec<&SkillIndexEntry> {
        let lower = tag.to_lowercase();
        self.index.skills.iter()
            .filter(|s| s.tags.iter().any(|t| t.to_lowercase() == lower))
            .collect()
    }

    pub fn index(&self) -> &SkillIndex { &self.index }
    pub fn source(&self) -> &str { &self.index.source }
}

// ── GitHub API integration ──────────────────────────────────

/// Search GitHub repos and convert to SkillIndexEntry.
async fn search_github_repos(
    http: &reqwest::Client,
    query: &str,
) -> Result<Vec<SkillIndexEntry>, String> {
    let resp = http
        .get(GITHUB_SEARCH_URL)
        .query(&[("q", query), ("per_page", "30"), ("sort", "stars"), ("order", "desc")])
        .send()
        .await
        .map_err(|e| format!("Request: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| format!("JSON: {e}"))?;
    let items = body["items"].as_array().cloned().unwrap_or_default();

    let skills: Vec<SkillIndexEntry> = items
        .iter()
        .filter_map(|repo| {
            let name = repo["name"].as_str()?;
            let desc = repo["description"].as_str().unwrap_or("");
            let owner = repo["owner"]["login"].as_str().unwrap_or("");
            let html_url = repo["html_url"].as_str().unwrap_or("");
            let language = repo["language"].as_str().unwrap_or("");
            let topics: Vec<String> = repo["topics"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            // Build tags from topics + language
            let mut tags = topics;
            if !language.is_empty() {
                tags.push(language.to_lowercase());
            }

            Some(SkillIndexEntry {
                name: name.to_string(),
                description: desc.to_string(),
                tags,
                author: owner.to_string(),
                url: html_url.to_string(),
                compatible: vec!["claude-code".to_string()],
            })
        })
        .collect();

    info!(query, count = skills.len(), "GitHub search completed");
    Ok(skills)
}
