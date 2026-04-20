//! Lightweight RL trajectory collector wired into the agent execution pipeline.
//!
//! Runs asynchronously after each interaction to avoid adding latency to the
//! hot path. Reads session messages from `SessionManager`, builds an
//! `RLTrajectory`, computes composite reward, and appends to a single
//! `~/.duduclaw/rl_trajectories.jsonl` file (one JSON object per line).

use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use super::reward::{CompositeReward, RewardComputer};
use super::trajectory_export::TrajectoryExporter;
use super::types::RLTrajectory;

/// Default path for the global RL trajectories JSONL file.
const RL_TRAJECTORIES_FILE: &str = "rl_trajectories.jsonl";

/// Collect a trajectory from the current session and append it to the global
/// JSONL file. Designed to be called from `tokio::spawn` so it does not block
/// the reply path.
///
/// `outcome_reward` is a coarse signal: 1.0 for successful reply, 0.0 for
/// fallback/error. The `CompositeReward` adds tool efficiency and overlong
/// penalties on top.
pub async fn collect_trajectory(
    home_dir: PathBuf,
    session_id: String,
    agent_id: String,
    model_id: String,
    messages: Vec<(String, String)>,
    outcome_reward: f64,
) {
    // Build trajectory from session messages
    let trajectory = TrajectoryExporter::build_trajectory(
        &session_id,
        &agent_id,
        &model_id,
        &messages,
        outcome_reward,
    );

    // Compute composite reward (outcome + tool efficiency + overlong penalty)
    let composite = CompositeReward::default_config();
    let final_reward = composite.compute(&trajectory);

    // Create the trajectory with computed reward
    let trajectory = RLTrajectory {
        outcome_reward: final_reward,
        ..trajectory
    };

    // Append to global JSONL file (atomic: serialize then single write)
    let jsonl_path = home_dir.join(RL_TRAJECTORIES_FILE);
    if let Err(e) = append_trajectory_jsonl(&jsonl_path, &trajectory) {
        warn!(
            session_id = %session_id,
            error = %e,
            "Failed to write RL trajectory"
        );
        return;
    }

    // Also write to per-agent export directory for `duduclaw rl export`
    let export_dir = home_dir.join("rl_trajectories");
    let exporter = TrajectoryExporter::new(export_dir);
    if let Err(e) = exporter.write_trajectory(&trajectory) {
        warn!(
            session_id = %session_id,
            error = %e,
            "Failed to write per-agent RL trajectory"
        );
    }

    info!(
        trajectory_id = %trajectory.trajectory_id,
        agent = %agent_id,
        turns = trajectory.turns.len(),
        tokens = trajectory.total_tokens,
        reward = format!("{:.3}", trajectory.outcome_reward),
        "RL trajectory collected"
    );
}

/// Append a single trajectory as one JSON line to a JSONL file.
fn append_trajectory_jsonl(path: &Path, trajectory: &RLTrajectory) -> std::io::Result<()> {
    let json = serde_json::to_string(trajectory)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", json)?;
    Ok(())
}

/// Read all trajectories from the global JSONL file.
pub fn read_trajectories(home_dir: &Path) -> std::io::Result<Vec<RLTrajectory>> {
    let path = home_dir.join(RL_TRAJECTORIES_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let mut trajectories = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<RLTrajectory>(line) {
            Ok(traj) => trajectories.push(traj),
            Err(e) => {
                debug!(line = i + 1, error = %e, "Skipping malformed trajectory line");
            }
        }
    }
    Ok(trajectories)
}

/// Compute basic statistics from collected trajectories.
pub struct TrajectoryStats {
    pub total_count: usize,
    pub total_tokens: u64,
    pub avg_reward: f64,
    pub avg_turns: f64,
    pub avg_tokens: f64,
    pub agent_counts: std::collections::HashMap<String, usize>,
}

impl TrajectoryStats {
    pub fn from_trajectories(trajectories: &[RLTrajectory]) -> Self {
        let total_count = trajectories.len();
        if total_count == 0 {
            return Self {
                total_count: 0,
                total_tokens: 0,
                avg_reward: 0.0,
                avg_turns: 0.0,
                avg_tokens: 0.0,
                agent_counts: std::collections::HashMap::new(),
            };
        }

        let total_tokens: u64 = trajectories.iter().map(|t| t.total_tokens).sum();
        let total_reward: f64 = trajectories.iter().map(|t| t.outcome_reward).sum();
        let total_turns: usize = trajectories.iter().map(|t| t.turns.len()).sum();

        let mut agent_counts = std::collections::HashMap::new();
        for t in trajectories {
            *agent_counts.entry(t.agent_id.clone()).or_insert(0) += 1;
        }

        Self {
            total_count,
            total_tokens,
            avg_reward: total_reward / total_count as f64,
            avg_turns: total_turns as f64 / total_count as f64,
            avg_tokens: total_tokens as f64 / total_count as f64,
            agent_counts,
        }
    }

    /// Filter trajectories by agent ID, then compute stats.
    pub fn for_agent(trajectories: &[RLTrajectory], agent_id: &str) -> Self {
        let filtered: Vec<_> = trajectories
            .iter()
            .filter(|t| t.agent_id == agent_id)
            .cloned()
            .collect();
        Self::from_trajectories(&filtered)
    }
}

/// Compute reward for a single trajectory file (JSONL with one entry).
pub fn compute_reward_for_file(path: &Path) -> std::io::Result<Vec<(String, f64)>> {
    let content = std::fs::read_to_string(path)?;
    let composite = CompositeReward::default_config();
    let mut results = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let traj: RLTrajectory = serde_json::from_str(line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let reward = composite.compute(&traj);
        results.push((traj.trajectory_id, reward));
    }

    Ok(results)
}
