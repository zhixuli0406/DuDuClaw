// Included independently below each credential owner and the legacy adapter.
// Never re-export this module or raw provider replies.
use crate::google_workspace::*;
use crate::google_workspace::native_call::{NativeCallTemplate, ParsedNativeArguments};
use serde_json::{Value, json};
const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";
const SHEETS_BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";
const FORMS_BASE: &str = "https://forms.googleapis.com/v1/forms";
const TASKS_BASE: &str = "https://tasks.googleapis.com/tasks/v1";
const DRIVE_BASE: &str = "https://www.googleapis.com/drive/v3/files";
const DOCS_BASE: &str = "https://docs.googleapis.com/v1/documents";
const SLIDES_BASE: &str = "https://slides.googleapis.com/v1/presentations";
const SHEETS_MAX_ROWS: usize = 200;
const FORMS_MAX_RESPONSES: usize = 50;
const TASKS_MAX_ITEMS: u32 = 100;
const DRIVE_MAX_FILES: u32 = 50;
const DOC_TEXT_MAX_CHARS: usize = 20_000;
const BODY_MAX_CHARS: usize = 8000;

pub(super) struct NativeStep<'a> {
    template: &'a NativeCallTemplate,
    method: reqwest::Method,
    url: String,
    query: Vec<(String, String)>,
    body: Option<Value>,
}
impl NativeStep<'_> {
    pub(super) fn template(&self) -> &NativeCallTemplate {
        self.template
    }
    pub(super) fn method(&self) -> &reqwest::Method {
        &self.method
    }
    pub(super) fn url(&self) -> &str {
        &self.url
    }
    pub(super) fn query(&self) -> &[(String, String)] {
        &self.query
    }
    pub(super) fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
}
#[async_trait::async_trait]
pub(super) trait LocalIo: LocalRender + Send {
    async fn json(&mut self, step: NativeStep<'_>) -> Result<Value, GoogleApiError>;
    async fn text(&mut self, step: NativeStep<'_>) -> Result<String, GoogleApiError>;
    fn checkpoint(&mut self) -> Result<(), GoogleApiError>;
    fn reject_derived(&mut self, candidate: &str) -> Result<(), GoogleApiError>;
    fn sanitize(&self, value: &str) -> String;
    fn closed(&self) -> bool;
}
pub(super) trait LocalRender {
    fn inspect_curated(&mut self, value: &Value) -> Result<(), GoogleApiError>;
    fn refuse(&mut self) -> GoogleApiError;
}
async fn google_request<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value, GoogleApiError> {
    io.checkpoint()?;
    let step = NativeStep {
        template,
        method,
        url: url.to_owned(),
        query: query
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        body: body.cloned(),
    };
    let result = io.json(step).await;
    io.checkpoint()?;
    result
}
async fn google_request_text<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
) -> Result<String, GoogleApiError> {
    io.checkpoint()?;
    let step = NativeStep {
        template,
        method,
        url: url.to_owned(),
        query: query
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        body: None,
    };
    let result = io.text(step).await;
    io.checkpoint()?;
    result
}
pub(super) async fn gmail_search<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    query: &str,
    max_results: u32,
) -> Result<GmailSearchResult, GoogleApiError> {
    let n = clamp(max_results, 1, 25);
    let list = google_request(
        io,
        template,
        reqwest::Method::GET,
        &format!("{GMAIL_BASE}/messages"),
        &[("q", query.to_string()), ("maxResults", n.to_string())],
        None,
    )
    .await?;

    let ids: Vec<(String, String)> = list
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    let tid = m
                        .get("threadId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((id, tid))
                })
                .collect()
        })
        .unwrap_or_default();

    if io.closed() && ids.len() > 25 {
        return Err(io.refuse());
    }
    let mut messages = Vec::with_capacity(ids.len());
    for (id, thread_id) in ids {
        io.reject_derived(&id)?;
        if io.closed() && !crate::google_workspace::native_call::safe_gmail_component(&id) {
            return Err(io.refuse());
        }
        let meta = google_request(
            io,
            template,
            reqwest::Method::GET,
            &format!("{GMAIL_BASE}/messages/{id}"),
            &[
                ("format", "metadata".to_string()),
                ("metadataHeaders", "From".to_string()),
                ("metadataHeaders", "Subject".to_string()),
                ("metadataHeaders", "Date".to_string()),
            ],
            None,
        )
        .await?;

        let headers = meta
            .get("payload")
            .and_then(|p| p.get("headers"))
            .cloned()
            .unwrap_or(Value::Null);

        messages.push(GmailMessageMeta {
            id,
            thread_id,
            from: header_value(&headers, "From").unwrap_or_default(),
            subject: header_value(&headers, "Subject").unwrap_or_default(),
            date: header_value(&headers, "Date").unwrap_or_default(),
            snippet: meta
                .get("snippet")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }

    Ok(GmailSearchResult {
        count: messages.len(),
        messages,
    })
}

pub(super) async fn gmail_read<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    message_id: &str,
) -> Result<GmailReadResult, GoogleApiError> {
    let msg = google_request(
        io,
        template,
        reqwest::Method::GET,
        &format!("{GMAIL_BASE}/messages/{message_id}"),
        &[("format", "full".to_string())],
        None,
    )
    .await?;

    let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
    let headers = payload.get("headers").cloned().unwrap_or(Value::Null);

    let body_raw = io.sanitize(&extract_message_body(&payload));
    let body = duduclaw_core::truncate_chars(&body_raw, BODY_MAX_CHARS);
    let body_truncated = body.chars().count() < body_raw.chars().count();
    let attachments = collect_attachments(&payload);

    Ok(GmailReadResult {
        id: message_id.to_string(),
        thread_id: msg
            .get("threadId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        from: header_value(&headers, "From").unwrap_or_default(),
        to: header_value(&headers, "To").unwrap_or_default(),
        subject: header_value(&headers, "Subject").unwrap_or_default(),
        date: header_value(&headers, "Date").unwrap_or_default(),
        snippet: msg
            .get("snippet")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        body,
        body_truncated,
        attachments,
    })
}

pub(super) async fn gmail_create_draft<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    to: &str,
    subject: &str,
    body_text: &str,
    cc: Option<&str>,
) -> Result<GmailDraftResult, GoogleApiError> {
    let raw = build_rfc2822(to, subject, body_text, cc);
    let encoded = encode_raw_message(&raw);
    let payload = json!({ "message": { "raw": encoded } });

    let resp = google_request(
        io,
        template,
        reqwest::Method::POST,
        &format!("{GMAIL_BASE}/drafts"),
        &[],
        Some(&payload),
    )
    .await?;

    Ok(GmailDraftResult {
        draft_id: resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        message_id: resp
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        to: to.to_string(),
        subject: subject.to_string(),
    })
}

