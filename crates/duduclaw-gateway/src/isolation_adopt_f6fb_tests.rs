//! F6fb fix round (F6 design r5 §3.3, §3.6, §3.10): adoption resume after a
//! crash past `save()` (Personal and Shared, through the real confirmation
//! store), Shared rollback of member bindings, the users.db bindings in the
//! zero count, the `Adopted` replay bound to the allocation, no compensation
//! of a changed tree after the commit, the normalised old-path check and the
//! licence cap. Child of `isolation_adopt_tests.rs` (same fixtures).
use super::*;
use crate::isolation_adopt::names_old_path;
use duduclaw_migration::contract::ConfirmationStatus as Status;

const SHARED: EmployeeId = EmployeeId([2; 16]);
const TEAM: &str = "team-x";
const TEAM_NEXT: &str = "team-y";

fn status(store: &ConfirmationStore, item: &ConfirmationItem) -> Status {
    store.list().unwrap().into_iter().find(|i| i.binding == item.binding).unwrap().status
}

fn bindings(home: &Path, label: &str) -> i64 {
    Connection::open(home.join(USERS_DB)).unwrap()
        .query_row("SELECT count(*) FROM user_agent_bindings WHERE agent_name=?1", [label], |r| r.get(0)).unwrap()
}

fn audit_log(home: &Path) -> String { std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default() }

/// The first stage dies right after `save()` (process death): the stage
/// panics, so the store never records a result and keeps the item `Applying`.
struct Dies<'a> { f: &'a Fixture, ax: &'a Ax }
impl ProposalApplier for Dies<'_> {
    fn stage(&mut self, item: &ConfirmationItem) -> Result<(), ConfirmationError> {
        let (home, new) = (self.f.home(), receipt(item, 9, false));
        let mut persist = |gate: &dyn Fn(&Connection) -> Result<(), ()>| save_gated(home, item, &new, gate);
        ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterSave)));
        let result = apply_item(home, &env(self.ax), item, &new, &self.f.bob, &mut persist);
        ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
        assert_eq!(result.err(), Some(AdoptError::Unavailable));
        panic!("f6fb: process death right after save()");
    }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { panic!("never compensated") }
}

/// Approve the card, let the first stage die past `save()`; the store item
/// stays `Applying` (the real resume path: the next `apply_next` stages again).
fn die_after_save(f: &Fixture, ax: &Ax, item: &ConfirmationItem) -> ConfirmationStore {
    let mut store = store(f.home());
    let session = ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]));
    store.approve(&[item.binding], &session, [2; 32], 100, &Accept).unwrap();
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.apply_next(&item.binding, &mut Dies { f, ax })));
    assert!(died.is_err(), "the first stage died");
    assert_eq!(status(&store, item), Status::Applying);
    store
}

/// `team-x`: a Shared employee with alice bound as operator, tombstoned
/// (members in the principal snapshot, bindings removed).
fn tombstoned_shared(f: &Fixture) {
    let home = f.home();
    let item = applied(home, 2, TEAM, &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(home, &item, &root).unwrap();
    f.users.bind_agent(&f.alice, TEAM, AccessLevel::Operator).unwrap();
    write_full_tree(home, TEAM, "team soul");
    let outcome = tombstone_employee(home, TEAM, root.employee, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() }).unwrap();
    assert_eq!(outcome.seal, "sealed");
    assert_eq!(bindings(home, TEAM), 0, "the tombstone removed the member bindings");
}

