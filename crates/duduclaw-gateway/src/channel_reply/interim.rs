//! P10 (2026-10-08) — never leave a channel user waiting silently.
//!
//! When a channel reply has shown nothing (no visible progress event, no
//! answer) after `[channel_reply] interim_status_secs` seconds, the reply's
//! own progress callback gets ONE [`ProgressEvent::Interim`] line, built
//! without any model call from state the gateway already has: how long this
//! reply has been running and whether the employee is also busy with a
//! claimed task-board run (and for how long).
//!
//! * Off by default (`[channel_reply] interim_status = false`): most CLI
//!   replies take longer than the 8-second default, so turning it on adds a
//!   message to nearly every turn on channels that post progress as a new
//!   message (and costs push quota on LINE / WhatsApp). Edit-in-place
//!   channels (Telegram, Slack, Discord, Teams, Google Chat) show it as the
//!   first state of the progress message that the answer later replaces.
//! * Once per turn; never for internal sessions (only the external channel
//!   session prefixes the branding footer uses); never when the channel gave
//!   no progress callback (a channel that cannot post interim messages).
//! * The running task's title is included only when
//!   `interim_status_show_task = true` (default false: a channel user can be
//!   an outside customer) and the task's audience lets this channel read it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{ProgressCallback, ProgressEvent};

/// Default delay before the status line (seconds).
pub const DEFAULT_INTERIM_SECS: u64 = 8;
/// Upper bound for the configured delay.
const MAX_INTERIM_SECS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterimConfig {
    /// `None` = off.
    pub delay: Option<Duration>,
    pub show_task: bool,
}

