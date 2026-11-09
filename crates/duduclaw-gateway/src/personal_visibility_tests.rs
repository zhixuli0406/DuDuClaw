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
pub(crate) fn private_store(home: &Path) {
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

// ── v1.81 P1b ────────────────────────────────────────────────────────────

pub(crate) const EPH: &str = "eph-0123456789ab";

/// An ephemeral scaffold whose sidecar names `sidecar_parent` and whose
/// host-owned org store entry reports to `org_parent`.
pub(crate) fn ephemeral(home: &Path, id: &str, sidecar_parent: &str, org_parent: &str) {
    let dir = home.join("agents").join(".ephemeral").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"eph\"\n").unwrap();
    std::fs::write(dir.join("ephemeral.toml"), format!(
        "parent = \"{sidecar_parent}\"\ntier = \"standard\"\ncreated_at = \"2026-11-08T00:00:00Z\"\n\
         expires_at = \"2026-11-09T00:00:00Z\"\n")).unwrap();
    {
        let org = duduclaw_core::org_store::lock_org_mutations(home).unwrap();
        duduclaw_core::org_store::upsert(&org, id, duduclaw_core::org_store::OrgEntry::new(org_parent, "")).unwrap();
    }
}

#[test]
fn p1b_ephemeral_subagent_is_its_parent_for_own_data() {
    let h = home();
    ephemeral(h.path(), EPH, PERSONAL, PERSONAL);
    assert_eq!(acting_employee(h.path(), EPH).as_deref(), Some(PERSONAL));
    assert_eq!(acting_employee(h.path(), SHARED).as_deref(), Some(SHARED), "regular ids are themselves");
    assert!(same_employee(h.path(), EPH, PERSONAL));
    assert_eq!(management_refusal(h.path(), true, EPH, PERSONAL), None);
    assert_eq!(hidden_from_employee(h.path(), true, EPH), Ok(BTreeSet::new()));
    // The sidecar alone is not trusted: the host org record must agree.
    const FORGED: &str = "eph-ffffffffffff";
    ephemeral(h.path(), FORGED, PERSONAL, SHARED);
    assert_eq!(acting_employee(h.path(), FORGED), None);
    assert!(!same_employee(h.path(), FORGED, PERSONAL));
    assert_eq!(management_refusal(h.path(), true, FORGED, PERSONAL), Some(PERSONAL_TARGET_REFUSED));
    assert_eq!(hidden_from_employee(h.path(), true, FORGED), Ok([PERSONAL.to_string()].into()));
    // An ephemeral of a Shared employee is not the Personal employee either.
    const OTHER: &str = "eph-aaaaaaaaaaaa";
    ephemeral(h.path(), OTHER, SHARED, SHARED);
    assert_eq!(acting_employee(h.path(), OTHER).as_deref(), Some(SHARED));
    assert_eq!(management_refusal(h.path(), true, OTHER, PERSONAL), Some(PERSONAL_TARGET_REFUSED));
    // Unknown scaffold or blank id: no employee.
    assert_eq!(acting_employee(h.path(), "eph-000000000000"), None);
    assert_eq!(acting_employee(h.path(), ""), None);
    assert!(!same_employee(h.path(), "", ""));
}

#[test]
fn p1b_hidden_from_employee_hides_every_other_personal_employee() {
    let h = home();
    record_personal(h.path(), "bob-personal", &h.bob);
    private_store(h.path());
    let both: BTreeSet<String> = [PERSONAL.to_string(), "bob-personal".to_string()].into();
    // Another AI employee (Shared, external client id): every Personal employee.
    assert_eq!(hidden_from_employee(h.path(), true, SHARED), Ok(both.clone()));
    assert_eq!(hidden_from_employee(h.path(), true, "external-client"), Ok(both));
    // A Personal employee sees itself, never another person's.
    assert_eq!(hidden_from_employee(h.path(), true, PERSONAL), Ok(["bob-personal".to_string()].into()));
    // Fail closed / unprotected.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(hidden_from_employee(empty.path(), true, SHARED), Err(Unavailable));
    assert_eq!(hidden_from_employee(Path::new("/nonexistent/duduclaw-home"), false, SHARED), Ok(BTreeSet::new()));
    assert_eq!(DataCategory::Work.as_str(), "work");
}

// ── v1.81 P1h (N2): owner-map caches ────────────────────────────────────

