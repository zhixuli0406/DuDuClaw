//! TaskSpec — structured multi-step task planning and execution (Phase 3 GVU²).
//!
//! Enables long-running workflows (4-5 hours) by decomposing complex tasks into
//! a sequence of steps, each with acceptance criteria and dependency tracking.
//!
//! The Orchestrator agent produces a TaskSpec, the dispatcher executes it
//! step-by-step, and the Evaluator agent verifies each step's output.
//!
//! Inspired by:
//! - Anthropic Harness Design (2026.03) — JSON spec + sprint decomposition
//! - PLAN-AND-ACT (arXiv:2503.09572) — Planner/Executor separation
//! - GoalAct (arXiv:2504.16563) — hierarchical goal decomposition
//! - Active Inference Planning (arXiv:2504.14898) — adaptive iteration depth

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A structured multi-step task specification.
///
/// Produced by the PlannerPhase (Haiku call) or by an Orchestrator agent.
/// Persisted as JSON in `~/.duduclaw/agents/{id}/tasks/{task_id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Unique task identifier.
    pub task_id: String,
    /// Agent that owns this task (orchestrator or direct executor).
    pub agent_id: String,
    /// Original goal / user request.
    pub goal: String,
    /// Ordered list of steps to execute.
    pub steps: Vec<Step>,
    /// Index of the current step being executed (0-based).
    pub current_step: usize,
    /// Maximum number of replans allowed (default 2).
    pub max_replans: u8,
    /// How many replans have been consumed so far.
    pub replan_count: u8,
    /// Arbitrary state carried between steps (updated by each step's result).
    #[serde(default)]
    pub state: serde_json::Value,
    /// Artifacts produced during execution (file paths, diffs, decisions).
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Overall task status.
    pub status: TaskStatus,
    /// When this task was created.
    pub created_at: DateTime<Utc>,
    /// When this task was last updated.
    pub updated_at: DateTime<Utc>,
}

/// A single step in a TaskSpec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Step index (0-based, matches position in `steps` vec).
    pub id: u8,
    /// Human-readable description of what this step does.
    pub description: String,
    /// Which agent should execute this step (empty = same as task owner).
    #[serde(default)]
    pub agent: String,
    /// Step IDs that must complete before this step can run.
    #[serde(default)]
    pub depends_on: Vec<u8>,
    /// Acceptance criteria — how to verify this step succeeded.
    #[serde(default)]
    pub acceptance_criteria: Vec<Criterion>,
    /// Current status of this step.
    pub status: StepStatus,
    /// Result of execution (populated after completion).
    #[serde(default)]
    pub result: Option<StepResult>,
    /// How many times this step has been retried (max 3).
    #[serde(default)]
    pub retry_count: u8,
}

/// Acceptance criterion for a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    /// Description of what to verify.
    pub description: String,
    /// How to verify it.
    pub method: VerificationMethod,
}

/// How a criterion should be verified.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    /// Pattern/keyword matching (zero LLM cost).
    #[default]
    Auto,
    /// Execute in container sandbox and check exit code / output.
    Sandbox,
    /// LLM judge evaluates against criteria (1 call).
    LlmJudge,
    /// Human verification via dashboard (push notification).
    Manual,
}

/// Status of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// Result of a step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// The output text from the agent.
    pub output: String,
    /// Artifacts produced by this step.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Which criteria passed / failed.
    #[serde(default)]
    pub criteria_results: Vec<CriterionResult>,
    /// Self-assessment confidence (0.0-1.0), reported by the agent.
    #[serde(default)]
    pub self_confidence: Option<f64>,
    /// When this step completed.
    pub completed_at: DateTime<Utc>,
}

/// Result of checking a single criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionResult {
    pub description: String,
    pub passed: bool,
    pub evidence: String,
}

/// An artifact produced during task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Artifact type: "file", "diff", "decision", "data".
    pub kind: String,
    /// Description or path.
    pub value: String,
}

/// Overall task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task has been planned but not started.
    Planned,
    /// Task is actively being executed step-by-step.
    Running,
    /// All steps completed successfully.
    Completed,
    /// Task failed after exhausting retries and replans.
    Failed,
    /// Task was cancelled by user or timeout.
    Cancelled,
}

