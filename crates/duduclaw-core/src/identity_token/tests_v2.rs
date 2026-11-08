//! F6m (S8): agent token v2, lifecycle-bound delegation contexts and internal
//! request signatures. Fixtures write the gateway's confirmed employee index
//! (`migration-authority/apply.db` `confirmed_employees`) with the same
//! schema as `handlers/migration_authority_employee.rs` `ensure_employee_schema`.
use super::*;
use rusqlite::{params, Connection};

const ALICE_ID: [u8; 16] = [0xa1; 16];
const LATER_ID: [u8; 16] = [0xb2; 16];

fn test_key() -> Vec<u8> {
    (0u8..32).collect()
}

fn grace_config(home: &Path) {
    std::fs::write(home.join("config.toml"), format!(
        "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = {:?}\n",
        (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())).unwrap();
}

fn db_path(home: &Path) -> PathBuf {
    home.join(APPLY_DB_DIR).join(APPLY_DB_FILE)
}

fn private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// A home with the employee index. `incarnation_column = false` is the
/// pre-F6a schema (no `incarnation` column yet).
fn index(incarnation_column: bool) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(APPLY_DB_DIR)).unwrap();
    let db = Connection::open(db_path(home.path())).unwrap();
    db.execute_batch("CREATE TABLE confirmed_employees (
        label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,digest BLOB NOT NULL,
        receipt TEXT NOT NULL,tenant_id TEXT);").unwrap();
    if incarnation_column {
        db.execute_batch("ALTER TABLE confirmed_employees ADD COLUMN incarnation INTEGER NOT NULL DEFAULT 0;")
            .unwrap();
    }
    drop(db);
    private(&db_path(home.path()));
    home
}

fn receipt(label: &str, employee: [u8; 16], disabled: bool) -> String {
    serde_json::json!({
        "label": label, "employee": employee, "uid": 70_001, "bytes": 67_108_864_u64,
        "inodes": 4096, "disabled": disabled,
    }).to_string()
}

/// Upsert the confirmed row for `label` (a new id on the label replaces the
/// row, exactly as `save_confirmed_receipt` does on conflict).
fn confirm(home: &Path, label: &str, employee: [u8; 16], disabled: bool) {
    let db = Connection::open(db_path(home)).unwrap();
    db.execute("INSERT INTO confirmed_employees(label,operation,digest,receipt) VALUES(?1,?2,x'00',?3)
        ON CONFLICT(label) DO UPDATE SET operation=excluded.operation,receipt=excluded.receipt",
        params![label, format!("op-{label}-{}", hex_lower(&employee)), receipt(label, employee, disabled)])
        .unwrap();
}

fn set_incarnation(home: &Path, label: &str, incarnation: i64) {
    let db = Connection::open(db_path(home)).unwrap();
    assert_eq!(db.execute("UPDATE confirmed_employees SET incarnation=?1 WHERE label=?2",
        params![incarnation, label]).unwrap(), 1);
}

fn env_token(home: &Path, label: &str) -> Option<String> {
    agent_identity_env_vars(home, label).into_iter()
        .find(|(name, _)| name == ENV_AGENT_TOKEN).map(|(_, value)| value)
}

fn alice(incarnation: u64) -> AgentIncarnation {
    AgentIncarnation { employee: ALICE_ID, incarnation }
}

