//! Skill compression — three-layer progressive loading.
//!
//! Layer 0: skill name tag (~5 tokens)
//! Layer 1: 1-2 line summary (~30 tokens)
//! Layer 2: full content (~200+ tokens)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A skill compressed into three layers for progressive injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedSkill {
    pub name: String,
    /// Layer 0: just the name (~5 tokens).
    pub tag: String,
    /// Layer 1: short summary (~30 tokens).
    pub summary: String,
    /// Layer 2: full markdown content.
    pub full_content: String,
    /// Estimated tokens for each layer.
    pub tokens_layer0: u32,
    pub tokens_layer1: u32,
    pub tokens_layer2: u32,
}

impl CompressedSkill {
    /// Compress a skill file into three layers.
    pub fn compress(name: &str, content: &str, description: Option<&str>) -> Self {
        let tag = name.to_string();

        let summary = if let Some(desc) = description {
            if desc.len() > 5 { desc.to_string() } else { first_lines(content, 2) }
        } else {
            first_lines(content, 2)
        };

        let tl0 = estimate_tokens_simple(&tag);
        let tl1 = estimate_tokens_simple(&summary);
        let tl2 = estimate_tokens_simple(content);
        Self {
            name: name.to_string(),
            tag,
            summary,
            full_content: content.to_string(),
            tokens_layer0: tl0,
            tokens_layer1: tl1,
            tokens_layer2: tl2,
        }
    }
}

/// Extract first N non-empty lines from content.
fn first_lines(content: &str, n: usize) -> String {
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("---") && !l.starts_with('#'))
        .take(n)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Simple token estimation (CJK-aware).
fn estimate_tokens_simple(text: &str) -> u32 {
    let mut cjk = 0u32;
    let mut other = 0u32;
    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3000..=0x9FFF).contains(&cp) || (0xF900..=0xFAFF).contains(&cp) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    let cjk_tokens = (cjk as f32 / 1.5).ceil() as u32;
    let other_tokens = (other as f32 / 4.0).ceil() as u32;
    cjk_tokens + other_tokens + 1
}

/// Cache of compressed skills, refreshed when SKILLS/ directory changes.
pub struct CompressedSkillCache {
    skills: HashMap<String, CompressedSkill>,
}

impl CompressedSkillCache {
    pub fn new() -> Self {
        Self { skills: HashMap::new() }
    }

    /// Refresh cache from loaded agent skills.
    pub fn refresh(&mut self, agent_skills: &[(String, String, Option<String>)]) {
        self.skills.clear();
        for (name, content, desc) in agent_skills {
            let compressed = CompressedSkill::compress(name, content, desc.as_deref());
            self.skills.insert(name.clone(), compressed);
        }
    }

    /// Get all compressed skills.
    pub fn all(&self) -> Vec<&CompressedSkill> {
        self.skills.values().collect()
    }

    /// Get a specific skill.
    pub fn get(&self, name: &str) -> Option<&CompressedSkill> {
        self.skills.get(name)
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}
