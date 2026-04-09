//! Trajectory Recorder — captures conversation trajectories for skill extraction.
//!
//! Records user messages, assistant replies, and tool usage during a session.
//! When finalized with a positive outcome, the trajectory can be fed to
//! `SkillExtractor::extract_heuristic()` for zero-cost skill extraction.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// User sentiment detected from message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sentiment {
    Positive,
    Negative,
}

/// Outcome of a recorded trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrajectoryOutcome {
    Success,
    Failure,
    Abandoned,
}

/// A single turn in a conversation trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryTurn {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub tools_used: Vec<String>,
}

/// A complete conversation trajectory for skill extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub session_key: String,
    pub agent_id: String,
    pub turns: Vec<TrajectoryTurn>,
    pub outcome: TrajectoryOutcome,
    pub sentiment: Option<Sentiment>,
    pub started_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

/// An extracted skill from a trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tools_used: Vec<String>,
    pub confidence: f64,
    pub source_session: String,
    pub extracted_at: DateTime<Utc>,
}

/// In-memory recording state for active trajectories.
#[derive(Debug, Clone)]
struct RecordingState {
    agent_id: String,
    turns: Vec<TrajectoryTurn>,
    started_at: DateTime<Utc>,
}

/// Records conversation trajectories and extracts skills.
#[derive(Debug, Default)]
pub struct TrajectoryRecorder {
    /// Active recordings keyed by session_key.
    active: HashMap<String, RecordingState>,
}

impl TrajectoryRecorder {
    pub fn new() -> Self {
        Self {
            active: HashMap::new(),
        }
    }

    /// Start recording a new trajectory for the given session.
    pub fn start(&mut self, session_key: &str, agent_id: &str) {
        self.active.insert(
            session_key.to_string(),
            RecordingState {
                agent_id: agent_id.to_string(),
                turns: Vec::new(),
                started_at: Utc::now(),
            },
        );
    }

    /// Check if a trajectory is currently being recorded for the session.
    pub fn is_recording(&self, session_key: &str) -> bool {
        self.active.contains_key(session_key)
    }

    /// Record a turn in the active trajectory.
    pub fn record_turn(
        &mut self,
        session_key: &str,
        role: &str,
        content: &str,
        tools_used: Vec<String>,
    ) {
        if let Some(state) = self.active.get_mut(session_key) {
            state.turns.push(TrajectoryTurn {
                role: role.to_string(),
                content: content.to_string(),
                timestamp: Utc::now(),
                tools_used,
            });
        }
    }

    /// Finalize the trajectory and return it for skill extraction.
    ///
    /// Returns `None` if no active recording exists or if the trajectory
    /// is too short (fewer than 2 turns) to extract a meaningful skill.
    pub fn finalize(
        &mut self,
        session_key: &str,
        outcome: TrajectoryOutcome,
        sentiment: Option<Sentiment>,
    ) -> Option<Trajectory> {
        let state = self.active.remove(session_key)?;

        // Need at least 2 turns (user + assistant) for meaningful extraction
        if state.turns.len() < 2 {
            return None;
        }

        Some(Trajectory {
            session_key: session_key.to_string(),
            agent_id: state.agent_id,
            turns: state.turns,
            outcome,
            sentiment,
            started_at: state.started_at,
            finalized_at: Some(Utc::now()),
        })
    }
}

/// Heuristic skill extractor (zero LLM cost).
///
/// Analyzes a finalized trajectory to determine if it represents a
/// reusable pattern worth saving as a skill.
pub struct SkillExtractor;

impl SkillExtractor {
    /// Bayesian confidence update (Beta-Bernoulli conjugate).
    ///
    /// Uses proper probability values: P(evidence|hypothesis) in [0, 1].
    pub fn bayesian_update(prior: f64, success: bool) -> f64 {
        let likelihood = if success { 0.9 } else { 0.1 };
        let marginal = prior * likelihood + (1.0 - prior) * (1.0 - likelihood);
        if marginal == 0.0 {
            return prior;
        }
        let posterior = (prior * likelihood) / marginal;
        posterior.clamp(0.01, 0.99)
    }

