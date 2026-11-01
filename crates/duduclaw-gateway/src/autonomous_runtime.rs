//! Carry host-owned autonomous identities to the final runtime handoff.
//! Human conversations carry an explicit marker. Autonomous scopes always take precedence.
use duduclaw_core::autonomy_gate::{self, AdmissionGuard, AutonomyPath, PauseInfo};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
struct AdmissionContext {
    home: PathBuf,
    agent: String,
    callers: Vec<String>,
}

type PauseCapture = Arc<std::sync::Mutex<Option<Box<PauseInfo>>>>;

tokio::task_local! {
    static CONTEXT: AdmissionContext;
    static HUMAN_INTERACTIVE: PathBuf;
    static DENIED: Arc<AtomicBool>;
    static HANDED_OFF: Arc<AtomicBool>;
    static ROUND_MESSAGE: (PathBuf, String);
    static PAUSE_CAPTURE: PauseCapture;
}

/// Preserve the original typed refusal across a nested runtime that reports
/// ordinary execution summaries. This is opt-in and does not change chat.
pub(crate) fn capture_pause<F: Future>(
    future: F,
) -> impl Future<Output = (F::Output, Option<Box<PauseInfo>>)> {
    let future = Box::pin(future);
    async move {
        let captured: PauseCapture = Arc::new(std::sync::Mutex::new(None));
        let result = PAUSE_CAPTURE.scope(captured.clone(), future).await;
        let pause = captured.lock().unwrap_or_else(|e| e.into_inner()).take();
        (result, pause)
    }
}

fn restore_pause_capture<F: Future>(
    capture: Option<PauseCapture>, future: F,
) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        match capture {
            Some(capture) => PAUSE_CAPTURE.scope(capture, future).await,
            None => future.await,
        }
    }
}

pub(crate) fn pause_refusal(pause: Box<PauseInfo>) -> String {
    let _ = PAUSE_CAPTURE.try_with(|capture| {
        let mut first = capture.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() { *first = Some(pause.clone()); }
    });
    refusal(pause)
}

/// This marker never crosses a Tokio spawn. A nested autonomous scope still
/// wins, so a human turn cannot exempt its background work from admission.
pub(crate) fn human_interactive<'a, F: Future + 'a>(
    home: &'a Path,
    future: F,
) -> impl Future<Output = F::Output> + 'a {
    HUMAN_INTERACTIVE.scope(home.to_path_buf(), Box::pin(future))
}

pub(crate) fn is_human_interactive(home: &Path) -> bool {
    CONTEXT.try_with(|_| ()).is_err() && HUMAN_INTERACTIVE.try_with(|h| h == home).unwrap_or(false)
}

/// Utility entry points must declare their origin before configuration lookup,
/// fallback, or provider work. Unscoped calls are programming errors, not chat.
pub(crate) async fn require_utility_context(home: &Path) -> Result<(), String> {
    if let Ok(context) = CONTEXT.try_with(Clone::clone) {
        if context.home != home {
            return Err(refusal("utility home differs from autonomous scope"));
        }
        return check_context(&context);
    }
    if is_human_interactive(home) {
        return Ok(());
    }
    Err("AUTONOMY_CONTEXT_REQUIRED: utility call must declare human or autonomous context".into())
}

pub(crate) fn scope<'a, F: Future + 'a>(
    home: &'a Path,
    agent: &'a str,
    callers: &'a [String],
    future: F,
) -> impl Future<Output = F::Output> + 'a {
    // Allocate before constructing the async frame: model futures can be large,
    // and embedding them through nested identity scopes overflows worker stacks.
    let future = Box::pin(future);
    async move {
        let inherited = CONTEXT.try_with(Clone::clone).ok();
        let mut merged = callers.to_vec();
        if let Some(ref parent) = inherited {
            // Nested model helpers must retain the initiating employee as a source.
            if parent.home != home {
                merged.push("invalid:autonomy-home".into());
            } else {
                merged.extend(parent.callers.iter().cloned());
                if !parent.agent.is_empty() && parent.agent != agent {
                    merged.push(parent.agent.clone());
                }
            }
        }
        merged.sort();
        merged.dedup();
        let scoped = CONTEXT.scope(
            AdmissionContext {
                home: home.to_path_buf(),
                agent: agent.to_string(),
                callers: merged,
            },
            transport_scope(future),
        );
        if inherited.is_some() {
            scoped.await
        } else {
            HANDED_OFF
                .scope(
                    Arc::new(AtomicBool::new(false)),
                    DENIED.scope(Arc::new(AtomicBool::new(false)), scoped),
                )
                .await
        }
    }
}

