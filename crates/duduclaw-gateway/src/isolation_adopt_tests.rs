//! F6g adoption under a new employee id (design r5 §3.10) against the real
//! stores of the lifecycle fixture. Child of `isolation_restore_tests.rs`
//! (alice's Personal employee, tombstoned and sealed). The schema-2 root,
//! routes, run stopper and the F6c2 rule are fakes; the confirmation store,
//! apply.db, users.db, the quarantine tree and the label stores are real.
use super::*;
use crate::isolation_adopt::{apply_item, bind_proposal, compensate_item, file_adoption, filter_settings, prepare_proposal,
    proposal_key, recover_adoptions, rename_agent, validate_item, AdoptEnv, AdoptError, AdoptHook, AdoptRef, Adopted,
    AdoptionPlan, Filer, ADOPT_HOOK, ADOPT_SETTINGS_KEYS};
use crate::isolation_adopt::{marker, Phase as AdoptPhase};
use crate::isolation_retire::RetireRoot;
use duduclaw_auth::admin_approval_rule::{AdminApprovalRule, ApprovalDecision, ApprovalRequest};
use duduclaw_migration::allocation::{Action, Answer, ErrorCode as Code, Phase as RootPhase2, Reply, Request};
use std::sync::Mutex;

const NEW: EmployeeId = EmployeeId([9; 16]);
const KEY: [u8; 32] = [7; 32];

enum RootAnswer { NotFound, Inconsistent, Unavailable, Ready(Receipt) }

/// Schema-2 root: records every action, answers as configured.
struct FakeRoot { answer: Mutex<RootAnswer>, actions: Mutex<Vec<String>> }
impl RetireRoot for FakeRoot {
    fn exchange(&self, request: &Request) -> Result<Answer, ()> {
        self.actions.lock().unwrap().push(format!("{:?}", request.action));
        match &*self.answer.lock().unwrap() {
            RootAnswer::NotFound => Ok(Answer::Refused(Code::NotFound)),
            RootAnswer::Inconsistent => Ok(Answer::Refused(Code::Inconsistent)),
            RootAnswer::Unavailable => Err(()),
            RootAnswer::Ready(receipt) => Ok(Answer::Reply(Reply { receipt: receipt.clone(), phase: RootPhase2::Ready, idle: Some(true) })),
        }
    }
}

/// F6c2 stand-in: any approver; self-approval exactly when approver == requester.
struct AnyAdmin;
impl AdminApprovalRule for AnyAdmin {
    fn decide(&self, _: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        ApprovalDecision::Allowed { self_approved: request.approver_user_id == request.requester_user_id, notice_addresses: Vec::new() }
    }
    /// F6fa2 stand-in: any third account approves an Admin-filed request; a
    /// party (filer or owner) never does.
    fn decide_admin_filed(&self, _: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        use duduclaw_auth::admin_approval_rule::ApprovalRefusal;
        let party = request.approver_user_id == request.requester_user_id || request.owner_user_id == Some(request.approver_user_id);
        if party { ApprovalDecision::Refused(ApprovalRefusal::SecondAdminRequired) }
        else { ApprovalDecision::Allowed { self_approved: false, notice_addresses: Vec::new() } }
    }
    fn check_admin_filing(&self, _: &Connection, filer: &str, owner: &str, _: i64)
        -> Result<(), duduclaw_auth::admin_approval_rule::ApprovalRefusal> {
        if filer == owner { Err(duduclaw_auth::admin_approval_rule::ApprovalRefusal::InvalidRequest) } else { Ok(()) }
    }
}

fn ok_hooks(_: &Path) -> Result<(), ()> { Ok(()) }

struct Ax { root: FakeRoot, routes: Routes, now: i64 }
fn ax() -> Ax {
    Ax { root: FakeRoot { answer: Mutex::new(RootAnswer::NotFound), actions: Mutex::new(Vec::new()) },
        routes: Routes(Rc::new(RefCell::new(Vec::new()))), now: now() }
}
fn env(ax: &Ax) -> AdoptEnv<'_> {
    AdoptEnv { root: &ax.root, routes: &ax.routes, runs: &NoLiveRuns, rule: &AnyAdmin, hooks: &ok_hooks, agent_cap: 0,
        actor: "admin-user", now: ax.now }
}

