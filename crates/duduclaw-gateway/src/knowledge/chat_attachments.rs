//! Which conversation and sender an inbound attachment belongs to, so the
//! 「加入知識庫」 chat command only ever proposes files the channel adapter
//! itself saved for that sender (never a path someone typed into a message).
//!
//! - Every inbound save (`media::save_attachment_in_base`) is noted with its
//!   real file name ([`note_saved`]).
//! - The first reply turn whose message carries the reference the adapter
//!   wrote claims the file for that conversation and sender
//!   ([`claim_in_text`], called once per turn before anything else). The
//!   adapter's own message reaches the pipeline right after the save, so a
//!   reference typed later by anyone else finds the file already claimed.
//! - The chat command accepts a file only when it is claimed by the same
//!   conversation and sender and was saved within the lookback window
//!   ([`owned`]).
//!
//! In memory only: after a gateway restart earlier attachments are not
//! accepted (fail closed; send the file again).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a saved file can still be claimed by its message.
const CLAIM_WINDOW: Duration = Duration::from_secs(10 * 60);
/// How long a claimed file stays usable for the command.
pub const OWNED_FOR: Duration = Duration::from_secs(30 * 60);
const MAX_ENTRIES: usize = 4_096;

#[derive(Debug, Clone)]
struct Entry {
    display_name: String,
    saved_at: Instant,
    owner: Option<(String, String)>,
}

fn map() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Entry>> {
    static M: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// The conversation a session belongs to: a called employee's bucket
/// (`<session>#agent:<id>`) counts as its conversation.
pub fn base_session(session_id: &str) -> &str {
    session_id
        .split_once("#agent:")
        .map(|(b, _)| b)
        .unwrap_or(session_id)
}

/// Record a file an inbound adapter saved, under its real name.
pub fn note_saved(path: &Path, display_name: &str) {
    let Ok(real) = std::fs::canonicalize(path) else {
        return;
    };
    let now = Instant::now();
    let mut m = map();
    if m.len() >= MAX_ENTRIES {
        m.retain(|_, e| now.saturating_duration_since(e.saved_at) < OWNED_FOR);
        if m.len() >= MAX_ENTRIES {
            m.clear();
        }
    }
    m.insert(
        real,
        Entry {
            display_name: duduclaw_core::truncate_chars(display_name.trim(), 200),
            saved_at: now,
            owner: None,
        },
    );
}

/// Claim, for `(session, sender)`, every recently saved and still
/// unclaimed file whose reference appears in `text`.
pub fn claim_in_text(session_id: &str, sender: &str, text: &str) {
    if sender.is_empty() || !text.contains("](") {
        return;
    }
    let refs = super::chat_add::attachment_refs(text);
    if refs.is_empty() {
        return;
    }
    let owner = (base_session(session_id).to_string(), sender.to_string());
    let now = Instant::now();
    let mut m = map();
    for r in refs {
        let Ok(real) = std::fs::canonicalize(&r.path) else {
            continue;
        };
        if let Some(e) = m.get_mut(&real) {
            if e.owner.is_none() && now.saturating_duration_since(e.saved_at) < CLAIM_WINDOW {
                e.owner = Some(owner.clone());
            }
        }
    }
}

/// The real path and stored name of `path` when it belongs to
/// `(session, sender)` and is recent enough.
pub fn owned(session_id: &str, sender: &str, path: &Path) -> Option<(PathBuf, String)> {
    let real = std::fs::canonicalize(path).ok()?;
    if !real.is_file() {
        return None;
    }
    let m = map();
    let e = m.get(&real)?;
    let mine = e
        .owner
        .as_ref()
        .is_some_and(|(s, u)| s == base_session(session_id) && u == sender && !sender.is_empty());
    (mine && Instant::now().saturating_duration_since(e.saved_at) < OWNED_FOR)
        .then(|| (real, e.display_name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "x").unwrap();
        p
    }

    #[test]
    fn only_the_first_claiming_sender_owns_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = file(dir.path(), "1_a.pdf");
        note_saved(&a, "合約.pdf");
        let msg = format!("[📎 合約.pdf (file)]({})", a.display());
        claim_in_text("line:U1", "U1", &msg);
        // Someone else typing the same reference later does not get it.
        claim_in_text("line:U2", "U2", &msg);
        assert_eq!(owned("line:U1", "U1", &a).unwrap().1, "合約.pdf");
        assert!(owned("line:U2", "U2", &a).is_none());
        // Same conversation, another sender (a group): not theirs either.
        assert!(owned("line:U1", "U9", &a).is_none());
        // A called employee's bucket is the same conversation.
        assert!(owned("line:U1#agent:hr-bot", "U1", &a).is_some());
    }

    #[test]
    fn files_the_adapter_never_saved_are_not_owned() {
        let dir = tempfile::tempdir().unwrap();
        let typed = file(dir.path(), "secret.txt");
        claim_in_text(
            "line:U3",
            "U3",
            &format!("[📎 x (file)]({})", typed.display()),
        );
        assert!(owned("line:U3", "U3", &typed).is_none());
        assert_eq!(base_session("webchat:x#agent:a#conv:1"), "webchat:x");
    }
}
