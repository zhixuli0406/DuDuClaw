//! R8r-2 schema-5 wire tests: confinement to schema 5, payload bounds and
//! redaction, the peer rule, the reply shape.
use super::*;
use crate::allocation::{Action, ErrorCode as Legacy, Response as LegacyResponse, SCHEMA_V2, SCHEMA_V4};
use duduclaw_isolation::{ids::Sha256Digest, migration::*};
use sha2::{Digest, Sha256};

const SECRET: &[u8] = br#"{"providers":[{"name":"main","api_key":"sk-test-SECRET-0123456789","base_url":"https://llm.example/v1"}]}"#;

fn digest_of(category: ConfirmationCategory, fields: &[ProposalField]) -> Sha256Digest {
    let mut hash = Sha256::new(); hash.update(b"duduclaw-migration-proposal-v1\0");
    hash.update(serde_json::to_vec(&(category, fields)).unwrap());
    Sha256Digest(hash.finalize().into())
}
fn employee(status: ConfirmationStatus) -> ConfirmationItem {
    let fields = vec![ProposalField { name: "disk_bytes".into(), value: ProposalValue::Text { value: "67108864".into() } },
        ProposalField { name: "employee_id".into(), value: ProposalValue::Text { value: "sales".into() } }];
    ConfirmationItem { binding: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
        proposal_digest: digest_of(ConfirmationCategory::Employee, &fields) }, category: ConfirmationCategory::Employee, status, high_risk: true, fields }
}
fn payload(bytes: &[u8]) -> Payload { Payload::new(Zeroizing::new(bytes.to_vec())).unwrap() }
fn by_label(label: &str) -> ProviderRequest {
    ProviderRequest { schema: SCHEMA_V5, nonce: RequestId([2; 16]), action: ProviderAction::ProvisionProviders, item: None,
        employee_id: None, label: Some(label.into()), payload: Some(payload(SECRET)), clear: false }
}
fn base64_of(bytes: &[u8]) -> String { encode(bytes).as_str().to_owned() }

