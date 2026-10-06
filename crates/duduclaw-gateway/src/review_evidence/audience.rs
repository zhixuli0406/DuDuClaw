//! TaskPacket audience as a limit on who may see a task (F3 follow-up).
//!
//! The packets are written by role members during a team round
//! (`team_handoff`), so an audience is something an AI wrote. In the packet
//! format it names who the content may reach: role ids (`verifier`) limit
//! the flow between roles, and only the three human-facing namespaces
//! [`HUMAN_AUDIENCE_PREFIXES`] limit people. This module is the one place
//! that turns packets into a limit on people, and the one place that applies
//! it:
//!
//! - role names never restrict a person; packets with no human-facing entry
//!   leave the task on its ordinary access rules;
//! - a dashboard Admin (re-read live) and the gateway admin token always
//!   read and decide: an AI-written list cannot remove oversight;
//! - an unreadable packet set denies everyone else (fail closed) and is
//!   reported to the admins, who keep access.
use std::{collections::BTreeSet, path::Path};

const MAX_PACKETS: usize = 256;
/// The only audience namespaces that restrict a person or a channel.
pub const HUMAN_AUDIENCE_PREFIXES: [&str; 3] = ["user:", "role:", "channel:"];
/// Intersection of conflicting human-facing lists: nobody but an admin.
pub const NO_PERMITTED_AUDIENCE: &str = "__no_permitted_audience__";

/// `true` when `entry` is in a human-facing namespace (a classification by
/// the three listed prefixes; matching against identities stays exact).
pub fn is_human_audience_entry(entry: &str) -> bool {
    entry == NO_PERMITTED_AUDIENCE
        || HUMAN_AUDIENCE_PREFIXES
            .iter()
            .any(|p| entry.len() > p.len() && entry.starts_with(p))
}

/// The human-facing part of a stored audience, sorted and de-duplicated.
pub fn human_audience(raw: &[String]) -> Vec<String> {
    let set: BTreeSet<String> = raw
        .iter()
        .filter(|e| is_human_audience_entry(e))
        .cloned()
        .collect();
    set.into_iter().collect()
}

/// What the packets of a task say about people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAudience {
    /// No human-facing limit: the task's own access rules apply.
    Open,
    /// Only these keys (exact match), plus admins.
    Limited(Vec<String>),
    /// Packets could not be read or are corrupt: admins only.
    Unreadable,
}

impl TaskAudience {
    pub fn keys(&self) -> &[String] {
        match self {
            TaskAudience::Limited(k) => k,
            _ => &[],
        }
    }
}

/// The task's human-facing audience. Never errors: failure is `Unreadable`.
pub fn task_audience(home: &Path, task_id: &str) -> TaskAudience {
    match task_packet_audience(home, task_id) {
        Ok(a) if a.is_empty() => TaskAudience::Open,
        Ok(a) => TaskAudience::Limited(a),
        Err(_) => TaskAudience::Unreadable,
    }
}

/// `true` for a live dashboard Admin or the gateway admin token.
pub fn is_overseer(ctx: &duduclaw_auth::UserContext) -> bool {
    ctx.is_admin()
}

/// THE dashboard check of an audience (task packets, saved snapshot, draft
/// or run audience). `ctx` must be the live identity. Task ACL is checked
/// by the caller; this only answers the audience question.
pub fn dashboard_may_read(ctx: &duduclaw_auth::UserContext, audience: &[String]) -> bool {
    if is_overseer(ctx) {
        return true;
    }
    let human = human_audience(audience);
    duduclaw_core::task_packet::audience_allows(true, &human, &dashboard_audience_keys(ctx))
}

/// [`dashboard_may_read`] for a task's packets.
pub fn dashboard_may_read_task(ctx: &duduclaw_auth::UserContext, audience: &TaskAudience) -> bool {
    match audience {
        TaskAudience::Open => true,
        TaskAudience::Limited(keys) => dashboard_may_read(ctx, keys),
        TaskAudience::Unreadable => is_overseer(ctx),
    }
}

