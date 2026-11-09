use super::*;

const GMAIL_READ: &str = "https://www.googleapis.com/auth/gmail.readonly";
const SEALED: &str = r#"{"version":1,"client_email":"dwd@project.iam.gserviceaccount.com","imported_at":"2026-11-08T00:00:00Z","sealed":"pk:v1:kid-1:body"}"#;
const SA_KEY: &str = r#"{"client_email":"sa@project.iam.gserviceaccount.com","private_key":"-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----\n"}"#;

struct Home(tempfile::TempDir);
impl Home {
    fn new() -> Self {
        let home = Self(tempfile::tempdir().unwrap());
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        home
    }
    fn path(&self) -> &Path { self.0.path() }
    fn agent(&self, google: &str) {
        std::fs::write(self.path().join("agents/bot/agent.toml"), format!("[google]\n{google}\n")).unwrap();
    }
    fn config(&self, extra: &str) {
        std::fs::write(self.path().join("config.toml"), format!(
            "[integrations.google_identity]\nallowed_subject_domains = [\"customer.com\"]\n\
             service_mailboxes = [\"support@customer.com\"]\ntenant_scopes = [\"{GMAIL_READ}\"]\n{extra}")).unwrap();
    }
    fn sealed(&self, body: &str) {
        std::fs::create_dir_all(self.path().join("protected")).unwrap();
        std::fs::write(super::super::delegation_key::path(self.path()), body).unwrap();
    }
}
const SA_SECTION: &str = "[integrations.google_service_account]\nkey_file = \"sa.json\"\nsubject = \"Support@Customer.com\"\n";

#[test]
fn e4_unreadable_key_unavailable() {
    // Delegate: the sealed delegation key is the source material.
    let home = Home::new();
    home.agent("identity = \"delegate:alice@customer.com\"");
    home.config("");
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "missing sealed key");
    home.sealed("not json");
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "damaged sealed record");
    home.sealed(r#"{"version":1,"client_email":"x","imported_at":"t","sealed":"plaintext-key"}"#);
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "record not sealed");
    std::fs::remove_file(super::super::delegation_key::path(home.path())).unwrap();
    std::fs::create_dir_all(super::super::delegation_key::path(home.path())).unwrap();
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "directory in place of the key");
    assert_eq!(installed(home.path()), Err(TenantSourceError::Unavailable));
    std::fs::remove_dir(super::super::delegation_key::path(home.path())).unwrap();
    home.sealed(SEALED);
    let source = read(home.path(), "bot").unwrap().expect("delegate tenant source");
    assert_eq!((source.kind(), source.subject()), (TenantSourceKind::Delegate, "alice@customer.com"));
    assert_eq!(installed(home.path()), Ok(true));
    // Same material ⇒ same proof; replaced material ⇒ a different source.
    assert_eq!(read(home.path(), "bot").unwrap().as_ref(), Some(&source));
    home.sealed(&SEALED.replace("body", "other"));
    let replaced = read(home.path(), "bot").unwrap().unwrap();
    assert_ne!(replaced, source);
    assert_ne!(replaced.digest(), source.digest());
    // Unreadable config refuses too, never "no tenant source".
    std::fs::write(home.path().join("config.toml"), "not = [toml").unwrap();
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable));

    // Service account: the configured key file is the source material.
    let home = Home::new();
    home.config(SA_SECTION);
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "missing key file");
    assert_eq!(installed(home.path()), Err(TenantSourceError::Unavailable));
    std::fs::write(home.path().join("sa.json"), "{ not json").unwrap();
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "malformed key file");
    std::fs::write(home.path().join("sa.json"), r#"{"client_email":"sa@p","private_key":""}"#).unwrap();
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable), "incomplete key file");
    std::fs::write(home.path().join("sa.json"), SA_KEY).unwrap();
    let source = read(home.path(), "bot").unwrap().expect("service-account tenant source");
    assert_eq!((source.kind(), source.subject()), (TenantSourceKind::ServiceAccount, "support@customer.com"));
    assert_eq!(installed(home.path()), Ok(true));
}

