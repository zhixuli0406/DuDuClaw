//! F6i quarantine cleanup (design r5 §3.8, Q5, N3, R10, S12) against the
//! real stores of the lifecycle fixture: alice's Personal employee is
//! tombstoned and sealed (F6a), bob is the purging Admin. The F6c2 rule is a
//! stand-in with F6c2's answers (its own tests cover the eligibility log);
//! apply.db, users.db, the confirmation store, the quarantine tree and the
//! label stores are real. Child of `isolation_adopt_tests.rs` (fixture,
//! restore fakes and the adoption flow).
use super::*;
use crate::isolation_label_stores::{check_label_not_quarantined, label_free, quarantine_key, GateRefusal};
use crate::isolation_purge::{approve_early, decide_early, journal_of, purge as purge_row, purge_request, quarantine_purge_rpc,
    recover_purges, recover_purges_on_boot, PurgeError, PurgeHook, PurgeOptions, PurgeOutcome, PurgeRule, PURGE_HOOK};
use crate::isolation_restore_wiring::LogRule;

const DAY: i64 = 86_400;
const CAROL: &str = "admin-carol";
const UNQUALIFIED: &str = "admin-recent";

fn opts(confirm_early: bool, retire: bool) -> PurgeOptions { PurgeOptions { confirm_early, retire } }

/// Move the tombstone `days` into the past.
fn age(home: &Path, days: i64) {
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET quarantined_at=quarantined_at-?1 WHERE incarnation=0", &[&(days * DAY)]);
}

fn task(home: &Path, id: &str, label: &str) {
    let path = home.join("tasks.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, assigned_to TEXT)").unwrap();
    db.execute("INSERT INTO tasks(id,assigned_to) VALUES(?1,?2)", [id, label]).unwrap();
    set_mode(&path, 0o600);
}

fn audit_events(home: &Path, event: &str) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default().lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["event_type"] == json!(event)).map(|entry| entry["details"].clone()).collect()
}

/// `(recipients, capped)` of every queued notice of `event`.
fn notices(home: &Path, event: &str) -> Vec<(Vec<String>, bool)> {
    let db = Connection::open(home.join(APPLY_DB)).unwrap();
    let mut statement = db.prepare("SELECT recipients,capped FROM employee_restore_notices WHERE event=?1 ORDER BY seq").unwrap();
    let rows = statement.query_map([event], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))).unwrap()
        .map(|row| row.unwrap()).map(|(to, capped)| (serde_json::from_str(&to).unwrap(), capped)).collect();
    rows
}

fn early_purge_of(home: &Path) -> Option<String> {
    Connection::open(home.join(APPLY_DB)).unwrap()
        .query_row("SELECT early_purge FROM employee_quarantine WHERE incarnation=0", [], |r| r.get(0)).unwrap()
}

fn purged_at_of(home: &Path) -> Option<i64> {
    Connection::open(home.join(APPLY_DB)).unwrap()
        .query_row("SELECT purged_at FROM employee_quarantine WHERE incarnation=0", [], |r| r.get(0)).unwrap()
}

fn not_found_root() -> FakeRoot { FakeRoot { answer: Mutex::new(RootAnswer::NotFound), actions: Mutex::new(Vec::new()) } }

fn filed_early(home: &Path, env: &RestoreEnv<'_>) -> RestoreRequest {
    match purge_row(home, env, EMP, 0, opts(true, false)) {
        Ok(PurgeOutcome::Requested { request, .. }) => request,
        _ => panic!("an early purge must file a request"),
    }
}

fn purged(home: &Path, env: &RestoreEnv<'_>) -> crate::isolation_purge::Purged {
    match purge_row(home, env, EMP, 0, opts(false, false)) {
        Ok(PurgeOutcome::Purged(purged)) => purged,
        Ok(_) => panic!("a row past 90 days is purged inside the call"),
        Err(error) => panic!("purge refused: {error:?}"),
    }
}