fn agent_toml(label: &str) -> String {
    include_str!("../../../templates/evaluator/agent.toml").replace("name = \"evaluator\"", &format!("name = \"{label}\""))
}

const SETTINGS: &str = r#"{"model":"claude-sonnet-4-6","env":{"X":"1"},"apiKeyHelper":"/bin/sh -c evil","statusLine":{"command":"x"},
  "hooks":{"PreToolUse":[]},"mcpServers":{"m":{}},"permissions":{"allow":["Bash(*)"],"deny":["Bash(rm:*)"],"ask":["Write"]}}"#;

/// The tree a long-lived employee leaves: allowlisted entries plus remainder.
fn write_full_tree(home: &Path, label: &str, soul: &str) {
    let dir = home.join("agents").join(label);
    std::fs::create_dir_all(dir.join("SKILLS/sub")).unwrap();
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::create_dir_all(dir.join(".codex")).unwrap();
    std::fs::write(dir.join("SOUL.md"), soul).unwrap();
    std::fs::write(dir.join("CONTRACT.toml"), "must_not = []\n").unwrap();
    std::fs::write(dir.join("SKILLS/a.md"), "skill").unwrap();
    std::fs::write(dir.join("SKILLS/sub/b.md"), "deep skill").unwrap();
    std::fs::write(dir.join("agent.toml"), agent_toml(label)).unwrap();
    std::fs::write(dir.join(".claude/settings.json"), SETTINGS).unwrap();
    std::fs::write(dir.join(".claude/settings.local.json"), "{}").unwrap();
    std::fs::write(dir.join(".codex/config.toml"), "x = 1\n").unwrap();
    std::fs::write(dir.join("CLAUDE.md"), "instructions").unwrap();
    std::fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
}

/// alice's Personal employee (EMP, incarnation 0) with the full tree, tombstoned.
fn tombstoned_full(f: &Fixture, soul: &str) -> (ConfirmationItem, Receipt) {
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(home, &item, &root).unwrap();
    write_full_tree(home, LABEL, soul);
    let outcome = tombstone_employee(home, LABEL, root.employee, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() }).unwrap();
    assert_eq!(outcome.seal, "sealed");
    (item, root)
}

fn file_personal(f: &Fixture, ax: &Ax, target: Option<&str>) -> Result<RestoreRequest, AdoptError> {
    file_adoption(f.home(), &env(ax), EMP, 0, target, Filer::Personal(Some(OwnerProof { user_id: &f.alice, factor: "totp" })))
}

fn store(home: &Path) -> ConfirmationStore {
    ConfirmationStore::from_connection(Connection::open(home.join(CONFIRMATIONS_DB)).unwrap()).unwrap()
}

/// Stage an Employee proposal with exactly these fields (the propose RPC's shape).
fn stage(home: &Path, id: u8, fields: &[(&str, &str)]) -> ConfirmationItem {
    let fields = fields.iter().map(|(name, value)| ProposalField { name: (*name).into(), value: ProposalValue::Text { value: (*value).into() } })
        .collect();
    let operation = ProposalOperationId { proposal_id: RequestId([id; 16]), revision: 1 };
    store(home).stage_proposal(operation, ConfirmationCategory::Employee, fields, &Accept).unwrap()
}

fn plan_fields<'a>(plan: &'a AdoptionPlan, adopt: &'a str, owner: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![("employee_id", plan.target.as_str()), ("disk_bytes", "67108864"), ("settings", plan.settings.as_str()),
        ("binding_kind", "personal"), ("owner_id", owner), ("adopt_quarantine", adopt)]
}

/// The propose step: plan, stage, bind (same apply.db transaction as the RPC's lock).
fn propose(f: &Fixture, ax: &Ax, request: &RestoreRequest) -> (AdoptionPlan, ConfirmationItem) {
    let home = f.home();
    let plan = prepare_proposal(home, &env(ax), &request.request_id).unwrap();
    let adopt = plan.adopt.encode();
    let item = stage(home, 0x42, &plan_fields(&plan, &adopt, &f.alice));
    let mut db = Connection::open(home.join(APPLY_DB)).unwrap();
    let tx = db.transaction().unwrap();
    bind_proposal(&tx, &plan, &proposal_key(&item), ax.now).unwrap();
    tx.commit().unwrap();
    (plan, item)
}

