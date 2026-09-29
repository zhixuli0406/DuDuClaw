//! Owner-operated invalidation of saved CCR originals.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_llm::{CcrScope, CcrStore};

pub fn revoke_artifact_version(
    db: &Path,
    tenant: &str,
    connector: &str,
    artifact: &str,
    version: &str,
) -> Result<()> {
    if !db.is_file() {
        return Err(DuDuClawError::Gateway("CCR database does not exist".into()));
    }
    let removed = CcrStore::new(db)
        .revoke_artifact_version(tenant, connector, artifact, version)
        .map_err(|error| DuDuClawError::Gateway(error.to_string()))?;
    println!("{}", serde_json::json!({"removed_originals": removed}));
    Ok(())
}

fn checked_revoke_scope(
    db: &Path,
    tenant: &str,
    agent: &str,
    session: &str,
    source_acl: &str,
) -> Result<usize> {
    if !db.is_file() {
        return Err(DuDuClawError::Gateway("CCR database does not exist".into()));
    }
    let scope = CcrScope {
        tenant_id: tenant.into(),
        agent_id: agent.into(),
        session_id: session.into(),
        source_acl: source_acl.into(),
    };
    CcrStore::new(db)
        .revoke_scope(&scope)
        .map_err(|error| DuDuClawError::Gateway(error.to_string()))
}

