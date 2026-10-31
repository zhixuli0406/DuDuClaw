//! Connection-local event reconciliation and opt-in run step delivery.
//! The run hub uses bounded mpsc queues, never the dashboard broadcast bus.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use serde_json::{Value, json};
use crate::protocol::WsFrame;

pub(crate) fn boot_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

#[derive(Default)]
pub(crate) struct EventSequence(u64);
impl EventSequence {
    /// Call only after authorization and filtering. Overflow closes the socket.
    pub(crate) fn encode(&mut self, mut frame: WsFrame) -> Option<String> {
        if let WsFrame::Event { seq, boot_id: boot, .. } = &mut frame {
            self.0 = self.0.checked_add(1)?;
            *seq = Some(self.0);
            *boot = Some(boot_id().to_string());
        }
        serde_json::to_string(&frame).ok()
    }
    pub(crate) fn filtered(&mut self, line: &str) -> Option<String> {
        let value: Value = serde_json::from_str(line).ok()?;
        // Older producers use raw {type: event_name, ...} objects.
        let frame = if value.get("type").and_then(Value::as_str) == Some("event") {
            serde_json::from_value(value).ok()?
        } else {
            let name = value.get("type")?.as_str()?.to_owned();
            WsFrame::event(&name, value)
        };
        self.encode(frame)
    }
    pub(crate) fn lost(&mut self) -> Option<String> {
        // Never expose the bus-wide dropped count: it includes other audiences.
        self.encode(WsFrame::event("events_lost", json!({})))
    }
}

pub(crate) const RUN_SUBSCRIPTION_CAP: usize = 16;
const RUN_QUEUE_CAP: usize = 64;
const RUN_COMMANDS_PER_SEC: u32 = 20;
pub(crate) const RUN_STEPS_PER_SEC: u32 = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunSelector { Run(String) }
impl RunSelector {
    pub(crate) fn parse(params: &Value) -> Result<Self, &'static str> {
        if !params.as_object().is_some_and(|object| object.len() == 1) {
            return Err("provide only run_id");
        }
        match (params.get("run_id"), params.get("agent_id")) {
            (Some(Value::String(s)), None) if !s.is_empty() && s.len() <= 512 => Ok(Self::Run(s.clone())),
            _ => Err("provide a nonempty run_id; agent-wide subscriptions are not supported"),
        }
    }
    fn matches(&self, step: &RunStep) -> bool {
        match self { Self::Run(id) => id == &step.run_id }
    }
}

#[derive(Default)]
pub(crate) struct RateWindow { start: u64, count: u32 }
impl RateWindow {
    pub(crate) fn take(&mut self, now_ms: u64, cap: u32) -> bool {
        if now_ms.saturating_sub(self.start) >= 1000 { self.start = now_ms; self.count = 0; }
        if self.count >= cap { return false; }
        self.count += 1;
        true
    }
    pub(crate) fn command(&mut self, now_ms: u64) -> bool { self.take(now_ms, RUN_COMMANDS_PER_SEC) }
}

