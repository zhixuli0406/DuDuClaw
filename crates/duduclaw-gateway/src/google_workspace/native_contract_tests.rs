use super::*;

fn valid_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("gmail_search", json!({"query":"from:alice is:unread"})),
        ("gmail_read", json!({"message_id":"abc123"})),
        (
            "gmail_create_draft",
            json!({"to":" a@b.test ","subject":"主旨","body":"body"}),
        ),
        ("gmail_send_draft", json!({"draft_id":"r-123"})),
        ("calendar_delete_event", json!({"event_id":"e","calendar_id":"team@example.com"})),
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
        ("docs_create", json!({"title":"Doc"})),
        ("docs_replace_text", json!({"document_id":"d","find":"old","replace":"new"})),
        ("sheets_create", json!({"title":"Sheet"})),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[["x"]]})),
        ("calendar_update_event", json!({"event_id":"e","summary":"Updated"})),
        ("google_chat_spaces", json!({})),
        ("google_chat_messages", json!({"space":"spaces/AAA"})),
    ]
}

#[test]
fn all_twenty_seven_native_inputs_are_frozen_without_status_or_remote_aliases() {
    let cases = valid_cases();
    assert_eq!(cases.len(), 27);
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
        matches!(draft.parsed(), ParsedNativeArguments::GmailDraft { to, subject, .. } if to == " a@b.test " && subject == "主旨")
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

#[test]
fn google_write_inputs_validate_ids_types_limits_and_unicode() {
    for (tool, args) in [
        ("docs_create", json!({"title":" "})),
        ("docs_create", json!({"title":"Doc","body":123})),
        ("gmail_create_draft", json!({"to":"a@b.test","subject":"s","body":"b","thread_id":true})),
        ("calendar_update_event", json!({"event_id":"e","calendar_id":"","summary":"s"})),
        ("docs_replace_text", json!({"document_id":"../x","find":"old","replace":"new"})),
        ("docs_replace_text", json!({"document_id":"d","find":"","replace":"new"})),
        ("docs_replace_text", json!({"document_id":"d","find":"old","replace":"new","match_case":"false"})),
        ("sheets_create", json!({"title":"s","sheet_names":[123]})),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[1]})),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[[{}]]})),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[[1]],"value_input_option":"INVALID"})),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[vec!["x";10001]]})),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"..","values":[[1]]})),
        ("calendar_update_event", json!({"event_id":"e"})),
        ("calendar_update_event", json!({"event_id":"x/y","summary":"s"})),
        ("calendar_update_event", json!({"event_id":"e","summary":"s","notify_attendees":"true"})),
        ("calendar_update_event", json!({"event_id":"e","start":"tomorrow"})),
        ("gmail_create_draft", json!({"to":"a@b.test","subject":"s","body":"b","thread_id":"t"})),
        ("gmail_create_draft", json!({"to":"a@b.test","subject":"s","body":"b","thread_id":"t","in_reply_to":"<id>\r\nBcc: other@example.test"})),
    ] {
        assert!(NativeCallTemplate::prepare(tool, &args).is_err(), "{tool}: {args}");
    }
    assert!(NativeCallTemplate::prepare("docs_create", &json!({"title":"文件🦀".repeat(1000),"body":"漢🦀".repeat(11000)})).is_err());
    let template = NativeCallTemplate::prepare("sheets_update", &json!({"spreadsheet_id":"s","range":"A1","values":[vec!["x";10000]]})).unwrap();
    assert!(template.validate_closed().is_ok());
}

