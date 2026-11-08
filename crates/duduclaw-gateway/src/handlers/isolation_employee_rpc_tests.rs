//! A4: Personal employee requests, admin proposals and the tombstone. The
//! root allocator is not reachable in unit tests: the apply path is exercised
//! through `save_confirmed_receipt` with a fixture receipt, as in A2.
use super::*;
use duduclaw_auth::members::{MemberBinding, NewMember};
use duduclaw_auth::{OrgError, UserStatus};
use duduclaw_migration::confirmation::{ApprovalSession, ProposalApplier};

const LABEL: &str = "alice-helper";
const CSRF: [u8; 32] = [7; 32];

struct Accept;
impl ProposalValidator for Accept {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}
impl ProposalApplier for Accept {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}

struct Fixture { dir: tempfile::TempDir, users: UserDb, alice: String, bob: String }
impl Fixture {
    fn home(&self) -> &Path { self.dir.path() }
}

fn fixture() -> Fixture {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let users = UserDb::new(&dir.path().join("users.db")).unwrap();
    let alice = users.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
    let bob = users.create_user("bob@example.test", "Bob", "password1", UserRole::Admin).unwrap().id;
    login(dir.path(), &alice);
    login(dir.path(), &bob);
    Fixture { dir, users, alice, bob }
}

fn sql(home: &Path, db: &str, statement: &str, values: &[&dyn rusqlite::ToSql]) {
    Connection::open(home.join(db)).unwrap().execute(statement, values).unwrap();
}

fn login(home: &Path, id: &str) {
    sql(home, "users.db", "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1", &[&id]);
}

fn items(home: &Path) -> Vec<ConfirmationItem> { read_items(home).unwrap() }

fn employee_items(home: &Path, label: &str) -> Vec<ConfirmationItem> {
    items(home).into_iter().filter(|item| field(item, "employee_id") == Some(label)).collect()
}

fn request(f: &Fixture, caller: &str, label: &str) -> Result<Value, &'static str> {
    request_personal(f.home(), &f.users, caller, label, "Alice 的助理", false)
}

fn request_id(value: &Value) -> RequestId {
    RequestId(parse_hex16(value["request_id"].as_str().unwrap()).unwrap())
}

fn mirror(home: &Path, label: &str) -> Option<String> {
    duduclaw_auth::db::personal_agent_owner(&Connection::open(home.join("users.db")).unwrap(), label).unwrap()
}

fn members(f: &Fixture, label: &str) -> u32 {
    f.users.isolation_authority_snapshot(None, label).unwrap().agent_member_count
}

fn proof(verified_at: i64) -> MigrationConfirmationSession {
    MigrationConfirmationSession { verified_at: verified_at as u64, csrf: CSRF }
}

fn decision_session() -> ApprovalSession {
    ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]))
}

fn propose_input(value: Value) -> ProposeRequest { serde_json::from_value(value).unwrap() }

/// Stage, approve and apply (accepting applier) one Employee proposal, then
/// record the root receipt through the real apply bookkeeping.
fn applied_personal(f: &Fixture, id: u8, label: &str, owner: &str) -> (ConfirmationItem, Receipt) {
    open_requests(f.home()).unwrap();
    let mut store = open_confirmations(f.home()).unwrap();
    let fields = vec![text_field("employee_id", label), text_field("disk_bytes", "67108864"),
        text_field("binding_kind", "personal"), text_field("owner_id", owner)];
    let operation = ProposalOperationId { proposal_id: RequestId([id; 16]), revision: 1 };
    let staged = store.stage_proposal(operation, ConfirmationCategory::Employee, fields, &Accept).unwrap();
    store.approve(&[staged.binding], &decision_session(), [2; 32], 100, &Accept).unwrap();
    for _ in 0..3 { store.apply_next(&staged.binding, &mut Accept).unwrap(); }
    let item = items(f.home()).into_iter().find(|i| i.binding == staged.binding).unwrap();
    assert_eq!(item.status, ConfirmationStatus::Applied);
    let spec = allocation::specification(&item).unwrap();
    let receipt = Receipt { binding: item.binding, label: spec.label, employee: EmployeeId([id; 16]),
        uid: 70_000 + u32::from(id), bytes: spec.bytes, inodes: spec.inodes, disabled: false,
        principal_binding: spec.binding };
    crate::handlers::save_confirmed_receipt(f.home(), &item, &receipt).unwrap();
    (item, receipt)
}

