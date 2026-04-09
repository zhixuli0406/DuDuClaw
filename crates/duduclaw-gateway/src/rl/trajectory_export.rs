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
            .map(|(role, content)| {
                let (turn_role, is_agent) = match role.as_str() {
                    "assistant" => (TurnRole::AgentAction, true),
                    "tool" | "system" => (TurnRole::EnvironmentFeedback, false),
                    _ => (TurnRole::UserMessage, false),
                };

                let token_count = estimate_tokens(content);

                RLTurn {
                    role: turn_role,
                    content: content.clone(),
                    tool_calls: None,
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
                &session_id[..8.min(session_id.len())]
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

/// CJK-aware token estimation.
///
/// CJK characters (U+2E80+) count as ~1 token each.
/// ASCII/Latin characters count as ~0.25 tokens each (4 chars per token).
pub fn estimate_tokens(text: &str) -> u32 {
    let cjk_count = text.chars().filter(|&c| c as u32 > 0x2E80).count() as u32;
    let ascii_count = text.chars().filter(|&c| (c as u32) <= 0x2E80).count() as u32;
    cjk_count + (ascii_count + 3) / 4
}
