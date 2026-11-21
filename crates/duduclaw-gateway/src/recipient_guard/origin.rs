//! Gateway-held proof of a turn-origin notification default, re-verified at
//! send time.
//!
//! A schedule, reminder or goal an AI employee creates during a live human
//! turn may default its notifications to that turn's conversation (a bound
//! channel chat, or the signed-in WebChat conversation). Two facts make that
//! safe, and both live only in the gateway:
//!
//! * **At creation** [`mint`] / [`mint_webchat`] read the gateway's own live
//!   turn registry (who is on the other side, through which bound channel
//!   identity or dashboard account, in which generation of the conversation),
//!   check that tuple against live state, and persist it as one record of the
//!   gateway-owned [`TABLE`] in `sessions.db`, keyed by an unguessable random
//!   id ([`ID_BYTES`] bytes from the system CSPRNG). Only that opaque id
//!   crosses the signed loopback route to the MCP process; it is what the
//!   cron row, the reminder, the goal task and the goal's deferred-queue
//!   copies store. The tuple itself never leaves the gateway.
//! * **Before every send** [`verify`] loads the tuple by exact id from the
//!   gateway table — never from fields of the job row — and re-checks it:
//!   the record names this employee and exactly this destination; the
//!   session row still exists as the same generation and its employee,
//!   owner and conversation match the record; the creator's account is
//!   still Active and may still call the employee; a channel identity is
//!   still bound to that account. Any read error denies.
//!
//! ## Why `sessions.db`
//!
//! It is the gateway's turn/session store and already holds the session rows
//! [`verify`] must read (one read-only connection serves both). Its writers
//! are the gateway's `SessionManager` and the gateway-side functions of this
//! module (`mint*`, [`forget`], [`collect_garbage`]); the employee's MCP
//! process opens it read-only (channel statistics) and never writes it. A
//! tampered MCP process can therefore at most store an id the gateway handed
//! to it for the same employee's own live turn: the record behind the id
//! still names that creator, identity and conversation, so it cannot be
//! pointed at another bound user or conversation that may call the employee.
//!
//! ## Bounds and cleanup
//!
//! At most [`MAX_PROOFS`] records in total and [`MAX_PROOFS_PER_EMPLOYEE`]
//! per employee; a full store mints nothing (the job then has no default
//! origin). A record is deleted by the gateway when its job is deleted
//! through a gateway path ([`delete_cron_task`], task removal, WebChat purge
//! cancellation). Everything else — a job deleted by the MCP process, an
//! owner change that cleared the origin, a reminder that fired or was
//! cancelled, an id minted for a tool call that stored nothing — is removed
//! by [`collect_garbage`] from the single-gateway maintenance loop
//! ([`maintenance`]): a record older than [`UNREFERENCED_GRACE_SECS`] that no
//! cron row, task row or pending reminder references. A record still
//! referenced by a job never expires. Deferred-queue entries carry copies of
//! their goal task's id and do not keep a record alive: when the task is
//! gone, its queued copies are refused.
//!
//! ## Released WebChat schedules
//!
//! WebChat schedules and reminders aimed at the creator's own conversation
//! shipped in v1.81 (RM-124), before proofs existed. [`startup_backfill`]
//! mints a record for each such row once, at gateway startup and before the
//! schedulers fire, through the same WebChat mint and live checks a live turn
//! uses; a row that fails them keeps no proof and is refused
//! (`origin_unproven`). A completed pass records a completion marker in
//! `sessions.db` and no later start backfills anything. Channel origins are
//! new in v1.82 and never backfilled.
//!
//! [`verify`] is the one fire-time check shared by cron results, reminders,
//! WebChat deliveries and every goal push (progress, observer, needs-human,
//! private notice and the deferred quiet-hours / takeover queue). A denial
//! is a closed [`OriginDenial`]; callers record its [`OriginDenial::code`]
//! (delivery reason / audit, never chat text) and never fall back to
//! another conversation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

mod backfill;
pub(crate) use backfill::startup_backfill;

/// The gateway-owned proof table in `sessions.db`.
pub const TABLE: &str = "schedule_origin_proofs";

/// Idempotent schema of [`TABLE`], run by `SessionManager::init_tables` and
/// before every gateway write here. Column names deliberately avoid the
/// isolation label columns (`agent_id`, `session_id`, …): the record is not
/// employee data and carries no writer-gate trigger. The import catalog pins
/// the same text (`duduclaw-import/src/session-origin-proofs-v182.sql`).
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS schedule_origin_proofs (id TEXT PRIMARY KEY NOT NULL, employee TEXT NOT NULL, recipient TEXT NOT NULL, creator_user_id TEXT NOT NULL, identity_channel TEXT NOT NULL, identity_user_id TEXT NOT NULL, conversation TEXT NOT NULL, generation TEXT NOT NULL, minted_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_schedule_origin_proofs_employee ON schedule_origin_proofs(employee, minted_at);
";

/// Random bytes per proof id (256 bit); the id is their lowercase hex.
pub const ID_BYTES: usize = 32;
/// Longest accepted record field (bytes). A WebChat target is a session id
/// (≤ 512) plus its creation stamp.
const MAX_FIELD_BYTES: usize = 1024;
/// Hard cap on stored proof records.
pub const MAX_PROOFS: i64 = 50_000;
/// Per-employee cap, so one employee cannot exhaust the store for others.
pub const MAX_PROOFS_PER_EMPLOYEE: i64 = 5_000;
/// An unreferenced record is kept this long after minting: the tool call
/// that asked for it writes its job right after.
pub const UNREFERENCED_GRACE_SECS: i64 = 3600;
/// Largest `reminders.jsonl` the collector reads (same bound as the
/// isolated reminder reader); a larger file aborts the pass.
const MAX_REMINDER_BYTES: u64 = 8 * 1024 * 1024;

/// The gateway's copy of who created a turn-origin default. Loaded only from
/// [`TABLE`]; never serialized to the MCP process or a job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginProof {
    /// Opaque record id (what jobs store).
    pub id: String,
    /// The employee whose live turn created the job.
    pub agent: String,
    /// The exact destination the proof covers: the normalized channel
    /// recipient (`<channel>:<chat>`), or the stamped WebChat target.
    pub recipient: String,
    /// The dashboard account behind the turn.
    pub user_id: String,
    /// The channel of the bound identity (`googlechat`, `line`, …), or
    /// `webchat` for a signed-in WebChat conversation.
    pub channel: String,
    /// The bound channel sender id; for WebChat the account id again.
    pub channel_user_id: String,
    /// The host session id of the turn.
    pub session: String,
    /// The session row's `created_at`: a cleared or recreated conversation
    /// gets a new one, so an old record no longer matches.
    pub generation: String,
    /// Unix seconds.
    pub minted_at: i64,
}

