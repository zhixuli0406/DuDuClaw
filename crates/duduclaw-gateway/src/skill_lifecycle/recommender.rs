//! Skill recommender — suggests graduated global skills for new agents.
//!
//! When an agent has few or no skills, the recommender analyzes the global
//! graduated skills and ranks them by relevance to the agent's SOUL.md.
//!
//! Reference: AgentGym / AgentEvol (ACL 2025)

use super::compression::CompressedSkill;
use super::graduation::GraduationRecord;
use super::relevance;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A skill recommendation for an agent.
#[derive(Debug, Clone)]
pub struct SkillRecommendation {
    /// Name of the recommended skill.
    pub skill_name: String,
    /// Which agent originally validated this skill.
    pub source_agent: String,
    /// Measured lift in the source agent.
    pub lift_in_source: f64,
    /// Relevance to this agent's SOUL.md (keyword overlap).
    pub relevance_score: f64,
    /// Combined ranking score.
    pub combined_score: f64,
}

// ---------------------------------------------------------------------------
// Recommendation engine
// ---------------------------------------------------------------------------

/// Weight for lift in combined score (proven effectiveness).
const LIFT_WEIGHT: f64 = 0.6;
/// Weight for relevance in combined score (topic match).
const RELEVANCE_WEIGHT: f64 = 0.4;

/// Generate skill recommendations for an agent.
///
/// Ranks global graduated skills by a combination of their proven lift
/// (from the source agent) and relevance to the target agent's SOUL.md.
pub fn recommend_for_agent(
    agent_soul: &str,
    global_skills: &[CompressedSkill],
    graduation_log: &[GraduationRecord],
) -> Vec<SkillRecommendation> {
    if global_skills.is_empty() || graduation_log.is_empty() {
        return Vec::new();
    }

    let ranked = relevance::rank_skills(agent_soul, global_skills);

    let mut recommendations: Vec<SkillRecommendation> = ranked
        .iter()
        .filter_map(|(idx, relevance_score)| {
            let skill = &global_skills[*idx];

            // Find graduation record for this skill
            let grad_record = graduation_log
                .iter()
                .filter(|r| r.skill_name == skill.name)
                .max_by(|a, b| a.lift.partial_cmp(&b.lift).unwrap_or(std::cmp::Ordering::Equal))?;

            let combined = LIFT_WEIGHT * grad_record.lift.min(1.0)
                + RELEVANCE_WEIGHT * relevance_score;

            Some(SkillRecommendation {
                skill_name: skill.name.clone(),
                source_agent: grad_record.source_agent.clone(),
                lift_in_source: grad_record.lift,
                relevance_score: *relevance_score,
                combined_score: combined,
            })
        })
        .collect();

    // Sort by combined score descending
    recommendations.sort_by(|a, b| {
        b.combined_score
            .partial_cmp(&a.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Return top 5
    recommendations.truncate(5);
    recommendations
}

/// Filter recommendations above a threshold for auto-activation.
pub fn filter_for_auto_activation(
    recommendations: &[SkillRecommendation],
    threshold: f64,
) -> Vec<&SkillRecommendation> {
    recommendations
        .iter()
        .filter(|r| r.combined_score >= threshold)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_skill(name: &str, content: &str) -> CompressedSkill {
        CompressedSkill::compress(name, content, None)
    }

    fn make_grad(name: &str, lift: f64) -> GraduationRecord {
        GraduationRecord {
            skill_name: name.to_string(),
            source_agent: "agent-a".to_string(),
            graduated_at: Utc::now(),
            lift,
            load_count: 60,
        }
    }

    #[test]
    fn test_recommend_relevant_skill() {
        let soul = "# Restaurant Bot\n\nHandles menu, orders, and customer complaints about food quality.";
        let skills = vec![
            make_skill("complaint-handler", "Guide for handling customer complaints about food quality and service issues"),
            make_skill("shipping-tracker", "Track shipping packages and delivery status for e-commerce"),
        ];
        let log = vec![
            make_grad("complaint-handler", 0.15),
            make_grad("shipping-tracker", 0.12),
        ];

        let recs = recommend_for_agent(soul, &skills, &log);
        assert!(!recs.is_empty());
        // complaint-handler should rank higher due to relevance
        assert_eq!(recs[0].skill_name, "complaint-handler");
    }

    #[test]
    fn test_empty_inputs() {
        let recs = recommend_for_agent("soul", &[], &[]);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_filter_threshold() {
        let recs = vec![
            SkillRecommendation {
                skill_name: "high".to_string(),
                source_agent: "a".to_string(),
                lift_in_source: 0.2,
                relevance_score: 0.5,
                combined_score: 0.35,
            },
            SkillRecommendation {
                skill_name: "low".to_string(),
                source_agent: "b".to_string(),
                lift_in_source: 0.05,
                relevance_score: 0.1,
                combined_score: 0.07,
            },
        ];

        let filtered = filter_for_auto_activation(&recs, 0.3);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].skill_name, "high");
    }

    #[test]
    fn test_max_5_recommendations() {
        let soul = "general purpose bot with many features";
        let skills: Vec<CompressedSkill> = (0..10)
            .map(|i| make_skill(&format!("skill-{i}"), &format!("handles general feature {i} purpose")))
            .collect();
        let log: Vec<GraduationRecord> = (0..10)
            .map(|i| make_grad(&format!("skill-{i}"), 0.1 + i as f64 * 0.01))
            .collect();

        let recs = recommend_for_agent(soul, &skills, &log);
        assert!(recs.len() <= 5);
    }
}