fn apply(f: &Fixture, ax: &Ax, item: &ConfirmationItem) -> (Result<Adopted, AdoptError>, Receipt) {
    let home = f.home();
    let new = receipt(item, 9, false);
    let mut persist = || save(home, item, &new).map_err(|_| ());
    (apply_item(home, &env(ax), item, &new, &f.bob, &mut persist), new)
}

/// Owner files, an Admin proposes and approves, the apply step adopts.
fn adopted(f: &Fixture, ax: &Ax, target: Option<&str>) -> (AdoptionPlan, ConfirmationItem, Receipt) {
    let request = file_personal(f, ax, target).unwrap();
    let (plan, item) = propose(f, ax, &request);
    let (result, new) = apply(f, ax, &item);
    assert_eq!(result.unwrap().employee, NEW);
    (plan, item, new)
}

fn mark_applied(home: &Path, item: &ConfirmationItem) {
    let mut store = store(home);
    let session = ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]));
    store.approve(&[item.binding], &session, [2; 32], 100, &Accept).unwrap();
    for _ in 0..3 { store.apply_next(&item.binding, &mut Accept).unwrap(); }
}

fn seal_of(home: &Path) -> String { row(home, 0).seal }
fn root_state(home: &Path) -> String {
    Connection::open(home.join(APPLY_DB)).unwrap()
        .query_row("SELECT root_state FROM root_allocations WHERE employee_hex=?1", [hex::encode(EMP.0)], |r| r.get(0)).unwrap()
}
fn quarantined(home: &Path, name: &str) -> bool { quarantine_path(home, LABEL, EMP, 0).join(name).exists() }
fn tasks_db(home: &Path, label: &str) {
    let path = home.join("tasks.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, assigned_to TEXT)").unwrap();
    db.execute("INSERT INTO tasks(id,assigned_to) VALUES('t1',?1)", [label]).unwrap();
    set_mode(&path, 0o600);
}
fn tasks_under(home: &Path, label: &str) -> u64 {
    crate::isolation_label_stores::LABEL_KEYED_STORES.iter().find(|s| s.name == "tasks").unwrap().count_under(home, label).unwrap()
}

#[test]
fn adopt_requires_authoritative_not_found() {
    let f = fixture();
    let (_, root) = tombstoned_full(&f, "soul");
    let ax = ax();
    *ax.root.answer.lock().unwrap() = RootAnswer::Unavailable;
    assert_eq!(file_personal(&f, &ax, None).err(), Some(AdoptError::Root(crate::isolation_retire::RootClass::Unavailable)));
    *ax.root.answer.lock().unwrap() = RootAnswer::Ready(root);
    assert_eq!(file_personal(&f, &ax, None).err(), Some(AdoptError::Root(crate::isolation_retire::RootClass::Ok)));
    *ax.root.answer.lock().unwrap() = RootAnswer::NotFound;
    assert_eq!(file_personal(&f, &ax, None).unwrap().kind, RequestKind::PersonalAdopt);
}

#[test]
fn adopt_refused_on_inconsistent() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    *ax.root.answer.lock().unwrap() = RootAnswer::Inconsistent;
    assert_eq!(file_personal(&f, &ax, None).err(), Some(AdoptError::Root(crate::isolation_retire::RootClass::Inconsistent)));
    let log = std::fs::read_to_string(f.home().join("security_audit.jsonl")).unwrap_or_default();
    assert!(log.contains("isolation.employee_root_inconsistent"));
    assert_eq!(seal_of(f.home()), "sealed");
}

#[test]
fn adopt_original_label_when_free() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (plan, _, new) = adopted(&f, &ax, None);
    assert_eq!(plan.target, LABEL);
    let (live, _, incarnation) = confirmed_row(f.home(), LABEL).unwrap().unwrap();
    assert_eq!((live.employee, live.disabled, incarnation), (new.employee, false, 0));
    assert_eq!(mirror(f.home(), LABEL).as_deref(), Some(f.alice.as_str()));
    // A label held by a live employee is never a target.
    assert!(file_personal(&f, &ax, Some(LABEL)).is_err());
}

