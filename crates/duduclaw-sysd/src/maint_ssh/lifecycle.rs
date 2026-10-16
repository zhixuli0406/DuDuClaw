//! Opening, closing, status, the R4 tick, the R5 startup cleanup and the
//! shutdown flush. Every function here expects the caller to hold
//! [`SSH_LOCK`] (or to own the state outright in tests).

use super::events::{
    fill_close_counters, flush_pending, ingest, load_pending, load_window, persist_pending, persist_window, read_state_json, remove_window_file,
    sanitize_last_close, state_dir, write_state_json,
};
use super::*;

async fn cancel_fallback<H: MaintHost>(host: &H) -> Result<(), String> {
    // Stopping or resetting a unit that does not exist fails; the read below
    // decides.
    host.run(&timer::stop_argv()).await?;
    let _ = host.run(&timer::reset_failed_argv()).await;
    let out = host.run(&timer::is_active_argv()).await?;
    match out.stdout.trim() {
        "inactive" | "failed" => Ok(()),
        other => Err(format!("fallback timer {} is still {other:?}", timer::FALLBACK_TIMER)),
    }
}

async fn fallback_active<H: MaintHost>(host: &H) -> Option<bool> {
    host.run(&timer::is_active_argv()).await.ok().map(|o| o.stdout.trim() == "active")
}