fn error_of(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => error.as_str().unwrap_or_default().to_owned(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn caller(id: &str, role: UserRole) -> UserContext {
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = id.into();
    ctx.role = role;
    ctx
}

#[test]
fn personal_request_owner_is_caller() {
    let f = fixture();
    // No request field can name (or claim) an owner or a principal.
    let ok = json!({"label": LABEL, "display_name": "Alice 的助理"});
    assert!(serde_json::from_value::<PersonalRequest>(ok.clone()).is_ok());
    for extra in ["owner_id", "user_id", "binding_kind", "tenant_id", "settings", "admin"] {
        let mut forged = ok.clone();
        forged[extra] = json!(f.bob);
        assert!(serde_json::from_value::<PersonalRequest>(forged).is_err(), "{extra}");
    }

    let value = request(&f, &f.alice, LABEL).unwrap();
    assert_eq!((value["status"].as_str(), value["stage"].as_str()), (Some("pending"), Some("requested")));
    let staged = employee_items(f.home(), LABEL);
    assert_eq!(staged.len(), 1);
    let item = &staged[0];
    assert_eq!(item.binding.operation_id.proposal_id, request_id(&value));
    assert_eq!(field(item, "owner_id"), Some(f.alice.as_str()));
    assert_eq!(field(item, "binding_kind"), Some("personal"));
    assert_eq!(field(item, "tenant_id"), None);
    assert_eq!(allocation::specification(item).unwrap().binding,
        Some(EmployeeBinding::Personal(Text::new(f.alice.clone()).unwrap())));

    // Only the caller's own requests are listed, and only the proposal proves ownership.
    let mine = list_own_requests(f.home(), &f.alice).unwrap();
    assert_eq!(mine["requests"].as_array().unwrap().len(), 1);
    assert_eq!(mine["requests"][0]["display_name"], "Alice 的助理");
    assert_eq!(mine["requests"][0]["label"], LABEL);
    assert_eq!(list_own_requests(f.home(), &f.bob).unwrap()["requests"], json!([]));
    let forged_id = value["request_id"].as_str().unwrap().to_owned();
    let index = open_requests(f.home()).unwrap();
    index.execute("UPDATE personal_employee_requests SET owner_user_id=?1 WHERE request_id=?2",
        params![f.bob, forged_id]).unwrap();
    assert_eq!(list_own_requests(f.home(), &f.bob).unwrap()["requests"], json!([]), "a forged index row is not a request");
    assert_eq!(list_own_requests(f.home(), &f.alice).unwrap()["requests"], json!([]));
}

#[tokio::test]
async fn personal_request_requires_active_user() {
    let f = fixture();
    let home = f.home();
    let carol = f.users.create_user("carol@example.test", "Carol", "password1", UserRole::Employee).unwrap().id;
    sql(home, "users.db", "UPDATE users SET must_change_password=0,last_login=NULL WHERE id=?1", &[&carol]);
    assert_eq!(request(&f, &carol, LABEL), Err(REQUESTER_INELIGIBLE), "never logged in");
    login(home, &carol);
    sql(home, "users.db", "UPDATE users SET must_change_password=1 WHERE id=?1", &[&carol]);
    assert_eq!(request(&f, &carol, LABEL), Err(REQUESTER_INELIGIBLE), "forced password change");
    for status in [UserStatus::Suspended, UserStatus::Offboarded] {
        login(home, &carol);
        f.users.set_user_status(&carol, status).unwrap();
        assert_eq!(request(&f, &carol, LABEL), Err(REQUESTER_INELIGIBLE), "{status}");
    }
    assert_eq!(request(&f, "missing-user", LABEL), Err(REQUESTER_INELIGIBLE));
    // Nothing was staged for any refused requester.
    assert!(!home.join(CONFIRMATIONS_DB).exists());
    // Labels and names are checked before anything is written.
    login(home, &carol);
    for label in ["", "Alice", "dashboard", "a/b", "x".repeat(65).as_str()] {
        assert_eq!(request(&f, &carol, label), Err(LABEL_INVALID), "{label}");
    }
    assert_eq!(request_personal(home, &f.users, &carol, LABEL, " \n", false), Err(DISPLAY_INVALID));
    assert_eq!(request_personal(home, &f.users, &carol, LABEL, "x", true), Err(LABEL_TAKEN));
    assert!(!home.join(CONFIRMATIONS_DB).exists());
    assert!(request(&f, &carol, LABEL).is_ok());

    // The RPC refuses outside protected mode and for forced-password callers.
    let handler = MethodHandler::new(home.to_path_buf()).await;
    let params = json!({"label": "carol-two", "display_name": "Carol"});
    if !crate::runtime_readiness::is_protected_linux() {
        let frame = handler.handle_isolation_employee(REQUEST, params.clone(), &caller(&carol, UserRole::Employee)).await;
        assert_eq!(error_of(&frame), NOT_PROTECTED);
    }
    let mut forced = caller(&carol, UserRole::Employee);
    forced.must_change_password = true;
    assert_eq!(error_of(&handler.handle_isolation_employee(REQUEST, params.clone(), &forced).await), REQUESTER_INELIGIBLE);
    assert_eq!(error_of(&handler.handle_isolation_employee(REQUEST, params, &UserContext::admin_fallback()).await), NOT_ALLOWED);
}

#[test]
fn personal_request_duplicate_refused() {
    let f = fixture();
    let first = request(&f, &f.alice, LABEL).unwrap();
    assert_eq!(request(&f, &f.alice, LABEL), Err(DUPLICATE));
    // Another user cannot request a label that already has an open proposal.
    assert_eq!(request(&f, &f.bob, LABEL), Err(LABEL_PENDING));
    assert_eq!(employee_items(f.home(), LABEL).len(), 1);
    // A bounded number of open requests per user.
    request(&f, &f.alice, "alice-two").unwrap();
    request(&f, &f.alice, "alice-three").unwrap();
    assert_eq!(request(&f, &f.alice, "alice-four"), Err(TOO_MANY));
    assert!(request(&f, &f.bob, "bob-one").is_ok(), "the cap is per user");

    // Once an Admin rejects it on the approval card, the label is free again.
    let mut store = open_confirmations(f.home()).unwrap();
    let pending = latest(&items(f.home()), request_id(&first)).unwrap().binding;
    store.reject_pending(&[pending], &decision_session(), [2; 32], 100).unwrap();
    let mine = list_own_requests(f.home(), &f.alice).unwrap();
    let rejected = mine["requests"].as_array().unwrap().iter().find(|r| r["label"] == LABEL).unwrap().clone();
    assert_eq!(rejected["status"], "rejected");
    let again = request(&f, &f.alice, LABEL).unwrap();
    assert_ne!(again["request_id"], first["request_id"]);
}

#[test]
fn personal_request_never_auto_applies() {
    let f = fixture();
    let value = request(&f, &f.alice, LABEL).unwrap();
    let item = latest(&items(f.home()), request_id(&value)).unwrap().clone();
    assert_eq!(item.status, ConfirmationStatus::Pending);
    assert!(item.high_risk, "never part of a bulk approval");
    // No product configuration: the approval validator cannot accept it as is.
    assert_eq!(field(&item, "settings"), None);
    // Nothing of the allocation, mirror or binding exists.
    assert!(confirmed_employees(f.home()).unwrap().is_empty());
    assert_eq!(mirror(f.home(), LABEL), None);
    assert_eq!(members(&f, LABEL), 0);
    assert!(lifecycle::read_lifecycle(f.home(), LABEL).is_err());
    // Repeating or listing never advances it.
    assert_eq!(request(&f, &f.alice, LABEL), Err(DUPLICATE));
    for _ in 0..2 { list_own_requests(f.home(), &f.alice).unwrap(); admin_list(f.home()).unwrap(); }
    assert_eq!(latest(&items(f.home()), request_id(&value)).unwrap().status, ConfirmationStatus::Pending);

    // Even the admin preparation only stages the next Pending revision.
    let now = 10_000;
    let prepared = prepare_propose(f.home(), &f.users, Some(&proof(now)), &f.bob, propose_input(json!({
        "label": LABEL, "binding_kind": "personal", "request_id": value["request_id"], "csrf_token": CSRF})), now).unwrap();
    assert_eq!(prepared.display_name, "Alice 的助理");
    let staged = stage_prepared(f.home(), Some(&proof(now)), &f.bob, &prepared, "{}", now).unwrap();
    assert_eq!((staged["revision"].as_u64(), staged["status"].as_str()), (Some(2), Some("pending")));
    let revisions = employee_items(f.home(), LABEL);
    assert_eq!(revisions.iter().map(|i| (i.binding.operation_id.revision, i.status)).collect::<Vec<_>>(),
        vec![(1, ConfirmationStatus::ExpiredRejected), (2, ConfirmationStatus::Pending)]);
    let mine = list_own_requests(f.home(), &f.alice).unwrap();
    assert_eq!((mine["requests"][0]["status"].as_str(), mine["requests"][0]["stage"].as_str()),
        (Some("pending"), Some("proposed")));
    assert!(confirmed_employees(f.home()).unwrap().is_empty());
    assert_eq!(members(&f, LABEL), 0);
}

#[tokio::test]
async fn admin_propose_requires_step_up() {
    let f = fixture();
    let home = f.home();
    // Role gates come first: a staff member, the bootstrap identity and an
    // Admin role without a current users.db Admin are all refused.
    let handler = MethodHandler::new(home.to_path_buf()).await;
    let params = json!({"label": "team-desk", "binding_kind": "shared", "csrf_token": CSRF});
    for method in [PROPOSE, TOMBSTONE, LIST] {
        let staff = handler.handle_isolation_employee(method, params.clone(), &caller(&f.alice, UserRole::Employee)).await;
        assert_eq!(error_of(&staff), ADMIN_REQUIRED, "{method}");
        let unverified = handler.handle_isolation_employee(method, params.clone(), &caller(&f.bob, UserRole::Admin)).await;
        assert_eq!(error_of(&unverified), ADMIN_REQUIRED, "{method}");
        let bootstrap = handler.handle_isolation_employee(method, params.clone(), &UserContext::admin_fallback()).await;
        assert_eq!(error_of(&bootstrap), NOT_ALLOWED, "{method}");
    }
    for unknown in ["isolation.employee_approve", "me.isolation.approve_personal_employee"] {
        assert!(owns(unknown));
        let frame = handler.handle_isolation_employee(unknown, json!({}), &caller(&f.bob, UserRole::Admin)).await;
        assert_eq!(error_of(&frame), NOT_ALLOWED, "{unknown}");
    }
    for other in ["isolation.connector_list", "isolation.recording_status", "me.google.status", "evil.isolation.employee_list"] {
        assert!(!owns(other), "{other}");
    }

    // Requests cannot carry authority claims.
    for extra in ["admin", "step_up_valid", "mfa_enrolled", "tenant_id", "settings", "approve"] {
        let mut forged = params.clone();
        forged[extra] = json!(true);
        assert!(serde_json::from_value::<ProposeRequest>(forged).is_err(), "{extra}");
    }
    assert!(serde_json::from_value::<ProposeRequest>(json!({"label": "x", "binding_kind": "shared"})).is_err(), "csrf required");

    // Without a fresh proof bound to this session and its CSRF token, nothing happens.
    let now = 10_000;
    let shared = || propose_input(params.clone());
    assert!(prepare_propose(home, &f.users, None, &f.bob, shared(), now).is_err());
    assert!(prepare_propose(home, &f.users, Some(&proof(now - 301)), &f.bob, shared(), now).is_err());
    assert!(prepare_propose(home, &f.users, Some(&proof(now + 1)), &f.bob, shared(), now).is_err());
    let mut wrong = shared();
    wrong.csrf_token = [8; 32];
    assert!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, wrong, now).is_err());
    assert!(!home.join(CONFIRMATIONS_DB).exists());

    // With the proof: the principal rules of the proposal.
    let tenant = f.users.isolation_authority_snapshot(None, "team-desk").unwrap().tenant_id;
    let ok = prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, shared(), now).unwrap();
    assert_eq!(ok.binding, EmployeeBinding::Shared(Text::new(tenant).unwrap()));
    for (kind, owner) in [("shared", Some(f.alice.as_str())), ("personal", None), ("tenant", None), ("Personal", Some(f.alice.as_str()))] {
        let input = propose_input(json!({"label": "team-desk", "binding_kind": kind, "owner_id": owner, "csrf_token": CSRF}));
        assert_eq!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, input, now).err(), Some(BINDING_INVALID), "{kind}");
    }
    sql(home, "users.db", "UPDATE users SET must_change_password=1 WHERE id=?1", &[&f.alice]);
    let input = propose_input(json!({"label": "alice-desk", "binding_kind": "personal", "owner_id": f.alice, "csrf_token": CSRF}));
    assert_eq!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, input, now).err(), Some(OWNER_INELIGIBLE));
    login(home, &f.alice);

    // A staff request is prepared only for its own owner and label.
    let value = request(&f, &f.alice, LABEL).unwrap();
    let id = value["request_id"].clone();
    for (label, owner) in [(LABEL, Some(f.bob.as_str())), ("other-label", None)] {
        let input = propose_input(json!({"label": label, "binding_kind": "personal", "owner_id": owner,
            "request_id": id, "csrf_token": CSRF}));
        assert_eq!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, input, now).err(), Some(REQUEST_MISMATCH), "{label}");
    }
    let input = propose_input(json!({"label": LABEL, "binding_kind": "shared", "request_id": id, "csrf_token": CSRF}));
    assert_eq!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, input, now).err(), Some(REQUEST_MISMATCH));
    let input = propose_input(json!({"label": LABEL, "binding_kind": "personal", "request_id": "AB", "csrf_token": CSRF}));
    assert_eq!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, input, now).err(), Some(REQUEST_NOT_FOUND));
    // A new proposal cannot take a label with an open request.
    let input = propose_input(json!({"label": LABEL, "binding_kind": "shared", "csrf_token": CSRF}));
    assert_eq!(prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, input, now).err(), Some(LABEL_PENDING));
    let prepared = prepare_propose(home, &f.users, Some(&proof(now)), &f.bob, propose_input(json!({
        "label": LABEL, "binding_kind": "personal", "request_id": id, "csrf_token": CSRF})), now).unwrap();
    assert_eq!(prepared.binding, EmployeeBinding::Personal(Text::new(f.alice.clone()).unwrap()));
    // The staging step checks the step-up again.
    assert!(stage_prepared(home, None, &f.bob, &prepared, "{}", now).is_err());
    assert!(stage_prepared(home, Some(&proof(now - 400)), &f.bob, &prepared, "{}", now).is_err());
    assert_eq!(employee_items(home, LABEL).len(), 1);
    // Settings must survive the validator's re-parse exactly.
    assert!(settings_for(&json!({"agent": {"name": LABEL}})).is_ok());
    assert_eq!(settings_for(&json!({"x": "y".repeat(MAX_PROPOSAL_VALUE_BYTES)})), Err(SETTINGS_INVALID));
}

