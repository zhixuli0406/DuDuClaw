//! One bounded, closed projection for search and portable conversation data.
//! Matching external search terms happens only after masking, never on PII.
use super::{Class, read};
use crate::{session::SessionMessage, team_history::mask};
use std::{collections::BTreeMap, path::Path, sync::OnceLock};

pub struct Session {
    pub metadata: read::Metadata,
    pub rows: Vec<read::StoredRow>,
}
pub fn load(
    home: &Path,
    user: &str,
    agent: &str,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Vec<Session>, String> {
    if !home.join("sessions.db").exists() {
        return Ok(vec![]);
    }
    let c = read::open(home)?;
    c.execute_batch("BEGIN DEFERRED")
        .map_err(|_| "history unavailable")?;
    let mut out = vec![];
    let mut bytes = 0usize;
    let mut count = 0usize;
    for metadata in read::list(&c, user, agent, 10_000)? {
        let rows = read::rows(&c, &metadata.id)?;
        for r in &rows {
            bytes = bytes.saturating_add(r.message.content.len());
        }
        count += rows.len();
        if count > 20_000 || bytes > read::MAX_BYTES {
            return Err("history unavailable".into());
        }
        out.push(Session { metadata, rows });
    }
    c.execute_batch("COMMIT")
        .map_err(|_| "history unavailable")?;
    Ok(out)
}
/// Own WebChat is full text. Do not reuse the manager attachment renderer:
/// its conservative whole-line removal is only appropriate for restricted
/// attachments and would erase ordinary text in the owner's export/search.
pub fn own(rows: Vec<SessionMessage>, agent: &str) -> mask::Transcript {
    let messages = rows
        .into_iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| {
            let prefix = crate::channel_reply::strip_sender_prefix(&m.content);
            let body = if m.role == "assistant" {
                crate::os_operator::strip_system_operator_pending_tag(prefix).0
            } else {
                prefix.to_owned()
            };
            mask::Message {
                sender_label: if m.role == "assistant" {
                    agent.into()
                } else {
                    "You".into()
                },
                role: m.role,
                parts: vec![mask::Part::Text { v: body }],
                timestamp: m.timestamp,
            }
        })
        .collect();
    mask::Transcript {
        messages,
        masked_counts: BTreeMap::new(),
        ner: false,
    }
}
pub async fn project(
    home: std::path::PathBuf,
    rows: Vec<SessionMessage>,
    external: bool,
    agent: String,
) -> Result<mask::Transcript, String> {
    if external {
        // TOML/regex/model errors can contain private configuration text.
        // Return only a closed diagnosis to every restricted consumer.
        mask::external(home, rows, BTreeMap::new(), agent)
            .await
            .map_err(|_| "masking_failed".into())
    } else {
        Ok(own(rows, &agent))
    }
}
/// Export collectors run on blocking workers. A single shared runtime only
/// schedules the existing bounded mask service: no second inference engine,
/// nested block_on, timeout escape, or additional model concurrency.
pub fn project_sync(
    home: &Path,
    rows: Vec<SessionMessage>,
    external: bool,
    agent: &str,
) -> Result<mask::Transcript, String> {
    if !external {
        return Ok(own(rows, agent));
    }
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .map_err(|_| "masking unavailable".into())
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let home = home.to_path_buf();
    let agent = agent.to_owned();
    runtime.spawn(async move {
        let result = project(home, rows, true, agent).await;
        let _ = send.send(result);
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(125))
        .map_err(|_| "masking timeout")?
}
pub fn clone_rows(rows: &[read::StoredRow]) -> Vec<SessionMessage> {
    rows.iter()
        .map(|r| SessionMessage {
            role: r.message.role.clone(),
            content: r.message.content.clone(),
            timestamp: r.message.timestamp.clone(),
            tokens: r.message.tokens,
        })
        .collect()
}
pub fn external(class: &Class) -> bool {
    matches!(class, Class::External { .. })
}
pub fn channel(class: &Class) -> &str {
    match class {
        Class::External { channel } => channel,
        _ => "webchat",
    }
}
