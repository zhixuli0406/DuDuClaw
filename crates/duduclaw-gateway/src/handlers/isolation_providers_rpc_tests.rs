//! R8r-3a: `isolation.provider_*` tests. The socket call and the employee
//! resolution are injected ([`ProviderRoot`]), the step-up proof source too
//! ([`AdminSession`]), so nothing here needs spawnd or an allocated employee.
use super::*;
use duduclaw_isolation::bounded::Text;
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationStatus, ProposalBinding, ProposalField,
    ProposalOperationId, ProposalValue};
use duduclaw_migration::ids::Sha256Digest;
use std::sync::Mutex;

const ADMIN: &str = "0b6f2f8e-8c1d-4a51-9b40-2f1d4f6c9a10";
const BOB: &str = "5a0c1e2d-3b4f-4a6e-8d7c-9b1a2c3d4e5f";
const TENANT: &str = "6f1c2f4e-3b7a-4d0e-9a8b-1c2d3e4f5a6b";
const SESSION: &str = "4c3a1f8e-0d2b-4e6a-9c1d-7b5e3f2a1c0d";
const CSRF: [u8; 32] = [7; 32];
const NOW: i64 = 1_800_000_000;
const UPDATED: u64 = 1_800_000_100;
const KEY: &str = "sk-CANARY-r8r3a-0123456789";
const PATH_MARKER: &str = "PATHMARKER";
const EMPLOYEE: EmployeeId = EmployeeId([3; 16]);

fn employee_hex() -> String { hex::encode(EMPLOYEE.0) }
fn shared() -> EmployeeBinding { EmployeeBinding::Shared(Text::new(TENANT.into()).unwrap()) }
fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(Text::new(owner.into()).unwrap()) }

fn applied_item() -> ConfirmationItem {
    ConfirmationItem { binding: ProposalBinding { operation_id: ProposalOperationId {
            proposal_id: duduclaw_migration::ids::RequestId([5; 16]), revision: 1 }, proposal_digest: Sha256Digest([4; 32]) },
        category: ConfirmationCategory::Employee, status: ConfirmationStatus::Applied, high_risk: true,
        fields: vec![ProposalField { name: "label".into(), value: ProposalValue::Text { value: "sales-bot".into() } }] }
}