/// Whether chat channel `channel` (e.g. `telegram`) may carry the task's
/// content and decide it. Exact `channel:<name>` match.
pub fn channel_may_read_task(channel: &str, audience: &TaskAudience) -> bool {
    match audience {
        TaskAudience::Open => true,
        TaskAudience::Limited(keys) => {
            let key = format!("channel:{channel}");
            keys.iter().any(|k| *k == key)
        }
        TaskAudience::Unreadable => false,
    }
}

/// One packet that limits people: who wrote it and to whom.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudienceSource {
    pub round: String,
    pub from_role: String,
    pub packet_id: String,
    pub keys: Vec<String>,
}

/// Packets that carry human-facing entries (for the admin's view of who
/// limited a task). Unreadable packets are skipped here; [`task_audience`]
/// reports them.
pub fn task_audience_sources(home: &Path, task_id: &str) -> Vec<AudienceSource> {
    let mut out = Vec::new();
    if !duduclaw_core::is_valid_agent_id(task_id) {
        return out;
    }
    let root = home
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(task_id);
    let Ok(rounds) = std::fs::read_dir(&root) else {
        return out;
    };
    for round in rounds.flatten() {
        if !round.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(round.path()) else {
            continue;
        };
        for file in files.flatten() {
            if !file.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let Ok(raw) = std::fs::read(file.path()) else {
                continue;
            };
            let Ok(p) = serde_json::from_slice::<duduclaw_core::task_packet::TaskPacket>(&raw)
            else {
                continue;
            };
            let keys = human_audience(&p.audience);
            if !keys.is_empty() {
                out.push(AudienceSource {
                    round: round.file_name().to_string_lossy().into_owned(),
                    from_role: p.from_role.to_string(),
                    packet_id: p.packet_id,
                    keys,
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.round, &a.packet_id).cmp(&(&b.round, &b.packet_id)));
    out
}

/// Activity type written when packets first limit who may see a task.
pub const AUDIENCE_RESTRICTED_EVENT: &str = "task_audience_restricted";
/// Activity type written when a task's packets cannot be read.
pub const AUDIENCE_UNREADABLE_EVENT: &str = "task_audience_unreadable";

/// Record that a packet written by `author` (role `from_role`, round
/// `round`) changed the task's human-facing audience from `before` to a
/// limit: one Activity Feed row and one security audit event. Called by
/// `team_handoff` after it filed the packet; a change that does not narrow
/// (or leaves the task open) records nothing. Best effort: a failed write is
/// logged, never blocks the hand-off.
pub async fn record_audience_restriction(
    home: &Path,
    task_id: &str,
    before: &TaskAudience,
    round: u32,
    from_role: &str,
    author: &str,
) -> bool {
    let after = task_audience(home, task_id);
    let TaskAudience::Limited(keys) = &after else {
        return false;
    };
    if *before == after {
        return false;
    }
    let details = serde_json::json!({
        "task_id": task_id,
        "round": round,
        "from_role": from_role,
        "author": author,
        "keys": keys,
        "before": before.keys(),
    });
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            AUDIENCE_RESTRICTED_EVENT,
            author,
            duduclaw_security::audit::Severity::Warning,
            details.clone(),
        ),
    );
    let Ok(store) = crate::task_store::TaskStore::open(home) else {
        tracing::warn!(task = task_id, "audience restriction: task store unavailable");
        return false;
    };
    let agent = match store.get_task(task_id).await {
        Ok(Some(t)) => t.assigned_to,
        _ => author.to_string(),
    };
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: AUDIENCE_RESTRICTED_EVENT.into(),
        agent_id: agent,
        task_id: Some(task_id.into()),
        summary: format!(
            "任務內容已被限制為只給 {}（第 {round} 輪，由 {from_role} 角色寫入）",
            keys.join("、")
        ),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: Some(details.to_string()),
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::warn!(task = task_id, error = %e, "audience restriction: activity not written");
    }
    true
}

