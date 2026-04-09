//! Failure-driven compression guidelines (ACON, arXiv 2510.00615).
//!
//! When compression causes agent failure (missing data it needed),
//! the guideline manager elevates the minimum fidelity for that tool.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::tool_classifier::ToolResultFidelity;

/// A compression guideline for a specific tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionGuideline {
    pub tool_name: String,
    /// Minimum fidelity — classifier cannot go below this.
    pub min_fidelity: ToolResultFidelity,
    /// JSON fields to always preserve when compressing.
    pub important_fields: Vec<String>,
    /// Why this guideline exists (from failure context).
    pub reason: String,
    /// Whether this was auto-created from a failure.
    pub created_from_failure: bool,
    /// How many times this guideline has been applied.
    pub apply_count: u32,
}

/// Manages per-tool compression guidelines.
pub struct GuidelineManager {
    guidelines: HashMap<String, CompressionGuideline>,
}

impl GuidelineManager {
    /// Create an empty guideline manager.
    pub fn new() -> Self {
        Self {
            guidelines: HashMap::new(),
        }
    }

    /// Load guidelines from a JSON string (array of CompressionGuideline).
    pub fn load_from_json(json: &str) -> Result<Self, serde_json::Error> {
        let entries: Vec<CompressionGuideline> = serde_json::from_str(json)?;
        let guidelines = entries
            .into_iter()
            .map(|g| (g.tool_name.clone(), g))
            .collect();
        Ok(Self { guidelines })
    }

    /// Record a compression failure — elevates the tool's min_fidelity.
    ///
    /// If the tool already has a guideline, the minimum fidelity is elevated
    /// one step: Discard → Placeholder → Compressed → Full.
    /// If no guideline exists, a new one is created with Compressed min_fidelity.
    pub fn record_failure(&mut self, tool_name: &str, error_context: &str) {
        if let Some(existing) = self.guidelines.get_mut(tool_name) {
            existing.min_fidelity = elevate(existing.min_fidelity);
            existing.reason = format!(
                "{}; elevated due to: {}",
                existing.reason, error_context
            );
            existing.apply_count += 1;
        } else {
            self.guidelines.insert(
                tool_name.to_string(),
                CompressionGuideline {
                    tool_name: tool_name.to_string(),
                    min_fidelity: ToolResultFidelity::Compressed,
                    important_fields: Vec::new(),
                    reason: format!("auto-created from failure: {}", error_context),
                    created_from_failure: true,
                    apply_count: 1,
                },
            );
        }
    }

    /// Get the guideline for a tool (if any).
    pub fn get(&self, tool_name: &str) -> Option<&CompressionGuideline> {
        self.guidelines.get(tool_name)
    }

    /// Check if a proposed fidelity meets the minimum for a tool.
    /// Returns the higher of (proposed, min_fidelity).
    pub fn enforce_minimum(
        &self,
        tool_name: &str,
        proposed: ToolResultFidelity,
    ) -> ToolResultFidelity {
        match self.guidelines.get(tool_name) {
            Some(g) => proposed.max(g.min_fidelity),
            None => proposed,
        }
    }

    /// Serialize all guidelines to a JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let entries: Vec<&CompressionGuideline> = self.guidelines.values().collect();
        serde_json::to_string_pretty(&entries)
    }
}

impl Default for GuidelineManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Elevate a fidelity tier by one step toward Full.
fn elevate(fidelity: ToolResultFidelity) -> ToolResultFidelity {
    match fidelity {
        ToolResultFidelity::Discard => ToolResultFidelity::Placeholder,
        ToolResultFidelity::Placeholder => ToolResultFidelity::Compressed,
        ToolResultFidelity::Compressed => ToolResultFidelity::Full,
        ToolResultFidelity::Full => ToolResultFidelity::Full,
    }
}
