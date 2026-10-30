//! Host-owned delegation contexts. References carry no authority outside
//! their home, employee and live runtime scope; `None` is explicit too.
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const ENV_CONTEXT: &str = "DUDUCLAW_PRINCIPAL_CONTEXT";
const MAX_CONTEXTS: usize = 4096;
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

struct Entry {
    home: PathBuf,
    agent: String,
    principal: super::PrincipalState,
    created: Instant,
    attendance: Option<crate::computer_use_sessions::attended_call::AttendedCall>,
    task: Option<String>,
    run: Option<String>,
}
fn entries() -> &'static Mutex<HashMap<String, Entry>> {
    static ENTRIES: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(Default::default)
}

tokio::task_local! { static CONTEXT: Option<String>; }

/// Dropping the runtime's guard invalidates its reference immediately.
pub struct ContextGuard(String);
impl Drop for ContextGuard {
    fn drop(&mut self) {
        let removed=entries()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
        if let Some(call)=removed.and_then(|entry|entry.attendance) {call.invalidate();}
    }
}
impl ContextGuard {
    pub fn reference(&self) -> &str {
        &self.0
    }
}

/// Mint only from a host scope, never from a wire-supplied principal.
pub(crate) fn register(home: &Path, agent: &str) -> Option<ContextGuard> {
    register_value(home, agent, super::current())
}
fn register_value(home: &Path, agent: &str, principal: Option<String>) -> Option<ContextGuard> {
    if !duduclaw_core::is_valid_agent_id(agent) {
        return None;
    }
    let principal = super::PrincipalState::from_wire(principal.as_deref());
    let home = home.canonicalize().ok()?;
    let mut entries = entries().lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    entries.retain(|_, value| {
        let keep = now.saturating_duration_since(value.created) < MAX_AGE;
        if !keep && let Some(call) = &value.attendance {
            call.invalidate();
        }
        keep
    });
    if entries.len() >= MAX_CONTEXTS {
        return None;
    }
    let random: [u8; 32] = rand::random();
    let reference = hex::encode(random);
    // Never replace an active context, even in the improbable event of collision.
    if entries.contains_key(&reference) {
        return None;
    }
    entries.insert(
        reference.clone(),
        Entry {
            home,
            agent: agent.into(),
            principal,
            created: now,
            attendance: crate::computer_use_sessions::attended_call::capture(agent),
            task: crate::runtime::round_task_env().map(|(_, task)| task),
            run: crate::memory_provenance::DISPATCH_RUN.try_with(Clone::clone).ok().flatten()
                .map(|(_, run)| run),
        },
    );
    Some(ContextGuard(reference))
}

/// Per-runtime proof, never inferred from the principal or upstream turn id.
pub(crate) fn attended_call(home: &Path, agent: &str, reference: &str)
    -> Option<crate::computer_use_sessions::attended_call::AttendedCall>
{
    lookup(home,agent,reference)?;
    entries().lock().unwrap_or_else(|p| p.into_inner()).get(reference)?
        .attendance.clone().filter(|call| call.valid_for(agent))
}

/// The task comes only from the gateway round scope at runtime registration.
/// A model's start `task` label and process environment cannot supply it.
pub(crate) fn host_task(home: &Path, agent: &str, reference: &str) -> Option<String> {
    lookup(home, agent, reference)?;
    entries().lock().unwrap_or_else(|p| p.into_inner()).get(reference)?.task.clone()
}
pub(crate) fn host_run(home: &Path, agent: &str, reference: &str) -> Option<String> {
    lookup(home, agent, reference)?;
    entries().lock().unwrap_or_else(|p| p.into_inner()).get(reference)?.run.clone()
}

/// `Some(None)` is a verified background context; `None` means unproven.
pub(crate) fn lookup(home: &Path, agent: &str, reference: &str) -> Option<Option<String>> {
    if reference.len() != 64
        || !reference
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let home = home.canonicalize().ok()?;
    let entries = entries().lock().unwrap_or_else(|p| p.into_inner());
    let e = entries.get(reference)?;
    (e.home == home && e.agent == agent && e.created.elapsed() < MAX_AGE)
        .then(|| e.principal.to_wire())
}

