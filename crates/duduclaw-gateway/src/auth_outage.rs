//! Authentication-outage alert (DESIGN-account-credential-hardening-2026-09 §D7).
//!
//! ## The failure mode this closes
//!
//! On 2026-09-08 a production container spent **18 hours** with every cron
//! dispatch, every goal-loop round and every channel reply failing on
//! `All accounts exhausted. Last error: … oauth_org_not_allowed`. The only
//! trace was one `warn!` line per failure in `debug.log`. Nothing reached a
//! human: the operator eventually found out by messaging the agent by hand
//! and getting no answer. A platform whose whole promise is "it keeps
//! working while you are asleep" must say something when it stops working.
//!
//! ## What it does
//!
//! Both spawn paths (`claude_runner::call_with_rotation` for
//! dispatch/cron/heartbeat/goal-loop, and `channel_reply`'s fallback branch
//! for user-facing replies) call [`record_outage`] when a call ends with
//! "all accounts exhausted" AND the last error classifies as
//! [`FailureReason::AuthFailed`][crate::channel_reply::classify_cli_failure].
//! The first such call flips a cross-process state file
//! (`<home>/state/auth_outage.json`) from inactive to active and notifies
//! **once**: an Activity Feed row plus one push to the agent's notification
//! target. Every later failure while still in outage is silent
//! ([`OutageTransition::StillInOutage`]) — no log spam, no push storm.
//!
//! Any subsequent success calls [`record_recovery`], which clears the file
//! and notifies once more ("認證已恢復"). Signal on *state change* only,
//! matching the reporting doctrine used by the GVU stagnation monitor.
//!
//! ## Trust boundary
//!
//! `detail` originates in CLI stderr / provider error text — untrusted DATA.
//! It is newline-stripped and byte-truncated (CJK-safe,
//! [`duduclaw_core::truncate_bytes`]) before it is stored or rendered, and
//! the user-facing message prefers a *classified* zh-TW phrase derived from
//! [`crate::channel_reply::auth_failure_kind_hint`] over the raw text.
//! Credentials never reach here: the classifier upstream only ever forwards
//! the provider's error label, and nothing in this module logs `detail` at a
//! level above `debug`.
//!
//! ## Fail-open everywhere
//!
//! A missing, unreadable or corrupt state file reads as "not in outage".
//! Every write failure is logged and swallowed. This module is an alarm bell
//! bolted onto the hot path — it must never be the reason a reply fails.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Byte cap for the stored/rendered `detail` (untrusted provider text).
const DETAIL_MAX_BYTES: usize = 240;

/// How often [`record_recovery`] is allowed to touch the disk when this
/// process has never itself recorded an outage. See [`should_check_state`].
const RECOVERY_POLL_INTERVAL_SECS: u64 = 60;

/// Set by [`record_outage`] whenever this process observes an active outage,
/// cleared by [`record_recovery`] when it clears one. Purely an optimisation
/// hint — see [`should_check_state`] for why a stale `false` is still safe.
static MAYBE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Unix seconds of the last disk check performed by [`record_recovery`]
/// while `MAYBE_ACTIVE` was false. `0` = never checked.
static LAST_RECOVERY_CHECK_UNIX: AtomicU64 = AtomicU64::new(0);

// ── State file ───────────────────────────────────────────────────────────

/// On-disk shape of `<home>/state/auth_outage.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthOutageState {
    /// Whether an outage is currently open.
    pub active: bool,
    /// RFC 3339 timestamp of when the outage was first observed.
    #[serde(default)]
    pub since: String,
    /// The agent whose dispatch/reply first hit the wall (attribution only —
    /// the outage itself is account-pool-wide, not agent-specific).
    #[serde(default)]
    pub agent_id: String,
    /// Sanitized (newline-stripped, byte-truncated) provider error text.
    #[serde(default)]
    pub detail: String,
    /// RFC 3339 timestamp of the one notification sent for this outage.
    /// `None` means "entered but the push never went out" — the debounce
    /// keys off `active`, not off this field, so a failed push does not
    /// re-arm the alarm.
    #[serde(default)]
    pub notified_at: Option<String>,
}

/// What one [`record_outage`] / [`record_recovery`] call actually changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutageTransition {
    /// inactive → active. This is the only transition that notifies.
    EnteredOutage,
    /// Already active; nothing written, nothing sent.
    StillInOutage,
    /// active → inactive. Notifies once.
    Recovered,
    /// Nothing to do (recovery while already healthy), or the state file
    /// could not be written (fail-open).
    Ignored,
}

