//! F6 part (b) re-review follow-ups on adoption (F6 design r5 §3.9, §3.10):
//! N4 (an adopted Shared employee uses no tenant-wide route, fail closed),
//! N5 (the old-path check collapses `seg/../` and decodes JSON `\uXXXX`) and
//! the `Zero::AfterSave` nit (the principal side still counts on the old
//! label when the target differs). Child of `isolation_adopt_tests.rs`.
use super::*;
use crate::isolation_adopt::names_old_path;
use crate::isolation_connectors::ConnectorKind;
use crate::isolation_restore_shared::tenant_route_gate;
use duduclaw_isolation::bounded::Text;
use duduclaw_isolation::capability::EmployeeBinding;
use duduclaw_mcp_boundary::VerificationError;

const SHARED: EmployeeId = EmployeeId([2; 16]);
const TEAM: &str = "team-x";
const TEAM_NEXT: &str = "team-y";

/// `team-x`: a Shared employee with alice bound as operator, tombstoned.
fn tombstoned_shared(f: &Fixture) {
    let home = f.home();
    let item = applied(home, 2, TEAM, &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(home, &item, &root).unwrap();
    f.users.bind_agent(&f.alice, TEAM, AccessLevel::Operator).unwrap();
    write_full_tree(home, TEAM, "team soul");
    let outcome = tombstone_employee(home, TEAM, root.employee, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() }).unwrap();
    assert_eq!(outcome.seal, "sealed");
}

/// An Admin files the Shared adoption onto `target` re-binding alice, and proposes it.
fn shared_card(f: &Fixture, ax: &Ax, target: &str) -> (RestoreRequest, ConfirmationItem) {
    let home = f.home();
    let members = json!([{"user_id": f.alice, "access_level": "operator"}]);
    let request = file_adoption(home, &env(ax), SHARED, 0, Some(target), Filer::Shared { requester: &f.bob, members }).unwrap();
    let plan = prepare_proposal(home, &env(ax), &request.request_id).unwrap();
    let adopt = plan.adopt.encode();
    let item = stage(home, 0x53, &[("employee_id", target), ("disk_bytes", "67108864"), ("settings", plan.settings.as_str()),
        ("binding_kind", "shared"), ("tenant_id", f.tenant.as_str()), ("adopt_quarantine", adopt.as_str())]);
    let mut db = Connection::open(home.join(APPLY_DB)).unwrap();
    let tx = db.transaction().unwrap();
    bind_proposal(&tx, &plan, &proposal_key(&item), ax.now).unwrap();
    tx.commit().unwrap();
    (request, item)
}

fn shared_seal(home: &Path) -> String {
    read_row(&Connection::open(home.join(APPLY_DB)).unwrap(), SHARED, 0).unwrap().unwrap().seal
}

/// N4: the new employee that adopted a Shared row uses no tenant-wide route
/// (`Revoked`, both kinds), whatever the account; a Shared employee that
/// never adopted anything and non-tenant kinds pass as before; the record
/// survives the row's purge (`adopted_by_employee` is kept).
#[test]
fn f6b3_adopted_shared_employee_has_no_tenant_routes() {
    let f = fixture();
    let home = f.home();
    tombstoned_shared(&f);
    let ax = new_ax();
    let (_, item) = shared_card(&f, &ax, TEAM_NEXT);
    let (adopted, new) = apply(&f, &ax, &item);
    assert_eq!(adopted.unwrap().employee, NEW);
    assert_eq!(shared_seal(home), "adopted");
    let shared = EmployeeBinding::Shared(Text::new(f.tenant.clone()).unwrap());
    for kind in [ConnectorKind::SharedTenant, ConnectorKind::SharedHome] {
        for account in ["acct-a", "acct-b"] {
            assert_eq!(tenant_route_gate(home, new.employee, 0, &shared, account, kind), Err(VerificationError::Revoked),
                "{kind:?} {account}: not re-approved");
        }
    }
    assert_eq!(tenant_route_gate(home, new.employee, 0, &shared, "acct-a", ConnectorKind::Personal), Ok(()), "other kinds: own checks");
    assert_eq!(tenant_route_gate(home, EmployeeId([3; 16]), 0, &shared, "acct-a", ConnectorKind::SharedTenant), Ok(()),
        "never adopted: unchanged");
    // Kept after the row is purged (the adoption record stays on the row).
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='purged' WHERE employee_hex=?1", &[&hex::encode(SHARED.0)]);
    assert_eq!(tenant_route_gate(home, new.employee, 0, &shared, "acct-a", ConnectorKind::SharedTenant), Err(VerificationError::Revoked));
    // An unreadable index refuses (fail closed).
    std::fs::remove_file(home.join(APPLY_DB)).unwrap();
    std::fs::write(home.join(APPLY_DB), b"not a SQLite database").unwrap();
    assert_eq!(tenant_route_gate(home, new.employee, 0, &shared, "acct-a", ConnectorKind::SharedTenant),
        Err(VerificationError::Unavailable));
}