/// users.db stand-in: a proof exists only for its own session (as
/// `migration_confirmation_session`, which keys it by the session hash and
/// credential epoch); `current: false` = no longer an Admin at that epoch.
struct FakeAdmin { session: &'static str, proof: Option<(u64, [u8; 32])>, current: bool, reveals: Mutex<Vec<String>> }
impl FakeAdmin {
    fn fresh() -> Self {
        Self { session: SESSION, proof: Some((NOW as u64 - 60, CSRF)), current: true, reveals: Mutex::new(Vec::new()) }
    }
    fn with_proof(proof: Option<(u64, [u8; 32])>) -> Self { Self { proof, ..Self::fresh() } }
}
impl AdminSession for FakeAdmin {
    fn proof(&self, user: &str, session: &str, _: u64, _: i64) -> Result<Option<MigrationConfirmationSession>, ()> {
        if !self.current || user != ADMIN { return Err(()); }
        Ok(self.proof.filter(|_| session == self.session)
            .map(|(verified_at, csrf)| MigrationConfirmationSession { verified_at, csrf }))
    }
    fn mfa_enrolled(&self, _: &str) -> Result<bool, ()> { Ok(true) }
    fn audit_reveal(&self, _: &str, detail: &str) -> Result<(), ()> {
        self.reveals.lock().unwrap().push(detail.to_owned());
        Ok(())
    }
}

/// spawnd stand-in: resolves [`EMPLOYEE`] only, keeps every request it got.
struct FakeRoot {
    binding: EmployeeBinding,
    resolve: Result<(), Refusal>,
    answer: Mutex<Delivery>,
    /// Answer `Done` exactly as configured (otherwise it matches the request).
    echo: bool,
    seen: Mutex<Vec<ProviderRequest>>,
}
impl FakeRoot {
    fn new(binding: EmployeeBinding) -> Self {
        Self { binding, resolve: Ok(()), answer: Mutex::new(done()), echo: false, seen: Mutex::new(Vec::new()) }
    }
    fn answering(binding: EmployeeBinding, answer: Delivery) -> Self {
        let root = Self::new(binding);
        *root.answer.lock().unwrap() = answer;
        root
    }
    fn answer(&self, answer: Delivery) { *self.answer.lock().unwrap() = answer; }
    fn sent(&self) -> usize { self.seen.lock().unwrap().len() }
}
impl ProviderRoot for FakeRoot {
    fn resolve(&self, _: &Path, employee: EmployeeId) -> Result<Target, Refusal> {
        self.resolve?;
        if employee != EMPLOYEE { return Err(NOT_ALLOCATED); }
        Ok(Target { employee, incarnation: 4, label: "sales-bot".into(), binding: self.binding.clone(), item: applied_item() })
    }
    fn provide(&self, request: ProviderRequest) -> Delivery {
        let clear = request.clear;
        self.seen.lock().unwrap().push(request);
        match *self.answer.lock().unwrap() {
            Delivery::Done(done) if !self.echo => Delivery::Done(Provisioned { provisioned: !clear, updated_unix: done.updated_unix }),
            other => other,
        }
    }
}
fn done() -> Delivery { Delivery::Done(Provisioned { provisioned: true, updated_unix: UPDATED }) }

fn caller() -> Caller<'static> { Caller { user: ADMIN, session: SESSION, epoch: 1 } }
fn entry(name: &str, key: &str, url: &str) -> Value { json!({ "name": name, "api_key": key, "base_url": url }) }
fn set_params(providers: Vec<Value>) -> Value {
    json!({ "employee_id": employee_hex(), "providers": providers, "step_up_csrf": CSRF.to_vec() })
}
fn one() -> Value { set_params(vec![entry("main", KEY, "https://api.example.test/v1")]) }
fn clear_params() -> Value { json!({ "employee_id": employee_hex(), "step_up_csrf": CSRF.to_vec() }) }
fn call(method: &str, mut params: Value) -> ProviderCall { ProviderCall::take_from(method, &mut params) }
fn run(home: &Path, admin: &FakeAdmin, root: &FakeRoot, method: &str, params: Value) -> Result<Value, Refusal> {
    serve(home, admin, &caller(), call(method, params), root, NOW)
}
fn audit_text(home: &Path) -> String { std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default() }
fn audit_entries(home: &Path) -> Vec<Value> {
    audit_text(home).lines().filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["event_type"].as_str().is_some_and(|event| event.starts_with("isolation.provider_"))).collect()
}
fn contains(haystack: &[u8], needle: &str) -> bool { haystack.windows(needle.len()).any(|window| window == needle.as_bytes()) }
fn principal(label: &str, id: u8, binding: EmployeeBinding) -> ConfirmedPrincipal {
    ConfirmedPrincipal { label: label.into(), employee: EmployeeId([id; 16]), binding: Some(binding), disabled: false }
}

#[test]
fn r8r3a_namespace_and_routing_tables() {
    for method in METHODS {
        assert!(owns(method), "{method}");
        assert!(is_enterprise_only_method(method), "{method}: Linux team edition only, like the connector RPCs");
        assert_eq!(super::super::dashboard_scope::minimum_role(method), Some(UserRole::Admin), "{method}");
        assert!(super::super::personal_p1g_gate::metadata_allowlisted(method), "{method}: reviewed lifecycle entry");
    }
    for method in ["isolation.provider_purge", "isolation.provider_status.extra"] { assert!(owns(method), "{method}"); }
    for method in ["isolation.connector_list", "isolation", "provider_status", "evil.isolation.provider_set", "isolation.providers"] {
        assert!(!owns(method), "{method}");
    }
    // An unknown member of the namespace is refused (never routed elsewhere).
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let refused = run(home.path(), &FakeAdmin::fresh(), &root, "isolation.provider_purge", one()).unwrap_err();
    assert_eq!(refused, NOT_ALLOWED);
    assert_eq!(root.sent(), 0);
    // Off isolation: the connector RPCs' fixed answer, word for word.
    assert_eq!(availability(false), Err(NOT_AVAILABLE));
    assert!(availability(true).is_ok());
    assert_eq!(NOT_AVAILABLE.message, "這個環境未啟用受保護模式，沒有可管理的隔離連線帳號。");
}