/// F6c2 stand-in for kind `early_purge`: the parties are the requester and
/// the Personal owner; `others` = another qualifying Admin exists, `cooling` =
/// an Admin lost eligibility within 7 days. Records
/// `(requester, owner, approver, inside a transaction)`. The self-approval
/// row is F6c2's real `record_self_approval`.
struct Rule { others: bool, cooling: bool, calls: RefCell<Vec<(String, Option<String>, String, bool)>> }
fn rule(others: bool, cooling: bool) -> Rule { Rule { others, cooling, calls: RefCell::new(Vec::new()) } }
impl PurgeRule for Rule {
    fn decide_purge(&self, users: &Connection, requester: &str, owner: Option<&str>, approver: &str) -> Result<bool, &'static str> {
        self.calls.borrow_mut().push((requester.to_owned(), owner.map(str::to_owned), approver.to_owned(), !users.is_autocommit()));
        if users.is_autocommit() { return Err("APPROVAL_NOT_IN_TRANSACTION"); }
        if approver != requester && owner != Some(approver) {
            return if approver == UNQUALIFIED { Err("APPROVER_NOT_QUALIFIED") } else { Ok(false) };
        }
        if self.others { Err("SECOND_ADMIN_REQUIRED") } else if self.cooling { Err("SELF_APPROVAL_COOLING") } else { Ok(true) }
    }
    fn record_purge_self_approval(&self, users: &Connection, requester: &str, owner: Option<&str>, approver: &str, subject: &str)
        -> Result<(), &'static str> {
        duduclaw_auth::admin_approval_rule::record_self_approval(users, &purge_request(requester, owner, approver, true, now()), subject)
            .map_err(|_| "SELF_APPROVAL_AUDIT_UNAVAILABLE")
    }
}

/// S12: under 90 days the call needs `confirm_early`, then files an
/// `early_purge` request and removes nothing; the open request blocks
/// restore requests on the row.
#[test]
fn purge_early_files_request_not_purge() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    assert_eq!(purge_row(home, &env, EMP, 0, opts(false, true)).err(), Some(PurgeError::ConfirmEarly));
    let Ok(PurgeOutcome::Requested { request, preview, notified }) = purge_row(home, &env, EMP, 0, opts(true, true)) else {
        panic!("an early purge must file a request");
    };
    assert_eq!((request.kind, request.state.as_str(), request.requester.as_str(), request.owner.as_deref()),
        (RequestKind::EarlyPurge, "requested", f.bob.as_str(), Some(f.alice.as_str())));
    assert_eq!(request.digest.len(), 64);
    assert!(notified);
    assert_eq!(preview["seal"], json!("sealed"));
    assert_eq!(preview["seal_digest"], json!(row(home, 0).digest));
    // Nothing removed, nothing marked.
    assert!(in_quarantine(home));
    assert_eq!(row(home, 0).seal, "sealed");
    assert_eq!(early_purge_of(home), None);
    assert_eq!(journal_of(home, EMP, 0), None);
    assert_eq!(stored(home, &request.request_id).kind, RequestKind::EarlyPurge);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::RestoreInProgress), "one open request per row");
    assert_eq!(purge_row(home, &env, EMP, 0, opts(true, true)).err(), Some(PurgeError::OpenRequest));
    assert_eq!(audit_events(home, "isolation.quarantine_purge_requested").len(), 1);
    // The Admin view: the row is purgeable early, the request is listed.
    let view = crate::isolation_restore_rpc::admin_view(home, now()).unwrap();
    let purge = &view["quarantine"][0]["purge"];
    assert_eq!((purge["purgeable"].clone(), purge["early"].clone(), purge["early_purge"].clone()), (json!(true), json!(true), Value::Null));
    assert_eq!(view["quarantine"][0]["open_request"]["kind"], json!("early_purge"));
}

