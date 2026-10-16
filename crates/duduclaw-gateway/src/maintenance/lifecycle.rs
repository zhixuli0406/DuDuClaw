//! Window lifecycle: enable (with the SSH key), disable, TTL sweep, boot
//! reconciliation, the supervisor's sysd poll and the status view.
//!
//! Authority: `commercial/docs/specs/DESIGN-maintenance-ssh-account-2026-10.md`
//! §4.1, §4.5, §6 (gateway side of P-3). The rules this file keeps:
//!
//! - One process-wide [`WINDOW_LOCK`] serialises enable, disable, the TTL
//!   sweep, reconciliation and the poll, so an `opening` row is only ever seen
//!   by the enable that wrote it (M2, T6).
//! - The window row is written as `opening` BEFORE sysd is asked to open, so a
//!   gateway killed mid-open leaves a row the next boot reconciles (L13).
//! - sysd answering `busy` never triggers a rollback stop (it could belong to
//!   another window); the row records `open_failed` and one reconciliation
//!   runs (M2 scenario B).
//! - On an appliance, a close that cannot reach sysd is a `close_error`, never
//!   a clean close (H1). Only off the appliance does "no sysd" mean "nothing
//!   to close".
//! - Strings from sysd are data: sanitised before they reach the audit file or
//!   the dashboard (L7).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use duduclaw_sysd::ssh_key::{self, ParsedKey};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use super::lockout::{REAUTH_LOCK_MINUTES, ReauthFailure};
use super::notify::{self, AdminNotifier, NoNotifier, UserDbAdminNotifier};
use super::sysd_port::{
    EVENT_TEXT_MAX_CHARS, MaintSysd, OpenReply, PROBLEM_TEXT_MAX_CHARS, StatusReply, SysdAccess,
    SysdCallError, SysdProvider, SystemSysdProvider, sanitize, sanitize_opt, valid_cursor,
};
use super::{
    MAX_TTL_HOURS, MaintenanceAuditLog, MaintenanceStore, MaintenanceWindow, NewWindow,
    REVOKE_REASON_ADMIN, SUB_CAPABILITY_SSH, WindowState, ssh_service_observed_active,
    validate_sub_capabilities,
};

/// Serialises every operation that changes a window or talks to sysd about
/// one (see the module doc).
static WINDOW_LOCK: Mutex<()> = Mutex::const_new(());

/// Boot reconciliation retry interval and deadline (design R3, R2-L6: sysd is
/// `Type=simple`, its socket may not be bound yet when the gateway starts).
pub const RECONCILE_RETRY: Duration = Duration::from_secs(2);
pub const RECONCILE_DEADLINE: Duration = Duration::from_secs(60);
/// Upper bound for sending one notice to every Admin.
const NOTICE_TIMEOUT: Duration = Duration::from_secs(30);
/// Supervisor period (TTL sweep, reminder, sysd poll and audit mirror).
pub const SUPERVISOR_TICK: Duration = Duration::from_secs(30);

/// Everything the lifecycle needs from its surroundings; tests swap the sysd
/// provider and the notifier and shorten the timings.
pub struct MaintEnv {
    pub home_dir: PathBuf,
    pub sysd: Arc<dyn SysdProvider>,
    pub notifier: Arc<dyn AdminNotifier>,
    pub reconcile_retry: Duration,
    pub reconcile_deadline: Duration,
    pub tick_interval: Duration,
}

impl MaintEnv {
    /// The production environment: the real sysd socket, and notices to the
    /// Admins of `user_db` (none when the user system is not up).
    pub fn system(home_dir: &Path, user_db: Option<Arc<duduclaw_auth::UserDb>>) -> Self {
        let notifier: Arc<dyn AdminNotifier> = match user_db {
            Some(db) => Arc::new(UserDbAdminNotifier::new(home_dir.to_path_buf(), db)),
            None => Arc::new(NoNotifier),
        };
        Self {
            home_dir: home_dir.to_path_buf(),
            sysd: Arc::new(SystemSysdProvider),
            notifier,
            reconcile_retry: RECONCILE_RETRY,
            reconcile_deadline: RECONCILE_DEADLINE,
            tick_interval: SUPERVISOR_TICK,
        }
    }

    fn store(&self) -> Result<MaintenanceStore, MaintError> {
        MaintenanceStore::open(&self.home_dir).map_err(MaintError::internal)
    }

    fn audit(&self) -> Result<MaintenanceAuditLog, MaintError> {
        MaintenanceAuditLog::open(&self.home_dir)
            .map_err(|e| MaintError::internal(format!("open maintenance audit log: {e}")))
    }

    /// Send collected notices, each bounded so a hung channel cannot stall
    /// the supervisor's TTL sweep or an RPC reply.
    async fn send(&self, notices: Vec<String>) {
        for text in notices {
            if tokio::time::timeout(NOTICE_TIMEOUT, self.notifier.notify_admins(&text))
                .await
                .is_err()
            {
                warn!("maintenance: an admin notice timed out");
            }
        }
    }
}

// ── Errors with stable codes (the dashboard branches on `code`) ──

/// Stable `code` values of a refused `maintenance.enable` / `disable`.
pub mod error_code {
    pub const CONFIRM_TEXT_MISMATCH: &str = "confirm_text_mismatch";
    pub const USER_SYSTEM_UNAVAILABLE: &str = "user_system_unavailable";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const SSH_KEY_REQUIRED: &str = "ssh_key_required";
    pub const SSH_KEY_UNEXPECTED: &str = "ssh_key_unexpected";
    /// The key failed `ssh_key::parse`; `reason` carries its code.
    pub const SSH_KEY_INVALID: &str = "ssh_key_invalid";
    /// Wrong password; `remaining_attempts` before the lock.
    pub const REAUTH_FAILED: &str = "reauth_failed";
    /// Locked; `retry_after_seconds`.
    pub const REAUTH_LOCKED: &str = "reauth_locked";
    pub const ALREADY_ACTIVE: &str = "already_active";
    /// This device cannot open a keyed SSH window (old OS image, old sysd,
    /// partial OS update, not an appliance). Never answered with a port-only
    /// open.
    pub const SSH_UNSUPPORTED: &str = "ssh_unsupported";
    pub const SSH_UNREACHABLE: &str = "ssh_unreachable";
    /// sysd still holds a key, is finishing a cleanup, or still owes its
    /// audit file a record; `reason` is `key_installed`, `cleanup_pending`,
    /// `audit_pending` or `unknown`.
    pub const SSH_BUSY: &str = "ssh_busy";
    /// sysd's own open limit (at least 5 s apart, at most 12 per hour).
    pub const SSH_RATE_LIMITED: &str = "ssh_rate_limited";
    pub const SSH_OPEN_FAILED: &str = "ssh_open_failed";
    pub const INTERNAL: &str = "internal";
}

