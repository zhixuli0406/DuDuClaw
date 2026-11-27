//! Proposals to add a source to a collection from outside the dashboard
//! (the `knowledge_add_source` MCP tool, the 「加入知識庫」 chat command).
//! Owner/avatar-only turns can opt into transactional, rate-limited self-add.
//!
//! - **Filing** ([`propose`]): the content is copied into a sidecar file
//!   under `<home>/knowledge/proposals/` (directory 0700, files 0600); the
//!   approval card stores metadata, the content's SHA-256, who asked (kind
//!   and channel) and, for text, a preview of the first 2,000 characters.
//!   A URL proposal stores the URL; it is fetched only after approval.
//!   Visitors, unbound senders and unregistered turns may not propose, and
//!   each employee / proposer may have only a few cards waiting
//!   ([`super::add_source_guard`]).
//! - **Deciding** ([`decide`]): dashboard, or the domain-bound Owner DM route
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
    Imported { title: String, source_id: String },
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
    propose_inner(home, p, None, None).await
}

pub(crate) async fn propose_in_turn(
    home: &Path, p: Proposal, turn_id: Option<String>, run_id: Option<String>,
) -> KbResult<Proposed> {
    let turn = super::owner_self_add::Turn { agent: p.agent_id.clone(), turn_id, run_id };
    let who = turn.requester();
    propose_inner(home, p, Some(who), Some(turn)).await
}

pub(crate) async fn propose_as(
    home: &Path, p: Proposal, who: duduclaw_knowledge::requester::RequesterIdentity,
) -> KbResult<Proposed> {
    propose_inner(home, p, Some(who), None).await
}