/// S12/R1/S2: neither the requester nor the Personal owner approves while
/// another qualifying Admin exists, an unqualified Admin never does; the
/// other qualifying Admin's approval purges inside the call.
#[test]
fn purge_early_needs_second_qualifying_admin() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    let request = filed_early(home, &env);
    let id = request.request_id.as_str();
    let others = rule(true, false);
    for party in [f.bob.as_str(), f.alice.as_str()] {
        assert_eq!(approve_early(home, &env, &others, id, party).err(),
            Some(PurgeError::Restore(Refusal::Approval("SECOND_ADMIN_REQUIRED"))), "{party}");
    }
    assert_eq!(approve_early(home, &env, &others, id, UNQUALIFIED).err(),
        Some(PurgeError::Restore(Refusal::Approval("APPROVER_NOT_QUALIFIED"))));
    assert_eq!((row(home, 0).seal.as_str(), stored(home, id).state.as_str()), ("sealed", "requested"));
    assert!(in_quarantine(home));
    // Decided inside the users.db transaction, with both parties.
    assert!(others.calls.borrow().iter().all(|(requester, owner, _, in_tx)|
        *requester == f.bob && owner.as_deref() == Some(f.alice.as_str()) && *in_tx));
    let done = approve_early(home, &env, &others, id, CAROL).unwrap();
    assert_eq!((done.request_id.as_deref(), done.approver.as_deref(), done.self_approved), (Some(id), Some(CAROL), Some(false)));
    assert_eq!(row(home, 0).seal, "purged");
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists());
    assert_eq!(early_purge_of(home).as_deref(), Some(id));
    let stored_request = stored(home, id);
    assert_eq!((stored_request.state.as_str(), stored_request.approver.as_deref(), stored_request.self_approved),
        ("done", Some(CAROL), Some(false)));
    assert_eq!(approve_early(home, &env, &others, id, CAROL).err(), Some(PurgeError::Restore(Refusal::NotFound)));
}

/// S12/R2/S1: with no other qualifying Admin the requester self-approves,
/// but not inside the cooling window; a self-approval is persisted, written
/// once as F6c2's users.db row and audited `self_approved`. The production
/// rule fails closed without a step-up or a readable eligibility log.
#[test]
fn purge_early_sole_admin_self_approves_with_cooling_and_audit() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    let request = filed_early(home, &env);
    let id = request.request_id.as_str();
    assert_eq!(approve_early(home, &env, &rule(false, true), id, &f.bob).err(),
        Some(PurgeError::Restore(Refusal::Approval("SELF_APPROVAL_COOLING"))));
    assert_eq!(self_approval_rows(home, id), 0);
    assert_eq!(row(home, 0).seal, "sealed");
    let done = approve_early(home, &env, &rule(false, false), id, &f.bob).unwrap();
    assert_eq!(done.self_approved, Some(true));
    assert_eq!(stored(home, id).self_approved, Some(true));
    assert_eq!(self_approval_rows(home, id), 1, "F6c2's users.db row, once");
    assert_eq!(audit_events(home, "isolation.quarantine_purge_self_approved").len(), 1);
    let purged = audit_events(home, "isolation.quarantine_purged");
    assert_eq!((purged[0]["self_approved"].clone(), purged[0]["early"].clone()), (json!(true), json!(true)));
    let users = Connection::open_in_memory().unwrap();
    users.execute_batch("BEGIN").unwrap();
    assert_eq!(LogRule::new(false, 10).decide_purge(&users, "a", Some("o"), "b"), Err("STEP_UP_REQUIRED"));
    assert_eq!(LogRule::new(true, 10).decide_purge(&users, "a", None, "a"), Err("ADMIN_LOG_UNAVAILABLE"));
}

/// The approval re-checks the request digest: anything the purge would
/// remove that changed after the request ends it (`PURGE_CHANGED`, nothing
/// removed); a new request names the new state.
#[test]
fn purge_early_digest_rechecked_at_approval() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    let key = quarantine_key(EMP, 0);
    let request = filed_early(home, &env);
    task(home, "t-late", &key);
    assert_eq!(approve_early(home, &env, &rule(true, false), &request.request_id, CAROL).err(), Some(PurgeError::Changed));
    let failed = stored(home, &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some("PURGE_CHANGED")));
    assert_eq!(row(home, 0).seal, "sealed");
    assert!(in_quarantine(home));
    assert_eq!(tasks_under(home, &key), 1, "nothing removed");
    let again = filed_early(home, &env);
    approve_early(home, &env, &rule(true, false), &again.request_id, CAROL).unwrap();
    assert_eq!(tasks_under(home, &key), 0);
}

