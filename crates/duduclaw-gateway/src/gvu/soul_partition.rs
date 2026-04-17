//! SOUL.md partitioning — immutable identity + evolvable behaviors + TTL observations.
//!
//! Partitions SOUL.md into sections with different mutability guarantees:
//! - **[identity]**: Human-authored, GVU cannot modify. SHA-256 protected.
//! - **[behaviors]**: GVU can modify within drift budget constraints.
//! - **[observations]**: GVU freely modifiable, auto-decays after TTL.
//!
//! Based on Alemohammad et al. (ICLR 2024) "Self-Consuming Models Go MAD":
//! retaining original data is the only proven anti-collapse mechanism.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A parsed section of SOUL.md with mutability metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoulSection {
    /// Section name (e.g., "identity", "behaviors", "observations").
    pub name: String,
    /// Section content (excluding the header line).
    pub content: String,
    /// Mutability level.
    pub mutability: SectionMutability,
    /// SHA-256 hash for integrity verification (identity sections only).
    pub integrity_hash: Option<String>,
}

/// Mutability level for SOUL.md sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionMutability {
    /// Cannot be modified by GVU under any circumstances.
    Immutable,
    /// Can be modified within drift budget constraints.
    Evolvable,
    /// Freely modifiable, entries auto-decay after TTL cycles.
    Observable,
}

/// Result of parsing a SOUL.md into sections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionedSoul {
    /// Parsed sections in order.
    pub sections: Vec<SoulSection>,
    /// Content that appears before any section header.
    pub preamble: String,
}

/// Section header patterns recognized by the parser.
///
/// Supports both DuDuClaw native format and SoulSpec v0.5 format:
/// - DuDuClaw: `## [identity]`, `## Identity`, `## 身份`
/// - SoulSpec v0.5: `## Core Identity`, `## Personality`, `## Purpose`
const IDENTITY_HEADERS: &[&str] = &[
    "## [identity]",
    "## Identity",
    "## 身份",
    // SoulSpec v0.5 compatible
    "## Core Identity",
    "## Purpose",
    "## Role",
    "## 核心身份",
];
const BEHAVIOR_HEADERS: &[&str] = &[
    "## [behaviors]",
    "## Behaviors",
    "## 行為",
    // SoulSpec v0.5 compatible
    "## Personality",
    "## Response Style",
    "## Communication Style",
    "## Core Responsibilities",
    "## Escalation Rules",
    "## 個性",
    "## 回應風格",
];
const OBSERVATION_HEADERS: &[&str] = &[
    "## [observations]",
    "## Observations",
    "## 觀察",
    // SoulSpec v0.5 compatible
    "## Learned Patterns",
    "## Adaptations",
    "## 學習模式",
];

impl PartitionedSoul {
    /// Parse a SOUL.md string into partitioned sections.
    pub fn parse(soul_content: &str) -> Self {
        let mut sections = Vec::new();
        let mut preamble = String::new();
        let mut current_name: Option<String> = None;
        let mut current_content = String::new();
        let mut current_mutability = SectionMutability::Evolvable;

        for line in soul_content.lines() {
            let trimmed = line.trim();

            // Check if this line is a section header
            let detected = Self::detect_section_header(trimmed);

            if let Some((name, mutability)) = detected {
                // Save previous section
                if let Some(prev_name) = current_name.take() {
                    sections.push(SoulSection {
                        name: prev_name,
                        content: current_content.trim_end().to_string(),
                        mutability: current_mutability,
                        integrity_hash: None,
                    });
                } else if !current_content.trim().is_empty() {
                    preamble = current_content.trim_end().to_string();
                }

                current_name = Some(name);
                current_mutability = mutability;
                current_content = String::new();
            } else {
                current_content.push_str(line);
                current_content.push('\n');
            }
        }

        // Save last section
        if let Some(name) = current_name {
            sections.push(SoulSection {
                name,
                content: current_content.trim_end().to_string(),
                mutability: current_mutability,
                integrity_hash: None,
            });
        } else if !current_content.trim().is_empty() {
            preamble = current_content.trim_end().to_string();
        }

        // Compute integrity hashes for immutable sections
        for section in &mut sections {
            if section.mutability == SectionMutability::Immutable {
                section.integrity_hash = Some(Self::compute_hash(&section.content));
            }
        }

        Self { sections, preamble }
    }

