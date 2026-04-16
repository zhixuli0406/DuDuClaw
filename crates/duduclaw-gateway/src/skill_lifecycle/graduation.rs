//! Skill graduation — promotes effective agent-local skills to global scope.
//!
//! When a skill's SkillLiftTracker shows sustained positive lift (mature + stable),
//! it is "graduated" to `~/.duduclaw/skills/` where all agents can access it.
//!
//! Reference: AgentGym / AgentEvol (ACL 2025), ADAS (ICLR 2025)

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::lift::SkillLiftTracker;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Criteria for skill graduation.
#[derive(Debug, Clone)]
pub struct GraduationCriteria {
    /// Minimum lift (error reduction) required.
    pub min_lift: f64,
    /// Minimum total loads required.
    pub min_load_count: u64,
    /// Whether stability is required.
    pub require_stable: bool,
}

impl Default for GraduationCriteria {
    fn default() -> Self {
        Self {
            min_lift: 0.1,
            min_load_count: 50,
            require_stable: true,
        }
    }
}

/// A skill that meets graduation criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraduationCandidate {
    pub skill_name: String,
    pub source_agent_id: String,
    pub lift: f64,
    pub load_count: u64,
    pub is_stable: bool,
    pub first_activated: DateTime<Utc>,
}

/// Record of a graduation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraduationRecord {
    pub skill_name: String,
    pub source_agent: String,
    pub graduated_at: DateTime<Utc>,
    pub lift: f64,
    pub load_count: u64,
}

// ---------------------------------------------------------------------------
// Graduation logic
// ---------------------------------------------------------------------------

/// Check if a skill tracker meets graduation criteria.
pub fn check_graduation(
    tracker: &SkillLiftTracker,
    criteria: &GraduationCriteria,
) -> Option<GraduationCandidate> {
    if !tracker.is_mature() {
        return None;
    }

    let lift = tracker.lift();
    if lift < criteria.min_lift {
        return None;
    }

    if criteria.require_stable && !tracker.is_stable() {
        return None;
    }

    Some(GraduationCandidate {
        skill_name: tracker.skill_name.clone(),
        source_agent_id: tracker.agent_id.clone(),
        lift,
        load_count: tracker.load_count,
        is_stable: tracker.is_stable(),
        first_activated: tracker.first_activated,
    })
}

/// Graduate a skill to global scope.
///
/// Reads the skill from the agent's SKILLS/ directory, adds graduation
/// metadata to frontmatter, and writes to `~/.duduclaw/skills/`.
pub async fn graduate_to_global(
    candidate: &GraduationCandidate,
    agent_skills_dir: &Path,
    global_skills_dir: &Path,
) -> Result<GraduationRecord, String> {
    let filename = format!("{}.md", candidate.skill_name);
    let source = agent_skills_dir.join(&filename);

    let content = tokio::fs::read_to_string(&source)
        .await
        .map_err(|e| format!("Failed to read skill {}: {e}", source.display()))?;

    // Add graduation metadata to frontmatter
    let graduated_content = inject_graduation_metadata(
        &content,
        &candidate.source_agent_id,
        candidate.lift,
    );

    // Write to global skills directory
    tokio::fs::create_dir_all(global_skills_dir)
        .await
        .map_err(|e| format!("Failed to create global skills dir: {e}"))?;

    let dest = global_skills_dir.join(&filename);
    tokio::fs::write(&dest, &graduated_content)
        .await
        .map_err(|e| format!("Failed to write global skill: {e}"))?;

    let record = GraduationRecord {
        skill_name: candidate.skill_name.clone(),
        source_agent: candidate.source_agent_id.clone(),
        graduated_at: Utc::now(),
        lift: candidate.lift,
        load_count: candidate.load_count,
    };

    info!(
        skill = %candidate.skill_name,
        source = %candidate.source_agent_id,
        lift = %format!("{:.1}%", candidate.lift * 100.0),
        dest = %dest.display(),
        "Skill graduated to global scope"
    );

    Ok(record)
}

/// Append a graduation record to the log file.
///
/// **Blocking I/O**: This function uses `std::fs` synchronous I/O.
/// When calling from an async context, wrap in `tokio::task::spawn_blocking`.
pub fn append_graduation_log(record: &GraduationRecord, home_dir: &Path) {
    let log_path = home_dir.join("graduation_log.jsonl");
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(mut f) => {
            if let Ok(json) = serde_json::to_string(record) {
                let _ = writeln!(f, "{json}");
            }
        }
        Err(e) => {
            warn!("Failed to write graduation log: {e}");
        }
    }
}

