//! Proposals to add a source to a collection from outside the dashboard
//! (the `knowledge_add_source` MCP tool, the 「加入知識庫」 chat command).
//! Every proposal waits for a human; there is no setting that skips it.
//!
//! - **Filing** ([`propose`]): the content is copied into a sidecar file
//!   under `<home>/knowledge/proposals/` (directory 0700, files 0600); the
//!   approval card stores metadata, the content's SHA-256, who asked (kind
//!   and channel) and, for text, a preview of the first 2,000 characters.
//!   A URL proposal stores the URL; it is fetched only after approval.
//!   Visitors, unbound senders and unregistered turns may not propose, and
//!   each employee / proposer may have only a few cards waiting
//!   ([`super::add_source_guard`]).
//! - **Deciding** ([`decide`]): only in the dashboard
//!   (`approval_notify::is_dashboard_only_kind`), only by an Admin or a
//!   person who is editor or owner of the collection **at decision time**.
//!   Approving re-hashes the sidecar; a mismatch refuses and closes the card
//!   as denied. Approval queues a normal ingest job; denial deletes the
//!   sidecar.
//!
//! Waiting files no pending card refers to are removed by
//! [`super::add_source_guard::sweep_unreferenced`] (on every proposal, every
//! proposal list and the worker's housekeeping).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_knowledge::acl::Role;
use duduclaw_knowledge::blob::{BlobStore, url_identity};
use duduclaw_knowledge::extract::Format;
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::requester::ResolvedUser;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind};
use duduclaw_knowledge::{KbError, KbResult, schema};
use serde_json::{Value, json};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};

/// `action_kind` of a proposal card.
pub const ACTION_KIND: &str = "knowledge_add_source";
/// How long a card waits (then expires = denied).
pub const TTL_SECS: i64 = 3 * 24 * 3600;
/// Fixed origin note on the card.
pub const CARD_NOTE: &str = "內容已另存一份；核准時會再比對內容雜湊，與送審時不同就不會匯入。";

/// What is proposed.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// A file already validated by the caller (inside an attachments
    /// directory). `name` is the name shown and used for the format.
    File {
        path: PathBuf,
        name: String,
    },
    Text(String),
    Url(String),
}

/// One proposal.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub agent_id: String,
    pub collection_id: String,
    pub title: Option<String>,
    pub content: Content,
    /// `agent:<id>` or `user:<id>`: who asked (shown on the card).
    pub proposed_by: String,
    /// `mcp` or `chat`.
    pub via: &'static str,
    /// `access_log` kind of the turn's person (`dashboard_user`,
    /// `channel_user`, `system`, …), shown on the card.
    pub requester_kind: String,
    /// The channel the turn came from, when it came from one.
    pub requester_channel: Option<String>,
    /// The conversation it came from (never notified about the card).
    pub origin: Option<(String, String)>,
}

/// Result of [`propose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposed {
    Filed {
        approval_id: String,
        title: String,
    },
    /// The same content is already in the collection.
    Duplicate {
        title: String,
    },
}

pub(crate) fn sidecar_dir(home: &Path) -> PathBuf {
    schema::knowledge_dir(home).join("proposals")
}

fn sidecar_path(home: &Path, staged: &str) -> Option<PathBuf> {
    (staged.len() == 32 && staged.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| sidecar_dir(home).join(format!("{staged}.bin")))
}

/// `path` if it is a regular file inside the employee's own
/// `<agent_dir>/attachments` after resolving links; else `None`. The shared
/// `<home>/attachments` holds other people's files and is not accepted.
pub fn own_attachment_path(agent_dir: &Path, path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    if !real.is_file() {
        return None;
    }
    let root = std::fs::canonicalize(agent_dir.join("attachments")).ok()?;
    real.starts_with(&root).then_some(real)
}