/// Non-admins, the shared bootstrap identity, unknown members and a host
/// without isolation never get an answer, and never see the key back.
#[tokio::test]
async fn r8r3a_rpc_refuses_non_admins_and_unknown_members() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let mut employee = UserContext::admin_fallback();
    employee.user_id = ADMIN.into();
    employee.role = UserRole::Employee;
    let bootstrap = UserContext::admin_fallback();
    for caller in [&employee, &bootstrap] {
        for (method, params) in [(STATUS, json!({})), (SET, one()), (CLEAR, clear_params()),
            ("isolation.provider_purge", json!({ "api_key": KEY }))] {
            let frame = handler.handle(method, params, caller).await;
            assert!(!matches!(frame, WsFrame::Response { ok: true, .. }), "{method}");
            assert!(!serde_json::to_string(&frame).unwrap().contains(KEY), "{method}");
        }
    }
    assert!(!home.path().join(records::RECORDS_FILE).exists());
    assert!(!audit_text(home.path()).contains(KEY));
}

/// Exactly the connector PROVISION gate: current Admin, fresh step-up of
/// this session (5 minutes), its CSRF token. Every attempt audited once.
#[cfg(unix)]
#[test]
fn r8r3a_gate_matrix() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let code = |result: Result<Value, Refusal>| result.err().map(|refusal| refusal.code);
    // No longer a current Admin at this credential epoch.
    let demoted = FakeAdmin { current: false, ..FakeAdmin::fresh() };
    assert_eq!(code(run(home.path(), &demoted, &root, SET, one())), Some("session_expired"));
    // Admin without a step-up.
    assert_eq!(code(run(home.path(), &FakeAdmin::with_proof(None), &root, SET, one())), Some("step_up_required"));
    // A stale step-up (older than five minutes) and one from the future.
    for at in [NOW as u64 - 301, NOW as u64 + 1] {
        assert_eq!(code(run(home.path(), &FakeAdmin::with_proof(Some((at, CSRF))), &root, SET, one())), Some("step_up_required"));
    }
    // The wrong CSRF token.
    let mut wrong = one();
    wrong["step_up_csrf"] = json!([8u8; 32].to_vec());
    assert_eq!(code(run(home.path(), &FakeAdmin::fresh(), &root, SET, wrong)), Some("step_up_required"));
    // Another dashboard session of the same Admin has no proof.
    let other_session = Caller { user: ADMIN, session: "9d8c7b6a-5f4e-4d3c-8b2a-1f0e9d8c7b6a", epoch: 1 };
    assert_eq!(code(serve(home.path(), &FakeAdmin::fresh(), &other_session, call(SET, one()), &root, NOW)), Some("step_up_required"));
    // A request cannot claim Admin, MFA or step-up state.
    for forged in ["step_up_valid", "admin", "mfa_enrolled", "item", "label"] {
        let mut params = one();
        params[forged] = json!(true);
        assert_eq!(code(run(home.path(), &FakeAdmin::fresh(), &root, SET, params)), Some("request_invalid"), "{forged}");
    }
    // Clear has the same gate.
    assert_eq!(code(run(home.path(), &FakeAdmin::with_proof(None), &root, CLEAR, clear_params())), Some("step_up_required"));
    assert_eq!(root.sent(), 0, "nothing reaches root before the gate passes");
    // OK.
    assert!(run(home.path(), &FakeAdmin::fresh(), &root, SET, one()).is_ok());
    assert_eq!(root.sent(), 1);
    let results: Vec<String> = audit_entries(home.path()).iter().map(|row| row["details"]["result"].as_str().unwrap().to_owned()).collect();
    assert_eq!(results, ["session_expired", "step_up_required", "step_up_required", "step_up_required", "step_up_required",
        "step_up_required", "request_invalid", "request_invalid", "request_invalid", "request_invalid", "request_invalid",
        "step_up_required", "ok"]);
    // Status needs no step-up, but its reveal does.
    let status = run(home.path(), &FakeAdmin::with_proof(None), &root, STATUS, json!({})).unwrap();
    assert_eq!((status["step_up_valid"].as_bool(), status["csrf_token"].is_null()), (Some(false), true));
    assert_eq!(code(run(home.path(), &FakeAdmin::with_proof(None), &root, STATUS, json!({ "step_up_csrf": CSRF.to_vec() }))),
        Some("step_up_required"));
    let status = run(home.path(), &FakeAdmin::fresh(), &root, STATUS, json!(null)).unwrap();
    assert_eq!(status["csrf_token"], json!(CSRF.to_vec()));
    assert_eq!(status["visibility"], "dashboard_changes_only");
}