/// Count an open attempt against the rate limit, refusing when it is used up.
fn rate_limit(state: &mut MaintState, cfg: &MaintConfig, now_boot: Duration) -> Result<(), SysdError> {
    let now = now_boot.as_millis() as u64;
    state.open_attempts_ms.retain(|t| now.saturating_sub(*t) < 3_600_000);
    let too_soon = state
        .open_attempts_ms
        .last()
        .is_some_and(|last| now.saturating_sub(*last) < cfg.open_min_interval.as_millis() as u64);
    if too_soon || state.open_attempts_ms.len() >= cfg.open_max_per_hour {
        return Err(SysdError::new(
            "rate_limited",
            format!(
                "too many maintenance SSH opens: at most {} per hour, {} s apart",
                cfg.open_max_per_hour,
                cfg.open_min_interval.as_secs()
            ),
        ));
    }
    state.open_attempts_ms.push(now);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OpenStage {
    KeyWritten,
    DeadlineSet,
    TimerArmed,
    Recorded,
    SshStarted,
}

/// `ssh_maint_open` (design §8.1). The order makes every observable effect
/// come after the one that limits it: key → window state (persistent) →
/// deadline → PID 1 fallback timer → open record → SSH started. Any failure
/// undoes the earlier steps, newest first.
pub async fn open<H: MaintHost>(
    state: &mut MaintState,
    host: &H,
    cfg: &MaintConfig,
    public_key: &str,
    ttl_seconds: u64,
    window_id: &str,
) -> DispatchResult {
    let now_boot = host.boottime().map_err(SysdError::io)?;
    rate_limit(state, cfg, now_boot)?;
    let host_key_fingerprints = verify_effective(host, cfg)
        .await
        .map_err(|e| SysdError::unsupported(format!("maintenance SSH is not available on this image: {e}")))?;
    validate_window_id(window_id)?;
    validate_ttl(ttl_seconds)?;
    let key = crate::ssh_key::parse(public_key).map_err(|e| SysdError::bad_request(format!("ssh_key:{}", e.code())))?;
    if state.window.is_some() || state.pending_cleanup || !state.pending.is_empty() {
        return Err(busy("a maintenance window is already open or still being cleaned up"));
    }
    let now_real = host.realtime().map_err(SysdError::io)?;
    let boot_id = host.boot_id().map_err(SysdError::io)?;
    let cursor = journal_position(host).await.map_err(SysdError::io)?;
    // The state directory first: without it the window could not survive a
    // reboot in the record.
    state_dir(cfg, true).map_err(SysdError::io)?;

    let dir = KeyDir::open_or_create(&cfg.key_dir, cfg.dir_owner).map_err(fs_err)?;
    match dir.write_atomic(KEY_FILE, key.authorized_keys_line().as_bytes(), cfg.key_owner, true) {
        Ok(()) => {}
        Err(FsError::Exists) => return Err(busy("a maintenance key is already installed")),
        Err(e) => {
            let _ = dir.remove(KEY_FILE);
            return Err(fs_err(e));
        }
    }

    let open_record = json!({
        "ts": now_real.as_secs(),
        "event": "open",
        "window_id": window_id,
        "key_type": key.key_type.name(),
        "key_fingerprint": key.fingerprint,
        "ttl_seconds": ttl_seconds,
        "expires_at": now_real.as_secs() + ttl_seconds,
        "host_key_fingerprints": host_key_fingerprints,
    });
    let window = WindowFile {
        format: 1,
        boot_id,
        window_id: window_id.to_string(),
        key_type: key.key_type.name().to_string(),
        key_fingerprint: key.fingerprint.clone(),
        host_key_fingerprints: host_key_fingerprints.clone(),
        ttl_seconds,
        deadline_boottime_ms: now_boot.as_millis() as u64 + ttl_seconds * 1000,
        opened_at_unix: now_real.as_secs(),
        open_cursor: cursor.clone(),
        last_cursor: cursor,
        logins_recorded: 0,
        logins_over_cap: 0,
        logouts_recorded: 0,
        logouts_over_cap: 0,
        auth_failures: 0,
        auth_failure_sources: Vec::new(),
        open_record: open_record.clone(),
    };

    let mut stage = OpenStage::KeyWritten;
    let result: Result<(), SysdError> = async {
        persist_window(cfg, &window).map_err(SysdError::io)?;
        state.window = Some(window.clone());
        stage = OpenStage::DeadlineSet;

        let _ = host.run(&timer::reset_failed_argv()).await;
        // Marked before running: a `systemd-run` that times out may still have
        // created the timer, and cancelling one that does not exist is harmless.
        stage = OpenStage::TimerArmed;
        run_ok(host, &timer::arm_argv(ttl_seconds, now_real))
            .await
            .map_err(|e| SysdError::io(format!("could not arm the fallback timer: {e}")))?;

        // On record before anyone can connect.
        cfg.audit.append(vec![open_record.clone()], Some(open_record.clone())).await.map_err(SysdError::io)?;
        stage = OpenStage::Recorded;

        let started = host.ssh_start().await;
        stage = OpenStage::SshStarted;
        match started {
            Ok(out) if out.success => Ok(()),
            Ok(out) => Err(SysdError::io(format!("could not start SSH: {}", out.stderr.trim()))),
            Err(e) => Err(SysdError::new(e.kind, format!("could not start SSH: {}", e.message))),
        }
    }
    .await;

    if let Err(e) = result {
        let problems = undo_open(state, host, cfg, &dir, stage).await;
        // Best effort: the audit file may be what failed.
        let failed = json!({
            "ts": now_real.as_secs(),
            "event": "open_failed",
            "window_id": window_id,
            "key_fingerprint": key.fingerprint,
            "error": e.kind,
            "stage": format!("{stage:?}"),
        });
        if let Err(audit_err) = cfg.audit.append(vec![failed], None).await {
            warn!(error = %audit_err, "sysd: could not record the failed maintenance SSH open");
        }
        let message = if problems.is_empty() { e.message } else { format!("{}; undoing the open: {}", e.message, problems.join("; ")) };
        return Err(SysdError::new(e.kind, message));
    }

    info!(window_id, fingerprint = %key.fingerprint, ttl_seconds, "sysd: maintenance SSH window opened");
    let body = json!({
        "login_user": MAINT_USER,
        "window_id": window_id,
        "key_type": key.key_type.name(),
        "key_fingerprint": key.fingerprint,
        "expires_in_s": ttl_seconds,
        "host_key_fingerprints": host_key_fingerprints,
    });
    Ok(SysdOpOutput { success: true, stdout: body.to_string(), stderr: String::new() })
}

async fn undo_open<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig, dir: &KeyDir, stage: OpenStage) -> Vec<String> {
    let mut problems = Vec::new();
    if stage >= OpenStage::SshStarted {
        match host.ssh_stop().await {
            Ok(out) if out.success => {}
            Ok(out) => problems.push(out.stderr),
            Err(e) => problems.push(e.message),
        }
    }
    if stage >= OpenStage::TimerArmed {
        if let Err(e) = cancel_fallback(host).await {
            problems.push(e);
        }
    }
    if let Err(e) = dir.remove(KEY_FILE) {
        problems.push(e.to_string());
    }
    state.window = None;
    if let Err(e) = remove_window_file(cfg) {
        problems.push(e);
    }
    // Something could not be undone: the tick keeps closing until it can.
    state.pending_cleanup = !problems.is_empty();
    problems
}