#[test]
fn token_v2_binds_employee_and_incarnation() {
    let k = test_key();
    let binding = alice(3);
    let token = mint_token_v2(&k, "alice", &binding);
    assert!(token.starts_with(AGENT_TOKEN_V2_PREFIX));
    assert_eq!(token.len(), AGENT_TOKEN_V2_PREFIX.len() + 64);
    // Independent construction of the documented message.
    let mut mac = HmacSha256::new_from_slice(&k).unwrap();
    mac.update(b"duduclaw-agent-token-v2");
    mac.update(&5u32.to_be_bytes());
    mac.update(b"alice");
    mac.update(hex_lower(&ALICE_ID).as_bytes());
    mac.update(&3u64.to_be_bytes());
    assert_eq!(token, format!("v2.{}", hex_lower(&mac.finalize().into_bytes())));

    assert!(verify_token_v2(&k, "alice", &binding, &token));
    assert!(verify_token_v2(&k, "alice", &binding, &format!("  {token}\n")));
    assert!(!verify_token_v2(&k, "bob", &binding, &token));
    assert!(!verify_token_v2(&k, "alice", &AgentIncarnation { employee: LATER_ID, ..binding }, &token));
    assert!(!verify_token_v2(&k, "alice", &alice(2), &token));
    assert!(!verify_token_v2(&k, "alice", &alice(4), &token));
    assert!(!verify_token_v2(&[9; 32], "alice", &binding, &token));
    // v1 and v2 never cross-verify, and the prefix is required.
    assert!(!verify_token(&k, "alice", &token));
    assert!(!verify_token_v2(&k, "alice", &binding, &mint_token(&k, "alice")));
    assert!(!verify_token_v2(&k, "alice", &binding, token.trim_start_matches(AGENT_TOKEN_V2_PREFIX)));
    assert!(!verify_token_v2(&k, "alice", &binding, "v2.zz"));

    // Minted from, and verified against, the live index row.
    let home = index(true);
    confirm(home.path(), "alice", ALICE_ID, false);
    set_incarnation(home.path(), "alice", 3);
    let key = ensure_key(home.path()).unwrap();
    assert_eq!(protected_employee(home.path(), "alice"), Ok(ProtectedEmployee::Live(binding)));
    let issued = env_token(home.path(), "alice").unwrap();
    assert_eq!(issued, mint_token_v2(&key, "alice", &binding));
    assert_eq!(current_agent_token(home.path(), &key, "alice").as_deref(), Some(issued.as_str()));
    assert!(verify_agent_token(home.path(), &key, "alice", &issued));
    for require in [false, true] {
        assert_eq!(verify_claim(home.path(), "alice", &issued, require), IdentityVerdict::Verified);
    }
}

#[test]
fn token_v1_refused_for_protected_employee() {
    let home = index(true);
    grace_config(home.path());
    confirm(home.path(), "alice", ALICE_ID, false);
    let key = ensure_key(home.path()).unwrap();
    let v1 = mint_token(&key, "alice");
    assert!(!verify_agent_token(home.path(), &key, "alice", &v1));
    // Soft mode never applies to a protected employee: v1, missing and
    // garbage tokens are refused in both modes, not merely Unverified.
    for require in [false, true] {
        for token in [v1.as_str(), "", "00", "v2.00"] {
            assert_eq!(verify_claim(home.path(), "alice", token, require), IdentityVerdict::Rejected, "{token:?}");
        }
    }
    assert!(verify_bridge_claim(home.path(), "alice", "alice", &v1).is_err());
    // An unprotected label in the same install keeps v1 and soft mode.
    assert_eq!(verify_claim(home.path(), "bob", &mint_token(&key, "bob"), true), IdentityVerdict::Verified);
    assert_eq!(verify_claim(home.path(), "bob", "", false), IdentityVerdict::Unverified);
    // Without a key a protected employee is never Disabled.
    let keyless = index(true);
    grace_config(keyless.path());
    confirm(keyless.path(), "alice", ALICE_ID, false);
    assert_eq!(verify_claim(keyless.path(), "alice", "", false), IdentityVerdict::Rejected);
    assert_eq!(verify_claim(keyless.path(), "bob", "", false), IdentityVerdict::Disabled);
}

#[test]
fn token_v1_still_accepted_without_protected_mode() {
    // No index at all: an ordinary install, byte-identical v1 behaviour.
    let home = tempfile::tempdir().unwrap();
    grace_config(home.path());
    assert_eq!(protected_employee(home.path(), "alice"), Ok(ProtectedEmployee::Unprotected));
    assert_eq!(agent_identity_env_vars(home.path(), "alice").len(), 1, "no key ⇒ id only");
    let key = ensure_key(home.path()).unwrap();
    let issued = env_token(home.path(), "alice").unwrap();
    assert_eq!(issued, mint_token(&key, "alice"));
    assert!(verify_agent_token(home.path(), &key, "alice", &issued));
    assert_eq!(verify_claim(home.path(), "alice", &issued, true), IdentityVerdict::Verified);
    assert_eq!(verify_claim(home.path(), "alice", "", false), IdentityVerdict::Unverified);
    assert_eq!(verify_claim(home.path(), "alice", &mint_token_v2(&key, "alice", &alice(0)), true),
        IdentityVerdict::Rejected, "v2 is only for protected employees");
    // v1 delegation contexts are unchanged too (no binding fields on the wire).
    let context = mint_delegation_context_in(home.path(), &key, "alice", "alice", "alice", 0, None, u64::MAX).unwrap();
    assert!(!context.contains("worker_employee") && !context.contains("worker_incarnation"));
    assert_eq!(verify_delegation_context_in(home.path(), &key, "alice", None, &context, 1).unwrap().version, 1);
    assert!(verify_delegation_context(&key, "alice", None, &context, 1).is_some());

    // An index directory without apply.db, an index without the table and an
    // index without a row for the label are all unprotected for that label.
    std::fs::create_dir(home.path().join(APPLY_DB_DIR)).unwrap();
    assert_eq!(protected_employee(home.path(), "alice"), Ok(ProtectedEmployee::Unprotected));
    drop(Connection::open(db_path(home.path())).unwrap());
    private(&db_path(home.path()));
    assert_eq!(protected_employee(home.path(), "alice"), Ok(ProtectedEmployee::Unprotected));
    let indexed = index(true);
    confirm(indexed.path(), "carol", ALICE_ID, false);
    assert_eq!(protected_employee(indexed.path(), "alice"), Ok(ProtectedEmployee::Unprotected));
    assert_eq!(protected_employee(indexed.path(), ""), Ok(ProtectedEmployee::Unprotected));
}