// ---------------------------------------------------------------------------
// TaskSpec operations
// ---------------------------------------------------------------------------

impl TaskSpec {
    /// Create a new TaskSpec from a goal and a list of steps.
    pub fn new(agent_id: &str, goal: &str, steps: Vec<Step>) -> Self {
        let now = Utc::now();
        Self {
            task_id: Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            goal: goal.to_string(),
            steps,
            current_step: 0,
            max_replans: 2,
            replan_count: 0,
            state: serde_json::Value::Null,
            artifacts: Vec::new(),
            status: TaskStatus::Planned,
            created_at: now,
            updated_at: now,
        }
    }

    /// Get the next step that is ready to execute.
    ///
    /// A step is ready if:
    /// 1. Its status is `Pending`
    /// 2. All its `depends_on` steps have status `Passed`
    pub fn next_ready_step(&self) -> Option<&Step> {
        self.steps.iter().find(|step| {
            step.status == StepStatus::Pending
                && step.depends_on.iter().all(|dep| {
                    self.steps
                        .iter()
                        .find(|s| s.id == *dep)
                        .map(|s| s.status == StepStatus::Passed)
                        .unwrap_or(false)
                })
        })
    }

    /// Get the next ready step index, or None if all done / blocked.
    pub fn next_ready_step_index(&self) -> Option<usize> {
        self.steps.iter().position(|step| {
            step.status == StepStatus::Pending
                && step.depends_on.iter().all(|dep| {
                    self.steps
                        .iter()
                        .find(|s| s.id == *dep)
                        .map(|s| s.status == StepStatus::Passed)
                        .unwrap_or(false)
                })
        })
    }

    /// Mark a step as running.
    pub fn mark_running(&mut self, step_index: usize) {
        if let Some(step) = self.steps.get_mut(step_index) {
            step.status = StepStatus::Running;
            self.current_step = step_index;
            self.status = TaskStatus::Running;
            self.updated_at = Utc::now();
        }
    }

    /// Mark a step as passed with its result.
    pub fn mark_passed(&mut self, step_index: usize, result: StepResult) {
        if let Some(step) = self.steps.get_mut(step_index) {
            self.artifacts.extend(result.artifacts.clone());
            step.status = StepStatus::Passed;
            step.result = Some(result);
            self.updated_at = Utc::now();

            // Check if all steps are done
            if self.steps.iter().all(|s| s.status == StepStatus::Passed || s.status == StepStatus::Skipped) {
                self.status = TaskStatus::Completed;
            }
        }
    }

    /// Mark a step as failed and decide next action.
    ///
    /// Returns the recommended action: Retry, Replan, or Abandon.
    /// Note: the step's status is set to the appropriate final state BEFORE returning
    /// (review issue #10 — no transient Failed→Pending flicker).
    pub fn mark_failed(&mut self, step_index: usize, error: &str) -> FailureAction {
        if let Some(step) = self.steps.get_mut(step_index) {
            step.retry_count += 1;
            self.updated_at = Utc::now();

            if step.retry_count < 3 {
                // Can retry — keep as Pending so the executor picks it up again
                step.status = StepStatus::Pending;
                return FailureAction::Retry {
                    step_index,
                    attempt: step.retry_count,
                    error: error.to_string(),
                };
            }

            // Retries exhausted — mark as Failed
            step.status = StepStatus::Failed;

            // Try replan if budget allows
            if self.replan_count < self.max_replans {
                return FailureAction::Replan {
                    failed_step: step_index,
                    error: error.to_string(),
                };
            }

            // All options exhausted
            self.status = TaskStatus::Failed;
            FailureAction::Abandon {
                reason: format!(
                    "Step {} failed after {} retries and {} replans: {}",
                    step_index, step.retry_count, self.replan_count, error
                ),
            }
        } else {
            FailureAction::Abandon {
                reason: format!("Invalid step index: {step_index}"),
            }
        }
    }