/// The fake root receives exactly the document the worker reads; the key is
/// in that payload and nowhere else.
#[cfg(unix)]
#[test]
fn r8r3a_set_hands_root_the_worker_document() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let first_url = format!("https://api.example.test:8443/v1/{PATH_MARKER}");
    let params = set_params(vec![entry("main", KEY, &first_url),
        json!({ "name": "主要備援", "api_key": "sk-second-CANARY", "base_url": "https://claude.example.test", "protocol": "anthropic" })]);
    let value = run(home.path(), &FakeAdmin::fresh(), &root, SET, params).unwrap();
    assert_eq!(value, json!({ "provisioned": true, "updated_unix": UPDATED, "result": "recorded" }));

    let seen = root.seen.lock().unwrap();
    let request = &seen[0];
    assert_eq!((request.schema, request.action, request.clear), (wire::SCHEMA_V5, ProviderAction::ProvisionProviders, false));
    assert!(request.employee_id.is_none() && request.label.is_none(), "the gateway peer names the employee by its item only");
    assert!(request.item.as_ref().is_some_and(|item| item.binding == applied_item().binding && item.status == ConfirmationStatus::Applied));
    assert_ne!(request.nonce.0, [0; 16]);
    let bytes = request.payload.as_ref().and_then(Payload::bytes).unwrap();
    assert!(bytes.len() <= MAX_DOCUMENT);
    let parsed: duduclaw_api_worker::Credentials = serde_json::from_slice(bytes).unwrap();
    assert_eq!(parsed.providers.len(), 2);
    let (a, b) = (&parsed.providers[0], &parsed.providers[1]);
    assert_eq!((a.name.as_str(), a.api_key.as_str(), a.base_url.as_str(), a.protocol),
        ("main", KEY, first_url.as_str(), Protocol::ChatCompletions));
    assert_eq!((b.name.as_str(), b.api_key.as_str(), b.base_url.as_str(), b.protocol),
        ("主要備援", "sk-second-CANARY", "https://claude.example.test", Protocol::Anthropic));
    // The worker's own selection accepts it.
    let selected = serde_json::from_slice::<duduclaw_api_worker::Credentials>(bytes).unwrap().select(Some("main"));
    assert!(selected.is_ok_and(|credential| credential.api_key == KEY));

    // Nowhere else: the reply, the audit log, the record store.
    for text in [value.to_string(), audit_text(home.path())] {
        assert!(!text.contains(KEY) && !text.contains("sk-second-CANARY") && !text.contains(PATH_MARKER), "{text}");
    }
    let store = std::fs::read(home.path().join(records::RECORDS_FILE)).unwrap();
    assert!(!contains(&store, KEY) && !contains(&store, "sk-second-CANARY") && !contains(&store, PATH_MARKER));
}

/// `take_from` moves every key out and overwrites what is left, on every
/// path (valid, refused shapes, other members of the namespace).
#[test]
fn r8r3a_take_wipes_the_params() {
    let mut params = one();
    let taken = ProviderCall::take_from(SET, &mut params);
    assert!(taken.input.is_ok());
    assert_eq!(taken.employee, Some(EMPLOYEE));
    let left = params.to_string();
    assert!(!left.contains(KEY) && !left.contains("api_key") && !left.contains("https://"), "{left}");

    let csrf = CSRF.to_vec();
    for mut bad in [
        json!({ "employee_id": employee_hex(), "providers": [{ "name": "a", "api_key": KEY, "base_url": "https://h", "extra": KEY }],
            "step_up_csrf": csrf }),
        set_params(vec![entry("a", KEY, "https://h"); 33]),
        json!({ "employee_id": employee_hex(), "providers": [{ "name": "a", "api_key": [KEY], "base_url": "https://h" }],
            "step_up_csrf": csrf }),
        json!({ "employee_id": employee_hex(), "providers": KEY, "step_up_csrf": csrf }),
        json!({ "employee_id": employee_hex(), "providers": [entry("a", KEY, "https://h")], "step_up_csrf": csrf, "note": KEY }),
        json!({ "employee_id": KEY, "providers": [entry("a", KEY, "https://h")], "step_up_csrf": KEY }),
        json!([KEY]),
    ] {
        let taken = ProviderCall::take_from(SET, &mut bad);
        assert!(taken.input.is_err());
        assert!(taken.employee.is_none() || taken.employee == Some(EMPLOYEE));
        assert!(!bad.to_string().contains(KEY), "{bad}");
    }
    for method in [STATUS, CLEAR, "isolation.provider_purge"] {
        let mut params = json!({ "api_key": KEY, "providers": [{ "api_key": KEY }] });
        let taken = ProviderCall::take_from(method, &mut params);
        assert!(taken.input.is_err(), "{method}");
        assert!(!params.to_string().contains(KEY), "{method}");
    }
}

