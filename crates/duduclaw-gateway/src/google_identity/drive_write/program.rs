//! Pure Drive request programs. The executor owns credentials, attachment
//! provenance/hash verification, approvals, sending and post-write checks.

use std::{collections::BTreeSet, fmt};
use axum::body::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use super::{
    args::{self, ArgError, CreateContent, DriveArgs},
    scope::ScopeVerdict,
    transport::{ClosedMime, GoogleId, JsonBody, WriteCall},
};

/// The executor must resolve this from the current requester's attachment and
/// verify sha256 against this turn's attachment hashes before calling build.
/// This module never opens a path or verifies provenance on the caller's behalf.
pub(crate) struct AttachmentContent {
    pub(crate) file_name: String,
    pub(crate) bytes: Bytes,
    pub(crate) sha256: String,
}

fn metadata(value: Value) -> Result<JsonBody, ArgError> {
    JsonBody::new(value).map_err(|_| ArgError::InvalidArgument)
}

fn checked_scope<'a>(args: &DriveArgs, verdict: &'a ScopeVerdict) -> Result<Option<&'a [String]>, ArgError> {
    let ScopeVerdict::InScope { target_id, parents, matched } = verdict else {
        return Err(ArgError::ScopeUnconfirmed);
    };
    let expected = match args {
        DriveArgs::Create { parent, .. } => parent.as_ref(),
        DriveArgs::Copy { source, .. } => Some(source),
        DriveArgs::Update { target, .. } | DriveArgs::Move { target, .. } | DriveArgs::Trash { target } => Some(target),
    };
    if target_id.as_deref() != expected.map(GoogleId::as_str)
        || (expected.is_none() && !matched.is_empty())
    { return Err(ArgError::ScopeUnconfirmed); }
    Ok(parents.as_deref())
}

/// Pass the MIME read from the checked target metadata for content updates.
/// A verdict is not an approval token: the executor must bind it, the args and
/// destination to the same approved invocation and repeat scope checks fresh.
pub(crate) fn build(
    args: &DriveArgs, verdict: &ScopeVerdict, content: Option<AttachmentContent>,
    target_mime: Option<&str>,
) -> Result<Vec<WriteCall>, ArgError> {
    let parents = checked_scope(args, verdict)?;
    if content.is_some() && !matches!(args, DriveArgs::Create { content: CreateContent::Attachment { .. }, .. }) {
        return Err(ArgError::InvalidAttachment);
    }
    let call = match args {
        DriveArgs::Create { name, parent, content: source, convert_to } => {
            let mut meta = json!({"name":name});
            if let Some(parent) = parent { meta["parents"] = json!([parent.as_str()]); }
            if let Some(convert_to) = convert_to { meta["mimeType"] = json!(convert_to.mime()); }
            match source {
                CreateContent::Folder => {
                    if convert_to.is_some() { return Err(ArgError::InvalidArgument); }
                    meta["mimeType"] = json!("application/vnd.google-apps.folder");
                    WriteCall::DriveCreateFolder { metadata_json: metadata(meta)? }
                }
                CreateContent::Text { text, mime } => WriteCall::DriveCreateMultipart {
                    metadata_json: metadata(meta)?, content: Bytes::from(text.clone()), content_type: *mime,
                },
                CreateContent::Attachment { file_name } => {
                    let attachment = content.ok_or(ArgError::InvalidAttachment)?;
                    args::attachment_name(&attachment.file_name)?;
                    if attachment.file_name != *file_name { return Err(ArgError::InvalidAttachment); }
                    if attachment.bytes.len() > 5 * 1024 * 1024 { return Err(ArgError::ContentTooLarge); }
                    let extension = attachment.file_name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
                    let content_type = match extension.as_deref() {
                        Some("txt") => ClosedMime::PlainText,
                        Some("csv") => ClosedMime::Csv,
                        Some("md") => ClosedMime::Markdown,
                        _ => ClosedMime::UnknownAttachment,
                    };
                    WriteCall::DriveCreateMultipart { metadata_json: metadata(meta)?, content: attachment.bytes, content_type }
                }
            }
        }
        DriveArgs::Copy { source, parent, name } => {
            let mut meta = json!({"parents":[parent.as_str()]});
            if let Some(name) = name { meta["name"] = json!(name); }
            WriteCall::DriveCopy { file_id: source.clone(), metadata_json: metadata(meta)? }
        }
        DriveArgs::Update { target, name, description, text } => {
            let mut meta = json!({});
            if let Some(name) = name { meta["name"] = json!(name); }
            if let Some(description) = description { meta["description"] = json!(description); }
            if let Some(text) = text {
                let mime = target_mime.filter(|mime| !mime.is_empty()).ok_or(ArgError::TargetMimeRequired)?;
                // Compare the MIME namespace token, including future native types.
                if mime.strip_prefix("application/vnd.google-apps.").is_some() {
                    return Err(ArgError::GoogleNativeContent);
                }
                WriteCall::DriveUpdateContent {
                    file_id: target.clone(), metadata_json: metadata(meta)?,
                    content: Bytes::from(text.clone()), content_type: ClosedMime::PlainText,
                }
            } else {
                WriteCall::DriveUpdateMetadata { file_id: target.clone(), metadata_json: metadata(meta)? }
            }
        }
        DriveArgs::Move { target, destination } => {
            let parents = parents.ok_or(ArgError::ScopeUnconfirmed)?;
            if parents.is_empty() || parents.len() > 10 || parents.iter().collect::<BTreeSet<_>>().len() != parents.len() {
                return Err(ArgError::ScopeUnconfirmed);
            }
            let remove_parents = parents.iter().map(|id| GoogleId::new(id.as_str()).map_err(|_| ArgError::ScopeUnconfirmed)).collect::<Result<Vec<_>, _>>()?;
            WriteCall::DriveMove { file_id: target.clone(), add_parent: destination.clone(), remove_parents }
        }
        DriveArgs::Trash { target } => WriteCall::DriveTrash { file_id: target.clone() },
    };
    Ok(vec![call])
}