    /// Apply a replan: replace remaining pending steps with new ones.
    ///
    /// Completed steps are preserved. Only pending/failed steps are replaced.
    pub fn replan(&mut self, new_remaining_steps: Vec<Step>) {
        self.replan_count += 1;
        self.updated_at = Utc::now();

        // Keep completed steps
        let completed: Vec<Step> = self.steps.iter()
            .filter(|s| s.status == StepStatus::Passed)
            .cloned()
            .collect();

        // Build old_id → new_id mapping for depends_on remapping (review R2-6).
        let offset = completed.len();
        let id_map: std::collections::HashMap<u8, u8> = new_remaining_steps.iter().enumerate()
            .map(|(i, s)| (s.id, (offset + i).min(u8::MAX as usize) as u8))
            .collect();

        let renumbered: Vec<Step> = new_remaining_steps.into_iter().enumerate().map(|(i, mut s)| {
            s.id = (offset + i).min(u8::MAX as usize) as u8;
            // Remap depends_on: new-batch references use the id_map,
            // references to completed steps stay as-is (they already have correct ids).
            s.depends_on = s.depends_on.iter().map(|dep| {
                *id_map.get(dep).unwrap_or(dep)
            }).collect();
            s
        }).collect();

        self.steps = completed;
        self.steps.extend(renumbered);
        self.current_step = self.steps.iter().position(|s| s.status == StepStatus::Pending).unwrap_or(0);

        info!(
            task = %self.task_id,
            replan = self.replan_count,
            remaining = self.steps.len() - self.current_step,
            "Task replanned"
        );
    }

    /// Build a briefing string summarizing completed steps for context injection.
    pub fn completed_steps_briefing(&self) -> String {
        let completed: Vec<String> = self.steps.iter()
            .filter(|s| s.status == StepStatus::Passed)
            .filter_map(|s| {
                s.result.as_ref().map(|r| {
                    let output_preview: String = r.output.chars().take(200).collect();
                    format!("Step {}: {} → {}", s.id, s.description, output_preview)
                })
            })
            .collect();

        if completed.is_empty() {
            "No prior steps completed.".to_string()
        } else {
            completed.join("\n")
        }
    }

    /// Build a DelegationEnvelope for a specific step.
    pub fn delegation_for_step(&self, step_index: usize) -> Option<crate::delegation::DelegationEnvelope> {
        let step = self.steps.get(step_index)?;

        let criteria_text: Vec<String> = step.acceptance_criteria.iter()
            .map(|c| format!("[{:?}] {}", c.method, c.description))
            .collect();

        let task_chain: Vec<crate::delegation::TaskChainEntry> = self.steps.iter()
            .filter(|s| s.status == StepStatus::Passed)
            .filter_map(|s| {
                s.result.as_ref().map(|r| {
                    let summary: String = r.output.chars().take(300).collect();
                    crate::delegation::TaskChainEntry {
                        agent_id: if s.agent.is_empty() { self.agent_id.clone() } else { s.agent.clone() },
                        status: "completed".to_string(),
                        summary,
                    }
                })
            })
            .collect();

        Some(crate::delegation::DelegationEnvelope {
            task: step.description.clone(),
            context: crate::delegation::DelegationContext {
                briefing: format!(
                    "You are executing step {} of {} for task: {}\n\n{}",
                    step.id + 1,
                    self.steps.len(),
                    self.goal,
                    self.completed_steps_briefing(),
                ),
                constraints: criteria_text,
                memory_refs: Vec::new(),
                task_chain,
            },
            expected_output: crate::delegation::OutputSpec::default(),
        })
    }

    /// Check if the task is complete (all steps passed or task failed/cancelled).
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled)
    }
}

