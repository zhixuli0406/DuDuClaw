//! Original token-taking APIs retain their transport and Bridge contract.
use super::*;
use super::native_call::{NativeCallTemplate, ParsedNativeArguments};
mod program {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_workspace/native_program.inc.rs"
    ));
}
struct LegacyIo<'a> {
    token: &'a str,
}
#[async_trait::async_trait]
impl program::LocalIo for LegacyIo<'_> {
    async fn json(&mut self, s: program::NativeStep<'_>) -> Result<Value, GoogleApiError> {
        let q: Vec<_> = s
            .query()
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        super::google_request(self.token, s.method().clone(), s.url(), &q, s.body()).await
    }
    async fn text(&mut self, s: program::NativeStep<'_>) -> Result<String, GoogleApiError> {
        let q: Vec<_> = s
            .query()
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        super::google_request_text(self.token, s.method().clone(), s.url(), &q).await
    }
    fn checkpoint(&mut self) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn reject_derived(&mut self, _: &str) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn sanitize(&self, s: &str) -> String {
        s.to_owned()
    }
    fn closed(&self) -> bool {
        false
    }
}
pub(super) async fn gmail_search(
    token: &str,
    query: &str,
    max_results: u32,
) -> Result<GmailSearchResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gmail_search");
    let mut io = LegacyIo { token };
    program::gmail_search(&mut io, &template, query, max_results).await
}
pub(super) async fn gmail_read(
    token: &str,
    message_id: &str,
) -> Result<GmailReadResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gmail_read");
    let mut io = LegacyIo { token };
    program::gmail_read(&mut io, &template, message_id).await
}
pub(super) async fn gmail_create_draft(
    token: &str,
    to: &str,
    subject: &str,
    body_text: &str,
    cc: Option<&str>,
) -> Result<GmailDraftResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gmail_create_draft");
    let mut io = LegacyIo { token };
    program::gmail_create_draft(&mut io, &template, to, subject, body_text, cc).await
}
pub(super) async fn calendar_list_events(
    token: &str,
    time_min: Option<&str>,
    time_max: Option<&str>,
    max_results: u32,
) -> Result<CalendarEventsResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("calendar_list_events");
    let mut io = LegacyIo { token };
    program::calendar_list_events(&mut io, &template, time_min, time_max, max_results).await
}
pub(super) async fn calendar_create_event(
    token: &str,
    summary: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    description: Option<&str>,
    attendees: Option<&[String]>,
    with_meet: bool,
) -> Result<CalendarCreateResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("calendar_create_event");
    let mut io = LegacyIo { token };
    program::calendar_create_event(
        &mut io,
        &template,
        summary,
        start_rfc3339,
        end_rfc3339,
        description,
        attendees,
        with_meet,
    )
    .await
}
pub(super) async fn sheets_read(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
) -> Result<SheetsReadResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("sheets_read");
    let mut io = LegacyIo { token };
    program::sheets_read(&mut io, &template, spreadsheet_id, range).await
}
pub(super) async fn sheets_append(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<String>,
) -> Result<SheetsAppendResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("sheets_append");
    let mut io = LegacyIo { token };
    program::sheets_append(&mut io, &template, spreadsheet_id, range, values).await
}
pub(super) async fn forms_get(token: &str, form_id: &str) -> Result<FormResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("forms_get");
    let mut io = LegacyIo { token };
    program::forms_get(&mut io, &template, form_id).await
}
pub(super) async fn forms_list_responses(
    token: &str,
    form_id: &str,
) -> Result<FormResponsesResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("forms_list_responses");
    let mut io = LegacyIo { token };
    program::forms_list_responses(&mut io, &template, form_id).await
}
pub(super) async fn tasks_list_tasklists(token: &str) -> Result<TaskListsResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gtasks_lists");
    let mut io = LegacyIo { token };
    program::tasks_list_tasklists(&mut io, &template).await
}
pub(super) async fn tasks_list(
    token: &str,
    task_list_id: &str,
    show_completed: bool,
    max_results: u32,
) -> Result<TasksResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gtasks_list");
    let mut io = LegacyIo { token };
    program::tasks_list(
        &mut io,
        &template,
        task_list_id,
        show_completed,
        max_results,
    )
    .await
}
pub(super) async fn tasks_create(
    token: &str,
    task_list_id: &str,
    title: &str,
    notes: Option<&str>,
    due: Option<&str>,
) -> Result<TaskEntry, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gtasks_create");
    let mut io = LegacyIo { token };
    program::tasks_create(&mut io, &template, task_list_id, title, notes, due).await
}
pub(super) async fn tasks_complete(
    token: &str,
    task_list_id: &str,
    task_id: &str,
) -> Result<TaskEntry, GoogleApiError> {
    let template = NativeCallTemplate::for_api("gtasks_complete");
    let mut io = LegacyIo { token };
    program::tasks_complete(&mut io, &template, task_list_id, task_id).await
}
pub(super) async fn drive_search(
    token: &str,
    query: &str,
    mime_type: Option<&str>,
    max_results: u32,
) -> Result<DriveSearchResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("drive_search");
    let mut io = LegacyIo { token };
    program::drive_search(&mut io, &template, query, mime_type, max_results).await
}
pub(super) async fn drive_read(
    token: &str,
    file_id: &str,
) -> Result<DriveReadResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("drive_read");
    let mut io = LegacyIo { token };
    program::drive_read(&mut io, &template, file_id).await
}
pub(super) async fn docs_read(
    token: &str,
    document_id: &str,
) -> Result<DocsReadResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("docs_read");
    let mut io = LegacyIo { token };
    program::docs_read(&mut io, &template, document_id).await
}
pub(super) async fn docs_append(
    token: &str,
    document_id: &str,
    text: &str,
) -> Result<DocsAppendResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("docs_append");
    let mut io = LegacyIo { token };
    program::docs_append(&mut io, &template, document_id, text).await
}
pub(super) async fn slides_read(
    token: &str,
    presentation_id: &str,
) -> Result<SlidesReadResult, GoogleApiError> {
    let template = NativeCallTemplate::for_api("slides_read");
    let mut io = LegacyIo { token };
    program::slides_read(&mut io, &template, presentation_id).await
}
pub(super) fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    program::clamp(n, lo, hi)
}
pub(super) fn truncate_text(s: &str, max_chars: usize) -> (String, bool) {
    program::truncate_text(s, max_chars)
}
pub(super) fn escape_drive_query_value(s: &str) -> String {
    program::escape_drive_query_value(s)
}
pub(super) fn export_mime_for(mime: &str) -> Option<&'static str> {
    program::export_mime_for(mime)
}
pub(super) fn is_text_like_mime(mime: &str) -> bool {
    program::is_text_like_mime(mime)
}
pub(super) fn extract_drive_file_id(input: &str) -> String {
    program::extract_drive_file_id(input)
}
pub(super) fn collect_doc_text(content: &Value, out: &mut String) {
    program::collect_doc_text(content, out)
}
pub(super) fn collect_slide_text(page_elements: &Value, out: &mut String) {
    program::collect_slide_text(page_elements, out)
}
pub(super) fn append_slide_text_elements(text: &Value, out: &mut String) {
    program::append_slide_text_elements(text, out)
}
pub(super) fn str_field(v: &Value, key: &str) -> String {
    program::str_field(v, key)
}
pub(super) fn extract_form_id(input: &str) -> String {
    program::extract_form_id(input)
}
pub(super) fn parse_form_item(item: &Value) -> Option<FormQuestion> {
    program::parse_form_item(item)
}
pub(super) fn parse_form_response(resp: &Value) -> FormResponseEntry {
    program::parse_form_response(resp)
}
pub(super) fn parse_task(t: &Value) -> TaskEntry {
    program::parse_task(t)
}
pub(super) fn extract_spreadsheet_id(input: &str) -> String {
    program::extract_spreadsheet_id(input)
}
pub(super) fn encode_path_component(s: &str) -> String {
    program::encode_path_component(s)
}
pub(super) fn cell_to_string(cell: &Value) -> String {
    program::cell_to_string(cell)
}
pub(super) fn encode_header_word(s: &str) -> String {
    program::encode_header_word(s)
}
pub(super) fn build_rfc2822(to: &str, subject: &str, body: &str, cc: Option<&str>) -> String {
    program::build_rfc2822(to, subject, body, cc)
}
pub(super) fn encode_raw_message(raw: &str) -> String {
    program::encode_raw_message(raw)
}
pub(super) fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    program::decode_b64url(s)
}
pub(super) fn header_value(headers: &Value, name: &str) -> Option<String> {
    program::header_value(headers, name)
}
pub(super) fn html_to_text(html: &str) -> String {
    program::html_to_text(html)
}
pub(super) fn extract_message_body(payload: &Value) -> String {
    program::extract_message_body(payload)
}
pub(super) fn find_part_text(node: &Value, want_mime: &str) -> Option<String> {
    program::find_part_text(node, want_mime)
}
pub(super) fn collect_attachments(payload: &Value) -> Vec<GmailAttachment> {
    program::collect_attachments(payload)
}
pub(super) fn collect_attachments_rec(node: &Value, out: &mut Vec<GmailAttachment>) {
    program::collect_attachments_rec(node, out)
}
pub(super) fn event_time(node: &Value, key: &str) -> String {
    program::event_time(node, key)
}
pub(super) fn extract_meet_link(event: &Value) -> Option<String> {
    program::extract_meet_link(event)
}
pub(super) fn parse_calendar_event(item: &Value) -> CalendarEvent {
    program::parse_calendar_event(item)
}
pub(super) fn extract_api_message(body: &str) -> String {
    program::extract_api_message(body)
}
pub(super) fn scope_guidance(api_error_body: &str) -> String {
    program::scope_guidance(api_error_body)
}