impl InterimConfig {
    /// `config.toml [channel_reply] interim_status` (bool, default false),
    /// `interim_status_secs` (default 8, `0` = off, capped at 600) and
    /// `interim_status_show_task` (bool, default false). Read per turn; an
    /// unreadable file or a wrong type reads as off.
    pub fn from_home(home: &Path) -> Self {
        let off = Self {
            delay: None,
            show_task: false,
        };
        let Some(table) = std::fs::read_to_string(home.join("config.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
        else {
            return off;
        };
        let Some(sec) = table.get("channel_reply").and_then(|v| v.as_table()) else {
            return off;
        };
        if sec.get("interim_status").and_then(|v| v.as_bool()) != Some(true) {
            return off;
        }
        let secs = match sec.get("interim_status_secs") {
            None => DEFAULT_INTERIM_SECS,
            Some(v) => match v.as_integer() {
                Some(n) if n >= 0 => (n as u64).min(MAX_INTERIM_SECS),
                _ => return off,
            },
        };
        Self {
            delay: (secs > 0).then(|| Duration::from_secs(secs)),
            show_task: sec
                .get("interim_status_show_task")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }
}

/// The channel of an external session (`telegram:…` ⇒ `telegram`), `None`
/// for internal sessions (dashboard console, cron, bus, `default`).
pub fn external_channel(session_id: &str) -> Option<&'static str> {
    super::guarded::FOOTER_CHANNELS.iter().copied().find(|c| {
        session_id
            .strip_prefix(c)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// Cheap pre-check: an external session with the feature on.
pub fn wanted(home: &Path, session_id: &str) -> bool {
    external_channel(session_id).is_some() && InterimConfig::from_home(home).delay.is_some()
}

/// Whether a progress event is something the channel user actually sees.
fn is_visible(ev: &ProgressEvent) -> bool {
    !matches!(ev, ProgressEvent::Step(_) | ProgressEvent::ModelInfo { .. })
}

/// What the employee is busy with on the task board, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyRun {
    pub minutes: i64,
    /// Only when the operator allowed it and the audience lets the channel
    /// read the task.
    pub title: Option<String>,
}

/// Compose the status line (model-free).
pub fn status_line(waited_secs: u64, busy: Option<&BusyRun>) -> String {
    let mut s = format!("⏳ 已收到，正在處理（已 {waited_secs} 秒）。");
    if let Some(b) = busy {
        match &b.title {
            Some(t) => s.push_str(&format!(
                "這位員工也正在進行「{}」（已 {} 分鐘），回覆可能會晚一點。",
                duduclaw_core::truncate_chars(t, 60),
                b.minutes.max(0)
            )),
            None => s.push_str(&format!(
                "這位員工另有一項背景工作進行中（已 {} 分鐘），回覆可能會晚一點。",
                b.minutes.max(0)
            )),
        }
    }
    s
}

/// The employee's longest-running claimed task, read-only and best effort
/// (any error ⇒ `None`, the status line then only says how long it waited).
async fn busy_run(home: &Path, agent: &str, channel: &str, show_task: bool) -> Option<BusyRun> {
    if !home.join("tasks.db").exists() {
        return None;
    }
    let store = crate::task_store::TaskStore::open(home).ok()?;
    let tasks = store
        .list_tasks(Some("in_progress"), Some(agent), None)
        .await
        .ok()?;
    let now = chrono::Utc::now();
    let t = tasks
        .into_iter()
        .filter(|t| t.claimed_by.as_deref().is_some_and(|c| !c.is_empty()))
        .min_by(|a, b| a.updated_at.cmp(&b.updated_at))?;
    let started = t
        .claimed_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))?;
    let title = if show_task {
        let audience = crate::review_evidence::audience::task_audience(home, &t.id);
        crate::review_evidence::audience::channel_may_read_task(channel, &audience)
            .then(|| t.title.clone())
    } else {
        None
    };
    Some(BusyRun {
        minutes: (now - started).num_minutes(),
        title,
    })
}

/// Wrap a turn: returns the callback to hand to the reply pipeline and a
/// watcher future to run beside it. The watcher waits `delay`, then (unless
/// a visible event or the answer arrived first, see `done`) sends one
/// status line through the original callback.
pub fn arm(
    home: &Path,
    agent: Option<String>,
    session_id: &str,
    on_progress: Option<ProgressCallback>,
) -> (Option<ProgressCallback>, Option<Watcher>) {
    let Some(cb) = on_progress else {
        return (None, None);
    };
    let Some(channel) = external_channel(session_id) else {
        return (Some(cb), None);
    };
    let cfg = InterimConfig::from_home(home);
    let Some(delay) = cfg.delay else {
        return (Some(cb), None);
    };
    let shared: Arc<ProgressCallback> = Arc::new(cb);
    let seen = Arc::new(AtomicBool::new(false));
    let inner_cb = Arc::clone(&shared);
    let inner_seen = Arc::clone(&seen);
    let wrapped: ProgressCallback = Box::new(move |ev| {
        if is_visible(&ev) {
            inner_seen.store(true, Ordering::SeqCst);
        }
        inner_cb(ev);
    });
    let watcher = Watcher {
        home: home.to_path_buf(),
        agent,
        channel,
        delay,
        show_task: cfg.show_task,
        cb: shared,
        seen,
    };
    (Some(wrapped), Some(watcher))
}

pub struct Watcher {
    home: std::path::PathBuf,
    agent: Option<String>,
    channel: &'static str,
    delay: Duration,
    show_task: bool,
    cb: Arc<ProgressCallback>,
    seen: Arc<AtomicBool>,
}

impl Watcher {
    /// Run `turn`; if it is still silent after the delay, post one line.
    pub async fn run<T>(self, turn: impl std::future::Future<Output = T>) -> T {
        tokio::pin!(turn);
        tokio::select! {
            out = &mut turn => return out,
            _ = tokio::time::sleep(self.delay) => {}
        }
        if !self.seen.load(Ordering::SeqCst) {
            let busy = match &self.agent {
                Some(a) => busy_run(&self.home, a, self.channel, self.show_task).await,
                None => None,
            };
            (self.cb)(ProgressEvent::Interim {
                text: status_line(self.delay.as_secs(), busy.as_ref()),
            });
        }
        turn.await
    }
}

#[cfg(test)]
mod interim_tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn config_defaults_off_and_reads_the_switch() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(InterimConfig::from_home(dir.path()).delay, None);
        std::fs::write(
            dir.path().join("config.toml"),
            "[channel_reply]\ninterim_status = true\n",
        )
        .unwrap();
        assert_eq!(
            InterimConfig::from_home(dir.path()).delay,
            Some(Duration::from_secs(8))
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[channel_reply]\ninterim_status = true\ninterim_status_secs = 0\n",
        )
        .unwrap();
        assert_eq!(InterimConfig::from_home(dir.path()).delay, None);
        std::fs::write(
            dir.path().join("config.toml"),
            "[channel_reply]\ninterim_status = true\ninterim_status_secs = \"x\"\n",
        )
        .unwrap();
        assert_eq!(InterimConfig::from_home(dir.path()).delay, None);
    }