    /// Check if a proposed change would modify an immutable section.
    pub fn would_modify_immutable(&self, proposed_content: &str) -> Option<String> {
        let lower_proposed = proposed_content.to_lowercase();

        for section in &self.sections {
            if section.mutability != SectionMutability::Immutable {
                continue;
            }

            // Check if proposal references the immutable section by name
            let section_lower = section.name.to_lowercase();
            if lower_proposed.contains(&format!("replace {section_lower}"))
                || lower_proposed.contains(&format!("modify {section_lower}"))
                || lower_proposed.contains(&format!("change {section_lower}"))
                || lower_proposed.contains(&format!("rewrite {section_lower}"))
            {
                return Some(format!(
                    "Proposal attempts to modify immutable section '[{}]'",
                    section.name
                ));
            }
        }

        None
    }

    /// Verify integrity of immutable sections (detect unauthorized changes).
    pub fn verify_integrity(&self, current_soul: &str) -> Result<(), String> {
        let current = Self::parse(current_soul);

        for original in &self.sections {
            if original.mutability != SectionMutability::Immutable {
                continue;
            }
            if let Some(ref expected_hash) = original.integrity_hash {
                // Find matching section in current
                if let Some(current_section) = current.sections.iter().find(|s| s.name == original.name) {
                    let actual_hash = Self::compute_hash(&current_section.content);
                    if actual_hash != *expected_hash {
                        return Err(format!(
                            "Immutable section '[{}]' has been modified (hash mismatch)",
                            original.name
                        ));
                    }
                } else {
                    return Err(format!(
                        "Immutable section '[{}]' is missing from current SOUL.md",
                        original.name
                    ));
                }
            }
        }

        Ok(())
    }

    /// Reassemble sections into a SOUL.md string.
    pub fn reassemble(&self) -> String {
        let mut parts = Vec::new();

        if !self.preamble.is_empty() {
            parts.push(self.preamble.clone());
        }

        for section in &self.sections {
            let header = match section.mutability {
                SectionMutability::Immutable => format!("## [identity] {}", section.name),
                SectionMutability::Evolvable => format!("## [behaviors] {}", section.name),
                SectionMutability::Observable => format!("## [observations] {}", section.name),
            };
            parts.push(format!("{}\n\n{}", header, section.content));
        }

        parts.join("\n\n")
    }

    fn detect_section_header(line: &str) -> Option<(String, SectionMutability)> {
        let lower = line.to_lowercase();

        for header in IDENTITY_HEADERS {
            if lower.starts_with(&header.to_lowercase()) {
                let name = line[header.len()..].trim().to_string();
                let name = if name.is_empty() { "identity".to_string() } else { name };
                return Some((name, SectionMutability::Immutable));
            }
        }

        for header in BEHAVIOR_HEADERS {
            if lower.starts_with(&header.to_lowercase()) {
                let name = line[header.len()..].trim().to_string();
                let name = if name.is_empty() { "behaviors".to_string() } else { name };
                return Some((name, SectionMutability::Evolvable));
            }
        }

        for header in OBSERVATION_HEADERS {
            if lower.starts_with(&header.to_lowercase()) {
                let name = line[header.len()..].trim().to_string();
                let name = if name.is_empty() { "observations".to_string() } else { name };
                return Some((name, SectionMutability::Observable));
            }
        }

        None
    }

