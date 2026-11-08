//! v1.81 P1: Personal employee visibility predicates over a real users.db
//! (hardened opener, private test home).
use super::*;
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use rusqlite::TransactionBehavior;

pub(crate) const PERSONAL: &str = "alice-personal";
pub(crate) const SHARED: &str = "team";

/// alice (Employee) owns the Personal employee `alice-personal` and the
/// Shared employee `team`; bob (Manager) is alice's manager; carol is Admin.
pub(crate) struct Home {
    pub dir: tempfile::TempDir,
    pub db: UserDb,
    pub alice: String,
    pub bob: String,
    pub admin: String,
}

impl Home {
    pub fn path(&self) -> &Path { self.dir.path() }
}

pub(crate) fn home() -> Home {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let db = UserDb::new(&dir.path().join(USERS_DB)).unwrap();
    private_store(dir.path());
    let alice = db.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
    let bob = db.create_user("bob@example.test", "Bob", "password1", UserRole::Manager).unwrap().id;
    let admin = db.create_user("carol@example.test", "Carol", "password1", UserRole::Admin).unwrap().id;
    db.set_manager(&alice, Some(&bob)).unwrap();
    db.bind_agent(&alice, SHARED, AccessLevel::Owner).unwrap();
    record_personal(dir.path(), PERSONAL, &alice);
    private_store(dir.path());
    Home { dir, db, alice, bob, admin }
}

/// The hardened opener refuses group/other-writable stores and sidecars.
fn private_store(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for name in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal"] {
        let path = home.join(name);
        if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

/// The employee apply path's users.db write: mirror row + owner binding.
pub(crate) fn record_personal(home: &Path, agent: &str, owner: &str) {
    let mut conn = rusqlite::Connection::open(home.join(USERS_DB)).unwrap();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    assert!(duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, agent, owner).unwrap());
    assert!(duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, agent, owner).unwrap());
    tx.commit().unwrap();
}

#[test]
fn p1_reader_is_owner_admin_break_glass_or_refused() {
    let h = home();
    assert_eq!(personal_owner(h.path(), PERSONAL), Ok(Some(h.alice.clone())));
    assert_eq!(personal_owner(h.path(), SHARED), Ok(None));
    assert_eq!(classify_reader(h.path(), true, &h.alice, false, PERSONAL), Ok(Reader::Owner));
    assert_eq!(classify_reader(h.path(), true, &h.admin, true, PERSONAL),
        Ok(Reader::BreakGlass { owner: h.alice.clone() }));
    // The owner's manager (and the legacy `system` admin context) get nothing.
    assert_eq!(classify_reader(h.path(), true, &h.bob, false, PERSONAL), Ok(Reader::Refused));
    assert_eq!(classify_reader(h.path(), true, "system", true, PERSONAL), Ok(Reader::Refused));
    // Shared employees keep the existing checks.
    for user in [&h.alice, &h.bob, &h.admin] {
        assert_eq!(classify_reader(h.path(), true, user, user == &h.admin, SHARED), Ok(Reader::Unrestricted));
    }
    // Listing ⇔ access: an Admin's cross-agent listings hide it, the owner's do not.
    assert_eq!(hidden_from(h.path(), true, &h.admin), Ok([PERSONAL.to_string()].into()));
    assert_eq!(hidden_from(h.path(), true, &h.alice), Ok(BTreeSet::new()));

    // The manager's live context has no derived grant (team_grants_on skips it).
    let manager = h.db.live_user_context(&h.bob).unwrap().unwrap();
    assert!(manager.can_call_agent(SHARED) && manager.can_assign_to(SHARED));
    assert!(!manager.can_call_agent(PERSONAL) && !manager.can_assign_to(PERSONAL));
}

#[test]
fn p1_unreadable_mirror_fails_closed() {
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(personal_owner(empty.path(), PERSONAL), Err(Unavailable));
    assert_eq!(classify_reader(empty.path(), true, "u", true, PERSONAL), Err(Unavailable));
    assert_eq!(hidden_from(empty.path(), true, "u"), Err(Unavailable));
    assert_eq!(management_refusal(empty.path(), true, SHARED, PERSONAL), Some(OWNERSHIP_UNAVAILABLE));
}

#[test]
fn p1_management_refusal_only_for_another_employees_personal_target() {
    let h = home();
    assert_eq!(management_refusal(h.path(), true, SHARED, PERSONAL), Some(PERSONAL_TARGET_REFUSED));
    assert_eq!(management_refusal(h.path(), true, "", PERSONAL), Some(PERSONAL_TARGET_REFUSED));
    assert_eq!(management_refusal(h.path(), true, PERSONAL, PERSONAL), None, "the employee itself");
    assert_eq!(management_refusal(h.path(), true, PERSONAL, SHARED), None, "Shared targets unchanged");
}

#[test]
fn p1_unprotected_install_unchanged() {
    let h = home();
    // Outside protected mode nothing is read, even with a mirror row present
    // or no users.db at all.
    let missing = Path::new("/nonexistent/duduclaw-home");
    for home in [h.path(), missing] {
        for (user, admin) in [(h.admin.as_str(), true), (h.bob.as_str(), false), ("system", true)] {
            assert_eq!(classify_reader(home, false, user, admin, PERSONAL), Ok(Reader::Unrestricted));
        }
        assert_eq!(hidden_from(home, false, &h.admin), Ok(BTreeSet::new()));
        assert_eq!(management_refusal(home, false, SHARED, PERSONAL), None);
    }
    // A home without Personal employees (every non-protected install) keeps
    // the Shared team behaviour: the manager's derived grant is unchanged.
    let plain = crate::fs_safe::private_test_home().unwrap();
    let db = UserDb::new(&plain.path().join(USERS_DB)).unwrap();
    private_store(plain.path());
    let emp = db.create_user("e@example.test", "E", "password1", UserRole::Employee).unwrap().id;
    let mgr = db.create_user("m@example.test", "M", "password1", UserRole::Manager).unwrap().id;
    db.set_manager(&emp, Some(&mgr)).unwrap();
    db.bind_agent(&emp, "sales", AccessLevel::Owner).unwrap();
    let access = db.effective_access(&mgr).unwrap().unwrap();
    assert_eq!(access.team["sales"].level, AccessLevel::Operator);
    assert_eq!(db.overseen_reports(&mgr, "sales").unwrap().len(), 1);
    assert_eq!(hidden_from(plain.path(), true, &mgr), Ok(BTreeSet::new()));
    assert_eq!(classify_reader(plain.path(), true, &mgr, false, "sales"), Ok(Reader::Unrestricted));
}
