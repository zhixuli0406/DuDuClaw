//! Test-only stand-in for the model turn, armed per gateway home so parallel
//! tests never see each other's seam.

use std::collections::HashSet;
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

/// `OWN_BOT_REPLY agent=<id> session=<sid>` after claiming the session for
/// the agent (like the real turn), or `SESSION_CLASH …` when it belongs to
/// another employee. `None` when not armed for this home.
pub(crate) async fn answer(
    ctx: &crate::channel_reply::ReplyContext,
    agent: Option<&str>,
    session_id: &str,
) -> Option<String> {
    if !armed().lock().unwrap().contains(&ctx.home_dir) {
        return None;
    }
    let agent = agent.unwrap_or("none");
    Some(
        match ctx.session_manager.get_or_create(session_id, agent).await {
            Ok(_) => format!("OWN_BOT_REPLY agent={agent} session={session_id}"),
            Err(e) => format!("SESSION_CLASH {e}"),
        },
    )
}