#[test]
fn e4_tenant_source_refuses_home_fallback_paths() {
    let home = Home::new();
    home.sealed(SEALED);
    home.config("");
    // A delegate failure may never be served from the home account.
    home.agent("identity = \"delegate:alice@customer.com\"\nfallback = \"home\"");
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Refused));
    // Outside the allowed subject domains.
    home.agent("identity = \"delegate:alice@elsewhere.com\"");
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Refused));
    // A service account beside an Apps Script bridge would fall back to it.
    let home = Home::new();
    std::fs::write(home.path().join("sa.json"), SA_KEY).unwrap();
    home.config(&format!("{SA_SECTION}[integrations.google_apps_script]\nurl = \"https://script.google.com/macros/s/x/exec\"\nsecret = \"s\"\n"));
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Refused));
    // A configured scope outside the minted set is a configuration error.
    let home = Home::new();
    std::fs::write(home.path().join("sa.json"), SA_KEY).unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[integrations.google_identity]\ntenant_scopes = [\"https://www.googleapis.com/auth/admin.directory.user\"]\n{SA_SECTION}")).unwrap();
    assert_eq!(read(home.path(), "bot"), Err(TenantSourceError::Unavailable));
    // Home OAuth (no service account) and Personal selectors are not tenant sources.
    let home = Home::new();
    home.config("");
    assert_eq!(read(home.path(), "bot"), Ok(None));
    assert_eq!(installed(home.path()), Ok(false));
    home.agent("identity = \"owner\"");
    assert_eq!(read(home.path(), "bot"), Ok(None));
}

#[test]
fn e4_subject_accounts_compare_normalized_email_exactly() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let alice = db.create_user("Alice@Customer.com", "Alice", "correct-horse-battery-staple",
        duduclaw_auth::UserRole::Employee).unwrap();
    db.create_user("alice@customer.com.evil.io", "Mallory", "correct-horse-battery-staple",
        duduclaw_auth::UserRole::Employee).unwrap();
    db.create_user("xalice@customer.com", "Eve", "correct-horse-battery-staple",
        duduclaw_auth::UserRole::Employee).unwrap();
    assert_eq!(subject_accounts(&db, "bot", "alice@customer.com").unwrap(),
        vec![SubjectAccount { user_id: alice.id, active: true, attested: false }]);
    assert!(subject_accounts(&db, "bot", "support@customer.com").unwrap().is_empty());
}

#[test]
fn e4_subject_rule_is_exact() {
    let person = TenantSource::fixture(TenantSourceKind::Delegate, "alice@customer.com", &[], &[GMAIL_READ]);
    let alice = [SubjectAccount { user_id: "alice-id".into(), active: true, attested: true }];
    assert_eq!(admit_subject(&person, Actor::Human("alice-id"), &alice), Ok(SubjectUse::RequestingUser));
    for actor in [Actor::Human("bob-id"), Actor::Human("ALICE-ID"), Actor::Human("alice-id "), Actor::Human(""), Actor::System] {
        assert_eq!(admit_subject(&person, actor, &alice), Err(VerificationError::BindingMismatch), "{actor:?}");
    }
    let inactive = [SubjectAccount { user_id: "alice-id".into(), active: false, attested: true }];
    assert_eq!(admit_subject(&person, Actor::Human("alice-id"), &inactive), Err(VerificationError::BindingMismatch));
    let twice = [alice[0].clone(), SubjectAccount { user_id: "alice-2".into(), active: true, attested: true }];
    assert_eq!(admit_subject(&person, Actor::Human("alice-id"), &twice), Err(VerificationError::BindingMismatch));
    // Not a person and not designated: nobody's mailbox to act as.
    assert_eq!(admit_subject(&person, Actor::Human("alice-id"), &[]), Err(VerificationError::BindingMismatch));
    // A designated service mailbox serves any admitted requester…
    let mailbox = TenantSource::fixture(TenantSourceKind::Delegate, "support@customer.com",
        &["support@customer.com"], &[GMAIL_READ]);
    for actor in [Actor::Human("bob-id"), Actor::System] {
        assert_eq!(admit_subject(&mailbox, actor, &[]), Ok(SubjectUse::ServiceMailbox));
    }
    // …unless it is in fact a person's account.
    assert_eq!(admit_subject(&mailbox, Actor::Human("alice-id"), &alice), Err(VerificationError::BindingMismatch));
}

