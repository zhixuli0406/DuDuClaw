//! Request-local compatibility projection for the existing MCP handlers.
//! No process environment is changed and no gateway identity is inherited.
use std::{env::VarError, ffi::{OsStr, OsString}, path::{Path, PathBuf}};
use duduclaw_isolation::capability::Principal;

pub(crate) fn verified_employee(home: &Path) -> Option<Result<String, ()>> {
    crate::mcp_isolation::current().map(|call| {
        if call.home == home { Ok(call.agent.clone()) } else { Err(()) }
    })
}

pub(crate) fn home() -> PathBuf {
    crate::mcp_isolation::current().map(|call| call.home.clone())
        .unwrap_or_else(duduclaw_core::duduclaw_home)
}

pub(crate) fn host_principal_checked()
    -> Result<Option<duduclaw_core::HumanPrincipal>, duduclaw_core::PrincipalRejection> {
    let Some(call) = crate::mcp_isolation::current() else {
        return duduclaw_core::host_principal_checked();
    };
    match &call.context.record().principal {
        Principal::Human { user_id, .. } => duduclaw_core::HumanPrincipal::user(user_id.as_str())
            .map(Some).ok_or_else(|| duduclaw_core::PrincipalRejection {
                reason: "invalid_isolated_principal", preview: String::new(),
            }),
        // A verified background run has no human principal, as on the
        // ordinary scheduled path. It is not malformed or an operator.
        Principal::Background { .. } => Ok(None),
    }
}

pub(crate) fn var(key: impl AsRef<OsStr>) -> Result<String, VarError> {
    let key = key.as_ref();
    let Some(call) = crate::mcp_isolation::current() else { return std::env::var(key); };
    let value = match key.to_str() {
        Some(duduclaw_core::ENV_AGENT_ID | duduclaw_core::ENV_DELEGATION_SENDER) => Some(call.agent.clone()),
        Some(duduclaw_core::ENV_AGENT_TOKEN) => duduclaw_core::identity_token::load_key(&call.home)
            .map(|key| duduclaw_core::mint_identity_token(&key, &call.agent)),
        Some(duduclaw_core::ENV_MCP_API_KEY) => internal_key(&call.home),
        Some("DUDUCLAW_PRINCIPAL_CONTEXT") => duduclaw_gateway::isolation_egress::principal_context(),
        // An absent mapping is not evidence of a root run. Existing depth
        // gates fail closed until the host supplies the registered metadata.
        Some(duduclaw_core::ENV_DELEGATION_DEPTH) => Some(duduclaw_gateway::isolation_egress::run_metadata()
            .map_or(u8::MAX, |metadata| metadata.delegation_depth).to_string()),
        Some(duduclaw_core::ENV_HOP_DEPTH) => Some(duduclaw_gateway::isolation_egress::run_metadata()
            .map_or(u8::MAX, |metadata| metadata.hop_depth).to_string()),
        Some(duduclaw_core::ENV_DELEGATION_ORIGIN) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.delegation_origin),
        Some(duduclaw_core::ENV_REPLY_CHANNEL) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.reply_channel),
        Some(duduclaw_core::ENV_REPLY_SENDER) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.reply_sender),
        Some(duduclaw_core::ENV_TASK_ID) => call.context.record().task.as_ref().map(|task| task.as_str().to_owned()),
        Some(duduclaw_core::ENV_ON_BEHALF_OF) => host_principal_checked().ok().flatten().map(|p| p.to_string()),
        Some(duduclaw_core::ENV_TRUST_TURN_ID) =>
            duduclaw_gateway::isolation_egress::resolved_identity()
                .and_then(|(turn, session)| session.map(|_| turn)),
        Some(duduclaw_core::egress_guard::ENV_TURN_ALIAS) =>
            duduclaw_gateway::isolation_egress::execution_turn(),
        Some(duduclaw_core::ENV_TRUST_SESSION_ID) =>
            duduclaw_gateway::isolation_egress::resolved_identity().and_then(|(_, session)| session),
        Some(duduclaw_core::ENV_DISPATCH_SESSION) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.dispatch_run.map(|(session, _)| session)),
        Some(duduclaw_core::ENV_DISPATCH_RUN_ID) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.dispatch_run.map(|(_, run)| run)),
        Some(duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.user_message.map(|(seq, _)| seq.to_string())),
        Some(duduclaw_core::ENV_TURN_USER_MESSAGE_AT) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.user_message.map(|(_, at)| at)),
        Some(duduclaw_core::ENV_UPSTREAM_UNKNOWN) =>
            duduclaw_gateway::isolation_egress::run_metadata().and_then(|metadata| metadata.upstream_unknown.then(|| "1".into())),
        // Delegation depth, workflow sessions and principal-context handles
        // require a host run mapping; absent values never inherit another run.
        _ => None,
    };
    value.ok_or(VarError::NotPresent)
}