#[test]
fn token_v2_refused_after_tombstone() {
    let home = index(true);
    grace_config(home.path());
    confirm(home.path(), "alice", ALICE_ID, false);
    let key = ensure_key(home.path()).unwrap();
    let issued = env_token(home.path(), "alice").unwrap();
    assert_eq!(verify_claim(home.path(), "alice", &issued, true), IdentityVerdict::Verified);
    confirm(home.path(), "alice", ALICE_ID, true);
    assert_eq!(protected_employee(home.path(), "alice"), Ok(ProtectedEmployee::Tombstoned));
    let v1 = mint_token(&key, "alice");
    for require in [false, true] {
        for token in [issued.as_str(), v1.as_str(), ""] {
            assert_eq!(verify_claim(home.path(), "alice", token, require), IdentityVerdict::Rejected);
        }
    }
    assert!(!verify_agent_token(home.path(), &key, "alice", &issued));
    assert_eq!(current_agent_token(home.path(), &key, "alice"), None);
    assert_eq!(env_token(home.path(), "alice"), None, "a tombstoned employee is issued no token");
}

#[test]
fn token_rotated_on_incarnation_change() {
    let home = index(true);
    confirm(home.path(), "alice", ALICE_ID, false);
    ensure_key(home.path()).unwrap();
    let first = env_token(home.path(), "alice").unwrap();
    assert_eq!(env_token(home.path(), "alice").unwrap(), first, "stable within one incarnation");
    // Same-id restore (F6c) bumps the incarnation by one.
    set_incarnation(home.path(), "alice", 1);
    let restored = env_token(home.path(), "alice").unwrap();
    assert_ne!(restored, first);
    assert_eq!(verify_claim(home.path(), "alice", &first, true), IdentityVerdict::Rejected);
    assert_eq!(verify_claim(home.path(), "alice", &restored, true), IdentityVerdict::Verified);
    // Adoption (F6g) / label reuse: a new employee id on the label, incarnation 0.
    confirm(home.path(), "alice", LATER_ID, false);
    set_incarnation(home.path(), "alice", 0);
    let adopted = env_token(home.path(), "alice").unwrap();
    assert_ne!(adopted, first);
    assert_ne!(adopted, restored);
    for old in [&first, &restored] {
        assert_eq!(verify_claim(home.path(), "alice", old, true), IdentityVerdict::Rejected,
            "a token of the old id must not verify on the reused label");
    }
    assert_eq!(verify_claim(home.path(), "alice", &adopted, true), IdentityVerdict::Verified);
}

