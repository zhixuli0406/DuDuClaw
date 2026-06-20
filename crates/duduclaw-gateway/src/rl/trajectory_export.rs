//! Exports agent sessions as RL training trajectories.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::info;

use super::types::*;

/// Exports sessions as RL trajectories in JSONL format.
pub struct TrajectoryExporter {
    export_dir: PathBuf,
}

impl TrajectoryExporter {
    pub fn new(export_dir: PathBuf) -> Self {
        Self { export_dir }
    }

    /// Convert raw session messages into an RLTrajectory.
    ///
    /// Messages are classified by role:
    /// - "assistant" -> AgentAction (is_agent_generated = true)
    /// - "tool" / "system" -> EnvironmentFeedback (is_agent_generated = false)
    /// - "user" -> UserMessage (is_agent_generated = false)
    pub fn build_trajectory(
        session_id: &str,
        agent_id: &str,
        model_id: &str,
        messages: &[(String, String)], // (role, content) pairs
        outcome_reward: f64,
    ) -> RLTrajectory {
        let turns: Vec<RLTurn> = messages
            .iter()
            .enumerate()
            .map(|(i, (role, content))| {
                let (turn_role, is_agent) = match role.as_str() {
                    "assistant" => (TurnRole::AgentAction, true),
                    "tool" | "system" => (TurnRole::EnvironmentFeedback, false),
                    _ => (TurnRole::UserMessage, false),
                };

                let token_count = estimate_tokens(content);

                // D3 fix: populate `tool_calls` for agent turns. Previously this
                // was hard-coded to `None`, so `total_tool_calls()` was always 0
                // and `ToolEfficiencyReward` produced a constant 0.0 — flattening
                // the composite reward to a near-constant ~0.7 with no learning
                // signal. We extract tool calls from the agent turn's content
                // (the next "tool" message, if any, supplies the result/outcome).
                let tool_calls = if is_agent {
                    let next = messages.get(i + 1).and_then(|(r, c)| {
                        if r == "tool" {
                            Some(c.as_str())
                        } else {
                            None
                        }
                    });
                    extract_tool_calls(content, next)
                } else {
                    None
                };

                RLTurn {
                    role: turn_role,
                    content: content.clone(),
                    tool_calls,
                    token_count,
                    is_agent_generated: is_agent,
                }
            })
            .collect();

        let total_tokens = turns.iter().map(|t| t.token_count as u64).sum();

        RLTrajectory {
            trajectory_id: format!(
                "traj_{}_{}",
                Utc::now().format("%Y%m%d%H%M%S"),
                // L8 fix: `&session_id[..8]` is a *byte* slice and panics when
                // byte 8 lands inside a multi-byte UTF-8 char. Take 8 chars
                // safely instead.
                session_id.chars().take(8).collect::<String>()
            ),
            agent_id: agent_id.to_string(),
            model_id: model_id.to_string(),
            turns,
            total_tokens,
            outcome_reward,
            metadata: std::collections::HashMap::new(),
            created_at: Utc::now(),
        }
    }

    /// Write a trajectory to a JSONL file.
    pub fn write_trajectory(&self, trajectory: &RLTrajectory) -> std::io::Result<PathBuf> {
        let dir = self
            .export_dir
            .join(&trajectory.agent_id)
            .join(trajectory.created_at.format("%Y-%m-%d").to_string());
        std::fs::create_dir_all(&dir)?;

        let path = dir.join(format!("{}.jsonl", trajectory.trajectory_id));
        let json = serde_json::to_string(trajectory)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&path, format!("{}\n", json))?;

        info!(path = %path.display(), tokens = trajectory.total_tokens, "Exported RL trajectory");
        Ok(path)
    }

    /// Get statistics for exported trajectories.
    pub fn stats(&self, agent_id: &str) -> ExportStats {
        let dir = self.export_dir.join(agent_id);
        let mut stats = ExportStats::default();

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Ok(files) = std::fs::read_dir(entry.path()) {
                        for file in files.flatten() {
                            if file
                                .path()
                                .extension()
                                .map(|e| e == "jsonl")
                                .unwrap_or(false)
                            {
                                stats.trajectory_count += 1;
                            }
                        }
                    }
                }
            }
        }
        stats
    }

    /// Get the export directory path.
    pub fn export_dir(&self) -> &Path {
        &self.export_dir
    }
}

