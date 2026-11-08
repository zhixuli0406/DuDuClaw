//! F6d surface of the F6g adoption engine: the request RPC bodies
//! (`isolation_restore_rpc::{adoption_challenge, request_adoption,
//! shared_adoption_request}`), the post-commit hook and boot recovery, over
//! the adoption fixture (child of `isolation_adopt_tests.rs`: real stores,
//! fake schema-2 root answering `NotFound`). The handler gates (system,
//! Admin + MFA + step-up, not listed to AI) are in
//! `handlers/isolation_employee_restore_tests.rs`.
use super::*;
use crate::isolation_restore_rpc::{self as rpc, BootAdoptions, RpcError};
use duduclaw_auth::mfa::{OwnerFactor, OwnerFactorError};

fn employee() -> String { hex::encode(EMP.0) }
fn adopt_purpose0() -> String { format!("adopt:{}:0", employee()) }
fn filed_count(home: &Path) -> i64 {
    Connection::open(home.join(APPLY_DB)).unwrap().query_row("SELECT count(*) FROM employee_restores", [], |r| r.get(0)).unwrap()
}

/// Owner only (NOT_FOUND first, no factor oracle), then the owner's strongest
/// factor bound to `adopt:<employee_id>:<incarnation>` (never the restore
/// purpose); a weaker factor files nothing.
#[test]
fn adoption_request_owner_strongest_factor() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let asked = RefCell::new(Vec::new());
    let check = |factor: OwnerFactor, purpose: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> {
        asked.borrow_mut().push(purpose.to_owned());
        Ok(factor)
    };
    for caller in [f.bob.as_str(), "", "system", &f.alice.to_uppercase()] {
        assert_eq!(rpc::request_adoption(f.home(), &env(&ax), caller, &employee(), 0, None, "totp", "1", &check).err(),
            Some(RpcError::Restore(Refusal::NotFound)), "{caller}");
    }
    assert!(asked.borrow().is_empty(), "no factor check for rows the caller does not own");
    // Malformed ids / target labels are refused before anything is read.
    // (`EMP`'s hex is all digits, so its uppercase is the valid id itself:
    // the uppercase case uses an id with hex letters.)
    for id in ["AB".repeat(16), employee()[..30].to_owned(), format!("{}g", &employee()[..31]), format!("{}0", employee())] {
        assert_eq!(rpc::request_adoption(f.home(), &env(&ax), &f.alice, &id, 0, None, "totp", "1", &check).err(),
            Some(RpcError::Invalid), "{id}");
    }
    assert_eq!(rpc::request_adoption(f.home(), &env(&ax), &f.alice, &employee(), 0, Some("../x"), "totp", "1", &check).err(),
        Some(RpcError::Invalid));
    // The challenge: owner only, bound to the adoption purpose.
    let issue = |purpose: &str| -> Result<String, OwnerFactorError> { Ok(purpose.to_owned()) };
    assert_eq!(rpc::adoption_challenge(f.home(), &f.bob, &employee(), 0, &issue).err(), Some(RpcError::Restore(Refusal::NotFound)));
    assert_eq!(rpc::adoption_challenge(f.home(), &f.alice, &employee(), 0, &issue), Ok(adopt_purpose0()));
    assert_ne!(adopt_purpose0(), format!("restore:{}:0", employee()));
    // Weaker than the strongest enrolled factor, or a failed proof: nothing filed.
    let weaker = |_: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> {
        Err(OwnerFactorError::NotStrongest(OwnerFactor::Passkey))
    };
    let refused = rpc::request_adoption(f.home(), &env(&ax), &f.alice, &employee(), 0, None, "totp", "1", &weaker).unwrap_err();
    assert_eq!(refused.code(), "FACTOR_NOT_STRONGEST");
    let failed = |_: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> { Err(OwnerFactorError::Failed) };
    assert_eq!(rpc::request_adoption(f.home(), &env(&ax), &f.alice, &employee(), 0, None, "totp", "0", &failed).err(),
        Some(RpcError::Factor(OwnerFactorError::Failed)));
    assert_eq!(rpc::request_adoption(f.home(), &env(&ax), &f.alice, &employee(), 0, None, "sms", "1", &check).err(),
        Some(RpcError::Factor(OwnerFactorError::Denied)), "unknown factor names never reach the check");
    assert_eq!(filed_count(f.home()), 0);
    // The owner with the strongest factor files a Personal adoption.
    let filed = rpc::request_adoption(f.home(), &env(&ax), &f.alice, &employee(), 0, None, "totp", "1", &check).unwrap();
    assert_eq!(*asked.borrow(), vec![adopt_purpose0()], "proof bound to the adoption purpose of this row");
    assert_eq!((filed["state"].as_str(), filed["label"].as_str(), filed["target_label"].as_str()),
        (Some("requested"), Some(LABEL), Some(LABEL)));
    assert_eq!(filed["approval_required"], true);
    let request = stored(f.home(), filed["request_id"].as_str().unwrap());
    assert_eq!((request.kind, request.requester.as_str(), request.owner.as_deref()),
        (RequestKind::PersonalAdopt, f.alice.as_str(), Some(f.alice.as_str())));
}