pub(super) async fn calendar_list_events<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    time_min: Option<&str>,
    time_max: Option<&str>,
    max_results: u32,
) -> Result<CalendarEventsResult, GoogleApiError> {
    let n = clamp(max_results, 1, 50);
    let (a, b) = template.time_window();
    let (def_min, def_max) = (a.to_owned(), b.to_owned());
    let tmin = time_min.map(|s| s.to_string()).unwrap_or(def_min);
    let tmax = time_max.map(|s| s.to_string()).unwrap_or(def_max);

    let resp = google_request(
        io,
        template,
        reqwest::Method::GET,
        &format!("{CALENDAR_BASE}/calendars/primary/events"),
        &[
            ("singleEvents", "true".to_string()),
            ("orderBy", "startTime".to_string()),
            ("timeMin", tmin),
            ("timeMax", tmax),
            ("maxResults", n.to_string()),
        ],
        None,
    )
    .await?;

    let events: Vec<CalendarEvent> = resp
        .get("items")
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().map(parse_calendar_event).collect())
        .unwrap_or_default();

    Ok(CalendarEventsResult {
        count: events.len(),
        events,
    })
}

pub(super) async fn calendar_create_event<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    summary: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    description: Option<&str>,
    attendees: Option<&[String]>,
    with_meet: bool,
) -> Result<CalendarCreateResult, GoogleApiError> {
    let mut body = json!({
        "summary": summary,
        "start": { "dateTime": start_rfc3339 },
        "end": { "dateTime": end_rfc3339 },
    });
    if let Some(d) = description {
        if !d.is_empty() {
            body["description"] = json!(d);
        }
    }
    if let Some(atts) = attendees {
        if !atts.is_empty() {
            body["attendees"] = Value::Array(atts.iter().map(|e| json!({ "email": e })).collect());
        }
    }

    let mut query: Vec<(&str, String)> = Vec::new();
    if with_meet {
        body["conferenceData"] = json!({
            "createRequest": {
                "requestId": template.meet_request_id().to_owned(),
                "conferenceSolutionKey": { "type": "hangoutsMeet" }
            }
        });
        query.push(("conferenceDataVersion", "1".to_string()));
    }

    let resp = google_request(
        io,
        template,
        reqwest::Method::POST,
        &format!("{CALENDAR_BASE}/calendars/primary/events"),
        &query,
        Some(&body),
    )
    .await?;

    Ok(CalendarCreateResult {
        id: resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        summary: resp
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or(summary)
            .to_string(),
        start: event_time(&resp, "start"),
        end: event_time(&resp, "end"),
        html_link: resp
            .get("htmlLink")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        meet_link: extract_meet_link(&resp),
    })
}

pub(super) async fn sheets_read<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    spreadsheet_id: &str,
    range: &str,
) -> Result<SheetsReadResult, GoogleApiError> {
    let id = extract_spreadsheet_id(spreadsheet_id);
    let url = format!(
        "{SHEETS_BASE}/{}/values/{}",
        encode_path_component(&id),
        encode_path_component(range)
    );
    let resp = google_request(io, template, reqwest::Method::GET, &url, &[], None).await?;

    let all_rows: Vec<Vec<String>> = resp
        .get("values")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(cell_to_string).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();

    let rows_truncated = all_rows.len() > SHEETS_MAX_ROWS;
    let rows: Vec<Vec<String>> = all_rows.into_iter().take(SHEETS_MAX_ROWS).collect();

    Ok(SheetsReadResult {
        range: resp
            .get("range")
            .and_then(|v| v.as_str())
            .unwrap_or(range)
            .to_string(),
        row_count: rows.len(),
        rows_truncated,
        rows,
    })
}