#[test]
fn adopt_new_label_rewrites_agent_name_only() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (plan, _, _) = adopted(&f, &ax, Some("alice-next"));
    assert_eq!(plan.target, "alice-next");
    let written = std::fs::read_to_string(f.home().join("agents/alice-next/agent.toml")).unwrap();
    let original = std::fs::read_to_string(quarantine_path(f.home(), LABEL, EMP, 0).join("agent.toml")).unwrap();
    let changed: Vec<_> = original.lines().zip(written.lines()).filter(|(a, b)| a != b).collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].1, "name = \"alice-next\"");
    assert_eq!(original.lines().count(), written.lines().count());
    assert!(confirmed_row(f.home(), "alice-next").unwrap().is_some());
}

#[test]
fn adopt_keeps_only_allowed_top() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    adopted(&f, &ax, None);
    let dir = f.home().join("agents").join(LABEL);
    let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, [".claude", "CONTRACT.toml", "SKILLS", "SOUL.md", "agent.toml"]);
    let claude: Vec<String> = std::fs::read_dir(dir.join(".claude")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(claude, ["settings.json"]);
    assert!(dir.join("SKILLS/sub/b.md").exists());
    for kept in ["CLAUDE.md", ".env", ".codex", ".claude/settings.local.json"] { assert!(quarantined(f.home(), kept), "{kept} sealed"); }
}

#[test]
fn adopt_strips_command_keys_from_settings() {
    assert_eq!(ADOPT_SETTINGS_KEYS, ["permissions.deny", "permissions.ask", "model"]);
    let filtered: Value = serde_json::from_slice(&filter_settings(Some(SETTINGS.as_bytes())).unwrap()).unwrap();
    assert_eq!(filtered, json!({"model": "claude-sonnet-4-6", "permissions": {"deny": ["Bash(rm:*)"], "ask": ["Write"]}}));
    // A malformed narrowing key refuses (dropping it would widen).
    assert!(filter_settings(Some(br#"{"permissions":{"deny":"Bash"}}"#)).is_err());
    assert_eq!(rename_agent(b"# c\n[agent]\nname = \"a\"\nrole = \"r\"\n[model]\nname = \"m\"\n", "b").unwrap(),
        b"# c\n[agent]\nname = \"b\"\nrole = \"r\"\n[model]\nname = \"m\"\n".to_vec());
    assert!(rename_agent(b"[model]\nname = \"m\"\n", "x").is_err(), "no [agent] name");
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    adopted(&f, &ax, None);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(f.home().join("agents").join(LABEL).join(".claude/settings.json")).unwrap()).unwrap();
    for dropped in ["env", "apiKeyHelper", "statusLine", "hooks", "mcpServers"] { assert!(written.get(dropped).is_none(), "{dropped}"); }
    assert!(written["permissions"].get("allow").is_none());
    // The original stays sealed, byte for byte.
    assert_eq!(std::fs::read_to_string(quarantine_path(f.home(), LABEL, EMP, 0).join(".claude/settings.json")).unwrap(), SETTINGS);
}

#[test]
fn adopt_rejects_absolute_old_label_paths() {
    let f = fixture();
    let old = f.home().join("agents").join(LABEL);
    tombstoned_full(&f, &format!("see {}/notes.md", old.display()));
    let ax = ax();
    let request = file_personal(&f, &ax, Some("alice-next")).unwrap();
    assert_eq!(prepare_proposal(f.home(), &env(&ax), &request.request_id).err(), Some(AdoptError::PathRejected));
    assert_eq!(AdoptError::PathRejected.code(), "ADOPT_PATH_REJECTED");
    assert!(!f.home().join("agents/alice-next").exists());
}

#[test]
fn adopt_remainder_stays_sealed_and_purged_with_row() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    tasks_db(f.home(), LABEL);
    let ax = ax();
    let (plan, _, _) = adopted(&f, &ax, None);
    assert_eq!(seal_of(f.home()), "adopted");
    for name in ["CLAUDE.md", ".env", "agent.toml", ".claude"] { assert!(quarantined(f.home(), name), "{name}"); }
    for name in ["SOUL.md", "SKILLS"] { assert!(!quarantined(f.home(), name), "{name} moved"); }
    // The row's sealed stores live under its key and go with the row (F6i purge).
    let key = plan.adopt.store_key();
    assert_eq!(tasks_under(f.home(), &key), 1);
    crate::isolation_label_stores::LABEL_KEYED_STORES.iter().find(|s| s.name == "tasks").unwrap().purge(f.home(), &key).unwrap();
    assert_eq!(tasks_under(f.home(), &key), 0);
}

#[test]
fn adopt_post_transform_passes_v2_digest() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (plan, item, _) = adopted(&f, &ax, None);
    let db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    let (_, digest, origin, reference) = marker(&db, LABEL).unwrap().unwrap();
    assert_eq!((origin.as_str(), reference.as_deref()), ("adopted", Some(plan.adopt.encode().as_str())));
    let snapshot = crate::handlers::employee_dirs::snapshot(f.home(), &f.home().join("agents"), LABEL).unwrap();
    assert_eq!(snapshot.digest, digest);
    assert_eq!(snapshot.settings().unwrap(), plan.settings);
    // Publish / read-back re-validate against the marker.
    assert_eq!(validate_item(f.home(), &env(&ax), &item, &plan.adopt, &f.bob), Ok(AdoptPhase::Adopted { employee: NEW }));
    std::fs::write(f.home().join("agents").join(LABEL).join("SOUL.md"), "edited").unwrap();
    assert_eq!(validate_item(f.home(), &env(&ax), &item, &plan.adopt, &f.bob).err(), Some(AdoptError::ForeignDir));
}