/// Recommended action after a step failure.
#[derive(Debug, Clone)]
pub enum FailureAction {
    /// Retry the same step with error context.
    Retry {
        step_index: usize,
        attempt: u8,
        error: String,
    },
    /// Replan remaining steps (failed step + subsequent).
    Replan {
        failed_step: usize,
        error: String,
    },
    /// Abandon the task entirely.
    Abandon {
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

impl TaskSpec {
    /// Save to disk as JSON.
    pub fn save(&self, agent_dir: &Path) -> Result<PathBuf, String> {
        let tasks_dir = agent_dir.join("tasks");
        std::fs::create_dir_all(&tasks_dir).map_err(|e| format!("Create tasks dir: {e}"))?;

        let path = tasks_dir.join(format!("{}.json", self.task_id));
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("Serialize: {e}"))?;

        // Atomic write: temp → rename
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json).map_err(|e| format!("Write tmp: {e}"))?;
        std::fs::rename(&tmp_path, &path).map_err(|e| format!("Rename: {e}"))?;

        Ok(path)
    }

    /// Load from disk.
    ///
    /// `task_id` is sanitized to prevent path traversal (review issue #13).
    pub fn load(agent_dir: &Path, task_id: &str) -> Result<Self, String> {
        // Sanitize: only allow alphanumeric + hyphens (UUID format)
        if !task_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("Invalid task_id: contains disallowed characters"));
        }
        let path = agent_dir.join("tasks").join(format!("{task_id}.json"));
        let content = std::fs::read_to_string(&path).map_err(|e| format!("Read: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("Parse: {e}"))
    }

    /// List all task IDs for an agent.
    pub fn list(agent_dir: &Path) -> Vec<String> {
        let tasks_dir = agent_dir.join("tasks");
        std::fs::read_dir(&tasks_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        if name.ends_with(".json") && !name.ends_with(".tmp") {
                            Some(name.trim_end_matches(".json").to_string())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// PlannerPhase — complexity routing + task decomposition
// ---------------------------------------------------------------------------

/// Complexity estimate for a task — determines whether to plan or execute directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    /// Single-step, <5 min. Execute directly without planning.
    Simple,
    /// 2-5 steps. Lightweight sequential plan.
    Medium,
    /// 5+ steps or cross-domain. Full TaskSpec with dependencies.
    Complex,
}

/// Estimate task complexity from the prompt text (zero LLM cost).
///
/// Heuristic based on:
/// - Message length
/// - Step-like keywords ("first", "then", "finally", "步驟", "接著")
/// - Cross-domain indicators ("and also", "另外", multiple distinct topics)
pub fn estimate_complexity(prompt: &str) -> TaskComplexity {
    let lower = prompt.to_lowercase();
    let char_count = prompt.chars().count();

    // Step indicators
    let step_keywords_en = ["first", "then", "next", "after that", "finally", "step 1", "step 2", "phase"];
    let step_keywords_zh = [
        "\u{6B65}\u{9A5F}",     // 步驟
        "\u{63A5}\u{8457}",     // 接著
        "\u{7136}\u{5F8C}",     // 然後
        "\u{6700}\u{5F8C}",     // 最後
        "\u{9996}\u{5148}",     // 首先
        "\u{7B2C}\u{4E00}\u{6B65}", // 第一步
    ];

    let step_count: usize = step_keywords_en.iter().filter(|kw| lower.contains(*kw)).count()
        + step_keywords_zh.iter().filter(|kw| lower.contains(*kw)).count();

    // Cross-domain indicators
    let cross_domain_en = ["and also", "in addition", "separately", "plus"];
    let cross_domain_zh = [
        "\u{53E6}\u{5916}",     // 另外
        "\u{800C}\u{4E14}",     // 而且
        "\u{9084}\u{8981}",     // 還要
        "\u{540C}\u{6642}",     // 同時
    ];

    let cross_domain: usize = cross_domain_en.iter().filter(|kw| lower.contains(*kw)).count()
        + cross_domain_zh.iter().filter(|kw| lower.contains(*kw)).count();

    // Numbered list detection
    let has_numbered_list = lower.contains("1.") && lower.contains("2.");

    if step_count >= 3 || (step_count >= 2 && cross_domain >= 1) || (has_numbered_list && char_count > 500) {
        TaskComplexity::Complex
    } else if step_count >= 1 || char_count > 300 || cross_domain >= 1 {
        TaskComplexity::Medium
    } else {
        TaskComplexity::Simple
    }
}

/// Build the prompt for the PlannerPhase (Haiku call) to decompose a task.
///
/// The planner receives the user's goal and produces a JSON TaskSpec.
pub fn build_planner_prompt(goal: &str, agent_id: &str) -> String {
    format!(
        "You are a task planner for agent '{agent_id}'. Decompose the following goal into \
         concrete, sequential steps.\n\n\
         ## Goal\n{goal}\n\n\
         ## Instructions\n\
         Respond with a JSON array of steps. Each step must have:\n\
         - \"description\": What to do (1-2 sentences)\n\
         - \"agent\": Which agent should handle it (empty string = default agent)\n\
         - \"depends_on\": Array of step IDs (0-based) that must complete first\n\
         - \"acceptance_criteria\": Array of objects with \"description\" and \"method\" \
           (\"auto\", \"sandbox\", \"llm_judge\", or \"manual\")\n\n\
         Example:\n\
         ```json\n\
         [\n\
           {{\n\
             \"description\": \"Implement the auth endpoint\",\n\
             \"agent\": \"coder\",\n\
             \"depends_on\": [],\n\
             \"acceptance_criteria\": [\n\
               {{\"description\": \"Endpoint returns 200 on valid token\", \"method\": \"sandbox\"}}\n\
             ]\n\
           }},\n\
           {{\n\
             \"description\": \"Write integration tests\",\n\
             \"agent\": \"coder\",\n\
             \"depends_on\": [0],\n\
             \"acceptance_criteria\": [\n\
               {{\"description\": \"All tests pass\", \"method\": \"sandbox\"}}\n\
             ]\n\
           }}\n\
         ]\n\
         ```\n\n\
         Keep it focused: 2-7 steps for medium tasks, 5-15 for complex ones.\n\
         Respond ONLY with the JSON array, no other text.",
    )
}

/// Parse the planner's JSON response into a Vec<Step>.
pub fn parse_planner_response(response: &str) -> Result<Vec<Step>, String> {
    // Extract JSON array from response (may be wrapped in ```json ... ```)
    let json_str = if let Some(start) = response.find('[') {
        if let Some(end) = response.rfind(']') {
            &response[start..=end]
        } else {
            return Err("No closing bracket found in planner response".to_string());
        }
    } else {
        return Err("No JSON array found in planner response".to_string());
    };

    let raw: Vec<serde_json::Value> = serde_json::from_str(json_str)
        .map_err(|e| format!("JSON parse error: {e}"))?;

    let steps: Vec<Step> = raw
        .into_iter()
        .enumerate()
        .map(|(i, val)| {
            let description = val.get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("(no description)")
                .to_string();

            let agent = val.get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let depends_on: Vec<u8> = val.get("depends_on")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(|n| n as u8)).collect())
                .unwrap_or_default();

            let acceptance_criteria: Vec<Criterion> = val.get("acceptance_criteria")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter().map(|c| {
                        let desc = c.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let method_str = c.get("method").and_then(|v| v.as_str()).unwrap_or("auto");
                        let method = match method_str {
                            "sandbox" => VerificationMethod::Sandbox,
                            "llm_judge" => VerificationMethod::LlmJudge,
                            "manual" => VerificationMethod::Manual,
                            _ => VerificationMethod::Auto,
                        };
                        Criterion { description: desc, method }
                    }).collect()
                })
                .unwrap_or_default();

            Step {
                id: i as u8,
                description,
                agent,
                depends_on,
                acceptance_criteria,
                status: StepStatus::Pending,
                result: None,
                retry_count: 0,
            }
        })
        .collect();

    if steps.is_empty() {
        return Err("Planner produced zero steps".to_string());
    }

    Ok(steps)
}