/// Empty means no explicit expected parent (implicit root creation), or no
/// usable verdict. post_check_parents deliberately reports Incomplete then;
/// the executor must not infer a verified root from an empty vector.
pub(crate) fn expected_parents(args: &DriveArgs, verdict: &ScopeVerdict) -> Vec<String> {
    let Ok(parents) = checked_scope(args, verdict) else { return vec![]; };
    match args {
        DriveArgs::Create { parent, .. } => parent.iter().map(|id| id.as_str().to_owned()).collect(),
        DriveArgs::Copy { parent, .. } => vec![parent.as_str().to_owned()],
        DriveArgs::Move { destination, .. } => vec![destination.as_str().to_owned()],
        DriveArgs::Update { .. } | DriveArgs::Trash { .. } => parents.unwrap_or_default().to_vec(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WriteOutcome {
    pub(crate) file_id: String,
    /// Bounded, control-free name for logs.
    pub(crate) file_name: String,
    /// Full, control-free name for AI-facing text.
    pub(crate) full_file_name: String,
    pub(crate) mime_type: String,
    pub(crate) parents_after: Option<Vec<String>>,
    pub(crate) drive_id: Option<String>,
    pub(crate) trashed: bool,
    pub(crate) link: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutcomeError { InsufficientPermissions, Google(u16), Unconfirmed }

impl OutcomeError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::InsufficientPermissions => "insufficient_permissions",
            Self::Google(_) => "google_error",
            Self::Unconfirmed => "result_unconfirmed",
        }
    }
    pub(crate) fn user_text(self) -> &'static str {
        match self {
            Self::InsufficientPermissions => "目前的 Google 帳號沒有足夠權限，這次操作無法完成，請帳號本人重新連結並同意所需權限。",
            Self::Google(_) => "Google 無法完成這次操作，請帳號本人檢查檔案狀態與存取權限。",
            Self::Unconfirmed => "無法確認這次操作的結果，請先到雲端硬碟查看，不要重複執行。",
        }
    }
}
impl fmt::Display for OutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.user_text()) }
}
impl std::error::Error for OutcomeError {}

// Preserve absent fields while rejecting explicit nulls and wrong types.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    id: String,
    name: String,
    mime_type: String,
    #[serde(default, deserialize_with = "present")]
    parents: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    drive_id: Option<String>,
    trashed: bool,
}

pub(crate) fn interpret(tool: &str, status: u16, body: &[u8]) -> Result<WriteOutcome, OutcomeError> {
    if !args::is_drive_tool(tool) { return Err(OutcomeError::Unconfirmed); }
    if !(200..=299).contains(&status) {
        if status == 403 {
            if let Ok(value) = serde_json::from_slice::<Value>(body) {
                let error = &value["error"];
                if error["status"].as_str() == Some("insufficientPermissions")
                    || error["errors"].as_array().is_some_and(|errors| errors.iter().any(|entry| entry["reason"].as_str() == Some("insufficientPermissions")))
                { return Err(OutcomeError::InsufficientPermissions); }
            }
        }
        return Err(OutcomeError::Google(status));
    }
    let response: Response = serde_json::from_slice(body).map_err(|_| OutcomeError::Unconfirmed)?;
    let id = GoogleId::new(response.id).map_err(|_| OutcomeError::Unconfirmed)?;
    if response.mime_type.trim().is_empty()
        || response.mime_type.chars().any(char::is_control)
        || response.drive_id.as_deref().is_some_and(|id| GoogleId::new(id).is_err())
        || response.parents.as_ref().is_some_and(|parents| {
            parents.iter().any(|id| GoogleId::new(id.as_str()).is_err())
                || parents.iter().collect::<BTreeSet<_>>().len() != parents.len()
        })
        || response.trashed != (tool == "drive_trash_file")
    { return Err(OutcomeError::Unconfirmed); }
    let full_file_name: String = response.name.chars().filter(|c| !c.is_control()).collect();
    Ok(WriteOutcome {
        file_id: id.as_str().to_owned(),
        file_name: duduclaw_core::truncate_chars(&full_file_name, 80),
        full_file_name, mime_type: response.mime_type,
        parents_after: response.parents, drive_id: response.drive_id, trashed: response.trashed,
        link: format!("https://drive.google.com/open?id={}", id.as_str()),
    })
}
