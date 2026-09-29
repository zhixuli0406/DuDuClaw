//! Unit tests for [`super`], moved verbatim out of the former `ccr.rs`.
//!
//! Shared fixtures live here; the cases themselves are split across the
//! sibling files for size only.

use super::*;

mod binding;
mod retrieval;
mod search;


#[derive(Debug)]
struct OnlyLiveSource;

impl CcrBoundSourceValidator for OnlyLiveSource {
    fn valid(&self, _scope: &CcrScope, artifact: &CcrSourceArtifact, _digest: &str) -> bool {
        artifact.artifact_id == "live"
    }
}

#[derive(Debug)]
struct ExactTestSource {
    tenant: &'static str,
    version: &'static str,
}

impl CcrBoundSourceValidator for ExactTestSource {
    fn valid(&self, scope: &CcrScope, artifact: &CcrSourceArtifact, digest: &str) -> bool {
        scope.tenant_id == self.tenant
            && artifact.artifact_id == "ticket-1"
            && artifact.version == self.version
            && digest.len() == 64
    }
}

#[derive(Debug)]
struct DeleteBindingOnSecondValidation {
    db: PathBuf,
    entry_id: String,
    checks: std::sync::atomic::AtomicUsize,
}

impl CcrBoundSourceValidator for DeleteBindingOnSecondValidation {
    fn valid(&self, _scope: &CcrScope, artifact: &CcrSourceArtifact, _digest: &str) -> bool {
        if self
            .checks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            Connection::open(&self.db)
                .unwrap()
                .execute(
                    "DELETE FROM ccr_artifact_bindings WHERE entry_id=?1",
                    [&self.entry_id],
                )
                .unwrap();
        }
        artifact.artifact_id == "live"
    }
}

fn scope(tenant: &str) -> CcrScope {
    CcrScope {
        tenant_id: tenant.into(),
        agent_id: "support".into(),
        session_id: "s1".into(),
        source_acl: "private-thread".into(),
    }
}