/// The Personal owner (tombstone-time address) and the log-eligible Admins
/// are told at request and at purge; never capped. Through the RPC bodies:
/// the request reply names what is lost, decide routes `early_purge` here
/// and leaves every other request to the restore decide.
#[test]
fn purge_early_notifies_owner_and_admins() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    let address = row(home, 0).principal_snapshot["notice_address"].as_str().unwrap().to_owned();
    let fake_root = not_found_root();
    let filed = quarantine_purge_rpc(home, &env, &hex::encode(EMP.0), 0, opts(true, false), &fake_root).unwrap();
    assert_eq!((filed["state"].clone(), filed["kind"].clone(), filed["notified"].clone()), (json!("requested"), json!("early_purge"), json!(true)));
    assert!(filed["lost"]["row_stores"].is_object());
    let id = filed["request_id"].as_str().unwrap().to_owned();
    let requested = notices(home, "isolation.quarantine_purge_requested");
    assert_eq!(requested.len(), 1);
    assert!(requested[0].0.contains(&address) && !requested[0].1, "{requested:?}");
    assert!(decide_early(home, &env, &rule(true, false), CAROL, &"0".repeat(32), true, &fake_root).is_none(),
        "not an early purge: the restore decide answers");
    let done = decide_early(home, &env, &rule(true, false), CAROL, &id, true, &fake_root).unwrap().unwrap();
    assert_eq!((done["state"].clone(), done["purged"]["state"].clone(), done["retire"].clone()),
        (json!("done"), json!("purged"), Value::Null));
    let purged = notices(home, "isolation.quarantine_purged");
    assert_eq!(purged.len(), 1);
    assert!(purged[0].0.contains(&address) && !purged[0].1, "{purged:?}");
    assert!(fake_root.actions.lock().unwrap().is_empty(), "retire unticked: the root is never asked");
}

/// Q5: a row 90 days past the tombstone is purged inside the call (no
/// request); the ticked retire box stages F6l right after the commit (an
/// authoritative `NotFound` records `root_absent`).
#[test]
fn purge_after_90_days_purges_in_call() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    age(home, 91);
    let fake_root = not_found_root();
    let done = quarantine_purge_rpc(home, &env, &hex::encode(EMP.0), 0, opts(false, true), &fake_root).unwrap();
    assert_eq!((done["state"].clone(), done["early"].clone(), done["request_id"].clone()), (json!("purged"), json!(false), Value::Null));
    assert_eq!(done["retire"]["state"], json!("root_absent"));
    assert_eq!(root_state(home), "root_absent");
    assert_eq!(row(home, 0).seal, "purged");
    assert!(purged_at_of(home).is_some());
    assert_eq!(early_purge_of(home), None);
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists());
    let requests: i64 = Connection::open(home.join(APPLY_DB)).unwrap()
        .query_row("SELECT count(*) FROM employee_restores", [], |r| r.get(0)).unwrap();
    assert_eq!(requests, 0, "no request");
    assert_eq!(purge_row(home, &env, EMP, 0, opts(false, false)).err(), Some(PurgeError::NotPurgeable), "closed");
    let view = crate::isolation_restore_rpc::admin_view(home, now()).unwrap();
    let purge = &view["quarantine"][0]["purge"];
    assert_eq!((purge["purgeable"].clone(), purge["early"].clone()), (json!(false), json!(false)));
    assert_eq!(purge["purged_at"], json!(purged_at_of(home)));
    assert_eq!(quarantine_purge_rpc(home, &env, "not-hex", 0, opts(false, false), &fake_root).err(),
        Some(crate::isolation_restore_rpc::RpcError::Invalid));
}

/// Never beside an open request, never from `pending` or `restored`.
#[test]
fn purge_refused_with_open_request() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    let request = file(&f, &fx).unwrap();
    age(home, 91);
    assert_eq!(purge_row(home, &env, EMP, 0, opts(true, false)).err(), Some(PurgeError::OpenRequest));
    assert_eq!(row(home, 0).seal, "sealed");
    assert!(in_quarantine(home));
    close_request(home, &request.request_id, &f.bob, Close::Cancelled, now()).unwrap();
    purged(home, &env);
    for seal in ["pending", "restored"] {
        let g = fixture();
        let (_, root) = tombstoned(&g);
        let gx = fakes(&root);
        let genv = RestoreEnv { actor: g.bob.as_str(), ..gx.env() };
        age(g.home(), 91);
        sql(g.home(), APPLY_DB, "UPDATE employee_quarantine SET seal=?1", &[&seal]);
        assert_eq!(purge_row(g.home(), &genv, EMP, 0, opts(false, false)).err(), Some(PurgeError::NotPurgeable), "{seal}");
        assert!(quarantine_path(g.home(), LABEL, EMP, 0).join("SOUL.md").exists(), "{seal}");
    }
}

