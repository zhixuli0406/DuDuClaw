//! SR4 items 4/5: the host lane is bound to the tenant source the isolation
//! bridge pinned, and a tenant assertion asks only for the configured scopes.
use super::*;

const GMAIL_READ: &str = "https://www.googleapis.com/auth/gmail.readonly";
const DRIVE_READ: &str = "https://www.googleapis.com/auth/drive.readonly";
const SEALED: &str = r#"{"version":1,"client_email":"dwd@project.iam.gserviceaccount.com","imported_at":"2026-11-08T00:00:00Z","sealed":"pk:v1:kid-1:body"}"#;

fn write_config(home: &Path, scopes: &[&str]) {
    let scopes = scopes.iter().map(|scope| format!("\"{scope}\"")).collect::<Vec<_>>().join(", ");
    std::fs::write(home.join("config.toml"), format!(
        "[integrations.google_identity]\nallowed_subject_domains = [\"customer.com\"]\ntenant_scopes = [{scopes}]\n"))
        .unwrap();
}

fn context(home: &Path) -> GoogleCallContext {
    GoogleCallContext::dedicated(home, "bot", crate::google_identity::requester_gate::people_for(home, None),
        "gmail_read", gi::google_arguments_digest(&serde_json::json!({})),
        CredentialRef { kind: gi::CredentialKind::Legacy, sha256: "a".repeat(64) })
        .unwrap()
}

#[test]
fn sr4_host_lane_refuses_a_tenant_source_other_than_the_pinned_one() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"), "[google]\nidentity = \"delegate:alice@customer.com\"\n")
        .unwrap();
    std::fs::create_dir_all(home.path().join("protected")).unwrap();
    let sealed = crate::google_identity::delegation_key::path(home.path());
    std::fs::write(&sealed, SEALED).unwrap();
    write_config(home.path(), &[GMAIL_READ]);

    let source = crate::google_identity::tenant_source::read(home.path(), "bot").unwrap().unwrap();
    let pin = TenantPin::of(&source);
    assert_eq!(pin.digest(), source.digest());
    let pinned = context(home.path()).pin_tenant(Some(pin.clone())).unwrap();
    assert!(pinned.check_tenant().is_ok());
    assert_eq!(pinned.assertion_scope().unwrap(), GMAIL_READ, "exactly the configured tenant scopes");
    // Unpinned (non-isolated) lanes keep the full minted set.
    assert_eq!(context(home.path()).assertion_scope().unwrap(), crate::google_workspace::REQUIRED_SCOPES.join(" "));

    // `tenant_scopes` widened after the bridge decided: the host lane refuses
    // instead of deciding on its own re-read.
    write_config(home.path(), &[GMAIL_READ, DRIVE_READ]);
    assert!(pinned.check_tenant().is_err());
    assert!(context(home.path()).pin_tenant(Some(pin.clone())).is_err());
    // The decided source again: admitted (the digest binds, not the read time).
    write_config(home.path(), &[GMAIL_READ]);
    assert!(pinned.check_tenant().is_ok());
    // A re-imported delegation key changes the source too.
    std::fs::write(&sealed, SEALED.replace("body", "other")).unwrap();
    assert!(pinned.check_tenant().is_err());
    // A configuration that no longer is a tenant source refuses.
    std::fs::write(&sealed, SEALED).unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"), "[google]\nidentity = \"home\"\n").unwrap();
    assert!(pinned.check_tenant().is_err());

    // A tenant pin without scopes never mints an assertion.
    let mut empty = context(home.path());
    empty.tenant = Some(TenantPin { digest: pin.digest().to_owned(), scopes: Vec::new() });
    assert!(empty.assertion_scope().is_err());
}
