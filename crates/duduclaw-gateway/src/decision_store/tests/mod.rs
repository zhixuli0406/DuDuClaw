    use super::*;

    #[derive(Debug)]
    struct TestActiveCausalSource(CausalStore);

    impl duduclaw_llm::CcrBoundSourceValidator for TestActiveCausalSource {
        fn valid(
            &self,
            scope: &duduclaw_llm::CcrScope,
            artifact: &duduclaw_llm::CcrSourceArtifact,
            saved_sha256: &str,
        ) -> bool {
            if artifact.connector != "causal" || artifact.acl_revision != scope.source_acl {
                return false;
            }
            let source_scope = EvidenceScope {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.source_acl.clone(),
            };
            matches!(
                self.0.read_artifact_metadata(&source_scope, &artifact.artifact_id),
                Ok(active) if active.version == artifact.version
                    && active.content_sha256 == saved_sha256
            )
        }
    }


mod causal;
mod observed;
mod outcome;
mod replay;
mod shadow_forecast;
mod shadow_reserve;
mod shadow_screen;
mod shadow_window;
mod store_basics;