/// Explicitly carry admission identity across spawned tasks/threads. A Tokio
/// task-local is not inherited by spawn or spawn_blocking.
#[derive(Clone)]
pub(crate) struct Snapshot {
    context: AdmissionContext,
    denied: Arc<AtomicBool>,
    handed_off: Arc<AtomicBool>,
    round: Option<(PathBuf, String)>,
    pause_capture: Option<PauseCapture>,
}

pub(crate) fn capture() -> Option<Snapshot> {
    Some(Snapshot {
        context: CONTEXT.try_with(Clone::clone).ok()?,
        denied: DENIED.try_with(Clone::clone).ok()?,
        handed_off: HANDED_OFF.try_with(Clone::clone).ok()?,
        round: ROUND_MESSAGE.try_with(Clone::clone).ok(),
        pause_capture: PAUSE_CAPTURE.try_with(Clone::clone).ok(),
    })
}

pub(crate) fn restore<F: Future>(
    snapshot: Option<Snapshot>,
    future: F,
) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        let Some(snapshot) = snapshot else {
            return future.await;
        };
        let invocation = CONTEXT.scope(snapshot.context, transport_scope(future));
        let invocation = DENIED.scope(
            snapshot.denied,
            HANDED_OFF.scope(snapshot.handed_off, invocation),
        );
        restore_pause_capture(snapshot.pause_capture, async move {
            match snapshot.round {
                Some(round) => ROUND_MESSAGE.scope(round, invocation).await,
                None => invocation.await,
            }
        }).await
    }
}

pub(crate) fn round_scope<'a, F: Future + 'a>(
    home: &'a Path,
    message: Option<&'a str>,
    future: F,
) -> impl Future<Output = F::Output> + 'a {
    let future = Box::pin(future);
    async move {
        match message {
            Some(message) => {
                ROUND_MESSAGE
                    .scope((home.to_path_buf(), message.to_string()), future)
                    .await
            }
            None => future.await,
        }
    }
}

pub(crate) fn request_scope<'a, F: Future + Send + 'a>(
    home: &'a Path,
    agent: &'a str,
    autonomous: bool,
    future: F,
) -> impl Future<Output = F::Output> + Send + 'a {
    // Erase the runtime future before building the scope frame. Pin<Box<F>>
    // bounds stack size but still exposes F to every caller's Send proof; the
    // nested chat/task-local pipeline otherwise overflows trait evaluation.
    let future: Pin<Box<dyn Future<Output = F::Output> + Send + 'a>> = Box::pin(future);
    async move {
        if !autonomous || CONTEXT.try_with(|_| ()).is_ok() {
            future.await
        } else {
            scope(home, agent, &[], future).await
        }
    }
}

/// Once an invocation may have run, a later refusal must not replay it as
/// an unstarted queue message. API attempts are conservatively counted here.
pub(crate) fn mark_handoff() {
    let _ = HANDED_OFF.try_with(|flag| flag.store(true, Ordering::Relaxed));
}

pub(crate) fn was_handed_off() -> bool {
    HANDED_OFF
        .try_with(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
}

pub(crate) fn was_denied() -> bool {
    DENIED
        .try_with(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
}

fn refusal(error: impl std::fmt::Display) -> String {
    let _ = DENIED.try_with(|flag| flag.store(true, Ordering::Relaxed));
    format!("AUTONOMY_PAUSED: {error}")
}

fn transport_scope<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        let snapshot = capture().expect("autonomous transport scope requires host identity");
        let handed_off = snapshot.handed_off.clone();
        duduclaw_core::autonomy_handoff::scope_owned(
            move || transport_admission(snapshot.clone()),
            move || handed_off.store(true, Ordering::Relaxed),
            future,
        )
        .await
    }
}

fn transport_admission(snapshot: Snapshot) -> duduclaw_core::autonomy_handoff::AdmissionFuture {
    Box::pin(async move {
        // Reinstall the host identity even when a lower-level crate detached a
        // model call with only the core transport hooks (e.g. UCCI shadow).
        let admission = CONTEXT.scope(
            snapshot.context,
            DENIED.scope(
                snapshot.denied,
                HANDED_OFF.scope(snapshot.handed_off, admit_current()),
            ),
        );
        restore_pause_capture(snapshot.pause_capture, async move {
            match snapshot.round {
                Some(round) => ROUND_MESSAGE.scope(round, admission).await,
                None => admission.await,
            }
        }).await
    })
}