impl OriginProof {
    fn is_webchat(&self) -> bool {
        self.channel == crate::scheduled_webchat::CHANNEL
    }

    fn well_formed(&self) -> bool {
        let fields = [&self.agent, &self.recipient, &self.user_id, &self.channel,
            &self.channel_user_id, &self.session, &self.generation];
        valid_id(&self.id)
            && duduclaw_core::is_valid_agent_id(&self.agent)
            && fields.iter().all(|field| !field.is_empty() && field.len() <= MAX_FIELD_BYTES
                && !field.chars().any(char::is_control))
    }
}

/// Why a turn-origin destination was refused at send time. Closed set: the
/// [`Self::code`] is what delivery records (`last_delivery_reason`, reminder
/// `error`, audit rows) carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginDenial {
    /// The target is the stored origin but no proof id is stored (legacy
    /// row, the turn could not be proven), or a queued goal card has no task
    /// (deleted) or no task id to re-check.
    NoProof,
    /// The proof id is malformed or unknown to the gateway, or the gateway's
    /// record names another employee, destination, owner or conversation.
    ProofInvalid,
    /// A public-visitor conversation (website widget, Live Chat), which
    /// never receives scheduled operator-facing pushes.
    PublicVisitor,
    /// The proof, identity, account or conversation store could not be read.
    Unreadable,
    /// The creator's account is disabled, suspended or gone.
    AccountInactive,
    /// The creator may no longer call this employee.
    AccessRevoked,
    /// The creator's channel identity is no longer bound to the account.
    IdentityUnbound,
    /// The conversation was cleared, deleted or recreated since creation.
    ConversationChanged,
}

impl OriginDenial {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoProof => "origin_unproven",
            Self::ProofInvalid => "origin_proof_invalid",
            Self::PublicVisitor => "origin_public_visitor",
            Self::Unreadable => "origin_state_unreadable",
            Self::AccountInactive => "origin_account_inactive",
            Self::AccessRevoked => "origin_access_revoked",
            Self::IdentityUnbound => "origin_identity_unbound",
            Self::ConversationChanged => "origin_conversation_changed",
        }
    }

    /// zh-TW operator-facing explanation (logs, reminder error, run summary).
    pub fn message(self) -> &'static str {
        match self {
            Self::NoProof => "排程通知的來源對話沒有 gateway 證明，已停止傳送到該對話。",
            Self::ProofInvalid => "排程通知的來源證明無效或不屬於這位員工，已停止傳送。",
            Self::PublicVisitor => "來源是公開訪客對話，排程通知不會推播到這裡。",
            Self::Unreadable => "無法讀取帳號或對話狀態，已停止傳送排程通知。",
            Self::AccountInactive => "建立者的帳號已停用或不存在，已停止傳送排程通知。",
            Self::AccessRevoked => "建立者已無權使用這位 AI 員工，已停止傳送排程通知。",
            Self::IdentityUnbound => "建立者的通道身分已解除綁定，已停止傳送排程通知。",
            Self::ConversationChanged => "來源對話已被清除或重建，已停止傳送排程通知。",
        }
    }
}

impl std::fmt::Display for OriginDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

/// A conversation that never receives a scheduled channel push:
/// public-visitor channels (exact channel match, also as the session scheme)
/// and WebChat (dashboard results are appended to the session through its
/// own proof kind, never pushed; the public widget is WebChat too).
pub fn refused_conversation(channel: &str, conversation: &str) -> bool {
    let scheme = conversation.split_once(':').map_or(conversation, |(scheme, _)| scheme);
    [channel, scheme].into_iter().any(|name| {
        duduclaw_knowledge::requester::is_public_visitor_channel(name)
            || name == crate::scheduled_webchat::CHANNEL
    })
}

/// Exact shape of a proof id: [`ID_BYTES`] bytes as lowercase hex.
fn valid_id(id: &str) -> bool {
    id.len() == ID_BYTES * 2 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A fresh id from the system CSPRNG (`ring::rand::SystemRandom`, as the
/// session and invitation tokens). `None` when the RNG is unavailable.
fn new_id() -> Option<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; ID_BYTES];
    ring::rand::SystemRandom::new().fill(&mut bytes).ok()?;
    Some(hex::encode(bytes))
}

fn sessions_path(home: &Path) -> PathBuf {
    home.join("sessions.db")
}

/// Read-only connection to the gateway's `sessions.db`. Missing ⇒ unreadable.
fn open_read(home: &Path) -> Result<Connection, OriginDenial> {
    let path = sessions_path(home);
    if !path.is_file() {
        return Err(OriginDenial::Unreadable);
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| OriginDenial::Unreadable)?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| OriginDenial::Unreadable)?;
    Ok(conn)
}