/// Create the proposals directory owner-only.
fn ensure_sidecar_dir(home: &Path) -> std::io::Result<PathBuf> {
    let dir = sidecar_dir(home);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

struct Staged {
    kind: SourceKind,
    format: Format,
    title: String,
    original_name: Option<String>,
    url: Option<String>,
    bytes: u64,
    sha: String,
    staged: Option<String>,
    preview: Option<String>,
}

fn stage(home: &Path, cfg: &KnowledgeConfig, p: &Proposal) -> KbResult<Staged> {
    let write_sidecar = |bytes: &[u8]| -> KbResult<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = ensure_sidecar_dir(home)?;
        let path = dir.join(format!("{id}.bin"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        opts.open(&path)?.write_all(bytes)?;
        Ok(id)
    };
    let too_large = || KbError::Rejected {
        code: "too_large",
        message: format!(
            "檔案超過 {} MB 上限。",
            cfg.max_source_bytes / (1024 * 1024)
        ),
    };
    match &p.content {
        Content::File { path, name } => {
            let format = Format::from_file_name(name).map_err(|e| KbError::Rejected {
                code: "unsupported_format",
                message: e.message,
            })?;
            if std::fs::metadata(path)?.len() > cfg.max_source_bytes {
                return Err(too_large());
            }
            let bytes = std::fs::read(path)?;
            if bytes.is_empty() {
                return Err(KbError::Rejected {
                    code: "empty_text",
                    message: "檔案是空的。".into(),
                });
            }
            Ok(Staged {
                kind: if p.via == "chat" {
                    SourceKind::Attachment
                } else {
                    SourceKind::File
                },
                format,
                title: p.title.clone().unwrap_or_else(|| name.clone()),
                original_name: Some(name.clone()),
                url: None,
                bytes: bytes.len() as u64,
                sha: duduclaw_knowledge::sha256_hex(&bytes),
                preview: super::add_source_guard::preview(format, &bytes),
                staged: Some(write_sidecar(&bytes)?),
            })
        }
        Content::Text(text) => {
            if text.trim().is_empty() {
                return Err(KbError::Invalid("要加入的文字是空的。".into()));
            }
            if text.chars().count() > cfg.max_source_chars {
                return Err(KbError::Rejected {
                    code: "too_many_chars",
                    message: format!("文字超過 {} 字的上限。", cfg.max_source_chars),
                });
            }
            Ok(Staged {
                kind: SourceKind::Paste,
                format: Format::Txt,
                title: p.title.clone().unwrap_or_else(|| "員工送審的文字".into()),
                original_name: None,
                url: None,
                bytes: text.len() as u64,
                sha: duduclaw_knowledge::sha256_hex(text.as_bytes()),
                preview: super::add_source_guard::preview(Format::Txt, text.as_bytes()),
                staged: Some(write_sidecar(text.as_bytes())?),
            })
        }
        Content::Url(raw) => {
            let url = super::url_ingest::validate(raw).map_err(|e| KbError::Rejected {
                code: "url_blocked",
                message: e.message,
            })?;
            Ok(Staged {
                kind: SourceKind::Url,
                format: Format::Html,
                title: p.title.clone().unwrap_or_else(|| url.clone()),
                original_name: None,
                sha: url_identity(&url),
                url: Some(url),
                bytes: 0,
                staged: None,
                preview: None,
            })
        }
    }
}

/// File a proposal. Blocking file and database work runs off the reactor.
pub async fn propose(home: &Path, p: Proposal) -> KbResult<Proposed> {
    super::add_source_guard::sweep_unreferenced(home).await;
    super::add_source_guard::check_caps(home, &p.agent_id, &p.proposed_by).await?;
    let cfg = KnowledgeConfig::from_home(home);
    let h = home.to_path_buf();
    let (payload, title, summary, duplicate) = super::blocking(move || {
        let conn = schema::open_home(&h)?;
        let coll = duduclaw_knowledge::store::get_collection(&conn, &p.collection_id)?
            .ok_or(KbError::PermissionDenied)?;
        let staged = stage(&h, &cfg, &p)?;
        let title = duduclaw_knowledge::sources::clean_title(&staged.title);
        let existing: Option<String> = conn
            .query_row(
                "SELECT title FROM sources WHERE collection_id = ?1 AND blob_sha256 = ?2",
                rusqlite::params![coll.id, staged.sha],
                |r| r.get(0),
            )
            .ok();
        if let Some(t) = existing {
            if let Some(path) = staged.staged.as_deref().and_then(|s| sidecar_path(&h, s)) {
                let _ = std::fs::remove_file(path);
            }
            return Ok((Value::Null, t, String::new(), true));
        }
        let payload = json!({
            "collection_id": coll.id,
            "collection_slug": coll.slug,
            "collection_name": coll.name,
            "kind": staged.kind.as_str(),
            "format": staged.format.as_str(),
            "title": title,
            "original_name": staged.original_name,
            "url": staged.url,
            "bytes": staged.bytes,
            "content_sha256": staged.sha,
            "staged": staged.staged,
            "proposed_by": p.proposed_by,
            "requester_kind": p.requester_kind,
            "requester_channel": p.requester_channel,
            "preview": staged.preview,
            "via": p.via,
            "agent_id": p.agent_id,
            "origin_channel": p.origin.as_ref().map(|o| o.0.clone()),
            "origin_chat_id": p.origin.as_ref().map(|o| o.1.clone()),
            "note": CARD_NOTE,
        });
        let summary = format!(
            "加入知識庫：「{}」→ 集合「{}」（{}）",
            duduclaw_core::truncate_chars(&title, 60),
            duduclaw_core::truncate_chars(&coll.name, 40),
            if p.via == "chat" {
                "聊天中送出"
            } else {
                "AI 員工提出"
            }
        );
        Ok((payload, title, summary, false))
    })
    .await?;
    if duplicate {
        return Ok(Proposed::Duplicate { title });
    }
    let agent = payload["agent_id"].as_str().unwrap_or("").to_string();
    let broker = ApprovalBroker::open(home).map_err(|e| KbError::Io(std::io::Error::other(e)))?;
    match broker
        .request(&agent, ACTION_KIND, &summary, payload.clone(), TTL_SECS)
        .await
    {
        Ok(id) => {
            super::audit_admin(
                home,
                "knowledge_add_source_proposed",
                payload["proposed_by"].as_str().unwrap_or(""),
                json!({"approval_id": id.to_string(), "collection_id": payload["collection_id"], "title": title, "via": payload["via"]}),
            );
            Ok(Proposed::Filed {
                approval_id: id.to_string(),
                title,
            })
        }
        Err(e) => {
            remove_sidecar(home, &payload);
            Err(KbError::Io(std::io::Error::other(e)))
        }
    }
}

fn remove_sidecar(home: &Path, payload: &Value) {
    if let Some(path) = payload["staged"]
        .as_str()
        .and_then(|s| sidecar_path(home, s))
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether `user` may decide `rec` now: Admin, or editor / owner of the
/// card's collection.
pub fn may_decide(home: &Path, rec: &ApprovalRecord, user: &ResolvedUser) -> KbResult<()> {
    if user.is_admin {
        return Ok(());
    }
    let cid = rec.payload["collection_id"]
        .as_str()
        .ok_or(KbError::PermissionDenied)?;
    let conn = schema::open_home(home)?;
    super::principals::require_role(
        home,
        &conn,
        cid,
        &user.user_id,
        user.department.as_deref(),
        user.is_admin,
        Role::Editor,
    )
    .map(|_| ())
}

fn lock_for(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, Weak};
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut m = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    m.retain(|_, w| w.strong_count() > 0);
    if let Some(l) = m.get(id).and_then(Weak::upgrade) {
        return l;
    }
    let l = Arc::new(tokio::sync::Mutex::new(()));
    m.insert(id.to_string(), Arc::downgrade(&l));
    l
}

/// The refusal when the decider is neither Admin nor an editor.
pub const NOT_EDITOR: &str =
    "只有管理員或這個集合的編輯者（含擁有者）可以決定這筆加入知識庫的請求。";
/// The refusal when the staged content changed.
pub const HASH_MISMATCH: &str =
    "送審後內容已被變更（內容雜湊不符），已拒絕這筆請求，沒有匯入任何內容。";

/// Decide a proposal card in the dashboard. Side effect first, decision
/// recorded after, both under a per-card lock.
pub async fn decide(
    home: &Path,
    broker: &ApprovalBroker,
    id: &ApprovalId,
    approve: bool,
    user: &ResolvedUser,
) -> Result<Value, String> {
    let lock = lock_for(id.as_str());
    let _g = lock.lock().await;
    let rec = match broker.get(id).await {
        Ok(Some(r)) if r.action_kind == ACTION_KIND => r,
        Ok(_) => return Err("找不到這筆審核".into()),
        Err(e) => return Err(format!("讀取審核失敗：{e}")),
    };
    let (h, r, u) = (home.to_path_buf(), rec.clone(), user.clone());
    let allowed = super::blocking(move || may_decide(&h, &r, &u)).await;
    if allowed.is_err() {
        return Err(NOT_EDITOR.into());
    }
    if rec.status != ApprovalStatus::Pending {
        return Err(format!(
            "approval {} already {}",
            rec.id,
            rec.status.as_str()
        ));
    }
    if rec.is_stale(chrono::Utc::now()) {
        let _ = broker.poll(id).await;
        remove_sidecar(home, &rec.payload);
        return Err(EXPIRED_TEXT.into());
    }
    crate::approval::check_expected_digest(&rec)?;
    let decided_by = format!("dashboard:{}", user.user_id);
    if !approve {
        broker.decide(id, false, &decided_by).await?;
        remove_sidecar(home, &rec.payload);
        audit(home, &rec, user, "denied");
        return Ok(json!({"id": id.to_string(), "decided": "denied"}));
    }
    let (h, r, u) = (home.to_path_buf(), rec.clone(), user.clone());
    match super::blocking(move || ingest(&h, &r, &u)).await {
        Ok(v) => {
            broker.decide(id, true, &decided_by).await?;
            remove_sidecar(home, &rec.payload);
            audit(home, &rec, user, "approved");
            super::worker::notify();
            Ok(v)
        }
        Err(KbError::Rejected {
            code: "hash_mismatch",
            ..
        }) => {
            broker.decide(id, false, &decided_by).await?;
            remove_sidecar(home, &rec.payload);
            audit(home, &rec, user, "hash_mismatch");
            Err(HASH_MISMATCH.into())
        }
        // Anything else leaves the card pending (retryable).
        Err(e) => Err(e.public_message()),
    }
}

/// The text for a decision on an expired card.
pub const EXPIRED_TEXT: &str = "這筆加入知識庫的請求已逾期，已自動拒絕；要加入請重新送出。";

fn audit(home: &Path, rec: &ApprovalRecord, user: &ResolvedUser, outcome: &str) {
    super::audit_admin(
        home,
        "knowledge_add_source_decided",
        &user.user_id,
        json!({
            "approval_id": rec.id.to_string(),
            "collection_id": rec.payload["collection_id"],
            "title": rec.payload["title"],
            "outcome": outcome,
        }),
    );
}

fn ingest(home: &Path, rec: &ApprovalRecord, user: &ResolvedUser) -> KbResult<Value> {
    let p = &rec.payload;
    let s = |k: &str| p[k].as_str().map(str::to_string);
    let cid = s("collection_id").ok_or(KbError::PermissionDenied)?;
    let expected = s("content_sha256").ok_or(KbError::PermissionDenied)?;
    let kind = match p["kind"].as_str() {
        Some("file") => SourceKind::File,
        Some("attachment") => SourceKind::Attachment,
        Some("paste") => SourceKind::Paste,
        Some("url") => SourceKind::Url,
        _ => return Err(KbError::Invalid("審核內容格式不正確。".into())),
    };
    let format = p["format"]
        .as_str()
        .and_then(Format::parse)
        .ok_or_else(|| KbError::Invalid("審核內容格式不正確。".into()))?;
    let cfg = KnowledgeConfig::from_home(home);
    let (sha, bytes) = if kind == SourceKind::Url {
        let url = s("url").ok_or(KbError::PermissionDenied)?;
        if url_identity(&url) != expected {
            return Err(mismatch());
        }
        (expected.clone(), 0u64)
    } else {
        let path = s("staged")
            .and_then(|st| sidecar_path(home, &st))
            .ok_or_else(mismatch)?;
        let data = std::fs::read(&path).map_err(|_| mismatch())?;
        if duduclaw_knowledge::sha256_hex(&data) != expected {
            return Err(mismatch());
        }
        let sha = BlobStore::for_home(home).put(&data)?;
        (sha, data.len() as u64)
    };
    let mut conn = schema::open_home(home)?;
    let new = NewSource {
        collection_id: cid.clone(),
        kind,
        title: s("title").unwrap_or_default(),
        original_name: s("original_name"),
        url: s("url"),
        format,
        blob_sha256: sha,
        bytes,
        origin: json!({
            "proposed_by": p["proposed_by"],
            "via": p["via"],
            "approval_id": rec.id.to_string(),
            "approved_by": user.user_id,
        }),
        created_by: p["proposed_by"].as_str().unwrap_or("").to_string(),
    };
    let (source, duplicate) = match duduclaw_knowledge::sources::add_source(&mut conn, &new, &cfg)?
    {
        Added::New(s) => (s, false),
        Added::Duplicate(s) => (s, true),
    };
    let entry = duduclaw_knowledge::audit::AccessEntry {
        op: "add_source".into(),
        collection_id: Some(cid),
        agent_id: Some(rec.agent_id.clone()),
        requester_kind: "dashboard_user".into(),
        requester_id: Some(user.user_id.clone()),
        results: vec![
            json!({"source_id": source.id, "duplicate": duplicate, "approval_id": rec.id.to_string()}),
        ],
        outcome: "ok".into(),
        ..Default::default()
    };
    duduclaw_knowledge::audit::log_access(&conn, &entry)?;
    Ok(json!({
        "id": rec.id.to_string(),
        "decided": "approved",
        "source_id": source.id,
        "duplicate": duplicate,
    }))
}

fn mismatch() -> KbError {
    KbError::Rejected {
        code: "hash_mismatch",
        message: HASH_MISMATCH.into(),
    }
}

/// The plain channel notice for a pending proposal: no file name, no
/// content, only that something waits and until when.
pub(crate) fn notice_body(rec: &ApprovalRecord, reminder: bool, deadline: &str) -> String {
    let head = if reminder {
        "⏰ 有一筆加入知識庫的請求快到期了，逾時會自動拒絕"
    } else {
        "📥 有一筆加入知識庫的請求等待審核"
    };
    format!(
        "{head}\n\
         AI 員工：{agent}\n\
         這類請求只能由管理員或該集合的編輯者在儀表板的待辦清單決定。\n\
         期限：{deadline}未審核將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

#[cfg(test)]
#[path = "add_source_approval_tests.rs"]
mod tests;
