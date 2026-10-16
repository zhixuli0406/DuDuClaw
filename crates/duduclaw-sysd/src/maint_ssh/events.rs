//! Journal events → audit records, and the persisted state that lets that
//! work survive a sysd restart or a reboot (`window.json` and
//! `audit-pending.json` in [`STATE_DIR`]).

use super::*;

pub(super) fn usec_to_secs(usec: u64) -> u64 {
    usec / 1_000_000
}

/// What one journal read produced for a window.
pub(super) struct Read {
    pub next: WindowFile,
    pub records: Vec<Value>,
    pub events: Vec<SshEvent>,
    /// Everything up to `until` (or the journal's end) was read.
    pub complete: bool,
}

/// Read journal events after `window.last_cursor` (at most
/// [`MAX_EVENTS_PER_READ`] lines, streamed) and turn them into audit records:
/// logins/logouts (first [`LOGIN_RECORD_CAP`] each, then counted) and failure
/// counts. With `until_usec`, entries after that time are left alone. The
/// trusted executables are resolved first; if that fails nothing is read.
pub(super) async fn read_events<H: MaintHost>(host: &H, cfg: &MaintConfig, window: &WindowFile, until_usec: Option<u64>) -> Result<Read, String> {
    if !journal::valid_cursor(&window.last_cursor) {
        return Err("the window's journal cursor is invalid".to_string());
    }
    let dropbear_exe = host.trusted_exe(&cfg.dropbear_exe)?;
    let menu_exe = host.trusted_exe(&cfg.menu_exe)?;
    let trust = journal::Trust { maint_uid: cfg.maint_uid, dropbear_exe: &dropbear_exe, menu_exe: &menu_exe };
    let out = host.run_lines(&journal_read_argv(&window.last_cursor), MAX_EVENTS_PER_READ).await?;
    let mut next = window.clone();
    let mut records = Vec::new();
    let mut events = Vec::new();
    let mut complete = !out.truncated;
    for line in &out.lines {
        let Some((cursor, event)) = journal::classify_line(line, &trust) else { continue };
        if let (Some(until), Some(e)) = (until_usec, &event) {
            if e.realtime_usec > until {
                complete = true;
                break;
            }
        }
        next.last_cursor = cursor;
        let Some(event) = event else { continue };
        match event.kind {
            EventKind::Login => {
                if next.logins_recorded < LOGIN_RECORD_CAP {
                    next.logins_recorded += 1;
                    let mut r = event_record("login", &next.window_id, &event);
                    r["fingerprint"] = json!(event.fingerprint);
                    records.push(r);
                } else {
                    next.logins_over_cap += 1;
                }
            }
            EventKind::Logout => {
                if next.logouts_recorded < LOGIN_RECORD_CAP {
                    next.logouts_recorded += 1;
                    records.push(event_record("logout", &next.window_id, &event));
                } else {
                    next.logouts_over_cap += 1;
                }
            }
            EventKind::AuthFailed => {
                next.auth_failures += 1;
                if let Some(src) = &event.source {
                    if next.auth_failure_sources.len() < AUTH_FAILURE_SOURCES && !next.auth_failure_sources.contains(src) {
                        next.auth_failure_sources.push(src.clone());
                    }
                }
            }
            EventKind::MenuAction => {}
        }
        events.push(event);
    }
    Ok(Read { next, records, events, complete })
}

/// Read new journal events into the open window (up to `until_usec` when
/// given): records go to the audit file, events to the status buffer. State
/// changes only after the audit write succeeded, so a failed write is retried
/// from the same cursor. Returns (window changed, everything read).
pub(super) async fn ingest<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig, until_usec: Option<u64>) -> Result<(bool, bool), String> {
    let Some(current) = state.window.as_ref() else { return Ok((false, true)) };
    let read = read_events(host, cfg, current, until_usec).await?;
    if read.next == *current {
        return Ok((false, read.complete));
    }
    cfg.audit.append(read.records, Some(read.next.open_record.clone())).await?;
    for event in read.events {
        apply_session(state, &event);
        state.events.push(event);
    }
    state.window = Some(read.next);
    Ok((true, read.complete))
}

pub(super) fn event_record(kind: &str, window_id: &str, event: &SshEvent) -> Value {
    json!({
        "ts": usec_to_secs(event.realtime_usec),
        "event": kind,
        "window_id": window_id,
        "source": event.source,
        "pid": event.pid,
        "cursor": event.cursor,
    })
}

fn apply_session(state: &mut MaintState, event: &SshEvent) {
    match event.kind {
        EventKind::Login => {
            let s = Session {
                pid: event.pid,
                source: event.source.clone(),
                fingerprint: event.fingerprint.clone(),
                since_usec: event.realtime_usec,
            };
            match event.pid {
                Some(pid) => {
                    state.sessions.insert(pid, s);
                }
                None => state.unkeyed_sessions.push(s),
            }
        }
        EventKind::Logout => {
            let removed = event.pid.and_then(|pid| state.sessions.remove(&pid)).is_some();
            if !removed {
                if let Some(i) = state.unkeyed_sessions.iter().position(|s| s.source == event.source) {
                    state.unkeyed_sessions.remove(i);
                }
            }
        }
        _ => {}
    }
}

