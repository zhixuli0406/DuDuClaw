//! Limits on who may propose a source and how many proposals may wait,
//! the content preview shown to the approver, and cleanup of waiting files.

use std::path::Path;
use std::time::Duration;

use duduclaw_knowledge::requester::{RequesterIdentity, is_public_visitor_channel};
use duduclaw_knowledge::{KbError, KbResult};

use super::add_source_approval::{ACTION_KIND, sidecar_dir};
use crate::approval::{ApprovalBroker, ApprovalStatus};

/// Most cards one employee may have waiting.
pub const MAX_PENDING_PER_AGENT: usize = 20;
/// Most cards one proposer (`agent:<id>` / `user:<id>`) may have waiting.
pub const MAX_PENDING_PER_PROPOSER: usize = 10;
/// Characters of text shown to the approver.
pub const PREVIEW_CHARS: usize = 2_000;
/// A waiting file no pending card refers to is removed after this long
/// (the card may still be being filed).
const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);

/// Who may propose from a tool call: a signed-in person, a person behind a
/// verified channel binding, or a system turn. A public visitor, an
/// unbound sender and an unregistered turn may not (a prompt-injected
/// employee in such a turn cannot file proposals).
pub fn requester_may_propose(who: &RequesterIdentity) -> KbResult<()> {
    let ok = match who {
        RequesterIdentity::DashboardUser { .. } | RequesterIdentity::System { .. } => true,
        RequesterIdentity::ChannelUser {
            channel, user_id, ..
        } => user_id.is_some() && !is_public_visitor_channel(channel),
        RequesterIdentity::Unknown => false,
    };
    if ok {
        Ok(())
    } else {
        Err(KbError::Rejected {
            code: "proposer_not_allowed",
            message: "這個對話的對象無法確認身分（訪客或未綁定帳號），不能送出加入知識庫的請求。"
                .into(),
        })
    }
}

/// Refuse when the employee or the proposer already has too many cards
/// waiting.
pub async fn check_caps(home: &Path, agent_id: &str, proposed_by: &str) -> KbResult<()> {
    let broker = ApprovalBroker::open(home).map_err(|e| KbError::Io(std::io::Error::other(e)))?;
    let now = chrono::Utc::now();
    let pending: Vec<_> = broker
        .list_by_kind(ACTION_KIND)
        .await
        .map_err(|e| KbError::Io(std::io::Error::other(e)))?
        .into_iter()
        .filter(|r| r.status == ApprovalStatus::Pending && !r.is_stale(now))
        .collect();
    let by_agent = pending.iter().filter(|r| r.agent_id == agent_id).count();
    let by_proposer = pending
        .iter()
        .filter(|r| r.payload["proposed_by"].as_str() == Some(proposed_by))
        .count();
    if by_agent >= MAX_PENDING_PER_AGENT || by_proposer >= MAX_PENDING_PER_PROPOSER {
        return Err(KbError::Rejected {
            code: "quota_proposals",
            message: format!(
                "等待審核的加入知識庫請求太多（每位 AI 員工最多 {MAX_PENDING_PER_AGENT} 筆、每位送出者最多 {MAX_PENDING_PER_PROPOSER} 筆），請先在儀表板處理。"
            ),
        });
    }
    Ok(())
}

/// The approver's preview: the first [`PREVIEW_CHARS`] characters of text
/// content (plain text, Markdown, CSV, HTML); `None` for binary formats,
/// whose card shows name, type and size only. Shown as text, never markup.
pub fn preview(format: duduclaw_knowledge::extract::Format, bytes: &[u8]) -> Option<String> {
    use duduclaw_knowledge::extract::Format;
    if !matches!(
        format,
        Format::Txt | Format::Md | Format::Csv | Format::Html
    ) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let flat: String = text
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    Some(duduclaw_core::truncate_chars(&flat, PREVIEW_CHARS))
}

/// Remove waiting files no pending card refers to (expired, denied,
/// withdrawn or never filed), older than a short grace period.
pub async fn sweep_unreferenced(home: &Path) -> usize {
    let dir = sidecar_dir(home);
    if !dir.is_dir() {
        return 0;
    }
    let Ok(broker) = ApprovalBroker::open(home) else {
        return 0;
    };
    let now = chrono::Utc::now();
    let Ok(cards) = broker.list_by_kind(ACTION_KIND).await else {
        return 0;
    };
    let live: Vec<String> = cards
        .iter()
        .filter(|r| r.status == ApprovalStatus::Pending && !r.is_stale(now))
        .filter_map(|r| r.payload["staged"].as_str().map(|s| format!("{s}.bin")))
        .collect();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if live.contains(&name) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > ORPHAN_GRACE);
        if old && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_may_propose() {
        let ch = |channel: &str, user: Option<&str>| RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: "c".into(),
            sender: "s".into(),
            user_id: user.map(str::to_string),
        };
        assert!(requester_may_propose(&ch("line", Some("u"))).is_ok());
        assert!(requester_may_propose(&ch("line", None)).is_err());
        assert!(requester_may_propose(&ch("widget", Some("u"))).is_err());
        assert!(requester_may_propose(&ch("odoolivechat", None)).is_err());
        assert!(requester_may_propose(&RequesterIdentity::Unknown).is_err());
        assert!(
            requester_may_propose(&RequesterIdentity::DashboardUser {
                user_id: "u".into()
            })
            .is_ok()
        );
        assert!(
            requester_may_propose(&RequesterIdentity::System {
                kind: "cron".into(),
                on_behalf_of: None
            })
            .is_ok()
        );
    }

    #[test]
    fn previews_text_only() {
        use duduclaw_knowledge::extract::Format;
        let long = "字".repeat(3_000);
        let p = preview(Format::Txt, long.as_bytes()).unwrap();
        assert_eq!(p.chars().count(), PREVIEW_CHARS);
        assert_eq!(
            preview(Format::Md, "<script>x</script>".as_bytes()).as_deref(),
            Some("<script>x</script>"),
            "kept as text; the dashboard renders it as text"
        );
        assert!(preview(Format::Pdf, b"%PDF").is_none());
        assert!(preview(Format::Docx, b"PK").is_none());
    }
}
