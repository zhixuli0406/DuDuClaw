//! Running per-employee channel bots (native Linux isolation v1.81, F6
//! design r5 §3.2 step 4, §3.3; H5, A11; task F6h).
//!
//! A per-employee bot (Telegram / Discord / Slack, the employee's own token)
//! runs as a gateway task. Its `AbortHandle` is registered here under
//! `(home, channel, label)` when it starts, so the tombstone (a blocking call
//! that has no access to the RPC handler's handle map) can stop it at once:
//! [`stop_label`] aborts every running bot of the exact label.
//!
//! Race: a start runs the bot gate, spawns, then [`admit`] registers the task
//! and runs the gate again. The tombstone commits the disabled row and the
//! open quarantine row first and only then calls [`stop_label`]. So either the
//! second gate sees the tombstone (the new task is aborted) or the task was
//! registered before the commit and [`stop_label`] finds it.
//!
//! Outside protected mode the gate always allows and nothing is ever stopped
//! by a tombstone (there is none), so behaviour is unchanged.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use tokio::task::AbortHandle;

/// Channels whose per-employee bots run as gateway tasks.
pub(crate) const BOT_CHANNELS: [&str; 3] = ["telegram", "discord", "slack"];

type Key = (PathBuf, &'static str, String);

fn running() -> MutexGuard<'static, HashMap<Key, Vec<AbortHandle>>> {
    static RUNNING: OnceLock<Mutex<HashMap<Key, Vec<AbortHandle>>>> = OnceLock::new();
    // A poisoned map is still the map: stopping must keep working.
    RUNNING.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn channel_name(channel: &str) -> Option<&'static str> {
    BOT_CHANNELS.iter().copied().find(|known| *known == channel)
}

/// Two spellings of one home are the same home (fail closed: stop more).
fn same_home(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Record a started bot of `label` on `channel`. Finished tasks are pruned.
pub(crate) fn register(home: &Path, channel: &str, label: &str, handle: AbortHandle) {
    let Some(channel) = channel_name(channel) else {
        // Not a known bot channel: never left running unregistered.
        handle.abort();
        return;
    };
    let mut map = running();
    let entry = map.entry((home.to_path_buf(), channel, label.to_owned())).or_default();
    entry.retain(|running| !running.is_finished());
    entry.push(handle);
}

/// Abort every registered bot of `label` on `channel` in `home` (exact label).
/// Returns how many running tasks were aborted.
pub(crate) fn stop(home: &Path, channel: &str, label: &str) -> usize {
    let mut map = running();
    let keys: Vec<Key> = map.keys()
        .filter(|(h, c, l)| *c == channel && l == label && same_home(h, home)).cloned().collect();
    let mut stopped = 0;
    for key in keys {
        for handle in map.remove(&key).unwrap_or_default() {
            if !handle.is_finished() { stopped += 1; }
            handle.abort();
        }
    }
    stopped
}

/// Abort every registered bot of `label` in `home`, on every channel
/// (tombstone hook). Returns the channels on which a running bot was stopped.
pub(crate) fn stop_label(home: &Path, label: &str) -> Vec<&'static str> {
    let stopped: Vec<&'static str> = BOT_CHANNELS.iter().copied().filter(|channel| stop(home, channel, label) > 0).collect();
    if !stopped.is_empty() {
        tracing::info!(agent = label, channels = ?stopped, "per-employee channel bots stopped (employee no longer live)");
    }
    stopped
}

/// Whether a registered bot of `label` on `channel` is still running.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn is_running(home: &Path, channel: &str, label: &str) -> bool {
    running().iter().any(|((h, c, l), handles)| *c == channel && l == label && same_home(h, home)
        && handles.iter().any(|handle| !handle.is_finished()))
}

/// Register a just-spawned per-employee bot, then re-run the bot gate. A
/// refusal aborts the task (and any other bot of the label on the channel)
/// and returns `false`; the caller must then report "not started".
pub(crate) async fn admit(home: &Path, channel: &'static str, label: &str, handle: AbortHandle) -> bool {
    register(home, channel, label, handle);
    if crate::isolation_label_stores::channel_bot_allowed(home, label, channel).await { return true; }
    stop(home, channel, label);
    false
}

/// A start of `label`'s bot on `channel` was refused by the gate: any bot of
/// the label still running on that channel (an older start) stops too.
pub(crate) fn refused(home: &Path, channel: &str, label: &str) {
    if stop(home, channel, label) > 0 {
        tracing::warn!(agent = label, channel, "per-employee channel bot stopped: start refused by the lifecycle gate");
    }
}