#[test]
fn adopt_never_copies_root_workspace() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    adopted(&f, &ax, None);
    // The engine only ever asked the root for a ReadBack of the old id; the
    // new allocation (Allocate) is the normal Employee apply, empty.
    let actions = ax.root.actions.lock().unwrap().clone();
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|action| action == &format!("{:?}", Action::ReadBack)), "{actions:?}");
    // Nothing but the allowlist reached the new directory.
    assert!(!f.home().join("agents").join(LABEL).join("workspace").exists());
}

#[test]
fn adopt_old_id_authorities_dead() {
    let f = fixture();
    let (_, old) = tombstoned_full(&f, "soul");
    let ax = ax();
    let (_, item, new) = adopted(&f, &ax, None);
    mark_applied(f.home(), &item);
    let old_token = duduclaw_core::identity_token::mint_token_v2(&KEY, LABEL,
        &duduclaw_core::identity_token::AgentIncarnation { employee: old.employee.0, incarnation: 0 });
    let new_token = duduclaw_core::current_agent_token(f.home(), &KEY, LABEL).unwrap();
    assert!(!duduclaw_core::verify_agent_token(f.home(), &KEY, LABEL, &old_token), "F6m token v2 refuses the old id");
    assert!(duduclaw_core::verify_agent_token(f.home(), &KEY, LABEL, &new_token));
    let lifecycle = read_lifecycle_with(f.home(), LABEL, &Root(vec![new.clone()])).unwrap();
    assert_eq!((lifecycle.employee, lifecycle.incarnation), (NEW, 0));
    assert_ne!(lifecycle.employee, old.employee);
    // The old id cannot be restored: its row is closed and its root absent.
    assert_eq!(seal_of(f.home()), "adopted");
    assert_eq!(root_state(f.home()), "root_absent");
}

#[test]
fn adopt_label_stores_sealed_not_carried() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    tasks_db(f.home(), LABEL);
    let ax = ax();
    let (plan, _, _) = adopted(&f, &ax, None);
    assert_eq!(tasks_under(f.home(), LABEL), 0, "the new id never sees old rows");
    assert_eq!(tasks_under(f.home(), &plan.adopt.store_key()), 1);
}

#[test]
fn adopt_crash_mid_rekey_never_goes_live_with_old_rows() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    tasks_db(f.home(), LABEL);
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (plan, item) = propose(&f, &ax, &request);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::MidRekey)));
    let (crashed, _) = apply(&f, &ax, &item);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    // Not live: the label's row is still the old, disabled id.
    let (row_receipt, _, _) = confirmed_row(f.home(), LABEL).unwrap().unwrap();
    assert_eq!((row_receipt.employee, row_receipt.disabled), (EMP, true));
    assert_eq!(tasks_under(f.home(), LABEL), 1, "crash before the tasks store moved");
    // Recovery finishes the re-key first (S10); the card is still open.
    recover_adoptions(f.home(), &env(&ax), LABEL).unwrap();
    assert_eq!(tasks_under(f.home(), LABEL), 0);
    assert_eq!(tasks_under(f.home(), &plan.adopt.store_key()), 1);
    // The store's compensation then rolls the tree back.
    compensate_item(f.home(), &env(&ax), &item).unwrap();
    assert!(quarantined(f.home(), "SOUL.md") && !f.home().join("agents").join(LABEL).exists());
    assert_eq!(seal_of(f.home()), "sealed");
}