pub(super) async fn sheets_append<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<String>,
) -> Result<SheetsAppendResult, GoogleApiError> {
    let id = extract_spreadsheet_id(spreadsheet_id);
    let url = format!(
        "{SHEETS_BASE}/{}/values/{}:append",
        encode_path_component(&id),
        encode_path_component(range)
    );
    let body = json!({ "values": [values] });
    let resp = google_request(
        io,
        template,
        reqwest::Method::POST,
        &url,
        &[
            ("valueInputOption", "USER_ENTERED".to_string()),
            ("insertDataOption", "INSERT_ROWS".to_string()),
        ],
        Some(&body),
    )
    .await?;

    let updates = resp.get("updates").cloned().unwrap_or(Value::Null);
    Ok(SheetsAppendResult {
        updated_range: updates
            .get("updatedRange")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        updated_rows: updates
            .get("updatedRows")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        updated_cells: updates
            .get("updatedCells")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

pub(super) async fn forms_get<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    form_id: &str,
) -> Result<FormResult, GoogleApiError> {
    let id = extract_form_id(form_id);
    let url = format!("{FORMS_BASE}/{}", encode_path_component(&id));
    let resp = google_request(io, template, reqwest::Method::GET, &url, &[], None).await?;

    let info = resp.get("info").cloned().unwrap_or(Value::Null);
    let questions: Vec<FormQuestion> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_form_item).collect())
        .unwrap_or_default();

    Ok(FormResult {
        form_id: resp
            .get("formId")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string(),
        title: info
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: info
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        question_count: questions.len(),
        questions,
    })
}

pub(super) async fn forms_list_responses<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    form_id: &str,
) -> Result<FormResponsesResult, GoogleApiError> {
    let id = extract_form_id(form_id);
    let url = format!("{FORMS_BASE}/{}/responses", encode_path_component(&id));
    let resp = google_request(
        io,
        template,
        reqwest::Method::GET,
        &url,
        &[("pageSize", FORMS_MAX_RESPONSES.to_string())],
        None,
    )
    .await?;

    let all: Vec<&Value> = resp
        .get("responses")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // A nextPageToken means Google has more than one page for us.
    let truncated = resp.get("nextPageToken").and_then(|v| v.as_str()).is_some();
    let responses: Vec<FormResponseEntry> = all
        .into_iter()
        .take(FORMS_MAX_RESPONSES)
        .map(parse_form_response)
        .collect();

    Ok(FormResponsesResult {
        form_id: id,
        count: responses.len(),
        truncated,
        responses,
    })
}

pub(super) async fn tasks_list_tasklists<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
) -> Result<TaskListsResult, GoogleApiError> {
    let url = format!("{TASKS_BASE}/users/@me/lists");
    let resp = google_request(
        io,
        template,
        reqwest::Method::GET,
        &url,
        &[("maxResults", "100".to_string())],
        None,
    )
    .await?;

    let lists: Vec<TaskListEntry> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|l| TaskListEntry {
                    id: str_field(l, "id"),
                    title: str_field(l, "title"),
                    updated: str_field(l, "updated"),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TaskListsResult {
        count: lists.len(),
        lists,
    })
}

pub(super) async fn tasks_list<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    task_list_id: &str,
    show_completed: bool,
    max_results: u32,
) -> Result<TasksResult, GoogleApiError> {
    let n = clamp(max_results, 1, TASKS_MAX_ITEMS);
    let url = format!(
        "{TASKS_BASE}/lists/{}/tasks",
        encode_path_component(task_list_id)
    );
    let mut query = vec![
        ("maxResults", n.to_string()),
        ("showCompleted", show_completed.to_string()),
    ];
    if show_completed {
        // Completed-and-hidden tasks stay invisible without this.
        query.push(("showHidden", "true".to_string()));
    }
    let resp = google_request(io, template, reqwest::Method::GET, &url, &query, None).await?;

    let tasks: Vec<TaskEntry> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(parse_task).collect())
        .unwrap_or_default();
    let truncated = resp.get("nextPageToken").and_then(|v| v.as_str()).is_some();

    Ok(TasksResult {
        task_list_id: task_list_id.to_string(),
        count: tasks.len(),
        truncated,
        tasks,
    })
}

pub(super) async fn tasks_create<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    task_list_id: &str,
    title: &str,
    notes: Option<&str>,
    due: Option<&str>,
) -> Result<TaskEntry, GoogleApiError> {
    let url = format!(
        "{TASKS_BASE}/lists/{}/tasks",
        encode_path_component(task_list_id)
    );
    let mut body = serde_json::Map::new();
    body.insert("title".into(), json!(title));
    if let Some(n) = notes.filter(|s| !s.trim().is_empty()) {
        body.insert("notes".into(), json!(n));
    }
    if let Some(d) = due.filter(|s| !s.trim().is_empty()) {
        body.insert("due".into(), json!(d));
    }
    let resp = google_request(
        io,
        template,
        reqwest::Method::POST,
        &url,
        &[],
        Some(&Value::Object(body)),
    )
    .await?;
    Ok(parse_task(&resp))
}

pub(super) async fn tasks_complete<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    task_list_id: &str,
    task_id: &str,
) -> Result<TaskEntry, GoogleApiError> {
    let url = format!(
        "{TASKS_BASE}/lists/{}/tasks/{}",
        encode_path_component(task_list_id),
        encode_path_component(task_id)
    );
    let body = json!({ "status": "completed" });
    let resp = google_request(io, template, reqwest::Method::PATCH, &url, &[], Some(&body)).await?;
    Ok(parse_task(&resp))
}

