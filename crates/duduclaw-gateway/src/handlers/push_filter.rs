//! Per-connection filter for dashboard WebSocket broadcast events (F3, F5-D).
//!
//! Every event the gateway broadcasts goes through [`PushGate::filter`] per
//! connection before it is sent. A session handover rechecks any retained
//! candidate with the replacement identity. The rule per event kind is in
//! [`classify`]; the full table is in `docs/guides/reviewable-workflow-drafts.md`
//! ("Live dashboard updates").
//!
//! - Task events follow the task gate (live identity, employee binding,
//!   TaskPacket audience): dropped, or reduced to the board card.
//! - Events about one AI employee (`agent_id` in the payload) reach only a
//!   connection bound to that employee (admins: all).
//! - Login and install output, channel delivery failures and any event this
//!   table does not know reach admins only (fail closed).
//! - A lock-screen (pre-auth) connection receives system status events only.
//!
//! Task/approval metadata is snapshotted once per broadcast frame. Reader
//! identity is read fresh in bounded blocking work for each delivery.
//! Agent identity is also re-read per frame. Other lookups are cached for
//! [`PUSH_CACHE_SECS`], reducing `users.db` reads for global broadcasts.
//! Revocation for those events reaches live updates within that many seconds;
//! task/approval/agent events and RPC reads re-check on every call.
use super::*;
use crate::review_evidence::audience::{TaskAudience, dashboard_may_read_task, task_audience};
use std::time::{Duration, Instant};

/// Identity cache lifetime for events other than task/approval/agent updates.
pub const PUSH_CACHE_SECS: u64 = 2;

/// What a kind of event needs before it may be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushRule {
    /// System status, no content: every connection, lock screen included.
    System,
    /// Authenticated connections, no further check (ids only).
    Authenticated,
    /// Task content: the task gate.
    Task,
    /// About one employee: binding on `payload.agent_id` (absent ⇒ authenticated).
    Agent,
    /// Autonomy scope: one visible employee, or an explicitly global change.
    Autonomy,
    /// Admins only.
    Admin,
    Approval,
    Never,
}

/// The rule for an event name (WsFrame `event`, or the `type` of a raw
/// JSON line). Unknown names are admin-only.
pub(crate) fn classify(event: &str) -> PushRule {
    match event {
        "system.status_changed"
        | "system.update_available"
        | "system.update_installed"
        | "system.update_progress"
        | "maintenance.status_changed" => PushRule::System,
        "events_lost" | "dashboard.navigate" | "task.removed" | "channel_queue_rejected" => PushRule::Authenticated,
        "task.created" | "task.updated" | "task.comment" | "activity.new"
        | "task.needs_human" | "goal.round_started" | "goal.round_finished"
        | "goal.done" | "goal.failed" => PushRule::Task,
        "approval.created" | "approval.decided" | "approval.expired" => PushRule::Approval,
        "agent.turn_started" | "agent.turn_finished" | "agent.status_changed" => PushRule::Agent,
        "run.step" => PushRule::Never,
        "autonomy.changed" => PushRule::Autonomy,
        "budget.breaker_open" | "autopilot.breaker_open" => PushRule::Admin,
        "chat.sessions.updated"
        | "plan.updated"
        | "canvas.updated"
        | "cron.changed"
        | "memory.changed"
        | "skill.changed"
        | "channel_config.changed" => PushRule::Agent,
        _ => PushRule::Admin,
    }
}

/// A snapshot belongs to exactly one broadcast frame, never to an entity TTL.
/// The fan-out worker constructs it once, before any connection sees it.
pub(crate) struct PreparedPush {
    line: String,
    task: Option<(String, Option<String>, TaskAudience)>,
    approval: Option<(crate::approval::ApprovalRecord, super::approval_visibility::ApprovalVisibility)>,
}

/// Bound both queued blocking work and concurrent disk reads across sockets.
/// Run-step authorization uses this same pool; do not add another one.
pub(crate) async fn push_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    static LIMIT: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let permit = LIMIT.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(8)))
        .clone().acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || { let _permit = permit; work() }).await.ok()
}

impl PreparedPush {
    pub(crate) async fn prepare(home: PathBuf, line: String) -> Option<Self> {
        push_blocking(move || Self::load(&home, line)).await.flatten()
    }