fn person(db: &UserDb, email: &str) -> String {
    db.create_user(email, "Person", "correct-horse-battery-staple", duduclaw_auth::UserRole::Employee).unwrap().id
}
fn attest(db: &UserDb, agent: &str, subject: &'static str) -> Result<(), String> {
    db.attest_google_delegate(agent, subject,
        &move |email: &str| gi::normalize_email(email).is_ok_and(|email| email == subject), "admin-id").map(|_| ())
}

/// SR4 item 1: setting your own login email to a delegate subject never makes
/// you that person; only an Admin attestation of exactly your account does,
/// and any later email change of the attested account revokes it.
#[test]
fn sr4_self_email_change_to_delegate_subject_refused() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let source = TenantSource::fixture(TenantSourceKind::Delegate, "ceo@customer.com", &[], &[GMAIL_READ]);
    let mallory = person(&db, "mallory@customer.com");
    // The CEO has no DuDuClaw account; Mallory claims the address.
    db.change_own_email(&mallory, "CEO@Customer.com").unwrap();
    let accounts = subject_accounts(&db, "bot", "ceo@customer.com").unwrap();
    assert_eq!(accounts, vec![SubjectAccount { user_id: mallory.clone(), active: true, attested: false }]);
    assert_eq!(subject_state(&source, &accounts), SubjectState::AttestationRequired);
    assert_eq!(admit_subject(&source, Actor::Human(&mallory), &accounts), Err(VerificationError::BindingMismatch));

    // An attested person is admitted for that person only.
    let alice = person(&db, "alice@customer.com");
    let alice_source = TenantSource::fixture(TenantSourceKind::Delegate, "alice@customer.com", &[], &[GMAIL_READ]);
    assert_eq!(admit_subject(&alice_source, Actor::Human(&alice),
        &subject_accounts(&db, "bot", "alice@customer.com").unwrap()), Err(VerificationError::BindingMismatch),
        "no attestation (an existing delegate config) ⇒ refused until an Admin attests");
    attest(&db, "bot", "alice@customer.com").unwrap();
    let accounts = subject_accounts(&db, "bot", "alice@customer.com").unwrap();
    assert_eq!(subject_state(&alice_source, &accounts), SubjectState::AttestedPerson);
    assert_eq!(admit_subject(&alice_source, Actor::Human(&alice), &accounts), Ok(SubjectUse::RequestingUser));
    for actor in [Actor::Human(mallory.as_str()), Actor::System] {
        assert_eq!(admit_subject(&alice_source, actor, &accounts), Err(VerificationError::BindingMismatch));
    }
    // The attestation is per employee.
    assert!(!subject_accounts(&db, "other-bot", "alice@customer.com").unwrap()[0].attested);
    // Alice changes her email and back: the attestation is revoked.
    db.change_own_email(&alice, "alice.tmp@customer.com").unwrap();
    db.change_own_email(&alice, "alice@customer.com").unwrap();
    let accounts = subject_accounts(&db, "bot", "alice@customer.com").unwrap();
    assert_eq!(subject_state(&alice_source, &accounts), SubjectState::AttestationRequired);
    assert_eq!(admit_subject(&alice_source, Actor::Human(&alice), &accounts), Err(VerificationError::BindingMismatch));
    // Alice leaves the address and Mallory takes it: the old attestation never
    // transfers (it names Alice's user id).
    attest(&db, "bot", "alice@customer.com").unwrap();
    db.change_own_email(&alice, "alice.new@customer.com").unwrap();
    db.change_own_email(&mallory, "alice@customer.com").unwrap();
    let accounts = subject_accounts(&db, "bot", "alice@customer.com").unwrap();
    assert!(!accounts[0].attested);
    assert_eq!(admit_subject(&alice_source, Actor::Human(&mallory), &accounts), Err(VerificationError::BindingMismatch));
    // A service account never acts as a person, attested or not.
    let sa = TenantSource::fixture(TenantSourceKind::ServiceAccount, "alice@customer.com", &[], &[GMAIL_READ]);
    let attested = [SubjectAccount { user_id: alice.clone(), active: true, attested: true }];
    assert_eq!(subject_state(&sa, &attested), SubjectState::NotAdmissible);
}