/// Gateway write connection. The file must already exist (the gateway's
/// `SessionManager` creates it); this never creates a database.
fn open_write(home: &Path) -> Result<Connection, String> {
    let path = sessions_path(home);
    if !path.is_file() {
        return Err("sessions.db is missing".into());
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| format!("open sessions.db: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    conn.execute_batch(SCHEMA).map_err(|e| format!("origin proof schema: {e}"))?;
    Ok(conn)
}

/// Create [`TABLE`] (called from `SessionManager::init_tables`).
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

/// The gateway's record `id`, exact match.
fn load(conn: &Connection, id: &str) -> Result<OriginProof, OriginDenial> {
    let proof = conn
        .query_row(
            "SELECT id, employee, recipient, creator_user_id, identity_channel, identity_user_id,
                    conversation, generation, minted_at
             FROM schedule_origin_proofs WHERE id = ?1",
            params![id],
            |r| Ok(OriginProof {
                id: r.get(0)?, agent: r.get(1)?, recipient: r.get(2)?, user_id: r.get(3)?,
                channel: r.get(4)?, channel_user_id: r.get(5)?, session: r.get(6)?,
                generation: r.get(7)?, minted_at: r.get(8)?,
            }),
        )
        .optional()
        .map_err(|_| OriginDenial::Unreadable)?
        .ok_or(OriginDenial::ProofInvalid)?;
    if proof.id != id || !proof.well_formed() {
        return Err(OriginDenial::ProofInvalid);
    }
    Ok(proof)
}

/// The live session row a record describes.
struct ConversationRow {
    agent: String,
    kind: Option<String>,
    owner: Option<String>,
    created_at: String,
    /// A user clear left at its original id (never deliverable).
    stuck_clear: bool,
}

fn conversation_row(conn: &Connection, session: &str) -> Result<Option<ConversationRow>, OriginDenial> {
    conn.query_row(
        &format!(
            "SELECT agent_id, origin_kind, owner_user_id, created_at, {} FROM sessions WHERE id = ?1",
            crate::session::STUCK_USER_CLEAR
        ),
        params![session],
        |r| Ok(ConversationRow {
            agent: r.get(0)?, kind: r.get(1)?, owner: r.get(2)?, created_at: r.get(3)?,
            stuck_clear: r.get(4)?,
        }),
    )
    .optional()
    .map_err(|_| OriginDenial::Unreadable)
}

/// The one fire-time check for a turn-origin destination. `recipient` is the
/// destination about to receive the message (normalized channel recipient,
/// or the stamped WebChat target); `proof_id` the opaque id stored with the
/// job. `Ok` (the gateway's record) only when every live fact still holds.
pub fn verify(
    home: &Path, agent: &str, recipient: &str, proof_id: Option<&str>,
) -> Result<OriginProof, OriginDenial> {
    let id = proof_id.filter(|id| !id.is_empty()).ok_or(OriginDenial::NoProof)?;
    if !valid_id(id) {
        return Err(OriginDenial::ProofInvalid);
    }
    let conn = open_read(home)?;
    let proof = load(&conn, id)?;
    check(home, &conn, agent, recipient, &proof)?;
    Ok(proof)
}

/// Live checks of one record (shared by [`verify`] and minting).
fn check(
    home: &Path, conn: &Connection, agent: &str, recipient: &str, proof: &OriginProof,
) -> Result<(), OriginDenial> {
    if !proof.well_formed() {
        return Err(OriginDenial::ProofInvalid);
    }
    if crate::webchat::is_widget_visitor(&proof.user_id)
        || crate::webchat::is_widget_visitor(&proof.channel_user_id)
        || (!proof.is_webchat()
            && (refused_conversation(&proof.channel, &proof.session)
                || refused_conversation(&proof.channel, &proof.recipient)))
    {
        return Err(OriginDenial::PublicVisitor);
    }
    if proof.agent != agent || proof.recipient != recipient {
        return Err(OriginDenial::ProofInvalid);
    }
    let row = conversation_row(conn, &proof.session)?.ok_or(OriginDenial::ConversationChanged)?;
    if row.stuck_clear || row.created_at != proof.generation {
        return Err(OriginDenial::ConversationChanged);
    }
    if row.agent != agent {
        return Err(OriginDenial::ProofInvalid);
    }
    if proof.is_webchat() { check_webchat_row(agent, proof, &row)? } else { check_channel_row(proof, &row)? }
    let db = crate::decision_notify::open_user_db(home)
        .map_err(|_| OriginDenial::Unreadable)?
        .ok_or(OriginDenial::Unreadable)?;
    check_account(home, &db, &proof.user_id, agent)?;
    if !proof.is_webchat() {
        match db.find_verified_user_id_by_channel(&proof.channel, &proof.channel_user_id) {
            Ok(Some(user)) if user == proof.user_id => {}
            Ok(_) => return Err(OriginDenial::IdentityUnbound),
            Err(_) => return Err(OriginDenial::Unreadable),
        }
    }
    Ok(())
}

/// A channel record: the row is a channel conversation (no dashboard owner)
/// whose delivery address is exactly the record's recipient.
fn check_channel_row(proof: &OriginProof, row: &ConversationRow) -> Result<(), OriginDenial> {
    use crate::session_origin::OriginKind;
    match row.kind.as_deref().map(OriginKind::parse) {
        Some(OriginKind::Channel) => {}
        Some(OriginKind::WebchatVisitor | OriginKind::PublicVisitor) => return Err(OriginDenial::PublicVisitor),
        _ => return Err(OriginDenial::ProofInvalid),
    }
    if row.owner.as_deref().is_some_and(|owner| !owner.is_empty()) {
        return Err(OriginDenial::ProofInvalid);
    }
    if duduclaw_core::recipient_policy::normalize_recipient(&proof.session).as_deref() != Some(proof.recipient.as_str()) {
        return Err(OriginDenial::ProofInvalid);
    }
    Ok(())
}

/// A WebChat record: a signed-in account's conversation owned by the
/// record's account (owner tag in the id), answered by `agent`, and the
/// record's recipient is exactly this session stamped with its generation.
fn check_webchat_row(agent: &str, proof: &OriginProof, row: &ConversationRow) -> Result<(), OriginDenial> {
    use crate::session_origin::OriginKind;
    match row.kind.as_deref().map(OriginKind::parse) {
        Some(OriginKind::WebchatUser) => {}
        Some(OriginKind::WebchatVisitor | OriginKind::PublicVisitor) => return Err(OriginDenial::PublicVisitor),
        _ => return Err(OriginDenial::ProofInvalid),
    }
    if row.owner.as_deref() != Some(proof.user_id.as_str()) || proof.channel_user_id != proof.user_id {
        return Err(OriginDenial::ProofInvalid);
    }
    let (tag, segment) = crate::scheduled_webchat::parse(&proof.session).ok_or(OriginDenial::ProofInvalid)?;
    if tag != crate::webchat::webchat_owner_tag(&proof.user_id) || segment.is_some_and(|segment| segment != agent) {
        return Err(OriginDenial::ProofInvalid);
    }
    if crate::scheduled_webchat::target_parts(&proof.recipient)
        != (proof.session.as_str(), Some(proof.generation.as_str()))
    {
        return Err(OriginDenial::ProofInvalid);
    }
    Ok(())
}

/// Active account that may still call `agent` — the same live computation
/// (`load_account_with_edition` + `may_call`) WebChat delivery uses, plus the
/// admin-only restriction recipient ownership applies to bound presets.
fn check_account(
    home: &Path, db: &duduclaw_auth::UserDb, user: &str, agent: &str,
) -> Result<(), OriginDenial> {
    let (access, _edition) = crate::trigger_route::access::load_account_with_edition(home, db, user)
        .map_err(|_| OriginDenial::Unreadable)?;
    let access = access.ok_or(OriginDenial::AccountInactive)?;
    if access.role == duduclaw_auth::UserRole::Admin {
        return Ok(());
    }
    if crate::machine_employee::requires_admin_for_id(home, agent) || !access.may_call(agent) {
        return Err(OriginDenial::AccessRevoked);
    }
    Ok(())
}

/// Creation-time proof for `agent`'s live turn `turn` whose source recipient
/// is `recipient`. Only a channel sender with a verified binding qualifies
/// (WebChat uses [`mint_webchat`]; unbound and public visitors have no
/// source at all). The tuple must already pass the live checks: a creator
/// who could not receive the result now gets no default origin. Returns the
/// opaque id of the persisted record.
pub(crate) fn mint(home: &Path, agent: &str, turn: &str, recipient: &str) -> Option<String> {
    use duduclaw_knowledge::requester::RequesterIdentity;
    let RequesterIdentity::ChannelUser { channel, sender, user_id: Some(user_id), .. } =
        crate::knowledge::turn_requesters::lookup(agent, turn)?
    else {
        return None;
    };
    let session = crate::turn_context::session_for(agent, turn)?;
    mint_checked(home, OriginProof {
        id: String::new(),
        agent: agent.to_owned(),
        recipient: recipient.to_owned(),
        user_id,
        channel,
        channel_user_id: sender,
        session,
        generation: String::new(),
        minted_at: 0,
    })
}

/// Creation-time proof for the signed-in WebChat conversation `target` (the
/// stamped target from `scheduled_webchat::bind_origin`) of `agent`'s live
/// turn `turn`. The account is the session row's owner; a turn registered
/// for anyone but that dashboard account gets none.
pub(crate) fn mint_webchat(home: &Path, agent: &str, turn: &str, target: &str) -> Option<String> {
    use duduclaw_knowledge::requester::RequesterIdentity;
    let claimed = match crate::knowledge::turn_requesters::lookup(agent, turn) {
        None => None,
        Some(RequesterIdentity::DashboardUser { user_id }) => Some(user_id),
        Some(_) => return None,
    };
    match try_mint_webchat(home, agent, target, claimed.as_deref()) {
        Ok(id) => Some(id),
        Err(MintRefusal::Denied(denial)) => {
            tracing::debug!(agent = %agent, reason = denial.code(), "turn origin not proven; no default destination");
            None
        }
        Err(refusal) => {
            tracing::warn!(agent = %agent, reason = refusal.code(), "turn origin proof not stored; no default destination");
            None
        }
    }
}

/// Why a mint persisted no record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MintRefusal {
    /// A live check failed, or live state could not be read
    /// ([`OriginDenial::Unreadable`]).
    Denied(OriginDenial),
    /// [`MAX_PROOFS`] / [`MAX_PROOFS_PER_EMPLOYEE`] reached.
    Full,
    /// No id could be generated or the record could not be written.
    Unwritable,
}

impl MintRefusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Denied(denial) => denial.code(),
            Self::Full => "origin_proof_store_full",
            Self::Unwritable => "origin_proof_unwritable",
        }
    }
}