/// The persistent state directory, verified; `Ok(None)` when it does not
/// exist (and `create` is false).
pub(super) fn state_dir(cfg: &MaintConfig, create: bool) -> Result<Option<KeyDir>, String> {
    if create {
        KeyDir::open_or_create(&cfg.state_dir, cfg.state_dir_owner).map(Some).map_err(|e| e.to_string())
    } else {
        KeyDir::open_existing(&cfg.state_dir, cfg.state_dir_owner).map_err(|e| e.to_string())
    }
}

pub(super) fn persist_window(cfg: &MaintConfig, window: &WindowFile) -> Result<(), String> {
    let dir = state_dir(cfg, true)?.ok_or_else(|| format!("{} is missing", cfg.state_dir.display()))?;
    let body = serde_json::to_vec(window).map_err(|e| e.to_string())?;
    dir.write_atomic(WINDOW_FILE, &body, cfg.state_owner, false).map_err(|e| e.to_string())
}

pub(super) fn load_window(cfg: &MaintConfig) -> Result<Option<WindowFile>, String> {
    let Some(dir) = state_dir(cfg, false)? else { return Ok(None) };
    if !dir.exists(WINDOW_FILE).map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let raw = dir.read(WINDOW_FILE, cfg.state_owner, MAX_WINDOW_FILE_BYTES).map_err(|e| e.to_string())?;
    serde_json::from_slice(&raw).map(Some).map_err(|e| format!("{WINDOW_FILE}: {e}"))
}