#[test]
fn every_bounded_text_field_rejects_overflow_and_preserves_the_exact_limit() {
    let cases = [
        ("docs_create", json!({"title":"Doc"}), "title", 1000),
        ("docs_create", json!({"title":"Doc"}), "body", 20_000),
        ("docs_replace_text", json!({"document_id":"d","find":"x","replace":"y"}), "find", 20_000),
        ("docs_replace_text", json!({"document_id":"d","find":"x","replace":"y"}), "replace", 20_000),
        ("sheets_create", json!({"title":"Sheet"}), "title", 1000),
        ("sheets_create", json!({"title":"Sheet"}), "sheet_names", 100),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[[1]]}), "range", 1000),
        ("sheets_update", json!({"spreadsheet_id":"s","range":"A1","values":[[1]]}), "values", 20_000),
        ("calendar_update_event", json!({"event_id":"e"}), "summary", 20_000),
        ("calendar_update_event", json!({"event_id":"e"}), "description", 20_000),
        ("calendar_update_event", json!({"event_id":"e"}), "location", 20_000),
        ("calendar_update_event", json!({"event_id":"e"}), "attendees", 320),
        ("gmail_create_draft", json!({"to":"a@example.test","subject":"s","body":"b"}), "to", 1000),
        ("gmail_create_draft", json!({"to":"a@example.test","subject":"s","body":"b"}), "cc", 1000),
        ("gmail_create_draft", json!({"to":"a@example.test","subject":"s","body":"b"}), "subject", 1000),
        ("gmail_create_draft", json!({"to":"a@example.test","subject":"s","body":"b"}), "body", 20_000),
    ];
    for (tool, base, field, max) in cases {
        for over in [false, true] {
            let size = max + usize::from(over);
            let value = if matches!(field, "to" | "cc") {
                // Five individually valid mailboxes, together exactly 1000/1001 chars.
                let mut addresses = vec![format!("{}@b.test", "a".repeat(192)); 5];
                addresses[0] = format!("{}@b.test", "a".repeat(193 + usize::from(over)));
                addresses.join(",")
            } else if field == "attendees" {
                format!("{}@b.test", "a".repeat(size - 7))
            } else { "🦀".repeat(size) };
            assert_eq!(value.chars().count(), size);
            let mut args = base.clone();
            args[field] = match field {
                "values" => json!([[value]]),
                "sheet_names" | "attendees" => json!([value]),
                _ => json!(value),
            };
            if over {
                let error = NativeCallTemplate::prepare(tool, &args).err().expect("over-limit input refused");
                assert!(error.contains(field) && error.contains(&max.to_string()), "{tool}/{field}: {error}");
                assert!(super::validate_inputs(tool, &args).is_err());
                continue;
            }
            let template = NativeCallTemplate::prepare(tool, &args).unwrap();
            let actual: &str = match template.parsed() {
                ParsedNativeArguments::DocsCreate { title, body } => if field == "title" { title } else { body.as_ref().unwrap() },
                ParsedNativeArguments::DocsReplace { find, replace, .. } => if field == "find" { find } else { replace },
                ParsedNativeArguments::SheetsCreate { title, sheet_names } => if field == "title" { title } else { &sheet_names.as_ref().unwrap()[0] },
                ParsedNativeArguments::SheetsUpdate { range, values, .. } => if field == "range" { range.as_str() } else { values[0][0].as_str().unwrap() },
                ParsedNativeArguments::CalendarUpdate { patch, .. } => if field == "attendees" { patch["attendees"][0]["email"].as_str().unwrap() } else { patch[field].as_str().unwrap() },
                ParsedNativeArguments::GmailDraft { to, cc, subject, body, .. } => match field {
                    "to" => to, "cc" => cc.as_ref().unwrap(), "subject" => subject, _ => body,
                },
                _ => panic!("unexpected tool {tool}"),
            };
            assert_eq!(actual, value, "{tool}/{field} must remain unchanged");
            assert_eq!(template.digest(), duduclaw_core::google_identity::google_arguments_digest(&args));
        }
    }
}

#[test]
fn gmail_recipient_headers_refuse_controls_and_validate_each_mailbox() {
    for field in ["to", "cc"] {
        for value in ["a@example.com\r\nBcc: x@example.com", "a@example.com\r", "a@example.com\n",
            "a@exam\0ple.com", "a@example.com\t", "a@example.com\u{7f}", "a@example.com\u{85}",
            "a@example.com,invalid", "invalid"] {
            let mut args = json!({"to":"a@example.com","subject":"s","body":"b"});
            args[field] = json!(value);
            assert!(NativeCallTemplate::prepare("gmail_create_draft", &args).is_err(), "{field}: {value:?}");
        }
    }
    let args = json!({"to":"Alice@example.com, bob@example.com","cc":"Carol@example.com","subject":"s","body":"b"});
    let template = NativeCallTemplate::prepare("gmail_create_draft", &args).unwrap();
    assert!(matches!(template.parsed(), ParsedNativeArguments::GmailDraft { to, cc: Some(cc), .. }
        if to == "Alice@example.com, bob@example.com" && cc == "Carol@example.com"));
}

#[test]
fn confirmed_tools_validate_ids_booleans_and_native_only_backend() {
    for (tool, args) in [
        ("gmail_send_draft", json!({"draft_id":"../send"})),
        ("gmail_send_draft", json!({"draft_id":"a%2Fb"})),
        ("calendar_delete_event", json!({"event_id":"e","calendar_id":"x/y"})),
        ("calendar_delete_event", json!({"event_id":"e","notify_attendees":"false"})),
        ("calendar_delete_event", json!({"event_id":"e","notify_attendees":null})),
    ] { assert!(NativeCallTemplate::prepare(tool, &args).is_err(), "{tool}: {args}"); }
    for tool in ["gmail_send_draft", "calendar_delete_event"] {
        assert!(!super::super::backend_supports_tool(false, tool, false));
        assert!(super::super::backend_supports_tool(true, tool, false));
    }
}