/// Regression: the `MidRekey` crash point used to surface through the apply
/// step's failure path, which rolled the row back to `sealed` and the request
/// to `failed` in-process, so recovery never saw an `adopting` row and never
/// finished the re-key. A crash leaves the row for recovery, which re-keys
/// every store before it rolls back (proposal gone).
#[test]
fn adopt_mid_rekey_crash_leaves_row_adopting_for_recovery() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    tasks_db(f.home(), LABEL);
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (plan, item) = propose(&f, &ax, &request);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::MidRekey)));
    let (crashed, _) = apply(&f, &ax, &item);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    // No cleanup ran: the moved tree, the `adopting` row and the `applying` request stay.
    assert_eq!(seal_of(f.home()), "adopting");
    assert_eq!(stored(f.home(), &request.request_id).state, "applying");
    assert!(f.home().join("agents").join(LABEL).join("SOUL.md").exists());
    assert_eq!(tasks_under(f.home(), LABEL), 1);
    // The proposal is gone: recovery re-keys first, then rolls back.
    sql(f.home(), APPLY_DB, "UPDATE employee_restores SET proposal_id=?1 WHERE request_id=?2", &[&"f".repeat(32), &request.request_id]);
    let done = recover_adoptions(f.home(), &env(&ax), LABEL).unwrap();
    assert_eq!(done, vec![(request.request_id.clone(), "rolled_back")]);
    assert_eq!(tasks_under(f.home(), LABEL), 0);
    assert_eq!(tasks_under(f.home(), &plan.adopt.store_key()), 1);
    assert!(quarantined(f.home(), "SOUL.md") && !f.home().join("agents").join(LABEL).exists());
    assert_eq!(seal_of(f.home()), "sealed");
    assert_eq!(stored(f.home(), &request.request_id).state, "failed");
}

#[test]
fn adopt_records_root_absent() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    assert_eq!(root_state(f.home()), "allocated");
    adopted(&f, &ax, None);
    assert_eq!(root_state(f.home()), "root_absent");
    let log = std::fs::read_to_string(f.home().join("security_audit.jsonl")).unwrap_or_default();
    assert!(log.contains("isolation.employee_root_absent") && log.contains("isolation.employee_adopted"));
}

#[test]
fn adopt_personal_same_owner_only() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    // Another user (an Admin) cannot file for alice's row: NOT_FOUND.
    let bob = file_adoption(f.home(), &env(&ax), EMP, 0, None, Filer::Personal(Some(OwnerProof { user_id: &f.bob, factor: "totp" })));
    assert_eq!(bob.err(), Some(AdoptError::Restore(Refusal::NotFound)));
    assert_eq!(file_adoption(f.home(), &env(&ax), EMP, 0, None, Filer::Personal(None)).err(),
        Some(AdoptError::Restore(Refusal::FactorRequired)));
    // A proposal naming another owner never validates.
    let request = file_personal(&f, &ax, None).unwrap();
    let plan = prepare_proposal(f.home(), &env(&ax), &request.request_id).unwrap();
    let adopt = plan.adopt.encode();
    let forged = stage(f.home(), 0x51, &plan_fields(&plan, &adopt, &f.bob));
    assert_eq!(validate_item(f.home(), &env(&ax), &forged, &plan.adopt, &f.bob).err(), Some(AdoptError::NotSameOwner));
}