/// `<home>/state/auth_outage.json`.
pub fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join("state").join("auth_outage.json")
}

/// Read the current state. Missing / unreadable / corrupt ⇒ inactive
/// (degrade, never fabricate: a broken file must not invent an outage, and
/// must not keep one open forever either).
fn load(path: &Path) -> AuthOutageState {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return AuthOutageState::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Atomic persist (temp + rename) with owner-only perms, inside a
/// caller-held lock.
fn persist(path: &Path, state: &AuthOutageState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

// ── Text ─────────────────────────────────────────────────────────────────

/// Fold untrusted provider text into one line and cap it at
/// [`DETAIL_MAX_BYTES`]. Newlines are stripped *before* truncation so the
/// cap applies to what is actually stored, and the truncation is CJK-safe.
pub(crate) fn sanitize_detail(detail: &str) -> String {
    let single_line: String = detail
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = single_line.split_whitespace().collect::<Vec<_>>().join(" ");
    duduclaw_core::truncate_bytes(&collapsed, DETAIL_MAX_BYTES).to_string()
}

/// Turn the raw error into a zh-TW cause phrase an end user can act on.
/// Falls back to the sanitized raw text when the failure is in the
/// AuthFailed family but not one of the two recognised kinds.
fn cause_phrase(detail: &str) -> String {
    match crate::channel_reply::auth_failure_kind_hint(detail) {
        Some("org_disabled") => "Anthropic 回報此組織已停用 Claude Code 訂閱存取".to_string(),
        Some("invalid_token") => "token 無效或已過期".to_string(),
        _ => sanitize_detail(detail),
    }
}

fn outage_text(detail: &str) -> String {
    format!(
        "⚠️ 所有 Claude 帳號的認證都失效了（原因：{}），排程與自動回覆已停擺。\
         請到 儀表板 → 設定 → 帳號 更新 token；更新後會自動恢復並再通知一次。",
        cause_phrase(detail)
    )
}

fn recovery_text() -> String {
    "✅ Claude 帳號認證已恢復，排程與自動回覆重新運作。".to_string()
}

// ── Notification seam ────────────────────────────────────────────────────

/// Which side of the transition a notice belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    Entered,
    Recovered,
}

impl NoticeKind {
    fn event_type(self) -> &'static str {
        match self {
            NoticeKind::Entered => "auth_outage",
            NoticeKind::Recovered => "auth_outage_recovered",
        }
    }

    fn level(self) -> crate::notify_governance::NotifyLevel {
        match self {
            // L3: urgent AND important AND actionable AND real — every
            // scheduled job in the install is dead until a human pastes a new
            // token, and L3 is deliberately not suppressed by quiet hours.
            NoticeKind::Entered => crate::notify_governance::NotifyLevel::Act,
            // L1: nothing to do, the machine already fixed itself.
            NoticeKind::Recovered => crate::notify_governance::NotifyLevel::Fyi,
        }
    }
}

/// The payload a transition *would* deliver. Tests capture this instead of
/// hitting a real channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutageNotice {
    pub agent_id: String,
    pub kind: NoticeKind,
    pub text: String,
}

/// Real delivery: Activity Feed row + one channel push. Both are best-effort
/// — an alarm that panics is worse than an alarm that misses.
async fn deliver(home_dir: &Path, notice: OutageNotice) {
    post_activity(home_dir, &notice).await;

    let outcome = crate::goal_notify::notify_agent_plain(
        home_dir,
        &notice.agent_id,
        notice.kind.level(),
        notice.kind.event_type(),
        &notice.text,
    )
    .await;
    match outcome {
        crate::goal_notify::NotifyOutcome::NoTarget => {
            info!(
                agent = %notice.agent_id,
                event = notice.kind.event_type(),
                "auth outage: no notification target configured (feed row only)"
            );
        }
        crate::goal_notify::NotifyOutcome::SendFailed => {
            warn!(
                agent = %notice.agent_id,
                event = notice.kind.event_type(),
                "auth outage: channel push failed (non-fatal)"
            );
        }
        other => {
            debug!(agent = %notice.agent_id, outcome = ?other, "auth outage notice delivered");
        }
    }
}

