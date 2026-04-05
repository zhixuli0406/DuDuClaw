//! Structured delegation protocol for inter-agent communication (Phase 2 GVU²).
//!
//! Replaces raw string payloads with typed `DelegationEnvelope` that carries
//! structured context, constraints, and expected output format. Backwards
//! compatible via `TaskPayload::Raw(String)` fallback.
//!
//! Inspired by:
//! - Google A2A Protocol (arXiv:2505.02279) — Task artifact structured handoff
//! - Anthropic Multi-Agent Research System (2025.06) — Orchestrator-Worker pattern
//! - MCP Multi-Agent Context Transfer (arXiv:2504.21030)

use serde::{Deserialize, Serialize};

/// Payload for a delegation message — either structured or raw text.
///
/// Uses `#[serde(untagged)]` for backwards compatibility. Variant order matters:
/// `Raw(String)` is tried LAST because any JSON value can deserialize as a string
/// via serde's untagged fallback. `Structured` is tried first — it requires a JSON
/// object with a `task` field, so plain strings will fail and fall through to `Raw`.
///
/// **Caveat (review issue #8)**: If a structured payload has a typo in a required
/// field, it silently degrades to `Raw`. Callers should validate via
/// `matches!(payload, TaskPayload::Structured(_))` when structured input is expected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskPayload {
    /// New structured delegation with context and expected output.
    /// Tried first: requires a JSON object with at least a `task` field.
    Structured(DelegationEnvelope),
    /// Legacy raw string payload (backwards compatible).
    /// Fallback: any JSON string deserializes here.
    Raw(String),
}

impl TaskPayload {
    /// Whether this is a structured payload (not a raw string fallback).
    pub fn is_structured(&self) -> bool {
        matches!(self, Self::Structured(_))
    }
}

impl TaskPayload {
    /// Extract the task description regardless of payload type.
    pub fn task_description(&self) -> &str {
        match self {
            Self::Structured(env) => &env.task,
            Self::Raw(s) => s,
        }
    }

    /// Build a prompt string for Claude CLI from this payload.
    ///
    /// For `Raw`, returns the string as-is.
    /// For `Structured`, assembles context + constraints + task into a prompt.
    pub fn to_prompt(&self) -> String {
        match self {
            Self::Raw(s) => s.clone(),
            Self::Structured(env) => env.to_prompt(),
        }
    }
}

impl Default for TaskPayload {
    fn default() -> Self {
        Self::Raw(String::new())
    }
}

impl From<String> for TaskPayload {
    fn from(s: String) -> Self {
        Self::Raw(s)
    }
}

impl From<&str> for TaskPayload {
    fn from(s: &str) -> Self {
        Self::Raw(s.to_string())
    }
}

/// Structured delegation envelope — carries full context for agent delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationEnvelope {
    /// The task to perform (required).
    pub task: String,
    /// Structured context from the delegating agent.
    #[serde(default)]
    pub context: DelegationContext,
    /// Expected output format and constraints.
    #[serde(default)]
    pub expected_output: OutputSpec,
}

impl DelegationEnvelope {
    /// Build a prompt string from the envelope.
    pub fn to_prompt(&self) -> String {
        let mut sections = Vec::new();

        // Briefing
        if !self.context.briefing.is_empty() {
            sections.push(format!("## Context\n{}", self.context.briefing));
        }

        // Constraints
        if !self.context.constraints.is_empty() {
            let list = self.context.constraints
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("## Constraints\n{list}"));
        }

        // Task chain (prior steps)
        if !self.context.task_chain.is_empty() {
            let chain = self.context.task_chain
                .iter()
                .map(|e| format!("- **{}** ({}): {}", e.agent_id, e.status, e.summary))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("## Prior Steps\n{chain}"));
        }

        // Task
        sections.push(format!("## Task\n{}", self.task));

        // Output spec
        match self.expected_output.format {
            OutputFormat::FreeText => {}
            OutputFormat::Json => {
                sections.push("## Output Format\nRespond with a valid JSON object.".to_string());
            }
            OutputFormat::Diff => {
                sections.push("## Output Format\nRespond with a unified diff.".to_string());
            }
            OutputFormat::Decision => {
                sections.push("## Output Format\nRespond with: APPROVED or REJECTED followed by rationale.".to_string());
            }
        }

        if let Some(max) = self.expected_output.max_length {
            sections.push(format!("Keep response under {max} characters."));
        }

        sections.join("\n\n")
    }
}

/// Structured context passed from delegating agent to worker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegationContext {
    /// High-level briefing from the delegating agent (≤2000 chars).
    #[serde(default)]
    pub briefing: String,
    /// Hard constraints the worker must respect.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// References to memory entries (episodic memory IDs).
    #[serde(default)]
    pub memory_refs: Vec<String>,
    /// Chain of prior task steps (for multi-step workflows).
    #[serde(default)]
    pub task_chain: Vec<TaskChainEntry>,
}

/// A single entry in the task chain (prior step result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskChainEntry {
    /// Which agent executed this step.
    pub agent_id: String,
    /// Step status: "completed" | "failed" | "partial".
    pub status: String,
    /// Summary of the step result (≤500 chars).
    pub summary: String,
}

/// Expected output specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSpec {
    /// Expected response format.
    #[serde(default)]
    pub format: OutputFormat,
    /// Maximum response length in characters.
    #[serde(default)]
    pub max_length: Option<usize>,
}

impl Default for OutputSpec {
    fn default() -> Self {
        Self {
            format: OutputFormat::FreeText,
            max_length: None,
        }
    }
}

/// Expected output format from a delegated agent.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    FreeText,
    Json,
    Diff,
    Decision,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_payload_compat() {
        let json = r#""hello world""#;
        let payload: TaskPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.task_description(), "hello world");
        assert_eq!(payload.to_prompt(), "hello world");
    }

    #[test]
    fn test_structured_payload() {
        let env = DelegationEnvelope {
            task: "Review this code".to_string(),
            context: DelegationContext {
                briefing: "We're fixing a bug in auth".to_string(),
                constraints: vec!["No breaking changes".to_string()],
                ..Default::default()
            },
            expected_output: OutputSpec {
                format: OutputFormat::Decision,
                max_length: Some(1000),
            },
        };
        let payload = TaskPayload::Structured(env);
        let json = serde_json::to_string(&payload).unwrap();

        // Deserialize back
        let parsed: TaskPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_description(), "Review this code");

        let prompt = parsed.to_prompt();
        assert!(prompt.contains("## Context"));
        assert!(prompt.contains("## Constraints"));
        assert!(prompt.contains("APPROVED or REJECTED"));
        assert!(prompt.contains("under 1000 characters"));
    }

    #[test]
    fn test_task_chain_in_prompt() {
        let env = DelegationEnvelope {
            task: "Verify result".to_string(),
            context: DelegationContext {
                task_chain: vec![
                    TaskChainEntry {
                        agent_id: "coder".to_string(),
                        status: "completed".to_string(),
                        summary: "Implemented auth fix".to_string(),
                    },
                ],
                ..Default::default()
            },
            expected_output: OutputSpec::default(),
        };
        let prompt = env.to_prompt();
        assert!(prompt.contains("## Prior Steps"));
        assert!(prompt.contains("**coder** (completed)"));
    }
}