pub(super) async fn drive_search<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    query: &str,
    mime_type: Option<&str>,
    max_results: u32,
) -> Result<DriveSearchResult, GoogleApiError> {
    let n = clamp(max_results, 1, DRIVE_MAX_FILES);
    let mut q = format!(
        "trashed = false and (name contains '{}' or fullText contains '{}')",
        escape_drive_query_value(query),
        escape_drive_query_value(query)
    );
    if let Some(m) = mime_type.filter(|s| !s.trim().is_empty()) {
        q.push_str(&format!(
            " and mimeType = '{}'",
            escape_drive_query_value(m)
        ));
    }

    let resp = google_request(
        io,
        template,
        reqwest::Method::GET,
        DRIVE_BASE,
        &[
            ("q", q),
            ("pageSize", n.to_string()),
            ("orderBy", "modifiedTime desc".to_string()),
            (
                "fields",
                "nextPageToken,files(id,name,mimeType,modifiedTime,size,webViewLink)".to_string(),
            ),
            // Shared-drive items are invisible without both flags.
            ("includeItemsFromAllDrives", "true".to_string()),
            ("supportsAllDrives", "true".to_string()),
        ],
        None,
    )
    .await?;

    let files: Vec<DriveFileMeta> = resp
        .get("files")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|f| DriveFileMeta {
                    id: str_field(f, "id"),
                    name: str_field(f, "name"),
                    mime_type: str_field(f, "mimeType"),
                    modified_time: str_field(f, "modifiedTime"),
                    size: str_field(f, "size"),
                    web_view_link: str_field(f, "webViewLink"),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(DriveSearchResult {
        count: files.len(),
        truncated: resp.get("nextPageToken").and_then(|v| v.as_str()).is_some(),
        files,
    })
}

pub(super) async fn drive_read<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    file_id: &str,
) -> Result<DriveReadResult, GoogleApiError> {
    let id = extract_drive_file_id(file_id);
    // Metadata first: the MIME type decides export vs download.
    let meta = google_request(
        io,
        template,
        reqwest::Method::GET,
        &format!("{DRIVE_BASE}/{}", encode_path_component(&id)),
        &[
            ("fields", "id,name,mimeType".to_string()),
            ("supportsAllDrives", "true".to_string()),
        ],
        None,
    )
    .await?;
    let name = str_field(&meta, "name");
    let mime = str_field(&meta, "mimeType");
    io.reject_derived(&mime)?;

    // Google-native → export; text-ish blob → download; else refuse politely.
    let (url, query, exported_as) = match export_mime_for(&mime) {
        Some(export_mime) => (
            format!("{DRIVE_BASE}/{}/export", encode_path_component(&id)),
            vec![("mimeType", export_mime.to_string())],
            Some(export_mime.to_string()),
        ),
        None if is_text_like_mime(&mime) => (
            format!("{DRIVE_BASE}/{}", encode_path_component(&id)),
            vec![
                ("alt", "media".to_string()),
                ("supportsAllDrives", "true".to_string()),
            ],
            None,
        ),
        None => {
            return Ok(DriveReadResult {
                id,
                name,
                mime_type: mime.clone(),
                exported_as: None,
                content: String::new(),
                truncated: false,
                note: Some(format!(
                    "This file type ({mime}) is not text — DuDuClaw does not download binary \
                     content. Open it via web_view_link, or ask for a Google Docs/Sheets/Slides \
                     file instead."
                )),
            });
        }
    };

    let raw = google_request_text(io, template, reqwest::Method::GET, &url, &query).await?;
    let raw = io.sanitize(&raw);
    let (content, truncated) = truncate_text(&raw, DOC_TEXT_MAX_CHARS);
    let note = (exported_as.as_deref() == Some("text/csv"))
        .then(|| "Sheets export covers the FIRST sheet only — use sheets_read for a specific range or tab.".to_string());

    Ok(DriveReadResult {
        id,
        name,
        mime_type: mime,
        exported_as,
        content,
        truncated,
        note,
    })
}

pub(super) async fn docs_read<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    document_id: &str,
) -> Result<DocsReadResult, GoogleApiError> {
    let id = extract_drive_file_id(document_id);
    let resp = google_request(
        io,
        template,
        reqwest::Method::GET,
        &format!("{DOCS_BASE}/{}", encode_path_component(&id)),
        &[],
        None,
    )
    .await?;

    let mut text = String::new();
    collect_doc_text(
        resp.get("body")
            .and_then(|b| b.get("content"))
            .unwrap_or(&Value::Null),
        &mut text,
    );
    let text = io.sanitize(&text);
    let (text, truncated) = truncate_text(&text, DOC_TEXT_MAX_CHARS);

    Ok(DocsReadResult {
        document_id: str_field(&resp, "documentId"),
        title: str_field(&resp, "title"),
        text,
        truncated,
    })
}

pub(super) async fn docs_append<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    document_id: &str,
    text: &str,
) -> Result<DocsAppendResult, GoogleApiError> {
    let id = extract_drive_file_id(document_id);
    let body = json!({
        "requests": [{
            "insertText": {
                "text": text,
                // Empty EndOfSegmentLocation = end of the document body.
                "endOfSegmentLocation": {}
            }
        }]
    });
    google_request(
        io,
        template,
        reqwest::Method::POST,
        &format!("{DOCS_BASE}/{}:batchUpdate", encode_path_component(&id)),
        &[],
        Some(&body),
    )
    .await?;

    Ok(DocsAppendResult {
        document_id: id,
        appended_chars: text.chars().count(),
    })
}

pub(super) async fn slides_read<I: LocalIo>(
    io: &mut I,
    template: &NativeCallTemplate,
    presentation_id: &str,
) -> Result<SlidesReadResult, GoogleApiError> {
    let id = extract_drive_file_id(presentation_id);
    let resp = google_request(
        io,
        template,
        reqwest::Method::GET,
        &format!("{SLIDES_BASE}/{}", encode_path_component(&id)),
        &[],
        None,
    )
    .await?;

    let slides: Vec<SlideText> = resp
        .get("slides")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut text = String::new();
                    collect_slide_text(s.get("pageElements").unwrap_or(&Value::Null), &mut text);
                    SlideText {
                        index: i + 1,
                        object_id: str_field(s, "objectId"),
                        text: truncate_text(&io.sanitize(&text), DOC_TEXT_MAX_CHARS).0,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SlidesReadResult {
        presentation_id: str_field(&resp, "presentationId"),
        title: str_field(&resp, "title"),
        slide_count: slides.len(),
        slides,
    })
}