/// Every root answer maps to one fixed RPC code; nothing is recorded.
#[cfg(unix)]
#[test]
fn r8r3a_spawnd_codes_map_to_fixed_rpc_codes() {
    use ProviderErrorCode as Code;
    let table = [
        (Delivery::Refused(Code::NotAllocated), "not_allocated"),
        (Delivery::Refused(Code::NotReady), "busy"),
        (Delivery::Refused(Code::HomeEntryUnsafe), "home_entry_unsafe"),
        (Delivery::Refused(Code::StorageUnavailable), "storage_unavailable"),
        (Delivery::Refused(Code::IsolationUnavailable), "unconfirmed"),
        (Delivery::Refused(Code::InvalidPayload), "invalid_provider"),
        (Delivery::Refused(Code::InvalidRequest), "not_allocated"),
        (Delivery::NotSent, "unreachable"),
        (Delivery::Unconfirmed, "unconfirmed"),
    ];
    for (answer, expected) in table {
        for (method, params) in [(SET, one()), (CLEAR, clear_params())] {
            let home = crate::fs_safe::private_test_home().unwrap();
            let root = FakeRoot::answering(shared(), answer);
            let refusal = run(home.path(), &FakeAdmin::fresh(), &root, method, params).unwrap_err();
            assert_eq!(refusal.code, expected, "{answer:?} {method}");
            assert!(!serde_json::to_string(&refusal.frame()).unwrap().contains(KEY));
            assert_eq!(root.sent(), 1);
            assert!(!home.path().join(records::RECORDS_FILE).exists(), "{answer:?} {method}: nothing recorded");
            assert_eq!(audit_entries(home.path()).last().unwrap()["details"]["result"], expected);
        }
    }
    // A "success" that does not match the request is not a confirmation.
    let home = crate::fs_safe::private_test_home().unwrap();
    let liar = FakeRoot { echo: true, ..FakeRoot::answering(shared(), Delivery::Done(Provisioned { provisioned: false, updated_unix: 1 })) };
    assert_eq!(run(home.path(), &FakeAdmin::fresh(), &liar, SET, one()).unwrap_err(), UNCONFIRMED);
    assert!(!home.path().join(records::RECORDS_FILE).exists());
    // Resolution refusals: nothing is sent.
    let unreachable = FakeRoot { resolve: Err(UNREACHABLE), ..FakeRoot::new(shared()) };
    assert_eq!(run(home.path(), &FakeAdmin::fresh(), &unreachable, SET, one()).unwrap_err(), UNREACHABLE);
    let mut unknown = one();
    unknown["employee_id"] = json!(hex::encode([9u8; 16]));
    let root = FakeRoot::new(shared());
    assert_eq!(run(home.path(), &FakeAdmin::fresh(), &root, SET, unknown).unwrap_err(), NOT_ALLOCATED);
    let mut malformed = one();
    malformed["employee_id"] = json!("sales-bot");
    assert_eq!(run(home.path(), &FakeAdmin::fresh(), &root, SET, malformed).unwrap_err(), REQUEST_INVALID);
    assert_eq!((unreachable.sent(), root.sent()), (0, 0));
}