/// A refusal with a stable code, a zh-TW sentence, and optional fields.
#[derive(Debug, Clone, PartialEq)]
pub struct MaintError {
    pub code: &'static str,
    pub message: String,
    pub extra: Map<String, Value>,
}

impl MaintError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            extra: Map::new(),
        }
    }

    fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }

    fn internal(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        // Logged here, not returned: the text can carry paths or SQLite errors.
        error!(error = %detail, "maintenance: internal error");
        Self::new(error_code::INTERNAL, "維修模式暫時無法操作，請稍後再試。")
    }

    /// `{"code", "message", ...extra}` for the RPC error frame.
    pub fn to_json(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("code".to_string(), json!(self.code));
        obj.insert("message".to_string(), json!(self.message));
        Value::Object(obj)
    }
}

impl std::fmt::Display for MaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

fn locked_error(retry_after_seconds: i64) -> MaintError {
    let minutes = (retry_after_seconds.max(1) + 59) / 60;
    MaintError::new(
        error_code::REAUTH_LOCKED,
        format!("密碼錯誤次數過多，{minutes} 分鐘後才能再開啟維修模式。"),
    )
    .with("retry_after_seconds", json!(retry_after_seconds.max(1)))
}

fn key_error(code: &str) -> MaintError {
    MaintError::new(
        error_code::SSH_KEY_INVALID,
        "這把公開金鑰無法使用，請確認貼上的是一整行公開金鑰。",
    )
    .with("reason", json!(code))
}

fn unsupported_error(reason: &str) -> MaintError {
    MaintError::new(
        error_code::SSH_UNSUPPORTED,
        "這台裝置的系統版本還不支援遠端登入，請先更新系統。",
    )
    .with("reason", json!(reason))
}

fn unreachable_error() -> MaintError {
    MaintError::new(
        error_code::SSH_UNREACHABLE,
        "裝置的系統服務沒有回應，無法開啟遠端登入，請稍後再試。",
    )
}

// ── Re-authentication ─────────────────────────────────────

/// Checks the caller's own password (`UserDb::verify_password` in
/// production; a counting fake in tests).
pub trait ReauthVerifier: Send + Sync {
    fn verify(&self, email: &str, password: &str) -> bool;
}

impl ReauthVerifier for duduclaw_auth::UserDb {
    fn verify(&self, email: &str, password: &str) -> bool {
        self.verify_password(email, password).is_ok()
    }
}

/// Lock check first, `verify` only when not locked (R2-L11).
async fn check_reauth(
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    req: &EnableRequest<'_>,
    verifier: &dyn ReauthVerifier,
    notices: &mut Vec<String>,
) -> Result<(), MaintError> {
    let now = Utc::now();
    match store.reauth_locked_until(req.user_id, now).await {
        Err(e) => return Err(MaintError::internal(e)),
        Ok(Some(until)) => return Err(locked_error((until - now).num_seconds())),
        Ok(None) => {}
    }
    if verifier.verify(req.email, req.reauth_password) {
        if let Err(e) = store.clear_reauth_failures(req.user_id).await {
            warn!(error = %e, "maintenance: could not reset the re-auth failure count");
        }
        return Ok(());
    }
    match store
        .record_reauth_failure(req.user_id, now)
        .await
        .map_err(MaintError::internal)?
    {
        ReauthFailure::Remaining(left) => {
            audit.record(
                "enable_reauth_failed",
                json!({ "user_id": req.user_id, "email": req.email, "remaining_attempts": left }),
            );
            Err(MaintError::new(
                error_code::REAUTH_FAILED,
                format!(
                    "密碼不正確。再錯 {left} 次，{REAUTH_LOCK_MINUTES} 分鐘內將無法開啟維修模式。"
                ),
            )
            .with("remaining_attempts", json!(left)))
        }
        ReauthFailure::LockedNow(until) => {
            audit.record("reauth_locked", json!({ "user_id": req.user_id, "email": req.email, "locked_until": until.to_rfc3339() }));
            notices.push(notify::reauth_locked_text(req.email, REAUTH_LOCK_MINUTES));
            Err(locked_error((until - now).num_seconds()))
        }
    }
}

// ── Enable ──────────────────────────────────────────────────

/// `maintenance.enable` arguments after the RPC layer checked the confirm text.
pub struct EnableRequest<'a> {
    pub user_id: &'a str,
    pub email: &'a str,
    pub ttl_hours: i64,
    pub sub_capabilities: &'a [String],
    /// Required with the `ssh` sub-capability, refused without it.
    pub ssh_public_key: Option<&'a str>,
    pub reauth_password: &'a str,
}