/// Whether a key (tmpfs) or window state (persistent) is lying around.
fn leftovers(cfg: &MaintConfig) -> Result<bool, String> {
    let key = match KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner).map_err(|e| e.to_string())? {
        Some(dir) => dir.exists(KEY_FILE).map_err(|e| e.to_string())?,
        None => false,
    };
    let window = match state_dir(cfg, false)? {
        Some(dir) => dir.exists(WINDOW_FILE).map_err(|e| e.to_string())?,
        None => false,
    };
    Ok(key || window)
}

/// Close the window. Steps, each run even if an earlier one failed:
/// write the events so far → remove the key → cancel the fallback timer →
/// stop dropbear, end its sessions and close the port (the legacy close
/// sequence) → read the events once more, up to now (sessions just ended
/// may have logged) → write the close record → remove the window state.
/// Events that cannot be read are kept for a later replay and the close
/// record is held back behind them. Anything unclear is a problem, never
/// "closed".
pub async fn close<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig, trigger: CloseTrigger) -> SysdOpOutput {
    let mut done: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    let key_dir = match KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner) {
        Ok(d) => d,
        Err(e) => {
            problems.push(format!("maintenance key directory: {e}"));
            None
        }
    };
    if state.window.is_none() {
        match load_window(cfg) {
            Ok(w) => state.window = w,
            Err(e) => problems.push(format!("window state unreadable, its events cannot be written: {e}")),
        }
    }
    let key_existed = match &key_dir {
        Some(d) => d.exists(KEY_FILE).unwrap_or(true),
        None => false,
    };
    if state.window.is_some() {
        if let Err(e) = ingest(state, host, cfg, None).await {
            problems.push(format!("writing SSH events: {e}"));
        }
    }
    let removed_fingerprint = state
        .window
        .as_ref()
        .map(|w| w.key_fingerprint.clone())
        .or_else(|| key_dir.as_ref().and_then(|d| installed_fingerprint(d, cfg)));

    if let Some(d) = &key_dir {
        match d.remove(KEY_FILE) {
            Ok(true) => done.push("removed the maintenance key".to_string()),
            Ok(false) => {}
            Err(e) => problems.push(format!("removing the maintenance key: {e}")),
        }
    }
    let open_sessions = state.sessions.len() + state.unkeyed_sessions.len();
    if let Err(e) = cancel_fallback(host).await {
        problems.push(e);
    }
    match host.ssh_stop().await {
        Ok(out) => {
            if !out.stdout.trim().is_empty() {
                done.push(out.stdout.trim().to_string());
            }
            if !out.success {
                problems.push(out.stderr.trim().to_string());
            }
        }
        Err(e) => problems.push(e.message),
    }

    // dropbear is gone now: one last read up to this moment.
    let close_usec = host.realtime().map(|d| d.as_micros() as u64).unwrap_or(u64::MAX);
    let mut events_complete = true;
    if state.window.is_some() {
        match ingest(state, host, cfg, Some(close_usec)).await {
            Ok((_, complete)) => events_complete = complete,
            Err(e) => {
                events_complete = false;
                problems.push(format!("reading the last SSH events (kept for a retry): {e}"));
            }
        }
    }

    let window = state.window.take();
    let had_window = window.is_some() || key_existed || state.pending_cleanup;
    if window.is_some() || key_existed {
        let w = window.as_ref();
        let mut record = json!({
            "ts": close_usec / 1_000_000,
            "event": "close",
            "trigger": trigger.as_str(),
            "window_id": w.map(|w| w.window_id.clone()),
            "removed_fingerprint": removed_fingerprint,
            "done": done,
            "problems": problems,
            "logins_recorded": Value::Null,
            "logins_over_cap": Value::Null,
            "logouts_recorded": Value::Null,
            "logouts_over_cap": Value::Null,
            "auth_failures": Value::Null,
            "auth_failure_sources": Value::Null,
            // Sessions still logged in when the close ended them (their
            // processes are killed, so dropbear may write no logout line).
            "sessions_open_at_close": open_sessions,
            "last_cursor": Value::Null,
            "events_complete": events_complete,
        });
        if let Some(w) = w {
            fill_close_counters(&mut record, w, events_complete);
        }
        let reopen = w.map(|w| w.open_record.clone());
        match (events_complete, w) {
            // Events still owed: hold the close record back until they are in.
            (false, Some(w)) => {
                state.pending.backfill = Some(Backfill { window: w.clone(), until_usec: close_usec, attempts: 0 });
                state.pending.records.push(record);
                state.pending.reopen = reopen;
            }
            _ => {
                if let Err(e) = cfg.audit.append(vec![record.clone()], reopen.clone()).await {
                    problems.push(format!("close record kept for a retry: {e}"));
                    state.pending.records.push(record);
                    state.pending.reopen = reopen;
                }
            }
        }
        if let Err(e) = persist_pending(cfg, &state.pending) {
            problems.push(format!("saving pending audit work: {e}"));
        }
        // A startup cleanup after the OS fallback timer already closed this
        // window keeps the script's `fallback_timer` as the reason.
        let ts = close_usec / 1_000_000;
        let mut reason = trigger.as_str().to_string();
        if matches!(trigger, CloseTrigger::StartupCleanup | CloseTrigger::Reboot) {
            if let (Ok(Some(prev)), Some(w)) = (read_state_json::<LastClose>(cfg, LAST_CLOSE_FILE), w) {
                if prev.trigger == "fallback_timer" && prev.ts >= w.opened_at_unix {
                    reason = prev.trigger;
                }
            }
        }
        let last = LastClose {
            window_id: w.map(|w| w.window_id.clone()),
            trigger: reason,
            events_complete,
            events_lost: false,
            ts,
        };
        if let Err(e) = write_state_json(cfg, LAST_CLOSE_FILE, &last) {
            problems.push(format!("saving the last close: {e}"));
        }
        info!(trigger = trigger.as_str(), problems = problems.len(), "sysd: maintenance SSH window closed");
    }
    // The window state goes last: until the pending work is saved it is the
    // only record of the cursor.
    if let Err(e) = remove_window_file(cfg) {
        problems.push(format!("removing {WINDOW_FILE}: {e}"));
    }
    state.sessions.clear();
    state.unkeyed_sessions.clear();
    state.events = EventBuffer::new();
    // A close that left something behind (key, timer, port or sessions) is
    // retried by the tick until it succeeds, whoever asked for it. Only for
    // windows: a failed legacy close on an image without the account is
    // reported to the caller as before.
    if had_window {
        state.pending_cleanup = !problems.is_empty();
    }

    SysdOpOutput {
        success: problems.is_empty(),
        stdout: if done.is_empty() { "nothing was open".to_string() } else { done.join("; ") },
        stderr: problems.join("; "),
    }
}