async fn propose_inner(
    home: &Path, p: Proposal,
    who: Option<duduclaw_knowledge::requester::RequesterIdentity>,
    turn: Option<super::owner_self_add::Turn>,
) -> KbResult<Proposed> {
    let requester = who.as_ref().and_then(super::owner_self_add::human_id).map(str::to_owned);
    let requester_for_payload = requester.clone();
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
        let mut payload = json!({
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
            "verified_requester": requester_for_payload,
        });
        super::add_source_chat::bind_owner(&h, &conn, &mut payload);
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
    if let Some(turn) = turn {
        let (h, data) = (home.to_path_buf(), payload.clone());
        let imported = super::blocking(move || {
            let mut conn = schema::open_home(&h)?;
            super::owner_self_add::attach_users(&conn, &h)?;
            let new = source_input(&h, &data, json!({"auto_import": true}))?;
            let cfg = KnowledgeConfig::from_home(&h);
            duduclaw_knowledge::sources::add_source_checked_with_effect(&mut conn, &new, &cfg, |tx| {
                if super::owner_self_add::human_id(&turn.requester()) != data["verified_requester"].as_str() {
                    return Err(KbError::PermissionDenied);
                }
                super::owner_self_add::authorize(tx, &h, &turn, &new)
            }, |tx, source| {
                let owner = data["verified_requester"].as_str().ok_or(KbError::PermissionDenied)?;
                super::owner_self_add_notify::enqueue(tx, source, owner)
            })
        }).await;
        if let Ok(added) = imported {
            remove_sidecar(home, &payload);
            super::owner_self_add_notify::notify();
            super::worker::notify();
            return Ok(match added {
                Added::New(source) => {
                    super::audit_admin(home, "knowledge_owner_self_add", &agent, json!({
                        "actor_ai": agent, "owner": requester, "collection_id": payload["collection_id"],
                        "source_id": source.id, "source_summary": title,
                    }));
                    Proposed::Imported { title, source_id: source.id }
                },
                Added::Duplicate(_) => Proposed::Duplicate { title },
            });
        }
        // No authorization/read failure may turn into an implicit allow.
    }
    let broker = ApprovalBroker::open(home).map_err(|e| KbError::Io(std::io::Error::other(e)))?;
    let principal = requester.as_deref().and_then(duduclaw_core::HumanPrincipal::user).map(|p| p.to_string());
    match crate::on_behalf_of::ON_BEHALF_OF.scope(principal,
        broker.request(&agent, ACTION_KIND, &summary, payload.clone(), TTL_SECS))
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

pub(crate) fn remove_sidecar(home: &Path, payload: &Value) {
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
    let lock = crate::approval::decision_service::decision_lock(id.as_str());
    let _guard = lock.lock().await;
    decide_inner(home, broker, id, approve, user, None, None).await
}

pub(crate) async fn decide_owner(
    home: &Path, broker: &ApprovalBroker, id: &ApprovalId, approve: bool,
    user: &ResolvedUser, context: &crate::approval::DecisionContext,
) -> Result<Value, String> {
    let lock = crate::approval::decision_service::decision_lock(id.as_str());
    let _guard = lock.lock().await;
    decide_inner(home, broker, id, approve, user, Some(context), None).await
}

/// Service-only entry: the caller holds decision_lock and has authenticated
/// ChatAdmin from live identity, configuration and the exact receipt.
/// Legacy dashboard/owner APIs above retain their signatures and share the lock.
pub(crate) async fn decide_as_locked(
    env: &crate::approval::decision_service::DecisionEnv,
    id: &ApprovalId, approve: bool,
    by: &crate::approval::decision_service::Decider,
    user: &ResolvedUser, actor: &crate::approval::chat_admin::DecisionActor,
) -> Result<Value, String> {
    if matches!(by, crate::approval::decision_service::Decider::ChatAdmin(_)) && !user.is_admin {
        return Err(NOT_EDITOR.into());
    }
    decide_inner(&env.home, &env.broker, id, approve, user, None, Some(actor)).await
}

async fn record_decision(
    broker: &ApprovalBroker, id: &ApprovalId, approve: bool, user: &ResolvedUser,
    owner: Option<&crate::approval::DecisionContext>,
    actor: Option<&crate::approval::chat_admin::DecisionActor>,
) -> Result<(), String> {
    if let Some(actor) = actor {
        // A rejected content hash records a denial even if the original
        // request was approve. Denials never require a step-up.
        if !approve && let crate::approval::chat_admin::DecisionActor::ChatAdmin(entry) = actor {
            let mut entry = entry.clone();
            entry.step_required = false;
            return broker.decide_as(id, false, &crate::approval::chat_admin::DecisionActor::ChatAdmin(entry)).await;
        }
        return broker.decide_as(id, approve, actor).await;
    }
    if let Some(ctx) = owner {
        return broker.decide(id, approve, &format!("channel:{}:{}", ctx.channel, ctx.principal_id)).await;
    }
    broker.decide_as(id, approve, &crate::approval::chat_admin::DecisionActor::Dashboard {
        user_id: user.user_id.clone(),
    }).await
}

async fn decide_inner(
    home: &Path, broker: &ApprovalBroker, id: &ApprovalId, approve: bool,
    user: &ResolvedUser, owner_context: Option<&crate::approval::DecisionContext>,
    actor: Option<&crate::approval::chat_admin::DecisionActor>,
) -> Result<Value, String> {
    let rec = match broker.get(id).await {
        Ok(Some(r)) if r.action_kind == ACTION_KIND => r,
        Ok(_) => return Err("找不到這筆審核".into()),
        Err(e) => return Err(format!("讀取審核失敗：{e}")),
    };
    if let Some(context) = owner_context {
        if super::add_source_chat::owner_user(home, &rec, context).as_ref().map(|u| &u.user_id) != Some(&user.user_id) {
            return Err(crate::channel_decision_route::DECISION_REFUSED.into());
        }
    }
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
    let decider = duduclaw_core::HumanPrincipal::user(&user.user_id).map(|p| p.to_string());
    if crate::on_behalf_of::self_approval::refuses(home, rec.on_behalf_of.as_deref(), decider.as_deref(), approve) {
        return Err(crate::on_behalf_of::self_approval::REFUSAL.into());
    }
    crate::approval::check_expected_digest(&rec)?;
    if !approve {
        record_decision(broker, id, false, user, owner_context, actor).await?;
        remove_sidecar(home, &rec.payload);
        audit(home, &rec, user, "denied");
        return Ok(json!({"id": id.to_string(), "decided": "denied"}));
    }
    let (h, r, u) = (home.to_path_buf(), rec.clone(), user.clone());
    let ingest_context = owner_context.cloned();
    match super::blocking(move || ingest(&h, &r, &u, ingest_context.as_ref())).await {
        Ok(v) => {
            record_decision(broker, id, true, user, owner_context, actor).await?;
            remove_sidecar(home, &rec.payload);
            audit(home, &rec, user, "approved");
            super::worker::notify();
            Ok(v)
        }
        Err(KbError::Rejected {
            code: "hash_mismatch",
            ..
        }) => {
            record_decision(broker, id, false, user, owner_context, actor).await?;
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

fn source_input(home: &Path, p: &Value, origin: Value) -> KbResult<NewSource> {
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
    let (sha, bytes) = if kind == SourceKind::Url {
        let url = s("url").ok_or(KbError::PermissionDenied)?;
        super::url_ingest::validate(&url).map_err(|e| KbError::Rejected { code: "url_blocked", message: e.message })?;
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
    Ok(NewSource {
        collection_id: cid.clone(),
        kind,
        title: s("title").unwrap_or_default(),
        original_name: s("original_name"),
        url: s("url"),
        format,
        blob_sha256: sha,
        bytes,
        origin,
        created_by: p["proposed_by"].as_str().unwrap_or("").to_string(),
    })
}

fn ingest(home: &Path, rec: &ApprovalRecord, user: &ResolvedUser, owner_context: Option<&crate::approval::DecisionContext>) -> KbResult<Value> {
    let p = &rec.payload;
    let new = source_input(home, p, json!({
        "proposed_by": p["proposed_by"], "via": p["via"],
        "approval_id": rec.id.to_string(), "approved_by": user.user_id,
    }))?;
    let cid = new.collection_id.clone();
    let cfg = KnowledgeConfig::from_home(home);
    let mut conn = schema::open_home(home)?;
    if owner_context.is_some() { super::owner_self_add::attach_users(&conn, home)?; }
    let (source, duplicate) = match duduclaw_knowledge::sources::add_source_checked(&mut conn, &new, &cfg, |tx| {
        if let Some(context) = owner_context {
            let db = crate::decision_notify::open_user_db_read_only(home).ok().flatten()
                .ok_or(KbError::PermissionDenied)?;
            let uid = crate::googlechat_identity::verified_user_id(home, &db, &context.channel, &context.principal_id)
                .ok().flatten();
            if uid.as_deref() != Some(user.user_id.as_str()) { return Err(KbError::PermissionDenied); }
            let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM self_add_users.users WHERE id=?1 AND status='active')",
                [&user.user_id], |r| r.get(0))?;
            if !active || !super::owner_self_add::is_direct_owner(tx, &cid, &user.user_id)? {
                return Err(KbError::PermissionDenied);
            }
        }
        Ok(())
    })?
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
         請管理員或集合編輯者至儀表板的待辦清單處理；擁有者也可使用另外收到的私訊核准卡。\n\
         期限：{deadline}未審核將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

#[cfg(test)]
#[path = "add_source_approval_tests.rs"]
mod tests;
