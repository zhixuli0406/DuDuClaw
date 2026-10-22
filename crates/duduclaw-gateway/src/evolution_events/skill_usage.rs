//! Per-(employee, skill) activation counts read from the audit index.
//!
//! A `skill_activate` event is written when the evolution loop decides an
//! employee needs a skill and switches it on for the conversation. It is the
//! only per-skill usage signal the platform records, so "use count" in the
//! skill library means *activations*, not "the skill text was in a prompt"
//! (every loaded skill is in every prompt).
//!
//! One grouped query over the indexed `event_type` column, capped at
//! [`MAX_USAGE_ROWS`] groups, so the cost does not grow with how many events
//! each pair has.

use std::collections::HashMap;

use rusqlite::params;

use super::query::AuditEventIndex;

/// Upper bound on (employee, skill) pairs returned by one call.
pub const MAX_USAGE_ROWS: i64 = 20_000;

/// Activation count and most recent activation for one (employee, skill).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillUsage {
    pub count: u64,
    /// RFC 3339 timestamp of the latest activation.
    pub last_used_at: String,
}

/// `(agent_id, skill_id)` → usage.
pub type SkillUsageMap = HashMap<(String, String), SkillUsage>;

impl AuditEventIndex {
    /// Group every indexed `skill_activate` event by employee and skill.
    pub async fn skill_usage(&self) -> Result<SkillUsageMap, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, skill_id, COUNT(*), MAX(timestamp)
                 FROM audit_index
                 WHERE event_type = 'skill_activate' AND skill_id IS NOT NULL
                 GROUP BY agent_id, skill_id
                 LIMIT ?1",
            )
            .map_err(|e| format!("prepare skill usage query: {e}"))?;
        let rows = stmt
            .query_map(params![MAX_USAGE_ROWS], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("run skill usage query: {e}"))?;
        let mut out = SkillUsageMap::new();
        for row in rows {
            let (agent, skill, count, last) = row.map_err(|e| format!("read skill usage row: {e}"))?;
            out.insert(
                (agent, skill),
                SkillUsage { count: u64::try_from(count).unwrap_or(0), last_used_at: last },
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evolution_events::schema::{AuditEvent, AuditEventType, Outcome};

    fn line(agent: &str, skill: &str, ts: &str) -> String {
        let mut ev = AuditEvent::now(AuditEventType::SkillActivate, agent, Outcome::Success)
            .with_skill_id(skill)
            .with_trigger_signal("test");
        ev.timestamp = ts.to_string();
        serde_json::to_string(&ev).unwrap()
    }

    #[tokio::test]
    async fn groups_activations_by_employee_and_skill() {
        let home = tempfile::tempdir().unwrap();
        let events = home.path().join("events");
        std::fs::create_dir_all(&events).unwrap();
        let other = serde_json::to_string(
            &AuditEvent::now(AuditEventType::SkillDeactivate, "alice", Outcome::Success)
                .with_skill_id("pdf"),
        )
        .unwrap();
        let body = [
            line("alice", "pdf", "2026-10-01T00:00:00Z"),
            line("alice", "pdf", "2026-10-03T00:00:00Z"),
            line("bob", "pdf", "2026-10-02T00:00:00Z"),
            other,
        ]
        .join("\n");
        std::fs::write(events.join("2026-10-03.jsonl"), body).unwrap();
        let idx = AuditEventIndex::open_with_events_dir(home.path(), &events).unwrap();
        idx.sync_from_files().await.unwrap();
        let usage = idx.skill_usage().await.unwrap();
        let alice = &usage[&("alice".to_string(), "pdf".to_string())];
        assert_eq!(alice.count, 2);
        assert_eq!(alice.last_used_at, "2026-10-03T00:00:00Z");
        assert_eq!(usage[&("bob".to_string(), "pdf".to_string())].count, 1);
        assert_eq!(usage.len(), 2, "deactivations are not usage");
    }
}