fn installed_fingerprint(dir: &KeyDir, cfg: &MaintConfig) -> Option<String> {
    let raw = dir.read(KEY_FILE, cfg.key_owner, crate::ssh_key::MAX_INPUT_BYTES * 2).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let mut fields = text.split_ascii_whitespace().skip(1);
    let (ty, b64) = (fields.next()?, fields.next()?);
    crate::ssh_key::parse(&format!("{ty} {b64}")).ok().map(|k| k.fingerprint)
}

/// `ssh_maint_status`. Reads new events first (so the audit file is never
/// behind what the gateway is told).
pub async fn status<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> DispatchResult {
    let mut problems: Vec<String> = Vec::new();
    match ingest(state, host, cfg, None).await {
        Ok((true, _)) => {
            if let Some(w) = state.window.as_ref() {
                if let Err(e) = persist_window(cfg, w) {
                    problems.push(e);
                }
            }
        }
        Ok((false, _)) => {}
        Err(e) => problems.push(format!("reading SSH events: {e}")),
    }
    let key_installed = match KeyDir::open_existing(&cfg.key_dir, cfg.dir_owner) {
        Ok(None) => Some(false),
        Ok(Some(d)) => d.exists(KEY_FILE).ok(),
        Err(e) => {
            problems.push(e.to_string());
            None
        }
    };
    let expires_in_s = match (state.window.as_ref(), host.boottime()) {
        (Some(w), Ok(now)) => Some(w.deadline_boottime_ms.saturating_sub(now.as_millis() as u64) / 1000),
        _ => None,
    };
    let ssh_active = host
        .run(&argv(&["systemctl", "is-active", DROPBEAR_SOCKET]))
        .await
        .ok()
        .map(|o| o.stdout.trim() == "active");
    let last_close = match read_state_json::<LastClose>(cfg, LAST_CLOSE_FILE).and_then(|o| o.map(sanitize_last_close).transpose()) {
        Ok(l) => l,
        Err(e) => {
            problems.push(format!("last close unreadable: {e}"));
            None
        }
    };
    let audit_backlog_lost = match (read_state_json::<BacklogLost>(cfg, BACKLOG_LOST_FILE), host.boot_id()) {
        (Ok(Some(b)), Ok(now)) if b.boot_id == now => Some(json!({ "ts": b.ts })),
        (Err(e), _) => {
            problems.push(format!("backlog marker unreadable: {e}"));
            None
        }
        _ => None,
    };
    let mut sessions: Vec<&Session> = state.sessions.values().collect();
    sessions.extend(state.unkeyed_sessions.iter());
    let w = state.window.as_ref();
    let body = json!({
        "ready": readiness(&cfg.ready_marker) == Readiness::Ready,
        "key_installed": key_installed,
        "key_fingerprint": w.map(|w| w.key_fingerprint.clone()),
        "window_id": w.map(|w| w.window_id.clone()),
        "expires_in_s": expires_in_s,
        "fallback_timer_active": fallback_active(host).await,
        "ssh_active": ssh_active,
        "sessions": sessions,
        "events": state.events.items,
        "event_overflow": state.events.overflow,
        "auth_failures": w.map(|w| w.auth_failures),
        "host_key_fingerprints": w.map(|w| w.host_key_fingerprints.clone()).unwrap_or_default(),
        "pending_cleanup": state.pending_cleanup,
        "pending_audit": !state.pending.is_empty(),
        "last_close": last_close,
        "audit_backlog_lost": audit_backlog_lost,
        "problems": problems,
    });
    Ok(SysdOpOutput { success: true, stdout: body.to_string(), stderr: String::new() })
}