    /// Extract a skill from a trajectory using heuristics.
    ///
    /// Only extracts when:
    /// - Outcome is Success
    /// - Sentiment is Positive (or None with sufficient turn count)
    /// - At least 2 distinct tools were used (indicates a non-trivial workflow)
    /// - Confidence threshold met (based on trajectory quality signals)
    pub fn extract_heuristic(trajectory: &Trajectory) -> Option<ExtractedSkill> {
        // Only extract from successful trajectories
        if trajectory.outcome != TrajectoryOutcome::Success {
            return None;
        }

        // Negative sentiment = user was unhappy, skip
        if trajectory.sentiment == Some(Sentiment::Negative) {
            return None;
        }

        // Collect all tools used across turns
        let mut all_tools: Vec<String> = trajectory
            .turns
            .iter()
            .flat_map(|t| t.tools_used.clone())
            .collect();
        all_tools.sort();
        all_tools.dedup();

        // Need at least 2 distinct tools for a non-trivial workflow
        if all_tools.len() < 2 {
            return None;
        }

        // Calculate confidence based on trajectory quality signals
        let mut confidence: f64 = 0.5;

        // More turns = more established pattern
        if trajectory.turns.len() >= 4 {
            confidence += 0.1;
        }
        if trajectory.turns.len() >= 6 {
            confidence += 0.1;
        }

        // Positive sentiment boosts confidence
        if trajectory.sentiment == Some(Sentiment::Positive) {
            confidence += 0.2;
        }

        // More diverse tools = richer workflow
        if all_tools.len() >= 3 {
            confidence += 0.1;
        }

        // Cap at 1.0
        confidence = confidence.min(1.0);

        // Only extract if confidence is above threshold
        if confidence < 0.6 {
            return None;
        }

        // Generate a skill name from the first user message
        let skill_name = trajectory
            .turns
            .iter()
            .find(|t| t.role == "user")
            .map(|t| {
                let preview: String = t.content.chars().take(50).collect();
                format!("auto:{}", sanitize_skill_name(&preview))
            })
            .unwrap_or_else(|| format!("auto:skill-{}", &trajectory.session_key[..8.min(trajectory.session_key.len())]));

        // Generate description from assistant turns
        let description = trajectory
            .turns
            .iter()
            .filter(|t| t.role == "assistant")
            .map(|t| {
                let preview: String = t.content.chars().take(100).collect();
                preview
            })
            .next()
            .unwrap_or_default();

        let skill_id = format!(
            "extracted-{}-{}",
            trajectory.session_key,
            Utc::now().timestamp()
        );

        Some(ExtractedSkill {
            id: skill_id,
            name: skill_name,
            description,
            tools_used: all_tools,
            confidence,
            source_session: trajectory.session_key.clone(),
            extracted_at: Utc::now(),
        })
    }
}

/// Persistent skill bank for extracted skills.
#[derive(Debug, Default)]
pub struct SkillCache {
    skills: Vec<ExtractedSkill>,
}