/// Note once per task that its packets cannot be read (admins still read
/// it; everyone else is refused). Returns `true` when a row was written.
pub async fn note_audience_unreadable(store: &crate::task_store::TaskStore, task_id: &str) -> bool {
    let already = store
        .list_activity_for_task(task_id, 500)
        .await
        .map(|rows| rows.iter().any(|r| r.event_type == AUDIENCE_UNREADABLE_EVENT))
        .unwrap_or(true);
    if already {
        return false;
    }
    let agent = match store.get_task(task_id).await {
        Ok(Some(t)) => t.assigned_to,
        _ => return false,
    };
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: AUDIENCE_UNREADABLE_EVENT.into(),
        agent_id: agent,
        task_id: Some(task_id.into()),
        summary: "這個任務的團隊交接資料無法讀取或已損壞：只有管理者看得到內容。".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    };
    store.append_activity(&row).await.is_ok()
}

/// What an admin's task view says about the audience (`tasks.timeline`).
pub fn audience_restriction_json(home: &Path, task_id: &str) -> serde_json::Value {
    let audience = task_audience(home, task_id);
    let state = match &audience {
        TaskAudience::Open => "open",
        TaskAudience::Limited(_) => "limited",
        TaskAudience::Unreadable => "unreadable",
    };
    serde_json::json!({
        "state": state,
        "keys": audience.keys(),
        "sources": task_audience_sources(home, task_id),
    })
}

/// Human-facing audience of a task: the intersection of every packet's
/// human-facing entries. A packet with only role names imposes nothing on
/// people. A conflicting intersection is [`NO_PERMITTED_AUDIENCE`] (admins
/// only); it never becomes public.
pub fn task_packet_audience(home: &Path, task_id: &str) -> Result<Vec<String>, String> {
    if !duduclaw_core::is_valid_agent_id(task_id) {
        return Err("invalid task id".into());
    }
    let root = home
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join(task_id);
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => return Err("task audience unavailable".into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("task audience path refused".into());
    }
    let canonical_home = home
        .canonicalize()
        .map_err(|_| "task audience unavailable")?;
    if root
        .canonicalize()
        .map_err(|_| "task audience unavailable")?
        != canonical_home
            .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
            .join(task_id)
    {
        return Err("task audience path refused".into());
    }
    let mut allowed: Option<BTreeSet<String>> = None;
    let mut count = 0;
    for round in std::fs::read_dir(root).map_err(|_| "task audience unavailable")? {
        let round = round.map_err(|_| "task audience unavailable")?;
        let kind = round.file_type().map_err(|_| "task audience unavailable")?;
        if kind.is_symlink() {
            return Err("task audience symlink refused".into());
        }
        if !kind.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(round.path()).map_err(|_| "task audience unavailable")? {
            let file = file.map_err(|_| "task audience unavailable")?;
            if file.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let kind = file.file_type().map_err(|_| "task audience unavailable")?;
            if !kind.is_file() || kind.is_symlink() {
                return Err("task audience file refused".into());
            }
            count += 1;
            if count > MAX_PACKETS
                || file
                    .metadata()
                    .map_err(|_| "task audience unavailable")?
                    .len()
                    > duduclaw_core::task_packet::TASK_PACKET_MAX_BYTES as u64
            {
                return Err("task audience bounds exceeded".into());
            }
            let raw = std::fs::read(file.path()).map_err(|_| "task audience unavailable")?;
            let packet: duduclaw_core::task_packet::TaskPacket =
                serde_json::from_slice(&raw).map_err(|_| "task audience invalid")?;
            if packet.goal_id != task_id || packet.validate().is_err() {
                return Err("task audience invalid".into());
            }
            let human = human_audience(&packet.audience);
            if !human.is_empty() {
                let constraint: BTreeSet<String> = human.into_iter().collect();
                allowed = Some(match allowed {
                    Some(previous) => previous.intersection(&constraint).cloned().collect(),
                    None => constraint,
                });
            }
        }
    }
    match allowed {
        None => Ok(vec![]),
        Some(a) if a.is_empty() => Ok(vec![NO_PERMITTED_AUDIENCE.into()]),
        Some(a) => Ok(a.into_iter().collect()),
    }
}
/// Only authenticated dashboard identity/route keys; no model role inference.
pub fn dashboard_audience_keys(ctx: &duduclaw_auth::UserContext) -> Vec<String> {
    vec![
        format!("user:{}", ctx.user_id),
        format!("role:{}", ctx.role),
        "channel:dashboard".into(),
    ]
}