/// The Shared body (the caller checked Admin + MFA + step-up): Shared rows
/// only, member selection ⊆ the snapshot, requester = the verified Admin.
#[test]
fn shared_adoption_request_files_shared_rows_only() {
    let f = fixture();
    let home = f.home();
    tombstoned_full(&f, "soul");
    let item = applied(home, 2, "team-x", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(home, &item, &root).unwrap();
    write_tree(home, "team-x");
    tombstone_employee(home, "team-x", root.employee, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() }).unwrap();
    let ax = ax();
    let shared = hex::encode(EmployeeId([2; 16]).0);
    // A Personal row is never adopted through the Shared request.
    assert_eq!(rpc::shared_adoption_request(home, &env(&ax), &f.bob, &employee(), 0, None, json!([])).err(),
        Some(RpcError::Restore(Refusal::NotFound)));
    assert_eq!(rpc::shared_adoption_request(home, &env(&ax), &f.bob, &shared, 0, None, json!({})).err(), Some(RpcError::Invalid));
    let outsider = rpc::shared_adoption_request(home, &env(&ax), &f.bob, &shared, 0, None,
        json!([{"user_id": f.alice, "access_level": "owner"}])).unwrap_err();
    assert_eq!((outsider, outsider.code()), (RpcError::Adopt(AdoptError::MembersInvalid), "ADOPT_MEMBERS_INVALID"));
    let filed = rpc::shared_adoption_request(home, &env(&ax), &f.bob, &shared, 0, Some("team-y"), json!([])).unwrap();
    assert_eq!(filed["target_label"], "team-y");
    let request = stored(home, filed["request_id"].as_str().unwrap());
    assert_eq!((request.kind, request.requester.as_str(), request.owner.as_deref()), (RequestKind::SharedAdopt, f.bob.as_str(), None));
}

/// Boot runs `recover_adoptions` for every `adopting` row: an apply that
/// crashed after the confirmed row and whose proposal is Applied rolls
/// forward; a second boot has nothing left; a home without apply.db is a no-op.
/// The post-commit hook recognises the adoption item (and only it).
#[test]
fn boot_recovers_adoptions() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    assert_eq!(rpc::recover_adoptions_at_boot(f.home(), &env(&ax)).unwrap(), BootAdoptions::default(), "nothing adopting");
    let request = file_personal(&f, &ax, None).unwrap();
    let (_, item) = propose(&f, &ax, &request);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterSave)));
    let (crashed, _) = apply(&f, &ax, &item);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    assert_eq!(seal_of(f.home()), "adopting");
    mark_applied(f.home(), &item);
    let report = rpc::recover_adoptions_at_boot(f.home(), &env(&ax)).unwrap();
    assert_eq!(report.done, vec![(LABEL.to_owned(), request.request_id.clone(), "rolled_forward")]);
    assert!(report.failed.is_empty());
    assert_eq!((seal_of(f.home()).as_str(), root_state(f.home()).as_str()), ("adopted", "root_absent"));
    assert_eq!(stored(f.home(), &request.request_id).state, "done");
    assert_eq!(rpc::recover_adoptions_at_boot(f.home(), &env(&ax)).unwrap(), BootAdoptions::default(), "idempotent");
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(rpc::recover_adoptions_at_boot(empty.path(), &env(&ax)).unwrap(), BootAdoptions::default());
    // Post-commit: the adoption item triggers the rescan; a normal one does not.
    assert!(rpc::adoption_committed(f.home(), &item, &f.bob, now()));
    let normal = stage(f.home(), 0x77, &[("employee_id", "other"), ("disk_bytes", "67108864"), ("settings", "{}")]);
    assert!(!rpc::adoption_committed(f.home(), &normal, &f.bob, now()));
}
