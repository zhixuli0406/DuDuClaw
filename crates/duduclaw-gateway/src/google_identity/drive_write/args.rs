//! Pure, closed argument vocabulary for the five Drive write tools.

use std::fmt;
use serde_json::{Map, Value};
use super::{scope::Plan, transport::{ClosedMime, GoogleId}};
use crate::google_workspace::drive_write_policy::extract_folder_id;

#[derive(Clone, Debug)]
pub(crate) enum CreateContent {
    Folder,
    Text { text: String, mime: ClosedMime },
    Attachment { file_name: String },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ConvertTo { Document, Spreadsheet }

impl ConvertTo {
    pub(crate) fn mime(self) -> &'static str {
        match self {
            Self::Document => "application/vnd.google-apps.document",
            Self::Spreadsheet => "application/vnd.google-apps.spreadsheet",
        }
    }
}

/// Construct through parse before passing to the scope checker or builder.
#[derive(Clone, Debug)]
pub(crate) enum DriveArgs {
    Create { name: String, parent: Option<GoogleId>, content: CreateContent, convert_to: Option<ConvertTo> },
    Copy { source: GoogleId, parent: GoogleId, name: Option<String> },
    Update { target: GoogleId, name: Option<String>, description: Option<String>, text: Option<String> },
    Move { target: GoogleId, destination: GoogleId },
    Trash { target: GoogleId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArgError {
    UnknownTool,
    InvalidArgument,
    InvalidId,
    ContentChoice,
    ContentTooLarge,
    InvalidAttachment,
    ScopeUnconfirmed,
    TargetMimeRequired,
    GoogleNativeContent,
    AnchorNotUnique,
    InvalidState,
    SheetConflict,
    SheetNotUnique,
}

impl ArgError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown_tool",
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidId => "invalid_id",
            Self::ContentChoice => "invalid_content_choice",
            Self::ContentTooLarge => "content_too_large",
            Self::InvalidAttachment => "invalid_attachment",
            Self::ScopeUnconfirmed => "scope_unconfirmed",
            Self::TargetMimeRequired => "target_mime_required",
            Self::GoogleNativeContent => "google_native_content",
            Self::AnchorNotUnique => "anchor_not_unique",
            Self::InvalidState => "invalid_document_state",
            Self::SheetConflict => "sheet_title_exists",
            Self::SheetNotUnique => "sheet_not_unique",
        }
    }

    pub(crate) fn user_text(self) -> &'static str {
        match self {
            Self::UnknownTool => "不支援這個雲端硬碟操作。",
            Self::InvalidArgument => "提供的資料不完整或格式不正確，請確認後再試。",
            Self::InvalidId => "檔案代號格式不正確，這次沒有執行，請確認檔案後重新提供。",
            Self::ContentChoice => "請只提供一種內容：文字、附件或建立資料夾。",
            Self::ContentTooLarge => "提供的文字或附件超過大小上限，這次沒有執行，請縮短文字或提供較小的附件。",
            Self::InvalidAttachment => "請提供本輪附件的檔名，不要提供本機路徑。",
            Self::ScopeUnconfirmed => "無法確認是否允許操作這個檔案或資料夾，這次沒有執行，請管理員檢查可使用的資料夾設定。",
            Self::TargetMimeRequired => "無法確認檔案格式，請稍後再試。",
            Self::GoogleNativeContent => "這是 Google 文件格式，請改用文件編輯工具。",
            Self::AnchorNotUnique => "找不到唯一的位置，請提供更精確的文字。",
            Self::InvalidState => "無法確認文件或試算表的目前內容，這次沒有執行，請稍後再試。",
            Self::SheetConflict => "已經有同名工作表，請使用其他名稱。",
            Self::SheetNotUnique => "找不到唯一的工作表，請確認名稱。",
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.user_text()) }
}
impl std::error::Error for ArgError {}

pub(crate) fn is_drive_tool(tool: &str) -> bool {
    matches!(tool, "drive_create_file" | "drive_copy_file" | "drive_update_file" | "drive_move_file" | "drive_trash_file")
}