/// §4.1. Validation, then under [`WINDOW_LOCK`]: lock check, password, key
/// parse, `opening` row, sysd open, `open` / `open_failed`. Notices are sent
/// after the lock is released.
pub async fn enable(
    env: &MaintEnv,
    req: &EnableRequest<'_>,
    verifier: &dyn ReauthVerifier,
) -> Result<MaintenanceWindow, MaintError> {
    let sub_caps = validate_sub_capabilities(req.sub_capabilities).map_err(|e| {
        MaintError::new(error_code::INVALID_REQUEST, "請至少選一項維修功能。")
            .with("detail", json!(e))
    })?;
    if !(1..=MAX_TTL_HOURS).contains(&req.ttl_hours) {
        return Err(MaintError::new(
            error_code::INVALID_REQUEST,
            format!("維修時間要在 1 到 {MAX_TTL_HOURS} 小時之間。"),
        ));
    }
    let wants_ssh = sub_caps.iter().any(|c| c == SUB_CAPABILITY_SSH);
    let key_text = req.ssh_public_key.map(str::trim).filter(|k| !k.is_empty());
    match (wants_ssh, key_text.is_some()) {
        (true, false) => {
            return Err(MaintError::new(
                error_code::SSH_KEY_REQUIRED,
                "請貼上技術支援人員的 SSH 公開金鑰。",
            ));
        }
        (false, true) => {
            return Err(MaintError::new(
                error_code::SSH_KEY_UNEXPECTED,
                "沒有勾選遠端技術支援連線時不需要公開金鑰。",
            ));
        }
        _ => {}
    }
    let store = env.store()?;
    let audit = env.audit()?;

    let mut notices = Vec::new();
    let result = {
        let _guard = WINDOW_LOCK.lock().await;
        enable_locked(
            env,
            &store,
            &audit,
            req,
            &sub_caps,
            key_text,
            verifier,
            &mut notices,
        )
        .await
    };
    env.send(notices).await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn enable_locked(
    env: &MaintEnv,
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    req: &EnableRequest<'_>,
    sub_caps: &[String],
    key_text: Option<&str>,
    verifier: &dyn ReauthVerifier,
    notices: &mut Vec<String>,
) -> Result<MaintenanceWindow, MaintError> {
    check_reauth(store, audit, req, verifier, notices).await?;

    let key = match key_text {
        Some(text) => match ssh_key::parse(text) {
            Ok(k) => Some(k),
            Err(e) => {
                // Only the code and the length, never the input (T18).
                audit.record(
                    "enable_failed",
                    json!({ "enabled_by": req.user_id, "code": error_code::SSH_KEY_INVALID, "reason": e.code(), "input_len": text.len() }),
                );
                return Err(key_error(e.code()));
            }
        },
        None => None,
    };

    if store
        .current_row()
        .await
        .map_err(MaintError::internal)?
        .is_some()
    {
        return Err(MaintError::new(
            error_code::ALREADY_ACTIVE,
            "維修模式已經開啟。",
        ));
    }

    let Some(key) = key else {
        let window = store
            .insert(NewWindow {
                enabled_by: req.user_id,
                enabled_by_email: req.email,
                ttl_hours: req.ttl_hours,
                sub_capabilities: sub_caps,
                state: WindowState::Open,
                ssh_key: None,
            })
            .await
            .map_err(MaintError::internal)?;
        record_enabled(audit, &window);
        return Ok(window);
    };

    let sysd = match env.sysd.access() {
        SysdAccess::Available(s) => s,
        SysdAccess::OffAppliance => {
            record_enable_failed(audit, req, error_code::SSH_UNSUPPORTED, "not an appliance");
            return Err(unsupported_error("off_appliance"));
        }
        SysdAccess::Unreachable(e) => {
            record_enable_failed(audit, req, error_code::SSH_UNREACHABLE, &e);
            return Err(unreachable_error());
        }
    };

    let row = store
        .insert(NewWindow {
            enabled_by: req.user_id,
            enabled_by_email: req.email,
            ttl_hours: req.ttl_hours,
            sub_capabilities: sub_caps,
            state: WindowState::Opening,
            ssh_key: Some(&key),
        })
        .await
        .map_err(MaintError::internal)?;
    let ttl_seconds = u64::try_from(row.ttl_seconds).unwrap_or(0);
    let reply = sysd.ssh_maint_open(&key.wire(), ttl_seconds, &row.id).await;

    match classify_open(reply, &key, &row.id) {
        OpenOutcome::Opened(reply) => {
            let host_fps: Vec<String> = reply
                .host_key_fingerprints
                .iter()
                .map(|f| sanitize(f, EVENT_TEXT_MAX_CHARS))
                .filter(|f| !f.is_empty())
                .collect();
            match store.mark_open(&row.id, &host_fps).await {
                Ok(true) => {}
                other => {
                    let detail = match other {
                        Err(e) => e,
                        _ => "the window row was no longer opening".to_string(),
                    };
                    let rollback = stop_ssh(sysd.as_ref()).await;
                    fail_open(
                        store,
                        audit,
                        notices,
                        req,
                        &row,
                        error_code::SSH_OPEN_FAILED,
                        &detail,
                        rollback,
                    )
                    .await;
                    return Err(MaintError::internal(detail));
                }
            }
            let window = MaintenanceWindow {
                state: WindowState::Open,
                host_key_fingerprints: host_fps,
                ..row
            };
            record_enabled(audit, &window);
            audit.record(
                "ssh_key_installed",
                json!({
                    "window_id": window.id,
                    "key_type": window.ssh_key_type,
                    "key_fingerprint": window.ssh_key_fingerprint,
                    "key_label": window.ssh_key_label,
                    "host_key_fingerprints": window.host_key_fingerprints,
                }),
            );
            notices.push(notify::opened_text(
                req.email,
                &window.expires_at,
                window.ssh_key_fingerprint.as_deref().unwrap_or_default(),
                window.ssh_key_label.as_deref(),
                &window.host_key_fingerprints,
            ));
            info!(window_id = %window.id, fingerprint = ?window.ssh_key_fingerprint, "maintenance mode ENABLED with an SSH key");
            Ok(window)
        }
        OpenOutcome::Busy(message) => {
            // Not ours to roll back: sysd holds some other key or is still
            // cleaning up. Record the attempt and reconcile once (§4.1).
            fail_open(
                store,
                audit,
                notices,
                req,
                &row,
                error_code::SSH_BUSY,
                &message,
                None,
            )
            .await;
            let seen = reconcile_with(store, audit, sysd.as_ref(), "busy", notices).await;
            let reason = match &seen {
                Some(st) if st.has_window() => "key_installed",
                Some(st) if st.pending_cleanup => "cleanup_pending",
                Some(st) if st.pending_audit => "audit_pending",
                _ => "unknown",
            };
            Err(MaintError::new(
                error_code::SSH_BUSY,
                "裝置還在收回上一次的遠端登入，請一分鐘後再試。",
            )
            .with("reason", json!(reason)))
        }
        OpenOutcome::Refused(err, detail) => {
            fail_open(store, audit, notices, req, &row, err.code, &detail, None).await;
            Err(err)
        }
        OpenOutcome::Unknown(detail) => {
            let rollback = stop_ssh(sysd.as_ref()).await;
            fail_open(
                store,
                audit,
                notices,
                req,
                &row,
                error_code::SSH_OPEN_FAILED,
                &detail,
                rollback,
            )
            .await;
            Err(MaintError::new(
                error_code::SSH_OPEN_FAILED,
                "無法開啟遠端登入，已經收回這次的設定。",
            )
            .with("detail", json!(sanitize(&detail, PROBLEM_TEXT_MAX_CHARS))))
        }
    }
}

enum OpenOutcome {
    Opened(OpenReply),
    /// sysd answered `busy`.
    Busy(String),
    /// A definite refusal: sysd did nothing (verified before any write, or
    /// never reached).
    Refused(MaintError, String),
    /// The open may have partly happened: roll back with a stop.
    Unknown(String),
}

fn classify_open(
    reply: Result<duduclaw_sysd::SysdOpOutput, SysdCallError>,
    key: &ParsedKey,
    window_id: &str,
) -> OpenOutcome {
    match reply {
        Ok(out) if out.success => match serde_json::from_str::<OpenReply>(out.stdout.trim()) {
            Ok(r)
                if r.key_fingerprint == key.fingerprint
                    && r.key_type == key.key_type.name()
                    && r.window_id.as_deref().is_none_or(|w| w == window_id) =>
            {
                OpenOutcome::Opened(r)
            }
            Ok(_) => OpenOutcome::Unknown(
                "sysd reported a different key or window than requested".to_string(),
            ),
            Err(e) => OpenOutcome::Unknown(format!("unreadable ssh_maint_open reply: {e}")),
        },
        Ok(out) => OpenOutcome::Unknown(format!(
            "ssh_maint_open reported failure: {}",
            out.stderr.trim()
        )),
        Err(SysdCallError::UnknownVerb) => OpenOutcome::Refused(
            unsupported_error("sysd_too_old"),
            "sysd does not know ssh_maint_open".to_string(),
        ),
        Err(SysdCallError::Unreachable(e)) => OpenOutcome::Refused(unreachable_error(), e),
        Err(SysdCallError::Rejected { kind, message }) => match kind.as_str() {
            "busy" => OpenOutcome::Busy(message),
            // Checked before anything is written.
            "rate_limited" => OpenOutcome::Refused(
                MaintError::new(
                    error_code::SSH_RATE_LIMITED,
                    "遠端登入開啟得太頻繁，請稍候幾分鐘再試。",
                ),
                message,
            ),
            "unsupported" => OpenOutcome::Refused(unsupported_error("image_not_ready"), message),
            "bad_request" => match message.strip_prefix("ssh_key:") {
                Some(code) => OpenOutcome::Refused(key_error(&sanitize(code, 32)), message.clone()),
                None => OpenOutcome::Refused(
                    MaintError::new(
                        error_code::SSH_OPEN_FAILED,
                        "裝置的系統服務拒絕了這次開啟。",
                    )
                    .with("detail", json!(sanitize(&message, PROBLEM_TEXT_MAX_CHARS))),
                    message,
                ),
            },
            "unauthorized" => OpenOutcome::Refused(
                MaintError::new(
                    error_code::SSH_OPEN_FAILED,
                    "裝置的系統服務拒絕了這次開啟。",
                ),
                message,
            ),
            _ => OpenOutcome::Unknown(format!("sysd refused ({kind}): {message}")),
        },
        Err(e @ SysdCallError::Transport(_)) => OpenOutcome::Unknown(e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn fail_open(
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    notices: &mut Vec<String>,
    req: &EnableRequest<'_>,
    row: &MaintenanceWindow,
    code: &str,
    detail: &str,
    rollback_error: Option<String>,
) {
    let detail = sanitize(detail, PROBLEM_TEXT_MAX_CHARS);
    if let Err(e) = store
        .mark_open_failed(&row.id, &detail, rollback_error.as_deref())
        .await
    {
        error!(error = %e, window_id = %row.id, "maintenance: could not record open_failed");
    }
    if rollback_error.is_some() {
        notices.push(notify::close_unconfirmed_text(&row.id));
    }
    audit.record(
        "enable_failed",
        json!({
            "window_id": row.id,
            "enabled_by": req.user_id,
            "enabled_by_email": req.email,
            "sub_capabilities": row.sub_capabilities,
            "key_fingerprint": row.ssh_key_fingerprint,
            "code": code,
            "error": detail,
            "rollback_error": rollback_error,
        }),
    );
}

fn record_enable_failed(
    audit: &MaintenanceAuditLog,
    req: &EnableRequest<'_>,
    code: &str,
    detail: &str,
) {
    audit.record(
        "enable_failed",
        json!({ "enabled_by": req.user_id, "enabled_by_email": req.email, "code": code, "error": sanitize(detail, PROBLEM_TEXT_MAX_CHARS) }),
    );
}

fn record_enabled(audit: &MaintenanceAuditLog, w: &MaintenanceWindow) {
    audit.record(
        "enabled",
        json!({
            "window_id": w.id,
            "enabled_by": w.enabled_by,
            "enabled_by_email": w.enabled_by_email,
            "ttl_hours": w.ttl_seconds / 3600,
            "sub_capabilities": w.sub_capabilities,
            "key_fingerprint": w.ssh_key_fingerprint,
            "key_label": w.ssh_key_label,
            "host_key_fingerprints": w.host_key_fingerprints,
        }),
    );
}

// ── Close ───────────────────────────────────────────────────

/// `ssh_service_stop`; `None` only when sysd confirmed everything closed.
async fn stop_ssh(sysd: &dyn MaintSysd) -> Option<String> {
    match sysd.ssh_service_stop().await {
        Ok(out) if out.success => None,
        Ok(out) => Some(format!(
            "ssh_service_stop reported problems: {}",
            sanitize(&out.stderr, PROBLEM_TEXT_MAX_CHARS)
        )),
        Err(e) => Some(sanitize(&e.to_string(), PROBLEM_TEXT_MAX_CHARS)),
    }
}

/// The close side of a window's sub-capabilities. `show_details` needs no
/// system action. For `ssh`: off the appliance there is nothing to close; on
/// an appliance an unreachable sysd is an unconfirmed close (H1).
pub async fn run_close_actions(env: &MaintEnv, window: &MaintenanceWindow) -> Option<String> {
    if !window
        .sub_capabilities
        .iter()
        .any(|c| c == SUB_CAPABILITY_SSH)
    {
        return None;
    }
    match env.sysd.access() {
        SysdAccess::OffAppliance => None,
        SysdAccess::Unreachable(e) => Some(format!(
            "sysd unreachable, the SSH close is not confirmed: {e}"
        )),
        SysdAccess::Available(sysd) => stop_ssh(sysd.as_ref()).await,
    }
}

/// Close actions, close-error bookkeeping and the audit line for a window the
/// store just revoked.
async fn close_window(
    env: &MaintEnv,
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    window: &MaintenanceWindow,
    kind: &str,
    extra: Value,
    notices: &mut Vec<String>,
) -> Option<String> {
    let close_err = run_close_actions(env, window).await;
    match &close_err {
        Some(err) => {
            store.set_close_error(&window.id, err).await;
            error!(window_id = %window.id, error = %err, kind, "maintenance: close not confirmed");
            notices.push(notify::close_unconfirmed_text(&window.id));
        }
        None if window.ssh_key_fingerprint.is_some() => {
            audit.record(
                "ssh_key_removed",
                json!({ "window_id": window.id, "key_fingerprint": window.ssh_key_fingerprint }),
            );
        }
        None => {}
    }
    let mut fields = match extra {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    fields.insert("window_id".to_string(), json!(window.id));
    fields.insert(
        "sub_capabilities".to_string(),
        json!(window.sub_capabilities),
    );
    fields.insert("close_error".to_string(), json!(close_err));
    audit.record(kind, Value::Object(fields));
    close_err
}

/// `maintenance.disable` — an admin turning it off. `reason` is audited as a
/// note; `revoke_reason` stays [`REVOKE_REASON_ADMIN`].
pub async fn disable(
    env: &MaintEnv,
    disabled_by: &str,
    reason: Option<&str>,
) -> Result<Option<MaintenanceWindow>, String> {
    let store = MaintenanceStore::open(&env.home_dir)?;
    let audit = MaintenanceAuditLog::open(&env.home_dir)
        .map_err(|e| format!("open maintenance audit log: {e}"))?;
    let mut notices = Vec::new();
    let window = {
        let _guard = WINDOW_LOCK.lock().await;
        let Some(window) = store.disable().await? else {
            return Ok(None);
        };
        let extra = json!({ "disabled_by": disabled_by, "revoke_reason": REVOKE_REASON_ADMIN, "note": reason });
        let close_err = close_window(
            env,
            &store,
            &audit,
            &window,
            "disabled",
            extra,
            &mut notices,
        )
        .await;
        let _ = store.forget_ssh_events_except(None).await;
        info!(window_id = %window.id, %disabled_by, close_confirmed = close_err.is_none(), "maintenance mode disabled by admin");
        MaintenanceWindow {
            close_error: close_err,
            ..window
        }
    };
    env.send(notices).await;
    Ok(Some(window))
}

/// TTL sweep and the 2/3 reminder (supervisor step). No-op when nothing is due.
pub async fn sweep_expired(env: &MaintEnv) {
    let (store, audit) = match (env.store(), env.audit()) {
        (Ok(s), Ok(a)) => (s, a),
        (Err(e), _) | (_, Err(e)) => {
            warn!(error = %e, "maintenance: cannot open store/audit for the TTL sweep");
            return;
        }
    };
    let mut notices = Vec::new();
    {
        let _guard = WINDOW_LOCK.lock().await;
        if let Ok(Some(active)) = store.active_window().await {
            if active.reminder_due(Utc::now()) {
                let _ = store.mark_reminded(&active.id).await;
                audit.record(
                    "reminder_sent",
                    json!({ "window_id": active.id, "remaining_seconds": active.remaining_seconds(Utc::now()) }),
                );
            }
        }
        match store.expire_stale().await {
            Ok(Some(swept)) => {
                close_window(
                    env,
                    &store,
                    &audit,
                    &swept,
                    "ttl_expired",
                    json!({}),
                    &mut notices,
                )
                .await;
                let _ = store.forget_ssh_events_except(None).await;
                info!(window_id = %swept.id, "maintenance mode auto-closed: TTL expired");
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "maintenance: expire_stale failed"),
        }
    }
    env.send(notices).await;
}

// ── Boot reconciliation and the sysd poll ─────────────────

/// Synchronous half of the boot path: revoke whatever the ledger still has
/// open (`gateway_restart`) before the dashboard can see it. Returns the
/// revoked rows; their close actions run in [`boot_reconcile`] once sysd
/// answers.
pub async fn revoke_on_boot(home_dir: &Path) -> Vec<MaintenanceWindow> {
    let store = match MaintenanceStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "maintenance: store open failed at boot — cannot verify no window is open");
            return Vec::new();
        }
    };
    let _guard = WINDOW_LOCK.lock().await;
    match store.reassert_closed_on_boot().await {
        Ok(Some(w)) => {
            warn!(window_id = %w.id, "maintenance mode force-closed: gateway restarted while a window was open");
            vec![w]
        }
        Ok(None) => Vec::new(),
        Err(e) => {
            error!(error = %e, "maintenance: reassert_closed_on_boot query failed");
            Vec::new()
        }
    }
}

/// What the boot wait found.
enum Reach {
    OffAppliance,
    Status(Arc<dyn MaintSysd>, StatusReply),
    /// sysd answers but predates `ssh_maint_status`: it cannot hold a keyed
    /// window, and the old `ssh_service_stop` still closes a legacy one.
    OldSysd,
    /// No usable answer within the deadline, or a refusal.
    Unconfirmed(String),
}

/// Retry `ssh_maint_status` every `reconcile_retry` until it answers or
/// `reconcile_deadline` passes (R3, R2-L6). Holds no lock while waiting.
async fn wait_for_sysd(env: &MaintEnv) -> Reach {
    let start = Instant::now();
    loop {
        let last = match env.sysd.access() {
            SysdAccess::OffAppliance => return Reach::OffAppliance,
            SysdAccess::Unreachable(e) => e,
            SysdAccess::Available(sysd) => match sysd.ssh_maint_status().await {
                Ok(out) if out.success => {
                    return match StatusReply::parse(&out.stdout) {
                        Ok(st) => Reach::Status(sysd, st),
                        Err(e) => Reach::Unconfirmed(e),
                    };
                }
                Ok(out) => {
                    return Reach::Unconfirmed(format!(
                        "ssh_maint_status failed: {}",
                        out.stderr.trim()
                    ));
                }
                Err(SysdCallError::UnknownVerb) => return Reach::OldSysd,
                Err(e @ SysdCallError::Rejected { .. }) => {
                    return Reach::Unconfirmed(e.to_string());
                }
                Err(e) => e.to_string(),
            },
        };
        if start.elapsed() >= env.reconcile_deadline {
            return Reach::Unconfirmed(format!(
                "sysd did not answer within {} s: {}",
                env.reconcile_deadline.as_secs(),
                sanitize(&last, PROBLEM_TEXT_MAX_CHARS)
            ));
        }
        tokio::time::sleep(env.reconcile_retry).await;
    }
}

/// R3: after the boot wait, close an orphan key (sysd holds a key that no
/// completed window of this gateway explains), then run the close actions of
/// the rows [`revoke_on_boot`] revoked. Returns [`SysdSupport::Old`] when sysd
/// predates the maintenance verbs, so the supervisor stops polling it.
pub async fn boot_reconcile(env: &MaintEnv, revoked: Vec<MaintenanceWindow>) -> SysdSupport {
    let reach = wait_for_sysd(env).await;
    let support = if matches!(reach, Reach::OldSysd) {
        SysdSupport::Old
    } else {
        SysdSupport::Current
    };
    let (store, audit) = match (env.store(), env.audit()) {
        (Ok(s), Ok(a)) => (s, a),
        (Err(e), _) | (_, Err(e)) => {
            error!(error = %e, "maintenance: cannot open store/audit for boot reconciliation");
            return support;
        }
    };
    let mut notices = Vec::new();
    {
        let _guard = WINDOW_LOCK.lock().await;
        match &reach {
            Reach::Status(sysd, st) => {
                // An `opening` row never completed: its key counts as an
                // orphan (L13). Only a row that reached `open` explains it.
                let explained = revoked.iter().any(|w| {
                    w.state == WindowState::Open && st.window_id.as_deref() == Some(w.id.as_str())
                });
                reconcile_reply(
                    &store,
                    &audit,
                    sysd.as_ref(),
                    st,
                    explained,
                    "boot",
                    &mut notices,
                )
                .await;
            }
            Reach::Unconfirmed(e) => {
                audit.record("ssh_reconcile_unconfirmed", json!({ "error": e }));
                error!(error = %e, "maintenance: boot reconciliation could not reach sysd");
                if !revoked
                    .iter()
                    .any(|w| w.sub_capabilities.iter().any(|c| c == SUB_CAPABILITY_SSH))
                {
                    notices.push(notify::reconcile_unreachable_text());
                }
            }
            Reach::OffAppliance | Reach::OldSysd => {}
        }
        for window in &revoked {
            close_window(
                env,
                &store,
                &audit,
                window,
                "gateway_restart_revoked",
                json!({}),
                &mut notices,
            )
            .await;
        }
        if !revoked.is_empty() {
            let _ = store.forget_ssh_events_except(None).await;
        }
    }
    env.send(notices).await;
    support
}

/// Whether sysd knows `ssh_maint_status`. An old sysd (OS image before the
/// maintenance account) cannot hold a keyed window, so polling it every 30 s
/// would only fill its log with refused calls; the gateway and sysd ship in
/// one image, so this only changes with an OS update, which restarts the
/// gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysdSupport {
    Current,
    Old,
}

/// One reconciliation against a live sysd (after a `busy` open). Caller holds
/// [`WINDOW_LOCK`]. Returns what sysd reported, when it answered.
async fn reconcile_with(
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    sysd: &dyn MaintSysd,
    context: &str,
    notices: &mut Vec<String>,
) -> Option<StatusReply> {
    let st = match sysd.ssh_maint_status().await {
        Ok(out) if out.success => match StatusReply::parse(&out.stdout) {
            Ok(st) => st,
            Err(e) => {
                warn!(error = %e, context, "maintenance: reconciliation got an unreadable status");
                return None;
            }
        },
        Ok(out) => {
            warn!(stderr = %out.stderr.trim(), context, "maintenance: reconciliation status failed");
            return None;
        }
        Err(e) => {
            warn!(error = %e, context, "maintenance: reconciliation could not read sysd status");
            return None;
        }
    };
    let explained = explained_by_open_row(store, &st).await;
    reconcile_reply(store, audit, sysd, &st, explained, context, notices).await;
    Some(st)
}

/// The unrevoked row is `open` and is the window sysd holds.
async fn explained_by_open_row(store: &MaintenanceStore, st: &StatusReply) -> bool {
    match store.current_row().await {
        Ok(Some(w)) => {
            w.state == WindowState::Open && st.window_id.as_deref() == Some(w.id.as_str())
        }
        Ok(None) => false,
        // Cannot read the ledger: do not close a key that might be legitimate
        // on that basis; the next poll retries.
        Err(_) => true,
    }
}

/// Mirror events, then close an unexplained key.
async fn reconcile_reply(
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    sysd: &dyn MaintSysd,
    st: &StatusReply,
    explained: bool,
    context: &str,
    notices: &mut Vec<String>,
) {
    mirror_events(store, audit, st).await;
    if !st.has_window() || explained {
        return;
    }
    let close_err = stop_ssh(sysd).await;
    audit.record(
        "ssh_orphan_closed",
        json!({
            "context": context,
            "sysd_window_id": sanitize_opt(&st.window_id),
            "key_fingerprint": sanitize_opt(&st.key_fingerprint),
            "close_error": close_err,
        }),
    );
    warn!(
        context,
        close_confirmed = close_err.is_none(),
        "maintenance: closed an SSH key with no open window"
    );
    notices.push(notify::orphan_closed_text(close_err.is_none()));
}

/// Supervisor step: ask sysd, mirror its events, close an orphan key, and
/// revoke a window the OS already closed (sysd deadline, PID 1 timer, sysd
/// restart cleanup).
pub async fn poll_sysd(env: &MaintEnv) -> SysdSupport {
    let SysdAccess::Available(sysd) = env.sysd.access() else {
        return SysdSupport::Current;
    };
    let (store, audit) = match (env.store(), env.audit()) {
        (Ok(s), Ok(a)) => (s, a),
        (Err(e), _) | (_, Err(e)) => {
            warn!(error = %e, "maintenance: cannot open store/audit for the sysd poll");
            return SysdSupport::Current;
        }
    };
    let mut notices = Vec::new();
    {
        // The status call runs under the lock: an answer taken before an
        // enable finished could otherwise revoke the window it just opened.
        let _guard = WINDOW_LOCK.lock().await;
        let st = match sysd.ssh_maint_status().await {
            Ok(out) if out.success => match StatusReply::parse(&out.stdout) {
                Ok(st) => st,
                Err(e) => {
                    warn!(error = %e, "maintenance: unreadable ssh_maint_status");
                    return SysdSupport::Current;
                }
            },
            Ok(out) => {
                warn!(stderr = %out.stderr.trim(), "maintenance: ssh_maint_status failed");
                return SysdSupport::Current;
            }
            Err(SysdCallError::UnknownVerb) => return SysdSupport::Old,
            Err(e) => {
                warn!(error = %e, "maintenance: ssh_maint_status unavailable");
                return SysdSupport::Current;
            }
        };
        let explained = explained_by_open_row(&store, &st).await;
        reconcile_reply(
            &store,
            &audit,
            sysd.as_ref(),
            &st,
            explained,
            "poll",
            &mut notices,
        )
        .await;
        if st.definitely_closed() {
            revoke_if_closed_by_os(&store, &audit, sysd.as_ref(), &st, &mut notices).await;
        }
    }
    env.send(notices).await;
    SysdSupport::Current
}

/// The key is gone on the sysd side. When sysd's own close left something
/// behind (`pending_cleanup`, the socket or a session still up), the gateway
/// sends its own stop first and keeps a failed one as `close_error`, so the
/// ledger never records a clean close the device did not confirm.
async fn revoke_if_closed_by_os(
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    sysd: &dyn MaintSysd,
    st: &StatusReply,
    notices: &mut Vec<String>,
) {
    let Ok(Some(row)) = store.current_row().await else {
        return;
    };
    if row.state != WindowState::Open
        || !row.sub_capabilities.iter().any(|c| c == SUB_CAPABILITY_SSH)
    {
        return;
    }
    // A legacy window (opened by an older gateway with the port-only verb)
    // has no key; sysd reporting "no key" says nothing about it.
    if row.ssh_key_fingerprint.is_none() {
        return;
    }
    let clean = !st.pending_cleanup && st.observed_active() == Some(false);
    let close_err = if clean { None } else { stop_ssh(sysd).await };
    match store.revoke_closed_by_os(&row.id).await {
        Ok(Some(w)) => {
            if let Some(err) = &close_err {
                store.set_close_error(&w.id, err).await;
                notices.push(notify::close_unconfirmed_text(&w.id));
            }
            audit.record(
                "ssh_closed_by_os",
                json!({ "window_id": w.id, "key_fingerprint": w.ssh_key_fingerprint, "os_close_clean": clean, "close_error": close_err }),
            );
            let _ = store.forget_ssh_events_except(None).await;
            info!(window_id = %w.id, clean, "maintenance: the OS closed the SSH window; ledger updated");
        }
        Ok(None) => {}
        Err(e) => warn!(error = %e, "maintenance: could not record closed_by_os"),
    }
}

/// Mirror new sysd events into `maintenance_audit.jsonl`, deduplicated by
/// journal cursor per window. Every string is sanitised again here.
async fn mirror_events(store: &MaintenanceStore, audit: &MaintenanceAuditLog, st: &StatusReply) {
    let Some(window_id) = st
        .window_id
        .as_deref()
        .map(|w| sanitize(w, 64))
        .filter(|w| !w.is_empty())
    else {
        return;
    };
    let _ = store.forget_ssh_events_except(Some(&window_id)).await;
    let mut failures: BTreeMap<String, u64> = BTreeMap::new();
    for ev in &st.events {
        if !valid_cursor(&ev.cursor) {
            continue;
        }
        match store.mark_ssh_event_seen(&window_id, &ev.cursor).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                warn!(error = %e, "maintenance: event dedup unavailable; mirror skipped this round");
                return;
            }
        }
        let ts = ev.realtime_usec / 1_000_000;
        match ev.kind.as_str() {
            "login" => audit.record(
                "ssh_session_started",
                json!({ "window_id": window_id, "source": sanitize_opt(&ev.source), "key_fingerprint": sanitize_opt(&ev.fingerprint), "pid": ev.pid, "event_ts": ts }),
            ),
            "logout" => audit.record(
                "ssh_session_ended",
                json!({ "window_id": window_id, "source": sanitize_opt(&ev.source), "pid": ev.pid, "event_ts": ts }),
            ),
            "auth_failed" => {
                let source = sanitize_opt(&ev.source).unwrap_or_else(|| "unknown".to_string());
                *failures.entry(source).or_default() += 1;
            }
            "menu_action" => audit.record(
                "ssh_menu_action",
                json!({ "window_id": window_id, "message": sanitize(&ev.message, EVENT_TEXT_MAX_CHARS), "pid": ev.pid, "event_ts": ts }),
            ),
            _ => {}
        }
    }
    for (source, count) in failures {
        audit.record(
            "ssh_auth_failed",
            json!({ "window_id": window_id, "source": source, "count": count }),
        );
    }
}