#[test]
fn schema5_round_trips_the_payload_and_never_prints_it() {
    let request = by_label("sales");
    let frame = serde_json::to_vec(&request).unwrap();
    let text = String::from_utf8(frame.clone()).unwrap();
    assert!(text.contains(&base64_of(SECRET)) && !text.contains("sk-test-SECRET"), "base64 on the wire: {text}");
    let Frame::Providers(decoded) = decode_frame(&frame).unwrap() else { panic!("a schema-5 frame is not a schema 1-4 request") };
    assert_eq!(decoded.payload.as_ref().unwrap().bytes().unwrap(), SECRET);
    assert_eq!(decoded.check().unwrap(), Identification::Label("sales".into()));
    for shown in [format!("{request:?}"), format!("{decoded:?}"), format!("{:?}", decoded.payload)] {
        assert!(!shown.contains("SECRET") && !shown.contains(&base64_of(SECRET)) && !shown.contains("providers"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }
    let reply = ProviderResponse::answer(request.nonce, Ok(Provisioned { provisioned: true, updated_unix: 7 }));
    let wire = serde_json::to_string(&reply).unwrap();
    assert_eq!(wire, r#"{"schema":5,"nonce":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"provisioned":true,"updated_unix":7}"#,
        "never the payload, never a hash");
}

#[test]
fn older_schemas_cannot_carry_the_action_or_its_fields() {
    for schema in [1u16, 2, 3, 4, 6] {
        let mut request = by_label("sales"); request.schema = schema;
        let frame = serde_json::to_vec(&request).unwrap();
        assert!(serde_json::from_slice::<Request>(&frame).is_err(), "schema {schema}: not a schema 1-4 request");
        let Frame::Providers(decoded) = decode_frame(&frame).unwrap() else { panic!() };
        assert!(decoded.envelope().is_err(), "schema {schema}: dropped unanswered");
        assert_eq!(decoded.check().unwrap_err(), ProviderErrorCode::InvalidRequest);
    }
    // A schema 1-4 request never carries a payload or `clear`.
    let readback = Request { schema: SCHEMA_V2, nonce: RequestId([2; 16]), action: Action::ReadBack,
        item: employee(ConfirmationStatus::Applied), approver: None, entitlement: None };
    let mut value = serde_json::to_value(&readback).unwrap();
    assert!(matches!(decode_frame(&serde_json::to_vec(&value).unwrap()).unwrap(), Frame::Allocation(_)));
    value["payload"] = serde_json::json!(base64_of(b"x"));
    assert!(decode_frame(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value = serde_json::to_value(&readback).unwrap(); value["clear"] = serde_json::json!(true);
    assert!(decode_frame(&serde_json::to_vec(&value).unwrap()).is_err());
    // … nor the action name.
    let mut value = serde_json::to_value(&readback).unwrap(); value["action"] = serde_json::json!("provision_providers");
    assert!(serde_json::from_slice::<Request>(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn schema5_codes_are_confined_to_schema5() {
    for (code, name) in [(ProviderErrorCode::NotAllocated, "not_allocated"), (ProviderErrorCode::NotReady, "not_ready"),
        (ProviderErrorCode::InvalidPayload, "invalid_payload"), (ProviderErrorCode::HomeEntryUnsafe, "home_entry_unsafe"),
        (ProviderErrorCode::StorageUnavailable, "storage_unavailable"), (ProviderErrorCode::InvalidRequest, "invalid_request"),
        (ProviderErrorCode::IsolationUnavailable, "isolation_unavailable")] {
        assert_eq!(serde_json::to_value(code).unwrap(), name);
    }
    for name in ["invalid_payload", "home_entry_unsafe", "storage_unavailable"] {
        assert!(serde_json::from_value::<Legacy>(serde_json::json!(name)).is_err(), "{name} is not a schema 1-4 code");
        let reply = format!(r#"{{"schema":2,"nonce":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"receipt":null,"error":"{name}"}}"#);
        assert!(serde_json::from_str::<LegacyResponse>(&reply).is_err());
    }
    // A schema-5 reply is checked against schema 5 and the request's nonce.
    let nonce = RequestId([2; 16]);
    let mut reply = ProviderResponse::answer(nonce, Err(ProviderErrorCode::HomeEntryUnsafe)); reply.schema = SCHEMA_V4;
    assert!(check_provider_response(nonce, false, reply).is_err());
    assert!(check_provider_response(RequestId([3; 16]), false, ProviderResponse::answer(nonce, Err(ProviderErrorCode::NotReady))).is_err());
    assert_eq!(check_provider_response(nonce, false, ProviderResponse::answer(nonce, Err(ProviderErrorCode::NotReady))).unwrap(),
        Err(ProviderErrorCode::NotReady));
}

#[test]
fn schema5_reply_must_match_what_was_asked() {
    let nonce = RequestId([2; 16]);
    let set = Provisioned { provisioned: true, updated_unix: 9 };
    let cleared = Provisioned { provisioned: false, updated_unix: 9 };
    assert_eq!(check_provider_response(nonce, false, ProviderResponse::answer(nonce, Ok(set))).unwrap(), Ok(set));
    assert_eq!(check_provider_response(nonce, true, ProviderResponse::answer(nonce, Ok(cleared))).unwrap(), Ok(cleared));
    assert!(check_provider_response(nonce, true, ProviderResponse::answer(nonce, Ok(set))).is_err());
    assert!(check_provider_response(nonce, false, ProviderResponse::answer(nonce, Ok(cleared))).is_err());
    let mixed = ProviderResponse { schema: SCHEMA_V5, nonce, error: Some(ProviderErrorCode::NotReady), provisioned: Some(true), updated_unix: Some(1) };
    assert!(check_provider_response(nonce, false, mixed).is_err());
    let partial = ProviderResponse { schema: SCHEMA_V5, nonce, error: None, provisioned: Some(true), updated_unix: None };
    assert!(check_provider_response(nonce, false, partial).is_err());
    assert!(serde_json::from_str::<ProviderResponse>(r#"{"schema":5,"nonce":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"provisioned":true,"updated_unix":1,"hash":"00"}"#).is_err());
}

#[test]
fn payload_is_bounded_strict_base64() {
    assert!(Payload::new(Zeroizing::new(vec![])).is_err());
    assert!(Payload::new(Zeroizing::new(vec![b'a'; MAX_PAYLOAD + 1])).is_err());
    for size in [1usize, 2, 3, 4, 5, MAX_PAYLOAD] {
        let bytes: Vec<u8> = (0..size).map(|i| (i * 7 + 3) as u8).collect();
        let mut request = by_label("sales"); request.payload = Some(payload(&bytes));
        let Frame::Providers(decoded) = decode_frame(&serde_json::to_vec(&request).unwrap()).unwrap() else { panic!() };
        assert_eq!(decoded.payload.as_ref().unwrap().bytes().unwrap(), &bytes[..], "size {size}");
    }
    let frame_with = |text: &str| format!(r#"{{"schema":5,"nonce":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"action":"provision_providers","label":"sales","payload":"{text}"}}"#);
    let oversized = "A".repeat(MAX_TEXT + 4);
    for bad in ["", "abc", "ab=c", "====", "QR==", "QUJ=", "QQ==QQ==", "Q Q=", "\\u0041AAA", "QUJD\\/A==", oversized.as_str()] {
        let Frame::Providers(decoded) = decode_frame(frame_with(bad).as_bytes()).unwrap() else { panic!("{bad}") };
        assert!(decoded.payload.as_ref().unwrap().bytes().is_none(), "{bad}");
        assert_eq!(decoded.check().unwrap_err(), ProviderErrorCode::InvalidPayload, "{bad}");
        assert!(!format!("{decoded:?}").contains(bad) || bad.is_empty(), "never echoed");
    }
    assert_eq!(decode(b"QUJD").unwrap().as_slice(), b"ABC");
    assert_eq!(decode(b"QQ==").unwrap().as_slice(), b"A");
}

#[test]
fn schema5_identification_and_change_are_exclusive() {
    let ok = by_label("sales");
    assert!(ok.check().is_ok());
    let mut both = by_label("sales"); both.clear = true;
    assert_eq!(both.check().unwrap_err(), ProviderErrorCode::InvalidRequest, "payload and clear");
    let mut neither = by_label("sales"); neither.payload = None;
    assert_eq!(neither.check().unwrap_err(), ProviderErrorCode::InvalidRequest, "neither payload nor clear");
    let mut clear = by_label("sales"); clear.payload = None; clear.clear = true;
    assert!(clear.check().is_ok());
    let long = "a".repeat(65);
    for label in ["", "sa les", "../x", long.as_str()] {
        assert_eq!(by_label(label).check().unwrap_err(), ProviderErrorCode::InvalidRequest, "label {label:?}");
    }
    let id = "0123456789abcdef0123456789abcdef";
    let mut by_id = by_label("sales"); by_id.label = None; by_id.employee_id = Some(id.into());
    assert_eq!(by_id.check().unwrap(), Identification::Employee(parse_employee_id(id).unwrap()));
    for bad in ["0123456789ABCDEF0123456789ABCDEF", "00000000000000000000000000000000", "0123", "0123456789abcdef0123456789abcdeg"] {
        let mut request = by_label("sales"); request.label = None; request.employee_id = Some(bad.into());
        assert_eq!(request.check().unwrap_err(), ProviderErrorCode::InvalidRequest, "{bad}");
    }
    let mut two = by_label("sales"); two.employee_id = Some(id.into());
    assert_eq!(two.check().unwrap_err(), ProviderErrorCode::InvalidRequest, "two identifications");
    let mut gateway = by_label("sales"); gateway.label = None; gateway.item = Some(employee(ConfirmationStatus::Applied));
    assert!(matches!(gateway.check().unwrap(), Identification::Binding(spec) if spec.label == "sales"));
    gateway.item = Some(employee(ConfirmationStatus::Pending));
    assert_eq!(gateway.check().unwrap_err(), ProviderErrorCode::InvalidRequest, "unconfirmed item");
}

#[test]
fn peer_rule_root_only_for_provision_providers() {
    let gateway = (990, 990);
    assert_eq!(classify(gateway, gateway), Some(Peer::Gateway));
    assert_eq!(classify((0, 0), gateway), Some(Peer::Root));
    for other in [(0, 990), (990, 0), (0, 1), (1, 0), (1000, 1000), (990, 991)] {
        assert_eq!(classify(other, gateway), None, "{other:?}");
    }
    assert_eq!(classify((0, 0), (0, 990)), None, "no peer without a configured gateway");
    for action in [Action::Allocate, Action::ReadBack, Action::Disable, Action::Retire, Action::Entitle] {
        let request = Request { schema: 1, nonce: RequestId([2; 16]), action, item: employee(ConfirmationStatus::Applying),
            approver: None, entitlement: None };
        let frame = Frame::Allocation(request);
        assert!(admits(Peer::Gateway, &frame), "{action:?}");
        assert!(!admits(Peer::Root, &frame), "root never sends {action:?}");
    }
    let id = "0123456789abcdef0123456789abcdef";
    let mut by_id = by_label("sales"); by_id.label = None; by_id.employee_id = Some(id.into());
    let mut by_item = by_label("sales"); by_item.label = None; by_item.item = Some(employee(ConfirmationStatus::Applied));
    let mut both = by_label("sales"); both.employee_id = Some(id.into());
    let mut none = by_label("sales"); none.label = None;
    let frames = [(by_label("sales"), true, false), (by_id, true, false), (by_item, false, true), (both, false, false), (none, false, false)];
    for (request, root, gateway_ok) in frames {
        let shown = format!("{request:?}");
        let frame = Frame::Providers(Box::new(request));
        assert_eq!(admits(Peer::Root, &frame), root, "{shown}");
        assert_eq!(admits(Peer::Gateway, &frame), gateway_ok, "{shown}");
    }
}