fn string<'a>(map: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, ArgError> {
    map.get(key).map(|v| v.as_str().ok_or(ArgError::InvalidArgument)).transpose()
}
fn required<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str, ArgError> {
    string(map, key)?.ok_or(ArgError::InvalidArgument)
}
fn name(value: &str) -> Result<String, ArgError> {
    // Check controls before trimming so a trailing newline cannot disappear.
    if value.chars().any(char::is_control) { return Err(ArgError::InvalidArgument); }
    let value = value.trim();
    if !(1..=255).contains(&value.chars().count()) { return Err(ArgError::InvalidArgument); }
    Ok(value.to_owned())
}
fn bounded(value: &str, limit: usize) -> Result<String, ArgError> {
    if value.chars().count() > limit { return Err(ArgError::ContentTooLarge); }
    Ok(value.to_owned())
}
pub(crate) fn attachment_name(value: &str) -> Result<(), ArgError> {
    if value.trim().is_empty() || value.chars().count() > 255
        || value.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || value.contains("..") || value == "."
    { return Err(ArgError::InvalidAttachment); }
    Ok(())
}

/// Reject normalization tricks before URL parsing. Percent escapes are not
/// needed in any supported ID/path; query decorations such as usp are allowed.
pub(crate) fn id(value: &str) -> Result<GoogleId, ArgError> {
    if let Ok(id) = GoogleId::new(value) { return Ok(id); }
    if value.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\\' | '%'))
        || value.split(['/', '?', '#']).any(|s| matches!(s, "." | ".."))
        || value.split_once("://").map(|(scheme, _)| scheme) != Some("https")
    { return Err(ArgError::InvalidId); }
    let url = url::Url::parse(value).map_err(|_| ArgError::InvalidId)?;
    if url.scheme() != "https" || !matches!(url.host_str(), Some("drive.google.com" | "docs.google.com"))
        || !url.username().is_empty() || url.password().is_some() || url.port().is_some()
    { return Err(ArgError::InvalidId); }
    let parts: Vec<_> = url.path_segments().ok_or(ArgError::InvalidId)?.collect();
    let query_ids: Vec<_> = url.query_pairs().filter(|(key, _)| key == "id").map(|(_, v)| v.into_owned()).collect();
    let path_id = match parts.as_slice() {
        ["drive", "folders", _] | ["drive", "u", _, "folders", _] => {
            // Keep policy and tool folder-link parsing aligned.
            let mut folder_url = url.clone();
            folder_url.set_query(None);
            Some(extract_folder_id(folder_url.as_str()).ok_or(ArgError::InvalidId)?)
        }
        ["file" | "document" | "spreadsheets", "d", id, ..] => Some((*id).to_owned()),
        [""] | ["open"] => None,
        _ => return Err(ArgError::InvalidId),
    };
    let extracted = match (path_id, query_ids.as_slice()) {
        (Some(id), []) => id,
        (Some(id), [query]) if id == *query => id,
        (None, [query]) => query.clone(),
        _ => return Err(ArgError::InvalidId),
    };
    GoogleId::new(extracted).map_err(|_| ArgError::InvalidId)
}