// ── Status ──────────────────────────────────────────────────

/// `maintenance.status`. Always asks sysd on an appliance (M2); `caller`
/// adds that account's re-auth lock state.
pub async fn status_json(env: &MaintEnv, caller: Option<&str>) -> Result<Value, String> {
    let store = MaintenanceStore::open(&env.home_dir)?;
    let now = Utc::now();
    let row = store.current_row().await?;
    let shown = row.filter(|w| w.is_active(now) || w.state == WindowState::Opening);
    let active = shown.as_ref().is_some_and(|w| w.is_active(now));
    let latest = store.latest_window().await?;

    let (ssh, observed, confirmed_closed, pending_cleanup) = ssh_status(env).await;
    let close_unconfirmed = pending_cleanup
        || latest.as_ref().is_some_and(|w| {
            w.revoked_at.is_some() && w.close_error.is_some() && !confirmed_closed
        });

    let mut body = json!({
        "active": active,
        "window": shown.as_ref().map(|w| json!({
            "id": w.id,
            "state": w.state,
            "enabled_by": w.enabled_by,
            "enabled_by_email": w.enabled_by_email,
            "enabled_at": w.enabled_at,
            "ttl_seconds": w.ttl_seconds,
            "expires_at": w.expires_at,
            "sub_capabilities": w.sub_capabilities,
            "remaining_seconds": w.remaining_seconds(now),
            "ssh_key_type": w.ssh_key_type,
            "ssh_key_fingerprint": w.ssh_key_fingerprint,
            "ssh_key_label": w.ssh_key_label,
            "host_key_fingerprints": w.host_key_fingerprints,
        })),
        "ssh_observed_active": observed,
        "ssh": ssh,
        "close_unconfirmed": close_unconfirmed,
        "push_channel_available": env.notifier.channel_available().await,
    });
    if let Some(user_id) = caller {
        let until = store.reauth_locked_until(user_id, now).await;
        body["reauth"] = match until {
            Ok(Some(t)) => {
                json!({ "locked": true, "retry_after_seconds": (t - now).num_seconds().max(1) })
            }
            Ok(None) => json!({ "locked": false, "retry_after_seconds": null }),
            // Unreadable lock state: the enable would be refused, say so.
            Err(_) => json!({ "locked": true, "retry_after_seconds": null }),
        };
    }
    Ok(body)
}