/// Refresh role and agent bindings from the identity store on every protected
/// review access. Long-lived JWT claims cannot retain revoked permissions.
pub(crate) fn fresh_dashboard_context(
    home: &Path,
    ctx: &duduclaw_auth::UserContext,
) -> Result<duduclaw_auth::UserContext, String> {
    // The gateway admin token (and the no-account local fallback) is not a
    // users.db principal: nothing there can revoke it, and an admin is never
    // shut out of a task by an AI-written audience.
    if ctx.user_id == "system" {
        return if ctx.is_admin() {
            Ok(duduclaw_auth::UserContext::admin_fallback())
        } else {
            Err("identity permission denied".into())
        };
    }
    trusted_dashboard_principal(home, &ctx.user_id)
}
/// Open an authority database read-only.
///
/// SQLite `NOFOLLOW` refuses a symlink in *any* path component, so a data
/// directory reached through a link (macOS `/var` -> `/private/var`, a home
/// moved to another disk and linked back) would make every read fail. The
/// containing directory is resolved first, the same way `ApprovalStore::open`
/// does; a symlink at the database file itself or at its `-wal`/`-shm`
/// sidecars is still refused, and `NOFOLLOW` stays on the final open.
pub(crate) fn readonly_db(path: &Path) -> Result<rusqlite::Connection, String> {
    let name = path.file_name().ok_or("authority database unavailable")?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let dir = std::fs::canonicalize(dir).map_err(|_| "authority database unavailable")?;
    let path = dir.join(name);
    let sidecar = |suffix: &str| std::path::PathBuf::from(format!("{}{suffix}", path.display()));
    for candidate in [path.clone(), sidecar("-wal"), sidecar("-shm")] {
        match std::fs::symlink_metadata(candidate) {
            Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
                return Err("authority database path refused".into());
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("authority database unavailable".into()),
        }
    }
    rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "authority database unavailable".into())
}
/// Resolve a server-persisted principal with read-only SQLite connections.
/// This never initializes schemas or creates a missing identity database.
pub fn trusted_dashboard_principal(
    home: &Path,
    principal: &str,
) -> Result<duduclaw_auth::UserContext, String> {
    use rusqlite::OptionalExtension;
    let path = home.join("users.db");
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return if principal == "system" {
                Ok(duduclaw_auth::UserContext::admin_fallback())
            } else {
                Err("identity unavailable".into())
            };
        }
        Err(_) => return Err("identity unavailable".into()),
        Ok(_) => (),
    }
    let db = readonly_db(&path)?;
    if principal == "system" {
        let count: i64 = db
            .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
            .map_err(|_| "identity unavailable")?;
        return if count == 0 {
            Ok(duduclaw_auth::UserContext::admin_fallback())
        } else {
            Err("identity permission denied".into())
        };
    }
    let user: Option<(String, String, String, bool)> = db
        .query_row(
            "SELECT email,role,status,must_change_password FROM users WHERE id=?1",
            [principal],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|_| "identity unavailable")?;
    let (email, role, status, password) = user.ok_or("identity unavailable")?;
    if status != "active" || password {
        return Err("identity permission denied".into());
    }
    let mut q = db
        .prepare("SELECT agent_name,access_level FROM user_agent_bindings WHERE user_id=?1")
        .map_err(|_| "identity unavailable")?;
    let rows = q
        .query_map([principal], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|_| "identity unavailable")?;
    let mut bindings = std::collections::HashMap::new();
    for row in rows {
        let (agent, level) = row.map_err(|_| "identity unavailable")?;
        bindings.insert(agent, level.parse()?);
    }
    Ok(duduclaw_auth::UserContext {
        user_id: principal.into(),
        email,
        role: role.parse()?,
        agent_access: bindings,
        must_change_password: false,
    })
}
/// Artifact/review audience belongs to the human dashboard decision principal,
/// not the worker actor. An empty audience still requires current task ACL.
pub async fn authorize_workflow_audience(
    home: &Path,
    ctx: &duduclaw_auth::UserContext,
    source_task: &str,
    saved_audience: &[String],
) -> Result<(), String> {
    let ctx = fresh_dashboard_context(home, ctx)?;
    let db = readonly_db(&home.join("tasks.db"))?;
    use rusqlite::OptionalExtension;
    let assigned: Option<String> = db
        .query_row(
            "SELECT assigned_to FROM tasks WHERE id=?1",
            [source_task],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "permission denied")?;
    // A removed source task leaves admins in charge (F5-D, P-M7).
    let allowed = match &assigned {
        Some(a) => ctx.has_agent_access(a, duduclaw_auth::AccessLevel::Viewer),
        None => is_overseer(&ctx),
    };
    if !allowed
        || !dashboard_may_read(&ctx, saved_audience)
        || !dashboard_may_read_task(&ctx, &task_audience(home, source_task))
    {
        return Err("permission denied".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_dashboard_identity_does_not_keep_revoked_authority() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db
            .create_user(
                "alice@test.invalid",
                "Alice",
                "isolated-test-password",
                duduclaw_auth::UserRole::Manager,
            )
            .unwrap();
        db.bind_agent(&user.id, "sales", duduclaw_auth::AccessLevel::Operator)
            .unwrap();
        let mut cached = duduclaw_auth::UserContext::admin_fallback();
        cached.user_id = user.id.clone();
        cached.role = user.role;
        cached
            .agent_access
            .insert("sales".into(), duduclaw_auth::AccessLevel::Operator);
        let refreshed = fresh_dashboard_context(home.path(), &cached).unwrap();
        assert!(refreshed.agent_access.contains_key("sales"));
        db.unbind_agent(&user.id, "sales").unwrap();
        db.update_user(
            &user.id,
            None,
            Some(duduclaw_auth::UserRole::Employee),
            None,
        )
        .unwrap();
        let refreshed = fresh_dashboard_context(home.path(), &cached).unwrap();
        assert!(refreshed.agent_access.is_empty());
        assert!(!refreshed.has_role(duduclaw_auth::UserRole::Manager));
        db.set_user_status(&user.id, duduclaw_auth::UserStatus::Suspended)
            .unwrap();
        assert!(fresh_dashboard_context(home.path(), &cached).is_err());
    }

    fn packet(home: &Path, id: &str, audience: Vec<String>) {
        let dir = home
            .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
            .join("task")
            .join("1");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = serde_json::json!({
            "packet_id": id,
            "goal_id": "task",
            "round": 1,
            "from_role": "executor",
            "to_role": "verifier",
            "objective": "Review report",
            "output_format": "files",
            "audience": audience
        });
        std::fs::write(dir.join(format!("{id}.json")), raw.to_string()).unwrap();
    }
    #[test]
    fn inherited_and_intersecting_audiences_never_become_public() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            task_packet_audience(home.path(), "task")
                .unwrap()
                .is_empty()
        );
        packet(
            home.path(),
            "a",
            vec!["user:alice".into(), "user:bob".into()],
        );
        packet(home.path(), "b", vec!["user:alice".into()]);
        assert_eq!(
            task_packet_audience(home.path(), "task").unwrap(),
            vec!["user:alice"]
        );
        packet(home.path(), "c", vec!["user:eve".into()]);
        assert_eq!(
            task_packet_audience(home.path(), "task").unwrap(),
            vec!["__no_permitted_audience__"]
        );
    }
    #[test]
    fn only_human_facing_entries_limit_people() {
        let home = tempfile::tempdir().unwrap();
        packet(home.path(), "a", vec!["verifier".into(), "executor".into()]);
        assert_eq!(task_audience(home.path(), "task"), TaskAudience::Open);
        packet(home.path(), "b", vec!["verifier".into(), "user:alice".into()]);
        assert_eq!(
            task_audience(home.path(), "task"),
            TaskAudience::Limited(vec!["user:alice".into()])
        );
        assert!(!is_human_audience_entry("user:"), "bare prefix names nobody");
        assert!(!is_human_audience_entry("users:alice"));
        assert!(!is_human_audience_entry("verifier"));
        let mut viewer = duduclaw_auth::UserContext::admin_fallback();
        viewer.user_id = "bob".into();
        viewer.role = duduclaw_auth::UserRole::Manager;
        let limited = TaskAudience::Limited(vec!["user:alice".into()]);
        assert!(!dashboard_may_read_task(&viewer, &limited));
        assert!(dashboard_may_read(&viewer, &["verifier".into()]));
        assert!(!dashboard_may_read_task(&viewer, &TaskAudience::Unreadable));
        let admin = duduclaw_auth::UserContext::admin_fallback();
        assert!(dashboard_may_read_task(&admin, &limited));
        assert!(dashboard_may_read_task(&admin, &TaskAudience::Unreadable));
        assert!(dashboard_may_read(&admin, &[NO_PERMITTED_AUDIENCE.into()]));
        assert!(!dashboard_may_read(&viewer, &[NO_PERMITTED_AUDIENCE.into()]));
    }

    #[test]
    fn unreadable_or_corrupt_packet_cannot_drop_private_constraint() {
        let home = tempfile::tempdir().unwrap();
        packet(home.path(), "a", vec!["user:alice".into()]);
        let dir = home
            .path()
            .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
            .join("task")
            .join("1");
        std::fs::write(dir.join("corrupt.json"), "{").unwrap();
        assert!(task_packet_audience(home.path(), "task").is_err());
        assert!(task_packet_audience(home.path(), "../task").is_err());
    }

    /// A data directory whose path passes through a symlinked parent (a home
    /// relocated to another disk and linked back) stays readable: both the raw
    /// read-only open and the private-audience identity lookup succeed.
    #[cfg(unix)]
    #[test]
    fn readonly_db_reads_through_symlinked_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-home");
        std::fs::create_dir_all(&real).unwrap();
        let linked = tmp.path().join("linked-home");
        std::os::unix::fs::symlink(&real, &linked).unwrap();

        let users = duduclaw_auth::UserDb::new(&real.join("users.db")).unwrap();
        let user = users
            .create_user(
                "alice@test.invalid",
                "Alice",
                "isolated-test-password",
                duduclaw_auth::UserRole::Manager,
            )
            .unwrap();
        users
            .bind_agent(&user.id, "sales", duduclaw_auth::AccessLevel::Operator)
            .unwrap();
        drop(users);

        let db = readonly_db(&linked.join("users.db")).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let ctx = trusted_dashboard_principal(&linked, &user.id).unwrap();
        assert_eq!(
            ctx.agent_access.get("sales"),
            Some(&duduclaw_auth::AccessLevel::Operator)
        );
    }

    /// A symlink at the database file itself, or at its WAL sidecar, is still
    /// refused after the directory is resolved.
    #[cfg(unix)]
    #[test]
    fn readonly_db_refuses_symlinked_database_file_and_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let target = home.join("elsewhere.db");
        rusqlite::Connection::open(&target)
            .unwrap()
            .execute_batch("CREATE TABLE t(x)")
            .unwrap();

        std::os::unix::fs::symlink(&target, home.join("users.db")).unwrap();
        assert_eq!(
            readonly_db(&home.join("users.db")).unwrap_err(),
            "authority database path refused"
        );

        let plain = home.join("tasks.db");
        rusqlite::Connection::open(&plain)
            .unwrap()
            .execute_batch("CREATE TABLE t(x)")
            .unwrap();
        std::os::unix::fs::symlink(&target, home.join("tasks.db-wal")).unwrap();
        assert_eq!(
            readonly_db(&plain).unwrap_err(),
            "authority database path refused"
        );
    }
}
