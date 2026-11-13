//! Test-only stand-in for the model turn, armed per gateway home so parallel
//! tests never see each other's seam.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn armed() -> &'static Mutex<HashSet<PathBuf>> {
    static A: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// Answer every turn under `home` with the seam reply.
pub(crate) fn arm(home: &Path) {
    armed().lock().unwrap().insert(home.to_path_buf());
}

#[derive(Clone)]
enum AdapterOutcome {
    Failure(String, crate::channel_reply::TurnFailureReason),
    Empty,
    Panic,
}

fn failures() -> &'static Mutex<HashMap<PathBuf, AdapterOutcome>> {
    static FAILURES: OnceLock<Mutex<HashMap<PathBuf, AdapterOutcome>>> = OnceLock::new();
    FAILURES.get_or_init(Default::default)
}

/// Simulate a runtime refusal after its question was saved. The real WebChat
/// adapter must keep the original live text even when a notice also exists.
pub(crate) fn failure_reply(home: &Path, text: String, reason: crate::channel_reply::TurnFailureReason) {
    failures().lock().unwrap().insert(home.into(), AdapterOutcome::Failure(text, reason));
}

/// Exercise the actual guarded entry points after user persistence, without
/// pre-writing a notice that could conceal a missing empty/panic handler.
pub(crate) fn empty_or_panicking_reply(home: &Path, panic: bool) {
    failures().lock().unwrap().insert(home.into(), if panic { AdapterOutcome::Panic } else { AdapterOutcome::Empty });
}

/// One model seam invocation paused until the test explicitly releases it.
pub(crate) struct WaitingTurn {
    pub session_id: String,
    pub delegation_id: Option<String>,
    pub release: tokio::sync::oneshot::Sender<()>,
}

fn waiting() -> &'static Mutex<HashMap<PathBuf, tokio::sync::mpsc::UnboundedSender<WaitingTurn>>> {
    static WAITING: OnceLock<Mutex<HashMap<PathBuf, tokio::sync::mpsc::UnboundedSender<WaitingTurn>>>> = OnceLock::new();
    WAITING.get_or_init(Default::default)
}

pub(crate) struct WaitingTurns {
    home: PathBuf,
    pub entered: tokio::sync::mpsc::UnboundedReceiver<WaitingTurn>,
}

fn delegation_capture() -> &'static Mutex<HashSet<PathBuf>> {
    static CAPTURE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    CAPTURE.get_or_init(Default::default)
}

impl WaitingTurns {
    /// Enqueue a real row from the model seam's host-owned lineage scope.
    pub(crate) fn capture_delegations(&self) {
        delegation_capture().lock().unwrap().insert(self.home.clone());
    }
}

pub(crate) fn arm_waiting(home: &Path) -> WaitingTurns {
    arm(home);
    let (tx, entered) = tokio::sync::mpsc::unbounded_channel();
    assert!(waiting().lock().unwrap().insert(home.to_path_buf(), tx).is_none());
    WaitingTurns { home: home.into(), entered }
}

impl Drop for WaitingTurns {
    fn drop(&mut self) {
        waiting().lock().unwrap().remove(&self.home);
        armed().lock().unwrap().remove(&self.home);
        delegation_capture().lock().unwrap().remove(&self.home);
        failures().lock().unwrap().remove(&self.home);
    }
}

#[derive(Debug)]
pub(crate) struct AttendanceObservation {
    pub agent: String,
    pub session: String,
    pub attended: bool,
}

fn observations() -> &'static Mutex<HashMap<PathBuf, Vec<AttendanceObservation>>> {
    static OBSERVED: OnceLock<Mutex<HashMap<PathBuf, Vec<AttendanceObservation>>>> =
        OnceLock::new();
    OBSERVED.get_or_init(Default::default)
}

pub(crate) fn take_attendance(home: &Path) -> Vec<AttendanceObservation> {
    observations()
        .lock()
        .unwrap()
        .remove(home)
        .unwrap_or_default()
}

