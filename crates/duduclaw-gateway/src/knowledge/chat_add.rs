//! The chat command 「加入知識庫 <集合代號>」: propose the file(s) attached to
//! this message — or, when it has none, the ones the same sender attached in
//! this conversation during the last [`LOOKBACK_MINUTES`] minutes — for a
//! knowledge collection. Every proposal waits for an Admin or the
//! collection's editor in the dashboard ([`super::add_source_approval`]).
//!
//! Only a person the gateway can name may propose: a signed-in WebChat
//! account or a messaging-app sender with a verified binding, who can at
//! least read the collection. Only files the channel adapter itself saved
//! for that sender in that conversation count; a file reference typed into
//! a message names nothing, and the card shows the stored file name. A missing collection and one the person may
//! not read get the same answer.

use std::path::{Path, PathBuf};

use duduclaw_knowledge::acl::{Role, ids_for_slugs};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_knowledge::schema;

use super::add_source_approval::{self as approval, Content, Proposal, Proposed};

/// The command word.
pub const COMMAND: &str = "加入知識庫";
/// How far back an earlier attachment of the same sender counts.
pub const LOOKBACK_MINUTES: i64 = 30;
/// Most files proposed by one command.
pub const MAX_FILES: usize = 5;

pub const NEED_BINDING: &str =
    "只有已綁定 DuDuClaw 帳號的使用者能把檔案加入知識庫。請先在儀表板完成通道帳號綁定。";
pub const NO_COLLECTION: &str = "找不到這個知識集合，或你沒有使用它的權限。";
pub const NO_FILE: &str =
    "沒有找到要加入的檔案。請先傳送檔案，再於 30 分鐘內輸入「加入知識庫 集合代號」。";
pub const USAGE: &str = "用法：加入知識庫 集合代號（例如：加入知識庫 hr-handbook）。";

/// The collection slug when `text` is the command, `Some("")` for the bare
/// command word, `None` when it is not the command.
pub fn parse(text: &str) -> Option<String> {
    let first = text.trim().lines().next()?.trim();
    let rest = first.strip_prefix(COMMAND)?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim().to_string())
}

/// An attachment reference written by the channel adapters:
/// `[📎 name.pdf (file)](/abs/path)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    pub name: String,
    pub path: PathBuf,
}

/// Every attachment reference in `text`, in order.
pub fn attachment_refs(text: &str) -> Vec<AttachmentRef> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        let Some(inner) = line.strip_prefix('[') else {
            continue;
        };
        let Some((label, after)) = inner.split_once("](") else {
            continue;
        };
        let Some((path, _)) = after.split_once(')') else {
            continue;
        };
        // label = "<emoji> <name> (<type>)"
        let Some((head, kind)) = label.rsplit_once(" (") else {
            continue;
        };
        if !matches!(kind, "file)" | "image)" | "audio)" | "video)") {
            continue;
        }
        let name = head
            .split_once(' ')
            .map(|(_, n)| n.trim())
            .unwrap_or("")
            .to_string();
        let path = path.strip_prefix("file://").unwrap_or(path);
        if name.is_empty() || path.is_empty() {
            continue;
        }
        out.push(AttachmentRef {
            name,
            path: PathBuf::from(path),
        });
    }
    out
}

/// The account the command acts for, if any.
fn account(req: &RequesterIdentity) -> Option<String> {
    match req {
        RequesterIdentity::DashboardUser { user_id } => Some(user_id.clone()),
        RequesterIdentity::ChannelUser {
            user_id: Some(u), ..
        } => Some(u.clone()),
        _ => None,
    }
}

fn origin(req: &RequesterIdentity) -> Option<(String, String)> {
    match req {
        RequesterIdentity::ChannelUser { channel, chat, .. } => {
            Some((channel.clone(), chat.clone()))
        }
        _ => None,
    }
}

/// Inputs for [`handle`].
pub struct Command<'a> {
    pub home: &'a Path,
    pub users: Option<std::sync::Arc<duduclaw_auth::db::UserDb>>,
    pub agent_id: &'a str,
    pub requester: RequesterIdentity,
    /// The conversation and the sender's platform id: only files the
    /// channel adapter saved for this sender here are accepted
    /// ([`super::chat_attachments`]).
    pub session_id: &'a str,
    pub sender: &'a str,
    pub slug: String,
    /// Attachments of this message, else recent ones of the same sender.
    pub attachments: Vec<AttachmentRef>,
}

