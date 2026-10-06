//! One gate for every dashboard read of a task's content (F3, V-H-1).
//!
//! A task's content is anything beyond its board card: description, result
//! summary, acceptance criteria, judge feedback, iterations, comments,
//! file-change snippets, role turns, activity summaries, artifacts. Two rules
//! apply to all of it, on every call:
//!
//! 1. **Live identity.** Role, status, forced password change and agent
//!    bindings are re-read from `users.db` per call
//!    ([`live_reader_context`]); a connection that was downgraded or unbound
//!    after it authenticated cannot keep reading.
//! 2. **TaskPacket audience.** A task whose packets carry an audience is
//!    readable only by the listed dashboard identities, on top of the task's
//!    agent ACL ([`task_audience_allows`]).
//!
//! **A new RPC that returns task content MUST go through this module**:
//! [`MethodHandler::authorize_private_task_read`] for one task, and
//! [`TaskReader`] (`task_json` / `activity_json`) for rows in a list. Push
//! events go through [`filter_push_event`]. Nothing here widens access: every
//! failure (unreadable identity store, unreadable packet, unknown task) denies
//! with the same `permission denied` text.
use super::*;

pub(crate) const PERMISSION_DENIED: &str = "permission denied";

/// Fields of a task row that hold what the task produced or was told. A
/// reader outside the audience gets the row with these emptied, so list
/// shapes and counts stay the same and nothing the task produced leaks.
const CONTENT_FIELDS: &[(&str, fn() -> Value)] = &[
    ("description", || Value::String(String::new())),
    ("tags", || Value::Array(Vec::new())),
    ("blocked_reason", || Value::Null),
    ("judge_feedback", || Value::Null),
    ("acceptance_criteria", || Value::Null),
    ("acceptance_criteria_baseline", || Value::Null),
    ("result_summary", || Value::Null),
    ("goal_state", || Value::Null),
    ("risk_boundary", || Value::Null),
    ("plan_pending", || Value::Null),
    ("message_id", || Value::Null),
];

/// Identity for one task-content read, re-read from the identity store.
///
/// The gateway admin token (`system` with the Admin role) is not a
/// `users.db` principal, so there is nothing to re-read or revoke there; it
/// keeps its admin role, and TaskPacket audiences still apply to it through
/// its own `user:system` / `role:admin` keys.
pub(crate) fn live_reader_context(home: &Path, ctx: &UserContext) -> Result<UserContext, ()> {
    if ctx.user_id == "system" && ctx.is_admin() {
        return Ok(ctx.clone());
    }
    crate::review_evidence::audience::fresh_dashboard_context(home, ctx).map_err(|_| ())
}

/// `true` when the task's packet audience (if any) admits this live reader.
/// An unreadable or corrupt packet set denies.
pub(crate) fn task_audience_allows(home: &Path, live: &UserContext, task_id: &str) -> bool {
    crate::review_evidence::audience::dashboard_may_read_task(
        live,
        &crate::review_evidence::audience::task_audience(home, task_id),
    )
}

/// `true` when `live` may read the content of the task owned by `owner`.
pub(crate) fn task_content_visible(
    home: &Path,
    live: &UserContext,
    task_id: &str,
    owner: &str,
) -> bool {
    live.has_agent_access(owner, AccessLevel::Viewer) && task_audience_allows(home, live, task_id)
}

/// The board card of a task whose content the reader may not see.
pub(crate) fn restricted_task_json(full: &Value) -> Value {
    let mut out = full.clone();
    if let Value::Object(map) = &mut out {
        for (key, empty) in CONTENT_FIELDS {
            if map.contains_key(*key) {
                map.insert((*key).to_string(), empty());
            }
        }
        map.insert("restricted".into(), Value::Bool(true));
    }
    out
}

/// An activity row whose task content the reader may not see: what happened
/// and when, without the summary text or metadata.
pub(crate) fn restricted_activity_json(full: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in ["id", "type", "agent_id", "task_id", "timestamp"] {
        if let Some(v) = full.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    out.insert("summary".into(), Value::String(String::new()));
    out.insert("metadata".into(), Value::Null);
    out.insert("restricted".into(), Value::Bool(true));
    Value::Object(out)
}

/// Per-call reader for list RPCs: one live identity, one audience lookup per
/// task (cached for the call).
pub(crate) struct TaskReader<'a> {
    home: &'a Path,
    live: UserContext,
    visible: std::sync::Mutex<HashMap<String, bool>>,
}