pub(super) fn remove_window_file(cfg: &MaintConfig) -> Result<(), String> {
    match state_dir(cfg, false)? {
        Some(dir) => dir.remove(WINDOW_FILE).map(|_| ()).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

pub(super) fn persist_pending(cfg: &MaintConfig, pending: &PendingAudit) -> Result<(), String> {
    if pending.is_empty() {
        return match state_dir(cfg, false)? {
            Some(dir) => dir.remove(PENDING_FILE).map(|_| ()).map_err(|e| e.to_string()),
            None => Ok(()),
        };
    }
    let dir = state_dir(cfg, true)?.ok_or_else(|| format!("{} is missing", cfg.state_dir.display()))?;
    let body = serde_json::to_vec(pending).map_err(|e| e.to_string())?;
    dir.write_atomic(PENDING_FILE, &body, cfg.state_owner, false).map_err(|e| e.to_string())
}

/// Load owed audit work saved by an earlier run. If the file exists but
/// cannot be read, write an `audit_backlog_lost` record and remove it (once
/// that record is written), so the loss is on record instead of silent.
pub(super) async fn load_pending<H: MaintHost>(host: &H, cfg: &MaintConfig) -> Option<PendingAudit> {
    let dir = match state_dir(cfg, false) {
        Ok(Some(d)) => d,
        Ok(None) => return None,
        Err(e) => {
            warn!(error = %e, "sysd: maintenance SSH state directory is not trustworthy");
            return None;
        }
    };
    match dir.exists(PENDING_FILE) {
        Ok(false) => return None,
        Ok(true) => {}
        Err(e) => {
            warn!(error = %e, "sysd: cannot inspect the pending maintenance SSH audit work");
            return None;
        }
    }
    let parsed = dir
        .read(PENDING_FILE, cfg.state_owner, MAX_PENDING_FILE_BYTES)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_slice::<PendingAudit>(&raw).map_err(|e| e.to_string()));
    match parsed {
        Ok(p) => Some(p),
        Err(e) => {
            let ts = host.realtime().map(|d| d.as_secs()).unwrap_or(0);
            let lost = json!({"ts": ts, "event": "audit_backlog_lost", "error": journal::truncate_chars(&e, 200)});
            match cfg.audit.append(vec![lost], None).await {
                Ok(()) => {
                    let _ = dir.remove(PENDING_FILE);
                    let mark = BacklogLost { ts, boot_id: host.boot_id().unwrap_or_default() };
                    if let Err(e) = write_state_json(cfg, BACKLOG_LOST_FILE, &mark) {
                        warn!(error = %e, "sysd: could not save the backlog-lost marker");
                    }
                }
                Err(w) => warn!(error = %w, "sysd: could not record the lost maintenance SSH audit backlog"),
            }
            None
        }
    }
}

/// Finish owed audit work: replay a closed window's remaining events, then
/// write the held-back records (its close record last, completed with the
/// final counters). Stops at the first failure and keeps the rest; a replay
/// that keeps failing is given up after [`MAX_BACKFILL_ATTEMPTS`].
pub(super) async fn flush_pending<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> Result<(), String> {
    if state.pending.is_empty() {
        return Ok(());
    }
    let result = flush_pending_inner(state, host, cfg).await;
    if let Err(e) = persist_pending(cfg, &state.pending) {
        warn!(error = %e, "sysd: could not save pending maintenance SSH audit work");
    }
    result
}

async fn flush_pending_inner<H: MaintHost>(state: &mut MaintState, host: &H, cfg: &MaintConfig) -> Result<(), String> {
    if let Some(bf) = state.pending.backfill.clone() {
        let reopen = Some(bf.window.open_record.clone());
        let replay = match read_events(host, cfg, &bf.window, Some(bf.until_usec)).await {
            Ok(read) => cfg.audit.append(read.records.clone(), reopen).await.map(|()| read),
            Err(e) => Err(e),
        };
        match replay {
            Ok(read) if read.complete => {
                complete_close_records(&mut state.pending.records, &read.next, true, false);
                update_last_close(cfg, &read.next.window_id, true, false);
                state.pending.backfill = None;
            }
            Ok(read) => {
                state.pending.backfill = Some(Backfill { window: read.next, until_usec: bf.until_usec, attempts: 0 });
                return Err("more events of a closed window remain to be written".to_string());
            }
            Err(e) => {
                let attempts = bf.attempts + 1;
                if attempts < MAX_BACKFILL_ATTEMPTS {
                    state.pending.backfill = Some(Backfill { attempts, ..bf });
                    return Err(e);
                }
                warn!(error = %e, "sysd: giving up on the remaining events of a closed maintenance SSH window");
                complete_close_records(&mut state.pending.records, &bf.window, false, true);
                update_last_close(cfg, &bf.window.window_id, false, true);
                state.pending.backfill = None;
            }
        }
    }
    let records = std::mem::take(&mut state.pending.records);
    if let Err(e) = cfg.audit.append(records.clone(), state.pending.reopen.clone()).await {
        state.pending.records = records;
        return Err(e);
    }
    state.pending.reopen = None;
    Ok(())
}

/// Write a small JSON state file (root 0600) in [`STATE_DIR`].
pub(super) fn write_state_json<T: Serialize>(cfg: &MaintConfig, name: &str, value: &T) -> Result<(), String> {
    let dir = state_dir(cfg, true)?.ok_or_else(|| format!("{} is missing", cfg.state_dir.display()))?;
    let body = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    dir.write_atomic(name, &body, cfg.state_owner, false).map_err(|e| e.to_string())
}

/// Read a small JSON state file; `Ok(None)` when it (or the directory) does
/// not exist.
pub(super) fn read_state_json<T: serde::de::DeserializeOwned>(cfg: &MaintConfig, name: &str) -> Result<Option<T>, String> {
    let Some(dir) = state_dir(cfg, false)? else { return Ok(None) };
    if !dir.exists(name).map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let raw = dir.read(name, cfg.state_owner, 4096).map_err(|e| e.to_string())?;
    serde_json::from_slice(&raw).map(Some).map_err(|e| format!("{name}: {e}"))
}

/// Validate a `last-close.json` read back (the OS script may write it too):
/// a known trigger and a well-formed window id, or nothing.
pub(super) fn sanitize_last_close(raw: LastClose) -> Result<LastClose, String> {
    if !CLOSE_TRIGGERS.contains(&raw.trigger.as_str()) {
        return Err(format!("{LAST_CLOSE_FILE} has an unknown trigger"));
    }
    let window_id = raw.window_id.filter(|id| validate_window_id(id).is_ok());
    Ok(LastClose { window_id, ..raw })
}

/// After a held-back close record was finally written: bring `last_close`
/// up to date if it is that window's.
fn update_last_close(cfg: &MaintConfig, window_id: &str, events_complete: bool, events_lost: bool) {
    let Ok(Some(mut last)) = read_state_json::<LastClose>(cfg, LAST_CLOSE_FILE) else { return };
    if last.window_id.as_deref() != Some(window_id) {
        return;
    }
    last.events_complete = events_complete;
    last.events_lost = events_lost;
    if let Err(e) = write_state_json(cfg, LAST_CLOSE_FILE, &last) {
        warn!(error = %e, "sysd: could not update the last close");
    }
}

fn complete_close_records(records: &mut [Value], w: &WindowFile, events_complete: bool, events_lost: bool) {
    for record in records.iter_mut() {
        if record["event"] == "close" && record["window_id"] == json!(w.window_id) {
            fill_close_counters(record, w, events_complete);
            if events_lost {
                record["events_lost"] = json!(true);
            }
        }
    }
}

pub(super) fn fill_close_counters(record: &mut Value, w: &WindowFile, events_complete: bool) {
    record["logins_recorded"] = json!(w.logins_recorded);
    record["logins_over_cap"] = json!(w.logins_over_cap);
    record["logouts_recorded"] = json!(w.logouts_recorded);
    record["logouts_over_cap"] = json!(w.logouts_over_cap);
    record["auth_failures"] = json!(w.auth_failures);
    record["auth_failure_sources"] = json!(w.auth_failure_sources);
    record["last_cursor"] = json!(w.last_cursor);
    record["events_complete"] = json!(events_complete);
}
