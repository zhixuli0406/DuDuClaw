//! RM-124 — schedule / reminder results delivered back into the WebChat
//! conversation that created them.
//!
//! A WebChat conversation has no bot identity a detached scheduler could push
//! to, so its "delivery target" is the conversation's own session history: the
//! result is appended as an assistant message and appears when the dashboard
//! chat (re)loads that conversation. There is no live push: the WebChat socket
//! only streams frames of turns it is running itself (the same constraint the
//! meeting-transcript completion notice lives with, `meeting_jobs::notice`).
//!
//! ## Trust model
//!
//! * The only accepted source of a WebChat target is
//!   [`duduclaw_core::ENV_ORIGIN_SESSION`], which the gateway sets for the MCP
//!   child of a `webchat:` turn ([`ORIGIN_SESSION`] task-local, scoped in
//!   `channel_reply::inner`). A `webchat` target the model (or a dashboard
//!   edit) supplies is refused ([`refuse_supplied_target`]).
//! * At creation the env value is bound to the verified acting employee
//!   ([`bind_origin`]): the `#agent:` segment (when present) and the stored
//!   session row must both name that employee, and the row must be a signed-in
//!   dashboard account's conversation (`origin_kind = 'webchat_user'`) whose
//!   owner hashes to the owner tag inside the id.
//! * At fire time ([`verify_delivery`] / [`deliver`]) everything is re-read:
//!   the session must still exist (archived is allowed), still belong to the same
//!   employee and the same dashboard account, that account must still be
//!   Active and still allowed to talk to the employee. Any miss drops the
//!   message with an audit row — never a fallback to another destination.

use std::path::Path;

use rusqlite::{OptionalExtension, params};
use serde_json::json;

tokio::task_local! {
    /// The full composed WebChat session id of the turn being answered
    /// (`webchat:webchat:<owner tag>:<conn>[#agent:<id>][#conv:<nonce>]`).
    /// Scoped ONLY around `webchat:` turns; exported to the spawned CLI as
    /// [`duduclaw_core::ENV_ORIGIN_SESSION`]. Deliberately separate from
    /// `claude_runner::REPLY_CHANNEL`: that one makes `send_to_agent`
    /// callbacks / decision pushes try to *send* to the conversation, which
    /// WebChat cannot receive.
    pub static ORIGIN_SESSION: String;
}

/// `notify_channel` / reminder `channel` value for a WebChat conversation.
pub const CHANNEL: &str = "webchat";

/// Session-id prefix every dashboard WebChat conversation carries
/// (`"webchat:" + user_id`, where `user_id = "webchat:<tag>:<conn>"`).
const PREFIX: &str = "webchat:webchat:";

/// Hard upper bound on an accepted session id (env + DB value).
pub const MAX_SESSION_LEN: usize = 512;

/// Same clamp as the other channel deliveries in `cron_scheduler`.
pub const MAX_RESULT_CHARS: usize = 3500;

/// Longest accepted schedule name (chars). Enforced on create/update of cron
/// tasks (MCP + dashboard RPC); delivery clamps legacy longer names anyway.
pub const MAX_NAME_CHARS: usize = 80;

/// Hard cap (chars) on the whole composed message (`⏰ [name] result` plus the
/// truncation marker), whatever the name length.
pub const MAX_BODY_CHARS: usize = MAX_RESULT_CHARS + MAX_NAME_CHARS + 16;

const TRUNCATED_MARKER: &str = "\n[…truncated]";

/// Separator between the reusable session id and the session's `created_at`
/// inside a stored target (`notify_chat_id` / reminder `chat_id`). The
/// `created_at` of a session row is never reused: a purged-and-recreated
/// conversation with the same id gets a new one, so an old schedule no longer
/// matches it.
const ORIGIN_SEP: &str = "|at=";

/// The `origin_kind` a signed-in dashboard account's conversation records.
const WEBCHAT_USER_KIND: &str = "webchat_user";

