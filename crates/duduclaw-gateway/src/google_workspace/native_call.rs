//! Immutable host inputs only. Provider replies never enter this module.
use serde_json::{Value, json};

pub(crate) enum ParsedNativeArguments {
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

impl NativeCallTemplate {
    pub(crate) fn prepare(tool: &str, args: &Value) -> Result<Self, String> {
        let parsed = match tool {
            "gmail_search" => ParsedNativeArguments::GmailSearch {
                query: required(args, "query", false, true)?,
                max: number(args, "max_results", 10),
            },
            "gmail_read" => ParsedNativeArguments::GmailRead {
                message_id: required(args, "message_id", false, true)?,
            },
            "gmail_create_draft" => ParsedNativeArguments::GmailDraft {
                to: required(args, "to", true, true)?,
                subject: required(args, "subject", false, false)?,
                body: required(args, "body", false, false)?,
                cc: optional(args, "cc", true),
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
        Ok(call)
    }
    // Legacy public API wrappers have typed arguments rather than an MCP DTO.
    pub(super) fn for_api(tool: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            tool: tool.into(),
            digest: String::new(),
            parsed: None,
            calendar_min: now.to_rfc3339(),
            calendar_max: (now + chrono::Duration::days(7)).to_rfc3339(),
            meet_request_id: uuid::Uuid::new_v4().to_string(),
        }
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
            GmailRead { message_id } => safe_gmail_component(message_id),
            SheetsRead { spreadsheet, range }
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
            DocsRead { document } | DocsAppend { document, .. } => {
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