    #[test]
    fn only_external_sessions() {
        assert_eq!(external_channel("telegram:123"), Some("telegram"));
        assert_eq!(external_channel("line:abc"), Some("line"));
        assert_eq!(external_channel("webchat:1"), None);
        assert_eq!(external_channel("cron"), None);
        assert_eq!(external_channel("telegramx:1"), None);
    }

    #[test]
    fn status_line_never_names_a_task_unless_allowed() {
        let busy = BusyRun {
            minutes: 12,
            title: None,
        };
        let s = status_line(8, Some(&busy));
        assert!(s.contains("8 秒") && s.contains("12 分鐘"), "{s}");
        let named = BusyRun {
            minutes: 3,
            title: Some("季報".into()),
        };
        assert!(status_line(8, Some(&named)).contains("季報"));
        assert!(!status_line(8, None).contains("分鐘"));
    }

    fn rig(cfg: &str) -> (tempfile::TempDir, Arc<Mutex<Vec<String>>>, ProgressCallback) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), cfg).unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let cb: ProgressCallback = Box::new(move |ev| g.lock().unwrap().push(ev.to_display()));
        (dir, got, cb)
    }

    #[tokio::test]
    async fn one_line_after_the_delay_when_silent() {
        let (dir, got, cb) =
            rig("[channel_reply]\ninterim_status = true\ninterim_status_secs = 1\n");
        let (_cb, w) = arm(dir.path(), None, "telegram:1", Some(cb));
        let out = w
            .unwrap()
            .run(async {
                tokio::time::sleep(Duration::from_millis(1600)).await;
                7
            })
            .await;
        assert_eq!(out, 7);
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("⏳"));
    }

    #[tokio::test]
    async fn nothing_when_progress_or_answer_came_first_or_internal() {
        let (dir, got, cb) =
            rig("[channel_reply]\ninterim_status = true\ninterim_status_secs = 1\n");
        let (wrapped, w) = arm(dir.path(), None, "telegram:1", Some(cb));
        let wrapped = wrapped.unwrap();
        w.unwrap()
            .run(async {
                wrapped(ProgressEvent::Keepalive);
                tokio::time::sleep(Duration::from_millis(1600)).await;
            })
            .await;
        assert_eq!(got.lock().unwrap().len(), 1); // only the keepalive itself
        // A fast answer: nothing.
        let (dir, got, cb) =
            rig("[channel_reply]\ninterim_status = true\ninterim_status_secs = 1\n");
        let (_w, w) = arm(dir.path(), None, "slack:1", Some(cb));
        w.unwrap().run(async {}).await;
        assert!(got.lock().unwrap().is_empty());
        // Internal session: no watcher at all.
        let (dir, _got, cb) = rig("[channel_reply]\ninterim_status = true\n");
        let (_cb, w) = arm(dir.path(), None, "webchat:1", Some(cb));
        assert!(w.is_none());
    }
}