#[test]
fn adopt_shared_same_tenant_only() {
    let f = fixture();
    let home = f.home();
    let item = applied(home, 2, "team-x", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(home, &item, &root).unwrap();
    write_tree(home, "team-x");
    tombstone_employee(home, "team-x", root.employee, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() }).unwrap();
    let ax = ax();
    let shared = EmployeeId([2; 16]);
    // Personal filing on a Shared row: NOT_FOUND; a member outside the snapshot refuses.
    assert_eq!(file_adoption(home, &env(&ax), shared, 0, None, Filer::Personal(Some(OwnerProof { user_id: &f.alice, factor: "totp" }))).err(),
        Some(AdoptError::Restore(Refusal::NotFound)));
    let outsider = json!([{"user_id": f.alice, "access_level": "owner"}]);
    assert_eq!(file_adoption(home, &env(&ax), shared, 0, None, Filer::Shared { requester: &f.bob, members: outsider }).err(),
        Some(AdoptError::MembersInvalid));
    let request = file_adoption(home, &env(&ax), shared, 0, None, Filer::Shared { requester: &f.bob, members: json!([]) }).unwrap();
    assert_eq!((request.kind, request.owner.as_deref(), request.tenant.as_str()), (RequestKind::SharedAdopt, None, f.tenant.as_str()));
    // A proposal for another tenant never validates.
    let adopt = AdoptRef { employee: shared, incarnation: 0, digest: row_digest(home, shared) }.encode();
    let forged = stage(home, 0x61, &[("employee_id", "team-x"), ("disk_bytes", "67108864"), ("settings", "{}"),
        ("binding_kind", "shared"), ("tenant_id", "other-tenant"), ("adopt_quarantine", adopt.as_str())]);
    let parsed = AdoptRef::parse(&adopt).unwrap();
    assert_eq!(validate_item(home, &env(&ax), &forged, &parsed, &f.bob).err(), Some(AdoptError::NotSameTenant));
}

fn row_digest(home: &Path, employee: EmployeeId) -> String {
    Connection::open(home.join(APPLY_DB)).unwrap().query_row("SELECT digest FROM employee_quarantine WHERE employee_hex=?1",
        [hex::encode(employee.0)], |r| r.get(0)).unwrap()
}

#[test]
fn adopt_normal_proposal_refuses_adopted_marker() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (_, _, new) = adopted(&f, &ax, None);
    // Even with the row disabled (no live owner of the directory), the
    // normal Personal flow never adopts a directory with an `adopted` marker.
    store_receipt(f.home(), LABEL, &Receipt { disabled: true, ..new });
    let db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    assert!(crate::handlers::employee_dirs::check_marker(&db, f.home(), &f.home().join("agents"), LABEL, &f.alice).is_err());
}

#[test]
fn adopt_digest_mismatch_refused() {
    assert!(AdoptRef::parse("00:0:00").is_none());
    assert!(AdoptRef::parse(&format!("{}:01:{}", "a".repeat(32), "b".repeat(64))).is_none(), "no leading zeros");
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (plan, item) = propose(&f, &ax, &request);
    let wrong = AdoptRef { digest: "0".repeat(64), ..plan.adopt.clone() };
    assert_eq!(validate_item(f.home(), &env(&ax), &item, &wrong, &f.bob).err(), Some(AdoptError::DigestMismatch));
    // A tree changed in quarantine after the proposal: refused, row invalid.
    std::fs::write(quarantine_path(f.home(), LABEL, EMP, 0).join("CLAUDE.md"), "changed").unwrap();
    let (result, _) = apply(&f, &ax, &item);
    assert_eq!(result.err(), Some(AdoptError::DigestMismatch));
    assert_eq!(seal_of(f.home()), "invalid");
    assert!(!f.home().join("agents").join(LABEL).exists());
}

/// Drives the real confirmation store: the stage step fails after the root
/// allocated the new id, so the store compensates (`disable_and_read_back`).
struct Applier<'a> { f: &'a Fixture, ax: &'a Ax, fail_save: bool, disabled: bool }
impl ProposalApplier for Applier<'_> {
    fn stage(&mut self, item: &ConfirmationItem) -> Result<(), ConfirmationError> {
        let new = receipt(item, 9, false);
        let (home, fail) = (self.f.home(), self.fail_save);
        let mut persist = || if fail { Err(()) } else { save(home, item, &new).map_err(|_| ()) };
        apply_item(home, &env(self.ax), item, &new, &self.f.bob, &mut persist).map(|_| ()).map_err(|_| ConfirmationError::Apply)
    }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn disable_and_read_back(&mut self, item: &ConfirmationItem) -> Result<(), ConfirmationError> {
        self.disabled = true;
        compensate_item(self.f.home(), &env(self.ax), item).map_err(|_| ConfirmationError::Compensation)
    }
}