#[derive(Clone, Debug)]
pub(crate) struct RunStep { pub agent_id: String, pub run_id: String, pub tool: String, pub phase: &'static str }
impl RunStep {
    pub(crate) fn frame(&self) -> WsFrame {
        WsFrame::event("run.step", json!({"agent_id": self.agent_id, "run_id": self.run_id,
            "tool": self.tool, "phase": self.phase,
            // Fixed summary: never copy tool arguments, paths or model prose.
            "summary": duduclaw_core::truncate_chars("Tool step recorded", 80)}))
    }
}
type RunAuthorizer = Arc<dyn Fn(RunStep) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync>;
const RUN_AUTH_CAP: usize = 4;
// Across sockets: two full 16-selector connections per user, sixteen globally.
// At most four jobs per blocking-pool permit (8), including final delivery checks.
const RUN_GLOBAL_SUBSCRIPTION_CAP: usize = 256;
const RUN_USER_SUBSCRIPTION_CAP: usize = 32;
const RUN_GLOBAL_AUTH_CAP: usize = 32;
const RUN_CAP_REFUSAL: &str = "run subscription cap reached";

struct RunLimits {
    subscriptions: Arc<tokio::sync::Semaphore>,
    users: Mutex<HashMap<String, Weak<tokio::sync::Semaphore>>>,
    authorization: Arc<tokio::sync::Semaphore>,
}
impl Default for RunLimits {
    fn default() -> Self {
        Self {
            subscriptions: Arc::new(tokio::sync::Semaphore::new(RUN_GLOBAL_SUBSCRIPTION_CAP)),
            users: Mutex::new(HashMap::new()),
            authorization: Arc::new(tokio::sync::Semaphore::new(RUN_GLOBAL_AUTH_CAP)),
        }
    }
}
fn run_limits() -> Arc<RunLimits> {
    static LIMITS: OnceLock<Arc<RunLimits>> = OnceLock::new();
    LIMITS.get_or_init(|| Arc::new(RunLimits::default())).clone()
}
struct ActiveSelector {
    selector: RunSelector,
    pending_loss: bool,
    _global: tokio::sync::OwnedSemaphorePermit,
    _user: tokio::sync::OwnedSemaphorePermit,
}


struct Listener {
    selectors: Mutex<Vec<ActiveSelector>>,
    limits: Arc<RunLimits>,
    user_slots: Arc<tokio::sync::Semaphore>,
    loss_notice: tokio::sync::Notify,
    tx: tokio::sync::mpsc::Sender<RunStep>,
    lost: AtomicBool,
    forwarder_closed: AtomicBool,
    authorize: Option<RunAuthorizer>,
    admission: Arc<tokio::sync::Semaphore>,
}
type Hub = Mutex<HashMap<PathBuf, Vec<Weak<Listener>>>>;
fn hub() -> &'static Hub { static HUB: OnceLock<Hub> = OnceLock::new(); HUB.get_or_init(Default::default) }

/// Dropping this connection handle unsubscribes all its selectors.
#[derive(Clone)]
pub(crate) struct RunSubscriptions(Arc<Listener>);
impl RunSubscriptions {
    #[cfg(test)]
    pub(crate) fn new(home: &Path) -> (Self, tokio::sync::mpsc::Receiver<RunStep>) {
        Self::register(home, "test", None, Arc::new(RunLimits::default()))
    }
    pub(crate) fn new_authorized(home: &Path, user_id: &str, authorize: RunAuthorizer) -> (Self, tokio::sync::mpsc::Receiver<RunStep>) {
        Self::register(home, user_id, Some(authorize), run_limits())
    }
    fn register(home: &Path, user_id: &str, authorize: Option<RunAuthorizer>, limits: Arc<RunLimits>) -> (Self, tokio::sync::mpsc::Receiver<RunStep>) {
        let user_slots = {
            let mut users = limits.users.lock().expect("run user limits poisoned");
            users.retain(|_, slots| slots.strong_count() > 0);
            match users.get(user_id).and_then(Weak::upgrade) {
                Some(slots) => slots,
                None => {
                    let slots = Arc::new(tokio::sync::Semaphore::new(RUN_USER_SUBSCRIPTION_CAP));
                    users.insert(user_id.to_owned(), Arc::downgrade(&slots));
                    slots
                }
            }
        };
        let (tx, rx) = tokio::sync::mpsc::channel(RUN_QUEUE_CAP);
        let listener = Arc::new(Listener { selectors: Mutex::new(vec![]), tx, lost: AtomicBool::new(false),
            forwarder_closed: AtomicBool::new(false),
            limits, user_slots, loss_notice: tokio::sync::Notify::new(), authorize, admission: Arc::new(tokio::sync::Semaphore::new(RUN_AUTH_CAP)) });
        if let Ok(mut h) = hub().lock() {
            let list = h.entry(home.to_path_buf()).or_default();
            list.retain(|l| l.strong_count() > 0);
            list.push(Arc::downgrade(&listener));
        }
        (Self(listener), rx)
    }
    pub(crate) fn update(&self, selector: RunSelector, subscribe: bool) -> Result<(), &'static str> {
        let mut list = self.0.selectors.lock().map_err(|_| "subscriptions unavailable")?;
        if self.0.forwarder_closed.load(Ordering::Relaxed) { return Err("run subscriptions unavailable on this connection"); }
        if !subscribe { list.retain(|s| s.selector != selector); return Ok(()); }
        if list.iter().any(|s| s.selector == selector) { return Ok(()); }
        if list.len() >= RUN_SUBSCRIPTION_CAP { return Err("run subscription cap reached (16)"); }
        let global = self.0.limits.subscriptions.clone().try_acquire_owned().map_err(|_| RUN_CAP_REFUSAL)?;
        let user = self.0.user_slots.clone().try_acquire_owned().map_err(|_| RUN_CAP_REFUSAL)?;
        list.push(ActiveSelector { selector, pending_loss: false, _global: global, _user: user }); Ok(())
    }
    pub(crate) fn matches(&self, step: &RunStep) -> bool {
        self.0.selectors.lock().is_ok_and(|s| s.iter().any(|s| s.selector.matches(step)))
    }
    pub(crate) fn clear(&self) {
        if let Ok(mut selectors) = self.0.selectors.lock() { selectors.clear(); }
    }
    pub(crate) async fn loss_notified(&self) { self.0.loss_notice.notified().await; }
    pub(crate) fn take_lost(&self) -> bool { self.0.lost.swap(false, Ordering::Relaxed) }
    #[cfg(test)]
    pub(crate) fn mark_authorized_loss_for_test(&self) {
        self.0.lost.store(true, Ordering::Relaxed);
        self.0.loss_notice.notify_one();
    }
    #[cfg(test)]
    pub(crate) async fn wait_for_authorization(&self) {
        let _all = self.0.admission.acquire_many(RUN_AUTH_CAP as u32).await.unwrap();
    }
}