impl SkillCache {
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
        }
    }

    /// Add a skill to the bank.
    pub fn add(&mut self, skill: ExtractedSkill) {
        self.skills.push(skill);
    }

    /// Get all stored skills.
    pub fn all(&self) -> &[ExtractedSkill] {
        &self.skills
    }

    /// Get skill count.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Record usage outcome and update confidence via Bayesian update.
    pub fn record_outcome(&mut self, skill_id: &str, success: bool) {
        if let Some(skill) = self.skills.iter_mut().find(|s| s.id == skill_id) {
            skill.confidence = SkillExtractor::bayesian_update(skill.confidence, success);
        }
    }

    /// Persist all cached skills to SQLite.
    pub fn save_to_sqlite(&self, db_path: &std::path::Path) -> std::result::Result<(), String> {
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| format!("Failed to open skill cache DB: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS skill_cache (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                tools_used_json TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5,
                source_session TEXT,
                extracted_at TEXT NOT NULL
            );"
        )
        .map_err(|e| format!("Failed to init schema: {e}"))?;

        for skill in &self.skills {
            conn.execute(
                "INSERT OR REPLACE INTO skill_cache \
                 (id, name, description, tools_used_json, confidence, source_session, extracted_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    skill.id,
                    skill.name,
                    skill.description,
                    serde_json::to_string(&skill.tools_used).unwrap_or_default(),
                    skill.confidence,
                    skill.source_session,
                    skill.extracted_at.to_rfc3339(),
                ],
            )
            .map_err(|e| format!("Failed to insert skill {}: {e}", skill.id))?;
        }
        Ok(())
    }

    /// Load skills from a SQLite database.
    pub fn load_from_sqlite(db_path: &std::path::Path) -> std::result::Result<Self, String> {
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| format!("Failed to open skill cache DB: {e}"))?;

        // Check if table exists before querying
        let table_exists: bool = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='skill_cache'",
                [],
                |row| row.get::<_, i32>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if !table_exists {
            return Ok(Self::new());
        }

        let mut stmt = conn
            .prepare(
                "SELECT id, name, description, tools_used_json, confidence, source_session, extracted_at \
                 FROM skill_cache ORDER BY confidence DESC",
            )
            .map_err(|e| format!("Query failed: {e}"))?;

        let skills: Vec<ExtractedSkill> = stmt
            .query_map([], |row| {
                Ok(ExtractedSkill {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    tools_used: serde_json::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or_default(),
                    confidence: row.get(4)?,
                    source_session: row.get::<_, Option<String>>(5)?
                        .unwrap_or_default(),
                    extracted_at: chrono::DateTime::parse_from_rfc3339(
                        &row.get::<_, String>(6)?,
                    )
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now()),
                })
            })
            .map_err(|e| format!("Read failed: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(Self { skills })
    }
}