    fn load(home: &Path, mut line: String) -> Option<Self> {
        let frame: Value = serde_json::from_str(&line).ok()?;
        let event = frame.get("event").or_else(|| frame.get("type"))?.as_str()?;
        let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
        let mut task = None;
        let mut approval = None;
        match classify(event) {
            PushRule::Task => {
                let id = if matches!(event, "task.created" | "task.updated") {
                    payload.get("id")
                } else { payload.get("task_id") }.and_then(Value::as_str);
                if let Some(id) = id {
                    if !duduclaw_core::is_valid_agent_id(id) { return None; }
                    let (owner, status) = crate::task_store::TaskStore::live_metadata(home, id).ok()??;
                    if !duduclaw_core::is_valid_agent_id(&owner) { return None; }
                    if matches!(event, "task.created" | "task.updated") {
                        if payload.get("assigned_to").and_then(Value::as_str).is_some_and(|v| v != owner) { return None; }
                        line = serde_json::to_string(&WsFrame::event(event,
                            json!({"id":id, "assigned_to":owner, "status":status}))).ok()?;
                    }
                    task = Some((id.to_owned(), Some(owner), task_audience(home, id)));
                }
            }
            PushRule::Approval => {
                let id = payload.get("id")?.as_str()?;
                if !duduclaw_core::is_valid_agent_id(id) || uuid::Uuid::parse_str(id).is_err() { return None; }
                let record = crate::approval::ApprovalStore::get_readonly(home,
                    &crate::approval::ApprovalId::from(id.to_owned())).ok()??;
                let visibility = super::approval_visibility::ApprovalVisibility::load(home, &record);
                line = serde_json::to_string(&WsFrame::event(event, json!({
                    "id":id, "agent_id":record.agent_id, "action_kind":record.action_kind, "status":record.status,
                }))).ok()?;
                approval = Some((record, visibility));
            }
            _ => {}
        }
        Some(Self { line, task, approval })
    }
}

/// One connection's filter state.
#[derive(Clone)]
pub struct PushGate {
    home: PathBuf,
    ctx: UserContext,
    pre_auth: bool,
    legacy_admin_authenticated: bool,
    live: Option<(Instant, Option<UserContext>)>,
    dropped: u64,
    task_snapshot: Option<(String, Option<String>, TaskAudience)>,
}

impl PushGate {
    pub fn new(home: &Path, ctx: &UserContext, pre_auth: bool) -> Self {
        Self::new_with_legacy(home, ctx, pre_auth, false)
    }

    pub fn new_with_legacy(
        home: &Path,
        ctx: &UserContext,
        pre_auth: bool,
        legacy_admin_authenticated: bool,
    ) -> Self {
        Self {
            home: home.to_path_buf(),
            ctx: ctx.clone(),
            pre_auth,
            legacy_admin_authenticated,
            live: None,
            dropped: 0,
            task_snapshot: None,
        }
    }

    fn fresh(at: Instant) -> bool {
        at.elapsed() < Duration::from_secs(PUSH_CACHE_SECS)
    }

    fn live(&mut self) -> Option<UserContext> {
        if let Some((at, v)) = &self.live
            && Self::fresh(*at)
        {
            return v.clone();
        }
        let v = live_reader_context(&self.home, &self.ctx).ok();
        self.live = Some((Instant::now(), v.clone()));
        v
    }

    fn task(&mut self, task_id: &str) -> (Option<String>, TaskAudience) {
        if let Some((id, owner, audience)) = &self.task_snapshot {
            if id == task_id { return (owner.clone(), audience.clone()); }
        }
        let owner = task_owner_readonly(&self.home, task_id);
        let audience = task_audience(&self.home, task_id);
        (owner, audience)
    }

    fn drop_event(&mut self, event: &str, why: &str) -> Option<String> {
        self.dropped += 1;
        if self.dropped == 1 || self.dropped % 100 == 0 {
            tracing::debug!(event, why, dropped = self.dropped, "push event withheld");
        }
        None
    }

    /// `true` while this connection's live identity may still receive the
    /// gateway log tail (`logs.subscribe` is Manager+).
    pub fn allows_log_tail(&mut self) -> bool {
        !self.pre_auth
            && self
                .live()
                .is_some_and(|live| live.role.level() >= UserRole::Manager.level())
    }