/// Early callback check only. Final transport admission rechecks under the lease.
/// Do not mark a durable round started until a transport is about to hand off.
pub(crate) async fn check_current() -> Result<(), String> {
    let Ok(context) = CONTEXT.try_with(Clone::clone) else {
        return Ok(());
    };
    check_context(&context)
}

fn check_context(context: &AdmissionContext) -> Result<(), String> {
    let path = AutonomyPath::Background;
    autonomy_gate::check_global(&context.home, path).map_err(pause_refusal)?;
    for id in std::iter::once(&context.agent).chain(context.callers.iter()) {
        if id.is_empty() {
            continue;
        }
        autonomy_gate::check(&context.home, id, path).map_err(pause_refusal)?;
    }
    Ok(())
}

/// The fd lives through spawn/send. All identities are rechecked under one
/// shared lease, so neither a target pause nor a caller pause races handoff.
pub(crate) async fn admit_current() -> Result<Option<AdmissionGuard>, String> {
    let Ok(context) = CONTEXT.try_with(Clone::clone) else {
        return Ok(None);
    };
    let path = AutonomyPath::Background;
    let guard = if context.agent.is_empty() {
        autonomy_gate::admit_global(&context.home, path).await
    } else {
        autonomy_gate::admit(&context.home, &context.agent, path).await
    }
    .map_err(pause_refusal)?;
    check_context(&context)?;
    if !was_handed_off() {
        if let Ok((home, message)) = ROUND_MESSAGE.try_with(Clone::clone) {
            crate::responsibility::mark_round_started(&home, &message)
                .await
                .map_err(|error| format!("round start could not be recorded: {error}"))?;
        }
    }
    Ok(Some(guard))
}

