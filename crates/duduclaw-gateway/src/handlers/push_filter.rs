//! Per-connection filter for dashboard WebSocket broadcast events (F3, F5-D).
//!
//! Every event the gateway broadcasts goes through [`PushGate::filter`] once
//! per connection before it is sent. The rule per event kind is in
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
//! Identity and task lookups are cached per connection for
//! [`PUSH_CACHE_SECS`]: a broadcast storm does not re-read `users.db` and the
//! packet directory per event. Revocation therefore reaches live updates
//! within that many seconds; RPC reads re-check on every call.
use super::*;
use crate::review_evidence::audience::{TaskAudience, dashboard_may_read_task, task_audience};
use std::time::{Duration, Instant};

/// How long a connection reuses its identity and task lookups.
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
    /// Admins only.
    Admin,
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
        "dashboard.navigate" | "task.removed" | "channel_queue_rejected" => PushRule::Authenticated,
        "task.created" | "task.updated" | "task.comment" | "activity.new" => PushRule::Task,
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

/// One connection's filter state.
pub struct PushGate {
    home: PathBuf,
    ctx: UserContext,
    pre_auth: bool,
    live: Option<(Instant, Option<UserContext>)>,
    tasks: HashMap<String, (Instant, Option<String>, TaskAudience)>,
    dropped: u64,
}

impl PushGate {
    pub fn new(home: &Path, ctx: &UserContext, pre_auth: bool) -> Self {
        Self {
            home: home.to_path_buf(),
            ctx: ctx.clone(),
            pre_auth,
            live: None,
            tasks: HashMap::new(),
            dropped: 0,
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
        if let Some((at, owner, audience)) = self.tasks.get(task_id)
            && Self::fresh(*at)
        {
            return (owner.clone(), audience.clone());
        }
        let owner = task_owner_readonly(&self.home, task_id);
        let audience = task_audience(&self.home, task_id);
        if self.tasks.len() > 512 {
            self.tasks.clear();
        }
        self.tasks.insert(
            task_id.to_string(),
            (Instant::now(), owner.clone(), audience.clone()),
        );
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
        let Some(live) = self.live() else {
            tracing::warn!(event, "push event withheld: identity could not be re-read");
            return self.drop_event(&event, "identity unavailable");
        };
        let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
        match rule {
            PushRule::System | PushRule::Authenticated => Some(line.to_string()),
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
                    _ => Some(line.to_string()),
                }
            }
            PushRule::Task => self.filter_task(&event, line, &payload, &live),
        }
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
        let (stored_owner, audience) = self.task(&task_id);
        let owner = match payload.get("assigned_to").and_then(Value::as_str) {
            Some(o) if event == "task.created" || event == "task.updated" => Some(o.to_string()),
            _ => stored_owner,
        };
        let Some(owner) = owner else {
            // A removed task: admins only (P-L9).
            return if live.is_admin() {
                Some(line.to_string())
            } else {
                self.drop_event(event, "unknown task")
            };
        };
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
        })
        .ok()
    }
}

/// One-shot form (tests and callers without a connection).
pub fn filter_push_event(home: &Path, ctx: &UserContext, line: &str) -> Option<String> {
    PushGate::new(home, ctx, false).filter(line)
}
