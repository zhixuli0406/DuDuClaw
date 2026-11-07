use super::*;
use duduclaw_core::google_identity::CredentialKind;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane},
    ids::{RunId, Sha256Digest}};
use crate::isolation_egress::{ResolvedRunTurn, RunMetadata, RunTurnIdentity};

fn reference() -> CredentialRef { CredentialRef { kind: CredentialKind::Legacy, sha256: "a".repeat(64) } }
fn personal(owner: &str) -> ProductConnectorAccount {
    ProductConnectorAccount::Personal { account: "personal-google".into(), owner: owner.into() }
}

/// Host run association double: one live observation, or a lookup failure.
struct Turns(Result<Option<RunMetadata>, VerificationError>);
impl RunTurnResolver for Turns {
    fn resolve_run(&self, _: &RequestContext) -> Result<ResolvedRunTurn, VerificationError> {
        Ok(ResolvedRunTurn { identity: RunTurnIdentity { turn: "host-turn".into(), approval_session: Some("session".into()) },
            egress_turn: None, metadata: self.0.clone()?, principal_context: None, utility_runner: None })
    }
    fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
        panic!("the bridge must use the combined host snapshot")
    }
}
fn turns(metadata: RunMetadata) -> Arc<dyn RunTurnResolver> { Arc::new(Turns(Ok(Some(metadata)))) }
fn dashboard() -> RunMetadata { RunMetadata { ingress: Some(IngressRequester::Dashboard), ..RunMetadata::root() } }
fn human(user: &str) -> Principal { Principal::Human { user_id: Text::new(user.into()).unwrap(), generation: 1 } }
fn context(principal: Principal) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([1; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: EmployeeBinding::Personal(Text::new("alice".into()).unwrap()),
        lane: Lane::Standard, tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [5; 32], jti: [6; 16],
    })
}
fn bridge(home: &std::path::Path, turns: Arc<dyn RunTurnResolver>) -> GoogleIsolationBridge {
    GoogleIsolationBridge::new(home.into(), "bot".into(), EmployeeId([1;16]), None, reference(),
        GoogleAccountRoutes { personal: BTreeMap::from([("alice".into(), personal("alice")),
            ("bob".into(), personal("bob"))]), home: None }, turns).unwrap()
}

#[test]
fn bridge_refuses_invalid_owner_routes_and_personal_home_substitution() {
    let home = tempfile::tempdir().unwrap();
    for routes in [
        GoogleAccountRoutes { personal: BTreeMap::from([("alice".into(), personal("bob"))]), home: None },
        GoogleAccountRoutes { personal: BTreeMap::new(), home: Some(personal("alice")) },
    ] {
        assert!(GoogleIsolationBridge::new(home.path().into(), "bot".into(), EmployeeId([1;16]), None,
            reference(), routes, turns(dashboard())).is_err());
    }
}
#[test]
fn bridge_requires_gateway_home_valid_employee_and_authentication_reference() {
    let home = tempfile::tempdir().unwrap();
    for agent in ["../bot", "heartbeat-scheduler", duduclaw_core::UNTRUSTED_AGENT_ID] {
        assert!(GoogleIsolationBridge::new(home.path().into(), agent.into(), EmployeeId([1;16]), None,
            reference(), GoogleAccountRoutes::default(), turns(dashboard())).is_err());
    }
    let mut invalid = reference(); invalid.sha256 = "not-a-digest".into();
    assert!(GoogleIsolationBridge::new(home.path().into(), "bot".into(), EmployeeId([1;16]), None,
        invalid, GoogleAccountRoutes::default(), turns(dashboard())).is_err());
    assert!(GoogleIsolationBridge::new(PathBuf::from("relative"), "bot".into(), EmployeeId([1;16]), None,
        reference(), GoogleAccountRoutes::default(), turns(dashboard())).is_err());
}
#[test]
fn missing_live_google_metadata_refuses_without_creating_a_credential() {
    let home = tempfile::tempdir().unwrap();
    let bridge = GoogleIsolationBridge::new(home.path().into(), "bot".into(), EmployeeId([1;16]), None,
        reference(), GoogleAccountRoutes::default(), turns(dashboard())).unwrap();
    assert!(bridge.source(&context(human("alice")), "gmail_read").is_err());
    assert!(!home.path().join(".keyfile").exists());
    assert!(!home.path().join("mcp-oauth-tokens.json").exists());
    assert!(!home.path().join("person_credentials").exists());
}

#[test]
fn bridge_dashboard_requester_selects_own_personal() {
    let home = tempfile::tempdir().unwrap();
    let bridge = bridge(home.path(), turns(dashboard()));
    let requester = bridge.requester(&context(human("alice"))).unwrap();
    assert_eq!(requester, Requester::Human { user_id: "alice".into(),
        identity: RequesterIdentity::DashboardUser { user_id: "alice".into() } });
    assert!(matches!(bridge.personal_route(&requester, Some("alice")), Ok(route) if route == personal("alice")));
}

