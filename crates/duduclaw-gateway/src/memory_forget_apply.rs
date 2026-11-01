//! Shared apply and idempotent follow-up steps for CLI and disconnect jobs.

use std::path::Path;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_memory::{ApplyOutcome, ForgetPlan, SqliteMemoryEngine};

pub fn enabled(home: &Path) -> bool {
    let path = home.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => return e.kind() == std::io::ErrorKind::NotFound,
    };
    let Ok(table) = text.parse::<toml::Table>() else { return false; };
    match table.get("memory") {
        None => true,
        Some(section) => match section.as_table() {
            None => false,
            Some(memory) => memory.get("forget_source").map_or(true, |v| v.as_bool() == Some(true)),
        },
    }
}

pub async fn apply(
    home: &Path,
    engine: &SqliteMemoryEngine,
    plan: &ForgetPlan,
) -> Result<(ApplyOutcome, Option<crate::memory_forget_steps::StepsReport>)> {
    if !enabled(home) {
        return Err(DuDuClawError::Memory("forget_source is disabled or unreadable".into()));
    }
    if duduclaw_core::is_valid_agent_id(&plan.document.agent_id)
        && ["memory.db", "state/memory.db"].iter().any(|relative|
            home.join("agents").join(&plan.document.agent_id).join(relative).exists())
    {
        return Err(DuDuClawError::Memory("unmerged employee memory.db".into()));
    }
    let external = if plan.document.selector.connector_ref.is_some() {
        Default::default()
    } else {
        crate::memory_forget_steps::collect_external_for_apply(home, plan).await
            .map_err(DuDuClawError::Memory)?
    };
    let outcome = engine.apply_forget_plan(&plan.plan_id, &external).await?;
    if let ApplyOutcome::Applied(r) = &outcome {
        duduclaw_security::audit::append_audit_event(
            home,
            &duduclaw_security::audit::AuditEvent::new(
                "memory_source_forget_applied",
                &plan.document.agent_id,
                duduclaw_security::audit::Severity::Warning,
                serde_json::json!({
                    "plan_id": r.plan_id,
                    "agent_id": r.agent_id,
                    "plan_hash": r.plan_hash,
                    "memories_deleted": r.memories_deleted,
                    "key_facts_deleted": r.key_facts_deleted,
                    "archive_deleted": r.archive_deleted,
                    "entity_embeddings_deleted": r.entity_embeddings_deleted,
                    "supersession_links_cut": r.supersession_links_cut,
                    "steps_pending": r.steps_pending,
                    "forget_epoch": r.forget_epoch,
                    "untracked_in_namespace": {
                        "planned": r.untracked_in_namespace_planned,
                        "at_apply": r.untracked_in_namespace_at_apply,
                    },
                    "other_namespaces_referencing": {
                        "planned": r.other_namespaces_referencing_planned,
                        "at_apply": r.other_namespaces_referencing_at_apply,
                    },
                }),
            ),
        );
    }
    let steps = if matches!(outcome, ApplyOutcome::Applied(_) | ApplyOutcome::AlreadyApplied) {
        Some(crate::memory_forget_steps::run_forget_steps(engine, home, &plan.plan_id).await
            .map_err(DuDuClawError::Memory)?)
    } else { None };
    Ok((outcome, steps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_memory_section_is_disabled() {
        let home = tempfile::tempdir().unwrap();
        for config in ["memory='bad'", "[memory]\nforget_source='yes'", "["] {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            assert!(!enabled(home.path()));
        }
    }

    #[tokio::test]
    async fn refuses_both_unmerged_employee_database_locations() {
        let home = tempfile::tempdir().unwrap();
        let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
        let duduclaw_memory::PlanOutcome::Planned(plan) = engine.plan_forget_connector(
            "alice", "google:home", Default::default()).await.unwrap() else { panic!("missing plan"); };
        for relative in ["memory.db", "state/memory.db"] {
            let path = home.path().join("agents/alice").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "stray").unwrap();
            assert!(apply(home.path(), &engine, &plan).await.is_err());
            assert_eq!(engine.get_forget_plan(&plan.plan_id).await.unwrap().unwrap().status, "planned");
            std::fs::remove_file(path).unwrap();
        }
    }
}
