//! H12c: protected (isolated) `skill_synthesis_run` of one employee.
//!
//! The provider call is the existing pipeline's (the employee's own utility
//! runtime/model, budget gate and autonomy gate, keyed by its own agent id);
//! events are parsed by E (`ReadEvolutionEvents`), the model output is
//! prepared by E (`PrepareSynthesizedSkill` + `ReadSkillIdentity`), and this
//! module adds the own-employee event filter and the E-scanned, no-clobber
//! publication into `agents/<own label>/SKILLS` instead of the global bank.
use std::path::Path;
use super::quality_scorer::TrajectoryEvent;

/// Keep only `owner`'s own events: an owned run never scores, prompts with or
/// reads episodic memory of another employee's trajectories.
pub fn own_events(events: Vec<TrajectoryEvent>, owner: &str) -> Vec<TrajectoryEvent> {
    events.into_iter().filter(|event| event.agent_id == owner).collect()
}

/// Publish an E-prepared skill into the owner's own SKILLS. Blocking: run it on
/// the blocking pool under the caller's captured authority. The bytes are
/// scanned by E once more (`ScanSkill`) right before the no-clobber write; an
/// existing skill is never overwritten. The proposal receipt keeps the
/// existing idempotence (`skill-proposals/<key>.published`).
pub fn publish(home: &Path, owner: &str, name: &str, markdown: &str, proposal_key: &str) -> Result<(), String> {
    if !duduclaw_core::is_valid_agent_id(owner) || duduclaw_core::is_reserved_agent_id(owner) {
        return Err("owned synthesis has no valid employee".into());
    }
    if name.is_empty() || name.len() > 50 || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("isolated skill returned an invalid publication name".into());
    }
    if markdown.is_empty() || markdown.len() > 64 * 1024 { return Err("synthesized skill exceeds the publication cap".into()); }
    if proposal_key.is_empty() || !proposal_key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid synthesis proposal key".into());
    }
    let (passed, risk, _) = duduclaw_parse_worker::adapters::scan_skill_bytes(markdown.as_bytes())
        .map_err(|_| "isolated security scan of the synthesized skill failed".to_string())?;
    if !passed || matches!(risk.as_str(), "High" | "Critical") {
        return Err(format!("synthesized skill '{name}' did not pass the isolated security scan"));
    }
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| "isolated authority ended before publication".to_string())?;
    crate::isolation_worker_plans::write_host_file(home, &["agents", owner, "SKILLS"], &format!("{name}.md"), markdown.as_bytes())
        .map_err(|_| "own skill publication unconfirmed; an existing skill is never overwritten".to_string())?;
    let receipt = serde_json::to_vec(&serde_json::json!({"skill":name,"scope":format!("agent:{owner}")}))
        .map_err(|error| error.to_string())?;
    crate::isolation_worker_plans::write_host_file(home, &["skill-proposals"], &format!("{proposal_key}.published"), &receipt)
        .map_err(|_| "skill published; receipt unconfirmed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(agent: &str) -> TrajectoryEvent {
        TrajectoryEvent { timestamp: chrono::Utc::now(), agent_id: agent.into(), effectiveness_score_delta: 0.5,
            task_complexity: 0.5, is_success: true, skill_id: None }
    }

    #[test]
    fn h12c_owned_synthesis_scores_only_own_events() {
        let events = vec![event("main"), event("other"), event("main"), event("main-2")];
        let own = own_events(events, "main");
        assert_eq!(own.len(), 2);
        assert!(own.iter().all(|event| event.agent_id == "main"));
    }

    #[tokio::test]
    async fn h12c_owned_run_refused_for_another_employee() {
        use super::super::pipeline::{run, PipelineConfig};
        let home = tempfile::tempdir().unwrap();
        let config = PipelineConfig { home_dir: home.path().to_path_buf(), events_dir: home.path().join("events"),
            target_agent_id: "main".into(), owned_by: Some("other".into()), dry_run: false,
            api_key: Some("must-not-be-used".into()), ..Default::default() };
        let result = run(&config).await;
        assert!(result.errors.iter().any(|error| error.contains("own employee")), "{:?}", result.errors);
        assert_eq!(result.skills_graduated, 0);
        assert!(!home.path().join("agents").exists() && !home.path().join("skills").exists());
    }

    #[test]
    fn h12c_owned_publication_refuses_bad_subject_before_any_write() {
        let home = tempfile::tempdir().unwrap();
        let md = "---\nname: notes\n---\nbody";
        for (owner, name, key) in [("", "notes", "ab"), ("../x", "notes", "ab"), ("main", "../notes", "ab"),
            ("main", "Notes", "ab"), ("main", "notes", "../k"), ("main", "notes", "")] {
            assert!(publish(home.path(), owner, name, md, key).is_err(), "{owner} {name} {key}");
        }
        assert!(publish(home.path(), "main", "notes", "", "ab").is_err());
        assert!(!home.path().join("agents").exists());
        assert!(!home.path().join("skills").exists(), "never the global bank");
    }
}
