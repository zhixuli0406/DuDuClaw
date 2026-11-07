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

/// The `origin_kind` a signed-in dashboard account's conversation records.
const WEBCHAT_USER_KIND: &str = "webchat_user";

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

/// The stored owner of a session row: `(agent_id, owner_user_id)`, only for a
/// signed-in dashboard conversation, including an archived one.
fn stored_owner(home: &Path, session: &str) -> Result<Option<(String, String)>, String> {
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
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT agent_id, origin_kind, owner_user_id FROM sessions
             WHERE id = ?1",
            params![session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| "對話紀錄無法讀取".to_string())?;
    Ok(row.and_then(|(agent, kind, owner)| {
        if kind.as_deref() != Some(WEBCHAT_USER_KIND) {
            return None;
        }
        owner.filter(|o| !o.is_empty()).map(|owner| (agent, owner))
    }))
}

/// Shape + stored-row checks shared by creation and fire time. Returns the
/// owning dashboard user id.
fn check_session(home: &Path, session: &str, agent: &str) -> Result<String, String> {
    let (tag, segment) = parse(session).ok_or("不是有效的 WebChat 對話")?;
    if segment.is_some_and(|s| s != agent) {
        return Err("WebChat 對話屬於另一位 AI 員工".into());
    }
    let (stored_agent, owner) = stored_owner(home, session)?
        .ok_or("WebChat 對話已不存在或不是已登入帳號的對話")?;
    if stored_agent != agent {
        return Err("WebChat 對話屬於另一位 AI 員工".into());
    }
    if crate::webchat::webchat_owner_tag(&owner) != tag {
        return Err("WebChat 對話的擁有者不一致".into());
    }
    Ok(owner)
}

/// Creation-time binding of the env-derived origin to the verified acting
/// employee. `None` (ignore the origin) on any mismatch or lookup failure.
pub fn bind_origin(home: &Path, origin: Option<&str>, acting_agent: &str) -> Option<String> {
    let origin = origin.map(str::trim).filter(|s| !s.is_empty())?;
    if !duduclaw_core::is_valid_agent_id(acting_agent) {
        return None;
    }
    match check_session(home, origin, acting_agent) {
        Ok(_) => Some(origin.to_string()),
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
fn account_may_call(home: &Path, user: &str, agent: &str) -> Result<(), String> {
    let db = crate::decision_notify::open_user_db(home)?.ok_or("帳號資料庫不存在")?;
    let (access, _) = crate::trigger_route::access::load_account_with_edition(home, &db, user)?;
    let access = access.ok_or("對話擁有者的帳號已停用或不存在")?;
    if access.role == duduclaw_auth::UserRole::Admin || access.may_call(agent) {
        Ok(())
    } else {
        Err("對話擁有者已無權與此 AI 員工對話".into())
    }
}

/// Fire-time re-check: session still exists, same employee, same dashboard
/// account, account still Active and still allowed to talk to the employee.
pub fn verify_delivery(home: &Path, agent: &str, session: &str) -> Result<String, String> {
    let owner = check_session(home, session, agent)?;
    account_may_call(home, &owner, agent)?;
    Ok(owner)
}

/// `result` clamped to [`MAX_RESULT_CHARS`] chars (CJK-safe), with a
/// `[…truncated]` marker like the other channel deliveries.
pub fn bound(result: &str) -> String {
    if result.chars().count() > MAX_RESULT_CHARS {
        format!("{}\n[…truncated]", duduclaw_core::truncate_chars(result, MAX_RESULT_CHARS))
    } else {
        result.to_string()
    }
}

/// `⏰ [<name>] <result>`, clamped by chars like the other channels.
pub fn format_body(name: &str, result: &str) -> String {
    format!("⏰ [{name}] {}", bound(result))
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
pub async fn deliver(home: &Path, agent: &str, session: &str, body: &str) -> Result<(), String> {
    let owner = verify_delivery(home, agent, session)?;
    let manager = crate::session::SessionManager::new(&home.join("sessions.db"))
        .map_err(|_| "對話紀錄無法開啟".to_string())?;
    let mut conn = manager.acquire().await;
    let tx = conn.transaction().map_err(|_| "對話紀錄無法寫入".to_string())?;
    let still: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND agent_id=?2
               AND origin_kind='webchat_user' AND owner_user_id=?3)",
            params![session, agent, owner],
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
    tx.commit().map_err(|_| "對話紀錄無法寫入".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use duduclaw_auth::{AccessLevel, UserDb, UserRole, UserStatus};

    pub(crate) struct Fixture {
        pub home: tempfile::TempDir,
        pub user: String,
        pub session: String,
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
        Fixture { home, user: user.id, session }
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
        assert_eq!(bind_origin(h, Some(&f.session), "sales").as_deref(), Some(f.session.as_str()));
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
        assert_eq!(bind_origin(h, Some(&main), "main").as_deref(), Some(main.as_str()));
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
        deliver(h, "sales", &f.session, &format_body("daily", "done")).await.unwrap();
        assert_eq!(messages(&f).await, vec!["⏰ [daily] done".to_string()]);
        // Wrong employee.
        assert!(deliver(h, "hr", &f.session, "x").await.is_err());
        // Binding revoked ⇒ dropped.
        let db = UserDb::new(&h.join("users.db")).unwrap();
        db.unbind_agent(&f.user, "sales").unwrap();
        assert!(deliver(h, "sales", &f.session, "after unbind").await.is_err());
        db.bind_agent(&f.user, "sales", AccessLevel::Operator).unwrap();
        // Suspended account ⇒ dropped.
        db.set_user_status(&f.user, UserStatus::Suspended).unwrap();
        assert!(deliver(h, "sales", &f.session, "after suspend").await.is_err());
        db.set_user_status(&f.user, UserStatus::Active).unwrap();
        // Purged conversation ⇒ dropped.
        let manager = crate::session::SessionManager::new(&h.join("sessions.db")).unwrap();
        manager.purge_session(&f.session).await.unwrap();
        assert!(deliver(h, "sales", &f.session, "after delete").await.is_err());
        assert!(messages(&f).await.is_empty(), "deleted conversation is not recreated");
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
        deliver(h, "sales", &f.session, &format_body("monthly", "done")).await.unwrap();
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