    /// The frame to send for `line`, a redacted frame, or `None`.
    pub fn filter(&mut self, line: &str) -> Option<String> {
        let Ok(Value::Object(frame)) = serde_json::from_str::<Value>(line) else {
            return self.drop_event("unparsable", "not a JSON object");
        };
        let event = frame
            .get("event")
            .or_else(|| frame.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let rule = classify(&event);
        if rule == PushRule::System {
            return Some(line.to_string());
        }
        if self.pre_auth {
            return self.drop_event(&event, "lock screen");
        }
        let is_login_transcript = matches!(event.as_str(), "auth.cli_login.output" | "auth.cli_login.status");
        // Sensitive transcripts bypass the two-second push cache so role
        // revocation takes effect on the very next PTY chunk.
        let live = if is_login_transcript {
            if self.ctx.user_id == "system" && self.legacy_admin_authenticated {
                Some(self.ctx.clone())
            } else {
                crate::review_evidence::audience::fresh_dashboard_context(
                    &self.home,
                    &self.ctx,
                ).ok()
            }
        } else if matches!(rule, PushRule::Task | PushRule::Approval | PushRule::Agent | PushRule::Autonomy) {
            live_reader_context(&self.home, &self.ctx).ok()
        } else {
            self.live()
        };
        let Some(live) = live else {
            tracing::warn!(event, "push event withheld: identity could not be re-read");
            return self.drop_event(&event, "identity unavailable");
        };
        let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
        // PTY output can contain a long-lived provider token. Even another
        // Admin must never receive a login transcript they did not start.
        if is_login_transcript
            && (live.user_id == "system"
                || !live.is_admin()
                || payload.get("owner_user_id").and_then(Value::as_str)
                    != Some(live.user_id.as_str()))
        {
            return self.drop_event(&event, "login session belongs to another user");
        }
        match rule {
            PushRule::System | PushRule::Authenticated => Some(line.to_string()),
            PushRule::Never => None,
            PushRule::Approval => self.drop_event(&event, "approval requires live asynchronous gate"),
            PushRule::Admin => {
                if live.is_admin() {
                    Some(line.to_string())
                } else {
                    self.drop_event(&event, "admin only")
                }
            }
            PushRule::Agent => {
                match payload
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .filter(|a| !a.is_empty())
                {
                    Some(agent) if !live.has_agent_access(agent, AccessLevel::Viewer) => {
                        self.drop_event(&event, "no binding")
                    }
                    None if matches!(event.as_str(), "agent.turn_started" | "agent.turn_finished" | "agent.status_changed") => self.drop_event(&event, "employee event without employee"),
                    None if !live.is_admin() => self.drop_event(&event, "unscoped employee event"),
                    _ => Some(line.to_string()),
                }
            }
            PushRule::Autonomy => {
                let agent = payload.get("agent_id").and_then(Value::as_str);
                match (agent, payload.get("all").and_then(Value::as_bool)) {
                    (Some(id), Some(false)) if !id.is_empty()
                        && live.has_agent_access(id, AccessLevel::Viewer) => Some(line.to_string()),
                    (None, Some(true)) => Some(line.to_string()),
                    _ => self.drop_event(&event, "invalid autonomy scope or no binding"),
                }
            }
            PushRule::Task => self.filter_task(&event, line, &payload, &live),
        }
    }

    /// One-shot entry for callers without a shared broadcast frame.
    pub async fn filter_live(&mut self, line: &str, _handler: &MethodHandler) -> Option<String> {
        let frame = PreparedPush::prepare(self.home.clone(), line.to_owned()).await?;
        self.filter_prepared(std::sync::Arc::new(frame)).await
    }

    /// Fresh reader I/O runs under the same bounded blocking pool as snapshots.
    /// Only the immutable entity snapshot is shared between connections.
    pub(crate) async fn filter_prepared(&mut self, frame: std::sync::Arc<PreparedPush>) -> Option<String> {
        let mut gate = self.clone();
        let (next, result) = push_blocking(move || {
            gate.task_snapshot = frame.task.clone();
            let result = if let Some((record, visibility)) = &frame.approval {
                if gate.pre_auth { None } else {
                    super::approval_visibility::fresh_reader(&gate.home, &gate.ctx).and_then(|(fresh, department)| {
                        visibility.may_read(record, &fresh, department.as_deref()).then(|| frame.line.clone())
                    })
                }
            } else { gate.filter(&frame.line) };
            gate.task_snapshot = None;
            (gate, result)
        }).await?;
        *self = next;
        result
    }

    fn filter_task(
        &mut self,
        event: &str,
        line: &str,
        payload: &Value,
        live: &UserContext,
    ) -> Option<String> {
        let task_id = match event {
            "task.created" | "task.updated" => payload.get("id"),
            _ => payload.get("task_id"),
        }
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
        let Some(task_id) = task_id else {
            // An activity row not tied to a task: about its employee.
            return match payload
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
            {
                Some(agent) if !live.has_agent_access(agent, AccessLevel::Viewer) => {
                    self.drop_event(event, "no binding")
                }
                _ if event == "activity.new" => Some(line.to_string()),
                _ => self.drop_event(event, "task event without task"),
            };
        };
        if !duduclaw_core::is_valid_agent_id(&task_id) {
            return self.drop_event(event, "invalid task id");
        }
        let (owner, audience) = self.task(&task_id);
        let Some(owner) = owner.filter(|id| duduclaw_core::is_valid_agent_id(id)) else {
            return self.drop_event(event, "unknown task");
        };
        if matches!(event, "task.created" | "task.updated")
            && payload.get("assigned_to").and_then(Value::as_str).is_some_and(|id| id != owner)
        {
            return self.drop_event(event, "task owner mismatch");
        }
        if !live.has_agent_access(&owner, AccessLevel::Viewer) {
            return self.drop_event(event, "no binding");
        }
        if dashboard_may_read_task(live, &audience) {
            return Some(line.to_string());
        }
        let redacted = match event {
            "task.created" | "task.updated" => restricted_task_json(payload),
            "activity.new" => restricted_activity_json(payload),
            _ => return self.drop_event(event, "audience"),
        };
        serde_json::to_string(&WsFrame::Event {
            event: event.to_string(),
            payload: redacted,
            seq: None,
            state_version: None,
            boot_id: None,
        })
        .ok()
    }
}

/// One-shot form (tests and callers without a connection).
pub fn filter_push_event(home: &Path, ctx: &UserContext, line: &str) -> Option<String> {
    PushGate::new(home, ctx, false).filter(line)
}

#[cfg(test)]
mod live_event_tests {
    use super::*;
    #[tokio::test]
    async fn next_agent_frame_rechecks_demotion_suspension_and_binding() {
        use crate::handlers::task_privacy_tests::{admin, fixture};
        let f = fixture().await;
        let root = admin(&f);
        let mut gate = PushGate::new(&f.home, &root, false);
        let frame = serde_json::to_string(&WsFrame::event("agent.turn_started", json!({"agent_id": "sales"}))).unwrap();
        // Warm even the old identity cache through an admin-only event.
        assert!(gate.filter(&serde_json::to_string(&WsFrame::event("unknown.admin", json!({}))).unwrap()).is_some());
        assert!(gate.filter_live(&frame, &f.handler).await.is_some());
        f.db.update_user(&root.user_id, None, Some(UserRole::Employee), None).unwrap();
        assert!(gate.filter_live(&frame, &f.handler).await.is_none());
        f.db.update_user(&root.user_id, None, Some(UserRole::Admin), None).unwrap();
        assert!(gate.filter_live(&frame, &f.handler).await.is_some());
        f.db.set_user_status(&root.user_id, duduclaw_auth::UserStatus::Suspended).unwrap();
        assert!(gate.filter_live(&frame, &f.handler).await.is_none());
        let mut alice = PushGate::new(&f.home, &f.alice, false);
        assert!(alice.filter_live(&frame, &f.handler).await.is_some());
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        assert!(alice.filter_live(&frame, &f.handler).await.is_none());
    }