/// The tree goes through held descriptors: a symlink inside it is unlinked,
/// never followed (its target stays); a quarantine root replaced by a
/// symlink is refused before anything changes, and alerted.
#[test]
fn purge_no_symlink_follow() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    age(home, 91);
    let outside = home.join("outside");
    std::fs::create_dir_all(outside.join("dir")).unwrap();
    std::fs::write(outside.join("dir/keep.txt"), "keep").unwrap();
    std::fs::write(outside.join("file.txt"), "keep").unwrap();
    let tree = quarantine_path(home, LABEL, EMP, 0);
    std::os::unix::fs::symlink(outside.join("dir"), tree.join("SKILLS/linked-dir")).unwrap();
    std::os::unix::fs::symlink(outside.join("file.txt"), tree.join("linked-file")).unwrap();
    let done = purged(home, &env);
    assert!(!tree.exists());
    assert_eq!(std::fs::read_to_string(outside.join("dir/keep.txt")).unwrap(), "keep", "link targets untouched");
    assert_eq!(std::fs::read_to_string(outside.join("file.txt")).unwrap(), "keep");
    assert!(done.removed.entries >= 6, "{:?}", done.removed);
    // The row's root itself swapped for a symlink.
    let g = fixture();
    let home = g.home();
    let (_, root) = tombstoned(&g);
    let gx = fakes(&root);
    let genv = RestoreEnv { actor: g.bob.as_str(), ..gx.env() };
    age(home, 91);
    let tree = quarantine_path(home, LABEL, EMP, 0);
    let moved = home.join("moved-tree");
    std::fs::rename(&tree, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &tree).unwrap();
    assert_eq!(purge_row(home, &genv, EMP, 0, opts(false, false)).err(), Some(PurgeError::TreeRefused));
    assert!(moved.join("SOUL.md").exists() && moved.join("SKILLS/a.md").exists(), "never followed");
    assert_eq!(row(home, 0).seal, "sealed", "refused before anything changed");
    assert_eq!(audit_events(home, "isolation.quarantine_purge_tree_refused").len(), 1);
}

/// R10/N3/S9: the tree, the row's sealed stores and — no live row holding
/// the label — everything still under the label go; the label is then free
/// at the gateway (positive zero, S10).
#[test]
fn purge_removes_tree_and_label_stores() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    age(home, 91);
    let key = quarantine_key(EMP, 0);
    task(home, "t-label", LABEL);
    task(home, "t-row", &key);
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined));
    let done = purged(home, &env);
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists());
    assert_eq!((tasks_under(home, LABEL), tasks_under(home, &key)), (0, 0));
    assert!(done.row_stores["tasks"].as_u64().unwrap() >= 1);
    assert!(done.label_stores["tasks"].as_u64().unwrap() >= 1);
    assert_eq!(label_free(home, LABEL), Ok(true));
    assert_eq!(label_free(home, &key), Ok(true));
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()));
    assert_eq!(row(home, 0).seal, "purged");
}

/// An adopted row keeps only its sealed remainder and sealed stores: those
/// go (once the adoption proposal is Applied); the new employee live on the
/// label, its tree and its state are untouched.
#[test]
fn purge_adopted_row_removes_remainder_and_sealed_stores() {
    let f = fixture();
    let home = f.home();
    let (_, old) = tombstoned_full(&f, "soul");
    tasks_db(home, LABEL);
    let ax = ax();
    let (plan, item, _) = adopted(&f, &ax, None);
    assert_eq!(seal_of(home), "adopted");
    let key = plan.adopt.store_key();
    assert_eq!(key, quarantine_key(EMP, 0));
    assert_eq!(tasks_under(home, &key), 1);
    assert!(quarantined(home, "CLAUDE.md"), "the remainder");
    task(home, "t-new", LABEL);
    age(home, 91);
    let fx = fakes(&old);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    assert_eq!(purge_row(home, &env, EMP, 0, opts(false, false)).err(), Some(PurgeError::Restore(Refusal::RestoreInProgress)),
        "the adoption proposal is still finishing");
    mark_applied(home, &item);
    let done = purged(home, &env);
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists(), "the sealed remainder is gone");
    assert_eq!(tasks_under(home, &key), 0, "the row's sealed stores are gone");
    assert_eq!(tasks_under(home, LABEL), 1, "the new employee's state is untouched");
    assert_eq!(done.label_stores, Value::Null);
    assert!(home.join("agents").join(LABEL).join("SOUL.md").exists(), "the adopted tree is untouched");
    assert_eq!(seal_of(home), "purged");
    let adopted_by: Option<String> = Connection::open(home.join(APPLY_DB)).unwrap()
        .query_row("SELECT adopted_by_employee FROM employee_quarantine WHERE incarnation=0", [], |r| r.get(0)).unwrap();
    assert_eq!(adopted_by, Some(hex::encode(NEW.0)), "the adoption record stays");
}

