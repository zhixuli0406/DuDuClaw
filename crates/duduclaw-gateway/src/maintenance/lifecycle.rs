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

mod errors;
mod reconcile;
mod status;

pub use errors::{MaintError, error_code};
use errors::{key_error, locked_error, unreachable_error, unsupported_error};
use reconcile::reconcile_with;

pub use reconcile::{SysdSupport, boot_reconcile, poll_sysd, revoke_on_boot};
pub(super) use reconcile::{boot_reconcile_guard, boot_reconcile_guarded};
pub use status::status_json;
