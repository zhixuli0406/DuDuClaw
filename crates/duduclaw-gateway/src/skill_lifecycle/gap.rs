//! Skill gap detection — writes detected gaps to feedback.jsonl
//! for the evolution engine to pick up and generate new skills.

use std::io::Write;
use std::path::Path;

use chrono::Utc;
use tracing::warn;

use super::diagnostician::SkillGap;

/// Inject a skill gap signal into feedback.jsonl.
///
/// This signal is consumed by the Meso reflection's external factors
/// collector, feeding into the evolution engine's candidate_skills generation.
pub fn inject_skill_gap(gap: &SkillGap, home_dir: &Path, agent_id: &str) {
    let signal = serde_json::json!({
        "signal_type": "skill_gap",
        "agent_id": agent_id,
        "detail": format!(
            "Skill gap detected: '{}' — {}. Evidence: {}",
            gap.suggested_name,
            gap.suggested_description,
            gap.evidence.join("; ")
        ),
        "channel": "evolution",
        "timestamp": Utc::now().to_rfc3339(),
    });

    let feedback_path = home_dir.join("feedback.jsonl");
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&feedback_path)
    {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", signal);
        }
        Err(e) => {
            warn!("Failed to write skill gap to feedback.jsonl: {e}");
        }
    }
}