// ---------------------------------------------------------------------------
// Step execution
// ---------------------------------------------------------------------------

/// Build the prompt for executing a single step of a TaskSpec.
///
/// Injects prior step results as context so the agent has full situational awareness.
pub fn build_step_prompt(spec: &TaskSpec, step_index: usize) -> Option<String> {
    let step = spec.steps.get(step_index)?;

    let mut sections = Vec::new();

    // Task context
    sections.push(format!(
        "## Task: {}\nYou are executing step {} of {}.",
        spec.goal,
        step.id + 1,
        spec.steps.len(),
    ));

    // Prior step results
    let briefing = spec.completed_steps_briefing();
    if !briefing.starts_with("No prior") {
        sections.push(format!("## Prior Steps\n{briefing}"));
    }

    // Current step
    sections.push(format!("## Your Step\n{}", step.description));

    // Acceptance criteria
    if !step.acceptance_criteria.is_empty() {
        let criteria: Vec<String> = step.acceptance_criteria.iter()
            .map(|c| format!("- {}", c.description))
            .collect();
        sections.push(format!("## Acceptance Criteria\n{}", criteria.join("\n")));
    }

    // Retry context
    if step.retry_count > 0 {
        sections.push(format!(
            "## Retry\nThis is attempt {} (previous attempts failed). \
             Please try a different approach.",
            step.retry_count + 1,
        ));
    }

    Some(sections.join("\n\n"))
}

