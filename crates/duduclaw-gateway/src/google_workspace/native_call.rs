//! Immutable host inputs only. Provider replies never enter this module.
use serde_json::{Value, json};

pub(crate) enum ParsedNativeArguments {
    GmailSend { draft: String },
    CalendarDelete { calendar: String, event: String, notify: bool },
    DocsCreate { title: String, body: Option<String> },
    DocsReplace { document: String, find: String, replace: String, match_case: bool },
    SheetsCreate { title: String, sheet_names: Option<Vec<String>> },
    SheetsUpdate { spreadsheet: String, range: String, values: Vec<Vec<Value>>, input_option: String },
    CalendarUpdate { calendar: String, event: String, patch: Value, notify: bool },
    ChatSpaces(super::chat_read::SpacesArgs),
    ChatMessages(super::chat_read::MessagesArgs),
    GmailSearch {
        query: String,
        max: u32,
    },
    GmailRead {
        message_id: String,
    },
    GmailDraft {
        to: String,
        subject: String,
        body: String,
        cc: Option<String>,
        thread_id: Option<String>,
        in_reply_to: Option<String>,
        references: Option<String>,
    },
    CalendarList {
        min: Option<String>,
        max_time: Option<String>,
        max: u32,
    },
    CalendarCreate {
        summary: String,
        start: String,
        end: String,
        description: Option<String>,
        attendees: Option<Vec<String>>,
        with_meet: bool,
    },
    SheetsRead {
        spreadsheet: String,
        range: String,
    },
    SheetsAppend {
        spreadsheet: String,
        range: String,
        values: Vec<String>,
    },
    FormsGet {
        form: String,
    },
    FormsResponses {
        form: String,
    },
    TasksLists,
    TasksList {
        list_id: String,
        show_completed: bool,
        max: u32,
    },
    TasksCreate {
        list_id: String,
        title: String,
        notes: Option<String>,
        due: Option<String>,
    },
    TasksComplete {
        list_id: String,
        task_id: String,
    },
    DriveSearch {
        query: String,
        mime: Option<String>,
        max: u32,
    },
    DriveRead {
        file: String,
    },
    DocsRead {
        document: String,
    },
    DocsAppend {
        document: String,
        text: String,
    },
    SlidesRead {
        presentation: String,
    },
}

pub(crate) struct NativeCallTemplate {
    confirmation: Option<super::native_confirmation::HostContext>,
    tool: String,
    digest: String,
    parsed: Option<ParsedNativeArguments>,
    calendar_min: String,
    calendar_max: String,
    meet_request_id: String,
}

fn required(args: &Value, key: &str, trim: bool, nonempty: bool) -> Result<String, String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !nonempty || !s.trim().is_empty())
        .ok_or_else(|| format!("Missing required parameter: {key}"))?;
    Ok(if trim { value.trim() } else { value }.to_owned())
}
fn optional(args: &Value, key: &str, trim: bool) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(|s| if trim { s.trim() } else { s })
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn number(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .or_else(|| {
            args.get(key)
                .and_then(Value::as_str)
                .and_then(|s| s.trim().parse::<u32>().ok())
        })
        .unwrap_or(default)
}
fn list(args: &Value) -> String {
    optional(args, "task_list_id", true).unwrap_or_else(|| "@default".into())
}
fn timestamp(label: &str, value: &str, example: &str) -> Result<(), String> {
    if !super::is_rfc3339(value) {
        let description = if matches!(label, "start" | "end") {
            "RFC-3339"
        } else {
            "a valid RFC-3339 timestamp"
        };
        return Err(format!(
            "Invalid {label}: '{value}' is not {description} (e.g. {example})"
        ));
    }
    Ok(())
}

fn capped(args: &Value, key: &str, nonempty: bool, max: usize) -> Result<String, String> {
    let value = required(args, key, false, nonempty)?;
    check_text_limit(key, &value, max)?;
    Ok(value)
}

fn check_text_limit(key: &str, value: &str, max: usize) -> Result<(), String> {
    if value.chars().count() > max {
        return Err(format!("Parameter '{key}' exceeds the limit of {max} characters"));
    }
    Ok(())
}