pub(crate) async fn handle(
    tool: &str,
    args: &Value,
    home: &Path,
    g: &dyn crate::google_identity::native_tools::NativeGoogleAccess,
) -> Value {
    let template = match NativeCallTemplate::prepare(tool, args) {
        Ok(t) => t,
        Err(e) => {
            crate::egress_client::builtin_not_dispatched();
            return super::native_call::tool_error(&e);
        }
    };
    let result: Result<program::Outcome, GoogleApiError> = match template.parsed() {
        ParsedNativeArguments::GmailSearch { query, max } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::gmail_search_via(&backend, query, *max)
                .await
                .map(program::Outcome::GmailSearch)
        }
        ParsedNativeArguments::GmailRead { message_id } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::gmail_read_via(&backend, message_id)
                .await
                .map(program::Outcome::GmailRead)
        }
        ParsedNativeArguments::GmailDraft {
            to,
            subject,
            body,
            cc,
        } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::gmail_create_draft_via(&backend, to, subject, body, cc.as_deref())
                .await
                .map(program::Outcome::GmailDraft)
        }
        ParsedNativeArguments::CalendarList { min, max_time, max } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::calendar_list_events_via(&backend, min.as_deref(), max_time.as_deref(), *max)
                .await
                .map(program::Outcome::CalendarList)
        }
        ParsedNativeArguments::CalendarCreate {
            summary,
            start,
            end,
            description,
            attendees,
            with_meet,
        } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::calendar_create_event_via(
                &backend,
                summary,
                start,
                end,
                description.as_deref(),
                attendees.as_deref(),
                *with_meet,
            )
            .await
            .map(program::Outcome::CalendarCreate)
        }
        ParsedNativeArguments::SheetsRead { spreadsheet, range } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::sheets_read_via(&backend, spreadsheet, range)
                .await
                .map(program::Outcome::SheetsRead)
        }
        ParsedNativeArguments::SheetsAppend {
            spreadsheet,
            range,
            values,
        } => {
            let backend = match g.backend(home, tool).await {
                Ok(b) => b,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::sheets_append_via(&backend, spreadsheet, range, values.clone())
                .await
                .map(program::Outcome::SheetsAppend)
        }
        ParsedNativeArguments::FormsGet { form } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::forms_get(&token, form)
                .await
                .map(program::Outcome::FormsGet)
        }
        ParsedNativeArguments::FormsResponses { form } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::forms_list_responses(&token, form)
                .await
                .map(program::Outcome::FormsResponses)
        }
        ParsedNativeArguments::TasksLists => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::tasks_list_tasklists(&token)
                .await
                .map(program::Outcome::TasksLists)
        }
        ParsedNativeArguments::TasksList {
            list_id,
            show_completed,
            max,
        } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::tasks_list(&token, list_id, *show_completed, *max)
                .await
                .map(program::Outcome::TasksList)
        }
        ParsedNativeArguments::TasksCreate {
            list_id,
            title,
            notes,
            due,
        } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::tasks_create(&token, list_id, title, notes.as_deref(), due.as_deref())
                .await
                .map(program::Outcome::TasksCreate)
        }
        ParsedNativeArguments::TasksComplete { list_id, task_id } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::tasks_complete(&token, list_id, task_id)
                .await
                .map(program::Outcome::TasksComplete)
        }
        ParsedNativeArguments::DriveSearch { query, mime, max } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::drive_search(&token, query, mime.as_deref(), *max)
                .await
                .map(program::Outcome::DriveSearch)
        }
        ParsedNativeArguments::DriveRead { file } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::drive_read(&token, file)
                .await
                .map(program::Outcome::DriveRead)
        }
        ParsedNativeArguments::DocsRead { document } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::docs_read(&token, document)
                .await
                .map(program::Outcome::DocsRead)
        }
        ParsedNativeArguments::DocsAppend { document, text } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::docs_append(&token, document, text)
                .await
                .map(program::Outcome::DocsAppend)
        }
        ParsedNativeArguments::SlidesRead { presentation } => {
            let token = match g.token(home, tool).await {
                Ok(t) => t,
                Err(e) => return super::native_call::tool_error(&e),
            };
            super::slides_read(&token, presentation)
                .await
                .map(program::Outcome::SlidesRead)
        }
    };
    match result {
        Ok(value) => {
            let mut renderer = LegacyRenderer;
            program::render(value, &mut renderer)
        }
        Err(e) => super::native_call::tool_error(&e.to_string()),
    }
}
impl program::LocalRender for LegacyIo<'_> {
    fn inspect_curated(&mut self, _: &Value) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn refuse(&mut self) -> GoogleApiError {
        GoogleApiError::Api {
            status: 502,
            message: "Invalid native response".into(),
        }
    }
}

