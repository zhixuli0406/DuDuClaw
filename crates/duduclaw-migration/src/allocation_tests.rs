use super::*;
fn digest_of(category: ConfirmationCategory, fields: &[ProposalField]) -> Sha256Digest {
    let mut hash = Sha256::new(); hash.update(b"duduclaw-migration-proposal-v1\0");
    hash.update(serde_json::to_vec(&(category, fields)).unwrap());
    Sha256Digest(hash.finalize().into())
}
fn employee(status: ConfirmationStatus) -> ConfirmationItem {
    let fields=vec![ProposalField{name:"disk_bytes".into(),value:ProposalValue::Text{value:"67108864".into()}},
        ProposalField{name:"employee_id".into(),value:ProposalValue::Text{value:"sales".into()}}];
    ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{proposal_id:RequestId([1;16]),revision:1},proposal_digest:digest_of(ConfirmationCategory::Employee,&fields)},
        category:ConfirmationCategory::Employee,status,high_risk:true,fields}
}
fn request(action: Action, item: ConfirmationItem) -> Request {
    Request { schema: 1, nonce: RequestId([2;16]), action, item, approver: None }
}
#[test]
fn allocation_refuses_preconfirmation_and_changed_full_payload() {
    let mut request=request(Action::Allocate,employee(ConfirmationStatus::Pending));
    assert_eq!(request.validate().unwrap_err(),Error::Authentication);
    request.item.status=ConfirmationStatus::Approved;
    assert_eq!(request.validate().unwrap_err(),Error::Authentication);
    request.item.status=ConfirmationStatus::Applying;
    assert_eq!(request.validate().unwrap().label,"sales");
    request.item.fields.push(ProposalField{name:"settings".into(),value:ProposalValue::Text{value:"tampered".into()}});
    assert!(request.validate().is_err());
}
#[test]
fn allocation_disable_requires_terminal_failure_or_rejection() {
    let mut request=request(Action::Disable,employee(ConfirmationStatus::Applying));
    assert!(request.validate().is_err());
    request.item.status=ConfirmationStatus::Failed;assert!(request.validate().is_ok());
    request.item.status=ConfirmationStatus::Rejecting;assert!(request.validate().is_ok());
    // An approver belongs to a retire only.
    request.approver=Some("admin-uuid".into());assert!(request.validate().is_err());
}
fn bound(extra: &[(&str, &str)]) -> ConfirmationItem {
    let mut fields: Vec<ProposalField> = [("disk_bytes", "67108864"), ("employee_id", "sales")].iter().chain(extra)
        .map(|(n, v)| ProposalField { name: (*n).into(), value: ProposalValue::Text { value: (*v).into() } }).collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    ConfirmationItem { binding: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1;16]), revision: 1 }, proposal_digest: digest_of(ConfirmationCategory::Employee, &fields) },
        category: ConfirmationCategory::Employee, status: ConfirmationStatus::Applying, high_risk: true, fields }
}
fn text(value: &str) -> Text<128> { Text::new(value.into()).unwrap() }
#[test]
fn allocation_specification_parses_personal_and_shared_binding() {
    let personal = specification(&bound(&[("binding_kind", "personal"), ("owner_id", "alice-uuid")])).unwrap();
    assert_eq!(personal.binding, Some(EmployeeBinding::Personal(text("alice-uuid"))));
    let shared = specification(&bound(&[("binding_kind", "shared"), ("tenant_id", "tenant-a")])).unwrap();
    assert_eq!(shared.binding, Some(EmployeeBinding::Shared(text("tenant-a"))));
    assert_eq!(specification(&bound(&[])).unwrap().binding, None, "legacy proposal stays unbound");
}
#[test]
fn allocation_specification_rejects_invalid_binding_fields() {
    let long = "a".repeat(129);
    for extra in [
        vec![("binding_kind", "Personal"), ("owner_id", "alice")],
        vec![("binding_kind", "team"), ("owner_id", "alice")],
        vec![("binding_kind", ""), ("owner_id", "alice")],
        vec![("binding_kind", "personal")],
        vec![("binding_kind", "personal"), ("owner_id", "")],
        vec![("binding_kind", "personal"), ("owner_id", " alice")],
        vec![("binding_kind", "personal"), ("owner_id", "al\nice")],
        vec![("binding_kind", "personal"), ("owner_id", long.as_str())],
        vec![("binding_kind", "personal"), ("owner_id", "alice"), ("tenant_id", "tenant-a")],
        vec![("binding_kind", "shared")],
        vec![("binding_kind", "shared"), ("owner_id", "alice")],
        vec![("binding_kind", "shared"), ("owner_id", "alice"), ("tenant_id", "tenant-a")],
        vec![("binding_kind", "shared"), ("tenant_id", "")],
        vec![("owner_id", "alice")],
        vec![("tenant_id", "tenant-a")],
    ] {
        assert_eq!(specification(&bound(&extra)).unwrap_err(), Error::Invalid, "{extra:?}");
    }
    let mut secret = bound(&[("binding_kind", "personal")]);
    secret.fields.push(ProposalField { name: "owner_id".into(), value: ProposalValue::SecretFingerprint { keyed_fingerprint: Sha256Digest([1;32]), needs_rotation: false } });
    assert!(specification(&secret).is_err());
    // The binding is digest-covered: swapping the owner without re-digesting is stale.
    let mut swapped = bound(&[("binding_kind", "personal"), ("owner_id", "alice")]);
    for f in &mut swapped.fields { if f.name == "owner_id" { f.value = ProposalValue::Text { value: "bob".into() }; } }
    assert!(specification(&swapped).is_err());
}
#[test]
fn allocation_receipt_legacy_json_reads_unbound_and_serializes_unchanged() {
    let legacy = Receipt { binding: bound(&[]).binding, label: "sales".into(), employee: EmployeeId([3;16]), uid: 70001, bytes: 67108864, inodes: 4096, disabled: false, principal_binding: None };
    let encoded = serde_json::to_string(&legacy).unwrap();
    assert!(!encoded.contains("principal_binding"));
    assert_eq!(serde_json::from_str::<Receipt>(&encoded).unwrap().principal_binding, None);
    let mut personal = legacy.clone(); personal.principal_binding = Some(EmployeeBinding::Personal(text("alice")));
    let encoded = serde_json::to_string(&personal).unwrap();
    assert!(encoded.contains(r#""principal_binding":{"kind":"personal","id":"alice"}"#), "{encoded}");
    assert_eq!(serde_json::from_str::<Receipt>(&encoded).unwrap().principal_binding, personal.principal_binding);
    assert!(serde_json::from_str::<Receipt>(&encoded.replace(r#""kind":"personal""#, r#""kind":"owner""#)).is_err());
    assert!(validate_principal_binding(&EmployeeBinding::Personal(text(""))).is_err());
    assert!(validate_principal_binding(&EmployeeBinding::Shared(text("tenant-a"))).is_ok());
}
#[test]
fn allocation_operation_binds_revision_and_compensation() {
    let binding = ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1;16]), revision:1 }, proposal_digest: Sha256Digest([2;32]) };
    assert_ne!(operation(binding,OperationKind::Allocate),operation(binding,OperationKind::Compensate));
    let mut next=binding; next.operation_id.revision+=1;
    assert_ne!(operation(binding,OperationKind::Allocate),operation(next,OperationKind::Allocate));
}

// ---- F6l: retire item, operation kinds, schema-2 response ----

pub(crate) fn retire_spec() -> RetireSpec {
    RetireSpec { label: "sales".into(), target: employee(ConfirmationStatus::Applied).binding, employee: EmployeeId([7;16]),
        uid: 70001, early: false, quarantine_digest_prefix: "0123456789abcdef".into(), requested_by: "admin-a".into() }
}
pub(crate) fn retire_item(spec: &RetireSpec, status: ConfirmationStatus) -> ConfirmationItem {
    let fields = retire_fields(spec);
    ConfirmationItem { binding: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([9;16]), revision: 1 },
        proposal_digest: digest_of(ConfirmationCategory::EmployeeRetire, &fields) },
        category: ConfirmationCategory::EmployeeRetire, status, high_risk: true, fields }
}
fn retire_request(item: ConfirmationItem) -> Request {
    Request { schema: SCHEMA_V2, nonce: RequestId([2;16]), action: Action::Retire, item, approver: Some("admin-b".into()) }
}
#[test]
fn allocation_retire_requires_applying_retire_item() {
    let spec = retire_spec();
    assert_eq!(retire_request(retire_item(&spec, ConfirmationStatus::Applying)).validate_retire().unwrap(), spec);
    for status in [ConfirmationStatus::Pending, ConfirmationStatus::Approved, ConfirmationStatus::Applied,
        ConfirmationStatus::Failed, ConfirmationStatus::Rejecting, ConfirmationStatus::Rejected] {
        assert_eq!(retire_request(retire_item(&spec, status)).validate_retire().unwrap_err(), Error::Authentication, "{status:?}");
    }
    // An Employee item can never be retired; a retire item can never be allocated, read back or disabled.
    assert!(retire_request(employee(ConfirmationStatus::Applying)).validate_retire().is_err());
    for action in [Action::Allocate, Action::ReadBack, Action::Disable] {
        let mut other = retire_request(retire_item(&spec, ConfirmationStatus::Applying));
        other.action = action; other.approver = None;
        assert!(other.validate().is_err() && other.validate_retire().is_err() && other.check().is_err(), "{action:?}");
    }
    // `Retire` is never valid through the Employee validation path (no entry creation).
    assert!(retire_request(retire_item(&spec, ConfirmationStatus::Applying)).validate().is_err());
    // Schema 2 only, approver required and well formed.
    let mut v1 = retire_request(retire_item(&spec, ConfirmationStatus::Applying)); v1.schema = 1;
    assert!(v1.validate_retire().is_err());
    for approver in [None, Some(String::new()), Some("ad min".into()), Some("a".repeat(129))] {
        let mut request = retire_request(retire_item(&spec, ConfirmationStatus::Applying)); request.approver = approver;
        assert!(request.validate_retire().is_err());
    }
    assert!(retire_request(retire_item(&spec, ConfirmationStatus::Applying)).check().is_ok());
}
#[test]
fn allocation_retire_fields_digest_bound() {
    let spec = retire_spec();
    let item = retire_item(&spec, ConfirmationStatus::Applying);
    assert_eq!(retire_specification(&item).unwrap(), spec);
    // Changing any one field without re-digesting is refused.
    for name in RETIRE_FIELDS {
        let mut changed = item.clone();
        for f in &mut changed.fields {
            if f.name == *name { if let ProposalValue::Text { value } = &mut f.value { value.push('0'); } }
        }
        assert!(retire_specification(&changed).is_err(), "{name}");
    }
    // Re-digested but invalid values are refused too.
    let (zero, upper_hex) = ("0".repeat(32), "A".repeat(32));
    let cases: Vec<(&str, &str)> = vec![("reason", "early"), ("storage", "archive"), ("early", "yes"), ("uid", "69999"),
        ("uid", "100000"), ("uid", "070001"), ("target_revision", "0"), ("target_revision", "+1"), ("employee_hex", zero.as_str()),
        ("employee_hex", upper_hex.as_str()), ("quarantine_digest_prefix", "0123"), ("quarantine_digest_prefix", "ABCDEF0123456789"),
        ("requested_by", "a b"), ("employee_id", "")];
    for (name, value) in cases {
        let mut fields = retire_fields(&spec);
        for f in &mut fields { if f.name == name { f.value = ProposalValue::Text { value: value.into() }; } }
        let mut bad = item.clone(); bad.binding.proposal_digest = digest_of(ConfirmationCategory::EmployeeRetire, &fields); bad.fields = fields;
        assert!(retire_specification(&bad).is_err(), "{name}={value}");
    }
    // A missing field (re-digested) is refused: every field is required.
    let mut fields = retire_fields(&spec); fields.retain(|f| f.name != "early");
    let mut short = item.clone(); short.binding.proposal_digest = digest_of(ConfirmationCategory::EmployeeRetire, &fields); short.fields = fields;
    assert!(retire_specification(&short).is_err());
    // The early flag round-trips.
    let mut early = spec.clone(); early.early = true;
    assert!(retire_specification(&retire_item(&early, ConfirmationStatus::Applying)).unwrap().early);
}
#[test]
fn allocation_operation_kinds_distinct_and_stable() {
    let binding = ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1;16]), revision: 3 }, proposal_digest: Sha256Digest([2;32]) };
    // The v1.80 formula with its `disable: bool` byte, recomputed independently.
    let legacy = |disable: bool| {
        let mut hash = Sha256::new();
        hash.update(b"duduclaw-confirmed-allocation-v1\0");
        hash.update(binding.operation_id.proposal_id.0);
        hash.update(binding.operation_id.revision.to_be_bytes());
        hash.update([u8::from(disable)]);
        let digest: [u8; 32] = hash.finalize().into();
        let mut id = [0u8; 16]; for (slot, byte) in id.iter_mut().zip(digest.iter()) { *slot = *byte; } RequestId(id)
    };
    assert_eq!(operation(binding, OperationKind::Allocate), legacy(false), "existing allocation ids replay");
    assert_eq!(operation(binding, OperationKind::Compensate), legacy(true), "existing compensation ids replay");
    let kinds = [OperationKind::Allocate, OperationKind::Compensate, OperationKind::Retire];
    for (i, a) in kinds.iter().enumerate() {
        for b in kinds.iter().skip(i + 1) { assert_ne!(operation(binding, *a), operation(binding, *b)); }
    }
    assert_eq!((OperationKind::Allocate as u8, OperationKind::Compensate as u8, OperationKind::Retire as u8), (0, 1, 2));
}
#[test]
fn allocation_response_schema2_error_code() {
    let spec = retire_spec();
    let retire = retire_request(retire_item(&spec, ConfirmationStatus::Applying));
    // Schema 1 replies keep the v1.80 shape exactly.
    let mut v1 = request(Action::ReadBack, employee(ConfirmationStatus::Applied));
    let reply = serde_json::to_value(Response::for_request(&v1, Err(ErrorCode::NotFound))).unwrap();
    let keys: Vec<&str> = reply.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 3, "schema 1 = schema, nonce, receipt only: {reply}");
    assert!(reply["receipt"].is_null() && reply["schema"] == 1);
    // Every refusal code survives the wire; refusals carry nothing else.
    for code in [ErrorCode::NotFound, ErrorCode::Inconsistent, ErrorCode::NotReady, ErrorCode::InvalidRequest, ErrorCode::IsolationUnavailable] {
        let encoded = serde_json::to_vec(&Response::for_request(&retire, Err(code))).unwrap();
        let decoded: Response = serde_json::from_slice(&encoded).unwrap();
        assert!(matches!(check_response(&retire, decoded).unwrap(), Answer::Refused(c) if c == code));
    }
    assert_eq!(serde_json::to_value(ErrorCode::IsolationUnavailable).unwrap(), "isolation_unavailable");
    assert_eq!(serde_json::to_value(ErrorCode::Inconsistent).unwrap(), "inconsistent");
    // A refusal with a receipt, a wrong nonce or schema, or an unknown code is refused.
    let retired = Receipt { binding: spec.target, label: spec.label.clone(), employee: spec.employee, uid: spec.uid, bytes: 67108864,
        inodes: 4096, disabled: true, principal_binding: None };
    let mixed = Response { schema: 2, nonce: retire.nonce, receipt: Some(retired.clone()), error: Some(ErrorCode::NotFound), phase: None, idle: None };
    assert!(check_response(&retire, mixed).is_err());
    let wrong_nonce = Response { schema: 2, nonce: RequestId([3;16]), receipt: None, error: Some(ErrorCode::NotFound), phase: None, idle: None };
    assert!(check_response(&retire, wrong_nonce).is_err());
    assert!(serde_json::from_str::<Response>(r#"{"schema":2,"nonce":"x","receipt":null,"error":"gone"}"#).is_err());
    // A Retired receipt must match the target, employee, UID and label.
    let ok = Response { schema: 2, nonce: retire.nonce, receipt: Some(retired.clone()), error: None, phase: Some(Phase::Retired), idle: None };
    assert!(matches!(check_response(&retire, ok).unwrap(), Answer::Reply(r) if r.phase == Phase::Retired));
    for change in 0..4 {
        let mut other = retired.clone();
        match change { 0 => other.uid += 1, 1 => other.employee = EmployeeId([8;16]), 2 => other.label = "other".into(), _ => other.disabled = false }
        let response = Response { schema: 2, nonce: retire.nonce, receipt: Some(other), error: None, phase: Some(Phase::Retired), idle: None };
        assert!(check_response(&retire, response).is_err(), "change {change}");
    }
    let still_ready = Response { schema: 2, nonce: retire.nonce, receipt: Some(retired), error: None, phase: Some(Phase::Ready), idle: None };
    assert!(check_response(&retire, still_ready).is_err());
    // ReadBack: any phase, idle only for Ready.
    v1.schema = SCHEMA_V2;
    let spec_e = v1.validate().unwrap();
    let live = Receipt { binding: v1.item.binding, label: spec_e.label, employee: EmployeeId([7;16]), uid: 70001, bytes: spec_e.bytes,
        inodes: spec_e.inodes, disabled: false, principal_binding: None };
    let ready = Response { schema: 2, nonce: v1.nonce, receipt: Some(live.clone()), error: None, phase: Some(Phase::Ready), idle: Some(false) };
    assert!(matches!(check_response(&v1, ready).unwrap(), Answer::Reply(r) if r.idle == Some(false)));
    let retiring = Response { schema: 2, nonce: v1.nonce, receipt: Some(live.clone()), error: None, phase: Some(Phase::Retiring), idle: Some(true) };
    assert!(check_response(&v1, retiring).is_err(), "idle is reported for Ready entries only");
    let mut dead = live; dead.disabled = true;
    let gone = Response { schema: 2, nonce: v1.nonce, receipt: Some(dead), error: None, phase: Some(Phase::Retired), idle: None };
    assert!(matches!(check_response(&v1, gone).unwrap(), Answer::Reply(r) if r.phase == Phase::Retired));
}