#[test]
fn delegation_context_bound_to_employee_id() {
    let home = index(true);
    confirm(home.path(), "alice", ALICE_ID, false);
    let key = ensure_key(home.path()).unwrap();
    let mint = || mint_delegation_context_in(home.path(), &key, "alice", "alice", "alice", 0, None, u64::MAX).unwrap();
    let verify = |token: &str| verify_delegation_context_in(home.path(), &key, "alice", None, token, 1);
    let token = mint();
    let context = verify(&token).unwrap();
    assert_eq!(context.version, 2);
    assert_eq!(context.worker_binding(), Some(alice(0)));
    assert_eq!(context.worker_employee.as_deref(), Some(hex_lower(&ALICE_ID).as_str()));
    // A label-only (v1) context is refused for a protected worker, and the
    // label-only verifier refuses a v2 context.
    let v1 = mint_delegation_context(&key, "alice", "alice", "alice", 0, None, u64::MAX).unwrap();
    assert!(verify(&v1).is_none());
    assert!(verify_delegation_context(&key, "alice", None, &token, 1).is_none());
    assert!(verify_delegation_context_for(&key, "alice", Some(&alice(0)), None, &token, 1).is_some());
    assert!(verify_delegation_context_for(&key, "alice", Some(&alice(1)), None, &token, 1).is_none());
    // The binding is covered by the signature.
    for (field, value) in [
        ("worker_employee", serde_json::json!(hex_lower(&LATER_ID))),
        ("worker_incarnation", serde_json::json!(1)),
        ("version", serde_json::json!(1)),
    ] {
        let mut forged: serde_json::Value = serde_json::from_str(&token).unwrap();
        forged["context"][field] = value;
        assert!(verify(&forged.to_string()).is_none(), "accepted changed {field}");
    }
    let mut stripped: serde_json::Value = serde_json::from_str(&token).unwrap();
    let object = stripped["context"].as_object_mut().unwrap();
    object.remove("worker_employee");
    object.remove("worker_incarnation");
    object.insert("version".into(), serde_json::json!(1));
    assert!(verify(&stripped.to_string()).is_none());
    // Old incarnation and old id on a reused label are refused.
    set_incarnation(home.path(), "alice", 1);
    assert!(verify(&token).is_none(), "context of the old incarnation");
    let restored = mint();
    assert_eq!(verify(&restored).unwrap().worker_binding(), Some(alice(1)));
    confirm(home.path(), "alice", LATER_ID, false);
    assert!(verify(&restored).is_none(), "context of the old employee id");
    let latest = mint();
    let later = AgentIncarnation { employee: LATER_ID, incarnation: 0 };
    assert_eq!(verify(&latest).unwrap().worker_binding(), Some(later));
    // A non-canonical (uppercase) employee hex is refused even with a valid MAC.
    let mut signed: SignedDelegationContext = serde_json::from_str(&latest).unwrap();
    signed.context.worker_employee = Some(hex_lower(&LATER_ID).to_ascii_uppercase());
    signed.signature = mint_token(&key, &signed.context.mac_payload());
    assert!(verify_delegation_context_for(&key, "alice", Some(&later), None,
        &serde_json::to_string(&signed).unwrap(), 1).is_none());
    // Tombstoned: nothing minted, nothing verified.
    confirm(home.path(), "alice", LATER_ID, true);
    assert!(verify(&latest).is_none());
    assert!(mint_delegation_context_in(home.path(), &key, "alice", "alice", "alice", 0, None, u64::MAX).is_none());
}

#[test]
fn internal_signature_bound_to_employee_id() {
    let home = index(true);
    confirm(home.path(), "alice", ALICE_ID, false);
    let key = ensure_key(home.path()).unwrap();
    let internal = b"ddc_prod_0123456789abcdef0123456789abcdef";
    let body = b"{\"op\":\"status\"}";
    // The client signs with the token it was spawned with (v2 here).
    let client = env_token(home.path(), "alice").unwrap();
    assert!(client.starts_with(AGENT_TOKEN_V2_PREFIX));
    let signature = internal_request_signature(internal, "alice", &client, "1700000000", "00ff", body);
    let receiver_accepts = |signature: &str| current_agent_token(home.path(), &key, "alice")
        .is_some_and(|expected| verify_internal_request_signature(
            internal, "alice", &expected, "1700000000", "00ff", body, signature));
    assert!(receiver_accepts(&signature));
    // A request signed over the label-only v1 token is refused.
    let v1 = internal_request_signature(internal, "alice", &mint_token(&key, "alice"), "1700000000", "00ff", body);
    assert!(!receiver_accepts(&v1));
    // A later employee on the same label: the old employee's signature fails.
    confirm(home.path(), "alice", LATER_ID, false);
    assert!(!receiver_accepts(&signature));
    let later = internal_request_signature(internal, "alice", &env_token(home.path(), "alice").unwrap(),
        "1700000000", "00ff", body);
    assert!(receiver_accepts(&later));
    // So does a signature of an older incarnation of the same id.
    set_incarnation(home.path(), "alice", 1);
    assert!(!receiver_accepts(&later));
    // Tombstoned: the receiver has no expected token at all.
    confirm(home.path(), "alice", LATER_ID, true);
    assert!(!receiver_accepts(&later));
}