pub(crate) fn var_os(key: impl AsRef<OsStr>) -> Option<OsString> {
    if crate::mcp_isolation::current().is_none() { return std::env::var_os(key); }
    var(key).ok().map(OsString::from)
}

// Compatibility for signed loopback clients only. Read existing registered
// material; never create/rotate a key while executing an employee tool.
fn internal_key(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let config: toml::Table = toml::from_str(&raw).ok()?;
    let keys = config.get("mcp_keys")?.as_table()?;
    keys.keys().find(|key| crate::mcp_auth::authenticate_with_key(key, home)
        .is_ok_and(|principal| !principal.is_external
            && principal.client_id == duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane}, ids::{EmployeeId, RunId, Sha256Digest}};
    use duduclaw_mcp_boundary::{RequestContext, resources::ResourceRoots};
    use crate::mcp_isolation::{IsolatedCall, ISOLATED_CALL};

    fn call(home: &Path, agent: &str) -> Arc<IsolatedCall> {
        Arc::new(IsolatedCall {
            home: home.to_path_buf(), agent: agent.to_owned(), roots: Arc::new(ResourceRoots::default()),
            parser: None, services: None, execution: None, worker: None, record_resolver: None,
            context: RequestContext::from_verified_record(CapabilityRecord {
                id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
                task: Some(Text::new("host-task".into()).unwrap()),
                principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
                employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Explore,
                tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
                gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
            }),
        })
    }

    #[tokio::test]
    async fn isolated_identity_projection_is_scoped_and_never_inherits_gateway_environment() {
        let home = tempfile::tempdir().unwrap();
        let check = |agent: &'static str| {
            let call = call(home.path(), agent);
            ISOLATED_CALL.scope(call, async move {
                tokio::task::yield_now().await;
                assert_eq!(var(duduclaw_core::ENV_AGENT_ID).unwrap(), agent);
                assert_eq!(var(duduclaw_core::ENV_TASK_ID).unwrap(), "host-task");
                assert_eq!(host_principal_checked().unwrap(), duduclaw_core::HumanPrincipal::user("owner"));
                for name in ["PATH", "DUDUCLAW_WORKFLOW_SESSION_ID", "DUDUCLAW_REPLY_CHANNEL"] {
                    assert_eq!(var(name), Err(VarError::NotPresent));
                }
                assert_eq!(crate::mcp::get_default_agent(&self::home()).await, agent);
                assert!(crate::mcp_memory_handlers::mcp_write_provenance(agent).is_ok());
            })
        };
        tokio::join!(check("one"), check("two"));
        assert!(crate::mcp_isolation::current().is_none());
    }

    #[tokio::test]
    async fn isolated_identity_reuses_soul_permission_without_operator_fallback() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/one")).unwrap();
        let config = home.path().join("agents/one/agent.toml");
        std::fs::write(&config, "[agent]\nname = \"one\"\n").unwrap();
        ISOLATED_CALL.scope(call(home.path(), "one"), async {
            assert_eq!(crate::mcp::caller_identity_verdict(home.path()), duduclaw_core::IdentityVerdict::Verified);
            assert_eq!(crate::mcp::soul_write_denial(home.path(), "one").await, Some("one".into()));
            std::fs::write(&config, "[permissions]\ncan_modify_own_soul = true\n").unwrap();
            assert_eq!(crate::mcp::soul_write_denial(home.path(), "one").await, None);
            assert_eq!(crate::mcp::soul_write_denial(home.path(), "other").await, Some("one".into()));
            let wrong = home.path().join("other-home");
            assert_eq!(crate::mcp::caller_identity_verdict(&wrong), duduclaw_core::IdentityVerdict::Rejected);
            assert!(crate::mcp_namespace::verified_employee_from_env(&wrong).is_none());
            assert!(!home.path().join("identity.key").exists());
        }).await;
    }

    #[tokio::test]
    async fn isolated_identity_uses_host_delegation_metadata_without_resetting_nested_runs() {
        use duduclaw_gateway::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver};
        #[derive(Clone)]
        struct Resolver(RunMetadata);
        impl RunTurnResolver for Resolver {
            fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, duduclaw_mcp_boundary::VerificationError> {
                Ok(RunTurnIdentity { turn: "host-turn".into(), approval_session: Some("session".into()) })
            }
            fn metadata(&self, _: &RequestContext) -> Result<Option<RunMetadata>, duduclaw_mcp_boundary::VerificationError> {
                Ok(Some(self.0.clone()))
            }
        }
        let home = tempfile::tempdir().unwrap();
        let nested = RunMetadata { delegation_depth: 2, hop_depth: 3,
            delegation_origin: Some("manager".into()), reply_channel: Some("telegram:123".into()),
            reply_sender: Some("sender".into()), ..RunMetadata::root() };
        for metadata in [RunMetadata::root(), nested] {
            let call = call(home.path(), "one");
            let context = call.context.clone();
            let resolver = Resolver(metadata.clone());
            duduclaw_gateway::isolation_egress::scope(home.path(), "one", &context, Some(Arc::new(resolver.clone())),
                ISOLATED_CALL.scope(call, async {
                    assert_eq!(var(duduclaw_core::ENV_DELEGATION_DEPTH).unwrap(), metadata.delegation_depth.to_string());
                    assert_eq!(var(duduclaw_core::ENV_HOP_DEPTH).unwrap(), metadata.hop_depth.to_string());
                    assert_eq!(var(duduclaw_core::ENV_DELEGATION_ORIGIN).ok(), metadata.delegation_origin);
                    assert_eq!(var(duduclaw_core::ENV_REPLY_CHANNEL).ok(), metadata.reply_channel);
                    assert_eq!(var(duduclaw_core::ENV_REPLY_SENDER).ok(), metadata.reply_sender);
                    assert_eq!(var("PATH"), Err(VarError::NotPresent));
                })).await.unwrap();
        }
        ISOLATED_CALL.scope(call(home.path(), "one"), async {
            assert_eq!(var(duduclaw_core::ENV_DELEGATION_DEPTH).unwrap(), u8::MAX.to_string());
            assert_eq!(var(duduclaw_core::ENV_HOP_DEPTH).unwrap(), u8::MAX.to_string());
            assert_eq!(var(duduclaw_core::ENV_REPLY_CHANNEL), Err(VarError::NotPresent));
        }).await;
    }

    #[tokio::test]
    async fn isolated_identity_preserves_background_and_memory_provenance_pairs() {
        use duduclaw_gateway::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver};
        #[derive(Clone)]
        struct Resolver { session: Option<String>, metadata: RunMetadata }
        impl RunTurnResolver for Resolver {
            fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, duduclaw_mcp_boundary::VerificationError> {
                Ok(RunTurnIdentity { turn: "host-turn".into(), approval_session: self.session.clone() })
            }
            fn metadata(&self, _: &RequestContext) -> Result<Option<RunMetadata>, duduclaw_mcp_boundary::VerificationError> {
                Ok(Some(self.metadata.clone()))
            }
            fn egress_turn(&self, _: &RequestContext) -> Result<Option<String>, duduclaw_mcp_boundary::VerificationError> {
                Ok(Some("host-execution".into()))
            }
        }
        let home = tempfile::tempdir().unwrap();
        let mut call = call(home.path(), "one");
        let mut record = call.context.record().clone();
        record.principal = Principal::Background { tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 };
        Arc::get_mut(&mut call).unwrap().context = RequestContext::from_verified_record(record);
        let context = call.context.clone();
        let resolver = Resolver { session: None, metadata: RunMetadata {
            dispatch_run: Some(("cron:one".into(), "registered-run".into())), upstream_unknown: true,
            ..RunMetadata::root() } };
        duduclaw_gateway::isolation_egress::scope(home.path(), "one", &context, Some(Arc::new(resolver.clone())),
            ISOLATED_CALL.scope(call.clone(), async {
                assert_eq!(host_principal_checked().unwrap(), None);
                assert_eq!(var(duduclaw_core::ENV_TRUST_TURN_ID), Err(VarError::NotPresent));
                assert_eq!(var(duduclaw_core::ENV_TRUST_SESSION_ID), Err(VarError::NotPresent));
                assert_eq!(var(duduclaw_core::egress_guard::ENV_TURN_ALIAS).unwrap(), "host-execution");
                assert_eq!(var(duduclaw_core::ENV_DISPATCH_SESSION).unwrap(), "cron:one");
                assert_eq!(var(duduclaw_core::ENV_DISPATCH_RUN_ID).unwrap(), "registered-run");
                assert_eq!(var(duduclaw_core::ENV_UPSTREAM_UNKNOWN).unwrap(), "1");
                let duduclaw_memory::lineage::Provenance::Sources(sources) =
                    crate::mcp_memory_handlers::mcp_write_provenance("one").unwrap() else { panic!("missing sources") };
                assert!(sources.iter().any(|source| source.kind == duduclaw_memory::SourceKind::DispatchRun));
                assert!(sources.iter().any(|source| source.kind == duduclaw_memory::SourceKind::UpstreamUnknown));
            })).await.unwrap();
        let resolver = Resolver { session: Some("conversation".into()), metadata: RunMetadata {
            user_message: Some((7, "2026-11-05T00:00:00Z".into())), ..RunMetadata::root() } };
        duduclaw_gateway::isolation_egress::scope(home.path(), "one", &context, Some(Arc::new(resolver.clone())),
            ISOLATED_CALL.scope(call.clone(), async {
                assert_eq!(var(duduclaw_core::ENV_TRUST_TURN_ID).unwrap(), "host-turn");
                assert_eq!(var(duduclaw_core::egress_guard::ENV_TURN_ALIAS).unwrap(), "host-execution");
                assert_eq!(var(duduclaw_core::ENV_TRUST_SESSION_ID).unwrap(), "conversation");
                assert_eq!(var(duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ).unwrap(), "7");
                assert_eq!(var(duduclaw_core::ENV_TURN_USER_MESSAGE_AT).unwrap(), "2026-11-05T00:00:00Z");
                let duduclaw_memory::lineage::Provenance::Sources(sources) =
                    crate::mcp_memory_handlers::mcp_write_provenance("one").unwrap() else { panic!("missing sources") };
                assert!(sources.iter().any(|source| source.kind == duduclaw_memory::SourceKind::McpTurn));
                assert!(sources.iter().any(|source| source.kind == duduclaw_memory::SourceKind::ChannelMessage && source.seq == Some(7)));
            })).await.unwrap();
        duduclaw_gateway::isolation_egress::scope(home.path(), "one", &context, None,
            ISOLATED_CALL.scope(call, async {
                assert_eq!(var(duduclaw_core::ENV_TRUST_TURN_ID), Err(VarError::NotPresent));
                assert_eq!(var(duduclaw_core::ENV_TRUST_SESSION_ID), Err(VarError::NotPresent));
                let duduclaw_memory::lineage::Provenance::Sources(sources) =
                    crate::mcp_memory_handlers::mcp_write_provenance("one").unwrap() else { panic!("missing sources") };
                assert_eq!(sources.len(), 1);
                assert_eq!(sources[0].kind, duduclaw_memory::SourceKind::McpExternal);
            })).await.unwrap();
        let orphan_message = Resolver { session: None, ..resolver };
        assert!(duduclaw_gateway::isolation_egress::scope(home.path(), "one", &context,
            Some(Arc::new(orphan_message)), async {}).await.is_err());
    }
}