/// An Admin files the Shared adoption onto `target` re-binding alice, and proposes it.
fn shared_card(f: &Fixture, ax: &Ax, target: &str) -> (RestoreRequest, ConfirmationItem) {
    let home = f.home();
    let members = json!([{"user_id": f.alice, "access_level": "operator"}]);
    let request = file_adoption(home, &env(ax), SHARED, 0, Some(target), Filer::Shared { requester: &f.bob, members }).unwrap();
    let plan = prepare_proposal(home, &env(ax), &request.request_id).unwrap();
    let adopt = plan.adopt.encode();
    let item = stage(home, 0x52, &[("employee_id", target), ("disk_bytes", "67108864"), ("settings", plan.settings.as_str()),
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

/// Item 1: a stage that died past `save()` (live new row, marker, request
/// `applying`) resumes through the real store: the validator accepts it
/// (`Saved`, not `Inconsistent`), the live tree is never undone and the
/// commit completes; nothing is compensated.
#[test]
fn f6fb_resume_after_save_personal_completes() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    tasks_db(f.home(), LABEL);
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (plan, item) = propose(&f, &ax, &request);
    let mut store = die_after_save(&f, &ax, &item);
    let (live, _, _) = confirmed_row(f.home(), LABEL).unwrap().unwrap();
    assert_eq!((live.employee, live.disabled), (NEW, false), "the crash came after save()");
    assert_eq!((seal_of(f.home()).as_str(), stored(f.home(), &request.request_id).state.as_str()), ("adopting", "applying"));
    let phase = validate_item(f.home(), &env(&ax), &item, &plan.adopt, &f.bob);
    assert!(matches!(&phase, Ok(AdoptPhase::Saved { employee, .. }) if *employee == NEW), "{phase:?}");
    // The store stages the same item again, then publishes and reads back.
    let mut applier = Applier { f: &f, ax: &ax, fail_save: false, disabled: false };
    for _ in 0..3 { store.apply_next(&item.binding, &mut applier).unwrap(); }
    assert!(!applier.disabled, "never compensated");
    assert_eq!(status(&store, &item), Status::Applied);
    assert_eq!((seal_of(f.home()).as_str(), root_state(f.home()).as_str()), ("adopted", "root_absent"));
    assert_eq!(stored(f.home(), &request.request_id).state, "done");
    assert_eq!(std::fs::read_to_string(f.home().join("agents").join(LABEL).join("SOUL.md")).unwrap(), "soul",
        "the live tree was never undone");
    assert_eq!(mirror(f.home(), LABEL).as_deref(), Some(f.alice.as_str()));
    assert_eq!(tasks_under(f.home(), LABEL), 0);
    assert!(audit_log(f.home()).contains("isolation.employee_adoption_resumed"));
    assert_eq!(validate_item(f.home(), &env(&ax), &item, &plan.adopt, &f.bob), Ok(AdoptPhase::Adopted { employee: NEW }));
}

/// Item 1, Shared onto a new label: the resumed stage keeps the live tree and
/// the bindings the first attempt wrote (exactly the selection), and commits.
#[test]
fn f6fb_resume_after_save_shared_new_label_completes() {
    let f = fixture();
    tombstoned_shared(&f);
    let ax = ax();
    let (request, item) = shared_card(&f, &ax, TEAM_NEXT);
    let mut store = die_after_save(&f, &ax, &item);
    assert_eq!(bindings(f.home(), TEAM_NEXT), 1, "bound before the crash");
    let mut applier = Applier { f: &f, ax: &ax, fail_save: false, disabled: false };
    for _ in 0..3 { store.apply_next(&item.binding, &mut applier).unwrap(); }
    assert!(!applier.disabled, "never compensated");
    assert_eq!(status(&store, &item), Status::Applied);
    assert_eq!(shared_seal(f.home()), "adopted");
    assert_eq!(stored(f.home(), &request.request_id).state, "done");
    assert_eq!(bindings(f.home(), TEAM_NEXT), 1);
    assert_eq!(f.users.check_agent_access(&f.alice, TEAM_NEXT).unwrap(), Some(AccessLevel::Operator));
    assert_eq!(std::fs::read_to_string(f.home().join("agents").join(TEAM_NEXT).join("SOUL.md")).unwrap(), "team soul");
    let (live, _, _) = confirmed_row(f.home(), TEAM_NEXT).unwrap().unwrap();
    assert_eq!((live.employee, live.disabled), (NEW, false));
}

/// Item 1: a Shared rollback removes the member bindings the adoption wrote,
/// so nothing outlives it on the label (and a later adoption can bind).
#[test]
fn f6fb_shared_rollback_removes_member_bindings() {
    let f = fixture();
    tombstoned_shared(&f);
    let ax = ax();
    let (request, item) = shared_card(&f, &ax, TEAM_NEXT);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterSave)));
    let (crashed, new) = apply(&f, &ax, &item);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    assert_eq!(bindings(f.home(), TEAM_NEXT), 1);
    // The store's compensation: the new allocation is disabled, then the tree goes back.
    save(f.home(), &item, &Receipt { disabled: true, ..new }).unwrap();
    compensate_item(f.home(), &env(&ax), &item).unwrap();
    assert_eq!(bindings(f.home(), TEAM_NEXT), 0, "no member binding outlives the rollback");
    assert!(!f.home().join("agents").join(TEAM_NEXT).exists());
    assert_eq!(shared_seal(f.home()), "sealed");
    assert_eq!(stored(f.home(), &request.request_id).state, "failed");
    let store = crate::isolation_label_stores::LABEL_KEYED_STORES.iter().find(|s| s.name == "member_bindings").unwrap();
    assert_eq!(store.count_under(f.home(), TEAM_NEXT), Ok(0));
}