/// Validate raw input before policy, approval and argument-digest binding.
/// Never shorten an approved payload, including optional or nested strings.
pub(crate) fn validate_inputs(tool: &str, args: &Value) -> Result<(), String> {
    if matches!(tool, "gmail_send_draft" | "calendar_delete_event") {
        for key in if tool == "gmail_send_draft" { &["draft_id"][..] } else { &["event_id"][..] } {
            let id = required(args, key, true, true)?;
            if id.len() > 1024 || !safe_gmail_component(&id) { return Err("Invalid Google resource ID".into()); }
        }
        if tool == "calendar_delete_event" {
            boolean(args, "notify_attendees", false)?;
            if let Some(calendar) = checked_optional(args, "calendar_id", true, true)? {
                if calendar.len() > 1024 || !safe_gmail_component(&calendar) { return Err("Invalid calendar ID".into()); }
            }
        }
    }
    let limits: &[(&str, usize)] = match tool {
        "docs_create" => &[("title", 1000), ("body", 20_000)],
        "docs_replace_text" => &[("find", 20_000), ("replace", 20_000)],
        "sheets_create" => &[("title", 1000)],
        "sheets_update" => &[("range", 1000)],
        "calendar_update_event" => &[("summary", 20_000), ("description", 20_000), ("location", 20_000)],
        "gmail_create_draft" => &[("to", 1000), ("cc", 1000), ("subject", 1000), ("body", 20_000)],
        _ => &[],
    };
    for (key, max) in limits {
        if let Some(value) = args.get(*key).and_then(Value::as_str) {
            check_text_limit(key, value, *max)?;
        }
    }
    let array_limit = match tool {
        "sheets_create" => Some(("sheet_names", 100, 100)),
        "calendar_update_event" => Some(("attendees", 1000, 320)),
        _ => None,
    };
    if let Some((key, count, max)) = array_limit {
        if let Some(values) = args.get(key).and_then(Value::as_array) {
            if values.len() > count {
                return Err(format!("Invalid array parameter: {key} (max {count})"));
            }
            for value in values.iter().filter_map(Value::as_str) {
                check_text_limit(key, value, max)?;
            }
        }
    }
    if tool == "sheets_update" {
        if let Some(rows) = args.get("values").and_then(Value::as_array) {
            let mut cells = 0usize;
            for row in rows.iter().filter_map(Value::as_array) {
                cells = cells.checked_add(row.len()).filter(|n| *n <= 10_000)
                    .ok_or("values exceeds 10000 cells")?;
                for value in row.iter().filter_map(Value::as_str) {
                    check_text_limit("values cell", value, 20_000)?;
                }
            }
        }
    }
    if tool == "gmail_create_draft" {
        reply_headers(args)?;
        if let Some(to) = args.get("to").and_then(Value::as_str) {
            validate_draft_recipients(to, args.get("cc").and_then(Value::as_str))?;
        }
    }
    Ok(())
}

pub(crate) fn validate_draft_recipients(to: &str, cc: Option<&str>) -> Result<(), String> {
    for (key, value) in [("to", Some(to)), ("cc", cc)] {
        let Some(value) = value else { continue; };
        // Check before trimming: even a trailing CR/LF must be refused.
        if value.chars().any(char::is_control) {
            return Err(format!("Invalid {key}: control characters are not allowed in email addresses"));
        }
        if key == "cc" && value.trim().is_empty() { continue; }
        for address in value.split(',') {
            duduclaw_auth::db::validate_account_email(address)
                .map_err(|_| format!("Invalid email address in {key}"))?;
        }
    }
    Ok(())
}
fn checked_optional(args: &Value, key: &str, trim: bool, nonempty: bool) -> Result<Option<String>, String> {
    if args.get(key).is_none() { return Ok(None); }
    required(args, key, trim, nonempty).map(Some)
}
fn boolean(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(format!("Invalid boolean parameter: {key}")),
    }
}
fn string_array(args: &Value, key: &str, limit: usize, max_chars: usize) -> Result<Option<Vec<String>>, String> {
    let Some(value) = args.get(key) else { return Ok(None); };
    let rows = value.as_array().filter(|v| v.len() <= limit)
        .ok_or_else(|| format!("Invalid array parameter: {key} (max {limit})"))?;
    rows.iter().map(|value| {
        let s = value.as_str().filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("Invalid string in {key}"))?;
        check_text_limit(key, s, max_chars)?;
        Ok(s.to_owned())
    }).collect::<Result<Vec<_>, String>>().map(Some)
}
fn reply_headers(args: &Value) -> Result<(Option<String>, Option<String>, Option<String>), String> {
    let thread = checked_optional(args, "thread_id", true, true)?;
    let reply = checked_optional(args, "in_reply_to", true, true)?;
    let references = checked_optional(args, "references", true, true)?.or_else(|| reply.clone());
    if thread.is_some() != reply.is_some() || (thread.is_none() && references.is_some()) {
        return Err("Reply drafts require thread_id and in_reply_to (original Message-ID).".into());
    }
    let message_id = |s: &str| s.len() <= 998 && s.starts_with('<') && s.ends_with('>')
        && s.chars().all(|c| c.is_ascii_graphic() && c != '"');
    if thread.as_deref().is_some_and(|s| !safe_gmail_component(s))
        || reply.as_deref().is_some_and(|s| !message_id(s))
        || references.as_deref().is_some_and(|s| s.len() > 8000 || s.contains(['\r', '\n'])
            || !s.split_whitespace().all(message_id)) {
        return Err("Invalid Gmail reply identifiers or headers.".into());
    }
    Ok((thread, reply, references))
}

