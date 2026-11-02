//! In-process ownership of delegated futures. The lock is a leaf lock: no I/O
//! or await while held. Running entries are never evicted to make room.
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_RUNS: usize = 4096;
pub(crate) const STOP_WAIT: Duration = Duration::from_secs(15);
pub(crate) const STOPPED: &str = "stopped with origin turn";

#[derive(Default)]
struct Completion {
    dropped: AtomicBool,
    pending: AtomicUsize,
    uncertain: AtomicBool,
}
impl Completion {
    fn complete(&self) -> bool {
        self.dropped.load(Ordering::Acquire)
            && self.pending.load(Ordering::Acquire) == 0
            && !self.uncertain.load(Ordering::Acquire)
    }
}
struct Run {
    home: PathBuf,
    session: String,
    turn: String,
    principal: Option<String>,
    token: CancellationToken,
    completion: Arc<Completion>,
}
#[derive(Default)]
struct Registry { entries: Mutex<HashMap<uuid::Uuid, Run>> }
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}
tokio::task_local! { static COMPLETION: Arc<Completion>; }

/// Captured before asynchronous cleanup is detached. Failure or panic remains
/// uncertain; dropping a future alone is not proof that a container is gone.
pub(crate) struct CleanupHold { completion: Arc<Completion>, resolved: bool }
pub(crate) fn cleanup_hold() -> Option<CleanupHold> {
    COMPLETION.try_with(|completion| {
        completion.pending.fetch_add(1, Ordering::AcqRel);
        CleanupHold { completion: completion.clone(), resolved: false }
    }).ok()
}
impl CleanupHold {
    pub(crate) fn finish(mut self, confirmed: bool) {
        self.resolved = confirmed;
    }
}
impl Drop for CleanupHold {
    fn drop(&mut self) {
        if !self.resolved { self.completion.uncertain.store(true, Ordering::Release); }
        self.completion.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct Guard {
    registry: &'static Registry,
    id: uuid::Uuid,
    token: CancellationToken,
    completion: Arc<Completion>,
}
impl Guard {
    /// The inner future is dropped inside the scope, before the caller records
    /// the terminal queue state and drops this guard.
    pub(crate) async fn run<F: Future>(&self, future: F) -> Result<F::Output, ()> {
        let completion = self.completion.clone();
        let observer: duduclaw_core::process_tree::CleanupObserver = Arc::new(move || {
            completion.pending.fetch_add(1, Ordering::AcqRel);
            let hold = CleanupHold { completion: completion.clone(), resolved: false };
            Box::new(move |confirmed| hold.finish(confirmed))
        });
        COMPLETION.scope(self.completion.clone(),
            duduclaw_core::process_tree::with_cleanup_observer(observer, async {
                tokio::select! {
                    biased;
                    _ = self.token.cancelled() => Err(()),
                    result = future => Ok(result),
                }
            }),
        ).await
    }
    pub(crate) fn uncertain(&self) {
        self.completion.uncertain.store(true, Ordering::Release);
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.registry.entries.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.id);
        self.completion.dropped.store(true, Ordering::Release);
    }
}

pub(crate) fn register(
    home: &Path, session: Option<&str>, turn: Option<&str>, principal: Option<&str>,
) -> Result<Option<Guard>, String> {
    #[cfg(test)]
    if crate::model_call_probe::downstream_registration_fails(home) {
        return Err("injected registration failure".into());
    }
    register_in(registry(), home, session, turn, principal)
}
fn register_in(
    registry: &'static Registry, home: &Path, session: Option<&str>, turn: Option<&str>, principal: Option<&str>,
) -> Result<Option<Guard>, String> {
    let (Some(session), Some(turn)) = (session, turn) else { return Ok(None); };
    if session.is_empty() || turn.is_empty() { return Ok(None); }
    let mut entries = registry.entries.lock().unwrap_or_else(|p| p.into_inner());
    if entries.len() >= MAX_RUNS { return Err("downstream run registry full".into()); }
    let id = uuid::Uuid::new_v4();
    let token = CancellationToken::new();
    let completion = Arc::new(Completion::default());
    entries.insert(id, Run {
        home: home.to_owned(), session: session.into(), turn: turn.into(),
        principal: principal.map(str::to_owned), token: token.clone(), completion: completion.clone(),
    });
    Ok(Some(Guard { registry, id, token, completion }))
}

pub(crate) fn principal_matches(raw: Option<&str>, principal: &str) -> bool {
    use crate::on_behalf_of::PrincipalState;
    match PrincipalState::from_wire(raw) {
        PrincipalState::Background => true,
        PrincipalState::Human(p) => PrincipalState::from_wire(Some(principal)) == PrincipalState::Human(p),
        PrincipalState::Unknown => true,
    }
}
fn selected(home: &Path, session: &str, turn: &str, principal: &str) -> Vec<(CancellationToken, Arc<Completion>)> {
    let (selected, mismatches) = {
        let entries = registry().entries.lock().unwrap_or_else(|p| p.into_inner());
        let mut selected = Vec::new();
        let mut mismatches = 0;
        for run in entries.values().filter(|r| r.home == home && r.session == session && r.turn == turn) {
            if principal_matches(run.principal.as_deref(), principal) {
                selected.push((run.token.clone(), run.completion.clone()));
            } else { mismatches += 1; }
        }
        (selected, mismatches)
    };
    if mismatches > 0 {
        audit(home, principal, turn, "principal_mismatch", StopResult::default(), mismatches);
    }
    selected
}
pub(crate) fn count(home: &Path, session: &str, turn: &str, principal: &str) -> usize {
    selected(home, session, turn, principal).len()
}
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StopResult { pub stopped: usize, pub still_stopping: usize }
pub(crate) async fn stop(home: &Path, session: &str, turn: &str, principal: &str, wait: Duration, cause: &str) -> StopResult {
    let runs = selected(home, session, turn, principal);
    for (token, _) in &runs { token.cancel(); }
    let deadline = tokio::time::Instant::now() + wait.min(STOP_WAIT);
    loop {
        let stopped = runs.iter().filter(|(_, c)| c.complete()).count();
        if stopped == runs.len() || tokio::time::Instant::now() >= deadline {
            let result = StopResult { stopped, still_stopping: runs.len() - stopped };
            audit(home, principal, turn, cause, StopResult { stopped, still_stopping: result.still_stopping }, 0);
            return result;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn audit(home: &Path, principal: &str, turn: &str, cause: &str, result: StopResult, excluded: usize) {
    duduclaw_security::audit::append_tool_call_with_extras(home, "system", "downstream_stop", "origin turn downstream stop", true,
        &[("principal", serde_json::json!(principal)), ("turn", serde_json::json!(turn)),
          ("cause", serde_json::json!(cause)), ("count", serde_json::json!(result.stopped + result.still_stopping + excluded)),
          ("excluded", serde_json::json!(excluded)),
          ("stopped", serde_json::json!(result.stopped)), ("still_stopping", serde_json::json!(result.still_stopping))]);
}

#[cfg(test)]
mod tests;