/// Item 1: a binding left on the target (`user_agent_bindings`) is part of
/// the positive zero count: the adoption refuses before the commit and
/// rolls back, and the foreign binding is left alone.
#[test]
fn f6fb_target_binding_blocks_adoption() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    f.users.bind_agent(&f.bob, "alice-next", AccessLevel::Viewer).unwrap();
    let request = file_personal(&f, &ax, Some("alice-next")).unwrap();
    let plan = prepare_proposal(f.home(), &env(&ax), &request.request_id).unwrap();
    let adopt = plan.adopt.encode();
    let item = stage(f.home(), 0x43, &plan_fields(&plan, &adopt, &f.alice));
    let mut db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    let tx = db.transaction().unwrap();
    bind_proposal(&tx, &plan, &proposal_key(&item), ax.now).unwrap();
    tx.commit().unwrap();
    let (result, _) = apply(&f, &ax, &item);
    assert_eq!(result.err(), Some(AdoptError::StoresNotEmpty));
    assert!(confirmed_row(f.home(), "alice-next").unwrap().is_none(), "never saved");
    assert!(!f.home().join("agents/alice-next").exists() && quarantined(f.home(), "SOUL.md"));
    assert_eq!(bindings(f.home(), "alice-next"), 1, "not this adoption's binding to remove");
}

/// Item 4: an `Adopted` replay is bound to this allocation (the live row's
/// id is the one recorded, and the receipt is this item's).
#[test]
fn f6fb_adopted_replay_bound_to_allocation() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (plan, item, new) = adopted(&f, &ax, None);
    let mut noop = |_: &dyn Fn(&Connection) -> Result<(), ()>| -> Result<(), ()> { Ok(()) };
    let other = Receipt { employee: EmployeeId([8; 16]), ..new.clone() };
    assert_eq!(apply_item(f.home(), &env(&ax), &item, &other, &f.bob, &mut noop).err(), Some(AdoptError::Inconsistent));
    let mut moved = new.clone();
    moved.binding.operation_id.revision = 2;
    assert_eq!(apply_item(f.home(), &env(&ax), &item, &moved, &f.bob, &mut noop).err(), Some(AdoptError::Inconsistent));
    assert_eq!(apply_item(f.home(), &env(&ax), &item, &new, &f.bob, &mut noop).unwrap().employee, NEW);
    // The commit's record must name the live row's id.
    sql(f.home(), APPLY_DB, "UPDATE employee_quarantine SET adopted_by_employee=?1", &[&hex::encode([8u8; 16])]);
    assert_eq!(validate_item(f.home(), &env(&ax), &item, &plan.adopt, &f.bob).err(), Some(AdoptError::Inconsistent));
}

/// Item 5: after the commit, compensation never moves what the new employee
/// changed into the old quarantine (Inconsistent + Critical audit); an
/// unchanged tree is still moved back.
#[test]
fn f6fb_no_compensation_after_commit_once_tree_changed() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let fx = ax();
    let (_, item, _) = adopted(&f, &fx, None);
    let soul = f.home().join("agents").join(LABEL).join("SOUL.md");
    std::fs::write(&soul, "written by the new employee").unwrap();
    assert_eq!(compensate_item(f.home(), &env(&fx), &item).err(), Some(AdoptError::Inconsistent));
    assert_eq!(std::fs::read_to_string(&soul).unwrap(), "written by the new employee");
    assert!(!quarantined(f.home(), "SOUL.md"));
    assert_eq!(seal_of(f.home()), "adopted");
    assert!(audit_log(f.home()).contains("isolation.employee_adoption_compensation_refused"));

    let g = fixture();
    tombstoned_full(&g, "soul");
    let bx = ax();
    let (_, item, new) = adopted(&g, &bx, None);
    save(g.home(), &item, &Receipt { disabled: true, ..new }).unwrap();
    compensate_item(g.home(), &env(&bx), &item).unwrap();
    assert!(quarantined(g.home(), "SOUL.md") && !g.home().join("agents").join(LABEL).exists());
    assert_eq!(seal_of(g.home()), "sealed");
}

