use super::*;

fn request(format: &str) -> Request {
    serde_json::from_value(serde_json::json!({"from":"2026-11-01T00:00:00Z",
        "until":"2026-11-02T00:00:00Z", "format":format})).unwrap()
}
fn fixture() -> Report {
    Report { outbound: vec![Row {
        time: "2026-11-01T03:00:00Z".into(), employee: "王小明".into(),
        destination: "customer.example.com".into(), categories: "EMAIL alice@example.com A123456789".into(),
        decision: "approved".into(), approver: "dashboard:陳小姐".into(),
        reason: "寄給林先生".into(), kind: "send_message".into(), requester: "王小明".into(), id: "record-1".into(),
    }], ..Report::default() }
}

#[test]
fn compliance_generator_masks_personal_data_by_default() {
    let report = fixture();
    for format in ["csv", "html"] {
        let output = String::from_utf8(render::encode(&report, &request(format)).unwrap()).unwrap();
        for value in ["王小明", "陳小姐", "林先生", "customer.example.com", "alice@example.com", "A123456789"] {
            assert!(!output.contains(value), "leaked {value} in {format}");
        }
        assert!(output.contains("person-"));
        assert!(output.contains("[MASKED]"));
        assert!(output.contains("approved"));
    }
    let xlsx = render::encode(&report, &request("xlsx")).unwrap();
    assert_eq!(xlsx.get(0..2), Some(b"PK".as_slice()));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx)).unwrap();
    let mut strings = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("xl/sharedStrings.xml").unwrap(), &mut strings).unwrap();
    for value in ["王小明", "陳小姐", "林先生", "customer.example.com", "alice@example.com", "A123456789"] {
        assert!(!strings.contains(value), "leaked {value} in xlsx");
    }
    assert!(strings.contains("[MASKED]"));
}

#[test]
fn compliance_generator_full_option_and_output_injection_boundaries() {
    let mut report = fixture();
    report.outbound[0].reason = "<script>alert(1)</script>".into();
    report.outbound[0].employee = " =HYPERLINK(\"evil\")".into();
    let mut req = request("html");
    req.include_personal_data = true;
    let html = String::from_utf8(render::encode(&report, &req).unwrap()).unwrap();
    assert!(html.contains("alice@example.com"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("default-src 'none'"));
    req.format = Format::Csv;
    let csv = String::from_utf8(render::encode(&report, &req).unwrap()).unwrap();
    assert!(csv.contains("' =HYPERLINK"));
    report.outbound[0].reason = "x".repeat(8_001);
    assert_eq!(render::encode(&report, &req).unwrap_err(), "COMPLIANCE_TOO_LARGE");
}

#[test]
fn compliance_generator_translates_columns_and_validates_range() {
    let mut req = request("csv");
    for (language, header) in [(Language::ZhTw, "核准者"), (Language::En, "Approver"), (Language::Ja, "承認者")] {
        req.language = language;
        let output = String::from_utf8(render::encode(&fixture(), &req).unwrap()).unwrap();
        assert!(output.contains(header));
    }
    req.until = req.from;
    assert!(req.validate().is_err());
    req.until = req.from + chrono::Duration::days(367);
    assert!(req.validate().is_err());
    assert!(serde_json::from_value::<Request>(serde_json::json!({"from":"2026-11-01T00:00:00Z",
        "until":"2026-11-02T00:00:00Z", "format":"csv", "include_personal_data":"true"})).is_err());
}

#[test]
fn compliance_edition_gate_and_live_admin_are_required() {
    let dir = tempfile::tempdir().unwrap();
    let admin = UserContext::admin_fallback();
    assert_eq!(authorize(dir.path(), &admin, EditionProfile::Personal).unwrap_err(), "enterprise_edition_required");
    assert!(authorize(dir.path(), &admin, EditionProfile::Enterprise).is_ok());
    let mut employee = admin.clone();
    employee.role = duduclaw_auth::models::UserRole::Employee;
    assert!(authorize(dir.path(), &employee, EditionProfile::Enterprise).is_err());
    let mut stale = admin;
    stale.user_id = "deleted-account".into();
    assert!(authorize(dir.path(), &stale, EditionProfile::Enterprise).is_err());
}

#[test]
fn compliance_full_values_still_mask_credentials() {
    let mut report = fixture();
    report.outbound[0].reason = format!("Bearer eyJ{}.{}.{}", "a".repeat(20), "b".repeat(20), "c".repeat(20));
    let mut req = request("csv");
    req.include_personal_data = true;
    let csv = String::from_utf8(render::encode(&report, &req).unwrap()).unwrap();
    assert!(!csv.contains(&report.outbound[0].reason));
    assert!(csv.contains("王小明"));
}

#[test]
fn compliance_full_export_requires_audit_success_before_release() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request("csv");
    req.include_personal_data = true;
    let admin = UserContext::admin_fallback();
    let output = generate(dir.path(), &admin, EditionProfile::Enterprise, &req).unwrap();
    assert!(!output.content_base64.is_empty());
    let events = duduclaw_security::audit::read_recent_events(dir.path(), 10);
    assert_eq!(events.len(), 1);
    let event = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(event["event_type"], "compliance_personal_data_export");
    let denied = tempfile::tempdir().unwrap();
    std::fs::create_dir(denied.path().join("security_audit.jsonl")).unwrap();
    assert_eq!(generate(denied.path(), &admin, EditionProfile::Enterprise, &req).err().as_deref(),
        Some("COMPLIANCE_AUDIT_UNAVAILABLE"));
}