/// `OWN_BOT_REPLY agent=<id> session=<sid>` after claiming the session for
/// the agent (like the real turn), or `SESSION_CLASH …` when it belongs to
/// another employee. `None` when not armed for this home.
pub(crate) async fn answer(
    ctx: &crate::channel_reply::ReplyContext,
    agent: Option<&str>,
    session_id: &str,
    turn_id: &str,
) -> Option<String> {
    if !armed().lock().unwrap().contains(&ctx.home_dir) {
        return None;
    }
    let agent = agent.unwrap_or("none");
    // This opt-in probe runs after the real pipeline resolves the employee
    // and session, using the same scope values its runtime would receive.
    let capture = delegation_capture().lock().unwrap().contains(&ctx.home_dir);
    let delegation_id = if capture {
        Some(duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(
            Some(session_id.into()),
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                Some(turn_id.into()), capture_delegation(&ctx.home_dir, agent),
            ),
        ).await)
    } else {
        None
    };
    let waiter = waiting().lock().unwrap().get(&ctx.home_dir).cloned();
    if let Some(waiter) = waiter {
        let (release, released) = tokio::sync::oneshot::channel();
        if waiter.send(WaitingTurn { session_id: session_id.into(), delegation_id, release }).is_ok() {
            let _ = released.await;
        }
    }
    let failure = failures().lock().unwrap().get(&ctx.home_dir).cloned();
    if let Some(outcome) = failure {
        let session = ctx.session_manager.get_or_create(session_id, agent).await.unwrap();
        ctx.session_manager.append_message(session_id, "user", "Hello", 1).await.unwrap();
        crate::channel_reply::bind_failure_session(session_id, &session.created_at, agent);
        return Some(match outcome {
            AdapterOutcome::Failure(text, reason) => {
                ctx.session_manager.record_turn_failure(session_id, turn_id, reason, None, Some(&session.created_at)).await.unwrap();
                text
            }
            AdapterOutcome::Empty => String::new(),
            AdapterOutcome::Panic => panic!("adapter panic after user persistence"),
        });
    }
    // Simulate only the model's host-owned environment AFTER the real reply
    // pipeline selected its employee/session. Register through the real API.
    let guard = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .scope(
            Some(session_id.into()),
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                Some(turn_id.into()),
                crate::claude_runner::REPLY_CHANNEL.scope(session_id.into(), async {
                    crate::computer_use_sessions::turns::register_current_turn(agent)
                }),
            ),
        )
        .await;
    let attended = crate::computer_use_sessions::turns::attended_live_for(agent, turn_id);
    observations()
        .lock()
        .unwrap()
        .entry(ctx.home_dir.clone())
        .or_default()
        .push(AttendanceObservation {
            agent: agent.into(),
            session: session_id.into(),
            attended,
        });
    drop(guard);
    Some(
        match ctx.session_manager.get_or_create(session_id, agent).await {
            Ok(_) => format!("OWN_BOT_REPLY agent={agent} session={session_id}"),
            Err(e) => format!("SESSION_CLASH {e}"),
        },
    )
}

async fn capture_delegation(home: &Path, agent: &str) -> String {
    use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
    let (turn_id, session_id, missing) = crate::memory_provenance::complete_upstream_pair(
        duduclaw_memory::feedback::CURRENT_TURN_ID.with(Clone::clone),
        duduclaw_memory::feedback::CURRENT_SESSION_ID.with(Clone::clone),
    );
    assert!(missing.is_none(), "the real WebChat route must carry a complete pair");
    let id = uuid::Uuid::new_v4().to_string();
    MessageQueue::open(home).unwrap().enqueue(&QueueMessage {
        id: id.clone(),
        sender: agent.into(),
        target: "downstream-fixture".into(),
        payload: "Lineage probe".into(),
        status: MessageStatus::Pending,
        retry_count: 0,
        delegation_depth: 1,
        origin_agent: Some(agent.into()),
        sender_agent: Some(agent.into()),
        error: None,
        response: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        acked_at: None,
        completed_at: None,
        reply_channel: session_id.clone(),
        turn_id,
        session_id,
        upstream_unknown: false,
        lane: None,
        on_behalf_of: crate::on_behalf_of::current(),
    }).await.unwrap();
    id
}