/// The record is written only on a confirmed set, removed only on a
/// confirmed clear, untouched by anything unconfirmed or refused.
#[cfg(unix)]
#[test]
fn r8r3a_record_written_only_on_confirmed_success() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let admin = FakeAdmin::fresh();
    run(home.path(), &admin, &root, SET, one()).unwrap();
    let recorded = records::read_all(home.path()).unwrap();
    let expected = Record { incarnation: 4, updated_unix: UPDATED, providers: vec![ProviderSummary { name: "main".into(),
        protocol: "chat_completions".into(), origin: "https://api.example.test".into() }] };
    assert_eq!(recorded.get(&EMPLOYEE), Some(&expected));

    let other = set_params(vec![entry("other", KEY, "https://other.example.test/v1")]);
    for answer in [Delivery::Unconfirmed, Delivery::Refused(ProviderErrorCode::IsolationUnavailable),
        Delivery::Refused(ProviderErrorCode::NotReady), Delivery::NotSent] {
        root.answer(answer);
        assert!(run(home.path(), &admin, &root, SET, other.clone()).is_err());
        assert!(run(home.path(), &admin, &root, CLEAR, clear_params()).is_err());
        assert_eq!(records::read_all(home.path()).unwrap().get(&EMPLOYEE), Some(&expected), "{answer:?}");
    }
    root.answer(done());
    let cleared = run(home.path(), &admin, &root, CLEAR, clear_params()).unwrap();
    assert_eq!(cleared, json!({ "provisioned": false, "updated_unix": UPDATED, "result": "recorded" }));
    assert!(root.seen.lock().unwrap().last().is_some_and(|request| request.clear && request.payload.is_none()));
    assert!(records::read_all(home.path()).unwrap().is_empty());
}

/// Status shows names, protocols and origins only; another person's
/// Personal employee is hidden unless revealed; a record counts only for
/// the incarnation it was made for.
#[cfg(unix)]
#[test]
fn r8r3a_status_never_shows_key_path_or_query() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let url = format!("https://api.example.test:8443/v1/{PATH_MARKER}/");
    run(home.path(), &FakeAdmin::fresh(), &root, SET, set_params(vec![entry("main", KEY, &url)])).unwrap();
    let recorded = records::read_all(home.path()).unwrap();
    let rows = vec![principal("alice-helper", 1, personal(ADMIN)), principal("bob-helper", 2, personal(BOB)),
        principal("sales-bot", 3, shared())];
    let current = |_: &ConfirmedPrincipal| -> Result<Option<u32>, ()> { Ok(Some(4)) };
    let (value, revealed) = status_value(&rows, &recorded, None, None, ADMIN, false, &current).unwrap();
    let text = value.to_string();
    for forbidden in [KEY, PATH_MARKER, "/v1", "api_key", "base_url", "bob-helper", BOB] {
        assert!(!text.contains(forbidden), "{forbidden}: {text}");
    }
    assert_eq!((revealed, value["hidden_personal"].as_u64(), value["next"].is_null()), (0, Some(1), true));
    let employees = value["employees"].as_array().unwrap();
    let row = |label: &str| employees.iter().find(|row| row["label"] == label).unwrap().clone();
    let sales = row("sales-bot");
    assert_eq!(sales, json!({ "employee_id": employee_hex(), "label": "sales-bot", "kind": "shared", "provisioned": true,
        "source": "dashboard", "updated_unix": UPDATED,
        "providers": [{ "name": "main", "protocol": "chat_completions", "origin": "https://api.example.test:8443" }] }));
    let alice = row("alice-helper");
    let mut keys: Vec<_> = alice.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["employee_id", "kind", "label", "provisioned", "providers"], "no record: no source / time");
    assert_eq!((alice["provisioned"].as_bool(), alice["kind"].as_str()), (Some(false), Some("personal")));

    // Break-glass reveal: the row is shown and counted for the audit.
    let (value, revealed) = status_value(&rows, &recorded, None, None, ADMIN, true, &current).unwrap();
    assert_eq!((revealed, value["hidden_personal"].as_u64()), (1, Some(0)));
    assert!(value.to_string().contains("bob-helper"));
    // Another incarnation (a same-id restore): the record does not count.
    let restored = |_: &ConfirmedPrincipal| -> Result<Option<u32>, ()> { Ok(Some(5)) };
    let (value, _) = status_value(&rows, &recorded, Some(EMPLOYEE), None, ADMIN, false, &restored).unwrap();
    assert_eq!(value["employees"][0]["provisioned"], false);
    let unreadable = |_: &ConfirmedPrincipal| -> Result<Option<u32>, ()> { Err(()) };
    assert!(status_value(&rows, &recorded, None, None, ADMIN, false, &unreadable).is_err(),
        "an unreadable incarnation refuses (fail closed)");
    // One employee; an unknown one is not allocated.
    let (value, _) = status_value(&rows, &recorded, Some(EMPLOYEE), None, ADMIN, false, &current).unwrap();
    assert_eq!(value["employees"].as_array().map(Vec::len), Some(1));
    assert_eq!(status_value(&rows, &recorded, Some(EmployeeId([8; 16])), None, ADMIN, false, &current).unwrap_err(), NOT_ALLOCATED);

    // Paging by label.
    let many: Vec<ConfirmedPrincipal> = (0..=PAGE).map(|index| {
        let mut id = [9u8; 16];
        id[0] = index as u8;
        id[1] = (index >> 8) as u8;
        ConfirmedPrincipal { label: format!("e{index:04}"), employee: EmployeeId(id), binding: Some(shared()), disabled: false }
    }).collect();
    let (page, _) = status_value(&many, &BTreeMap::new(), None, None, ADMIN, false, &current).unwrap();
    assert_eq!(page["employees"].as_array().map(Vec::len), Some(PAGE));
    let next = page["next"].as_str().unwrap().to_owned();
    assert_eq!(next, format!("e{:04}", PAGE - 1));
    let (rest, _) = status_value(&many, &BTreeMap::new(), None, Some(next.as_str()), ADMIN, false, &current).unwrap();
    assert_eq!((rest["employees"].as_array().map(Vec::len), rest["next"].is_null()), (Some(1), true));
}