impl NativeCallTemplate {
    pub(crate) fn prepare(tool: &str, args: &Value) -> Result<Self, String> {
        validate_inputs(tool, args)?;
        let parsed = match tool {
            "gmail_send_draft" => ParsedNativeArguments::GmailSend {
                draft: required(args, "draft_id", true, true)?,
            },
            "calendar_delete_event" => ParsedNativeArguments::CalendarDelete {
                calendar: checked_optional(args, "calendar_id", true, true)?.unwrap_or_else(|| "primary".into()),
                event: required(args, "event_id", true, true)?,
                notify: boolean(args, "notify_attendees", false)?,
            },
            "docs_create" => ParsedNativeArguments::DocsCreate {
                title: capped(args, "title", true, 1000)?,
                body: checked_optional(args, "body", false, false)?,
            },
            "docs_replace_text" => ParsedNativeArguments::DocsReplace {
                document: required(args, "document_id", true, true)?,
                find: capped(args, "find", true, 20_000)?,
                replace: capped(args, "replace", false, 20_000)?,
                match_case: boolean(args, "match_case", true)?,
            },
            "sheets_create" => ParsedNativeArguments::SheetsCreate {
                title: capped(args, "title", true, 1000)?,
                sheet_names: string_array(args, "sheet_names", 100, 100)?,
            },
            "sheets_update" => {
                let rows = args.get("values").and_then(Value::as_array).filter(|r| !r.is_empty() && r.len() <= 10_000)
                    .ok_or("values must be a nonempty 2-D array (max 10000 cells)")?;
                let mut values = Vec::new();
                let mut cells = 0usize;
                for row in rows {
                    let row = row.as_array().filter(|r| !r.is_empty()).ok_or("Each values row must be a nonempty array")?;
                    cells = cells.checked_add(row.len()).filter(|n| *n <= 10_000).ok_or("values exceeds 10000 cells")?;
                    let mut parsed_row = Vec::new();
                    for cell in row {
                        parsed_row.push(match cell {
                            Value::String(_) => cell.clone(),
                            Value::Null | Value::Bool(_) | Value::Number(_) => cell.clone(),
                            _ => return Err("Cells must be strings, numbers, booleans or null".into()),
                        });
                    }
                    values.push(parsed_row);
                }
                let input_option = checked_optional(args, "value_input_option", true, true)?.unwrap_or_else(|| "RAW".into());
                if !matches!(input_option.as_str(), "RAW" | "USER_ENTERED") { return Err("value_input_option must be RAW or USER_ENTERED".into()); }
                ParsedNativeArguments::SheetsUpdate {
                    spreadsheet: required(args, "spreadsheet_id", true, true)?,
                    range: capped(args, "range", true, 1000)?, values, input_option,
                }
            },
            "calendar_update_event" => {
                let mut patch = json!({});
                for field in ["summary", "description", "location"] {
                    if args.get(field).is_some() { patch[field] = json!(capped(args, field, false, 20_000)?); }
                }
                for field in ["start", "end"] {
                    if args.get(field).is_some() {
                        let value = required(args, field, true, true)?;
                        timestamp(field, &value, "2026-11-24T10:00:00Z")?;
                        patch[field] = json!({"dateTime":value});
                    }
                }
                if let Some(attendees) = string_array(args, "attendees", 1000, 320)? {
                    if attendees.iter().any(|email| !email.contains('@') || email.chars().any(char::is_whitespace)) {
                        return Err("Invalid attendee email address".into());
                    }
                    patch["attendees"] = json!(attendees.iter().map(|email| json!({"email":email})).collect::<Vec<_>>());
                }
                if patch.as_object().is_none_or(|p| p.is_empty()) { return Err("Provide at least one event field to update".into()); }
                ParsedNativeArguments::CalendarUpdate {
                    calendar: checked_optional(args, "calendar_id", true, true)?.unwrap_or_else(|| "primary".into()),
                    event: required(args, "event_id", true, true)?, patch,
                    notify: boolean(args, "notify_attendees", false)?,
                }
            },
            "google_chat_spaces" => ParsedNativeArguments::ChatSpaces(super::chat_read::spaces_args(args)?),
            "google_chat_messages" => ParsedNativeArguments::ChatMessages(super::chat_read::messages_args(args)?),
            "gmail_search" => ParsedNativeArguments::GmailSearch {
                query: required(args, "query", false, true)?,
                max: number(args, "max_results", 10),
            },
            "gmail_read" => ParsedNativeArguments::GmailRead {
                message_id: required(args, "message_id", false, true)?,
            },
            "gmail_create_draft" => {
                let (thread_id, in_reply_to, references) = reply_headers(args)?;
                ParsedNativeArguments::GmailDraft {
                    to: capped(args, "to", true, 1000)?,
                    subject: capped(args, "subject", false, 1000)?,
                    body: capped(args, "body", false, 20_000)?,
                    cc: checked_optional(args, "cc", false, false)?,
                    thread_id, in_reply_to, references,
                }
            },
            "calendar_list_events" => {
                let min = optional(args, "time_min", true);
                let max_time = optional(args, "time_max", true);
                for (label, value) in [("time_min", &min), ("time_max", &max_time)] {
                    if let Some(value) = value {
                        timestamp(label, value, "2026-07-26T14:00:00+08:00")?;
                    }
                }
                ParsedNativeArguments::CalendarList {
                    min,
                    max_time,
                    max: number(args, "max_results", 20),
                }
            }
            "calendar_create_event" => {
                let summary = required(args, "summary", false, true)?;
                let start = required(args, "start", true, true)?;
                let end = required(args, "end", true, true)?;
                timestamp("start", &start, "2026-07-26T14:00:00+08:00")?;
                timestamp("end", &end, "2026-07-26T15:00:00+08:00")?;
                let attendees = args.get("attendees").and_then(Value::as_str).map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                });
                let with_meet = args
                    .get("with_meet")
                    .and_then(Value::as_bool)
                    .or_else(|| {
                        args.get("with_meet")
                            .and_then(Value::as_str)
                            .map(|s| s.eq_ignore_ascii_case("true"))
                    })
                    .unwrap_or(false);
                ParsedNativeArguments::CalendarCreate {
                    summary,
                    start,
                    end,
                    description: optional(args, "description", false),
                    attendees,
                    with_meet,
                }
            }
            "sheets_read" => ParsedNativeArguments::SheetsRead {
                spreadsheet: required(args, "spreadsheet_id", true, true)?,
                range: required(args, "range", true, true)?,
            },
            "sheets_append" => {
                let spreadsheet = required(args, "spreadsheet_id", true, true)?;
                let range = required(args, "range", true, true)?;
                let values:Vec<String>=match args.get("values") {
                    Some(Value::Array(a))=>a.iter().map(|v| match v {Value::String(s)=>s.clone(),Value::Null=>String::new(),other=>other.to_string()}).collect(),
                    Some(Value::String(s))=>s.split(',').map(|s|s.trim().to_owned()).collect(),
                    _=>return Err("Missing required parameter: values (JSON array of strings or comma-separated list)".into()),
                };
                if values.is_empty() {
                    return Err("Parameter 'values' must contain at least one cell".into());
                }
                ParsedNativeArguments::SheetsAppend {
                    spreadsheet,
                    range,
                    values,
                }
            }
            "forms_get" => ParsedNativeArguments::FormsGet {
                form: required(args, "form_id", false, true)?,
            },
            "forms_list_responses" => ParsedNativeArguments::FormsResponses {
                form: required(args, "form_id", false, true)?,
            },
            "gtasks_lists" => ParsedNativeArguments::TasksLists,
            "gtasks_list" => ParsedNativeArguments::TasksList {
                list_id: list(args),
                show_completed: args
                    .get("show_completed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                max: number(args, "max_results", 50),
            },
            "gtasks_create" => {
                let title = required(args, "title", false, true)?;
                let due = optional(args, "due", true);
                if let Some(due) = &due {
                    timestamp("due", due, "2026-08-01T00:00:00Z")?;
                }
                ParsedNativeArguments::TasksCreate {
                    list_id: list(args),
                    title,
                    notes: optional(args, "notes", true),
                    due,
                }
            }
            "gtasks_complete" => ParsedNativeArguments::TasksComplete {
                list_id: list(args),
                task_id: required(args, "task_id", true, true)?,
            },
            "drive_search" => ParsedNativeArguments::DriveSearch {
                query: required(args, "query", false, true)?,
                mime: optional(args, "mime_type", true),
                max: number(args, "max_results", 20),
            },
            "drive_read" => ParsedNativeArguments::DriveRead {
                file: required(args, "file_id", false, true)?,
            },
            "docs_read" => ParsedNativeArguments::DocsRead {
                document: required(args, "document_id", false, true)?,
            },
            "docs_append" => {
                let document = required(args, "document_id", false, true)?;
                let text = required(args, "text", false, false)?;
                if text.is_empty() {
                    return Err("Missing required parameter: text".into());
                }
                ParsedNativeArguments::DocsAppend { document, text }
            }
            "slides_read" => ParsedNativeArguments::SlidesRead {
                presentation: required(args, "presentation_id", false, true)?,
            },
            _ => return Err("Google 工具不存在。".into()),
        };
        let mut call = Self::for_api(tool);
        call.digest = duduclaw_core::google_identity::google_arguments_digest(args);
        call.parsed = Some(parsed);
        if matches!(tool, "gmail_send_draft" | "calendar_delete_event" | "docs_create" | "docs_replace_text" | "sheets_create" | "sheets_update" | "calendar_update_event") {
            call.validate_closed()?;
        }
        Ok(call)
    }
    // Legacy public API wrappers have typed arguments rather than an MCP DTO.
    pub(super) fn for_api(tool: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            confirmation: None,
            tool: tool.into(),
            digest: String::new(),
            parsed: None,
            calendar_min: now.to_rfc3339(),
            calendar_max: (now + chrono::Duration::days(7)).to_rfc3339(),
            meet_request_id: uuid::Uuid::new_v4().to_string(),
        }
    }
    pub(crate) fn bind_confirmation(&mut self, context: super::native_confirmation::HostContext) {
        self.confirmation = Some(context);
    }
    pub(crate) fn confirmation(&self) -> Result<&super::native_confirmation::HostContext, String> {
        self.confirmation.as_ref().ok_or_else(|| "這項操作需要 gateway 逐次確認，未送出任何變更。".into())
    }
    pub(crate) fn tool(&self) -> &str {
        &self.tool
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn parsed(&self) -> &ParsedNativeArguments {
        self.parsed
            .as_ref()
            .expect("host prepared native arguments")
    }
    pub(crate) fn time_window(&self) -> (&str, &str) {
        (&self.calendar_min, &self.calendar_max)
    }
    pub(crate) fn meet_request_id(&self) -> &str {
        &self.meet_request_id
    }
    pub(crate) fn validate_closed(&self) -> Result<(), String> {
        use ParsedNativeArguments::*;
        let safe = match self.parsed() {
            GmailDraft { thread_id, .. } => thread_id.as_deref().is_none_or(safe_gmail_component),
            CalendarUpdate { calendar, event, .. } => safe_gmail_component(calendar) && safe_gmail_component(event),
            GmailSend { draft } => safe_gmail_component(draft),
            CalendarDelete { calendar, event, .. } => safe_gmail_component(calendar) && safe_gmail_component(event),
            GmailRead { message_id } => safe_gmail_component(message_id),
            SheetsRead { spreadsheet, range }
            | SheetsUpdate { spreadsheet, range, .. }
            | SheetsAppend {
                spreadsheet, range, ..
            } => {
                safe_gmail_component(&super::extract_spreadsheet_id(spreadsheet))
                    && range != "."
                    && range != ".."
            }
            FormsGet { form } | FormsResponses { form } => {
                safe_gmail_component(&super::extract_form_id(form))
            }
            TasksList { list_id, .. } | TasksCreate { list_id, .. } => {
                safe_gmail_component(list_id)
            }
            TasksComplete { list_id, task_id } => {
                safe_gmail_component(list_id) && safe_gmail_component(task_id)
            }
            DriveRead { file } => safe_gmail_component(&super::extract_drive_file_id(file)),
            DocsRead { document } | DocsAppend { document, .. } | DocsReplace { document, .. } => {
                safe_gmail_component(&super::extract_drive_file_id(document))
            }
            SlidesRead { presentation } => {
                safe_gmail_component(&super::extract_drive_file_id(presentation))
            }
            _ => true,
        };
        if !safe {
            return Err("Google 資源識別碼格式不正確。".into());
        }
        Ok(())
    }
}

