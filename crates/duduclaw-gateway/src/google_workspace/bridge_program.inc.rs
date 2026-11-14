// One recipe/curation source instantiated privately in legacy and closed owners.
use crate::google_workspace::*;
use crate::google_apps_script;
use serde_json::{Value, json};
#[async_trait::async_trait]
pub(super) trait LocalIo: Send {
    async fn call(
        &mut self,
        action: google_apps_script::BridgeAction,
        params: Value,
    ) -> Result<Value, GoogleApiError>;
}
pub(super) async fn gmail_search(
    io: &mut impl LocalIo,
    query: &str,
    max_results: u32,
) -> Result<GmailSearchResult, GoogleApiError> {
    let v = io
        .call(
            google_apps_script::BridgeAction::GmailSearch,
            json!({ "query": query, "limit": max_results }),
        )
        .await?;
    require_success(google_apps_script::BridgeAction::GmailSearch, &v, &[Needs::List("messages")])?;
    let messages: Vec<GmailMessageMeta> = v
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| GmailMessageMeta {
                    id: bs(m, "message_id"),
                    // Apps Script's GmailApp exposes threads, but the
                    // bridge returns the first message of each; there is
                    // no separate thread id to report.
                    thread_id: String::new(),
                    from: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(m, "from")).text,
                    subject: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(m, "subject")).text,
                    date: bs(m, "date"),
                    snippet: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(m, "snippet")).text,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(GmailSearchResult {
        count: messages.len(),
        messages,
    })
}
pub(super) async fn gmail_read(
    io: &mut impl LocalIo,
    message_id: &str,
) -> Result<GmailReadResult, GoogleApiError> {
    let v = io
        .call(
            google_apps_script::BridgeAction::GmailRead,
            json!({ "message_id": message_id }),
        )
        .await?;
    require_success(google_apps_script::BridgeAction::GmailRead, &v, &[Needs::Text("message_id")])?;
    let attachments = v
        .get("attachments")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .map(|a| GmailAttachment {
                    filename: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(a, "name")).text,
                    size: bu(a, "size"),
                })
                .collect()
        })
        .unwrap_or_default();
    let body = duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(&v, "body")).text;
    Ok(GmailReadResult {
        id: bs(&v, "message_id"),
        thread_id: String::new(),
        from: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(&v, "from")).text,
        to: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(&v, "to")).text,
        subject: duduclaw_security::one_time_credential::strip_one_time_credentials(&bs(&v, "subject")).text,
        date: bs(&v, "date"),
        // The bridge has no separate snippet concept; the first line of
        // the body is the honest equivalent rather than a fabricated one.
        snippet: duduclaw_core::truncate_chars(body.lines().next().unwrap_or(""), 200),
        body,
        body_truncated: v
            .get("truncated")
            .and_then(|t| t.as_bool())
            .unwrap_or(false),
        attachments,
    })
}
pub(super) async fn gmail_create_draft(
    io: &mut impl LocalIo,
    to: &str,
    subject: &str,
    body_text: &str,
    cc: Option<&str>,
) -> Result<GmailDraftResult, GoogleApiError> {
    // The shipped bridge script takes no cc field. Rather than silently
    // dropping a recipient the user asked for, refuse.
    if cc.is_some_and(|c| !c.trim().is_empty()) {
        return Err(bridge_err(google_apps_script::BridgeError::Unsupported(
            "gmail_create_draft with cc".into(),
        )));
    }
    let v = io
        .call(
            google_apps_script::BridgeAction::GmailCreateDraft,
            json!({ "to": to, "subject": subject, "body": body_text }),
        )
        .await?;
    // No draft id = no proof a draft exists; never report "Draft created".
    require_success(google_apps_script::BridgeAction::GmailCreateDraft, &v, &[Needs::Text("draft_id")])?;
    Ok(GmailDraftResult {
        draft_id: bs(&v, "draft_id"),
        message_id: bs(&v, "message_id"),
        to: to.to_string(),
        subject: duduclaw_security::one_time_credential::strip_one_time_credentials(subject).text,
    })
}
pub(super) async fn calendar_list_events(
    io: &mut impl LocalIo,
    time_min: Option<&str>,
    time_max: Option<&str>,
    max_results: u32,
) -> Result<CalendarEventsResult, GoogleApiError> {
    // The bridge takes a forward-looking day count, not a range. A
    // caller-supplied window is honoured as "days from now"; anything
    // else falls back to the same 7-day default the direct path uses.
    let days = days_until(time_max).unwrap_or(7);
    let v = io
        .call(
            google_apps_script::BridgeAction::CalendarListEvents,
            json!({ "days": days }),
        )
        .await?;
    require_success(google_apps_script::BridgeAction::CalendarListEvents, &v, &[Needs::List("events")])?;
    let events: Vec<CalendarEvent> = v
        .get("events")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .take(max_results.max(1) as usize)
                .map(|e| CalendarEvent {
                    id: bs(e, "id"),
                    summary: bs(e, "title"),
                    start: bs(e, "start"),
                    end: bs(e, "end"),
                    location: Some(bs(e, "location")).filter(|s| !s.is_empty()),
                    html_link: String::new(),
                    meet_link: None,
                    attendees_count: 0,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(CalendarEventsResult {
        count: events.len(),
        events,
    })
}
pub(super) async fn calendar_create_event(
    io: &mut impl LocalIo,
    summary: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    description: Option<&str>,
    attendees: Option<&[String]>,
    with_meet: bool,
) -> Result<CalendarCreateResult, GoogleApiError> {
    // Attendees and Meet links are outside the shipped script's action
    // surface. Creating the event minus the guests the user listed would
    // look like success while quietly failing the actual request.
    if attendees.is_some_and(|a| !a.is_empty()) {
        return Err(bridge_err(google_apps_script::BridgeError::Unsupported(
            "calendar_create_event with attendees".into(),
        )));
    }
    if with_meet {
        return Err(bridge_err(google_apps_script::BridgeError::Unsupported(
            "calendar_create_event with a Meet link".into(),
        )));
    }
    let v = io
        .call(
            google_apps_script::BridgeAction::CalendarCreateEvent,
            json!({
                "title": summary,
                "start": start_rfc3339,
                "end": end_rfc3339,
                "description": description.unwrap_or(""),
            }),
        )
        .await?;
    // No event id back = no proof the event exists.
    require_success(google_apps_script::BridgeAction::CalendarCreateEvent, &v, &[Needs::Text("id")])?;
    Ok(CalendarCreateResult {
        id: bs(&v, "id"),
        summary: Some(bs(&v, "title")).filter(|s| !s.is_empty()).unwrap_or_else(|| summary.to_string()),
        start: Some(bs(&v, "start")).filter(|s| !s.is_empty()).unwrap_or_else(|| start_rfc3339.to_string()),
        end: end_rfc3339.to_string(),
        // The script returns no link; the renderer omits an empty one.
        html_link: String::new(),
        meet_link: None,
        // Attendees are refused above, so nobody was invited.
        attendees_invited: 0,
    })
}
pub(super) async fn sheets_read(
    io: &mut impl LocalIo,
    spreadsheet_id: &str,
    range: &str,
) -> Result<SheetsReadResult, GoogleApiError> {
    let v = io
        .call(
            google_apps_script::BridgeAction::SheetsRead,
            json!({ "spreadsheet": spreadsheet_id, "range": range }),
        )
        .await?;
    require_success(google_apps_script::BridgeAction::SheetsRead, &v, &[Needs::List("values")])?;
    let rows: Vec<Vec<String>> = v
        .get("values")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(bridge_cell_to_string).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(SheetsReadResult {
        range: range.to_string(),
        row_count: rows.len(),
        // The bridge caps at 200 rows server-side; a full page back is
        // the only signal available that more may exist.
        rows_truncated: rows.len() >= 200,
        rows,
    })
}
pub(super) async fn sheets_append(
    io: &mut impl LocalIo,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<String>,
) -> Result<SheetsAppendResult, GoogleApiError> {
    // The script appends one row after the last row of the FIRST sheet and
    // takes no range. (It also accepts an optional sheet name, but a copy
    // deployed from an older template may not, and would then append to the
    // first sheet anyway — so none is sent.) The result says so instead of
    // echoing the requested range back as if it had been used.
    let v = io
        .call(
            google_apps_script::BridgeAction::SheetsAppend,
            json!({ "spreadsheet": spreadsheet_id, "values": values }),
        )
        .await?;
    require_success(google_apps_script::BridgeAction::SheetsAppend, &v, &[Needs::Yes("appended")])?;
    let row = bu(&v, "row");
    Ok(SheetsAppendResult {
        // Only what the script reported: the last row number after the append.
        updated_range: if row > 0 { format!("first sheet, row {row}") } else { String::new() },
        // The script reports no row/cell counts; omit them rather than guess.
        updated_rows: None,
        updated_cells: None,
        note: Some(bridge_append_note(range)),
    })
}

/// What a bridge append did with the requested range.
pub(super) fn bridge_append_note(range: &str) -> String {
    let range = range.trim();
    if range.is_empty() {
        "The Apps Script bridge appended the row to the first sheet of the spreadsheet.".to_string()
    } else {
        format!(
            "The Apps Script bridge appended the row to the first sheet of the spreadsheet; the requested range \"{}\" was ignored.",
            duduclaw_core::truncate_chars(range, 200)
        )
    }
}

/// One field a bridge reply must carry to count as a success.
#[derive(Clone, Copy)]
pub(super) enum Needs {
    /// A non-empty string (an id).
    Text(&'static str),
    /// A JSON array (possibly empty).
    List(&'static str),
    /// The literal `true`.
    Yes(&'static str),
}

/// A bridge reply is a success only in the shape the shipped script returns
/// for that action. An empty object, `{"ok": false}`, an `error` of any type
/// (string, object, number) or a missing/blank required field is a tool
/// error with a reason — never a success filled with empty strings.
pub(super) fn require_success(
    action: google_apps_script::BridgeAction,
    v: &Value,
    needs: &[Needs],
) -> Result<(), GoogleApiError> {
    let fail = |reason: String| {
        Err(GoogleApiError::Api {
            status: 502,
            message: format!("Apps Script bridge `{}` did not succeed: {reason}", action.as_str()),
        })
    };
    let Some(map) = v.as_object() else {
        return fail("the reply is not a JSON object".into());
    };
    if let Some(error) = map.get("error").filter(|e| !e.is_null()) {
        let text = error
            .as_str()
            .or_else(|| error.get("message").and_then(Value::as_str))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| duduclaw_core::truncate_chars(t, 300));
        return fail(text.unwrap_or_else(|| {
            duduclaw_core::tool_error_reason::missing_error_reason(action.as_str(), Some("apps_script_bridge"), None, None)
        }));
    }
    if map.get("ok").is_some_and(|ok| ok.as_bool() != Some(true)) {
        return fail("the script reported ok: false".into());
    }
    for need in needs {
        let missing = match *need {
            Needs::Text(key) => !map
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty()),
            Needs::List(key) => !map.get(key).is_some_and(Value::is_array),
            Needs::Yes(key) => map.get(key).and_then(Value::as_bool) != Some(true),
        };
        if missing {
            let key = match *need {
                Needs::Text(k) | Needs::List(k) | Needs::Yes(k) => k,
            };
            return fail(format!("the reply has no `{key}`"));
        }
    }
    Ok(())
}

pub(super) fn bs(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}
pub(super) fn bu(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}
pub(super) fn days_until(time_max: Option<&str>) -> Option<u32> {
    let ts = time_max?;
    let target = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let now = chrono::Utc::now();
    let delta = target.with_timezone(&chrono::Utc) - now;
    let days = delta.num_days();
    if days <= 0 {
        Some(1)
    } else {
        Some(days.min(365) as u32)
    }
}

pub(super) fn bridge_err(e: crate::google_apps_script::BridgeError) -> GoogleApiError {
    GoogleApiError::Api {
        status: 502,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod success_shape_tests {
    use super::*;

    /// Answers every call with one fixed reply.
    struct Reply(Value);
    #[async_trait::async_trait]
    impl LocalIo for Reply {
        async fn call(
            &mut self,
            _: google_apps_script::BridgeAction,
            _: Value,
        ) -> Result<Value, GoogleApiError> {
            Ok(self.0.clone())
        }
    }

    fn not_success() -> Vec<Value> {
        vec![
            json!({}),
            json!({"ok": false}),
            json!({"error": {"code": 500}}),
            json!({"error": {"message": "quota exceeded"}}),
            json!({"error": 7}),
            json!({"error": "boom"}),
            json!([]),
            json!("ok"),
        ]
    }

    #[tokio::test]
    async fn empty_failed_or_error_replies_are_errors_with_a_reason() {
        for reply in not_success() {
            let draft = gmail_create_draft(&mut Reply(reply.clone()), "a@b.test", "s", "b", None).await;
            let event = calendar_create_event(&mut Reply(reply.clone()), "s", "2026-08-01T00:00:00Z", "2026-08-01T01:00:00Z", None, None, false).await;
            let append = sheets_append(&mut Reply(reply.clone()), "sheet", "A1", vec!["x".into()]).await;
            let search = gmail_search(&mut Reply(reply.clone()), "q", 5).await;
            for message in [
                draft.err().map(|e| e.to_string()),
                event.err().map(|e| e.to_string()),
                append.err().map(|e| e.to_string()),
                search.err().map(|e| e.to_string()),
            ] {
                let message = message.unwrap_or_else(|| panic!("{reply} must be an error"));
                assert!(message.contains("did not succeed"), "{message}");
                assert!(message.len() > "did not succeed: ".len());
            }
        }
        let error = gmail_create_draft(&mut Reply(json!({"error":{"message":"quota exceeded"}})), "a@b.test", "s", "b", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("quota exceeded"), "{error}");
        let error = gmail_create_draft(&mut Reply(json!({"error":{"code":1}})), "a@b.test", "s", "b", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("returned no error message"), "{error}");
    }

    #[tokio::test]
    async fn blank_ids_are_not_success() {
        assert!(gmail_create_draft(&mut Reply(json!({"draft_id": "", "message_id": "m"})), "a@b.test", "s", "b", None).await.is_err());
        assert!(gmail_create_draft(&mut Reply(json!({"draft_id": "  "})), "a@b.test", "s", "b", None).await.is_err());
        assert!(calendar_create_event(&mut Reply(json!({"id": "", "title": "s"})), "s", "a", "b", None, None, false).await.is_err());
        assert!(sheets_append(&mut Reply(json!({"appended": false, "row": 3})), "sheet", "A1", vec![]).await.is_err());
        assert!(sheets_append(&mut Reply(json!({"row": 3})), "sheet", "A1", vec![]).await.is_err());
        let draft = gmail_create_draft(&mut Reply(json!({"draft_id": "d1", "message_id": "m1"})), "a@b.test", "s", "b", None).await.unwrap();
        assert_eq!(draft.draft_id, "d1");
    }

    #[tokio::test]
    async fn calendar_event_reports_no_attendees() {
        let event = calendar_create_event(
            &mut Reply(json!({"id": "ev1", "title": "s", "start": "2026-08-01T00:00:00.000Z"})),
            "s", "2026-08-01T00:00:00Z", "2026-08-01T01:00:00Z", None, None, false,
        )
        .await
        .unwrap();
        assert_eq!(event.id, "ev1");
        assert_eq!(event.attendees_invited, 0);
        assert!(event.html_link.is_empty());
    }

    #[tokio::test]
    async fn sheets_append_says_the_range_was_ignored_and_reports_no_assumed_counts() {
        let result = sheets_append(&mut Reply(json!({"appended": true, "row": 12})), "sheet", "Sheet2!B3", vec!["x".into(), "y".into()])
            .await
            .unwrap();
        assert_eq!(result.updated_range, "first sheet, row 12");
        assert_eq!(result.updated_rows, None);
        assert_eq!(result.updated_cells, None);
        let note = result.note.unwrap();
        assert!(note.contains("first sheet"), "{note}");
        assert!(note.contains("\"Sheet2!B3\" was ignored"), "{note}");
        let result = sheets_append(&mut Reply(json!({"appended": true})), "sheet", "", vec![]).await.unwrap();
        assert!(result.updated_range.is_empty());
        assert!(!result.note.unwrap().contains("ignored"));
    }
}

#[cfg(test)]
mod credential_stripping_tests {
    use super::*;

    const LOGIN_LINK: &str = "https://example.test/magic/AbCdEf0123456789";
    const CODE: &str = "482913";

    struct FakeIo;
    #[async_trait::async_trait]
    impl LocalIo for FakeIo {
        async fn call(
            &mut self,
            action: google_apps_script::BridgeAction,
            params: Value,
        ) -> Result<Value, GoogleApiError> {
            let text = format!("驗證碼 {CODE} {LOGIN_LINK} 中文 😀");
            let message = json!({
                "message_id": "m123", "from": text, "to": text,
                "subject": text, "snippet": text, "body": text,
                "attachments": [{"name": text, "size": 12}],
            });
            match action {
                google_apps_script::BridgeAction::GmailSearch => Ok(json!({"messages": [message]})),
                google_apps_script::BridgeAction::GmailRead => Ok(message),
                google_apps_script::BridgeAction::GmailCreateDraft => {
                    assert_eq!(params["subject"], text);
                    assert_eq!(params["body"], "body");
                    Ok(json!({"draft_id": "d123", "message_id": "m123"}))
                }
                _ => panic!("unexpected bridge action"),
            }
        }
    }

    fn assert_stripped(text: &str) {
        assert!(!text.contains(CODE), "{text}");
        assert!(!text.contains(LOGIN_LINK), "{text}");
        assert!(text.contains("[已移除 1 組驗證碼]"), "{text}");
        assert!(text.contains("[已移除 1 個登入連結]"), "{text}");
        assert!(text.contains("中文 😀"));
    }

    #[tokio::test]
    async fn bridge_gmail_draft_strips_returned_subject() {
        let subject = format!("驗證碼 {CODE} {LOGIN_LINK} 中文 😀");
        let result = gmail_create_draft(&mut FakeIo, "me@example.test", &subject, "body", None).await.unwrap();
        assert_eq!(result.draft_id, "d123");
        assert_stripped(&result.subject);
    }

    #[tokio::test]
    async fn bridge_gmail_search_strips_subject_and_snippet() {
        let result = gmail_search(&mut FakeIo, "in:inbox", 1).await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.messages[0].id, "m123");
        assert_stripped(&result.messages[0].from);
        assert_stripped(&result.messages[0].subject);
        assert_stripped(&result.messages[0].snippet);
    }

    #[tokio::test]
    async fn bridge_gmail_read_strips_subject_body_and_derived_snippet() {
        let result = gmail_read(&mut FakeIo, "m123").await.unwrap();
        assert_eq!(result.id, "m123");
        assert_stripped(&result.from);
        assert_stripped(&result.to);
        assert_stripped(&result.attachments[0].filename);
        assert_stripped(&result.subject);
        assert_stripped(&result.body);
        assert_stripped(&result.snippet);
        assert_eq!(result.snippet, duduclaw_core::truncate_chars(result.body.lines().next().unwrap(), 200));
    }
}