/// New MCP rows explicitly identify employee creation. Legacy rows are bound
/// only when their creator is an installed employee (human UUIDs and channel
/// labels are not agent identities). Never read the model's prompt for this.
pub(crate) fn task_caller(home: &Path, task: &crate::task_store::TaskRow) -> Option<String> {
    let id = &task.created_by;
    if task.created_via.as_deref() == Some("agent") {
        // A typed employee origin must not disappear on malformed identity.
        return Some(
            if id.is_empty()
                || duduclaw_core::is_system_sender(id)
                || duduclaw_core::is_reserved_queue_sender(id)
            {
                "invalid:task-origin".to_string()
            } else {
                id.clone()
            },
        );
    }
    if !duduclaw_core::is_valid_agent_id(id)
        || duduclaw_core::is_system_sender(id)
        || duduclaw_core::is_reserved_queue_sender(id)
    {
        return None;
    }
    if home.join("agents").join(id).join("agent.toml").exists()
        || crate::ephemeral::is_ephemeral_id(id)
        || duduclaw_core::autonomy_gate::status(home)
            .agents
            .contains_key(id)
    {
        Some(id.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn utility_without_declared_context_is_denied_before_model_entry() {
        let home = tempfile::tempdir().unwrap();
        let before = crate::model_call_probe::calls();
        let error = crate::runtime_dispatch::run_utility_prompt(
            home.path(),
            None,
            "test-utility",
            "",
            "never send",
            16,
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("AUTONOMY_CONTEXT_REQUIRED:"));
        assert_eq!(crate::model_call_probe::calls(), before);
    }

    #[tokio::test]
    async fn human_marker_does_not_exempt_nested_or_detached_background_work() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        autonomy_gate::set_pause(home.path(), None, "pause", "operator").unwrap();
        human_interactive(home.path(), async {
            assert!(require_utility_context(home.path()).await.is_ok());
            assert!(
                scope(
                    home.path(),
                    "alice",
                    &[],
                    require_utility_context(home.path())
                )
                .await
                .is_err()
            );
            // Even an explicitly nested human marker cannot erase autonomy.
            assert!(
                scope(
                    home.path(),
                    "alice",
                    &[],
                    human_interactive(home.path(), require_utility_context(home.path()))
                )
                .await
                .is_err()
            );
            let detached_home = home.path().to_path_buf();
            let detached =
                tokio::spawn(async move { require_utility_context(&detached_home).await });
            assert!(
                detached
                    .await
                    .unwrap()
                    .unwrap_err()
                    .starts_with("AUTONOMY_CONTEXT_REQUIRED:")
            );
        })
        .await;
    }

    #[tokio::test]
    async fn paused_session_maintenance_keeps_real_owner_and_skips_models() {
        for global in [false, true] {
            let home = tempfile::tempdir().unwrap();
            employee(home.path(), "alice");
            let sessions =
                crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
            sessions
                .get_or_create("maintenance", "alice")
                .await
                .unwrap();
            sessions
                .append_message(
                    "maintenance",
                    "user",
                    "Please summarize the deployment decisions",
                    1,
                )
                .await
                .unwrap();
            sessions
                .append_message(
                    "maintenance",
                    "assistant",
                    "We will retain the original deployment records",
                    1,
                )
                .await
                .unwrap();
            let titles = sessions.list_title_candidates(3600).await.unwrap();
            assert_eq!(titles.len(), 1);
            assert_eq!(titles[0].agent_id, "alice");
            autonomy_gate::set_pause(
                home.path(),
                if global { None } else { Some("alice") },
                "pause",
                "operator",
            )
            .unwrap();
            let before = crate::model_call_probe::calls();
            crate::session_summarizer_task::tick_once(
                &sessions,
                home.path(),
                &crate::session_summarizer::SummarizeParams {
                    min_new_turns_to_trigger: 1,
                    cooldown_seconds: 0,
                    max_per_tick: 1,
                },
            )
            .await;
            crate::session_titler_task::tick_once(
                &sessions,
                home.path(),
                &crate::session_titler_task::TitleParams::default(),
            )
            .await;
            assert_eq!(crate::model_call_probe::calls(), before);
            assert_eq!(
                sessions.list_title_candidates(3600).await.unwrap()[0].titled_through_turn,
                0
            );
            assert_eq!(
                sessions.list_summary_candidates().await.unwrap()[0].summarized_through_turn,
                0
            );
        }
    }

    #[test]
    fn autonomy_scopes_bound_model_future_size() {
        async fn large_future() {
            let payload = [0u8; 64 * 1024];
            std::future::pending::<()>().await;
            std::hint::black_box(payload);
        }
        fn assert_small(future: impl Future<Output = ()>) {
            assert!(std::mem::size_of_val(&future) < 4096);
        }
        let home = Path::new("unused-future-size-fixture");
        assert!(std::mem::size_of_val(&large_future()) >= 64 * 1024);
        assert_small(scope(home, "alice", &[], large_future()));
        assert_small(restore(None, large_future()));
        assert_small(round_scope(home, None, large_future()));
        assert_small(request_scope(home, "alice", true, large_future()));
        assert_small(transport_scope(large_future()));
    }

    #[tokio::test]
    async fn scope_inherits_identity_when_polled_not_when_constructed() {
        let home = Path::new("unused-scope-identity-fixture");
        let nested = scope(home, "bob", &[], async {
            let snapshot = capture().expect("identity installed while polling");
            assert_eq!(snapshot.context.agent, "bob");
            assert_eq!(snapshot.context.callers, ["alice"]);
            mark_handoff();
        });
        scope(home, "alice", &[], async {
            nested.await;
            assert!(was_handed_off());
        })
        .await;
        assert!(capture().is_none());
    }

    fn employee(home: &Path, id: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nstatus = \"active\"\n").unwrap();
    }

    #[tokio::test]
    async fn trusted_caller_is_rechecked_at_handoff_and_chat_is_unaffected() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        employee(home.path(), "bob");
        let callers = vec!["alice".to_string()];
        scope(home.path(), "bob", &callers, async {
            drop(admit_current().await.unwrap());
            autonomy_gate::set_pause(home.path(), Some("alice"), "pause", "user").unwrap();
            assert!(admit_current().await.is_err());
            assert!(was_denied());
            assert!(!was_handed_off());
        })
        .await;
        assert!(admit_current().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn later_api_refusal_remembers_the_first_handoff() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        scope(home.path(), "alice", &[], async {
            let admission = admit_current().await.unwrap();
            mark_handoff();
            drop(admission);
            autonomy_gate::set_pause(home.path(), None, "pause", "user").unwrap();
            assert!(admit_current().await.is_err());
            assert!(was_denied() && was_handed_off());
        })
        .await;
    }
    #[tokio::test]
    async fn long_fake_model_does_not_block_pause_after_handoff() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let model_home = home.path().to_path_buf();
        let model = tokio::spawn(async move {
            scope(&model_home, "alice", &[], async {
                let admission = duduclaw_core::autonomy_handoff::admit().await.unwrap();
                admission.handed_off();
                sent_tx.send(()).unwrap();
                // Simulate a slow response while the request is already sent.
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => panic!("response barrier not released"),
                    _ = finish_rx => {},
                }
                assert!(was_handed_off());
            }).await;
        });
        sent_rx.await.unwrap();
        let pause_home = home.path().to_path_buf();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                autonomy_gate::set_pause(&pause_home, Some("alice"), "pause", "operator")
            }),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert!(
            !model.is_finished(),
            "pause must finish while the model is still running"
        );
        finish_tx.send(()).unwrap();
        model.await.unwrap();
    }

    #[tokio::test]
    async fn pause_waits_only_for_the_check_to_spawn_window() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        scope(home.path(), "alice", &[], async {
            let admission = duduclaw_core::autonomy_handoff::admit().await.unwrap();
            let pause_home = home.path().to_path_buf();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let mut pause = tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                autonomy_gate::set_pause(&pause_home, None, "pause", "operator")
            });
            started_rx.await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut pause)
                    .await
                    .is_err()
            );
            assert!(autonomy_gate::status(home.path()).global.is_none());
            // Fake successful spawn: the child may keep running after this.
            admission.handed_off();
            tokio::time::timeout(std::time::Duration::from_secs(2), pause)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(duduclaw_core::autonomy_handoff::admit().await.is_err());
            assert!(was_handed_off() && was_denied());
        })
        .await;
    }

    #[tokio::test]
    async fn nested_model_scope_retains_caller_and_handoff_flags() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        employee(home.path(), "bob");
        scope(home.path(), "alice", &[], async {
            scope(home.path(), "bob", &[], async {
                duduclaw_core::autonomy_handoff::admit()
                    .await
                    .unwrap()
                    .handed_off();
                autonomy_gate::set_pause(home.path(), Some("alice"), "pause", "operator").unwrap();
                assert!(duduclaw_core::autonomy_handoff::admit().await.is_err());
            })
            .await;
            assert!(was_handed_off() && was_denied());
        })
        .await;
    }

    #[tokio::test]
    async fn detached_core_transport_retains_caller_and_shared_flags() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        employee(home.path(), "bob");
        scope(home.path(), "bob", &["alice".into()], async {
            let hooks = duduclaw_core::autonomy_handoff::capture();
            let pause_home = home.path().to_path_buf();
            tokio::spawn(duduclaw_core::autonomy_handoff::restore(
                hooks,
                async move {
                    // No gateway task-local is inherited by the detached task.
                    assert!(CONTEXT.try_with(|_| ()).is_err());
                    duduclaw_core::autonomy_handoff::admit()
                        .await
                        .unwrap()
                        .handed_off();
                    autonomy_gate::set_pause(&pause_home, Some("alice"), "pause", "operator")
                        .unwrap();
                    assert!(duduclaw_core::autonomy_handoff::admit().await.is_err());
                },
            ))
            .await
            .unwrap();
            assert!(was_handed_off() && was_denied());
        })
        .await;
    }

    #[tokio::test]
    async fn cancelled_send_cannot_be_replayed_as_an_unstarted_turn() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        scope(home.path(), "alice", &[], async {
            let mut admission = duduclaw_core::autonomy_handoff::admit().await.unwrap();
            admission.request_started();
            // Cancelling while waiting for headers cannot prove non-delivery.
            drop(admission);
            autonomy_gate::set_pause(home.path(), None, "pause", "operator").unwrap();
            assert!(duduclaw_core::autonomy_handoff::admit().await.is_err());
            assert!(was_handed_off() && was_denied());
        })
        .await;
    }

    #[tokio::test]
    async fn captured_pause_survives_detached_transport_and_resume() {
        let home = tempfile::tempdir().unwrap();
        employee(home.path(), "alice");
        employee(home.path(), "bob");
        let (_, pause) = capture_pause(scope(home.path(), "bob", &["alice".into()], async {
            let hooks = duduclaw_core::autonomy_handoff::capture();
            let worker_home = home.path().to_path_buf();
            tokio::spawn(duduclaw_core::autonomy_handoff::restore(hooks, async move {
                autonomy_gate::set_pause(&worker_home, Some("alice"), "original pause", "operator").unwrap();
                assert!(duduclaw_core::autonomy_handoff::admit().await.is_err());
                autonomy_gate::resume(&worker_home, Some("alice")).unwrap();
            })).await.unwrap();
        })).await;
        let pause = pause.expect("typed refusal must outlive transport scope and resume");
        assert_eq!(pause.reason, "agent_pause");
        assert_eq!(pause.entry.unwrap().reason, "original pause");
        assert!(autonomy_gate::check(home.path(), "alice", AutonomyPath::Background).is_ok());
    }

}