pub(super) fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    n.max(lo).min(hi)
}

pub(super) fn truncate_text(s: &str, max_chars: usize) -> (String, bool) {
    if s.chars().count() <= max_chars {
        return (s.to_string(), false);
    }
    (s.chars().take(max_chars).collect(), true)
}

pub(super) fn escape_drive_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            // Control characters have no place in a query literal.
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

pub(super) fn export_mime_for(mime: &str) -> Option<&'static str> {
    match mime {
        "application/vnd.google-apps.document" => Some("text/plain"),
        "application/vnd.google-apps.spreadsheet" => Some("text/csv"),
        "application/vnd.google-apps.presentation" => Some("text/plain"),
        _ => None,
    }
}

pub(super) fn is_text_like_mime(mime: &str) -> bool {
    let base = mime.split(';').next().unwrap_or(mime).trim();
    base.starts_with("text/")
        || matches!(
            base,
            "application/json"
                | "application/xml"
                | "application/x-yaml"
                | "application/yaml"
                | "application/x-ndjson"
                | "application/toml"
        )
}

pub(super) fn extract_drive_file_id(input: &str) -> String {
    let s = input.trim();
    if let Some(after) = s.split("/d/").nth(1) {
        let after = after.strip_prefix("e/").unwrap_or(after);
        let id: String = after
            .chars()
            .take_while(|c| *c != '/' && *c != '#' && *c != '?')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    // Legacy `open?id=<ID>` / `uc?id=<ID>` links.
    if let Some(after) = s.split("id=").nth(1) {
        let id: String = after
            .chars()
            .take_while(|c| *c != '&' && *c != '#')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    s.to_string()
}

pub(super) fn collect_doc_text(content: &Value, out: &mut String) {
    let Some(arr) = content.as_array() else {
        return;
    };
    for el in arr {
        if let Some(paragraph) = el.get("paragraph") {
            if let Some(elements) = paragraph.get("elements").and_then(|v| v.as_array()) {
                for e in elements {
                    if let Some(t) = e
                        .get("textRun")
                        .and_then(|t| t.get("content"))
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(t);
                    }
                }
            }
        } else if let Some(table) = el.get("table") {
            for row in table
                .get("tableRows")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                for cell in row
                    .get("tableCells")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    collect_doc_text(cell.get("content").unwrap_or(&Value::Null), out);
                }
            }
        }
        // sectionBreak / tableOfContents carry no author text of their own.
    }
}

pub(super) fn collect_slide_text(page_elements: &Value, out: &mut String) {
    let Some(arr) = page_elements.as_array() else {
        return;
    };
    for el in arr {
        if let Some(text) = el.get("shape").and_then(|s| s.get("text")) {
            append_slide_text_elements(text, out);
        }
        if let Some(table) = el.get("table") {
            for row in table
                .get("tableRows")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                for cell in row
                    .get("tableCells")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = cell.get("text") {
                        append_slide_text_elements(text, out);
                    }
                }
            }
        }
        if let Some(children) = el.get("elementGroup").and_then(|g| g.get("children")) {
            collect_slide_text(children, out);
        }
    }
}

pub(super) fn append_slide_text_elements(text: &Value, out: &mut String) {
    let Some(elements) = text.get("textElements").and_then(|v| v.as_array()) else {
        return;
    };
    for e in elements {
        if let Some(t) = e
            .get("textRun")
            .and_then(|r| r.get("content"))
            .and_then(|v| v.as_str())
        {
            out.push_str(t);
        }
    }
}

