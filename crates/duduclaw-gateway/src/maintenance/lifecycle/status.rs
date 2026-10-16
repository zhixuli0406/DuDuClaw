//! `maintenance.status`: the ledger plus what sysd reports, asked every time.

use super::*;

// ── Status ──────────────────────────────────────────────────

/// `maintenance.status`. Always asks sysd on an appliance (M2); `caller`
/// adds that account's re-auth lock state.
pub async fn status_json(env: &MaintEnv, caller: Option<&str>) -> Result<Value, String> {
    let store = MaintenanceStore::open(&env.home_dir)?;
    let now = Utc::now();
    let row = store.current_row().await?;
    let shown = row.filter(|w| w.is_active(now) || w.state == WindowState::Opening);
    let active = shown.as_ref().is_some_and(|w| w.is_active(now));
    let unresolved_close = store.has_unconfirmed_close().await?;

    let (ssh, observed, confirmed_closed, pending_cleanup) = ssh_status(env).await;
    let close_unconfirmed = pending_cleanup || (unresolved_close && !confirmed_closed);

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
            "last_close": null,
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
        "last_close": st.last_close.as_ref().map(|lc| lc.to_json()),
        "problems": st.problems.iter().map(|p| sanitize(p, PROBLEM_TEXT_MAX_CHARS)).collect::<Vec<_>>(),
        "host_key_fingerprints": st.host_key_fingerprints.iter().map(|f| sanitize(f, EVENT_TEXT_MAX_CHARS)).collect::<Vec<_>>(),
    });
    (ssh, observed, confirmed_closed, st.pending_cleanup)
}