    fn compute_hash(content: &str) -> String {
        use ring::digest;
        let d = digest::digest(&digest::SHA256, content.as_bytes());
        d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    // ── SoulSpec v0.5 compatibility ────────────────────────────────

    /// Validate SoulSpec v0.5 structural requirements.
    ///
    /// Returns a list of issues. Empty list means fully compliant.
    /// SoulSpec v0.5 requires: Identity (or Core Identity/Role/Purpose),
    /// Personality (or Behaviors), and Language sections.
    pub fn validate_soulspec_v05(&self) -> Vec<String> {
        let mut issues = Vec::new();

        let section_names: Vec<String> = self
            .sections
            .iter()
            .map(|s| s.name.to_lowercase())
            .collect();

        // Check for identity section (immutable)
        let has_identity = self
            .sections
            .iter()
            .any(|s| s.mutability == SectionMutability::Immutable);
        if !has_identity {
            issues.push(
                "SoulSpec v0.5: missing identity section (## Identity, ## Core Identity, ## Role, or ## Purpose)"
                    .to_string(),
            );
        }

        // Check for personality/behaviors section
        let has_personality = section_names.iter().any(|n| {
            n.contains("personality")
                || n.contains("behavior")
                || n.contains("style")
                || n.contains("responsibilit")
                || n.contains("個性")
                || n.contains("行為")
        });
        if !has_personality {
            issues.push(
                "SoulSpec v0.5: missing personality/behaviors section (## Personality, ## Behaviors, or ## Response Style)"
                    .to_string(),
            );
        }

        // Check for language section (optional but recommended)
        let has_language = self.preamble.to_lowercase().contains("language")
            || section_names.iter().any(|n| n.contains("language") || n.contains("語言"));
        if !has_language {
            issues.push(
                "SoulSpec v0.5: recommend adding a ## Language section for multi-language agents"
                    .to_string(),
            );
        }

        issues
    }

    /// Check if this SOUL.md follows SoulSpec v0.5 format.
    pub fn is_soulspec_compliant(&self) -> bool {
        self.validate_soulspec_v05()
            .iter()
            .all(|i| i.contains("recommend"))
    }

    /// Export this SOUL.md in SoulSpec v0.5 canonical format.
    ///
    /// Normalizes section headers to SoulSpec v0.5 naming convention.
    pub fn to_soulspec_format(&self) -> String {
        let mut parts = Vec::new();

        if !self.preamble.is_empty() {
            parts.push(self.preamble.clone());
        }

        for section in &self.sections {
            let header = match section.mutability {
                SectionMutability::Immutable => format!("## Identity: {}", section.name),
                SectionMutability::Evolvable => format!("## {}", section.name),
                SectionMutability::Observable => format!("## Observations: {}", section.name),
            };
            parts.push(format!("{}\n\n{}", header, section.content));
        }

        parts.join("\n\n")
    }
}

/// Observation entry with TTL for auto-decay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationEntry {
    /// Content of the observation.
    pub content: String,
    /// When this observation was added.
    pub added_at: DateTime<Utc>,
    /// How many GVU cycles this observation has survived without reinforcement.
    pub cycles_without_reinforcement: u32,
    /// Half-life in GVU cycles (default 20). Removed when exceeded.
    pub half_life_cycles: u32,
    /// Number of times this observation was reinforced by subsequent evidence.
    pub reinforcement_count: u32,
}

impl ObservationEntry {
    pub fn new(content: String) -> Self {
        Self {
            content,
            added_at: Utc::now(),
            cycles_without_reinforcement: 0,
            half_life_cycles: 20,
            reinforcement_count: 0,
        }
    }

    /// Whether this observation has expired (should be removed).
    pub fn is_expired(&self) -> bool {
        self.cycles_without_reinforcement > self.half_life_cycles
    }

    /// Record a GVU cycle passing without reinforcement.
    pub fn tick(&mut self) {
        self.cycles_without_reinforcement += 1;
    }

    /// Reset decay counter (evidence reinforced this observation).
    pub fn reinforce(&mut self) {
        self.cycles_without_reinforcement = 0;
        self.reinforcement_count += 1;
    }
}