#[test]
fn legacy_row_without_incarnation_column_reads_incarnation_zero() {
    let home = index(false);
    confirm(home.path(), "alice", ALICE_ID, false);
    assert_eq!(protected_employee(home.path(), "alice"), Ok(ProtectedEmployee::Live(alice(0))));
    let key = ensure_key(home.path()).unwrap();
    assert_eq!(env_token(home.path(), "alice").unwrap(), mint_token_v2(&key, "alice", &alice(0)));
}

#[test]
fn unreadable_or_corrupt_index_refuses_protected_tokens() {
    let assert_closed = |home: &Path, what: &str| {
        assert_eq!(protected_employee(home, "alice"), Err(LifecycleUnavailable), "{what}");
        let key = load_key(home).unwrap();
        for token in [mint_token(&key, "alice"), mint_token_v2(&key, "alice", &alice(0))] {
            for require in [false, true] {
                assert_eq!(verify_claim(home, "alice", &token, require), IdentityVerdict::Rejected, "{what}");
            }
            assert!(!verify_agent_token(home, &key, "alice", &token), "{what}");
        }
        assert_eq!(env_token(home, "alice"), None, "{what}");
        assert!(mint_delegation_context_in(home, &key, "alice", "alice", "alice", 0, None, u64::MAX).is_none());
    };
    // Not a database.
    let home = index(true);
    grace_config(home.path());
    ensure_key(home.path()).unwrap();
    std::fs::write(db_path(home.path()), b"not a database at all, just bytes").unwrap();
    assert_closed(home.path(), "garbage store");
    // Corrupt receipt, receipt for another label, negative or NULL incarnation.
    for (receipt_json, incarnation) in [
        ("{".to_owned(), Some(0)),
        (receipt("mallory", ALICE_ID, false), Some(0)),
        (receipt("alice", ALICE_ID, false), Some(-1)),
        (receipt("alice", ALICE_ID, false), None),
    ] {
        let home = tempfile::tempdir().unwrap();
        grace_config(home.path());
        ensure_key(home.path()).unwrap();
        std::fs::create_dir(home.path().join(APPLY_DB_DIR)).unwrap();
        let db = Connection::open(db_path(home.path())).unwrap();
        db.execute_batch("CREATE TABLE confirmed_employees (label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,
            digest BLOB NOT NULL,receipt TEXT NOT NULL,tenant_id TEXT,incarnation INTEGER);").unwrap();
        db.execute("INSERT INTO confirmed_employees(label,operation,digest,receipt,incarnation) VALUES('alice','op',x'00',?1,?2)",
            params![receipt_json, incarnation]).unwrap();
        drop(db);
        private(&db_path(home.path()));
        assert_closed(home.path(), &format!("row {receipt_json} / {incarnation:?}"));
    }
    // The index directory itself is not a directory.
    let home = tempfile::tempdir().unwrap();
    ensure_key(home.path()).unwrap();
    std::fs::write(home.path().join(APPLY_DB_DIR), b"").unwrap();
    assert_closed(home.path(), "index dir is a file");
}

#[cfg(unix)]
#[test]
fn unsafe_index_store_refuses_protected_tokens() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let check = |home: &Path, what: &str| {
        assert_eq!(protected_employee(home, "alice"), Err(LifecycleUnavailable), "{what}");
        let key = load_key(home).unwrap();
        assert_eq!(verify_claim(home, "alice", &mint_token(&key, "alice"), true), IdentityVerdict::Rejected);
        assert_eq!(env_token(home, "alice"), None);
    };
    let fixture = || {
        let home = index(true);
        confirm(home.path(), "alice", ALICE_ID, false);
        ensure_key(home.path()).unwrap();
        home
    };
    // Group/other writable store.
    let home = fixture();
    std::fs::set_permissions(db_path(home.path()), std::fs::Permissions::from_mode(0o666)).unwrap();
    check(home.path(), "world-writable");
    // Hard-linked store.
    let home = fixture();
    std::fs::hard_link(db_path(home.path()), home.path().join("alias.db")).unwrap();
    check(home.path(), "hard link");
    // Symlinked store and symlinked directory, even to a valid index.
    let elsewhere = fixture();
    let home = fixture();
    std::fs::remove_file(db_path(home.path())).unwrap();
    symlink(db_path(elsewhere.path()), db_path(home.path())).unwrap();
    check(home.path(), "symlinked store");
    let home = fixture();
    std::fs::remove_file(db_path(home.path())).unwrap();
    std::fs::remove_dir(home.path().join(APPLY_DB_DIR)).unwrap();
    symlink(elsewhere.path().join(APPLY_DB_DIR), home.path().join(APPLY_DB_DIR)).unwrap();
    check(home.path(), "symlinked directory");
}