/// SR4 item 2(iii): a stored email that cannot be compared is Unavailable,
/// never skipped; only an address that names no mailbox at all is inert.
#[test]
fn sr4_unrecognised_stored_email_is_unavailable() {
    assert_eq!(stored_email("Alice@Customer.com"), Ok(Some("alice@customer.com".into())));
    for inert in ["admin@local", "admin@LOCAL.", "operator", ""] {
        assert_eq!(stored_email(inert), Ok(None), "{inert}");
    }
    for unknown in ["alice@customer.com.", "alice @customer.com", "a@b@customer.com", "x@", "alice@exämple.com"] {
        assert_eq!(stored_email(unknown), Err(VerificationError::Unavailable), "{unknown}");
    }
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    person(&db, "admin@local");
    assert!(subject_accounts(&db, "bot", "support@customer.com").unwrap().is_empty(), "the bootstrap address is inert");
    // `validate_account_email` is a shape check only: this is storable.
    let odd = person(&db, "support@customer.com.");
    assert_eq!(subject_accounts(&db, "bot", "support@customer.com"), Err(VerificationError::Unavailable));
    // Its former address in the history is compared too.
    db.change_own_email(&odd, "odd@customer.com").unwrap();
    assert_eq!(subject_accounts(&db, "bot", "support@customer.com"), Err(VerificationError::Unavailable));
}

/// SR4 item 2 (i)/(ii): an address any account EVER held (history) is
/// refused at use time for every actor, even after it was given up.
#[test]
fn sr4_history_address_refused_at_use_time() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let bob = person(&db, "support@customer.com");
    db.change_own_email(&bob, "bob@customer.com").unwrap();
    let mailbox = TenantSource::fixture(TenantSourceKind::Delegate, "support@customer.com",
        &["support@customer.com"], &[GMAIL_READ]);
    let accounts = subject_accounts(&db, "bot", "support@customer.com").unwrap();
    assert_eq!(accounts, vec![SubjectAccount { user_id: String::new(), active: false, attested: false }]);
    assert_eq!(subject_state(&mailbox, &accounts), SubjectState::MailboxHeldByAccount);
    for actor in [Actor::System, Actor::Human(bob.as_str()), Actor::Human("")] {
        assert_eq!(admit_subject(&mailbox, actor, &accounts), Err(VerificationError::BindingMismatch));
    }
}

/// SR4 items 1 / 2(iv): the Admin notice names the subject's standing.
#[test]
fn sr4_subject_notice_surfaces_conflicts_and_missing_attestation() {
    let home = Home::new();
    home.sealed(SEALED);
    home.config("");
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    home.agent("identity = \"owner\"");
    assert_eq!(subject_notice(home.path(), "bot", Some(&db)), serde_json::Value::Null);
    // A delegate person without an attestation.
    home.agent("identity = \"delegate:alice@customer.com\"");
    let alice = person(&db, "alice@customer.com");
    assert_eq!(subject_notice(home.path(), "bot", Some(&db))["state"], "attestation_required");
    assert_eq!(subject_notice(home.path(), "bot", None)["state"], "users_unavailable");
    attest(&db, "bot", "alice@customer.com").unwrap();
    let notice = subject_notice(home.path(), "bot", Some(&db));
    assert_eq!((notice["state"].as_str(), notice["subject"].as_str()), (Some("attested_person"), Some("alice@customer.com")));
    // Alice gives the address up: only a former holder is left.
    db.change_own_email(&alice, "alice2@customer.com").unwrap();
    assert_eq!(subject_notice(home.path(), "bot", Some(&db))["state"], "person_inactive");
    // A staff member blocks a designated service mailbox: shown, still refused.
    home.agent("identity = \"delegate:support@customer.com\"");
    assert_eq!(subject_notice(home.path(), "bot", Some(&db))["state"], "service_mailbox");
    let staff = person(&db, "staff@customer.com");
    db.change_own_email(&staff, "Support@customer.com").unwrap();
    let notice = subject_notice(home.path(), "bot", Some(&db));
    assert_eq!(notice["state"], "service_mailbox_conflict");
    assert!(notice["message"].as_str().unwrap().contains("服務信箱"));
    // An unreadable configuration is unavailable, never "no tenant source".
    std::fs::write(home.path().join("config.toml"), "not = [toml").unwrap();
    assert_eq!(subject_notice(home.path(), "bot", Some(&db))["state"], "unavailable");
}

