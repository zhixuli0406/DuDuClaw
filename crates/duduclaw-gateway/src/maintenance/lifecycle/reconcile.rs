//! Boot reconciliation (R3) and the supervisor's sysd poll: orphan keys,
//! windows the OS closed itself, and the audit mirror of sysd's events.

use super::*;

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
/// `reconcile_deadline` passes (R3, R2-L6). The caller keeps the window writer
/// lock: no new key may open before restart cleanup finishes.
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
    let guard = boot_reconcile_guard().await;
    boot_reconcile_guarded(env, revoked, guard).await
}

/// Acquire before spawning the supervisor, so even an enable dispatched
/// before that task's first poll must wait for the boot cleanup.
pub(in crate::maintenance) async fn boot_reconcile_guard() -> tokio::sync::MutexGuard<'static, ()> {
    WINDOW_LOCK.lock().await
}

/// Run R3 with the writer reservation acquired before the supervisor spawn.
/// Release it before pushing notices, which may wait on external channels.
pub(in crate::maintenance) async fn boot_reconcile_guarded(
    env: &MaintEnv,
    revoked: Vec<MaintenanceWindow>,
    guard: tokio::sync::MutexGuard<'static, ()>,
) -> SysdSupport {
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
    drop(guard);
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
pub(super) async fn reconcile_with(
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
            if !st.pending_cleanup && st.observed_active() == Some(false) {
                if let Err(e) = store.confirm_ssh_closed().await {
                    warn!(error = %e, "maintenance: could not persist a confirmed SSH close");
                }
            }
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
    let os_trigger = st
        .last_close
        .as_ref()
        .filter(|lc| lc.window_id.as_deref() == Some(row.id.as_str()))
        .map(|lc| lc.trigger_token());
    let close_err = if clean { None } else { stop_ssh(sysd).await };
    match store.revoke_closed_by_os(&row.id).await {
        Ok(Some(w)) => {
            if let Some(err) = &close_err {
                store.set_close_error(&w.id, err).await;
                notices.push(notify::close_unconfirmed_text(&w.id));
            }
            audit.record(
                "ssh_closed_by_os",
                json!({ "window_id": w.id, "key_fingerprint": w.ssh_key_fingerprint, "os_trigger": os_trigger, "os_close_clean": clean, "close_error": close_err }),
            );
            let _ = store.forget_ssh_events_except(None).await;
            info!(window_id = %w.id, clean, "maintenance: the OS closed the SSH window; ledger updated");
        }
        Ok(None) => {}
        Err(e) => warn!(error = %e, "maintenance: could not record closed_by_os"),
    }
}

/// Dedup key space for sysd close records (one row per closed window, kept
/// across windows by `forget_ssh_events_except`).
const CLOSES_KEY: &str = "__closes__";

/// Record sysd's last close once (trigger such as `reboot` or
/// `firewall_changed`, and whether login records were lost), when sysd
/// reports it.
async fn mirror_last_close(
    store: &MaintenanceStore,
    audit: &MaintenanceAuditLog,
    st: &StatusReply,
) {
    let Some(lc) = &st.last_close else { return };
    let Some(window_id) = sanitize_opt(&lc.window_id) else {
        return;
    };
    if !valid_cursor(&window_id) {
        return;
    }
    match store.mark_ssh_event_seen(CLOSES_KEY, &window_id).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            warn!(error = %e, "maintenance: close-record dedup unavailable");
            return;
        }
    }
    let mut fields = lc.to_json();
    fields["window_id"] = json!(window_id);
    audit.record("ssh_os_close_record", fields);
}

/// Mirror new sysd events into `maintenance_audit.jsonl`, deduplicated by
/// journal cursor per window. Every string is sanitised again here.
async fn mirror_events(store: &MaintenanceStore, audit: &MaintenanceAuditLog, st: &StatusReply) {
    mirror_last_close(store, audit, st).await;
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