#[tokio::test]
async fn p1h_owner_map_opens_users_db_at_most_once_per_request() {
    let h = home();
    let before = open_count(h.path());
    // One request checking N rows (the shape of a list handler running the
    // ACL per row, plus the hidden set of the reply filter).
    with_break_glass_scope(async {
        for _ in 0..100 {
            for agent in [PERSONAL, SHARED, "unknown-employee"] {
                let _ = acl_denies(h.path(), true, &h.admin, agent);
                let _ = acl_denies(h.path(), true, &h.alice, agent);
            }
        }
        assert!(acl_denies(h.path(), true, &h.admin, PERSONAL));
        assert!(!acl_denies(h.path(), true, &h.alice, PERSONAL));
        assert_eq!(hidden_from(h.path(), true, &h.admin), Ok([PERSONAL.to_string()].into()));
    })
    .await;
    assert!(open_count(h.path()) - before <= 1, "{} opens in one request", open_count(h.path()) - before);
    // Another request under the same ACL generation: at most one open too.
    let second = open_count(h.path());
    with_break_glass_scope(async {
        for _ in 0..100 {
            let _ = acl_denies(h.path(), true, &h.bob, PERSONAL);
        }
    })
    .await;
    assert!(open_count(h.path()) - second <= 1);
    // Outside any request (blocking-pool closures, background jobs) the
    // process cache serves a current entry without opening the store.
    let _ = personal_owner(h.path(), PERSONAL);
    let warm = open_count(h.path());
    for _ in 0..100 {
        assert_eq!(personal_owner(h.path(), PERSONAL), Ok(Some(h.alice.clone())));
    }
    assert_eq!(open_count(h.path()), warm, "a current process entry needs no open");
}

#[tokio::test]
async fn p1h_mirror_write_invalidates_the_owner_cache() {
    let h = home();
    assert_eq!(personal_owner(h.path(), "alice-second"), Ok(None));
    let _ = personal_owner(h.path(), "alice-second");
    // The lifecycle apply path's users.db write (mirror row + owner binding;
    // it bumps the ACL generation): the next check sees it, no stale entry.
    record_personal(h.path(), "alice-second", &h.alice);
    private_store(h.path());
    let warm = open_count(h.path());
    assert_eq!(personal_owner(h.path(), "alice-second"), Ok(Some(h.alice.clone())));
    assert!(acl_denies(h.path(), true, &h.admin, "alice-second"));
    assert!(open_count(h.path()) > warm, "the write made the entry miss");
    // The explicit invalidation (called by the lifecycle after each mirror
    // write) drops the entry too: the next check reads the store again.
    let _ = personal_owner(h.path(), PERSONAL);
    let cached = open_count(h.path());
    invalidate_owner_cache(h.path());
    assert_eq!(personal_owner(h.path(), PERSONAL), Ok(Some(h.alice.clone())));
    assert_eq!(open_count(h.path()), cached + 1);
    // Inside a request: one snapshot, dropped by the invalidation.
    with_break_glass_scope(async {
        assert_eq!(personal_owner(h.path(), "alice-third"), Ok(None));
        record_personal(h.path(), "alice-third", &h.alice);
        private_store(h.path());
        assert_eq!(personal_owner(h.path(), "alice-third"), Ok(None), "one ownership snapshot per request");
        invalidate_owner_cache(h.path());
        assert_eq!(personal_owner(h.path(), "alice-third"), Ok(Some(h.alice.clone())));
    })
    .await;
    // A read error is never served from (or stored in) the cache: a store the
    // hardened opener refuses denies at once, warm cache or not.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(h.path().join(USERS_DB), std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(personal_owner(h.path(), PERSONAL), Err(Unavailable));
    assert!(acl_denies(h.path(), true, &h.alice, SHARED), "unreadable ⇒ every employee refused");
    private_store(h.path());
    assert_eq!(personal_owner(h.path(), PERSONAL), Ok(Some(h.alice.clone())));
    // The lifecycle writers call the invalidation.
    let lifecycle = include_str!("isolation_lifecycle.rs");
    assert_eq!(lifecycle.matches("crate::personal_visibility::invalidate_owner_cache(home);").count(), 2);
}

#[test]
fn p1b_self_target_decided_before_users_db() {
    // No users.db: the employee itself (and its sub-agent) is still allowed;
    // anyone else is refused as unverifiable.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(management_refusal(empty.path(), true, PERSONAL, PERSONAL), None);
    ephemeral(empty.path(), EPH, PERSONAL, PERSONAL);
    assert_eq!(management_refusal(empty.path(), true, EPH, PERSONAL), None);
    assert_eq!(management_refusal(empty.path(), true, SHARED, PERSONAL), Some(OWNERSHIP_UNAVAILABLE));
}