/// `(ssh object, ssh_observed_active, sysd confirmed nothing is open, pending_cleanup)`.
async fn ssh_status(env: &MaintEnv) -> (Value, Option<bool>, bool, bool) {
    let base = |supported: Value, reachable: bool, reason: &str| {
        json!({
            "supported": supported,
            "reachable": reachable,
            "reason": reason,
            "ready": null,
            "key_installed": null,
            "key_fingerprint": null,
            "window_id": null,
            "expires_in_s": null,
            "fallback_timer_active": null,
            "active_sessions": 0,
            "sessions": [],
            "auth_failures": null,
            "event_overflow": 0,
            "pending_cleanup": false,
            "pending_audit": false,
            "problems": [],
            "host_key_fingerprints": [],
        })
    };
    let sysd = match env.sysd.access() {
        SysdAccess::OffAppliance => {
            return (
                base(json!(false), false, "off_appliance"),
                None,
                true,
                false,
            );
        }
        SysdAccess::Unreachable(_) => {
            return (
                base(Value::Null, false, "sysd_unreachable"),
                None,
                false,
                false,
            );
        }
        SysdAccess::Available(s) => s,
    };
    let st = match sysd.ssh_maint_status().await {
        Ok(out) if out.success => match StatusReply::parse(&out.stdout) {
            Ok(st) => st,
            Err(_) => {
                return (
                    base(Value::Null, true, "status_unreadable"),
                    None,
                    false,
                    false,
                );
            }
        },
        Ok(out) => {
            let mut v = base(Value::Null, true, "status_failed");
            v["problems"] = json!([sanitize(&out.stderr, PROBLEM_TEXT_MAX_CHARS)]);
            return (v, None, false, false);
        }
        // Old sysd: no keyed windows exist; the legacy probe still reports
        // whether an SSH server happens to run.
        Err(SysdCallError::UnknownVerb) => {
            return (
                base(json!(false), true, "sysd_too_old"),
                ssh_service_observed_active().await,
                false,
                false,
            );
        }
        Err(e) => {
            return (
                base(Value::Null, e.reached_sysd(), "status_failed"),
                None,
                false,
                false,
            );
        }
    };
    let observed = st.observed_active();
    let confirmed_closed = st.definitely_closed() && observed == Some(false);
    let sessions: Vec<Value> = st
        .sessions
        .iter()
        .map(|s| json!({ "source": sanitize_opt(&s.source), "key_fingerprint": sanitize_opt(&s.fingerprint), "since_unix": s.since_usec / 1_000_000 }))
        .collect();
    let ssh = json!({
        "supported": st.ready,
        "reachable": true,
        "reason": if st.ready { Value::Null } else { json!("image_not_ready") },
        "ready": st.ready,
        "key_installed": st.key_installed,
        "key_fingerprint": sanitize_opt(&st.key_fingerprint),
        "window_id": sanitize_opt(&st.window_id),
        "expires_in_s": st.expires_in_s,
        "fallback_timer_active": st.fallback_timer_active,
        "active_sessions": sessions.len(),
        "sessions": sessions,
        "auth_failures": st.auth_failures,
        "event_overflow": st.event_overflow,
        "pending_cleanup": st.pending_cleanup,
        "pending_audit": st.pending_audit,
        "problems": st.problems.iter().map(|p| sanitize(p, PROBLEM_TEXT_MAX_CHARS)).collect::<Vec<_>>(),
        "host_key_fingerprints": st.host_key_fingerprints.iter().map(|f| sanitize(f, EVENT_TEXT_MAX_CHARS)).collect::<Vec<_>>(),
    });
    (ssh, observed, confirmed_closed, st.pending_cleanup)
}
