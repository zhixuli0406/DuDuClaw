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
    assert_eq!(subject_accounts(&db, "alice@customer.com").unwrap(),
        vec![SubjectAccount { user_id: alice.id, active: true }]);
    assert!(subject_accounts(&db, "support@customer.com").unwrap().is_empty());
}

#[test]
fn e4_subject_rule_is_exact() {
    let person = TenantSource::fixture(TenantSourceKind::Delegate, "alice@customer.com", &[], &[GMAIL_READ]);
    let alice = [SubjectAccount { user_id: "alice-id".into(), active: true }];
    assert_eq!(admit_subject(&person, Actor::Human("alice-id"), &alice), Ok(SubjectUse::RequestingUser));
    for actor in [Actor::Human("bob-id"), Actor::Human("ALICE-ID"), Actor::Human("alice-id "), Actor::Human(""), Actor::System] {
        assert_eq!(admit_subject(&person, actor, &alice), Err(VerificationError::BindingMismatch), "{actor:?}");
    }
    let inactive = [SubjectAccount { user_id: "alice-id".into(), active: false }];
    assert_eq!(admit_subject(&person, Actor::Human("alice-id"), &inactive), Err(VerificationError::BindingMismatch));
    let twice = [alice[0].clone(), SubjectAccount { user_id: "alice-2".into(), active: true }];
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