pub(super) fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn extract_form_id(input: &str) -> String {
    let s = input.trim();
    if let Some(after) = s.split("/d/").nth(1) {
        // The viewer form embeds the published id under an extra `e/` segment.
        let after = after.strip_prefix("e/").unwrap_or(after);
        let id: String = after
            .chars()
            .take_while(|c| *c != '/' && *c != '#' && *c != '?')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    s.to_string()
}

pub(super) fn parse_form_item(item: &Value) -> Option<FormQuestion> {
    let question = item.get("questionItem")?.get("question")?;
    let (kind, options) = if let Some(choice) = question.get("choiceQuestion") {
        let opts = choice
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| {
                        // An "Other" option has no `value`, only `isOther`.
                        o.get("value")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| "(other)".to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        ("choice", opts)
    } else if question.get("textQuestion").is_some() {
        ("text", Vec::new())
    } else if question.get("scaleQuestion").is_some() {
        ("scale", Vec::new())
    } else if question.get("dateQuestion").is_some() {
        ("date", Vec::new())
    } else if question.get("timeQuestion").is_some() {
        ("time", Vec::new())
    } else if question.get("fileUploadQuestion").is_some() {
        ("file_upload", Vec::new())
    } else if question.get("ratingQuestion").is_some() {
        ("rating", Vec::new())
    } else if question.get("rowQuestion").is_some() {
        ("row", Vec::new())
    } else {
        ("other", Vec::new())
    };

    Some(FormQuestion {
        question_id: str_field(question, "questionId"),
        title: str_field(item, "title"),
        kind: kind.to_string(),
        options,
        required: question
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

pub(super) fn parse_form_response(resp: &Value) -> FormResponseEntry {
    let mut answers = std::collections::BTreeMap::new();
    if let Some(map) = resp.get("answers").and_then(|v| v.as_object()) {
        for (qid, ans) in map {
            let joined = ans
                .get("textAnswers")
                .and_then(|t| t.get("answers"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get("value").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            answers.insert(qid.clone(), joined);
        }
    }
    FormResponseEntry {
        response_id: str_field(resp, "responseId"),
        // `lastSubmittedTime` is the edit-aware timestamp; fall back to create.
        submitted_at: {
            let last = str_field(resp, "lastSubmittedTime");
            if last.is_empty() {
                str_field(resp, "createTime")
            } else {
                last
            }
        },
        respondent_email: str_field(resp, "respondentEmail"),
        answers,
    }
}

pub(super) fn parse_task(t: &Value) -> TaskEntry {
    TaskEntry {
        id: str_field(t, "id"),
        title: str_field(t, "title"),
        notes: str_field(t, "notes"),
        status: str_field(t, "status"),
        due: str_field(t, "due"),
        completed: str_field(t, "completed"),
        parent: str_field(t, "parent"),
    }
}

pub(super) fn extract_spreadsheet_id(input: &str) -> String {
    let s = input.trim();
    if let Some(after) = s.split("/d/").nth(1) {
        // Take up to the next path separator or fragment/query.
        let id: String = after
            .chars()
            .take_while(|c| *c != '/' && *c != '#' && *c != '?')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    s.to_string()
}

pub(super) fn encode_path_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

pub(super) fn cell_to_string(cell: &Value) -> String {
    match cell {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(super) fn encode_header_word(s: &str) -> String {
    if s.is_ascii() && !s.chars().any(|c| c.is_control()) {
        s.to_string()
    } else {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        format!("=?UTF-8?B?{b64}?=")
    }
}

pub(super) fn build_rfc2822(to: &str, subject: &str, body: &str, cc: Option<&str>) -> String {
    let mut msg = String::new();
    msg.push_str(&format!("To: {to}\r\n"));
    if let Some(cc) = cc {
        if !cc.is_empty() {
            msg.push_str(&format!("Cc: {cc}\r\n"));
        }
    }
    msg.push_str(&format!("Subject: {}\r\n", encode_header_word(subject)));
    msg.push_str("MIME-Version: 1.0\r\n");
    msg.push_str("Content-Type: text/plain; charset=\"UTF-8\"\r\n");
    msg.push_str("Content-Transfer-Encoding: 8bit\r\n");
    msg.push_str("\r\n");
    msg.push_str(body);
    msg
}

pub(super) fn encode_raw_message(raw: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

pub(super) fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE
        .decode(s)
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .ok()
        })
}

pub(super) fn header_value(headers: &Value, name: &str) -> Option<String> {
    headers.as_array()?.iter().find_map(|h| {
        let hn = h.get("name").and_then(|n| n.as_str())?;
        if hn.eq_ignore_ascii_case(name) {
            h.get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
    })
}

pub(super) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

pub(super) fn extract_message_body(payload: &Value) -> String {
    if let Some(t) = find_part_text(payload, "text/plain") {
        return t;
    }
    if let Some(h) = find_part_text(payload, "text/html") {
        return html_to_text(&h);
    }
    String::new()
}

pub(super) fn find_part_text(node: &Value, want_mime: &str) -> Option<String> {
    let mime = node.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
    if mime == want_mime {
        if let Some(data) = node
            .get("body")
            .and_then(|b| b.get("data"))
            .and_then(|d| d.as_str())
        {
            if let Some(bytes) = decode_b64url(data) {
                return Some(String::from_utf8_lossy(&bytes).to_string());
            }
        }
    }
    if let Some(parts) = node.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            if let Some(t) = find_part_text(p, want_mime) {
                return Some(t);
            }
        }
    }
    None
}

pub(super) fn collect_attachments(payload: &Value) -> Vec<GmailAttachment> {
    let mut out = Vec::new();
    collect_attachments_rec(payload, &mut out);
    out
}

pub(super) fn collect_attachments_rec(node: &Value, out: &mut Vec<GmailAttachment>) {
    let filename = node.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    if !filename.is_empty() {
        let size = node
            .get("body")
            .and_then(|b| b.get("size"))
            .and_then(|s| s.as_u64())
            .unwrap_or(0);
        out.push(GmailAttachment {
            filename: filename.to_string(),
            size,
        });
    }
    if let Some(parts) = node.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            collect_attachments_rec(p, out);
        }
    }
}

pub(super) fn event_time(node: &Value, key: &str) -> String {
    node.get(key)
        .and_then(|s| s.get("dateTime").or_else(|| s.get("date")))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn extract_meet_link(event: &Value) -> Option<String> {
    if let Some(link) = event.get("hangoutLink").and_then(|v| v.as_str()) {
        if !link.is_empty() {
            return Some(link.to_string());
        }
    }
    event
        .get("conferenceData")
        .and_then(|c| c.get("entryPoints"))
        .and_then(|e| e.as_array())
        .and_then(|eps| {
            eps.iter().find_map(|ep| {
                let kind = ep
                    .get("entryPointType")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if kind == "video" {
                    ep.get("uri")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
}

pub(super) fn parse_calendar_event(item: &Value) -> CalendarEvent {
    CalendarEvent {
        id: item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        summary: item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no title)")
            .to_string(),
        start: event_time(item, "start"),
        end: event_time(item, "end"),
        location: item
            .get("location")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        html_link: item
            .get("htmlLink")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        meet_link: extract_meet_link(item),
        attendees_count: item
            .get("attendees")
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
    }
}

pub(super) fn extract_api_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| body.to_string())
}

pub(super) fn scope_guidance(api_error_body: &str) -> String {
    let (reason, enable_url) = crate::google_workspace::permission_failure(api_error_body);
    match reason.as_str() {
        "SERVICE_DISABLED" | "accessNotConfigured" => match enable_url {
            Some(url) => format!("Google API is disabled ({reason}). Enable this API: {url}"),
            None => format!("Google API is disabled ({reason}). Enable it in Google Cloud Console > APIs & services > Library."),
        },
        "ACCESS_TOKEN_SCOPE_INSUFFICIENT" | "insufficientPermissions" | "insufficient_scope" => format!(
            "Google denied the request (403): insufficient authentication scopes. Reconnect Google from the dashboard Integrations → Google page to grant: {}.",
            REQUIRED_SCOPES.join(", ")
        ),
        _ => format!("Google denied the request (403): {reason}."),
    }
}

pub(super) enum Outcome {
    GmailSearch(GmailSearchResult),
    GmailRead(GmailReadResult),
    GmailDraft(GmailDraftResult),
    CalendarList(CalendarEventsResult),
    CalendarCreate(CalendarCreateResult),
    SheetsRead(SheetsReadResult),
    SheetsAppend(SheetsAppendResult),
    FormsGet(FormResult),
    FormsResponses(FormResponsesResult),
    TasksLists(TaskListsResult),
    TasksList(TasksResult),
    TasksCreate(TaskEntry),
    TasksComplete(TaskEntry),
    DriveSearch(DriveSearchResult),
    DriveRead(DriveReadResult),
    DocsRead(DocsReadResult),
    DocsAppend(DocsAppendResult),
    SlidesRead(SlidesReadResult),
}

fn tool_text(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}]})
}
fn tool_error(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":true})
}
pub(super) fn render<I: LocalRender>(value: Outcome, io: &mut I) -> Value {
    match value {
        Outcome::GmailSearch(r) => render_json(&r, io),
        Outcome::GmailRead(r) => render_json(&r, io),
        Outcome::CalendarList(r) => render_json(&r, io),
        Outcome::SheetsRead(r) => render_json(&r, io),
        Outcome::FormsGet(r) => render_json(&r, io),
        Outcome::FormsResponses(r) => render_json(&r, io),
        Outcome::TasksLists(r) => render_json(&r, io),
        Outcome::TasksList(r) => render_json(&r, io),
        Outcome::DriveSearch(r) => render_json(&r, io),
        Outcome::DriveRead(r) => render_json(&r, io),
        Outcome::DocsRead(r) => render_json(&r, io),
        Outcome::SlidesRead(r) => render_json(&r, io),
        Outcome::GmailDraft(r) => tool_text(&format!(
            "Draft created — NOT sent. Review and send it manually in Gmail.\nDraft ID: {}\nTo: {}\nSubject: {}",
            r.draft_id, r.to, r.subject
        )),
        Outcome::CalendarCreate(r) => {
            let meet = r
                .meet_link
                .as_deref()
                .map(|l| format!("\nGoogle Meet: {l}"))
                .unwrap_or_default();
            tool_text(&format!(
                "Calendar event created (externally visible; attendees notified).\nEvent ID: {}\nSummary: {}\nStart: {}\nEnd: {}\nLink: {}{}",
                r.id, r.summary, r.start, r.end, r.html_link, meet
            ))
        }
        Outcome::SheetsAppend(r) => tool_text(&format!(
            "Appended 1 row to the sheet.\nUpdated range: {}\nUpdated rows: {}\nUpdated cells: {}",
            r.updated_range, r.updated_rows, r.updated_cells
        )),
        Outcome::TasksCreate(t) => tool_text(&format!(
            "Task created in the user's Google Tasks.\nTask ID: {}\nTitle: {}\nStatus: {}\nDue: {}",
            t.id,
            t.title,
            t.status,
            if t.due.is_empty() { "(none)" } else { &t.due }
        )),
        Outcome::TasksComplete(t) => tool_text(&format!(
            "Task marked completed.\nTask ID: {}\nTitle: {}\nStatus: {}",
            t.id, t.title, t.status
        )),
        Outcome::DocsAppend(r) => tool_text(&format!(
            "Appended to the Google Doc.\nDocument ID: {}\nCharacters appended: {}",
            r.document_id, r.appended_chars
        )),
    }
}
pub(super) async fn run<I: LocalIo>(template: &NativeCallTemplate, io: &mut I) -> Value {
    if let Err(e) = io.checkpoint() {
        return tool_error(&e.to_string());
    }
    let result: Result<Outcome, GoogleApiError> = match template.parsed() {
        ParsedNativeArguments::GmailSearch { query, max } => {
            gmail_search(io, template, query, *max)
                .await
                .map(Outcome::GmailSearch)
        }
        ParsedNativeArguments::GmailRead { message_id } => gmail_read(io, template, message_id)
            .await
            .map(Outcome::GmailRead),
        ParsedNativeArguments::GmailDraft {
            to,
            subject,
            body,
            cc,
        } => gmail_create_draft(io, template, to, subject, body, cc.as_deref())
            .await
            .map(Outcome::GmailDraft),
        ParsedNativeArguments::CalendarList { min, max_time, max } => {
            calendar_list_events(io, template, min.as_deref(), max_time.as_deref(), *max)
                .await
                .map(Outcome::CalendarList)
        }
        ParsedNativeArguments::CalendarCreate {
            summary,
            start,
            end,
            description,
            attendees,
            with_meet,
        } => calendar_create_event(
            io,
            template,
            summary,
            start,
            end,
            description.as_deref(),
            attendees.as_deref(),
            *with_meet,
        )
        .await
        .map(Outcome::CalendarCreate),
        ParsedNativeArguments::SheetsRead { spreadsheet, range } => {
            sheets_read(io, template, spreadsheet, range)
                .await
                .map(Outcome::SheetsRead)
        }
        ParsedNativeArguments::SheetsAppend {
            spreadsheet,
            range,
            values,
        } => sheets_append(io, template, spreadsheet, range, values.clone())
            .await
            .map(Outcome::SheetsAppend),
        ParsedNativeArguments::FormsGet { form } => {
            forms_get(io, template, form).await.map(Outcome::FormsGet)
        }
        ParsedNativeArguments::FormsResponses { form } => forms_list_responses(io, template, form)
            .await
            .map(Outcome::FormsResponses),
        ParsedNativeArguments::TasksLists => tasks_list_tasklists(io, template)
            .await
            .map(Outcome::TasksLists),
        ParsedNativeArguments::TasksList {
            list_id,
            show_completed,
            max,
        } => tasks_list(io, template, list_id, *show_completed, *max)
            .await
            .map(Outcome::TasksList),
        ParsedNativeArguments::TasksCreate {
            list_id,
            title,
            notes,
            due,
        } => tasks_create(
            io,
            template,
            list_id,
            title,
            notes.as_deref(),
            due.as_deref(),
        )
        .await
        .map(Outcome::TasksCreate),
        ParsedNativeArguments::TasksComplete { list_id, task_id } => {
            tasks_complete(io, template, list_id, task_id)
                .await
                .map(Outcome::TasksComplete)
        }
        ParsedNativeArguments::DriveSearch { query, mime, max } => {
            drive_search(io, template, query, mime.as_deref(), *max)
                .await
                .map(Outcome::DriveSearch)
        }
        ParsedNativeArguments::DriveRead { file } => {
            drive_read(io, template, file).await.map(Outcome::DriveRead)
        }
        ParsedNativeArguments::DocsRead { document } => docs_read(io, template, document)
            .await
            .map(Outcome::DocsRead),
        ParsedNativeArguments::DocsAppend { document, text } => {
            docs_append(io, template, document, text)
                .await
                .map(Outcome::DocsAppend)
        }
        ParsedNativeArguments::SlidesRead { presentation } => {
            slides_read(io, template, presentation)
                .await
                .map(Outcome::SlidesRead)
        }
    };
    if let Err(e) = io.checkpoint() {
        return tool_error(&e.to_string());
    }
    match result {
        Ok(value) => render(value, io),
        Err(e) => tool_error(&e.to_string()),
    }
}
// Every representation transform stays in this owner-local instance.
pub(super) fn contains_representation(value: &str, secret: &str) -> bool {
    if secret.is_empty() {
        return true;
    }
    if value.contains(secret) || html_to_text(value).contains(secret) {
        return true;
    }
    if decode_b64url(value).is_some_and(|b| String::from_utf8_lossy(&b).contains(secret)) {
        return true;
    }
    let mut bytes = Vec::with_capacity(value.len());
    let source = value.as_bytes();
    let mut i = 0;
    while i < source.len() {
        if source[i] == b'%' && i + 2 < source.len() {
            if let (Some(a), Some(b)) = (
                (source[i + 1] as char).to_digit(16),
                (source[i + 2] as char).to_digit(16),
            ) {
                bytes.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(source[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).contains(secret)
}
pub(super) fn scrub_final(value: &mut Value, secret: &str) -> bool {
    match value {
        Value::String(s) => {
            *s = s.replace(secret, "•••");
            true
        }
        Value::Array(a) => a.iter_mut().all(|v| scrub_final(v, secret)),
        Value::Object(o) => {
            if o.keys().any(|k| k.contains(secret)) {
                return false;
            }
            o.values_mut().all(|v| scrub_final(v, secret))
        }
        _ => true,
    }
}

pub(super) fn contains_value_representation(value: &Value, secret: &str) -> bool {
    match value {
        Value::String(s) => contains_representation(s, secret),
        Value::Array(a) => a.iter().any(|v| contains_value_representation(v, secret)),
        Value::Object(o) => o.iter().any(|(k, v)| {
            contains_representation(k, secret) || contains_value_representation(v, secret)
        }),
        _ => false,
    }
}

#[cfg(test)]
mod source_goldens {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_workspace/native_program_tests.inc.rs"
    ));
}

fn render_json<T: serde::Serialize, I: LocalRender>(value: &T, io: &mut I) -> Value {
    let projected = match serde_json::to_value(value) {
        Ok(value) => value,
        Err(_) => return tool_error(&io.refuse().to_string()),
    };
    if let Err(error) = io.inspect_curated(&projected) {
        return tool_error(&error.to_string());
    }
    // Preserve the original struct serialization order for legacy callers.
    tool_text(&serde_json::to_string_pretty(value).unwrap_or_default())
}
pub(super) fn key_contains_secret(value: &Value, secret: &str) -> bool {
    match value {
        Value::Object(o) => o
            .iter()
            .any(|(k, v)| k.contains(secret) || key_contains_secret(v, secret)),
        Value::Array(a) => a.iter().any(|v| key_contains_secret(v, secret)),
        _ => false,
    }
}

// A source-first oracle submits a recipe from another immutable template into
// an existing owner transport; the production pointer gate must refuse it.
#[cfg(test)]
pub(super) async fn submit_foreign_fixture_step<I: LocalIo>(
    io: &mut I,
    other: &NativeCallTemplate,
) -> Result<Value, GoogleApiError> {
    google_request(
        io,
        other,
        reqwest::Method::GET,
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/abc",
        &[],
        None,
    )
    .await
}