/// The WebChat mint shared by a live turn ([`mint_webchat`]) and the
/// startup backfill of legacy rows (`backfill`): the account is the session
/// row's owner, and when `claimed` names an account (the live turn's
/// dashboard requester, or the creator a legacy row recorded) it must be
/// exactly that owner. Then the same [`try_mint_checked`] live checks.
pub(crate) fn try_mint_webchat(
    home: &Path, agent: &str, target: &str, claimed: Option<&str>,
) -> Result<String, MintRefusal> {
    let (session, _) = crate::scheduled_webchat::target_parts(target);
    let owner = {
        let conn = open_read(home).map_err(MintRefusal::Denied)?;
        conversation_row(&conn, session)
            .map_err(MintRefusal::Denied)?
            .ok_or(MintRefusal::Denied(OriginDenial::ConversationChanged))?
            .owner
            .filter(|owner| !owner.is_empty())
            .ok_or(MintRefusal::Denied(OriginDenial::ProofInvalid))?
    };
    if claimed.is_some_and(|claimed| claimed != owner) {
        return Err(MintRefusal::Denied(OriginDenial::ProofInvalid));
    }
    try_mint_checked(home, OriginProof {
        id: String::new(),
        agent: agent.to_owned(),
        recipient: target.to_owned(),
        user_id: owner.clone(),
        channel: crate::scheduled_webchat::CHANNEL.to_owned(),
        channel_user_id: owner,
        session: session.to_owned(),
        generation: String::new(),
        minted_at: 0,
    })
}

/// Stamp the live generation, run the live checks and persist.
fn mint_checked(home: &Path, draft: OriginProof) -> Option<String> {
    let agent = draft.agent.clone();
    match try_mint_checked(home, draft) {
        Ok(id) => Some(id),
        Err(MintRefusal::Denied(denial)) => {
            tracing::debug!(agent = %agent, reason = denial.code(), "turn origin not proven; no default destination");
            None
        }
        Err(refusal) => {
            tracing::warn!(agent = %agent, reason = refusal.code(), "turn origin proof not stored; no default destination");
            None
        }
    }
}

