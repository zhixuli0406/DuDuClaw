//! LINE durable-inbox adapter: acceptance snapshot, workers, delivery.
//!
//! - [`revision`]: the route/authority snapshot of one event.
//! - [`worker`]: the bounded workers.
//! - [`upkeep`]: the snapshot lane and the maintenance task.
//! - [`delivery`]: reply / late-reply Push / progress / document notices.
use super::*;

mod delivery;
mod revision;
mod upkeep;
mod worker;

pub(super) use delivery::{LineNoticeCollector, deliver, line_progress_callback};
#[cfg(test)]
use revision::line_revision;
pub(super) use upkeep::spawn_snapshots;
pub(super) use worker::drain_line_ingress;
#[cfg(test)]
use worker::{IngressLane, drain_line_worker};

/// What a send needs to revalidate: the store, the event, and the snapshot
/// the run was admitted under.
#[derive(Clone)]
pub(crate) struct Binding {
    pub state: LineState,
    pub revision: String,
    pub authorization: String,
    pub payload: serde_json::Value,
    pub row: crate::channel_ingress::IngressRow,
}

// A run's receipt is scoped to one accepted event; it never stores provider
// text or tokens.
tokio::task_local! {
    pub(super) static RUN: std::cell::RefCell<delivery::RunReceipt>;
    static PROGRESS: Arc<delivery::ProgressLedger>;
    pub(super) static INGRESS_BINDING: Binding;
}

/// Record why the run's answer did not go out. The most serious reason wins
/// (a possibly-delivered outcome is never downgraded; review I-LOW-2).
pub(super) fn record_reply_failure(reason: &'static str) {
    let _ = RUN.try_with(|r| {
        let mut r = r.borrow_mut();
        let keep = r
            .outcome
            .is_some_and(|old| delivery::is_uncertain(old) && !delivery::is_uncertain(reason));
        if !keep {
            r.outcome = Some(reason);
        }
    });
}

/// The stop switch (`[channel_ingress] line_enabled`, default on).
pub(crate) async fn durable_line_enabled(home: &Path) -> bool {
    crate::channel_ingress::config::IngressConfig::load(home)
        .await
        .line_enabled
}

pub(super) fn line_conversation(event: &LineEvent) -> String {
    let source = event.source.as_ref();
    source
        .and_then(|s| {
            s.group_id
                .as_deref()
                .or(s.room_id.as_deref())
                .or(s.user_id.as_deref())
        })
        .unwrap_or("unknown")
        .to_string()
}

pub(super) fn claim_worker_home(state: &LineState) -> bool {
    static HOMES: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<
                PathBuf,
                std::sync::Weak<crate::channel_ingress::IngressStore>,
            >,
        >,
    > = std::sync::OnceLock::new();
    let Some(store) = &state.ingress else {
        return false;
    };
    let Ok(mut homes) = HOMES.get_or_init(Default::default).lock() else {
        return false;
    };
    // Review L14a: the same key the store uses, so two spellings of one
    // home do not start two worker sets.
    let key = std::fs::canonicalize(&state.home_dir).unwrap_or_else(|_| state.home_dir.clone());
    if homes.get(&key).and_then(std::sync::Weak::upgrade).is_some() {
        return false;
    }
    homes.insert(key, Arc::downgrade(store));
    true
}

#[cfg(test)]
mod durable_ingress_tests;
#[cfg(test)]
mod f2_tests;
#[cfg(test)]
mod f5_tests;
#[cfg(test)]
mod native_fastlane_tests;

#[cfg(test)]
type DispatchTestHook = (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
#[cfg(test)]
#[derive(Default)]
struct WorkerTestProbe {
    starts: std::sync::atomic::AtomicUsize,
    renewals: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    panic_events: std::sync::Mutex<std::collections::HashSet<String>>,
}
#[cfg(test)]
fn worker_test_probes()
-> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<WorkerTestProbe>>> {
    static PROBES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<WorkerTestProbe>>>,
    > = std::sync::OnceLock::new();
    PROBES.get_or_init(Default::default)
}
#[cfg(test)]
fn worker_test_probe(home: &Path) -> Option<Arc<WorkerTestProbe>> {
    worker_test_probes().lock().unwrap().get(home).cloned()
}
#[cfg(test)]
fn dispatch_test_hooks()
-> &'static std::sync::Mutex<std::collections::HashMap<(PathBuf, String), DispatchTestHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(PathBuf, String), DispatchTestHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(Default::default)
}
#[cfg(test)]
async fn dispatch_test_pause(state: &LineState, row: &crate::channel_ingress::IngressRow) {
    let hook = dispatch_test_hooks()
        .lock()
        .unwrap()
        .get(&(state.home_dir.clone(), row.event_id.clone()))
        .cloned();
    if let Some((entered, release)) = hook {
        entered.notify_one();
        release.notified().await;
    }
    if worker_test_probe(&state.home_dir)
        .is_some_and(|p| p.panic_events.lock().unwrap().remove(&row.event_id))
    {
        panic!("injected dispatch panic");
    }
}

#[cfg(test)]
pub(super) async fn decision_commit_test_pause(state: &LineState, event_id: &str) {
    let hook = dispatch_test_hooks()
        .lock()
        .unwrap()
        .get(&(state.home_dir.clone(), format!("decision-after-{event_id}")))
        .cloned();
    if let Some((entered, release)) = hook {
        entered.notify_one();
        release.notified().await;
    }
}