/// Every host runtime call gets its own lifetime, including background calls.
pub(crate) fn scope<F: Future>(
    home: &Path,
    agent: &str,
    fut: F,
) -> impl Future<Output = F::Output> {
    // Box before constructing another async state machine. Capturing a large
    // runtime future by value in every scope inflated dispatcher poll frames.
    let fut = Box::pin(fut);
    async move {
        // Resolve the principal when polled under the caller's scope, not
        // when this future is constructed outside that scope.
        let guard = register(home, agent);
        CONTEXT
            .scope(guard.as_ref().map(|g| g.0.clone()), fut)
            .await
    }
}

pub(crate) fn env_pair() -> Option<(String, String)> {
    CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map(|r| (ENV_CONTEXT.into(), r))
}

/// Direct Claude spawn guards own their contexts independently of task locals.
pub(crate) fn inject(cmd: &mut tokio::process::Command, guard: Option<&ContextGuard>) {
    cmd.env_remove(ENV_CONTEXT);
    if let Some(g) = guard {
        cmd.env(ENV_CONTEXT, g.reference());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn browser_background_linkage_comes_only_from_the_registered_host_scope() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let guard = register(home.path(), "alice").unwrap();
        assert!(host_task(home.path(), "alice", guard.reference()).is_none());
        assert!(host_run(home.path(), "alice", guard.reference()).is_none());
        let attr = crate::runtime::GoalRoundAttribution { episode_id:"verified-task".into(), round:Some(2) };
        let run = "0123456789abcdef0123456789abcdef";
        let guard = crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(attr,
            crate::memory_provenance::DISPATCH_RUN.scope(Some(("dispatch:alice".into(),run.into())),
                async { register(home.path(), "alice").unwrap() })).await;
        let reference = guard.reference().to_owned();
        assert_eq!(host_task(home.path(), "alice", &reference).as_deref(),Some("verified-task"));
        assert_eq!(host_run(home.path(), "alice", &reference).as_deref(),Some(run));
        assert!(host_task(other.path(), "alice", &reference).is_none());
        assert!(host_run(home.path(), "bob", &reference).is_none());
        drop(guard);
        assert!(host_task(home.path(), "alice", &reference).is_none());
        assert!(host_run(home.path(), "alice", &reference).is_none());
    }
    #[tokio::test]
    async fn host_context_is_bound_to_home_agent_and_lifetime_including_none() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let guard = register(home.path(), "bot").unwrap();
        let r = guard.reference().to_owned();
        assert_eq!(lookup(home.path(), "bot", &r), Some(None));
        assert_eq!(lookup(other.path(), "bot", &r), None);
        assert_eq!(lookup(home.path(), "other", &r), None);
        drop(guard);
        assert_eq!(lookup(home.path(), "bot", &r), None);
        let p = super::super::scope_first(Some("user:alice".into()), async {
            scope(home.path(), "bot", async {
                let (_, r) = env_pair().unwrap();
                assert_eq!(
                    lookup(home.path(), "bot", &r),
                    Some(Some("user:alice".into()))
                );
                r
            })
            .await
        })
        .await;
        assert_eq!(lookup(home.path(), "bot", &p), None);
        assert!(env_pair().is_none());
    }

    #[tokio::test]
    async fn rejected_stored_provenance_stays_unknown_in_host_context_and_nested_scopes() {
        let home = tempfile::tempdir().unwrap();
        for raw in ["not-a-principal", "user:dashboard", "", " "] {
            let carried = super::super::from_stored(home.path(), Some(raw), "bot", "test_queue");
            super::super::scope_first(carried, async {
                super::super::scope_first(None, async {
                    let guard = register(home.path(), "bot").unwrap();
                    let stored = lookup(home.path(), "bot", guard.reference()).unwrap();
                    assert_eq!(super::super::PrincipalState::from_wire(stored.as_deref()),
                        super::super::PrincipalState::Unknown);
                    assert!(!super::super::access::system_operation_permitted(home.path(), stored.as_deref()));
                    assert!(super::super::env_pairs().iter().any(|(key, value)| {
                        key == duduclaw_core::ENV_ON_BEHALF_OF
                            && super::super::PrincipalState::from_wire(Some(value))
                                == super::super::PrincipalState::Unknown
                    }));
                }).await;
            }).await;
        }
    }
    #[tokio::test]
    async fn scope_boxes_large_futures_and_registers_the_poll_time_principal() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let payload = [7u8; 16 * 1024];
        let inner = async move {
            tokio::task::yield_now().await;
            let (_, reference) = env_pair().unwrap();
            assert_eq!(
                lookup(h, "bot", &reference),
                Some(Some("user:poll-person".into()))
            );
            (reference, std::hint::black_box(payload)[0])
        };
        let inner_size = std::mem::size_of_val(&inner);
        // Construct before the caller's principal scope is installed.
        let wrapped = scope(h, "bot", inner);
        let wrapper_size = std::mem::size_of_val(&wrapped);
        eprintln!("principal context future: inner={inner_size}, wrapper={wrapper_size}");
        assert!(inner_size >= 16 * 1024);
        assert!(
            wrapper_size <= 512,
            "context must box its inner future before adding an async layer: {wrapper_size}"
        );
        let (reference, byte) =
            super::super::scope_first(Some("user:poll-person".into()), wrapped).await;
        assert_eq!(byte, 7);
        assert_eq!(
            lookup(h, "bot", &reference),
            None,
            "guard drops on completion"
        );

        // Measure the real dispatcher entry type without polling it or
        // invoking a model. The unchanged dispatcher fixture is the stack gate.
        let queue = std::sync::Arc::new(crate::message_queue::MessageQueue::open(h).unwrap());
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::AgentRegistry::new(h.join("agents")),
        ));
        let dispatcher = Box::pin(crate::dispatcher::poll_and_dispatch_sqlite(
            &queue, h, &registry, None, None,
        ));
        eprintln!(
            "SQLite dispatcher entry future: {} bytes",
            std::mem::size_of_val(dispatcher.as_ref().get_ref())
        );
        drop(dispatcher);
    }

    #[test]
    fn expired_and_malformed_references_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let guard = register(home.path(), "bot").unwrap();
        entries()
            .lock()
            .unwrap()
            .get_mut(guard.reference())
            .unwrap()
            .created = Instant::now() - MAX_AGE;
        assert_eq!(lookup(home.path(), "bot", guard.reference()), None);
        assert_eq!(lookup(home.path(), "bot", "bad"), None);
    }

    #[tokio::test]
    async fn expired_context_sweep_revokes_attended_clones_even_before_guard_drop() {
        let home = tempfile::tempdir().unwrap();
        let session = "webchat:context-expiry";
        let agent = "context-expiry-agent";
        let turn = format!("context-expiry-{}", uuid::Uuid::new_v4());
        crate::computer_use_sessions::attended::verified_webchat_message(
            session, Some("owner"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.into()),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn.clone()), async {
                    assert!(crate::computer_use_sessions::attended::bind_final_route(session, session, agent, &turn));
                    let guard = register(home.path(), agent).unwrap();
                    let captured = attended_call(home.path(), agent, guard.reference()).unwrap();
                    entries().lock().unwrap().get_mut(guard.reference()).unwrap().created =
                        Instant::now() - MAX_AGE - Duration::from_secs(1);
                    let replacement = register(home.path(), agent).unwrap();
                    assert!(!captured.valid_for(agent), "TTL sweep must revoke in-flight clones");
                    drop(guard);
                    assert!(!captured.valid_for(agent), "a missing registry row cannot defeat revocation");
                    assert!(attended_call(home.path(), agent, replacement.reference()).unwrap().valid_for(agent), "an old runtime cannot revoke a different current runtime");
                })),
        ).await;
    }
}