/// F6 b3 N1: purging an `adopted` row never freezes the live employee that
/// adopted its label — while the purge runs (`purging`, journal
/// `prior_seal = adopted`), after a refused one (`PURGE_TREE_REFUSED`, row
/// left `purging`) and after it finishes. A `purging` row from any other
/// state still holds the label (covered by `purge_crash_finishes_first`).
#[test]
fn f6b3_purge_adopted_row_keeps_live_adopter_writable() {
    use crate::isolation_label_stores::{check_label_writable, label_hold, TEST_PROTECTED};
    struct Protected;
    impl Drop for Protected { fn drop(&mut self) { TEST_PROTECTED.with(|cell| cell.set(None)); } }
    let f = fixture();
    let home = f.home();
    let (_, old) = tombstoned_full(&f, "soul");
    let ax = ax();
    let (_, item, _) = adopted(&f, &ax, None);
    mark_applied(home, &item);
    age(home, 91);
    let fx = fakes(&old);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    // Protected mode only around the writer-gate checks (the purge runs as in production).
    let writable = || {
        TEST_PROTECTED.with(|cell| cell.set(Some(true)));
        let _protected = Protected;
        check_label_writable(home, LABEL)
    };
    let live = |when: &str| {
        assert_eq!(writable(), Ok(()), "{when}: the adopter writes");
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()), "{when}: its proposals pass");
        assert!(label_hold(home, LABEL).unwrap().open_rows.is_empty(), "{when}");
    };
    live("adopted");
    // Interrupted right after the start: `purging`, the adopter still writes.
    PURGE_HOOK.with(|hook| *hook.borrow_mut() = Some(Box::new(|point| point == PurgeHook::AfterStart)));
    assert!(purge_row(home, &env, EMP, 0, opts(false, false)).is_err());
    PURGE_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(seal_of(home), "purging");
    assert_eq!(journal_of(home, EMP, 0).unwrap().prior_seal, "adopted");
    live("purging");
    task(home, "t-live", LABEL);
    // The remainder swapped for a symlink: the purge refuses and the row stays `purging`.
    let tree = quarantine_path(home, LABEL, EMP, 0);
    let moved = home.join("moved-remainder");
    std::fs::rename(&tree, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &tree).unwrap();
    assert_eq!(purge_row(home, &env, EMP, 0, opts(false, false)).err(), Some(PurgeError::TreeRefused));
    assert_eq!(seal_of(home), "purging");
    live("refused purge");
    // Put back: recovery finishes it; the adopter's state is untouched.
    std::fs::remove_file(&tree).unwrap();
    std::fs::rename(&moved, &tree).unwrap();
    purged(home, &env);
    assert_eq!(seal_of(home), "purged");
    live("purged");
    assert_eq!(tasks_under(home, LABEL), 1, "the live employee's rows stay");
    // A `purging` row without a journal (or from any other state) still holds the label.
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='purging' WHERE incarnation=0", &[]);
    sql(home, APPLY_DB, "UPDATE employee_purge_journal SET prior_seal='sealed'", &[]);
    assert_eq!(writable(), Err(GateRefusal::Quarantined));
    sql(home, APPLY_DB, "DELETE FROM employee_purge_journal", &[]);
    assert_eq!(writable(), Err(GateRefusal::Quarantined));
}

