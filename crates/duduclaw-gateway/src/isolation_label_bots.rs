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
//! F6fb item 9: a bot's per-update work (Discord message handlers, Telegram
//! callback queries, progress / typing / cleanup tasks, executor jobs) runs
//! in tasks of its own. Each bot task runs inside a [`BotScope`]
//! ([`scoped`]); every such task is started with [`spawn_scoped`] (or wrapped
//! with [`inherit`] when another scheduler spawns it), which tracks it in the
//! scope. Stopping the bot — [`stop`] / [`stop_label`], a refused [`admit`],
//! or any abort of the bot task (the scope is stopped when its future is
//! dropped before completion) — aborts every tracked task, so no handler of
//! a stopped bot can still send a reply.
//!
//! Outside protected mode the gate always allows and nothing is ever stopped
//! by a tombstone (there is none), so behaviour is unchanged.
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tokio::task::{AbortHandle, JoinHandle};

/// Channels whose per-employee bots run as gateway tasks.
pub(crate) const BOT_CHANNELS: [&str; 3] = ["telegram", "discord", "slack"];

/// The tasks one bot started (its abort scope).
#[derive(Default)]
pub(crate) struct BotScope {
    /// `(stopped, tracked tasks)`; one lock so a task tracked after the stop
    /// is aborted at once.
    state: Mutex<(bool, Vec<AbortHandle>)>,
}

impl BotScope {
    pub(crate) fn new() -> Arc<Self> { Arc::new(Self::default()) }

    fn lock(&self) -> MutexGuard<'_, (bool, Vec<AbortHandle>)> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn track(&self, handle: AbortHandle) {
        let mut state = self.lock();
        if state.0 {
            handle.abort();
            return;
        }
        state.1.retain(|task| !task.is_finished());
        state.1.push(handle);
    }

    /// Abort every tracked task; later tasks are aborted when tracked.
    pub(crate) fn stop(&self) -> usize {
        let mut state = self.lock();
        state.0 = true;
        let tasks = std::mem::take(&mut state.1);
        let running = tasks.iter().filter(|task| !task.is_finished()).count();
        for task in tasks { task.abort(); }
        running
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_stopped(&self) -> bool { self.lock().0 }
}

tokio::task_local! {
    static SCOPE: Arc<BotScope>;
}

/// The bot scope of the current task, if it runs inside one.
fn current() -> Option<Arc<BotScope>> { SCOPE.try_with(Arc::clone).ok() }

/// Run a bot's future inside `scope`. Dropping it before it completes (an
/// abort of the bot task) stops the scope.
pub(crate) async fn scoped<F: Future>(scope: Arc<BotScope>, future: F) -> F::Output {
    struct StopOnDrop(Arc<BotScope>, bool);
    impl Drop for StopOnDrop {
        fn drop(&mut self) { if !self.1 { self.0.stop(); } }
    }
    let mut guard = StopOnDrop(scope.clone(), false);
    let output = SCOPE.scope(scope, future).await;
    guard.1 = true;
    output
}

/// `future`, carrying the current bot scope (if any) into whatever task runs
/// it, so the tasks it starts with [`spawn_scoped`] are tracked too.
pub(crate) fn inherit<F>(future: F) -> Pin<Box<dyn Future<Output = F::Output> + Send>>
where F: Future + Send + 'static, F::Output: 'static {
    match current() {
        Some(scope) => Box::pin(SCOPE.scope(scope, future)),
        None => Box::pin(future),
    }
}

/// `tokio::spawn`, tracked in the current bot scope (a no-op difference
/// outside one: global bots and every other caller are unchanged).
pub(crate) fn spawn_scoped<F>(future: F) -> JoinHandle<F::Output>
where F: Future + Send + 'static, F::Output: Send + 'static {
    let scope = current();
    let handle = tokio::spawn(inherit(future));
    if let Some(scope) = scope { scope.track(handle.abort_handle()); }
    handle
}