pub fn revoke_scope(
    db: &Path,
    tenant: &str,
    agent: &str,
    session: &str,
    source_acl: Option<&str>,
    principal: Option<&str>,
) -> Result<()> {
    let resolved_acl = match (source_acl, principal) {
        (Some(acl), None) if !acl.trim().is_empty() => acl.to_owned(),
        (None, Some(principal)) => {
            duduclaw_gateway::ccr_runtime::source_acl_for_principal(agent, session, principal)
                .ok_or_else(|| DuDuClawError::Gateway("invalid CCR principal scope".into()))?
        }
        _ => {
            return Err(DuDuClawError::Gateway(
                "provide exactly one of --source-acl or --principal".into(),
            ));
        }
    };
    let removed = checked_revoke_scope(db, tenant, agent, session, &resolved_acl)?;
    println!("{}", serde_json::json!({"removed_originals": removed}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_on_big_stack;
    use clap::Parser;

    #[derive(Debug)]
    struct FixtureCurrentSource {
        digest: String,
    }

    impl duduclaw_llm::CcrBoundSourceValidator for FixtureCurrentSource {
        fn valid(
            &self,
            scope: &CcrScope,
            artifact: &duduclaw_llm::CcrSourceArtifact,
            saved_sha256: &str,
        ) -> bool {
            scope.tenant_id == "local"
                && scope.agent_id == "support"
                && scope.session_id == "s1"
                && scope.source_acl == "private"
                && artifact.connector == "support"
                && artifact.artifact_id == "ticket-1"
                && artifact.version == "v2"
                && artifact.acl_revision == "acl-1"
                && saved_sha256 == self.digest
        }
    }

    // `run_on_big_stack` (why: clap's derive-generated `augment_subcommands`
    // chain overflows libtest's default 2 MiB test-thread stack when it
    // walks the full `Commands` enum — see `crate::test_support` for the
    // full rationale) now lives in `crate::test_support`, shared with every
    // other test module that calls `Cli::try_parse_from`/`Cli::parse()`.

    #[test]
    fn parsed_artifact_version_revocation_only_removes_matching_version() {
        run_on_big_stack(parsed_artifact_version_revocation_only_removes_matching_version_body);
    }

    fn parsed_artifact_version_revocation_only_removes_matching_version_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ccr.db");
        let store = CcrStore::new(&db);
        let scope = CcrScope {
            tenant_id: "local".into(),
            agent_id: "support".into(),
            session_id: "s1".into(),
            source_acl: "private".into(),
        };
        let old = duduclaw_llm::CcrSourceArtifact {
            connector: "support".into(),
            artifact_id: "ticket-1".into(),
            version: "v1".into(),
            acl_revision: "acl-1".into(),
        };
        let mut current = old.clone();
        current.version = "v2".into();
        let source_tool = serde_json::to_string(&("fixture", "search")).unwrap();
        let old_entry = store
            .put_bound(&scope, &source_tool, "call-1", "old", &old)
            .unwrap();
        let current_entry = store
            .put_bound(&scope, &source_tool, "call-2", "current", &current)
            .unwrap();
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "ccr-revoke-artifact-version",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "local",
            "--connector",
            "support",
            "--artifact",
            "ticket-1",
            "--version",
            "v1",
        ])
        .unwrap();
        let crate::Commands::Ccr(crate::CcrCommands::CcrRevokeArtifactVersion {
            db,
            tenant,
            connector,
            artifact,
            version,
        }) = parsed.command
        else {
            panic!("CCR artifact command did not parse")
        };
        revoke_artifact_version(&db, &tenant, &connector, &artifact, &version).unwrap();
        assert!(store.retrieve(&scope, &old_entry.id, None, 0, 100).is_err());
        let runtime = duduclaw_llm::CcrRuntime::new(store.clone(), scope)
            .restrict_sources([("fixture".into(), "search".into())])
            .with_bound_source_validator(std::sync::Arc::new(FixtureCurrentSource {
                digest: current_entry.content_sha256.clone(),
            }));
        assert!(runtime.retrieve(&current_entry.id, None, 0, 100).is_ok());
    }

    #[test]
    fn parsed_scope_revocation_blocks_retrieval_and_reinsertion() {
        run_on_big_stack(parsed_scope_revocation_blocks_retrieval_and_reinsertion_body);
    }

    fn parsed_scope_revocation_blocks_retrieval_and_reinsertion_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ccr.db");
        let store = CcrStore::new(&db);
        let scope = CcrScope {
            tenant_id: "local".into(),
            agent_id: "support".into(),
            session_id: "s1".into(),
            source_acl: "agent:support:session:s1".into(),
        };
        let entry = store.put(&scope, "search", "call-1", "original").unwrap();
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "ccr-revoke-scope",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "local",
            "--agent",
            "support",
            "--session",
            "s1",
            "--source-acl",
            "agent:support:session:s1",
        ])
        .unwrap();
        let crate::Commands::Ccr(crate::CcrCommands::CcrRevokeScope {
            db,
            tenant,
            agent,
            session,
            source_acl,
            principal,
        }) = parsed.command
        else {
            panic!("CCR scope command did not parse")
        };
        assert!(principal.is_none());
        assert_eq!(
            checked_revoke_scope(
                &db,
                &tenant,
                &agent,
                &session,
                source_acl.as_deref().unwrap()
            )
            .unwrap(),
            1
        );
        assert_eq!(
            checked_revoke_scope(
                &db,
                &tenant,
                &agent,
                &session,
                source_acl.as_deref().unwrap()
            )
            .unwrap(),
            0
        );
        assert!(store.retrieve(&scope, &entry.id, None, 0, 100).is_err());
        assert!(store.put(&scope, "search", "call-2", "new").is_err());
    }

    #[test]
    fn principal_flag_revokes_the_gateway_derived_scope() {
        run_on_big_stack(principal_flag_revokes_the_gateway_derived_scope_body);
    }

    fn principal_flag_revokes_the_gateway_derived_scope_body() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ccr.db");
        let store = CcrStore::new(&db);
        let scope = CcrScope {
            tenant_id: "local".into(),
            agent_id: "support".into(),
            session_id: "s1".into(),
            source_acl: duduclaw_gateway::ccr_runtime::source_acl_for_principal(
                "support", "s1", "user-1",
            )
            .unwrap(),
        };
        let entry = store.put(&scope, "search", "call-1", "original").unwrap();
        let parsed = crate::Cli::try_parse_from([
            "duduclaw",
            "ccr-revoke-scope",
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "local",
            "--agent",
            "support",
            "--session",
            "s1",
            "--principal",
            "user-1",
        ])
        .unwrap();
        let crate::Commands::Ccr(crate::CcrCommands::CcrRevokeScope {
            db,
            tenant,
            agent,
            session,
            source_acl,
            principal,
        }) = parsed.command
        else {
            panic!("CCR principal command did not parse")
        };
        assert!(source_acl.is_none());
        revoke_scope(&db, &tenant, &agent, &session, None, principal.as_deref()).unwrap();
        assert!(store.retrieve(&scope, &entry.id, None, 0, 64).is_err());
        assert!(revoke_scope(&db, &tenant, &agent, &session, None, None).is_err());
    }
}