/// Load graduation records from log file.
///
/// **Blocking I/O**: This function uses `std::fs` synchronous I/O.
/// When calling from an async context, wrap in `tokio::task::spawn_blocking`.
pub fn load_graduation_log(home_dir: &Path) -> Vec<GraduationRecord> {
    let log_path = home_dir.join("graduation_log.jsonl");
    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Sanitize a string for safe use as a YAML scalar value.
/// Strips characters that could break YAML structure (newlines, colons at line start).
fn sanitize_yaml_scalar(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
        .take(64)
        .collect()
}

/// Inject graduation metadata into skill frontmatter.
fn inject_graduation_metadata(content: &str, source_agent: &str, lift: f64) -> String {
    let safe_agent = sanitize_yaml_scalar(source_agent);
    let trimmed = content.trim();

    if !trimmed.starts_with("---") {
        // No frontmatter — wrap content with new frontmatter
        return format!(
            "---\ngraduated_from: \"{safe_agent}\"\ngraduated_at: \"{}\"\nvalidated_lift: {lift:.3}\n---\n\n{content}",
            Utc::now().to_rfc3339()
        );
    }

    // Find closing ---
    let after_first = &trimmed[3..].trim_start_matches(['\r', '\n']);
    if let Some(end) = after_first.find("\n---") {
        let existing_yaml = &after_first[..end];
        let body = &after_first[end + 4..];

        format!(
            "---\n{existing_yaml}\ngraduated_from: \"{safe_agent}\"\ngraduated_at: \"{}\"\nvalidated_lift: {lift:.3}\n---{body}",
            Utc::now().to_rfc3339()
        )
    } else {
        // Malformed frontmatter — return as-is
        content.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_tracker(skill: &str, lift: f64, load_count: u64, stable: bool) -> SkillLiftTracker {
        let mut tracker = SkillLiftTracker::new(skill.to_string(), "agent-a".to_string());
        let base_error = 0.5;
        for _ in 0..(load_count.max(20)) {
            tracker.record_with(base_error - lift);
            tracker.record_without(base_error);
        }
        tracker.load_count = load_count;
        tracker
    }

    #[test]
    fn test_meets_criteria() {
        let tracker = make_tracker("test-skill", 0.15, 60, true);
        let criteria = GraduationCriteria::default();
        let candidate = check_graduation(&tracker, &criteria);
        assert!(candidate.is_some());
        let c = candidate.unwrap();
        assert_eq!(c.skill_name, "test-skill");
        assert!(c.lift >= 0.1);
    }

    #[test]
    fn test_below_lift() {
        let tracker = make_tracker("test-skill", 0.05, 60, true);
        let criteria = GraduationCriteria::default();
        assert!(check_graduation(&tracker, &criteria).is_none());
    }

    #[test]
    fn test_not_mature() {
        let tracker = make_tracker("test-skill", 0.15, 5, true);
        let criteria = GraduationCriteria::default();
        // load_count too low for is_mature()
        assert!(check_graduation(&tracker, &criteria).is_none());
    }

    #[test]
    fn test_inject_graduation_metadata() {
        let content = "---\nname: test-skill\ndescription: test\n---\n\n# Content";
        let result = inject_graduation_metadata(content, "agent-a", 0.15);
        assert!(result.contains("graduated_from: \"agent-a\""));
        assert!(result.contains("validated_lift: 0.150"));
        assert!(result.contains("name: test-skill"));
    }

    #[test]
    fn test_inject_no_frontmatter() {
        let content = "# Plain skill\n\nSome content";
        let result = inject_graduation_metadata(content, "agent-b", 0.2);
        assert!(result.contains("graduated_from: \"agent-b\""));
        assert!(result.contains("# Plain skill"));
    }

    #[test]
    fn test_inject_sanitizes_agent_id() {
        let content = "---\nname: test\n---\n\nBody";
        // Attempt YAML injection via newline in agent_id
        let result = inject_graduation_metadata(content, "evil\nmalicious: true", 0.1);
        // Newline should be stripped by sanitize_yaml_scalar
        assert!(!result.contains("malicious: true"));
        assert!(result.contains("graduated_from: \"evilmalicious true\""));
    }

    #[test]
    fn test_graduation_log_roundtrip() {
        let dir = std::env::temp_dir().join("duduclaw_test_grad");
        let _ = std::fs::create_dir_all(&dir);

        let record = GraduationRecord {
            skill_name: "test-skill".to_string(),
            source_agent: "agent-a".to_string(),
            graduated_at: Utc::now(),
            lift: 0.15,
            load_count: 60,
        };

        // Clean up first
        let _ = std::fs::remove_file(dir.join("graduation_log.jsonl"));

        append_graduation_log(&record, &dir);
        let records = load_graduation_log(&dir);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].skill_name, "test-skill");

        // Clean up
        let _ = std::fs::remove_file(dir.join("graduation_log.jsonl"));
    }
}