/// Created before spawning so even cancellation before the first poll cleans up.
struct ForwarderGuard(RunSubscriptions);
impl Drop for ForwarderGuard {
    fn drop(&mut self) {
        // Serialize closure with update: no subscription may take a permit
        // after cleanup, even when an RPC response races with task failure.
        let mut selectors = self.0.0.selectors.lock().unwrap_or_else(|error| error.into_inner());
        self.0.0.forwarder_closed.store(true, Ordering::Relaxed);
        selectors.clear();
        self.0.0.lost.store(false, Ordering::Relaxed);
    }
}

pub(crate) fn spawn_forwarder(
    subscriptions: RunSubscriptions,
    inbound: tokio::sync::mpsc::Receiver<RunStep>,
    outbound: tokio::sync::mpsc::Sender<RunStep>,
    authorize: RunAuthorizer,
) -> tokio::task::JoinHandle<()> {
    let guard = ForwarderGuard(subscriptions);
    tokio::spawn(async move {
        forward_authorized(&guard.0, inbound, outbound, authorize).await;
    })
}

/// Re-check a queued step immediately before it can be sent. A failed check
/// is dropped here and does not set `events_lost`. The outbound channel is
/// one slot, and that slot is reserved before the check, so at most one
/// already-authorized step can wait for a slow socket.
///
/// Returns when the connection drops its receiver. `inbound` never closes on
/// its own (the listener owns its sender), so `outbound.closed()` is what ends
/// this task and releases the listener.
pub(crate) async fn forward_authorized(
    subscriptions: &RunSubscriptions,
    mut inbound: tokio::sync::mpsc::Receiver<RunStep>,
    outbound: tokio::sync::mpsc::Sender<RunStep>,
    authorize: RunAuthorizer,
) {
    loop {
        let step = tokio::select! {
            step = inbound.recv() => match step { Some(step) => step, None => break },
            _ = outbound.closed() => break,
        };
        if !subscriptions.matches(&step) {
            continue;
        }
        let Ok(permit) = outbound.reserve().await else { break };
        // A closed connection must not wait for authorization queued behind
        // the shared blocking pool; a job already running finishes on its own.
        let Ok(_admission) = subscriptions.0.limits.authorization.clone().try_acquire_owned() else {
            set_pending_loss(&subscriptions.0, &step);
            continue;
        };
        let allowed = tokio::select! {
            allowed = authorize(step.clone()) => allowed,
            _ = outbound.closed() => break,
        };
        if !allowed {
            continue;
        }
        if !subscriptions.matches(&step) {
            continue;
        }
        flush_pending_loss(&subscriptions.0, &step);
        permit.send(step);
    }
    if outbound.is_closed() { subscriptions.clear(); }
}