/// SR4 item 2(iv): a login address colliding with a tenant subject is named
/// for the Admin audit (the use stays refused).
#[test]
fn sr4_email_conflicts_name_mailbox_and_delegate() {
    let home = Home::new();
    home.config("");
    home.agent("identity = \"delegate:ceo@customer.com\"");
    assert!(email_conflicts(home.path(), "bob@customer.com").is_empty());
    assert_eq!(email_conflicts(home.path(), " Support@Customer.com"), vec![json!({"kind": "service_mailbox"})]);
    assert_eq!(email_conflicts(home.path(), "CEO@customer.com"),
        vec![json!({"kind": "delegate_subject", "agent_id": "bot"})]);
    assert!(email_conflicts(home.path(), "admin@local").is_empty(), "names no mailbox");
}

/// F5 UI-RPC: the dashboard view shows state and a fingerprint, never key
/// material; unreadable material is `unavailable`, never a fingerprint.
#[test]
fn f5_dashboard_view_fingerprints_without_key_material() {
    let home = Home::new();
    let view = dashboard_view(home.path());
    assert_eq!(view, json!({"delegate": {"state": "not_configured"}, "service_account": {"state": "not_configured"}}));

    // Delegate: fingerprint of the sealed record; a re-import changes it.
    home.sealed(SEALED);
    let first = dashboard_view(home.path());
    assert_eq!(first["delegate"]["state"], "installed");
    assert_eq!(first["delegate"]["client_email"], "dwd@project.iam.gserviceaccount.com");
    let fingerprint = first["delegate"]["fingerprint"].as_str().unwrap().to_owned();
    assert_eq!(fingerprint.len(), 64);
    assert!(!first.to_string().contains("body"), "{first}");
    home.sealed(&SEALED.replace("body", "other"));
    assert_ne!(dashboard_view(home.path())["delegate"]["fingerprint"].as_str(), Some(fingerprint.as_str()));
    home.sealed("not json");
    assert_eq!(dashboard_view(home.path())["delegate"], json!({"state": "unavailable"}));

    // Service account: fingerprint of the Google key id, never the key.
    let home = Home::new();
    home.config(SA_SECTION);
    assert_eq!(dashboard_view(home.path())["service_account"],
        json!({"state": "unavailable", "subject": "support@customer.com"}), "missing key file");
    std::fs::write(home.path().join("sa.json"), SA_KEY).unwrap();
    let no_id = dashboard_view(home.path());
    assert_eq!(no_id["service_account"]["state"], "installed");
    assert_eq!(no_id["service_account"]["fingerprint"], serde_json::Value::Null, "no key id ⇒ no fingerprint");
    let with_id = SA_KEY.replacen('{', r#"{"private_key_id":"abc123def456","#, 1);
    std::fs::write(home.path().join("sa.json"), &with_id).unwrap();
    let view = dashboard_view(home.path());
    let sa = &view["service_account"];
    assert_eq!((sa["state"].as_str(), sa["client_email"].as_str(), sa["subject"].as_str()),
        (Some("installed"), Some("sa@project.iam.gserviceaccount.com"), Some("support@customer.com")));
    let fingerprint = sa["fingerprint"].as_str().unwrap();
    assert_eq!(fingerprint.len(), 64);
    let text = view.to_string();
    for secret in ["PRIVATE KEY", "fixture", "abc123def456"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    // The displayed fingerprint is not the bridge's pinned material digest.
    assert_ne!(fingerprint, material_digest("service_account", with_id.as_bytes()));
    std::fs::write(home.path().join("config.toml"), "not = [toml").unwrap();
    assert_eq!(dashboard_view(home.path())["service_account"], json!({"state": "unavailable"}),
        "an unparsable config is unavailable, never \"not configured\"");
}