#[test]
fn apply_binds_owner_member_count_one() {
    let f = fixture();
    let (item, receipt) = applied_personal(&f, 1, LABEL, &f.alice);
    assert_eq!(mirror(f.home(), LABEL).as_deref(), Some(f.alice.as_str()));
    assert_eq!(members(&f, LABEL), 1);
    assert_eq!(f.users.check_agent_access(&f.alice, LABEL).unwrap(), Some(AccessLevel::Owner));
    // Stage, publish and read-back each save; still exactly one member.
    for _ in 0..2 { crate::handlers::save_confirmed_receipt(f.home(), &item, &receipt).unwrap(); }
    assert_eq!(members(&f, LABEL), 1);
    assert!(f.users.bind_agent(&f.bob, LABEL, AccessLevel::Viewer).is_err());
    assert_eq!(members(&f, LABEL), 1);
    // Compensation removes the mirror and the owner binding together.
    let mut dead = receipt.clone();
    dead.disabled = true;
    crate::handlers::save_confirmed_receipt(f.home(), &item, &dead).unwrap();
    assert_eq!((mirror(f.home(), LABEL), members(&f, LABEL)), (None, 0));
    // The admin listing names the employee and its owner, never the tenant.
    let listed = admin_list(f.home()).unwrap();
    assert_eq!(listed["employees"][0]["label"], LABEL);
    assert_eq!(listed["employees"][0]["binding"], json!({"type": "personal", "owner_user_id": f.alice}));
    assert_eq!(listed["employees"][0]["employee_id"], hex::encode([1u8; 16]));
    let tenant = f.users.isolation_authority_snapshot(None, LABEL).unwrap().tenant_id;
    assert!(!listed.to_string().contains(&tenant));
}