/// [`mint_checked`] with the closed reason of a refusal.
fn try_mint_checked(home: &Path, mut draft: OriginProof) -> Result<String, MintRefusal> {
    let conn = open_read(home).map_err(MintRefusal::Denied)?;
    draft.generation = conversation_row(&conn, &draft.session)
        .map_err(MintRefusal::Denied)?
        .ok_or(MintRefusal::Denied(OriginDenial::ConversationChanged))?
        .created_at;
    draft.id = new_id().ok_or(MintRefusal::Unwritable)?;
    draft.minted_at = chrono::Utc::now().timestamp();
    let (agent, recipient) = (draft.agent.clone(), draft.recipient.clone());
    check(home, &conn, &agent, &recipient, &draft).map_err(MintRefusal::Denied)?;
    drop(conn);
    persist(home, &draft)?;
    Ok(draft.id)
}

/// Insert one record, refusing beyond the caps (one IMMEDIATE transaction).
fn persist(home: &Path, proof: &OriginProof) -> Result<(), MintRefusal> {
    persist_inner(home, proof).map_err(|error| match error {
        PersistError::Full => MintRefusal::Full,
        PersistError::Failed(error) => {
            tracing::debug!(%error, "origin proof record not written");
            MintRefusal::Unwritable
        }
    })
}

enum PersistError {
    Full,
    Failed(String),
}

impl From<String> for PersistError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

fn persist_inner(home: &Path, proof: &OriginProof) -> Result<(), PersistError> {
    if !proof.well_formed() {
        return Err(PersistError::Failed("malformed origin proof".into()));
    }
    let mut conn = open_write(home)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let (total, mine): (i64, i64) = tx
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(employee = ?1), 0) FROM schedule_origin_proofs",
            params![proof.agent],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if total >= MAX_PROOFS || mine >= MAX_PROOFS_PER_EMPLOYEE {
        return Err(PersistError::Full);
    }
    tx.execute(
        "INSERT INTO schedule_origin_proofs (id, employee, recipient, creator_user_id, identity_channel,
             identity_user_id, conversation, generation, minted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![proof.id, proof.agent, proof.recipient, proof.user_id, proof.channel,
            proof.channel_user_id, proof.session, proof.generation, proof.minted_at],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| PersistError::Failed(e.to_string()))
}

/// Delete the records `ids` (gateway side: their job was deleted or its
/// origin cleared). Best effort; a failure is logged and the maintenance
/// collector removes the then-unreferenced record later.
pub(crate) fn forget(home: &Path, ids: &[String]) {
    let ids: Vec<&String> = ids.iter().filter(|id| valid_id(id)).collect();
    if ids.is_empty() || !sessions_path(home).is_file() {
        return;
    }
    let result = open_write(home).and_then(|conn| {
        for id in ids {
            conn.execute("DELETE FROM schedule_origin_proofs WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    if let Err(error) = result {
        tracing::warn!(%error, "origin proof record not deleted; the maintenance pass retries");
    }
}

/// [`forget`] off the async runtime.
pub(crate) async fn forget_async(home: &Path, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let home = home.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || forget(&home, &ids)).await;
}

/// Gateway deletion of one cron job together with its proof record.
pub(crate) async fn delete_cron_task(
    home: &Path, store: &crate::cron_store::CronStore, id: &str,
) -> Result<bool, String> {
    let proof = store.get(id).await?.and_then(|row| row.notify_origin_proof);
    let deleted = store.delete(id).await?;
    if deleted {
        forget_async(home, proof.into_iter().collect()).await;
    }
    Ok(deleted)
}

/// Remove every record older than [`UNREFERENCED_GRACE_SECS`] that no cron
/// row, task row or pending reminder references. Any unreadable reference
/// source aborts the pass before anything is deleted. Returns the number of
/// removed records.
pub fn collect_garbage(home: &Path) -> Result<usize, String> {
    if !sessions_path(home).is_file() {
        return Ok(0);
    }
    let referenced = referenced_ids(home)?;
    let mut conn = open_write(home)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let cutoff = chrono::Utc::now().timestamp() - UNREFERENCED_GRACE_SECS;
    let candidates: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM schedule_origin_proofs WHERE minted_at < ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![cutoff], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())?
    };
    let mut removed = 0;
    for id in candidates.iter().filter(|id| !referenced.contains(id.as_str())) {
        removed += tx.execute("DELETE FROM schedule_origin_proofs WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(removed)
}

/// One maintenance pass; only the process holding the gateway lock works.
pub fn maintenance(home: &Path) {
    if !duduclaw_core::gateway_instance::held(home) {
        return;
    }
    match collect_garbage(home) {
        Ok(0) => {}
        Ok(removed) => tracing::info!(removed, "unreferenced origin proof records removed"),
        Err(error) => tracing::warn!(%error, "origin proof cleanup skipped"),
    }
}

/// Every proof id a job still references.
fn referenced_ids(home: &Path) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    column_ids(&home.join("cron_tasks.db"),
        "SELECT notify_origin_proof FROM cron_tasks WHERE notify_origin_proof IS NOT NULL", &mut ids)?;
    column_ids(&home.join("tasks.db"),
        "SELECT source_origin_proof FROM tasks WHERE source_origin_proof IS NOT NULL", &mut ids)?;
    reminder_ids(&home.join("reminders.jsonl"), &mut ids)?;
    Ok(ids)
}

fn column_ids(path: &Path, sql: &str, ids: &mut HashSet<String>) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
    for row in rows {
        ids.insert(row.map_err(|e| e.to_string())?);
    }
    Ok(())
}

/// Ids of pending reminders (a delivered, failed or cancelled reminder never
/// sends again). An unparseable line aborts the pass.
fn reminder_ids(path: &Path, ids: &mut HashSet<String>) -> Result<(), String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let mut content = String::new();
    file.take(MAX_REMINDER_BYTES + 1).read_to_string(&mut content).map_err(|e| e.to_string())?;
    if content.len() as u64 > MAX_REMINDER_BYTES {
        return Err("reminder storage exceeds bound".into());
    }
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let pending = value.get("status").and_then(serde_json::Value::as_str).is_none_or(|status| status == "pending");
        if let Some(id) = value.get("notify_origin_proof").and_then(serde_json::Value::as_str).filter(|_| pending) {
            ids.insert(id.to_owned());
        }
    }
    Ok(())
}