/// N5: `seg/../` is collapsed and JSON `\uXXXX` decoded before matching; a
/// collapse never makes a sibling or a parent match.
#[test]
fn f6b3_path_check_collapses_parent_segments_and_unicode_escapes() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let home = f.home();
    let q = row(home, 0);
    let agents = home.join("agents").display().to_string();
    let old = format!("{agents}/{LABEL}");
    let parent = home.parent().unwrap().display().to_string();
    let probe = |text: String| names_old_path(home, &q, Some(&parent), text.as_bytes());
    assert!(probe(format!("see {agents}/other-desk/../{LABEL}/notes.md")));
    assert!(probe(format!("see {old}/sub/deeper/../../notes.md")));
    assert!(probe(format!("see {agents}/../agents/{LABEL}")));
    assert!(probe(format!("cd {old}/x/..")), "a trailing `/..`");
    assert!(probe(format!("{{\"path\":\"{}\"}}", old.replace('/', "\\u002f"))));
    assert!(probe(format!("{{\"path\":\"{}\"}}", old.replace('/', "\\u002F"))), "either case");
    let first = LABEL.as_bytes()[0];
    assert!(probe(format!("{agents}/\\u{first:04x}{}/notes.md", &LABEL[1..])), "an escaped letter");
    let escaped = format!("{agents}/x").replace('/', "\\/");
    assert!(probe(format!("{{\"p\":\"{escaped}\\u002f..\\u002f{LABEL}\"}}")), "escapes and `..` together");
    // Never a sibling, a longer label or the parent.
    assert!(!probe(format!("{old}/../{LABEL}-x/notes.md")));
    assert!(!probe(format!("{old}x/..")), "collapses to agents/, not the label");
    assert!(!probe(format!("{old}/../../agents")));
    assert!(!probe(format!("{agents}/\\u{first:04x}{}-x", &LABEL[1..])));
    assert!(!probe("docs/../README.md and \\u12".to_owned()), "relative and malformed escapes stay");
}

/// Nit: after `save()` the principal side is skipped on the target only (the
/// new id's own bindings are there); a binding left on the old label (≠
/// target) still blocks the roll forward (S10). Without it the same crash
/// rolls forward.
#[test]
fn f6b3_after_save_counts_principal_on_old_label() {
    for planted in [true, false] {
        let f = fixture();
        let home = f.home();
        tombstoned_shared(&f);
        let ax = new_ax();
        let (request, item) = shared_card(&f, &ax, TEAM_NEXT);
        ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterSave)));
        let (crashed, _) = apply(&f, &ax, &item);
        ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
        assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
        assert_eq!(shared_seal(home), "adopting");
        if planted { f.users.bind_agent(&f.bob, TEAM, AccessLevel::Viewer).unwrap(); }
        mark_applied(home, &item);
        let recovered = recover_adoptions(home, &env(&ax), TEAM);
        if planted {
            assert_eq!(recovered.err(), Some(AdoptError::Inconsistent), "a binding on the old label is not zero");
            assert_ne!(shared_seal(home), "adopted");
            let log = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default();
            assert!(log.contains("STORES_NOT_EMPTY"), "the reason is the zero count");
        } else {
            assert_eq!(recovered.unwrap(), vec![(request.request_id.clone(), "rolled_forward")]);
            assert_eq!(shared_seal(home), "adopted");
        }
    }
}
