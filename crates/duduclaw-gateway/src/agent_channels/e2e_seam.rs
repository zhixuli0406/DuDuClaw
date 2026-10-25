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