/// Sanitize text into a valid skill name (lowercase, no special chars).
fn sanitize_skill_name(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
        .chars()
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recorder_lifecycle() {
        let mut recorder = TrajectoryRecorder::new();
        recorder.start("sess-1", "agent-a");
        assert!(recorder.is_recording("sess-1"));

        recorder.record_turn("sess-1", "user", "Help me refactor this code", vec![]);
        recorder.record_turn(
            "sess-1",
            "assistant",
            "I'll read the file first",
            vec!["Read".into(), "Edit".into()],
        );

        let trajectory = recorder
            .finalize("sess-1", TrajectoryOutcome::Success, Some(Sentiment::Positive))
            .unwrap();
        assert_eq!(trajectory.turns.len(), 2);
        assert_eq!(trajectory.outcome, TrajectoryOutcome::Success);
        assert!(!recorder.is_recording("sess-1"));
    }

    #[test]
    fn test_extract_heuristic_success() {
        let trajectory = Trajectory {
            session_key: "sess-1".into(),
            agent_id: "agent-a".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Refactor the auth module".into(),
                    timestamp: Utc::now(),
                    tools_used: vec![],
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "I'll refactor it now".into(),
                    timestamp: Utc::now(),
                    tools_used: vec!["Read".into(), "Edit".into()],
                },
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Thanks, that looks great!".into(),
                    timestamp: Utc::now(),
                    tools_used: vec![],
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "You're welcome!".into(),
                    timestamp: Utc::now(),
                    tools_used: vec!["Bash".into()],
                },
            ],
            outcome: TrajectoryOutcome::Success,
            sentiment: Some(Sentiment::Positive),
            started_at: Utc::now(),
            finalized_at: Some(Utc::now()),
        };

        let skill = SkillExtractor::extract_heuristic(&trajectory);
        assert!(skill.is_some());
        let skill = skill.unwrap();
        assert!(skill.confidence >= 0.6);
        assert!(skill.tools_used.contains(&"Read".to_string()));
    }

    #[test]
    fn test_extract_heuristic_negative_sentiment_skipped() {
        let trajectory = Trajectory {
            session_key: "sess-2".into(),
            agent_id: "agent-a".into(),
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Fix the bug".into(),
                    timestamp: Utc::now(),
                    tools_used: vec![],
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Done".into(),
                    timestamp: Utc::now(),
                    tools_used: vec!["Read".into(), "Edit".into()],
                },
            ],
            outcome: TrajectoryOutcome::Success,
            sentiment: Some(Sentiment::Negative),
            started_at: Utc::now(),
            finalized_at: Some(Utc::now()),
        };

        assert!(SkillExtractor::extract_heuristic(&trajectory).is_none());
    }

    #[test]
    fn test_skill_bank() {
        let mut bank = SkillCache::new();
        assert!(bank.is_empty());

        bank.add(ExtractedSkill {
            id: "test-1".into(),
            name: "auto:test-skill".into(),
            description: "A test skill".into(),
            tools_used: vec!["Read".into()],
            confidence: 0.8,
            source_session: "sess-1".into(),
            extracted_at: Utc::now(),
        });

        assert_eq!(bank.len(), 1);
        assert_eq!(bank.all()[0].name, "auto:test-skill");
    }

    #[test]
    fn test_sanitize_skill_name() {
        assert_eq!(sanitize_skill_name("Hello World!"), "hello-world");
        assert_eq!(sanitize_skill_name("fix: bug #123"), "fix-bug-123");
    }

    #[test]
    fn test_bayesian_update_success_increases() {
        let prior = 0.5;
        let post = SkillExtractor::bayesian_update(prior, true);
        assert!(post > prior);
    }

    #[test]
    fn test_bayesian_update_failure_decreases() {
        let prior = 0.5;
        let post = SkillExtractor::bayesian_update(prior, false);
        assert!(post < prior);
    }

    #[test]
    fn test_bayesian_update_bounded() {
        let high = SkillExtractor::bayesian_update(0.99, true);
        assert!(high <= 0.99);
        let low = SkillExtractor::bayesian_update(0.01, false);
        assert!(low >= 0.01);
    }

    #[test]
    fn test_record_outcome() {
        let mut cache = SkillCache::new();
        cache.add(ExtractedSkill {
            id: "s1".into(),
            name: "auto:test".into(),
            description: "test".into(),
            tools_used: vec!["Read".into()],
            confidence: 0.5,
            source_session: "sess-1".into(),
            extracted_at: Utc::now(),
        });

        cache.record_outcome("s1", true);
        assert!(cache.all()[0].confidence > 0.5);

        cache.record_outcome("s1", false);
        // After one success then one failure from 0.5 should be near 0.5
        let conf = cache.all()[0].confidence;
        assert!(conf > 0.01 && conf < 0.99);
    }

    #[test]
    fn test_sqlite_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut cache = SkillCache::new();
        cache.add(ExtractedSkill {
            id: "s1".into(),
            name: "auto:test-skill".into(),
            description: "A test skill".into(),
            tools_used: vec!["Read".into(), "Edit".into()],
            confidence: 0.75,
            source_session: "sess-1".into(),
            extracted_at: Utc::now(),
        });

        cache.save_to_sqlite(tmp.path()).unwrap();
        let loaded = SkillCache::load_from_sqlite(tmp.path()).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.all()[0].name, "auto:test-skill");
        assert!((loaded.all()[0].confidence - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_load_from_sqlite_empty_db() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // No table created yet — should return empty cache
        let loaded = SkillCache::load_from_sqlite(tmp.path()).unwrap();
        assert!(loaded.is_empty());
    }
}