impl<'a> TaskReader<'a> {
    pub(crate) fn new(home: &'a Path, ctx: &UserContext) -> Result<Self, WsFrame> {
        let live = live_reader_context(home, ctx)
            .map_err(|_| WsFrame::error_response("", PERMISSION_DENIED))?;
        Ok(Self {
            home,
            live,
            visible: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn live(&self) -> &UserContext {
        &self.live
    }

    pub(crate) fn can_read(&self, task_id: &str, owner: &str) -> bool {
        let cached = self
            .visible
            .lock()
            .ok()
            .and_then(|m| m.get(task_id).copied());
        if let Some(v) = cached {
            return v;
        }
        let v = task_content_visible(self.home, &self.live, task_id, owner);
        if let Ok(mut m) = self.visible.lock() {
            m.insert(task_id.to_string(), v);
        }
        v
    }

    /// Like [`Self::can_read`] for a task whose owner may be unknown: a task
    /// that no longer exists (`owner == None`) is readable by admins only.
    pub(crate) fn can_read_owned(&self, task_id: &str, owner: Option<&str>) -> bool {
        match owner {
            Some(owner) => self.can_read(task_id, owner),
            None => self.live.is_admin(),
        }
    }

    /// Full row when readable, the board card otherwise.
    pub(crate) fn task_json(&self, row: &TaskRow) -> Value {
        let full = task_row_to_json(row);
        if self.can_read(&row.id, &row.assigned_to) {
            full
        } else {
            restricted_task_json(&full)
        }
    }

    /// Activity rows tied to a task the reader may not read lose their text.
    /// `owner` is the task's owning agent (`None` = removed task ⇒ admins
    /// only).
    pub(crate) fn activity_json(&self, row: &ActivityRow, owner: Option<&str>) -> Value {
        let full = activity_row_to_json(row);
        match row.task_id.as_deref().filter(|t| !t.is_empty()) {
            Some(task_id) if !self.can_read_owned(task_id, owner) => {
                restricted_activity_json(&full)
            }
            _ => full,
        }
    }
}

/// Owning agent of every task referenced by `rows`, read once per call.
pub(crate) async fn activity_task_owners(
    store: &TaskStore,
    rows: &[ActivityRow],
) -> HashMap<String, String> {
    let mut owners = HashMap::new();
    for task_id in rows.iter().filter_map(|r| r.task_id.as_deref()) {
        if task_id.is_empty() || owners.contains_key(task_id) {
            continue;
        }
        if let Ok(Some(t)) = store.get_task(task_id).await {
            owners.insert(task_id.to_string(), t.assigned_to);
        }
    }
    owners
}

impl MethodHandler {
    /// THE entry for reading one task's content (see the module docs). Uses
    /// the live identity, the task's agent ACL at `level` and its packet
    /// audience; returns the task and the live identity for further checks.
    /// Every refusal is `permission denied` (an admin asking for a task that
    /// does not exist still gets "not found").
    pub(crate) async fn authorize_private_task_read(
        &self,
        store: &TaskStore,
        ctx: &UserContext,
        task_id: &str,
        level: AccessLevel,
    ) -> Result<(TaskRow, UserContext), WsFrame> {
        let denied = || WsFrame::error_response("", PERMISSION_DENIED);
        let live = live_reader_context(&self.home_dir, ctx).map_err(|_| denied())?;
        let task = match store.get_task(task_id).await {
            Ok(Some(row)) => row,
            Ok(None) if live.is_admin() => {
                return Err(WsFrame::error_response(
                    "",
                    &format!("Task not found: {task_id}"),
                ));
            }
            Ok(None) => return Err(denied()),
            Err(e) => return Err(WsFrame::error_response("", &format!("get task: {e}"))),
        };
        let audience = crate::review_evidence::audience::task_audience(&self.home_dir, task_id);
        if !live.has_agent_access(&task.assigned_to, level)
            || !crate::review_evidence::audience::dashboard_may_read_task(&live, &audience)
        {
            return Err(denied());
        }
        if audience == crate::review_evidence::audience::TaskAudience::Unreadable {
            // Only an admin gets here; leave a visible trace once.
            crate::review_evidence::audience::note_audience_unreadable(store, task_id).await;
        }
        Ok((task, live))
    }
}

impl MethodHandler {
    /// [`Self::authorize_private_task_read`] for content that outlives its
    /// task (round transcripts, activity): when the task was removed, an
    /// admin may still read it and everyone else is refused.
    pub(crate) async fn authorize_task_content_read(
        &self,
        store: &TaskStore,
        ctx: &UserContext,
        task_id: &str,
        level: AccessLevel,
    ) -> Result<(), WsFrame> {
        let denied = || WsFrame::error_response("", PERMISSION_DENIED);
        if matches!(store.get_task(task_id).await, Ok(None)) {
            let live = live_reader_context(&self.home_dir, ctx).map_err(|_| denied())?;
            return if live.is_admin() { Ok(()) } else { Err(denied()) };
        }
        self.authorize_private_task_read(store, ctx, task_id, level)
            .await
            .map(|_| ())
    }
}

/// Owning agent of a task, read without opening a writable store (push-event
/// path; the gateway is the only writer).
pub(crate) fn task_owner_readonly(home: &Path, task_id: &str) -> Option<String> {
    let db = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).ok()?;
    db.query_row(
        "SELECT assigned_to FROM tasks WHERE id=?1",
        [task_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// The task an approval card was built from, when its payload names one
/// (`goal_kickoff` → `task_id`; automation cards → the triggering event's
/// `task_id` / `task.id` / `activity.task_id`).
pub(crate) fn approval_task_ref(payload: &Value) -> Option<String> {
    let fields = payload.get("fields");
    [
        payload.get("task_id"),
        fields.and_then(|f| f.get("task_id")),
        fields.and_then(|f| f.get("task")).and_then(|t| t.get("id")),
        fields
            .and_then(|f| f.get("activity"))
            .and_then(|a| a.get("task_id")),
    ]
    .into_iter()
    .flatten()
    .find_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string))
}