    #[tokio::test]
    async fn task_audience_filters_before_sequence_assignment() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let mut bob = PushGate::new(&f.home, &f.bob, false);
        let private = serde_json::to_string(&WsFrame::event("goal.round_started",
            json!({"task_id": crate::handlers::task_privacy_tests::PRIVATE, "agent_id":"sales"}))).unwrap();
        assert!(bob.filter(&private).is_none());
        let open = serde_json::to_string(&WsFrame::event("goal.done",
            json!({"task_id": crate::handlers::task_privacy_tests::OPEN, "agent_id":"sales"}))).unwrap();
        let mut seq = crate::live_events::EventSequence::default();
        let encoded = seq.filtered(&bob.filter(&open).unwrap()).unwrap();
        let frame: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(frame["seq"], 1);
    }

    #[tokio::test]
    async fn approval_push_uses_stored_audience_kind_and_manager_gate() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "tool_call", "secret prompt",
            json!({"task_id": crate::handlers::task_privacy_tests::PRIVATE}), 300).await.unwrap();
        let line = serde_json::to_string(&WsFrame::event("approval.created",
            json!({"id": id.as_str(), "agent_id":"forged", "summary":"secret"}))).unwrap();
        let mut alice = PushGate::new(&f.home, &f.alice, false);
        let mut bob = PushGate::new(&f.home, &f.bob, false);
        assert!(bob.filter_live(&line, &f.handler).await.is_none());
        let allowed = alice.filter_live(&line, &f.handler).await.unwrap();
        assert!(!allowed.contains("secret")); assert!(!allowed.contains("forged"));
        let employee = f.db.create_user("live-reader@test.invalid", "Reader", "test-password-strong", UserRole::Employee).unwrap();
        f.db.bind_agent(&employee.id, "sales", AccessLevel::Owner).unwrap();
        let ctx = f.db.live_user_context(&employee.id).unwrap().unwrap();
        let id = broker.request("sales", "tool_call", "secret", json!({}), 300).await.unwrap();
        let line = serde_json::to_string(&WsFrame::event("approval.created", json!({"id": id.as_str()}))).unwrap();
        assert!(PushGate::new(&f.home, &ctx, false).filter_live(&line, &f.handler).await.is_none());
        let id = broker.request("sales", "computer_workspace_admin", "secret", json!({}), 300).await.unwrap();
        let line = serde_json::to_string(&WsFrame::event("approval.created", json!({"id": id.as_str()}))).unwrap();
        assert!(alice.filter_live(&line, &f.handler).await.is_none());
        let admin = crate::handlers::task_privacy_tests::admin(&f);
        assert!(PushGate::new(&f.home, &admin, false).filter_live(&line, &f.handler).await.is_some());
    }

    #[tokio::test]
    async fn direct_task_push_carries_no_task_text() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        f.handler.set_event_tx(tx).await;
        f.handler.broadcast_event("task.updated", json!({"id":"t", "assigned_to":"sales", "status":"done",
            "description":"secret prompt", "result_summary":"secret result"})).await;
        let line = rx.recv().await.unwrap();
        assert!(!line.contains("secret")); assert!(!line.contains("description"));
    }

    #[tokio::test]
    async fn forged_owner_and_immediate_revocation_never_use_cached_authority() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let id = crate::handlers::task_privacy_tests::OPEN;
        let mut gate = PushGate::new(&f.home, &f.alice, false);
        let line = |owner: &str| serde_json::to_string(&WsFrame::event("task.updated",
            json!({"id":id, "assigned_to":owner}))).unwrap();
        assert!(gate.filter_live(&line("sales"), &f.handler).await.is_some());
        assert!(gate.filter_live(&line("attacker"), &f.handler).await.is_none());
        assert!(gate.filter(&line("attacker")).is_none());
        // Warm both caches, then revoke without waiting for their TTL.
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        assert!(gate.filter_live(&line("sales"), &f.handler).await.is_none());
    }

    #[tokio::test]
    async fn owner_and_audience_changes_apply_to_next_task_event() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let id = crate::handlers::task_privacy_tests::PRIVATE;
        let mut gate = PushGate::new(&f.home, &f.alice, false);
        let line = serde_json::to_string(&WsFrame::event("goal.done", json!({"task_id":id}))).unwrap();
        assert!(gate.filter(&line).is_some());
        let packet = f.home.join(duduclaw_core::task_packet::TEAM_PACKETS_DIR).join(id).join("1/p.json");
        let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&packet).unwrap()).unwrap();
        value["audience"] = json!([format!("user:{}", f.bob.user_id)]);
        std::fs::write(packet, value.to_string()).unwrap();
        assert!(gate.filter(&line).is_none());
        let conn = rusqlite::Connection::open(f.home.join("tasks.db")).unwrap();
        conn.execute("UPDATE tasks SET assigned_to='other' WHERE id=?1", params![id]).unwrap();
        assert!(gate.filter(&line).is_none());
        // A forged owner that the reader IS bound to cannot reveal a task
        // currently owned by a different, unbound employee.
        let forged = serde_json::to_string(&WsFrame::event("task.updated",
            json!({"id":id, "assigned_to":"sales"}))).unwrap();
        assert!(gate.filter(&forged).is_none());
        assert!(gate.filter_live(&forged, &f.handler).await.is_none());
    }

    #[tokio::test]
    async fn shared_frame_reuses_task_snapshot_but_never_reader_identity() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let id = crate::handlers::task_privacy_tests::PRIVATE;
        let line = serde_json::to_string(&WsFrame::event("goal.done", json!({"task_id":id}))).unwrap();
        let frame = std::sync::Arc::new(PreparedPush::prepare(f.home.clone(), line.clone()).await.unwrap());
        // Removing the packet after preparation must not cause per-reader I/O
        // or accidentally widen the audience of this already prepared frame.
        let packet = f.home.join(duduclaw_core::task_packet::TEAM_PACKETS_DIR).join(id).join("1/p.json");
        std::fs::remove_file(packet).unwrap();
        let mut alice = PushGate::new(&f.home, &f.alice, false);
        let mut bob = PushGate::new(&f.home, &f.bob, false);
        assert!(alice.filter_prepared(frame.clone()).await.is_some());
        assert!(bob.filter_prepared(frame.clone()).await.is_none());
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        assert!(alice.filter_prepared(frame).await.is_none());
        // The next frame gets a new snapshot, even with identical wire JSON.
        let next = std::sync::Arc::new(PreparedPush::prepare(f.home.clone(), line).await.unwrap());
        assert!(bob.filter_prepared(next).await.is_some());
    }

    #[tokio::test]
    async fn autonomy_frame_rechecks_binding_after_cache_warmup() {
        let f = crate::handlers::task_privacy_tests::fixture().await;
        let line = serde_json::to_string(&WsFrame::event("autonomy.changed",
            json!({"agent_id":"sales", "all":false, "paused":true}))).unwrap();
        let frame = std::sync::Arc::new(PreparedPush::prepare(f.home.clone(), line).await.unwrap());
        let mut alice = PushGate::new(&f.home, &f.alice, false);
        assert!(alice.filter_prepared(frame.clone()).await.is_some());
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        assert!(alice.filter_prepared(frame).await.is_none());
    }

    #[test]
    fn live_event_rules_are_explicit_and_unknown_is_restricted() {
        for name in ["agent.turn_started", "agent.turn_finished", "agent.status_changed"] { assert_eq!(classify(name), PushRule::Agent); }
        for name in ["task.needs_human", "goal.round_started", "goal.round_finished", "goal.done", "goal.failed"] { assert_eq!(classify(name), PushRule::Task); }
        for name in ["approval.created", "approval.decided", "approval.expired"] { assert_eq!(classify(name), PushRule::Approval); }
        for name in ["unknown", "agent.unknown", "task.unknown", "approval.unknown", "budget.breaker_open", "autopilot.breaker_open"] { assert_eq!(classify(name), PushRule::Admin); }
        assert_eq!(classify("run.step"), PushRule::Never);
    }
}

#[cfg(test)]
mod mfa_push_tests {
    use super::*;

    #[test]
    fn push_filter_mfa_retired_admin_receives_no_login_transcript() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("push@example.com", "Push", "password-test", UserRole::Admin).unwrap();
        let ctx = db.live_user_context(&user.id).unwrap().unwrap();
        let mut gate = PushGate::new(home.path(), &ctx, false);
        let frame = serde_json::json!({"event":"auth.cli_login.output", "payload":{"owner_user_id":user.id,"text":"secret"}}).to_string();
        assert!(gate.filter(&frame).is_some());
        let raw = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        raw.execute("ALTER TABLE users RENAME TO saved_users", []).unwrap();
        assert!(gate.filter(&frame).is_none());
        raw.execute("ALTER TABLE saved_users RENAME TO users", []).unwrap();
        assert!(gate.filter(&frame).is_some());
        db.mfa_operator_reset(&user.email).unwrap();
        assert!(gate.filter(&frame).is_none());
    }
}