/// True when some live connection's selector is exactly this run. A poisoned
/// hub lock fails open so the caller still does its own lookup.
pub(crate) fn has_run_subscriber(home: &Path, run_id: &str) -> bool {
    let Ok(mut listeners) = hub().lock() else { return true; };
    let Some(list) = listeners.get_mut(home) else { return false; };
    list.retain(|weak| weak.strong_count() > 0);
    let probe = RunStep {
        agent_id: String::new(),
        run_id: run_id.to_owned(),
        tool: String::new(),
        phase: "started",
    };
    for weak in list.iter() {
        let Some(listener) = weak.upgrade() else { continue; };
        match listener.selectors.lock() {
            Ok(selectors) if selectors.iter().any(|selector| selector.selector.matches(&probe)) => return true,
            Ok(_) => {}
            Err(_) => return true,
        }
    }
    false
}

pub(crate) fn publish_step(home: &Path, agent_id: &str, run_id: &str, tool: &str, phase: &'static str) {
    // Tool names are identifiers, not arbitrary labels; omit an unsafe label.
    let safe_tool = if tool.len() <= 120 && tool.chars().all(|c| c.is_ascii_alphanumeric() || "_-.:/".contains(c)) {
        tool
    } else { "tool" };
    let step = RunStep { agent_id: agent_id.into(), run_id: run_id.into(),
        tool: duduclaw_core::truncate_chars(safe_tool, 80), phase };
    // Copy listeners before awaiting authorization; never hold the hub lock
    // while reading identity/task stores. Candidate work is strictly bounded.
    let listeners = {
        let Ok(mut h) = hub().lock() else { return; };
        let Some(listeners) = h.get_mut(home) else { return; };
        listeners.retain(|weak| weak.strong_count() > 0);
        listeners.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
    };
    for listener in listeners {
        if !listener.selectors.lock().is_ok_and(|s| s.iter().any(|s| s.selector.matches(&step))) { continue; }
        if listener.authorize.is_none() {
            // Only the unit-test constructor can omit authorization.
            enqueue(&listener, step.clone());
            continue;
        }
        let Ok(permit) = listener.admission.clone().try_acquire_owned() else {
            // No candidate queue and no loss signal before authorization:
            // even a loss signal would reveal unauthorized activity.
            continue;
        };
        let Ok(global_permit) = listener.limits.authorization.clone().try_acquire_owned() else {
            set_pending_loss(&listener, &step);
            continue;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { continue; };
        let step = step.clone();
        runtime.spawn(async move {
            authorize_and_enqueue(&listener, step).await;
            drop((permit, global_permit));
        });
    }
}

fn set_pending_loss(listener: &Listener, step: &RunStep) {
    if let Ok(mut selectors) = listener.selectors.lock() {
        if let Some(active) = selectors.iter_mut().find(|active| active.selector.matches(step)) {
            active.pending_loss = true;
        }
    }
}

fn flush_pending_loss(listener: &Listener, step: &RunStep) {
    let pending = listener.selectors.lock().ok().and_then(|mut selectors| {
        selectors.iter_mut().find(|active| active.selector.matches(step))
            .map(|active| std::mem::replace(&mut active.pending_loss, false))
    }).unwrap_or(false);
    if pending { mark_lost(listener); }
}

fn mark_lost(listener: &Listener) {
    listener.lost.store(true, Ordering::Relaxed);
    listener.loss_notice.notify_one();
}

fn enqueue(listener: &Listener, step: RunStep) {
    if !listener.selectors.lock().is_ok_and(|s| s.iter().any(|s| s.selector.matches(&step))) { return; }
    if matches!(listener.tx.try_send(step), Err(tokio::sync::mpsc::error::TrySendError::Full(_))) {
        mark_lost(listener);
    }
}

async fn authorize_and_enqueue(listener: &Listener, step: RunStep) {
    let Some(authorize) = &listener.authorize else { return; };
    if authorize(step.clone()).await {
        // A pending admission loss is visible only after this fresh check.
        if listener.selectors.lock().is_ok_and(|s| s.iter().any(|s| s.selector.matches(&step))) {
            flush_pending_loss(listener, &step);
            enqueue(listener, step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limited(home: &Path, user: &str, limits: &Arc<RunLimits>) -> RunSubscriptions {
        RunSubscriptions::register(home, user, None, limits.clone()).0
    }

    #[test]
    fn per_user_cap_spans_connections_and_unsubscribe_releases_slots() {
        let home = tempfile::tempdir().unwrap();
        let limits = Arc::new(RunLimits::default());
        let a = limited(home.path(), "alice", &limits);
        let b = limited(home.path(), "alice", &limits);
        let c = limited(home.path(), "alice", &limits);
        for n in 0..RUN_SUBSCRIPTION_CAP {
            a.update(RunSelector::Run(format!("a{n}")), true).unwrap();
            b.update(RunSelector::Run(format!("b{n}")), true).unwrap();
        }
        a.update(RunSelector::Run("a0".into()), true).unwrap();
        assert_eq!(c.update(RunSelector::Run("extra".into()), true), Err(RUN_CAP_REFUSAL));
        a.update(RunSelector::Run("a0".into()), false).unwrap();
        c.update(RunSelector::Run("extra".into()), true).unwrap();
        a.clear(); b.clear(); c.clear();
        assert_eq!(limits.subscriptions.available_permits(), RUN_GLOBAL_SUBSCRIPTION_CAP);
        assert_eq!(c.0.user_slots.available_permits(), RUN_USER_SUBSCRIPTION_CAP);
    }

    #[test]
    fn global_cap_and_listener_drop_release_slots() {
        let home = tempfile::tempdir().unwrap();
        let limits = Arc::new(RunLimits::default());
        let mut listeners = Vec::new();
        for n in 0..RUN_GLOBAL_SUBSCRIPTION_CAP / RUN_SUBSCRIPTION_CAP {
            let listener = limited(home.path(), &format!("user{n}"), &limits);
            for run in 0..RUN_SUBSCRIPTION_CAP {
                listener.update(RunSelector::Run(format!("run{run}")), true).unwrap();
            }
            listeners.push(listener);
        }
        let extra = limited(home.path(), "extra", &limits);
        assert_eq!(extra.update(RunSelector::Run("extra".into()), true), Err(RUN_CAP_REFUSAL));
        drop(listeners.pop());
        extra.update(RunSelector::Run("extra".into()), true).unwrap();
        drop(listeners); drop(extra);
        assert_eq!(limits.subscriptions.available_permits(), RUN_GLOBAL_SUBSCRIPTION_CAP);
    }

    #[tokio::test]
    async fn saturated_global_admission_defers_one_loss_until_authorized() {
        let home = tempfile::tempdir().unwrap();
        let limits = Arc::new(RunLimits::default());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let (s, mut rx) = RunSubscriptions::register(home.path(), "alice", Some(Arc::new(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { true })
        })), limits.clone());
        s.update(RunSelector::Run("run".into()), true).unwrap();
        let permits = limits.authorization.clone().acquire_many_owned(RUN_GLOBAL_AUTH_CAP as u32).await.unwrap();
        for _ in 0..1000 { publish_step(home.path(), "a", "run", "Read", "started"); }
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(s.0.admission.available_permits(), RUN_AUTH_CAP);
        assert!(rx.try_recv().is_err());
        assert!(!s.take_lost());
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), s.loss_notified()).await.is_err());
        drop(permits);
        publish_step(home.path(), "a", "run", "Read", "started");
        s.wait_for_authorization().await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(rx.try_recv().is_ok());
        tokio::time::timeout(std::time::Duration::from_secs(1), s.loss_notified()).await.unwrap();
        assert!(s.take_lost());
        assert!(!s.take_lost());
        publish_step(home.path(), "a", "run", "Read", "started");
        s.wait_for_authorization().await;
        assert!(rx.try_recv().is_ok());
        assert!(!s.take_lost());
    }

    #[tokio::test]
    async fn revoked_listener_stays_silent_after_saturated_admission() {
        let home = tempfile::tempdir().unwrap();
        let limits = Arc::new(RunLimits::default());
        let allowed = Arc::new(AtomicBool::new(true));
        let reader = allowed.clone();
        let (s, mut rx) = RunSubscriptions::register(home.path(), "alice", Some(Arc::new(move |_| {
            let allowed = reader.load(Ordering::Relaxed);
            Box::pin(async move { allowed })
        })), limits.clone());
        s.update(RunSelector::Run("run".into()), true).unwrap();
        let permits = limits.authorization.clone().acquire_many_owned(RUN_GLOBAL_AUTH_CAP as u32).await.unwrap();
        for _ in 0..1000 { publish_step(home.path(), "a", "run", "Read", "started"); }
        assert!(s.0.selectors.lock().unwrap()[0].pending_loss);
        assert!(!s.take_lost());
        allowed.store(false, Ordering::Relaxed);
        drop(permits);
        for _ in 0..3 {
            publish_step(home.path(), "a", "run", "Read", "started");
            s.wait_for_authorization().await;
        }
        assert!(rx.try_recv().is_err());
        assert!(!s.take_lost());
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), s.loss_notified()).await.is_err());
    }

    #[tokio::test]
    async fn another_run_does_not_flush_revoked_run_loss() {
        let home = tempfile::tempdir().unwrap();
        let limits = Arc::new(RunLimits::default());
        let allowed_a = Arc::new(AtomicBool::new(true));
        let reader_a = allowed_a.clone();
        let (s, mut rx) = RunSubscriptions::register(home.path(), "alice", Some(Arc::new(move |step| {
            let allowed = step.run_id == "run-b" || reader_a.load(Ordering::Relaxed);
            Box::pin(async move { allowed })
        })), limits.clone());
        s.update(RunSelector::Run("run-a".into()), true).unwrap();
        s.update(RunSelector::Run("run-b".into()), true).unwrap();

        let permits = limits.authorization.clone().acquire_many_owned(RUN_GLOBAL_AUTH_CAP as u32).await.unwrap();
        for _ in 0..1000 { publish_step(home.path(), "a", "run-a", "Read", "started"); }
        assert!(s.0.selectors.lock().unwrap().iter().find(|active| active.selector == RunSelector::Run("run-a".into())).unwrap().pending_loss);
        allowed_a.store(false, Ordering::Relaxed);
        drop(permits);

        publish_step(home.path(), "a", "run-b", "Read", "started");
        s.wait_for_authorization().await;
        assert_eq!(rx.try_recv().unwrap().run_id, "run-b");
        assert!(!s.take_lost());
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), s.loss_notified()).await.is_err());

        for _ in 0..3 { publish_step(home.path(), "a", "run-b", "Read", "started"); }
        s.wait_for_authorization().await;
        assert_eq!(rx.try_recv().unwrap().run_id, "run-b");
        assert!(!s.take_lost());
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), s.loss_notified()).await.is_err());
    }

    #[tokio::test]
    async fn forwarder_saturation_defers_loss_and_rechecks_revocation() {
        for revoked in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let (s, inbound) = RunSubscriptions::new(home.path());
            s.update(RunSelector::Run("run".into()), true).unwrap();
            let allowed = Arc::new(AtomicBool::new(true));
            let reader = allowed.clone();
            let checked = Arc::new(tokio::sync::Notify::new());
            let check = checked.clone();
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let task = spawn_forwarder(s.clone(), inbound, tx, Arc::new(move |_| {
                let allowed = reader.load(Ordering::Relaxed);
                let check = check.clone();
                Box::pin(async move { check.notify_one(); allowed })
            }));
            let permits = s.0.limits.authorization.clone().acquire_many_owned(RUN_GLOBAL_AUTH_CAP as u32).await.unwrap();
            publish_step(home.path(), "a", "run", "Read", "started");
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !s.0.selectors.lock().unwrap()[0].pending_loss { tokio::task::yield_now().await; }
            }).await.unwrap();
            assert!(!s.take_lost());
            assert!(rx.try_recv().is_err());
            allowed.store(!revoked, Ordering::Relaxed);
            drop(permits);
            publish_step(home.path(), "a", "run", "Read", "started");
            tokio::time::timeout(std::time::Duration::from_secs(1), checked.notified()).await.unwrap();
            if revoked {
                assert!(tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv()).await.is_err());
                assert!(!s.take_lost());
                assert!(tokio::time::timeout(std::time::Duration::from_millis(20), s.loss_notified()).await.is_err());
            } else {
                tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
                assert!(s.take_lost());
                assert!(!s.take_lost());
            }
            drop(rx);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn panicking_forwarder_releases_caps_with_connection_still_open() {
        let home = tempfile::tempdir().unwrap();
        let (s, inbound) = RunSubscriptions::new(home.path());
        let global_before = s.0.limits.subscriptions.available_permits();
        let user_before = s.0.user_slots.available_permits();
        s.update(RunSelector::Run("run".into()), true).unwrap();
        assert_eq!(s.0.limits.subscriptions.available_permits(), global_before - 1);
        assert_eq!(s.0.user_slots.available_permits(), user_before - 1);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let task = spawn_forwarder(s.clone(), inbound, tx, Arc::new(|_| {
            Box::pin(async { panic!("injected authorizer panic") })
        }));
        publish_step(home.path(), "a", "run", "Read", "started");
        assert!(tokio::time::timeout(std::time::Duration::from_secs(1), task).await.unwrap().unwrap_err().is_panic());
        // Keep both connection-owned handles alive while checking cleanup.
        assert!(rx.recv().await.is_none());
        assert!(!has_run_subscriber(home.path(), "run"));
        assert_eq!(s.0.limits.subscriptions.available_permits(), global_before);
        assert_eq!(s.0.user_slots.available_permits(), user_before);
        assert_eq!(s.update(RunSelector::Run("run".into()), true), Err("run subscriptions unavailable on this connection"));
        assert_eq!(s.0.limits.subscriptions.available_permits(), global_before);
    }

    #[test]
    fn has_run_subscriber_tracks_exact_run_id() {
        let home = tempfile::tempdir().unwrap();
        assert!(!has_run_subscriber(home.path(), "s#1"));
        let (s, _rx) = RunSubscriptions::new(home.path());
        s.update(RunSelector::Run("s#1".into()), true).unwrap();
        assert!(has_run_subscriber(home.path(), "s#1"));
        assert!(!has_run_subscriber(home.path(), "s#2"));
        s.update(RunSelector::Run("s#1".into()), false).unwrap();
        assert!(!has_run_subscriber(home.path(), "s#1"));
    }
    #[tokio::test]
    async fn forwarder_exits_and_releases_listener_when_connection_closes() {
        let home = tempfile::tempdir().unwrap();
        let (s, inbound) = RunSubscriptions::new(home.path());
        s.update(RunSelector::Run("s#1".into()), true).unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let forwarder = s.clone();
        let task = tokio::spawn(async move {
            forward_authorized(&forwarder, inbound, tx, Arc::new(move |_step| {
                Box::pin(async move { true })
            }))
            .await;
        });
        // The connection loop ends and drops its receiver; the listener still
        // owns the inbound sender, so only `outbound.closed()` can end the task.
        let retained = s.clone();
        drop(rx);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("forwarder must exit when the connection closes")
            .unwrap();
        assert_eq!(retained.0.limits.subscriptions.available_permits(), RUN_GLOBAL_SUBSCRIPTION_CAP);
        assert_eq!(retained.0.user_slots.available_permits(), RUN_USER_SUBSCRIPTION_CAP);
        drop(s);
        assert!(!has_run_subscriber(home.path(), "s#1"));
    }
    #[test]
    fn sequence_only_counts_delivered_frames_and_loss() {
        let mut a = EventSequence::default(); let mut b = EventSequence::default();
        assert!(a.filtered("invalid").is_none());
        for expected in 1..=3 { let v: Value = serde_json::from_str(&a.lost().unwrap()).unwrap(); assert_eq!(v["seq"], expected); assert_eq!(v["boot_id"], boot_id()); }
        let v: Value = serde_json::from_str(&b.lost().unwrap()).unwrap(); assert_eq!(v["seq"], 1);
    }
    #[test]
    fn subscription_cap_idempotence_and_rate_limit() {
        let home = tempfile::tempdir().unwrap(); let (s, _rx) = RunSubscriptions::new(home.path());
        for n in 0..RUN_SUBSCRIPTION_CAP { s.update(RunSelector::Run(format!("a{n}")), true).unwrap(); }
        s.update(RunSelector::Run("a0".into()), true).unwrap();
        assert!(s.update(RunSelector::Run("extra".into()), true).is_err());
        s.update(RunSelector::Run("a0".into()), false).unwrap();
        s.update(RunSelector::Run("extra".into()), true).unwrap();
        let mut rate = RateWindow::default(); for _ in 0..20 { assert!(rate.command(0)); }
        assert!(!rate.command(999)); assert!(rate.command(1000));
    }
    #[test]
    fn targeted_delivery_redaction_and_queue_loss() {
        let home = tempfile::tempdir().unwrap(); let (s, mut rx) = RunSubscriptions::new(home.path());
        publish_step(home.path(), "a", "dispatch:1", "Read", "start"); assert!(rx.try_recv().is_err());
        s.update(RunSelector::Run("dispatch:1".into()), true).unwrap();
        publish_step(home.path(), "a", "dispatch:1", "Read secret=abc", "start");
        let encoded = serde_json::to_string(&rx.try_recv().unwrap().frame()).unwrap(); assert!(!encoded.contains("abc"));
        for _ in 0..=RUN_QUEUE_CAP { publish_step(home.path(), "a", "dispatch:1", "Read", "start"); }
        assert!(s.take_lost()); assert!(!s.take_lost());
        s.update(RunSelector::Run("dispatch:1".into()), false).unwrap();
        assert!(!s.matches(&rx.try_recv().unwrap()));
    }
    #[tokio::test]
    async fn authorization_work_is_bounded_without_pre_authorization_loss() {
        let home = tempfile::tempdir().unwrap();
        let pending = Arc::new(tokio::sync::Notify::new());
        let gate = pending.clone();
        let (s, mut rx) = RunSubscriptions::new_authorized(home.path(), "test", Arc::new(move |_| {
            let gate = gate.clone();
            Box::pin(async move { gate.notified().await; false })
        }));
        s.update(RunSelector::Run("dispatch:1".into()), true).unwrap();
        for _ in 0..1000 { publish_step(home.path(), "a", "dispatch:1", "Read", "started"); }
        assert_eq!(s.0.admission.available_permits(), 0);
        assert!(rx.try_recv().is_err());
        assert!(!s.take_lost());
        // Give the admitted futures a chance to install their notify waiters.
        tokio::task::yield_now().await;
        pending.notify_waiters();
        s.wait_for_authorization().await;
        assert_eq!(s.0.admission.available_permits(), RUN_AUTH_CAP);
        assert!(rx.try_recv().is_err());
        assert!(!s.take_lost());
    }

    #[tokio::test]
    async fn broadcast_lag_produces_reconciliation_frame() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(1);
        tx.send(1).unwrap(); tx.send(2).unwrap();
        assert!(matches!(rx.recv().await, Err(tokio::sync::broadcast::error::RecvError::Lagged(_))));
        let mut seq = EventSequence::default();
        let frame: Value = serde_json::from_str(&seq.lost().unwrap()).unwrap();
        assert_eq!(frame["event"], "events_lost"); assert_eq!(frame["seq"], 1);
    }
}