pub(crate) fn parse(tool: &str, arguments: &Value) -> Result<DriveArgs, ArgError> {
    let keys: &[&str] = match tool {
        "drive_create_file" => &["name", "parent_folder_id", "text", "attachment", "folder", "convert_to", "mime_type"],
        "drive_copy_file" => &["file_id", "parent_folder_id", "name"],
        "drive_update_file" => &["file_id", "name", "description", "text"],
        "drive_move_file" => &["file_id", "destination_folder_id"],
        "drive_trash_file" => &["file_id"],
        _ => return Err(ArgError::UnknownTool),
    };
    let map = arguments.as_object().ok_or(ArgError::InvalidArgument)?;
    if map.keys().any(|key| !keys.contains(&key.as_str())) { return Err(ArgError::InvalidArgument); }
    match tool {
        "drive_create_file" => {
            let name = name(required(map, "name")?)?;
            let parent = string(map, "parent_folder_id")?.map(id).transpose()?;
            let text = string(map, "text")?.map(|s| bounded(s, 100_000)).transpose()?;
            let attachment = string(map, "attachment")?;
            let folder = map.get("folder").map(|v| v.as_bool().ok_or(ArgError::InvalidArgument)).transpose()?.unwrap_or(false);
            if usize::from(text.is_some()) + usize::from(attachment.is_some()) + usize::from(folder) != 1 {
                return Err(ArgError::ContentChoice);
            }
            let convert_to = match string(map, "convert_to")? {
                Some("document") => Some(ConvertTo::Document),
                Some("spreadsheet") => Some(ConvertTo::Spreadsheet),
                None => None,
                _ => return Err(ArgError::InvalidArgument),
            };
            if folder && convert_to.is_some() { return Err(ArgError::InvalidArgument); }
            let mime = match string(map, "mime_type")? {
                Some(_) if text.is_none() => return Err(ArgError::InvalidArgument),
                None | Some("text/plain") => ClosedMime::PlainText,
                Some("text/csv") => ClosedMime::Csv,
                Some("text/markdown") => ClosedMime::Markdown,
                _ => return Err(ArgError::InvalidArgument),
            };
            let content = if let Some(text) = text { CreateContent::Text { text, mime } }
                else if let Some(file_name) = attachment {
                    attachment_name(file_name)?;
                    CreateContent::Attachment { file_name: file_name.to_owned() }
                } else { CreateContent::Folder };
            Ok(DriveArgs::Create { name, parent, content, convert_to })
        }
        "drive_copy_file" => Ok(DriveArgs::Copy {
            source: id(required(map, "file_id")?)?, parent: id(required(map, "parent_folder_id")?)?,
            name: string(map, "name")?.map(name).transpose()?,
        }),
        "drive_update_file" => {
            let target = id(required(map, "file_id")?)?;
            let name = string(map, "name")?.map(name).transpose()?;
            let description = string(map, "description")?.map(|s| bounded(s, 4_000)).transpose()?;
            let text = string(map, "text")?.map(|s| bounded(s, 100_000)).transpose()?;
            if name.is_none() && description.is_none() && text.is_none() { return Err(ArgError::InvalidArgument); }
            Ok(DriveArgs::Update { target, name, description, text })
        }
        "drive_move_file" => Ok(DriveArgs::Move { target: id(required(map, "file_id")?)?, destination: id(required(map, "destination_folder_id")?)? }),
        "drive_trash_file" => Ok(DriveArgs::Trash { target: id(required(map, "file_id")?)? }),
        _ => Err(ArgError::UnknownTool),
    }
}

pub(crate) fn plan(args: &DriveArgs) -> Plan {
    match args {
        DriveArgs::Create { parent, .. } => Plan::Create { parent: parent.clone() },
        DriveArgs::Copy { source, parent, .. } => Plan::Copy { source: source.clone(), parent: parent.clone() },
        DriveArgs::Update { target, name, .. } => Plan::Modify { target: target.clone(), rename: name.is_some() },
        DriveArgs::Move { target, destination } => Plan::Move { target: target.clone(), destination: destination.clone() },
        DriveArgs::Trash { target } => Plan::Trash { target: target.clone() },
    }
}

/// Unified executor entry point for all nine RM-131 tools.
#[derive(Clone)]
pub(crate) enum ToolArgs {
    Drive(DriveArgs),
    DocsSheets(super::args_docs::DocsSheetsArgs),
}

pub(crate) fn parse_any(tool: &str, value: &Value) -> Result<ToolArgs, ArgError> {
    if is_drive_tool(tool) { parse(tool, value).map(ToolArgs::Drive) }
    else { super::args_docs::parse(tool, value).map(ToolArgs::DocsSheets) }
}