#[tokio::test]
async fn tombstone_requires_admin_step_up() {
    let f = fixture();
    // An id with hex letters, so case matters.
    let (_, receipt) = applied_personal(&f, 0xab, LABEL, &f.alice);
    let employee = hex::encode(receipt.employee.0);
    let input = |employee: &str, csrf: [u8; 32]| -> TombstoneRequest {
        serde_json::from_value(json!({"label": LABEL, "employee_id": employee, "csrf_token": csrf})).unwrap()
    };
    let handler = MethodHandler::new(f.home().to_path_buf()).await;
    let params = json!({"label": LABEL, "employee_id": employee, "csrf_token": CSRF});
    let staff = handler.handle_isolation_employee(TOMBSTONE, params.clone(), &caller(&f.alice, UserRole::Employee)).await;
    assert_eq!(error_of(&staff), ADMIN_REQUIRED);
    let mut forged = params.clone();
    forged["force"] = json!(true);
    assert!(serde_json::from_value::<TombstoneRequest>(forged).is_err());

    let now = 10_000;
    assert!(tombstone(f.home(), None, &f.bob, input(&employee, CSRF), now).is_err());
    assert!(tombstone(f.home(), Some(&proof(now - 301)), &f.bob, input(&employee, CSRF), now).is_err());
    assert!(tombstone(f.home(), Some(&proof(now)), &f.bob, input(&employee, [8; 32]), now).is_err());
    assert_eq!((mirror(f.home(), LABEL).as_deref(), members(&f, LABEL)), (Some(f.alice.as_str()), 1));
    // Exact, lowercase ids only; another employee identity is stale.
    assert_eq!(tombstone(f.home(), Some(&proof(now)), &f.bob, input(&employee.to_uppercase(), CSRF), now).err(),
        Some(EMPLOYEE_NOT_FOUND));
    assert_eq!(tombstone(f.home(), Some(&proof(now)), &f.bob, input(&hex::encode([9u8; 16]), CSRF), now).err(), Some(STALE));
    assert_eq!(members(&f, LABEL), 1);

    let done = tombstone(f.home(), Some(&proof(now)), &f.bob, input(&employee, CSRF), now).unwrap();
    assert_eq!(done["tombstoned"], true);
    assert_eq!((mirror(f.home(), LABEL), members(&f, LABEL)), (None, 0));
    assert_eq!(admin_list(f.home()).unwrap()["employees"][0]["disabled"], true);
    // Idempotent for the same identity; a Shared or unknown label is refused.
    assert!(tombstone(f.home(), Some(&proof(now)), &f.bob, input(&employee, CSRF), now).is_ok());
    let unknown: TombstoneRequest = serde_json::from_value(json!({"label": "nobody", "employee_id": employee, "csrf_token": CSRF})).unwrap();
    assert_eq!(tombstone(f.home(), Some(&proof(now)), &f.bob, unknown, now).err(), Some(NOT_PERSONAL));
}