/// Reject a schedule name longer than [`MAX_NAME_CHARS`] chars.
pub fn check_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n > MAX_NAME_CHARS {
        return Err(format!("名稱過長（{n} 字，上限 {MAX_NAME_CHARS}）(name too long: {n} chars, max {MAX_NAME_CHARS})"));
    }
    Ok(())
}

/// `session` + the session's creation stamp, as stored with a schedule.
fn compose_target(session: &str, created_at: &str) -> String {
    format!("{session}{ORIGIN_SEP}{created_at}")
}

/// `(session id, creation stamp)` of a stored target; the stamp is `None` for
/// a bare session id (never accepted for delivery).
fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.rsplit_once(ORIGIN_SEP) {
        Some((session, at)) if !at.is_empty() => (session, Some(at)),
        _ => (target, None),
    }
}

/// Cancel every schedule / pending reminder aimed at `session` (any
/// generation). Called when a conversation is purged. Best effort: delivery is
/// independently refused for a purged / recreated conversation.
pub async fn cancel_for_session(home: &Path, session: &str) {
    let prefix = format!("{session}{ORIGIN_SEP}");
    let aimed = |channel: Option<&str>, chat: Option<&str>| {
        channel == Some(CHANNEL) && chat.is_some_and(|c| c.starts_with(&prefix))
    };
    if home.join("cron_tasks.db").is_file() {
        match crate::cron_store::CronStore::open(home) {
            Ok(store) => match store.list_all().await {
                Ok(rows) => {
                    for row in rows
                        .iter()
                        .filter(|r| aimed(r.notify_channel.as_deref(), r.notify_chat_id.as_deref()))
                    {
                        if let Err(e) = store.delete(&row.id).await {
                            tracing::warn!(id = %row.id, error = %e, "cancel schedule of purged WebChat session failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "list schedules for purged WebChat session failed"),
            },
            Err(e) => tracing::warn!(error = %e, "open cron store for purged WebChat session failed"),
        }
    }
    if home.join("reminders.jsonl").is_file() {
        let prefix = prefix.clone();
        let result = crate::reminder_scheduler::cancel_pending_where(home, move |r| {
            r.channel == CHANNEL && r.chat_id.starts_with(&prefix)
        })
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "cancel reminders of purged WebChat session failed");
        }
    }
}

/// The value `channel_reply` scopes as [`ORIGIN_SESSION`] for a turn: the
/// session id for a signed-in dashboard account's WebChat turn answered by
/// `agent_id` (`#agent:` segment absent or equal), empty otherwise. Empty is
/// "no origin" (filtered by [`current_origin`]), so callers can scope
/// unconditionally.
pub fn turn_origin(session_id: &str, agent_id: &str) -> String {
    let principal = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
        .try_with(Clone::clone)
        .ok()
        .filter(|p| !p.is_empty() && !crate::webchat::is_widget_visitor(p));
    let Some(principal) = principal else { return String::new(); };
    match parse(session_id) {
        Some((tag, segment))
            if tag == crate::webchat::webchat_owner_tag(&principal)
                && segment.is_none_or(|s| s == agent_id)
                && !agent_id.is_empty() =>
        {
            session_id.to_string()
        }
        _ => String::new(),
    }
}

/// The current turn's origin session, if this is a WebChat turn.
pub fn current_origin() -> Option<String> {
    ORIGIN_SESSION.try_with(Clone::clone).ok().filter(|s| !s.is_empty())
}

/// Put [`ORIGIN_SESSION`] into a CLI spawn's environment. Always clears an
/// inherited value first, so a non-WebChat turn can never carry one.
pub fn inject_env(cmd: &mut tokio::process::Command) {
    cmd.env_remove(duduclaw_core::ENV_ORIGIN_SESSION);
    if let Some(session) = current_origin() {
        cmd.env(duduclaw_core::ENV_ORIGIN_SESSION, session);
    }
}

/// `(name, value)` form of [`inject_env`] for runtimes that pass env pairs.
pub fn env_pair() -> Option<(String, String)> {
    current_origin().map(|s| (duduclaw_core::ENV_ORIGIN_SESSION.to_string(), s))
}

/// Syntactic shape of a dashboard WebChat session id: the 12-hex owner tag
/// and the optional (single) `#agent:` segment. `None` for anything else.
pub fn parse(session: &str) -> Option<(&str, Option<&str>)> {
    if session.is_empty()
        || session.len() > MAX_SESSION_LEN
        || session.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let rest = session.strip_prefix(PREFIX)?;
    let (tag, tail) = rest.split_once(':')?;
    if tag.len() != 12 || !tag.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    if tail.is_empty() {
        return None;
    }
    const AGENT: &str = "#agent:";
    let agent = match session.match_indices(AGENT).count() {
        0 => None,
        1 => {
            let start = session.find(AGENT)? + AGENT.len();
            let seg = &session[start..];
            let seg = &seg[..seg.find('#').unwrap_or(seg.len())];
            if !duduclaw_core::is_valid_agent_id(seg) {
                return None;
            }
            Some(seg)
        }
        _ => return None,
    };
    Some((tag, agent))
}

/// The stored owner of a session row: `(agent_id, owner_user_id, created_at)`,
/// only for a signed-in dashboard conversation, including an archived one.
fn stored_owner(home: &Path, session: &str) -> Result<Option<(String, String, String)>, String> {
    let path = home.join("sessions.db");
    if !path.is_file() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "對話紀錄無法讀取".to_string())?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    let row: Option<(String, Option<String>, Option<String>, String)> = conn
        .query_row(
            "SELECT agent_id, origin_kind, owner_user_id, created_at FROM sessions
             WHERE id = ?1",
            params![session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|_| "對話紀錄無法讀取".to_string())?;
    Ok(row.and_then(|(agent, kind, owner, created_at)| {
        if kind.as_deref() != Some(WEBCHAT_USER_KIND) {
            return None;
        }
        owner.filter(|o| !o.is_empty()).map(|owner| (agent, owner, created_at))
    }))
}

/// Shape + stored-row checks shared by creation and fire time. Returns the
/// owning dashboard user id and the row's `created_at`.
fn check_session(home: &Path, session: &str, agent: &str) -> Result<(String, String), String> {
    let (tag, segment) = parse(session).ok_or("不是有效的 WebChat 對話")?;
    if segment.is_some_and(|s| s != agent) {
        return Err("WebChat 對話屬於另一位 AI 員工".into());
    }
    let (stored_agent, owner, created_at) = stored_owner(home, session)?
        .ok_or("WebChat 對話已不存在或不是已登入帳號的對話")?;
    if stored_agent != agent {
        return Err("WebChat 對話屬於另一位 AI 員工".into());
    }
    if crate::webchat::webchat_owner_tag(&owner) != tag {
        return Err("WebChat 對話的擁有者不一致".into());
    }
    Ok((owner, created_at))
}

/// Creation-time binding of the env-derived origin to the verified acting
/// employee. `None` (ignore the origin) on any mismatch or lookup failure.
/// On success returns the stored target: the session id stamped with the
/// session row's `created_at` (see [`ORIGIN_SEP`]).
pub fn bind_origin(home: &Path, origin: Option<&str>, acting_agent: &str) -> Option<String> {
    let origin = origin.map(str::trim).filter(|s| !s.is_empty())?;
    if !duduclaw_core::is_valid_agent_id(acting_agent) {
        return None;
    }
    match check_session(home, origin, acting_agent) {
        Ok((_, created_at)) => Some(compose_target(origin, &created_at)),
        Err(reason) => {
            tracing::warn!(agent = acting_agent, %reason, "origin WebChat session ignored");
            None
        }
    }
}

/// Refuse a `webchat` notify target supplied by anything other than the
/// gateway-attested origin (the model, or a dashboard edit). `existing` is the
/// row's stored target: echoing it back unchanged is allowed.
pub fn refuse_supplied_target(
    channel: Option<&str>,
    chat: Option<&str>,
    existing: Option<(Option<&str>, Option<&str>)>,
) -> Result<(), &'static str> {
    if !channel.is_some_and(|c| c.trim().eq_ignore_ascii_case(CHANNEL)) {
        return Ok(());
    }
    if existing.is_some_and(|(c, id)| c == channel && id == chat) {
        return Ok(());
    }
    Err("webchat 不能指定為通知對象：在 WebChat 對話中建立排程時，結果會自動回到該對話。 \
         (A webchat target cannot be supplied; schedules created in a WebChat conversation \
         report back to that conversation automatically.)")
}

/// Is the dashboard account still allowed to talk to `agent`? Admins always;
/// everyone else needs a binding / team grant (same predicate WebChat uses
/// for explicit agent selection). Inactive / missing accounts fail closed.
fn account_may_call(
    home: &Path,
    user: &str,
    agent: &str,
) -> Result<crate::team_edition::EditionSnapshot, String> {
    let db = crate::decision_notify::open_user_db(home)?.ok_or("帳號資料庫不存在")?;
    let (access, edition) = crate::trigger_route::access::load_account_with_edition(home, &db, user)?;
    let access = access.ok_or("對話擁有者的帳號已停用或不存在")?;
    if access.role == duduclaw_auth::UserRole::Admin || access.may_call(agent) {
        Ok(edition)
    } else {
        Err("對話擁有者已無權與此 AI 員工對話".into())
    }
}

/// Fire-time re-check of a stored `target`: session still exists *as the same
/// conversation* (its `created_at` equals the stamp stored with the
/// schedule), same employee, same dashboard account, account still Active and
/// still allowed to talk to the employee. Returns the owner user id.
pub fn verify_delivery(home: &Path, agent: &str, target: &str) -> Result<String, String> {
    verify_delivery_leased(home, agent, target).map(|(owner, _)| owner)
}

/// [`verify_delivery`] that also returns the edition snapshot the decision was
/// made under, so the caller can hold it until the write commits.
fn verify_delivery_leased(
    home: &Path,
    agent: &str,
    target: &str,
) -> Result<(String, crate::team_edition::EditionSnapshot), String> {
    let (session, origin_at) = split_target(target);
    let origin_at = origin_at.ok_or("排程目標缺少對話世代，已停止傳送")?;
    let (owner, created_at) = check_session(home, session, agent)?;
    if created_at != origin_at {
        return Err("WebChat 對話已被刪除並重建，已停止傳送".into());
    }
    let edition = account_may_call(home, &owner, agent)?;
    Ok((owner, edition))
}

/// `result` clamped to [`MAX_RESULT_CHARS`] chars (CJK-safe), with a
/// `[…truncated]` marker like the other channel deliveries.
pub fn bound(result: &str) -> String {
    if result.chars().count() > MAX_RESULT_CHARS {
        format!("{}{TRUNCATED_MARKER}", duduclaw_core::truncate_chars(result, MAX_RESULT_CHARS))
    } else {
        result.to_string()
    }
}

/// Clamp a fully composed body to [`MAX_BODY_CHARS`] chars (marker included).
pub fn clamp_body(body: &str) -> String {
    if body.chars().count() <= MAX_BODY_CHARS {
        return body.to_string();
    }
    let keep = MAX_BODY_CHARS - TRUNCATED_MARKER.chars().count();
    format!("{}{TRUNCATED_MARKER}", duduclaw_core::truncate_chars(body, keep))
}

/// `⏰ [<name>] <result>`: result bounded, name shortened to
/// [`MAX_NAME_CHARS`] and the whole body clamped, so a legacy over-long name
/// cannot bloat the session.
pub fn format_body(name: &str, result: &str) -> String {
    let name = duduclaw_core::truncate_chars(name, MAX_NAME_CHARS);
    clamp_body(&format!("⏰ [{name}] {}", bound(result)))
}

/// Record a dropped WebChat delivery (warn + tool-call audit row).
pub fn audit_drop(home: &Path, agent: &str, source: &str, id: &str, reason: &str) {
    tracing::warn!(agent, source, id, reason, "scheduled WebChat delivery dropped");
    duduclaw_security::audit::append_tool_call_with_extras(
        home,
        agent,
        "scheduled_notification",
        &format!("webchat delivery dropped: {reason}"),
        false,
        &[
            ("recipient_guard", json!("webchat_owner_check_failed")),
            ("source", json!(source)),
            ("id", json!(id)),
        ],
    );
}

/// Append `body` as an assistant message to the WebChat session after
/// [`verify_delivery`]. The employee/owner binding is checked again inside
/// the write transaction so a concurrent delete or re-assignment between the
/// check and the write cannot land the message elsewhere.
///
/// `target` is the stored target (session id + creation stamp). The body is
/// clamped to [`MAX_BODY_CHARS`] here so no caller can bypass the cap.
///
/// Authorization race: the account/ACL state lives in `users.db`, the write in
/// `sessions.db`, so they cannot share a transaction. Instead the ACL
/// generation (`auth_meta.acl_generation`, bumped by every suspension, unbind
/// and grant change) is read *before* the account check; the sessions.db commit
/// then happens inside `UserDb::with_acl_generation` (an IMMEDIATE users.db
/// lease that blocks authorisation writes) and `EditionSnapshot::with_current`
/// (edition downgrades do not bump the generation). If either moved, the
/// transaction is rolled back and the message never lands.
pub async fn deliver(home: &Path, agent: &str, target: &str, body: &str) -> Result<(), String> {
    deliver_with_hook(home, agent, target, body, || {}).await
}

async fn deliver_with_hook(
    home: &Path,
    agent: &str,
    target: &str,
    body: &str,
    after_append: impl FnOnce(),
) -> Result<(), String> {
    let generation = crate::trigger_route::access::read_generation(home)
        .ok_or("授權狀態無法讀取，已停止傳送")?;
    let (owner, edition) = verify_delivery_leased(home, agent, target)?;
    let users = crate::decision_notify::open_user_db(home)?.ok_or("帳號資料庫不存在")?;
    let (session, origin_at) = split_target(target);
    let origin_at = origin_at.ok_or("排程目標缺少對話世代，已停止傳送")?;
    let body = clamp_body(body);
    let manager = crate::session::SessionManager::new(&home.join("sessions.db"))
        .map_err(|_| "對話紀錄無法開啟".to_string())?;
    let mut conn = manager.acquire().await;
    let tx = conn.transaction().map_err(|_| "對話紀錄無法寫入".to_string())?;
    let still: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND agent_id=?2
               AND origin_kind='webchat_user' AND owner_user_id=?3 AND created_at=?4)",
            params![session, agent, owner, origin_at],
            |row| row.get(0),
        )
        .map_err(|_| "對話紀錄無法讀取".to_string())?;
    if !still {
        return Err("WebChat 對話已不存在或無法存取".into());
    }
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO session_messages(session_id,role,content,tokens,timestamp)
         VALUES (?1,'assistant',?2,0,?3)",
        params![session, body, now],
    )
    .map_err(|_| "對話紀錄無法寫入".to_string())?;
    tx.execute("UPDATE sessions SET last_active=?2, archived_at=NULL WHERE id=?1", params![session, now])
        .map_err(|_| "對話紀錄無法寫入".to_string())?;
    after_append();
    // Commit while holding the users.db ACL lease (BEGIN IMMEDIATE blocks every
    // authorisation write until we are done) and the edition snapshot (an
    // Enterprise→Personal downgrade does not bump the ACL generation). If
    // either moved, `tx` is dropped un-committed and the append rolls back.
    users
        .with_acl_generation(generation, || edition.with_current(|| tx.commit()))
        .map_err(|_| "對話擁有者的授權剛剛變更，已停止傳送".to_string())?
        .map_err(|_| "對話擁有者的授權剛剛變更，已停止傳送".to_string())?
        .map_err(|_| "對話紀錄無法寫入".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use duduclaw_auth::{AccessLevel, UserDb, UserRole, UserStatus};

    pub(crate) struct Fixture {
        pub home: tempfile::TempDir,
        pub user: String,
        pub session: String,
        /// The stored schedule target (session id + creation stamp).
        pub target: String,
    }

    /// A dashboard Employee bound to `agent`, owning one WebChat session.
    pub(crate) async fn fixture(agent: &str) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("owner@test.invalid", "Owner", "fixture-password", UserRole::Employee).unwrap();
        db.bind_agent(&user.id, agent, AccessLevel::Operator).unwrap();
        let tag = crate::webchat::webchat_owner_tag(&user.id);
        let session = format!("webchat:webchat:{tag}:c0ffee#agent:{agent}#conv:n1");
        let manager = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
        manager
            .get_or_create_with_origin(&session, agent, &crate::session_origin::SessionOrigin::webchat_user(&user.id))
            .await
            .unwrap();
        let target = bind_origin(home.path(), Some(&session), agent).expect("fixture origin binds");
        Fixture { home, user: user.id, session, target }
    }

    pub(crate) async fn messages(f: &Fixture) -> Vec<String> {
        let manager = crate::session::SessionManager::new(&f.home.path().join("sessions.db")).unwrap();
        manager.get_messages(&f.session).await.unwrap().into_iter().map(|m| m.content).collect()
    }

    #[test]
    fn parse_accepts_only_dashboard_webchat_ids() {
        let tag = "0123456789ab";
        assert_eq!(parse(&format!("webchat:webchat:{tag}:c1")), Some((tag, None)));
        assert_eq!(parse(&format!("webchat:webchat:{tag}:c1#agent:sales#conv:x")), Some((tag, Some("sales"))));
        for bad in [
            "telegram:123".to_string(),
            format!("webchat:{tag}:c1"),
            format!("webchat:webchat:{tag}:"),
            format!("webchat:webchat:XYZ:c1"),
            format!("webchat:webchat:{tag}:c1#agent:a#agent:b"),
            format!("webchat:webchat:{tag}:c1#agent:../x"),
            format!("webchat:webchat:{tag}:c 1"),
            format!("webchat:webchat:{tag}:{}", "x".repeat(MAX_SESSION_LEN)),
        ] {
            assert_eq!(parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn supplied_webchat_targets_are_refused_unless_unchanged() {
        assert!(refuse_supplied_target(Some("line"), Some("U1"), None).is_ok());
        assert!(refuse_supplied_target(None, None, None).is_ok());
        assert!(refuse_supplied_target(Some("webchat"), Some("webchat:webchat:x"), None).is_err());
        assert!(refuse_supplied_target(Some(" WebChat "), Some("s"), None).is_err());
        let stored = Some((Some("webchat"), Some("s1")));
        assert!(refuse_supplied_target(Some("webchat"), Some("s1"), stored).is_ok());
        assert!(refuse_supplied_target(Some("webchat"), Some("s2"), stored).is_err());
    }

    #[tokio::test]
    async fn bind_origin_requires_the_acting_employee_and_a_signed_in_owner() {
        let f = fixture("sales").await;
        let h = f.home.path();
        assert_eq!(bind_origin(h, Some(&f.session), "sales").as_deref(), Some(f.target.as_str()));
        assert_eq!(split_target(&f.target).0, f.session);
        assert_eq!(bind_origin(h, Some(&f.session), "hr"), None, "agent segment mismatch");
        assert_eq!(bind_origin(h, None, "sales"), None);
        // Unknown session (never created) is ignored.
        let other = f.session.replace("c0ffee", "beef");
        assert_eq!(bind_origin(h, Some(&other), "sales"), None);
        // A session without `#agent:` still needs the stored row to match.
        let tag = crate::webchat::webchat_owner_tag(&f.user);
        let main = format!("webchat:webchat:{tag}:c2");
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.get_or_create_with_origin(&main, "main", &crate::session_origin::SessionOrigin::webchat_user(&f.user))
            .await.unwrap();
        assert_eq!(split_target(&bind_origin(h, Some(&main), "main").unwrap()).0, main);
        assert_eq!(bind_origin(h, Some(&main), "sales"), None);
        // Owner tag must hash from the stored owner.
        let forged = "webchat:webchat:ffffffffffff:c3#agent:sales".to_string();
        manager.get_or_create_with_origin(&forged, "sales", &crate::session_origin::SessionOrigin::webchat_user(&f.user))
            .await.unwrap();
        assert_eq!(bind_origin(h, Some(&forged), "sales"), None);
    }

    #[tokio::test]
    async fn deliver_appends_and_drops_when_ownership_is_lost() {
        let f = fixture("sales").await;
        let h = f.home.path();
        deliver(h, "sales", &f.target, &format_body("daily", "done")).await.unwrap();
        assert_eq!(messages(&f).await, vec!["⏰ [daily] done".to_string()]);
        // Wrong employee.
        assert!(deliver(h, "hr", &f.target, "x").await.is_err());
        // A bare session id (no creation stamp) is never accepted.
        assert!(deliver(h, "sales", &f.session, "bare").await.is_err());
        // Binding revoked ⇒ dropped.
        let db = UserDb::new(&h.join("users.db")).unwrap();
        db.unbind_agent(&f.user, "sales").unwrap();
        assert!(deliver(h, "sales", &f.target, "after unbind").await.is_err());
        db.bind_agent(&f.user, "sales", AccessLevel::Operator).unwrap();
        // Suspended account ⇒ dropped.
        db.set_user_status(&f.user, UserStatus::Suspended).unwrap();
        assert!(deliver(h, "sales", &f.target, "after suspend").await.is_err());
        db.set_user_status(&f.user, UserStatus::Active).unwrap();
        // Purged conversation ⇒ dropped.
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.purge_session(&f.session).await.unwrap();
        assert!(deliver(h, "sales", &f.target, "after delete").await.is_err());
        assert!(messages(&f).await.is_empty(), "deleted conversation is not recreated");
    }

    #[tokio::test]
    async fn suspension_between_check_and_commit_rolls_the_append_back() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let db = UserDb::new(&h.join("users.db")).unwrap();
        let result = deliver_with_hook(h, "sales", &f.target, "late", || {
            db.set_user_status(&f.user, UserStatus::Suspended).unwrap();
        })
        .await;
        assert!(result.is_err(), "ACL generation moved during the append");
        assert!(messages(&f).await.is_empty(), "the message must not land");
        db.set_user_status(&f.user, UserStatus::Active).unwrap();
        deliver(h, "sales", &f.target, "ok").await.unwrap();
        assert_eq!(messages(&f).await, vec!["ok".to_string()]);
    }

    #[tokio::test]
    async fn edition_downgrade_between_check_and_commit_rolls_the_append_back() {
        let f = fixture("sales").await;
        let h = f.home.path();
        crate::team_edition::record(h, duduclaw_core::EditionProfile::Enterprise);
        let result = deliver_with_hook(h, "sales", &f.target, "late", || {
            // No ACL generation bump: only the edition epoch moves.
            crate::team_edition::record(h, duduclaw_core::EditionProfile::Personal);
        })
        .await;
        assert!(result.is_err(), "edition changed during the append");
        assert!(messages(&f).await.is_empty(), "the message must not land");
        deliver(h, "sales", &f.target, "ok").await.unwrap();
        assert_eq!(messages(&f).await, vec!["ok".to_string()]);
    }

    async fn aimed_cron(f: &Fixture) -> crate::cron_store::CronStore {
        let store = crate::cron_store::CronStore::open(f.home.path()).unwrap();
        let mut aimed = crate::cron_store::CronTaskRow::new("c1".into(), "aimed".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        aimed.notify_channel = Some(CHANNEL.into());
        aimed.notify_chat_id = Some(f.target.clone());
        let other = crate::cron_store::CronTaskRow::new("c2".into(), "other".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        store.insert(&aimed).await.unwrap();
        store.insert(&other).await.unwrap();
        store
    }

    #[tokio::test]
    async fn gdpr_erase_cancels_schedules_aimed_at_erased_conversations() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let store = aimed_cron(&f).await;
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        let contact = format!("webchat:webchat:{}", crate::webchat::webchat_owner_tag(&f.user));
        let (sessions, _) = manager.erase_sessions_for_contact(&contact).await.unwrap();
        assert_eq!(sessions, 1);
        let left: Vec<String> = store.list_all().await.unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(left, vec!["c2".to_string()]);
    }

    #[tokio::test]
    async fn retention_purge_cancels_schedules_aimed_at_purged_conversations() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let store = aimed_cron(&f).await;
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(100)).to_rfc3339();
        {
            let conn = manager.acquire().await;
            conn.execute("UPDATE sessions SET archived_at=?2 WHERE id=?1", params![f.session, old]).unwrap();
        }
        manager.cleanup_inactive_with_retention(24, 90).await.unwrap();
        let left: Vec<String> = store.list_all().await.unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(left, vec!["c2".to_string()]);
    }

    #[tokio::test]
    async fn recreated_conversation_with_the_same_id_refuses_old_targets() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.purge_session(&f.session).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        manager
            .get_or_create_with_origin(&f.session, "sales", &crate::session_origin::SessionOrigin::webchat_user(&f.user))
            .await
            .unwrap();
        assert!(deliver(h, "sales", &f.target, "stale").await.is_err());
        assert!(messages(&f).await.is_empty());
        // A target bound to the new conversation works.
        let fresh = bind_origin(h, Some(&f.session), "sales").unwrap();
        assert_ne!(fresh, f.target);
        deliver(h, "sales", &fresh, "fresh").await.unwrap();
    }

    #[tokio::test]
    async fn purge_cancels_schedules_aimed_at_the_conversation() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let store = crate::cron_store::CronStore::open(h).unwrap();
        let mut aimed = crate::cron_store::CronTaskRow::new("c1".into(), "aimed".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        aimed.notify_channel = Some(CHANNEL.into());
        aimed.notify_chat_id = Some(f.target.clone());
        let other = crate::cron_store::CronTaskRow::new("c2".into(), "other".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        store.insert(&aimed).await.unwrap();
        store.insert(&other).await.unwrap();
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.purge_session(&f.session).await.unwrap();
        let left: Vec<String> = store.list_all().await.unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(left, vec!["c2".to_string()]);
    }

    #[test]
    fn names_are_capped_and_legacy_long_names_cannot_bypass_the_body_cap() {
        assert!(check_name(&"字".repeat(MAX_NAME_CHARS)).is_ok());
        assert!(check_name(&"字".repeat(MAX_NAME_CHARS + 1)).is_err());
        let body = format_body(&"N".repeat(100_000), &"x".repeat(MAX_RESULT_CHARS * 2));
        assert!(body.chars().count() <= MAX_BODY_CHARS);
        assert!(body.ends_with("[…truncated]"));
        let clamped = clamp_body(&"y".repeat(100_000));
        assert_eq!(clamped.chars().count(), MAX_BODY_CHARS);
    }

    #[tokio::test]
    async fn archived_session_receives_result_and_is_restored() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(32)).to_rfc3339();
        {
            let conn = manager.acquire().await;
            conn.execute("UPDATE sessions SET last_active=?2 WHERE id=?1", params![f.session, old]).unwrap();
        }
        assert_eq!(manager.cleanup_inactive(24).await.unwrap(), 1);
        {
            let conn = manager.acquire().await;
            let archived: bool = conn.query_row(
                "SELECT archived_at IS NOT NULL FROM sessions WHERE id=?1",
                params![f.session], |row| row.get(0),
            ).unwrap();
            assert!(archived);
        }
        deliver(h, "sales", &f.target, &format_body("monthly", "done")).await.unwrap();
        assert_eq!(messages(&f).await, vec!["⏰ [monthly] done".to_string()]);
        let conn = manager.acquire().await;
        let (archived_at, last_active): (Option<String>, String) = conn.query_row(
            "SELECT archived_at, last_active FROM sessions WHERE id=?1",
            params![f.session], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(archived_at, None);
        assert!(last_active > old, "delivery counts as new activity");
    }

    #[test]
    fn body_is_bounded_by_chars() {
        let long = "字".repeat(MAX_RESULT_CHARS + 10);
        let body = format_body("n", &long);
        assert!(body.ends_with("[…truncated]"));
        assert_eq!(body.chars().filter(|c| *c == '字').count(), MAX_RESULT_CHARS);
    }
}