/// Item 6: the old-path check normalises (`//`, `/./`, `\/`, `$HOME`,
/// `${HOME}`, `~/`), covers the quarantine path and the old root workspace,
/// and matches whole paths only.
#[test]
fn f6fb_path_check_normalises_and_anchors() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let home = f.home();
    let q = row(home, 0);
    let old = home.join("agents").join(LABEL).display().to_string();
    let parent = home.parent().unwrap().display().to_string();
    let base = home.file_name().unwrap().to_str().unwrap().to_owned();
    let probe = |text: String| names_old_path(home, &q, Some(&parent), text.as_bytes());
    assert!(probe(format!("see {old}/notes.md")));
    assert!(probe(format!("see {}", old.replace("/agents/", "//agents/"))));
    assert!(probe(format!("see {}/x", old.replace("/agents/", "/./agents/"))));
    assert!(probe(format!("{{\"path\":\"{}\"}}", old.replace('/', "\\/"))));
    assert!(probe(format!("cd $HOME/{base}/agents/{LABEL}/x")));
    assert!(probe(format!("cd ${{HOME}}/{base}/agents/{LABEL}")));
    assert!(probe(format!("cat ~/{base}/agents/{LABEL}/SOUL.md")));
    assert!(probe(format!("'{old}'")));
    assert!(probe(format!("{old}.")));
    assert!(probe(quarantine_path(home, LABEL, EMP, 0).join("CLAUDE.md").display().to_string()));
    assert!(probe(format!("/var/lib/duduclaw-spawnd/images/{}.mnt/work", hex::encode(EMP.0))));
    // Whole paths only: siblings and longer labels never match.
    assert!(!probe(format!("{old}-x/notes.md")));
    assert!(!probe(format!("{old}x")));
    assert!(!probe(format!("{old}.md")));
    assert!(!probe(format!("/srv{old}")));
    assert!(!probe(format!("$HOMEDIR/{base}/agents/{LABEL}")));
    assert!(!probe("nothing here".to_owned()));
    // End to end: a `$HOME`-free but doubled-slash path still refuses the proposal.
    let g = fixture();
    let doubled = g.home().join("agents").join(LABEL).display().to_string().replace("/agents/", "//agents/");
    tombstoned_full(&g, &format!("see {doubled}/notes.md"));
    let gx = ax();
    let request = file_personal(&g, &gx, Some("alice-next")).unwrap();
    assert_eq!(prepare_proposal(g.home(), &env(&gx), &request.request_id).err(), Some(AdoptError::PathRejected));
}

/// Item 8: the licence cap applies at approval and apply (one more live
/// employee), as at request and proposal.
#[test]
fn f6fb_adoption_enforces_agent_cap() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (plan, item) = propose(&f, &ax, &request);
    // Another live employee takes the only seat.
    let other = applied(f.home(), 3, "bob-desk", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    save(f.home(), &other, &receipt(&other, 3, false)).unwrap();
    let capped = AdoptEnv { agent_cap: 1, ..env(&ax) };
    assert_eq!(validate_item(f.home(), &capped, &item, &plan.adopt, &f.bob).err(),
        Some(AdoptError::Restore(Refusal::QuotaExceeded)));
    let mut persist = |_: &dyn Fn(&Connection) -> Result<(), ()>| -> Result<(), ()> { panic!("never saved over the cap") };
    let new = receipt(&item, 9, false);
    assert_eq!(apply_item(f.home(), &capped, &item, &new, &f.bob, &mut persist).err(),
        Some(AdoptError::Restore(Refusal::QuotaExceeded)));
    assert!(quarantined(f.home(), "SOUL.md"));
    assert_eq!(AdoptError::Restore(Refusal::QuotaExceeded).message(), "已達授權的 AI 員工數量上限，無法由新員工接手。");
    let roomy = AdoptEnv { agent_cap: 2, ..env(&ax) };
    assert!(matches!(validate_item(f.home(), &roomy, &item, &plan.adopt, &f.bob), Ok(AdoptPhase::Pending { .. })));
}

/// F6fb2 item 3: a Personal adoption accepts only the strongest-factor
/// rule's factors (passkey, TOTP, email); a Google re-auth (or anything else)
/// is `FACTOR_REQUIRED` and files nothing.
#[test]
fn f6fb2_adoption_refuses_google_reauth_factor() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    assert_eq!(crate::isolation_adopt::ADOPT_OWNER_FACTORS, ["passkey", "totp", "email"]);
    for factor in ["google_reauth", "sms", ""] {
        let refused = file_adoption(f.home(), &env(&ax), EMP, 0, None,
            Filer::Personal(Some(OwnerProof { user_id: &f.alice, factor })));
        assert_eq!(refused.err(), Some(AdoptError::Restore(Refusal::FactorRequired)), "{factor:?}");
    }
    // Nothing was filed by the refusals: the owner's TOTP proof still files.
    let filed = file_adoption(f.home(), &env(&ax), EMP, 0, None,
        Filer::Personal(Some(OwnerProof { user_id: &f.alice, factor: "totp" }))).unwrap();
    assert_eq!(filed.kind, RequestKind::PersonalAdopt);
}