/// R4: finish owed audit work, re-check the egress rule (close if it is
/// gone), write new events, then close at the deadline. Also retries an
/// unfinished cleanup.
pub async fn tick<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) {
    if let Err(e) = flush_pending(state, host, cfg).await {
        warn!(error = %e, "sysd: maintenance SSH audit work still pending; retrying next tick");
    }
    if state.window.is_none() {
        let leftover = match leftovers(cfg) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "sysd: cannot inspect the maintenance SSH directories");
                true
            }
        };
        if state.pending_cleanup || leftover {
            let out = close(state, host, cfg, CloseTrigger::StartupCleanup).await;
            if !out.success {
                warn!(problems = %out.stderr, "sysd: maintenance SSH cleanup incomplete; retrying next tick");
            }
        }
        return;
    }
    if let Err(e) = egress_rules_ok(host, cfg).await {
        warn!(error = %e, "sysd: the maintenance egress rule is gone or unreadable; closing the window");
        let out = close(state, host, cfg, CloseTrigger::FirewallChanged).await;
        if !out.success {
            warn!(problems = %out.stderr, "sysd: maintenance SSH close incomplete; retrying next tick");
        }
        return;
    }
    match ingest(state, host, cfg, None).await {
        Ok((true, _)) => {
            if let Some(w) = state.window.as_ref() {
                if let Err(e) = persist_window(cfg, w) {
                    warn!(error = %e, "sysd: could not save the maintenance window state");
                }
            }
        }
        Ok((false, _)) => {}
        Err(e) => warn!(error = %e, "sysd: could not read maintenance SSH events"),
    }
    let expired = match (state.window.as_ref(), host.boottime()) {
        (Some(w), Ok(now)) => now.as_millis() as u64 >= w.deadline_boottime_ms,
        // No trustworthy clock: close rather than keep a key without a deadline.
        (Some(_), Err(_)) => true,
        (None, _) => false,
    };
    if expired {
        let out = close(state, host, cfg, CloseTrigger::Deadline).await;
        if !out.success {
            warn!(problems = %out.stderr, "sysd: maintenance SSH deadline close incomplete; retrying next tick");
        }
    }
}

