//! List snapshots are authoritative; events only invalidate them.
use crate::{
    Error, Result,
    auth::Role,
    client::{Client, ConnectionInfo, ConnectionState, ServerEvent},
    sanitize::TerminalText,
    strings,
};
use serde::{Deserialize, Serialize};
use crate::state_extra::{Activity, ScheduledRun};
use std::collections::HashMap;
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};
use tokio::{
    sync::{broadcast, watch, Notify},
    task::JoinHandle,
    time::Instant,
};

pub const POLL_INTERVAL: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
                    .is_some_and(|k| k.is("question")) =>
            {
                strings::SOURCE_QUESTION
            }
            Self::Approval(a) if a.kind.is("budget_breaker") || a.kind.is("autopilot_breaker") => strings::SOURCE_BREAKER,
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
    pub schedules: Snapshot<ScheduledRun>,
    pub autonomy: Option<TerminalText>,
    pub activity: HashMap<String, Activity>,
    pub recent_activity: HashMap<String, Activity>,
    pub inbox: Snapshot<InboxItem>,
    pub employees: Snapshot<Employee>,
    pub role: Option<Role>,
    pub login_required: Option<Error>,
    pub goal_operators: HashSet<String>,
    pub local_refresh: u64,
    boot_id: Option<String>,
    seq: Option<u64>,
    generation: u64,
    revision: u64,
}
impl Model {
    pub fn invalidate_all(&mut self) {
        self.inbox.invalidate();
        self.employees.invalidate();
        self.schedules.invalidate();
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn connected(&mut self, info: &ConnectionInfo) {
        self.activity.clear();
        self.autonomy = info.autonomy.clone();
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
            self.activity.clear();
            self.seq = None;
            self.invalidate_all();
        }
        let regressed = event.seq.zip(self.seq).is_some_and(|(new, old)| new < old);
        if let Some(seq) = event.seq {
            // A gap or a regression invalidates; exact duplicates are harmless.
            if self
                .seq
                .is_some_and(|old| seq != old && old.checked_add(1) != Some(seq))
            {
                self.activity.clear();
                self.invalidate_all();
            }
            self.seq = Some(seq);
        }
        // Only visible employees can populate the persistent activity map;
        // unknown-agent event floods must not allocate unbounded entries.
        if !regressed && event.payload.get("agent_id").and_then(Value::as_str)
            .is_some_and(|id| self.employees.items.iter().any(|e| e.id() == id)) {
            crate::state_extra::activity_event(&mut self.activity, &event.name, &event.payload);
        }
        if event.name == "autonomy.changed" {
            self.autonomy = event.payload.get("state").or_else(|| event.payload.get("autonomy"))
                .filter(|value| !value.is_null())
                .map(|value| TerminalText::new(value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())));
        }
        match event.name.as_str() {
            "events_lost" => { self.activity.clear(); self.invalidate_all(); },
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
                self.goal_operators = lists.goal_operators;
                self.employees.replace(lists.employees);
                self.activity.retain(|id, _| self.employees.items.iter().any(|e| e.id() == id));
                self.inbox.replace(lists.inbox);
                match lists.recent_activity {
                    Ok(activity) => self.recent_activity = activity,
                    Err(error) => { self.employees.invalidate(); self.employees.error = Some(error); }
                }
                match lists.schedules {
                    Ok(rows) => self.schedules.replace(rows),
                    Err(error) => { self.schedules.invalidate(); self.schedules.error = Some(error); }
                }
            }
            Err(error) => {
                self.inbox.invalidate();
                self.employees.invalidate();
                self.schedules.invalidate();
                self.inbox.error = Some(error);
                self.employees.error = Some(error);
            }
        }
        true
    }
}
struct Lists {
    goal_operators: HashSet<String>,
    recent_activity: Result<HashMap<String, Activity>>,
    schedules: Result<Vec<ScheduledRun>>,
    employees: Vec<Employee>,
    inbox: Vec<InboxItem>,
}
async fn fetch(client: &Client, role: Role) -> Result<Lists> {
    #[derive(Deserialize)]
    struct Agents {
        agents: Vec<Employee>,
    }
    let mut budget = crate::budget::SnapshotBudget::default();
    let response = client.call("agents.list", json!({}), RPC_TIMEOUT).await?;
    budget.charge(&response, "agents")?;
    let agents: Agents = serde_json::from_value(response).map_err(|_| Error::Protocol)?;
    // users.me supplies actual Operator/Owner bindings, not role heuristics.
    // Missing capability data leaves goals read-only, while observation continues.
    let mut goal_operators = HashSet::new();
    if let Ok(me) = client.call("users.me", json!({}), RPC_TIMEOUT).await {
        budget.charge(&me, "bindings")?;
        let admin = me["user"]["role"] == "admin";
        for agent in &agents.agents {
            let operator = me["bindings"].as_array().is_some_and(|bindings| bindings.iter().any(|b|
                b["agent_name"] == agent.id() && matches!(b["access_level"].as_str(), Some("operator" | "owner"))));
            if admin || operator { goal_operators.insert(agent.id().to_owned()); }
        }
    }
    // approvals.list requires Manager, whereas goal_decide accepts an
    // employee's Operator binding. Do not fail the whole employee inbox by
    // making an RPC that its role cannot call.
    let approvals = if role == Role::Employee { json!({"approvals":[]}) }
        else { client.call("approvals.list", json!({}), RPC_TIMEOUT).await? };
    budget.charge(&approvals, "approvals")?;
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
            budget.charge(&response, "tasks")?;
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
            if expected_total.is_some_and(|old| old != total) {
                return Err(Error::Protocol);
            }
            // total is per agent; budget remains shared across every agent.
            budget.ensure_items(total.saturating_sub(offset + rows.len() as u64))?;
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
    let recent_activity = crate::state_extra::recent_activity(client, &agents.agents, &mut budget).await;
    if matches!(recent_activity, Err(Error::SnapshotBudget)) { return Err(Error::SnapshotBudget); }
    let schedules = if role == Role::Admin { crate::state_extra::schedules(client, &mut budget).await } else { Ok(Vec::new()) };
    if matches!(schedules, Err(Error::SnapshotBudget)) { return Err(Error::SnapshotBudget); }
    Ok(Lists {
        goal_operators,
        recent_activity,
        schedules,
        employees: agents.agents,
        inbox,
    })
}