/// §3.6: a purge interrupted after it started leaves the row `purging`
/// (the label stays held, requests refused); the next purge step on the
/// label — or boot — finishes it before anything else (an interrupted early
/// purge ends its request `done`); a row without its journal alerts.
#[test]
fn purge_crash_finishes_first() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    age(home, 91);
    let key = quarantine_key(EMP, 0);
    task(home, "t-row", &key);
    PURGE_HOOK.with(|hook| *hook.borrow_mut() = Some(Box::new(|point| point == PurgeHook::AfterTree)));
    assert_eq!(purge_row(home, &env, EMP, 0, opts(false, false)).err(), Some(PurgeError::Restore(Refusal::Unavailable)));
    PURGE_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(row(home, 0).seal, "purging");
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists(), "the tree went before the crash");
    assert_eq!(tasks_under(home, &key), 1, "the stores did not");
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined));
    assert!(file(&f, &fx).is_err(), "no request on a purging row");
    let done = purged(home, &env);
    assert_eq!((done.employee, done.incarnation), (EMP, 0));
    assert_eq!(row(home, 0).seal, "purged");
    assert_eq!(tasks_under(home, &key), 0);
    assert_eq!(audit_events(home, "isolation.quarantine_purge_recovered").len(), 1);
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()));
    // An early purge interrupted right after its approval: boot finishes it.
    let g = fixture();
    let home = g.home();
    let (_, root) = tombstoned(&g);
    let gx = fakes(&root);
    let genv = RestoreEnv { actor: g.bob.as_str(), ..gx.env() };
    let request = filed_early(home, &genv);
    PURGE_HOOK.with(|hook| *hook.borrow_mut() = Some(Box::new(|point| point == PurgeHook::AfterStart)));
    assert!(approve_early(home, &genv, &rule(true, false), &request.request_id, CAROL).is_err());
    PURGE_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert_eq!((row(home, 0).seal.as_str(), stored(home, &request.request_id).state.as_str()), ("purging", "applying"));
    assert!(quarantine_path(home, LABEL, EMP, 0).exists());
    let (finished, failed) = recover_purges_on_boot(home, now()).unwrap();
    assert_eq!((finished, failed), (vec![LABEL.to_owned()], Vec::new()));
    assert_eq!((row(home, 0).seal.as_str(), stored(home, &request.request_id).state.as_str()), ("purged", "done"));
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists());
    // A `purging` row without its journal (hand edit, dev build) alerts.
    let h = fixture();
    let home = h.home();
    tombstoned(&h);
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='purging'", &[]);
    assert_eq!(recover_purges(home, LABEL, now()).err(), Some(PurgeError::Restore(Refusal::Inconsistent)));
    assert_eq!(audit_events(home, "isolation.quarantine_purge_inconsistent").len(), 1);
}

/// M7: the purge audit keeps the seal digest, the sealed manifest totals,
/// the store marks and what was removed; the journal keeps them too.
#[test]
fn purge_audit_keeps_digest_and_totals() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let env = RestoreEnv { actor: f.bob.as_str(), ..fx.env() };
    let before = row(home, 0);
    let digest = before.digest.clone().unwrap();
    let entries = before.stat_manifest.as_ref().unwrap()["entries"].as_array().unwrap().len();
    age(home, 91);
    purged(home, &env);
    let audit = audit_events(home, "isolation.quarantine_purged");
    assert_eq!(audit.len(), 1);
    let details = &audit[0];
    assert_eq!(details["seal_digest"], json!(digest));
    assert_eq!(details["digest_prefix"], json!(digest.chars().take(16).collect::<String>()));
    assert_eq!(details["sealed_manifest"]["entries"], json!(entries));
    assert_eq!(details["removed"]["entries"], json!(entries), "everything the seal listed was removed");
    assert!(details["removed"]["file_bytes"].as_u64().unwrap() > 0);
    assert_eq!(details["store_marks"], marks());
    assert_eq!((details["early"].clone(), details["prior_seal"].clone()), (json!(false), json!("sealed")));
    assert_eq!(details["requester_user_id"], json!(f.bob));
    let journal = journal_of(home, EMP, 0).unwrap();
    assert_eq!((journal.prior_seal.as_str(), journal.preview["seal_digest"].clone()), ("sealed", json!(digest)));
}
