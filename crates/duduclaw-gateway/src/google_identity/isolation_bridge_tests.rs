use super::*;
use duduclaw_core::google_identity::CredentialKind;

fn reference() -> CredentialRef { CredentialRef { kind: CredentialKind::Legacy, sha256: "a".repeat(64) } }
fn personal(owner: &str) -> ProductConnectorAccount {
    ProductConnectorAccount::Personal { account: "personal-google".into(), owner: owner.into() }
}
#[test]
fn bridge_refuses_invalid_owner_routes_and_personal_home_substitution() {
    let home = tempfile::tempdir().unwrap();
    for routes in [
        GoogleAccountRoutes { personal: BTreeMap::from([("alice".into(), personal("bob"))]), home: None },
        GoogleAccountRoutes { personal: BTreeMap::new(), home: Some(personal("alice")) },
    ] {
        assert!(GoogleIsolationBridge::new(home.path().into(), "bot".into(), EmployeeId([1;16]), None,
            reference(), routes).is_err());
    }
}
#[test]
fn bridge_requires_gateway_home_valid_employee_and_authentication_reference() {
    let home = tempfile::tempdir().unwrap();
    for agent in ["../bot", "heartbeat-scheduler", duduclaw_core::UNTRUSTED_AGENT_ID] {
        assert!(GoogleIsolationBridge::new(home.path().into(), agent.into(), EmployeeId([1;16]), None,
            reference(), GoogleAccountRoutes::default()).is_err());
    }
    let mut invalid = reference(); invalid.sha256 = "not-a-digest".into();
    assert!(GoogleIsolationBridge::new(home.path().into(), "bot".into(), EmployeeId([1;16]), None,
        invalid, GoogleAccountRoutes::default()).is_err());
    assert!(GoogleIsolationBridge::new(PathBuf::from("relative"), "bot".into(), EmployeeId([1;16]), None,
        reference(), GoogleAccountRoutes::default()).is_err());
}
#[test]
fn missing_live_google_metadata_refuses_without_creating_a_credential() {
    let home = tempfile::tempdir().unwrap();
    let bridge = GoogleIsolationBridge::new(home.path().into(), "bot".into(), EmployeeId([1;16]), None,
        reference(), GoogleAccountRoutes::default()).unwrap();
    assert!(bridge.source("gmail_read").is_err());
    assert!(!home.path().join(".keyfile").exists());
    assert!(!home.path().join("mcp-oauth-tokens.json").exists());
    assert!(!home.path().join("person_credentials").exists());
}