/// One audit entry per attempt: actor, employee id / label, action, result,
/// provider names and origins only. Another person's Personal employee may
/// be set (the connector PROVISION rule) and is marked in the entry.
#[cfg(unix)]
#[test]
fn r8r3a_audit_entry_contents() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(personal(BOB));
    let url = format!("https://api.example.test/v1/{PATH_MARKER}");
    run(home.path(), &FakeAdmin::fresh(), &root, SET,
        set_params(vec![json!({ "name": "main", "api_key": KEY, "base_url": url, "protocol": "gemini" })])).unwrap();
    run(home.path(), &FakeAdmin::fresh(), &root, CLEAR, clear_params()).unwrap();
    let entries = audit_entries(home.path());
    assert_eq!(entries.len(), 2);
    let set = &entries[0];
    assert_eq!((set["event_type"].as_str(), set["agent_id"].as_str()), (Some("isolation.provider_set"), Some(AUDIT_AGENT)));
    assert!(set["timestamp"].as_str().is_some_and(|time| !time.is_empty()));
    assert_eq!(set["details"], json!({ "actor_user_id": ADMIN, "action": "set", "employee_id": employee_hex(),
        "label": "sales-bot", "kind": "personal", "foreign_personal": true, "result": "ok",
        "providers": [{ "name": "main", "origin": "https://api.example.test" }], "recorded": true }));
    let clear = &entries[1];
    assert_eq!(clear["event_type"], "isolation.provider_clear");
    assert_eq!((clear["details"]["action"].as_str(), clear["details"]["result"].as_str(), clear["details"]["providers"].clone()),
        (Some("clear"), Some("ok"), json!([])));
    let text = audit_text(home.path());
    for forbidden in [KEY, PATH_MARKER, "api_key", "base_url", "/v1", "gemini"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
}