type Key = (PathBuf, &'static str, String);
type Running = (AbortHandle, Option<Arc<BotScope>>);

fn running() -> MutexGuard<'static, HashMap<Key, Vec<Running>>> {
    static RUNNING: OnceLock<Mutex<HashMap<Key, Vec<Running>>>> = OnceLock::new();
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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn register(home: &Path, channel: &str, label: &str, handle: AbortHandle) {
    register_scoped(home, channel, label, handle, None);
}

/// [`register`] with the bot's task scope (stopped with the bot).
pub(crate) fn register_scoped(home: &Path, channel: &str, label: &str, handle: AbortHandle, scope: Option<Arc<BotScope>>) {
    let Some(channel) = channel_name(channel) else {
        // Not a known bot channel: never left running unregistered.
        handle.abort();
        if let Some(scope) = scope { scope.stop(); }
        return;
    };
    let mut map = running();
    let entry = map.entry((home.to_path_buf(), channel, label.to_owned())).or_default();
    entry.retain(|(running, _)| !running.is_finished());
    entry.push((handle, scope));
}

/// Abort every registered bot of `label` on `channel` in `home` (exact label),
/// with every task its scope tracks. Returns how many running bots were aborted.
pub(crate) fn stop(home: &Path, channel: &str, label: &str) -> usize {
    let mut map = running();
    let keys: Vec<Key> = map.keys()
        .filter(|(h, c, l)| *c == channel && l == label && same_home(h, home)).cloned().collect();
    let mut stopped = 0;
    for key in keys {
        for (handle, scope) in map.remove(&key).unwrap_or_default() {
            if !handle.is_finished() { stopped += 1; }
            handle.abort();
            if let Some(scope) = scope { scope.stop(); }
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
        && handles.iter().any(|(handle, _)| !handle.is_finished()))
}

/// Register a just-spawned per-employee bot (with its task scope), then
/// re-run the bot gate. A refusal aborts the task, its tracked tasks and any
/// other bot of the label on the channel, and returns `false`; the caller
/// must then report "not started".
pub(crate) async fn admit(home: &Path, channel: &'static str, label: &str, handle: AbortHandle, scope: Arc<BotScope>) -> bool {
    register_scoped(home, channel, label, handle, Some(scope));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fake bot loop: one update arrives, its handler (and a task the
    /// handler starts, and a job run by another scheduler) would reply later.
    async fn fake_loop(replies: tokio::sync::mpsc::UnboundedSender<&'static str>, ready: Arc<tokio::sync::Notify>) {
        let handler = replies.clone();
        spawn_scoped(async move {
            let progress = handler.clone();
            spawn_scoped(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                let _ = progress.send("progress edit");
            });
            tokio::time::sleep(Duration::from_millis(80)).await;
            let _ = handler.send("late reply");
        });
        // An executor-style job: spawned by a plain task, carrying the scope.
        let job = replies.clone();
        tokio::spawn(inherit(async move {
            spawn_scoped(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                let _ = job.send("executor job reply");
            });
        }));
        ready.notify_one();
        std::future::pending::<()>().await;
    }

    fn start(scope: &Arc<BotScope>) -> (JoinHandle<()>, tokio::sync::mpsc::UnboundedReceiver<&'static str>, Arc<tokio::sync::Notify>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ready = Arc::new(tokio::sync::Notify::new());
        let bot = tokio::spawn(scoped(scope.clone(), fake_loop(tx, ready.clone())));
        (bot, rx, ready)
    }

    /// `stop_label` (the tombstone hook) stops the bot and every
    /// per-update task it started: nothing replies afterwards.
    #[tokio::test]
    async fn f6fb_stop_label_stops_per_update_handlers() {
        let home = tempfile::tempdir().unwrap();
        let scope = BotScope::new();
        let (bot, mut replies, ready) = start(&scope);
        ready.notified().await;
        for _ in 0..4 { tokio::task::yield_now().await; }
        register_scoped(home.path(), "telegram", "f6fb-bot", bot.abort_handle(), Some(scope.clone()));
        assert_eq!(stop_label(home.path(), "f6fb-bot"), vec!["telegram"]);
        assert!(scope.is_stopped());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(replies.try_recv().is_err(), "no handler of a stopped bot replied");
        assert!(bot.await.unwrap_err().is_cancelled());
        // A task tracked after the stop is aborted at once.
        let late = tokio::spawn(SCOPE.scope(scope.clone(), async { spawn_scoped(std::future::pending::<()>()) })).await.unwrap();
        assert!(late.await.unwrap_err().is_cancelled());
    }

    /// Any abort of the bot task (e.g. the RPC's hot stop through its join
    /// handle) stops the scope too.
    #[tokio::test]
    async fn f6fb_aborted_bot_task_stops_its_handlers() {
        let scope = BotScope::new();
        let (bot, mut replies, ready) = start(&scope);
        ready.notified().await;
        for _ in 0..4 { tokio::task::yield_now().await; }
        bot.abort();
        assert!(bot.await.unwrap_err().is_cancelled());
        assert!(scope.is_stopped(), "the drop of the aborted bot future stopped the scope");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(replies.try_recv().is_err());
        // Outside a scope `spawn_scoped` is a plain spawn.
        assert_eq!(spawn_scoped(async { 7 }).await.unwrap(), 7);
    }
}