/// R5: at startup, pick up owed audit work and close whatever a previous run
/// (or a previous boot) left behind. A window opened in another boot closes
/// with trigger `reboot`, after replaying its events from the saved cursor.
pub async fn startup_cleanup<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) {
    if let Some(p) = load_pending(host, cfg).await {
        state.pending = p;
    }
    let leftover = match leftovers(cfg) {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "sysd: cannot inspect the maintenance SSH directories at startup");
            true
        }
    };
    if leftover {
        let trigger = match (load_window(cfg), host.boot_id()) {
            (Ok(Some(w)), Ok(now)) if !w.boot_id.is_empty() && w.boot_id != now => CloseTrigger::Reboot,
            _ => CloseTrigger::StartupCleanup,
        };
        let out = close(state, host, cfg, trigger).await;
        if out.success {
            info!(trigger = trigger.as_str(), "sysd: closed a maintenance SSH window left by a previous run");
        } else {
            warn!(problems = %out.stderr, "sysd: startup cleanup of maintenance SSH incomplete; retrying every tick");
        }
    }
    if let Err(e) = flush_pending(state, host, cfg).await {
        warn!(error = %e, "sysd: maintenance SSH audit work still pending; retrying every tick");
    }
}

/// The R5 + R4 supervisor for the real system. `first` is [`SSH_LOCK`] taken
/// before the socket accepts connections, so the startup cleanup runs before
/// any SSH verb.
pub async fn run_supervisor(mut first: tokio::sync::MutexGuard<'static, MaintState>) {
    let cfg = MaintConfig::system();
    let host = host::SystemHost;
    startup_cleanup(&mut first, &host, &cfg).await;
    drop(first);
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        interval.tick().await;
        let mut state = SSH_LOCK.lock().await;
        tick(&mut state, &host, &cfg).await;
    }
}

/// On SIGTERM (a reboot or poweroff stops sysd this way): wait a bounded time
/// for [`SSH_LOCK`], then write the events read so far and save the cursor,
/// so the next boot's cleanup has as little as possible to replay.
pub async fn shutdown_flush(wait: Duration) {
    let Ok(mut state) = tokio::time::timeout(wait, SSH_LOCK.lock()).await else {
        warn!("sysd: maintenance SSH state busy at shutdown; the next startup replays from the saved cursor");
        return;
    };
    let cfg = MaintConfig::system();
    let host = host::SystemHost;
    let work = async {
        if state.window.is_some() {
            match ingest(&mut state, &host, &cfg, None).await {
                Ok((true, _)) => {
                    if let Some(w) = state.window.as_ref() {
                        if let Err(e) = persist_window(&cfg, w) {
                            warn!(error = %e, "sysd: could not save the maintenance window state at shutdown");
                        }
                    }
                }
                Ok((false, _)) => {}
                Err(e) => warn!(error = %e, "sysd: could not read maintenance SSH events at shutdown"),
            }
        }
        let _ = flush_pending(&mut state, &host, &cfg).await;
    };
    if tokio::time::timeout(wait, work).await.is_err() {
        warn!("sysd: maintenance SSH shutdown flush timed out");
    }
}