/// Statistics about exported trajectories.
#[derive(Debug, Default)]
pub struct ExportStats {
    pub trajectory_count: usize,
}

/// Extract tool calls from an agent turn's `content`, using the following
/// `tool` message (if any) as the result/success signal.
///
/// DuDuClaw agent turns carry tool use in a few shapes depending on the runtime
/// (Claude stream-json, CLI text, etc.). We try, in order:
///   1. Parse `content` as JSON and collect any `{"type":"tool_use", ...}` blocks
///      (top-level value or an array/`content` array of blocks).
///   2. Fall back to a single synthetic record when the next message is a `tool`
///      result — the strongest signal that exactly one tool call occurred.
///
/// Returns `None` when no tool call can be inferred, so non-tool turns stay
/// `tool_calls: None` (preserving loss-masking semantics).
fn extract_tool_calls(content: &str, next_tool_result: Option<&str>) -> Option<Vec<ToolCallRecord>> {
    // Success heuristic: a tool result that does not look like an error.
    let result_value = next_tool_result.map(|r| serde_json::json!(r));
    let success = next_tool_result.map_or(true, |r| !looks_like_error(r));

    // 1. Structured tool_use blocks embedded in the agent content.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        let mut records = Vec::new();
        collect_tool_use_blocks(&value, &result_value, success, &mut records);
        if !records.is_empty() {
            return Some(records);
        }
    }

    // 2. Adjacency fallback: an agent turn immediately followed by a tool result
    //    implies (at least) one tool call even when the content is opaque text.
    if next_tool_result.is_some() {
        return Some(vec![ToolCallRecord {
            name: "unknown".to_string(),
            arguments: serde_json::Value::Null,
            result: result_value.unwrap_or(serde_json::Value::Null),
            success,
        }]);
    }

    None
}

/// Recursively collect `tool_use` blocks from a JSON value (object, array, or a
/// Claude-style `{"content": [ ... ]}` envelope).
fn collect_tool_use_blocks(
    value: &serde_json::Value,
    result_value: &Option<serde_json::Value>,
    success: bool,
    out: &mut Vec<ToolCallRecord>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            let is_tool_use = obj.get("type").and_then(|t| t.as_str()) == Some("tool_use");
            if is_tool_use {
                let name = obj
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let arguments = obj
                    .get("input")
                    .or_else(|| obj.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                out.push(ToolCallRecord {
                    name,
                    arguments,
                    result: result_value.clone().unwrap_or(serde_json::Value::Null),
                    success,
                });
            }
            // Descend into a `content` block array (Claude message envelope).
            if let Some(serde_json::Value::Array(arr)) = obj.get("content") {
                for v in arr {
                    collect_tool_use_blocks(v, result_value, success, out);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_tool_use_blocks(v, result_value, success, out);
            }
        }
        _ => {}
    }
}

/// Cheap heuristic for whether a tool result string indicates failure.
fn looks_like_error(result: &str) -> bool {
    let lower = result.to_ascii_lowercase();
    lower.contains("\"error\"")
        || lower.contains("error:")
        || lower.contains("exception")
        || lower.contains("failed")
        || lower.contains("traceback")
}

/// CJK-aware token estimation.
///
/// CJK characters (U+2E80+) count as ~1 token each.
/// ASCII/Latin characters count as ~0.25 tokens each (4 chars per token).
pub fn estimate_tokens(text: &str) -> u32 {
    let cjk_count = text.chars().filter(|&c| c as u32 > 0x2E80).count() as u32;
    let ascii_count = text.chars().filter(|&c| (c as u32) <= 0x2E80).count() as u32;
    cjk_count + (ascii_count + 3) / 4
}
