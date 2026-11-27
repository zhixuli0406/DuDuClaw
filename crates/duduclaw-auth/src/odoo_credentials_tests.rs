use super::*;
use crate::{UserRole, test_support::test_db};

struct BoundSealer;
impl OdooSecretSealer for BoundSealer {
    fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = aad.to_vec();
        out.push(0);
        out.extend_from_slice(plaintext);
        Ok(out)
    }
    fn open(&self, ct: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
        let mut prefix = aad.to_vec();
        prefix.push(0);
        ct.strip_prefix(prefix.as_slice()).map(|s| Zeroizing::new(s.to_vec()))
            .ok_or_else(|| "binding mismatch".into())
    }
}
fn put(db: &UserDb, id: &str) {
    db.odoo_credential_put(id, "member@example.test", 7, "Member", "hidden-api-key", "server-a", &BoundSealer).unwrap();
}
#[test]
fn odoo_credential_lifecycle_and_metadata() {
    let (db, _home) = test_db();
    let user = db.create_user("member@test.invalid", "Member", "password", UserRole::Employee).unwrap();
    assert!(db.odoo_credential_status(&user.id).unwrap().is_none());
    put(&db, &user.id);
    let status = db.odoo_credential_status(&user.id).unwrap().unwrap();
    assert_eq!(status.rev, 1);
    assert!(!serde_json::to_string(&status).unwrap().contains("hidden-api-key"));
    assert!(!serde_json::to_string(&status).unwrap().contains("secret"));
    let secret = db.odoo_credential_unseal(&user.id, "server-a", &BoundSealer).unwrap().unwrap();
    assert_eq!(&*secret.secret, "hidden-api-key");
    assert!(!format!("{secret:?}").contains("hidden-api-key"));
    put(&db, &user.id);
    db.odoo_credential_mark_state(&user.id, 1, "invalid").unwrap();
    assert_eq!(db.odoo_credential_status(&user.id).unwrap().unwrap().rev, 2);
    assert_eq!(db.odoo_credential_status(&user.id).unwrap().unwrap().state, "ok");
    db.odoo_credential_touch(&user.id).unwrap();
    assert!(db.odoo_credential_status(&user.id).unwrap().unwrap().last_used_at.is_some());
    assert!(db.odoo_credential_delete(&user.id).unwrap());
    assert!(!db.odoo_credential_delete(&user.id).unwrap());
}
#[test]
fn odoo_credential_aad_prevents_cross_member_copy() {
    let (db, _home) = test_db();
    let a = db.create_user("a@test.invalid", "A", "password", UserRole::Employee).unwrap();
    let b = db.create_user("b@test.invalid", "B", "password", UserRole::Employee).unwrap();
    put(&db, &a.id);
    put(&db, &b.id);
    db.conn().execute("UPDATE member_odoo_credentials SET secret_ct=(SELECT secret_ct FROM member_odoo_credentials WHERE user_id=?1) WHERE user_id=?2", params![a.id, b.id]).unwrap();
    assert!(db.odoo_credential_unseal(&b.id, "server-a", &BoundSealer).is_err());
}
#[test]
fn odoo_credential_offboarding_and_inactive_users() {
    let (db, _home) = test_db();
    let user = db.create_user("a@test.invalid", "A", "password", UserRole::Employee).unwrap();
    put(&db, &user.id);
    db.conn().execute("UPDATE users SET status='suspended' WHERE id=?1", [&user.id]).unwrap();
    assert!(db.odoo_credential_unseal(&user.id, "server-a", &BoundSealer).unwrap().is_none());
    assert!(db.odoo_credential_put(&user.id, "a", 7, "A", "key", "server-a", &BoundSealer).is_err());
    db.conn().execute("UPDATE users SET status='offboarded' WHERE id=?1", [&user.id]).unwrap();
    assert!(db.odoo_credential_status(&user.id).unwrap().is_none());
}
#[test]
fn odoo_credential_server_and_state_gates() {
    let (db, _home) = test_db();
    let user = db.create_user("a@test.invalid", "A", "password", UserRole::Employee).unwrap();
    put(&db, &user.id);
    assert!(db.odoo_credential_unseal(&user.id, "server-b", &BoundSealer).unwrap().is_none());
    assert_eq!(db.odoo_credential_status(&user.id).unwrap().unwrap().state, "needs_reverify");
    for state in ["invalid", "needs_reverify"] {
        db.odoo_credential_mark_state(&user.id, 1, state).unwrap();
        assert!(db.odoo_credential_unseal(&user.id, "server-a", &BoundSealer).unwrap().is_none());
    }
    assert!(db.odoo_credential_mark_state(&user.id, 1, "ok").is_err());
    assert!(db.odoo_credential_unseal_for_reverification(&user.id, 1, &BoundSealer).unwrap().is_some());
    assert!(!db.odoo_credential_reverified(&user.id, 2, 7, "A", "server-b").unwrap());
    assert!(db.odoo_credential_reverified(&user.id, 1, 7, "A", "server-b").unwrap());
    assert!(db.odoo_credential_unseal(&user.id, "server-b", &BoundSealer).unwrap().is_some());
    db.odoo_credential_delete(&user.id).unwrap();
    assert!(!db.odoo_credential_reverified(&user.id, 2, 7, "A", "server-b").unwrap());
}
#[test]
fn odoo_credential_admin_list_is_masked_and_live() {
    let (db, _home) = test_db();
    let member = db.create_user("m@test.invalid", "Member", "password", UserRole::Employee).unwrap();
    let admin = db.create_user("a@test.invalid", "Admin", "password", UserRole::Admin).unwrap();
    put(&db, &member.id);
    assert!(db.odoo_credentials_admin_list(&member.id).is_err());
    assert!(db.odoo_credential_admin_revoke(&member.id, &admin.id).is_err());
    let rows = db.odoo_credentials_admin_list(&admin.id).unwrap();
    assert_eq!(rows.len(), 1);
    let rendered = serde_json::to_string(&rows).unwrap();
    assert!(!rendered.contains("secret"));
    assert!(!rendered.contains("hidden-api-key"));
    db.conn().execute("UPDATE users SET role='employee' WHERE id=?1", [&admin.id]).unwrap();
    assert!(db.odoo_credentials_admin_list(&admin.id).is_err());
}
#[test]
fn odoo_credential_init_is_idempotent_and_status_does_not_select_ciphertext() {
    let (db, home) = test_db();
    let user = db.create_user("a@test.invalid", "A", "password", UserRole::Employee).unwrap();
    put(&db, &user.id);
    drop(db);
    let db = UserDb::new(home.path()).unwrap();
    assert_eq!(db.odoo_credential_status(&user.id).unwrap().unwrap().rev, 1);
    // Removing the ciphertext column proves metadata queries do not select it.
    db.conn().execute_batch("ALTER TABLE member_odoo_credentials DROP COLUMN secret_ct;").unwrap();
    assert!(db.odoo_credential_status(&user.id).unwrap().is_some());
}
