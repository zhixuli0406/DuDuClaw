//! List snapshots are authoritative; events only invalidate them.
use crate::{
    Error, Result,
    auth::Role,
    client::{Client, ConnectionInfo, ConnectionState, ServerEvent},
    sanitize::TerminalText,
    strings,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
    time::Instant,
};

pub const POLL_INTERVAL: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Deserialize)]
pub struct Employee {
    /// Opaque RPC key, never draw this directly; use name_text().
    name: String,
    pub display_name: TerminalText,
    pub status: TerminalText,
    pub archived: bool,
}
impl Employee {
    pub fn id(&self) -> &str {
        &self.name
    }
    pub fn name_text(&self) -> TerminalText {
        TerminalText::new(self.name.clone())
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct Approval {
    id: String,
    pub agent_id: TerminalText,
    pub kind: TerminalText,
    pub summary: TerminalText,
    pub status: TerminalText,
    pub request_kind: Option<TerminalText>,
    pub expires_at: Option<i64>,
    pub decided_in_dashboard_only: bool,
    // Keep the entire detail as safe text for the phase-2 detail pane. This
    // preserves visible-control inspection without exposing raw strings to UI.
    #[serde(skip)]
    pub detail: TerminalText,
}
impl Approval {
    pub fn id(&self) -> &str {
        &self.id
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct HumanTask {
    id: String,
    pub title: TerminalText,
    pub assigned_to: TerminalText,
    pub status: TerminalText,
    pub pause_reason: Option<TerminalText>,
    #[serde(default)]
    pub goal_mode: bool,
    #[serde(skip)]
    pub detail: TerminalText,
}
impl HumanTask {
    pub fn id(&self) -> &str {
        &self.id
    }
}
#[derive(Clone, Debug)]
pub enum InboxItem {
    Approval(Approval),
    Task(HumanTask),
}
impl InboxItem {
    pub fn source_label(&self) -> &'static str {
        match self {
            Self::Approval(a)
                if a.request_kind
                    .as_ref()
                    .is_some_and(|k| k.plain() == "question") =>
            {
                strings::SOURCE_QUESTION
            }
            Self::Approval(_) => strings::SOURCE_APPROVAL,
            Self::Task(_) => strings::SOURCE_GOAL,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Snapshot<T> {
    pub items: Vec<T>,
    pub stale: bool,
    pub refetch_pending: bool,
    pub last_success: Option<Instant>,
    pub error: Option<Error>,
}
impl<T> Default for Snapshot<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            stale: true,
            refetch_pending: true,
            last_success: None,
            error: None,
        }
    }
}
impl<T> Snapshot<T> {
    pub fn count(&self) -> Option<usize> {
        (!self.stale && !self.refetch_pending).then_some(self.items.len())
    }
    fn invalidate(&mut self) {
        self.stale = true;
        self.refetch_pending = true;
    }
    fn replace(&mut self, items: Vec<T>) {
        self.items = items;
        self.stale = false;
        self.refetch_pending = false;
        self.last_success = Some(Instant::now());
        self.error = None;
    }
}
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub inbox: Snapshot<InboxItem>,
    pub employees: Snapshot<Employee>,
    pub role: Option<Role>,
    pub login_required: Option<Error>,
    boot_id: Option<String>,
    seq: Option<u64>,
    generation: u64,
    revision: u64,
}
impl Model {
    pub fn invalidate_all(&mut self) {
        self.inbox.invalidate();
        self.employees.invalidate();
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn connected(&mut self, info: &ConnectionInfo) {
        self.generation = info.generation;
        self.role = Some(info.role);
        self.boot_id = info.boot_id.clone();
        self.seq = None;
        self.login_required = None;
        self.invalidate_all();
    }
    pub fn event(&mut self, event: &ServerEvent) {
        let boot_changed = event
            .boot_id
            .as_ref()
            .is_some_and(|boot| self.boot_id.as_ref().is_some_and(|old| old != boot));
        if let Some(boot) = &event.boot_id {
            self.boot_id = Some(boot.clone());
        }
        if boot_changed {
            self.seq = None;
            self.invalidate_all();
        }
        if let Some(seq) = event.seq {
            // A gap or a regression invalidates; exact duplicates are harmless.
            if self
                .seq
                .is_some_and(|old| seq != old && old.checked_add(1) != Some(seq))
            {
                self.invalidate_all();
            }
            self.seq = Some(seq);
        }
        match event.name.as_str() {
            "events_lost" => self.invalidate_all(),
            "approval.created" | "approval.decided" | "approval.expired" => {
                self.inbox.invalidate();
                self.revision = self.revision.wrapping_add(1);
            }
            "agent.status_changed"
            | "agent.turn_started"
            | "agent.turn_finished"
            | "autonomy.changed" => self.invalidate_all(),
            name if matches!(name.split_once('.'), Some(("task" | "goal", _))) => {
                self.invalidate_all()
            }
            _ => {}
        }
    }
    fn apply(&mut self, revision: u64, result: Result<Lists>) -> bool {
        if revision != self.revision {
            return false;
        }
        match result {
            Ok(lists) => {
                self.employees.replace(lists.employees);
                self.inbox.replace(lists.inbox);
            }
            Err(error) => {
                self.inbox.invalidate();
                self.employees.invalidate();
                self.inbox.error = Some(error);
                self.employees.error = Some(error);
            }
        }
        true
    }
}
struct Lists {
    employees: Vec<Employee>,
    inbox: Vec<InboxItem>,
}
async fn fetch(client: &Client, role: Role) -> Result<Lists> {
    #[derive(Deserialize)]
    struct Agents {
        agents: Vec<Employee>,
    }
    let agents: Agents =
        serde_json::from_value(client.call("agents.list", json!({}), RPC_TIMEOUT).await?)
            .map_err(|_| Error::Protocol)?;
    let approvals = client
        .call("approvals.list", json!({}), RPC_TIMEOUT)
        .await?;
    let values = approvals
        .get("approvals")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol)?;
    let mut inbox = Vec::new();
    for value in values {
        let mut approval: Approval =
            serde_json::from_value(value.clone()).map_err(|_| Error::Protocol)?;
        approval.detail = TerminalText::new(value.to_string());
        inbox.push(InboxItem::Approval(approval));
    }
    // Non-admin tasks.list_page requires an explicit permitted agent_id.
    // agents.list supplies only the caller's direct bindings.
    let scopes: Vec<Option<&str>> = if role == Role::Admin {
        vec![None]
    } else {
        agents.agents.iter().map(|a| Some(a.id())).collect()
    };
    let mut seen = HashSet::new();
    for scope in scopes {
        let mut offset = 0;
        let mut expected_total = None;
        loop {
            let mut params =
                json!({"status":"needs_human", "limit":200, "offset":offset, "archived":false});
            if let Some(agent) = scope {
                params["agent_id"] = json!(agent);
            }
            let response = client.call("tasks.list_page", params, RPC_TIMEOUT).await?;
            let rows = response
                .get("tasks")
                .and_then(Value::as_array)
                .ok_or(Error::Protocol)?;
            let total = response
                .get("total")
                .and_then(Value::as_u64)
                .ok_or(Error::Protocol)?;
            // Offset pagination is not a transaction. Detect observed movement
            // or truncation and keep counts unknown until a later poll succeeds.
            if expected_total.is_some_and(|old| old != total) || total > 10_000 {
                return Err(Error::Protocol);
            }
            expected_total = Some(total);
            for row in rows {
                let mut task: HumanTask =
                    serde_json::from_value(row.clone()).map_err(|_| Error::Protocol)?;
                task.detail = TerminalText::new(row.to_string());
                if !seen.insert(task.id.clone()) {
                    return Err(Error::Protocol);
                }
                inbox.push(InboxItem::Task(task));
            }
            offset += rows.len() as u64;
            if offset == total {
                break;
            }
            if rows.is_empty() || offset > total {
                return Err(Error::Protocol);
            }
        }
    }
    Ok(Lists {
        employees: agents.agents,
        inbox,
    })
}

/// Owns the polling task; drop it when the screen or inbox command exits.
pub struct StateFeed {
    pub receiver: watch::Receiver<Model>,
    task: JoinHandle<()>,
}
impl Drop for StateFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl StateFeed {
    pub fn start(client: Client, interval: Duration) -> Self {
        let (tx, receiver) = watch::channel(Model::default());
        let task = tokio::spawn(run(client, tx, interval.max(Duration::from_secs(1))));
        Self { receiver, task }
    }
}
async fn run(client: Client, tx: watch::Sender<Model>, interval: Duration) {
    let mut connection = client.connection();
    let mut events = client.events();
    let mut model = Model::default();
    let mut poll = tokio::time::interval(interval);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut fetching: Option<futures_util::future::BoxFuture<'static, (u64, Result<Lists>)>> = None;
    let mut dirty = true;
    loop {
        let status = connection.borrow_and_update().clone();
        match status {
            ConnectionState::Connected(info) => {
                if model.generation != info.generation {
                    model.connected(&info);
                    dirty = true;
                }
                if dirty && fetching.is_none() {
                    let client = client.clone();
                    let revision = model.revision;
                    fetching = Some(Box::pin(async move {
                        (revision, fetch(&client, info.role).await)
                    }));
                    dirty = false;
                }
            }
            ConnectionState::LoginRequired(error) => {
                model.invalidate_all();
                model.login_required = Some(error);
                drop(fetching);
                tx.send_replace(model);
                return;
            }
            _ => {
                model.invalidate_all();
                dirty = true;
                fetching = None;
            }
        }
        tx.send_replace(model.clone());
        tokio::select! {
            changed = connection.changed() => { if changed.is_err() { return; } }
            event = events.recv() => match event {
                Ok(event) => { let revision = model.revision; model.event(&event); dirty |= model.revision != revision; }
                Err(broadcast::error::RecvError::Lagged(_)) => { model.invalidate_all(); dirty = true; }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = poll.tick() => { dirty = true; }
            completed = async { fetching.as_mut().expect("guarded fetch").await }, if fetching.is_some() => {
                fetching = None;
                let (revision, result) = completed;
                {
                        // A failure waits for the next modest poll, avoiding a hot retry loop.
                        if !model.apply(revision, result) { dirty = true; }
                }
            }
            _ = tx.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fresh() -> Model {
        let mut m = Model::default();
        assert!(m.apply(
            0,
            Ok(Lists {
                employees: vec![],
                inbox: vec![]
            })
        ));
        m
    }
    fn event(name: &str, seq: Option<u64>, boot_id: Option<&str>) -> ServerEvent {
        ServerEvent {
            name: name.into(),
            payload: json!({}),
            seq,
            boot_id: boot_id.map(str::to_owned),
        }
    }
    #[test]
    fn loss_gap_regression_boot_and_reconnect_make_counts_unknown() {
        for name in [
            "events_lost",
            "task.updated",
            "goal.paused",
            "agent.status_changed",
        ] {
            let mut m = fresh();
            m.event(&event(name, None, None));
            assert_eq!(m.inbox.count(), None);
            assert_eq!(m.employees.count(), None);
        }
        for sequence in [vec![1, 3], vec![3, 2]] {
            let mut m = fresh();
            for seq in sequence {
                m.event(&event("future.event", Some(seq), None));
            }
            assert_eq!(m.inbox.count(), None);
        }
        let mut m = fresh();
        m.event(&event("future.event", Some(1), Some("boot-a")));
        m.event(&event("future.event", Some(2), Some("boot-b")));
        assert_eq!(m.inbox.count(), None);
        let mut m = fresh();
        m.connected(&ConnectionInfo {
            generation: 1,
            user_id: "u1".into(),
            role: Role::Admin,
            boot_id: None,
            version: None,
        });
        assert_eq!(m.inbox.count(), None);
    }
    #[test]
    fn unknown_and_duplicate_events_do_not_invalidate() {
        let mut m = fresh();
        for seq in [1, 1, 2] {
            m.event(&event("future.event", Some(seq), None));
        }
        assert_eq!(m.inbox.count(), Some(0));
    }
    #[test]
    fn refetch_races_and_failures_never_produce_false_zero() {
        let mut m = fresh();
        let old_revision = m.revision;
        m.invalidate_all();
        assert!(!m.apply(
            old_revision,
            Ok(Lists {
                employees: vec![],
                inbox: vec![]
            })
        ));
        assert_eq!(m.inbox.count(), None);
        assert!(m.apply(m.revision, Err(Error::RpcRejected)));
        assert_eq!(m.inbox.count(), None);
        assert!(m.apply(
            m.revision,
            Ok(Lists {
                employees: vec![],
                inbox: vec![]
            })
        ));
        assert_eq!(m.inbox.count(), Some(0));
    }
}