#[test]
fn bridge_other_requester_refused() {
    let home = tempfile::tempdir().unwrap();
    let bridge = bridge(home.path(), turns(dashboard()));
    // bob is authenticated and even has his own route; alice's source stays hers.
    let bob = bridge.requester(&context(human("bob"))).unwrap();
    assert_eq!(bridge.personal_route(&bob, Some("alice")).err(), Some(VerificationError::BindingMismatch));
    assert_eq!(bridge.personal_route(&bob, None).err(), Some(VerificationError::BindingMismatch));
    assert_eq!(bridge.personal_route(&bob, Some("")).err(), Some(VerificationError::BindingMismatch));
    // An owner without an installed route is not substituted by another route.
    let carol = bridge.requester(&context(human("carol"))).unwrap();
    assert_eq!(bridge.personal_route(&carol, Some("carol")).err(), Some(VerificationError::BindingMismatch));
    // A channel turn is a person only through the verified channel→user
    // mapping. A sender id spelled like the user id is not a dashboard user.
    let channel = bridge_with(home.path(), RunMetadata { ingress: Some(IngressRequester::Channel {
        channel: "telegram".into(), sender: "alice".into() }), reply_channel: Some("telegram:42".into()),
        ..RunMetadata::root() });
    assert_eq!(channel.requester(&context(human("alice"))), Err(VerificationError::Revoked));
}
fn bridge_with(home: &std::path::Path, metadata: RunMetadata) -> GoogleIsolationBridge { bridge(home, turns(metadata)) }

#[test]
fn bridge_channel_requester_uses_verified_mapping() {
    let home = tempfile::tempdir().unwrap();
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let alice = db.create_user("alice@example.com", "Alice", "correct-horse-battery-staple",
        duduclaw_auth::UserRole::Admin).unwrap();
    db.bind_channel_identity(&alice.id, "telegram", "tg-alice", true).unwrap();
    db.bind_channel_identity(&alice.id, "telegram", "tg-unverified", false).unwrap();
    let channel = |sender: &str| bridge_with(home.path(), RunMetadata { ingress: Some(IngressRequester::Channel {
        channel: "telegram".into(), sender: sender.into() }), reply_channel: Some("telegram:42".into()),
        ..RunMetadata::root() });
    assert_eq!(channel("tg-alice").requester(&context(human(&alice.id))), Ok(Requester::Human {
        user_id: alice.id.clone(), identity: RequesterIdentity::ChannelUser { channel: "telegram".into(),
            chat: "telegram:42".into(), sender: "tg-alice".into(), user_id: Some(alice.id.clone()) } }));
    // The live mapping must name the capability's own user.
    assert_eq!(channel("tg-alice").requester(&context(human("bob"))), Err(VerificationError::Revoked));
    assert_eq!(channel("tg-unverified").requester(&context(human(&alice.id))), Err(VerificationError::Revoked));
}

#[test]
fn bridge_background_never_personal_without_record_authority() {
    let home = tempfile::tempdir().unwrap();
    // Background records carry the owner as a Human principal but no ingress
    // marker: System work, never the owner in person.
    let record = bridge_with(home.path(), RunMetadata::root());
    let system = record.requester(&context(human("alice"))).unwrap();
    assert!(matches!(&system, Requester::System(RequesterIdentity::System { on_behalf_of: None, .. })));
    assert_eq!(record.personal_route(&system, Some("alice")).err(), Some(VerificationError::BindingMismatch));
    let tenant = record.requester(&context(Principal::Background {
        tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 })).unwrap();
    assert_eq!(record.personal_route(&tenant, Some("alice")).err(), Some(VerificationError::BindingMismatch));
    // An interactive marker on a background principal or a nested/dispatched
    // run is inconsistent host state and refuses outright.
    let marked = bridge_with(home.path(), dashboard());
    assert_eq!(marked.requester(&context(Principal::Background {
        tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 })), Err(VerificationError::BindingMismatch));
    for metadata in [
        RunMetadata { delegation_depth: 1, ..dashboard() },
        RunMetadata { hop_depth: 1, ..dashboard() },
        RunMetadata { delegation_origin: Some("manager".into()), ..dashboard() },
        RunMetadata { dispatch_run: Some(("cron:one".into(), "run".into())), ..dashboard() },
        RunMetadata { dispatch_run: Some(("cron:one".into(), "run".into())), upstream_unknown: true, ..dashboard() },
    ] {
        assert_eq!(bridge_with(home.path(), metadata).requester(&context(human("alice"))),
            Err(VerificationError::BindingMismatch));
    }
}

#[test]
fn bridge_resolver_failure_refuses() {
    let home = tempfile::tempdir().unwrap();
    for (resolver, expected) in [
        (Turns(Err(VerificationError::Revoked)), VerificationError::Revoked),
        (Turns(Err(VerificationError::Unavailable)), VerificationError::Unavailable),
        // A missing host association is not a root run and not Unknown.
        (Turns(Ok(None)), VerificationError::Unavailable),
    ] {
        let bridge = bridge(home.path(), Arc::new(resolver));
        assert_eq!(bridge.requester(&context(human("alice"))), Err(expected));
        // Resolution precedes every source read: the resolver's refusal is
        // returned, not a later registry/home result.
        assert!(matches!(bridge.source(&context(human("alice")), "gmail_read"), Err(error) if error == expected));
    }
}