/// Owns the polling task; drop it when the screen or inbox command exits.
pub struct StateFeed {
    pub receiver: watch::Receiver<Model>,
    task: JoinHandle<()>,
    refresh: std::sync::Arc<Notify>,
}
impl Drop for StateFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl StateFeed {
    pub fn refetch(&self) { self.refresh.notify_one(); }
    pub fn start(client: Client, interval: Duration) -> Self {
        let (tx, receiver) = watch::channel(Model::default());
        let refresh = std::sync::Arc::new(Notify::new());
        let task = tokio::spawn(run(client, tx, interval.max(Duration::from_secs(1)), refresh.clone()));
        Self { receiver, task, refresh }
    }
}
async fn run(client: Client, tx: watch::Sender<Model>, interval: Duration, refresh: std::sync::Arc<Notify>) {
    run_with_sources(client.connection(), client.events(), tx, interval, refresh, move |role| {
        let client = client.clone();
        Box::pin(async move { fetch(&client, role).await })
    }).await;
}

async fn run_with_sources(
    mut connection: watch::Receiver<ConnectionState>,
    mut events: broadcast::Receiver<ServerEvent>,
    tx: watch::Sender<Model>,
    interval: Duration,
    refresh: std::sync::Arc<Notify>,
    mut fetch_lists: impl FnMut(Role) -> futures_util::future::BoxFuture<'static, Result<Lists>>,
) {
    let mut model = Model::default();
    // The connection already triggers the initial fetch. An immediate first
    // interval tick would mark that fetch dirty and schedule a duplicate.
    let mut poll = tokio::time::interval_at(Instant::now() + interval, interval);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut fetching: Option<futures_util::future::BoxFuture<'static, (u64, Result<Lists>)>> = None;
    let mut dirty = true;
    let mut requested_refresh = 0;
    loop {
        let status = connection.borrow_and_update().clone();
        match status {
            ConnectionState::Connected(info) => {
                if model.generation != info.generation {
                    model.connected(&info);
                    dirty = true;
                }
                if dirty && fetching.is_none() {
                    // Cover all already queued invalidations with this fetch.
                    // Events received after it starts still request one follow-up.
                    loop {
                        match events.try_recv() {
                            Ok(event) => model.event(&event),
                            Err(broadcast::error::TryRecvError::Lagged(_)) => model.invalidate_all(),
                            Err(broadcast::error::TryRecvError::Empty) => break,
                            Err(broadcast::error::TryRecvError::Closed) => return,
                        }
                    }
                    let revision = model.revision;
                    let fetch = fetch_lists(info.role);
                    fetching = Some(Box::pin(async move {
                        (revision, fetch.await)
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
            _ = refresh.notified() => {
                requested_refresh += 1;
                model.invalidate_all(); dirty = true;
            }
            changed = connection.changed() => { if changed.is_err() { return; } }
            event = events.recv() => match event {
                Ok(event) => { let revision = model.revision; model.event(&event); dirty |= model.revision != revision; }
                Err(broadcast::error::RecvError::Lagged(_)) => { model.invalidate_all(); dirty = true; }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = poll.tick() => { dirty |= fetching.is_none(); }
            completed = async { fetching.as_mut().expect("guarded fetch").await }, if fetching.is_some() => {
                fetching = None;
                let (revision, result) = completed;
                let succeeded = result.is_ok();
                let applied = model.apply(revision, result);
                if succeeded && applied { model.local_refresh = requested_refresh; }
                // A stale successful result needs one follow-up. A failure,
                // including one raced by an event, waits for a new trigger.
                dirty = succeeded && (dirty || !applied);
            }
            _ = tx.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn explicit_decision_refetch_invalidates_and_waits_for_authoritative_lists() {
        use tokio::sync::{mpsc, oneshot};
        let (_conn_tx, connection) = watch::channel(ConnectionState::Connected(ConnectionInfo {
            generation: 1, user_id: "u1".into(), role: Role::Admin,
            boot_id: Some("boot".into()), version: None, autonomy: None,
        }));
        let (_events, event_rx) = broadcast::channel(16);
        let refresh = std::sync::Arc::new(Notify::new());
        let (tx, mut models) = watch::channel(Model::default());
        let (requests, mut calls) = mpsc::unbounded_channel::<oneshot::Sender<Result<Lists>>>();
        let task = tokio::spawn(run_with_sources(connection, event_rx, tx,
            Duration::from_secs(3600), refresh.clone(), move |_| {
                let requests = requests.clone();
                Box::pin(async move { let (reply, rx) = oneshot::channel(); requests.send(reply).unwrap(); rx.await.unwrap() })
            }));
        let complete = || Ok(Lists { goal_operators: HashSet::new(), recent_activity: Ok(HashMap::new()),
            schedules: Ok(vec![]), employees: vec![], inbox: vec![] });
        tokio::time::timeout(Duration::from_secs(2), async {
            calls.recv().await.unwrap().send(complete()).unwrap_or_else(|_| panic!("fetch dropped"));
            loop { models.changed().await.unwrap(); if models.borrow().inbox.count().is_some() { break; } }
            refresh.notify_one();
            let pending = calls.recv().await.unwrap();
            assert!(models.borrow().inbox.count().is_none());
            assert_eq!(models.borrow().local_refresh, 0);
            pending.send(complete()).unwrap_or_else(|_| panic!("fetch dropped"));
            loop { models.changed().await.unwrap(); if models.borrow().local_refresh == 1 { break; } }
            assert_eq!(models.borrow().inbox.count(), Some(0));
        }).await.unwrap();
        task.abort();
    }
    fn fresh() -> Model {
        let mut m = Model::default();
        assert!(m.apply(
            0,
            Ok(Lists {
                goal_operators: HashSet::new(),
                recent_activity: Ok(HashMap::new()),
                schedules: Ok(vec![]),
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
    #[tokio::test]
    async fn refetch_coalesces_events_and_failure_waits_for_a_new_trigger() {
        use tokio::sync::{mpsc, oneshot};
        use tokio::time::timeout;

        let (_connection, connection) = watch::channel(ConnectionState::Connected(ConnectionInfo {
            generation: 1, user_id: "u1".into(), role: Role::Admin,
            boot_id: Some("boot-a".into()), version: None, autonomy: None,
        }));
        let (events, event_rx) = broadcast::channel(32);
        let (tx, mut models) = watch::channel(Model::default());
        let (requests, mut calls) = mpsc::unbounded_channel::<oneshot::Sender<Result<Lists>>>();
        let task = tokio::spawn(run_with_sources(connection, event_rx, tx, Duration::from_secs(60), std::sync::Arc::new(Notify::new()), move |_| {
            let requests = requests.clone();
            Box::pin(async move {
                let (reply, result) = oneshot::channel();
                requests.send(reply).unwrap();
                result.await.unwrap()
            })
        }));
        let complete = || Ok(Lists {
            goal_operators: HashSet::new(),
            recent_activity: Ok(HashMap::new()), schedules: Ok(vec![]),
            employees: vec![], inbox: vec![],
        });
        let first = timeout(Duration::from_secs(2), calls.recv()).await.unwrap().unwrap();
        first.send(complete()).unwrap_or_else(|_| panic!("fetch dropped"));
        timeout(Duration::from_secs(2), async {
            loop {
                models.changed().await.unwrap();
                if models.borrow().inbox.count() == Some(0) { break; }
            }
        }).await.unwrap();
        assert!(timeout(Duration::from_millis(30), calls.recv()).await.is_err(), "startup must not schedule a second fetch");

        // A queued burst is covered by the same snapshot, even with seq gaps.
        for seq in [1, 3, 5] {
            assert!(events.send(event("events_lost", Some(seq), Some("boot-a"))).is_ok());
        }
        let burst = timeout(Duration::from_secs(2), calls.recv()).await.unwrap().unwrap();
        burst.send(complete()).unwrap_or_else(|_| panic!("fetch dropped"));
        assert!(timeout(Duration::from_millis(30), calls.recv()).await.is_err(), "one burst must cause one fetch");

        assert!(events.send(event("events_lost", Some(6), Some("boot-a"))).is_ok());
        let running = timeout(Duration::from_secs(2), calls.recv()).await.unwrap().unwrap();
        for seq in [7, 8, 9] {
            assert!(events.send(event("task.updated", Some(seq), Some("boot-a"))).is_ok());
        }
        // Wait until all invalidations have been processed before completing the fetch.
        timeout(Duration::from_secs(2), async {
            while models.borrow_and_update().seq != Some(9) {
                models.changed().await.unwrap();
            }
        }).await.unwrap();
        running.send(complete()).unwrap_or_else(|_| panic!("fetch dropped"));
        let followup = timeout(Duration::from_secs(2), calls.recv()).await.unwrap().unwrap();
        assert_eq!(models.borrow().inbox.count(), None, "outdated success must stay stale");
        // Even another invalidation during a failed follow-up must not hot retry.
        assert!(events.send(event("events_lost", Some(10), Some("boot-a"))).is_ok());
        timeout(Duration::from_secs(2), async {
            while models.borrow_and_update().seq != Some(10) {
                models.changed().await.unwrap();
            }
        }).await.unwrap();
        followup.send(Err(Error::SnapshotBudget)).unwrap_or_else(|_| panic!("fetch dropped"));
        assert!(timeout(Duration::from_millis(30), calls.recv()).await.is_err(), "failed fetch must wait for the next trigger");
        assert_eq!(models.borrow().inbox.count(), None);
        assert_eq!(models.borrow().employees.count(), None);

        assert!(events.send(event("events_lost", Some(11), Some("boot-a"))).is_ok());
        let recovery = timeout(Duration::from_secs(2), calls.recv()).await.unwrap().unwrap();
        recovery.send(complete()).unwrap_or_else(|_| panic!("fetch dropped"));
        timeout(Duration::from_secs(2), async {
            loop {
                models.changed().await.unwrap();
                if models.borrow().inbox.count() == Some(0) { break; }
            }
        }).await.unwrap();
        assert!(timeout(Duration::from_millis(30), calls.recv()).await.is_err());
        task.abort();
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
            autonomy: None,
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
                goal_operators: HashSet::new(),
                recent_activity: Ok(HashMap::new()),
                schedules: Ok(vec![]),
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
                goal_operators: HashSet::new(),
                recent_activity: Ok(HashMap::new()),
                schedules: Ok(vec![]),
                employees: vec![],
                inbox: vec![]
            })
        ));
        assert_eq!(m.inbox.count(), Some(0));
    }
    #[test]
    fn budget_failure_keeps_previous_complete_snapshot_stale() {
        let mut model = fresh();
        model.employees.items.push(Employee {
            name: "old".into(), display_name: TerminalText::new("Old".into()),
            status: TerminalText::new("active".into()), archived: false,
        });
        model.inbox.items.push(InboxItem::Task(HumanTask {
            id: "previous".into(), title: TerminalText::new("Previous".into()),
            assigned_to: TerminalText::new("old".into()), status: TerminalText::new("needs_human".into()),
            pause_reason: None, goal_mode: true, detail: TerminalText::default(),
        }));
        model.invalidate_all();
        assert!(model.apply(model.revision, Err(Error::SnapshotBudget)));
        assert_eq!(model.employees.items[0].id(), "old");
        assert_eq!(model.inbox.items.len(), 1);
        assert_eq!(model.inbox.count(), None);
        assert_eq!(model.employees.count(), None);
        assert_eq!(model.inbox.error, Some(Error::SnapshotBudget));
    }
    #[test]
    fn unknown_employee_event_flood_cannot_grow_live_activity() {
        let mut model = fresh();
        for id in 0..3_000 {
            let mut e = event("agent.turn_started", None, None);
            e.payload = json!({"agent_id":format!("unknown-{id}")});
            model.event(&e);
        }
        assert!(model.activity.is_empty());
        model.activity.insert("removed".into(), Activity::default());
        assert!(model.apply(model.revision, Ok(Lists {
            goal_operators: HashSet::new(),
            recent_activity: Ok(HashMap::new()), schedules: Ok(vec![]),
            employees: vec![], inbox: vec![],
        })));
        assert!(model.activity.is_empty(), "rotating employee IDs cannot accumulate across snapshots");
    }

    #[test]
    fn unavailable_history_does_not_claim_fresh_employee_state() {
        let mut model = fresh();
        assert!(model.apply(model.revision, Ok(Lists {
            goal_operators: HashSet::new(),
            recent_activity: Err(Error::RpcRejected),
            schedules: Ok(vec![]), employees: vec![], inbox: vec![],
        })));
        assert_eq!(model.inbox.count(), Some(0));
        assert_eq!(model.employees.count(), None);
        assert_eq!(model.employees.error, Some(Error::RpcRejected));
    }

}