/// Record a refused origin delivery (warn + tool-call audit row). Content
/// free: only the closed reason and the job id.
pub fn audit_refusal(home: &Path, agent: &str, source: &str, id: &str, denial: OriginDenial) {
    tracing::warn!(agent, source, id, reason = denial.code(), "turn-origin delivery refused");
    duduclaw_security::audit::append_tool_call_with_extras(
        home,
        agent,
        "scheduled_notification",
        &format!("origin delivery refused: {}", denial.code()),
        false,
        &[
            ("recipient_guard", serde_json::json!(denial.code())),
            ("source", serde_json::json!(source)),
            ("id", serde_json::json!(id)),
        ],
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use duduclaw_auth::{AccessLevel, UserDb, UserRole, UserStatus};
    use crate::session_origin::{OriginKind, SessionOrigin};

    pub(crate) const RECIPIENT: &str = "googlechat:spaces/ORIGIN";
    pub(crate) const SESSION: &str = "googlechat:spaces/ORIGIN";
    pub(crate) const SENDER: &str = "users/42";

    pub(crate) struct Fixture {
        pub home: tempfile::TempDir,
        pub user: String,
        /// Opaque id of the gateway record for (`agent`, [`RECIPIENT`]).
        pub proof: String,
    }

    /// A channel session row of `agent` at `session`.
    pub(crate) async fn channel_session(home: &Path, session: &str, agent: &str) {
        crate::session::SessionManager::new(&home.join("sessions.db")).unwrap()
            .get_or_create_with_origin(session, agent, &SessionOrigin::of_kind(OriginKind::Channel)).await.unwrap();
    }

    /// The tuple a live Google Chat turn of `user` in `session` would carry.
    pub(crate) fn draft(home: &Path, agent: &str, recipient: &str, session: &str, user: &str) -> OriginProof {
        let generation = open_read(home).ok().and_then(|conn| conversation_row(&conn, session).ok().flatten())
            .map(|row| row.created_at).unwrap_or_else(|| "none".into());
        OriginProof {
            id: String::new(), agent: agent.into(), recipient: recipient.into(), user_id: user.into(),
            channel: "googlechat".into(), channel_user_id: SENDER.into(), session: session.into(),
            generation, minted_at: 0,
        }
    }

    /// Store `proof` as the gateway would, without the live checks (so a
    /// test can plant records for other employees, recipients or rows).
    pub(crate) fn insert_for_test(home: &Path, mut proof: OriginProof) -> String {
        proof.id = new_id().unwrap();
        proof.minted_at = chrono::Utc::now().timestamp();
        persist(home, &proof).unwrap();
        proof.id
    }

    fn age(home: &Path, id: &str, seconds: i64) {
        rusqlite::Connection::open(home.join("sessions.db")).unwrap()
            .execute("UPDATE schedule_origin_proofs SET minted_at = minted_at - ?2 WHERE id = ?1", params![id, seconds])
            .unwrap();
    }

    fn stored(home: &Path, id: &str) -> bool {
        rusqlite::Connection::open(home.join("sessions.db")).unwrap()
            .query_row("SELECT EXISTS(SELECT 1 FROM schedule_origin_proofs WHERE id = ?1)", params![id], |r| r.get(0))
            .unwrap()
    }

    /// An Employee account bound (Operator) to `agent`, with a verified
    /// Google Chat identity, a live channel session row and its record.
    pub(crate) async fn fixture(agent: &str) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("origin@test.invalid", "Origin", "fixture-password", UserRole::Employee).unwrap();
        db.bind_agent(&user.id, agent, AccessLevel::Operator).unwrap();
        db.bind_channel_identity(&user.id, "googlechat", SENDER, true).unwrap();
        channel_session(home.path(), SESSION, agent).await;
        let proof = insert_for_test(home.path(), draft(home.path(), agent, RECIPIENT, SESSION, &user.id));
        Fixture { home, user: user.id, proof }
    }

    #[tokio::test]
    async fn live_proof_passes_and_forged_or_unknown_ids_never_do() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let record = verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap();
        assert_eq!((record.user_id.as_str(), record.channel_user_id.as_str()), (f.user.as_str(), SENDER));
        // (f) A legacy row carries no id: no origin exemption.
        assert_eq!(verify(h, "sales", RECIPIENT, None).unwrap_err(), OriginDenial::NoProof);
        assert_eq!(verify(h, "sales", RECIPIENT, Some("")).unwrap_err(), OriginDenial::NoProof);
        // A round-1 style JSON tuple, a truncated, an upper-cased (ids are
        // compared exactly) and a well-formed but unknown id.
        let tuple = format!("{{\"v\":1,\"agent\":\"sales\",\"recipient\":\"{RECIPIENT}\"}}");
        let upper = f.proof.to_ascii_uppercase();
        let short = f.proof.chars().take(ID_BYTES).collect::<String>();
        let unknown = new_id().unwrap();
        for forged in [tuple.as_str(), upper.as_str(), short.as_str(), unknown.as_str()] {
            assert_eq!(verify(h, "sales", RECIPIENT, Some(forged)).unwrap_err(), OriginDenial::ProofInvalid, "{forged}");
        }
        // The same id presented by another employee or for another destination.
        assert_eq!(verify(h, "hr", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::ProofInvalid);
        assert_eq!(verify(h, "sales", "googlechat:spaces/OTHER", Some(&f.proof)).unwrap_err(), OriginDenial::ProofInvalid);
    }

    // A row carrying a valid id that the gateway minted for a different
    // employee or a different recipient is refused.
    #[tokio::test]
    async fn valid_id_minted_for_another_employee_or_recipient_is_denied() {
        let f = fixture("sales").await;
        let h = f.home.path();
        UserDb::new(&h.join("users.db")).unwrap().bind_agent(&f.user, "hr", AccessLevel::Operator).unwrap();
        channel_session(h, "googlechat:spaces/HR", "hr").await;
        channel_session(h, "googlechat:spaces/OTHER", "sales").await;
        let hr = insert_for_test(h, draft(h, "hr", "googlechat:spaces/HR", "googlechat:spaces/HR", &f.user));
        let other = insert_for_test(h, draft(h, "sales", "googlechat:spaces/OTHER", "googlechat:spaces/OTHER", &f.user));
        assert!(verify(h, "hr", "googlechat:spaces/HR", Some(&hr)).is_ok(), "control: valid for its own tuple");
        assert!(verify(h, "sales", "googlechat:spaces/OTHER", Some(&other)).is_ok(), "control");
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&hr)).unwrap_err(), OriginDenial::ProofInvalid);
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&other)).unwrap_err(), OriginDenial::ProofInvalid);
    }

    // The session row's employee, owner and conversation must match the
    // gateway record, not only its generation.
    #[tokio::test]
    async fn session_row_agent_owner_or_conversation_mismatch_is_denied() {
        let f = fixture("sales").await;
        let h = f.home.path();
        // A record naming `sales` for a conversation whose row belongs to `hr`.
        channel_session(h, "googlechat:spaces/HR", "hr").await;
        let foreign = insert_for_test(h, draft(h, "sales", "googlechat:spaces/HR", "googlechat:spaces/HR", &f.user));
        assert_eq!(verify(h, "sales", "googlechat:spaces/HR", Some(&foreign)).unwrap_err(), OriginDenial::ProofInvalid);
        // A channel record whose row is a signed-in account's conversation (owner set).
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.get_or_create_with_origin("googlechat:spaces/OWNED", "sales", &SessionOrigin::webchat_user(&f.user))
            .await.unwrap();
        let owned = insert_for_test(h, draft(h, "sales", "googlechat:spaces/OWNED", "googlechat:spaces/OWNED", &f.user));
        assert_eq!(verify(h, "sales", "googlechat:spaces/OWNED", Some(&owned)).unwrap_err(), OriginDenial::ProofInvalid);
        // A record whose recipient is not the conversation of its session row.
        let moved = insert_for_test(h, draft(h, "sales", "googlechat:spaces/ELSEWHERE", SESSION, &f.user));
        assert_eq!(verify(h, "sales", "googlechat:spaces/ELSEWHERE", Some(&moved)).unwrap_err(), OriginDenial::ProofInvalid);
        assert!(verify(h, "sales", RECIPIENT, Some(&f.proof)).is_ok(), "the matching record still passes");
    }

    // (b) Creator disabled / unbound / identity revoked / conversation changed.
    #[tokio::test]
    async fn creator_state_is_re_read_before_every_send() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let db = UserDb::new(&h.join("users.db")).unwrap();
        db.set_user_status(&f.user, UserStatus::Suspended).unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::AccountInactive);
        db.set_user_status(&f.user, UserStatus::Active).unwrap();
        db.unbind_agent(&f.user, "sales").unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::AccessRevoked);
        db.bind_agent(&f.user, "sales", AccessLevel::Operator).unwrap();
        assert!(verify(h, "sales", RECIPIENT, Some(&f.proof)).is_ok());
        db.unbind_channel_identity("googlechat", SENDER).unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::IdentityUnbound);
        db.bind_channel_identity(&f.user, "googlechat", SENDER, true).unwrap();
        assert!(verify(h, "sales", RECIPIENT, Some(&f.proof)).is_ok());
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.delete_session(SESSION).await.unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::ConversationChanged);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        manager.get_or_create_with_origin(SESSION, "sales", &SessionOrigin::of_kind(OriginKind::Channel)).await.unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::ConversationChanged,
            "a recreated conversation is a new generation");
    }

    // (c) Store read errors deny.
    #[tokio::test]
    async fn unreadable_identity_or_conversation_store_denies() {
        let f = fixture("sales").await;
        let h = f.home.path();
        std::fs::rename(h.join("sessions.db"), h.join("sessions.moved")).unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::Unreadable);
        std::fs::rename(h.join("sessions.moved"), h.join("sessions.db")).unwrap();
        for name in ["users.db-wal", "users.db-shm"] { let _ = std::fs::remove_file(h.join(name)); }
        std::fs::write(h.join("users.db"), b"not a database").unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::Unreadable);
        std::fs::remove_file(h.join("users.db")).unwrap();
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::Unreadable);
    }

    // (e) Public-visitor origins are refused explicitly.
    #[tokio::test]
    async fn public_visitor_origins_are_refused() {
        let f = fixture("sales").await;
        let h = f.home.path();
        for change in [
            (|p: &mut OriginProof| p.channel = "odoolivechat".into()) as fn(&mut OriginProof),
            |p| p.channel = "widget".into(),
            |p| p.session = "odoolivechat:visitor-1".into(),
            |p| p.session = "webchat:widget-visitor:1".into(),
            |p| p.user_id = "widget-visitor:1".into(),
        ] {
            let mut proof = draft(h, "sales", RECIPIENT, SESSION, &f.user);
            change(&mut proof);
            let id = insert_for_test(h, proof);
            assert_eq!(verify(h, "sales", RECIPIENT, Some(&id)).unwrap_err(), OriginDenial::PublicVisitor);
        }
        assert!(refused_conversation("odoolivechat", "x"));
        assert!(refused_conversation("telegram", "odoolivechat:x"));
        assert!(refused_conversation("webchat", "webchat:webchat:0123456789ab:c"));
        assert!(!refused_conversation("telegram", "12345"));
        assert!(!refused_conversation("googlechat", "spaces/odoolivechat"));
    }

    #[tokio::test]
    async fn mint_requires_a_bound_live_channel_turn_and_persists_the_tuple() {
        use duduclaw_knowledge::requester::RequesterIdentity;
        let f = fixture("mint-sales").await;
        let h = f.home.path();
        let turn = "mint-turn";
        let _turn = duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(SESSION.to_string()), async {
            crate::turn_context::register("mint-sales", turn, RECIPIENT).unwrap()
        }).await;
        assert!(mint(h, "mint-sales", turn, RECIPIENT).is_none(), "no requester registered");
        let bound = crate::knowledge::turn_requesters::register("mint-sales", turn, RequesterIdentity::ChannelUser {
            channel: "googlechat".into(), chat: "spaces/ORIGIN".into(), sender: SENDER.into(), user_id: Some(f.user.clone()),
            attested_private: false,
        }).unwrap();
        let id = mint(h, "mint-sales", turn, RECIPIENT).expect("bound live turn");
        assert!(valid_id(&id) && id != f.proof, "a fresh opaque id per mint");
        let record = verify(h, "mint-sales", RECIPIENT, Some(&id)).unwrap();
        assert_eq!((record.user_id.as_str(), record.channel_user_id.as_str(), record.session.as_str()),
            (f.user.as_str(), SENDER, SESSION));
        assert!(mint(h, "other", turn, RECIPIENT).is_none(), "another employee's turn");
        assert!(mint(h, "mint-sales", turn, "googlechat:spaces/OTHER").is_none(), "not this conversation");
        drop(bound);
        let _unbound = crate::knowledge::turn_requesters::register("mint-sales", turn, RequesterIdentity::ChannelUser {
            channel: "googlechat".into(), chat: "spaces/ORIGIN".into(), sender: SENDER.into(), user_id: None,
            attested_private: false,
        }).unwrap();
        assert!(mint(h, "mint-sales", turn, RECIPIENT).is_none(), "unbound sender");
    }

    #[tokio::test]
    async fn webchat_mint_binds_the_session_owner_and_its_generation() {
        let f = crate::scheduled_webchat::tests::fixture("sales").await;
        let h = f.home.path();
        let record = verify(h, "sales", &f.target, Some(&f.proof)).unwrap();
        assert_eq!((record.user_id.as_str(), record.session.as_str()), (f.user.as_str(), f.session.as_str()));
        assert_eq!(verify(h, "hr", &f.target, Some(&f.proof)).unwrap_err(), OriginDenial::ProofInvalid);
        assert_eq!(verify(h, "sales", &f.session, Some(&f.proof)).unwrap_err(), OriginDenial::ProofInvalid,
            "only the stamped target");
        // A turn registered for another person gets no WebChat proof.
        let _other = crate::knowledge::turn_requesters::register("sales", "webchat-turn",
            duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id: "someone-else".into() }).unwrap();
        assert!(mint_webchat(h, "sales", "webchat-turn", &f.target).is_none());
        UserDb::new(&h.join("users.db")).unwrap().set_user_status(&f.user, UserStatus::Suspended).unwrap();
        assert_eq!(verify(h, "sales", &f.target, Some(&f.proof)).unwrap_err(), OriginDenial::AccountInactive);
        assert!(mint_webchat(h, "sales", "fresh-turn", &f.target).is_none(), "an inactive creator gets no proof");
    }

    #[tokio::test]
    async fn records_are_capped_per_employee() {
        let f = fixture("sales").await;
        let h = f.home.path();
        rusqlite::Connection::open(h.join("sessions.db")).unwrap().execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO schedule_origin_proofs SELECT printf('%064x', i), 'sales', 'r', 'u', 'c', 'cu', 's', 'g', 0 FROM n",
            params![MAX_PROOFS_PER_EMPLOYEE],
        ).unwrap();
        let mut full = draft(h, "sales", RECIPIENT, SESSION, &f.user);
        full.id = new_id().unwrap();
        assert!(persist(h, &full).is_err(), "the employee's share is full");
        let mut other = draft(h, "hr", RECIPIENT, SESSION, &f.user);
        other.id = new_id().unwrap();
        assert!(persist(h, &other).is_ok(), "other employees are unaffected");
    }

    #[tokio::test]
    async fn proof_is_deleted_with_its_job() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let store = crate::cron_store::CronStore::open(h).unwrap();
        let mut row = crate::cron_store::CronTaskRow::new("job".into(), "Daily".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        row.notify_origin = Some(RECIPIENT.into());
        row.notify_origin_proof = Some(f.proof.clone());
        store.insert(&row).await.unwrap();
        assert!(delete_cron_task(h, &store, "job").await.unwrap());
        assert!(!stored(h, &f.proof));
        assert_eq!(verify(h, "sales", RECIPIENT, Some(&f.proof)).unwrap_err(), OriginDenial::ProofInvalid);
        assert!(!delete_cron_task(h, &store, "job").await.unwrap(), "already gone");
    }

    #[tokio::test]
    async fn collector_removes_unreferenced_proofs_only() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let record = || draft(h, "sales", RECIPIENT, SESSION, &f.user);
        let (cron, task, pending, delivered, orphan, fresh) = (f.proof.clone(), insert_for_test(h, record()),
            insert_for_test(h, record()), insert_for_test(h, record()), insert_for_test(h, record()), insert_for_test(h, record()));
        for id in [&cron, &task, &pending, &delivered, &orphan] { age(h, id, UNREFERENCED_GRACE_SECS + 60); }
        let store = crate::cron_store::CronStore::open(h).unwrap();
        let mut row = crate::cron_store::CronTaskRow::new("job".into(), "Daily".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        row.notify_origin = Some(RECIPIENT.into());
        row.notify_origin_proof = Some(cron.clone());
        row.enabled = false;
        store.insert(&row).await.unwrap();
        let mut goal = crate::task_store::TaskRow::new("goal".into(), "Goal".into(), "d".into(), "medium".into(),
            "sales".into(), "sales".into());
        goal.source_channel = Some("googlechat".into());
        goal.source_chat_id = Some("spaces/ORIGIN".into());
        goal.source_origin_proof = Some(task.clone());
        crate::task_store::TaskStore::open(h).unwrap().insert_task(&goal).await.unwrap();
        std::fs::write(h.join("reminders.jsonl"), format!(
            "{{\"id\":\"r1\",\"status\":\"pending\",\"notify_origin_proof\":\"{pending}\"}}\n\
             {{\"id\":\"r2\",\"status\":\"delivered\",\"notify_origin_proof\":\"{delivered}\"}}\n")).unwrap();
        assert_eq!(collect_garbage(h).unwrap(), 2);
        for kept in [&cron, &task, &pending, &fresh] { assert!(stored(h, kept), "referenced or within grace"); }
        assert!(!stored(h, &delivered) && !stored(h, &orphan));
        // An unreadable reference source aborts the pass: nothing is deleted.
        let late = insert_for_test(h, record());
        age(h, &late, UNREFERENCED_GRACE_SECS + 60);
        std::fs::write(h.join("reminders.jsonl"), b"{not json\n").unwrap();
        assert!(collect_garbage(h).is_err());
        assert!(stored(h, &late));
    }
}