#[test]
fn offboarding_personal_owner_with_transfer_refused_clearly() {
    let f = fixture();
    let (_, receipt) = applied_personal(&f, 1, LABEL, &f.alice);
    f.users.bind_agent(&f.alice, "team", AccessLevel::Operator).unwrap();

    // The transfer would hand the Personal employee to someone else: a coded,
    // plain-language refusal naming the employee, not a raw trigger error.
    let error = f.users.offboard_user(&f.alice, Some(&f.bob), Some(&f.bob)).unwrap_err();
    assert_eq!(error, OrgError::PersonalEmployee(LABEL.into()));
    match super::super::users_org::org_error_frame(&error) {
        WsFrame::Response { ok: false, error: Some(err), .. } => {
            assert_eq!(err["code"], "personal_employee_handover_required");
            assert_eq!(err["agent"], LABEL);
            let message = err["message"].as_str().unwrap();
            assert!(message.contains("個人專屬") && message.contains("管理員"), "{message}");
            assert!(!message.contains("trigger") && !message.contains("ABORT"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(f.users.get_user(&f.alice).unwrap().unwrap().status, UserStatus::Active);
    assert_eq!(f.users.check_agent_access(&f.bob, "team").unwrap(), None);

    // Binding a new member to the Personal employee is a coded refusal too.
    let admin = f.users.get_user(&f.bob).unwrap().unwrap();
    let member = NewMember { display_name: "New".into(), email: "new@example.test".into(), role: UserRole::Employee,
        department: None, primary_agent_id: LABEL.into(), invite: false,
        bindings: vec![MemberBinding { agent_name: LABEL.into(), access_level: AccessLevel::Operator }] };
    assert_eq!(f.users.create_member_at_epoch(&member, &admin.id, admin.credential_epoch).err(), Some("member_agent_personal"));

    // After the admin tombstone the transfer proceeds without the Personal employee.
    let now = 10_000;
    let input: TombstoneRequest = serde_json::from_value(json!({"label": LABEL,
        "employee_id": hex::encode(receipt.employee.0), "csrf_token": CSRF})).unwrap();
    tombstone(f.home(), Some(&proof(now)), &f.bob, input, now).unwrap();
    let outcome = f.users.offboard_user(&f.alice, Some(&f.bob), Some(&f.bob)).unwrap();
    assert_eq!(outcome.transferred_agents, vec!["team".to_string()]);
    assert_eq!(f.users.check_agent_access(&f.bob, LABEL).unwrap(), None);
}

#[test]
fn direct_create_refusal_points_to_the_proposal_path() {
    assert!(DIRECT_CREATE_REFUSED.contains("員工配置提案") && DIRECT_CREATE_REFUSED.contains("核准"));
}
