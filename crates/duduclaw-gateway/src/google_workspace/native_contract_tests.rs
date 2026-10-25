use super::*;

fn valid_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("gmail_search", json!({"query":"from:alice is:unread"})),
        ("gmail_read", json!({"message_id":"abc123"})),
        (
            "gmail_create_draft",
            json!({"to":" a@b.test ","subject":"主旨","body":"body"}),
        ),
        ("calendar_list_events", json!({})),
        (
            "calendar_create_event",
            json!({"summary":"s","start":"2026-08-01T00:00:00Z","end":"2026-08-01T01:00:00Z","with_meet":"true"}),
        ),
        ("sheets_read", json!({"spreadsheet_id":"s","range":"A1:C2"})),
        (
            "sheets_append",
            json!({"spreadsheet_id":"s","range":"A1","values":["a",null,3,true]}),
        ),
        ("forms_get", json!({"form_id":"f"})),
        ("forms_list_responses", json!({"form_id":"f"})),
        ("gtasks_lists", json!({})),
        (
            "gtasks_list",
            json!({"show_completed":"true","max_results":" 7 "}),
        ),
        ("gtasks_create", json!({"title":"t"})),
        ("gtasks_complete", json!({"task_id":"t"})),
        ("drive_search", json!({"query":"a'b"})),
        ("drive_read", json!({"file_id":"d"})),
        ("docs_read", json!({"document_id":"d"})),
        ("docs_append", json!({"document_id":"d","text":"x"})),
        ("slides_read", json!({"presentation_id":"p"})),
    ]
}

#[test]
fn all_eighteen_native_inputs_are_frozen_without_status_or_remote_aliases() {
    let cases = valid_cases();
    assert_eq!(cases.len(), 18);
    for (tool, arguments) in cases {
        let call = NativeCallTemplate::prepare(tool, &arguments).unwrap();
        assert_eq!(call.tool(), tool);
        assert_eq!(
            call.digest(),
            duduclaw_core::google_identity::google_arguments_digest(&arguments)
        );
    }
    for invalid in [
        "google_status",
        "google_remote_mcp",
        "mail_poll",
        "gmail_read ",
        "GMAIL_READ",
    ] {
        assert!(NativeCallTemplate::prepare(invalid, &json!({})).is_err());
    }
}

#[test]
fn frozen_raw_digest_retains_unused_fields_and_representation_changes() {
    let first =
        NativeCallTemplate::prepare("gmail_search", &json!({"query":"q","max_results":7})).unwrap();
    let string =
        NativeCallTemplate::prepare("gmail_search", &json!({"query":"q","max_results":"7"}))
            .unwrap();
    let extra = NativeCallTemplate::prepare(
        "gmail_search",
        &json!({"query":"q","max_results":7,"unused":true}),
    )
    .unwrap();
    assert_ne!(first.digest(), string.digest());
    assert_ne!(first.digest(), extra.digest());
    assert_eq!(
        NativeCallTemplate::prepare("gmail_search", &json!({"max_results":7,"query":"q"}))
            .unwrap()
            .digest(),
        first.digest()
    );
}

#[test]
fn parse_goldens_preserve_trim_number_boolean_and_cell_rules() {
    let cases = valid_cases();
    let draft = NativeCallTemplate::prepare(cases[2].0, &cases[2].1).unwrap();
    assert!(
        matches!(draft.parsed(), ParsedNativeArguments::GmailDraft { to, subject, .. } if to == "a@b.test" && subject == "主旨")
    );
    let cells = NativeCallTemplate::prepare(cases[6].0, &cases[6].1).unwrap();
    assert!(
        matches!(cells.parsed(), ParsedNativeArguments::SheetsAppend { values, .. } if values == &vec!["a".to_string(), "".to_string(), "3".to_string(), "true".to_string()])
    );
    let tasks = NativeCallTemplate::prepare(cases[10].0, &cases[10].1).unwrap();
    assert!(
        matches!(tasks.parsed(), ParsedNativeArguments::TasksList { list_id, show_completed, max } if list_id == "@default" && !show_completed && *max == 7)
    );
    assert_eq!(
        NativeCallTemplate::prepare("docs_append", &json!({"document_id":"d"}))
            .err()
            .unwrap(),
        "Missing required parameter: text"
    );
    assert_eq!(
        NativeCallTemplate::prepare(
            "sheets_append",
            &json!({"spreadsheet_id":"s","range":"A1","values":[]})
        )
        .err()
        .unwrap(),
        "Parameter 'values' must contain at least one cell"
    );
}

#[test]
fn closed_path_safety_does_not_change_legacy_parsing() {
    for id in [
        "../x",
        "x/y",
        "x?alt=media",
        "x#fragment",
        "%2e%2e",
        "x%2Fy",
        "x y",
    ] {
        let call = NativeCallTemplate::prepare("gmail_read", &json!({"message_id":id})).unwrap();
        assert!(call.validate_closed().is_err());
    }
    assert!(
        NativeCallTemplate::prepare("gmail_read", &json!({"message_id":"0123abc"}))
            .unwrap()
            .validate_closed()
            .is_ok()
    );
}

#[test]
fn calendar_create_timestamp_errors_keep_original_start_end_wording() {
    let start = NativeCallTemplate::prepare(
        "calendar_create_event",
        &json!({"summary":"s","start":"bad-start","end":"2026-08-01T01:00:00Z"}),
    )
    .err()
    .unwrap();
    assert_eq!(
        start,
        "Invalid start: 'bad-start' is not RFC-3339 (e.g. 2026-07-26T14:00:00+08:00)"
    );
    let end = NativeCallTemplate::prepare(
        "calendar_create_event",
        &json!({"summary":"s","start":"2026-08-01T00:00:00Z","end":"bad-end"}),
    )
    .err()
    .unwrap();
    assert_eq!(
        end,
        "Invalid end: 'bad-end' is not RFC-3339 (e.g. 2026-07-26T15:00:00+08:00)"
    );
    assert_eq!(
        NativeCallTemplate::prepare("calendar_list_events", &json!({"time_min":"bad-min"}))
            .err()
            .unwrap(),
        "Invalid time_min: 'bad-min' is not a valid RFC-3339 timestamp (e.g. 2026-07-26T14:00:00+08:00)"
    );
    assert_eq!(
        NativeCallTemplate::prepare("gtasks_create", &json!({"title":"t","due":"bad-due"}))
            .err()
            .unwrap(),
        "Invalid due: 'bad-due' is not a valid RFC-3339 timestamp (e.g. 2026-08-01T00:00:00Z)"
    );
}