/// Invalid providers are refused with one fixed code that contains nothing
/// of the input; nothing is sent, recorded or audited from them.
#[cfg(unix)]
#[test]
fn r8r3a_validation_never_echoes_input() {
    const ECHO: &str = "ECHOMARKER";
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let bad: Vec<Value> = vec![
        entry("main", KEY, &format!("http://{ECHO}.example.test/v1")),
        entry("main", KEY, &format!("https://api.example.test/v1?token={ECHO}")),
        entry("main", KEY, &format!("https://api.example.test/v1#{ECHO}")),
        entry("main", KEY, &format!("https://{ECHO}:pw@api.example.test/v1")),
        entry("main", &format!("{KEY}\n{ECHO}"), "https://api.example.test/v1"),
        entry(&format!("{ECHO}\"quoted"), KEY, "https://api.example.test/v1"),
        entry("main", KEY, &format!("https://api.example.test/{ECHO}\\v1")),
        entry("main", &format!("{ECHO}\\{KEY}"), "https://api.example.test/v1"),
        entry("main", KEY, &format!("https://api.example.test/v1\t{ECHO}")),
        entry("main", "", "https://api.example.test/v1"),
        entry(&"E".repeat(129), KEY, "https://api.example.test/v1"),
        json!({ "name": "main", "api_key": KEY, "base_url": "https://api.example.test/v1", "protocol": ECHO }),
        json!({ "name": "main", "api_key": KEY, "base_url": "https://api.example.test/v1", "model": ECHO }),
        json!({ "name": ECHO, "base_url": "https://api.example.test/v1" }),
        json!({ "name": ECHO, "api_key": 42, "base_url": "https://api.example.test/v1" }),
    ];
    for provider in bad {
        let refusal = run(home.path(), &FakeAdmin::fresh(), &root, SET, set_params(vec![provider.clone()])).unwrap_err();
        assert_eq!(refusal, INVALID_PROVIDER, "{provider}");
        let frame = serde_json::to_string(&refusal.frame()).unwrap();
        assert!(!frame.contains(ECHO) && !frame.contains(KEY), "{frame}");
    }
    assert_eq!(run(home.path(), &FakeAdmin::fresh(), &root, SET, set_params(vec![])).unwrap_err(), INVALID_PROVIDER);
    assert_eq!(root.sent(), 0);
    assert!(!home.path().join(records::RECORDS_FILE).exists());
    let text = audit_text(home.path());
    assert!(!text.contains(ECHO) && !text.contains(KEY));
    assert!(audit_entries(home.path()).iter().all(|row| row["details"]["result"] == "invalid_provider"
        && row["details"]["providers"] == json!([])));
}

/// 1..=32 uniquely named providers, at most 65,536 bytes of document.
#[cfg(unix)]
#[test]
fn r8r3a_document_cap_duplicates_and_count() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let root = FakeRoot::new(shared());
    let admin = FakeAdmin::fresh();
    let url = "https://api.example.test/v1";
    // Duplicate names and 33 providers.
    assert_eq!(run(home.path(), &admin, &root, SET, set_params(vec![entry("main", KEY, url), entry("main", "sk-2", url)])).unwrap_err(),
        INVALID_PROVIDER);
    let many = |count: usize| (0..count).map(|index| entry(&format!("p{index}"), KEY, url)).collect::<Vec<_>>();
    assert_eq!(run(home.path(), &admin, &root, SET, set_params(many(33))).unwrap_err(), INVALID_PROVIDER);
    assert_eq!(root.sent(), 0);
    run(home.path(), &admin, &root, SET, set_params(many(32))).unwrap();
    // Over the cap end to end (each key within the worker's 16,384 limit).
    let big = "k".repeat(14_000);
    let sized = |count: usize| (0..count).map(|index| entry(&format!("p{index}"), &big, url)).collect::<Vec<_>>();
    assert_eq!(run(home.path(), &admin, &root, SET, set_params(sized(5))).unwrap_err(), INVALID_PROVIDER);
    assert_eq!(root.sent(), 1);
    run(home.path(), &admin, &root, SET, set_params(sized(4))).unwrap();
    {
        let seen = root.seen.lock().unwrap();
        let bytes = seen.last().unwrap().payload.as_ref().and_then(Payload::bytes).unwrap();
        assert!(bytes.len() <= MAX_DOCUMENT);
        assert_eq!(serde_json::from_slice::<duduclaw_api_worker::Credentials>(bytes).unwrap().providers.len(), 4);
    }
    // The exact boundary of the hand-written document.
    let one_entry = |key_len: usize| vec![Entry { name: Zeroizing::new("main".into()), api_key: Zeroizing::new("k".repeat(key_len)),
        base_url: Zeroizing::new(url.into()), protocol: Protocol::default() }];
    let overhead = document(&one_entry(1)).unwrap().len() - 1;
    let exact = document(&one_entry(MAX_DOCUMENT - overhead)).unwrap();
    assert_eq!(exact.len(), MAX_DOCUMENT);
    assert_eq!(exact.capacity(), MAX_DOCUMENT, "allocated once at its final size");
    assert!(serde_json::from_slice::<Value>(&exact).is_ok());
    assert_eq!(document(&one_entry(MAX_DOCUMENT - overhead + 1)).unwrap_err(), INVALID_PROVIDER);
}