// Rendering has no token, request, or transport capability.
struct LegacyRenderer;
impl program::LocalRender for LegacyRenderer {
    fn inspect_curated(&mut self, _: &Value) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn refuse(&mut self) -> GoogleApiError {
        GoogleApiError::Api {
            status: 502,
            message: "Invalid native result".into(),
        }
    }
}

// Host-only recipe preflight. It owns no credential/client and intentionally
// stops on the first request before supplying any provider response.
struct FirstRecipePreflight {
    seen: bool,
    valid: bool,
}
impl FirstRecipePreflight {
    fn record(&mut self, step: program::NativeStep<'_>) -> GoogleApiError {
        self.seen = true;
        self.valid = match reqwest::Url::parse(step.url()) {
            Ok(mut url) => {
                if !step.query().is_empty() {
                    url.query_pairs_mut()
                        .extend_pairs(step.query().iter().map(|(k, v)| (k, v)));
                }
                let body = step.body().map(serde_json::to_vec).transpose();
                body.is_ok_and(|b| {
                    url.as_str()
                        .len()
                        .checked_add(b.map_or(0, |v| v.len()))
                        .is_some_and(|n| n <= 1024 * 1024)
                })
            }
            Err(_) => false,
        };
        GoogleApiError::Api {
            status: 400,
            message: "Host recipe preflight stopped.".into(),
        }
    }
}
#[async_trait::async_trait]
impl program::LocalIo for FirstRecipePreflight {
    async fn json(&mut self, step: program::NativeStep<'_>) -> Result<Value, GoogleApiError> {
        Err(self.record(step))
    }
    async fn text(&mut self, step: program::NativeStep<'_>) -> Result<String, GoogleApiError> {
        Err(self.record(step))
    }
    fn checkpoint(&mut self) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn reject_derived(&mut self, _: &str) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn sanitize(&self, value: &str) -> String {
        value.into()
    }
    fn closed(&self) -> bool {
        false
    }
}
impl program::LocalRender for FirstRecipePreflight {
    fn inspect_curated(&mut self, _: &Value) -> Result<(), GoogleApiError> {
        Ok(())
    }
    fn refuse(&mut self) -> GoogleApiError {
        GoogleApiError::Api {
            status: 400,
            message: "Host recipe preflight stopped.".into(),
        }
    }
}
pub(crate) async fn first_recipe_fits(template: &NativeCallTemplate) -> bool {
    let mut io = FirstRecipePreflight {
        seen: false,
        valid: false,
    };
    let _ = program::run(template, &mut io).await;
    io.seen && io.valid
}
