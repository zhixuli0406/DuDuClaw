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
    Ok(CalendarCreateResult {
        id: bs(&v, "id"),
        summary: bs(&v, "title"),
        start: bs(&v, "start"),
        end: end_rfc3339.to_string(),
        html_link: String::new(),
        meet_link: None,
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
    let cells = values.len() as u64;
    let v = io
        .call(
            google_apps_script::BridgeAction::SheetsAppend,
            json!({ "spreadsheet": spreadsheet_id, "values": values }),
        )
        .await?;
    let row = bu(&v, "row");
    Ok(SheetsAppendResult {
        updated_range: if row > 0 {
            format!("row {row}")
        } else {
            range.to_string()
        },
        updated_rows: 1,
        updated_cells: cells,
    })
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