/// Run the command; the reply text.
pub async fn handle(cmd: Command<'_>) -> String {
    if cmd.slug.is_empty() {
        return USAGE.to_string();
    }
    let Some(user_id) = account(&cmd.requester) else {
        return NEED_BINDING.to_string();
    };
    // Only files the adapter saved for this sender in this conversation;
    // a reference typed into a message names nothing.
    let attachments: Vec<(std::path::PathBuf, String)> = cmd
        .attachments
        .iter()
        .filter_map(|a| super::chat_attachments::owned(cmd.session_id, cmd.sender, &a.path))
        .take(MAX_FILES)
        .collect();
    if attachments.is_empty() {
        return NO_FILE.to_string();
    }
    let (home, slug, users, uid) = (
        cmd.home.to_path_buf(),
        cmd.slug.clone(),
        cmd.users.clone(),
        user_id.clone(),
    );
    let collection = super::blocking(move || {
        let dir = super::user_directory(&home, users.as_ref());
        let Some(user) = dir.lookup(&uid).ok().flatten() else {
            return Ok(None);
        };
        let conn = schema::open_home(&home)?;
        let (ids, _) = ids_for_slugs(&conn, &[slug])?;
        let Some(id) = ids.into_iter().next() else {
            return Ok(None);
        };
        let role = duduclaw_knowledge::acl::user_role(
            &conn,
            &id,
            &user.user_id,
            user.department.as_deref(),
            user.is_admin,
        )?;
        let name = duduclaw_knowledge::store::get_collection(&conn, &id)?.map(|c| c.name);
        Ok(role
            .filter(|r| *r >= Role::Viewer)
            .and(name)
            .map(|n| (id, n)))
    })
    .await;
    let (collection_id, collection_name) = match collection {
        Ok(Some(c)) => c,
        Ok(None) => return NO_COLLECTION.to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: chat add lookup failed");
            return NO_COLLECTION.to_string();
        }
    };
    let mut lines = Vec::new();
    for (real, name) in attachments {
        let proposal = Proposal {
            agent_id: cmd.agent_id.to_string(),
            collection_id: collection_id.clone(),
            // The name the adapter stored, never text from the message.
            title: Some(name.clone()),
            content: Content::File {
                path: real,
                name: name.clone(),
            },
            proposed_by: format!("user:{user_id}"),
            via: "chat",
            requester_kind: cmd.requester.kind().to_string(),
            requester_channel: cmd.requester.channel().map(str::to_string),
            origin: origin(&cmd.requester),
        };
        lines.push(match approval::propose(cmd.home, proposal).await {
            Ok(Proposed::Filed { approval_id, title }) => format!(
                "「{title}」已送出審核（編號 {}），管理員或集合編輯者在儀表板核准後，會自動加入「{collection_name}」。",
                duduclaw_core::truncate_chars(&approval_id, 8)
            ),
            Ok(Proposed::Duplicate { title }) => {
                format!("「{title}」已經在「{collection_name}」裡，不需要再加入。")
            }
            Err(e) => format!("「{name}」沒有送出：{}", e.public_message()),
        });
    }
    lines.join("\n")
}

/// Run the command inside a channel reply turn: the person from the turn's
/// requester, attachments from `text` or the sender's recent messages.
pub async fn run_in_turn(
    ctx: &crate::channel_reply::ReplyContext,
    agent_id: &str,
    session_id: &str,
    sender: &str,
    text: &str,
    slug: String,
) -> String {
    let mut attachments = attachment_refs(text);
    if attachments.is_empty() {
        if let Ok(history) = ctx.session_manager.get_messages(session_id).await {
            attachments = recent_attachments(&history, sender, chrono::Utc::now());
        }
    }
    handle(Command {
        home: &ctx.home_dir,
        users: None,
        agent_id,
        requester: super::turn_requesters::current_requester(),
        session_id,
        sender,
        slug,
        attachments,
    })
    .await
}

/// The sender's attachments from the last [`LOOKBACK_MINUTES`] minutes of
/// `history` (newest message first), when the command message had none.
/// Only user messages whose sender prefix names `sender` count.
pub fn recent_attachments(
    history: &[crate::session::SessionMessage],
    sender: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AttachmentRef> {
    let prefix = format!("{}{sender}]\n", crate::channel_reply::SENDER_PREFIX_OPEN);
    for m in history.iter().rev().filter(|m| m.role == "user") {
        let recent = chrono::DateTime::parse_from_rfc3339(&m.timestamp)
            .map(|t| now.signed_duration_since(t.with_timezone(&chrono::Utc)))
            .is_ok_and(|age| age.num_minutes() <= LOOKBACK_MINUTES && age.num_seconds() >= -60);
        if !recent {
            break;
        }
        let Some(body) = m.content.strip_prefix(&prefix) else {
            continue;
        };
        let refs = attachment_refs(body);
        if !refs.is_empty() {
            return refs;
        }
    }
    Vec::new()
}

#[cfg(test)]
#[path = "chat_add_tests.rs"]
mod tests;