pub(crate) fn safe_gmail_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, '/' | '?' | '#' | '%' | '\\')
        })
}
pub(crate) fn tool_text(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}]})
}
pub(crate) fn tool_error(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":true})
}

#[cfg(test)]
#[path = "native_contract_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) mod testing {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock},
    };
    struct Fixture {
        endpoint: reqwest::Url,
        nonce: String,
        hooks: HashMap<String, Box<dyn FnOnce() + Send>>,
        completed: Vec<serde_json::Value>,
    }
    fn fixtures() -> &'static Mutex<HashMap<PathBuf, Fixture>> {
        static F: OnceLock<Mutex<HashMap<PathBuf, Fixture>>> = OnceLock::new();
        F.get_or_init(Default::default)
    }
    pub(crate) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Ok(mut rows) = fixtures().lock() {
                rows.remove(&self.0);
            }
            let _ = std::fs::remove_file(self.0.join(".closed-native-owned-fixture"));
        }
    }
    pub(crate) fn install(home: &Path, base: &str) -> Guard {
        let home = home.canonicalize().expect("owned fixture home");
        let endpoint = reqwest::Url::parse(base).unwrap();
        assert_eq!(endpoint.scheme(), "http");
        assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
        assert!(endpoint.port().is_some_and(|p| p >= 1024));
        assert_eq!(endpoint.path(), "/");
        assert!(
            endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
        );
        let nonce = uuid::Uuid::new_v4().to_string();
        std::fs::write(home.join(".closed-native-owned-fixture"), &nonce).unwrap();
        assert!(
            fixtures()
                .lock()
                .unwrap()
                .insert(
                    home.clone(),
                    Fixture {
                        endpoint,
                        nonce,
                        hooks: HashMap::new(),
                        completed: Vec::new()
                    }
                )
                .is_none()
        );
        Guard(home)
    }
    pub(crate) fn hook(home: &Path, stage: &str, callback: Box<dyn FnOnce() + Send>) {
        fixtures()
            .lock()
            .unwrap()
            .get_mut(&home.canonicalize().unwrap())
            .unwrap()
            .hooks
            .insert(stage.into(), callback);
    }
    pub(crate) fn boundary(home: &Path, stage: &str) {
        let callback = home
            .canonicalize()
            .ok()
            .and_then(|home| fixtures().lock().ok()?.get_mut(&home)?.hooks.remove(stage));
        if let Some(callback) = callback {
            callback();
        }
    }
    pub(crate) fn completed(home: &Path, value: &serde_json::Value) {
        if let Some(row) = fixtures()
            .lock()
            .unwrap()
            .get_mut(&home.canonicalize().unwrap())
        {
            row.completed.push(value.clone());
        }
    }
    pub(crate) fn observations(home: &Path) -> Vec<serde_json::Value> {
        fixtures()
            .lock()
            .unwrap()
            .get(&home.canonicalize().unwrap())
            .unwrap()
            .completed
            .clone()
    }
    pub(crate) fn required(home: &Path) -> bool {
        endpoint(home, "https://gmail.googleapis.com/").is_some()
    }
    pub(crate) fn endpoint(home: &Path, original: &str) -> Option<reqwest::Url> {
        let home = home.canonicalize().ok()?;
        let rows = fixtures().lock().ok()?;
        let f = rows.get(&home)?;
        if std::fs::read_to_string(home.join(".closed-native-owned-fixture"))
            .ok()?
            .as_str()
            != f.nonce
        {
            return None;
        }
        let original = reqwest::Url::parse(original).ok()?;
        let mut endpoint = f.endpoint.clone();
        endpoint.set_path(&format!("/{}{}", original.host_str()?, original.path()));
        endpoint.set_query(original.query());
        Some(endpoint)
    }
}
