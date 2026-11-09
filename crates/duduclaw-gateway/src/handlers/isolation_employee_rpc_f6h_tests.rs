//! F6h (design r5 §3.10 step 7, R7, S7): staging an Employee proposal for a
//! label held by a quarantine row that is not closed refuses
//! `LABEL_QUARANTINED`, at `prepare_propose` and again at `stage_prepared`.
use super::*;
use crate::isolation_label_stores::LABEL_QUARANTINED;

const DESK: &str = "team-desk";

fn quarantine(home: &Path, label: &str, seal: &'static str) {
    open_requests(home).unwrap();
    let db = Connection::open(home.join(APPLY_DB)).unwrap();
    crate::isolation_quarantine::ensure_employee_schema(&db).unwrap();
    crate::isolation_quarantine::insert_quarantine(&db, &crate::isolation_quarantine::NewQuarantine {
        employee: duduclaw_isolation::ids::EmployeeId([4; 16]), incarnation: 0, label, owner: None, tenant: "tenant-a",
        quarantined_at: 1, root: None, seal, principal_snapshot: &json!({"kind": "shared"}), stripped: &json!({}),
    }).unwrap();
}

#[test]
fn proposal_on_quarantined_label_refused() {
    let f = fixture();
    let now = 10_000;
    let shared = || propose_input(json!({"label": DESK, "binding_kind": "shared", "csrf_token": CSRF}));
    // Prepared before the tombstone; the staging step re-checks.
    let prepared = prepare_propose(f.home(), &f.users, Some(&proof(now)), &f.bob, shared(), now).unwrap();
    quarantine(f.home(), DESK, "sealed");
    assert_eq!(stage_prepared(f.home(), Some(&proof(now)), &f.bob, &prepared, "{}", now).err(), Some(LABEL_QUARANTINED));
    assert_eq!(prepare_propose(f.home(), &f.users, Some(&proof(now)), &f.bob, shared(), now).err(), Some(LABEL_QUARANTINED));
    assert!(employee_items(f.home(), DESK).is_empty(), "nothing was staged");
    // Exact label only: another label is unaffected.
    let other = propose_input(json!({"label": "team-desk-2", "binding_kind": "shared", "csrf_token": CSRF}));
    assert!(prepare_propose(f.home(), &f.users, Some(&proof(now)), &f.bob, other, now).is_ok());
    // S7: a closed row (restored) no longer holds the label.
    sql(f.home(), APPLY_DB, "UPDATE employee_quarantine SET seal='restored' WHERE label=?1", &[&DESK]);
    let prepared = prepare_propose(f.home(), &f.users, Some(&proof(now)), &f.bob, shared(), now).unwrap();
    assert!(stage_prepared(f.home(), Some(&proof(now)), &f.bob, &prepared, "{}", now).is_ok());
}