/// Best-effort Activity Feed append. Mirrors
/// `gvu::stagnation::StagnationMonitor::post_activity` — telemetry, not
/// control flow.
async fn post_activity(home_dir: &Path, notice: &OutageNotice) {
    let store = match crate::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "auth outage: failed to open task store (non-fatal)");
            return;
        }
    };
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: notice.kind.event_type().to_string(),
        agent_id: notice.agent_id.clone(),
        task_id: None,
        summary: notice.text.clone(),
        timestamp: Utc::now().to_rfc3339(),
        metadata: None,
    };
    if let Err(e) = store.append_activity(&row).await {
        debug!(error = %e, "auth outage: activity append failed (non-fatal)");
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Record that a dispatch/reply ended with every account exhausted on an
/// authentication failure. Only the inactive→active transition notifies.
pub async fn record_outage(home_dir: &Path, agent_id: &str, detail: &str) -> OutageTransition {
    let home = home_dir.to_path_buf();
    record_outage_with(home_dir, agent_id, detail, move |notice| async move {
        deliver(&home, notice).await;
    })
    .await
}

/// Record that an account answered successfully. Clears an open outage and
/// notifies once; a no-op when nothing was open.
///
/// **Hot-path cost.** Called on every successful CLI spawn, so the common
/// (healthy, never-degraded) case must not pay for a lock + read. It is
/// gated by [`should_check_state`]: this process always re-reads when it has
/// itself seen an outage, and otherwise at most once per
/// [`RECOVERY_POLL_INTERVAL_SECS`] — enough to still clear an outage that a
/// *different* process (the cron path vs. the channel path) opened, without
/// a per-reply syscall.
pub async fn record_recovery(home_dir: &Path, agent_id: &str) -> OutageTransition {
    if !should_check_state(Utc::now().timestamp().max(0) as u64) {
        return OutageTransition::Ignored;
    }
    let home = home_dir.to_path_buf();
    record_recovery_with(home_dir, agent_id, move |notice| async move {
        deliver(&home, notice).await;
    })
    .await
}

/// Fast-path gate for [`record_recovery`], factored out so it is testable
/// without touching the filesystem.
///
/// Returns `true` when the state file should actually be read. A stale
/// `MAYBE_ACTIVE == false` (outage opened by another process) costs at most
/// [`RECOVERY_POLL_INTERVAL_SECS`] of delay before the periodic check picks
/// it up — it can never make a recovery permanently invisible.
fn should_check_state(now_unix: u64) -> bool {
    if MAYBE_ACTIVE.load(Ordering::Relaxed) {
        return true;
    }
    let last = LAST_RECOVERY_CHECK_UNIX.load(Ordering::Relaxed);
    if now_unix.saturating_sub(last) < RECOVERY_POLL_INTERVAL_SECS {
        return false;
    }
    LAST_RECOVERY_CHECK_UNIX.store(now_unix, Ordering::Relaxed);
    true
}

// ── Internals (test seam) ────────────────────────────────────────────────

/// [`record_outage`] with the notification side-effect injected. Production
/// passes [`deliver`]; tests pass a capturing closure so no real channel is
/// touched.
pub(crate) async fn record_outage_with<N, F>(
    home_dir: &Path,
    agent_id: &str,
    detail: &str,
    notify: N,
) -> OutageTransition
where
    N: FnOnce(OutageNotice) -> F,
    F: std::future::Future<Output = ()>,
{
    let path = state_path(home_dir);
    let clean = sanitize_detail(detail);
    let now = Utc::now().to_rfc3339();

    // Lock + read + write in one critical section so two concurrent
    // exhaustion paths (cron and channel) cannot both decide they are the
    // first and both notify.
    let outcome = duduclaw_core::with_file_lock(&path, || {
        let current = load(&path);
        if current.active {
            return Ok(None);
        }
        let next = AuthOutageState {
            active: true,
            since: now.clone(),
            agent_id: agent_id.to_string(),
            detail: clean.clone(),
            notified_at: Some(now.clone()),
        };
        persist(&path, &next)?;
        Ok(Some(next))
    });

    match outcome {
        Ok(None) => {
            MAYBE_ACTIVE.store(true, Ordering::Relaxed);
            debug!(agent = %agent_id, "auth outage already open — staying quiet");
            OutageTransition::StillInOutage
        }
        Ok(Some(_)) => {
            MAYBE_ACTIVE.store(true, Ordering::Relaxed);
            warn!(
                agent = %agent_id,
                "all Claude accounts failed authentication — entering auth outage"
            );
            notify(OutageNotice {
                agent_id: agent_id.to_string(),
                kind: NoticeKind::Entered,
                text: outage_text(detail),
            })
            .await;
            OutageTransition::EnteredOutage
        }
        Err(e) => {
            // Fail-open: an unwritable state file must not break the caller.
            warn!(error = %e, "auth outage: state write failed (non-fatal)");
            OutageTransition::Ignored
        }
    }
}

/// [`record_recovery`] with the notification side-effect injected, and
/// without the [`should_check_state`] fast path (tests want determinism).
pub(crate) async fn record_recovery_with<N, F>(
    home_dir: &Path,
    agent_id: &str,
    notify: N,
) -> OutageTransition
where
    N: FnOnce(OutageNotice) -> F,
    F: std::future::Future<Output = ()>,
{
    let path = state_path(home_dir);

    let outcome = duduclaw_core::with_file_lock(&path, || {
        let current = load(&path);
        if !current.active {
            return Ok(false);
        }
        // Clearing = removing the file. A missing file is the canonical
        // "healthy" state, so this is idempotent and leaves no stale row.
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
        }
    });

    match outcome {
        Ok(false) => OutageTransition::Ignored,
        Ok(true) => {
            MAYBE_ACTIVE.store(false, Ordering::Relaxed);
            info!(agent = %agent_id, "Claude account authentication recovered — clearing auth outage");
            notify(OutageNotice {
                agent_id: agent_id.to_string(),
                kind: NoticeKind::Recovered,
                text: recovery_text(),
            })
            .await;
            OutageTransition::Recovered
        }
        Err(e) => {
            warn!(error = %e, "auth outage: recovery clear failed (non-fatal)");
            OutageTransition::Ignored
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Capturing notifier: records the would-be notice instead of sending.
    fn capture(
        sink: Arc<Mutex<Vec<OutageNotice>>>,
    ) -> impl FnOnce(OutageNotice) -> std::future::Ready<()> {
        move |notice| {
            sink.lock().unwrap().push(notice);
            std::future::ready(())
        }
    }

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn first_outage_writes_state_and_notifies_exactly_once() {
        let dir = home();
        let sink = Arc::new(Mutex::new(Vec::new()));

        let t = record_outage_with(
            dir.path(),
            "trader-lead",
            "All accounts exhausted. Last error: claude CLI assistant error: oauth_org_not_allowed",
            capture(sink.clone()),
        )
        .await;
        assert_eq!(t, OutageTransition::EnteredOutage);

        let state: AuthOutageState =
            serde_json::from_str(&std::fs::read_to_string(state_path(dir.path())).unwrap())
                .unwrap();
        assert!(state.active);
        assert_eq!(state.agent_id, "trader-lead");
        assert!(state.notified_at.is_some());
        assert!(!state.since.is_empty());
        assert!(!state.detail.contains('\n'));

        let notices = sink.lock().unwrap();
        assert_eq!(notices.len(), 1, "exactly one notification per outage");
        assert_eq!(notices[0].kind, NoticeKind::Entered);
        assert!(
            notices[0].text.contains("已停用 Claude Code 訂閱存取"),
            "org-disabled cause phrase must be used: {}",
            notices[0].text
        );
    }

    #[tokio::test]
    async fn repeated_outage_is_still_in_outage_and_silent() {
        let dir = home();
        let sink = Arc::new(Mutex::new(Vec::new()));

        assert_eq!(
            record_outage_with(
                dir.path(),
                "a",
                "authentication_failed",
                capture(sink.clone())
            )
            .await,
            OutageTransition::EnteredOutage
        );
        for _ in 0..5 {
            assert_eq!(
                record_outage_with(
                    dir.path(),
                    "a",
                    "authentication_failed",
                    capture(sink.clone())
                )
                .await,
                OutageTransition::StillInOutage
            );
        }
        assert_eq!(
            sink.lock().unwrap().len(),
            1,
            "debounce: never more than one push per open outage"
        );
    }

    #[tokio::test]
    async fn recovery_when_inactive_is_ignored() {
        let dir = home();
        let sink = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            record_recovery_with(dir.path(), "a", capture(sink.clone())).await,
            OutageTransition::Ignored
        );
        assert!(sink.lock().unwrap().is_empty());
        assert!(!state_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn recovery_when_active_clears_file_and_notifies_once() {
        let dir = home();
        let sink = Arc::new(Mutex::new(Vec::new()));

        record_outage_with(
            dir.path(),
            "a",
            "authentication_failed",
            capture(sink.clone()),
        )
        .await;
        assert!(state_path(dir.path()).exists());

        assert_eq!(
            record_recovery_with(dir.path(), "a", capture(sink.clone())).await,
            OutageTransition::Recovered
        );
        assert!(
            !state_path(dir.path()).exists(),
            "recovery must clear the state file"
        );

        let notices = sink.lock().unwrap();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[1].kind, NoticeKind::Recovered);
        assert!(notices[1].text.contains("已恢復"));

        drop(notices);
        // A second recovery has nothing left to clear.
        assert_eq!(
            record_recovery_with(dir.path(), "a", capture(sink.clone())).await,
            OutageTransition::Ignored
        );
        assert_eq!(sink.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn corrupt_state_file_reads_as_inactive() {
        let dir = home();
        std::fs::create_dir_all(dir.path().join("state")).unwrap();
        std::fs::write(state_path(dir.path()), "{ this is not json").unwrap();

        // Reads as inactive ⇒ a fresh outage still counts as "entering".
        let sink = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            record_outage_with(
                dir.path(),
                "a",
                "authentication_failed",
                capture(sink.clone())
            )
            .await,
            OutageTransition::EnteredOutage
        );
        assert_eq!(sink.lock().unwrap().len(), 1);

        // And a corrupt file never leaves a phantom outage open for recovery.
        std::fs::write(state_path(dir.path()), "\u{0}not json either").unwrap();
        let sink2 = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            record_recovery_with(dir.path(), "a", capture(sink2.clone())).await,
            OutageTransition::Ignored
        );
        assert!(sink2.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_state_file_reads_as_inactive() {
        let dir = home();
        assert!(!state_path(dir.path()).exists());
        assert!(!load(&state_path(dir.path())).active);
    }

    #[test]
    fn sanitize_detail_strips_newlines_and_caps_bytes() {
        let raw = "line one\nline two\r\n\tline three";
        let clean = sanitize_detail(raw);
        assert_eq!(clean, "line one line two line three");

        // CJK-safe byte cap: 200 CJK chars = 600 bytes, must land on a char
        // boundary at or below 240 bytes.
        let cjk = "認證失敗".repeat(100);
        let clean = sanitize_detail(&cjk);
        assert!(clean.len() <= DETAIL_MAX_BYTES);
        assert!(cjk.starts_with(&clean));
    }

    #[test]
    fn cause_phrase_maps_the_two_known_kinds() {
        assert_eq!(
            cause_phrase("claude CLI assistant error: oauth_org_not_allowed"),
            "Anthropic 回報此組織已停用 Claude Code 訂閱存取"
        );
        assert_eq!(
            cause_phrase("claude CLI assistant error: authentication_failed"),
            "token 無效或已過期"
        );
        // Not an auth failure at all ⇒ the sanitized raw text, never a
        // fabricated cause.
        assert_eq!(cause_phrase("some weird thing"), "some weird thing");
    }

    #[test]
    fn recovery_fast_path_throttles_only_when_not_maybe_active() {
        // Deliberately exercises the process-global atomics; restored at the
        // end so sibling tests in the same binary are unaffected.
        let saved_flag = MAYBE_ACTIVE.load(Ordering::Relaxed);
        let saved_ts = LAST_RECOVERY_CHECK_UNIX.load(Ordering::Relaxed);

        MAYBE_ACTIVE.store(true, Ordering::Relaxed);
        assert!(
            should_check_state(1_000_000),
            "known outage always re-reads"
        );
        assert!(should_check_state(1_000_001));

        MAYBE_ACTIVE.store(false, Ordering::Relaxed);
        LAST_RECOVERY_CHECK_UNIX.store(0, Ordering::Relaxed);
        assert!(should_check_state(1_000_000), "first check always reads");
        assert!(
            !should_check_state(1_000_030),
            "within the poll interval ⇒ no disk touch"
        );
        assert!(
            should_check_state(1_000_000 + RECOVERY_POLL_INTERVAL_SECS),
            "past the poll interval ⇒ re-read so cross-process outages recover"
        );

        MAYBE_ACTIVE.store(saved_flag, Ordering::Relaxed);
        LAST_RECOVERY_CHECK_UNIX.store(saved_ts, Ordering::Relaxed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = home();
        let sink = Arc::new(Mutex::new(Vec::new()));
        record_outage_with(dir.path(), "a", "authentication_failed", capture(sink)).await;
        let mode = std::fs::metadata(state_path(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "auth_outage.json must be owner-only, got {mode:o}"
        );
    }
}