#[test]
fn adopt_failure_compensates_new_allocation() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let request = file_personal(&f, &ax, None).unwrap();
    let (_, item) = propose(&f, &ax, &request);
    let mut store = store(f.home());
    let session = ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]));
    store.approve(&[item.binding], &session, [2; 32], 100, &Accept).unwrap();
    let mut applier = Applier { f: &f, ax: &ax, fail_save: true, disabled: false };
    let _ = store.apply_next(&item.binding, &mut applier);
    assert!(applier.disabled, "the new allocation is compensated");
    assert!(quarantined(f.home(), "SOUL.md") && quarantined(f.home(), "SKILLS"));
    assert!(!f.home().join("agents").join(LABEL).exists());
    assert_eq!(seal_of(f.home()), "sealed", "needs_reseal re-verified");
    assert_eq!(stored(f.home(), &request.request_id).state, "failed");
    let db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    assert!(marker(&db, LABEL).unwrap().is_none());
}

#[test]
fn adopt_crash_rolls_back_or_forward() {
    // Forward: crash after the confirmed row, proposal then Applied.
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax0 = ax();
    let request = file_personal(&f, &ax0, None).unwrap();
    let (_, item) = propose(&f, &ax0, &request);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterSave)));
    let (crashed, _) = apply(&f, &ax0, &item);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    assert_eq!(seal_of(f.home()), "adopting");
    mark_applied(f.home(), &item);
    let done = recover_adoptions(f.home(), &env(&ax0), LABEL).unwrap();
    assert_eq!(done, vec![(request.request_id.clone(), "rolled_forward")]);
    assert_eq!((seal_of(f.home()).as_str(), root_state(f.home()).as_str()), ("adopted", "root_absent"));
    assert_eq!(stored(f.home(), &request.request_id).state, "done");

    // Back: crash right after the move; the proposal is gone (never Applied).
    let g = fixture();
    tombstoned_full(&g, "soul");
    let bx = ax();
    let request = file_personal(&g, &bx, None).unwrap();
    let (_, item) = propose(&g, &bx, &request);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(|point| point == AdoptHook::AfterMove)));
    let (crashed, _) = apply(&g, &bx, &item);
    ADOPT_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(crashed.err(), Some(AdoptError::Unavailable));
    assert!(g.home().join("agents").join(LABEL).join("SOUL.md").exists());
    sql(g.home(), APPLY_DB, "UPDATE employee_restores SET proposal_id=?1 WHERE request_id=?2", &[&"f".repeat(32), &request.request_id]);
    let done = recover_adoptions(g.home(), &env(&bx), LABEL).unwrap();
    assert_eq!(done, vec![(request.request_id.clone(), "rolled_back")]);
    assert!(quarantined(g.home(), "SOUL.md") && !g.home().join("agents").join(LABEL).exists());
    assert_eq!(seal_of(g.home()), "sealed");
    assert_eq!(stored(g.home(), &request.request_id).state, "failed");
}

#[test]
fn adopt_single_use_per_row() {
    let f = fixture();
    tombstoned_full(&f, "soul");
    let ax = ax();
    let (plan, _, _) = adopted(&f, &ax, Some("alice-next"));
    // The row is closed: no second request, no second proposal.
    assert!(file_personal(&f, &ax, Some("alice-third")).is_err());
    let adopt = plan.adopt.encode();
    let mut second = plan.clone();
    second.target = "alice-third".into();
    let again = stage(f.home(), 0x71, &plan_fields(&second, &adopt, &f.alice));
    assert_eq!(validate_item(f.home(), &env(&ax), &again, &plan.adopt, &f.bob).err(), Some(AdoptError::AlreadyUsed));
    let db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    assert!(bind_proposal(&db, &plan, &proposal_key(&again), ax.now).is_err());
}

/// F6d: the adoption request RPC bodies, post-commit hook and boot recovery.
#[path = "isolation_adopt_rpc_tests.rs"]
mod f6d;

/// F6fb fix round: resume after `save()`, Shared rollback, replay binding,
/// compensation after commit, path check, licence cap.
#[path = "isolation_adopt_f6fb_tests.rs"]
mod f6fb;