/// Verify a step's output against its acceptance criteria (zero LLM for Auto).
pub fn verify_step_auto(step: &Step, output: &str) -> Vec<CriterionResult> {
    step.acceptance_criteria
        .iter()
        .filter(|c| matches!(c.method, VerificationMethod::Auto))
        .map(|c| {
            // Simple keyword check: does the output seem to address the criterion?
            let keywords: Vec<&str> = c.description.split_whitespace()
                .filter(|w| w.len() > 3)
                .collect();
            let matches = keywords.iter()
                .filter(|kw| output.to_lowercase().contains(&kw.to_lowercase()))
                .count();
            // Require at least 1 keyword match (review issue #14 — length fallback removed)
            let passed = matches > 0;

            CriterionResult {
                description: c.description.clone(),
                passed,
                evidence: if passed {
                    format!("Output matches {}/{} keywords", matches, keywords.len())
                } else {
                    "Output does not appear to address this criterion".to_string()
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_steps() -> Vec<Step> {
        vec![
            Step {
                id: 0,
                description: "Write the auth module".to_string(),
                agent: "coder".to_string(),
                depends_on: vec![],
                acceptance_criteria: vec![
                    Criterion { description: "Auth endpoint exists".to_string(), method: VerificationMethod::Auto },
                ],
                status: StepStatus::Pending,
                result: None,
                retry_count: 0,
            },
            Step {
                id: 1,
                description: "Write tests".to_string(),
                agent: "coder".to_string(),
                depends_on: vec![0],
                acceptance_criteria: vec![],
                status: StepStatus::Pending,
                result: None,
                retry_count: 0,
            },
            Step {
                id: 2,
                description: "Deploy to staging".to_string(),
                agent: "deployer".to_string(),
                depends_on: vec![0, 1],
                acceptance_criteria: vec![],
                status: StepStatus::Pending,
                result: None,
                retry_count: 0,
            },
        ]
    }

    #[test]
    fn test_next_ready_step() {
        let spec = TaskSpec::new("orch", "Build auth", sample_steps());
        // Only step 0 is ready (no deps)
        assert_eq!(spec.next_ready_step().unwrap().id, 0);
    }

    #[test]
    fn test_step_progression() {
        let mut spec = TaskSpec::new("orch", "Build auth", sample_steps());

        // Step 0 → running → passed
        spec.mark_running(0);
        assert_eq!(spec.steps[0].status, StepStatus::Running);

        spec.mark_passed(0, StepResult {
            output: "Auth module done".to_string(),
            artifacts: vec![],
            criteria_results: vec![],
            self_confidence: Some(0.9),
            completed_at: Utc::now(),
        });
        assert_eq!(spec.steps[0].status, StepStatus::Passed);

        // Now step 1 should be ready (depends on 0 which is passed)
        assert_eq!(spec.next_ready_step().unwrap().id, 1);
        // Step 2 not ready (depends on 0 AND 1)
        assert_ne!(spec.next_ready_step().unwrap().id, 2);
    }

    #[test]
    fn test_failure_retry() {
        let mut spec = TaskSpec::new("orch", "Build auth", sample_steps());
        spec.mark_running(0);

        let action = spec.mark_failed(0, "Compile error");
        assert!(matches!(action, FailureAction::Retry { step_index: 0, attempt: 1, .. }));
        assert_eq!(spec.steps[0].status, StepStatus::Pending); // reset for retry
    }

    #[test]
    fn test_failure_replan() {
        let mut spec = TaskSpec::new("orch", "Build auth", sample_steps());
        spec.mark_running(0);
        // Exhaust retries (3 failures)
        spec.mark_failed(0, "err1");
        spec.mark_failed(0, "err2");
        let action = spec.mark_failed(0, "err3");
        assert!(matches!(action, FailureAction::Replan { .. }));
    }

    #[test]
    fn test_failure_abandon() {
        let mut spec = TaskSpec::new("orch", "Build auth", sample_steps());
        spec.max_replans = 0; // No replans allowed
        spec.mark_running(0);
        spec.mark_failed(0, "err1");
        spec.mark_failed(0, "err2");
        let action = spec.mark_failed(0, "err3");
        assert!(matches!(action, FailureAction::Abandon { .. }));
    }

    #[test]
    fn test_replan() {
        let mut spec = TaskSpec::new("orch", "Build auth", sample_steps());
        // Complete step 0
        spec.mark_running(0);
        spec.mark_passed(0, StepResult {
            output: "done".to_string(),
            artifacts: vec![],
            criteria_results: vec![],
            self_confidence: None,
            completed_at: Utc::now(),
        });

        // Replan remaining
        spec.replan(vec![
            Step {
                id: 0, // will be renumbered
                description: "New approach for tests".to_string(),
                agent: "coder".to_string(),
                depends_on: vec![],
                acceptance_criteria: vec![],
                status: StepStatus::Pending,
                result: None,
                retry_count: 0,
            },
        ]);

        assert_eq!(spec.replan_count, 1);
        assert_eq!(spec.steps.len(), 2); // 1 completed + 1 new
        assert_eq!(spec.steps[1].id, 1); // renumbered
        assert_eq!(spec.steps[1].description, "New approach for tests");
    }

    #[test]
    fn test_estimate_complexity() {
        assert_eq!(estimate_complexity("Fix this bug"), TaskComplexity::Simple);
        assert_eq!(estimate_complexity("First write the auth module, then add tests"), TaskComplexity::Medium);
        assert_eq!(
            estimate_complexity("Step 1: design the schema. Step 2: implement the API. Step 3: write tests. Finally deploy."),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_parse_planner_response() {
        let response = r#"```json
[
  {"description": "Write auth", "agent": "coder", "depends_on": [], "acceptance_criteria": [{"description": "Tests pass", "method": "sandbox"}]},
  {"description": "Deploy", "agent": "", "depends_on": [0], "acceptance_criteria": []}
]
```"#;
        let steps = parse_planner_response(response).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].description, "Write auth");
        assert_eq!(steps[0].agent, "coder");
        assert_eq!(steps[1].depends_on, vec![0]);
        assert!(matches!(steps[0].acceptance_criteria[0].method, VerificationMethod::Sandbox));
    }

    #[test]
    fn test_delegation_for_step() {
        let mut spec = TaskSpec::new("orch", "Build auth", sample_steps());
        spec.mark_running(0);
        spec.mark_passed(0, StepResult {
            output: "Auth done".to_string(),
            artifacts: vec![],
            criteria_results: vec![],
            self_confidence: None,
            completed_at: Utc::now(),
        });

        let envelope = spec.delegation_for_step(1).unwrap();
        assert!(envelope.task.contains("Write tests"));
        assert!(envelope.context.briefing.contains("step 2 of 3"));
        assert!(!envelope.context.task_chain.is_empty());
    }

    #[test]
    fn test_persistence() {
        let spec = TaskSpec::new("orch", "Build auth", sample_steps());
        let tmp = tempfile::TempDir::new().unwrap();
        let path = spec.save(tmp.path()).unwrap();
        assert!(path.exists());

        let loaded = TaskSpec::load(tmp.path(), &spec.task_id).unwrap();
        assert_eq!(loaded.task_id, spec.task_id);
        assert_eq!(loaded.steps.len(), 3);
    }
}
