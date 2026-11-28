//! Content-free Drive write audit records and bounded Admin notices.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::google_identity) enum Outcome { NotSent, MayHaveSent, Confirmed, Refused }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::google_identity) enum PostCheck { Ok, Violation, Incomplete, None }

/// Only registered codes can enter the reason field, never provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(in crate::google_identity) struct Reason(&'static str);
impl Reason {
    pub(in crate::google_identity) fn from_code(code: &str) -> Option<Self> {
        const EXECUTION_CODES: &[&str] = &[
            "not_sent", "approval_unavailable", "action_off", "folder_required", "folder_out_of_scope", "parent_not_visible",
            "trashed", "shortcut_not_allowed", "capability_denied", "cross_drive_move",
            "listed_folder_protected", "folder_check_limit", "failed", "result_unconfirmed",
            "insufficient_permissions", "google_error", "approval_denied", "post_check_violation",
            "post_check_incomplete", "unknown_tool", "invalid_argument", "invalid_id",
            "invalid_content_choice", "content_too_large", "invalid_attachment", "scope_unconfirmed",
            "target_mime_required", "google_native_content", "anchor_not_unique",
            "invalid_document_state", "sheet_title_exists", "sheet_not_unique",
        ];
        crate::google_identity::REFUSAL_CODES.iter().chain(EXECUTION_CODES.iter())
            .find(|known| **known == code).map(|known| Self(*known))
    }
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::google_identity) struct Approval {
    pub mode: String,
    pub approval_ids: Vec<String>,
    pub decided_by: Vec<String>,
}

/// Audit identity projection: no chat ids, sender names or Debug payloads.
#[derive(Debug, Clone, Serialize)]
pub(in crate::google_identity) struct AuditRequester {
    pub kind: &'static str,
    pub user_id: Option<String>,
    pub channel: Option<String>,
}

impl AuditRequester {
    pub(in crate::google_identity) fn from_identity(who: &duduclaw_knowledge::requester::RequesterIdentity) -> Self {
        use duduclaw_knowledge::requester::{RequesterIdentity, is_public_visitor_channel};
        let (kind, user_id, channel) = match who {
            RequesterIdentity::DashboardUser { user_id } => ("dashboard_user", Some(user_id.clone()), None),
            RequesterIdentity::ChannelUser { user_id, channel, .. } => {
                let user_id = if is_public_visitor_channel(channel) { None } else { user_id.clone() };
                let kind = if user_id.is_some() { "channel_user" } else { "channel_unbound" };
                (kind, user_id, Some(channel.clone()))
            }
            RequesterIdentity::System { on_behalf_of, .. } => ("system", on_behalf_of.clone(), None),
            RequesterIdentity::Unknown => ("unknown", None, None),
        };
        Self { kind, user_id, channel }
    }
}

/// Explicit fields only: no argument/content bags or token-bearing structs.
pub(in crate::google_identity) struct DriveWriteAudit {
    pub call_id: String,
    pub google_turn_id: String,
    pub id_source: &'static str,
    pub run_id: Option<String>,
    pub agent_id: String,
    pub identity_kind: String,
    pub account_email: String,
    pub requester: AuditRequester,
    pub action: String,
    pub tool: String,
    pub file_id: Option<String>,
    pub file_name: Option<String>,
    pub parents_before: Vec<String>,
    pub parents_after: Vec<String>,
    pub destination_folder_id: Option<String>,
    pub folder_rule: Option<String>,
    pub approval: Approval,
    pub bytes: Option<u64>,
    pub content_sha256: Option<String>,
    pub outcome: Outcome,
    pub reason: Option<Reason>,
    pub post_check: PostCheck,
}

pub(in crate::google_identity) fn append(home: &Path, record: &DriveWriteAudit) {
    let file_name = record.file_name.as_deref().map(|name| duduclaw_core::truncate_chars(name, 80));
    duduclaw_security::audit::append_tool_call_with_extras(
        home, &record.agent_id, "google_drive_write", &record.tool,
        record.outcome == Outcome::Confirmed && record.post_check == PostCheck::Ok,
        &[
            ("call_id", json!(record.call_id)), ("google_turn_id", json!(record.google_turn_id)),
            ("id_source", json!(record.id_source)),
            ("run_id", json!(record.run_id)), ("identity_kind", json!(record.identity_kind)),
            ("account_email", json!(record.account_email)), ("requester", json!(record.requester)),
            ("action", json!(record.action)), ("tool", json!(record.tool)),
            ("file_id", json!(record.file_id)), ("file_name", json!(file_name)),
            ("parents_before", json!(record.parents_before)), ("parents_after", json!(record.parents_after)),
            ("destination_folder_id", json!(record.destination_folder_id)), ("folder_rule", json!(record.folder_rule)),
            ("approval", json!(record.approval)), ("bytes", json!(record.bytes)),
            ("content_sha256", json!(record.content_sha256)), ("outcome", json!(record.outcome)),
            ("reason", json!(record.reason)), ("post_check", json!(record.post_check)),
        ],
    );
}

pub(in crate::google_identity) async fn notify_admin_post_check_violation(home: &Path, agent: &str, file_id: &str) {
    static SENT: OnceLock<Mutex<HashMap<(PathBuf, String, String), Instant>>> = OnceLock::new();
    let targets = match crate::notify_push::admin_targets(home) {
        Ok(targets) if !targets.is_empty() => targets,
        Ok(_) => return,
        Err(_) => {
            tracing::warn!(agent, "Drive write post-check notice: Admin destinations unavailable");
            return;
        }
    };
    let Ok(home_key) = home.canonicalize() else { return; };
    if !super::credential::claim_notice(SENT.get_or_init(Default::default),
        (home_key, agent.into(), file_id.into()), Duration::from_secs(24 * 3600)) { return; }
    crate::notify_push::push_plain_notice(home, "google.drive_write.post_check_violation",
        &format!("AI 員工「{agent}」寫入後，發現雲端硬碟檔案（{file_id}）超出允許範圍。請檢查檔案位置與分享設定。"), targets).await;
}
